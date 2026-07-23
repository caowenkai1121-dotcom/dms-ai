//! dms-ai 服务端：M0 骨架（/api/health）+ M1 权限内核（principal/scope/inject + scope 判官子命令）。

mod auth;
mod db;
mod direct;
mod graph;
mod inject;
mod llm;
mod meta;
mod pipeline;
mod principal;
mod scope;
mod viewspec;

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use sqlx::{MySqlPool, PgPool};

struct AppState {
    mysql: MySqlPool,
    pg: PgPool,
    llm: llm::LlmClient,
    dms_base_url: String,
}

fn llm_client(cfg: &db::Settings) -> llm::LlmClient {
    llm::LlmClient::new(&cfg.llm_base_url, &cfg.llm_api_key, &cfg.llm_model_fast, &cfg.llm_model_precise)
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

    let args: Vec<String> = std::env::args().collect();

    // M2 子命令：meta sync —— 采集 schema 入 PG 并播种警告/强制补表
    if args.len() >= 3 && args[1] == "meta" && args[2] == "sync" {
        let mysql = db::mysql_pool(&cfg.mysql_url).await?;
        let pg = db::pg_pool(&cfg.pg_url).await?;
        meta::migrate(&pg).await?;
        let (nt, nc) = meta::sync_schema(&mysql, &pg).await?;
        meta::seed(&pg).await?;
        println!("{}", serde_json::json!({ "tables": nt, "columns": nc }));
        return Ok(());
    }

    // M6b 子命令：graph sync —— 聚合客户-商品购买边入 AGE 图
    if args.len() >= 3 && args[1] == "graph" && args[2] == "sync" {
        let mysql = db::mysql_pool(&cfg.mysql_url).await?;
        let pg = db::pg_pool(&cfg.pg_url).await?;
        let (nc, ng, ne) = graph::sync(&mysql, &pg).await?;
        println!("{}", serde_json::json!({ "customers": nc, "goods": ng, "edges": ne }));
        return Ok(());
    }

    // M2 子命令：retrieve "<问题>" —— 三路召回冒烟
    if args.len() >= 3 && args[1] == "retrieve" {
        let pg = db::pg_pool(&cfg.pg_url).await?;
        let ctxs = meta::retrieve(&pg, &args[2], 6).await?;
        let table_names: Vec<String> = ctxs.iter().map(|c| c.table_name.clone()).collect();
        let pitfalls = meta::recall_pitfalls(&pg, &args[2], &table_names, 5).await?;
        println!(
            "{}",
            serde_json::json!({
                "tables": ctxs.iter().map(|c| serde_json::json!({
                    "table": c.table_name, "score": c.score, "forced": c.forced,
                })).collect::<Vec<_>>(),
                "pitfalls": pitfalls,
                "schema_chars": ctxs.iter().map(|c| c.schema_text.len()).sum::<usize>(),
            })
        );
        return Ok(());
    }

    // M3 子命令：ask <login_name> "<问题>" [role_code] —— 完整问答链
    if args.len() >= 4 && args[1] == "ask" {
        let mysql = db::mysql_pool(&cfg.mysql_url).await?;
        let pg = db::pg_pool(&cfg.pg_url).await?;
        let client = llm_client(&cfg);
        let p = principal::load_principal(&mysql, &args[2], args.get(4).map(|s| s.as_str())).await?;
        let r = pipeline::ask(&client, &mysql, &pg, &p, &args[3]).await?;
        println!("{}", serde_json::to_string(&r)?);
        return Ok(());
    }

    // 判官子命令：scope <login_name> [role_code] —— 输出权限集合 JSON + t_sales_order 注入示例
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
    meta::migrate(&pg).await?;

    let state = Arc::new(AppState {
        mysql,
        pg,
        llm: llm_client(&cfg),
        dms_base_url: cfg.dms_base_url.clone(),
    });
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/sso", post(api_sso))
        .route("/api/ask", post(api_ask))
        .with_state(state);

    tracing::info!("dms-ai server listening on {}", cfg.listen);
    let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct SsoReq {
    /// DMS 的 x-access-token（iframe 嵌入时由 DMS 前端透传）
    dms_token: String,
    /// DMS 当前激活角色（可选，前端知道）
    role_code: Option<String>,
}

/// SSO 换签：验真 DMS token → 颁自有会话 token
async fn api_sso(
    State(st): State<Arc<AppState>>,
    Json(req): Json<SsoReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    let login_name = auth::verify_dms_token(&st.dms_base_url, &req.dms_token)
        .await
        .map_err(|e| err(StatusCode::UNAUTHORIZED, e.to_string()))?;
    let token = auth::issue(login_name.clone(), req.role_code.clone());
    Ok(Json(serde_json::json!({ "token": token, "login_name": login_name })))
}

#[derive(serde::Deserialize)]
struct AskReq {
    question: String,
    /// 开发/内网模式的直接身份传递；生产走 Authorization Bearer 会话 token
    login_name: Option<String>,
    role_code: Option<String>,
}

async fn api_ask(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<AskReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    // 身份来源优先级：Authorization Bearer 会话 token > body.login_name（开发）
    let (login_name, role_code) = match bearer(&headers).and_then(|t| auth::resolve(&t)) {
        Some((ln, rc)) => (ln, rc),
        None => match req.login_name.clone() {
            Some(ln) => (ln, req.role_code.clone()),
            None => return Err(err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name".into())),
        },
    };
    let p = principal::load_principal(&st.mysql, &login_name, role_code.as_deref())
        .await
        .map_err(|e| err(StatusCode::FORBIDDEN, e.to_string()))?;
    let r = pipeline::ask(&st.llm, &st.mysql, &st.pg, &p, &req.question)
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    Ok(Json(serde_json::to_value(r).unwrap()))
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.to_string())
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
