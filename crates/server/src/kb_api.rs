//! 【K1/K2】知识库 HTTP 面：上传 / 列表 / 详情 / 删除 / 问答 / 引用原文回查。变更原因＝知识库协议。
//!
//! **零业务判定**：类型白名单、大小上限、sha256 去重、状态机全在 `dms_knowledge::ingest`——
//! 同一信任边界上两份实现必然漂，漂出来宽松的那份就是入口。本文件只做协议转换与身份换算。
//!
//! T10 把 server 拆成 `api/` 目录时本文件整体平移成 `api/kb.rs`（函数形状已按目标组织）。

use crate::AppState;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, Sse};
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
/// 许可数只许从这里改：429 文案（`upload_gate_full`）与单测都读这个常量。
const UPLOAD_PERMITS: usize = 4;
static UPLOAD_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(UPLOAD_PERMITS);
const UPLOAD_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// 下载并发闸（`download_doc`）：整文件入内存 × N 并发，与上传闸防的是同一个问题。
const DOWNLOAD_PERMITS: usize = 8;
static DOWNLOAD_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(DOWNLOAD_PERMITS);

const IMAGE_OCR_PROMPT: &str = "请逐字识别图片中的全部可见文字，并完整还原表格结构、金额、数字和日期。保持原文顺序和字段，不总结、不改写、不补全、不猜测；无法辨认处标记[无法辨认]。仅输出识别结果。";

/// 持有 owned `LlmClient`（`Clone` 共享运行时配置）：入库重活在后台任务里执行，
/// OCR 实现必须能 move 进 `tokio::spawn`，借 `&st.llm` 的引用形态过不去任务边界。
struct RuntimeImageOcr {
    llm: crate::llm::LlmClient,
}

impl ingest::ImageOcr for RuntimeImageOcr {
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

/// 500 收敛文案：`kb_err` 的 Db 臂与各端点的直写 500 共用同一处，改文案全族生效。
const MSG_KB_UNAVAILABLE: &str = "知识库服务暂时不可用，请稍后重试";
/// 落盘文件读失败的 500 文案（reprocess/download_doc 两处共用）。
const MSG_FILE_UNREADABLE: &str = "文档文件暂时不可读取";

/// 上传/URL 入库并发闸占满的统一 429：许可数与文案同源（`UPLOAD_PERMITS`），改数不漂。
fn upload_gate_full() -> ApiErr {
    err(
        StatusCode::TOO_MANY_REQUESTS,
        format!("上传并发已满（同时最多 {UPLOAD_PERMITS} 个），请稍后重试"),
    )
}

/// 401 统一文案（`viewer`/`manager_principal` 两处同一条认证收口）。
fn unauthenticated() -> ApiErr {
    err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name")
}

/// 空间写权限 403（多个端点同一文案，改一处全族生效）。
fn no_space_write(space_id: &str) -> ApiErr {
    err(StatusCode::FORBIDDEN, format!("无权修改空间 {space_id} 的文档"))
}

/// 空间读权限 403（多个端点同一文案）。
fn no_space_read(space_id: &str) -> ApiErr {
    err(StatusCode::FORBIDDEN, format!("无权访问知识空间 {space_id}"))
}

/// Principal 现查的统一 403 映射（`load_viewer`/`manager_principal` 共用；底层错误不透出）。
/// 【share_config v2 · 部门支路】拿到 Principal 后顺手把 login→部门 映射刷进 PG
/// （`kb.user_dept` 是 dept 授权可见性 SQL 的求值底座，见 knowledge/acl.rs 头注）：
/// 先同步再放行，本请求随后的全部 ACL 判定用的就是这一刻现算的部门归属。
/// 同步失败只留痕不拒请求——映射滞留最坏是 dept 支路按旧值求值，
/// login/role 两路不该为一张映射表的抖动陪葬。
async fn principal_or_403(
    st: &AppState,
    login: &str,
    role: Option<&str>,
) -> Result<crate::dms_policy_core::Principal, ApiErr> {
    let p = crate::auth::load_principal(&st.auth_mysql, login, role)
        .await
        .map_err(|_| err(StatusCode::FORBIDDEN, "当前 DMS 身份或角色不可用"))?;
    let dept = p.department_id.map(|d| d.to_string());
    if let Err(e) = acl::sync_viewer_dept(&st.owned, &p.login_name, dept.as_deref()).await {
        tracing::warn!(login = %p.login_name, reason = %e, "KB 部门映射刷新失败（dept 授权按既有映射求值）");
    }
    Ok(p)
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
    err(code, kb_err_msg(&e))
}

/// `kb_err` 的文案半段（状态码映射的同款判定）：流式端点的 error 帧只有文案没有状态码，
/// 两处共用一份，不许各写各的收敛规则。
fn kb_err_msg(e: &KbError) -> String {
    match e {
        KbError::BadInput(_) | KbError::Forbidden(_) | KbError::NotFound(_) => e.to_string(),
        KbError::Upstream(_) => "文档处理服务暂时不可用，请稍后重试".to_string(),
        KbError::Db(_) => MSG_KB_UNAVAILABLE.to_string(),
    }
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
    // default：缺 name 字段落到下方 400「名称不能为空」的友好文案，而非 axum 反序列化 422
    #[serde(default)]
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
    // default：与 related_doc_ids 同口径——缺 tags 字段不该走 422
    #[serde(default)]
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
        .ok_or_else(unauthenticated)?;
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
    let p = principal_or_403(st, &login, role.as_deref()).await?;
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
        .ok_or_else(unauthenticated)?;
    let p = principal_or_403(st, &login, role.as_deref()).await?;
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
    // 只比前缀：不为判定把最长 500 字符的 URI 整体小写化分配一次
    uri.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"))
        || uri.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://"))
}

pub async fn upload(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    mp: Multipart,
) -> Result<ApiOk, ApiErr> {
    // 先认证再占上传槽：未认证慢请求不能耗尽许可。
    let v = session_viewer(&st, &headers).await?;
    // 许可跟着重活走（move 进 spawn 的任务里持有到结束）：字节与解析产物都活在后台任务里，
    // 许可若随请求返回就释放，「4 并发」闸门形同虚设。
    let permit = UPLOAD_GATE.try_acquire().map_err(|_| upload_gate_full())?;
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
    // 快路径（校验/去重/建行/落盘）在请求内完成；重活（parse→chunk→embed）后台跑——
    // 大文件解析 50s+，同步 await 会被浏览器/nginx 超时断连把 handler future 直接 drop，
    // 入库半路取消 → 文档永卡 parsing（生产已复现）。响应立即返回进行态 doc 行，前端轮询。
    let prepared = ingest::prepare(&st.owned, &v, &st.kb_cfg, req).await.map_err(kb_err)?;
    if let Some(job) = prepared.job {
        spawn_ingest_job(st.clone(), v.clone(), prepared.doc_id.clone(), name.clone(), job, permit);
    }
    // 去重命中既有文档时 prepare 会直接复用 doc_id；这里统一再绑定一次目标目录，
    // 同时让写权限在返回结果前于同一条写语句中复核，撤权后不返回裸文档元数据。
    // （绑定失败不再清理数据源：通道②产物在后台任务里才诞生，登记前的失权复核由
    // `register_source` 自带的 `still_writable` 兜底。）
    store::move_doc(
        &st.owned,
        &v,
        &prepared.doc_id,
        &space_id,
        form.q.folder_id.as_deref(),
    )
    .await
    .map_err(kb_err)?;
    let row = acl::doc_for_viewer(&st.owned, &v, &prepared.doc_id).await.map_err(kb_err)?;
    Ok(Json(upload_doc_json(&row, prepared.replaced.as_deref())))
}

/// 后台跑入库重活：许可 move 进任务、持有到重活结束（闸门防的内存就活在这段期间）。
fn spawn_ingest_job(
    st: Arc<AppState>,
    v: Viewer,
    doc_id: String,
    doc_name: String,
    job: ingest::IngestJob,
    permit: tokio::sync::SemaphorePermit<'static>,
) {
    tokio::spawn(async move {
        run_ingest_job_logged(&st, &v, &doc_id, &doc_name, job, permit).await;
    });
}

/// 重活执行 + 结果收尾：在线上传的后台任务与启动自愈共用这一条。
/// - 通道②数据源登记依赖重活产物（`source`），只能在这里完成——请求早已返回；
/// - Fresh 的失败文案 `run_job` 已落库（文档列表可见），Rebuild/Overwrite 失败保留线上版本，
///   三条链在这里都只欠一条日志；
/// - Overwrite 的通道②收尾看新内容：有表 → 重新登记/同步结构（登记是 upsert，ds_id 由
///   doc_id 派生不变）；无表 → 旧版本登记过的数据源必须随之退役（幂等三步清理）。
async fn run_ingest_job_logged(
    st: &AppState,
    v: &Viewer,
    doc_id: &str,
    doc_name: &str,
    job: ingest::IngestJob,
    _permit: tokio::sync::SemaphorePermit<'_>,
) {
    let ocr = RuntimeImageOcr { llm: st.llm.clone() };
    let is_overwrite = matches!(&job, ingest::IngestJob::Overwrite { .. });
    match ingest::run_job(&st.owned, &st.doc, &st.embed, v, &st.kb_cfg, doc_id, job, Some(&ocr))
        .await
    {
        Ok(source) => {
            if let Some(src) = source {
                register_source(st, v, doc_name, doc_id, &src).await;
            } else if is_overwrite {
                // 覆盖后的新内容没有表格：旧版本若登记过数据源必须退役（否则问数拿旧版本的
                // 数据答新版本的文档）。三步幂等清理，失败只留日志（重传同名文件可收敛）。
                if let Err(e) = cleanup_source(st, doc_id).await {
                    tracing::warn!(doc_id, error = %e.1.0, "同名覆盖后旧数据源退役清理失败");
                }
            }
        }
        Err(e) => {
            tracing::warn!(doc_id, error = %e, "入库重活失败（Fresh 已落失败文案；Rebuild/Overwrite 保留线上版本）");
        }
    }
}

// ════════════════════════ 启动自愈（recover_pending）════════════════════════

/// 卡死判定阈值：进行态文档 10 分钟没动过 ＝ 跑它的那个任务已经随进程死了
///（正常解析再久，状态推进也会刷 `updated_at`）。
const RECOVER_STALE_MINUTES: i32 = 10;
/// 单次启动自愈的份数上限：重启撞上大批量卡死时，恢复本身不该拖垮启动后的第一波请求
const RECOVER_BATCH_LIMIT: i64 = 50;

/// 【启动自愈】重跑上次进程死亡留下的「进行中」文档（Yuxi recover_pending 同款）。
/// 同步 ingest 时代浏览器/nginx 超时断连会把 handler future 直接 drop、文档永卡 parsing——
/// 重活后台化之后这个坑没了，但进程重启（部署/崩溃）仍会留下僵尸，启动时扫一遍补上。
/// 挂后台不阻塞启动；逐份串行（每份都走上传闸取许可，不与在线上传抢内存）。
pub fn spawn_recover_pending(st: Arc<AppState>) {
    tokio::spawn(async move {
        match recover_pending(&st).await {
            Ok(0) => {}
            Ok(n) => tracing::info!("知识库启动自愈：{n} 份卡死文档已重跑入库"),
            Err(e) => tracing::warn!("知识库启动自愈失败（剩余文档下次启动再试）: {e}"),
        }
    });
}

async fn recover_pending(st: &AppState) -> Result<usize, KbError> {
    let stuck = store::stuck_docs(&st.owned, RECOVER_STALE_MINUTES, RECOVER_BATCH_LIMIT).await?;
    let mut done = 0usize;
    for d in stuck {
        if recover_one(st, d).await {
            done += 1;
        }
    }
    Ok(done)
}

/// 单份卡死文档的重跑。分派键是分块存在性：有块走影子重建（首入链的 `insert_chunks`
/// 全量冲突会报 0 行，那条链不许重入）；无块走首入链（表格通道②才能补上物理表）。
/// 返回「是否处理到终态」——读文件失败这类可重试错误不算（下次启动再来）。
async fn recover_one(st: &AppState, d: store::StuckDoc) -> bool {
    // 系统任务没有会话身份：以空间 owner 执行（owner 恒过写门禁，不是 ACL 旁路）
    let v = Viewer::new(d.owner.clone(), Vec::new());
    let Some(path) = stored_file(&st.kb_cfg.root, &d.doc_id).await else {
        tracing::warn!(doc_id = %d.doc_id, name = %d.name, "知识库启动自愈：原始文件已不存在，标记失败");
        let _ = store::set_status(&st.owned, &v, &d.doc_id, store::DocStatus::Failed, "原始文件已不存在，请重新上传").await;
        return true;
    };
    let Ok(bytes) = tokio::fs::read(&path).await else {
        tracing::warn!(doc_id = %d.doc_id, name = %d.name, "知识库启动自愈：原始文件读取失败，下次启动重试");
        return false;
    };
    let kind = match ingest::classify(&d.name, bytes.len() as u64, st.kb_cfg.max_bytes) {
        Ok(kind) => kind,
        Err(e) => {
            // 上限收紧后历史大文件不再过校验：标失败让用户看得见，而不是每轮启动空转
            tracing::warn!(doc_id = %d.doc_id, name = %d.name, err = %e, "知识库启动自愈：文件不再过类型/大小校验，标记失败");
            let _ = store::set_status(&st.owned, &v, &d.doc_id, store::DocStatus::Failed, &e.to_string()).await;
            return true;
        }
    };
    // 与在线上传同一条内存闸：恢复同样整文件入内存 + 解析。后台任务等得起，排队取（不 429）。
    let Ok(permit) = UPLOAD_GATE.acquire().await else { return false };
    tracing::info!(doc_id = %d.doc_id, name = %d.name, has_chunks = d.has_chunks, "知识库启动自愈：重跑入库重活");
    let req = ingest::OwnedUploadReq {
        space_id: d.space_id.clone(),
        folder_id: d.folder_id.clone(),
        file_name: d.name.clone(),
        mime: d.mime.clone(),
        bytes,
        // preset 未持久化到 doc 行（与 reprocess 端点同一个有意简化）：自愈按 general 重建
        preset: None,
    };
    let job = if d.has_chunks {
        ingest::IngestJob::Rebuild { req }
    } else {
        ingest::IngestJob::Fresh { req, kind }
    };
    run_ingest_job_logged(st, &v, &d.doc_id, &d.name, job, permit).await;
    true
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
    // 读端点上每次必写太亏：先列，缺个人空间再幂等建并重列（常见路径省一次 INSERT）
    let rows = store::list_spaces(&st.owned, &v.login, &v.roles).await.map_err(kb_err)?;
    let mut rows = if rows.iter().any(|s| s.space_id == v.login) {
        rows
    } else {
        store::ensure_space(&st.owned, &v.login, &v.login).await.map_err(kb_err)?;
        store::list_spaces(&st.owned, &v.login, &v.roles).await.map_err(kb_err)?
    };
    // 【share_config v2 · 部门支路】store::list_spaces 的手写内联判据只认 owner/login/role
    // （store.rs 本轮属另一路改动窗口），dept 授权带来的可见空间在这里并上——
    // 纯增量合并：按 space_id 去重，不删减既有任何一行。
    let dept_rows = acl::dept_visible_spaces(&st.owned, &v.login).await.map_err(kb_err)?;
    if !dept_rows.is_empty() {
        let mut seen: HashSet<String> = rows.iter().map(|s| s.space_id.clone()).collect();
        rows.extend(dept_rows.into_iter().filter(|s| seen.insert(s.space_id.clone())));
    }
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
    // 存在性判定用点查：COUNT 恒返一行（fetch_optional 语义错位），且用不着全表计数
    let collision = st
        .auth_mysql
        .fixed("SELECT 1 FROM t_employee WHERE login_name=? AND deleted_flag=0 LIMIT 1")
        .bind(&space_id)
        .fetch_optional::<(i64,)>()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "DMS 身份服务暂时不可用"))?
        .is_some();
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
    // 本地改写：一次小写化 + 字面量后缀比较（原形态每次调用 4 次 format! 分配）
    // 表格文档不再 409 挡路（裁决：重处理＝同内容的替换重建）——Overwrite 影子链把
    // 知识索引与问数表两条通道的旧数据都清理重建，切换前任何失败旧版本原样保留，
    // 不存在「覆盖仍可用数据」的窗口。
    let lower_name = row.name.to_ascii_lowercase();
    let is_tabular = [".csv", ".xls", ".xlsx", ".xlsm"].iter().any(|ext| lower_name.ends_with(ext));
    let path = stored_file(&st.kb_cfg.root, &row.doc_id)
        .await
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "原始文件已不存在，请重新上传"))?;
    // 与 upload 同一条内存闸：重处理同样整文件入内存，不占许可就绕过了 20MB × N 的防线；
    // 429 语义与 upload 一致（拿不到直接拒，不排队）。许可随重活 move 进后台任务。
    let permit = UPLOAD_GATE.try_acquire().map_err(|_| upload_gate_full())?;
    let bytes = tokio::time::timeout(UPLOAD_READ_TIMEOUT, tokio::fs::read(&path))
        .await
        .map_err(|_| err(StatusCode::REQUEST_TIMEOUT, "文档文件读取超时，请重试"))?
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, MSG_FILE_UNREADABLE))?;
    // 同步预检：类型/大小超限当场 400（进了后台就只能留日志）。重建链内部还会再
    // `classify` 一次——类型白名单只有 knowledge 那一处实现，这里是提前报错，不是第二份判定。
    ingest::classify(&row.name, bytes.len() as u64, st.kb_cfg.max_bytes).map_err(kb_err)?;
    // 影子重建是重活（parse→chunk→embed），后台跑——同步 await 会被超时断连取消，
    // 与 upload 同一个坑。响应立即返回当前行（影子链不完成，线上版本不动），前端轮询。
    let req = ingest::OwnedUploadReq {
        space_id: row.space_id.clone(),
        folder_id: row.folder_id.clone(),
        file_name: row.name.clone(),
        mime: row.mime.clone(),
        bytes,
        // 重处理不保留原分块策略（恒 general）：preset 未持久化到 doc 行，当前是有意的
        // 简化——按 qa/laws 等策略上传的文档重处理会回到 general 分块，恢复策略需先落库。
        preset: None,
    };
    // 表格走覆盖链（旧物理表/数据源登记随新结构重建，无需重传文件）；其余原地重建。
    let job = if is_tabular {
        ingest::IngestJob::Overwrite { req }
    } else {
        ingest::IngestJob::Rebuild { req }
    };
    spawn_ingest_job(st.clone(), v, row.doc_id.clone(), row.name.clone(), job, permit);
    Ok(Json(doc_json(&row, chrono::Local::now().date_naive())))
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
        return Err(no_space_write(&row.space_id));
    }
    store::set_enabled(&st.owned, &v, &id, req.enabled).await.map_err(kb_err)?;
    // 走到这里文档状态已翻转：同步失败报裸 500 会让调用方以为整单没成（重试又返回 ok），
    // 文案必须带上「状态已变更」这一半真相。
    sync_source_state(&st, &id, req.enabled).await.map_err(|_| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "文档状态已变更，但问数数据源状态同步失败，请重试以收敛两侧状态",
        )
    })?;
    Ok(Json(serde_json::json!({ "ok": true, "enabled": req.enabled })))
}

pub async fn update_doc_metadata(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<DocMetadataReq>,
) -> Result<ApiOk, ApiErr> {
    // manager_viewer 的展开形态：身份字段直接借 req，不克隆两份 String 组一次性 KbQuery
    let p = manager_principal(&st, &headers, &req.login_name, &req.role_code).await?;
    let v = Viewer::new(p.login_name, vec![p.role_code]);
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
    // 非表格文档没有上传数据源，sync 只是白打一条 UPDATE——仅存在上传源时才同步；
    // 且同步失败只 warn：元数据变更已提交，报 500 就是「已提交又报失败」的部分失败。
    let has_source = dms_semantic::registry::datasource::get_datasource(
        st.owned.pool(),
        &tabular::upload_ds_id(&id),
    )
    .await
    .ok()
    .flatten()
    .is_some();
    if has_source {
        if let Err(e) = sync_source_state(&st, &id, updated.enabled).await {
            tracing::warn!(doc_id = %id, err = %e.1.0, "元数据已提交，问数数据源状态同步失败");
        }
    }
    Ok(Json(doc_json(&updated, chrono::Local::now().date_naive())))
}

// ════════════════════════ KB 运营小包（Y12 + Y7）════════════════════════
//
// 三个 handler **已在 `main.rs` 接线**（注册行如下，路由变动时两边同步改）：
//   .route("/api/kb/ingest-url", post(kb_api::ingest_url))
//   .route("/api/kb/space/{id}/export", get(kb_api::export_space))
//   .route("/api/kb/doc/{id}/description", post(kb_api::generate_description))
//
// 权限判据：先过 `kb_manager` 管理闸（本块三个 handler 均属管理面），空间级仍沿用既有口径——
// 读 = `acl::space_readable`，写 = `acl::space_writable`，且写语句内联复核（fail-closed）。
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
        // 先认证再占上传槽（与 `upload` 同序）：未认证请求不能耗尽许可。
        // 认证口径与 upload 有刻意差异：upload 走 `session_viewer`（multipart 必须先认证
        // 再读 body，刻意拒绝 body 身份回退）；本端点是 JSON body 可预读，故用
        // `manager_viewer`（接受 body 的 login_name 回退，同一条 resolve_identity 收口）。
        let v = manager_viewer(&st, &headers, &req.q).await?;
        // 许可随重活 move 进后台任务（与 `upload` 同一条闸、同一个理由）
        let permit = UPLOAD_GATE.try_acquire().map_err(|_| upload_gate_full())?;
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
        // 快路径在请求内，重活后台跑（与 `upload` 同一条异步化理由：PDF 抓取件解析同样
        // 可能超过代理超时）。响应立即返回进行态 doc 行，前端轮询。
        let prepared = ingest::prepare(&st.owned, &v, &st.kb_cfg, up).await.map_err(kb_err)?;
        if let Some(job) = prepared.job {
            spawn_ingest_job(st.clone(), v.clone(), prepared.doc_id.clone(), page.file_name.clone(), job, permit);
        }
        // 与 `upload` 同序：统一再绑定目标目录，写权限在返回结果前于同一条写语句中复核。
        // html/pdf 不会产表格数据源（通道②只看 sheets 非空），无需登记/清理数据源。
        store::move_doc(&st.owned, &v, &prepared.doc_id, &space_id, req.q.folder_id.as_deref())
            .await
            .map_err(kb_err)?;
        // source_uri 记**最终落地 URL**（重定向后的真实来源）。回写失败（撤权竞态/DB 抖动）
        // 不抹掉已入库文档：来源地址是治理元数据，不是入库正确性。
        if let Err(e) = store::set_doc_source_uri(&st.owned, &v, &prepared.doc_id, &page.final_url).await {
            tracing::warn!(doc_id = %prepared.doc_id, err = %e, "URL 已入库，来源地址回写失败");
        }
        let row = acl::doc_for_viewer(&st.owned, &v, &prepared.doc_id).await.map_err(kb_err)?;
        Ok(Json(upload_doc_json(&row, prepared.replaced.as_deref())))
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
    ///
    /// 每跳重建 reqwest Client 是**刻意的安全换性能**：resolve 钉定（`resolve_to_addrs`）
    /// 绑在 client 上，共享 client 会让被钉地址串到别的 host。要优化可按 `(host, addrs)`
    /// 做小型缓存，但现状（≤4 跳、低频管理面操作）是有意权衡，先别动。
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
        // 先截取再一次性小写化（原形态 collect Vec 再 lossy 转换，两次分配）
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
        let text = head.trim_start_matches('\u{feff}').trim_start();
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
        let mut len = 0usize; // 字符计数器：chars().count() 每字符重扫全串是 O(n²)
        for c in stem.chars() {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') || ('\u{4e00}'..='\u{9fff}').contains(&c) {
                slug.push(c);
                len += 1;
            } else if !slug.ends_with('_') {
                slug.push('_');
                len += 1;
            }
            if len >= 60 {
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
            return Err(no_space_read(&id));
        }
        let (limit, offset) = export_page_params(q.limit, q.offset);
        // 三条查询互不依赖：并发发出，不串行累加 RTT（同一池，错误各自收敛）
        let (total, rows, folders) = tokio::join!(
            store::count_space_docs(&st.owned, &v, &id),
            store::list_docs_page(&st.owned, &v, &id, limit, offset),
            store::list_folders(&st.owned, &v, &id),
        );
        let total = total.map_err(kb_err)?;
        let rows = rows.map_err(kb_err)?;
        let mut folders = folders.map_err(kb_err)?;
        folders.truncate(EXPORT_MAX_FOLDERS);
        let next_offset = (offset + limit < total).then_some(offset + limit);
        let today = chrono::Local::now().date_naive();
        Ok(Json(serde_json::json!({
            "space_id": id,
            "offset": offset,
            "limit": limit,
            "total_docs": total,
            "next_offset": next_offset,
            "folders": folders.iter().map(folder_json).collect::<Vec<_>>(),
            "docs": rows.iter().map(|d| doc_json(d, today)).collect::<Vec<_>>(),
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
    /// grantee 三路（login/role/dept）与 `acl::space_acl_sql!` 逐字同口径。
    const DESC_EXCERPT_SQL: &str =
        "SELECT c.text FROM kb.chunk c JOIN kb.doc d ON d.doc_id = c.doc_id
         WHERE c.doc_id = $1
           AND d.enabled = true AND d.status IN ('chunked','embedded')
           AND EXISTS (SELECT 1 FROM kb.space s WHERE s.space_id = d.space_id
             AND (s.owner = $2 OR EXISTS (SELECT 1 FROM kb.acl a
               WHERE a.scope = 'space' AND a.target_id = s.space_id
                 AND a.perm IN ('read','write')
                 AND ((a.grantee_kind = 'login' AND a.grantee = $2)
                   OR (a.grantee_kind = 'role' AND a.grantee = ANY($3::text[]))
                   OR (a.grantee_kind = 'dept' AND a.grantee =
                       (SELECT m.dept FROM kb.user_dept m WHERE m.login = $2))))))
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
            return Err(no_space_write(&row.space_id));
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
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, MSG_KB_UNAVAILABLE))?;
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
        Ok(Json(doc_json(&updated, chrono::Local::now().date_naive())))
    }

    /// LLM 产出 → 单段描述：压缩空白为一行、剥首尾成对引号、封顶 `DESC_MAX_CHARS`。
    fn sanitize_description(text: &str) -> String {
        let mut out = String::new();
        let mut len = 0usize; // 字符计数器：chars().count() 每字符重扫是 O(n²)
        let mut pending_space = false;
        for c in text.trim().chars() {
            if c.is_whitespace() {
                pending_space = true;
                continue;
            }
            if pending_space && !out.is_empty() {
                out.push(' ');
                len += 1;
            }
            pending_space = false;
            out.push(c);
            len += 1;
            if len >= DESC_MAX_CHARS {
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
            // URL 入库与上传共用同一条 ingest 链（不许有第二份入库实现）：
            // 请求内只做抓取 + 快路径（prepare），重活必须经 spawn_ingest_job 进后台
            let body = src.split("pub async fn ingest_url").nth(1).unwrap();
            let body = body.split("enum FetchedKind").next().unwrap();
            assert!(body.contains("ingest::prepare(") && body.contains("store::move_doc("));
            assert!(body.contains("spawn_ingest_job("), "重活必须后台化: {body}");
            assert!(!body.contains("ingest::run_job("), "请求内不许同步跑重活: {body}");
            let fetch = body.find("fetch_url_guarded").unwrap();
            let prepare = body.find("ingest::prepare(").unwrap();
            let spawn = body.find("spawn_ingest_job(").unwrap();
            assert!(fetch < prepare && prepare < spawn, "必须先抓取、再快路径、最后挂后台: {body}");
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
    // 角色与部门两份目录互不依赖：并发发出（授权对话框的下拉候选同源）
    let (roles, departments) = tokio::join!(dms_role_options(&st), dms_dept_options(&st));
    let roles = roles?;
    let departments = departments?;
    let role_names: HashMap<&str, &str> = roles
        .iter()
        .map(|r| (r.role_code.as_str(), r.role_name.as_str()))
        .collect();
    let dept_names: HashMap<&str, &str> = departments
        .iter()
        .map(|d| (d.dept_id.as_str(), d.dept_name.as_str()))
        .collect();
    let rows = acl::list_target(&st.owned, acl::AclScope::Space, &id).await.map_err(kb_err)?;
    Ok(Json(serde_json::json!({
        "grants": rows.into_iter().map(|r| {
            let grantee_name = match r.grantee_kind.as_str() {
                "role" => role_names.get(r.grantee.as_str()).copied(),
                "dept" => dept_names.get(r.grantee.as_str()).copied(),
                _ => None,
            };
            serde_json::json!({
                "grantee_kind": r.grantee_kind, "grantee": r.grantee, "perm": r.perm,
                "grantee_name": grantee_name,
            })
        }).collect::<Vec<_>>()
        ,"roles": roles,
        // 部门目录随授权清单同包下发（管理面闸已覆盖本端点），前端下拉免增新路由
        "departments": departments,
        "limits": { "batch_grants": MAX_BATCH_GRANTS }
    })))
}

pub async fn grant_space(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SpaceGrantReq>,
) -> Result<ApiOk, ApiErr> {
    manager_principal(&st, &headers, &req.login_name, &req.role_code).await?;
    ensure_space_exists(&st, &id).await?;

    if !req.role_codes.is_empty() {
        return grant_roles(&st, &id, &req).await;
    }

    let entry = space_acl_entry(&id, &req)?;
    validate_grantee(&st, &entry.grantee).await?;
    if let acl::Grantee::Dept(_) = &entry.grantee {
        // store 层空间授权函数的 kind 白名单本轮仍只有 login|role（store.rs 属另一路
        // 改动窗口），部门授权走 acl 层同语义两步：先撤反向档再落新档，保持
        // 「同一对象只保留一个权限档」；revoke 在前——中途失败只会丢授权（fail-closed），
        // 不会留下 read+write 双档。
        let opposite = match entry.perm {
            acl::Perm::Read => acl::Perm::Write,
            acl::Perm::Write => acl::Perm::Read,
        };
        let clear = acl::AclEntry { perm: opposite, ..entry.clone() };
        acl::revoke(&st.owned, &clear).await.map_err(kb_err)?;
        acl::grant(&st.owned, &entry).await.map_err(kb_err)?;
        return Ok(Json(serde_json::json!({
            "ok": true, "updated": true, "succeeded": 1, "failed": []
        })));
    }
    store::grant_space_acl(
        &st.owned,
        &id,
        entry.grantee.kind(),
        entry.grantee.id(),
        entry.perm.as_str(),
    )
    .await
    .map_err(kb_err)?;
    // wire 契约冻结：单授权的 "succeeded" 是数字 1，批量（grant_roles）是对象数组——
    // 前端按两种形状解析；统一为数组留给下个协议版本。
    Ok(Json(serde_json::json!({
        "ok": true, "updated": true, "succeeded": 1, "failed": []
    })))
}

async fn grant_roles(
    st: &AppState,
    id: &str,
    req: &SpaceGrantReq,
) -> Result<ApiOk, ApiErr> {
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
    Ok(Json(serde_json::json!({
        "ok": true, "updated": true, "partial": false, "succeeded": succeeded, "failed": [],
    })))
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
        if seen.insert(code) {
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
    // 撤权不要求对象仍存在于 DMS；否则角色/部门被删除后会留下永远无法清理的历史 ACL。
    // store 层撤权函数的 kind 白名单同样只认 login|role，dept 走 acl 层同语义删除。
    if let acl::Grantee::Dept(_) = &entry.grantee {
        acl::revoke(&st.owned, &entry).await.map_err(kb_err)?;
        return Ok(Json(serde_json::json!({ "ok": true })));
    }
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
    // KB 共享面用 `parse_shareable`（login|role|dept）；ds 面的严格 parse 不收 dept，勿混用
    let grantee = acl::Grantee::parse_shareable(&req.grantee_kind, grantee_id)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "grantee_kind 只能是 login、role 或 dept"))?;
    let max = match &grantee {
        acl::Grantee::Login(_) => MAX_LOGIN_NAME,
        acl::Grantee::Role(_) => MAX_ROLE_CODE,
        // department_id 的字符串形（i64 至多 20 位）：与 login 同一条长度闸已足够
        acl::Grantee::Dept(_) => MAX_LOGIN_NAME,
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
            // 点查：拉全表目录（LIMIT 500）校验一个 code 既费，又会因目录截断把合法角色
            // 误判「不存在」——存在性判定就该是 SELECT 1
            let exists = st
                .auth_mysql
                .fixed("SELECT 1 FROM t_role WHERE TRIM(role_code)=? LIMIT 1")
                .bind(code)
                .fetch_optional::<(i64,)>()
                .await
                .map_err(|_| err(StatusCode::BAD_GATEWAY, "DMS 角色目录暂时不可用"))?
                .is_some();
            if !exists {
                return Err(err(StatusCode::BAD_REQUEST, "DMS 角色不存在"));
            }
        }
        acl::Grantee::Dept(dept) => {
            // grantee 存 department_id 的字符串形：非数字直接 400，不落永远匹配不上的垃圾行
            let Ok(dept_id) = dept.parse::<i64>() else {
                return Err(err(StatusCode::BAD_REQUEST, "部门授权对象必须是 DMS 部门 ID"));
            };
            // 与角色同 idiom 的点查（部门目录同样走 LIMIT 截断，存在性判定不许拉全表）
            let exists = st
                .auth_mysql
                .fixed(
                    "SELECT 1 FROM t_department \
                     WHERE department_id=? AND status=1 AND deleted_flag=0 LIMIT 1",
                )
                .bind(dept_id)
                .fetch_optional::<(i64,)>()
                .await
                .map_err(|_| err(StatusCode::BAD_GATEWAY, "DMS 部门目录暂时不可用"))?
                .is_some();
            if !exists {
                return Err(err(StatusCode::BAD_REQUEST, "DMS 部门不存在或已停用"));
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
    // 借用判重：只在构造 RoleOption 时克隆一次（原形态每行都克隆一次 code）
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for (role_code, role_name) in &rows {
        if seen.insert(role_code.as_str()) {
            out.push(RoleOption { role_code: role_code.clone(), role_name: role_name.clone() });
        }
    }
    Ok(out)
}

#[derive(Clone, serde::Serialize)]
struct DeptOption {
    /// `t_department.department_id` 的字符串形：与 `kb.acl.grantee`/`kb.user_dept.dept` 同型，
    /// 前端下拉值直接当 grantee 提交，免一次类型换算
    dept_id: String,
    dept_name: String,
}

/// 与角色目录同口径的有界枚举：有效部门（status=1 且未删除），单表、显式字段。
/// 过滤条件与 policy 层 `self_and_children_departments` 的全表扫描口径一致。
const DMS_DEPT_OPTIONS_SQL: &str =
    "SELECT department_id, TRIM(name) \
     FROM t_department \
     WHERE status = 1 AND deleted_flag = 0 \
       AND name IS NOT NULL AND TRIM(name)<>'' \
     ORDER BY TRIM(name), department_id \
     LIMIT 500";

async fn dms_dept_options(st: &AppState) -> Result<Vec<DeptOption>, ApiErr> {
    let rows: Vec<(i64, String)> = st
        .auth_mysql
        .fixed(DMS_DEPT_OPTIONS_SQL)
        .fetch_all()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "DMS 部门目录暂时不可用"))?;
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for (dept_id, dept_name) in rows {
        if seen.insert(dept_id) {
            out.push(DeptOption { dept_id: dept_id.to_string(), dept_name });
        }
    }
    Ok(out)
}

async fn ensure_space_exists(st: &AppState, id: &str) -> Result<(), ApiErr> {
    if store::space_exists(&st.owned, id).await.map_err(kb_err)? {
        Ok(())
    } else {
        Err(err(StatusCode::NOT_FOUND, format!("知识空间 {id} 不存在")))
    }
}

/// 撤权复核（fail-closed）：写路径两步之间权限可能已失效，查不到/查错一律按不可写。
async fn still_writable(st: &AppState, v: &Viewer, doc_id: &str) -> bool {
    match store::get_doc(&st.owned, doc_id).await {
        Ok(Some(row)) => acl::space_writable(&st.owned, v, &row.space_id).await.unwrap_or(false),
        _ => false,
    }
}

/// 【K4 通道②】登记数据源。可见性由数据源注册表动态继承来源文档的空间 owner/ACL，
/// 不复制一套会与空间分享、撤权漂移的 ds ACL。
///
/// 文本检索已经可用时，问数源失败不抹掉整份文档；但物理 schema、数据源登记与结构注册
/// 必须成套成功，否则立即清理半成品并把降级提示写回文档。
/// 入参是通道②的**产物**而不是 `Ingested`：入库异步化后登记只能发生在后台任务里
/// （`run_job` 返回时才有 `source`），调用方保证 `src` 非 None。
async fn register_source(
    st: &AppState,
    v: &Viewer,
    doc_name: &str,
    doc_id: &str,
    src: &tabular::TabularSource,
) -> bool {
    let allowed = still_writable(st, v, doc_id).await;
    if !allowed {
        let _ = cleanup_source(st, doc_id).await;
        tracing::warn!(doc_id, reason = "upload_permission_revoked", "表格数据源登记前写权限已失效");
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
        let _ = cleanup_source(st, doc_id).await;
        let _ = store::append_notice(
            &st.owned,
            v,
            doc_id,
            "表格已入知识库，问数数据源登记失败，请重新处理",
        )
        .await;
        tracing::warn!(ds_id = %src.ds_id, doc_id, reason = "datasource_register_failed", "上传表格已建表，但登记数据源失败");
        return false;
    }
    if !sync_upload_schema(st, src).await {
        let _ = cleanup_source(st, doc_id).await;
        let _ = store::append_notice(
            &st.owned,
            v,
            doc_id,
            "表格已入知识库，问数结构采集失败，请重新处理",
        )
        .await;
        return false;
    }
    let still_allowed = still_writable(st, v, doc_id).await;
    if !still_allowed {
        let _ = cleanup_source(st, doc_id).await;
        tracing::warn!(doc_id, reason = "upload_permission_revoked", "表格数据源登记后写权限已失效，已清理");
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

pub async fn docs(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = manager_viewer(&st, &headers, &q).await?;
    let space_id = space_of(&v, &q);
    if !acl::space_readable(&st.owned, &v, &space_id).await.map_err(kb_err)? {
        return Err(no_space_read(&space_id));
    }
    // 两条查询互不依赖：并发发出，不串行累加 RTT
    let (rows, folders) = tokio::join!(
        store::list_docs(&st.owned, &v, &space_id),
        store::list_folders(&st.owned, &v, &space_id),
    );
    let mut rows = rows.map_err(kb_err)?;
    let mut folders = folders.map_err(kb_err)?;
    // 【share_config v2 · 部门支路】store 两份清单的内联判据不认 dept（另一路窗口）：
    // 并上 dept 授权的可见增量（纯增量，按主键去重，不删减既有任何一行）。
    // 与上方空间读闸的口径差由 acl::dept_visible_* 的注释兜底说明。
    {
        let (extra_docs, extra_folders) = tokio::join!(
            acl::dept_visible_docs(&st.owned, &v.login, &space_id),
            acl::dept_visible_folders(&st.owned, &v.login, &space_id),
        );
        let extra_docs = extra_docs.map_err(kb_err)?;
        if !extra_docs.is_empty() {
            let mut seen: HashSet<String> = rows.iter().map(|d| d.doc_id.clone()).collect();
            rows.extend(extra_docs.into_iter().filter(|d| seen.insert(d.doc_id.clone())));
        }
        let extra_folders = extra_folders.map_err(kb_err)?;
        if !extra_folders.is_empty() {
            let mut seen: HashSet<String> = folders.iter().map(|f| f.folder_id.clone()).collect();
            folders.extend(extra_folders.into_iter().filter(|f| seen.insert(f.folder_id.clone())));
        }
    }
    let today = chrono::Local::now().date_naive();
    let docs: Vec<serde_json::Value> = rows.iter().map(|d| doc_json(d, today)).collect();
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
        return Err(no_space_read(&space_id));
    }
    let mut rows = store::list_folders(&st.owned, &v, &space_id).await.map_err(kb_err)?;
    // dept 支路的可见增量并集（与 docs 端点同一口径，纯增量不删减）
    let extra = acl::dept_visible_folders(&st.owned, &v.login, &space_id).await.map_err(kb_err)?;
    if !extra.is_empty() {
        let mut seen: HashSet<String> = rows.iter().map(|f| f.folder_id.clone()).collect();
        rows.extend(extra.into_iter().filter(|f| seen.insert(f.folder_id.clone())));
    }
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
        return Err(no_space_write(&row.space_id));
    }
    store::move_doc(&st.owned, &v, &id, &row.space_id, req.folder_id.as_deref())
        .await
        .map_err(kb_err)?;
    let updated = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    Ok(Json(doc_json(&updated, chrono::Local::now().date_naive())))
}

pub async fn doc(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = manager_viewer(&st, &headers, &q).await?;
    // 两条查询互不依赖：并发发出，不串行累加 RTT
    let (row, related) = tokio::join!(
        acl::doc_for_viewer(&st.owned, &v, &id),
        store::related_docs(&st.owned, &v, &id),
    );
    let row = row.map_err(kb_err)?;
    let related = related.map_err(kb_err)?;
    let mut body = doc_json(&row, chrono::Local::now().date_naive());
    if let Some(obj) = body.as_object_mut() {
        obj.insert("related_documents".into(), serde_json::json!(related.iter().map(relation_json).collect::<Vec<_>>()));
    }
    Ok(Json(body))
}

/// 下载/预览原始文档（`/api/kb/doc/{id}/download` 与 `/file` 共用这个只读端点）。
/// 鉴权双通道：会话（Bearer 优先，`login_name` 回退由 `resolve_identity` 的开关收口）走与
/// 详情/引用相同的 ACL（`doc_for_viewer`，不存在与不可见统一 403，fail-closed）；
/// 或 15 分钟预览票据（`preview_ticket` 端点签发）——iframe 放不了 Authorization 头，
/// 票据是把那次 ACL 授权浓缩成的有时效能力凭证，**只对本端点放行**。
///
/// 🔴 Content-Type 是**扩展名白名单**（`serve_mime`），不信上传时自报的 `row.mime`：
/// 自报 mime 是攻击面——一个 `text/html`/`image/svg+xml` 就能把脚本写进预览上下文。
/// svg 刻意不在白名单（可执行脚本），html 按 text/plain 给（安全转文本），
/// Office 等一律 octet-stream + attachment + nosniff：只许下载，不许内嵌渲染。
///
/// 预览增强（全部可选 query 组合）：`inline=1` → `Content-Disposition: inline`；
/// `Range: bytes=a-b` 单区间 → 206 分段读（上行带宽小，PDF 预览不再等全量下载完）；
/// `office_pdf=1` 且落盘扩展名属 Office 白名单 → soffice 转 PDF 返回（见 `office_pdf_path`）。
pub async fn download_doc(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> Result<axum::response::Response, ApiErr> {
    let row = if let Some(ticket) = q.ticket.as_deref() {
        // 票据通道：验签（常量时间比较）+ 时效 + doc_id 绑定三关全过才算数；
        // 过了也只核对文档还在——授权本身发生在票据签发那一刻。
        if !verify_preview_ticket(ticket, &id, chrono::Utc::now().timestamp()) {
            return Err(err(StatusCode::UNAUTHORIZED, "预览票据无效或已过期"));
        }
        store::get_doc(&st.owned, &id)
            .await
            .map_err(kb_err)?
            .ok_or_else(|| err(StatusCode::NOT_FOUND, "文档已不存在"))?
    } else {
        let kq = KbQuery {
            login_name: q.login_name.clone(),
            role_code: q.role_code.clone(),
            ..Default::default()
        };
        let v = viewer(&st, &headers, &kq).await?;
        acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?
    };
    let path = stored_file(&st.kb_cfg.root, &row.doc_id)
        .await
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "原始文件已不存在"))?;
    let inline = flag_on(&q.inline);
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    // Office 原件级预览（Yuxi 同款：soffice headless 转 PDF + 磁盘缓存）。
    // 非 Office 扩展名带 office_pdf=1 不报错——原样返回原件，前端按实际 Content-Type 降级。
    if flag_on(&q.office_pdf) && is_office_ext(&path) {
        let pdf = office_pdf_path(&st.kb_cfg.root, &row.doc_id, &path).await?;
        return serve_file(&pdf, &pdf_display_name(&row.name), range, inline).await;
    }
    serve_file(&path, &row.name, range, inline).await
}

/// `download_doc` 的 query。**不用 `#[serde(flatten)]`**：`Query` 走 serde_urlencoded，
/// flatten 在那边直接报 unsupported（与 `ChunkQuery` 同一个坑）。
#[derive(serde::Deserialize, Default)]
pub struct FileQuery {
    /// 预览票据（`POST /api/kb/doc/{id}/preview-ticket` 签发）：Bearer 之外的第二鉴权通道
    ticket: Option<String>,
    /// `1`/`true` → `Content-Disposition: inline`（iframe 预览）；缺省 attachment
    inline: Option<String>,
    /// `1`/`true` → Office 原件转 PDF 返回（仅 doc/docx/ppt/pptx/xls/xlsx/xlsm 生效）
    office_pdf: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// 开关型 query 的真值口径：`1`/`true`（大小写不敏感）为真，其余（含空串）为假
fn flag_on(v: &Option<String>) -> bool {
    v.as_deref().is_some_and(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true"))
}

/// 文件流式返回（原件与 Office 转换产物共用出口）：
/// - 无 Range：整读（原件 ≤ `kb_max_mb`，内存可控），200；
/// - 单区间 Range：分段读 → 206 + Content-Range；越界 → 416 + `Content-Range: bytes */size`；
/// - 永远带 `Accept-Ranges: bytes`（浏览器 PDF 查看器靠它决定发不发分段请求）。
async fn serve_file(
    path: &std::path::Path,
    display_name: &str,
    range_header: Option<&str>,
    inline: bool,
) -> Result<axum::response::Response, ApiErr> {
    use axum::response::IntoResponse;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let size = tokio::fs::metadata(path)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, MSG_FILE_UNREADABLE))?
        .len();
    let mut out = HeaderMap::new();
    out.insert(header::CONTENT_TYPE, HeaderValue::from_static(serve_mime(path)));
    out.insert(header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff"));
    out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    let encoded = percent_encode_filename(display_name);
    let disposition = format!(
        "{}; filename=\"knowledge-file\"; filename*=UTF-8''{encoded}",
        if inline { "inline" } else { "attachment" }
    );
    out.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition)
            .unwrap_or_else(|_| HeaderValue::from_static(if inline { "inline" } else { "attachment" })),
    );
    if let Some(range) = parse_range(range_header, size) {
        let (start, end) = match range {
            Ok(r) => r,
            Err(()) => {
                let mut h = HeaderMap::new();
                h.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{size}"))
                        .unwrap_or_else(|_| HeaderValue::from_static("bytes */0")),
                );
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    h,
                    Json(serde_json::json!({ "error": format!("Range 越界（文件共 {size} 字节）") })),
                )
                    .into_response());
            }
        };
        // 读入内存 × N 并发与上传闸防的是同一个问题：分段读上限就是文件尺寸，同一道闸。
        let _permit = DOWNLOAD_GATE.try_acquire().map_err(|_| {
            err(
                StatusCode::TOO_MANY_REQUESTS,
                format!("下载并发已满（同时最多 {DOWNLOAD_PERMITS} 个），请稍后重试"),
            )
        })?;
        let mut buf = vec![0u8; (end - start + 1) as usize];
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, MSG_FILE_UNREADABLE))?;
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, MSG_FILE_UNREADABLE))?;
        file.read_exact(&mut buf)
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, MSG_FILE_UNREADABLE))?;
        out.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                .unwrap_or_else(|_| HeaderValue::from_static("bytes")),
        );
        out.insert(header::CONTENT_LENGTH, HeaderValue::from(buf.len() as u64));
        return Ok((StatusCode::PARTIAL_CONTENT, out, buf).into_response());
    }
    // 先过 ACL 再占下载槽（无权请求不耗许可）；拿不到直接 429（不排队，同 UPLOAD_GATE 的理由）。
    let _permit = DOWNLOAD_GATE.try_acquire().map_err(|_| {
        err(
            StatusCode::TOO_MANY_REQUESTS,
            format!("下载并发已满（同时最多 {DOWNLOAD_PERMITS} 个），请稍后重试"),
        )
    })?;
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, MSG_FILE_UNREADABLE))?;
    out.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    Ok((StatusCode::OK, out, bytes).into_response())
}

/// 单区间 Range 解析（`bytes=a-b` / `a-` / `-b`，闭区间语义同 RFC 9110；`b` 越过 EOF 收敛到
/// 最后一字节）。`None` = 语法不认或多区间——按无 Range 全量 200（RFC 允许忽略不支持的形式）；
/// `Some(Err(()))` = 语法合法但不可满足（起点越过 EOF / 后缀 0 / 空文件）→ 调用方回 416。
fn parse_range(header: Option<&str>, size: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = header?.trim().strip_prefix("bytes=")?.trim();
    // 多区间不支持：忽略整头回 200，比回 416 对客户端更可用（PDF 查看器只发单区间）
    if spec.contains(',') {
        return None;
    }
    if size == 0 {
        return Some(Err(()));
    }
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        // 后缀区间：最后 b 字节（b ≥ size 时收敛为全文件）
        let suffix: u64 = b.parse().ok()?;
        if suffix == 0 {
            return Some(Err(()));
        }
        return Some(Ok((size.saturating_sub(suffix), size - 1)));
    }
    let start: u64 = a.parse().ok()?;
    if start >= size {
        return Some(Err(()));
    }
    let end = if b.is_empty() { size - 1 } else { b.parse::<u64>().ok()?.min(size - 1) };
    if end < start {
        return Some(Err(()));
    }
    Some(Ok((start, end)))
}

// ════════════════════════ 预览票据（15 分钟单文档能力凭证）════════════════════════

/// 票据有效时长（秒）：大 PDF 渐进阅读时浏览器会滚动到哪页才发哪页的 Range 请求，
/// 120s 太短——用户读到后半票据过期， Range 请求 401 直接掉回降级预览；15 分钟覆盖正常阅读，
/// 撤权时效仍在可接受窗口（单文档、只读、HMAC 绑定 doc_id，泄露面本就一次预览）
const PREVIEW_TICKET_TTL_SECS: i64 = 900;

/// `POST /api/kb/doc/{id}/preview-ticket` —— 签 15 分钟单文档预览票据。
/// 走检索面会话认证 + `doc_for_viewer` 授权（与 download 同一条）：
/// 票据只是把这次授权浓缩成有时效的能力凭证，本身不放大任何权限。
pub async fn preview_ticket(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KbQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &q).await?;
    acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    Ok(Json(serde_json::json!({
        "ticket": mint_preview_ticket(&id, chrono::Utc::now().timestamp()),
        "expires_in": PREVIEW_TICKET_TTL_SECS,
    })))
}

/// 票据签名钥匙：与 settings 凭据同一条 `DMS_SECRET_KEY` 派钥路径（`db::crypto::default_key`，
/// 未配置时机器指纹兜底）——不新发明第二把钥匙。票据只在签发它的部署内有效，
/// 与 settings 密文的迁移语义天然一致（换机未配 DMS_SECRET_KEY 时两把钥匙一起换）。
fn preview_ticket_key() -> [u8; 32] {
    crate::db::crypto::default_key().0
}

/// payload = `doc_id|exp(Unix秒)|nonce(16字节hex)`；
/// ticket = `base64url(payload).hex(hmac_sha256(payload))`。
fn mint_preview_ticket(doc_id: &str, now: i64) -> String {
    mint_preview_ticket_with(&preview_ticket_key(), doc_id, now)
}

fn mint_preview_ticket_with(key: &[u8; 32], doc_id: &str, now: i64) -> String {
    use base64::Engine as _;
    let payload = format!("{doc_id}|{}|{}", now + PREVIEW_TICKET_TTL_SECS, random_nonce_hex());
    let sig = ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), payload.as_bytes());
    format!(
        "{}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes()),
        hex_encode(sig.as_ref())
    )
}

/// 验票：先验签（`hmac::verify` 常量时间比较）再解析——没过签名的票据连字段都不配被读。
fn verify_preview_ticket(ticket: &str, doc_id: &str, now: i64) -> bool {
    verify_preview_ticket_with(&preview_ticket_key(), ticket, doc_id, now)
}

fn verify_preview_ticket_with(key: &[u8; 32], ticket: &str, doc_id: &str, now: i64) -> bool {
    use base64::Engine as _;
    let Some((payload_b64, sig_hex)) = ticket.split_once('.') else { return false };
    let Ok(payload) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64)
    else { return false };
    let Ok(sig) = hex_decode(sig_hex) else { return false };
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    if ring::hmac::verify(&k, &payload, &sig).is_err() {
        return false;
    }
    let Ok(payload) = String::from_utf8(payload) else { return false };
    // 恰好三段：doc_id 是 uuid 不含 '|'，多一段少一段都是构造出来的怪票
    let mut parts = payload.split('|');
    let (Some(id), Some(exp), Some(nonce), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else { return false };
    if id != doc_id || nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    match exp.parse::<i64>() {
        Ok(exp) => now <= exp,
        Err(_) => false,
    }
}

/// 16 字节随机 nonce 的 hex（32 字符）：票据唯一性的载体（每次签发都是新票），
/// 随机源与 settings 加密同源；授权本体是签名，nonce 不参与判定。
fn random_nonce_hex() -> String {
    use ring::rand::SecureRandom as _;
    let mut buf = [0u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut buf).is_err() {
        // 随机源理论上不该失败；真失败用 uuid 的 CSPRNG 兜底，不静默产出弱 nonce
        return uuid::Uuid::new_v4().simple().to_string();
    }
    hex_encode(&buf)
}

/// 小写 hex 编解码：为票据这一处编解码不引 hex crate（同 `encode_base64` 的理由）。
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if s.len() % 2 != 0 {
        return Err(());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16).ok_or(())?;
        let lo = (pair[1] as char).to_digit(16).ok_or(())?;
        out.push((hi * 16 + lo) as u8);
    }
    Ok(out)
}

// ════════════════════════ Office 原件 → PDF（Yuxi 同款预览方案）════════════════════════

/// Office 原件级预览的扩展名白名单：只认**落盘扩展名**（与 `serve_mime` 同一信任口径）。
const OFFICE_PDF_EXTS: [&str; 7] = ["doc", "docx", "ppt", "pptx", "xls", "xlsx", "xlsm"];
/// soffice 转换超时：大 PPT 实测可达几十秒，90s 与 Yuxi 同口径
const OFFICE_CONVERT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
/// 并发转换闸：单个 soffice 进程数百 MB，点击风暴能 OOM 宿主机——排队不拒绝（预览等得起）
static OFFICE_CONVERT_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

fn is_office_ext(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| OFFICE_PDF_EXTS.contains(&ext.as_str()))
}

/// 缓存键带源文件指纹（mtime 秒 + size）：重传/重建换了文件自动失效，旧缓存成为惰性孤儿
///（重启自愈或人工清理回收，不影响正确性）。
/// `v2` = 转换参数代际：xls* 族 2026-08-11 起用 SinglePageSheets（宽表不再断列），
/// 不改代际号会让旧分页产物一直命中（缓存键不含转换参数，参数变了必须换名）。
fn office_pdf_cache_path(root: &std::path::Path, doc_id: &str, mtime_secs: u64, size: u64) -> std::path::PathBuf {
    root.join(".preview_cache").join(format!("{doc_id}-{mtime_secs}-{size}-v2.pdf"))
}

/// Office 原件 → 缓存 PDF 路径。缓存命中直接返回；未命中做 per-doc 去重的转换
/// （同一文档的并发请求，第二个在锁上等第一个的产物）。**任何失败统一 404
/// `office_pdf_unavailable`**——前端据此降级到解析内容预览，绝不许 500。
async fn office_pdf_path(
    root: &std::path::Path,
    doc_id: &str,
    src: &std::path::Path,
) -> Result<std::path::PathBuf, ApiErr> {
    let meta = tokio::fs::metadata(src).await.map_err(|_| office_pdf_unavailable())?;
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cache = office_pdf_cache_path(root, doc_id, mtime_secs, meta.len());
    if tokio::fs::metadata(&cache).await.is_ok() {
        return Ok(cache);
    }
    let lock = office_convert_lock(doc_id);
    let _guard = lock.lock().await;
    // 拿到锁后再看一次缓存：等锁期间第一个请求可能已经把产物放进来了
    let result = match tokio::fs::metadata(&cache).await {
        Ok(_) => Ok(cache.clone()),
        Err(_) => convert_office_to_pdf(src, &cache)
            .await
            .map(|_| cache.clone())
            .map_err(|_| office_pdf_unavailable()),
    };
    release_office_convert_lock(doc_id, &lock);
    result
}

/// 转换失败（soffice 缺席/超时/非零退出/IO 失败）的统一出口：404 + 固定错误码 JSON。
fn office_pdf_unavailable() -> ApiErr {
    err(StatusCode::NOT_FOUND, "office_pdf_unavailable")
}

/// per-doc 转换锁表（进程内去重，无跨实例需求：缓存文件本身就是跨实例的去重结果）
fn office_convert_locks(
) -> &'static std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static LOCKS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        std::sync::OnceLock::new();
    LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn office_convert_lock(doc_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    office_convert_locks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(doc_id.to_string())
        .or_default()
        .clone()
}

/// 用完即清（锁表不许随文档数无限涨）：只剩「锁表 + 本调用」两个引用时才摘——
/// 还有等待者就留给等待者清。取与摘都在同一把 map 锁下，不会与并发取用打架。
fn release_office_convert_lock(doc_id: &str, lock: &Arc<tokio::sync::Mutex<()>>) {
    let mut map = office_convert_locks().lock().unwrap_or_else(|e| e.into_inner());
    if Arc::strong_count(lock) == 2 {
        map.remove(doc_id);
    }
}

/// soffice headless 转换（Yuxi 同款命令形态）：唯一 UserInstallation 临时目录防并发 profile 锁；
/// 工作目录落在缓存目录里的 `tmp-<uuid>/`，产物 rename 进缓存——同文件系统原子换名，
/// 并发读者不会看到半个 PDF。超时/缺席/非零退出统一 Err（调用方收敛成 404 降级）。
async fn convert_office_to_pdf(src: &std::path::Path, cache: &std::path::Path) -> Result<(), ()> {
    let Some(cache_dir) = cache.parent() else { return Err(()) };
    tokio::fs::create_dir_all(cache_dir).await.map_err(|_| ())?;
    // canonicalize：kb_root 可能是相对路径，而 soffice 的 -env:UserInstallation 只吃 file:// URL
    let cache_dir = tokio::fs::canonicalize(cache_dir).await.map_err(|_| ())?;
    let src_abs = tokio::fs::canonicalize(src).await.map_err(|_| ())?;
    let work = cache_dir.join(format!("tmp-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&work).await.map_err(|_| ())?;
    let result = convert_office_in(&src_abs, &work).await;
    // 先取产物再清理：produced 就在 work 里，先 remove_dir_all 会把 PDF 一起删掉
    //（2026-08-11 生产事故：rename 扑空 ENOENT，全量预览 404）
    let produced = result?;
    let cache_abs = cache_dir.join(cache.file_name().ok_or(())?);
    let renamed = tokio::fs::rename(&produced, &cache_abs).await.map_err(|e| {
        tracing::warn!(err = %e, to = %cache_abs.display(), "Office 预览 PDF 落缓存失败");
    });
    // 临时目录清理 best-effort：失败留下的 tmp-* 只是磁盘垃圾，不影响缓存正确性
    if let Err(e) = tokio::fs::remove_dir_all(&work).await {
        tracing::warn!(path = %work.display(), err = %e, "Office 预览临时目录清理失败");
    }
    renamed
}

/// 返回 soffice 产物路径（`<work>/<源文件stem>.pdf`）。
async fn convert_office_in(src: &std::path::Path, work: &std::path::Path) -> Result<std::path::PathBuf, ()> {
    let profile = reqwest::Url::from_file_path(work.join("profile")).map_err(|_| ())?;
    // 电子表格启用 SinglePageSheets（每 sheet 一整页）：默认分页会把宽表从列中间切成好几页
    //（2026-08-11 实测 4 页断列），用户要求与源文件一致。LO ≥7.4 支持该 filter option。
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let convert_to = if matches!(ext.as_str(), "xls" | "xlsx" | "xlsm") {
        "pdf:calc_pdf_Export:{\"SinglePageSheets\":{\"type\":\"boolean\",\"value\":true}}"
    } else {
        "pdf"
    };
    // kill_on_drop：超时被 timeout 丢掉的 output() future 必须把 soffice 子进程一起带走，
    // 否则每次超时泄漏一个几百 MB 的僵尸进程
    let mut cmd = tokio::process::Command::new("soffice");
    cmd.args(["--headless", "--nologo", "--nodefault"])
        .arg(format!("-env:UserInstallation={profile}"))
        .args(["--convert-to", convert_to, "--outdir"])
        .arg(work)
        .arg(src)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // 转换闸排队（不拒绝）：并发 soffice 进程数被封顶，超出的请求等同文档锁的等待语义
    let _permit = OFFICE_CONVERT_GATE.acquire().await.map_err(|_| ())?;
    match tokio::time::timeout(OFFICE_CONVERT_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {}
        Ok(Ok(out)) => {
            tracing::warn!(status = %out.status, src = %src.display(), "soffice 转 PDF 非零退出");
            return Err(());
        }
        Ok(Err(e)) => {
            tracing::warn!(err = %e, "soffice 启动失败（容器内未装 LibreOffice？）");
            return Err(());
        }
        Err(_) => {
            tracing::warn!(src = %src.display(), "soffice 转 PDF 超时（90s）");
            return Err(());
        }
    }
    let stem = src.file_stem().and_then(|s| s.to_str()).ok_or(())?;
    let produced = work.join(format!("{stem}.pdf"));
    match tokio::fs::metadata(&produced).await {
        Ok(_) => Ok(produced),
        Err(e) => {
            tracing::warn!(err = %e, path = %produced.display(), "soffice 零退出但产物不在");
            Err(())
        }
    }
}

/// 转换产物的展示文件名：原名换 `.pdf` 尾缀（只进 RFC 5987 展示头，不进磁盘路径）
fn pdf_display_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => format!("{stem}.pdf"),
        _ => format!("{name}.pdf"),
    }
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
    use std::fmt::Write as _;
    let mut out = String::new();
    for b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b'.') {
            out.push(*b as char);
        } else {
            // 直接写进 out：每字节一次 format! 堆分配，CJK 文件名每字符 3 次
            let _ = write!(out, "%{b:02X}");
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
    // 认证优先（与文件内其他端点同序）：未认证空问题应得 401 而非 400
    let v = viewer(&st, &headers, &req.q).await?;
    let question = nonempty_question(&req.question)?;
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
    // 与 search 同一条边缘校验（同族端点同文案）；knowledge 层的 BadInput 仍是兜底防线
    let question = nonempty_question(&req.question)?;
    let a = dms_knowledge::answer::answer(
        &st.owned,
        &st.embed,
        &st.llm,
        &v,
        req.q.space_id.as_deref(),
        question,
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
        // span=1 也是合并跨度回查（retrieve::span 内部 clamp(1,16)）——落进 window 分支
        // 会把「单块」偷偷换成「三块上下文」，语义不对
        Some(n) if n >= 1 => dms_knowledge::retrieve::span(&st.owned, &v, id, n).await,
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

// ---------------------------------------------------------------- SSE 流式问答
//
// 协议 `kb-ask-stream v1`（`POST /api/kb/ask/stream` 首发，`/api/ask/stream` 与
// `/api/xcx/ask/stream` 两个会话型变体复用同一装配）。每帧 `data:` 是**单行 JSON**
// （serde_json 转义换行，帧格式恒一 data 行）：
//
// - `event: meta`   —— 检索完成、生成开始前发一次：
//     `{"trace_id","citations":[Citation…],"searched_docs":N|null, …端点附加键}`。
//     citations 是**候选命中**（未按正文引用压缩，最终引用以 done 为准）；
//     端点附加键：`/api/ask/stream`、`/api/xcx/ask/stream` 带 `conv_id`，本端点带 `space_id`。
// - `event: delta`  —— 正文增量**预览** `{"text":"…"}`，可多次。攒批（ChunkedEventWriter
//     口径）：≥512 字立即发，否则 100ms 一拍冲掉。增量是模型原文、未过口径后处理，
//     客户端必须在 done 时整体替换。
// - `event: done`   —— 成功终止：`{"answer":{Answer}}`，Answer 与 `POST /api/kb/ask`
//     同步端点的 wire 逐字同形（route/kind/markdown/citations/elapsed_ms/trace_id）。
// - `event: error`  —— 失败终止：`{"message":"固定友好文案"}`，真因只进服务端日志。
//
// 零命中路径不调 LLM：meta（citations 空）→ done（「知识库里没有相关内容…」），无 delta。
// 认证/入参错误（401/400）在 SSE 开始之前以普通 JSON 错误返回，不进事件流。

/// delta 攒批口径：攒够 512 字立即发，否则 100ms 一拍冲掉。「字」= Unicode 字符数
/// （中文一字一符），不是字节 —— 按字节会把中文的阈值悄悄压低到 1/3。
const DELTA_FLUSH_CHARS: usize = 512;
const DELTA_FLUSH_MS: u64 = 100;

/// 工人异常消失（panic）时泵补给客户端的终止帧文案：不留白流尾，客户端按失败走降级。
const MSG_STREAM_BROKEN: &str = "回答生成中断，请重试";

/// 流式问答的内部通道项：knowledge 的 `AnswerEvent`（Meta/Delta）+ server 自加的终止项。
pub(crate) enum SseItem {
    Meta(dms_knowledge::answer::AnswerMeta),
    Delta(String),
    /// 最终答案（与同步端点逐字同口径），`elapsed_ms`/`trace_id` 已钉好
    Done(Box<dms_kernel::Answer>),
    /// 失败终止帧；文案由端点给（各自的收敛口径不同），真因已留痕
    Fail(String),
}

/// meta 帧载荷。extra 先插、基础键后插：端点附加键（conv_id 等）撞名也盖不掉协议键。
fn meta_payload(
    m: &dms_knowledge::answer::AnswerMeta,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut o = extra.clone();
    o.insert("trace_id".into(), serde_json::json!(m.trace_id));
    o.insert(
        "citations".into(),
        serde_json::to_value(&m.citations).expect("Citation 是纯数据 struct，序列化不失败"),
    );
    o.insert(
        "searched_docs".into(),
        m.searched_docs.map_or(serde_json::Value::Null, |n| serde_json::json!(n)),
    );
    serde_json::to_string(&o).expect("JSON map 序列化不失败")
}

fn delta_payload(text: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "text": text })).expect("JSON map 序列化不失败")
}

fn done_payload(a: &dms_kernel::Answer) -> String {
    serde_json::to_string(&serde_json::json!({ "answer": a })).expect("Answer 是纯数据 struct，序列化不失败")
}

fn error_payload(msg: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "message": msg })).expect("JSON map 序列化不失败")
}

fn sse_event(name: &'static str, payload: String) -> Event {
    Event::default().event(name).data(payload)
}

/// delta 攒批器：`push` 攒到 ≥512 字立即吐，`flush` 把不足一拍的残余吐掉
/// （100ms 那一拍由 `pump_sse` 的 interval 驱动）。
#[derive(Default)]
struct DeltaBatcher {
    buf: String,
}

impl DeltaBatcher {
    fn push(&mut self, piece: &str) -> Option<String> {
        self.buf.push_str(piece);
        (self.buf.chars().count() >= DELTA_FLUSH_CHARS).then(|| std::mem::take(&mut self.buf))
    }
    fn flush(&mut self) -> Option<String> {
        (!self.buf.is_empty()).then(|| std::mem::take(&mut self.buf))
    }
}

/// 事件泵：工人通道 → SSE 帧。meta/done/error 到达前先冲掉未发 delta（事件序 = 产生序）；
/// `tx` 是有界 mpsc，客户端消费慢时背压自然传到攒批器。send 失败 = 客户端断流，泵即收工
/// （工人在生成侧继续跑完 —— 与同步路径一样，断流不撤销已发起的生成，落账照常）。
async fn pump_sse(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<SseItem>,
    extra: serde_json::Map<String, serde_json::Value>,
    tx: tokio::sync::mpsc::Sender<Result<Event, std::convert::Infallible>>,
) {
    let mut batcher = DeltaBatcher::default();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(DELTA_FLUSH_MS));
    // 攒批期间工人狂推时跳拍（Skip）：连发几拍补帧只会把同一缓冲切成两半，没有意义
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            item = rx.recv() => match item {
                Some(SseItem::Delta(t)) => {
                    if let Some(chunk) = batcher.push(&t) {
                        if tx.send(Ok(sse_event("delta", delta_payload(&chunk)))).await.is_err() { return; }
                    }
                }
                Some(other) => {
                    if let Some(chunk) = batcher.flush() {
                        if tx.send(Ok(sse_event("delta", delta_payload(&chunk)))).await.is_err() { return; }
                    }
                    let (name, payload, terminal) = match other {
                        SseItem::Meta(m) => ("meta", meta_payload(&m, &extra), false),
                        SseItem::Done(a) => ("done", done_payload(&a), true),
                        SseItem::Fail(msg) => ("error", error_payload(&msg), true),
                        SseItem::Delta(_) => unreachable!("delta 已在上个分支收走"),
                    };
                    if tx.send(Ok(sse_event(name, payload))).await.is_err() { return; }
                    if terminal { return; }
                }
                // 工人异常消失（通道无终止帧就关了）：冲掉残余后补 error 帧，客户端不傻等
                None => {
                    if let Some(chunk) = batcher.flush() {
                        let _ = tx.send(Ok(sse_event("delta", delta_payload(&chunk)))).await;
                    }
                    let _ = tx.send(Ok(sse_event("error", error_payload(MSG_STREAM_BROKEN)))).await;
                    return;
                }
            },
            _ = tick.tick() => {
                if let Some(chunk) = batcher.flush() {
                    if tx.send(Ok(sse_event("delta", delta_payload(&chunk)))).await.is_err() { return; }
                }
            }
        }
    }
}

/// 三条流式端点共用的 SSE 装配：事件泵 spawn + 15s keep-alive 注释帧（防反代掐空闲连接）。
pub(crate) fn sse_response(
    rx: tokio::sync::mpsc::UnboundedReceiver<SseItem>,
    extra: serde_json::Map<String, serde_json::Value>,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, out) = tokio::sync::mpsc::channel(8);
    tokio::spawn(pump_sse(rx, extra, tx));
    let stream = futures::stream::unfold(out, |mut out| async move {
        out.recv().await.map(|item| (item, out))
    });
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    )
}

/// 三条流式端点共用的问答工人：`answer_stream` 的事件转进通道；落定后可选会话持久化
/// （user/ai 两条，与 `/api/ask` 同一条 `save_msg_logged`），再发终止帧。
/// `fail_msg` 由各端点给（文案收敛口径不同）；真因只进 tracing。
pub(crate) fn spawn_kb_worker(
    st: &Arc<AppState>,
    v: Viewer,
    space: Option<String>,
    question: &str,
    conv_id: Option<i64>,
    fail_msg: fn(&KbError) -> String,
) -> tokio::sync::mpsc::UnboundedReceiver<SseItem> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SseItem>();
    let st = st.clone();
    let question = question.to_string();
    tokio::spawn(async move {
        let on_event = |ev: dms_knowledge::answer::AnswerEvent| {
            let item = match ev {
                dms_knowledge::answer::AnswerEvent::Meta(m) => SseItem::Meta(m),
                dms_knowledge::answer::AnswerEvent::Delta(t) => SseItem::Delta(t),
            };
            // 客户端断流后 send 失败：丢弃预览增量，答案仍生成完并落账（与同步路径一致）
            let _ = tx.send(item);
        };
        let cfg = st.cfg();
        let out = dms_knowledge::answer::answer_stream(
            &st.owned,
            &st.embed,
            &st.llm,
            &v,
            space.as_deref(),
            &question,
            &cfg.kb_rrf_weights,
            &on_event,
        )
        .await;
        match out {
            Ok(a) => {
                // 会话型端点：与 /api/ask 同口径存 user/ai 两条（写库失败只丢历史，不拦响应）
                if let Some(cid) = conv_id {
                    let payload = serde_json::to_value(&a).unwrap_or_else(|e| {
                        tracing::warn!(conv_id = cid, reason = %e, "流式问答结果序列化失败，会话内落空对象");
                        serde_json::json!({})
                    });
                    crate::chat::save_msg_logged(st.owned.pool(), cid, crate::chat::ROLE_USER, &question, None).await;
                    crate::chat::save_msg_logged(st.owned.pool(), cid, crate::chat::ROLE_AI, "", Some(&payload)).await;
                }
                let _ = tx.send(SseItem::Done(Box::new(a)));
            }
            Err(e) => {
                let msg = fail_msg(&e);
                tracing::warn!(reason = %e, "KB 流式问答失败（客户端收固定文案，细节只留痕）");
                let _ = tx.send(SseItem::Fail(msg));
            }
        }
    });
    rx
}

/// `POST /api/kb/ask/stream` —— `ask` 的 SSE 流式变体（协议见本文件「SSE 流式问答」段头注）。
/// 同步端点契约一字不动：老客户端零影响；认证/校验与 `ask` 同序同文案 —— 401/400 在
/// SSE 开始之前以普通 JSON 错误返回，不进事件流。
pub async fn ask_stream(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<AskKbReq>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>>, ApiErr> {
    let v = viewer(&st, &headers, &req.q).await?;
    // 与 search/ask 同一条边缘校验（同族端点同文案）；knowledge 层的 BadInput 仍是兜底防线
    let question = nonempty_question(&req.question)?;
    let mut extra = serde_json::Map::new();
    extra.insert(
        "space_id".into(),
        req.q.space_id.clone().map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    // 无会话概念（与 `ask` 一致）：conv_id = None 不做持久化
    let rx = spawn_kb_worker(&st, v, req.q.space_id.clone(), question, None, kb_err_msg);
    Ok(sse_response(rx, extra))
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
            // 分块策略（可选）：与 ingest_url 的 JSON 路径同口径，缺省 general
            "preset" => f.q.preset = text(field).await,
            _ => {}
        }
    }
    Ok(f)
}

/// 空串按缺省处理（表单里没填的字段常以空串到达）；文本值先 trim——`space_id="  "`
/// 不该原样当空间 ID 用（走到 403 才暴露）。读取失败记 warn 后按缺省：与未提交不可区分，
/// 但失败本身不该无声。
async fn text(field: axum::extract::multipart::Field<'_>) -> Option<String> {
    match field.text().await {
        Ok(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        Err(e) => {
            tracing::warn!(err = %e, "multipart 文本字段读取失败，按缺省处理");
            None
        }
    }
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

/// `DocRow` → JSON（`DocRow` 不实现 Serialize：knowledge 不该为了 HTTP 形状而依赖协议层）。
/// `today` 由处理函数入口取一次传入：列表端点 N 个文档不该做 N 次时区系统调用。
fn doc_json(d: &DocRow, today: chrono::NaiveDate) -> serde_json::Value {
    let (quality_level, quality_label) = doc_quality(
        &d.status,
        d.enabled,
        d.chunk_count,
        &d.notice,
        d.effective_from,
        d.effective_to,
        today,
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

/// 上传/URL 入库的响应：`doc_json` 之上叠加同名覆盖标记——覆盖命中时前端上传队列行据
/// `replaced` + `replaced_doc_name` 显示「已覆盖旧版本」；非覆盖恒为 `false`/`null`
/// （字段恒在，前端不做存在性分支）。
fn upload_doc_json(d: &DocRow, replaced: Option<&str>) -> serde_json::Value {
    let mut body = doc_json(d, chrono::Local::now().date_naive());
    let obj = body.as_object_mut().expect("doc_json 恒为 JSON 对象");
    obj.insert("replaced".into(), serde_json::json!(replaced.is_some()));
    obj.insert(
        "replaced_doc_name".into(),
        replaced.map_or(serde_json::Value::Null, serde_json::Value::from),
    );
    body
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
    let mut rd = match tokio::fs::read_dir(root).await {
        Ok(rd) => rd,
        Err(e) => {
            // 孤儿文件无声累积是这里唯一的代价，失败必须留痕
            tracing::warn!(doc_id, err = %e, "文档磁盘清理失败：目录不可读，孤儿文件待回收");
            return;
        }
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let p = entry.path();
        if p.file_stem().is_some_and(|s| s == doc_id) {
            if let Err(e) = tokio::fs::remove_file(&p).await {
                tracing::warn!(doc_id, err = %e, path = %p.display(), "文档磁盘文件删除失败，孤儿文件待回收");
            }
        }
    }
}

/// 三步各自 best-effort：物理表/登记行/结构文档任一失败都不跳过剩余步骤——首个失败即
/// early-return 会留下另外两行孤儿（与 delete 注释承诺的「幂等回收」不齐）。错误汇总返回，
/// 调用方（upload/register_source/delete）本就只拿它记 warn。
async fn cleanup_source(st: &AppState, doc_id: &str) -> Result<(), ApiErr> {
    let up_ds = tabular::upload_ds_id(doc_id);
    let mut failed: Vec<&str> = Vec::new();
    if let Err(e) = tabular::drop_source(&st.owned, doc_id).await {
        tracing::warn!(doc_id, err = %e, "上传物理表清理失败");
        failed.push("物理表");
    }
    if let Err(e) =
        dms_semantic::registry::datasource::delete_datasource(st.owned.pool(), &up_ds).await
    {
        tracing::warn!(doc_id, err = %e, "数据源登记行清理失败");
        failed.push("数据源登记");
    }
    if let Err(e) =
        dms_semantic::ingest::schema_sync::drop_schema_docs(st.owned.pool(), &up_ds).await
    {
        tracing::warn!(doc_id, err = %e, "结构注册清理失败");
        failed.push("结构注册");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("知识库数据源清理失败（{}），可重跑同名清理幂等回收", failed.join("/")),
        ))
    }
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
        for name in ["ask", "search", "chunk", "download_doc", "preview_ticket"] {
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

    /// 【share_config v2 · 部门支路】grant 端点参数校验：dept 一支的收/拒与长度闸
    #[test]
    fn space_grant_accepts_dept_kind() {
        let req = SpaceGrantReq {
            grantee_kind: "dept".into(), grantee: " 42 ".into(), ..Default::default()
        };
        let e = space_acl_entry("enterprise-sales", &req).unwrap();
        assert_eq!(e.grantee, acl::Grantee::Dept("42".into()), "外围空白应被归一");
        assert_eq!(e.perm, acl::Perm::Read, "缺省 perm 仍为 read");

        // 非法 kind 的拒绝文案要带上三种合法值
        let bad = SpaceGrantReq {
            grantee_kind: "group".into(), grantee: "x".into(), ..Default::default()
        };
        let (_, Json(body)) = space_acl_entry("s", &bad).unwrap_err();
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("login") && msg.contains("role") && msg.contains("dept"), "{msg}");

        // 长度闸复用 MAX_LOGIN_NAME（department_id 字符串形至多 20 位，64 足够）
        let long = SpaceGrantReq {
            grantee_kind: "dept".into(), grantee: "9".repeat(MAX_LOGIN_NAME + 1), ..Default::default()
        };
        assert!(space_acl_entry("s", &long).is_err(), "超长部门标识必须拒");
        let ok = SpaceGrantReq {
            grantee_kind: "dept".into(), grantee: "9".repeat(MAX_LOGIN_NAME), ..Default::default()
        };
        assert!(space_acl_entry("s", &ok).is_ok());
    }

    /// dept 授权落库走 acl 层（store 层授权/撤权函数的 kind 白名单本轮仍只认
    /// login|role）；且「先撤反向档再落新档」——顺序反了中途失败会留双档
    #[test]
    fn dept_grants_route_through_acl_layer_with_fail_closed_order() {
        let src = include_str!("kb_api.rs");
        let grant = src.split("pub async fn grant_space").nth(1).unwrap();
        let grant = grant.split("\n}\n").next().unwrap();
        assert!(grant.contains("space_acl_entry"), "{grant}");
        let dept_arm = grant.split("if let acl::Grantee::Dept(_)").nth(1)
            .unwrap_or_else(|| panic!("grant_space 缺 dept 支路: {grant}"));
        // 锚定到支路收尾的 return：支路之后是 login/role 的 store 层落库，不属于本支路
        let dept_arm = dept_arm.split("return Ok").next().unwrap();
        assert!(
            dept_arm.find("acl::revoke").unwrap() < dept_arm.find("acl::grant").unwrap(),
            "必须先撤反向档再落新档（fail-closed）: {dept_arm}"
        );
        assert!(!dept_arm.contains("store::grant_space_acl"), "dept 不能走 store 白名单函数");
        let revoke = src.split("pub async fn revoke_space").nth(1).unwrap();
        let revoke = revoke.split("\n}\n").next().unwrap();
        let dept_arm = revoke.split("if let acl::Grantee::Dept(_)").nth(1)
            .unwrap_or_else(|| panic!("revoke_space 缺 dept 支路: {revoke}"));
        let dept_arm = dept_arm.split("return Ok").next().unwrap();
        assert!(dept_arm.contains("acl::revoke"));
        assert!(!dept_arm.contains("store::revoke_space_acl"), "dept 不能走 store 白名单函数");
    }

    /// 部门目录与存在性判定的 SQL 形态：有界枚举 + 点查（与角色目录同 idiom）
    #[test]
    fn dept_catalog_and_existence_sql_shapes() {
        assert!(DMS_DEPT_OPTIONS_SQL.contains("FROM t_department"));
        assert!(DMS_DEPT_OPTIONS_SQL.contains("status = 1 AND deleted_flag = 0"));
        assert!(DMS_DEPT_OPTIONS_SQL.contains("LIMIT 500"), "部门目录必须有界");
        let src = include_str!("kb_api.rs");
        let body = src.split("acl::Grantee::Dept(dept) =>").nth(1).unwrap();
        let body = body.split("\n    }\n").next().unwrap();
        assert!(
            body.contains("SELECT 1 FROM t_department") && body.contains("LIMIT 1"),
            "部门存在性判定必须是点查: {body}"
        );
        assert!(body.contains("parse::<i64>()"), "dept grantee 必须是数字形部门 ID: {body}");
        // 摘录复核 SQL 与 acl 宏同口径（三路 grantee）。常量住在 ops_pack 模块里，
        // 测试模块够不到，按源码文本锚（与本文件其它锚点测试同 idiom）
        let exc = src.split("DESC_EXCERPT_SQL: &str =").nth(1).unwrap();
        let exc = exc.split("ORDER BY c.ord").next().unwrap();
        assert!(exc.contains("a.grantee_kind = 'dept'"), "摘录复核丢了部门支路");
        assert!(exc.contains("kb.user_dept"), "摘录复核没走部门映射");
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

    /// 票据往返：签发 → 验真；改 doc_id、换钥匙、过期、篡改签名/载荷、畸形串全部验假
    #[test]
    fn preview_ticket_roundtrip_and_tamper_resistance() {
        const K1: [u8; 32] = [3u8; 32];
        const K2: [u8; 32] = [4u8; 32];
        let now = 1_800_000_000i64;
        let doc = "11111111-2222-3333-4444-555555555555";
        let ticket = mint_preview_ticket_with(&K1, doc, now);
        // 形状：base64url(payload).64hex —— payload 恰好 `doc_id|exp|nonce` 三段
        let (payload_b64, sig) = ticket.split_once('.').unwrap();
        assert_eq!(sig.len(), 64, "HMAC-SHA256 的 hex 必须 64 字符: {ticket}");
        use base64::Engine as _;
        let payload = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64).unwrap(),
        )
        .unwrap();
        let parts: Vec<&str> = payload.split('|').collect();
        assert_eq!(parts.len(), 3, "{payload}");
        assert_eq!(parts[0], doc);
        assert_eq!(parts[1].parse::<i64>().unwrap(), now + PREVIEW_TICKET_TTL_SECS);
        assert_eq!(parts[2].len(), 32, "nonce 是 16 字节 hex: {payload}");

        assert!(verify_preview_ticket_with(&K1, &ticket, doc, now));
        assert!(verify_preview_ticket_with(&K1, &ticket, doc, now + PREVIEW_TICKET_TTL_SECS), "边界时刻仍有效");
        assert!(!verify_preview_ticket_with(&K1, &ticket, doc, now + PREVIEW_TICKET_TTL_SECS + 1), "过期即废");
        assert!(!verify_preview_ticket_with(&K1, &ticket, "99999999-2222-3333-4444-555555555555", now), "换文档不许用");
        assert!(!verify_preview_ticket_with(&K2, &ticket, doc, now), "换钥匙（换部署）不许用");
        // 篡改签名（末位 hex 翻转）/ 篡改载荷（坏 base64 / 坏 hex / 空串）全部验假
        let mut bad = ticket.clone();
        let last = bad.pop().unwrap();
        bad.push(if last == '0' { '1' } else { '0' });
        assert!(!verify_preview_ticket_with(&K1, &bad, doc, now), "签名改一个字符也必须响亮失败");
        assert!(!verify_preview_ticket_with(&K1, "不是base64!!.00", doc, now));
        assert!(!verify_preview_ticket_with(&K1, &format!("{payload_b64}.zz"), doc, now));
        assert!(!verify_preview_ticket_with(&K1, "", doc, now));
    }

    /// Range 单区间全形态：a-b / a- / -b / 收敛 / 越界 416 / 多区间与垃圾语法按无 Range 处理
    #[test]
    fn range_parsing_covers_all_forms() {
        let ok = |h: &str, size: u64| parse_range(Some(h), size).and_then(Result::ok);
        assert_eq!(ok("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(ok("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(ok("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(ok("bytes=900-2000", 1000), Some((900, 999)), "尾端越 EOF 收敛到最后一字节");
        assert_eq!(ok("bytes=-5000", 1000), Some((0, 999)), "后缀长于全文件收敛为全量");
        assert_eq!(ok(" bytes=0-0 ", 1000), Some((0, 0)), "首尾空白容忍");
        // 不可满足 → 416
        assert_eq!(parse_range(Some("bytes=1000-"), 1000), Some(Err(())), "起点越 EOF");
        assert_eq!(parse_range(Some("bytes=5-2"), 1000), Some(Err(())), "空区间");
        assert_eq!(parse_range(Some("bytes=-0"), 1000), Some(Err(())), "后缀 0");
        assert_eq!(parse_range(Some("bytes=0-0"), 0), Some(Err(())), "空文件无可服务区间");
        // 忽略（None → 全量 200）：无头 / 多区间 / 垃圾语法 / 非 bytes 单位
        assert_eq!(parse_range(None, 1000), None);
        assert_eq!(parse_range(Some("bytes=0-1,3-4"), 1000), None, "多区间不支持");
        assert_eq!(parse_range(Some("bytes=a-b"), 1000), None);
        assert_eq!(parse_range(Some("items=0-9"), 1000), None);
        assert_eq!(parse_range(Some("bytes=1-2-3"), 1000), None, "多一个横杠不是合法单区间");
    }

    /// Office 预览契约：扩展名白名单恰好 7 个；缓存键带 doc_id + mtime + size 指纹；
    /// 失败出口是 404 + `office_pdf_unavailable`（前端按它降级，绝不许 500）
    #[test]
    fn office_pdf_helpers_pin_contract() {
        for ext in ["doc", "docx", "ppt", "pptx", "xls", "xlsx", "xlsm"] {
            assert!(is_office_ext(std::path::Path::new(&format!("d/{ext}f.{ext}"))), "{ext}");
        }
        for ext in ["pdf", "txt", "png", "csv", "md", "exe", ""] {
            assert!(!is_office_ext(std::path::Path::new(&format!("d/f.{ext}"))), "{ext}");
        }
        // 大小写不敏感（落盘扩展名已是白名单小写，这里钉防御口径）
        assert!(is_office_ext(std::path::Path::new("d/f.DOCX")));

        let p = office_pdf_cache_path(std::path::Path::new("data/kb"), "doc1", 123, 456);
        assert_eq!(p, std::path::Path::new("data/kb/.preview_cache/doc1-123-456-v2.pdf"));
        // 指纹变了键就变（重传/重建自动失效）
        assert_ne!(p, office_pdf_cache_path(std::path::Path::new("data/kb"), "doc1", 124, 456));
        assert_ne!(p, office_pdf_cache_path(std::path::Path::new("data/kb"), "doc1", 123, 457));

        let (code, body) = office_pdf_unavailable();
        assert_eq!(code, StatusCode::NOT_FOUND, "前端靠 404 降级，不许 500");
        assert_eq!(body.0["error"], "office_pdf_unavailable");

        assert_eq!(pdf_display_name("报销制度.docx"), "报销制度.pdf");
        assert_eq!(pdf_display_name("noext"), "noext.pdf");
    }

    /// soffice 缺席/输入非法时转换必须 Err（调用方收敛成 404）——不许 panic、不许 Ok
    #[tokio::test]
    async fn convert_office_to_pdf_fails_closed_on_bad_input() {
        let dir = std::env::temp_dir().join(format!("dms_lo_test_{}", std::process::id()));
        let cache = dir.join(".preview_cache").join("doc1-1-1.pdf");
        // 源文件不存在：canonicalize 直接失败，走不到 spawn——与 soffice 装没装无关，处处稳定
        let r = convert_office_to_pdf(std::path::Path::new("绝不存在的文件.docx"), &cache).await;
        assert!(r.is_err(), "坏输入必须 Err");
        assert!(!cache.exists(), "失败不许留下假缓存");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// 异步化结构断言（源码钉住）：upload 请求内只跑 prepare，重活连许可一起进 spawn；
    /// reprocess 同样不许在请求里同步跑重活
    #[test]
    fn heavy_ingest_work_is_spawned_with_the_permit() {
        let src = include_str!("kb_api.rs");
        let body = src.split("pub async fn upload").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("ingest::prepare("), "upload 请求内只许跑快路径: {body}");
        assert!(body.contains("spawn_ingest_job("), "upload 重活必须后台化: {body}");
        assert!(!body.contains("ingest::run_job("), "upload 请求内不许同步跑重活: {body}");
        // spawn 助手签名必须接收许可（闸门防的内存活在后台任务里）
        let helper = src.split("fn spawn_ingest_job").nth(1).unwrap();
        let helper = helper.split("async fn run_ingest_job_logged").next().unwrap();
        assert!(helper.contains("tokio::spawn"), "重活必须进后台任务: {helper}");
        assert!(helper.contains("permit: tokio::sync::SemaphorePermit"), "许可必须随任务走: {helper}");
        // 复跑端点：prepare 没有它的份（文档行早已在），但重活同样后台化
        let re = src.split("pub async fn reprocess").nth(1).unwrap();
        let re = re.split("pub async fn set_doc_state").next().unwrap();
        assert!(re.contains("spawn_ingest_job(") && !re.contains("ingest::run_job("), "reprocess 重活同样后台化: {re}");
    }

    fn doc_row_fixture() -> DocRow {
        DocRow {
            doc_id: "d1".into(),
            space_id: "kb-hr".into(),
            folder_id: Some("f1".into()),
            folder_path: "/制度".into(),
            name: "制度.pdf".into(),
            mime: "application/pdf".into(),
            bytes: 123,
            sha256: "a".repeat(64),
            status: "embedded".into(),
            enabled: true,
            tags: vec![],
            business_domain: None,
            effective_from: None,
            effective_to: None,
            source_uri: None,
            document_family: None,
            document_revision: None,
            error: String::new(),
            notice: String::new(),
            description: String::new(),
            page_count: 1,
            chunk_count: 3,
            uploaded_by: "zhangsan".into(),
            created_at: "2026-08-01 00:00:00".into(),
            updated_at: "2026-08-01 00:00:00".into(),
        }
    }

    /// 同名覆盖的响应透出（前端上传队列行据 `replaced`/`replaced_doc_name` 显示
    /// 「已覆盖旧版本」）：覆盖命中 → true + 文档名；非覆盖 → false/null，两字段恒在。
    #[test]
    fn upload_response_carries_replaced_flag() {
        let row = doc_row_fixture();
        let hit = upload_doc_json(&row, Some("制度.pdf"));
        assert_eq!(hit["replaced"], serde_json::json!(true));
        assert_eq!(hit["replaced_doc_name"], serde_json::json!("制度.pdf"));
        assert_eq!(hit["doc_id"], serde_json::json!("d1"), "覆盖复用既有 doc_id");
        let fresh = upload_doc_json(&row, None);
        assert_eq!(fresh["replaced"], serde_json::json!(false));
        assert_eq!(fresh["replaced_doc_name"], serde_json::Value::Null);
    }

    /// 同名覆盖的接线合同（源码钉住）：两个入库入口的响应都经 `upload_doc_json` 透出
    /// replaced 标记；覆盖任务的通道②收尾（有表重新登记 / 无表退役清理）在后台任务里
    /// 分派——`cleanup_source` 必须挂在 Overwrite 臂上，Rebuild（自愈会重建表格文档，
    /// 旧数据源原样保留）不许被误挂清理。
    #[test]
    fn overwrite_is_wired_into_upload_responses_and_job_finale() {
        let src = include_str!("kb_api.rs");
        let up = src.split("pub async fn upload").nth(1).unwrap();
        let up = up.split("\n}\n").next().unwrap();
        assert!(up.contains("upload_doc_json(&row, prepared.replaced.as_deref())"), "upload 响应必须透出 replaced: {up}");
        let url = src.split("pub async fn ingest_url").nth(1).unwrap();
        let url = url.split("enum FetchedKind").next().unwrap();
        assert!(url.contains("upload_doc_json(&row, prepared.replaced.as_deref())"), "ingest_url 响应必须透出 replaced: {url}");
        let job = src.split("async fn run_ingest_job_logged").nth(1).unwrap();
        let job = job.split("\n}\n").next().unwrap();
        assert!(job.contains("ingest::IngestJob::Overwrite"), "覆盖任务必须走通道②收尾分派: {job}");
        assert!(job.contains("register_source("), "新内容有表必须重新登记: {job}");
        assert!(job.contains("cleanup_source("), "新内容无表必须退役旧数据源: {job}");
    }

    /// 启动自愈结构断言（源码钉住）：main.rs 挂了自愈 spawn；分派键是分块存在性；
    /// 恢复执行身份是空间 owner（不开 ACL 旁路）；恢复也走上传内存闸
    #[test]
    fn recover_pending_is_wired_and_fail_closed() {
        let main = include_str!("main.rs");
        assert!(main.contains("kb_api::spawn_recover_pending(state.clone())"), "main.rs 必须挂启动自愈");
        for route in [
            ".route(\"/api/kb/doc/{id}/preview-ticket\", post(kb_api::preview_ticket))",
            ".route(\"/api/kb/doc/{id}/file\", get(kb_api::download_doc))",
        ] {
            assert!(main.contains(route), "main.rs 缺路由: {route}");
        }
        let src = include_str!("kb_api.rs");
        let body = src.split("async fn recover_one").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("Viewer::new(d.owner.clone()"), "自愈必须以空间 owner 身份执行: {body}");
        assert!(body.contains("UPLOAD_GATE.acquire()"), "自愈必须走上传内存闸: {body}");
        assert!(body.contains("d.has_chunks"), "自愈必须按分块存在性分派首入/重建: {body}");
        assert!(body.contains("DocStatus::Failed"), "文件没了必须标失败让用户看见: {body}");
    }

    /// 预览票据端点必须与 download 同一条认证 + ACL；票据通道只放行 file 端点
    /// （download_doc 体内必须同时存在票据校验与 doc_for_viewer 两条路）
    #[test]
    fn preview_ticket_endpoint_auths_like_download() {
        let src = include_str!("kb_api.rs");
        let body = src.split("pub async fn preview_ticket").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("viewer("), "票据签发必须过会话认证: {body}");
        assert!(body.contains("acl::doc_for_viewer"), "票据签发必须过 ACL: {body}");
        let dl = src.split("pub async fn download_doc").nth(1).unwrap();
        let dl = dl.split("\n}\n").next().unwrap();
        assert!(dl.contains("verify_preview_ticket"), "file 端点必须支持票据通道: {dl}");
        assert!(dl.contains("acl::doc_for_viewer"), "会话通道的 ACL 不许被票据改造绕掉: {dl}");
        assert!(dl.contains("store::get_doc"), "票据通道只核对文档存在（授权在签发时）: {dl}");
    }

    /// 流式端点契约（源码钉住）：Accept-Ranges 常带；206/416 齐备；inline 与 attachment 分叉；
    /// Office 分支挂在 ACL 之后（`office_pdf_path` 的调用点在 download_doc 体内）
    #[test]
    fn serve_file_contract_is_complete() {
        let src = include_str!("kb_api.rs");
        let body = src.split("async fn serve_file").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("ACCEPT_RANGES"), "必须常带 Accept-Ranges: {body}");
        assert!(body.contains("StatusCode::PARTIAL_CONTENT"), "缺 206: {body}");
        assert!(body.contains("RANGE_NOT_SATISFIABLE"), "缺 416: {body}");
        assert!(body.contains("\"inline\""), "缺 inline 分叉: {body}");
        assert!(body.contains("DOWNLOAD_GATE.try_acquire"), "流式读取不许绕过下载闸: {body}");
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
        // 入库异步化后数据源登记在后台任务里完成（`run_ingest_job_logged` → `register_source`），
        // 不再随上传响应返回——钉住这条链路别被改丢
        let code = include_str!("kb_api.rs");
        let spawn_body = code.split("async fn run_ingest_job_logged").nth(1).unwrap();
        let spawn_body = spawn_body.split("\n}\n").next().unwrap();
        assert!(spawn_body.contains("register_source("), "后台任务必须登记通道②数据源: {spawn_body}");
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

    /// multipart 解析：preset 必须入 `f.q`（上传入口的分块策略不再是死参数）；
    /// 文本字段 trim、全空白按缺省（与「空串按缺省」同口径）
    #[tokio::test]
    async fn read_form_parses_preset_and_trims_text_fields() {
        use axum::extract::FromRequest;
        let body = concat!(
            "--testboundary\r\nContent-Disposition: form-data; name=\"preset\"\r\n\r\nqa\r\n",
            "--testboundary\r\nContent-Disposition: form-data; name=\"space_id\"\r\n\r\n  kb-hr  \r\n",
            "--testboundary\r\nContent-Disposition: form-data; name=\"folder_id\"\r\n\r\n   \r\n",
            "--testboundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n",
            "--testboundary--\r\n",
        );
        let req = axum::http::Request::builder()
            .header(
                axum::http::header::CONTENT_TYPE,
                "multipart/form-data; boundary=testboundary",
            )
            .body(axum::body::Body::from(body))
            .unwrap();
        let mp = Multipart::from_request(req, &()).await.unwrap();
        let form = read_form(mp).await.unwrap();
        assert_eq!(form.q.preset.as_deref(), Some("qa"), "multipart 的 preset 必须入队");
        assert_eq!(form.q.space_id.as_deref(), Some("kb-hr"), "文本字段要 trim");
        assert_eq!(form.q.folder_id, None, "全空白按缺省");
        assert_eq!(
            form.file.as_ref().map(|(n, _, b)| (n.as_str(), b.len())),
            Some(("a.txt", 5))
        );
    }

    /// 下载闸：DOWNLOAD_PERMITS 个许可用完即拒（与上传闸同款的「不排队」语义）
    #[test]
    fn download_gate_rejects_when_full() {
        let held: Vec<_> =
            (0..DOWNLOAD_PERMITS).map(|_| DOWNLOAD_GATE.try_acquire().unwrap()).collect();
        assert!(DOWNLOAD_GATE.try_acquire().is_err());
        drop(held);
        assert!(DOWNLOAD_GATE.try_acquire().is_ok());
    }

    /// reprocess 必须复用上传内存闸 + 读超时（不占许可就绕过了 20MB × N 的防线）
    #[test]
    fn reprocess_shares_upload_gate_and_read_timeout() {
        let src = include_str!("kb_api.rs");
        let body = src.split("pub async fn reprocess").nth(1).unwrap();
        let body = body.split("pub async fn set_doc_state").next().unwrap();
        assert!(body.contains("UPLOAD_GATE.try_acquire"), "reprocess 必须占上传许可");
        assert!(body.contains("UPLOAD_READ_TIMEOUT"), "reprocess 读取必须有超时");
        let permit = body.find("UPLOAD_GATE.try_acquire").unwrap();
        let read = body.find("tokio::fs::read").unwrap();
        assert!(permit < read, "先占许可再读文件: {body}");
    }

    /// 表格文档重处理＝替换语义（裁决：旧数据清理后重建）：409 甩锅文案退役，
    /// 表格走 Overwrite 影子链（知识索引 + 问数表两条通道都随切换重建，失败保留旧版本），
    /// 非表格仍走 Rebuild。结构采集/数据源登记的降级提示统一指路「重新处理」（重传不再是唯一出路）。
    #[test]
    fn reprocess_dispatches_tabular_to_overwrite_chain() {
        let src = include_str!("kb_api.rs");
        assert!(!src.contains(concat!("请直接上传", "新版本")), "甩锅文案必须退役");
        assert!(src.contains("问数结构采集失败，请重新处理"), "降级提示必须指路重处理");
        assert!(src.contains("问数数据源登记失败，请重新处理"), "降级提示必须指路重处理");
        let body = src.split("pub async fn reprocess").nth(1).unwrap();
        let body = body.split("pub async fn set_doc_state").next().unwrap();
        assert!(!body.contains("StatusCode::CONFLICT"), "表格文档不许再被 409 挡在重处理门外: {body}");
        assert!(body.contains("ingest::IngestJob::Overwrite"), "表格重处理必须走覆盖链: {body}");
        assert!(body.contains("ingest::IngestJob::Rebuild"), "非表格重处理仍走影子重建: {body}");
        let tab = body.find("is_tabular").unwrap();
        let job = body.find("ingest::IngestJob::Overwrite").unwrap();
        assert!(tab < job, "必须先判表格再分派覆盖链: {body}");
    }

    /// spaces 读端点：先列后建（常见路径省一次幂等 INSERT）
    #[test]
    fn spaces_lists_before_ensuring_personal_space() {
        let src = include_str!("kb_api.rs");
        let body = src.split("pub async fn spaces").nth(1).unwrap();
        let body = body.split("pub async fn create_space").next().unwrap();
        let list = body.find("store::list_spaces").unwrap();
        let ensure = body.find("store::ensure_space").unwrap();
        assert!(list < ensure, "spaces 必须先列后建: {body}");
    }

    /// 存在性判定用点查：COUNT + fetch_optional 语义错位，角色目录 LIMIT 截断会误判合法角色
    #[test]
    fn existence_checks_use_point_queries() {
        let src = include_str!("kb_api.rs");
        assert!(src.contains("SELECT 1 FROM t_employee WHERE login_name=? AND deleted_flag=0 LIMIT 1"));
        assert!(src.contains("SELECT 1 FROM t_role WHERE TRIM(role_code)=? LIMIT 1"));
        assert!(
            !src.contains(concat!("SELECT COUNT(*) FROM t_employee WHERE login_name", "=?")),
            "碰撞检查不该再全表计数"
        );
    }

    /// ask/search 同族同校验：都过 nonempty_question，且都先认证后校验（401 优先于 400）
    #[test]
    fn ask_and_search_validate_question_after_auth() {
        let src = include_str!("kb_api.rs");
        for (name, next) in
            [("pub async fn search", "pub async fn ask"), ("pub async fn ask", "/// `GET /api/kb/chunk/{id}?window=1` 的 query。")]
        {
            let body = src.split(name).nth(1).unwrap().split(next).next().unwrap();
            let auth = body.find("viewer(").unwrap();
            let check = body
                .find("nonempty_question")
                .unwrap_or_else(|| panic!("{name} 必须过 nonempty_question"));
            assert!(auth < check, "{name} 必须先认证再校验问题: {body}");
        }
    }

    /// span=1 必须走合并跨度回查（落进 window 分支会把「单块」偷偷换成「三块上下文」）
    #[test]
    fn chunk_span_one_uses_span_not_window() {
        let src = include_str!("kb_api.rs");
        let body = src.split("pub async fn chunk").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("Some(n) if n >= 1"), "span=1 不许落 window 分支: {body}");
    }

    /// 缺 name/tags 字段落到友好 400 而非 axum 422（serde default 口径一致）
    #[test]
    fn create_space_and_metadata_req_tolerate_missing_fields() {
        let r: CreateSpaceReq = serde_json::from_str(r#"{"login_name":"zhangsan"}"#).unwrap();
        assert_eq!(r.name, "");
        let m: DocMetadataReq = serde_json::from_str(r#"{"business_domain":"财务"}"#).unwrap();
        assert!(m.tags.is_empty());
    }

    /// update_doc_metadata：仅存在上传源时才同步，且同步失败只 warn（部分失败不报 500）；
    /// set_doc_state 的 500 文案必须带「状态已变更」（否则状态面自相矛盾）
    #[test]
    fn metadata_sync_is_conditional_and_warn_only() {
        let src = include_str!("kb_api.rs");
        let body = src.split("pub async fn update_doc_metadata").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let check = body
            .find("get_datasource")
            .unwrap_or_else(|| panic!("必须先查上传源存在性: {body}"));
        let sync = body.find("sync_source_state").unwrap();
        assert!(check < sync, "先查存在性再同步");
        assert!(body.contains("tracing::warn!"), "同步失败只 warn: {body}");
        assert!(src.contains("文档状态已变更"), "set_doc_state 的同步失败文案变了");
    }

    // ---------------- SSE 流式问答：攒批与帧形态 ----------------

    /// 攒批边界：512 字阈值按**字符**计（中文一字一符，不是字节 —— 按字节会把中文阈值
    /// 悄悄压低到 1/3）。跨多次 push 累计，够线即吐且吐的是**全部**累积。
    #[test]
    fn delta_batcher_char_boundary() {
        let mut b = DeltaBatcher::default();
        assert_eq!(b.flush(), None, "空缓冲冲不出东西");
        // 511 个汉字（1533 字节）不到线：按字节算早该吐了
        let piece = "报".repeat(DELTA_FLUSH_CHARS - 1);
        assert_eq!(b.push(&piece), None);
        assert_eq!(b.push("销"), Some("报".repeat(DELTA_FLUSH_CHARS - 1) + "销"), "第 512 字到线即吐");
        assert_eq!(b.flush(), None, "吐完缓冲已空");
        // 单块超线：整块原样吐出（不在字符中间切）
        let big = "x".repeat(DELTA_FLUSH_CHARS * 2);
        assert_eq!(b.push(&big), Some(big));
        // 残余靠 flush
        assert_eq!(b.push("半截"), None);
        assert_eq!(b.flush(), Some("半截".into()));
        assert_eq!(b.flush(), None, "flush 只吐一次");
    }

    /// 帧载荷形态钉死：单行 JSON（SSE 一帧一 data 行的前提），协议键齐全，
    /// 端点附加键撞名盖不掉协议键。
    #[test]
    fn sse_payload_shapes() {
        let meta = dms_knowledge::answer::AnswerMeta {
            trace_id: "t-1".into(),
            citations: vec![],
            searched_docs: Some(7),
        };
        let mut extra = serde_json::Map::new();
        extra.insert("conv_id".into(), serde_json::json!(42));
        extra.insert("trace_id".into(), serde_json::json!("伪造"),);
        let p = meta_payload(&meta, &extra);
        assert!(!p.contains('\n'), "帧载荷必须单行: {p}");
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["trace_id"], "t-1", "附加键不许盖掉协议键");
        assert_eq!(v["conv_id"], 42);
        assert_eq!(v["searched_docs"], 7);
        assert_eq!(v["citations"], serde_json::json!([]));
        // searched_docs = None（没真正检索）也上线为 null —— 与 Citation.page 的「null 也上线」同款理由：
        // 客户端不猜键缺席的含义
        let meta_none = dms_knowledge::answer::AnswerMeta { trace_id: "t-2".into(), citations: vec![], searched_docs: None };
        let v: serde_json::Value = serde_json::from_str(&meta_payload(&meta_none, &serde_json::Map::new())).unwrap();
        assert!(v.get("searched_docs").is_some_and(serde_json::Value::is_null));

        let d: serde_json::Value = serde_json::from_str(&delta_payload("正文\n增量")).unwrap();
        assert_eq!(d, serde_json::json!({ "text": "正文\n增量" }), "换行进 \\n 转义，不拆帧");
        let e: serde_json::Value = serde_json::from_str(&error_payload("固定文案")).unwrap();
        assert_eq!(e, serde_json::json!({ "message": "固定文案" }));
        assert!(!error_payload("x").contains('\n'));

        let a = dms_kernel::Answer::text("正文[^1]".into(), vec![], 12);
        let done: serde_json::Value = serde_json::from_str(&done_payload(&a)).unwrap();
        assert_eq!(done["answer"]["kind"], "text", "done 的 Answer 与同步端点同 wire");
        assert_eq!(done["answer"]["route"], "knowledge");
        assert_eq!(done["answer"]["markdown"], "正文[^1]");
    }

    /// 事件帧组装：event 名与单行 data。axum `Event` 不带公开读取口，走 Debug 形态钉 —
    /// 钉的是「帧里确实有 event: meta 且 data 单行」，不是 Debug 格式本身。
    #[test]
    fn sse_event_frame_names() {
        for (name, payload) in [
            ("meta", meta_payload(&dms_knowledge::answer::AnswerMeta { trace_id: "t".into(), citations: vec![], searched_docs: None }, &serde_json::Map::new())),
            ("delta", delta_payload("x")),
            ("done", done_payload(&dms_kernel::Answer::text("m".into(), vec![], 1))),
            ("error", error_payload("固定文案")),
        ] {
            assert!(!payload.contains('\n'), "{name} 载荷多行会拆帧: {payload}");
            let frame = format!("{:?}", sse_event(name, payload));
            assert!(frame.contains(name), "{name} 帧缺 event 名: {frame}");
        }
    }

    /// 事件泵端到端：低于阈值的 delta 攒住 → Meta 到达前先冲残余（事件序 = 产生序）→
    /// Done 终止后泵收工。不依赖时间拍（全部走「终止/元事件到达先冲」那条确定性路径）。
    /// 载荷用 ASCII：axum `Event` 的 Debug 对非 ASCII 字节转义，断言要看得懂。
    #[tokio::test]
    async fn pump_flushes_pending_delta_before_meta_and_done() {
        let (wtx, wrx) = tokio::sync::mpsc::unbounded_channel::<SseItem>();
        let (otx, mut orx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(pump_sse(wrx, serde_json::Map::new(), otx));
        wtx.send(SseItem::Delta("part-1".into())).unwrap();
        wtx.send(SseItem::Meta(dms_knowledge::answer::AnswerMeta {
            trace_id: "t-1".into(),
            citations: vec![],
            searched_docs: Some(2),
        }))
        .unwrap();
        wtx.send(SseItem::Delta("part-2".into())).unwrap();
        wtx.send(SseItem::Done(Box::new(dms_kernel::Answer::text("final".into(), vec![], 3)))).unwrap();
        drop(wtx);
        let mut frames = Vec::new();
        while let Some(Ok(ev)) = orx.recv().await {
            frames.push(format!("{ev:?}"));
        }
        assert_eq!(frames.len(), 4, "delta冲帧 + meta + delta冲帧 + done: {frames:?}");
        assert!(frames[0].contains("delta") && frames[0].contains("part-1"), "{}", frames[0]);
        assert!(frames[1].contains("meta") && frames[1].contains("t-1"), "{}", frames[1]);
        assert!(frames[2].contains("part-2"), "{}", frames[2]);
        assert!(frames[3].contains("done") && frames[3].contains("final"), "{}", frames[3]);
    }

    /// 工人通道无终止帧就关闭（panic 路径）：泵冲掉残余 delta 后必须补 error 帧 ——
    /// 客户端拿它走降级，不傻等。
    #[tokio::test]
    async fn pump_emits_error_when_worker_vanishes() {
        let (wtx, wrx) = tokio::sync::mpsc::unbounded_channel::<SseItem>();
        let (otx, mut orx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(pump_sse(wrx, serde_json::Map::new(), otx));
        wtx.send(SseItem::Delta("partial".into())).unwrap();
        drop(wtx); // 工人没了
        let mut frames = Vec::new();
        while let Some(Ok(ev)) = orx.recv().await {
            frames.push(format!("{ev:?}"));
        }
        assert_eq!(frames.len(), 2, "残余 delta + error: {frames:?}");
        assert!(frames[0].contains("partial"), "{}", frames[0]);
        // error 帧必到（文案本身由 sse_payload_shapes 钉；Event Debug 对中文转义，这里只认事件名）
        assert!(frames[1].contains("error"), "{}", frames[1]);
    }
}
