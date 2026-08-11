//! 【A10】Trace 时间线（DataFoundry trace-dag 的对应物，`docs/research/datafoundry.json`）：
//! 一会话一事件流回放 —— 用户提问 → 路由链（命中/未命中/跳过）→（自修/失败重试）→
//! AI 回答 → 产物生成，按时间序组装。
//!
//! ## 端点契约（路由已在 `main.rs` 注册，本文件只供 handler 与纯函数）
//!
//! ### `GET /api/chat/conv/{id}/trace?login_name=&role_code=`
//! 已接线（`main.rs` 的 Router 处，`/api/conv/{id}` 那一组旁）：
//! `.route("/api/chat/conv/{id}/trace", get(trace_api::conv_trace))`
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
//!     "payload_truncated": false,
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
//!   可归属耗时（轮内自修没有独立计时，它含在 answer 的 ms 里）。例外：`Route.ms` 是
//!   i64 无法为 null，steps 条目缺 `ms` 时记 0 —— 「0ms」与「无计时」对前端不可区分。
//! - **降级**：payload 无 `steps`（本字段上线前的老消息 / 知识库文本答）→ 一轮只剩
//!   「问题→回答」两节点，路由链一节都不编。
//!
//! ### 【性能④】列表态的 payload 投影 + 截断标记
//! 事件组装只需要 `route/steps/elapsed_ms/sql/row_count/artifact` 六个键，而 ai 行的全量
//! payload 带着整份结果行（`AskResult.rows`，单条几十 KB 起）—— 逐行全拉是这个端点唯一的
//! 体量来源。`MSGS_SQL` 因此在**库侧**只投出这六键（`jsonb_build_object`；缺键投成 JSON null，
//! 与「键缺席」在 `assemble` 里走同一条 `and_then` 降级链，事件输出与全量版**逐字相同**），
//! 并按 3KB 阈值给每轮带回 `payload_truncated` 标记。要全文的按 `msg_id` 走下面的单条端点。
//! 🔴 投影形状 = `assemble` 的读取面：`assemble` 哪天多读一个键，`MSGS_SQL` 必须一起加。
//!
//! ### `GET /api/chat/msg/{msg_id}/payload?login_name=&role_code=`
//! 单条消息的**全量** payload（列表态只带六键投影，原文走这里）。已接线：
//! `.route("/api/chat/msg/{msg_id}/payload", get(trace_api::msg_payload))`
//! 身份与属主闸门同 `conv_trace`（401/403/500 同一判据同一文案）；msg 不存在 404。
//! 口径取舍：`conv_trace` 对「会话不存在」与「非属主」同回 403（防会话 id 枚举，
//! 见 `ensure_owner`），这里 msg 存在性已在属主闸门前由 404 透出 —— msg_id 全库唯一、
//! 不含会话结构信息，且属主校验紧随其后，枚举收益为零，故从简 404。
//! 响应：`{ "msg_id": 456, "conv_id": 123, "payload": {…} }`（user 行 payload 为 null）。
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

use crate::admin_api::{err, ApiErr};
use crate::AppState;

/// 会话全部消息（id 升序 = 落库序）。与 `chat::conv_msgs` 同表同序，多取 id/created_at：
/// 配对成轮与跨源对时都要。
///
/// 【性能④】payload **库侧投影**成事件组装要的六键（详见文件头「列表态的 payload 投影」）：
/// 全量 payload 带整份结果行（`AskResult.rows`），逐行全拉曾把这个端点顶到秒级。
/// `r` = `result_of` 的 SQL 版（`result` 键是对象才剥一层，与 Rust 侧逐字同义）；
/// 内层保留 `payload` 本体只为取 `artifact`（它挂在包裹层，不在 `result` 里）。
/// `payload_truncated` = 原 payload 超 3KB 的标记（全文走 `/api/chat/msg/{id}/payload`）。
/// user 行 payload 本就是 NULL：外层 CASE 保持 NULL 透出（不包成全 null 字段的对象），
/// 与 `MsgRow.payload: Option<Value>` 及「user 行 payload 为 null」的契约语义对齐。
const MSGS_SQL: &str =
    "SELECT id, role, question, created_at, payload_truncated, payload FROM (
       SELECT id, role, question, created_at,
              COALESCE(octet_length(payload::text) > 3072, false) AS payload_truncated,
              CASE WHEN payload IS NULL THEN NULL ELSE jsonb_build_object(
                'route',      r->'route',
                'elapsed_ms', r->'elapsed_ms',
                'steps',      r->'steps',
                'sql',        r->'sql',
                'row_count',  r->'row_count',
                'artifact',   payload->'artifact'
              ) END AS payload
       FROM (
         SELECT id, role, question, created_at, payload,
                CASE WHEN jsonb_typeof(payload->'result') = 'object' THEN payload->'result' ELSE payload END AS r
         FROM chat.msg
         WHERE conv_id = $1
       ) m
     ) t
     ORDER BY id";

/// 单条消息的会话归属（属主闸门的入参）。payload **不许**在这条里一起取 ——
/// 「任何取数都在闸门之后」（见 `conv_trace` 头注），全文在闸门通过后由 `MSG_PAYLOAD_SQL` 取。
const MSG_CONV_SQL: &str = "SELECT conv_id FROM chat.msg WHERE id = $1";

/// 单条消息的全量 payload（`msg_payload` 专用，只在属主闸门通过后执行）。
const MSG_PAYLOAD_SQL: &str = "SELECT payload FROM chat.msg WHERE id = $1";

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
    /// ai 行的结果载荷（**列表态为六键投影**，见 `MSGS_SQL` 头注；全文走 `msg_payload`）
    pub payload: Option<Value>,
    pub at: DateTime<Utc>,
    /// 原 payload 是否超列表态阈值（3KB）：true = 完整 payload 可按 `id` 单条取
    pub payload_truncated: bool,
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
    /// ai 行原 payload 是否超列表态阈值（3KB）：true = 完整 payload 按 `msg_id` 单条取
    ///（`GET /api/chat/msg/{msg_id}/payload`）；无 ai 行的轮（interrupted/失败）恒 false
    pub payload_truncated: bool,
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

/// 🔴 属主闸门（fail-closed，与 `api_ask`/`api_conv_msgs` 同一判据同一文案）：
/// 非属主 403；闸门自身读失败 500 拒 —— 拿不准属主时一律不放行。
/// 口径取舍：`Ok(None)`（conv 不存在）与「非属主」同走 403 —— 会话存在性不透给
/// 无权限的人（防 id 枚举）；`msg_payload` 的 404 差异见文件头该端点一节。
async fn ensure_owner(st: &AppState, conv_id: i64, login: &str) -> Result<(), ApiErr> {
    match crate::chat::conv_owner(st.owned.pool(), conv_id).await {
        Ok(Some(owner)) if owner == login => Ok(()),
        Ok(_) => Err(err(StatusCode::FORBIDDEN, "无权访问该会话")),
        Err(_) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "会话状态读取失败，请稍后重试",
        )),
    }
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
    ensure_owner(&st, id, &login).await?;
    // 两路取数互不依赖，并行跑；任一失败即整体 500（错误文案两处一致，无优先级歧义）
    let (msgs, failed) = tokio::try_join!(fetch_msgs(&st, id), fetch_failed(&st, id))?;
    let rounds = assemble(&msgs, &failed);
    Ok(Json(serde_json::json!({ "conv_id": id, "rounds": rounds })))
}

/// `GET /api/chat/msg/{msg_id}/payload` —— 单条消息的**全量** payload（只读）。
/// 列表态（`conv_trace`）只带六键投影 + 截断标记，原文走这里；契约见文件头。
pub async fn msg_payload(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(msg_id): Path<i64>,
    Query(q): Query<TraceQuery>,
) -> Result<Json<Value>, ApiErr> {
    let (login, _) = crate::resolve_identity(&st, &headers, &q.login_name, &q.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    // 先取会话归属（属主闸门的入参）；payload 在闸门通过后才取 —— 「任何取数都在闸门之后」
    let conv_id: i64 = sqlx::query(MSG_CONV_SQL)
        .bind(msg_id)
        .fetch_optional(st.owned.pool())
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "消息不存在"))?
        .try_get("conv_id")
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    ensure_owner(&st, conv_id, &login).await?;
    let payload: Option<Value> = sqlx::query(MSG_PAYLOAD_SQL)
        .bind(msg_id)
        .fetch_optional(st.owned.pool())
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .map(|r| r.try_get("payload"))
        .transpose()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .flatten();
    Ok(Json(serde_json::json!({ "msg_id": msg_id, "conv_id": conv_id, "payload": payload })))
}

/// 会话消息（同 `chat::conv_msgs` 的读法：静态 SQL + 按名列取）。
/// 解码用 `try_get`：列缺失/类型漂移报干净 500，不让 worker panic。
async fn fetch_msgs(st: &AppState, conv_id: i64) -> Result<Vec<MsgRow>, ApiErr> {
    let rows = sqlx::query(MSGS_SQL)
        .bind(conv_id)
        .fetch_all(st.owned.pool())
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    rows.iter()
        .map(|r| -> Result<MsgRow, ApiErr> {
            Ok(MsgRow {
                id: r.try_get("id").map_err(decode_err)?,
                role: r.try_get("role").map_err(decode_err)?,
                question: r.try_get("question").map_err(decode_err)?,
                payload: r.try_get("payload").map_err(decode_err)?,
                at: r.try_get("created_at").map_err(decode_err)?,
                payload_truncated: r.try_get("payload_truncated").map_err(decode_err)?,
            })
        })
        .collect()
}

/// 行解码失败 → 干净 500（列名是静态 SQL 写死的，出现失败 = 库 schema 已漂）
fn decode_err(e: sqlx::Error) -> ApiErr {
    err(StatusCode::INTERNAL_SERVER_ERROR, e)
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
    let mut timed: Vec<(DateTime<Utc>, Round)> = Vec::with_capacity(msgs.len() + failed.len());
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
        Some(u) => (u.question.clone(), u.at.to_rfc3339()),
        None => (String::new(), ai.at.to_rfc3339()),
    };
    let mut events = vec![Event::Question {
        at: asked_at.clone(),
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
        .map(clamp_ms);
    // 路由链：steps 数组序 = 事实发生序（agent 按 router 表逐个 push）
    if let Some(steps) = r.and_then(|r| r.get("steps")).and_then(Value::as_array) {
        for s in steps {
            let (Some(stage), Some(kind)) = (
                s.get("stage").and_then(Value::as_str),
                s.get("kind").and_then(Value::as_str),
            ) else {
                // 畸形条目：steps 是自己落库的，出现即上游形态已变 —— 留痕后跳过
                tracing::debug!(step = %s, "trace steps 条目缺 stage/kind，跳过");
                continue;
            };
            let ms = s
                .get("ms")
                .and_then(Value::as_u64)
                .map(clamp_ms)
                .unwrap_or(0); // 缺 ms 记 0（Route.ms 是 i64 无法为 null，见文件头）
            events.push(Event::Route {
                stage: stage.to_string(),
                result: kind.to_string(),
                ms,
            });
        }
    }
    // 轮内自修重试：`+repair` 是 route 上唯一的留痕（按 `+` 分段整词匹配，
    // 防未来 `xx+repairy` 类路由值子串误命中）
    if route.split('+').any(|seg| seg == "repair") {
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
        at: asked_at,
        status: "succeeded".into(),
        route,
        elapsed_ms: elapsed,
        payload_truncated: ai.payload_truncated,
        events,
    }
}

/// user 行落库后 ai 行再没落 —— 这轮唯一的留痕方式
fn interrupted_round(u: &MsgRow) -> Round {
    let at = u.at.to_rfc3339();
    let question = u.question.clone();
    Round {
        msg_id: None,
        question: question.clone(),
        at: at.clone(),
        status: "interrupted".into(),
        route: String::new(),
        elapsed_ms: None,
        payload_truncated: false, // 没有 ai 行就没有 payload 可言
        events: vec![Event::Question { at, text: question }],
    }
}

/// 硬失败轮（query_log 补回）：问题 → 失败原因。`api_ask` 失败早返回不落 chat.msg，
/// 不从 query_log 补的话「问了三遍才成功」这件事在时间线上不存在。
fn failed_round(f: &FailedRow) -> Round {
    let at = f.at.to_rfc3339();
    let question = f.question.clone();
    let status = f.status.clone();
    Round {
        msg_id: None,
        question: question.clone(),
        at: at.clone(),
        status: status.clone(),
        route: f.route.clone(),
        elapsed_ms: Some(f.elapsed_ms),
        payload_truncated: false, // 失败轮不落 chat.msg，没有 payload 可言
        events: vec![
            Event::Question { at, text: question },
            Event::Retry {
                reason: status,
                ms: Some(f.elapsed_ms),
                error: f.error.clone(),
            },
        ],
    }
}

/// payload/query_log 的 u64 耗时 → i64 透出（超 `i64::MAX` 折顶，两处同一把尺）
fn clamp_ms(ms: u64) -> i64 {
    ms.min(i64::MAX as u64) as i64
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
            payload_truncated: false,
        }
    }

    fn ai_msg(id: i64, payload: Option<Value>, secs: i64) -> MsgRow {
        ai_msg_t(id, payload, secs, false)
    }

    /// 带截断标记的 ai 行（【性能④】：原 payload 超 3KB 时列表态只带六键投影）
    fn ai_msg_t(id: i64, payload: Option<Value>, secs: i64, payload_truncated: bool) -> MsgRow {
        MsgRow {
            id,
            role: "ai".into(),
            question: String::new(),
            payload,
            at: at(secs),
            payload_truncated,
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

    /// 🔴 属主闸门锚点：`ensure_owner` 必须在任何取数之前；非属主 403 与闸门读失败 500
    /// 两个拒绝分支都在（fail-closed：拿不准属主时一律不放行）。两 handler 共用同一 helper，
    /// 判据/文案只有一份（抽出处即本测试钉住处）。
    #[test]
    fn owner_gate_precedes_any_data_read() {
        let src = include_str!("trace_api.rs");
        let body = src.split("pub async fn conv_trace").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let auth = body.find("crate::resolve_identity").expect("身份解析不在了");
        let gate = body.find("ensure_owner").expect("属主闸门不在了");
        let read = body.find("fetch_msgs").expect("取数不在了");
        assert!(auth < gate && gate < read, "顺序必须是 身份 → 属主闸门 → 取数: {body}");
        let helper = src.split("async fn ensure_owner").nth(1).unwrap();
        let helper = helper.split("\n}\n").next().unwrap();
        assert!(helper.contains("crate::chat::conv_owner"), "闸门判据来源: {helper}");
        assert!(helper.contains("Ok(Some(owner)) if owner == login"), "属主判据: {helper}");
        assert!(helper.contains("无权访问该会话"), "403 分支: {helper}");
        assert!(helper.contains("会话状态读取失败"), "闸门读失败的 500 分支: {helper}");
    }

    /// 只读锚点：两条 SQL 都是单句 SELECT —— 本端点不下推任何写（任务纪律①）
    #[test]
    fn sqls_are_read_only_selects() {
        for sql in [MSGS_SQL, FAILED_SQL, MSG_CONV_SQL, MSG_PAYLOAD_SQL] {
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
        // 【性能④】列表态必须是库侧投影 + 3KB 截断标记：全量 payload（含整份结果行）
        // 逐行全拉曾把这个端点顶到秒级；`result` 剥层与 Rust 侧 `result_of` 逐字同义
        assert!(MSGS_SQL.contains("jsonb_build_object"), "库侧投影没了: {MSGS_SQL}");
        assert!(MSGS_SQL.contains("3072"), "3KB 截断标记没了: {MSGS_SQL}");
        assert!(MSGS_SQL.contains("jsonb_typeof(payload->'result') = 'object'"), "剥层判据: {MSGS_SQL}");
        // user 行 payload 是 NULL：外层 CASE 保 NULL 透出，不许包成全 null 字段的对象
        assert!(
            MSGS_SQL.contains("CASE WHEN payload IS NULL THEN NULL ELSE jsonb_build_object"),
            "NULL payload 的 CASE 保护没了: {MSGS_SQL}"
        );
        assert!(MSG_PAYLOAD_SQL.contains("WHERE id = $1"), "单条全文必须按主键取: {MSG_PAYLOAD_SQL}");
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

    /// 【性能④】截断标记随轮透出：ai 行超阈值 → 该轮 true 且事件一字不受标记影响；
    /// interrupted / 失败轮（没有 ai 行）恒 false。
    #[test]
    fn payload_truncated_marker_follows_the_ai_row() {
        let payload = serde_json::json!({"route": "llm", "elapsed_ms": 5});
        let rounds = assemble(&[user_msg(1, "q", 10), ai_msg_t(2, Some(payload), 20, true)], &[]);
        assert!(rounds[0].payload_truncated, "ai 行超阈值必须带出标记");
        assert_eq!(kinds(&rounds[0]), ["question", "answer"], "标记只换传输形态，不换事件");
        assert!(!assemble(&[user_msg(1, "q", 10)], &[])[0].payload_truncated, "interrupted 轮恒 false");
        let failed = [FailedRow {
            question: "q".into(), route: String::new(), elapsed_ms: 1,
            status: "failed".into(), error: "e".into(), at: at(5),
        }];
        assert!(!assemble(&[], &failed)[0].payload_truncated, "失败轮恒 false");
    }

    /// 【性能④】单条全文端点：契约写在文件头；闸门顺序 = 身份 → 取 conv_id → 属主闸门 →
    /// 取 payload（payload 在闸门通过后才离开库 —— 与 `conv_trace` 同一条 fail-closed 纪律）；
    /// 路由已在 `main.rs` 注册（见文件头接线行）。
    #[test]
    fn msg_payload_endpoint_contract_and_gate_order() {
        let src = include_str!("trace_api.rs");
        let head = src.split("\nuse ").next().unwrap();
        assert!(head.contains("GET /api/chat/msg/{msg_id}/payload"), "契约路径: {head}");
        assert!(head.contains(r#".route("/api/chat/msg/{msg_id}/payload", get(trace_api::msg_payload))"#), "接线行: {head}");
        let body = src
            .split("pub async fn msg_payload")
            .nth(1)
            .expect("handler 没了")
            .split("\n}\n")
            .next()
            .unwrap();
        let auth = body.find("crate::resolve_identity").expect("身份解析不在了");
        let conv = body.find("MSG_CONV_SQL").expect("conv_id 读取不在了");
        let gate = body.find("ensure_owner").expect("属主闸门不在了");
        let pay = body.find("MSG_PAYLOAD_SQL").expect("payload 读取不在了");
        assert!(
            auth < conv && conv < gate && gate < pay,
            "顺序必须是 身份 → conv_id → 属主闸门 → payload: {body}"
        );
        assert!(body.contains("消息不存在"), "404 分支: {body}");
        let helper = src.split("async fn ensure_owner").nth(1).unwrap();
        let helper = helper.split("\n}\n").next().unwrap();
        assert!(helper.contains("无权访问该会话"), "403 分支: {helper}");
        assert!(helper.contains("会话状态读取失败"), "闸门读失败的 500 分支: {helper}");
    }

    /// u64→i64 夹取：折顶一把尺，两处共用；`+repair` 按分段整词匹配，子串形似不误命中
    #[test]
    fn clamp_ms_and_repair_segment_match() {
        assert_eq!(clamp_ms(0), 0);
        assert_eq!(clamp_ms(1234), 1234);
        assert_eq!(clamp_ms(u64::MAX), i64::MAX, "超 i64::MAX 折顶");
        // 分段整词：命中 repair 段；`+repairy` 这类未来路由值不命中
        let repair = serde_json::json!({"route": "llm+repair", "elapsed_ms": 1, "steps": []});
        let rounds = assemble(&[user_msg(1, "q", 10), ai_msg(2, Some(repair), 20)], &[]);
        assert!(rounds[0].events.iter().any(|e| matches!(e, Event::Retry { .. })));
        let lookalike = serde_json::json!({"route": "llm+repairy", "elapsed_ms": 1, "steps": []});
        let rounds = assemble(&[user_msg(1, "q", 10), ai_msg(2, Some(lookalike), 20)], &[]);
        assert!(
            !rounds[0].events.iter().any(|e| matches!(e, Event::Retry { .. })),
            "子串形似的段不许误命中 retry"
        );
    }

    /// 并行与解码锚点：两路取数 `try_join!` 并行；行解码走 `try_get`（列漂移报 500 不 panic）
    #[test]
    fn parallel_fetch_and_try_get_anchors() {
        let src = include_str!("trace_api.rs");
        let body = src.split("pub async fn conv_trace").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("tokio::try_join!"), "两路取数必须并行: {body}");
        let fetch = src.split("async fn fetch_msgs").nth(1).unwrap();
        let fetch = fetch.split("\n}\n").next().unwrap();
        assert!(fetch.contains("try_get"), "行解码必须 try_get: {fetch}");
        assert!(!fetch.contains(".get(\""), "Row::get 会 panic，不许回潮: {fetch}");
    }
}
