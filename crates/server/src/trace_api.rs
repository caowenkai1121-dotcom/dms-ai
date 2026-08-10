//! 【A10】Trace 时间线（DataFoundry trace-dag 的对应物，`docs/research/datafoundry.json`）：
//! 一会话一事件流回放 —— 用户提问 → 路由链（命中/未命中/跳过）→（自修/失败重试）→
//! AI 回答 → 产物生成，按时间序组装。
//!
//! ## 端点契约（**路由注册由集成方在 `main.rs` 补**，本文件只供 handler 与纯函数）
//!
//! ### `GET /api/chat/conv/{id}/trace?login_name=&role_code=`
//! 集成接线（`main.rs` 的 Router 处，`/api/conv/{id}` 那一组旁）：
//! `.route("/api/chat/conv/{id}/trace", get(trace_api::conv_trace))`
//! 接线后把 `mod trace_api;` 上的 `#[allow(dead_code)]` 一行删掉（与 `usage_api` 同一模子：
//! handler 一经 `.route` 引用，全模块即活）。
//!
//! 身份：`resolve_identity`（401）→ `chat::conv_owner` 属主闸门（**fail-closed 内联**：
//! 非属主 403、闸门自身读失败 500；任何取数都在闸门之后）。全程**只读**：本文件两条 SQL
//! 都是 SELECT，不下推任何写（有锚点单测钉着）。
//!
//! 响应：
//! ```json
//! { "conv_id": 123,
//!   "rounds": [ { "msg_id": 456, "question": "本月销售额", "at": "2026-08-08T10:00:01+00:00",
//!     "status": "succeeded", "route": "llm+repair", "elapsed_ms": 1234,
//!     "events": [
//!       {"kind":"question","at":"…","text":"本月销售额"},
//!       {"kind":"route","stage":"semantic-cache","result":"miss","ms":3},
//!       {"kind":"route","stage":"llm","result":"hit","ms":1180},
//!       {"kind":"retry","reason":"repair","ms":null,"error":""},
//!       {"kind":"answer","route":"llm+repair","ms":1234,"sql":"SELECT …","row_count":12},
//!       {"kind":"artifact","id":5,"title":"…","preview_url":"/api/artifact/5/view"}
//!     ] } ] }
//! ```
//! - `rounds[].status`：`succeeded`（chat.msg 成对落库的正常轮）/ `interrupted`（user 行
//!   落库、ai 行没落 —— 两次 `save_msg` 之间崩了的唯一留痕）/ query_log 失败态
//!   （`failed`/`timeout`/`blocked`：硬失败轮**不进** chat.msg，只能从 query_log 补回）。
//! - 事件 `ms`：steps 逐阶段耗时与整轮 `elapsed_ms` **原样透出**；`null` = 该事件没有
//!   可归属耗时（轮内自修没有独立计时，它含在 answer 的 ms 里）。
//! - **降级**：payload 无 `steps`（本字段上线前的老消息 / 知识库文本答）→ 一轮只剩
//!   「问题→回答」两节点，路由链一节都不编。
//!
//! 两个事实源（不多不少）：
//! - `chat.msg`（按 id 升序配 user→ai 成轮）：payload 的 `steps/route/elapsed_ms/sql`。
//!   深度模式落库形态是 `{"result":…,"artifact":…}` 包裹（`deep_api::compose`），
//!   取事件前先剥 `result` 一层；`artifact` 键 = 产物生成事件。
//! - `meta.query_log` 同 `conv_id` 的**失败行**：成功轮的事实 chat.msg 已有（更丰富），
//!   query_log 在这里只补「这轮失败过/被重试过」这一层 —— 失败轮 chat.msg 一行都不落
//!   （`api_ask` 早返回），不问 query_log 这段历史就消失了。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;

use crate::AppState;

type ApiErr = (StatusCode, Json<Value>);

fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// 会话全部消息（id 升序 = 落库序）。与 `chat::conv_msgs` 同表同序，多取 id/created_at：
/// 配对成轮与跨源对时都要。
const MSGS_SQL: &str =
    "SELECT id, role, question, payload, created_at FROM chat.msg WHERE conv_id = $1 ORDER BY id";

/// 同会话的**失败**尝试（成功轮的事实 chat.msg 已有，不重复取）。
/// 老行（status 列上线前）为空串：error 非空折成 'failed'；error 也空的是老成功行，不取。
/// `conv_id` 列是文本形（`ddl.rs` 的 ALTER），绑会话主键的 `to_string()`。
const FAILED_SQL: &str =
    "SELECT question, route, elapsed_ms,
            CASE WHEN status = '' THEN 'failed' ELSE status END AS status,
            error, at
     FROM meta.query_log
     WHERE conv_id = $1
       AND (status IN ('blocked','failed','timeout') OR (status = '' AND error <> ''))
     ORDER BY at";

/// `chat.msg` 一行（`MSGS_SQL` 的列序）
pub struct MsgRow {
    pub id: i64,
    pub role: String,
    /// user 行的问句原文（ai 行恒空串，见 `chat::save_msg` 调用点）
    pub question: String,
    /// ai 行的结果载荷（`AskResult` / `Answer` / 深度模式包裹）
    pub payload: Option<Value>,
    pub at: DateTime<Utc>,
}

/// `meta.query_log` 失败行（`FAILED_SQL` 的列序）
pub struct FailedRow {
    pub question: String,
    pub route: String,
    pub elapsed_ms: i64,
    /// 四值失败态之一（老行空串已在 SQL 里折成 'failed'）
    pub status: String,
    pub error: String,
    pub at: DateTime<Utc>,
}

/// 一轮问答（或一次未成轮的失败尝试）。字段即端点契约，见文件头。
#[derive(Serialize, Debug, PartialEq)]
pub struct Round {
    /// ai 消息 id；query_log 补回的失败轮 / interrupted 轮没有 ai 行 → null
    pub msg_id: Option<i64>,
    pub question: String,
    /// 问句（或失败尝试）时间，RFC3339
    pub at: String,
    /// `succeeded` / `interrupted` / query_log 失败态（`failed`/`timeout`/`blocked`）
    pub status: String,
    /// 命中路由（`AskResult.route`；知识库为 `knowledge`；无则空串）
    pub route: String,
    /// 整轮耗时（payload / query_log 原样透出；无则 null）
    pub elapsed_ms: Option<i64>,
    pub events: Vec<Event>,
}

/// 时间线节点。`kind` 五值 = 事件分类：question / route / retry / answer / artifact。
#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// 用户提问
    Question { at: String, text: String },
    /// 路由链一步（payload.steps 原样转写：stage=成员表标签，result=hit/miss/skip）
    Route { stage: String, result: String, ms: i64 },
    /// 重试。reason="repair" = 轮内 SQL 自修（`route` 带 `+repair` 的留痕；自修发生在
    /// llm 成员内部，steps 不再细分，独立耗时不存在 → null）。其余三值 = query_log 的
    /// 失败尝试（error=入库的脱敏原因，ms=那次尝试的耗时）。
    Retry { reason: String, ms: Option<i64>, error: String },
    /// AI 回答（一轮的收口）：sql/row_count 取自 payload；知识库文本答没有 sql → null
    Answer { route: String, ms: Option<i64>, sql: Option<String>, row_count: Option<i64> },
    /// 产物生成（payload.artifact：深度模式固化出的报告页）
    Artifact { id: i64, title: String, preview_url: String },
}

#[derive(serde::Deserialize, Default)]
pub struct TraceQuery {
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `GET /api/chat/conv/{id}/trace` —— 会话事件流回放（只读；属主闸门在所有取数之前）。
pub async fn conv_trace(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<TraceQuery>,
) -> Result<Json<Value>, ApiErr> {
    let (login, _) = crate::resolve_identity(&st, &headers, &q.login_name, &q.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    // 🔴 属主闸门（fail-closed 内联，与 `api_ask`/`api_conv_msgs` 同一判据同一文案）：
    // 非属主 403；闸门自身读失败 500 拒 —— 拿不准属主时一律不放行。
    match crate::chat::conv_owner(st.owned.pool(), id).await {
        Ok(Some(owner)) if owner == login => {}
        Ok(_) => return Err(err(StatusCode::FORBIDDEN, "无权访问该会话")),
        Err(_) => return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "会话状态读取失败，请稍后重试",
        )),
    }
    let msgs = fetch_msgs(&st, id).await?;
    let failed = fetch_failed(&st, id).await?;
    let rounds = assemble(&msgs, &failed);
    Ok(Json(serde_json::json!({ "conv_id": id, "rounds": rounds })))
}

/// 会话消息（同 `chat::conv_msgs` 的读法：静态 SQL + `Row::get`）
async fn fetch_msgs(st: &AppState, conv_id: i64) -> Result<Vec<MsgRow>, ApiErr> {
    let rows = sqlx::query(MSGS_SQL)
        .bind(conv_id)
        .fetch_all(st.owned.pool())
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(rows
        .iter()
        .map(|r| MsgRow {
            id: r.get("id"),
            role: r.get("role"),
            question: r.get("question"),
            payload: r.get("payload"),
            at: r.get("created_at"),
        })
        .collect())
}

type FailedTuple = (String, String, i64, String, String, DateTime<Utc>);

/// 同会话失败尝试（`OwnedStore::fixed` 读，与 `quality_api` 同款先例）
async fn fetch_failed(st: &AppState, conv_id: i64) -> Result<Vec<FailedRow>, ApiErr> {
    let rows = st
        .owned
        .fixed(FAILED_SQL)
        .bind(conv_id.to_string())
        .fetch_all::<FailedTuple>()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(rows
        .into_iter()
        .map(|(question, route, elapsed_ms, status, error, at)| FailedRow {
            question,
            route,
            elapsed_ms,
            status,
            error,
            at,
        })
        .collect())
}

/// 事件组装（**纯函数**，单测锚定）：消息行配对成轮 + 失败行补轮，全体按时刻升序。
/// 轮内顺序 = question → route 链 →（repair）→ answer →（artifact），即事实发生序。
pub fn assemble(msgs: &[MsgRow], failed: &[FailedRow]) -> Vec<Round> {
    let mut timed: Vec<(DateTime<Utc>, Round)> = Vec::new();
    let mut pending: Option<&MsgRow> = None; // 等 ai 配对的 user 行
    for m in msgs {
        match m.role.as_str() {
            "user" => {
                // 上一个 user 没等到 ai（两次 save_msg 之间崩了）：留痕为 interrupted 轮
                if let Some(u) = pending.take() {
                    timed.push((u.at, interrupted_round(u)));
                }
                pending = Some(m);
            }
            "ai" => {
                let u = pending.take();
                timed.push((u.map_or(m.at, |u| u.at), answered_round(u, m)));
            }
            // role 只有 user/ai 两个写入点（api_ask / deep_api）；别的形态不猜不编
            _ => {}
        }
    }
    if let Some(u) = pending.take() {
        timed.push((u.at, interrupted_round(u)));
    }
    for f in failed {
        timed.push((f.at, failed_round(f)));
    }
    // 稳定排序：同时刻保持「消息轮在前、失败轮在后」的插入序
    timed.sort_by_key(|(at, _)| *at);
    timed.into_iter().map(|(_, r)| r).collect()
}

/// 配齐的一轮（user + ai）。降级纪律：payload 无 `steps`（老消息/知识库文本答）
/// → 只剩「问题→回答」两节点，路由链一节都不编。
fn answered_round(user: Option<&MsgRow>, ai: &MsgRow) -> Round {
    let (question, asked_at) = match user {
        Some(u) => (u.question.clone(), u.at),
        None => (String::new(), ai.at),
    };
    let mut events = vec![Event::Question {
        at: asked_at.to_rfc3339(),
        text: question.clone(),
    }];
    let p = ai.payload.as_ref();
    let r = p.map(result_of);
    let route = r
        .and_then(|r| r.get("route"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let elapsed = r
        .and_then(|r| r.get("elapsed_ms"))
        .and_then(Value::as_u64)
        .map(|ms| ms.min(i64::MAX as u64) as i64);
    // 路由链：steps 数组序 = 事实发生序（agent 按 router 表逐个 push）
    if let Some(steps) = r.and_then(|r| r.get("steps")).and_then(Value::as_array) {
        for s in steps {
            let (Some(stage), Some(kind)) = (
                s.get("stage").and_then(Value::as_str),
                s.get("kind").and_then(Value::as_str),
            ) else {
                continue; // 缺 stage/kind 的畸形条目跳过，不编
            };
            let ms = s
                .get("ms")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(i64::MAX as u64) as i64;
            events.push(Event::Route {
                stage: stage.to_string(),
                result: kind.to_string(),
                ms,
            });
        }
    }
    // 轮内自修重试：`+repair` 是 route 上唯一的留痕
    if route.contains("+repair") {
        events.push(Event::Retry {
            reason: "repair".into(),
            ms: None,
            error: String::new(),
        });
    }
    let sql = r
        .and_then(|r| r.get("sql"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let row_count = r.and_then(|r| r.get("row_count")).and_then(Value::as_i64);
    events.push(Event::Answer {
        route: route.clone(),
        ms: elapsed,
        sql,
        row_count,
    });
    // 产物（深度模式报告）挂在轮末 —— 它由本轮结果固化而来
    if let Some(a) = p.and_then(|p| p.get("artifact")) {
        if let (Some(id), Some(title)) = (
            a.get("id").and_then(Value::as_i64),
            a.get("title").and_then(Value::as_str),
        ) {
            events.push(Event::Artifact {
                id,
                title: title.to_string(),
                preview_url: a
                    .get("preview_url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Round {
        msg_id: Some(ai.id),
        question,
        at: asked_at.to_rfc3339(),
        status: "succeeded".into(),
        route,
        elapsed_ms: elapsed,
        events,
    }
}

/// user 行落库后 ai 行再没落 —— 这轮唯一的留痕方式
fn interrupted_round(u: &MsgRow) -> Round {
    Round {
        msg_id: None,
        question: u.question.clone(),
        at: u.at.to_rfc3339(),
        status: "interrupted".into(),
        route: String::new(),
        elapsed_ms: None,
        events: vec![Event::Question {
            at: u.at.to_rfc3339(),
            text: u.question.clone(),
        }],
    }
}

/// 硬失败轮（query_log 补回）：问题 → 失败原因。`api_ask` 失败早返回不落 chat.msg，
/// 不从 query_log 补的话「问了三遍才成功」这件事在时间线上不存在。
fn failed_round(f: &FailedRow) -> Round {
    Round {
        msg_id: None,
        question: f.question.clone(),
        at: f.at.to_rfc3339(),
        status: f.status.clone(),
        route: f.route.clone(),
        elapsed_ms: Some(f.elapsed_ms),
        events: vec![
            Event::Question {
                at: f.at.to_rfc3339(),
                text: f.question.clone(),
            },
            Event::Retry {
                reason: f.status.clone(),
                ms: Some(f.elapsed_ms),
                error: f.error.clone(),
            },
        ],
    }
}

/// ai payload 的「结果体」：深度模式落库形态是 `{"result":…,"artifact":…}` 包裹
/// （`deep_api::compose`），剥一层取 `result`；裸 `AskResult`/`Answer`（`api_ask`）原样。
/// 安全前提：两个裸形态都没有 `result` 键（AskResult/Answer 字段清单里没有它）。
fn result_of(payload: &Value) -> &Value {
    payload
        .get("result")
        .filter(|r| r.is_object())
        .unwrap_or(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn user_msg(id: i64, q: &str, secs: i64) -> MsgRow {
        MsgRow {
            id,
            role: "user".into(),
            question: q.into(),
            payload: None,
            at: at(secs),
        }
    }

    fn ai_msg(id: i64, payload: Option<Value>, secs: i64) -> MsgRow {
        MsgRow {
            id,
            role: "ai".into(),
            question: String::new(),
            payload,
            at: at(secs),
        }
    }

    /// 一轮的事件 kind 序列（断言顺序的速记）
    fn kinds(r: &Round) -> Vec<&str> {
        r.events
            .iter()
            .map(|e| match e {
                Event::Question { .. } => "question",
                Event::Route { .. } => "route",
                Event::Retry { .. } => "retry",
                Event::Answer { .. } => "answer",
                Event::Artifact { .. } => "artifact",
            })
            .collect()
    }

    /// 顺序 + 耗时透出：question → steps 数组序的 route 链 → answer；step.ms 与
    /// 整轮 elapsed_ms 原样进事件，一步不添一步不减
    #[test]
    fn events_follow_fact_order_and_ms_passthrough() {
        let payload = serde_json::json!({
            "sql": "SELECT sum(amt) FROM t", "row_count": 12, "route": "llm",
            "elapsed_ms": 1234,
            "steps": [
                {"stage": "semantic-cache", "kind": "miss", "ms": 3},
                {"stage": "direct-agg", "kind": "skip", "ms": 0},
                {"stage": "llm", "kind": "hit", "ms": 1180}
            ]
        });
        let rounds = assemble(&[user_msg(1, "本月销售额", 10), ai_msg(2, Some(payload), 20)], &[]);
        assert_eq!(rounds.len(), 1);
        let r = &rounds[0];
        assert_eq!(kinds(r), ["question", "route", "route", "route", "answer"]);
        let Event::Route { stage, result, ms } = &r.events[1] else { panic!() };
        assert_eq!((stage.as_str(), result.as_str(), *ms), ("semantic-cache", "miss", 3));
        let Event::Route { stage, result, ms } = &r.events[3] else { panic!() };
        assert_eq!((stage.as_str(), result.as_str(), *ms), ("llm", "hit", 1180));
        let Event::Answer { route, ms, sql, row_count } = &r.events[4] else { panic!() };
        assert_eq!((route.as_str(), *ms), ("llm", Some(1234)), "整轮耗时原样透出");
        assert_eq!(sql.as_deref(), Some("SELECT sum(amt) FROM t"));
        assert_eq!(*row_count, Some(12));
        assert_eq!((r.status.as_str(), r.route.as_str(), r.elapsed_ms), ("succeeded", "llm", Some(1234)));
        assert_eq!(r.msg_id, Some(2));
        assert_eq!(r.at, at(10).to_rfc3339(), "轮时间 = 问句时间");
    }

    /// 降级①：payload 无 steps（本字段上线前的老消息形态）→ 恰好「问题→回答」两节点
    #[test]
    fn payload_without_steps_degrades_to_two_nodes() {
        // 知识库文本答（`Answer` 序列化形态）本就没有 steps/sql —— 同一条降级路径
        let payload = serde_json::json!({
            "kind": "text", "route": "knowledge", "elapsed_ms": 88, "text": "报销上限 5000 元"
        });
        let rounds = assemble(&[user_msg(1, "报销上限", 10), ai_msg(2, Some(payload), 20)], &[]);
        let r = &rounds[0];
        assert_eq!(kinds(r), ["question", "answer"], "无 steps 一轮只剩两节点");
        let Event::Answer { route, ms, sql, row_count } = &r.events[1] else { panic!() };
        assert_eq!((route.as_str(), *ms), ("knowledge", Some(88)), "elapsed_ms 仍透出");
        assert_eq!((sql, row_count), (&None, &None), "文本答没有 sql/row_count 不编");
        let Event::Question { text, .. } = &r.events[0] else { panic!() };
        assert_eq!(text, "报销上限");
    }

    /// 降级②：payload 整个缺失（ai 行 NULL）→ 仍是两节点，answer 字段全空不编
    #[test]
    fn null_payload_still_two_nodes() {
        let rounds = assemble(&[user_msg(1, "q", 10), ai_msg(2, None, 20)], &[]);
        let r = &rounds[0];
        assert_eq!(kinds(r), ["question", "answer"]);
        let Event::Answer { route, ms, sql, row_count } = &r.events[1] else { panic!() };
        assert_eq!((route.as_str(), *ms), ("", None));
        assert_eq!((sql, row_count), (&None, &None));
    }

    /// 轮内自修重试：route 带 `+repair` → retry 事件钉在 answer 之前；普通路由没有
    #[test]
    fn repair_route_emits_retry_before_answer() {
        let payload = serde_json::json!({
            "sql": "SELECT 1", "row_count": 1, "route": "llm+repair", "elapsed_ms": 900,
            "steps": [{"stage": "llm", "kind": "hit", "ms": 890}]
        });
        let rounds = assemble(&[user_msg(1, "q", 10), ai_msg(2, Some(payload), 20)], &[]);
        let r = &rounds[0];
        assert_eq!(kinds(r), ["question", "route", "retry", "answer"]);
        let Event::Retry { reason, ms, error } = &r.events[2] else { panic!() };
        assert_eq!((reason.as_str(), *ms, error.as_str()), ("repair", None, ""), "自修没有独立耗时");
        // 对照：普通 llm 命中不多出 retry
        let plain = serde_json::json!({"route": "llm", "elapsed_ms": 1, "steps": []});
        let rounds = assemble(&[user_msg(1, "q", 10), ai_msg(2, Some(plain), 20)], &[]);
        assert!(!rounds[0].events.iter().any(|e| matches!(e, Event::Retry { .. })));
    }

    /// 深度模式包裹形态：剥 `result` 取事件，`artifact` 键挂轮末
    #[test]
    fn deep_wrapper_unwrapped_and_artifact_last() {
        let payload = serde_json::json!({
            "result": {
                "sql": "SELECT 1", "row_count": 3, "route": "compound", "elapsed_ms": 5000,
                "steps": [{"stage": "llm", "kind": "hit", "ms": 4900}]
            },
            "artifact": {"id": 5, "title": "本月经营报告", "preview_url": "/api/artifact/5/view"},
            "page": {"kind": "daily"}
        });
        let rounds = assemble(&[user_msg(1, "深度分析本月", 10), ai_msg(2, Some(payload), 20)], &[]);
        let r = &rounds[0];
        assert_eq!(kinds(r), ["question", "route", "answer", "artifact"]);
        assert_eq!((r.route.as_str(), r.elapsed_ms), ("compound", Some(5000)), "剥包裹后读 result");
        let Event::Artifact { id, title, preview_url } = &r.events[3] else { panic!() };
        assert_eq!((*id, title.as_str(), preview_url.as_str()), (5, "本月经营报告", "/api/artifact/5/view"));
    }

    /// query_log 失败行 → 独立的失败轮，按时刻插进消息轮之间；状态与耗时原样透出
    #[test]
    fn failed_attempts_become_rounds_in_time_order() {
        let msgs = [user_msg(1, "本月销售额", 30), ai_msg(2, Some(serde_json::json!({
            "route": "llm", "elapsed_ms": 100
        })), 40)];
        let failed = [
            FailedRow {
                question: "本月销售额".into(), route: String::new(), elapsed_ms: 30_000,
                status: "timeout".into(), error: "取数超时 [dms]".into(), at: at(10),
            },
            FailedRow {
                question: "本月销售额".into(), route: String::new(), elapsed_ms: 50,
                status: "failed".into(), error: "SQL 执行报错".into(), at: at(20),
            },
        ];
        let rounds = assemble(&msgs, &failed);
        assert_eq!(rounds.len(), 3);
        assert_eq!(rounds[0].status, "timeout", "按时刻升序，最早的失败轮在最前");
        assert_eq!(rounds[2].status, "succeeded");
        let f = &rounds[0];
        assert_eq!(f.msg_id, None, "失败轮没有消息行");
        assert_eq!(kinds(f), ["question", "retry"]);
        let Event::Retry { reason, ms, error } = &f.events[1] else { panic!() };
        assert_eq!((reason.as_str(), *ms, error.as_str()), ("timeout", Some(30_000), "取数超时 [dms]"));
        assert_eq!(f.elapsed_ms, Some(30_000), "失败尝试的耗时透出到轮级");
    }

    /// user 行没等到 ai（两次 save_msg 之间崩了）→ interrupted 轮，只有 question 节点
    #[test]
    fn orphan_user_msg_is_interrupted() {
        let rounds = assemble(&[user_msg(1, "问了一半", 10)], &[]);
        assert_eq!(rounds.len(), 1);
        let r = &rounds[0];
        assert_eq!((r.status.as_str(), r.msg_id), ("interrupted", None));
        assert_eq!(kinds(r), ["question"]);
        // 两个连续 user：前一个 interrupted，后一个正常配对
        let rounds = assemble(
            &[user_msg(1, "崩了", 10), user_msg(2, "重来", 20), ai_msg(3, None, 30)],
            &[],
        );
        assert_eq!(rounds[0].status, "interrupted");
        assert_eq!(rounds[1].status, "succeeded");
        assert_eq!(rounds[1].question, "重来");
    }

    /// 事件 JSON 形状：`tag = "kind"` 是前端分派契约，rename 后五值必须稳定
    #[test]
    fn event_json_shape_uses_kind_tag() {
        let v = serde_json::to_value(Event::Route {
            stage: "llm".into(), result: "hit".into(), ms: 7,
        }).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "route", "stage": "llm", "result": "hit", "ms": 7}));
        let v = serde_json::to_value(Event::Retry {
            reason: "repair".into(), ms: None, error: String::new(),
        }).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "retry", "reason": "repair", "ms": null, "error": ""}));
        let v = serde_json::to_value(Event::Artifact {
            id: 5, title: "t".into(), preview_url: "u".into(),
        }).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "artifact", "id": 5, "title": "t", "preview_url": "u"}));
    }

    /// 🔴 属主闸门锚点：`conv_owner` 必须在任何取数之前；非属主 403 与闸门读失败 500
    /// 两个拒绝分支都在（fail-closed：拿不准属主时一律不放行）
    #[test]
    fn owner_gate_precedes_any_data_read() {
        let src = include_str!("trace_api.rs");
        let body = src.split("pub async fn conv_trace").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let auth = body.find("crate::resolve_identity").expect("身份解析不在了");
        let gate = body.find("crate::chat::conv_owner").expect("属主闸门不在了");
        let read = body.find("fetch_msgs").expect("取数不在了");
        assert!(auth < gate && gate < read, "顺序必须是 身份 → 属主闸门 → 取数: {body}");
        assert!(body.contains("Ok(Some(owner)) if owner == login"), "属主判据: {body}");
        assert!(body.contains("无权访问该会话"), "403 分支: {body}");
        assert!(body.contains("会话状态读取失败"), "闸门读失败的 500 分支: {body}");
    }

    /// 只读锚点：两条 SQL 都是单句 SELECT —— 本端点不下推任何写（任务纪律①）
    #[test]
    fn sqls_are_read_only_selects() {
        for sql in [MSGS_SQL, FAILED_SQL] {
            assert!(sql.trim_start().starts_with("SELECT"), "{sql}");
            assert!(!sql.contains(';'), "多句拼接 = 下推通道: {sql}");
            for kw in ["INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "GRANT", "TRUNCATE"] {
                assert!(!sql.contains(kw), "写关键词 {kw}: {sql}");
            }
        }
        // 失败行的口径：空串老行按 error 有无区分失败/成功（与 query_log.rs 的列注释同义）
        assert!(FAILED_SQL.contains("status IN ('blocked','failed','timeout')"), "{FAILED_SQL}");
        assert!(FAILED_SQL.contains("(status = '' AND error <> '')"), "老行折算: {FAILED_SQL}");
        assert!(FAILED_SQL.contains("conv_id = $1"), "会话过滤必须内联在 SQL 里: {FAILED_SQL}");
        assert!(MSGS_SQL.contains("WHERE conv_id = $1"), "{MSGS_SQL}");
    }

    /// 端点契约锚点：文件头写清路径、响应键与集成接线行（父代理按它注册路由）
    #[test]
    fn endpoint_contract_is_written_in_header() {
        let src = include_str!("trace_api.rs");
        let head = src.split("\nuse ").next().unwrap();
        assert!(head.contains("GET /api/chat/conv/{id}/trace"), "{head}");
        assert!(head.contains(r#".route("/api/chat/conv/{id}/trace", get(trace_api::conv_trace))"#), "接线行: {head}");
        assert!(head.contains("\"rounds\"") && head.contains("\"events\""), "响应键: {head}");
        for h in ["pub async fn conv_trace", "pub fn assemble"] {
            assert!(src.contains(h), "{h}");
        }
    }
}
