//! 配置加载与双库连接池。

use serde::Deserialize;
use sqlx::{mysql::MySqlPoolOptions, postgres::PgPoolOptions, MySqlPool, PgPool};

#[derive(Deserialize, Clone)]
pub struct Settings {
    pub mysql_url: String,
    pub pg_url: String,
    #[serde(default = "default_listen")]
    pub listen: String,
}

fn default_listen() -> String {
    "127.0.0.1:8100".into()
}

pub fn load_settings() -> anyhow::Result<Settings> {
    // 就近找 settings.json：优先当前目录，其次仓库根（cargo run 时 cwd=仓库根）
    for p in ["settings.json", "../settings.json", "../../settings.json"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            return Ok(serde_json::from_str(&s)?);
        }
    }
    anyhow::bail!("settings.json 未找到（参考 settings.example.json）")
}

/// 🔴 红线：DMS 生产库只读——每个连接建立时把会话设为 READ ONLY，
/// 任何写语句在 MySQL 层直接报错，代码层再有疏漏也写不进去。
pub async fn mysql_pool(url: &str) -> anyhow::Result<MySqlPool> {
    Ok(MySqlPoolOptions::new()
        .max_connections(10)
        .after_connect(|conn, _| {
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute("SET SESSION TRANSACTION READ ONLY").await?;
                Ok(())
            })
        })
        .connect(url)
        .await?)
}

pub async fn pg_pool(url: &str) -> anyhow::Result<PgPool> {
    Ok(PgPoolOptions::new().max_connections(10).connect(url).await?)
}
