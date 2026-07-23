//! dms-ai 服务端：M0 骨架（/api/health）+ M1 权限内核（principal/scope/inject + scope 判官子命令）。

mod db;
mod inject;
mod principal;
mod scope;

use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};
use sqlx::{MySqlPool, PgPool};

struct AppState {
    mysql: MySqlPool,
    pg: PgPool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 日志一律走 stderr：stdout 留给子命令的 JSON 输出（判官脚本要解析）
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = db::load_settings()?;

    // 判官子命令：scope <login_name> [role_code] —— 输出权限集合 JSON + t_sales_order 注入示例
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "scope" {
        let mysql = db::mysql_pool(&cfg.mysql_url).await?;
        let login = &args[2];
        let role = args.get(3).map(|s| s.as_str());
        let p = principal::load_principal(&mysql, login, role).await?;
        let sets = scope::compute_scope(&mysql, &p).await?;
        let demo = inject::inject(
            "SELECT COUNT(*) AS cnt FROM t_sales_order so WHERE so.deleted_flag = 0",
            &sets,
        )?;
        println!(
            "{}",
            serde_json::json!({
                "principal": p,
                "sets": {
                    "employee_ids": sets.employee_ids,
                    "employee_codes": sets.employee_codes,
                    "customer_codes": sets.customer_codes,
                    "unrestricted": sets.is_unrestricted(),
                },
                "demo_sql": demo,
            })
        );
        return Ok(());
    }

    let mysql = db::mysql_pool(&cfg.mysql_url).await?;
    let pg = db::pg_pool(&cfg.pg_url).await?;

    let state = Arc::new(AppState { mysql, pg });
    let app = Router::new()
        .route("/api/health", get(health))
        .with_state(state);

    tracing::info!("dms-ai server listening on {}", cfg.listen);
    let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mysql_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&st.mysql)
        .await
        .is_ok();
    let mysql_readonly = sqlx::query_scalar::<_, i64>("SELECT @@session.transaction_read_only")
        .fetch_one(&st.mysql)
        .await
        .unwrap_or(0)
        == 1;
    let pg_exts: Vec<String> = sqlx::query_scalar("SELECT extname FROM pg_extension ORDER BY 1")
        .fetch_all(&st.pg)
        .await
        .unwrap_or_default();

    Json(serde_json::json!({
        "ok": mysql_ok && mysql_readonly && !pg_exts.is_empty(),
        "mysql": { "connected": mysql_ok, "session_read_only": mysql_readonly },
        "pg": { "extensions": pg_exts },
    }))
}
