//! dms-ai 服务端 M0 骨架：配置加载 + 双库连通（MySQL 只读 / PG）+ /health。

use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};
use serde::Deserialize;
use sqlx::{mysql::MySqlPoolOptions, postgres::PgPoolOptions, MySqlPool, PgPool};

#[derive(Deserialize, Clone)]
struct Settings {
    mysql_url: String,
    pg_url: String,
    #[serde(default = "default_listen")]
    listen: String,
}

fn default_listen() -> String {
    "127.0.0.1:8100".into()
}

struct AppState {
    mysql: MySqlPool,
    pg: PgPool,
}

fn load_settings() -> anyhow::Result<Settings> {
    // 就近找 settings.json：优先当前目录，其次仓库根（cargo run 时 cwd=仓库根）
    for p in ["settings.json", "../settings.json", "../../settings.json"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            return Ok(serde_json::from_str(&s)?);
        }
    }
    anyhow::bail!("settings.json 未找到（参考 settings.example.json）")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = load_settings()?;

    // 🔴 红线：DMS 生产库只读——每个连接建立时把会话设为 READ ONLY，
    // 任何写语句在 MySQL 层直接报错，代码层再有疏漏也写不进去。
    let mysql = MySqlPoolOptions::new()
        .max_connections(10)
        .after_connect(|conn, _| {
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute("SET SESSION TRANSACTION READ ONLY").await?;
                Ok(())
            })
        })
        .connect(&cfg.mysql_url)
        .await?;

    let pg = PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.pg_url)
        .await?;

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
    // MySQL：连通 + 只读会话双确认
    let mysql_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&st.mysql)
        .await
        .is_ok();
    let mysql_readonly = sqlx::query_scalar::<_, i64>("SELECT @@session.transaction_read_only")
        .fetch_one(&st.mysql)
        .await
        .unwrap_or(0)
        == 1;
    // PG：连通 + 扩展清单
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
