//! 【K3-A】数据源管理 HTTP 面：列表 / 登记 / 注销 / 连通性测试 / schema 采集触发。
//! 变更原因＝数据源管理协议。
//!
//! 两条纪律：
//! 1. **明文 DSN 只在 settings.json**。本文件只收/回 `dsn_ref` 键名；`dsn_ref` 里误填明文 DSN
//!    会被 `check_dsn_ref` 当场拒——口令入库一次就再也拿不出来了。
//! 2. **读按 ds 级可见性、写按 `administrator_flag`**。可见性判据整块在
//!    `ds_reg::visible_datasources` 的 SQL 里（第二份 ACL 实现＝下一个越权面）。
//!
//! T10 把 server 拆成 `api/` 目录时本文件整体平移成 `api/ds.rs`。

use crate::AppState;
// 数据源注册表已迁 dms-semantic（`server/src/meta.rs` 已删）。
use dms_semantic::registry::datasource as ds_reg;
use crate::dms_policy::principal;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use dms_connector::registry::DsSpec;
use dms_kernel::DsId;
use std::sync::Arc;

type ApiErr = (StatusCode, Json<serde_json::Value>);
type ApiOk = Json<serde_json::Value>;

/// 沿用现有 `{"error": msg}` 形状（前端只认这一种）
fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// 内部错误的统一出口（安全审查②）：响应只带固定文案 —— anyhow/sqlx/ConnectorError 原文
/// 可能含关系名、约束名与连接细节，回前端等于泄露内部结构。真因一律 `tracing::warn!`
/// 留服务端（照 `kb_api::kb_err` 的收敛模子）。响应形状不变：`{"error": 固定文案}` + 原状态码。
fn internal_err(context: &'static str, e: impl std::fmt::Display) -> ApiErr {
    tracing::warn!(error = %e, "{context}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "服务暂时不可用，请稍后重试")
}

/// 422 版（连通性测试 / schema 采集：管理员触发的动作失败归 422 而非 500）。
/// 诊断细节留在服务端日志，不回前端（连接细节同样是内部结构）。
fn unprocessable_err(context: &'static str, e: impl std::fmt::Display) -> ApiErr {
    tracing::warn!(error = %e, "{context}");
    err(StatusCode::UNPROCESSABLE_ENTITY, "操作失败，请检查数据源配置后重试")
}

/// 身份核验失败（同 `api_ask` 的 403 文案）：load_principal 的 anyhow 可能携带身份库
/// 错误原文（连接细节），不外回；业务分类（多角色未选等）由 warn 留痕。
fn identity_err(login: &str, e: impl std::fmt::Display) -> ApiErr {
    tracing::warn!(login = %login, error = %e, "身份核验被 load_principal 拒");
    err(StatusCode::FORBIDDEN, "当前账号或角色不可用")
}

#[derive(serde::Deserialize, Default)]
pub struct DsQuery {
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `ds_id='dms'` 是存量主源，注销它等于让所有取数无源可用——**任何身份都不许删**。
const PROTECTED_DS: &str = ds_reg::DMS_DS_ID;

fn deletable(ds_id: &str) -> bool {
    ds_id != PROTECTED_DS
}

/// admin_only 判据：**只认 `administrator_flag`**（从 DMS 库读出来的），
/// 不认 `role_code=="admin"`——那是前端传的串，可伪造。
fn is_admin(p: &principal::Principal) -> bool {
    p.administrator_flag
}

/// 身份换算：Bearer 会话 token 优先，回退 login_name（与 `/api/ask` 同一个 `resolve_identity`）
async fn caller(
    st: &AppState,
    headers: &HeaderMap,
    q: &DsQuery,
) -> Result<principal::Principal, ApiErr> {
    let (login, role) = crate::resolve_identity(st, headers, &q.login_name, &q.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    principal::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|e| identity_err(&login, e))
}

async fn admin(
    st: &AppState,
    headers: &HeaderMap,
    q: &DsQuery,
) -> Result<principal::Principal, ApiErr> {
    let p = caller(st, headers, q).await?;
    if !is_admin(&p) {
        return Err(err(StatusCode::FORBIDDEN, "数据源管理需要管理员权限"));
    }
    Ok(p)
}

/// 响应体：`dsn_ref`（键名）可回，明文 DSN 与口令绝不回
fn ds_json(d: &ds_reg::DsSpecRow) -> serde_json::Value {
    serde_json::json!({
        "ds_id": d.ds_id, "name": d.name, "kind": d.kind, "dialect": d.dialect,
        "dsn_ref": d.dsn_ref, "policy_kind": d.policy_kind,
        "description": d.description, "status": d.status,
    })
}

/// `GET /api/ds` —— 只回该 viewer 可见的源。
/// 可见集合由 `visible_datasources` 在 SQL 内算（含 `kb.acl` 的 ds 级授权）；
/// 这里的 `retain` 只是与那份 SQL 结果取交集，**不可能放宽**。
pub async fn list(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<DsQuery>,
) -> Result<ApiOk, ApiErr> {
    let p = caller(&st, &headers, &q).await?;
    let pg = st.owned.pool();
    let visible = ds_reg::visible_datasources(pg, &p.login_name, &[p.role_code.clone()])
        .await
        .map_err(|e| internal_err("数据源可见性判定失败", e))?;
    let rows = ds_reg::list_datasources(pg)
        .await
        .map_err(|e| internal_err("数据源列表读取失败", e))?;
    let ds: Vec<serde_json::Value> =
        rows.iter().filter(|r| visible.contains(&r.ds_id)).map(ds_json).collect();
    Ok(Json(serde_json::json!({ "datasources": ds })))
}

#[derive(serde::Deserialize)]
pub struct DsUpsertReq {
    ds_id: String,
    kind: String,
    dsn_ref: String,
    name: Option<String>,
    dialect: Option<String>,
    policy_kind: Option<String>,
    description: Option<String>,
    status: Option<String>,
    #[serde(flatten)]
    q: DsQuery,
}

/// `POST /api/ds` —— 登记/更新（admin_only）
pub async fn upsert(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<DsUpsertReq>,
) -> Result<ApiOk, ApiErr> {
    admin(&st, &headers, &req.q).await?;
    let row = validate(&req).map_err(|m| err(StatusCode::BAD_REQUEST, m))?;
    ds_reg::upsert_datasource(st.owned.pool(), &row)
        .await
        .map_err(|e| internal_err("数据源登记失败", e))?;
    Ok(Json(ds_json(&row)))
}

/// `DELETE /api/ds/{id}` —— 注销（admin_only；主源不可删）
pub async fn remove(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<DsQuery>,
) -> Result<ApiOk, ApiErr> {
    admin(&st, &headers, &q).await?;
    if !deletable(&id) {
        return Err(err(StatusCode::BAD_REQUEST, format!("主源 {id} 不可注销")));
    }
    let pg = st.owned.pool();
    ensure_row(pg, &id).await?;
    ds_reg::delete_datasource(pg, &id)
        .await
        .map_err(|e| internal_err("数据源注销失败", e))?;
    st.sources.close(&DsId::new(&id)).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /api/ds/{id}/probe` —— 连通性测试（admin_only）。走 `SourceRegistry::probe`：
/// 独立短连接、不进池，配错的 DSN 不会在注册表里留下一个坏池。
pub async fn probe(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<DsQuery>,
) -> Result<ApiOk, ApiErr> {
    admin(&st, &headers, &q).await?;
    let row = ensure_row(st.owned.pool(), &id).await?;
    let spec = DsSpec {
        ds_id: DsId::new(&row.ds_id),
        kind: ds_reg::source_kind(&row.kind)
            .ok_or_else(|| err(StatusCode::UNPROCESSABLE_ENTITY, "kind 非法"))?,
        dsn_ref: row.dsn_ref.clone(),
        max_conn: 2,
        schema: dms_knowledge::tabular::upload_schema_of_ds(&row.ds_id),
    };
    // 安全审查②：错误原文不再回前端（连接细节属内部结构）—— 响应固定文案，
    // 诊断细节（ConnectorError 带源标识与 dsn_ref 键名）留在服务端 warn 日志。
    let version = st
        .sources
        .probe(&spec)
        .await
        .map_err(|e| unprocessable_err("数据源连通性测试失败", e))?;
    Ok(Json(serde_json::json!({ "ok": true, "version": version })))
}

/// `POST /api/ds/{id}/sync` —— 触发该源的 schema 采集（admin_only）。
///
/// 🔴 非 dms 源一律拒：`meta.table_doc`/`column_doc` **还没有 `ds_id` 列**（K3-B 的活），
/// 现在采第二个源会把 DMS 的表文档整片覆盖，且 `sync_schema` 的陈旧行清理会把 DMS 的行删掉。
pub async fn sync(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<DsQuery>,
) -> Result<ApiOk, ApiErr> {
    admin(&st, &headers, &q).await?;
    let pg = st.owned.pool();
    ensure_row(pg, &id).await?;
    if id != ds_reg::DMS_DS_ID {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{id} 的 schema 采集需先完成注册表 ds_id 化（K3-B）；当前只支持 ds_id=dms"),
        ));
    }
    // 采集（IO）在 server，入库（PG）在 semantic。`ds` 传 `DMS_DS_ID` 就是上面那道校验的语义
    let assets = dms_semantic::warehouse_catalog::metadata_assets();
    let (mut snap, warehouse_catalog) = st
        .mysql
        .probe_schema_with_warehouse_catalog(&assets)
        .await
        .map_err(|e| unprocessable_err("schema 探测失败", e))?;
    let warehouse_comments = st.mysql.enrich_dms_snapshot(&mut snap).await.unwrap_or(0);
    let (tables, columns) =
        // `true`＝过滤备份表：DMS 是别人建的库，里头确有 bak_*/日期后缀的垃圾表
        dms_semantic::ingest::schema_sync::sync_schema(pg, ds_reg::DMS_DS_ID, &snap, true)
            .await
            .map_err(|e| unprocessable_err("schema 采集入库失败", e))?;
    dms_semantic::warehouse_catalog::seed(pg, ds_reg::DMS_DS_ID)
        .await
        .map_err(|e| internal_err("warehouse 目录播种失败", e))?;
    Ok(Json(serde_json::json!({
        "ds_id": id,
        "tables": tables,
        "columns": columns,
        "warehouse_comments": warehouse_comments,
        "warehouse_catalog_requested": warehouse_catalog.requested,
        "warehouse_catalog_tables": warehouse_catalog.tables,
        "warehouse_catalog_columns": warehouse_catalog.columns,
        "warehouse_catalog_missing": warehouse_catalog.missing
    })))
}

/// 未登记的 ds_id 一律 404（`probe`/`sync`/`delete` 都不许对着空气干活）
async fn ensure_row(pg: &sqlx::PgPool, id: &str) -> Result<ds_reg::DsSpecRow, ApiErr> {
    ds_reg::get_datasource(pg, id)
        .await
        .map_err(|e| internal_err("数据源登记读取失败", e))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("数据源 {id} 未登记")))
}


/// `ds_id` 会成为连接池 key、错误文案与（K4）`up_*` schema 名的一部分：白名单收窄。
fn valid_ds_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `dsn_ref` 只能是 settings.json 里的**键名**。误填明文 DSN 就会把生产口令写进
/// `meta.datasource`，而这张表会进接口响应——当场拒，别指望事后清理。
fn check_dsn_ref(s: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err("dsn_ref 不能为空（填 settings.json 里的键名，如 mysql_url）".into());
    }
    if !valid_ds_id(s) {
        return Err(format!(
            "dsn_ref 只能填键名（字母数字与 _-），不许填明文 DSN：{s}"
        ));
    }
    Ok(())
}

/// 请求 → 注册表行。缺省全部 fail-closed：
/// `policy_kind` 默认 `global`（可见性只靠 ds 级 ACL），**不默认 `dms_datascope`**——
/// 那一档对所有认证用户可见，绝不能靠「忘填」拿到。
fn validate(r: &DsUpsertReq) -> Result<ds_reg::DsSpecRow, String> {
    if !valid_ds_id(&r.ds_id) {
        return Err(format!("ds_id 只允许字母数字与 _- 且 ≤64 字符：{}", r.ds_id));
    }
    check_dsn_ref(&r.dsn_ref)?;
    let dialect = r.dialect.clone().unwrap_or_else(|| r.kind.clone());
    for (label, v) in [("kind", &r.kind), ("dialect", &dialect)] {
        if ds_reg::source_kind(v).is_none() {
            return Err(format!("{label} 只能是 mysql | postgres：{v}"));
        }
    }
    let policy_kind = r.policy_kind.clone().unwrap_or_else(|| "global".into());
    if !matches!(policy_kind.as_str(), "dms_datascope" | "global") {
        return Err(format!("policy_kind 只能是 dms_datascope | global：{policy_kind}"));
    }
    let status = r.status.clone().unwrap_or_else(|| "active".into());
    if !matches!(status.as_str(), "active" | "disabled") {
        return Err(format!("status 只能是 active | disabled：{status}"));
    }
    Ok(ds_reg::DsSpecRow {
        ds_id: r.ds_id.clone(),
        name: r.name.clone().unwrap_or_else(|| r.ds_id.clone()),
        kind: r.kind.clone(),
        dialect,
        dsn_ref: r.dsn_ref.clone(),
        policy_kind,
        description: r.description.clone().unwrap_or_default(),
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(admin: bool, role: &str) -> principal::Principal {
        principal::Principal {
            employee_id: 1,
            login_name: "zhangsan".into(),
            actual_name: "张三".into(),
            administrator_flag: admin,
            department_id: None,
            role_id: 1,
            role_code: role.into(),
        }
    }

    /// admin_only 只认 `administrator_flag`：前端传 role_code=admin 不算
    #[test]
    fn admin_only_reads_administrator_flag() {
        assert!(is_admin(&principal(true, "city_manager")));
        assert!(!is_admin(&principal(false, "admin")));
        assert!(!is_admin(&principal(false, "city_manager")));
    }

    /// 主源不可注销（删了它所有取数就没源了）
    #[test]
    fn dms_datasource_is_not_deletable() {
        assert!(!deletable("dms"));
        assert!(deletable("crm_pg"));
        assert!(deletable("upload_abc"));
    }

    fn req(json: &str) -> DsUpsertReq {
        serde_json::from_str(json).unwrap()
    }

    /// 明文 DSN 误填 `dsn_ref` 必须当场拒——口令入库就收不回来了
    #[test]
    fn plain_dsn_in_dsn_ref_rejected() {
        let bad = req(r#"{"ds_id":"crm_pg","kind":"postgres","dsn_ref":"postgres://u:p@h/db"}"#);
        let e = validate(&bad).unwrap_err();
        assert!(e.contains("不许填明文 DSN"), "{e}");
        assert!(validate(&req(r#"{"ds_id":"x","kind":"mysql","dsn_ref":""}"#)).is_err());
    }

    /// 缺省 policy_kind 必须是 global（fail-closed）：dms_datascope 对全员可见，不许靠忘填拿到
    #[test]
    fn defaults_are_fail_closed() {
        let r = validate(&req(r#"{"ds_id":"crm_pg","kind":"postgres","dsn_ref":"crm_url"}"#)).unwrap();
        assert_eq!(r.policy_kind, "global");
        assert_eq!(r.dialect, "postgres", "dialect 缺省跟随 kind");
        assert_eq!(r.status, "active");
        assert_eq!(r.name, "crm_pg");
    }

    #[test]
    fn illegal_ds_id_and_kind_rejected() {
        for j in [
            r#"{"ds_id":"a b","kind":"mysql","dsn_ref":"k"}"#,
            r#"{"ds_id":"a;DROP","kind":"mysql","dsn_ref":"k"}"#,
            r#"{"ds_id":"","kind":"mysql","dsn_ref":"k"}"#,
            r#"{"ds_id":"ok","kind":"oracle","dsn_ref":"k"}"#,
            r#"{"ds_id":"ok","kind":"mysql","dsn_ref":"k","dialect":"oracle"}"#,
            r#"{"ds_id":"ok","kind":"mysql","dsn_ref":"k","policy_kind":"rule_table"}"#,
        ] {
            assert!(validate(&req(j)).is_err(), "该拒未拒：{j}");
        }
    }

    /// 响应体不许带明文 DSN/口令（只回 dsn_ref 键名）
    #[test]
    fn response_never_carries_secrets() {
        let row = validate(&req(r#"{"ds_id":"crm_pg","kind":"postgres","dsn_ref":"crm_url"}"#)).unwrap();
        let s = ds_json(&row).to_string();
        assert!(s.contains("crm_url"));
        assert!(!s.contains("://") && !s.contains("password"), "{s}");
    }

    /// 身份字段走 flatten：失效就会全部退化成 401
    #[test]
    fn upsert_body_reads_identity() {
        let r = req(r#"{"ds_id":"crm_pg","kind":"postgres","dsn_ref":"crm_url","login_name":"lisi"}"#);
        assert_eq!(r.q.login_name.as_deref(), Some("lisi"));
    }

    // ── 安全审查②：内部错误只回固定文案 ──

    /// 固定文案 + 原状态码 + 响应形状不变；原文只进 warn 不进响应体
    #[test]
    fn internal_err_has_fixed_message_and_keeps_shape() {
        let raw = "查询失败 [crm_pg] connection refused (10.0.0.8:5432)";
        let (code, Json(body)) = internal_err("测试上下文", raw);
        assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, serde_json::json!({ "error": "服务暂时不可用，请稍后重试" }));
        let (code, Json(body)) = unprocessable_err("连通性测试失败", raw);
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!body.to_string().contains("10.0.0.8"), "连接细节不许外泄：{body}");
        let (code, Json(body)) = identity_err("zhangsan", raw);
        assert_eq!(code, StatusCode::FORBIDDEN);
        assert_eq!(body, serde_json::json!({ "error": "当前账号或角色不可用" }));
    }

    /// 源码闸：`err(状态码, e)` 直回原文的写法不许回来
    #[test]
    fn raw_causes_never_reach_the_client() {
        let src = include_str!("ds_api.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        for bad in [
            "err(StatusCode::INTERNAL_SERVER_ERROR, e)",
            "err(StatusCode::UNPROCESSABLE_ENTITY, e)",
            "err(StatusCode::FORBIDDEN, e)",
        ] {
            assert!(!code.contains(bad), "错误原文泄露回来了：{bad}");
        }
    }
}
