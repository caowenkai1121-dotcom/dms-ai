//! 多会话持久化：一个会话(conv)含多轮问答(msg)，按登录用户归属。
//! 对齐 SuperSonic conversation 模型。修复「一问一会话」——同会话内多轮归一条，侧栏列会话。
//!
//! 【分支会话】`POST /api/chat/conv/{id}/branch` —— 存储函数是 `branch_conv`，
//! 路由行与 handler 由 main.rs 持有（handler 放现有 api_conv_* 旁，风格对齐 api_conv_msgs）：
//!   `.route("/api/chat/conv/{id}/branch", post(api_conv_branch))`
//! handler 契约：Json body `{ login_name?: String, from_seq?: i64 }`（login_name 仅供
//! insecure_login_fallback 回退，同 ConvQuery；from_seq 缺省=整条会话）；
//! resolve_identity 取不出登录人 → 401 `{"error":"未认证"}`；
//! `chat::branch_conv(pool, id, &login, from_seq)` 返回 `Ok(None)`（非属主/不存在）
//! → 403 `{"error":"无权访问该会话"}`；`Ok(Some((conv_id, copied)))`
//! → 200 `{"conv_id": conv_id, "copied": copied}`。
//!
//! 【Y5 steer 插话】`POST /api/chat/conv/{id}/steer` —— handler 是本文件的 `api_conv_steer`
//! （与 branch 不同：本 handler 自持身份解析，用 `auth::resolve_identity_dual` 双通道 —
//! 这也是【D10】REST API key 在 chat 面的落地端点），路由行由 main.rs 接线：
//!   `.route("/api/chat/conv/{id}/steer", post(chat::api_conv_steer))`
//! 语义：会话属主校验（非属主 403）；仅运行中可 steer（信箱在 `dms_agent::run`，
//! 否则 409）；内容剥控制字符 + 500 字护栏（全剥空 400）；队列满 429。

use serde_json::Value;
use sqlx::{PgPool, Row};

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use crate::{auth, AppState};

pub async fn migrate(pg: &PgPool) -> anyhow::Result<()> {
    let ddl = r#"
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
    for stmt in ddl.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

/// 会话列表（按登录用户，最近更新在前）
pub async fn list_convs(pg: &PgPool, login: &str) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT id, title, to_char(updated_at,'MM-DD HH24:MI') AS t FROM chat.conv
         WHERE login_name = $1 ORDER BY updated_at DESC LIMIT 100",
    )
    .bind(login)
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

/// 会话消息回放（按 id 升序）
pub async fn conv_msgs(pg: &PgPool, conv_id: i64) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT role, question, payload FROM chat.msg WHERE conv_id = $1 ORDER BY id",
    )
    .bind(conv_id)
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

/// 存一条消息；首条 user 消息顺手把会话标题设为问题前 18 字
pub async fn save_msg(
    pg: &PgPool,
    conv_id: i64,
    role: &str,
    question: &str,
    payload: Option<&Value>,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO chat.msg(conv_id, role, question, payload) VALUES ($1,$2,$3,$4)")
        .bind(conv_id)
        .bind(role)
        .bind(question)
        .bind(payload)
        .execute(pg)
        .await?;
    sqlx::query("UPDATE chat.conv SET updated_at = now() WHERE id = $1")
        .bind(conv_id)
        .execute(pg)
        .await?;
    if role == "user" {
        // 仅当仍是默认标题时用首问设标题
        let title: String = question.chars().take(18).collect();
        sqlx::query("UPDATE chat.conv SET title = $2 WHERE id = $1 AND title = '新会话'")
            .bind(conv_id)
            .bind(&title)
            .execute(pg)
            .await?;
    }
    Ok(())
}

/// 会话最近一轮的 **(用户问句, 那一轮实际执行的 SQL)** —— 喂给多轮追问改写
/// （`dms_agent::PrevTurn`）。本轮 user 尚未落库，取到的是上一轮。
///
/// 🔴 SQL 只能从 `chat.msg.payload->>'sql'` 取，**不是 `meta.query_log`**：
/// query_log 没有 `conv_id`，从它拿不回「本会话上一轮」（只能拿到「该用户上一条查询」，
/// 那在多标签页/多会话下是另一个会话的 SQL）。
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
    let row = sqlx::query(
        "SELECT u.question,
                (SELECT a.payload->>'sql' FROM chat.msg a
                  WHERE a.conv_id = u.conv_id AND a.role = 'ai' AND a.id > u.id
                  ORDER BY a.id LIMIT 1) AS sql
           FROM chat.msg u
          WHERE u.conv_id = $1 AND u.role = 'user'
          ORDER BY u.id DESC LIMIT 1",
    )
    .bind(conv_id)
    .fetch_optional(pg)
    .await?;
    Ok(row.map(|r| (r.get("question"), r.get("sql"))))
}

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
// 消费者 = main.rs 的 `/api/chat/conv/{id}/branch` 路由（见文件头契约，接线后去掉本行）。
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
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM chat.msg WHERE conv_id = $1")
        .bind(src_conv_id)
        .fetch_one(&mut *tx)
        .await?;
    let copied = sqlx::query(
        "INSERT INTO chat.msg(conv_id, role, question, payload)
         SELECT $2, role, question, payload FROM chat.msg
         WHERE conv_id = $1 ORDER BY id LIMIT $3",
    )
    .bind(src_conv_id)
    .bind(new_id)
    .bind(branch_cut(from_seq, total))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(Some((new_id, copied as i64)))
}

/// 分支截取条数：`from_seq` 缺省=整条；越界钳进 `[0, total]`（空会话恒 0）。
fn branch_cut(from_seq: Option<i64>, total: i64) -> i64 {
    match from_seq {
        None => total,
        Some(n) => n.clamp(0, total),
    }
}

// ─────────────────────── 【Y5】steer 插话端点 ───────────────────────

#[derive(serde::Deserialize)]
pub struct SteerReq {
    /// 插话内容（剥控制字符 + 500 字护栏在 agent 侧 `sanitize_steer` 收口，这里只透传）
    pub content: String,
}

/// `POST /api/chat/conv/{id}/steer`：给**运行中**的任务插一条修正指令
/// （「不是这个口径，按 X 重算」），执行器在下一安全点并入当前上下文重走一次组 SQL。
/// 路由行由 main.rs 接线（契约见文件头）。信箱与「运行中」登记在 `dms_agent::run` ——
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
    let key_hdr = headers.get("X-API-Key").and_then(|v| v.to_str().ok());
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
    // 会话属主校验（防越权往他人会话插话；文案与 api_conv_msgs 一致）。
    // 注意顺序：先身份后属主 —— 非属主与不存在同 403，不泄存在性。
    match conv_owner(st.owned.pool(), conv_id).await {
        Ok(Some(owner)) if owner == login => {}
        Ok(_) => return Err(err(StatusCode::FORBIDDEN, "无权访问该会话")),
        Err(_) => {
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, "会话状态读取失败，请稍后重试"));
        }
    }
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

    /// 缺省 from_seq ⇒ 整条会话全量复制（空会话缺省也是 0 条）。
    #[test]
    fn branch_cut_default_copies_everything() {
        assert_eq!(branch_cut(None, 7), 7, "缺省必须全量");
        assert_eq!(branch_cut(None, 1), 1);
        assert_eq!(branch_cut(None, 0), 0, "空会话缺省也是 0 条");
    }

    /// 显式 from_seq：取前 from_seq 条（含）；越界钳位；空会话任何取值都恒 0。
    #[test]
    fn branch_cut_clamps_into_range() {
        assert_eq!(branch_cut(Some(3), 7), 3);
        assert_eq!(branch_cut(Some(1), 7), 1, "第 1 条之前（含）= 首条");
        assert_eq!(branch_cut(Some(9), 7), 7, "超出总长钳到全量");
        assert_eq!(branch_cut(Some(0), 7), 0, "0 之前（含）= 空分支");
        assert_eq!(branch_cut(Some(-2), 7), 0, "负数按 0 处理");
        assert_eq!(branch_cut(Some(3), 0), 0, "空会话任何 from_seq 都是 0 条");
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
        // 先身份后属主（非属主/不存在同 403，不泄存在性）
        let auth_at = body.find("resolve_identity_dual").expect("身份解析");
        let owner_at = body.find("conv_owner").expect("属主校验");
        assert!(auth_at < owner_at, "必须先身份后属主：{body}");
        assert!(body.contains("StatusCode::FORBIDDEN"), "非属主 403 没了：{body}");
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

    /// steer 请求体的形状契约（前端/脚本按这个字段名发）；顺带把未接线的 handler
    /// 标记成「已用」（函数项引用 —— 路由行接进 main.rs 前不算死代码，接线后可删该行）。
    #[test]
    fn steer_req_body_contract() {
        let req: SteerReq = serde_json::from_str(r#"{"content":"按净额重算"}"#).unwrap();
        assert_eq!(req.content, "按净额重算");
        assert!(serde_json::from_str::<SteerReq>(r"{}").is_err(), "content 是必填字段");
        let _ = api_conv_steer;
    }
}
