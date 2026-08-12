//! 多会话持久化：一个会话(conv)含多轮问答(msg)，按登录用户归属。
//! 对齐 SuperSonic conversation 模型。修复「一问一会话」——同会话内多轮归一条，侧栏列会话。
//!
//! 【分支会话】`POST /api/chat/conv/{id}/branch` —— 存储函数是本文件的 `branch_conv`，
//! 路由行与 handler（`api_conv_branch`）已在 main.rs 接线。
//! handler 契约：Json body `{ login_name?: String, from_seq?: i64 }`（login_name 仅供
//! insecure_login_fallback 回退，同 ConvQuery；from_seq 缺省=整条会话）；
//! resolve_identity 取不出登录人 → 401 `{"error":"未认证"}`；
//! `chat::branch_conv(pool, id, &login, from_seq)` 返回 `Ok(None)`（非属主/不存在）
//! → 403 `{"error":"无权访问该会话"}`；`Ok(Some((conv_id, copied)))`
//! → 200 `{"conv_id": conv_id, "copied": copied}`；`Err(_)` → 500 通用文案
//! `{"error":"分支会话失败，请稍后重试"}`（原始错误只进服务端 warn，不外泄给客户端）。
//!
//! 【Y5 steer 插话】`POST /api/chat/conv/{id}/steer` —— handler 是本文件的 `api_conv_steer`
//! （与 branch 不同：本 handler 自持身份解析，用 `auth::resolve_identity_dual` 双通道 —
//! 这也是【D10】REST API key 在 chat 面的落地端点），路由行已在 main.rs 接线。
//! 语义：会话属主校验（非属主 403）；仅运行中可 steer（信箱在 `dms_agent::run`，
//! 否则 409）；内容剥控制字符 + 500 字护栏（全剥空 400）；队列满 429。

use serde_json::Value;
use sqlx::{PgPool, Row};

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use crate::{auth, AppState};

/// 新会话默认标题。UPDATE 谓词「仍处默认标题」用 bind 取它；DDL 的 DEFAULT 是 SQL 文本插不进
/// 常量，由单测钉住两处一致（改一忘一 = 标题永不刷新）。
pub const DEFAULT_TITLE: &str = "新会话";
/// 消息角色取值（本文件与 8+ 调用点共用，别再造 `"user"`/`"ai"` 字面量）
pub const ROLE_USER: &str = "user";
pub const ROLE_AI: &str = "ai";
/// 侧栏会话条数上限
const MAX_LIST_CONVS: i64 = 100;
/// 单会话消息回放上限（防御性：超长会话不全量进内存整包序列化）
const MAX_CONV_MSGS: i64 = 1000;
/// X-API-Key 头名常量（`&str` 查询每请求要做一次大小写不敏感匹配，常量复用）
const X_API_KEY: axum::http::HeaderName = axum::http::HeaderName::from_static("x-api-key");

const DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS chat;
CREATE TABLE IF NOT EXISTS chat.conv(
  id bigserial PRIMARY KEY,
  login_name text NOT NULL,
  title text NOT NULL DEFAULT '新会话',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_conv_login ON chat.conv(login_name, updated_at DESC);
CREATE TABLE IF NOT EXISTS chat.msg(
  id bigserial PRIMARY KEY,
  conv_id bigint NOT NULL REFERENCES chat.conv(id) ON DELETE CASCADE,
  role text NOT NULL,
  question text NOT NULL DEFAULT '',
  payload jsonb,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_msg_conv ON chat.msg(conv_id, id);
"#;

/// 建表。走 `crate::run_ddl`（按分号逐句切 + 单事务；split 纪律见该函数文档）。
pub async fn migrate(pg: &PgPool) -> anyhow::Result<()> {
    crate::run_ddl(pg, DDL).await
}

/// 会话列表（按登录用户，最近更新在前；updated_at 同值时按 id 兜底 —— PG 不保证同值稳定序）
pub async fn list_convs(pg: &PgPool, login: &str) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT id, title, to_char(updated_at,'MM-DD HH24:MI') AS t FROM chat.conv
         WHERE login_name = $1 ORDER BY updated_at DESC, id DESC LIMIT $2",
    )
    .bind(login)
    .bind(MAX_LIST_CONVS)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<i64, _>("id"),
                "title": r.get::<String, _>("title"),
                "time": r.get::<String, _>("t"),
            })
        })
        .collect())
}

pub async fn new_conv(pg: &PgPool, login: &str) -> anyhow::Result<i64> {
    let id: i64 = sqlx::query_scalar("INSERT INTO chat.conv(login_name) VALUES ($1) RETURNING id")
        .bind(login)
        .fetch_one(pg)
        .await?;
    Ok(id)
}

/// 会话消息回放（按 id 升序，上限 `MAX_CONV_MSGS` 条防御截断）
///
/// 🔴 本函数**不做属主过滤**：调用前必须先过 `ensure_owner`（属主闸），漏一步就是越权读。
pub async fn conv_msgs(pg: &PgPool, conv_id: i64) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT role, question, payload FROM chat.msg WHERE conv_id = $1 ORDER BY id LIMIT $2",
    )
    .bind(conv_id)
    .bind(MAX_CONV_MSGS)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "role": r.get::<String, _>("role"),
                "question": r.get::<String, _>("question"),
                "result": r.get::<Option<Value>, _>("payload"),
            })
        })
        .collect())
}

/// 存一条消息（同一事务：INSERT 与 updated_at 刷新合并成一条 CTE，省一次往返）；
/// 首个仍处默认标题时的 user 消息顺手把标题设为问题前 18 字（trim + 剥 `\r\n\t`，空标题不设）。
pub async fn save_msg(
    pg: &PgPool,
    conv_id: i64,
    role: &str,
    question: &str,
    payload: Option<&Value>,
) -> anyhow::Result<()> {
    let mut tx = pg.begin().await?;
    // INSERT 与 updated_at 刷新合并成一条 CTE：少一次 DB 往返，且同事务不落
    // 「消息进了、侧栏排序/标题没刷」的中间态（原来三条独立 SQL 各自上岸）
    sqlx::query(
        "WITH m AS (INSERT INTO chat.msg(conv_id, role, question, payload) VALUES ($1,$2,$3,$4))
         UPDATE chat.conv SET updated_at = now() WHERE id = $1",
    )
    .bind(conv_id)
    .bind(role)
    .bind(question)
    .bind(payload)
    .execute(&mut *tx)
    .await?;
    if role == ROLE_USER {
        // 仅当仍是默认标题时设标题；空标题（空问句/全是空白控制字符）不设，
        // 否则会把标题刷成 ''（Rust 侧空串早退，与 SQL 谓词守卫等价）
        if let Some(title) = title_of(question) {
            sqlx::query("UPDATE chat.conv SET title = $2 WHERE id = $1 AND title = $3")
                .bind(conv_id)
                .bind(&title)
                .bind(DEFAULT_TITLE)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// 会话标题：trim + 剥 `\r\n\t` 后按**字符**取前 18 个（不切出半个多字节字）；
/// 空标题（空问句/全是空白控制字符）返回 `None` = 不设。
fn title_of(question: &str) -> Option<String> {
    let title: String = question
        .trim()
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '\t'))
        .take(18)
        .collect();
    if title.is_empty() { None } else { Some(title) }
}

/// `save_msg` 的留痕版：失败 warn 后吞掉 —— 会话消息丢失不允许无声（对照裸 `let _ =` 调用点），
/// 但落库失败也不许炸主链路（同 query_log 的观测降级纪律）。
pub async fn save_msg_logged(
    pg: &PgPool,
    conv_id: i64,
    role: &str,
    question: &str,
    payload: Option<&Value>,
) {
    if let Err(e) = save_msg(pg, conv_id, role, question, payload).await {
        tracing::warn!(conv_id, role, "会话消息落库失败: {e}");
    }
}

/// 会话最近一轮的 **(用户问句, 那一轮实际执行的 SQL)** —— 喂给多轮追问改写
/// （`dms_agent::PrevTurn`）。本轮 user 尚未落库，取到的是上一轮。
///
/// 🔴 SQL 只能从 `chat.msg.payload->>'sql'` 取，**不是 `meta.query_log`**：
/// query_log 如今有 `conv_id` 列（main.rs 透传），但①失败行无 SQL（route/sql 空）、
/// ②它是只写不读的观测表 —— 「本会话上一轮实际执行的 SQL」这个事实源只能是 chat.msg。
///
/// 第二列为 `None` 的真实形态有两种，都由 `rewrite_followup` 的失败轮守卫接住：
/// ① 上一轮走了知识库（payload 是 `dms_kernel::Answer`，没有 `sql` 键）；
/// ② 上一轮是复合容器（`sql` 是字面量 `[复合问题拆解]`，不是 SQL —— 那一层由守卫判形态）。
/// 而**硬失败的轮次一行都不落库**（`api_ask` 里 `?` 早返回，两条 `save_msg` 都跑不到），
/// 所以「最近一条」本来就等于「最近一条成功轮」——上游那句「只取最近一条 SUCCESS」在这里
/// 是由写入侧保证的，不需要在读取侧再筛一遍状态。
///
/// 用相关子查询按 `a.id > u.id` 配对同一轮的答案，而不是各取最新一条：
/// 后者在「上一轮问完但答案还没落库」的瞬间会把**更早一轮**的 SQL 配给最新的问句。
pub async fn last_turn(
    pg: &PgPool,
    conv_id: i64,
) -> anyhow::Result<Option<(String, Option<String>)>> {
    // 上一轮问句优先取 AI 产物里的「生效问句」（resolved_question：追问改写/归一后的完整形态）——
    // 用户上一句可能是碎片（「上月呢？」），链式追问拿碎片当上下文会丢实体（2026-08-12 实测）。
    // 深度轮的产物包一层 `{"result": …}`（deep_api 的落账形状）——sql/resolved_question 都在里层，
    // 两档都要看（2026-08-12 实测深度轮后追问断链）。
    let row = sqlx::query(
        "SELECT u.question AS raw_q,
                (SELECT a.payload FROM chat.msg a
                  WHERE a.conv_id = u.conv_id AND a.role = 'ai' AND a.id > u.id
                  ORDER BY a.id LIMIT 1) AS payload
           FROM chat.msg u
          WHERE u.conv_id = $1 AND u.role = 'user'
          ORDER BY u.id DESC LIMIT 1",
    )
    .bind(conv_id)
    .fetch_optional(pg)
    .await?;
    Ok(row.map(|r| {
        let raw: String = r.get("raw_q");
        let payload: Option<serde_json::Value> = r.get("payload");
        let pick = |key: &str| {
            payload.as_ref().and_then(|p| {
                p.get(key)
                    .or_else(|| p.get("result").and_then(|inner| inner.get(key)))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
        };
        (pick("resolved_question").unwrap_or(raw), pick("sql"))
    }))
}

/// 最近 N 轮的**生效问句**（新→旧，resolved_question 优先）：追问改写的对话上下文。
/// 读失败 = 空 Vec（追问降级为首问语义不变，与 `last_turn` 的失败口径一致）。
pub async fn recent_questions(pg: &PgPool, conv_id: i64, limit: i64) -> Vec<String> {
    let rows = sqlx::query(
        "SELECT u.question AS raw_q,
                (SELECT a.payload FROM chat.msg a
                  WHERE a.conv_id = u.conv_id AND a.role = 'ai' AND a.id > u.id
                  ORDER BY a.id LIMIT 1) AS payload
           FROM chat.msg u
          WHERE u.conv_id = $1 AND u.role = 'user'
          ORDER BY u.id DESC LIMIT $2",
    )
    .bind(conv_id)
    .bind(limit)
    .fetch_all(pg)
    .await;
    match rows {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                let raw: String = r.get("raw_q");
                let payload: Option<serde_json::Value> = r.get("payload");
                payload
                    .as_ref()
                    .and_then(|p| {
                        p.get("resolved_question")
                            .or_else(|| p.get("result").and_then(|inner| inner.get("resolved_question")))
                            .and_then(|v| v.as_str())
                    })
                    .map(str::to_string)
                    .unwrap_or(raw)
            })
            .collect(),
        Err(e) => {
            tracing::warn!(conv_id, error = %e, "取会话上下文失败，本轮按无历史处理");
            vec![]
        }
    }
}

/// 删除会话。**`Ok` ≠ 删了行**：非属主/不存在时 WHERE 命中 0 行也是 Ok ——
/// 刻意不泄存在性（与属主闸「非属主/不存在同 403」同哲学）；只有 DB 错才走 `Err`。
pub async fn delete_conv(pg: &PgPool, conv_id: i64, login: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM chat.conv WHERE id = $1 AND login_name = $2")
        .bind(conv_id)
        .bind(login)
        .execute(pg)
        .await?;
    Ok(())
}

/// 校验会话归属（防越权访问他人会话）
pub async fn conv_owner(pg: &PgPool, conv_id: i64) -> anyhow::Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT login_name FROM chat.conv WHERE id = $1")
        .bind(conv_id)
        .fetch_optional(pg)
        .await?)
}

/// 会话属主闸（api_ask / api_conv_msgs / steer 三处共用的唯一事实源，判据与文案不许再抄第二份）：
/// 属主放行；非属主/不存在同 403（不泄存在性）；属主查询本身 DB 错 → 500（DB 错不是「无权」）。
pub async fn ensure_owner(
    pg: &PgPool,
    conv_id: i64,
    login: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    match conv_owner(pg, conv_id).await {
        Ok(Some(owner)) if owner == login => Ok(()),
        Ok(_) => Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "无权访问该会话" })),
        )),
        Err(e) => {
            tracing::warn!(conv_id, "会话属主查询失败: {e}");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "会话状态读取失败，请稍后重试" })),
            ))
        }
    }
}

/// 分支会话：以 `from_seq`（1 基序号，按 id 升序；「之前（含）」即前 from_seq 条）为界，
/// 把原会话前段消息深拷贝进一条新会话；`from_seq = None` ⇒ 整条会话全量复制。
/// 返回 `(新会话 id, 复制条数)`；`Ok(None)` = 非属主或会话不存在。
///
/// - 属主校验**内联**在建新会话的 INSERT 里（`login_name = $2`），非属主/不存在
///   一行不插 ⇒ `None`，路由层映射 403 —— fail-closed，无跨用户分支；
/// - 深拷贝 `chat.msg` 行：新行新 id，新会话后续写入只触新行，与原会话零共享；
/// - payload 按值复制——深度页的 page/artifact 引用是**只读引用**（artifact 本体与
///   `share_token` 都在 `meta.artifact`，不随消息走），share token 天然不复制；
/// - 建会话 + 复制在同一事务里：复制失败则新会话一并回滚，不留空壳分支。
pub async fn branch_conv(
    pg: &PgPool,
    src_conv_id: i64,
    login: &str,
    from_seq: Option<i64>,
) -> anyhow::Result<Option<(i64, i64)>> {
    let mut tx = pg.begin().await?;
    let new_id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO chat.conv(login_name, title)
         SELECT login_name, title FROM chat.conv
         WHERE id = $1 AND login_name = $2
         RETURNING id",
    )
    .bind(src_conv_id)
    .bind(login)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(new_id) = new_id else {
        return Ok(None);
    };
    let copied = sqlx::query(
        "INSERT INTO chat.msg(conv_id, role, question, payload)
         SELECT $2, role, question, payload FROM chat.msg
         WHERE conv_id = $1 ORDER BY id LIMIT $3",
    )
    .bind(src_conv_id)
    .bind(new_id)
    .bind(branch_cut(from_seq))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(Some((new_id, i64::try_from(copied).unwrap_or(i64::MAX))))
}

/// 分支截取条数：`from_seq` 缺省=整条（LIMIT 顶到实际不可达的 `i64::MAX` —— PG `LIMIT n > 总行数`
/// 自然只复制现有行，不需要先 `count(*)` 一次：那次 count 是白跑的往返，且 count→copy 之间
/// 还有并发窗口）；负数钳 0（PG `LIMIT` 负数报错）。
fn branch_cut(from_seq: Option<i64>) -> i64 {
    from_seq.map_or(i64::MAX, |n| n.max(0))
}

// ─────────────────────── 【Y5】steer 插话端点 ───────────────────────

#[derive(serde::Deserialize)]
pub struct SteerReq {
    /// 插话内容（剥控制字符 + 500 字护栏在 agent 侧 `sanitize_steer` 收口，这里只透传）
    pub content: String,
}

/// `POST /api/chat/conv/{id}/steer`：给**运行中**的任务插一条修正指令
/// （「不是这个口径，按 X 重算」），执行器在下一安全点并入当前上下文重走一次组 SQL。
/// 路由行已在 main.rs 接线（契约见文件头）。信箱与「运行中」登记在 `dms_agent::run` ——
/// agent 不能反向依赖 server，所以运行态的事实源只能在它那边，本 handler 只查不收。
pub async fn api_conv_steer(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(conv_id): Path<i64>,
    Json(req): Json<SteerReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let err = |code: StatusCode, msg: &str| (code, Json(serde_json::json!({ "error": msg })));
    // 【D10】双通道身份：API key（X-API-Key / Bearer <key>）或会话 token。
    // BadKey（显式递错 key）→ 401 不降级；Absent → 401 同会话端点现有文案。
    let key_hdr = headers.get(&X_API_KEY).and_then(|v| v.to_str().ok());
    let bearer = auth::bearer_value(
        headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()),
    );
    let login = match auth::resolve_identity_dual(&st.mcp_keys, key_hdr, bearer) {
        auth::IdentityChannel::ApiKey(login) => login,
        auth::IdentityChannel::Session(s) => s.login_name,
        auth::IdentityChannel::BadKey | auth::IdentityChannel::Absent => {
            return Err(err(StatusCode::UNAUTHORIZED, "未认证"));
        }
    };
    // 会话属主校验（防越权往他人会话插话；判据与文案同 api_ask/api_conv_msgs，收口在 ensure_owner）。
    // 注意顺序：先身份后属主 —— 非属主与不存在同 403，不泄存在性。
    ensure_owner(st.owned.pool(), conv_id, &login).await?;
    let Some(content) = dms_agent::run::sanitize_steer(&req.content) else {
        return Err(err(StatusCode::BAD_REQUEST, "插话内容为空（或全是控制字符）"));
    };
    match dms_agent::run::push_steer(&conv_id.to_string(), content) {
        Ok(queued) => Ok(Json(serde_json::json!({ "ok": true, "queued": queued }))),
        Err(dms_agent::run::SteerReject::NotRunning) => Err(err(
            StatusCode::CONFLICT,
            "当前会话没有运行中的任务，插话未受理",
        )),
        Err(dms_agent::run::SteerReject::Full) => Err(err(
            StatusCode::TOO_MANY_REQUESTS,
            "插话队列已满，请等当前任务消化后再试",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺省 from_seq ⇒ 整条会话全量复制：LIMIT 顶到实际不可达的 i64::MAX
    /// （PG `LIMIT n > 总行数` 自然只复制现有行 —— 不再需要先 count 一次的往返）。
    #[test]
    fn branch_cut_default_copies_everything() {
        assert_eq!(branch_cut(None), i64::MAX, "缺省必须全量（上限顶到实际不可达）");
    }

    /// 显式 from_seq：取前 from_seq 条（含）；负数钳 0（PG `LIMIT` 负数直接报错）。
    #[test]
    fn branch_cut_clamps_into_range() {
        assert_eq!(branch_cut(Some(3)), 3);
        assert_eq!(branch_cut(Some(1)), 1, "第 1 条之前（含）= 首条");
        assert_eq!(branch_cut(Some(0)), 0, "0 之前（含）= 空分支");
        assert_eq!(branch_cut(Some(-2)), 0, "负数按 0 处理");
    }

    /// migrate 幂等纪律镜像（同 query_log.rs 的判据）：DDL 每句都必须可重复执行（启动路径每次全跑）；
    /// 首行代码必须 CREATE 开头 —— 注释混入 ASCII 分号会切出碎句，启动期才炸，提前到这里。
    #[test]
    fn ddl_statements_are_idempotent() {
        for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            assert!(stmt.contains("IF NOT EXISTS"), "非幂等语句: {stmt}");
            let first_code = stmt
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("--"))
                .unwrap_or("");
            assert!(first_code.starts_with("CREATE"), "split 切出的碎句（注释内分号？）: {stmt}");
        }
        // 默认标题字面量两处同源：DDL 是 SQL 文本插不进常量，钉死一致（改一忘一 = 标题永不刷新）
        assert!(
            DDL.contains(&format!("DEFAULT '{DEFAULT_TITLE}'")),
            "DDL 的默认标题必须与 DEFAULT_TITLE 逐字一致"
        );
    }

    /// 标题：trim + 剥 `\r\n\t` + 按字符取前 18（不切出半个多字节字）；空标题不设
    #[test]
    fn title_of_trims_strips_and_clips_by_chars() {
        assert_eq!(title_of("本月销售额").as_deref(), Some("本月销售额"));
        let long = "销售订单金额按月份统计汇总表明细数据查询分析";
        let t = title_of(long).expect("长问句必须有标题");
        assert_eq!(t.chars().count(), 18, "按字符截，不按字节");
        assert_eq!(title_of("  本月\n销售额\t ").as_deref(), Some("本月销售额"), "trim + 剥控制字符");
        assert_eq!(title_of(""), None, "空问句不设标题（否则刷成 ''）");
        assert_eq!(title_of("\n\t  "), None, "全是空白控制字符不设标题");
    }

    /// save_msg 同事务纪律（无库可测，照本仓既有形态扫源码）：
    /// INSERT/updated_at/标题三步包在一个 tx（崩在中间不留半态），
    /// 且 INSERT 与 updated_at 刷新合并成一条 CTE（省一次往返）。
    #[test]
    fn save_msg_is_transactional_and_merges_touch_cte() {
        let src = include_str!("chat.rs");
        let body = src
            .split("pub async fn save_msg(")
            .nth(1)
            .expect("save_msg 没了 —— 顺手把这条判据一起改")
            .split("\n}\n")
            .next()
            .unwrap();
        assert!(body.contains("pg.begin()") && body.contains("tx.commit()"),
            "三条 SQL 必须包在一个事务里：{body}");
        assert!(body.contains("WITH m AS (INSERT INTO chat.msg"),
            "INSERT 与 updated_at 刷新必须合并成一条 CTE：{body}");
    }

    /// 会话列表排序：updated_at 同值时按 id 兜底（PG 不保证同值稳定序）
    #[test]
    fn list_convs_orders_by_updated_at_then_id() {
        let src = include_str!("chat.rs");
        assert!(src.contains("ORDER BY updated_at DESC, id DESC"), "updated_at 同值缺 id 兜底");
    }

    /// 属主校验 fail-closed + 顺序保持：钉住 branch_conv 的 SQL 形态。
    /// （本 crate 单测不起 PG，存储层不变量用源码断言守住，同 deep_api 的写法。）
    #[test]
    fn branch_sql_keeps_owner_gate_and_order() {
        let src = include_str!("chat.rs");
        let body = src
            .split("pub async fn branch_conv")
            .nth(1)
            .and_then(|tail| tail.split("#[cfg(test)]").next())
            .expect("branch_conv 应存在");
        assert!(
            body.contains("WHERE id = $1 AND login_name = $2"),
            "属主校验必须内联在建新会话的 INSERT 里（fail-closed）：{body}"
        );
        assert!(
            body.contains("fetch_optional"),
            "非属主必须走 None ⇒ 403，而不是报错泄存在性：{body}"
        );
        let owner_gate = body.find("FROM chat.conv").expect("先建新会话");
        let copy = body.find("INSERT INTO chat.msg").expect("再复制消息");
        assert!(owner_gate < copy, "必须先过属主闸再复制消息：{body}");
        let order = body.find("ORDER BY id").expect("复制必须按原序");
        let limit = body.find("LIMIT $3").expect("复制必须有截取上限");
        assert!(order < limit, "先按 id 升序排再截取：{body}");
        assert!(
            body.contains("INSERT INTO chat.msg(conv_id, role, question, payload)"),
            "只深拷贝 role/question/payload 三列，不携带任何 token：{body}"
        );
        assert!(body.contains("tx.commit()"), "建会话+复制必须在同一事务里：{body}");
    }

    /// 【Y5】steer 端点的接线判据（handler 走 AppState/PG，无库测不了，扫源码）：
    /// 双通道身份 → 属主闸 → 脱敏 → 信箱，顺序与状态码一个都不许漂。锚点 `concat!` 拼（自匹配家族）。
    #[test]
    fn steer_endpoint_keeps_auth_owner_and_state_codes() {
        let src = include_str!("chat.rs");
        let body = src
            .split(concat!("pub async fn api_conv_", "steer("))
            .nth(1)
            .expect("api_conv_steer 没了 —— 顺手把这条判据一起改")
            .split("#[cfg(test)]")
            .next()
            .expect("api_conv_steer 的边界没了");
        // 【D10】双通道身份（本端点是 REST API key 在 chat 面的落地处）
        assert!(body.contains("auth::resolve_identity_dual"), "steer 端点必须走双通道身份：{body}");
        assert!(body.contains("IdentityChannel::BadKey"), "错 key 必须 fail-closed 401：{body}");
        // 先身份后属主（非属主/不存在同 403，不泄存在性）—— 判据收口在 ensure_owner
        let auth_at = body.find("resolve_identity_dual").expect("身份解析");
        let owner_at = body.find("ensure_owner").expect("属主校验（ensure_owner）");
        assert!(auth_at < owner_at, "必须先身份后属主：{body}");
        // 属主闸判据与文案的唯一事实源是 ensure_owner（api_ask/api_conv_msgs/steer 三处共用）：
        // 非属主 403 + 文案不动；属主查询 DB 错必须 500（并进 403 会把故障藏成「无权」）
        let gate = src
            .split("pub async fn ensure_owner")
            .nth(1)
            .expect("ensure_owner 没了 —— 顺手把这条判据一起改")
            .split("\n}\n")
            .next()
            .unwrap();
        assert!(gate.contains("StatusCode::FORBIDDEN") && gate.contains("无权访问该会话"),
            "非属主 403 或文案没了：{gate}");
        assert!(gate.contains("StatusCode::INTERNAL_SERVER_ERROR") && gate.contains("tracing::warn!"),
            "属主查询 DB 错必须 warn + 500：{gate}");
        // 脱敏在入队前（控制字符/长度护栏，untrusted 纪律同 refs）
        let san_at = body.find("sanitize_steer").expect("脱敏");
        let push_at = body.find("push_steer").expect("入队");
        assert!(san_at < push_at, "必须先脱敏后入队：{body}");
        // 状态机映射：仅运行中可 steer（409）、队列满（429）
        assert!(body.contains("SteerReject::NotRunning") && body.contains("StatusCode::CONFLICT"),
            "非运行中必须 409：{body}");
        assert!(body.contains("SteerReject::Full") && body.contains("StatusCode::TOO_MANY_REQUESTS"),
            "队列满必须 429：{body}");
        // 信箱在 agent 侧（agent 不能反向依赖 server）：本文件只查不收
        assert!(body.contains("dms_agent::run::"), "信箱必须走 dms_agent::run：{body}");
    }

    /// steer 请求体的形状契约（前端/脚本按这个字段名发）
    #[test]
    fn steer_req_body_contract() {
        let req: SteerReq = serde_json::from_str(r#"{"content":"按净额重算"}"#).unwrap();
        assert_eq!(req.content, "按净额重算");
        assert!(serde_json::from_str::<SteerReq>(r"{}").is_err(), "content 是必填字段");
    }
}
