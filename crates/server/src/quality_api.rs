//! S1 质量控制面：用户反馈 + 管理员质量统计。
//! 查询日志仍是一次问答的事实源；本模块只补反馈闭环和 PostgreSQL 聚合读侧。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use crate::admin_api::{err, ApiErr};
use crate::AppState;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta.query_feedback(
  id bigserial PRIMARY KEY,
  trace_id text NOT NULL,
  conv_id text,
  login_name text NOT NULL,
  kind text NOT NULL CHECK (kind IN ('correct','caliber','data','permission','display')),
  detail text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved')),
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE(trace_id, login_name)
);
CREATE INDEX IF NOT EXISTS idx_query_feedback_at ON meta.query_feedback(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_query_feedback_status ON meta.query_feedback(status, created_at DESC);
"#;

/// 三句 DDL 全是 IF NOT EXISTS：幂等靠「失败则下次启动重跑」补齐，未包事务
/// （与 datamap_api / kg_api / query_log 同一惯例）——中途失败留的半迁移无害。
pub async fn migrate(pg: &sqlx::PgPool) -> anyhow::Result<()> {
    for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

/// 内部错误的统一出口（照 ds_api::internal_err 的模子）：ConnectorError 原文可能含
/// 关系名、约束名等内部结构，回前端等于泄露——真因 tracing::warn! 留服务端，
/// 响应只带固定文案。响应形状不变：`{"error": 固定文案}` + 500。
fn internal_err(context: &'static str, e: impl std::fmt::Display) -> ApiErr {
    tracing::warn!(error = %e, "{context}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "服务暂时不可用，请稍后重试")
}

/// 反馈绑定的重试预算（ms）：query_log 异步落库，极快点击时短暂等它；
/// 逐次 0/40/120/240，总预算 400ms——不为反馈把主查询改成同步等 PostgreSQL。
const BIND_RETRY_BUDGET_MS: [u64; 4] = [0, 40, 120, 240];

#[derive(serde::Deserialize)]
pub struct FeedbackReq {
    trace_id: String,
    kind: String,
    #[serde(default)]
    detail: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// trace_id 归一化：大写 / 无连字符变体也能过 parse，但与库中小写标准形按字节不等
/// （text 列）——只校验不归一化会让合法变体白白重试 400ms 后 404。None 即非法输入。
fn normalize_trace_id(raw: &str) -> Option<String> {
    uuid::Uuid::parse_str(raw.trim()).ok().map(|u| u.to_string())
}

/// 用户反馈只允许绑定本人 query_log。日志写入是异步的，极快点击时短暂重试，
/// 不为反馈功能把主查询改成同步等待 PostgreSQL。
///
/// 绑定谓词只有 `trace_id` + 本人，**与 route 无关**：问数行与 KB 文档问答行
/// （`route='knowledge'`，Y2 起由 knowledge 层落账）走同一条通道，admin 质量页的
/// 反馈列表按 `trace_id` JOIN 回 query_log，knowledge 行自然带出 route 进列表。
pub async fn feedback(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<FeedbackReq>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let (login, _) = crate::resolve_identity(&st, &headers, &req.login_name, &req.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let trace = match normalize_trace_id(&req.trace_id) {
        Some(t) => t,
        None => return Err(err(StatusCode::BAD_REQUEST, "trace_id 无效")),
    };
    if !matches!(req.kind.as_str(), "correct" | "caliber" | "data" | "permission" | "display") {
        return Err(err(StatusCode::BAD_REQUEST, "反馈类型无效"));
    }
    let detail: String = req.detail.trim().chars().take(1000).collect();
    for wait in BIND_RETRY_BUDGET_MS {
        if wait > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
        }
        let row = st.owned.fixed(
            "INSERT INTO meta.query_feedback(trace_id,conv_id,login_name,kind,detail,status)
             SELECT q.trace_id,q.conv_id,$2,$3,$4,CASE WHEN $3='correct' THEN 'resolved' ELSE 'open' END
             FROM meta.query_log q
             WHERE q.trace_id=$1 AND q.login_name=$2
             ORDER BY q.id DESC LIMIT 1
             ON CONFLICT(trace_id,login_name) DO UPDATE SET
               kind=EXCLUDED.kind,detail=EXCLUDED.detail,status=EXCLUDED.status
             RETURNING id",
        )
        .bind(&trace)
        .bind(&login)
        .bind(&req.kind)
        .bind(&detail)
        .fetch_optional::<(i64,)>()
        .await
        .map_err(|e| internal_err("反馈写入失败", e))?;
        if let Some((id,)) = row {
            return Ok(Json(serde_json::json!({ "ok": true, "id": id })));
        }
    }
    // 重试预算耗尽仍未绑上：记录不存在 / 已被保留期清理 / 非本人——文案不得再暗示重试有效
    Err(err(StatusCode::NOT_FOUND, "未找到本人对应的查询记录：记录不存在或已过期"))
}

#[derive(serde::Deserialize, Default)]
pub struct QualityQuery {
    days: Option<i32>,
    login_name: Option<String>,
    role_code: Option<String>,
}

type SummaryRow = (i64, i64, f64, f64, f64, f64, f64, i64, i64);
type RouteRow = (String, i64, f64, i64);
type FeedbackRow = (i64, String, String, String, chrono::DateTime<chrono::Utc>, String, String, String);

pub async fn quality(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<QualityQuery>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    crate::admin_api::admin_only(&st, &headers, (&q.login_name, &q.role_code)).await?;
    let days = q.days.unwrap_or(7).clamp(1, 90);
    // 三条只读查询互不依赖：try_join 并行，省掉串行 await 白付的两个 RTT。
    let (summary, routes, feedback) = tokio::try_join!(
        st.owned.fixed(
            "SELECT count(*)::bigint,
              count(*) FILTER (WHERE error='')::bigint,
              COALESCE(percentile_cont(0.5) WITHIN GROUP (ORDER BY elapsed_ms),0)::float8,
              COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY elapsed_ms),0)::float8,
              COALESCE(100.0*count(*) FILTER (WHERE route LIKE 'llm%')/NULLIF(count(*),0),0)::float8,
              COALESCE(100.0*count(*) FILTER (WHERE cache_hit)/NULLIF(count(*),0),0)::float8,
              COALESCE(avg(prompt_tokens+completion_tokens),0)::float8,
              (SELECT count(*)::bigint FROM meta.query_feedback f WHERE f.created_at >= now()-$1::int*interval '1 day'),
              count(*) FILTER (WHERE error<>'')::bigint
             FROM meta.query_log WHERE at >= now()-$1::int*interval '1 day'",
        ).bind(days).fetch_optional::<SummaryRow>(),
        st.owned.fixed(
            "SELECT COALESCE(NULLIF(route,''),'失败') route,count(*)::bigint,
              COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY elapsed_ms),0)::float8,
              count(*) FILTER (WHERE error<>'')::bigint
             FROM meta.query_log WHERE at >= now()-$1::int*interval '1 day'
             GROUP BY 1 ORDER BY count(*) DESC",
        ).bind(days).fetch_all::<RouteRow>(),
        st.owned.fixed(
            "SELECT f.id,f.kind,f.detail,f.status,f.created_at,
              COALESCE(q.login_name,''),COALESCE(q.question,''),COALESCE(q.route,'')
             FROM meta.query_feedback f
             LEFT JOIN LATERAL (
               SELECT login_name,question,route FROM meta.query_log
               WHERE trace_id=f.trace_id ORDER BY id DESC LIMIT 1
             ) q ON true
             WHERE f.created_at >= now()-$1::int*interval '1 day'
             ORDER BY f.created_at DESC LIMIT 30",
        ).bind(days).fetch_all::<FeedbackRow>(),
    ).map_err(|e| internal_err("质量统计查询失败", e))?;
    // 无 GROUP BY 的聚合查询恒返一行，None 分支不可达；PgStmt 只有 fetch_optional，
    // unwrap_or_default 的零值仅作类型兜底（fetch_optional 形态不变）。
    let summary = summary.unwrap_or_default();
    // 具名解构给 9 个位置值起名（顺序与上方 SQL 投影一一对应），代替 .0–.8 位置访问。
    let (total, success, p50_ms, p95_ms, llm_rate, cache_rate, avg_tokens, feedback_count, error_count) =
        summary;
    let success_rate = if total == 0 { 0.0 } else { 100.0 * success as f64 / total as f64 };
    Ok(Json(serde_json::json!({
        "days": days,
        "summary": {
            "total": total, "success": success, "success_rate": success_rate,
            "p50_ms": p50_ms, "p95_ms": p95_ms, "llm_rate": llm_rate,
            "cache_rate": cache_rate, "avg_tokens": avg_tokens,
            "feedback_count": feedback_count, "error_count": error_count
        },
        "routes": routes.into_iter().map(|x| serde_json::json!({
            "route": x.0, "count": x.1, "p95_ms": x.2, "errors": x.3
        })).collect::<Vec<_>>(),
        "feedback": feedback.into_iter().map(|x| serde_json::json!({
            "id": x.0, "kind": x.1, "detail": x.2, "status": x.3,
            "at": x.4.to_rfc3339(), "login_name": x.5, "question": x.6, "route": x.7
        })).collect::<Vec<_>>()
    })))
}

#[derive(serde::Deserialize)]
pub struct StatusReq {
    status: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

pub async fn set_feedback_status(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<StatusReq>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    crate::admin_api::admin_only(&st, &headers, (&req.login_name, &req.role_code)).await?;
    if !matches!(req.status.as_str(), "open" | "resolved") {
        return Err(err(StatusCode::BAD_REQUEST, "状态无效"));
    }
    let n = st.owned.fixed("UPDATE meta.query_feedback SET status=$2 WHERE id=$1")
        .bind(id).bind(&req.status).execute().await
        .map_err(|e| internal_err("反馈状态更新失败", e))?;
    if n == 0 {
        return Err(err(StatusCode::NOT_FOUND, "反馈不存在"));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_kinds_are_closed() {
        for k in ["correct", "caliber", "data", "permission", "display"] {
            assert!(DDL.contains(k));
        }
        assert!(!DDL.contains("other"));
    }

    /// 反馈绑定与路由无关（源锚点）：knowledge 行（route='knowledge'）必须能进同一通道，
    /// 绑定谓词里出现 route 过滤就是 KB 反馈绑不上（Y2 回归）
    #[test]
    fn feedback_binding_is_route_agnostic() {
        let src = include_str!("quality_api.rs");
        let body = src.split("pub async fn feedback").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(!body.contains("route"), "绑定按 trace_id+本人，不许按路由过滤: {body}");
        // admin 质量页的反馈列表按 trace_id JOIN 回 query_log —— knowledge 行自然进列表
        let q = src.split("pub async fn quality").nth(1).unwrap();
        let q = q.split("\n}\n").next().unwrap();
        assert!(q.contains("LEFT JOIN LATERAL"), "反馈列表必须 JOIN 回 query_log: {q}");
    }

    /// trace_id 归一化（纯函数）：大写 / 无连字符 / 首尾空白变体都归一到小写标准形，
    /// 非法输入拒掉——只校验不归一化会让合法变体白白重试 400ms 后 404。
    #[test]
    fn trace_id_normalizes_to_lowercase_canonical() {
        let canonical = "13e373c2-da92-4947-a8f5-b33b45d613bb";
        assert_eq!(normalize_trace_id(canonical).as_deref(), Some(canonical));
        assert_eq!(normalize_trace_id(&canonical.to_uppercase()).as_deref(), Some(canonical));
        assert_eq!(normalize_trace_id(&canonical.replace('-', "")).as_deref(), Some(canonical));
        assert_eq!(normalize_trace_id(&format!("  {canonical}  ")).as_deref(), Some(canonical));
        assert!(normalize_trace_id("not-a-uuid").is_none());
    }

    /// 404 文案锚点：到 404 时重试预算已耗尽（记录不存在 / 已过期 / 非本人），
    /// 文案不得再暗示重试有效。
    #[test]
    fn feedback_404_does_not_promise_retry() {
        let src = include_str!("quality_api.rs");
        let body = src.split("pub async fn feedback").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("记录不存在或已过期"), "404 须说明记录不存在或已过期: {body}");
        assert!(!body.contains("请稍后重试"), "重试已耗尽，404 再暗示重试是误导: {body}");
    }

    /// 重提交不改写 created_at（源锚点）：created_at 是首次反馈时间；ON CONFLICT 更新它
    /// 会把旧反馈顶到反馈列表（按 created_at DESC）最前，且列名与「最后修改时间」语义不符。
    #[test]
    fn resubmit_keeps_first_created_at() {
        let src = include_str!("quality_api.rs");
        let body = src.split("pub async fn feedback").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let conflict = body.split("ON CONFLICT").nth(1).unwrap();
        let conflict = conflict.split("RETURNING").next().unwrap();
        assert!(!conflict.contains("created_at"), "重提交不许覆盖首次反馈时间: {conflict}");
    }

    /// kind 闭集 handler 侧锚点（DDL 侧由 feedback_kinds_are_closed 守）：
    /// DDL CHECK 与 matches! 两处硬编码，漂移 = handler 放进 DDL 拒收的 kind 或反之。
    #[test]
    fn handler_kind_guard_is_closed() {
        let src = include_str!("quality_api.rs");
        let body = src.split("pub async fn feedback").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        for k in ["correct", "caliber", "data", "permission", "display"] {
            assert!(body.contains(&format!("\"{k}\"")), "handler kind 闭集缺 {k}: {body}");
        }
    }

    /// status 闭集两侧锚点：DDL CHECK 与 set_feedback_status 的 matches! 各一份硬编码。
    #[test]
    fn handler_status_guard_is_closed() {
        assert!(DDL.contains("'open','resolved'"), "DDL status 闭集: {DDL}");
        let src = include_str!("quality_api.rs");
        let body = src.split("pub async fn set_feedback_status").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        for s in ["open", "resolved"] {
            assert!(body.contains(&format!("\"{s}\"")), "handler status 闭集缺 {s}: {body}");
        }
    }

    /// 反馈列表三列来自 LEFT JOIN LATERAL：query_log 行被保留期清理后是 NULL，
    /// 非 Option 解码会让整个端点 500——SQL 侧 COALESCE 兜底（wire 类型保持 string 不变）。
    #[test]
    fn feedback_list_coalesces_purged_join_columns() {
        let src = include_str!("quality_api.rs");
        let q = src.split("pub async fn quality").nth(1).unwrap();
        let q = q.split("\n}\n").next().unwrap();
        for col in ["q.login_name", "q.question", "q.route"] {
            assert!(q.contains(&format!("COALESCE({col},'')")), "JOIN 列 {col} 须 COALESCE 兜底: {q}");
        }
    }

    /// 三条只读查询并行（源锚点）：互不依赖却串行 await 是白付两个 RTT。
    #[test]
    fn quality_queries_run_concurrently() {
        let src = include_str!("quality_api.rs");
        let q = src.split("pub async fn quality").nth(1).unwrap();
        let q = q.split("\n}\n").next().unwrap();
        assert!(q.contains("tokio::try_join!"), "三条只读查询须 try_join 并行: {q}");
    }

    /// 重试预算锚点：总预算 400ms 是等异步落库的上限，不许悄悄加码拖慢反馈接口。
    #[test]
    fn retry_budget_totals_400ms() {
        assert_eq!(BIND_RETRY_BUDGET_MS.iter().sum::<u64>(), 400);
    }
}
