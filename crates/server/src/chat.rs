//! 多会话持久化：一个会话(conv)含多轮问答(msg)，按登录用户归属。
//! 对齐 SuperSonic conversation 模型。修复「一问一会话」——同会话内多轮归一条，侧栏列会话。

use serde_json::Value;
use sqlx::{PgPool, Row};

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
