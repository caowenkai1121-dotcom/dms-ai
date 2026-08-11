//! 【S1】Agent 产物（artifact）预览地基：**产物与 BI 日报共用一张表**（`meta.artifact`）。
//!
//! 形态照 datanote（Claude Artifacts / Codex 式）：
//! - 服务端把 markdown/数据渲染成**整页精美 HTML** 存表，前端只在 iframe 沙箱里展示 ——
//!   渲染逻辑全部在服务端（前端零渲染信任），CSP sandbox + iframe
//!   `sandbox="allow-scripts"`（无 allow-same-origin ⇒ 不透明源，碰不到父页/Cookie）双隔离；
//!   报表图形使用无脚本 inline SVG，CSP 继续以 `script-src 'none'` 收口历史 HTML。
//! - `/view` 给人看（沙箱页），`/download` 给文件（附件）。产物页**不含**任何凭据是铁律
//!   （它是会分享的 HTML —— 与「口径注明来源」同一条信任边界）。
//!
//! 归属：全局日报 `conv_id=''`，预览、下载、分享和 feed 均重新校验 DMS 管理员；
//! 用户周报和其他会话产物按 conv 归属（**非属主禁看**，与 `api_ask` 同一条）。
//!
//! 【D6 产物层增强】版本链 + 表格导出 + 跨会话引用（promote）：
//! - 版本链：同 conv 同 (kind,title) 的 save 版本自增，老版本保留（迁移在 semantic/ddl.rs）。
//!   `GET /api/artifact/{id}/versions` 列链；`view/download/export` 接受 `?version=N` 回看单版本。
//! - 导出：`GET /api/artifact/{id}/export?fmt=csv|xlsx` 把产物页里的 `<table>` 导成文件。
//!   xlsx 是手写最小 ZIP+SpreadsheetML（零新增第三方依赖是架构硬规则，不许引 crate）。
//! - 引用：`POST /api/artifact/{id}/promote` 把产物引用钉进**自己**的另一会话
//!   （写口复用 `chat::save_msg`，事件形态 `payload.kind='artifact_promote'`，前端回放渲染成产物卡）。
//!
//! 🔴 接线契约（main.rs 持有路由表，本文件不动它）——新增三条：
//!   `.route("/api/artifact/{id}/versions", get(artifact_api::versions))`
//!   `.route("/api/artifact/{id}/export", get(artifact_api::export))`
//!   `.route("/api/artifact/{id}/promote", post(artifact_api::promote))`
//! `view/download` 的 `?version=N` 复用既有路由，无需新行。

use std::fmt::Write as _;
use std::sync::{Arc, LazyLock};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    Json,
};

use crate::AppState;

type ApiErr = (StatusCode, Json<serde_json::Value>);
type ApiRes = Result<Json<serde_json::Value>, ApiErr>;

/// 沿用 admin_api 的 `{"error": msg}` 形状（前端只认这一种）
fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// 驱动错误可能带主机、库名或 SQL，产物接口一律只返回固定文案 —— 真因只进服务端日志
///（此前全文件零日志：DB 故障对外 500「请稍后重试」，运维侧没有任何线索）。
fn db_err(e: impl std::fmt::Display) -> ApiErr {
    tracing::warn!(err = %e, "artifact DB 操作失败");
    err(StatusCode::INTERNAL_SERVER_ERROR, "产物操作失败，请稍后重试")
}

/// 400 文案回显用户入参的上限（kind/fmt/feed 都是无长度上限的用户字符串）
fn clipped_echo(s: &str) -> String {
    s.chars().take(64).collect()
}

/// 管理员产物不接受兼容模式下的 `login_name` 回退，必须持有服务端签发的会话。
fn require_bearer_session(h: &HeaderMap) -> Result<(), ApiErr> {
    let valid = h
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(crate::auth::resolve)
        .is_some();
    if valid {
        Ok(())
    } else {
        Err(err(StatusCode::UNAUTHORIZED, "该产物需要有效会话 token"))
    }
}

const ARTIFACT_CSP: &str = "sandbox allow-scripts; default-src 'none'; style-src 'unsafe-inline'; script-src 'none'; img-src data:; font-src data:; connect-src 'none'; media-src 'none'; object-src 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; manifest-src 'none'; base-uri 'none'; form-action 'none'; navigate-to 'none'";
const CSP_META_MARKER: &str = "data-dms-artifact-csp";

/// 【D6 版本链】版本自增在库内单条语句完成（MAX(version)+1），唯一索引
/// `idx_artifact_chain` 兜底并发撞号（撞号 = 第二次写入报错、由调用方重试，不产生重复版本）。
const INSERT_SQL: &str =
    "INSERT INTO meta.artifact(conv_id, kind, title, html, created_by, version) \
     VALUES ($1,$2,$3,$4,$5,(SELECT COALESCE(MAX(version),0)+1 FROM meta.artifact \
       WHERE conv_id=$1 AND kind=$2 AND title=$3)) RETURNING id, version";
const GET_SQL: &str =
    "SELECT id, conv_id, kind, title, html, created_by, version FROM meta.artifact WHERE id = $1";
/// 版本链：链键 = (conv_id, kind, title)。单版本回看与链列表共用这组谓词。
const GET_VERSION_SQL: &str =
    "SELECT id, conv_id, kind, title, html, created_by, version FROM meta.artifact \
     WHERE conv_id = $1 AND kind = $2 AND title = $3 AND version = $4";
const CHAIN_SQL: &str =
    "SELECT id, version, created_at::text FROM meta.artifact \
     WHERE conv_id = $1 AND kind = $2 AND title = $3 ORDER BY version DESC, id DESC LIMIT 100";
const LIST_SQL: &str = "SELECT id, kind, title, created_at::text FROM meta.artifact \
     WHERE conv_id = $1 AND (conv_id <> '' OR created_by = $2) \
       AND created_by NOT IN ('daily-digest','weekly-digest') \
     ORDER BY id DESC LIMIT $3";
const DAILY_FEED_SQL: &str = "SELECT id, kind, title, created_at::text FROM meta.artifact \
     WHERE conv_id = '' AND created_by = 'daily-digest' \
       AND (created_at AT TIME ZONE 'Asia/Shanghai')::date = \
           (CURRENT_TIMESTAMP AT TIME ZONE 'Asia/Shanghai')::date \
     ORDER BY id DESC LIMIT 1";
const WEEKLY_FEED_SQL: &str = "SELECT a.id, a.kind, a.title, a.created_at::text FROM meta.artifact a \
     JOIN chat.conv c ON c.id::text = a.conv_id AND c.login_name = $1 \
     WHERE a.conv_id <> '' AND a.created_by = $1 AND a.kind = 'report' \
       AND date_trunc('week', a.created_at AT TIME ZONE 'Asia/Shanghai') = \
           date_trunc('week', CURRENT_TIMESTAMP AT TIME ZONE 'Asia/Shanghai') \
       AND a.title ~ '经营周报（[0-9]{4}-[0-9]{2}-[0-9]{2} 至 [0-9]{4}-[0-9]{2}-[0-9]{2}）$' \
     ORDER BY a.id DESC LIMIT 1";

/// 供后续阶段（分析报表/日报）共用的写入点。返回产物 id。
/// 签名保持不变（deep_api / daily_digest 的直接调用不受影响）；需要版本号的走 `save_artifact_versioned`。
pub async fn save_artifact(
    st: &AppState, conv_id: &str, kind: &str, title: &str, html: &str, created_by: &str,
) -> anyhow::Result<i64> {
    Ok(save_artifact_versioned(st, conv_id, kind, title, html, created_by).await?.0)
}

/// 【D6】写入并返回 (id, 版本号)：同 conv 同 (kind,title) 重生成时版本自增，老版本保留。
pub async fn save_artifact_versioned(
    st: &AppState, conv_id: &str, kind: &str, title: &str, html: &str, created_by: &str,
) -> anyhow::Result<(i64, i32)> {
    let title = secure_artifact_title(title);
    let html = secure_artifact_html(html);
    let row: Option<(i64, i32)> = st.owned.fixed(INSERT_SQL)
        .bind(conv_id).bind(kind).bind(&title).bind(&html).bind(created_by)
        .fetch_optional().await.map_err(|_| anyhow::anyhow!("产物写入失败"))?;
    row.ok_or_else(|| anyhow::anyhow!("产物写入未返回 id"))
}

#[derive(serde::Deserialize)]
pub struct CreateReq {
    kind: Option<String>,
    title: Option<String>,
    /// markdown 正文（`kind=markdown`）或整页 HTML（`kind=html`，入库前统一安全收口）
    content: String,
    conv_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/artifact` —— 手工/内部造物（admin_only；分析与日报不走这里，直接调 `save_artifact`）。
pub async fn create(
    State(st): State<Arc<AppState>>, h: HeaderMap, Json(mut req): Json<CreateReq>,
) -> ApiRes {
    require_bearer_session(&h)?;
    let p = crate::admin_api::admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    // title/kind/conv_id 先 take 出来：html 分支要 move content，借用序先清场
    let title = req.title.take().unwrap_or_else(|| "分析报告".to_string());
    let kind = req.kind.take().unwrap_or_else(|| "markdown".to_string());
    let conv_id = req.conv_id.take().unwrap_or_default();
    if !conv_id.is_empty() {
        let cid: i64 = conv_id
            .parse()
            .map_err(|_| err(StatusCode::BAD_REQUEST, "conv_id 必须是会话主键数字"))?;
        let owner = crate::chat::conv_owner(st.owned.pool(), cid)
            .await
            .map_err(db_err)?;
        if owner.as_deref() != Some(p.login_name.as_str()) {
            return Err(err(StatusCode::FORBIDDEN, "无权向该会话写入产物"));
        }
    }
    let html = match kind.as_str() {
        "markdown" | "report" => page_shell(&title, &md_to_html(&req.content)),
        "html" => std::mem::take(&mut req.content), // 整页 HTML 直接 move，不再整页克隆
        other => return Err(err(StatusCode::BAD_REQUEST, format!("kind 只能是 markdown|report|html：{}", clipped_echo(other)))),
    };
    let (id, version) = save_artifact_versioned(&st, &conv_id, &kind, &title, &html, &p.login_name)
        .await
        .map_err(db_err)?;
    Ok(Json(serde_json::json!({
        "id": id,
        "version": version,
        "preview_url": format!("/api/artifact/{id}/view"),
        "download_url": format!("/api/artifact/{id}/download"),
    })))
}

#[derive(serde::Deserialize, Default)]
pub struct ViewQuery {
    login_name: Option<String>,
    role_code: Option<String>,
    /// 【D6】回看链上指定版本；缺省 = 该 id 自身那一版（行为与历史完全一致）。
    version: Option<i32>,
}

struct Row {
    id: i64,
    conv_id: String,
    kind: String,
    title: String,
    html: String,
    created_by: String,
    version: i32,
}

/// GET_SQL / GET_VERSION_SQL 的行 → Row（两条 SQL 的列序必须保持一致）。
fn map_row(r: (i64, String, String, String, String, String, i32)) -> Row {
    Row { id: r.0, conv_id: r.1, kind: r.2, title: r.3, html: r.4, created_by: r.5, version: r.6 }
}

/// 读取并校验「这个 id 本身」（share/unshare/versions 用；不做版本解析）。
async fn load(st: &AppState, h: &HeaderMap, q: &ViewQuery, id: i64) -> Result<Row, ApiErr> {
    Ok(load_versioned(st, h, q, id, None).await?.0)
}

/// 归属校验 + 【D6】链内版本解析，返回 (行, 当前登录人)（promote 要用登录人核目标会话属主）。
/// `version` 只允许落在同一条 (conv_id,kind,title) 链上 —— 跨不进别人的产物；
/// 解析出的版本行**重新过一遍**权限判据（链上 created_by 可能不同，fail-closed）。
async fn load_versioned(
    st: &AppState, h: &HeaderMap, q: &ViewQuery, id: i64, version: Option<i32>,
) -> Result<(Row, String), ApiErr> {
    // 归属校验：会话产物非属主禁看；后台日报是全量管理视角，只允许 DMS 管理员。
    require_bearer_session(h)?;
    let (login, _role) = crate::resolve_identity(st, h, &q.login_name, &q.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let base: Option<(i64, String, String, String, String, String, i32)> =
        st.owned.fixed(GET_SQL).bind(id).fetch_optional().await
            .map_err(db_err)?;
    let Some(base) = base else {
        return Err(err(StatusCode::NOT_FOUND, "产物不存在"));
    };
    let base = map_row(base);
    let row = match version {
        None => base,
        Some(v) if v < 1 => return Err(err(StatusCode::BAD_REQUEST, "version 必须从 1 开始")),
        Some(v) => {
            let r: Option<(i64, String, String, String, String, String, i32)> = st
                .owned
                .fixed(GET_VERSION_SQL)
                .bind(&base.conv_id)
                .bind(&base.kind)
                .bind(&base.title)
                .bind(v)
                .fetch_optional()
                .await
                .map_err(db_err)?;
            let Some(r) = r else {
                return Err(err(StatusCode::NOT_FOUND, format!("该产物链没有版本 v{v}")));
            };
            map_row(r)
        }
    };
    check_perm(st, h, q, &row, &login).await?;
    Ok((row, login))
}

/// 产物访问判据单点收口（fail-closed）：digest 重新校验管理员；会话产物非属主禁看；
/// 无会话产物只认创建者本人（空会话桶不是公共区）。
async fn check_perm(
    st: &AppState, h: &HeaderMap, q: &ViewQuery, row: &Row, login: &str,
) -> Result<(), ApiErr> {
    if matches!(row.created_by.as_str(), "daily-digest" | "weekly-digest") {
        let _ = crate::admin_api::admin_only(st, h, (&q.login_name, &q.role_code)).await?;
    }
    if !row.conv_id.is_empty() {
        // conv_id 存的是会话主键的文本形；解析不了就不认这个归属（按「不是你的」处理）
        let cid: i64 = row
            .conv_id
            .parse()
            .map_err(|_| {
                // 解析失败 = 库里数据异常：留痕再拒（数据腐化不该静默 403）
                tracing::warn!(artifact_id = row.id, conv_id = %row.conv_id, "产物 conv_id 无法解析为会话主键");
                err(StatusCode::FORBIDDEN, "产物归属异常，禁止访问")
            })?;
        let owner = crate::chat::conv_owner(st.owned.pool(), cid).await
            .map_err(db_err)?;
        if owner.as_deref() != Some(login) {
            return Err(err(StatusCode::FORBIDDEN, "无权访问该产物"));
        }
    } else if !matches!(row.created_by.as_str(), "daily-digest" | "weekly-digest")
        && row.created_by != login
    {
        // 无会话产物没有 conv_owner 可核验，只能由创建者本人访问；禁止把空会话桶当公共区。
        return Err(err(StatusCode::FORBIDDEN, "无权访问该产物"));
    }
    Ok(())
}

/// 沙箱响应头（双隔离的一半）：无 `allow-same-origin`；图表是无脚本 SVG，
/// `script-src 'none'` 继续阻断历史产物里的脚本。产物始终读不到父页、Cookie、localStorage。
fn sandbox_headers(content_type: &'static str) -> HeaderMap {
    // 七个头里六个全静态：底图只建一次，每次响应克隆一份再补 CONTENT_TYPE
    static BASE: LazyLock<HeaderMap> = LazyLock::new(|| {
        let mut h = HeaderMap::new();
        h.insert(
            header::HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static(ARTIFACT_CSP),
        );
        h.insert(
            header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        );
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store, max-age=0"));
        h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        h.insert(
            header::HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        );
        h.insert(
            header::HeaderName::from_static("x-robots-tag"),
            HeaderValue::from_static("noindex, nofollow, noarchive"),
        );
        h
    });
    let mut h = BASE.clone();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h
}

/// 【D6】按扩展名出文件名（下载/导出共用同一套清洗）。
fn download_name_ext(title: &str, ext: &str) -> String {
    let title = title
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ' ') { ch } else { '_' })
        .collect::<String>();
    let title = title
        .trim_matches(|ch| matches!(ch, '_' | ' '))
        .chars()
        .take(60)
        .collect::<String>();
    if title.is_empty() { format!("artifact.{ext}") } else { format!("{title}.{ext}") }
}

/// RFC5987 `filename*`：非 ASCII 全转义（中文标题靠这条进 Content-Disposition）。
fn encoded_download_name_ext(title: &str, ext: &str) -> String {
    let mut out = String::new();
    // 分段迭代 title/ext 字节（不先拼 "{title}.{ext}" 整串）；转义 write! 直写缓冲
    for byte in title.bytes().chain([b'.']).chain(ext.bytes()) {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Content-Disposition 附件头（download/export 共用；值非法时降级为裸 attachment）。
fn attachment_headers(content_type: &'static str, title: &str, ext: &str) -> HeaderMap {
    let mut h = sandbox_headers(content_type);
    let name = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        download_name_ext(title, ext),
        encoded_download_name_ext(title, ext)
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&name).unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    h
}

/// `GET /api/artifact/{id}/view` —— 沙箱预览页
pub async fn view(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<i64>, Query(q): Query<ViewQuery>,
) -> Result<(HeaderMap, String), ApiErr> {
    let (row, _) = load_versioned(&st, &h, &q, id, q.version).await?;
    Ok((sandbox_headers("text/html; charset=utf-8"), secure_artifact_html(&row.html)))
}

/// `GET /api/artifact/{id}/download` —— 附件下载
pub async fn download(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<i64>, Query(q): Query<ViewQuery>,
) -> Result<(HeaderMap, String), ApiErr> {
    let (row, _) = load_versioned(&st, &h, &q, id, q.version).await?;
    let title = secure_artifact_title(&row.title);
    Ok((attachment_headers("text/html; charset=utf-8", &title, "html"), secure_artifact_html(&row.html)))
}

// ─────────────────── 【D6】版本链 / 表格导出 / 跨会话引用（promote） ───────────────────

/// `GET /api/artifact/{id}/versions` —— 版本链列表（属主判据复用 load；链 = 同 conv 同 kind 同 title）。
pub async fn versions(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<i64>, Query(q): Query<ViewQuery>,
) -> ApiRes {
    let row = load(&st, &h, &q, id).await?;
    let chain: Vec<(i64, i32, String)> = st
        .owned
        .fixed(CHAIN_SQL)
        .bind(&row.conv_id)
        .bind(&row.kind)
        .bind(&row.title)
        .fetch_all()
        .await
        .map_err(db_err)?;
    let items: Vec<serde_json::Value> = chain
        .into_iter()
        .enumerate()
        .map(|(i, (vid, v, at))| serde_json::json!({
            "id": vid,
            "version": v,
            "created_at": at,
            // ORDER BY version DESC：首条即链上最新版（回看的锚点）
            "latest": i == 0,
            "view_url": format!("/api/artifact/{vid}/view"),
        }))
        .collect();
    Ok(Json(serde_json::json!({ "versions": items })))
}

#[derive(serde::Deserialize, Default)]
pub struct ExportQuery {
    /// csv | xlsx（缺省 csv）
    fmt: Option<String>,
    /// 导出链上指定版本；缺省 = 该 id 自身那一版
    version: Option<i32>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `GET /api/artifact/{id}/export?fmt=csv|xlsx[&version=N]` —— 把产物页里的 `<table>` 导成文件。
/// 与 view 同一条信任边界：先过 `secure_artifact_html` 再解析表格（内部段不进导出件）。
pub async fn export(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<i64>, Query(q): Query<ExportQuery>,
) -> Result<(HeaderMap, Vec<u8>), ApiErr> {
    // version 走独立参数（load_versioned 只读参数版，塞进 ViewQuery 是死赋值）
    let vq = ViewQuery {
        login_name: q.login_name.clone(),
        role_code: q.role_code.clone(),
        ..Default::default()
    };
    let (row, _) = load_versioned(&st, &h, &vq, id, q.version).await?;
    let tables = extract_tables(&secure_artifact_html(&row.html));
    if tables.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "该产物不含表格，无法导出"));
    }
    let title = secure_artifact_title(&row.title);
    // fmt 大小写不敏感（低代码平台常传大写 CSV/XLSX）
    let fmt = q.fmt.as_deref().unwrap_or("csv").to_ascii_lowercase();
    match fmt.as_str() {
        "csv" => Ok((
            attachment_headers("text/csv; charset=utf-8", &title, "csv"),
            to_csv(&tables).into_bytes(),
        )),
        "xlsx" => Ok((
            attachment_headers(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                &title,
                "xlsx",
            ),
            build_xlsx(&tables),
        )),
        other => Err(err(StatusCode::BAD_REQUEST, format!("fmt 只能是 csv|xlsx：{}", clipped_echo(other)))),
    }
}

#[derive(serde::Deserialize)]
pub struct PromoteReq {
    /// 目标会话主键（**只许是自己的会话**）
    target_conv_id: i64,
    /// 引用链上指定版本；缺省 = 该 id 自身那一版
    version: Option<i32>,
    /// 钉过去时附的一句说明（可选；剥控制字符、限长 —— 它要进会话历史）
    note: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/artifact/{id}/promote` —— 把产物引用钉进自己的另一会话。
/// 事件形态：`chat.msg` 一条 `role='ai'` 的消息，`payload.kind='artifact_promote'`
/// （写口复用 `chat::save_msg` 既有公开函数；前端回放按 kind 渲染成产物卡）。
/// payload 不带 `sql` 键 ⇒ 多轮追问改写把它当「知识库轮」形态跳过，不会污染上一轮 SQL。
pub async fn promote(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<i64>, Json(req): Json<PromoteReq>,
) -> ApiRes {
    // version 走独立参数（同上：塞 ViewQuery 是死赋值）
    let q = ViewQuery {
        login_name: req.login_name.clone(),
        role_code: req.role_code.clone(),
        ..Default::default()
    };
    // ① 产物读权限：属主/管理员判据全部复用 load_versioned（读不到产物的人更不许引用它）
    let (row, login) = load_versioned(&st, &h, &q, id, req.version).await?;
    // ② 目标会话属主校验（fail-closed：查不到属主 = 不是你的会话）
    let owner = crate::chat::conv_owner(st.owned.pool(), req.target_conv_id)
        .await
        .map_err(db_err)?;
    if owner.as_deref() != Some(login.as_str()) {
        return Err(err(StatusCode::FORBIDDEN, "只能引用到自己的会话"));
    }
    let note = req.note.as_deref().map(sanitize_promote_note).filter(|n| !n.is_empty());
    let title = secure_artifact_title(&row.title);
    let payload = serde_json::json!({
        "kind": "artifact_promote",
        "artifact_id": row.id,
        "version": row.version,
        "title": title,
        "preview_url": format!("/api/artifact/{}/view", row.id),
        "from_conv_id": row.conv_id,
        "note": note,
        "promoted_by": login,
    });
    crate::chat::save_msg(st.owned.pool(), req.target_conv_id, "ai", "", Some(&payload))
        .await
        .map_err(db_err)?;
    // 安全敏感端点的成功路径必须有审计轨迹（id、操作人、目标 conv）
    tracing::info!(artifact_id = row.id, operator = %login, target_conv = req.target_conv_id, "产物引用已钉入会话");
    Ok(Json(serde_json::json!({ "ok": true, "conv_id": req.target_conv_id })))
}

/// promote 备注清洗：控制字符换空格（防换行粘连成新词，与 secure_artifact_title 同一先例）、
/// 按字符数限长（CJK 不按字节切）。
fn sanitize_promote_note(note: &str) -> String {
    note.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .chars()
        .take(200)
        .collect()
}

// ── 表格抽取（导出用）：产物页是自渲染 HTML，解析面只认 <table>/<tr>/<th>/<td> ──

/// 导出行数护栏：产物是自渲染内容，理论体量小；上限防的是异常输入把导出拖死。
const MAX_EXPORT_ROWS: usize = 20_000;
/// 单元格总数护栏：行数护栏挡不住「2 万行 × 超宽表」在内存里拼出巨型 CSV/XML
const MAX_EXPORT_CELLS: usize = 200_000;

/// 从 HTML 抽出所有表格（表 → 行 → 单元格文本）。`<th>` 行天然就是表头行。
fn extract_tables(html: &str) -> Vec<Vec<Vec<String>>> {
    let mut tables = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    let mut rows_left = MAX_EXPORT_ROWS;
    let mut cells_left = MAX_EXPORT_CELLS;
    while rows_left > 0 && cells_left > 0 {
        let Some(relative) = lower[cursor..].find("<table") else { break };
        let start = cursor + relative;
        // 标签边界闸：`<tablex` 伪标签不算表格起点（与 extract_cells 同一先例）
        let boundary = lower[start + 6..].chars().next();
        if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
            cursor = start + 6;
            continue;
        }
        let Some(open_end_rel) = lower[start..].find('>') else { break };
        let open_end = start + open_end_rel + 1;
        let Some(end) = matching_element_end(&lower, "table", open_end) else { break };
        let rows = extract_rows(&html[start..end], &mut rows_left, &mut cells_left);
        if !rows.is_empty() {
            tables.push(rows);
        }
        cursor = end;
    }
    tables
}

fn extract_rows(table: &str, rows_left: &mut usize, cells_left: &mut usize) -> Vec<Vec<String>> {
    let lower = table.to_ascii_lowercase();
    let mut rows = Vec::new();
    let mut cursor = 0;
    while *rows_left > 0 && *cells_left > 0 {
        let Some(relative) = lower[cursor..].find("<tr") else { break };
        let start = cursor + relative;
        // 标签边界闸：`<trxyz` 不算行起点（与 extract_cells 同一先例）
        let boundary = lower[start + 3..].chars().next();
        if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
            cursor = start + 3;
            continue;
        }
        let Some(end_relative) = lower[start..].find("</tr>") else { break };
        let end = start + end_relative + "</tr>".len();
        let cells = extract_cells(&table[start..end]);
        if !cells.is_empty() {
            *cells_left = (*cells_left).saturating_sub(cells.len());
            rows.push(cells);
            *rows_left -= 1;
        }
        cursor = end;
    }
    rows
}

/// 一行里的 `<th>`/`<td>` 文本（按出现顺序）。
fn extract_cells(row: &str) -> Vec<String> {
    let lower = row.to_ascii_lowercase();
    let mut cells = Vec::new();
    let mut cursor = 0;
    loop {
        let th = lower[cursor..].find("<th").map(|offset| cursor + offset);
        let td = lower[cursor..].find("<td").map(|offset| cursor + offset);
        let (start, tag) = match (th, td) {
            (Some(a), Some(b)) => {
                if a <= b { (a, "th") } else { (b, "td") }
            }
            (Some(a), None) => (a, "th"),
            (None, Some(b)) => (b, "td"),
            (None, None) => break,
        };
        // 标签边界闸：`<tdx` 之类不算（与同文件 remove_elements_by_tag 同一先例）
        // `<th`/`<td` 都是 3 字节，边界字符在 start+3
        let boundary = lower[start + 3..].chars().next();
        if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
            cursor = start + 2;
            continue;
        }
        let Some(open_end_rel) = lower[start..].find('>') else { break };
        let content_start = start + open_end_rel + 1;
        let close = format!("</{tag}>");
        let Some(close_rel) = lower[content_start..].find(&close) else { break };
        cells.push(cell_text(&row[content_start..content_start + close_rel]));
        cursor = content_start + close_rel + close.len();
    }
    cells
}

/// 单元格 → 纯文本：剥标签、解最小实体集、折叠空白（CSV 单元格不内嵌换行）。
fn cell_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        match rest[i..].find('>') {
            Some(j) => {
                // <br>/<br/> 是换行语义：剥掉前留一个空白，否则 "a<br>b" 导出成 "ab" 粘连
                if rest[i + 1..i + j].trim_end_matches(['/', ' ', '\t']).eq_ignore_ascii_case("br") {
                    out.push(' ');
                }
                rest = &rest[i + j + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    decode_entities(&out).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 最小实体集：自渲染链路（escape/display_value）只会产出这几个。
/// 单趟扫描 `&` 起跳一次解码 —— 左到右匹配天然等价于「`&amp;` 最后解」
///（`&amp;lt;` → 先命中 `&amp;` 产 `&`，余下 `lt;` 原样 = 旧实现结果），不再串 7 次全文 replace。
fn decode_entities(s: &str) -> String {
    const ENTITIES: &[(&str, &str)] = &[
        ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"),
        ("&apos;", "'"), ("&nbsp;", " "), ("&amp;", "&"),
    ];
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        match ENTITIES.iter().find(|(e, _)| tail.starts_with(e)) {
            Some((e, r)) => {
                out.push_str(r);
                rest = &tail[e.len()..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ── CSV：形状对齐前端 downloadCsv（全量加引号、CRLF、BOM），另加公式注入护栏 ──

/// 公式注入护栏（OWASP CSV Injection）：以 `=` `+` `-` `@` 或制表/回车开头的单元格，
/// Excel/表格软件会当公式求值 —— 前置 `'` 强制按文本显示。
fn csv_defang(cell: &str) -> String {
    match cell.chars().next() {
        Some('=') | Some('+') | Some('-') | Some('@') | Some('\t') | Some('\r') => {
            format!("'{cell}")
        }
        _ => cell.to_string(),
    }
}

fn csv_cell(cell: &str) -> String {
    format!("\"{}\"", csv_defang(cell).replace('"', "\"\""))
}

fn to_csv(tables: &[Vec<Vec<String>>]) -> String {
    // BOM：Excel 双击打开 UTF-8 中文不乱码（与前端 downloadCsv 同一形状）
    let mut out = String::from("\u{feff}");
    for (i, table) in tables.iter().enumerate() {
        if i > 0 {
            out.push_str("\r\n"); // 多表之间空一行
        }
        for row in table {
            let line: Vec<String> = row.iter().map(|cell| csv_cell(cell)).collect();
            out.push_str(&line.join(","));
            out.push_str("\r\n");
        }
    }
    out
}

// ── xlsx：手写最小 ZIP(stored) + SpreadsheetML（零新增第三方依赖是架构硬规则）──
// 全部单元格用 inlineStr —— 结构性免疫公式注入（`<f>` 才是公式），也省掉 sharedStrings 部件。

/// CRC-32 (IEEE)。ZIP 本地头与中央目录都要它；逐位实现换零依赖（导出体量小，性能无关紧要）。
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// 列号 → Excel 列标（1→A, 26→Z, 27→AA）。`n=0` 会返回空串 —— 契约是「从 1 起」，
/// 调用方恒传 `ci+1`，debug_assert 把契约钉在函数里而不是靠调用方自觉。
fn col_letter(mut n: usize) -> String {
    debug_assert!(n > 0, "列号从 1 起");
    let mut s = String::new();
    while n > 0 {
        n -= 1;
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        n /= 26;
    }
    s
}

/// XML 文本转义 + 剥 XML 1.0 非法控制字符（保留 \t \n \r）。
fn xml_text(s: &str) -> String {
    let cleaned: String =
        s.chars().filter(|ch| !ch.is_control() || matches!(ch, '\t' | '\n' | '\r')).collect();
    cleaned.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn worksheet_xml(tables: &[Vec<Vec<String>>]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
    );
    let mut row_no = 0usize;
    for (i, table) in tables.iter().enumerate() {
        if i > 0 {
            row_no += 1; // 表间空行：row 元素允许跳行号，不必写空 row
        }
        for row in table {
            row_no += 1;
            // write! 直写缓冲：每行每格 format! 出临时 String 是数万次白分配
            let _ = write!(out, "<row r=\"{row_no}\">");
            for (ci, cell) in row.iter().enumerate() {
                let _ = write!(
                    out,
                    "<c r=\"{}{}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                    col_letter(ci + 1),
                    row_no,
                    xml_text(cell)
                );
            }
            out.push_str("</row>");
        }
    }
    out.push_str("</sheetData></worksheet>");
    out
}

/// 最小 ZIP（stored，不压缩）：本地头 + 中央目录 + EOCD。内容全在内存，尺寸/CRC 先算后写。
fn zip_stored(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    // ZIP32 字段上限断言：`as u32`/`as u16` 是静默截断 —— 单部件 >4GiB、部件名 >65535、
    // 偏移 >4GiB 时 ZIP 头会写错值而无任何报错。内容全在内存（KB~MB 级），撞上即实现 bug，
    // panic 好过产出错文件。
    assert!(parts.len() <= u16::MAX as usize, "ZIP 部件数超 65535");
    for (name, data) in parts {
        assert!(out.len() <= u32::MAX as usize, "ZIP 本地偏移超 4GiB");
        assert!(data.len() <= u32::MAX as usize, "ZIP 单部件超 4GiB");
        assert!(name.len() <= u16::MAX as usize, "ZIP 部件名超 65535 字节");
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let name_bytes = name.as_bytes();
        // 本地文件头
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method = stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);
        // 中央目录项
        central.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }
    assert!(out.len() <= u32::MAX as usize && central.len() <= u32::MAX as usize, "ZIP 目录偏移超 4GiB");
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    // EOCD（签名 0x06054B50，磁盘字节形 "PK\x05\x06"）
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn build_xlsx(tables: &[Vec<Vec<String>>]) -> Vec<u8> {
    const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
        <Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>\
        </Types>";
    const RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
        </Relationships>";
    const WORKBOOK: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
        xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
        <sheets><sheet name=\"导出\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
    const WORKBOOK_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>\
        </Relationships>";
    let sheet = worksheet_xml(tables);
    zip_stored(&[
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
        ("_rels/.rels", RELS.as_bytes()),
        ("xl/workbook.xml", WORKBOOK.as_bytes()),
        ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

// ─────────────────────────── 【分享】token 即能力 ───────────────────────────
// 形状：uuid 链接，持链接者免登录只读；**只授读，不授写**。归属校验只发生在
// 「发链接」那一刻（load() 的属主校验），之后持链接 = 持证。撤销 = 清空 token。

const SHARE_CLEAR_SQL: &str = "UPDATE meta.artifact SET share_token = '' WHERE id = $1";
const SHARE_GET_SQL: &str =
    "SELECT html FROM meta.artifact WHERE share_token = $1 AND share_token <> ''";

/// `POST /api/artifact/{id}/share` —— 发链接（属主；已有 token 原样返回，不轮换）。
pub async fn share(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<i64>, Query(q): Query<ViewQuery>,
) -> ApiRes {
    // 属主校验复用 load_versioned（非属主连产物都读不到，更不许发链接）
    let (_, login) = load_versioned(&st, &h, &q, id, None).await?;
    // 已有 token 时（CASE 保留旧值）这个 uuid 白生成 —— 接受的取舍：uuid 生成是纳秒级
    // 纯 CPU，为它先 SELECT 查现有 token 反而多付一次 PG 往返
    let token = uuid::Uuid::new_v4().to_string();
    let row: Option<(String,)> = st
        .owned
        .fixed("UPDATE meta.artifact SET share_token = CASE WHEN share_token = '' THEN $2 ELSE share_token END WHERE id = $1 RETURNING share_token")
        .bind(id)
        .bind(&token)
        .fetch_optional()
        .await
        .map_err(db_err)?;
    let Some((tok,)) = row else {
        return Err(err(StatusCode::NOT_FOUND, "产物不存在"));
    };
    tracing::info!(artifact_id = id, operator = %login, "产物分享链接已签发（或复用既有 token）");
    Ok(Json(serde_json::json!({ "share_url": format!("/api/artifact/shared/{tok}") })))
}

/// `POST /api/artifact/{id}/unshare` —— 撤销链接（属主）。旧链接当场失效。
pub async fn unshare(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<i64>, Query(q): Query<ViewQuery>,
) -> ApiRes {
    let (_, login) = load_versioned(&st, &h, &q, id, None).await?;
    let affected = st.owned
        .fixed(SHARE_CLEAR_SQL)
        .bind(id)
        .execute()
        .await
        .map_err(db_err)?;
    if affected == 0 {
        return Err(err(StatusCode::NOT_FOUND, "产物不存在"));
    }
    tracing::info!(artifact_id = id, operator = %login, "产物分享链接已撤销");
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /api/artifact/shared/{token}` —— 免登录分享视图（**token 即能力**）。
/// 同样给沙箱头：内容是不可信渲染物，分享不改变它的信任级。
pub async fn shared(
    State(st): State<Arc<AppState>>, Path(token): Path<String>,
) -> Result<(HeaderMap, String), ApiErr> {
    // 形状闸：token 只收 uuid 形（防拿它当注入通道 —— 它进 SQL 是 bind，但别把奇怪东西查进去）
    if uuid::Uuid::parse_str(&token).is_err() {
        return Err(err(StatusCode::NOT_FOUND, "链接无效"));
    }
    let row: Option<(String,)> =
        st.owned.fixed(SHARE_GET_SQL).bind(&token).fetch_optional().await
            .map_err(db_err)?;
    let Some((html,)) = row else {
        return Err(err(StatusCode::NOT_FOUND, "链接无效或已撤销"));
    };
    Ok((sandbox_headers("text/html; charset=utf-8"), secure_artifact_html(&html)))
}

#[derive(serde::Deserialize, Default)]
pub struct ListQuery {
    conv_id: Option<String>,
    feed: Option<String>,
    /// 仅普通会话产物列表生效；daily/weekly feed 是单条快照，固定最多返回 1 条。
    limit: Option<i64>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `GET /api/artifact/list?conv_id=` —— 会话产物清单。
/// `feed=daily` 是管理员当日全局日报；`feed=weekly` 是当前用户本周最新会话周报。
/// 两个 feed 都是单条快照，固定最多返回 1 条；`limit` 只服务普通会话历史。
pub async fn list(
    State(st): State<Arc<AppState>>, h: HeaderMap, Query(q): Query<ListQuery>,
) -> ApiRes {
    require_bearer_session(&h)?;
    let (login, _role) = crate::resolve_identity(&st, &h, &q.login_name, &q.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let rows: Vec<(i64, String, String, String)> = match q.feed.as_deref() {
        Some("daily") => {
            if q.conv_id.is_some() {
                return Err(err(StatusCode::BAD_REQUEST, "日报/周报 feed 不能同时指定 conv_id"));
            }
            let _ = crate::admin_api::admin_only(
                &st,
                &h,
                (&q.login_name, &q.role_code),
            )
            .await?;
            st.owned.fixed(DAILY_FEED_SQL).fetch_all().await.map_err(db_err)?
        }
        Some("weekly") => {
            if q.conv_id.is_some() {
                return Err(err(StatusCode::BAD_REQUEST, "日报/周报 feed 不能同时指定 conv_id"));
            }
            st.owned
                .fixed(WEEKLY_FEED_SQL)
                .bind(&login)
                .fetch_all()
                .await
                .map_err(db_err)?
        }
        Some(other) => return Err(err(StatusCode::BAD_REQUEST, format!("未知产物 feed：{}", clipped_echo(other)))),
        None => {
            // 传了 conv_id 就校验归属；不传仍只查空 conv，不许枚举别人的会话产物。
            if let Some(cid) = &q.conv_id {
                let cid_num: i64 = cid
                    .parse()
                    .map_err(|_| err(StatusCode::BAD_REQUEST, "conv_id 必须是会话主键数字"))?;
                let owner = crate::chat::conv_owner(st.owned.pool(), cid_num).await
                    .map_err(db_err)?;
                if owner.as_deref() != Some(login.as_str()) {
                    return Err(err(StatusCode::FORBIDDEN, "无权访问该会话"));
                }
            }
            st.owned.fixed(LIST_SQL)
                .bind(q.conv_id.as_deref().unwrap_or(""))
                .bind(&login)
                .bind(q.limit.unwrap_or(20).clamp(1, 100))
                .fetch_all().await.map_err(db_err)?
        }
    };
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, kind, title, at)| serde_json::json!({
            "id": id, "kind": kind, "title": secure_artifact_title(&title), "created_at": at,
            "preview_url": format!("/api/artifact/{id}/view"),
            "download_url": format!("/api/artifact/{id}/download"),
        }))
        .collect();
    Ok(Json(serde_json::json!({ "artifacts": items })))
}

/// 整页外壳（标题 + 正文）。样式**全部内联**（沙箱里没有任何外部资源可达）。
pub fn page_shell(title: &str, body_html: &str) -> String {
    let title = secure_artifact_title(title);
    // 已有顶级 <h1> 就不叠双标题：大小写不敏感 + 边界闸（`<H1>` 不命中会叠双标题，
    // `<h1foo` 误命中会吞标题）
    let body_trim = body_html.trim_start();
    let has_h1 = body_trim.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("<h1"))
        && matches!(
            body_trim.get(4..).and_then(|r| r.chars().next()),
            Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')
        );
    let heading = if has_h1 {
        String::new()
    } else {
        format!("<h1>{}</h1>", escape(&title))
    };
    format!(
        "<!doctype html><html lang=\"zh\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <meta http-equiv=\"Content-Security-Policy\" data-dms-artifact-csp content=\"{csp}\">\
         <title>{t}</title><style>{css}</style></head>\
         <body><main class=\"page\">{heading}{body}</main></body></html>",
        t = escape(&title),
        csp = ARTIFACT_CSP,
        css = BASE_CSS,
        heading = heading,
        body = body_html
    )
}

/// 旧产物也会经过这里：父页用 Bearer `fetch` 后把 HTML 放进 `srcdoc`，HTTP 响应头不会
/// 随文本继承，因此 CSP/referrer 必须同时写进文档。这里还统一清理内部编号与敏感连接串，
/// 并把 AI 段移动到业务数据之后。
fn secure_artifact_html(html: &str) -> String {
    let mut html = redact_sensitive(html);
    for tag in ["script", "iframe", "object", "embed", "form", "link", "base"] {
        html = remove_elements_by_tag(&html, tag);
    }
    for class_name in ["sqlx", "method-sec", "methodx", "trustx", "evidence", "data-boundary"] {
        html = remove_elements_with_class(&html, class_name);
    }
    html = remove_elements_containing_terms(
        &html,
        &["p", "pre", "li", "code"],
        &[
            "sales_dw.", "dms_ods.", "select ", " from ", " join ", " where ", "sum(", "nullif(",
            "trace_id", "trace-id", "技术信任", "系统定时生成的全量经营快照",
            "仅应向具备全量经营权限",
        ],
    );
    for heading in [
        "SQL", "执行 SQL", "口径说明", "数据边界", "技术信任", "调试信息", "计算逻辑",
        "数据来源", "验证信息", "证据", "证据目录", "数据依据",
    ] {
        html = remove_heading_section(&html, heading);
    }
    html = remove_table_rows_with_terms(
        &html,
        &[
            "trace_id", "trace id", "trace-id", "调用次数", "路由详情", "api_key",
            "apikey", "password", "passwd", "authorization", "dsn", "secret", "jdbc:",
            "mysql://", "postgres://", "postgresql://", "sales_dw.", "dms_ods.",
        ],
    );
    html = remove_table_columns_with_headers(
        &html,
        &["证据", "依据", "evidence", "trace", "技术信任", "调用次数", "路由详情"],
    );
    html = strip_internal_refs(&html);
    html = html
        .replace("证据编号", "")
        .replace("证据", "数据")
        .replace("（订单事实去重）", "")
        .replace("(订单事实去重)", "");
    html = move_ai_to_end(&html);
    html = remove_meta_tags_containing(
        &html,
        &[CSP_META_MARKER, "http-equiv", "referrer"],
    );
    let meta = format!(
        "<meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"Content-Security-Policy\" {CSP_META_MARKER} content=\"{ARTIFACT_CSP}\">"
    );
    if let Some(index) = find_ascii_case_insensitive(&html, "<head>") {
        html.insert_str(index + "<head>".len(), &meta);
        html
    } else {
        format!(
            "<!doctype html><html lang=\"zh\"><head><meta charset=\"utf-8\">{meta}<style>{BASE_CSS}</style></head><body><main class=\"page\">{html}</main></body></html>"
        )
    }
}

fn secure_artifact_title(title: &str) -> String {
    let title = strip_internal_refs(&redact_sensitive(title));
    // 「证据」在标题里整词删除、在正文里改名「数据」（secure_artifact_html）：标题是紧凑
    // 展示位，改名后语义立不住；正文要保可读性，故改名 —— 两种处置是有意的，不是漂移
    let title = title
        .replace("证据编号", "")
        .replace("证据", "")
        .replace("trace_id", "")
        .replace("trace-id", "");
    let title = title
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let title = title.trim().chars().take(120).collect::<String>();
    if title.is_empty() { "分析报告".to_string() } else { title }
}

/// needle 入参约定**已小写**（调用点全是小写字面量，debug_assert 钉着）：只小写化 haystack，
/// 不再每次调用连 needle 也小写化一遍、haystack 整串复制一遍之外再多一份
fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    debug_assert!(needle.bytes().all(|b| !b.is_ascii_uppercase()), "needle 必须已小写：{needle}");
    haystack.to_ascii_lowercase().find(needle)
}

fn redact_sensitive(input: &str) -> String {
    let mut out = input.to_string();
    // lower 与 out 增量同步（掩码是纯 CJK，无大小写差）：替换发生在哪就同步改哪，
    // 不再每轮迭代对整串重算小写化（命中多时 O(n×次数) 反复全量分配）
    let mut lower = out.to_ascii_lowercase();
    for prefix in [
        "mysql://", "postgres://", "postgresql://", "jdbc:", "redis://", "mongodb://",
        "bearer ",
    ] {
        loop {
            let Some(start) = lower.find(prefix) else { break };
            let mut end = start + prefix.len();
            while end < out.len() {
                let ch = out[end..].chars().next().expect("字符边界");
                if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | ')' | ']' | '}') {
                    break;
                }
                end += ch.len_utf8();
            }
            out.replace_range(start..end, "[敏感连接信息已隐藏]");
            lower.replace_range(start..end, "[敏感连接信息已隐藏]");
        }
    }
    for label in [
        "api_key", "apikey", "password", "passwd", "authorization", "dsn", "secret",
        "trace_id", "trace-id", "trace id", "llm_api_key", "access_token", "database_url",
        "base_url", "access_key", "secret_key", "api-token", "密钥", "密码", "连接串",
    ] {
        redact_assignment(&mut out, label);
    }
    redact_long_prefixed(&mut out, "sk-", 20);
    redact_ipv4_ports(&mut out);
    out
}

fn redact_long_prefixed(text: &mut String, prefix: &str, minimum_len: usize) {
    let mut lower = text.to_ascii_lowercase(); // 增量同步，理由同 redact_sensitive
    let mut from = 0;
    loop {
        let Some(relative) = lower[from..].find(prefix) else { break };
        let start = from + relative;
        let mut end = start + prefix.len();
        while end < text.len() {
            let ch = text[end..].chars().next().expect("字符边界");
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                end += ch.len_utf8();
            } else {
                break;
            }
        }
        if end - start >= minimum_len {
            text.replace_range(start..end, "[敏感凭据已隐藏]");
            lower.replace_range(start..end, "[敏感凭据已隐藏]");
            from = start + "[敏感凭据已隐藏]".len();
        } else {
            from = end;
        }
    }
}

fn redact_assignment(text: &mut String, label: &str) {
    let mut lower = text.to_ascii_lowercase(); // 增量同步，理由同 redact_sensitive
    let mut from = 0;
    loop {
        let Some(relative) = lower[from..].find(label) else { break };
        let start = from + relative + label.len();
        let mut value_start = start;
        while value_start < text.len() {
            let rest = &text[value_start..];
            if let Some(entity) = ["&quot;", "&#34;", "&#39;", "&apos;"]
                .into_iter()
                .find(|entity| rest.starts_with(*entity))
            {
                value_start += entity.len();
                continue;
            }
            let ch = text[value_start..].chars().next().expect("字符边界");
            if ch.is_whitespace() || matches!(ch, ':' | '：' | '=' | '"' | '\'') {
                value_start += ch.len_utf8();
            } else {
                break;
            }
        }
        if value_start == start || value_start >= text.len() || text[value_start..].starts_with('<') {
            from = start;
            continue;
        }
        let mut end = value_start;
        while end < text.len() {
            let ch = text[end..].chars().next().expect("字符边界");
            if ch.is_whitespace()
                || matches!(ch, '<' | '>' | '"' | '\'' | ',' | ';' | '&' | ')' | ']' | '}')
            {
                break;
            }
            end += ch.len_utf8();
        }
        if end > value_start {
            text.replace_range(value_start..end, "[已隐藏]");
            lower.replace_range(value_start..end, "[已隐藏]");
            from = value_start + "[已隐藏]".len();
        } else {
            from = start;
        }
    }
}

fn redact_ipv4_ports(text: &mut String) {
    let mut cursor = 0;
    loop {
        let bytes = text.as_bytes();
        let mut found = None;
        let mut index = cursor;
        while index < bytes.len() {
            if bytes[index].is_ascii_digit()
                && (index == 0 || (!bytes[index - 1].is_ascii_digit() && bytes[index - 1] != b'.'))
            {
                let mut end = index;
                while end < bytes.len()
                    && (bytes[end].is_ascii_digit() || matches!(bytes[end], b'.' | b':'))
                {
                    end += 1;
                }
                if is_ipv4_port(&text[index..end]) {
                    found = Some((index, end));
                    break;
                }
                index = end.max(index + 1);
            } else {
                index += 1;
            }
        }
        let Some((start, end)) = found else { break };
        const MASK: &str = "[敏感连接地址已隐藏]";
        text.replace_range(start..end, MASK);
        cursor = start + MASK.len();
    }
}

fn is_ipv4_port(value: &str) -> bool {
    let Some((ip, port)) = value.split_once(':') else { return false };
    ip.split('.').count() == 4
        && ip.split('.').all(|part| !part.is_empty() && part.parse::<u8>().is_ok())
        && port.parse::<u16>().is_ok_and(|port| port > 0)
}

fn strip_internal_refs(input: &str) -> String {
    let mut out = input.to_string();
    // upper 与 out 增量同步（替换全是删除，不动大小写）：不再每轮全量重算大写化
    let mut upper = out.to_ascii_uppercase();
    for prefix in ["[KPI-", "[SEC-", "[CON-"] {
        let mut cursor = 0;
        loop {
            let Some(relative) = upper[cursor..].find(prefix) else { break };
            let start = cursor + relative;
            let mut end = start + prefix.len();
            while end < out.len() && out.as_bytes()[end].is_ascii_digit() {
                end += 1;
            }
            if end > start + prefix.len() && out.as_bytes().get(end) == Some(&b']') {
                out.replace_range(start..=end, "");
                upper.replace_range(start..=end, "");
                cursor = start;
            } else {
                cursor = end.max(start + prefix.len());
            }
        }
    }
    for prefix in ["KPI-", "SEC-", "CON-"] {
        let mut cursor = 0;
        loop {
            let Some(relative) = upper[cursor..].find(prefix) else { break };
            let start = cursor + relative;
            let mut end = start + prefix.len();
            while end < out.len() {
                let ch = out.as_bytes()[end] as char;
                if !ch.is_ascii_digit() {
                    break;
                }
                end += 1;
            }
            if end == start + prefix.len() {
                cursor = end;
                continue;
            }
            out.replace_range(start..end, "");
            upper.replace_range(start..end, "");
            cursor = start;
        }
    }
    out
}

fn remove_elements_by_tag(input: &str, tag: &str) -> String {
    let mut out = input.to_string();
    let mut lower = out.to_ascii_lowercase(); // 增量同步（替换全是删除），理由同 redact_sensitive
    loop {
        let needle = format!("<{tag}");
        let mut cursor = 0;
        let mut range = None;
        while let Some(relative) = lower[cursor..].find(&needle) {
            let start = cursor + relative;
            let boundary = lower[start + needle.len()..].chars().next();
            if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
                cursor = start + needle.len();
                continue;
            }
            let Some(open_end_relative) = lower[start..].find('>') else { break };
            let open_end = start + open_end_relative + 1;
            let end = matching_element_end(&lower, tag, open_end).unwrap_or(open_end);
            range = Some((start, end));
            break;
        }
        let Some((start, end)) = range else { break };
        out.replace_range(start..end, "");
        lower.replace_range(start..end, "");
    }
    out
}

/// 开标签里 class 属性的取值列表（只够收自渲染产物的口径，不是通用 HTML 解析器）：
/// 等号两侧空白（`class = "x"`）与无引号写法（`class=x`）都认；`data-class` 之类不算。
fn class_values(opening: &str) -> Vec<&str> {
    let mut values = Vec::new();
    let mut rest = opening;
    while let Some(i) = rest.find("class") {
        // 词边界：前一字符贴着属性名字符（data-class）不算
        let before_ok = rest[..i]
            .chars()
            .next_back()
            .map_or(true, |c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')));
        let after = rest[i + 5..].trim_start();
        rest = after;
        if !before_ok {
            continue;
        }
        let Some(after_eq) = after.strip_prefix('=') else { continue };
        let v = after_eq.trim_start();
        let (raw, remain) = match v.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let body = &v[1..];
                match body.find(q) {
                    Some(j) => (&body[..j], &body[j..]),
                    None => (body, ""),
                }
            }
            _ => {
                let j = v.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(v.len());
                (&v[..j], &v[j..])
            }
        };
        values.extend(raw.split_whitespace());
        rest = remain;
    }
    values
}

fn remove_elements_with_class(input: &str, class_name: &str) -> String {
    let mut out = input.to_string();
    let mut lower = out.to_ascii_lowercase(); // 增量同步（替换全是删除），理由同上
    for tag in ["section", "details", "div", "aside", "article"] {
        loop {
            let needle = format!("<{tag}");
            let mut cursor = 0;
            let mut found = None;
            while let Some(relative) = lower[cursor..].find(&needle) {
                let start = cursor + relative;
                let Some(open_end_rel) = lower[start..].find('>') else { break };
                let open_end = start + open_end_rel + 1;
                let opening = &lower[start..open_end];
                if class_values(opening).iter().any(|v| *v == class_name) {
                    found = Some((start, open_end));
                    break;
                }
                cursor = open_end;
            }
            let Some((start, open_end)) = found else { break };
            let end = matching_element_end(&lower, tag, open_end).unwrap_or(open_end);
            out.replace_range(start..end, "");
            lower.replace_range(start..end, "");
        }
    }
    out
}

fn matching_element_end(lower: &str, tag: &str, content_start: usize) -> Option<usize> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut cursor = content_start;
    let mut depth = 1usize;
    while depth > 0 {
        let next_open = lower[cursor..].find(&open).map(|offset| cursor + offset);
        let next_close = lower[cursor..].find(&close).map(|offset| cursor + offset);
        match (next_open, next_close) {
            (_, None) => return None,
            (Some(open_at), Some(close_at)) if open_at < close_at => {
                // 开标签边界闸：`<tablex` 不计作 `<table` 嵌套（深度算错会让结束位偏移）
                let boundary = lower[open_at + open.len()..].chars().next();
                if matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
                    depth += 1;
                }
                cursor = open_at + open.len();
            }
            (_, Some(close_at)) => {
                depth -= 1;
                cursor = close_at + close.len();
            }
        }
    }
    Some(cursor)
}

fn remove_elements_containing_terms(input: &str, tags: &[&str], terms: &[&str]) -> String {
    let mut out = input.to_string();
    let mut lower = out.to_ascii_lowercase(); // 增量同步（替换全是删除），理由同上
    // terms 小写化一次（逐元素 any() 闭包里反复 to_ascii_lowercase 是白分配）
    let terms: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    for tag in tags {
        loop {
            let needle = format!("<{tag}");
            let mut cursor = 0;
            let mut range = None;
            while let Some(relative) = lower[cursor..].find(&needle) {
                let start = cursor + relative;
                let boundary = lower[start + needle.len()..].chars().next();
                if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
                    cursor = start + needle.len();
                    continue;
                }
                let Some(open_end_relative) = lower[start..].find('>') else { break };
                let open_end = start + open_end_relative + 1;
                let Some(end) = matching_element_end(&lower, tag, open_end) else { break };
                if terms.iter().any(|term| lower[start..end].contains(term)) {
                    range = Some((start, end));
                    break;
                }
                cursor = end;
            }
            let Some((start, end)) = range else { break };
            out.replace_range(start..end, "");
            lower.replace_range(start..end, "");
        }
    }
    out
}

fn remove_heading_section(input: &str, heading: &str) -> String {
    let mut out = input.to_string();
    let mut lower = out.to_ascii_lowercase(); // 增量同步（替换全是删除），理由同上
    for level in ["h2", "h3"] {
        loop {
            let needle = format!("<{level}");
            let close = format!("</{level}>");
            let mut cursor = 0;
            let mut range = None;
            while let Some(relative) = lower[cursor..].find(&needle) {
                let start = cursor + relative;
                // 边界闸 + 带属性开标签（`<h2 class="x">` 里的该删标题不许静默漏过）
                let boundary = lower[start + needle.len()..].chars().next();
                if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
                    cursor = start + needle.len();
                    continue;
                }
                let Some(open_end_rel) = lower[start..].find('>') else { break };
                let body_start = start + open_end_rel + 1;
                let Some(close_relative) = lower[body_start..].find(&close) else { break };
                let heading_end = body_start + close_relative + close.len();
                let text = out[body_start..body_start + close_relative].trim();
                if text.eq_ignore_ascii_case(heading) {
                    let end = lower[heading_end..]
                        .find(&needle)
                        .map(|offset| heading_end + offset)
                        .or_else(|| lower[heading_end..].find("</main>").map(|offset| heading_end + offset))
                        .or_else(|| lower[heading_end..].find("</body>").map(|offset| heading_end + offset))
                        .unwrap_or(out.len());
                    range = Some((start, end));
                    break;
                }
                cursor = heading_end;
            }
            let Some((start, end)) = range else { break };
            out.replace_range(start..end, "");
            lower.replace_range(start..end, "");
        }
    }
    out
}

fn remove_table_rows_with_terms(input: &str, terms: &[&str]) -> String {
    let mut out = input.to_string();
    let mut lower = out.to_ascii_lowercase(); // 增量同步（替换全是删除），理由同上
    loop {
        let mut cursor = 0;
        let mut range = None;
        while let Some(relative) = lower[cursor..].find("<tr") {
            let start = cursor + relative;
            // 标签边界闸：`<trxyz` 不算行起点（与 extract_cells 同一先例）
            let boundary = lower[start + 3..].chars().next();
            if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
                cursor = start + 3;
                continue;
            }
            let Some(end_relative) = lower[start..].find("</tr>") else { break };
            let end = start + end_relative + "</tr>".len();
            if terms.iter().any(|term| lower[start..end].contains(term)) {
                range = Some((start, end));
                break;
            }
            cursor = end;
        }
        let Some((start, end)) = range else { break };
        out.replace_range(start..end, "");
        lower.replace_range(start..end, "");
    }
    out
}

fn remove_table_columns_with_headers(input: &str, terms: &[&str]) -> String {
    let mut out = input.to_string();
    let mut lower = out.to_ascii_lowercase(); // 增量同步，理由同上
    let mut cursor = 0;
    loop {
        let Some(relative) = lower[cursor..].find("<table") else { break };
        let start = cursor + relative;
        // 标签边界闸：`<tablex` 伪标签不算（与 extract_cells 同一先例）
        let boundary = lower[start + 6..].chars().next();
        if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
            cursor = start + 6;
            continue;
        }
        let Some(open_end_relative) = lower[start..].find('>') else { break };
        let open_end = start + open_end_relative + 1;
        let Some(end) = matching_element_end(&lower, "table", open_end) else { break };
        let replacement = strip_table_columns(&out[start..end], terms);
        if replacement.as_str() != &out[start..end] {
            out.replace_range(start..end, &replacement);
            lower.replace_range(start..end, &replacement.to_ascii_lowercase());
            cursor = start + replacement.len();
        } else {
            cursor = end;
        }
    }
    out
}

fn strip_table_columns(table: &str, terms: &[&str]) -> String {
    let lower = table.to_ascii_lowercase();
    let Some(header_start) = lower.find("<tr") else { return table.to_string() };
    let Some(header_end_relative) = lower[header_start..].find("</tr>") else {
        return table.to_string();
    };
    let header_end = header_start + header_end_relative + "</tr>".len();
    let headers = cell_ranges(&table[header_start..header_end], "th");
    // terms 小写化一次（逐单元格 any() 闭包里反复算白分配；调用点本就全小写字面量）
    let terms: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    let hidden = headers
        .iter()
        .enumerate()
        .filter_map(|(index, (start, end))| {
            let cell = table[header_start + start..header_start + end].to_ascii_lowercase();
            terms
                .iter()
                .any(|term| cell.contains(term))
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if hidden.is_empty() {
        return table.to_string();
    }
    if hidden.len() == headers.len() {
        return String::new();
    }

    let mut out = table.to_string();
    let lower = out.to_ascii_lowercase();
    let mut rows = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("<tr") {
        let start = cursor + relative;
        // 标签边界闸：`<trxyz` 不算行起点（与 extract_cells 同一先例）
        let boundary = lower[start + 3..].chars().next();
        if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
            cursor = start + 3;
            continue;
        }
        let Some(end_relative) = lower[start..].find("</tr>") else { break };
        let end = start + end_relative + "</tr>".len();
        rows.push((start, end));
        cursor = end;
    }
    for (start, end) in rows.into_iter().rev() {
        let mut row = out[start..end].to_string();
        let tag = if row.to_ascii_lowercase().contains("<th") { "th" } else { "td" };
        let cells = cell_ranges(&row, tag);
        for index in hidden.iter().rev().copied() {
            if let Some((cell_start, cell_end)) = cells.get(index).copied() {
                row.replace_range(cell_start..cell_end, "");
            }
        }
        out.replace_range(start..end, &row);
    }
    out
}

fn cell_ranges(row: &str, tag: &str) -> Vec<(usize, usize)> {
    let lower = row.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(&open) {
        let start = cursor + relative;
        let boundary = lower[start + open.len()..].chars().next();
        if !matches!(boundary, Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')) {
            cursor = start + open.len();
            continue;
        }
        let Some(end_relative) = lower[start..].find(&close) else { break };
        let end = start + end_relative + close.len();
        ranges.push((start, end));
        cursor = end;
    }
    ranges
}

fn remove_meta_tags_containing(input: &str, terms: &[&str]) -> String {
    let mut out = input.to_string();
    let mut lower = out.to_ascii_lowercase(); // 增量同步（替换全是删除），理由同上
    let terms: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    loop {
        let mut cursor = 0;
        let mut range = None;
        while let Some(relative) = lower[cursor..].find("<meta") {
            let start = cursor + relative;
            let Some(end_relative) = lower[start..].find('>') else { break };
            let end = start + end_relative + 1;
            if terms.iter().any(|term| lower[start..end].contains(term)) {
                range = Some((start, end));
                break;
            }
            cursor = end;
        }
        let Some((start, end)) = range else { break };
        out.replace_range(start..end, "");
        lower.replace_range(start..end, "");
    }
    out
}

fn move_ai_to_end(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    if let Some(start) = lower.find("<section class=\"bi-ai\"") {
        if let Some(open_end) = lower[start..].find('>').map(|offset| start + offset + 1) {
            if let Some(end) = matching_element_end(&lower, "section", open_end) {
                return move_segment_before_document_end(input, start, end);
            }
        }
    }
    for heading in ["<h2>AI 解读</h2>", "<h2>AI 分析摘要</h2>", "<h2>AI 经营点评</h2>"] {
        let Some(start) = input.find(heading) else { continue };
        let after_heading = start + heading.len();
        let end = [
            "<h2>数据</h2>",
            "<h2>经营数据</h2>",
            "<h2>业务明细</h2>",
            "<h2>SQL</h2>",
        ]
        .into_iter()
        .filter_map(|boundary| input[after_heading..].find(boundary).map(|offset| after_heading + offset))
        .min()
            .or_else(|| input[after_heading..].find("</main>").map(|offset| after_heading + offset))
            .or_else(|| input[after_heading..].find("</body>").map(|offset| after_heading + offset))
            .unwrap_or(input.len());
        return move_segment_before_document_end(input, start, end);
    }
    input.to_string()
}

fn move_segment_before_document_end(input: &str, start: usize, end: usize) -> String {
    let segment = input[start..end].to_string();
    let mut out = input.to_string();
    out.replace_range(start..end, "");
    // 大小写不敏感：大写闭合标签时锚点丢失会退化成追加到文末
    let insert_at = find_ascii_case_insensitive(&out, "</main>")
        .or_else(|| find_ascii_case_insensitive(&out, "</body>"))
        .unwrap_or(out.len());
    out.insert_str(insert_at, &segment);
    out
}

const BASE_CSS: &str = r#"
:root { color-scheme: light; }
* { box-sizing: border-box; }
html { background: #eef1f6; }
body { margin: 0; font: 14px/1.72 -apple-system, BlinkMacSystemFont, "Segoe UI", "Microsoft YaHei", sans-serif; color: #1f2937; background: #eef1f6; }
.page { width: min(1180px,100%); margin: 0 auto; padding: 38px 42px 72px; background: #fff; min-height: 100vh; box-shadow: 0 8px 32px rgba(31,45,77,.08); }
h1 { font-size: 24px; line-height: 1.35; margin: 0 0 22px; padding-bottom: 16px; border-bottom: 2px solid #3567d6; color: #17213a; }
h2 { font-size: 17px; margin: 28px 0 11px; color: #243455; }
h3 { font-size: 14px; margin: 18px 0 8px; color: #344666; }
p { margin: 8px 0; }
table { border-collapse: separate; border-spacing: 0; width: 100%; margin: 13px 0; font-size: 13px; border: 1px solid #dfe4ed; border-radius: 7px; overflow: hidden; }
th, td { border: 0; border-bottom: 1px solid #e7eaf0; padding: 9px 11px; text-align: left; vertical-align: top; overflow-wrap: anywhere; }
th { background: #f2f5fa; color: #31415f; font-weight: 650; white-space: nowrap; }
tr:last-child td { border-bottom: 0; }
tr:nth-child(even) td { background: #fafbfd; }
code { background: #eef1fb; padding: 1px 5px; border-radius: 4px; font-family: ui-monospace, Consolas, monospace; font-size: 12.5px; }
pre { background: #f4f6fa; color: #29354e; border: 1px solid #dfe4ec; padding: 12px 14px; border-radius: 7px; overflow-x: auto; font-size: 12.5px; }
pre code { background: none; padding: 0; color: inherit; }
ul, ol { margin: 8px 0 8px 22px; padding: 0; }
li { margin: 3px 0; }
hr { border: none; border-top: 1px solid #e3e7f3; margin: 22px 0; }
strong { color: #2b3a67; }
.kpi-grid { display: grid; grid-template-columns: repeat(auto-fit,minmax(210px,1fr)); gap: 14px; margin: 18px 0 22px; }
.kpi { background: #fff; border: 1px solid #dfe4ec; border-top: 3px solid #3567d6; border-radius: 7px; padding: 16px 18px; box-shadow: 0 5px 18px rgba(35,48,80,.05); }
.kpi .l { font-size: 12px; color: #6a759a; }
.kpi .v { font-size: 28px; line-height: 1.25; font-weight: 720; color: #202b4d; margin-top: 5px; font-variant-numeric: tabular-nums; }
.kpi.comparison { border-top-color: #a9b3c9; background: #f8f9fb; }
.kpi.comparison .v { font-size: 24px; }
.kpi.comparison.up .v { color: #b63832; }
.kpi.comparison.down .v { color: #17825c; }
.kpi .n { margin-top: 4px; color: #8b93a7; font-size: 11px; }
.highlight-grid { display: grid; grid-template-columns: repeat(3,minmax(0,1fr)); gap: 12px; margin: -8px 0 24px; }
.highlight { border: 1px solid #dfe4ec; background: #f8f9fb; padding: 12px 14px; border-radius: 7px; }
.highlight .l { font-size: 11px; color: #6a759a; }
.highlight .v { margin-top: 3px; font-size: 18px; font-weight: 700; color: #202b4d; font-variant-numeric: tabular-nums; }
.highlight .n { margin-top: 2px; color: #77819b; font-size: 11px; line-height: 1.5; }
.fact-grid { display: grid; grid-template-columns: repeat(auto-fit,minmax(210px,1fr)); gap: 10px; margin: 0 0 22px; }
.fact { min-width: 0; border: 1px solid #dfe4ec; background: #fff; padding: 11px 13px; border-radius: 7px; }
.fact span { display: block; margin-bottom: 4px; color: #77819b; font-size: 11px; }
.fact b { display: block; color: #202b4d; font-size: 13px; line-height: 1.45; overflow-wrap: anywhere; }
/* 【深度 BI 页】头部与板块卡（S2 报表/日报同步受益） */
.bi-head { border-bottom: 1px solid #dfe4ec; padding-bottom: 18px; margin-bottom: 22px; }
.bi-head h1 { border: none; margin: 0 0 6px; padding: 0; }
.bi-meta { font-size: 12px; color: #6a759a; display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
.bi-badge { background: #edf2fb; color: #304b80; border-radius: 99px; padding: 2px 10px; font-size: 11px; }
.bi-sec { background: #fff; border: 1px solid #dfe4ec; border-radius: 7px; padding: 20px 22px; margin: 16px 0; box-shadow: 0 4px 16px rgba(35,48,80,.04); }
.bi-sec table { background: #fff; }
.bi-brief { background: #f7f8fb; border-left: 3px solid #8094df; padding: 14px 18px; margin: 0 0 18px; }
.bi-brief p { margin: 4px 0 0; color: #36415f; font-size: 14px; }
.eyebrow { color: #66739a; font-size: 11px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; }
.sec-head { display: flex; justify-content: space-between; gap: 20px; align-items: flex-start; margin-bottom: 14px; }
.sec-head h2 { margin: 3px 0 2px; color: #202b4d; font-size: 17px; }
.sec-head p { margin: 0; color: #77819b; font-size: 12px; }
.sec-note { color: #77819b; border: 1px solid #dfe4ec; padding: 2px 8px; font-size: 11px; white-space: nowrap; }
.bi-ai { background: #f7f9fc; color: #30384d; border: 1px solid #dfe4ec; border-left: 4px solid #3567d6; border-radius: 7px; padding: 18px 20px; margin: 32px 0 0; }
.bi-ai .eyebrow { color: #3567d6; }
.bi-ai h2,.bi-ai h3,.bi-ai strong { color: #202b4d; }
.bi-ai .ai-grid { display: block; }
.bi-ai .ai-grid > h2 { margin: 14px 0 6px; padding-top: 12px; border-top: 1px solid #dfe4ec; font-size: 14px; }
.bi-ai .ai-grid > h2:first-child { border-top: 0; padding-top: 0; margin-top: 0; }
.bi-ai p,.bi-ai ul { margin: 4px 0 8px; color: #4b556f; }
.bi-ai table { background: #fff; margin: 8px 0 14px; }
.bi-ai code { background: #eef1fb; color: #33416e; }
.section-kicker { display: flex; align-items: baseline; gap: 10px; margin: 28px 0 10px; border-bottom: 1px solid #dfe4ec; padding-bottom: 9px; }
.section-kicker span { color: #66739a; font-size: 11px; font-weight: 700; letter-spacing: .08em; }
.section-kicker b { color: #202b4d; font-size: 16px; }
.detail-sec { background: #fafbfc; }
.contribution-sec { border-left: 4px solid #3567d6; }
.note { background: #fff8e8; border-left: 3px solid #e8b93e; padding: 8px 12px; border-radius: 6px; color: #6b5a1e; font-size: 13px; }
/* 【图表】inline SVG 图（chart_svg.rs 手绘，零外部资源） */
figure.chart { margin: 12px 0 18px; padding: 13px 14px; background: #fbfcfe; border: 1px solid #e1e6ef; border-radius: 7px; overflow-x: auto; }
figure.chart figcaption { font-size: 13px; font-weight: 650; color: #293b60; margin-bottom: 7px; }
figure.chart svg { display: block; min-width: 560px; height: auto; }
.chart-empty { margin: 10px 0 16px; padding: 22px; text-align: center; color: #77819b; background: #f8fafc; border: 1px dashed #cfd6e2; border-radius: 7px; }
@media (max-width: 760px) {
  .page { padding: 22px 15px 48px; box-shadow: none; }
  h1 { font-size: 20px; }
  .highlight-grid { grid-template-columns: 1fr; }
  .sec-head { flex-direction: column; gap: 6px; }
  .bi-sec { padding: 16px 14px; }
  table { display: block; overflow-x: auto; white-space: nowrap; }
}
"#;

/// HTML 转义（渲染器的**第一道** —— 任何用户/LLM 文本先进它，没有它产物页就是注入面）
pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 行内样式（`**粗**` 与 `` `码` ``；先转义再包标签，文本永不成 HTML）
fn inline(s: &str) -> String {
    let s = escape(s);
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("**") {
        let Some(j) = rest[i + 2..].find("**") else { break };
        out.push_str(&rest[..i]);
        out.push_str("<strong>");
        out.push_str(&rest[i + 2..i + 2 + j]);
        out.push_str("</strong>");
        rest = &rest[i + 2 + j + 2..];
    }
    out.push_str(rest);
    // `code`
    let s = out;
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find('`') {
        let Some(j) = rest[i + 1..].find('`') else { break };
        out.push_str(&rest[..i]);
        out.push_str("<code>");
        out.push_str(&rest[i + 1..i + 1 + j]);
        out.push_str("</code>");
        rest = &rest[i + 1 + j + 1..];
    }
    out.push_str(rest);
    out
}

/// 极简 markdown → HTML（标题/粗体/行内码/围栏码/表格/列表/分隔线/段落）。
/// **刻意不引依赖**：产物内容是不可信文本，渲染面越小越好；表格只允许一行表头。
/// 覆盖不了的形态（图片/嵌套列表/HTML 块）**降级为转义文本**，不保证精致，保证安全。
pub fn md_to_html(md: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut in_table = false;
    let mut table_headers: Vec<String> = vec![];
    let mut list_open: Option<char> = None;
    let mut para: Vec<String> = vec![];
    let flush_para = |out: &mut String, para: &mut Vec<String>| {
        if !para.is_empty() {
            out.push_str("<p>");
            out.push_str(&inline(&para.join(" ")));
            out.push_str("</p>");
            para.clear();
        }
    };
    let close_list = |out: &mut String, list_open: &mut Option<char>| {
        if let Some(k) = list_open.take() {
            out.push_str(if k == 'u' { "</ul>" } else { "</ol>" });
        }
    };
    let close_table = |out: &mut String, in_table: &mut bool, headers: &mut Vec<String>| {
        if *in_table {
            out.push_str("</table>");
            *in_table = false;
            headers.clear();
        }
    };
    for line in md.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            if in_code {
                out.push_str("</code></pre>");
                in_code = false;
            } else {
                flush_para(&mut out, &mut para);
                close_list(&mut out, &mut list_open);
                close_table(&mut out, &mut in_table, &mut table_headers);
                out.push_str("<pre><code>");
                in_code = true;
            }
            continue;
        }
        if in_code {
            out.push_str(&escape(line));
            out.push('\n');
            continue;
        }
        if t.is_empty() {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut list_open);
            close_table(&mut out, &mut in_table, &mut table_headers);
            continue;
        }
        if t.starts_with("|") && t.ends_with('|') {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut list_open);
            let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
            // 分隔行 |---|---| 跳过（上一行已按表头渲）
            if cells.iter().all(|c| c.chars().all(|x| x == '-' || x == ':' || x == ' ')) {
                continue;
            }
            if !in_table {
                out.push_str("<table><tr>");
                for c in &cells {
                    out.push_str("<th>");
                    out.push_str(&inline(c));
                    out.push_str("</th>");
                }
                out.push_str("</tr>");
                table_headers = cells.iter().map(|cell| (*cell).to_string()).collect();
                in_table = true;
            } else {
                out.push_str("<tr>");
                for (index, c) in cells.iter().enumerate() {
                    out.push_str("<td>");
                    let header = table_headers.get(index).map(String::as_str).unwrap_or("");
                    let value = crate::chart_svg::display_value(
                        header,
                        &serde_json::Value::String((*c).to_string()),
                    );
                    out.push_str(&inline(&value));
                    out.push_str("</td>");
                }
                out.push_str("</tr>");
            }
            continue;
        }
        close_table(&mut out, &mut in_table, &mut table_headers);
        if let Some(h) = t.strip_prefix("# ") {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut list_open);
            out.push_str("<h1>");
            out.push_str(&inline(h));
            out.push_str("</h1>");
        } else if let Some(h) = t.strip_prefix("### ") {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut list_open);
            out.push_str("<h3>");
            out.push_str(&inline(h));
            out.push_str("</h3>");
        } else if let Some(h) = t.strip_prefix("## ") {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut list_open);
            out.push_str("<h2>");
            out.push_str(&inline(h));
            out.push_str("</h2>");
        } else if t == "---" || t == "***" {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut list_open);
            out.push_str("<hr>");
        } else if let Some(li) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            flush_para(&mut out, &mut para);
            if list_open != Some('u') {
                close_list(&mut out, &mut list_open);
                out.push_str("<ul>");
                list_open = Some('u');
            }
            out.push_str("<li>");
            out.push_str(&inline(li));
            out.push_str("</li>");
        } else if t.chars().next().is_some_and(|c| c.is_ascii_digit())
            && t.find(". ").is_some_and(|i| i < 4)
        {
            flush_para(&mut out, &mut para);
            if list_open != Some('o') {
                close_list(&mut out, &mut list_open);
                out.push_str("<ol>");
                list_open = Some('o');
            }
            let li = t.splitn(2, ". ").nth(1).unwrap_or(t);
            out.push_str("<li>");
            out.push_str(&inline(li));
            out.push_str("</li>");
        } else {
            para.push(t.to_string());
        }
    }
    flush_para(&mut out, &mut para);
    close_list(&mut out, &mut list_open);
    close_table(&mut out, &mut in_table, &mut table_headers);
    if in_code {
        out.push_str("</code></pre>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 渲染器的铁律：**文本永不成 HTML**（产物内容是 LLM/用户给的，没这条就是注入面）
    #[test]
    fn md_escapes_everything_by_default() {
        let html = md_to_html("# <script>alert(1)</script>\n正文 **<b>x</b>** 与 `<img>`");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>"), "转义漏了：{html}");
        assert!(html.contains("<strong>&lt;b&gt;x&lt;/b&gt;</strong>"), "{html}");
        // 表格形态
        let t = md_to_html("| 月份 | 销售额 |\n|---|---|\n| 七月 | 206,084,819.19 |");
        assert!(t.contains("<th>月份</th>") && t.contains("<td>¥20608.48万</td>"), "{t}");
        // 围栏代码块整段转义
        let c = md_to_html("```\nSELECT * FROM t WHERE x = '<script>'\n```");
        assert!(c.contains("&lt;script&gt;") && c.contains("<pre><code>"), "{c}");
        // page_shell 的标题也转义
        assert!(!page_shell("<img onerror=alert(1)>", "x").contains("<img"));
    }

    /// 日报是管理员全局产物；周报是用户会话产物，SQL 直接关联 conv_owner。
    #[test]
    fn digest_feeds_keep_distinct_permission_models() {
        assert!(DAILY_FEED_SQL.contains("conv_id = ''"), "{DAILY_FEED_SQL}");
        assert!(DAILY_FEED_SQL.contains("created_by = 'daily-digest'"), "{DAILY_FEED_SQL}");
        assert!(DAILY_FEED_SQL.contains("AT TIME ZONE 'Asia/Shanghai'"), "{DAILY_FEED_SQL}");
        assert!(!DAILY_FEED_SQL.contains("CURRENT_DATE"), "{DAILY_FEED_SQL}");
        assert!(DAILY_FEED_SQL.contains("ORDER BY id DESC LIMIT 1"), "{DAILY_FEED_SQL}");
        assert!(WEEKLY_FEED_SQL.contains("conv_id <> ''"), "{WEEKLY_FEED_SQL}");
        assert!(WEEKLY_FEED_SQL.contains("created_by = $1"), "{WEEKLY_FEED_SQL}");
        assert!(WEEKLY_FEED_SQL.contains("JOIN chat.conv") && WEEKLY_FEED_SQL.contains("c.login_name = $1"), "{WEEKLY_FEED_SQL}");
        assert!(WEEKLY_FEED_SQL.contains("AT TIME ZONE 'Asia/Shanghai'"), "{WEEKLY_FEED_SQL}");
        assert!(WEEKLY_FEED_SQL.contains("经营周报（[0-9]{4}-[0-9]{2}-[0-9]{2} 至"), "{WEEKLY_FEED_SQL}");
        assert!(!WEEKLY_FEED_SQL.contains("LIKE '%周报%'"), "普通深度报告不得被模糊匹配成周报：{WEEKLY_FEED_SQL}");
        assert!(!WEEKLY_FEED_SQL.contains("created_by = 'weekly-digest'"), "{WEEKLY_FEED_SQL}");
        assert!(WEEKLY_FEED_SQL.contains("ORDER BY a.id DESC LIMIT 1"), "{WEEKLY_FEED_SQL}");
        assert!(LIST_SQL.contains("created_by NOT IN ('daily-digest','weekly-digest')"), "默认产物列表不得枚举日报/周报：{LIST_SQL}");
        assert!(LIST_SQL.contains("conv_id <> '' OR created_by = $2"), "空会话产物必须按创建人收口：{LIST_SQL}");

        let src = include_str!("artifact_api.rs");
        let list = src
            .split("pub async fn list(").nth(1).expect("list 没了")
            .split("/// 整页外壳").next().expect("list 边界没了");
        assert!(list.contains("Some(\"daily\")") && list.contains("Some(\"weekly\")"), "{list}");
        assert!(list.contains("DAILY_FEED_SQL") && list.contains("WEEKLY_FEED_SQL"), "{list}");
        assert!(list.contains("admin_only"), "全局日报 feed 必须重新校验 DMS 管理员：{list}");
        assert!(list.contains("fixed(WEEKLY_FEED_SQL)") && list.contains("bind(&login)"), "周报 feed 必须绑定当前用户：{list}");
        assert!(list.contains("日报/周报 feed 不能同时指定 conv_id"), "{list}");

        let load = src
            .split("async fn load(").nth(1).expect("load 没了")
            .split("/// 沙箱响应头").next().expect("load 边界没了");
        assert!(load.contains("\"daily-digest\" | \"weekly-digest\""), "{load}");
        assert!(load.contains("admin_only"), "历史全局 digest 预览、下载、分享必须走管理员校验：{load}");
        // D6 重构后权限判据收在 check_perm（load → load_versioned → check_perm 都在这一扫描段内）；
        // login 以 &str 传入，属主比较写作 Some(login)。
        assert!(load.contains("conv_owner") && load.contains("Some(login)"), "用户周报必须按会话属主校验：{load}");
        assert!(load.contains("row.created_by != login"), "空会话产物必须校验创建人：{load}");
    }

    /// 【分享】三件套的形状（源码扫描 —— token 流要连库）：
    /// ① 发链接复用 load() 的属主校验（非属主不许发）；② 免登录视图只认
    /// `share_token <> ''` 的行 + uuid 形状闸；③ 撤销是清空不是删行。
    #[test]
    fn share_flow_keeps_ownership_gate_and_token_shape() {
        let src = include_str!("artifact_api.rs");
        let share = src.split("pub async fn share(").nth(1).expect("share 没了");
        assert!(share.contains("load_versioned(&st, &h, &q, id, None).await?"), "发链接少了属主校验：{share}");
        // 已有 token 不轮换（轮换 = 旧链接静默失效，用户不知道）
        assert!(share.contains("CASE WHEN share_token = '' THEN $2 ELSE share_token END"), "{share}");
        let shared = src.split("pub async fn shared(").nth(1).expect("shared 没了");
        assert!(shared.contains("share_token <> ''"), "空 token 不许命中：{shared}");
        assert!(shared.contains("uuid::Uuid::parse_str"), "token 必须过 uuid 形状闸：{shared}");
        assert!(shared.contains("sandbox_headers"), "分享页也必须给沙箱头：{shared}");
        let unshare = src.split("pub async fn unshare(").nth(1).expect("unshare 没了");
        assert!(unshare.contains("share_token = ''"), "撤销 = 清空 token：{unshare}");
        assert!(unshare.contains("load_versioned(&st, &h, &q, id, None).await?"), "撤销也少属主校验：{unshare}");
    }

    #[test]
    fn artifact_output_is_sandboxed_redacted_and_business_only() {
        let html = page_shell(
            "报告",
            "<section class=\"bi-sec\">数据</section><details class=\"sqlx\"><summary>执行 SQL</summary><pre>SELECT 1</pre></details><section class=\"bi-ai\">结论 [SEC-01]</section><p>dsn=mysql://user:pass@host/db trace_id=abc</p>",
        );
        let secured = secure_artifact_html(&html);
        assert!(secured.contains(CSP_META_MARKER) && secured.contains("connect-src 'none'"), "{secured}");
        assert!(secured.contains("script-src 'none'"), "{secured}");
        assert!(!secured.contains("SELECT 1") && !secured.contains("SEC-01"), "{secured}");
        assert!(!secured.contains("user:pass@host") && !secured.contains("trace_id=abc"), "{secured}");
        assert!(secured.find("数据</section>").unwrap() < secured.find("class=\"bi-ai\"").unwrap(), "AI 必须最后：{secured}");
    }

    #[test]
    fn artifact_output_drops_internal_columns_scripts_and_network_targets() {
        let html = "<html><head><meta http-equiv=\"refresh\" content=\"0;url=https://bad\"></head><body>\
            <table><tr><th>结论</th><th>证据</th></tr><tr><td>增长</td><td>SEC-01</td></tr></table>\
            <script>alert(1)</script><iframe src=\"https://bad\"></iframe>\
            <p>服务地址 203.0.113.7:19030</p></body></html>";
        let secured = secure_artifact_html(html);
        assert!(secured.contains("<th>结论</th>") && !secured.contains("<th>证据</th>"), "{secured}");
        assert!(!secured.contains("<script") && !secured.contains("<iframe"), "{secured}");
        assert!(!secured.contains("203.0.113.7:19030") && !secured.contains("http-equiv=\"refresh"), "{secured}");
    }

    /// 【D6 版本链】链键 = (conv_id,kind,title)：自增、单版本回看、链列表、唯一索引兜底，
    /// 迁移幂等（老数据按 id 序回填，单行链保持 1）。
    #[test]
    fn version_chain_sql_pins_increment_backfill_and_lookup() {
        assert!(INSERT_SQL.contains("COALESCE(MAX(version),0)+1"), "版本自增子查询没了：{INSERT_SQL}");
        assert!(INSERT_SQL.contains("RETURNING id, version"), "{INSERT_SQL}");
        // 自增谓词与链谓词同形（否则自增的链与回看的链不是同一条）
        assert!(INSERT_SQL.contains("WHERE conv_id=$1 AND kind=$2 AND title=$3"), "{INSERT_SQL}");
        assert!(GET_VERSION_SQL.contains("conv_id = $1 AND kind = $2 AND title = $3 AND version = $4"), "{GET_VERSION_SQL}");
        assert!(CHAIN_SQL.contains("conv_id = $1 AND kind = $2 AND title = $3"), "{CHAIN_SQL}");
        assert!(CHAIN_SQL.contains("ORDER BY version DESC"), "{CHAIN_SQL}");
        // 迁移：加列幂等 + 老数据回填（id 序重编号，单行链 no-op）+ 唯一索引防并发撞号
        let ddl = include_str!("../../semantic/src/ddl.rs");
        assert!(ddl.contains("ALTER TABLE meta.artifact ADD COLUMN IF NOT EXISTS version int NOT NULL DEFAULT 1"), "version 加列迁移缺失");
        assert!(ddl.contains("row_number() OVER (PARTITION BY conv_id, kind, title ORDER BY id)"), "老数据回填缺失");
        assert!(ddl.contains("a.version <> s.v"), "回填必须幂等（只动版本不一致的行）");
        assert!(ddl.contains("CREATE UNIQUE INDEX IF NOT EXISTS idx_artifact_chain ON meta.artifact(conv_id, kind, title, version)"), "链唯一索引缺失");
        // save_artifact 旧签名保留（deep_api / daily_digest 直呼），版本号走 versioned 变体
        let src = include_str!("artifact_api.rs");
        assert!(src.contains("pub async fn save_artifact("), "{src}");
        assert!(src.contains("pub async fn save_artifact_versioned("), "{src}");
    }

    /// 【D6 导出】表格抽取：th/td 顺序、内部标签剥除、实体解码、空白折叠。
    #[test]
    fn extract_tables_reads_cells_and_decodes_entities() {
        let html = "<main><p>前文</p><table><tr><th>月份</th><th>销售额</th></tr>\
            <tr><td>七月 &amp; 八月</td><td><b>¥20608.48万</b></td></tr></table>\
            <table><tr><td> lone </td><td>x&lt;y</td></tr></table></main>";
        let tables = extract_tables(html);
        assert_eq!(tables.len(), 2, "{tables:?}");
        assert_eq!(tables[0][0], vec!["月份".to_string(), "销售额".to_string()], "{:?}", tables[0]);
        assert_eq!(tables[0][1], vec!["七月 & 八月".to_string(), "¥20608.48万".to_string()], "{:?}", tables[0]);
        assert_eq!(tables[1][0], vec!["lone".to_string(), "x<y".to_string()], "{:?}", tables[1]);
        // 无表格 → 空（端点据此 400「该产物不含表格」）
        assert!(extract_tables("<p>纯文本报告</p>").is_empty());
    }

    /// 单元格文本：<br> 是换行语义，剥掉要留空白（否则 "a<br>b" 导出成 "ab" 粘连）
    #[test]
    fn cell_text_keeps_br_as_whitespace() {
        assert_eq!(cell_text("a<br>b"), "a b");
        assert_eq!(cell_text("a<BR/>b"), "a b", "大小写与自闭合都要认");
        assert_eq!(cell_text("a<br>  b"), "a b", "多空白照常折叠");
        assert_eq!(cell_text("a<b>b</b>c"), "abc", "无语义标签照旧剥掉");
    }

    /// 单趟实体解码与旧「&amp; 最后解」逐字等价（含嵌套形态）
    #[test]
    fn decode_entities_single_pass_matches_legacy_order() {
        assert_eq!(decode_entities("&lt;"), "<");
        assert_eq!(decode_entities("&amp;"), "&");
        assert_eq!(decode_entities("&amp;lt;"), "&lt;", "嵌套只解一层");
        assert_eq!(decode_entities("&amp;amp;"), "&amp;");
        assert_eq!(decode_entities("x&lt;y &amp; z&quot;"), "x<y & z\"");
        assert_eq!(decode_entities("&unknown;"), "&unknown;", "未知实体原样保留");
        assert_eq!(decode_entities("无实体"), "无实体");
    }

    /// class 属性解析：等号两侧空白与无引号写法都认；data-class 不算
    #[test]
    fn class_values_accept_whitespace_around_equals() {
        assert_eq!(class_values(r#"<section class="sqlx x">"#), vec!["sqlx", "x"]);
        assert_eq!(class_values(r#"<section class = "sqlx">"#), vec!["sqlx"], "等号带空格");
        assert_eq!(class_values("<section class=sqlx>"), vec!["sqlx"], "无引号");
        assert_eq!(class_values(r#"<section class='sqlx'>"#), vec!["sqlx"], "单引号");
        assert!(class_values(r#"<section data-class="sqlx">"#).is_empty(), "data-class 不算");
        // 清洗端对端：带空格的 class 写法里的该删段不许漏
        let cleaned = remove_elements_with_class(r#"<p>留</p><div class = "sqlx"><p>删</p></div>"#, "sqlx");
        assert!(cleaned.contains('留') && !cleaned.contains('删'), "{cleaned}");
    }

    /// 导出护栏常量钉：行数 + 单元格总数（改数值的人必须读常量注释再想一遍）
    #[test]
    fn export_budget_constants_are_pinned() {
        assert_eq!(MAX_EXPORT_ROWS, 20_000);
        assert_eq!(MAX_EXPORT_CELLS, 200_000);
    }

    /// 【D6 导出】CSV：BOM、全量引号、内嵌引号翻倍、公式注入前置 `'`、多表空行分隔。
    #[test]
    fn csv_escapes_quotes_and_defangs_formula_cells() {
        let tables = vec![vec![
            vec!["=HYPERLINK(\"http://bad\")".to_string(), "+cmd".to_string(), "-2+3".to_string(), "@SUM(1)".to_string()],
            vec!["普通".to_string(), "含\"引号\"".to_string(), "含,逗号".to_string(), "12".to_string()],
        ]];
        let csv = to_csv(&tables);
        assert!(csv.starts_with('\u{feff}'), "缺 BOM：{csv:?}");
        let mut lines = csv.trim_start_matches('\u{feff}').lines();
        let header = lines.next().unwrap();
        assert!(header.contains("\"'=HYPERLINK(\"\"http://bad\"\")\""), "公式单元格必须前置 '：{header}");
        assert!(header.contains("\"'+cmd\"") && header.contains("\"'-2+3\"") && header.contains("\"'@SUM(1)\""), "{header}");
        let row = lines.next().unwrap();
        assert!(row.contains("\"含\"\"引号\"\"\""), "引号必须翻倍：{row}");
        assert!(row.contains("\"含,逗号\""), "{row}");
        assert!(csv.contains("\r\n"), "行尾必须 CRLF：{csv:?}");
        // 多表之间空一行
        let two = to_csv(&vec![vec![vec!["a".to_string()]], vec![vec!["b".to_string()]]]);
        assert!(two.contains("\"a\"\r\n\r\n\"b\"\r\n"), "{two:?}");
    }

    /// 【D6 导出】xlsx 手写件：CRC32 标准向量、列标进位、XML 转义、ZIP 五部件与 EOCD。
    #[test]
    fn xlsx_writer_emits_valid_zip_and_escaped_sheet() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926, "CRC32 标准向量不符");
        assert_eq!(col_letter(1), "A");
        assert_eq!(col_letter(26), "Z");
        assert_eq!(col_letter(27), "AA");
        assert_eq!(col_letter(703), "AAA");
        // inlineStr 结构性免疫公式注入：以 = 开头的文本原样落格，不需要 csv 的 ' 护栏
        let tables = vec![vec![
            vec!["=1+1".to_string(), "a<b&c\u{0007}".to_string()],
            vec!["七月".to_string(), "206,084".to_string()],
        ]];
        let sheet = worksheet_xml(&tables);
        assert!(sheet.contains("<row r=\"1\">") && sheet.contains("<c r=\"A1\" t=\"inlineStr\">"), "{sheet}");
        assert!(sheet.contains(">=1+1</t>"), "{sheet}");
        assert!(sheet.contains("a&lt;b&amp;c"), "XML 必须转义：{sheet}");
        assert!(!sheet.contains('\u{0007}'), "XML 1.0 非法控制字符必须剥除：{sheet}");
        assert!(sheet.contains("<c r=\"B2\" t=\"inlineStr\">"), "{sheet}");
        let bytes = build_xlsx(&tables);
        assert!(bytes.starts_with(b"PK\x03\x04"), "不是 ZIP 本地头");
        assert!(bytes.windows(4).any(|w| w == b"PK\x05\x06"), "缺 EOCD");
        for part in ["[Content_Types].xml", "_rels/.rels", "xl/workbook.xml", "xl/_rels/workbook.xml.rels", "xl/worksheets/sheet1.xml"] {
            assert!(bytes.windows(part.len()).any(|w| w == part.as_bytes()), "缺部件 {part}");
        }
    }

    /// 【D6 版本/导出/引用 端点形态】（源码扫描 —— 权限流要连库）：
    /// ① 三端点全走 load_versioned/load 的属主判据；② view/download/export 支持 ?version=N；
    /// ③ promote 核目标会话属主、事件走 chat::save_msg、kind 标记固定。
    #[test]
    fn d6_endpoints_keep_owner_gates_and_event_shape() {
        let src = include_str!("artifact_api.rs");
        let versions = src.split("pub async fn versions(").nth(1).expect("versions 没了");
        assert!(versions.contains("load(&st, &h, &q, id).await?"), "版本链列表少了属主校验：{versions}");
        assert!(versions.contains("CHAIN_SQL"), "{versions}");
        for name in ["pub async fn view(", "pub async fn download(", "pub async fn export("] {
            let body = src.split(name).nth(1).unwrap_or_else(|| panic!("{name} 没了"));
            assert!(body.contains("load_versioned"), "{name} 必须走版本解析 + 属主判据");
        }
        let export = src.split("pub async fn export(").nth(1).expect("export 没了");
        assert!(export.contains("secure_artifact_html"), "导出必须过同一条安全收口：{export}");
        assert!(export.contains("该产物不含表格，无法导出"), "{export}");
        let promote = src.split("pub async fn promote(").nth(1).expect("promote 没了");
        assert!(promote.contains("load_versioned(&st, &h, &q, id, req.version).await?"), "promote 少了产物读权限：{promote}");
        assert!(promote.contains("conv_owner") && promote.contains("Some(login.as_str())"), "promote 必须核目标会话属主：{promote}");
        assert!(promote.contains("只能引用到自己的会话"), "{promote}");
        assert!(promote.contains("chat::save_msg"), "事件写口必须复用 chat::save_msg：{promote}");
        assert!(promote.contains("\"kind\": \"artifact_promote\"") || promote.contains("\"artifact_promote\""), "{promote}");
        // 接线契约注释：main.rs 不在本文件改动范围，路由形状钉在文件头
        let head = src.split("//! 【D6 产物层增强】").nth(1).expect("文件头 D6 契约没了");
        assert!(head.contains("/api/artifact/{id}/versions") && head.contains("/api/artifact/{id}/export") && head.contains("/api/artifact/{id}/promote"), "{head}");
    }

    /// promote 备注：控制字符换空格、按字符限长 200。
    #[test]
    fn promote_note_is_sanitized() {
        assert_eq!(sanitize_promote_note("看\n这个\t数"), "看 这个 数");
        assert_eq!(sanitize_promote_note("  前后  "), "前后");
        let long = "长".repeat(500);
        assert_eq!(sanitize_promote_note(&long).chars().count(), 200);
    }
}
