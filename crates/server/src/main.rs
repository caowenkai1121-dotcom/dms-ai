//! dms-ai 服务端：M0 骨架（/api/health）+ M1 权限内核（principal/scope/inject + scope 判官子命令）。

mod auth;
mod chat;
mod corrector;
mod db;
mod embed;
mod direct;
mod graph;
mod inject;
mod llm;
mod meta;
mod pipeline;
mod principal;
mod scope;
mod viewspec;
mod wework;

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
    wework: wework::WeworkCfg,
    /// AGE 图最近定时刷新结果（健康检查可见）
    graph_status: Arc<std::sync::Mutex<String>>,
}

/// 元数据启动引导：建表 → 种子 upsert（指标/维度/术语/码表/join 边）→ 权限档案灌表+加载。
/// 三条路径（服务/ask/exec-sql）统一走这里——否则改注册表种子后不跑 `meta sync` 永不生效
/// （真踩过：新增 metric.time_col 种子在评测里全空，口径卡缺时间列钉不住）。
async fn bootstrap_meta(pg: &PgPool) -> anyhow::Result<()> {
    meta::migrate(pg).await?;
    meta::seed(pg).await?;
    inject::seed_rules(pg).await?;
    let n = inject::load_rules(pg).await?;
    tracing::info!("元数据引导完成：scope_binding 权限档案 {n} 张表");
    Ok(())
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

    // 引擎 A1 子命令：meta autodiscover —— 字典码列自动对码注册（数据驱动，字典变了重跑即自适应）
    if args.len() >= 3 && args[1] == "meta" && args[2] == "autodiscover" {
        let mysql = db::mysql_pool(&cfg.mysql_url).await?;
        let pg = db::pg_pool(&cfg.pg_url).await?;
        meta::migrate(&pg).await?;
        let r = meta::autodiscover_dict_columns(&mysql, &pg).await?;
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }

    // 子命令：review-pending —— 批量复核 pending 语料（SuperSonic MemoryReviewTask）
    if args.len() >= 2 && args[1] == "review-pending" {
        let pg = db::pg_pool(&cfg.pg_url).await?;
        let client = llm_client(&cfg);
        let n = pipeline::review_all_pending(&client, &pg, 100).await?;
        println!("复核处理 {n} 条 pending 语料");
        return Ok(());
    }

    // 引擎 C 子命令：review-lessons —— 批量复核失败复盘产出的候选教训（candidate → active/disabled）
    if args.len() >= 2 && args[1] == "review-lessons" {
        let pg = db::pg_pool(&cfg.pg_url).await?;
        let client = llm_client(&cfg);
        let n = pipeline::review_lessons(&client, &pg, 100).await?;
        println!("复核处理 {n} 条候选教训");
        return Ok(());
    }

    // 子命令：check-sql "<sql>" —— SchemaCorrector 字段校验冒烟
    if args.len() >= 3 && args[1] == "check-sql" {
        let pg = db::pg_pool(&cfg.pg_url).await?;
        match corrector::schema_check(&pg, &args[2]).await? {
            Some(hint) => println!("发现幻觉列:\n{hint}"),
            None => println!("OK 字段全部合法"),
        }
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
        bootstrap_meta(&pg).await?;
        let client = llm_client(&cfg);
        let p = principal::load_principal(&mysql, &args[2], args.get(4).map(|s| s.as_str())).await?;
        let r = pipeline::ask(&client, &mysql, &pg, &p, &args[3], None).await?;
        println!("{}", serde_json::to_string(&r)?);
        return Ok(());
    }

    // 评测子命令：exec-sql <login_name> "<sql>" [role_code] —— 以该用户身份执行给定 SQL。
    // 三道防线一个不少（只读红线 → 权限注入 → 只读连接），供 tools/evaluation.py 跑 gold SQL 对拍。
    if args.len() >= 4 && args[1] == "exec-sql" {
        let mysql = db::mysql_pool(&cfg.mysql_url).await?;
        let pg = db::pg_pool(&cfg.pg_url).await?;
        bootstrap_meta(&pg).await?;
        let p = principal::load_principal(&mysql, &args[2], args.get(4).map(|s| s.as_str())).await?;
        let sets = scope::compute_scope_cached(&mysql, &p).await?;
        pipeline::is_safe_select(&args[3])?;
        let injected = inject::inject(&args[3], &sets)?;
        let t0 = std::time::Instant::now();
        let (columns, rows) = pipeline::execute(&mysql, &injected).await?;
        println!(
            "{}",
            serde_json::json!({
                "sql": injected,
                "columns": columns,
                "rows": rows,
                "row_count": rows.len(),
                "elapsed_ms": t0.elapsed().as_millis() as u64,
            })
        );
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
    bootstrap_meta(&pg).await?;
    chat::migrate(&pg).await?;

    let graph_status = Arc::new(std::sync::Mutex::new(String::from("never")));
    let state = Arc::new(AppState {
        mysql,
        pg,
        llm: llm_client(&cfg),
        dms_base_url: cfg.dms_base_url.clone(),
        wework: wework::WeworkCfg {
            corpid: cfg.wework_corpid.clone(),
            secret: cfg.wework_secret.clone(),
            agentid: cfg.wework_agentid.clone(),
        },
        graph_status: graph_status.clone(),
    });

    // M6c：AGE 图 nightly 定时刷新（本地 03:00 低谷期，一次性全量重建 ~4min；
    // 失败记 warn 次日重试，不影响服务）。图数据当日增量靠次日刷新补齐。
    {
        let mysql = state.mysql.clone();
        let pg = state.pg.clone();
        tokio::spawn(async move {
            loop {
                let wait = secs_until_next_3am();
                tracing::info!("graph sync 定时刷新：{wait}s 后（下个本地 03:00）执行");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                tracing::info!("graph sync 定时刷新开始");
                let msg = match graph::sync(&mysql, &pg).await {
                    Ok((c, g, e)) => {
                        format!("ok {} customers={c} goods={g} edges={e}", chrono::Local::now().format("%F %T"))
                    }
                    Err(e) => {
                        format!("fail {} {e}", chrono::Local::now().format("%F %T"))
                    }
                };
                if msg.starts_with("ok") {
                    tracing::info!("graph sync 完成：{msg}");
                } else {
                    tracing::warn!("graph sync 失败（次日重试）：{msg}");
                }
                *graph_status.lock().unwrap() = msg;
            }
        });
    }
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/sso", post(api_sso))
        .route("/api/wework/login", get(api_wework_login))
        .route("/api/ask", post(api_ask))
        .route("/api/convs", get(api_convs))
        .route("/api/conv/new", post(api_conv_new))
        .route("/api/conv/{id}", get(api_conv_msgs).delete(api_conv_delete))
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
struct WeworkQuery {
    code: String,
}

/// 企微 OAuth 回调：code → 员工 → 会话 token，302 重定向前端带 token
async fn api_wework_login(
    State(st): State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<WeworkQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match wework::login_by_code(&st.wework, &st.mysql, &q.code).await {
        Ok(login_name) => {
            let token = auth::issue(login_name, None);
            // 重定向前端，会话 token 走 fragment（不进服务端日志）
            axum::response::Redirect::to(&format!("/#token={token}")).into_response()
        }
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct AskReq {
    question: String,
    /// 开发/内网模式的直接身份传递；生产走 Authorization Bearer 会话 token
    login_name: Option<String>,
    role_code: Option<String>,
    /// 归属会话 id（多轮问答存进同一会话）
    conv_id: Option<i64>,
}

/// 从 header/body 解析身份（Bearer 会话 token 优先，回退 login_name）
fn resolve_identity(headers: &axum::http::HeaderMap, ln: &Option<String>, rc: &Option<String>) -> Option<(String, Option<String>)> {
    match bearer(headers).and_then(|t| auth::resolve(&t)) {
        Some((l, r)) => Some((l, r)),
        None => ln.clone().map(|l| (l, rc.clone())),
    }
}

async fn api_ask(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<AskReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    let (login_name, role_code) = resolve_identity(&headers, &req.login_name, &req.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name".into()))?;
    let p = principal::load_principal(&st.mysql, &login_name, role_code.as_deref())
        .await
        .map_err(|e| err(StatusCode::FORBIDDEN, e.to_string()))?;
    // 会话归属校验：非属主禁止读写（防越权借他人 conv_id 泄露上一问/写入消息）
    if let Some(cid) = req.conv_id {
        match chat::conv_owner(&st.pg, cid).await {
            Ok(Some(owner)) if owner == login_name => {}
            Ok(_) => return Err(err(StatusCode::FORBIDDEN, "无权访问该会话".into())),
            Err(e) => return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        }
    }
    // 多轮追问改写用的上一轮问题（同会话）
    let prev = match req.conv_id {
        Some(cid) => chat::last_question(&st.pg, cid).await.ok().flatten(),
        None => None,
    };
    let r = pipeline::ask(&st.llm, &st.mysql, &st.pg, &p, &req.question, prev.as_deref())
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let payload = serde_json::to_value(&r).unwrap();
    // 存会话消息（用户问 + AI 结果），首问顺手设标题
    if let Some(cid) = req.conv_id {
        let _ = chat::save_msg(&st.pg, cid, "user", &req.question, None).await;
        let _ = chat::save_msg(&st.pg, cid, "ai", "", Some(&payload)).await;
    }
    Ok(Json(payload))
}

#[derive(serde::Deserialize)]
struct ConvQuery {
    login_name: Option<String>,
}

async fn api_convs(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&headers, &q.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    let convs = chat::list_convs(&st.pg, &login).await.unwrap_or_default();
    Ok(Json(serde_json::json!({ "convs": convs })))
}

async fn api_conv_new(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(q): Json<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&headers, &q.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    let id = chat::new_conv(&st.pg, &login)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))))?;
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn api_conv_msgs(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::extract::Query(q): axum::extract::Query<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&headers, &q.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    // 会话归属校验（防越权读他人会话）
    match chat::conv_owner(&st.pg, id).await {
        Ok(Some(owner)) if owner == login => {}
        _ => return Err((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "无权访问该会话" })))),
    }
    let msgs = chat::conv_msgs(&st.pg, id).await.unwrap_or_default();
    Ok(Json(serde_json::json!({ "msgs": msgs })))
}

async fn api_conv_delete(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::extract::Query(q): axum::extract::Query<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&headers, &q.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    let _ = chat::delete_conv(&st.pg, id, &login).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 距下一个本地 03:00 的秒数（AGE 图 nightly 刷新，对齐业务低谷）
fn secs_until_next_3am() -> u64 {
    let now = chrono::Local::now();
    let Some(t3) = now.date_naive().and_hms_opt(3, 0, 0) else {
        return 3600;
    };
    let Some(today3) = t3.and_local_timezone(chrono::Local).single() else {
        return 3600;
    };
    let target = if now < today3 { today3 } else { today3 + chrono::Duration::days(1) };
    (target - now).num_seconds().max(60) as u64
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.to_string())
}

async fn health(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {    let mysql_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
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
        "graph_sync": st.graph_status.lock().map(|s| s.clone()).unwrap_or_default(),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn next_3am_within_a_day() {
        // 下次 03:00 必在 (60s, 24h] 内
        let s = super::secs_until_next_3am();
        assert!((60..=24 * 3600).contains(&s), "{s}");
    }
}
