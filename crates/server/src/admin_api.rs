//! 【K6-C】管理面 CRUD：术语 / SQL 示例复核 / 数据源授权。变更原因＝运营管理协议。
//!
//! 四条纪律：
//! 1. **一律 admin_only**：判据只有 `principal.administrator_flag`（DMS 库里的位），不认前端传的
//!    `role_code == "admin"`——那是可伪造的串。
//! 2. **列表全分页**（缺省 50 / 上限 200）：`meta.sql_exemplar` 存的是他人问句与 SQL 全文。
//! 3. **术语的 `ds_id` 过白名单**（`'dms'` / `'*'` / 已登记的源）：乱填的 ds_id 永远匹配不上
//!    `registry::ds_pred` 的 `ds_id IN ($ds,'*')`——**写进去了却永不生效**，是最难查的一类
//!    「配置没生效」（表里明明有这一行）。
//! 4. **`kb.acl` 的 grantee 只有 login / role**：类型与写入全用 `dms_knowledge::acl`
//!    （`AclEntry`/`Grantee`/`Perm`），不在 server 建第二套 ACL 实现。
//!
//! ## 刻意不提供「新增 SQL 示例」端点
//! `meta.sql_exemplar` 只许来自**真实问答 + 人工复核**，故本文件只有「列表 / 改状态 / 删」。
//! 手工塞的示例会把错口径写进 few-shot，而 few-shot 是**自我传播**的：LLM 照错示例生成的 SQL
//! 又会沉淀成新示例（`pipeline` 的语料沉淀），一条手滑污染整条口径链，事后无法从结果反查源头。
//! 复核通道（`POST /api/admin/exemplars/{id}/status`）是唯一入口。
//!
//! 🔴 这句话此前写的是「`ROUTES` + 单测钉住它」——**那是假的**，交叉审实测：
//! 往 `main.rs` 的路由表加一条真的 `POST /api/admin/exemplars`，143 条单测 **0 red**。
//! `ROUTES` 是本文件测试里自造的字面量，与 wire 侧手写的 `.route(...)` 零编译期连接。
//! 现在 `no_create_exemplar_route` 改成 `include_str!("main.rs")` 反查真实路由表 ——
//! 「唯一入口」这件事才第一次真的有判据。

use crate::AppState;
use dms_semantic::registry::datasource as ds_reg;
use dms_connector::SqlSource;
use dms_connector::registry::DsSpec;
use crate::dms_policy::principal;
use axum::{extract::{Path, Query, State}, http::{HeaderMap, StatusCode}, Json};
use dms_kernel::DsId;
use dms_knowledge::acl::{self, AclEntry, AclScope, Grantee, Perm};
use std::sync::Arc;

pub(crate) type ApiErr = (StatusCode, Json<serde_json::Value>);
pub(crate) type ApiOk = Json<serde_json::Value>;
pub(crate) type ApiRes = Result<ApiOk, ApiErr>;
type St = State<Arc<AppState>>;
/// 身份字段 `(login_name, role_code)`：各 query/body 都能给出这一对（D4，不连排 6 个 `&str`）
type Ident<'a> = (&'a Option<String>, &'a Option<String>);

/// exemplar 纪律相关的路由快照（**不是本模块全量端点清单**：terms.csv / exemplars.csv /\r
/// bulk status / table-enabled / schema-comments / sql-edit / llm-config / db-config 等不在其列）。\r
/// **唯一消费者是本文件单测**（`no_create_exemplar_route`）——
/// 所以是 `#[cfg(test)]`：wire 侧那张表（`main.rs` 的 `.route(...)` 逐条）是**手写**的，
/// 与本表零编译期连接，改路由要人工同步两处。
///
/// 原来这里挂着 `#[allow(dead_code)] // 消费者＝wire 侧路由表 + 本文件单测` —— 前半句是假的
/// （wire 侧一次都没引用它，`grep ROUTES` 在 main.rs 只命中一行注释）。删掉 allow 实测冒出
/// `warning: constant ROUTES is never used`（bin crate 里 `pub` 不代表对外可达），
/// 正说明「它只活在测试里」；用 `cfg(test)` 把这件事写进代码，而不是用 allow 把警告压掉。
#[cfg(test)]
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/admin/terms"), ("POST", "/api/admin/terms"), ("DELETE", "/api/admin/terms"),
    ("GET", "/api/admin/exemplars"), ("POST", "/api/admin/exemplars/{id}/status"),
    ("DELETE", "/api/admin/exemplars/{id}"),
    ("POST", "/api/ds/{id}/grant"), ("DELETE", "/api/ds/{id}/grant"),
];

/// 沿用现有 `{"error": msg}` 形状（前端只认这一种）
pub(crate) fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// DB/连接错误一律 500，但绝不把驱动错误链返回浏览器：其中可能包含 host、库名或 SQL。
fn db_err(e: impl std::fmt::Display) -> ApiErr {
    // 底层错误只进服务端日志：全文件 20+ 处 `.map_err(db_err)` 在此留痕，响应仍是固定文案
    tracing::warn!(error = %e, reason = "admin_db_failed", "管理面数据库操作失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "管理操作失败，请稍后重试；持续失败请联系管理员查看服务状态",
    )
}

/// 0 行影响即 404（F8「删除假成功」同款口径）；文案闭包仅在 404 时构造，成功路径零分配
fn affected(n: u64, what: impl FnOnce() -> String) -> Result<(), ApiErr> {
    if n == 0 { Err(err(StatusCode::NOT_FOUND, what())) } else { Ok(()) }
}

/// admin_only 判据：**只认 `administrator_flag`**（与 `ds_api::is_admin` 同一口径；
/// `usage_api::is_admin` 多认 `role_code == "admin"` 是其全局块口径，差异属各自语义）
fn is_admin(p: &principal::Principal) -> bool {
    p.administrator_flag
}

/// 管理面统一入口（给本文件外的端点用：`artifact_api` 等）。admin_only 判据只认
/// `administrator_flag` —— 与 `ds_api::is_admin` 同一口径，别开第二份判据。
pub async fn admin_only(st: &AppState, h: &HeaderMap, id: Ident<'_>) -> Result<principal::Principal, ApiErr> {
    admin(st, h, id).await
}

/// 剥 `Bearer ` 前缀：RFC 6750 的 scheme **大小写不敏感**（`bearer xxx` 也是合法写法）。
fn bearer_token(v: &str) -> Option<&str> {
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token)
}

/// 系统设置比普通管理面更窄：只有 DMS 登录名精确为 `admin` 的管理员可读写配置。
/// 前端隐藏按钮只是体验，这里才是不能绕过的权限边界。
pub async fn settings_admin_only(
    st: &AppState,
    h: &HeaderMap,
    _id: Ident<'_>,
) -> Result<(), ApiErr> {
    // 设置面不继承 `insecure_login_fallback`：即使判官模式显式打开了自报身份，配置写端点
    // 仍只接受服务端签发的 Bearer 会话，避免 `?login_name=admin` 绕过认证。
    let (login, _) = h
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token)
        .and_then(crate::auth::resolve)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "系统设置需要有效会话 token"))?;
    if login != "admin" {
        return Err(err(StatusCode::FORBIDDEN, "系统设置仅 admin 账号可用"));
    }
    // 设置权限不是业务数据角色：精确 admin 即使有多个角色且尚未选择 active role，
    // 也只需按 DMS 员工状态与 administrator_flag 判定，不能被“请选择登录角色”阻塞。
    let administrator: Option<(Option<i8>,)> = st
        .auth_mysql
        .fixed(
            "SELECT administrator_flag FROM t_employee \
             WHERE login_name = ? AND deleted_flag = 0 AND disabled_flag = 0 LIMIT 1",
        )
        .bind(&login)
        .fetch_optional()
        .await
        .map_err(|_| err(
            StatusCode::SERVICE_UNAVAILABLE,
            "系统设置身份校验暂不可用，请稍后重试",
        ))?;
    if administrator.and_then(|(flag,)| flag).unwrap_or(0) != 1 {
        return Err(err(StatusCode::FORBIDDEN, "系统设置仅 admin 账号可用"));
    }
    Ok(())
}

/// ponytail: 与 `ds_api::{caller,admin}` 是同一段身份换算的第二份拷贝——两边都私有，互相取不到。
/// ARCHITECTURE §4.7 的 `mw/auth.rs`（T10 统一认证中间件）是它们的合并点。
async fn admin(st: &AppState, h: &HeaderMap, id: Ident<'_>) -> Result<principal::Principal, ApiErr> {
    let (login, role) = crate::resolve_identity(st, h, id.0, id.1)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let p = principal::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|_| err(
            StatusCode::SERVICE_UNAVAILABLE,
            "管理员身份校验暂不可用，请稍后重试",
        ))?;
    if !is_admin(&p) {
        return Err(err(StatusCode::FORBIDDEN, "管理面需要管理员权限"));
    }
    Ok(p)
}

/// 缺省 50、上限 200、下限 1（`limit=0` 查出空列表会被读成「没数据」）
fn page_limit(n: Option<i64>) -> i64 {
    n.unwrap_or(50).clamp(1, 200)
}

fn page_offset(n: Option<i64>) -> i64 {
    n.unwrap_or(0).max(0)
}

// ─────────────────────────── 术语 meta.term ───────────────────────────

#[derive(serde::Deserialize, Default)]
pub struct TermQuery {
    ds_id: Option<String>,
    /// 只有 DELETE 用（按 `(ds_id, term)` 主键删）
    term: Option<String>,
    limit: Option<i64>, offset: Option<i64>,
    login_name: Option<String>, role_code: Option<String>,
}

/// **管理面按 ds_id 精确过滤**，不是召回的 `ds_id IN ($ds,'*')`：管理的是「这一行落在哪个源」，
/// 把全局条目混进某个源的列表，删的时候就会误删跨源共享的口径（故本文件不拼 `registry::ds_pred`）。
const TERM_LIST_SQL: &str = "SELECT ds_id, term, definition, aliases, status FROM meta.term \
     WHERE ($1::text IS NULL OR ds_id = $1) ORDER BY ds_id, term LIMIT $2 OFFSET $3";
const TERM_UPSERT_SQL: &str =
    "INSERT INTO meta.term(ds_id, term, definition, aliases, status) VALUES ($1,$2,$3,$4,$5) \
     ON CONFLICT (ds_id, term) DO UPDATE SET definition=$3, aliases=$4, status=$5";
const TERM_DELETE_SQL: &str = "DELETE FROM meta.term WHERE ds_id=$1 AND term=$2";

/// `(ds_id, term, definition, aliases, status)`
type TermRow = (String, String, String, Vec<String>, String);

fn term_json(t: &TermRow) -> serde_json::Value {
    serde_json::json!({ "ds_id": t.0, "term": t.1, "definition": t.2, "aliases": t.3, "status": t.4 })
}

/// `GET /api/admin/terms?ds_id=&limit=&offset=`
pub async fn terms(State(st): St, h: HeaderMap, Query(q): Query<TermQuery>) -> ApiRes {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    let limit = page_limit(q.limit);
    let rows: Vec<TermRow> = st.owned.fixed(TERM_LIST_SQL)
        .bind(q.ds_id).bind(limit).bind(page_offset(q.offset))
        .fetch_all().await.map_err(db_err)?;
    let terms: Vec<serde_json::Value> = rows.iter().map(term_json).collect();
    Ok(Json(serde_json::json!({ "terms": terms, "limit": limit })))
}

#[derive(serde::Deserialize)]
pub struct TermUpsertReq {
    term: String, definition: String,
    ds_id: Option<String>, aliases: Option<Vec<String>>, status: Option<String>,
    login_name: Option<String>, role_code: Option<String>,
}

/// `POST /api/admin/terms` —— upsert（admin_only）。**改术语不再需要改代码种子重启**。
pub async fn upsert_term(State(st): St, h: HeaderMap, Json(req): Json<TermUpsertReq>) -> ApiRes {
    admin(&st, &h, (&req.login_name, &req.role_code)).await?;
    let known = ds_ids(&st).await?;
    let t = validate_term(&req, &known).map_err(|m| err(StatusCode::BAD_REQUEST, m))?;
    st.owned.fixed(TERM_UPSERT_SQL)
        .bind(&t.0).bind(&t.1).bind(&t.2).bind(&t.3).bind(&t.4)
        .execute().await.map_err(db_err)?;
    Ok(Json(term_json(&t)))
}

/// `DELETE /api/admin/terms?term=&ds_id=`（admin_only）。走 query 而非 body：`(ds_id, term)`
/// 是主键，且 DELETE 带 body 在网关/代理侧常被丢。
pub async fn delete_term(State(st): St, h: HeaderMap, Query(q): Query<TermQuery>) -> ApiRes {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    let term = q.term.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let term = term.ok_or_else(|| err(StatusCode::BAD_REQUEST, "缺 term 参数"))?;
    let ds = q.ds_id.unwrap_or_else(|| ds_reg::DMS_DS_ID.into());
    let n = st.owned.fixed(TERM_DELETE_SQL).bind(&ds).bind(term).execute().await.map_err(db_err)?;
    affected(n, || format!("术语 {ds}/{term} 不存在"))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 已登记的 ds_id（术语白名单的动态部分）
async fn ds_ids(st: &AppState) -> Result<Vec<String>, ApiErr> {
    let rows = ds_reg::list_datasources(st.owned.pool()).await.map_err(db_err)?;
    Ok(rows.into_iter().map(|d| d.ds_id).collect())
}

/// 术语 `ds_id` 白名单：`'dms'`（主源）/ `'*'`（全局生效）/ 已登记的源。见纪律 3。
fn term_ds_ok(ds_id: &str, known: &[String]) -> bool {
    ds_id == ds_reg::DMS_DS_ID || ds_id == DsId::GLOBAL || known.iter().any(|k| k == ds_id)
}

/// 请求 → `meta.term` 行。缺省 `ds_id='dms'`、`status='active'`（与建表默认值一致）。
fn validate_term(r: &TermUpsertReq, known: &[String]) -> Result<TermRow, String> {
    // 先 trim 再判白名单："dms " 这类输入不该撞上令人困惑的白名单报错
    let ds_id = r.ds_id.as_deref().map(str::trim).unwrap_or(ds_reg::DMS_DS_ID).to_string();
    if !term_ds_ok(&ds_id, known) {
        return Err(format!("ds_id 只能是 dms | * | 已登记的数据源：{ds_id}"));
    }
    let term = r.term.trim();
    if term.is_empty() || term.chars().count() > 64 {
        return Err(format!("term 不能为空且 ≤64 字符：{term}"));
    }
    let def = r.definition.trim();
    if def.is_empty() || def.chars().count() > 2000 {
        return Err("definition 不能为空且 ≤2000 字符".into());
    }
    // 入库的是 trim 后的值：" GMV" 原样落库会让召回匹配落空
    let aliases: Vec<String> =
        r.aliases.clone().unwrap_or_default().iter().map(|a| a.trim().to_string()).collect();
    if aliases.len() > 20 || aliases.iter().any(|a| a.is_empty()) {
        return Err("aliases 最多 20 个且不许有空串（空别名会命中任意问句）".into());
    }
    let status = r.status.as_deref().map(str::trim).unwrap_or("active").to_string();
    if !matches!(status.as_str(), "active" | "disabled") {
        return Err(format!("status 只能是 active | disabled：{status}"));
    }
    Ok((ds_id, term.into(), def.into(), aliases, status))
}

// ──────────────── SQL 示例 meta.sql_exemplar（只复核，不新增）────────────────

#[derive(serde::Deserialize, Default)]
pub struct ExemplarQuery {
    status: Option<String>, limit: Option<i64>, offset: Option<i64>,
    login_name: Option<String>, role_code: Option<String>,
}

/// `created_at::text` 而非原生 timestamptz：省掉一个时间类型 feature（零新增依赖）
const EX_LIST_SQL: &str =
    "SELECT id, ds_id, question, sql, status, validation_status, ai_review, reviewed_by, \
            COALESCE(reviewed_at::text,''), COALESCE(validated_at::text,''), validated_source, \
            validated_fingerprint, invalid_reason, metric_versions, created_at::text \
     FROM meta.sql_exemplar \
     WHERE ($1::text IS NULL OR status = $1) ORDER BY id DESC LIMIT $2 OFFSET $3";
const EX_DISABLE_SQL: &str = "UPDATE meta.sql_exemplar SET status='disabled', reviewed_by=$1, reviewed_at=now() WHERE id=$2";
const EX_DELETE_SQL: &str = "DELETE FROM meta.sql_exemplar WHERE id=$1";
const EX_VALIDATE_GET_SQL: &str = "SELECT ds_id, question, sql FROM meta.sql_exemplar WHERE id=$1";
const EX_VALIDATE_OK_SQL: &str =
    "UPDATE meta.sql_exemplar SET status='enabled', validation_status='valid', reviewed_by=$1, \
     reviewed_at=now(), validated_at=now(), validated_source=$2, validated_fingerprint=$3, \
     invalid_reason='', metric_versions=$4 WHERE id=$5";
const EX_VALIDATE_BAD_SQL: &str =
    "UPDATE meta.sql_exemplar SET status='disabled', validation_status='invalid', reviewed_by=$1, \
     reviewed_at=now(), validated_at=now(), validated_source=$2, invalid_reason=$3 WHERE id=$4";
const EX_FINGERPRINT_SQL: &str = "SELECT encode(sha256($1::bytea),'hex')";
const EX_STALE_SOURCE_SQL: &str =
    "UPDATE meta.sql_exemplar SET validation_status='stale', invalid_reason='业务数据源已切换，需要重新执行验证' \
     WHERE validation_status='valid' AND ds_id=$1 AND validated_source<>$2";

/// SQL 样例审计行。元组保持固定查询通道，不引入只用一次的 DTO。
type ExRow = (i64, String, String, String, String, String, String, String, String, String, String, String, String, String, String);
type ExValidateRow = (String, String, String);

/// 复核态三值（`pipeline` 沉淀出来的是 `pending`）
fn exemplar_status_ok(s: &str) -> bool {
    matches!(s, "pending" | "enabled" | "disabled")
}

/// **人工复核只许置 enabled / disabled**：退回 `pending` 没有运营含义——复核队列按
/// `status='pending'` 取，退回等于让同一条反复排队。
fn review_status_ok(s: &str) -> bool {
    matches!(s, "enabled" | "disabled")
}

fn ex_json(e: &ExRow) -> serde_json::Value {
    serde_json::json!({
        "id": e.0, "ds_id": e.1, "question": e.2, "sql": e.3, "status": e.4,
        "validation_status": e.5, "ai_review": e.6, "reviewed_by": e.7,
        "reviewed_at": e.8, "validated_at": e.9, "validated_source": e.10,
        "validated_fingerprint": e.11, "invalid_reason": e.12,
        "metric_versions": e.13, "created_at": e.14,
    })
}

/// `GET /api/admin/exemplars?status=&limit=&offset=`
pub async fn exemplars(State(st): St, h: HeaderMap, Query(q): Query<ExemplarQuery>) -> ApiRes {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    // 非法 status 当场拒：静默返回空列表会被读成「没有待复核的示例」
    if q.status.as_deref().is_some_and(|s| !exemplar_status_ok(s)) {
        return Err(err(StatusCode::BAD_REQUEST, "status 只能是 pending | enabled | disabled"));
    }
    let limit = page_limit(q.limit);
    let rows: Vec<ExRow> = st.owned.fixed(EX_LIST_SQL)
        .bind(q.status).bind(limit).bind(page_offset(q.offset))
        .fetch_all().await.map_err(db_err)?;
    let items: Vec<serde_json::Value> = rows.iter().map(ex_json).collect();
    Ok(Json(serde_json::json!({ "exemplars": items, "limit": limit })))
}

#[derive(serde::Deserialize)]
pub struct StatusReq {
    status: String, login_name: Option<String>, role_code: Option<String>,
}

/// `POST /api/admin/exemplars/{id}/status` —— 人工复核（admin_only）
pub async fn set_exemplar_status(
    State(st): St, h: HeaderMap, Path(id): Path<i64>, Json(req): Json<StatusReq>,
) -> ApiRes {
    let p = admin(&st, &h, (&req.login_name, &req.role_code)).await?;
    if !review_status_ok(&req.status) {
        return Err(err(StatusCode::BAD_REQUEST, "复核只能置 enabled | disabled"));
    }
    if req.status == "disabled" {
        let n = st.owned.fixed(EX_DISABLE_SQL).bind(&p.login_name).bind(id)
            .execute().await.map_err(db_err)?;
        affected(n, || format!("示例 {id} 不存在"))?;
        return Ok(Json(serde_json::json!({ "id": id, "status": "disabled" })));
    }
    validate_exemplar(&st, &p, id).await
}

async fn validate_exemplar(st: &AppState, p: &principal::Principal, id: i64) -> ApiRes {
    let row: ExValidateRow = st.owned.fixed(EX_VALIDATE_GET_SQL).bind(id)
        .fetch_optional().await.map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("示例 {id} 不存在")))?;
    let (ds, question, sql) = row;
    let (source, ds_global, target): (Arc<dyn SqlSource>, bool, String) = if ds == ds_reg::DMS_DS_ID {
        (st.mysql.clone(), false, st.mysql.target_name())
    } else {
        let row = ds_reg::get_datasource(st.owned.pool(), &ds).await.map_err(db_err)?
            .ok_or_else(|| err(StatusCode::UNPROCESSABLE_ENTITY, format!("数据源 {ds} 不存在")))?;
        let spec = DsSpec {
            ds_id: DsId::new(&row.ds_id),
            kind: ds_reg::source_kind(&row.kind)
                .ok_or_else(|| err(StatusCode::UNPROCESSABLE_ENTITY, format!("数据源 {ds} 类型非法")))?,
            dsn_ref: row.dsn_ref,
            max_conn: 2,
            schema: dms_knowledge::tabular::upload_schema_of_ds(&row.ds_id),
        };
        let global = row.policy_kind == "global";
        let source = st.sources.get(&spec).await.map_err(db_err)?;
        (source, global, ds.clone())
    };

    let tables: Vec<String> = dms_kernel::sql::lex::from_table_aliases(&sql)
        .into_iter().map(|(table, _)| table).collect();
    let rules = dms_semantic::registry::caliber::build_rules(st.owned.pool(), &ds, &question, &tables)
        .await.map_err(db_err)?;
    let violations = dms_kernel::check_caliber(&sql, &rules);
    if !violations.is_empty() {
        let reason = violations.iter().take(3)
            .map(|v| format!("{}：{}", v.rule, v.hint)).collect::<Vec<_>>().join("；");
        mark_exemplar_invalid(st, p, id, &target, &reason).await?;
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, format!("口径验证未通过：{reason}")));
    }

    let scope = dms_policy::scope::compute_scope_cached(&st.auth_mysql, p).await.map_err(db_err)?;
    let scoped = match dms_agent::gate_on(p, &sql, &scope, ds_global, source.dialect()) {
        Ok(s) => s,
        Err(e) => {
            let reason = format!("安全闸门未通过：{e}");
            mark_exemplar_invalid(st, p, id, &target, &reason).await?;
            return Err(err(StatusCode::UNPROCESSABLE_ENTITY, reason));
        }
    };
    if source
        .fetch(&scoped, dms_agent::MAX_ROWS, dms_agent::EXEC_TIMEOUT)
        .await
        .is_err()
    {
        // 驱动错误链可能包含 host、库名、SQL 或服务端原文：既不回浏览器，也不写入复核表。
        let reason = "真实只读执行失败：请检查数据源、字段、权限与只读限制";
        mark_exemplar_invalid(st, p, id, &target, reason).await?;
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, reason));
    }

    let fingerprint: (String,) = st.owned.fixed(EX_FINGERPRINT_SQL)
        .bind(sql.as_bytes().to_vec()).fetch_optional().await.map_err(db_err)?
        .ok_or_else(|| db_err("SQL 指纹计算无返回"))?;
    let metrics = dms_semantic::registry::model::load_metric_policies(st.owned.pool(), &ds)
        .await.map_err(db_err)?;
    let versions = metrics.iter()
        .filter(|m| dms_kernel::nl::text::match_word(&question, &m.name, &m.aliases).is_some())
        .map(|m| format!("{}@{}", m.metric_code, m.version))
        .collect::<Vec<_>>().join(",");
    let n = st.owned.fixed(EX_VALIDATE_OK_SQL)
        .bind(&p.login_name).bind(&target).bind(&fingerprint.0).bind(&versions).bind(id)
        .execute().await.map_err(db_err)?;
    affected(n, || format!("示例 {id} 不存在"))?;
    Ok(Json(serde_json::json!({
        "id": id, "status": "enabled", "validation_status": "valid",
        "validated_source": target, "metric_versions": versions,
    })))
}

async fn mark_exemplar_invalid(
    st: &AppState, p: &principal::Principal, id: i64, target: &str, reason: &str,
) -> Result<(), ApiErr> {
    st.owned.fixed(EX_VALIDATE_BAD_SQL)
        .bind(&p.login_name).bind(target).bind(reason).bind(id)
        .execute().await.map_err(db_err)?;
    Ok(())
}

/// `DELETE /api/admin/exemplars/{id}` —— 剔除（admin_only）
pub async fn delete_exemplar(
    State(st): St, h: HeaderMap, Path(id): Path<i64>, Query(q): Query<ExemplarQuery>,
) -> ApiRes {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    let n = st.owned.fixed(EX_DELETE_SQL).bind(id).execute().await.map_err(db_err)?;
    affected(n, || format!("示例 {id} 不存在"))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ─────────────────────────── 【B6】CSV 往返与批量复核 ───────────────────────────
// 术语全量导入导出（业务人员用表格软件维护口径）。示例库**只导出不导入**：
// 纪律在本文件头 —— 示例只许来自真实问答 + 人工复核，CSV 导入就是开后门绕过它。
// 60 条 pending 的复核走下面的批量 status 端点（与逐条版同一组 `review_status_ok` 校验）。

/// 导出上限：这两张表是人工维护的口径与沉淀的语料，量级几十到几百；
/// 上限是防「把导出当全表抽取工具」的闸，不是预期会碰到的数。
const CSV_MAX_ROWS: i64 = 5000;

/// 导入行数上限：CSV 导入是逐行串行 upsert，无上限时 2MB body 就是数千次串行 INSERT
///（bulk status 有 500 闸，同一纪律）。超限整体 400，不落半截数据。
const CSV_IMPORT_MAX_ROWS: usize = 1000;

/// CSV 字段转义（RFC 4180）：含 `"` `,` 或换行就整体加引号、内部引号翻倍。
fn csv_field(s: &str) -> String {
    if s.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// 逐字解析（**引号内可以吃换行与逗号**，`""` 是字面引号）。示例 SQL 必含逗号与换行，
/// 半吊子切逗号会在第一条就碎 —— 这就是为什么不引依赖也不能用 `split(',')`。
fn csv_parse(body: &str) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut field) = (vec![], vec![], String::new());
    let mut chars = body.chars().peekable();
    let mut in_quotes = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => in_quotes = false,
                _ => field.push(c),
            }
        } else {
            match c {
                '"' if field.is_empty() => in_quotes = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                '\r' => {} // \r\n 的 \r 丢掉
                _ => field.push(c),
            }
        }
    }
    // 结尾没换行时收尾最后一行（空 body / 只有尾换行时一行都不产）
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

fn csv_response(body: String) -> axum::response::Response {
    axum::response::IntoResponse::into_response((
        [(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")],
        body,
    ))
}

/// `GET /api/admin/terms.csv` —— 术语全量导出（aliases 用 `|` 连接：逗号已是分隔符）
pub async fn terms_csv(
    State(st): St, h: HeaderMap, Query(q): Query<TermQuery>,
) -> Result<axum::response::Response, ApiErr> {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    let rows: Vec<TermRow> = st.owned.fixed(TERM_LIST_SQL)
        .bind(q.ds_id).bind(CSV_MAX_ROWS).bind(0i64)
        .fetch_all().await.map_err(db_err)?;
    if rows.len() as i64 == CSV_MAX_ROWS {
        tracing::warn!(reason = "terms_csv_truncated", "术语导出达到 CSV_MAX_ROWS 上限，结果已截断");
    }
    let mut out = String::from("ds_id,term,definition,aliases,status\n");
    for t in &rows {
        out.push_str(&[&t.0, &t.1, &t.2, &t.3.join("|"), &t.4]
            .map(|f| csv_field(f))
            .join(","));
        out.push('\n');
    }
    Ok(csv_response(out))
}

/// `POST /api/admin/terms.csv?login_name=&role_code=` —— 导入并 upsert。
/// **逐行校验、坏行点名返回**：全有或全无会让一个错行挡住五十行好数据，
/// 静默跳过则会让人以为全都导进去了。身份走 query（body 是 CSV 本身）。
pub async fn import_terms_csv(
    State(st): St, h: HeaderMap, Query(q): Query<TermQuery>, body: String,
) -> ApiRes {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    let known = ds_ids(&st).await?;
    let rows = csv_parse(&body);
    // 表头行（导出文件的 round-trip）：首行五列全等才丢，长得像数据的「表头」按数据走
    let header = ["ds_id", "term", "definition", "aliases", "status"];
    let rows = match rows.first() {
        Some(r) if r.iter().map(String::as_str).eq(header) => &rows[1..],
        _ => &rows[..],
    };
    if rows.len() > CSV_IMPORT_MAX_ROWS {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("一次最多导入 {CSV_IMPORT_MAX_ROWS} 行，请拆分 CSV 分批导入"),
        ));
    }
    let mut ok = 0usize;
    let mut failed = vec![];
    for (i, r) in rows.iter().enumerate() {
        let line = i + 2; // 表头占第 1 行，报错按人看的行号
        let cell = |idx: usize| r.get(idx).cloned().unwrap_or_default();
        let req = TermUpsertReq {
            ds_id: Some(cell(0)),
            term: cell(1),
            definition: cell(2),
            aliases: Some(cell(3).split('|').map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()).collect()),
            status: Some(cell(4)),
            login_name: None,
            role_code: None,
        };
        match validate_term(&req, &known) {
            Err(m) => failed.push(serde_json::json!({ "line": line, "term": req.term, "error": m })),
            Ok(t) => {
                if st.owned.fixed(TERM_UPSERT_SQL)
                    .bind(&t.0).bind(&t.1).bind(&t.2).bind(&t.3).bind(&t.4)
                    .execute().await.is_err()
                {
                    tracing::warn!(line, reason = "term_import_row_failed", "术语 CSV 导入行保存失败");
                    failed.push(serde_json::json!({
                        "line": line,
                        "term": t.1,
                        "error": "保存失败，请检查术语字段后重试",
                    }));
                } else {
                    ok += 1;
                }
            }
        }
    }
    Ok(Json(serde_json::json!({ "ok": ok, "failed": failed })))
}

/// `GET /api/admin/exemplars.csv?status=` —— 示例导出（复核工作流的「取下来看」那一半；
/// 「放回去」那一半是批量 status，**没有** CSV 导入 —— 见段头纪律）。
pub async fn exemplars_csv(
    State(st): St, h: HeaderMap, Query(q): Query<ExemplarQuery>,
) -> Result<axum::response::Response, ApiErr> {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    if q.status.as_deref().is_some_and(|s| !exemplar_status_ok(s)) {
        return Err(err(StatusCode::BAD_REQUEST, "status 只能是 pending | enabled | disabled"));
    }
    let rows: Vec<ExRow> = st.owned.fixed(EX_LIST_SQL)
        .bind(q.status).bind(CSV_MAX_ROWS).bind(0i64)
        .fetch_all().await.map_err(db_err)?;
    if rows.len() as i64 == CSV_MAX_ROWS {
        tracing::warn!(reason = "exemplars_csv_truncated", "示例导出达到 CSV_MAX_ROWS 上限，结果已截断");
    }
    let mut out = String::from("id,ds_id,question,sql,status,validation_status,ai_review,reviewed_by,reviewed_at,validated_at,validated_source,validated_fingerprint,invalid_reason,metric_versions,created_at\n");
    for e in &rows {
        out.push_str(&[&e.0.to_string(), &e.1, &e.2, &e.3, &e.4, &e.5, &e.6, &e.7, &e.8, &e.9, &e.10, &e.11, &e.12, &e.13, &e.14]
            .map(|f| csv_field(f))
            .join(","));
        out.push('\n');
    }
    Ok(csv_response(out))
}

const EX_BULK_DISABLE_SQL: &str =
    "UPDATE meta.sql_exemplar SET status='disabled', reviewed_by=$1, reviewed_at=now() WHERE id = ANY($2::bigint[])";

// ───────────── 【A11】schema 注释业务自助维护（CSV 往返）─────────────
// 业务人员导出 → 表格软件填 → 回传。写 `custom_comment` 不写原生列（A2 分列纪律：
// sync 只写原生列、人工列谁都不许覆盖，渲染时人工列优先）。每个值过
// `dms_semantic::ingest::sanitize_comment` —— 与 schema sync **同一处信任边界**，
// 不在 admin 侧开第二份（上传的备注是不可信文本，`wrap_untrusted_schema` 防的就是它）。
// 只 UPDATE 已登记的行（表/列来自 schema sync，管理面不创造它们）：
// 拼错的表名按行点名，而不是静默造一行永远渲染不到的文档。

const DOC_TABLE_ROWS_SQL: &str = "SELECT table_name, custom_comment, table_comment \
     FROM meta.table_doc WHERE ds_id = $1 ORDER BY table_name";
const DOC_COL_ROWS_SQL: &str = "SELECT table_name, column_name, custom_comment, col_comment \
     FROM meta.column_doc WHERE ds_id = $1 ORDER BY table_name, ordinal";
const SET_TABLE_COMMENT_SQL: &str =
    "UPDATE meta.table_doc SET custom_comment = $1 WHERE table_name = $2 AND ds_id = $3";
const SET_COL_COMMENT_SQL: &str =
    "UPDATE meta.column_doc SET custom_comment = $1 WHERE table_name = $2 AND column_name = $3 AND ds_id = $4";

/// 【A20】人工勾选开关：**只 UPDATE**（与注释端点同一条「管理面不创造文档行」）
const SET_TABLE_ENABLED_SQL: &str =
    "UPDATE meta.table_doc SET enabled = $1 WHERE table_name = $2 AND ds_id = $3";

#[derive(serde::Deserialize)]
pub struct TableEnabledReq {
    table_name: String,
    enabled: bool,
    ds_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/table-enabled` —— 表级启停（admin_only）。
/// `enabled=false` 的表不进任何一路召回（向量/trgm 谓词 + render 总闸在
/// `recall/schema.rs`，drift 有同形守卫）。误采的业务表从此不用改 Rust 规则下线。
pub async fn set_table_enabled(
    State(st): St, h: HeaderMap, Json(req): Json<TableEnabledReq>,
) -> ApiRes {
    admin(&st, &h, (&req.login_name, &req.role_code)).await?;
    let table = req.table_name.trim();
    if table.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "table_name 不能为空"));
    }
    let ds = req.ds_id.as_deref().map(str::trim).unwrap_or(ds_reg::DMS_DS_ID).to_string();
    let n = st.owned.fixed(SET_TABLE_ENABLED_SQL)
        .bind(req.enabled).bind(table).bind(&ds)
        .execute().await.map_err(db_err)?;
    affected(n, || format!("表 {ds}/{table} 未登记（管理面只改已登记的，不创造）"))?;
    Ok(Json(serde_json::json!({ "ok": true, "table_name": table, "enabled": req.enabled })))
}

// ───────────────── 【双供应商】LLM 配置查看与热切换 ─────────────────
// 供应商目录（base_url/模型名/视觉能力）内建在 `db::provider_catalog`；
// key 只在 settings.json（`llm_keys` + `llm_api_key`）—— 本文件任何响应**不含 key**；
// 运行时开关落 `meta.kv['llm_provider']`（保存即生效，见 `set_conf` 的热锁）。

const KV_LLM_PROVIDER: &str = "llm_provider";
/// `pub(crate)`：S5 日报（daily_digest）的「今日已出」标记也用 meta.kv —— 同一张开关表。
pub(crate) const KV_GET_SQL: &str = "SELECT v FROM meta.kv WHERE k = $1";
pub(crate) const KV_SET_SQL: &str =
    "INSERT INTO meta.kv(k, v) VALUES ($1, $2) ON CONFLICT (k) DO UPDATE SET v = $2";

/// 当前生效的供应商：meta.kv 覆盖 settings.json 的文件供应商（文件供应商的解析在 db.rs）
async fn current_provider(st: &AppState) -> String {
    // 读取失败留痕再回落：`.ok().flatten()` 会把 DB 故障静默吞成「用文件配置」
    let kv: Option<(String,)> =
        match st.owned.fixed(KV_GET_SQL).bind(KV_LLM_PROVIDER).fetch_optional().await {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(error = %e, key = KV_LLM_PROVIDER, reason = "kv_read_failed", "meta.kv 读取失败，回落文件配置");
                None
            }
        };
    kv.map(|(v,)| v).filter(|v| !v.is_empty()).unwrap_or_else(|| file_provider(&st.cfg()))
}

fn file_provider(cfg: &crate::db::Settings) -> String {
    crate::db::file_provider_name(cfg)
}

/// 启动时应用一次运行时开关（`main.rs` 在 AppState 建好后调）。
/// kv 没有记录 = settings.json 的文件配置（与今天逐字等价）。
pub async fn apply_runtime_llm_provider(st: &AppState) {
    let name = current_provider(st).await;
    let cfg = st.cfg();
    let configs = crate::db::resolve_provider(&name, &cfg).and_then(|primary| {
        crate::db::resolve_fallback_vision(&cfg)
            .map(|fallback| (primary, fallback.map(|(_, conf)| conf)))
    });
    match configs {
        Ok((primary, fallback)) => {
            if st.llm.set_runtime_configs(primary, fallback).is_err() {
                tracing::warn!(provider = %name, reason = "invalid_runtime_config", "LLM 运行时配置应用失败，沿用启动配置");
            } else {
                tracing::info!(provider = %name, "LLM 主模型与备用视觉配置已原子生效");
            }
        }
        Err(_) => tracing::warn!(provider = %name, reason = "provider_resolution_failed", "LLM 运行时供应商解析失败，沿用启动配置"),
    }
}

/// 启动回落目标名：与 `settings.example.json` 的默认分析库条目同名，改名要同步示例配置与部署文档。
const BOOT_FALLBACK_TARGET: &str = "doris_warehouse";

/// 启动分析目标解析：只从 `mysql_targets` 选择非 `dms` 目标。
/// kv 无效时优先 `doris_warehouse`，再取首个分析目标；没有分析目标就响亮失败，绝不把
/// `mysql_url`（DMS 身份/权限库）当成问数回退。
pub async fn db_boot_target(
    owned: &dms_connector::OwnedStore,
    cfg: &crate::db::Settings,
) -> anyhow::Result<(String, String)> {
    // 同 current_provider：读取失败留痕再回落，不静默吞成「无 kv」
    let kv: Option<(String,)> =
        match owned.fixed(KV_GET_SQL).bind(KV_MYSQL_TARGET).fetch_optional().await {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(error = %e, key = KV_MYSQL_TARGET, reason = "kv_read_failed", "meta.kv 读取失败，按 settings.json 目录选库");
                None
            }
        };
    let requested = kv
        .map(|(v,)| v)
        .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("dms"));
    let targets = crate::db::db_targets(cfg);
    if let Some(name) = requested.as_deref() {
        // 与 settings_api matching_key 同口径：目标名大小写不敏感，沿用目录里的登记名
        if let Some((registered, url)) = targets.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            tracing::info!(target = %registered, "分析库目标按 kv 启动（运行时配置）");
            return Ok((registered.clone(), url.clone()));
        }
        tracing::warn!(target = %name, "分析库目标不在 settings.json 目录，改用配置中的分析目标");
    }
    let fallback = targets
        .iter()
        .find(|(name, _)| name == BOOT_FALLBACK_TARGET)
        .or_else(|| targets.first())
        .ok_or_else(|| anyhow::anyhow!(
            "settings.json 至少要显式配置一个非 dms 的 warehouse 或 production_lookup 查询目标；mysql_url 不会隐式成为查询目标"
        ))?;
    tracing::info!(target = %fallback.0, "分析库按配置默认目标启动");
    Ok(fallback.clone())
}

/// `GET /api/admin/llm-config` —— 目录 + 当前生效（**永远不含 key**，红线）。
fn provider_vision_json(model: Option<&str>) -> serde_json::Value {
    match model {
        Some(model) => serde_json::Value::String(model.to_owned()),
        None => serde_json::Value::Null,
    }
}

pub async fn llm_config(State(st): St, h: HeaderMap, Query(q): Query<DocQuery>) -> ApiRes {
    settings_admin_only(&st, &h, (&q.login_name, &q.role_code)).await?;
    // 以真实运行时快照为准：无效 KV 会在启动时被拒绝，不能继续在页面冒充“当前生效”。
    let current = st.llm.primary_provider();
    let cfg = st.cfg();
    let file = file_provider(&cfg);
    let catalog: Vec<serde_json::Value> = crate::db::provider_catalog()
        .iter()
        // 自定义同名条目完整覆盖内建目录；响应也只保留实际生效的那一条，避免页面
        // 将已显式关闭的 vision 能力与内建能力合并后误标为支持多模态。
        .filter(|(n, _)| !cfg.llm_providers.keys().any(|name| name.eq_ignore_ascii_case(*n)))
        .map(|(n, s)| {
            let provider = *n;
            let (thinking_url, thinking_extra) = if file.eq_ignore_ascii_case(provider) {
                (cfg.llm_base_url.as_str(), cfg.llm_extra_body.clone())
            } else {
                (s.base_url, serde_json::from_str(s.extra).unwrap_or_else(|e| {
                    // 内建预设的 extra 是编译期常量，解析失败是仓库级 bug，要响亮留痕
                    tracing::warn!(provider = %provider, error = %e, reason = "builtin_provider_extra_invalid", "内建供应商 extra 解析失败");
                    serde_json::Map::default()
                }))
            };
            serde_json::json!({
                "name": provider,
                "base_url": s.base_url,
                "model_fast": s.model_fast,
                "model_precise": s.model_precise,
                // 返回实际视觉模型，而不只是能力布尔值。设置页编辑内建供应商时必须
                // 保留真实模型名，否则会误把 fast 文本模型写成 vision 模型。
                "vision": provider_vision_json(s.vision),
                "thinking": crate::settings_api::configured_thinking_level(thinking_url, &thinking_extra),
                // 这家有没有 key 配置好（布尔，不是 key 本身 —— key 永不出响应）
                "key_ready": crate::db::provider_key_ready(&cfg, provider),
            })
        })
        .collect();
    // 自定义供应商（`llm_providers`）也进切换列表 —— 页面加的供应商要能当场被选中
    let custom: Vec<serde_json::Value> = cfg
        .llm_providers
        .iter()
        .map(|(n, c)| serde_json::json!({
            "name": n,
            "base_url": crate::db::public_service_url(&c.base_url),
            "model_fast": c.model_fast,
            "model_precise": if c.model_precise.is_empty() { &c.model_fast } else { &c.model_precise },
            "vision": provider_vision_json(c.vision.as_deref()),
            "thinking": crate::settings_api::configured_thinking_level(&c.base_url, &c.extra_body),
            "key_ready": crate::db::provider_key_ready(&cfg, n),
        }))
        .collect();
    let catalog: Vec<serde_json::Value> = catalog.into_iter().chain(custom).collect();
    let (base_url, fast, precise, vision, _) = st.llm.public_conf();
    Ok(Json(serde_json::json!({
        "provider": current,
        "providers": catalog,
        "effective": {
            "base_url": crate::db::public_service_url(&base_url),
            "model_fast": fast, "model_precise": precise,
            "vision": provider_vision_json(vision.as_deref()),
        },
    })))
}

#[derive(serde::Deserialize)]
pub struct LlmProviderReq {
    provider: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/llm-provider` —— 热切换供应商（保存即生效，**不需要重启**）。
/// 校验：必须在目录里且 key 已配置（`resolve_provider` 自己响亮报错，不静默落空）。
pub async fn set_llm_provider(
    State(st): St, h: HeaderMap, Json(req): Json<LlmProviderReq>,
) -> ApiRes {
    settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    let name = req.provider.trim();
    let cfg = st.cfg();
    let conf = crate::db::resolve_provider(name, &cfg)
        .map_err(|_| err(StatusCode::BAD_REQUEST,
            "模型切换未生效。请检查供应商、模型名称、Key 及能力配置"))?;
    let fallback = crate::db::resolve_fallback_vision(&cfg)
        .map_err(|_| err(StatusCode::BAD_REQUEST,
            "模型切换未生效。请检查备用多模态供应商、Key 及能力配置"))?
        .map(|(_, conf)| conf);
    let effective_name = conf.provider.clone();
    let old_provider = st.llm.primary_provider();
    let old_conf = crate::db::resolve_provider(&old_provider, &cfg)
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR,
            "当前模型运行时快照无法恢复，已拒绝切换"))?;
    let old_fallback = crate::db::resolve_fallback_vision(&cfg)
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR,
            "当前备用多模态运行时快照无法恢复，已拒绝切换"))?
        .map(|(_, conf)| conf);
    // 主模型与备用视觉作为一份快照热改；任一项非法时旧快照原样保留。
    st.llm.set_runtime_configs(conf, fallback).map_err(|_| err(StatusCode::BAD_REQUEST,
        "模型切换未生效。请检查供应商、模型名称、Key 及能力配置"))?;
    if st.owned.fixed(KV_SET_SQL)
        .bind(KV_LLM_PROVIDER).bind(&effective_name)
        .execute().await.is_err()
    {
        let rollback_failed = st.llm.set_runtime_configs(old_conf, old_fallback).is_err();
        if rollback_failed {
            tracing::error!(
                provider = %old_provider,
                reason = "runtime_rollback_failed",
                "LLM 供应商状态保存失败且运行时恢复失败",
            );
        }
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "模型状态保存失败，运行时已尝试恢复原模型，请稍后重试",
        ));
    }
    tracing::info!(provider = %effective_name, "LLM 供应商已热切换（保存即生效）");
    Ok(Json(serde_json::json!({ "ok": true, "provider": effective_name, "hot": true })))
}

// ───────────────────── 【分析库热切换】`mysql_targets` 的键─────────────────────
// 与 LLM 供应商同一模子：目录在 settings.json（DSN 红线：kv 只存名字，响应只给脱敏 host）、
// 先验证后热换（`swap_pool` 只读校验不过旧池原样）、`meta.kv['mysql_target']` 启动应用。
//
// `dms` 是固定身份/角色/数据权限源，只允许 `auth_mysql` 读取，绝不能切成分析查询池。
// ⚠️ 口径声明不跟库走：meta（指标/维度/码表/权限档案）是 **DMS schema** 的声明。
// 切到同构库（如中台同构镜像）一切照常；切到 schema 不同的库，声明会指到不存在的表 ——
// 那是响亮失败（闸门/执行报错），不是静默错答。

const KV_MYSQL_TARGET: &str = "mysql_target";

/// `GET /api/admin/db-config`：目标目录（脱敏 host）+ 当前生效目标。
pub async fn db_config(State(st): St, h: HeaderMap, Query(q): Query<DocQuery>) -> ApiRes {
    settings_admin_only(&st, &h, (&q.login_name, &q.role_code)).await?;
    let current = current_db_target(&st);
    let cfg = st.cfg();
    let mut targets = vec![serde_json::json!({
        "name": "dms",
        "host": crate::db::mask_dsn(&cfg.mysql_url),
        "current": false,
        "purpose": "identity_permission",
        "selectable": false,
        "protected": true,
        "builtin": true,
    })];
    targets.extend(crate::db::db_targets(&cfg)
        .iter()
        .map(|(name, url)| {
            let capability = crate::db::db_target_capability(&cfg, name);
            serde_json::json!({
                "name": name,
                "host": crate::db::mask_dsn(url),   // host:port/db —— 用户/口令永不出响应（红线）
                "type": match capability {
                    dms_connector::mysql::MysqlCapability::Warehouse => "warehouse",
                    dms_connector::mysql::MysqlCapability::ProductionLookup
                    | dms_connector::mysql::MysqlCapability::IdentityPermission => "production_lookup",
                },
                // 与 settings_api 同语义判断同一口径：目标名大小写不敏感
                "current": name.eq_ignore_ascii_case(&current),
                "purpose": match capability {
                    dms_connector::mysql::MysqlCapability::Warehouse => "analytics",
                    dms_connector::mysql::MysqlCapability::ProductionLookup
                    | dms_connector::mysql::MysqlCapability::IdentityPermission => "production_lookup",
                },
                "selectable": true,
                "protected": false,
                "builtin": false,
            })
        })
    );
    Ok(Json(serde_json::json!({
        "target": current,
        "targets": targets,
        "note": "口径声明（指标/维度/码表）按 DMS schema 登记；切到 schema 不同的库会响亮报错，不会静默错答",
    })))
}

/// 当前生效的分析目标名；连接池与名字由 `swap_pool_named` 同锁提交。
fn current_db_target(st: &AppState) -> String {
    st.mysql.target_name()
}

/// `/api/health` 用（名字不是凭据，可上报；host 不走这里，`mask_dsn` 也只给 admin 端点）
pub async fn current_db_target_pub(st: &AppState) -> String {
    current_db_target(st)
}

/// `graph_status` 带时间戳写入的一处收口：形状 `{state} {本地时间} target={name}[ {note}]`，
/// 锁与时间格式只此一份。恢复旧值是整体回填，不走这里。
fn set_graph_status(st: &AppState, state: &str, name: &str, note: &str) {
    let note = if note.is_empty() { String::new() } else { format!(" {note}") };
    *st.graph_status.lock().expect("graph status 锁中毒") = format!(
        "{state} {} target={name}{note}",
        chrono::Local::now().format("%F %T")
    );
}

/// 热切换后的持久状态：先提交已验证的新池，再写 kv；kv 失败就恢复旧池。
/// 这样 API 返回成功时，KV 与运行时一定指向同一个分析目标。
pub(crate) async fn persist_db_target(
    st: &AppState,
    name: &str,
    url: &str,
    capability: dms_connector::mysql::MysqlCapability,
) -> Result<(), ApiErr> {
    let old_name = st.mysql.target_name();
    let cfg = st.cfg();
    let old_capability = crate::db::db_target_capability(&cfg, &old_name);
    let old_url = crate::db::db_targets(&cfg)
        .into_iter()
        .find(|(target, _)| target.eq_ignore_ascii_case(&old_name))
        .map(|(_, url)| url)
        .ok_or_else(|| err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "当前分析目标配置已失效，请先修复目标目录后重试",
        ))?;
    let previous_graph_status = st.graph_status.lock().expect("graph status 锁中毒").clone();
    // 先关图、再换池：从连接池切换开始，任何新问答都不能读取旧目标的 AGE 快照。
    // 失效令牌只在换池本身失败或完整回滚成功时恢复；不确定状态一律保持不可用。
    let graph_invalidation = dms_connector::graph::invalidate_for_target(name);
    set_graph_status(st, "switching", name, "");
    if st.mysql.swap_pool_named(url, 10, name, capability).await.is_err() {
        let _ = dms_connector::graph::restore_after_failed_switch(graph_invalidation);
        *st.graph_status.lock().expect("graph status 锁中毒") = previous_graph_status;
        return Err(err(
            StatusCode::BAD_REQUEST,
            crate::settings_api::DB_SWITCH_GUIDANCE,
        ));
    }
    if st.owned.fixed(KV_SET_SQL)
        .bind(KV_MYSQL_TARGET).bind(name)
        .execute().await.is_err()
    {
        let rollback_ok = st.mysql
            .swap_pool_named(&old_url, 10, &old_name, old_capability)
            .await
            .is_ok();
        if rollback_ok {
            let _ = dms_connector::graph::restore_after_failed_switch(graph_invalidation);
            *st.graph_status.lock().expect("graph status 锁中毒") = previous_graph_status;
        } else {
            set_graph_status(st, "disabled", name, "rollback_failed");
            tracing::error!(target = %old_name, reason = "runtime_rollback_failed", "分析库切换持久化失败且旧池恢复失败");
        }
        return Err(err(StatusCode::INTERNAL_SERVER_ERROR,
            "分析目标状态保存失败，运行时已尝试恢复原目标，请稍后重试"));
    }
    Ok(())
}

/// 切换完成后的元数据维护不改变已提交目标。失败只记固定分类，避免底层错误带出 DSN。
pub(crate) async fn after_db_target_switch(st: &Arc<AppState>, name: &str) {
    if st.owned.fixed(EX_STALE_SOURCE_SQL)
        .bind(ds_reg::DMS_DS_ID).bind(name)
        .execute().await.is_err()
    {
        tracing::warn!(target = %name, reason = "exemplar_stale_mark_failed", "分析库切换后的示例失效标记失败");
    }
    tracing::info!(target = %name, "分析库已热切换（保存即生效）");

    if !st.mysql.is_warehouse() {
        set_graph_status(st, "disabled", name, "production_lookup");
        tracing::info!(target = %name, "production_lookup 热切换不执行通用 schema sync");
        return;
    }

    set_graph_status(st, "pending", name, "");

    // 切库后异步刷新 schema 与 AGE 图；切换响应不等全量重建，失败只留固定分类。
    let refresh = Arc::clone(st);
    tokio::spawn(async move {
        let assets = dms_semantic::warehouse_catalog::metadata_assets();
        match refresh.mysql.probe_schema_with_warehouse_catalog(&assets).await {
            Ok((mut snap, warehouse_catalog)) => {
                // 富化失败按 0 条计但要留痕：紧邻的 seed/sync 失败都有 warn
                let warehouse_comments = match refresh.mysql.enrich_dms_snapshot(&mut snap).await {
                    Ok(n) => n,
                    Err(_) => {
                        tracing::warn!(reason = "warehouse_comments_enrich_failed", "DMS 注释富化失败，按 0 条计");
                        0
                    }
                };
                match dms_semantic::ingest::schema_sync::sync_schema(
                    refresh.owned.pool(), ds_reg::DMS_DS_ID, &snap, true,
                )
                .await
                {
                    Ok((tables, columns)) => {
                        let catalog_seeded = dms_semantic::warehouse_catalog::seed(
                            refresh.owned.pool(),
                            ds_reg::DMS_DS_ID,
                        )
                        .await;
                        if catalog_seeded.is_err() {
                            tracing::warn!(
                                reason = "warehouse_catalog_seed_failed",
                                "分析库目录注释热刷新失败"
                            );
                        }
                        tracing::info!(
                            tables,
                            columns,
                            warehouse_comments,
                            warehouse_catalog_requested = warehouse_catalog.requested,
                            warehouse_catalog_tables = warehouse_catalog.tables,
                            warehouse_catalog_columns = warehouse_catalog.columns,
                            warehouse_catalog_missing = warehouse_catalog.missing,
                            catalog_seeded = catalog_seeded.is_ok(),
                            "分析库 schema 已热刷新"
                        );
                    }
                    Err(_) => tracing::warn!(reason = "schema_sync_failed", "分析库 schema 热刷新失败"),
                }
            }
            Err(_) => tracing::warn!(reason = "schema_probe_failed", "分析库 schema 探针失败"),
        }
        crate::graph_sync_and_record(&refresh).await;
    });
}

#[derive(serde::Deserialize)]
pub struct DbTargetReq {
    target: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/db-target` —— 热切换分析 MySQL/Doris（保存即生效）。
/// 先验证后换（`swap_pool` 内部：新池建不上/会话非只读 → 旧池原样），再落 kv。
pub async fn set_db_target(
    State(st): St, h: HeaderMap, Json(req): Json<DbTargetReq>,
) -> ApiRes {
    settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    let name = req.target.trim();
    if name.eq_ignore_ascii_case("dms") {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "DMS 权限库只用于身份、角色与数据权限校验，不能作为分析查询目标",
        ));
    }
    let cfg = st.cfg();
    let targets = crate::db::db_targets(&cfg);
    // 与 settings_api PUT 路径（matching_key）同口径：目标名大小写不敏感，落库用目录登记名
    let Some((registered, url)) = targets.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) else {
        return Err(err(StatusCode::BAD_REQUEST,
            format!("未知目标 {name}（目录：{}；在 settings.json 的 mysql_targets 里加）",
                    targets.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(" | "))));
    };
    let capability = crate::db::db_target_capability(&cfg, registered);
    persist_db_target(&st, registered, url, capability).await?;
    after_db_target_switch(&st, registered).await;
    Ok(Json(serde_json::json!({ "ok": true, "target": registered, "hot": true })))
}

// ─────────────────────────── 【A23】HITL：人改 SQL 再放行（edit 一档）───────────────────────────
// 只做 edit（deepagents 四档里唯一今天有价值的）：管理员改 SQL → **必过闸门** → 执行 →
// 沉淀待复核。两条硬前置（计划原话）：① 改后的 SQL 必过与线上一模一样的那条闸门
// （旁路一次 I1 就作废）；② 进语料前仍要过 `review_exemplar` 判词 —— 「人工背书」
// 不许绕过投毒对策。只支持 DMS 主源（人改 SQL 的场景就是业务表；上传源要改先走 ds 管理）。

#[derive(serde::Deserialize)]
pub struct SqlEditReq {
    question: String,
    sql: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// sql-edit 入参上限（字节）：巨型 SQL 不该直接进闸门 + 执行 + 语料沉淀（术语侧有 64/2000 闸，同纪律）
const SQL_EDIT_MAX_QUESTION: usize = 2000;
const SQL_EDIT_MAX_SQL: usize = 32 * 1024;

/// `POST /api/admin/sql-edit` —— 改 SQL、过闸、执行、沉淀（admin_only）
pub async fn sql_edit_exec(
    State(st): St, h: HeaderMap, Json(req): Json<SqlEditReq>,
) -> ApiRes {
    let p = admin(&st, &h, (&req.login_name, &req.role_code)).await?;
    let question = req.question.trim();
    let sql = req.sql.trim();
    if question.is_empty() || sql.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "question 与 sql 不能为空"));
    }
    if question.len() > SQL_EDIT_MAX_QUESTION || sql.len() > SQL_EDIT_MAX_SQL {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("question 与 sql 超长（上限 {SQL_EDIT_MAX_QUESTION}/{SQL_EDIT_MAX_SQL} 字节）"),
        ));
    }
    // ① 与线上同一条闸门（`dms_agent::gate`，含只读红线/权限注入/LIMIT），一步没宽
    let scope = dms_policy::scope::compute_scope_cached(&st.auth_mysql, &p).await.map_err(db_err)?;
    let scoped = dms_agent::gate(&p, sql, &scope, &dms_kernel::MysqlDialect)
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, format!("闸门未过（与线上同一条）：{e}")))?;
    // ② 与服务同一条取数路径（只读通道 + 行上限 + 敏感列脱敏）
    let rs = st.mysql
        .fetch(&scoped, dms_agent::MAX_ROWS, dms_agent::EXEC_TIMEOUT)
        .await
        .map_err(|_| err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "查询执行失败，请检查 SQL、字段、权限和只读限制后重试",
        ))?;
    // ③ 沉淀 pending + 复核通道（人改的也要过 `review_exemplar` 判词 —— 投毒对策不设后门）。
    //    `save` 返回 false = 同问句已有语料（不重复复核，与线上一致的省法）。
    let review = if dms_semantic::registry::exemplar::save(
        st.owned.pool(), ds_reg::DMS_DS_ID, question, scoped.wire(),
    ).await {
        let llm: Arc<dyn dms_kernel::ChatModel> = Arc::new(st.llm.clone());
        let (pg, ds, q, s) =
            (st.owned.pool().clone(), ds_reg::DMS_DS_ID.to_string(), question.to_string(), scoped.wire().to_string());
        tokio::spawn(async move {
            dms_agent::review::review_exemplar(llm.as_ref(), &pg, &ds, &q, &s).await;
        });
        "pending"
    } else {
        "duplicate"
    };
    Ok(Json(serde_json::json!({
        "ok": true,
        "sql": scoped.wire(),
        "columns": rs.columns,
        "row_count": rs.rows.len(),
        "truncated": rs.rows.len() >= dms_agent::MAX_ROWS,
        "review": review,
    })))
}

#[derive(serde::Deserialize, Default)]
pub struct DocQuery {
    ds_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

fn doc_ds(q: &DocQuery) -> String {
    q.ds_id.clone().unwrap_or_else(|| ds_reg::DMS_DS_ID.into())
}

/// `GET /api/admin/schema-comments.csv?ds_id=` —— 表注释 + 列注释一个文件
/// （`kind` 列区分；`native_comment` 是库原生 COMMENT 的只读参照，编辑时对照用）。
pub async fn schema_comments_csv(
    State(st): St, h: HeaderMap, Query(q): Query<DocQuery>,
) -> Result<axum::response::Response, ApiErr> {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    let ds = doc_ds(&q);
    // 两条导出查询互不依赖，并发跑
    let (tables, cols) = tokio::join!(
        st.owned.fixed(DOC_TABLE_ROWS_SQL).bind(&ds).fetch_all(),
        st.owned.fixed(DOC_COL_ROWS_SQL).bind(&ds).fetch_all(),
    );
    let tables: Vec<(String, String, String)> = tables.map_err(db_err)?;
    let cols: Vec<(String, String, String, String)> = cols.map_err(db_err)?;
    let mut out = String::from("kind,table_name,column_name,custom_comment,native_comment\n");
    for (t, cc, nc) in &tables {
        out.push_str(&["table", t, "", cc, nc].map(csv_field).join(","));
        out.push('\n');
    }
    for (t, c, cc, nc) in &cols {
        out.push_str(&["column", t, c, cc, nc].map(csv_field).join(","));
        out.push('\n');
    }
    Ok(csv_response(out))
}

/// `POST /api/admin/schema-comments.csv?ds_id=` —— 批量写 `custom_comment`。
/// 空串合法（= 清除人工注释、回落原生列）。逐行校验、坏行按行号点名（同 B6 术语导入）。
pub async fn import_schema_comments_csv(
    State(st): St, h: HeaderMap, Query(q): Query<DocQuery>, body: String,
) -> ApiRes {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    let ds = doc_ds(&q);
    let rows = csv_parse(&body);
    let header = ["kind", "table_name", "column_name", "custom_comment", "native_comment"];
    let rows = match rows.first() {
        Some(r) if r.iter().map(String::as_str).eq(header) => &rows[1..],
        _ => &rows[..],
    };
    if rows.len() > CSV_IMPORT_MAX_ROWS {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("一次最多导入 {CSV_IMPORT_MAX_ROWS} 行，请拆分 CSV 分批导入"),
        ));
    }
    let mut ok = 0usize;
    let mut failed = vec![];
    for (i, r) in rows.iter().enumerate() {
        let line = i + 2;
        let cell = |idx: usize| r.get(idx).cloned().unwrap_or_default();
        let (kind, table, column) = (cell(0), cell(1), cell(2));
        // 与 schema sync 同一处信任边界（截 120 字 + 剥控制字符），不开第二份
        let comment = dms_semantic::ingest::sanitize_comment(&cell(3));
        let res = match kind.as_str() {
            "table" => st.owned.fixed(SET_TABLE_COMMENT_SQL)
                .bind(&comment).bind(&table).bind(&ds).execute().await,
            "column" if !column.is_empty() => st.owned.fixed(SET_COL_COMMENT_SQL)
                .bind(&comment).bind(&table).bind(&column).bind(&ds).execute().await,
            _ => {
                failed.push(serde_json::json!({ "line": line, "error": format!("kind 只能是 table|column 且 column 行必须带 column_name：{kind}") }));
                continue;
            }
        };
        match res {
            Ok(0) => failed.push(serde_json::json!({ "line": line, "table": table, "column": column, "error": "表/列未登记（管理面只改已登记的，不创造）" })),
            Ok(_) => ok += 1,
            Err(_) => {
                tracing::warn!(line, reason = "schema_comment_import_row_failed", "注释 CSV 导入行保存失败");
                failed.push(serde_json::json!({
                    "line": line,
                    "table": table,
                    "column": column,
                    "error": "保存失败，请检查表名、列名和注释格式后重试",
                }))
            }
        }
    }
    Ok(Json(serde_json::json!({ "ok": ok, "failed": failed })))
}


#[derive(serde::Deserialize)]
pub struct BulkStatusReq {
    ids: Vec<i64>,
    status: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/exemplars/status` —— 批量复核（admin_only）。
/// 与逐条版同一组校验（`review_status_ok`）；`ids` 上限 500：这是人工复核通道，
/// 一次勾几百条本身就说明没在复核。
pub async fn set_exemplars_status(
    State(st): St, h: HeaderMap, Json(req): Json<BulkStatusReq>,
) -> ApiRes {
    let p = admin(&st, &h, (&req.login_name, &req.role_code)).await?;
    if !review_status_ok(&req.status) {
        return Err(err(StatusCode::BAD_REQUEST, "复核只能置 enabled | disabled"));
    }
    if req.ids.is_empty() || req.ids.len() > 500 {
        return Err(err(StatusCode::BAD_REQUEST, "ids 不能为空且一次 ≤500 条"));
    }
    if req.status == "disabled" {
        let n = st.owned.fixed(EX_BULK_DISABLE_SQL)
            .bind(&p.login_name).bind(&req.ids)
            .execute().await.map_err(db_err)?;
        // 全不存在的 ids 不许 ok:true 假成功（F8 口径，与单条删除的 affected() 对齐）
        affected(n, || "没有匹配的示例 id（可能已删除）".to_string())?;
        return Ok(Json(serde_json::json!({ "ok": true, "updated": n })));
    }
    if req.ids.len() > 100 {
        return Err(err(StatusCode::BAD_REQUEST, "批量执行验证一次最多 100 条"));
    }
    // 去重：同一 id 传两次就真实执行两遍（每次都是全量取数 + 闸门 + 真实执行）
    let mut ids = req.ids;
    ids.sort_unstable();
    ids.dedup();
    let mut ok = 0usize;
    let mut failed = Vec::new();
    for id in ids {
        match validate_exemplar(&st, &p, id).await {
            Ok(_) => ok += 1,
            Err((_, Json(v))) => failed.push(serde_json::json!({ "id": id, "error": v["error"] })),
        }
    }
    Ok(Json(serde_json::json!({ "ok": failed.is_empty(), "updated": ok, "failed": failed })))
}


// ─────────────────── 数据源授权 kb.acl(scope='ds') ───────────────────

/// POST 走 body、DELETE 走 query，**同一个结构**（`serde_urlencoded` 不支持 `flatten`，
/// 故身份字段与业务字段一律平铺）
#[derive(serde::Deserialize)]
pub struct GrantReq {
    grantee_kind: String, grantee: String, perm: Option<String>,
    login_name: Option<String>, role_code: Option<String>,
}

/// 请求 → `AclEntry`。**kind 白名单就是 `Grantee::parse`**（login | role），不复述第二份；
/// `perm` 缺省 `read`（fail-closed：让人查数不等于让他改）。
fn acl_entry(ds_id: &str, r: &GrantReq) -> Result<AclEntry, String> {
    let id = r.grantee.trim();
    if id.is_empty() || id.chars().count() > 64 {
        return Err("grantee 不能为空且 ≤64 字符".into());
    }
    // kind/perm 先归一（trim + 小写）：parse 对大小写/空白不做宽容（`dms_knowledge::acl` 注释自承），
    // 页面传 `"Login"`/`"READ"` 不该被拒
    let kind = r.grantee_kind.trim().to_ascii_lowercase();
    let grantee = Grantee::parse(&kind, id)
        .ok_or_else(|| format!("grantee_kind 只能是 login | role：{}", r.grantee_kind))?;
    let p = r.perm.as_deref().unwrap_or("read");
    let perm = Perm::parse(&p.trim().to_ascii_lowercase())
        .ok_or_else(|| format!("perm 只能是 read | write：{p}"))?;
    Ok(AclEntry { scope: AclScope::Ds, target_id: ds_id.to_string(), grantee, perm })
}

/// ds_id **必须先存在**：给不存在的源授权，会在将来某人建了同名 ds_id 那天变成幽灵授权
/// （`kb.acl.target_id` 只是字符串，没有外键能拦）。
async fn ensure_ds(st: &AppState, ds_id: &str) -> Result<(), ApiErr> {
    let row = ds_reg::get_datasource(st.owned.pool(), ds_id).await.map_err(db_err)?;
    row.ok_or_else(|| err(StatusCode::NOT_FOUND, format!("数据源 {ds_id} 未登记")))?;
    Ok(())
}

fn grant_json(ds_id: &str, e: &AclEntry) -> serde_json::Value {
    serde_json::json!({ "ok": true, "ds_id": ds_id, "grantee_kind": e.grantee.kind(),
        "grantee": e.grantee.id(), "perm": e.perm.as_str() })
}

/// `POST /api/ds/{id}/grant` —— 把数据源（含上传表格建出的源）分享给同事/角色（admin_only）。
/// 收尾评审点名的缺口：此前只有「上传时自动授上传者」，分享给同事没有任何办法。
pub async fn grant(
    State(st): St, h: HeaderMap, Path(id): Path<String>, Json(req): Json<GrantReq>,
) -> ApiRes {
    admin(&st, &h, (&req.login_name, &req.role_code)).await?;
    let e = acl_entry(&id, &req).map_err(|m| err(StatusCode::BAD_REQUEST, m))?;
    ensure_ds(&st, &id).await?;
    acl::grant(&st.owned, &e).await.map_err(db_err)?;
    Ok(Json(grant_json(&id, &e)))
}

/// `DELETE /api/ds/{id}/grant?grantee_kind=&grantee=&perm=` —— 收回（admin_only）。
/// 幂等（底层是 `DELETE … WHERE`）：撤权重试不该报 404。
pub async fn revoke(
    State(st): St, h: HeaderMap, Path(id): Path<String>, Query(q): Query<GrantReq>,
) -> ApiRes {
    admin(&st, &h, (&q.login_name, &q.role_code)).await?;
    let e = acl_entry(&id, &q).map_err(|m| err(StatusCode::BAD_REQUEST, m))?;
    // 撤权不验「源必须存在」：源被删后遗留的 kb.acl 行（`target_id` 只是字符串、无外键，见上）
    // 也必须能收回，否则幽灵授权永远撤不掉。revoke 底层是幂等 DELETE，无行自然 0 影响。
    acl::revoke(&st.owned, &e).await.map_err(db_err)?;
    Ok(Json(grant_json(&id, &e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term_req(json: &str) -> TermUpsertReq {
        serde_json::from_str(json).unwrap()
    }

    fn grant_req(json: &str) -> GrantReq {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn llm_config_vision_contract_is_model_name_or_null() {
        let model = provider_vision_json(Some("vision-model"));
        assert_eq!(model.as_str(), Some("vision-model"));
        assert!(!model.is_boolean());
        assert!(provider_vision_json(None).is_null());

        // 只截取 handler，且锚点拆开书写，测试代码本身不能满足源码合同。
        let src = include_str!("admin_api.rs");
        let body = src
            .split("pub async fn llm_config(")
            .nth(1)
            .expect("llm_config 不见了")
            .split("\n}\n\n#[derive")
            .next()
            .unwrap();
        assert!(body.contains(concat!(
            "\"vision\": provider_vision_json(s.",
            "vision)"
        )));
        assert!(body.contains(concat!(
            "\"vision\": provider_vision_json(c.",
            "vision.as_deref())"
        )));
        assert!(!body.contains(concat!(".vision.", "is_some()")));
    }

    /// admin_only 只认 `administrator_flag`：前端传 `role_code=admin` 不算
    #[test]
    fn admin_only_reads_administrator_flag() {
        let p = |flag| principal::Principal {
            employee_id: 1, login_name: "zhangsan".into(), actual_name: "张三".into(),
            administrator_flag: flag, department_id: None, role_id: 1, role_code: "admin".into(),
        };
        assert!(is_admin(&p(true)));
        assert!(!is_admin(&p(false)), "role_code=admin 不算管理员（那是可伪造的串）");
    }

    /// 设置面必须比普通管理面更窄：只吃 Bearer 会话、精确账号 admin，再查 DMS 管理员位。
    #[test]
    fn settings_admin_never_uses_insecure_identity_fallback() {
        let src = include_str!("admin_api.rs");
        let body = src
            .split("pub async fn settings_admin_only(")
            .nth(1)
            .expect("settings_admin_only 不见了")
            .split("\n}\n")
            .next()
            .unwrap();
        assert!(body.contains("AUTHORIZATION") && body.contains("bearer_token"),
                "设置面没有强制 Bearer 会话：{body}");
        assert!(body.contains("crate::auth::resolve"), "设置面没有解析服务端会话：{body}");
        assert!(body.contains("login != \"admin\"")
                && body.contains("SELECT administrator_flag FROM t_employee")
                && body.contains("deleted_flag = 0")
                && body.contains("disabled_flag = 0"),
                "设置面没有精确账号 + DMS 管理员位校验：{body}");
        assert!(!body.contains("resolve_identity"),
                "设置面继承了 insecure_login_fallback，login_name=admin 可绕过：{body}");
        assert!(!body.contains("load_principal"),
                "设置面被业务角色选择阻塞：{body}");
        assert!(body.contains("let (login, _)"),
                "设置面不应消费会话里的 active role：{body}");
    }

    /// 启动问数源必须可传播失败：无分析目标不能 panic，也不能回退 mysql_url。
    #[test]
    fn analysis_boot_target_returns_result_without_dms_fallback() {
        let src = include_str!("admin_api.rs");
        let body = src
            .split("pub async fn db_boot_target(")
            .nth(1)
            .expect("db_boot_target 不见了")
            .split("\n}\n")
            .next()
            .unwrap();
        assert!(body.contains("anyhow::Result<(String, String)>") && body.contains("ok_or_else"),
                "无分析目标没有作为 Result 传播：{body}");
        assert!(!body.contains(concat!("panic", "!(")), "启动选库仍会 panic：{body}");
        assert!(!body.contains("cfg.mysql_url"), "启动选库又回退 DMS 权限库：{body}");
        let main = include_str!("main.rs");
        assert!(main.contains(concat!("db_boot_target(owned, cfg).await", "?")),
                "唯一调用点没有传播启动选库错误");
    }

    /// 管理 API 的数据库/连接失败不能把驱动错误链带回浏览器或写入复核记录。
    #[test]
    fn connector_errors_are_redacted_at_admin_boundary() {
        let src = include_str!("admin_api.rs");
        let db_err = src
            .split("fn db_err(")
            .nth(1)
            .expect("db_err 不见了")
            .split("\n}")
            .next()
            .unwrap();
        // 底层错误只进服务端日志（tracing::warn），响应仍是固定文案、不带驱动错误链
        assert!(db_err.contains("tracing::warn!"), "DB 故障服务端零痕迹（运维全盲）：{db_err}");
        assert!(db_err.contains("管理操作失败，请稍后重试"), "回浏览器的固定文案变了：{db_err}");
        assert!(!db_err.contains("format!"), "驱动错误链仍可能拼进响应：{db_err}");

        let validate = src
            .split("async fn validate_exemplar(")
            .nth(1)
            .expect("validate_exemplar 不见了")
            .split("\nasync fn mark_exemplar_invalid(")
            .next()
            .unwrap();
        assert!(validate.contains("真实只读执行失败：请检查数据源、字段、权限与只读限制"));
        assert!(!validate.contains(concat!("真实只读执行失败：{", "e}")),
                "真实执行错误仍被公开或持久化：{validate}");
    }

    /// 分页：缺省 50、上限 200、`limit=0`/负数收成 1；两条列表 SQL 都必须带 LIMIT/OFFSET
    #[test]
    fn page_limit_defaults_and_caps() {
        for (got, want) in [(None, 50), (Some(10), 10), (Some(200), 200), (Some(1000), 200)] {
            assert_eq!(page_limit(got), want, "上限 200，不许一把拉全表：{got:?}");
        }
        assert_eq!(page_limit(Some(0)), 1);
        assert_eq!(page_limit(Some(-5)), 1);
        assert_eq!(page_offset(None), 0);
        assert_eq!(page_offset(Some(-1)), 0, "负 offset 是 PG 语法错");
        for s in [TERM_LIST_SQL, EX_LIST_SQL] {
            assert!(s.contains("LIMIT $2 OFFSET $3"), "列表 SQL 没分页：{s}");
        }
    }

    /// 术语 `ds_id` 白名单：dms / * / 已登记；乱填的永远匹配不上召回谓词，必须当场拒
    #[test]
    fn term_ds_id_whitelist() {
        let known = ["crm_pg".to_string(), "upload_d1".to_string()];
        assert!(term_ds_ok("dms", &[]));
        assert!(term_ds_ok("*", &[]), "'*' = 全局生效");
        assert!(term_ds_ok("crm_pg", &known));
        assert!(!term_ds_ok("crm_py", &known), "拼错一个字母就是永不生效的死行");
        assert!(!term_ds_ok("", &known));
        assert!(!term_ds_ok("crm_pg", &[]), "未登记的源不许挂术语");
    }

    /// upsert 的缺省与拒绝面：缺省 dms/active；非法 ds_id、空 term、空别名、坏 status 全拒
    #[test]
    fn term_upsert_defaults_and_rejections() {
        let ok = validate_term(&term_req(r#"{"term":"GMV","definition":"成交总额"}"#), &[]).unwrap();
        assert_eq!(ok.0, "dms", "缺省 ds_id");
        assert_eq!(ok.4, "active", "缺省 status");
        assert!(ok.3.is_empty());
        // ds_id/status/aliases 入库前归一（trim）："dms " 不该撞白名单报错，" GMV" 别名不该原样落库
        let t = validate_term(
            &term_req(r#"{"term":"GMV","definition":"x","ds_id":" dms ","status":" disabled ","aliases":[" 成交额 "]}"#),
            &[],
        ).unwrap();
        assert_eq!(t.0, "dms");
        assert_eq!(t.4, "disabled", "status 先 trim 再匹配");
        assert_eq!(t.3, vec!["成交额".to_string()], "别名存 trim 后的值，原样落库会让召回落空");
        for j in [
            r#"{"term":"GMV","definition":"x","ds_id":"nope"}"#,
            r#"{"term":"  ","definition":"x"}"#,
            r#"{"term":"GMV","definition":""}"#,
            r#"{"term":"GMV","definition":"x","aliases":["成交额",""]}"#,
            r#"{"term":"GMV","definition":"x","status":"pending"}"#,
        ] {
            assert!(validate_term(&term_req(j), &[]).is_err(), "该拒未拒：{j}");
        }
        // 身份字段平铺：失效就会全部退化成 401
        let r = term_req(r#"{"term":"G","definition":"x","login_name":"lisi"}"#);
        assert_eq!(r.login_name.as_deref(), Some("lisi"));
    }

    /// grantee 的 kind 白名单 + perm 白名单 + 缺省 read（fail-closed）
    #[test]
    fn grantee_kind_and_perm_whitelist() {
        let g = grant_req(r#"{"grantee_kind":"login","grantee":"lisi"}"#);
        let e = acl_entry("upload_d1", &g).unwrap();
        assert_eq!(e.scope, AclScope::Ds);
        assert_eq!(e.target_id, "upload_d1");
        assert_eq!(e.grantee, Grantee::Login("lisi".into()));
        assert_eq!(e.perm, Perm::Read, "缺省只读：让人查数不等于让他改");
        // kind/perm 归一：trim + 小写（parse 不做宽容，归一是本文件职责）
        let mixed = grant_req(r#"{"grantee_kind":" Login ","grantee":"lisi","perm":"READ"}"#);
        let e = acl_entry("d", &mixed).unwrap();
        assert_eq!(e.grantee, Grantee::Login("lisi".into()));
        assert_eq!(e.perm, Perm::Read);
        let w = grant_req(r#"{"grantee_kind":"role","grantee":"101","perm":"write"}"#);
        assert_eq!(acl_entry("d", &w).unwrap().perm, Perm::Write);
        for j in [
            r#"{"grantee_kind":"dept","grantee":"1"}"#,
            r#"{"grantee_kind":"*","grantee":"x"}"#,
            r#"{"grantee_kind":"login","grantee":"  "}"#,
            r#"{"grantee_kind":"login","grantee":"lisi","perm":"admin"}"#,
        ] {
            assert!(acl_entry("d", &grant_req(j)).is_err(), "该拒未拒：{j}");
        }
    }

    /// 🔴 **不提供「新增示例」**：路由表里没有建示例的写法，本文件也不许有插入语句。
    /// 手工塞的示例会进 few-shot 并自我传播（见模块文档），复核是唯一入口。
    /// 顺带钉住：复核只许 enabled/disabled（退回 pending 会让同一条反复排队），列表过滤收三值。
    #[test]
    fn no_create_exemplar_route() {
        // 🔴 判据必须打在**wire 侧那张手写表**上，不是打在本文件自造的 `ROUTES` 字面量上。
        // 交叉审实测：往 main.rs 加一条真的 `POST /api/admin/exemplars`，143 条单测 0 red ——
        // 也就是说这条判据此前在守一个只存在于测试里的常量。
        let wire = include_str!("main.rs");
        // 「新增示例」的形态：`"/api/admin/exemplars"` 这个**精确路径**后面接 `.post(` 或 `, post(`。
        // 带 `{id}` 的三条不受影响（它们的路径字符串不同）。
        for bad in ["\"/api/admin/exemplars\", post(", "\"/api/admin/exemplars\").post("] {
            assert!(!wire.contains(bad), "wire 侧出现了新增示例端点：{bad}");
        }
        // 反面（防恒真）：复核那条**必须**真的在 wire 表里 —— 没有这条，
        // 上面那两个 `!contains` 在「有人把整段路由删了」时照样绿。
        assert!(
            wire.contains("\"/api/admin/exemplars/{id}/status\""),
            "复核端点不在 wire 路由表里 —— 「唯一入口」没了"
        );
        assert!(wire.contains("\"/api/admin/exemplars\""), "列表端点不在 wire 路由表里");
        // `ROUTES` 仍然值得对齐一遍（它是这份清单的可读快照），但它不再是唯一证据
        assert!(!ROUTES.contains(&("POST", "/api/admin/exemplars")), "出现了新增示例端点");
        assert!(!ROUTES.iter().any(|(m, p)| *m == "PUT" && p.starts_with("/api/admin/exemplars")));
        let ex: Vec<_> = ROUTES.iter().filter(|(_, p)| p.contains("exemplars")).collect();
        assert_eq!(ex.len(), 3, "只许列表/改状态/删三条：{ex:?}");
        assert!(ROUTES.contains(&("POST", "/api/admin/exemplars/{id}/status")));
        // `ROUTES` 里每条 admin 路径都必须真的出现在 wire 表里（人工同步两处的那道缝）
        for (_, p) in ROUTES.iter().filter(|(_, p)| p.starts_with("/api/admin")) {
            assert!(wire.contains(&format!("\"{p}\"")), "ROUTES 有 {p} 而 wire 表里没有");
        }
        // 字面量拼起来写：否则这行断言自己就会被自己匹配到
        let ins = format!("INSERT INTO {}", "meta.sql_exemplar");
        assert!(!include_str!("admin_api.rs").contains(&ins), "本文件不许写入示例表");
        assert!(review_status_ok("enabled") && review_status_ok("disabled"));
        assert!(!review_status_ok("pending") && !review_status_ok("active"));
        assert!(exemplar_status_ok("pending"), "列表过滤要能取待复核队列");
        assert!(!exemplar_status_ok("active"));
    }

    /// 删除/改状态按 `rows_affected` 判 404（F8「删除假成功」）——SQL 得是主键定位的单行操作
    #[test]
    fn writes_are_keyed_and_counted() {
        assert!(TERM_DELETE_SQL.contains("ds_id=$1 AND term=$2"), "{TERM_DELETE_SQL}");
        assert!(EX_DELETE_SQL.contains("WHERE id=$1"));
        assert!(EX_DISABLE_SQL.contains("WHERE id=$2"));
        assert!(EX_VALIDATE_OK_SQL.contains("WHERE id=$5"));
        assert!(EX_VALIDATE_BAD_SQL.contains("WHERE id=$4"));
        assert!(TERM_UPSERT_SQL.contains("ON CONFLICT (ds_id, term)"), "{TERM_UPSERT_SQL}");
        assert_eq!(affected(0, || "x".into()).unwrap_err().0, StatusCode::NOT_FOUND);
        assert!(affected(1, || "x".into()).is_ok());
    }

    /// 【B6】CSV 往返：逗号/引号/换行/中文全要活过一个来回（示例 SQL 必含前三样）。
    /// `csv_parse` 是手写的 —— 半吊子切逗号会在第一条就碎，所以判据必须打在这些形态上。
    #[test]
    fn csv_roundtrip_survives_quotes_commas_and_newlines() {
        let fields = ["dms", "客单价", "销售额/订单数，别叫\"均价\"", "含\n换行", ""];
        let line = fields.map(csv_field).join(",") + "\n";
        let rows = csv_parse(&line);
        assert_eq!(rows, vec![fields.map(String::from).to_vec()], "{rows:?}");
        // 多行 + 引号内换行混合（示例库 SQL 的真实形态）
        let body = "id,sql\n1,\"SELECT a,\n  b FROM t WHERE x = 'y, z'\"\n2,SELECT 1\n";
        let r = csv_parse(body);
        assert_eq!(r.len(), 3, "{r:?}");
        assert_eq!(r[1][1], "SELECT a,\n  b FROM t WHERE x = 'y, z'", "{:?}", r[1]);
        assert_eq!(r[2], vec!["2".to_string(), "SELECT 1".to_string()]);
        // 空 body 一行不产；尾换行不多产空行
        assert!(csv_parse("").is_empty());
        assert_eq!(csv_parse("a,b\n").len(), 1);
    }

    /// 批量禁用仍走 ANY 数组；批量启用逐条真实执行，不能一条 UPDATE 绕过 VQR。
    #[test]
    fn bulk_status_is_array_based_and_same_review_gate() {
        assert!(EX_BULK_DISABLE_SQL.contains("id = ANY($2::bigint[])"), "{EX_BULK_DISABLE_SQL}");
        let src = include_str!("admin_api.rs");
        let body = src.split("pub async fn set_exemplars_status(").nth(1).unwrap().split("\n\n// ").next().unwrap();
        assert!(body.contains("validate_exemplar(&st, &p, id).await"), "批量启用绕过了 VQR：{body}");
        assert!(body.contains("dedup()"), "批量启用不去重：同一 id 传两遍会真实执行两遍：{body}");
        // 逐条与批量共用 `review_status_ok`：pending 不许被「复核」回 pending（反复排队）
        assert!(review_status_ok("enabled") && review_status_ok("disabled"));
        assert!(!review_status_ok("pending") && !review_status_ok("active"));
    }

    /// 【A11】注释导入的三条红线：① 只 UPDATE 不 INSERT（管理面不创造文档行）；
    /// ② 每个值都过 `sanitize_comment`（与 schema sync 同一处信任边界，不开第二份）；
    /// ③ 全部带 ds_id 谓词（改注释不许跨源）。
    #[test]
    fn schema_comment_import_is_sanitized_scoped_and_update_only() {
        let src = include_str!("admin_api.rs");
        let body = src
            .split("pub async fn import_schema_comments_csv(")
            .nth(1)
            .expect("导入端点没了")
            .split("\n/// ").next().unwrap();
        assert!(body.contains("sanitize_comment"), "不可信文本没过洗就入库：{body}");
        assert!(!body.contains("INSERT INTO meta.table_doc") && !body.contains("INSERT INTO meta.column_doc"),
                "管理面不许创造文档行（表/列来自 schema sync）：{body}");
        assert!(SET_TABLE_COMMENT_SQL.contains("AND ds_id = $3"), "{SET_TABLE_COMMENT_SQL}");
        assert!(SET_COL_COMMENT_SQL.contains("AND ds_id = $4"), "{SET_COL_COMMENT_SQL}");
        assert!(SET_TABLE_COMMENT_SQL.starts_with("UPDATE meta.table_doc SET custom_comment"),
                "写原生列就会被下一次 sync 抹掉/污染：{SET_TABLE_COMMENT_SQL}");
    }

    /// Bearer scheme 大小写不敏感（RFC 6750）：`bearer xxx` 不该被 401
    #[test]
    fn bearer_scheme_is_case_insensitive() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer abc"), Some("abc"));
        assert_eq!(bearer_token("BEARER abc"), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer"), None, "没有 token 部分");
        assert_eq!(bearer_token(""), None);
    }

    /// 分析目标名匹配大小写不敏感（与 settings_api matching_key 同口径）：
    /// boot 选库 / db-config 的 current / persist 找旧 url / POST 切换，四处同一纪律。
    #[test]
    fn db_target_names_match_case_insensitively() {
        let src = include_str!("admin_api.rs");
        let body = |anchor: &str| {
            src.split(anchor)
                .nth(1)
                .unwrap_or_else(|| panic!("{anchor} 不见了"))
                .split("\n}")
                .next()
                .unwrap()
        };
        assert!(body("pub async fn db_boot_target(").contains("n.eq_ignore_ascii_case(name)"),
                "boot 选库仍大小写敏感：kv 与 settings 大小写不一致就静默走 fallback");
        assert!(body("pub async fn db_config(").contains("name.eq_ignore_ascii_case(&current)"),
                "db-config 的 current 仍大小写敏感（settings_api 同语义判断用 eq_ignore_ascii_case）");
        assert!(body("pub(crate) async fn persist_db_target(").contains("target.eq_ignore_ascii_case(&old_name)"),
                "persist 找旧 url 仍大小写敏感，与 capability 查找自相矛盾");
        assert!(body("pub async fn set_db_target(").contains("n.eq_ignore_ascii_case(name)"),
                "POST 切换仍大小写敏感：PUT 用错大小写能存，POST 却报「未知目标」");
    }

    /// CSV 导入有行数闸：无上限时 2MB body 是数千次串行 INSERT；表头丢弃是切片不是 O(n) 平移
    #[test]
    fn csv_import_has_row_cap_and_slice_header() {
        let src = include_str!("admin_api.rs");
        for anchor in ["pub async fn import_terms_csv(", "pub async fn import_schema_comments_csv("] {
            let body = src
                .split(anchor)
                .nth(1)
                .unwrap_or_else(|| panic!("{anchor} 不见了"))
                .split("\n}")
                .next()
                .unwrap();
            assert!(body.contains("CSV_IMPORT_MAX_ROWS"), "{anchor} 没有行数闸");
            assert!(body.contains("&rows[1..]"), "{anchor} 表头丢弃仍是 O(n) 的 remove(0)");
        }
    }

    /// sql-edit 的 question/sql 有长度上限（术语有 64/2000 闸，这里同纪律）
    #[test]
    fn sql_edit_has_length_caps() {
        let src = include_str!("admin_api.rs");
        let body = src
            .split("pub async fn sql_edit_exec(")
            .nth(1)
            .expect("sql_edit_exec 不见了")
            .split("\n}")
            .next()
            .unwrap();
        assert!(body.contains("SQL_EDIT_MAX_QUESTION") && body.contains("SQL_EDIT_MAX_SQL"),
                "question/sql 无长度上限：巨型 SQL 直接进闸门+执行+沉淀：{body}");
    }

    /// revoke 不验源存在：源已删的遗留 kb.acl 行也必须能收回（target_id 无外键）；
    /// grant 仍须先验源存在（幽灵授权对策）。
    #[test]
    fn revoke_works_for_orphan_acl_rows() {
        let src = include_str!("admin_api.rs");
        let revoke = src
            .split("pub async fn revoke(")
            .nth(1)
            .expect("revoke 不见了")
            .split("\n}")
            .next()
            .unwrap();
        assert!(!revoke.contains("ensure_ds"), "revoke 仍被已删除的源挡住：{revoke}");
        let grant = src
            .split("pub async fn grant(")
            .nth(1)
            .expect("grant 不见了")
            .split("\n}")
            .next()
            .unwrap();
        assert!(grant.contains("ensure_ds"), "grant 仍须先验源存在（幽灵授权对策）：{grant}");
    }

    /// 批量 disable 全 miss 时不许 `ok:true` 假成功（F8 口径，与单条删除的 affected() 对齐）
    #[test]
    fn bulk_disable_all_miss_is_404() {
        let src = include_str!("admin_api.rs");
        let body = src
            .split("pub async fn set_exemplars_status(")
            .nth(1)
            .expect("set_exemplars_status 不见了")
            .split("\n\n// ")
            .next()
            .unwrap();
        let disabled = body
            .split("if req.status == \"disabled\" {")
            .nth(1)
            .expect("disabled 分支不见了")
            .split("\n    }")
            .next()
            .unwrap();
        assert!(disabled.contains("affected(n,"), "批量 disable 全 miss 仍 ok:true 假成功：{disabled}");
        assert!(disabled.contains("\"updated\": n"), "成功路径仍回 updated 计数：{disabled}");
    }

    /// 注释导入的失败行点名到列：column 行只带 table 时定位失败行要靠猜
    #[test]
    fn schema_comment_import_failures_name_the_column() {
        let src = include_str!("admin_api.rs");
        let body = src
            .split("pub async fn import_schema_comments_csv(")
            .nth(1)
            .expect("导入端点没了")
            .split("\n/// ")
            .next()
            .unwrap();
        assert!(body.matches("\"column\": column").count() >= 2,
                "column 行失败记录仍不带 column_name：{body}");
    }
}
