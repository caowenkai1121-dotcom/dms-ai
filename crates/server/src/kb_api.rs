//! 【K1/K2】知识库 HTTP 面：上传 / 列表 / 详情 / 删除 / 问答 / 引用原文回查。变更原因＝知识库协议。
//!
//! **零业务判定**：类型白名单、大小上限、sha256 去重、状态机全在 `dms_knowledge::ingest`——
//! 同一信任边界上两份实现必然漂，漂出来宽松的那份就是入口。本文件只做协议转换与身份换算。
//!
//! T10 把 server 拆成 `api/` 目录时本文件整体平移成 `api/kb.rs`（函数形状已按目标组织）。

use crate::AppState;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::Json;
use dms_kernel::{ChatModel, ChatRequest, ModelTier};
use dms_knowledge::store::DocRow;
use dms_knowledge::{acl, ingest, store, tabular, KbError, Viewer};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

type ApiErr = (StatusCode, Json<serde_json::Value>);
type ApiOk = Json<serde_json::Value>;

/// 上传并发闸：20MB（`kb_max_mb` 默认）全量入内存 × N 并发会打爆进程。
/// 拿不到许可**直接 429 而不排队**——排队只是把内存问题推迟到队列长度上。
static UPLOAD_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
const UPLOAD_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

const IMAGE_OCR_PROMPT: &str = "请逐字识别图片中的全部可见文字，并完整还原表格结构、金额、数字和日期。保持原文顺序和字段，不总结、不改写、不补全、不猜测；无法辨认处标记[无法辨认]。仅输出识别结果。";

struct RuntimeImageOcr<'a> {
    llm: &'a crate::llm::LlmClient,
}

impl<'runtime> ingest::ImageOcr for RuntimeImageOcr<'runtime> {
    fn recognize<'a>(
        &'a self,
        file_name: &'a str,
        mime: &'a str,
        bytes: &'a [u8],
    ) -> ingest::ImageOcrFuture<'a> {
        Box::pin(async move {
            let data_mime = image_data_mime(file_name, mime);
            if !image_fits_vision_data_url(data_mime, bytes.len()) {
                return None;
            }
            let image_url = format!(
                "data:{};base64,{}",
                data_mime,
                encode_base64(bytes)
            );
            self.llm
                .vision_chat(IMAGE_OCR_PROMPT, &image_url)
                .await
                .ok()
                .map(|(text, _, _)| text)
                .filter(|text| !text.trim().is_empty())
        })
    }
}

/// 响应体沿用现有 `{"error": msg}` 形状（前端只认这一种）
fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// `KbError` → HTTP。只公开可操作的业务错误；数据库与文档服务错误可能携带
/// 连接信息或上游正文，统一收敛为固定文案。
fn kb_err(e: KbError) -> ApiErr {
    let code = match &e {
        KbError::BadInput(_) => StatusCode::BAD_REQUEST,
        KbError::Forbidden(_) => StatusCode::FORBIDDEN,
        KbError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let message = match &e {
        KbError::BadInput(_) | KbError::Forbidden(_) | KbError::NotFound(_) => e.to_string(),
        KbError::Upstream(_) => "文档处理服务暂时不可用，请稍后重试".to_string(),
        KbError::Db(_) => "知识库服务暂时不可用，请稍后重试".to_string(),
    };
    err(code, message)
}

/// 知识库端点共用的 query。上传 multipart 只接收空间/目录，身份只认会话 header。
#[derive(serde::Deserialize, Default)]
pub struct KbQuery {
    login_name: Option<String>,
    role_code: Option<String>,
    space_id: Option<String>,
    folder_id: Option<String>,
    /// 分块策略（可选）：general/qa/book/laws/semantic；缺省 general（与历史行为一致）
    preset: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct CreateSpaceReq {
    name: String,
    space_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct ReprocessReq {
    login_name: Option<String>,
    role_code: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct DocStateReq {
    enabled: bool,
    login_name: Option<String>,
    role_code: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct DocMetadataReq {
    tags: Vec<String>,
    business_domain: Option<String>,
    effective_from: Option<String>,
    effective_to: Option<String>,
    source_uri: Option<String>,
    document_family: Option<String>,
    document_revision: Option<String>,
    #[serde(default)]
    related_doc_ids: Vec<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

struct DocMetadata {
    tags: Vec<String>,
    business_domain: Option<String>,
    effective_from: Option<chrono::NaiveDate>,
    effective_to: Option<chrono::NaiveDate>,
    source_uri: Option<String>,
    document_family: Option<String>,
    document_revision: Option<String>,
}

const MAX_BATCH_GRANTS: usize = 100;
const MAX_LOGIN_NAME: usize = 64;
const MAX_ROLE_CODE: usize = 500;

#[derive(serde::Deserialize, Default)]
pub struct SpaceGrantReq {
    #[serde(default)]
    grantee_kind: String,
    #[serde(default)]
    grantee: String,
    perm: Option<String>,
    #[serde(default)]
    role_codes: Vec<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

#[derive(Clone, serde::Serialize)]
struct RoleOption {
    role_code: String,
    role_name: String,
}

/// 身份：Bearer 会话 token 优先，回退 login_name（与 `/api/ask` 同一个 `resolve_identity`）。
/// `Viewer` 只要 login + 角色码两个字符串（knowledge 刻意不依赖 policy）。
/// 🔴 吃 `&AppState` 是因为**认证回退是配置开关**（`insecure_login_fallback`，默认关）——
/// `resolve_identity` 是全仓 13 个调用点的唯一收口，本函数必须把开关传下去，
/// 否则 KB 这一族端点会绕过那道闸（`/api/kb/chunk/{id}` 直取块正文，绕过它就是读他人文档）。
async fn viewer(st: &AppState, headers: &HeaderMap, q: &KbQuery) -> Result<Viewer, ApiErr> {
    let (login, role) = crate::resolve_identity(st, headers, &q.login_name, &q.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    load_viewer(st, login, role).await
}

/// 上传必须在读取 multipart 之前完成认证，因此该入口不接受 body/query 身份回退。
/// 上传属管理面：过 `kb_manager` 闸（缺省仅管理员）。
async fn session_viewer(st: &AppState, headers: &HeaderMap) -> Result<Viewer, ApiErr> {
    let none = None::<String>;
    let p = manager_principal(st, headers, &none, &none).await?;
    Ok(Viewer::new(p.login_name, vec![p.role_code]))
}

async fn load_viewer(
    st: &AppState,
    login: String,
    role: Option<String>,
) -> Result<Viewer, ApiErr> {
    let p = crate::auth::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|_| err(StatusCode::FORBIDDEN, "当前 DMS 身份或角色不可用"))?;
    Ok(Viewer::new(p.login_name, vec![p.role_code]))
}

/// 【KB 管理闸】管理面授权判定（纯函数，单测钉全分支）：
/// `administrator_flag` 恒真（与 `admin_only` 同一口径，管理员永远能管）；
/// 否则 login ∈ `grants.logins` 或 role ∈ `grants.roles`（或关系，精确字符串比对）；
/// 缺省/None（含空名单）= 仅管理员。配置形态见 `db::Settings::kb_manager_grants`。
fn kb_manager_allowed(
    p: &crate::dms_policy_core::Principal,
    grants: Option<&crate::db::KbManagerGrants>,
) -> bool {
    if p.administrator_flag {
        return true;
    }
    let Some(g) = grants else { return false };
    g.logins.iter().any(|login| login == &p.login_name)
        || g.roles.iter().any(|role| role == &p.role_code)
}

/// 管理面统一闸：认证（`resolve_identity` 唯一收口，与 `viewer` 同一条）→ 现查 Principal →
/// `kb_manager_allowed`。不过 = 403 统一文案「知识库管理未对你开放」——不区分「没配置」
/// 与「配置了但没你」，免得从响应差异透出授权清单的存在性。
/// 🔴 检索面（ask/search/chunk/download_doc）**不走这里**：普通用户问 KB 问题用的是检索面。
async fn manager_principal(
    st: &AppState,
    headers: &HeaderMap,
    login_name: &Option<String>,
    role_code: &Option<String>,
) -> Result<crate::dms_policy_core::Principal, ApiErr> {
    let (login, role) = crate::resolve_identity(st, headers, login_name, role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let p = crate::auth::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|_| err(StatusCode::FORBIDDEN, "当前 DMS 身份或角色不可用"))?;
    if !kb_manager_allowed(&p, st.cfg().kb_manager_grants.as_ref()) {
        return Err(err(StatusCode::FORBIDDEN, "知识库管理未对你开放"));
    }
    Ok(p)
}

/// 管理面端点的 Viewer 入口：过闸 + 换算成 knowledge 的 `Viewer`（与 `load_viewer` 同一条换算，
/// 只是 Principal 已经查过一遍，不重复打库）。
async fn manager_viewer(st: &AppState, headers: &HeaderMap, q: &KbQuery) -> Result<Viewer, ApiErr> {
    let p = manager_principal(st, headers, &q.login_name, &q.role_code).await?;
    Ok(Viewer::new(p.login_name, vec![p.role_code]))
}

/// 缺省空间＝登录名＝个人空间（`ensure_space` 幂等建）
fn space_of(v: &Viewer, q: &KbQuery) -> String {
    q.space_id.clone().unwrap_or_else(|| v.login.clone())
}

fn validate_doc_metadata(req: &DocMetadataReq) -> Result<DocMetadata, String> {
    let mut tags = Vec::new();
    for raw in &req.tags {
        let tag = raw.trim();
        if tag.is_empty() {
            continue;
        }
        if tag.chars().count() > 30 {
            return Err("单个标签不能超过 30 个字符".into());
        }
        if !tags.iter().any(|v| v == tag) {
            tags.push(tag.to_string());
            if tags.len() > 20 {
                return Err("标签最多 20 个".into());
            }
        }
    }

    let business_domain = optional_text(req.business_domain.as_deref(), 60, "业务域")?;
    let source_uri = optional_text(req.source_uri.as_deref(), 500, "来源地址")?;
    let document_family = optional_text(req.document_family.as_deref(), 120, "文档族")?;
    let document_revision = optional_text(req.document_revision.as_deref(), 60, "版本号")?;
    if source_uri.as_deref().is_some_and(|uri| !is_safe_source_uri(uri)) {
        return Err("来源地址只支持 http:// 或 https://".into());
    }
    let effective_from = optional_date(req.effective_from.as_deref(), "生效日期")?;
    let effective_to = optional_date(req.effective_to.as_deref(), "失效日期")?;
    if effective_from.zip(effective_to).is_some_and(|(from, to)| from > to) {
        return Err("生效日期不能晚于失效日期".into());
    }

    Ok(DocMetadata {
        tags, business_domain, effective_from, effective_to, source_uri,
        document_family, document_revision,
    })
}

fn optional_text(value: Option<&str>, max: usize, label: &str) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    if value.chars().count() > max {
        return Err(format!("{label}不能超过 {max} 个字符"));
    }
    Ok(Some(value.to_string()))
}

fn optional_date(value: Option<&str>, label: &str) -> Result<Option<chrono::NaiveDate>, String> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("{label}必须使用 YYYY-MM-DD 格式"))?;
    if date.format("%Y-%m-%d").to_string() != value {
        return Err(format!("{label}必须使用 YYYY-MM-DD 格式"));
    }
    Ok(Some(date))
}

fn is_safe_source_uri(uri: &str) -> bool {
    let uri = uri.to_ascii_lowercase();
    uri.starts_with("https://") || uri.starts_with("http://")
}

pub async fn upload(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    mp: Multipart,
) -> Result<ApiOk, ApiErr> {
    // 先认证再占上传槽：未认证慢请求不能耗尽 4 个许可。
    let v = session_viewer(&st, &headers).await?;
    let _permit = UPLOAD_GATE.try_acquire().map_err(|_| {
        err(StatusCode::TOO_MANY_REQUESTS, "上传并发已满（同时最多 4 个），请稍后重试")
    })?;
    let form = tokio::time::timeout(UPLOAD_READ_TIMEOUT, read_form(mp))
        .await
        .map_err(|_| err(StatusCode::REQUEST_TIMEOUT, "上传读取超时，请重试"))??;
    let space_id = space_of(&v, &form.q);
    let (name, mime, bytes) =
        form.file.ok_or_else(|| err(StatusCode::BAD_REQUEST, "缺 file 字段"))?;
    let req = ingest::UploadReq {
        space_id: &space_id,
        folder_id: form.q.folder_id.as_deref(),
        file_name: &name,
        mime: &mime,
        bytes: &bytes,
        preset: form.q.preset.as_deref(),
    };
    let image_ocr = RuntimeImageOcr { llm: &st.llm };
    let mut out =
        ingest::ingest(&st.owned, &st.doc, &st.embed, &v, &st.kb_cfg, req, Some(&image_ocr))
            .await
            .map_err(kb_err)?;
    // 去重命中既有文档时 ingest 会直接复用 doc_id；这里统一再绑定一次目标目录，
    // 同时让写权限在返回结果前于同一条写语句中复核，撤权后不返回裸文档元数据。
    if let Err(e) = store::move_doc(
        &st.owned,
        &v,
        &out.doc_id,
        &space_id,
        form.q.folder_id.as_deref(),
    )
    .await
    {
        if out.source.is_some() {
            let _ = cleanup_source(&st, &out.doc_id).await;
        }
        return Err(kb_err(e));
    }
    if !register_source(&st, &v, &name, &out).await {
        out.source = None;
    }
    // 入库是同步链，回来时状态已终态——顺手带上，省前端一次轮询
    let row = acl::doc_for_viewer(&st.owned, &v, &out.doc_id).await.map_err(kb_err)?;
    let mut body = doc_json(&row);
    // 通道②的产物：不带出来，「上传即可问数」对前端就是不可见的
    if let (Some(src), Some(obj)) = (&out.source, body.as_object_mut()) {
        obj.insert("datasource".into(), source_json(src));
    }
    Ok(Json(body))
}

pub async fn spaces(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    // 空间清单只服务管理面（检索面不需要它），统一过 kb_manager 闸。
    // `kb_manager: true` 透出给前端做入口显隐 —— 能拿到这份响应的就是已过闸的人；
    // 未过闸者在上面已经拿到 403，前端据 r.ok 判定，隐藏只是体验，闸在这里。
    let p = manager_principal(&st, &headers, &q.login_name, &q.role_code).await?;
    let is_admin = p.administrator_flag;
    let v = Viewer::new(p.login_name, vec![p.role_code]);
    store::ensure_space(&st.owned, &v.login, &v.login).await.map_err(kb_err)?;
    let rows = store::list_spaces(&st.owned, &v.login, &v.roles).await.map_err(kb_err)?;
    Ok(Json(serde_json::json!({
        "is_admin": is_admin,
        "kb_manager": true,
        "spaces": rows.into_iter().map(|s| serde_json::json!({
            "space_id": s.space_id, "name": s.name, "owner": s.owner,
            "visibility": s.visibility, "writable": s.writable, "doc_count": s.doc_count,
        })).collect::<Vec<_>>()
    })))
}

pub async fn create_space(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateSpaceReq>,
) -> Result<ApiOk, ApiErr> {
    let q = KbQuery {
        login_name: req.login_name,
        role_code: req.role_code,
        space_id: None,
        folder_id: None,
        preset: None,
    };
    let p = manager_principal(&st, &headers, &q.login_name, &q.role_code).await?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(err(StatusCode::BAD_REQUEST, "知识空间名称不能为空且不超过 60 字"));
    }
    let space_id = req.space_id.unwrap_or_else(|| format!("enterprise-{}", uuid::Uuid::new_v4()));
    if !valid_space_id(&space_id) {
        return Err(err(StatusCode::BAD_REQUEST, "space_id 只能包含字母、数字、下划线和短横线，且不超过 64 字符"));
    }
    let collision = st
        .auth_mysql
        .fixed("SELECT COUNT(*) FROM t_employee WHERE login_name=? AND deleted_flag=0")
        .bind(&space_id)
        .fetch_optional::<(i64,)>()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "DMS 身份服务暂时不可用"))?
        .is_some_and(|(n,)| n > 0);
    if collision {
        return Err(err(StatusCode::CONFLICT, "space_id 不能与 DMS 登录账号相同"));
    }
    store::create_space(&st.owned, &space_id, name, &p.login_name).await.map_err(kb_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "space_id": space_id, "name": name })))
}

pub async fn reprocess(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ReprocessReq>,
) -> Result<ApiOk, ApiErr> {
    let q = KbQuery {
        login_name: req.login_name, role_code: req.role_code, space_id: None, folder_id: None,
        preset: None,
    };
    let v = manager_viewer(&st, &headers, &q).await?;
    let row = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    if !acl::space_writable(&st.owned, &v, &row.space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权重处理空间 {} 的文档", row.space_id)));
    }
    let is_tabular = ["csv", "xls", "xlsx", "xlsm"]
        .iter()
        .any(|ext| row.name.to_ascii_lowercase().ends_with(&format!(".{ext}")));
    if is_tabular {
        return Err(err(
            StatusCode::CONFLICT,
            "表格文档包含知识索引与问数表两条通道；为避免覆盖仍可用的数据，请直接上传新版本并停用旧版本",
        ));
    }
    let path = stored_file(&st.kb_cfg.root, &row.doc_id)
        .await
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "原始文件已不存在，请重新上传"))?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "文档文件暂时不可读取"))?;
    let req = ingest::UploadReq {
        space_id: &row.space_id,
        folder_id: row.folder_id.as_deref(),
        file_name: &row.name,
        mime: &row.mime,
        bytes: &bytes,
        preset: None,
    };
    let image_ocr = RuntimeImageOcr { llm: &st.llm };
    let out = ingest::reprocess(
        &st.owned,
        &st.doc,
        &st.embed,
        &v,
        &st.kb_cfg,
        req,
        &row.doc_id,
        Some(&image_ocr),
    )
    .await
    .map_err(kb_err)?;
    register_source(&st, &v, &row.name, &out).await;
    let row = acl::doc_for_viewer(&st.owned, &v, &out.doc_id).await.map_err(kb_err)?;
    Ok(Json(doc_json(&row)))
}

pub async fn set_doc_state(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<DocStateReq>,
) -> Result<ApiOk, ApiErr> {
    let q = KbQuery {
        login_name: req.login_name, role_code: req.role_code, space_id: None, folder_id: None,
        preset: None,
    };
    let v = manager_viewer(&st, &headers, &q).await?;
    let row = store::get_doc(&st.owned, &id)
        .await
        .map_err(kb_err)?
        .ok_or_else(|| err(StatusCode::FORBIDDEN, format!("文档 {id} 不可见")))?;
    if !acl::space_readable(&st.owned, &v, &row.space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("文档 {id} 不可见")));
    }
    if !acl::space_writable(&st.owned, &v, &row.space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权修改空间 {} 的文档", row.space_id)));
    }
    store::set_enabled(&st.owned, &v, &id, req.enabled).await.map_err(kb_err)?;
    sync_source_state(&st, &id, req.enabled).await?;
    Ok(Json(serde_json::json!({ "ok": true, "enabled": req.enabled })))
}

pub async fn update_doc_metadata(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<DocMetadataReq>,
) -> Result<ApiOk, ApiErr> {
    let q = KbQuery {
        login_name: req.login_name.clone(), role_code: req.role_code.clone(),
        space_id: None, folder_id: None,
        preset: None,
    };
    let v = manager_viewer(&st, &headers, &q).await?;
    let meta = validate_doc_metadata(&req).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    store::update_doc_metadata_and_links(
        &st.owned,
        &v,
        &id,
        &store::DocMetadataUpdate {
            tags: &meta.tags,
            business_domain: meta.business_domain.as_deref(),
            effective_from: meta.effective_from,
            effective_to: meta.effective_to,
            source_uri: meta.source_uri.as_deref(),
            document_family: meta.document_family.as_deref(),
            document_revision: meta.document_revision.as_deref(),
            related_doc_ids: &req.related_doc_ids,
        },
    )
    .await
    .map_err(kb_err)?;
    let updated = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    sync_source_state(&st, &id, updated.enabled).await?;
    Ok(Json(doc_json(&updated)))
}

// ════════════════════════ KB 运营小包（Y12 + Y7）════════════════════════
//
// 三个 handler 的**接线契约**（本轮不注册 `main.rs`，集成时各加一行）：
//   .route("/api/kb/ingest-url", post(kb_api::ingest_url))
//   .route("/api/kb/space/{id}/export", get(kb_api::export_space))
//   .route("/api/kb/doc/{id}/description", post(kb_api::generate_description))
//
// 权限判据：先过 `kb_manager` 管理闸（本块三个 handler 均属管理面），空间级仍沿用既有口径——
// 读 = `acl::space_readable`，写 = `acl::space_writable`，且写语句内联复核（fail-closed）。
//
// 接线前整块属未达代码：`allow` 挂子模块（`artifact_api`/`trace_api` 同一模子）。
mod ops_pack {
    use super::*;

    // ───────────────────── 【Y12】URL 抓取入库 ─────────────────────

    /// 抓取护栏常量：总超时 15s（reqwest 的 timeout 覆盖到响应体读完）、大小帽 5MB、
    /// 手动跟随重定向 ≤3 跳、URL 长度封顶 2048。
    const URL_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    const URL_FETCH_MAX_BYTES: usize = 5 * 1_048_576;
    const URL_FETCH_MAX_REDIRECTS: usize = 3;
    const URL_MAX_LEN: usize = 2048;

    #[derive(serde::Deserialize, Default)]
    pub struct IngestUrlReq {
        url: String,
        #[serde(flatten)]
        q: KbQuery,
    }

    /// body `{"url","space_id"?,"folder_id"?,"preset"?,...身份}`：服务端抓取网页（HTML）或
    /// PDF，存成 `<slug>.html`/`.pdf` 后**复用既有 ingest 全流程**（白名单校验/落盘/解析/
    /// 分块/向量/权限），`source_uri` 列记最终落地 URL。响应与 `/api/kb/upload` 同形。
    pub async fn ingest_url(
        State(st): State<Arc<AppState>>,
        headers: HeaderMap,
        Json(req): Json<IngestUrlReq>,
    ) -> Result<ApiOk, ApiErr> {
        // 先认证再占上传槽（与 `upload` 同序）：未认证请求不能耗尽 4 个许可。
        let v = manager_viewer(&st, &headers, &req.q).await?;
        let _permit = UPLOAD_GATE.try_acquire().map_err(|_| {
            err(StatusCode::TOO_MANY_REQUESTS, "上传并发已满（同时最多 4 个），请稍后重试")
        })?;
        let page = fetch_url_guarded(req.url.trim()).await?;
        let space_id = space_of(&v, &req.q);
        let up = ingest::UploadReq {
            space_id: &space_id,
            folder_id: req.q.folder_id.as_deref(),
            file_name: &page.file_name,
            mime: page.kind.mime(),
            bytes: &page.bytes,
            preset: req.q.preset.as_deref(),
        };
        let image_ocr = RuntimeImageOcr { llm: &st.llm };
        let out = ingest::ingest(&st.owned, &st.doc, &st.embed, &v, &st.kb_cfg, up, Some(&image_ocr))
            .await
            .map_err(kb_err)?;
        // 与 `upload` 同序：统一再绑定目标目录，写权限在返回结果前于同一条写语句中复核。
        // html/pdf 不会产表格数据源（通道②只看 sheets 非空），无需 register_source/cleanup。
        store::move_doc(&st.owned, &v, &out.doc_id, &space_id, req.q.folder_id.as_deref())
            .await
            .map_err(kb_err)?;
        // source_uri 记**最终落地 URL**（重定向后的真实来源）。回写失败（撤权竞态/DB 抖动）
        // 不抹掉已入库文档：来源地址是治理元数据，不是入库正确性。
        if let Err(e) = store::set_doc_source_uri(&st.owned, &v, &out.doc_id, &page.final_url).await {
            tracing::warn!(doc_id = %out.doc_id, err = %e, "URL 已入库，来源地址回写失败");
        }
        let row = acl::doc_for_viewer(&st.owned, &v, &out.doc_id).await.map_err(kb_err)?;
        Ok(Json(doc_json(&row)))
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FetchedKind {
        Html,
        Pdf,
    }

    impl FetchedKind {
        /// 落盘扩展名是白名单字面量（与 `ingest::EXTS` 同集），用户输入一个字都进不了路径
        fn ext(self) -> &'static str {
            match self {
                FetchedKind::Html => "html",
                FetchedKind::Pdf => "pdf",
            }
        }
        fn mime(self) -> &'static str {
            match self {
                FetchedKind::Html => "text/html",
                FetchedKind::Pdf => "application/pdf",
            }
        }
    }

    struct FetchedPage {
        bytes: Vec<u8>,
        kind: FetchedKind,
        /// 重定向后的最终 URL（`source_uri` 写它）
        final_url: String,
        /// 由 URL 派生的展示文件名（`<slug>.<白名单扩展名>`）
        file_name: String,
    }

    /// URL 形状闸（纯函数，单测钉住）：仅 http/https、必须有 host、禁 userinfo、
    /// 端口只放 80/443、长度封顶。IP 段判定不在这里——必须等 DNS 解析后做（见 `resolve_checked`）。
    fn checked_url_shape(raw: &str) -> Result<reqwest::Url, ApiErr> {
        if raw.is_empty() || raw.len() > URL_MAX_LEN {
            return Err(err(StatusCode::BAD_REQUEST, format!("URL 不能为空且不超过 {URL_MAX_LEN} 字符")));
        }
        let url = reqwest::Url::parse(raw).map_err(|_| err(StatusCode::BAD_REQUEST, "URL 格式无效"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(err(StatusCode::BAD_REQUEST, "只支持 http:// 或 https:// 地址"));
        }
        if url.host_str().is_none() {
            return Err(err(StatusCode::BAD_REQUEST, "URL 缺少主机名"));
        }
        // userinfo（user:pass@host）的唯一用途是混淆真实目标，一律拒
        if !url.username().is_empty() || url.password().is_some() {
            return Err(err(StatusCode::BAD_REQUEST, "URL 不允许携带账号信息"));
        }
        // 只放标准 Web 端口：非常用端口是打内网服务（redis/管理面/元数据 API）的经典 SSRF 通道
        if !matches!(url.port_or_known_default(), Some(80 | 443)) {
            return Err(err(StatusCode::BAD_REQUEST, "只支持 80/443 端口的地址"));
        }
        Ok(url)
    }

    /// SSRF 核心护栏：本机/私网/链路本地/保留段一律拒。std 稳定判断（`is_private` 等）只覆盖
    /// 一部分段；CGNAT（100.64/10）与 v6 的 ULA/链路本地没有稳定 API，按段字面量补全。
    /// v4 映射的 v6 地址（::ffff:127.0.0.1）解包后按 v4 判，不允许借壳绕过。
    fn is_forbidden_ip(ip: std::net::IpAddr) -> bool {
        match ip {
            std::net::IpAddr::V4(v4) => {
                let o = v4.octets();
                v4.is_private()            // 10/8、172.16/12、192.168/16
                    || v4.is_loopback()    // 127/8
                    || v4.is_link_local()  // 169.254/16
                    || v4.is_unspecified() // 0.0.0.0
                    || v4.is_multicast()   // 224/4
                    || v4.is_broadcast()   // 255.255.255.255
                    || o[0] == 0           // 0/8「本网络」
                    || (o[0] == 100 && (64..=127).contains(&o[1])) // 100.64/10 CGNAT 共享段
                    || o[0] >= 240 // 240/4 保留段
            }
            std::net::IpAddr::V6(v6) => {
                if let Some(mapped) = v6.to_ipv4_mapped() {
                    return is_forbidden_ip(std::net::IpAddr::V4(mapped));
                }
                let seg = v6.segments();
                v6.is_loopback()           // ::1
                    || v6.is_unspecified() // ::
                    || v6.is_multicast()   // ff00::/8
                    || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA 私网段
                    || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 链路本地
            }
        }
    }

    /// DNS 解析 + **全量** IP 校验：任一解析结果落在禁止段即整域拒绝（客户端可能挑任一地址
    /// 连接，放过一个就是放过全部）。返回的地址随后钉进 reqwest 的 DNS 覆盖
    /// （`resolve_to_addrs`）——连接时不再二次解析，这是防 DNS rebinding
    /// （校验时返公网 IP、连接时换成 127.0.0.1）的那道闸。
    async fn resolve_checked(url: &reqwest::Url) -> Result<Vec<std::net::SocketAddr>, ApiErr> {
        let host = url.host_str().unwrap_or_default();
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| err(StatusCode::BAD_GATEWAY, "目标地址无法解析"))?
            .collect();
        if addrs.is_empty() {
            return Err(err(StatusCode::BAD_GATEWAY, "目标地址无法解析"));
        }
        if addrs.iter().any(|a| is_forbidden_ip(a.ip())) {
            return Err(err(StatusCode::BAD_REQUEST, "目标地址指向内网或本机，不允许抓取"));
        }
        Ok(addrs)
    }

    /// 服务端抓取（SSRF 护栏全链）：
    /// 1. 每跳都过 `checked_url_shape`——重定向目标同样受限，跳转不是绕护栏的后门；
    /// 2. 每跳 DNS 解析后全量校验 IP，并把校验过的地址钉进该跳专用 client（防 rebinding）；
    /// 3. 重定向手动跟随 ≤3 跳（reqwest 自动跟随无法在跳转间重验目标，故 `Policy::none()`）；
    /// 4. 15s 总超时、5MB 大小帽（Content-Length 预检只是早退，真正的帽是分块流式累计）。
    async fn fetch_url_guarded(raw: &str) -> Result<FetchedPage, ApiErr> {
        let mut current = checked_url_shape(raw)?;
        for hop in 0..=URL_FETCH_MAX_REDIRECTS {
            let addrs = resolve_checked(&current).await?;
            let client = reqwest::Client::builder()
                .timeout(URL_FETCH_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(current.host_str().unwrap_or_default(), &addrs)
                .user_agent(concat!("dms-kb-url-ingest/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "抓取客户端初始化失败"))?;
            let mut resp = client
                .get(current.clone())
                .send()
                .await
                .map_err(|_| err(StatusCode::BAD_GATEWAY, "目标地址抓取失败或超时（15s）"))?;
            let status = resp.status();
            if status.is_redirection() {
                if hop == URL_FETCH_MAX_REDIRECTS {
                    return Err(err(StatusCode::BAD_REQUEST, "重定向次数过多（最多 3 次）"));
                }
                let location = resp
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| err(StatusCode::BAD_GATEWAY, "上游返回了无效的重定向"))?;
                // join 兼容相对跳转；形状闸与 IP 闸在下一跳开头重验
                current = current
                    .join(location)
                    .map_err(|_| err(StatusCode::BAD_GATEWAY, "上游返回了无效的重定向"))?;
                continue;
            }
            if !status.is_success() {
                return Err(err(StatusCode::BAD_GATEWAY, format!("目标地址返回 HTTP {status}")));
            }
            if resp.content_length().is_some_and(|n| n as usize > URL_FETCH_MAX_BYTES) {
                return Err(err(StatusCode::BAD_REQUEST, "页面超过 5MB 上限，未入库"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|_| err(StatusCode::BAD_GATEWAY, "读取目标内容失败或超时"))?
            {
                if !capped_append(&mut bytes, &chunk) {
                    return Err(err(StatusCode::BAD_REQUEST, "页面超过 5MB 上限，未入库"));
                }
            }
            if bytes.is_empty() {
                return Err(err(StatusCode::BAD_REQUEST, "目标页面为空"));
            }
            let content_type = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok());
            let kind = classify_content(content_type, &bytes).ok_or_else(|| {
                err(StatusCode::BAD_REQUEST, "只支持 HTML 页面或 PDF 文档（按 Content-Type 与内容判定）")
            })?;
            let file_name = url_file_name(&current, kind);
            return Ok(FetchedPage { bytes, kind, final_url: current.to_string(), file_name });
        }
        unreachable!("重定向跳数闸在循环内已返回")
    }

    /// 流式大小帽：追加后超 5MB 返回 false（调用方中断并 400）。抽成纯函数是为了单测钉住。
    fn capped_append(buf: &mut Vec<u8>, chunk: &[u8]) -> bool {
        if buf.len() + chunk.len() > URL_FETCH_MAX_BYTES {
            return false;
        }
        buf.extend_from_slice(chunk);
        true
    }

    /// 内容分派：Content-Type 优先；`application/pdf` 头与 `%PDF-` 魔数双通道认 PDF；
    /// `text/plain`/缺报的服务器若正文明显是 HTML 也按 HTML 收。其余一律拒——入库链的
    /// 类型白名单只有 html/pdf 两个出口，这里不许放宽。
    fn classify_content(content_type: Option<&str>, bytes: &[u8]) -> Option<FetchedKind> {
        let ct = content_type.unwrap_or_default().to_ascii_lowercase();
        let ct = ct.split(';').next().unwrap_or_default().trim();
        if ct.contains("text/html") || ct.contains("application/xhtml") {
            return Some(FetchedKind::Html);
        }
        if ct.contains("application/pdf") || bytes.starts_with(b"%PDF-") {
            return Some(FetchedKind::Pdf);
        }
        if (ct.is_empty() || ct.contains("text/plain")) && looks_like_html(bytes) {
            return Some(FetchedKind::Html);
        }
        None
    }

    /// 首 512 字节转小写、去 BOM 与空白后，以 `<!doctype html` / `<html` 开头
    fn looks_like_html(bytes: &[u8]) -> bool {
        let head: Vec<u8> = bytes.iter().take(512).map(|b| b.to_ascii_lowercase()).collect();
        let text = String::from_utf8_lossy(&head);
        let text = text.trim_start_matches('\u{feff}').trim_start();
        text.starts_with("<!doctype html") || text.starts_with("<html")
    }

    /// URL → 展示文件名。slug 取路径末段（空则主机名），只留字母/数字/`-`/`_`/CJK，
    /// 其余折叠成单个 `_`，封顶 60 字符；扩展名由内容判定给（白名单字面量，路径穿越面
    /// 为零——与 `ingest::doc_path` 同一理由：原始名字从不进磁盘路径）。
    /// 仅当末段尾缀本就是 html/htm/pdf 时才剥掉重补（避免 `a.html.html`，也避免把
    /// 主机名 `example.com` 的 `.com` 误当文档扩展名剥掉）。
    fn url_file_name(url: &reqwest::Url, kind: FetchedKind) -> String {
        let raw = url
            .path_segments()
            .and_then(|segs| segs.filter(|s| !s.is_empty()).next_back())
            .map(percent_decode)
            .unwrap_or_else(|| url.host_str().unwrap_or("page").to_string());
        let stem = match raw.rsplit_once('.') {
            Some((s, ext)) if matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm" | "pdf") => s,
            _ => raw.as_str(),
        };
        let mut slug = String::new();
        for c in stem.chars() {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') || ('\u{4e00}'..='\u{9fff}').contains(&c) {
                slug.push(c);
            } else if !slug.ends_with('_') {
                slug.push('_');
            }
            if slug.chars().count() >= 60 {
                break;
            }
        }
        let slug = slug.trim_matches('_');
        let slug = if slug.is_empty() { "page" } else { slug };
        format!("{slug}.{}", kind.ext())
    }

    /// 路径段的 percent-decode（`path_segments()` 返回的是编码形态，CJK 文件名不先解就是
    /// 一串 `E6_8A`）。非法 `%` 序列按原样保留——只服务展示名，解码失败不产生安全后果。
    /// 手写 20 行避免为一个解码点扩大依赖面（同 `encode_base64` 的理由）。
    fn percent_decode(seg: &str) -> String {
        let bytes = seg.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    // ───────────────────── 【Y7】空间导出 ─────────────────────

    /// 导出护栏：单页文档上限（响应体有界，大空间按 `next_offset` 翻页拉取）与目录数上限。
    const EXPORT_MAX_LIMIT: i64 = 500;
    const EXPORT_MAX_FOLDERS: usize = 2000;

    #[derive(serde::Deserialize, Default)]
    pub struct ExportQuery {
        offset: Option<i64>,
        limit: Option<i64>,
        login_name: Option<String>,
        role_code: Option<String>,
    }

    /// 分页参数收口（纯函数，单测钉住）：limit 缺省=封顶值，clamp 到 [1, EXPORT_MAX_LIMIT]；
    /// offset 负值归零。
    fn export_page_params(limit: Option<i64>, offset: Option<i64>) -> (i64, i64) {
        (limit.unwrap_or(EXPORT_MAX_LIMIT).clamp(1, EXPORT_MAX_LIMIT), offset.unwrap_or(0).max(0))
    }

    /// `GET /api/kb/space/{id}/export?offset=&limit=`：空间文档清单 + 元数据 + 目录树 +
    /// chunk 计数（doc_json 全字段，**不含向量**——向量从不进任何 API 响应）。
    /// 空间读权限（fail-closed：不存在与不可读同 403，不暴露空间存在性，与 `docs` 同口径）。
    pub async fn export_space(
        State(st): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(id): Path<String>,
        Query(q): Query<ExportQuery>,
    ) -> Result<ApiOk, ApiErr> {
        let kq = KbQuery {
            login_name: q.login_name, role_code: q.role_code,
            space_id: None, folder_id: None, preset: None,
        };
        let v = manager_viewer(&st, &headers, &kq).await?;
        if !acl::space_readable(&st.owned, &v, &id).await.map_err(kb_err)? {
            return Err(err(StatusCode::FORBIDDEN, format!("无权访问知识空间 {id}")));
        }
        let (limit, offset) = export_page_params(q.limit, q.offset);
        let total = store::count_space_docs(&st.owned, &v, &id).await.map_err(kb_err)?;
        let rows = store::list_docs_page(&st.owned, &v, &id, limit, offset).await.map_err(kb_err)?;
        let mut folders = store::list_folders(&st.owned, &v, &id).await.map_err(kb_err)?;
        folders.truncate(EXPORT_MAX_FOLDERS);
        let next_offset = (offset + limit < total).then_some(offset + limit);
        Ok(Json(serde_json::json!({
            "space_id": id,
            "offset": offset,
            "limit": limit,
            "total_docs": total,
            "next_offset": next_offset,
            "folders": folders.iter().map(folder_json).collect::<Vec<_>>(),
            "docs": rows.iter().map(doc_json).collect::<Vec<_>>(),
        })))
    }

    // ───────────────────── 【Y7】AI 文档描述 ─────────────────────

    /// fast 调用预算（与样例问题/思维导图同一档：锦上添花，不许拖住页面）
    const DESC_LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    /// 写回上限：DB 列无长度约束，应用层收口；列表展示与 metadata 召回语料都读这一列
    const DESC_MAX_CHARS: usize = 500;
    /// 摘录预算：开头 6 块 × 300 字（开头是标题/导语，最具代表性；prompt 体量封顶）
    const DESC_EXCERPT_CHUNKS: i64 = 6;
    const DESC_EXCERPT_CLIP_CHARS: usize = 300;

    /// 摘录 SQL。文档已过 `doc_for_viewer` + 空间写闸，这里仍内联一次空间级读谓词——
    /// 撤权若发生在两步之间，本语句一行都返不出（同 `retrieve`/usage_api 两步内联的理由）。
    const DESC_EXCERPT_SQL: &str =
        "SELECT c.text FROM kb.chunk c JOIN kb.doc d ON d.doc_id = c.doc_id
         WHERE c.doc_id = $1
           AND d.enabled = true AND d.status IN ('chunked','embedded')
           AND EXISTS (SELECT 1 FROM kb.space s WHERE s.space_id = d.space_id
             AND (s.owner = $2 OR EXISTS (SELECT 1 FROM kb.acl a
               WHERE a.scope = 'space' AND a.target_id = s.space_id
                 AND a.perm IN ('read','write')
                 AND ((a.grantee_kind = 'login' AND a.grantee = $2)
                   OR (a.grantee_kind = 'role' AND a.grantee = ANY($3::text[]))))))
         ORDER BY c.ord LIMIT $4";

    #[derive(serde::Deserialize, Default)]
    pub struct DescriptionReq {
        login_name: Option<String>,
        role_code: Option<String>,
    }

    /// `POST /api/kb/doc/{id}/description`：fast 档按开头摘录生成一段描述并**写回
    /// `kb.doc.description`**（该列即持久层，不另设 kv 缓存；重新生成=显式点击覆盖）。
    /// 写权限 = 空间写（fail-closed，写语句内联复核）。描述列已挂进检索 metadata 召回语料
    /// （`retrieve::METADATA_SQL`，见该处 Y7 注释）。
    ///
    /// 🔴 LLM 失败**不写回不编造**（与样例问题的回退不同）：描述会进检索语料，
    /// 一段编造的假描述比没有描述更糟。
    pub async fn generate_description(
        State(st): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(id): Path<String>,
        Json(req): Json<DescriptionReq>,
    ) -> Result<ApiOk, ApiErr> {
        let kq = KbQuery {
            login_name: req.login_name, role_code: req.role_code,
            space_id: None, folder_id: None, preset: None,
        };
        let v = manager_viewer(&st, &headers, &kq).await?;
        let row = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
        if !acl::space_writable(&st.owned, &v, &row.space_id).await.map_err(kb_err)? {
            return Err(err(StatusCode::FORBIDDEN, format!("无权修改空间 {} 的文档", row.space_id)));
        }
        if !matches!(row.status.as_str(), "chunked" | "embedded") || row.chunk_count == 0 {
            return Err(err(StatusCode::CONFLICT, "文档尚未完成入库或无可检索内容，无法生成描述"));
        }
        let chunks: Vec<(String,)> = st
            .owned
            .fixed(DESC_EXCERPT_SQL)
            .bind(&id)
            .bind(&v.login)
            .bind(&v.roles)
            .bind(DESC_EXCERPT_CHUNKS)
            .fetch_all()
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
        let mut excerpt = String::new();
        for (text,) in &chunks {
            excerpt.push_str(&text.chars().take(DESC_EXCERPT_CLIP_CHARS).collect::<String>());
            excerpt.push('\n');
        }
        if excerpt.trim().is_empty() {
            return Err(err(StatusCode::CONFLICT, "文档暂无可摘录内容，无法生成描述"));
        }
        const SYSTEM: &str = "你是知识库助手。根据用户提供的文档开头摘录，用中文写一段该文档的内容描述，\
            供知识库检索与列表展示使用。只输出描述本身：一两句话、不超过 120 字、客观陈述文档的\
            主题与用途；不编号、不加引号、不复述文件名、不评价。";
        let user = format!("文档《{}》摘录：\n{excerpt}", row.name);
        let mut chat = ChatRequest::text(ModelTier::Fast, SYSTEM, &user, Some(0.1));
        chat.max_tokens = Some(300);
        let reply = tokio::time::timeout(DESC_LLM_TIMEOUT, st.llm.chat(chat))
            .await
            .map_err(|_| err(StatusCode::BAD_GATEWAY, "AI 描述生成超时，请稍后重试"))?
            .map_err(|_| err(StatusCode::BAD_GATEWAY, "AI 描述生成暂时不可用，请稍后重试"))?;
        let desc = sanitize_description(reply.content.as_deref().unwrap_or_default());
        if desc.is_empty() {
            return Err(err(StatusCode::BAD_GATEWAY, "AI 未返回有效描述，请稍后重试"));
        }
        store::set_doc_description(&st.owned, &v, &id, &desc).await.map_err(kb_err)?;
        let updated = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
        Ok(Json(doc_json(&updated)))
    }

    /// LLM 产出 → 单段描述：压缩空白为一行、剥首尾成对引号、封顶 `DESC_MAX_CHARS`。
    fn sanitize_description(text: &str) -> String {
        let mut out = String::new();
        let mut pending_space = false;
        for c in text.trim().chars() {
            if c.is_whitespace() {
                pending_space = true;
                continue;
            }
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
            if out.chars().count() >= DESC_MAX_CHARS {
                break;
            }
        }
        out.trim_matches(|c| matches!(c, '"' | '\'' | '「' | '」' | '“' | '”' | '‘' | '’'))
            .trim()
            .to_string()
    }

    #[cfg(test)]
    mod ops_tests {
        use super::*;

        /// URL 形状闸：非法 scheme / 缺 host / userinfo / 非常用端口 / 超长一律 400
        #[test]
        fn url_shape_gate_rejects_non_web_and_confusing_targets() {
            for bad in [
                "file:///etc/passwd",
                "ftp://example.com/a.pdf",
                "javascript:alert(1)",
                "http://",
                "http://user:pass@example.com/",
                "http://user@example.com/",
                "http://example.com:8080/",
                "https://example.com:6379/",
                "gopher://example.com/",
            ] {
                assert!(checked_url_shape(bad).is_err(), "{bad}");
            }
            let too_long = format!("https://example.com/{}", "a".repeat(URL_MAX_LEN));
            assert!(checked_url_shape(&too_long).is_err());
            assert!(checked_url_shape("").is_err());
            for ok in [
                "http://example.com/a/b.html",
                "https://example.com:443/x.pdf",
                "http://example.com:80/",
                "https://example.com",
                "https://example.com/path?q=1#frag",
            ] {
                assert!(checked_url_shape(ok).is_ok(), "{ok}");
            }
        }

        /// SSRF IP 护栏：私网/环回/链路本地/CGNAT/保留段/v6 ULA/v4 映射 v6 全拒；公网放行
        #[test]
        fn ssrf_ip_blocklist_covers_private_loopback_and_reserved() {
            use std::net::IpAddr;
            let blocked = [
                "127.0.0.1", "127.0.1.1", "10.0.0.1", "10.255.255.255", "172.16.0.1",
                "172.31.255.255", "192.168.1.1", "169.254.1.1", "0.0.0.0", "0.1.2.3",
                "100.64.0.1", "100.127.255.254", "224.0.0.1", "240.0.0.1",
                "255.255.255.255", "::1", "::", "fe80::1", "fc00::1", "fd00::1",
                "ff02::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:192.168.0.1",
            ];
            for ip in blocked {
                assert!(is_forbidden_ip(ip.parse::<IpAddr>().unwrap()), "{ip} 应被拒");
            }
            let allowed = ["8.8.8.8", "1.1.1.1", "100.63.255.255", "100.128.0.1", "172.15.0.1", "172.32.0.1", "2606:4700:4700::1111"];
            for ip in allowed {
                assert!(!is_forbidden_ip(ip.parse::<IpAddr>().unwrap()), "{ip} 应放行");
            }
        }

        /// 大小帽：累计超 5MB 即拒（Content-Length 缺报/谎报时的真正护栏）
        #[test]
        fn fetch_cap_aborts_over_5mb_stream() {
            let mut buf = Vec::new();
            assert!(capped_append(&mut buf, &vec![0u8; URL_FETCH_MAX_BYTES]));
            assert_eq!(buf.len(), URL_FETCH_MAX_BYTES);
            assert!(!capped_append(&mut buf, &[1u8; 1]), "超帽必须拒");
            let mut small = Vec::new();
            assert!(capped_append(&mut small, &[0u8; 1024]));
            assert!(!capped_append(&mut small, &vec![0u8; URL_FETCH_MAX_BYTES]));
            assert_eq!(small.len(), 1024, "拒绝时不得污染已读内容");
        }

        /// 内容分派：Content-Type 优先，PDF 魔数兜底，text/plain 误配的 HTML 也收，其余一律拒
        #[test]
        fn fetched_content_classification_is_html_or_pdf_only() {
            assert_eq!(classify_content(Some("text/html; charset=utf-8"), b"x"), Some(FetchedKind::Html));
            assert_eq!(classify_content(Some("application/xhtml+xml"), b"x"), Some(FetchedKind::Html));
            assert_eq!(classify_content(Some("application/pdf"), b"x"), Some(FetchedKind::Pdf));
            assert_eq!(classify_content(None, b"%PDF-1.7 rest"), Some(FetchedKind::Pdf));
            assert_eq!(classify_content(Some("application/octet-stream"), b"%PDF-1.4"), Some(FetchedKind::Pdf));
            assert_eq!(classify_content(Some("text/plain"), b"  <!DOCTYPE html><html>"), Some(FetchedKind::Html));
            assert_eq!(classify_content(None, b"<html lang=\"zh\">"), Some(FetchedKind::Html));
            assert_eq!(classify_content(Some("text/plain"), b"\xef\xbb\xbf<html>"), Some(FetchedKind::Html));
            for (ct, body) in [
                (Some("image/png"), &b"\x89PNG"[..]),
                (Some("application/zip"), &b"PK\x03\x04"[..]),
                (Some("text/plain"), &b"just some text"[..]),
                (None, &b"{}"[..]),
            ] {
                assert_eq!(classify_content(ct, body), None, "{ct:?}");
            }
        }

        /// 文件名派生：slug 清洗、尾缀剥除、CJK 保留、长度封顶、空名回退
        #[test]
        fn url_file_name_sanitizes_and_caps() {
            let u = |s: &str| reqwest::Url::parse(s).unwrap();
            assert_eq!(url_file_name(&u("https://example.com/a/b.html"), FetchedKind::Html), "b.html");
            assert_eq!(url_file_name(&u("https://example.com/report.pdf"), FetchedKind::Pdf), "report.pdf");
            assert_eq!(url_file_name(&u("https://example.com/report.pdf?v=2"), FetchedKind::Pdf), "report.pdf");
            assert_eq!(url_file_name(&u("https://example.com/"), FetchedKind::Html), "example_com.html");
            assert_eq!(url_file_name(&u("https://example.com/.../!!!/"), FetchedKind::Html), "page.html");
            assert_eq!(url_file_name(&u("https://example.com/报销制度-2026"), FetchedKind::Html), "报销制度-2026.html");
            // percent 编码段先解码再清洗；非法 % 序列原样折叠，不炸
            assert_eq!(url_file_name(&u("https://example.com/%E6%8A%A5%E9%94%80"), FetchedKind::Html), "报销.html");
            assert_eq!(url_file_name(&u("https://example.com/%zz%2"), FetchedKind::Html), "zz_2.html");
            // 末段带点号也只吃主体，清洗后不残留路径分隔符
            let name = url_file_name(&u("https://example.com/dir/../../etc/passwd"), FetchedKind::Html);
            assert!(!name.contains('/') && !name.contains('\\'), "{name}");
            let long = url_file_name(&u(&format!("https://example.com/{}", "长".repeat(200))), FetchedKind::Html);
            assert!(long.trim_end_matches(".html").chars().count() <= 60, "{long}");
        }

        /// 导出分页收口：limit clamp 到 [1, 500]，offset 负值归零
        #[test]
        fn export_pagination_params_are_clamped() {
            assert_eq!(export_page_params(None, None), (EXPORT_MAX_LIMIT, 0));
            assert_eq!(export_page_params(Some(0), Some(-5)), (1, 0));
            assert_eq!(export_page_params(Some(10_000), Some(3)), (EXPORT_MAX_LIMIT, 3));
            assert_eq!(export_page_params(Some(50), Some(500)), (50, 500));
        }

        /// 描述整形：多行压一行、引号剥除、封顶 500 字
        #[test]
        fn description_is_single_line_and_capped() {
            assert_eq!(sanitize_description("  这是\n一份　制度\r\n文件  "), "这是 一份 制度 文件");
            assert_eq!(sanitize_description("「报销制度说明」"), "报销制度说明");
            assert_eq!(sanitize_description("\"quoted\""), "quoted");
            assert_eq!(sanitize_description("  \n \t"), "");
            let long = sanitize_description(&"描".repeat(600));
            assert_eq!(long.chars().count(), DESC_MAX_CHARS);
        }

        /// 三个新 handler 的接线契约注释必须在（集成方按注释加 main.rs 注册行）
        #[test]
        fn ops_pack_handlers_carry_wiring_contracts() {
            let src = include_str!("kb_api.rs");
            for route in [
                ".route(\"/api/kb/ingest-url\", post(kb_api::ingest_url))",
                ".route(\"/api/kb/space/{id}/export\", get(kb_api::export_space))",
                ".route(\"/api/kb/doc/{id}/description\", post(kb_api::generate_description))",
            ] {
                assert!(src.contains(route), "缺接线契约注释: {route}");
            }
            // 摘录 SQL 必须内联空间级读谓词（撤权竞态 fail-closed）
            assert!(DESC_EXCERPT_SQL.contains("a.perm IN ('read','write')"));
            // URL 入库与上传共用同一条 ingest 链（不许有第二份入库实现）
            let body = src.split("pub async fn ingest_url").nth(1).unwrap();
            let body = body.split("enum FetchedKind").next().unwrap();
            assert!(body.contains("ingest::ingest(") && body.contains("store::move_doc("));
            assert!(body.find("fetch_url_guarded").unwrap() < body.find("ingest::ingest(").unwrap());
        }
    }
}
pub(crate) use ops_pack::*;

pub async fn space_grants(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    manager_principal(&st, &headers, &q.login_name, &q.role_code).await?;
    ensure_space_exists(&st, &id).await?;
    let roles = dms_role_options(&st).await?;
    let role_names: HashMap<&str, &str> = roles
        .iter()
        .map(|r| (r.role_code.as_str(), r.role_name.as_str()))
        .collect();
    let rows = acl::list_target(&st.owned, acl::AclScope::Space, &id).await.map_err(kb_err)?;
    Ok(Json(serde_json::json!({
        "grants": rows.into_iter().map(|r| {
            let grantee_name = (r.grantee_kind == "role")
                .then(|| role_names.get(r.grantee.as_str()).copied())
                .flatten();
            serde_json::json!({
                "grantee_kind": r.grantee_kind, "grantee": r.grantee, "perm": r.perm,
                "grantee_name": grantee_name,
            })
        }).collect::<Vec<_>>()
        ,"roles": roles,
        "limits": { "batch_grants": MAX_BATCH_GRANTS }
    })))
}

pub async fn grant_space(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SpaceGrantReq>,
) -> Result<(StatusCode, ApiOk), ApiErr> {
    manager_principal(&st, &headers, &req.login_name, &req.role_code).await?;
    ensure_space_exists(&st, &id).await?;

    if !req.role_codes.is_empty() {
        return grant_roles(&st, &id, &req).await;
    }

    let entry = space_acl_entry(&id, &req)?;
    validate_grantee(&st, &entry.grantee).await?;
    store::grant_space_acl(
        &st.owned,
        &id,
        entry.grantee.kind(),
        entry.grantee.id(),
        entry.perm.as_str(),
    )
    .await
    .map_err(kb_err)?;
    Ok((StatusCode::OK, Json(serde_json::json!({
        "ok": true, "updated": true, "succeeded": 1, "failed": []
    }))))
}

async fn grant_roles(
    st: &AppState,
    id: &str,
    req: &SpaceGrantReq,
) -> Result<(StatusCode, ApiOk), ApiErr> {
    if req.grantee_kind != "role" {
        return Err(err(StatusCode::BAD_REQUEST, "批量授权只支持 DMS 角色"));
    }
    if req.role_codes.len() > MAX_BATCH_GRANTS {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("单次最多授权 {MAX_BATCH_GRANTS} 个角色"),
        ));
    }
    let perm = acl::Perm::parse(req.perm.as_deref().unwrap_or("read"))
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "perm 只能是 read 或 write"))?;
    let roles = dms_role_options(st).await?;
    let catalog: HashMap<&str, &str> = roles
        .iter()
        .map(|r| (r.role_code.as_str(), r.role_name.as_str()))
        .collect();
    let codes = validated_role_codes(&req.role_codes, &catalog)
        .map_err(|reason| err(StatusCode::BAD_REQUEST, format!("{reason}，未写入任何授权")))?;
    store::grant_space_roles(&st.owned, id, &codes, perm.as_str())
        .await
        .map_err(kb_err)?;
    let succeeded = codes
        .iter()
        .map(|code| serde_json::json!({
            "role_code": code,
            "role_name": catalog.get(code.as_str()).copied(),
            "perm": perm.as_str(),
        }))
        .collect::<Vec<_>>();
    Ok((StatusCode::OK, Json(serde_json::json!({
        "ok": true, "updated": true, "partial": false, "succeeded": succeeded, "failed": [],
    }))))
}

fn validated_role_codes(
    raw: &[String],
    catalog: &HashMap<&str, &str>,
) -> Result<Vec<String>, &'static str> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw_code in raw {
        let code = raw_code.trim();
        if code.is_empty() {
            return Err("角色编码不能为空");
        }
        if code.chars().count() > MAX_ROLE_CODE {
            return Err("角色编码超过 DMS 字段上限");
        }
        if !catalog.contains_key(code) {
            return Err("所选 DMS 角色不存在");
        }
        if seen.insert(code.to_string()) {
            out.push(code.to_string());
        }
    }
    if out.is_empty() { Err("至少选择一个角色") } else { Ok(out) }
}

pub async fn revoke_space(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(req): Query<SpaceGrantReq>,
) -> Result<ApiOk, ApiErr> {
    manager_principal(&st, &headers, &req.login_name, &req.role_code).await?;
    ensure_space_exists(&st, &id).await?;
    let entry = space_acl_entry(&id, &req)?;
    // 撤权不要求对象仍存在于 DMS；否则角色被删除后会留下永远无法清理的历史 ACL。
    store::revoke_space_acl(
        &st.owned,
        &id,
        entry.grantee.kind(),
        entry.grantee.id(),
        entry.perm.as_str(),
    )
    .await
    .map_err(kb_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn space_acl_entry(id: &str, req: &SpaceGrantReq) -> Result<acl::AclEntry, ApiErr> {
    let grantee_id = req.grantee.trim();
    let grantee = acl::Grantee::parse(&req.grantee_kind, grantee_id)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "grantee_kind 只能是 login 或 role"))?;
    let max = match &grantee {
        acl::Grantee::Login(_) => MAX_LOGIN_NAME,
        acl::Grantee::Role(_) => MAX_ROLE_CODE,
    };
    if grantee_id.is_empty() || grantee_id.chars().count() > max {
        return Err(err(StatusCode::BAD_REQUEST, format!("授权对象不能为空且不超过 {max} 字")));
    }
    let perm = acl::Perm::parse(req.perm.as_deref().unwrap_or("read"))
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "perm 只能是 read 或 write"))?;
    Ok(acl::AclEntry { scope: acl::AclScope::Space, target_id: id.to_string(), grantee, perm })
}

async fn validate_grantee(st: &AppState, grantee: &acl::Grantee) -> Result<(), ApiErr> {
    match grantee {
        acl::Grantee::Login(login) => {
            let active = st
                .auth_mysql
                .fixed(
                    "SELECT COUNT(*) FROM t_employee \
                     WHERE login_name=? AND deleted_flag=0 AND disabled_flag=0",
                )
                .bind(login)
                .fetch_optional::<(i64,)>()
                .await
                .map_err(|_| err(StatusCode::BAD_GATEWAY, "DMS 账号目录暂时不可用"))?
                .is_some_and(|(n,)| n > 0);
            if !active {
                return Err(err(StatusCode::BAD_REQUEST, "DMS 账号不存在、已删除或已禁用"));
            }
        }
        acl::Grantee::Role(code) => {
            if !dms_role_options(st).await?.iter().any(|role| role.role_code == *code) {
                return Err(err(StatusCode::BAD_REQUEST, "DMS 角色不存在"));
            }
        }
    }
    Ok(())
}

const DMS_ROLE_OPTIONS_SQL: &str =
    "SELECT TRIM(role_code), TRIM(role_name) \
     FROM t_role \
     WHERE role_code IS NOT NULL AND TRIM(role_code)<>'' \
       AND role_name IS NOT NULL AND TRIM(role_name)<>'' \
     ORDER BY TRIM(role_name), TRIM(role_code) \
     LIMIT 500";

/// 对齐 DMS `RoleService.getAllRole()`：单表、显式字段、有界枚举，不关联员工表。
async fn dms_role_options(st: &AppState) -> Result<Vec<RoleOption>, ApiErr> {
    let rows: Vec<(String, String)> = st
        .auth_mysql
        .fixed(DMS_ROLE_OPTIONS_SQL)
        .fetch_all()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "DMS 角色目录暂时不可用"))?;
    let mut seen = HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|(role_code, _)| seen.insert(role_code.clone()))
        .map(|(role_code, role_name)| RoleOption { role_code, role_name })
        .collect())
}

async fn ensure_space_exists(st: &AppState, id: &str) -> Result<(), ApiErr> {
    if store::space_exists(&st.owned, id).await.map_err(kb_err)? {
        Ok(())
    } else {
        Err(err(StatusCode::NOT_FOUND, format!("知识空间 {id} 不存在")))
    }
}

/// 【K4 通道②】登记数据源。可见性由数据源注册表动态继承来源文档的空间 owner/ACL，
/// 不复制一套会与空间分享、撤权漂移的 ds ACL。
///
/// 文本检索已经可用时，问数源失败不抹掉整份文档；但物理 schema、数据源登记与结构注册
/// 必须成套成功，否则立即清理半成品并把降级提示写回文档。
async fn register_source(st: &AppState, v: &Viewer, doc_name: &str, out: &ingest::Ingested) -> bool {
    let Some(src) = &out.source else { return true };
    let allowed = match store::get_doc(&st.owned, &out.doc_id).await {
        Ok(Some(row)) => acl::space_writable(&st.owned, v, &row.space_id).await.unwrap_or(false),
        _ => false,
    };
    if !allowed {
        let _ = cleanup_source(st, &out.doc_id).await;
        tracing::warn!(doc_id = %out.doc_id, reason = "upload_permission_revoked", "表格数据源登记前写权限已失效");
        return false;
    }
    let desc = ds_description(doc_name, src);
    if dms_semantic::registry::datasource::register_upload_datasource(
        st.owned.pool(),
        &src.ds_id,
        doc_name,
        &desc,
    )
    .await
    .is_err()
    {
        let _ = cleanup_source(st, &out.doc_id).await;
        let _ = store::append_notice(
            &st.owned,
            v,
            &out.doc_id,
            "表格已入知识库，问数数据源登记失败，请重新上传",
        )
        .await;
        tracing::warn!(ds_id = %src.ds_id, doc_id = %out.doc_id, reason = "datasource_register_failed", "上传表格已建表，但登记数据源失败");
        return false;
    }
    if !sync_upload_schema(st, src).await {
        let _ = cleanup_source(st, &out.doc_id).await;
        let _ = store::append_notice(
            &st.owned,
            v,
            &out.doc_id,
            "表格已入知识库，问数结构采集失败，请重新上传",
        )
        .await;
        return false;
    }
    let still_allowed = match store::get_doc(&st.owned, &out.doc_id).await {
        Ok(Some(row)) => acl::space_writable(&st.owned, v, &row.space_id).await.unwrap_or(false),
        _ => false,
    };
    if !still_allowed {
        let _ = cleanup_source(st, &out.doc_id).await;
        tracing::warn!(doc_id = %out.doc_id, reason = "upload_permission_revoked", "表格数据源登记后写权限已失效，已清理");
        return false;
    }
    true
}

/// 【K4 通道②之三】把上传表的表/列结构采进 `meta.table_doc` / `column_doc`。
///
/// 🔴 **没有这一步，「上传即可问数」是空转的**：召回（`recall::schema`）只读注册表，
/// 而注册表此前只有 `ds_id='dms'` 的行 —— LLM 拿到的「可用表结构」是空段，
/// 于是它照着别处的表名硬猜，实测报的是 `relation ... does not exist`。
/// 建表已成、检索已通、只有问数死掉，仍是那种最难归因的半可用。
///
/// 复用既有两级机件而不是从 `UploadTableSpec` 手写一份行：`probe_schema()`（PG 探针）
/// + `sync_schema()`（幂等 upsert + 清理陈旧行 + 注释清洗 F4 + trgm 的 `search_doc`）。
/// 手写那份会漏掉清洗与 `search_doc`，而漏了清洗就是把上传表头当权威注释送进 prompt。
///
/// 中文表头进的是**列注释**（列名退化成 `c0/c1`，标识符安全），所以注释是 LLM 认出
/// 「c2 是部门」的唯一线索 —— 采集必须带上它，`probe_schema` 的 `col_description` 就是它。
///
/// 返回是否形成完整可问数数据源；失败由调用方撤销物理表与登记信息。
async fn sync_upload_schema(st: &AppState, src: &tabular::TabularSource) -> bool {
    let spec = dms_connector::registry::DsSpec {
        ds_id: dms_kernel::DsId::new(&src.ds_id),
        kind: dms_connector::source::SourceKind::Postgres,
        dsn_ref: dms_semantic::registry::datasource::UPLOAD_DSN_REF.into(),
        max_conn: 2,
        schema: Some(src.schema.clone()),
    };
    let synced = async {
        // 走 `sources.get` 而不是自己建池：顺带让 F3 自检（只读角色不许看见 meta/kb/chat）
        // 在**上传当场**跑一次 —— 否则 `pg_ro_url` 配错要等到第一次问数才暴露。
        let source = st.sources.get(&spec).await?;
        let snap = source.probe_schema().await?;
        // `false`＝不过滤备份表：这些表名是我们自己生成的，而备份表启发式会误伤约 1/6 的
        // uuid（详见 `sync_schema` 的文档与 `upload_table_names_do_trip_the_backup_heuristic`）
        let n =
            dms_semantic::ingest::schema_sync::sync_schema(st.owned.pool(), &src.ds_id, &snap, false)
                .await?;
        Ok::<_, anyhow::Error>(n)
    }
    .await;
    match synced {
        Ok((tables, columns)) => {
            tracing::info!(ds_id = %src.ds_id, tables, columns, "上传源 schema 已入注册表");
            true
        }
        Err(_) => {
            tracing::warn!(
                ds_id = %src.ds_id, schema = %src.schema, reason = "schema_sync_failed",
                "上传表格已登记数据源，但 schema 采集失败——已撤销不可用问数源"
            );
            false
        }
    }
}

/// 数据源描述：**向量选源（K3-B）的唯一素材**，得写清「这是什么表、有哪些 sheet」。
fn ds_description(doc_name: &str, src: &tabular::TabularSource) -> String {
    let tables: Vec<String> = src
        .tables
        .iter()
        .map(|t| format!("{}（表 {}，{} 行）", t.sheet, t.table, t.rows))
        .collect();
    let mut d = format!(
        "上传表格《{doc_name}》物化的 PG 数据源，schema {}：{}",
        src.schema,
        tables.join("；")
    );
    if !src.skipped.is_empty() {
        d.push_str(&format!("。空表或无表头未建表的 sheet：{}", src.skipped.join("、")));
    }
    d
}

fn source_json(src: &tabular::TabularSource) -> serde_json::Value {
    serde_json::json!({
        "ds_id": src.ds_id,
        "schema": src.schema,
        "tables": src.tables.iter().map(|t| serde_json::json!({
            "sheet": t.sheet, "table": t.table, "rows": t.rows,
        })).collect::<Vec<_>>(),
        "skipped": src.skipped,
    })
}

pub async fn docs(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = manager_viewer(&st, &headers, &q).await?;
    let space_id = space_of(&v, &q);
    if !acl::space_readable(&st.owned, &v, &space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权访问知识空间 {space_id}")));
    }
    let rows = store::list_docs(&st.owned, &v, &space_id).await.map_err(kb_err)?;
    let folders = store::list_folders(&st.owned, &v, &space_id).await.map_err(kb_err)?;
    let docs: Vec<serde_json::Value> = rows.iter().map(doc_json).collect();
    Ok(Json(serde_json::json!({
        "space_id": space_id,
        "folders": folders.iter().map(folder_json).collect::<Vec<_>>(),
        "docs": docs,
    })))
}

#[derive(serde::Deserialize, Default)]
pub struct FolderReq {
    name: String,
    parent_id: Option<String>,
    space_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct MoveDocReq {
    folder_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

pub async fn folders(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = manager_viewer(&st, &headers, &q).await?;
    let space_id = space_of(&v, &q);
    if !acl::space_readable(&st.owned, &v, &space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权访问知识空间 {space_id}")));
    }
    let rows = store::list_folders(&st.owned, &v, &space_id).await.map_err(kb_err)?;
    Ok(Json(serde_json::json!({
        "space_id": space_id,
        "folders": rows.iter().map(folder_json).collect::<Vec<_>>(),
    })))
}

pub async fn create_folder(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<FolderReq>,
) -> Result<ApiOk, ApiErr> {
    let q = KbQuery {
        login_name: req.login_name, role_code: req.role_code,
        space_id: req.space_id, folder_id: None,
        preset: None,
    };
    let v = manager_viewer(&st, &headers, &q).await?;
    let space_id = space_of(&v, &q);
    if space_id == v.login {
        store::ensure_space(&st.owned, &space_id, &v.login).await.map_err(kb_err)?;
    }
    if !acl::space_writable(&st.owned, &v, &space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权修改知识空间 {space_id}")));
    }
    let row = store::create_folder(
        &st.owned, &v, &space_id, req.parent_id.as_deref(), &req.name,
    )
    .await
    .map_err(kb_err)?;
    Ok(Json(folder_json(&row)))
}

pub async fn update_folder(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<FolderReq>,
) -> Result<ApiOk, ApiErr> {
    let q = KbQuery {
        login_name: req.login_name, role_code: req.role_code,
        space_id: req.space_id, folder_id: None,
        preset: None,
    };
    let v = manager_viewer(&st, &headers, &q).await?;
    let row = store::move_folder(
        &st.owned,
        &v,
        &id,
        q.space_id.as_deref(),
        req.parent_id.as_deref(),
        &req.name,
    )
        .await
        .map_err(kb_err)?;
    Ok(Json(folder_json(&row)))
}

pub async fn delete_folder(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = manager_viewer(&st, &headers, &q).await?;
    store::delete_folder(&st.owned, &v, &id, q.space_id.as_deref())
        .await
        .map_err(kb_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn move_doc(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<MoveDocReq>,
) -> Result<ApiOk, ApiErr> {
    let q = KbQuery {
        login_name: req.login_name, role_code: req.role_code,
        space_id: None, folder_id: None,
        preset: None,
    };
    let v = manager_viewer(&st, &headers, &q).await?;
    let row = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    if !acl::space_writable(&st.owned, &v, &row.space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权修改空间 {} 的文档", row.space_id)));
    }
    store::move_doc(&st.owned, &v, &id, &row.space_id, req.folder_id.as_deref())
        .await
        .map_err(kb_err)?;
    let updated = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    Ok(Json(doc_json(&updated)))
}

pub async fn doc(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = manager_viewer(&st, &headers, &q).await?;
    let row = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    let related = store::related_docs(&st.owned, &v, &id).await.map_err(kb_err)?;
    let mut body = doc_json(&row);
    if let Some(obj) = body.as_object_mut() {
        obj.insert("related_documents".into(), serde_json::json!(related.iter().map(relation_json).collect::<Vec<_>>()));
    }
    Ok(Json(body))
}

/// 下载原始文档（预览/下载共用这一个只读端点）。先走与详情/引用相同的 ACL
/// （`doc_for_viewer`，不存在与不可见统一 403，fail-closed），再从 doc_id 派生的
/// 服务器文件名取内容；原始文件名只进入 RFC 5987 的展示头，不参与磁盘路径。
///
/// 🔴 Content-Type 是**扩展名白名单**（`serve_mime`），不信上传时自报的 `row.mime`：
/// 自报 mime 是攻击面——一个 `text/html`/`image/svg+xml` 就能把脚本写进预览上下文。
/// svg 刻意不在白名单（可执行脚本），html 按 text/plain 给（安全转文本），
/// Office 等一律 octet-stream + attachment + nosniff：只许下载，不许内嵌渲染。
pub async fn download_doc(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KbQuery>,
) -> Result<(HeaderMap, Vec<u8>), ApiErr> {
    let v = viewer(&st, &headers, &q).await?;
    let row = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    let path = stored_file(&st.kb_cfg.root, &row.doc_id)
        .await
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "原始文件已不存在"))?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "文档文件暂时不可读取"))?;
    let mut out = HeaderMap::new();
    out.insert(header::CONTENT_TYPE, HeaderValue::from_static(serve_mime(&path)));
    out.insert(header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff"));
    let encoded = percent_encode_filename(&row.name);
    let disposition = format!("attachment; filename=\"knowledge-file\"; filename*=UTF-8''{encoded}");
    out.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    Ok((out, bytes))
}

/// 下载/预览响应的 Content-Type 白名单：只认**落盘扩展名**（ingest 白名单字面量派生，
/// 用户输入一个字都到不了这里）。svg 不许给图片 mime（可执行脚本），html 不给
/// text/html（防 XSS），文本类统一 text/plain 让预览侧展示原文而不是渲染标记。
fn serve_mime(path: &std::path::Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "txt" | "md" | "markdown" | "log" | "csv" | "json" | "html" => {
            "text/plain; charset=utf-8"
        }
        // Office/其余一律二进制下载：浏览器与 iframe 都不会尝试内嵌渲染
        _ => "application/octet-stream",
    }
}

fn percent_encode_filename(name: &str) -> String {
    let mut out = String::new();
    for b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b'.') {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub async fn delete(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = manager_viewer(&st, &headers, &q).await?;
    // 先判可见（不可见即 403，不泄露他人文档存在性），再判可写——只读授权者不许删别人的文档
    let row = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    if !acl::space_writable(&st.owned, &v, &row.space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权删除空间 {} 的文档", row.space_id)));
    }
    // 先在写语句内复核当前 DMS 身份并删除文档。上传数据源的可见性动态依赖这行文档，
    // 因此删除一旦成功，残留源也立即不可选；随后再做物理清理，避免撤权竞态先删掉别人的源。
    let n = store::delete_doc(&st.owned, &v, &row.doc_id).await.map_err(kb_err)?;
    if n == 0 {
        return Err(err(StatusCode::FORBIDDEN, "文档已不存在或写权限已失效"));
    }
    // kb.chunk 由外键 CASCADE 带走。清理失败只留下不可见孤儿，不回滚已经授权完成的删除；
    // 后续同名清理与管理员维护均可幂等回收。
    if cleanup_source(&st, &row.doc_id).await.is_err() {
        tracing::warn!(doc_id = %row.doc_id, reason = "upload_source_cleanup_failed", "文档已删除，上传数据源待清理");
    }
    remove_files(&st.kb_cfg.root, &row.doc_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 【K2】问答 body：`{question, space_id?, login_name?, role_code?}`。
/// **不加 `deny_unknown_fields`**：前端与 kb_eval 会透传 `intent`，serde 默认忽略即可。
#[derive(serde::Deserialize)]
pub struct AskKbReq {
    question: String,
    #[serde(flatten)]
    q: KbQuery,
}

/// `POST /api/kb/search` 的调试请求。身份字段沿用知识库其他端点，ACL 仍由 retrieve SQL 保证。
#[derive(serde::Deserialize)]
pub struct SearchKbReq {
    question: String,
    #[serde(flatten)]
    q: KbQuery,
}

fn nonempty_question(question: &str) -> Result<&str, ApiErr> {
    let question = question.trim();
    if question.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "问题不能为空"));
    }
    Ok(question)
}

fn preview(text: &str) -> String {
    text.chars().take(260).collect()
}

/// ACL 安全的原始检索调试面：只透出既有排序结果，不改召回、融合或阈值算法。
pub async fn search(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<SearchKbReq>,
) -> Result<ApiOk, ApiErr> {
    let question = nonempty_question(&req.question)?;
    let v = viewer(&st, &headers, &req.q).await?;
    let dms_knowledge::retrieve::SearchReport {
        normalized_query,
        hits,
        vector_degraded,
        stats,
    } = dms_knowledge::retrieve::search_report(
        &st.owned,
        &st.embed,
        &v,
        req.q.space_id.as_deref(),
        question,
        // Y3：每次请求取 cfg 快照（保存即生效）；本行是编译修复，语义以 Y3 注释为准
        &st.cfg().kb_rrf_weights,
    )
    .await
    .map_err(kb_err)?;
    let hits = hits
        .into_iter()
        .map(|h| {
            serde_json::json!({
                "chunk_id": h.chunk_id,
                "doc_id": h.doc_id,
                "doc_name": h.doc_name,
                "folder_id": h.folder_id,
                "folder_path": h.folder_path,
                "heading_path": h.heading_path,
                "page": h.page,
                "score": h.score,
                "span": h.merged,
                "tags": h.tags,
                "business_domain": h.business_domain,
                "effective_from": h.effective_from,
                "effective_to": h.effective_to,
                "source_uri": h.source_uri,
                "document_family": h.document_family,
                "document_revision": h.document_revision,
                "source_hash": h.source_hash,
                "doc_updated_at": h.doc_updated_at,
                "channels": h.channels,
                "relations": h.relations,
                "preview": preview(&h.text),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({
        "query": question,
        "normalized_query": normalized_query,
        "vector_degraded": vector_degraded,
        "stats": {
            "visible_docs": stats.visible_docs,
            "vector_candidates": stats.vector_candidates,
            "fts_candidates": stats.fts_candidates,
            "trgm_candidates": stats.trgm_candidates,
            "title_candidates": stats.title_candidates,
            "metadata_candidates": stats.metadata_candidates,
            "relation_candidates": stats.relation_candidates,
            "kg_candidates": stats.kg_candidates,
            "fused_candidates": stats.fused_candidates,
        },
        "hits": hits,
    })))
}

/// `POST /api/kb/ask` —— 直接返回 `Answer` 的 JSON（新端点，无历史兼容包袱）。
/// `space_id` 缺省＝**不限空间**（全部可见文档），不是个人空间：
/// 被授权看别人空间的人也得能检索到，ACL 由 `retrieve` 在 SQL 内把关。
///
/// 【Y2】响应自带 `trace_id`：落账在 knowledge 层统一完成（`route='knowledge'` 写
/// `meta.query_log`，`/api/ask` 分诊分支、kb_eval、MCP 同一条埋点），本端点一行代码不加。
/// 反馈走既有 `POST /api/feedback`（绑 `trace_id` + 本人，与路由无关）。
pub async fn ask(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<AskKbReq>,
) -> Result<Json<dms_kernel::Answer>, ApiErr> {
    let v = viewer(&st, &headers, &req.q).await?;
    let a = dms_knowledge::answer::answer(
        &st.owned,
        &st.embed,
        &st.llm,
        &v,
        req.q.space_id.as_deref(),
        &req.question,
        &st.cfg().kb_rrf_weights,
    )
    .await
    .map_err(kb_err)?;
    Ok(Json(a))
}

/// `GET /api/kb/chunk/{id}?window=1` 的 query。
/// **不用 `#[serde(flatten)]`**：`Query` 走 serde_urlencoded，flatten 在那边直接报 unsupported。
#[derive(serde::Deserialize, Default)]
pub struct ChunkQuery {
    window: Option<i32>,
    /// 按**合并跨度**取回（`Citation.span` 原样回传）：那才是模型真正看到的那一条命中。
    /// 给了它就走 `retrieve::span`，否则走 `retrieve::window`（上下文各看几块，人工浏览用）。
    span: Option<u32>,
    /// 回答生成时记录的文档版本。当前版本不同则返回 409，不静默展示新正文。
    source_hash: Option<String>,
    doc_updated_at: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// 引用原文回查。越权闸在 `retrieve::window` 里（过 `acl::doc_for_viewer`，非属主 403）——
/// server 不许自己拼 ACL SQL。
pub async fn chunk(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(cq): Query<ChunkQuery>,
) -> Result<ApiOk, ApiErr> {
    let q = KbQuery {
        login_name: cq.login_name,
        role_code: cq.role_code,
        space_id: None,
        folder_id: None,
        preset: None,
    };
    let v = viewer(&st, &headers, &q).await?;
    // `span` 优先：引用的价值全在可核对，而 `window` 的 ±3 覆盖不了 5 块的合并跨度
    // （实测：支撑答案的那句在第 5 块，读者点开引用看不到它）。
    let hits = match cq.span {
        Some(n) if n > 1 => dms_knowledge::retrieve::span(&st.owned, &v, id, n).await,
        _ => dms_knowledge::retrieve::window(&st.owned, &v, id, cq.window.unwrap_or(1)).await,
    }
    .map_err(kb_err)?;
    let anchor = hits
        .iter()
        .find(|h| h.chunk_id == id)
        .or_else(|| hits.first())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "块不存在"))?;
    let stale_hash = cq
        .source_hash
        .as_deref()
        .filter(|v| !v.is_empty())
        .is_some_and(|expected| expected != anchor.source_hash);
    let stale_time = cq
        .doc_updated_at
        .as_deref()
        .filter(|v| !v.is_empty())
        .is_some_and(|expected| expected != anchor.doc_updated_at);
    if stale_hash || stale_time {
        return Err(err(
            StatusCode::CONFLICT,
            "引用来源已更新或重建，请重新检索后核对最新版本",
        ));
    }
    let chunks: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            serde_json::json!({
                "chunk_id": h.chunk_id, "ord": h.ord, "text": h.text,
                "page": h.page, "heading_path": h.heading_path,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "chunk_id": id,
        "doc_id": anchor.doc_id,
        "doc_name": anchor.doc_name,
        "folder_id": anchor.folder_id,
        "folder_path": anchor.folder_path,
        "relations": anchor.relations,
        "page": anchor.page,
        "heading_path": anchor.heading_path,
        "source_hash": anchor.source_hash,
        "doc_updated_at": anchor.doc_updated_at,
        // 前端两种形状都收：整段拼好的 text，或逐块的 chunks
        "text": hits.iter().map(|h| h.text.as_str()).collect::<Vec<_>>().join("\n\n"),
        "chunks": chunks,
    })))
}

struct Form {
    q: KbQuery,
    /// (原始文件名, content-type, 字节)
    file: Option<(String, String, Vec<u8>)>,
}

/// multipart → 目标空间/目录 + 文件。身份字段即使被提交也会被忽略；认证只认 header 会话。
/// **不做任何类型/大小判定**（唯一实现在 `ingest::classify`）。
async fn read_form(mut mp: Multipart) -> Result<Form, ApiErr> {
    let bad = |e: axum::extract::multipart::MultipartError| err(StatusCode::BAD_REQUEST, e);
    let mut f = Form { q: KbQuery::default(), file: None };
    while let Some(field) = mp.next_field().await.map_err(bad)? {
        let key = field.name().unwrap_or_default().to_string();
        match key.as_str() {
            "file" => {
                let name = field.file_name().unwrap_or_default().to_string();
                let mime = field.content_type().unwrap_or("application/octet-stream").to_string();
                let bytes = field.bytes().await.map_err(bad)?;
                f.file = Some((name, mime, bytes.to_vec()));
            }
            "space_id" => f.q.space_id = text(field).await,
            "folder_id" => f.q.folder_id = text(field).await,
            _ => {}
        }
    }
    Ok(f)
}

/// 空串按缺省处理（表单里没填的字段常以空串到达）
async fn text(field: axum::extract::multipart::Field<'_>) -> Option<String> {
    field.text().await.ok().filter(|s| !s.is_empty())
}

/// data URL 的 MIME 只从图片白名单中取，绝不把 multipart 的任意字符串拼进协议头。
fn image_data_mime(file_name: &str, mime: &str) -> &'static str {
    match file_name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()) {
        Some(ext) if matches!(ext.as_str(), "jpg" | "jpeg") => "image/jpeg",
        Some(ext) if ext == "bmp" => "image/bmp",
        Some(ext) if ext == "gif" => "image/gif",
        Some(ext) if matches!(ext.as_str(), "tif" | "tiff") => "image/tiff",
        Some(ext) if ext == "webp" => "image/webp",
        Some(ext) if ext == "png" => "image/png",
        _ => match mime.trim().to_ascii_lowercase().as_str() {
            "image/jpeg" | "image/jpg" => "image/jpeg",
            "image/bmp" => "image/bmp",
            "image/gif" => "image/gif",
            "image/tiff" => "image/tiff",
            "image/webp" => "image/webp",
            _ => "image/png",
        },
    }
}

/// Base64 会把原图膨胀约 4/3；在分配编码字符串前预估完整 data URL，超出
/// `vision_chat` 的 16MB 请求上限就返回 false，由 knowledge 链继续本地 OCR。
fn image_fits_vision_data_url(mime: &str, bytes_len: usize) -> bool {
    let Some(encoded_len) = bytes_len.checked_add(2).map(|n| n / 3).and_then(|n| n.checked_mul(4))
    else {
        return false;
    };
    let Some(total) = "data:".len()
        .checked_add(mime.len())
        .and_then(|n| n.checked_add(";base64,".len()))
        .and_then(|n| n.checked_add(encoded_len))
    else {
        return false;
    };
    total <= crate::llm::MAX_VISION_IMAGE_URL_BYTES
}

/// server 已经间接携带 base64，但没有直接依赖；这 20 行避免为一个编码点扩大依赖面。
fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((bytes.len() + 2) / 3) * 4);
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// `DocRow` → JSON（`DocRow` 不实现 Serialize：knowledge 不该为了 HTTP 形状而依赖协议层）
fn doc_json(d: &DocRow) -> serde_json::Value {
    let (quality_level, quality_label) = doc_quality(
        &d.status,
        d.enabled,
        d.chunk_count,
        &d.notice,
        d.effective_from,
        d.effective_to,
        chrono::Local::now().date_naive(),
    );
    serde_json::json!({
        "doc_id": d.doc_id,
        "space_id": d.space_id,
        "folder_id": d.folder_id,
        "folder_path": d.folder_path,
        "name": d.name,
        "mime": d.mime,
        "bytes": d.bytes,
        "sha256": d.sha256,
        "status": d.status,
        "enabled": d.enabled,
        "tags": d.tags,
        "business_domain": d.business_domain,
        "effective_from": d.effective_from.map(|v| v.to_string()),
        "effective_to": d.effective_to.map(|v| v.to_string()),
        "source_uri": d.source_uri,
        "document_family": d.document_family,
        "document_revision": d.document_revision,
        "quality": { "level": quality_level, "label": quality_label },
        "error": d.error,
        "notice": d.notice,
        "description": d.description,
        "page_count": d.page_count,
        "chunk_count": d.chunk_count,
        "uploaded_by": d.uploaded_by,
        "created_at": d.created_at,
        "updated_at": d.updated_at,
    })
}

fn folder_json(f: &store::FolderRow) -> serde_json::Value {
    serde_json::json!({
        "folder_id": f.folder_id,
        "space_id": f.space_id,
        "parent_id": f.parent_id,
        "name": f.name,
        "path": f.path,
        "depth": f.depth,
        "child_count": f.child_count,
        "doc_count": f.doc_count,
        "created_at": f.created_at,
        "updated_at": f.updated_at,
    })
}

fn relation_json(r: &store::DocRelationRow) -> serde_json::Value {
    serde_json::json!({
        "doc_id": r.doc_id,
        "doc_name": r.doc_name,
        "folder_id": r.folder_id,
        "folder_path": r.folder_path,
        "document_family": r.document_family,
        "document_revision": r.document_revision,
        "relation": r.relation,
    })
}

fn doc_quality(
    status: &str,
    enabled: bool,
    chunk_count: i32,
    notice: &str,
    effective_from: Option<chrono::NaiveDate>,
    effective_to: Option<chrono::NaiveDate>,
    today: chrono::NaiveDate,
) -> (&'static str, &'static str) {
    if matches!(status, "pending" | "parsing") {
        return ("processing", "处理中");
    }
    if status == "failed" {
        return ("danger", "处理失败");
    }
    if !enabled {
        return ("warning", "已停用");
    }
    if effective_from.is_some_and(|date| date > today) {
        return ("warning", "待生效");
    }
    if effective_to.is_some_and(|date| date < today) {
        return ("warning", "已失效");
    }
    if matches!(status, "chunked" | "embedded") && chunk_count == 0 {
        return ("danger", "无可检索内容");
    }
    if status == "chunked" {
        return ("warning", "待向量化");
    }
    if !notice.trim().is_empty() {
        return ("warning", "有处理提示");
    }
    if status == "embedded" && chunk_count > 0 {
        return ("good", "可检索");
    }
    ("warning", "状态待确认")
}

fn valid_space_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

async fn stored_file(root: &std::path::Path, doc_id: &str) -> Option<std::path::PathBuf> {
    let mut rd = tokio::fs::read_dir(root).await.ok()?;
    while let Ok(Some(e)) = rd.next_entry().await {
        let p = e.path();
        if p.file_stem().is_some_and(|s| s == doc_id) {
            return Some(p);
        }
    }
    None
}

/// 删磁盘文件：扫目录按 `file_stem == doc_id` 匹配。
///
/// **刻意不拼 `<doc_id>.<ext>`**——扩展名要从 `d.name`（用户可控）推，一条
/// `a.pdf/../../x` 的原名就能拼出目录穿越；而 `ingest` 的扩展名映射表是私有的，
/// 在 server 复述一份又是第二份真相源。read_dir 只 join 系统给的条目名，穿越面为零。
///
/// `ponytail:` O(目录条数) 扫描，单空间上万文档时才需要索引/或让 knowledge 的
/// `delete_doc` 一并返回路径。
async fn remove_files(root: &std::path::Path, doc_id: &str) {
    let Ok(mut rd) = tokio::fs::read_dir(root).await else { return };
    while let Ok(Some(e)) = rd.next_entry().await {
        let p = e.path();
        if p.file_stem().is_some_and(|s| s == doc_id) {
            let _ = tokio::fs::remove_file(&p).await;
        }
    }
}

async fn cleanup_source(st: &AppState, doc_id: &str) -> Result<(), ApiErr> {
    tabular::drop_source(&st.owned, doc_id).await.map_err(kb_err)?;
    let up_ds = tabular::upload_ds_id(doc_id);
    dms_semantic::registry::datasource::delete_datasource(st.owned.pool(), &up_ds)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库数据源清理失败"))?;
    dms_semantic::ingest::schema_sync::drop_schema_docs(st.owned.pool(), &up_ds)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库结构清理失败"))?;
    Ok(())
}

async fn sync_source_state(
    st: &AppState,
    doc_id: &str,
    enabled: bool,
) -> Result<(), ApiErr> {
    let ds_id = tabular::upload_ds_id(doc_id);
    dms_semantic::registry::datasource::set_upload_datasource_active(
        st.owned.pool(),
        &ds_id,
        enabled,
    )
    .await
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库数据源状态同步失败"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(login: &str, role: &str, admin: bool) -> crate::dms_policy_core::Principal {
        crate::dms_policy_core::Principal {
            employee_id: 1,
            login_name: login.into(),
            actual_name: login.into(),
            administrator_flag: admin,
            department_id: None,
            role_id: 0,
            role_code: role.into(),
        }
    }

    /// 【KB 管理闸】全分支：管理员恒真 / None 与空名单仅管理员 / login 命中 / role 命中 /
    /// 都不命中为假 / 相似串不算（精确比对，无大小写折叠与通配）。
    #[test]
    fn kb_manager_allowed_pins_every_branch() {
        use crate::db::KbManagerGrants;
        let grants = KbManagerGrants {
            roles: vec!["kb_admin".into()],
            logins: vec!["zhangsan".into()],
        };
        // 管理员恒真：有没有配置、在不在名单都真
        assert!(kb_manager_allowed(&principal("lisi", "sales", true), None));
        assert!(kb_manager_allowed(&principal("lisi", "sales", true), Some(&grants)));
        // 缺省 None 与空名单 = 仅管理员（即使在名单语义上也轮不到非管理员）
        assert!(!kb_manager_allowed(&principal("zhangsan", "kb_admin", false), None));
        assert!(!kb_manager_allowed(
            &principal("zhangsan", "kb_admin", false),
            Some(&KbManagerGrants::default())
        ));
        // login 命中 / role 命中（或关系，各命中一边即真）
        assert!(kb_manager_allowed(&principal("zhangsan", "sales", false), Some(&grants)));
        assert!(kb_manager_allowed(&principal("lisi", "kb_admin", false), Some(&grants)));
        // 都不命中 = 假；相似但不等的串不算
        assert!(!kb_manager_allowed(&principal("lisi", "sales", false), Some(&grants)));
        assert!(!kb_manager_allowed(&principal("Zhangsan", "sales", false), Some(&grants)));
    }

    /// 闸的覆盖清单（源码断言，端点增删时这里必须跟着想一遍）：
    /// 管理面写端点（上传/删除/移动/目录/授权/重处理/描述/ingest-url）与空间管理读端点
    /// 必须过 manager 闸；检索面（ask/search/chunk/download）**不许**过——
    /// 普通用户问 KB 问题走检索面，误伤就是把「问答」也收进管理授权。
    #[test]
    fn manager_gate_covers_management_surface_only() {
        let src = include_str!("kb_api.rs");
        let body_of = |name: &str| -> String {
            let needle = format!("pub async fn {name}(");
            let body = src.split(&needle).nth(1).unwrap_or_else(|| panic!("端点 {name} 不存在"));
            body.split("\n}\n").next().unwrap().to_string()
        };
        for name in [
            "upload", "create_space", "reprocess", "set_doc_state", "update_doc_metadata",
            "ingest_url", "export_space", "generate_description", "space_grants", "grant_space",
            "revoke_space", "docs", "folders", "create_folder", "update_folder", "delete_folder",
            "move_doc", "doc", "delete", "spaces",
        ] {
            let body = body_of(name);
            assert!(
                body.contains("manager_principal") || body.contains("manager_viewer") || body.contains("session_viewer"),
                "管理面端点 {name} 没过 kb_manager 闸"
            );
        }
        for name in ["ask", "search", "chunk", "download_doc"] {
            let body = body_of(name);
            assert!(
                !body.contains("manager_principal") && !body.contains("manager_viewer") && !body.contains("session_viewer"),
                "检索面端点 {name} 不许过管理闸"
            );
        }
    }

    /// KB 落账只在 knowledge 层（`/api/kb/ask`、`/api/ask` 分诊分支、kb_eval、MCP
    /// 四个调用点一处埋点）：server 端点再写一份就是双写（Y2）
    #[test]
    fn kb_ask_logging_lives_in_knowledge_layer_only() {
        let src = include_str!("kb_api.rs");
        let body = src.split("pub async fn ask(").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let needle = concat!("meta.", "query_log");
        assert!(!body.contains(needle), "端点里再写一份就是双写: {body}");
        let ans = include_str!("../../knowledge/src/answer.rs");
        assert!(ans.contains("qa_log::finish"), "knowledge 层的落账点没了");
    }

    #[test]
    fn dms_role_options_query_is_single_table_explicit_and_bounded() {
        let sql = DMS_ROLE_OPTIONS_SQL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_uppercase();
        assert!(sql.starts_with("SELECT TRIM(ROLE_CODE), TRIM(ROLE_NAME) FROM T_ROLE "));
        assert_eq!(sql.matches(" FROM ").count(), 1);
        assert!(!sql.contains(" JOIN "));
        assert!(!sql.contains("SELECT *"));
        assert!(sql.ends_with("LIMIT 500"));
    }

    #[test]
    fn image_data_url_helpers_are_stable_and_whitelisted() {
        assert_eq!(encode_base64(b"Man"), "TWFu");
        assert_eq!(encode_base64(b"Ma"), "TWE=");
        assert_eq!(encode_base64(b"M"), "TQ==");
        assert_eq!(image_data_mime("scan.jpg", "application/octet-stream"), "image/jpeg");
        assert_eq!(image_data_mime("scan.png", "text/html"), "image/png");
        assert_eq!(image_data_mime("scan.jpg", "image/png"), "image/jpeg");
        assert_eq!(image_data_mime("动图.gif", "application/octet-stream"), "image/gif");
        assert_eq!(image_data_mime("动图.GIF", "image/gif"), "image/gif");
        assert!(image_fits_vision_data_url("image/png", 1));
        assert!(image_fits_vision_data_url("image/png", 12 * 1024 * 1024 - 64));
        assert!(!image_fits_vision_data_url("image/png", 16 * 1024 * 1024));
    }

    fn code(e: KbError) -> u16 {
        kb_err(e).0.as_u16()
    }

    /// 错误映射是外部契约：400/403/404 分开，其余一律 500（别把 DB 错报成 400）
    #[test]
    fn kb_error_status_mapping() {
        assert_eq!(code(KbError::BadInput("x".into())), 400);
        assert_eq!(code(KbError::Forbidden("x".into())), 403);
        assert_eq!(code(KbError::NotFound("x".into())), 404);
        assert_eq!(code(KbError::Upstream("x".into())), 500);
        assert_eq!(code(KbError::Db("x".into())), 500);
        // 文案原样透传 knowledge 的 Display，server 不改写
        let (_, body) = kb_err(KbError::Forbidden("文档 d1 不可见".into()));
        assert_eq!(body.0["error"], "无权访问：文档 d1 不可见");
    }

    #[test]
    fn default_space_is_login() {
        let v = Viewer::new("zhangsan", vec![]);
        assert_eq!(space_of(&v, &KbQuery::default()), "zhangsan");
        let q = KbQuery { space_id: Some("kb-hr".into()), ..Default::default() };
        assert_eq!(space_of(&v, &q), "kb-hr");
    }

    #[test]
    fn enterprise_space_id_is_safe() {
        for ok in ["enterprise-hr", "kb_2026", "A1"] {
            assert!(valid_space_id(ok), "{ok}");
        }
        for bad in ["", "../hr", "人事", "a b"] {
            assert!(!valid_space_id(bad), "{bad}");
        }
    }

    #[test]
    fn space_grant_defaults_to_read() {
        let req = SpaceGrantReq {
            grantee_kind: "role".into(), grantee: "sales".into(), ..Default::default()
        };
        let e = space_acl_entry("enterprise-sales", &req).unwrap();
        assert_eq!(e.scope, acl::AclScope::Space);
        assert_eq!(e.perm, acl::Perm::Read);
    }

    #[test]
    fn batch_role_grants_validate_every_role_before_writing() {
        let catalog = HashMap::from([("sales", "销售"), ("finance", "财务")]);
        let roles = validated_role_codes(
            &[" sales ".into(), "finance".into(), "sales".into()],
            &catalog,
        )
        .unwrap();
        assert_eq!(roles, vec!["sales", "finance"]);
        assert!(validated_role_codes(&["missing".into()], &catalog).is_err());

        let src = include_str!("kb_api.rs");
        let body = src.split("async fn grant_roles").nth(1).unwrap();
        let body = body.split("fn validated_role_codes").next().unwrap();
        assert!(body.find("validated_role_codes").unwrap() < body.find("grant_space_roles").unwrap());
    }

    #[test]
    fn image_data_url_cap_accounts_for_base64_expansion() {
        assert!(image_fits_vision_data_url("image/png", 1));
        assert!(!image_fits_vision_data_url("image/png", crate::llm::MAX_VISION_IMAGE_URL_BYTES));
    }

    /// 问答 body 的 flatten 一旦失效，身份就退化成「未认证」→ 全部 401。
    /// 顺带钉住「未知字段（前端的 intent）必须被忽略」。
    #[test]
    fn ask_body_reads_identity_and_ignores_intent() {
        let r: AskKbReq = serde_json::from_str(
            r#"{"question":"报销上限","login_name":"zhangsan","intent":"knowledge"}"#,
        )
        .unwrap();
        assert_eq!(r.question, "报销上限");
        assert_eq!(r.q.login_name.as_deref(), Some("zhangsan"));
        assert!(r.q.space_id.is_none(), "缺省不限空间");
    }

    #[test]
    fn search_helpers_reject_blank_question_and_truncate_unicode_safely() {
        let e = nonempty_question(" \t\n ").unwrap_err();
        assert_eq!(e.0, StatusCode::BAD_REQUEST);
        assert_eq!(nonempty_question("  报销制度  ").unwrap(), "报销制度");

        let source = format!("{}尾", "知".repeat(260));
        let out = preview(&source);
        assert_eq!(out.chars().count(), 260);
        assert!(out.chars().all(|c| c == '知'));
    }

    #[test]
    fn download_filename_is_rfc5987_safe() {
        assert_eq!(percent_encode_filename("制度 2026.pdf"), "%E5%88%B6%E5%BA%A6%202026.pdf");
        assert_eq!(percent_encode_filename("a\";x.txt"), "a%22%3Bx.txt");
        assert!(!percent_encode_filename("../secret.pdf").contains('/'));
    }

    /// 下载 mime 只认落盘扩展名白名单：图片/pdf/文本可内嵌，html 转纯文本，
    /// svg 与 Office 一律 octet-stream（自报 mime 一个字都到不了这里）
    #[test]
    fn serve_mime_is_extension_whitelist() {
        let p = |ext: &str| std::path::PathBuf::from(format!("data/kb/doc1.{ext}"));
        assert_eq!(serve_mime(&p("pdf")), "application/pdf");
        assert_eq!(serve_mime(&p("png")), "image/png");
        assert_eq!(serve_mime(&p("jpg")), "image/jpeg");
        assert_eq!(serve_mime(&p("jpeg")), "image/jpeg");
        assert_eq!(serve_mime(&p("gif")), "image/gif");
        assert_eq!(serve_mime(&p("webp")), "image/webp");
        assert_eq!(serve_mime(&p("bmp")), "image/bmp");
        assert_eq!(serve_mime(&p("tif")), "image/tiff");
        for text in ["txt", "md", "markdown", "log", "csv", "json", "html"] {
            assert_eq!(serve_mime(&p(text)), "text/plain; charset=utf-8", "{text}");
        }
        // 🔴 XSS 闸：svg 不给 image mime，html 不给 text/html，Office 不可内嵌渲染
        assert_eq!(serve_mime(&p("svg")), "application/octet-stream");
        for bin in ["docx", "doc", "pptx", "ppt", "xlsx", "xls", "xlsm", "exe", ""] {
            assert_eq!(serve_mime(&p(bin)), "application/octet-stream", "{bin}");
        }
    }

    #[test]
    fn metadata_validation_normalizes_and_rejects_bad_values() {
        let req = DocMetadataReq {
            tags: vec![" 制度 ".into(), "制度".into(), "财务".into(), " ".into()],
            business_domain: Some(" 财务管理 ".into()),
            effective_from: Some("2026-08-01".into()),
            effective_to: Some("2026-08-31".into()),
            source_uri: Some(" https://example.test/policy ".into()),
            document_family: Some(" 培训报销制度 ".into()),
            document_revision: Some(" v2.1 ".into()),
            ..Default::default()
        };
        let m = validate_doc_metadata(&req).unwrap();
        assert_eq!(m.tags, vec!["制度", "财务"]);
        assert_eq!(m.business_domain.as_deref(), Some("财务管理"));
        assert_eq!(m.source_uri.as_deref(), Some("https://example.test/policy"));
        assert_eq!(m.document_family.as_deref(), Some("培训报销制度"));
        assert_eq!(m.document_revision.as_deref(), Some("v2.1"));

        let unsafe_uri = DocMetadataReq { source_uri: Some("javascript:alert(1)".into()), ..Default::default() };
        assert!(validate_doc_metadata(&unsafe_uri).is_err());

        let bad_range = DocMetadataReq {
            effective_from: Some("2026-09-01".into()),
            effective_to: Some("2026-08-31".into()),
            ..Default::default()
        };
        assert!(validate_doc_metadata(&bad_range).is_err());
        let bad_format = DocMetadataReq { effective_from: Some("2026-8-1".into()), ..Default::default() };
        assert!(validate_doc_metadata(&bad_format).is_err());
        let too_many = DocMetadataReq {
            tags: (0..21).map(|i| format!("标签{i}")).collect(),
            ..Default::default()
        };
        assert!(validate_doc_metadata(&too_many).is_err());
    }

    #[test]
    fn document_quality_covers_processing_danger_warning_and_good() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 5).unwrap();
        let q = |status, enabled, chunks, notice, from, to| {
            doc_quality(status, enabled, chunks, notice, from, to, today)
        };
        assert_eq!(q("parsing", true, 0, "", None, None), ("processing", "处理中"));
        assert_eq!(q("failed", true, 0, "", None, None), ("danger", "处理失败"));
        assert_eq!(q("embedded", false, 3, "", None, None), ("warning", "已停用"));
        assert_eq!(
            q("embedded", true, 3, "", Some(today.succ_opt().unwrap()), None),
            ("warning", "待生效")
        );
        assert_eq!(
            q("embedded", true, 3, "", None, Some(today.pred_opt().unwrap())),
            ("warning", "已失效")
        );
        assert_eq!(q("embedded", true, 0, "", None, None), ("danger", "无可检索内容"));
        assert_eq!(q("chunked", true, 3, "", None, None), ("warning", "待向量化"));
        assert_eq!(q("embedded", true, 3, "有跳过页", None, None), ("warning", "有处理提示"));
        assert_eq!(q("embedded", true, 3, "", None, None), ("good", "可检索"));
    }

    /// 【K4】数据源描述是向量选源的唯一素材：sheet 名、表名、行数、被跳过的 sheet 都得在里面
    #[test]
    fn upload_source_description_names_sheets_and_skips() {
        let src = tabular::TabularSource {
            ds_id: "upload_d1".into(),
            schema: "up_d1".into(),
            tables: vec![tabular::TabularTable {
                sheet: "一月".into(),
                table: "t0___".into(),
                rows: 3,
            }],
            skipped: vec!["空表".into()],
        };
        let d = ds_description("销售台账.xlsx", &src);
        assert!(d.contains("销售台账.xlsx") && d.contains("up_d1"), "{d}");
        assert!(d.contains("一月（表 t0___，3 行）"), "{d}");
        assert!(d.contains("空表"), "被跳过的 sheet 不许静默：{d}");
        // 响应里必须带出 ds_id，否则前端不知道该问哪个源
        assert_eq!(source_json(&src)["ds_id"], "upload_d1");
    }

    /// 上传闸：4 个许可用完即 429（拿不到许可这一支不许悄悄排队）
    #[test]
    fn upload_gate_has_four_permits() {
        let held: Vec<_> = (0..4).map(|_| UPLOAD_GATE.try_acquire().unwrap()).collect();
        assert!(UPLOAD_GATE.try_acquire().is_err());
        drop(held);
        assert!(UPLOAD_GATE.try_acquire().is_ok());
    }

    #[test]
    fn upload_auth_precedes_permit_and_multipart_read() {
        let src = include_str!("kb_api.rs");
        let body = src
            .split("pub async fn upload")
            .nth(1)
            .unwrap()
            .split("pub async fn spaces")
            .next()
            .unwrap();
        let auth = body.find("session_viewer").unwrap();
        let permit = body.find("UPLOAD_GATE.try_acquire").unwrap();
        let read = body.find("read_form(mp)").unwrap();
        assert!(auth < permit && permit < read, "上传必须先认证，再占许可，最后读取 multipart: {body}");
        assert!(body.contains("tokio::time::timeout(UPLOAD_READ_TIMEOUT"), "上传读取必须有超时: {body}");
        let form = src
            .split("async fn read_form")
            .nth(1)
            .unwrap()
            .split("fn image_data_mime")
            .next()
            .unwrap();
        assert!(!form.contains("\"login_name\"") && !form.contains("\"role_code\""));
    }

    /// 磁盘删除只按 doc_id 的 stem 匹配：同目录别人的文件与穿越路径都不许被删
    #[tokio::test]
    async fn remove_files_only_touches_that_doc() {
        let root = std::env::temp_dir().join(format!("kb_rm_{}", std::process::id()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let id = "11111111-2222-3333-4444-555555555555";
        for f in [&format!("{id}.pdf"), &format!("{id}.txt"), "other.pdf", &format!("{id}x.pdf")] {
            tokio::fs::write(root.join(f), b"x").await.unwrap();
        }
        remove_files(&root, id).await;
        assert!(!root.join(format!("{id}.pdf")).exists());
        assert!(!root.join(format!("{id}.txt")).exists());
        assert!(root.join("other.pdf").exists());
        assert!(root.join(format!("{id}x.pdf")).exists());
        let _ = tokio::fs::remove_dir_all(&root).await;
    }
}
