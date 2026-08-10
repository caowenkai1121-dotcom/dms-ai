//! S1 质量控制面：用户反馈 + 管理员质量统计。
//! 查询日志仍是一次问答的事实源；本模块只补反馈闭环和 PostgreSQL 聚合读侧。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use crate::AppState;

type ApiErr = (StatusCode, Json<serde_json::Value>);

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

pub async fn migrate(pg: &sqlx::PgPool) -> anyhow::Result<()> {
    for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

#[derive(serde::Deserialize)]
pub struct FeedbackReq {
    trace_id: String,
    kind: String,
    #[serde(default)]
    detail: String,
    login_name: Option<String>,
    role_code: Option<String>,
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
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证"))?;
    let trace = req.trace_id.trim();
    if uuid::Uuid::parse_str(trace).is_err() {
        return Err(err(StatusCode::BAD_REQUEST, "trace_id 无效"));
    }
    if !matches!(req.kind.as_str(), "correct" | "caliber" | "data" | "permission" | "display") {
        return Err(err(StatusCode::BAD_REQUEST, "反馈类型无效"));
    }
    let detail: String = req.detail.trim().chars().take(1000).collect();
    for wait in [0, 40, 120, 240] {
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
               kind=EXCLUDED.kind,detail=EXCLUDED.detail,status=EXCLUDED.status,created_at=now()
             RETURNING id",
        )
        .bind(trace)
        .bind(&login)
        .bind(&req.kind)
        .bind(&detail)
        .fetch_optional::<(i64,)>()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        if let Some((id,)) = row {
            return Ok(Json(serde_json::json!({ "ok": true, "id": id })));
        }
    }
    Err(err(StatusCode::NOT_FOUND, "未找到本人对应的查询记录，请稍后重试"))
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
    let summary: SummaryRow = st.owned.fixed(
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
    ).bind(days).fetch_optional::<SummaryRow>().await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .unwrap_or((0, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0, 0));
    let routes = st.owned.fixed(
        "SELECT COALESCE(NULLIF(route,''),'失败') route,count(*)::bigint,
          COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY elapsed_ms),0)::float8,
          count(*) FILTER (WHERE error<>'')::bigint
         FROM meta.query_log WHERE at >= now()-$1::int*interval '1 day'
         GROUP BY 1 ORDER BY count(*) DESC",
    ).bind(days).fetch_all::<RouteRow>().await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let feedback = st.owned.fixed(
        "SELECT f.id,f.kind,f.detail,f.status,f.created_at,q.login_name,q.question,q.route
         FROM meta.query_feedback f
         LEFT JOIN LATERAL (
           SELECT login_name,question,route FROM meta.query_log
           WHERE trace_id=f.trace_id ORDER BY id DESC LIMIT 1
         ) q ON true
         WHERE f.created_at >= now()-$1::int*interval '1 day'
         ORDER BY f.created_at DESC LIMIT 30",
    ).bind(days).fetch_all::<FeedbackRow>().await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let success_rate = if summary.0 == 0 { 0.0 } else { 100.0 * summary.1 as f64 / summary.0 as f64 };
    Ok(Json(serde_json::json!({
        "days": days,
        "summary": {
            "total": summary.0, "success": summary.1, "success_rate": success_rate,
            "p50_ms": summary.2, "p95_ms": summary.3, "llm_rate": summary.4,
            "cache_rate": summary.5, "avg_tokens": summary.6,
            "feedback_count": summary.7, "error_count": summary.8
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
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
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
}
