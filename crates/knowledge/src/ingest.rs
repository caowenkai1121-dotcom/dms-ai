//! 上传入库编排：校验 → 写权限 → 去重 → 落盘 → 解析 → 分块 → 向量，**每步落库，失败可查**。
//! 变更原因＝状态机。
//!
//! **类型白名单与大小上限只有这一处实现**（`classify`）——server 侧不许有第二份，
//! 否则两处口径一旦分叉，被绕过的那份就是漏洞。

use crate::store::{self, CharSpan, DocInsert, DocStatus, NewDoc};
use crate::tabular::{self, TabularSource};
use crate::{acl, KbError, Viewer};
use dms_connector::doc::{Block, Chunk, DocService, ParsedDoc, Sheet};
use dms_connector::embed::{to_pgvector, EmbedClient, EmbedMode};
use dms_connector::owned::OwnedStore;
use std::future::Future;
use std::pin::Pin;

/// 分块参数：贴 bge-small-zh-v1.5 的 512 窗口（裁决：700/80 → 400/60，块尾会被静默截断）
const TARGET_TOKENS: i32 = 400;
const OVERLAP: i32 = 60;
/// 单个 sheet 进文本索引的行上限（完整数据走 K4 的表格通道）
const SHEET_ROWS: usize = 500;

pub struct UploadReq<'a> {
    pub space_id: &'a str,
    pub folder_id: Option<&'a str>,
    pub file_name: &'a str,
    pub mime: &'a str,
    pub bytes: &'a [u8],
    /// 分块策略（可选）：general/qa/book/laws/semantic；None = general（与历史行为一致）
    pub preset: Option<&'a str>,
}

const IMAGE_OCR_FALLBACK_NOTICE: &str = "运行时图片识别未返回有效结果，已使用文档解析 OCR";

pub type ImageOcrFuture<'a> = Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;

/// server 注入运行时视觉路由；knowledge 只接收 OCR 正文，不依赖 LLM 实现与凭据。
pub trait ImageOcr: Send + Sync {
    fn recognize<'a>(&'a self, file_name: &'a str, mime: &'a str, bytes: &'a [u8]) -> ImageOcrFuture<'a>;
    /// 图片含义（一两句语义描述，不是 OCR 正文）：导图/检索要回答「这张图是什么」。
    /// 默认恒 None——没有视觉通道的实现（tesseract 降级等）不受影响。
    fn describe<'a>(&'a self, file_name: &'a str, mime: &'a str, bytes: &'a [u8]) -> ImageOcrFuture<'a> {
        let _ = (file_name, mime, bytes);
        Box::pin(async { None })
    }
}

pub struct IngestCfg {
    /// 落盘根目录（例 `data/kb`）
    pub root: std::path::PathBuf,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Pdf,
    Docx,
    Xlsx,
    Csv,
    Pptx,
    Text,
    /// 旧二进制 Office（.doc/.xls/.ppt）：文档服务侧走 LibreOffice headless 转新格式再解析。
    LegacyOffice,
    /// 图片：运行时视觉模型优先，文档服务本地 OCR 降级。
    Image,
}

impl FileKind {
    /// 表格类型判定。**通道②的实际分派不看它**（看 `ParsedDoc.sheets` 非空，见 `run`）——
    /// 留着是给调用方做「这文件大概会建表」的预判，不是入库链上的开关。
    pub fn is_tabular(self) -> bool {
        matches!(self, FileKind::Xlsx | FileKind::Csv)
    }
}

/// 扩展名白名单——**唯一一份**。落盘用的扩展名也从这张表取（静态字面量，天然防路径穿越）。
///
/// 🔴 这张表曾只有 7 项，而文档服务侧（`tools/embed_service.py` 的 `CAPS`）已经支持 19 项 ——
/// 于是新增的 12 个扩展名（旧 Office 3 个 + 图片 7 个 + xlsm/markdown）在**产品唯一入口**
/// `classify` 里就被 400 拒掉，只有直接 `POST /parse` 才走得到。
/// 也就是「解析器支持 PPT」与「用户能上传 PPT」是两件事，中间这张表是那道门。
/// 判据 `exts_cover_the_doc_service_capabilities` 钉住两张表的**集合相等**：
/// 加格式必须两侧同加（本轮对齐上传全清单：json/log/html 文本族 + gif 图片，共 23 项）。
const EXTS: [(&str, FileKind); 23] = [
    ("pdf", FileKind::Pdf),
    ("docx", FileKind::Docx),
    ("xlsx", FileKind::Xlsx),
    ("xlsm", FileKind::Xlsx),
    ("csv", FileKind::Csv),
    ("pptx", FileKind::Pptx),
    ("md", FileKind::Text),
    ("markdown", FileKind::Text),
    ("txt", FileKind::Text),
    // json/log/html：纯文本族（json 转代码块、html 去标签，全在文档服务 `_p_json`/`_p_html`）
    ("json", FileKind::Text),
    ("log", FileKind::Text),
    ("html", FileKind::Text),
    // 旧二进制 Office：文档服务用 LibreOffice headless 转成新格式再解析
    ("doc", FileKind::LegacyOffice),
    ("xls", FileKind::LegacyOffice),
    ("ppt", FileKind::LegacyOffice),
    // 图片：运行时视觉模型优先，文档服务本地 OCR 降级
    ("png", FileKind::Image),
    ("jpg", FileKind::Image),
    ("jpeg", FileKind::Image),
    ("bmp", FileKind::Image),
    ("gif", FileKind::Image),
    ("tif", FileKind::Image),
    ("tiff", FileKind::Image),
    ("webp", FileKind::Image),
];

/// 纯函数校验：扩展名白名单 + 大小上限。不看内容（内容由文档服务判，见 `DocError::Unsupported`）。
pub fn classify(file_name: &str, len: u64, max_bytes: u64) -> Result<FileKind, KbError> {
    if len == 0 {
        return Err(KbError::BadInput("文件为空".into()));
    }
    if len > max_bytes {
        // 一位小数：整除 MB 在非对齐/不足 1MB 时会打出「文件 0 MB 超过上限 0 MB」式误导
        return Err(KbError::BadInput(format!(
            "文件 {:.1} MB 超过上限 {:.1} MB",
            len as f64 / 1_048_576.0,
            max_bytes as f64 / 1_048_576.0
        )));
    }
    match lookup(file_name) {
        Some((_, kind)) => Ok(kind),
        // 支持清单从 EXTS 生成：手抄清单曾与表白名单漂移（markdown 在表里、文案里没有）
        None => Err(KbError::BadInput(format!(
            "不支持的文件类型 .{}（支持：{}）",
            ext_of(file_name),
            EXTS.iter().map(|(e, _)| *e).collect::<Vec<_>>().join("/")
        ))),
    }
}

fn ext_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((_, ext)) => ext.to_ascii_lowercase(),
        None => String::new(),
    }
}

fn lookup(file_name: &str) -> Option<(&'static str, FileKind)> {
    // 零分配比较：白名单全小写，`eq_ignore_ascii_case` 省一次小写 String
    let (_, ext) = file_name.rsplit_once('.')?;
    EXTS.iter().find(|(e, _)| e.eq_ignore_ascii_case(ext)).map(|(e, k)| (*e, *k))
}

/// 磁盘路径 `<root>/<doc_id>.<白名单扩展名>`。
/// **原始文件名一个字都不进路径**——它是不可信输入（`..\..\` / 绝对路径 / NUL）。
fn doc_path(cfg: &IngestCfg, doc_id: &str, file_name: &str) -> std::path::PathBuf {
    let ext = lookup(file_name).map(|(e, _)| e).unwrap_or("bin");
    cfg.root.join(format!("{doc_id}.{ext}"))
}

#[derive(Debug, PartialEq, Eq)]
struct InferredDocVersion {
    family: String,
    revision: String,
}

/// 推断出的文档族/版本号长度上限（字符）：防把整段长文件名当版本尾缀吞进来
const MAX_FAMILY_CHARS: usize = 120;
const MAX_REVISION_CHARS: usize = 60;

/// 只识别明确版本尾缀；普通数字、名称中的字母 v 或“最新版”等模糊词一律不猜。
fn infer_doc_version(file_name: &str) -> Option<InferredDocVersion> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem).trim();
    let mut found = None;
    for (index, _) in stem.char_indices() {
        let raw = stem[index..].trim();
        // 候选位置快筛（等价收敛，省长文件名的近 O(n²)）：只有「前一字符是分隔符 /
        // 以『第』开头 / 以括号开头」的位置才可能命中，其余位置跑了也白跑
        let preceded = stem[..index]
            .chars()
            .next_back()
            .is_some_and(|c| matches!(c, '_' | '-' | ' ' | '(' | '（' | '[' | '【'));
        if !preceded && !raw.starts_with('第') && !raw.starts_with(['(', '（', '[', '【']) {
            continue;
        }
        let token = unwrap_version_token(raw);
        if !is_version_token(token) {
            continue;
        }
        let direct_edition = token.starts_with('第');
        let wrapped = raw.len() != token.len();
        if !direct_edition && !preceded && !wrapped {
            continue;
        }
        let family = stem[..index]
            .trim_end_matches(|c: char| matches!(c, '_' | '-' | '.' | ' ' | '(' | '（' | '[' | '【'))
            .trim();
        if family.is_empty()
            || family.chars().count() > MAX_FAMILY_CHARS
            || token.chars().count() > MAX_REVISION_CHARS
        {
            continue;
        }
        found = Some(InferredDocVersion {
            family: family.to_string(),
            revision: token.to_string(),
        });
    }
    found
}

fn unwrap_version_token(raw: &str) -> &str {
    let s = raw.trim();
    for (open, close) in [('(', ')'), ('（', '）'), ('[', ']'), ('【', '】')] {
        if s.starts_with(open) && s.ends_with(close) {
            let start = open.len_utf8();
            let end = s.len() - close.len_utf8();
            return s[start..end].trim();
        }
    }
    s
}

fn is_version_token(token: &str) -> bool {
    // 迭代器/切片写法，不分配 Vec<char>（'v'/'V' 是 ASCII，strip 后下标安全）
    if let Some(rest) = token.strip_prefix(['v', 'V']) {
        return rest.chars().next().is_some_and(|c| c.is_ascii_digit())
            && rest.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
            && rest.ends_with(|c: char| c.is_ascii_digit());
    }
    if token.starts_with('第') && token.ends_with('版') {
        // 各剥一次：`trim_start_matches('第')` 会把「第第3版」误剥成合法版本尾缀
        let middle = token.strip_prefix('第').and_then(|t| t.strip_suffix('版')).unwrap_or("");
        return !middle.is_empty()
            && middle.chars().all(|c| c.is_ascii_digit() || "一二三四五六七八九十百".contains(c));
    }
    let separator = if token.contains('-') { '-' } else if token.contains('.') { '.' } else { return false };
    let parts: Vec<&str> = token.split(separator).collect();
    parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|part| part.chars().all(|c| c.is_ascii_digit()))
}

/// `UploadReq` 的 owning 形态：重活进 `tokio::spawn` 要 `'static`，请求作用域的借用过不去。
/// 字段与 `UploadReq` 一一对应（`as_req` 原位借回，入库链本体不感知所有权差异）。
#[derive(Debug, Clone)]
pub struct OwnedUploadReq {
    pub space_id: String,
    pub folder_id: Option<String>,
    pub file_name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
    /// 分块策略（可选）：口径同 `UploadReq.preset`
    pub preset: Option<String>,
}

impl OwnedUploadReq {
    fn from_req(req: &UploadReq<'_>) -> Self {
        Self {
            space_id: req.space_id.to_string(),
            folder_id: req.folder_id.map(str::to_string),
            file_name: req.file_name.to_string(),
            mime: req.mime.to_string(),
            bytes: req.bytes.to_vec(),
            preset: req.preset.map(str::to_string),
        }
    }

    fn as_req(&self) -> UploadReq<'_> {
        UploadReq {
            space_id: &self.space_id,
            folder_id: self.folder_id.as_deref(),
            file_name: &self.file_name,
            mime: &self.mime,
            bytes: &self.bytes,
            preset: self.preset.as_deref(),
        }
    }
}

/// `prepare` 的产物。`doc_id` 立即可回给前端轮询；`job = None` ＝ 去重秒传复用
///（那次上传已入库或正在跑，没有重活要跑，物理表数据源也在那次登记过）。
/// `replaced` ＝ 同名覆盖命中文档名（同目录同名不同内容）：既有 doc_id 原地重建，
/// 前端上传队列行据它显示「已覆盖旧版本」；非覆盖恒 `None`。
pub struct Prepared {
    pub doc_id: String,
    pub job: Option<IngestJob>,
    pub replaced: Option<String>,
}

/// 入库重活（parse → chunk → embed → 状态推进）的全部上下文，owning 形态可直接 move 进
/// `tokio::spawn`。三个变体对应本文件既有的三条链，语义与各链的同步执行逐字一致。
pub enum IngestJob {
    /// 新文档首入：`run` 链（解析 → 分块 → 向量 → 通道②）；失败文案落库。
    Fresh { req: OwnedUploadReq, kind: FileKind },
    /// 原地重建（去重命中 failed/chunked、重建端点、启动自愈）：`reprocess` 影子链，
    /// 失败时线上版本原样保留。kind 不随任务走——`reprocess` 内部会再 `classify` 一次
    ///（类型白名单只有那一处实现，信任边界上不抄第二份）。
    Rebuild { req: OwnedUploadReq },
    /// 同名覆盖（同目录同名不同内容）：`overwrite` 影子链——新内容灌进既有 doc_id，
    /// 目录归属/授权/标签/元数据全部保留；文件与索引先建后切，失败时旧版本保持可检索；
    /// 通道②物理表随新内容重建（旧数据源退役由 server 在任务收尾完成）。
    Overwrite { req: OwnedUploadReq },
}

/// 上传入库快路径（请求内同步完成）：校验 → 建目录 → 空间/写权限 → 目录解析 → sha 去重 →
/// 建行 → 落盘。重活装进 `IngestJob` 交调用方后台执行——大文件解析 50s+，请求里同步 await
/// 会被浏览器/nginx 超时断连把 handler future 直接 drop，入库半路取消、文档永卡 parsing。
pub async fn prepare(
    st: &OwnedStore,
    v: &Viewer,
    cfg: &IngestCfg,
    req: UploadReq<'_>,
) -> Result<Prepared, KbError> {
    // kind 只用于校验：通道②的分派按**解析结果**（`parsed.sheets` 非空）走，不按扩展名——
    // 文档服务才知道一个 .csv 里到底有没有表格。kind 随调用链下传，parse_input 不再二次查表。
    let kind = classify(req.file_name, req.bytes.len() as u64, cfg.max_bytes)?;
    // 落盘根目录在入口统一建一次（run/build_shadow 内不再各建一遍）
    tokio::fs::create_dir_all(&cfg.root).await.map_err(io_err)?;
    // 个人空间首次上传时尚不存在，先幂等创建；若同名空间已由别人持有，ON CONFLICT
    // 不会改 owner，下面的真实 owner/ACL 判定仍会拒绝，不能靠字符串碰撞越权。
    if req.space_id == v.login {
        store::ensure_space(st, req.space_id, &v.login).await?;
    }
    if !acl::space_writable(st, v, req.space_id).await? {
        return Err(KbError::Forbidden(format!("无权向知识空间 {} 写入", req.space_id)));
    }
    let (folder_id, _) = store::resolve_folder(st, req.space_id, req.folder_id).await?;
    let sha = store::sha256_hex(st, req.bytes).await?;
    if let Some(existing) = store::find_by_sha(st, req.space_id, &sha).await? {
        return dedup_dispatch(st, &req, existing).await;
    }
    // 同名覆盖（「文件相同时，可以覆盖之前的文件」）：目标目录内已有同名文档且内容不同 →
    // 新内容灌进既有 doc_id（影子链原地重建），不产生第二篇同名文档。顺序刻意排在 sha 去重
    // 之后：同内容（哪怕落在别的目录/别的名下）维持秒传复用——(space_id, sha256) 唯一约束
    // 下，同内容全空间只能有一篇，这是唯一不撞唯一约束的走向。
    if let Some(existing) =
        store::find_by_name_in_folder(st, req.space_id, folder_id.as_deref(), req.file_name).await?
    {
        return overwrite_dispatch(st, &req, existing).await;
    }
    let new = NewDoc {
        space_id: req.space_id,
        folder_id: folder_id.as_deref(),
        name: req.file_name,
        mime: req.mime,
        bytes: req.bytes.len() as i64,
        sha256: &sha,
        uploaded_by: &v.login,
        writer_roles: &v.roles,
    };
    let doc_id = match store::insert_doc(st, &new).await? {
        DocInsert::New(id) => id,
        // find_by_sha 之后被并发上传抢占（同空间同 hash）：走同一套秒传/重建分派，
        // 不重复建行，也不重复消耗解析与向量（B7）。
        DocInsert::Duplicate(existing) => return dedup_dispatch(st, &req, existing).await,
    };
    try_apply_inferred_version(st, v, &doc_id, req.file_name).await;
    // 先推进到 parsing 再交还：响应带回去的 doc 行得是进行态，且启动自愈的扫描
    //（parsing/chunked）才能覆盖「建行后、重活起跑前」进程死亡的窗口。
    store::set_status(st, v, &doc_id, DocStatus::Parsing, "").await?;
    // 落盘也在快路径完成：doc 行一旦可见，文件必定可读（下载/预览/自愈都不用等后台）。
    // `run` 里的重复写是同字节幂等覆盖，保留它是因为那条链对自己落盘负有完整契约。
    let path = doc_path(cfg, &doc_id, req.file_name);
    if let Err(e) = tokio::fs::write(&path, req.bytes).await {
        // 落盘失败即终态（请求侧也拿到 500）：不留 parsing 僵尸等下次启动自愈收尸
        let e = io_err(e);
        let _ = store::set_status(st, v, &doc_id, DocStatus::Failed, &e.to_string()).await;
        return Err(e);
    }
    Ok(Prepared {
        doc_id,
        job: Some(IngestJob::Fresh { req: OwnedUploadReq::from_req(&req), kind }),
        replaced: None,
    })
}

/// 入库重活分派（后台任务里跑）。`Fresh` 失败把文案落库（用户在文档列表里看得见）；
/// `Rebuild`/`Overwrite` 走影子构建，失败时线上版本原样保留，且把本次失败
/// 另行落库（不改线上 `status`），让列表可见但旧版本仍可检索。
/// 返回通道②数据源（Fresh/Overwrite 且新内容真是表格时非空）——登记/授权是 server 侧的事，
/// 随任务后台化。Overwrite 返回 `None` 表示新内容无表：旧版本登记过的数据源由调用方退役。
pub async fn run_job(
    st: &OwnedStore,
    doc: &DocService,
    embed: &EmbedClient,
    v: &Viewer,
    cfg: &IngestCfg,
    doc_id: &str,
    job: IngestJob,
    image_ocr: Option<&dyn ImageOcr>,
) -> Result<Option<TabularSource>, KbError> {
    match job {
        IngestJob::Fresh { req, kind } => {
            match run(st, doc, embed, v, cfg, &req.as_req(), doc_id, image_ocr, kind).await {
                Ok(source) => Ok(source),
                Err(e) => {
                    // 不许静默成功：失败文案落库，用户在文档列表里看得见
                    if let Err(se) =
                        store::set_status(st, v, doc_id, DocStatus::Failed, &e.to_string()).await
                    {
                        tracing::warn!(doc_id, error = %se, "失败文案落库也失败（用户在列表里看不到原因）");
                    }
                    Err(e)
                }
            }
        }
        IngestJob::Rebuild { req } => {
            match reprocess(st, doc, embed, v, cfg, req.as_req(), doc_id, image_ocr).await {
                Ok(()) => Ok(None),
                Err(e) => {
                    persist_shadow_failure(st, v, doc_id, &e).await;
                    Err(e)
                }
            }
        }
        IngestJob::Overwrite { req } => {
            match overwrite(st, doc, embed, v, cfg, req.as_req(), doc_id, image_ocr).await {
                Ok(source) => Ok(source),
                Err(e) => {
                    persist_shadow_failure(st, v, doc_id, &e).await;
                    Err(e)
                }
            }
        }
    }
}

async fn persist_shadow_failure(st: &OwnedStore, viewer: &Viewer, doc_id: &str, error: &KbError) {
    let message = dms_kernel::qalog::sanitize(&error.to_string());
    if let Err(e) = store::set_last_ingest_error(st, viewer, doc_id, &message).await {
        tracing::warn!(doc_id, error = %e, "影子入库失败状态落库失败（旧版本仍保留）");
    }
}

/// 版本元数据自动补全（推断 + 落库）：ingest 与 reprocess 同一形态，只此一份。
/// 失败只 warn（元数据不挡主链），但 warn 必须带 error——吞掉错误本体排障拿不到原因。
async fn try_apply_inferred_version(st: &OwnedStore, v: &Viewer, doc_id: &str, file_name: &str) {
    if let Some(version) = infer_doc_version(file_name) {
        if let Err(e) =
            store::apply_inferred_doc_version(st, v, doc_id, &version.family, &version.revision).await
        {
            tracing::warn!(doc_id, error = %e, "文档版本元数据自动补全失败");
        }
    }
}

/// 同内容（hash）命中的统一分派（B7 秒传去重）。
/// `embedded` 直接复用；`pending/parsing` 说明另一次上传正在跑——复用句柄而不是重复扣解析/向量
/// （进程崩溃留下的僵尸「处理中」由启动自愈兜底：自愈直连重建链、不经过本分派，否则
/// 会被这里的 Reuse 原样弹回）；
/// `failed/chunked` 重传真重跑（影子构建随 `job` 交后台，失败时旧块原样保留），重试不重复建行。
async fn dedup_dispatch(
    st: &OwnedStore,
    req: &UploadReq<'_>,
    existing: String,
) -> Result<Prepared, KbError> {
    let row = store::get_doc(st, &existing).await?;
    // 并发删除：find_by_sha 命中后文档没了——直接 NotFound；走 Rebuild 最终会报
    // 「写权限已失效」，语义误导
    let Some(row) = row else {
        return Err(KbError::NotFound(format!("文档 {existing} 已不存在")));
    };
    match dedup_action(Some(row.status.as_str())) {
        // 那次上传已登记过物理表数据源，这边没有重活要跑
        DedupAction::Reuse => Ok(Prepared { doc_id: existing, job: None, replaced: None }),
        DedupAction::Reprocess => Ok(Prepared {
            doc_id: existing,
            job: Some(IngestJob::Rebuild { req: OwnedUploadReq::from_req(req) }),
            replaced: None,
        }),
    }
}

/// dedup 命中文档的处置。未知/缺失状态一律 `Reprocess`（保守：重建是幂等影子切换）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DedupAction {
    Reuse,
    Reprocess,
}

fn dedup_action(status: Option<&str>) -> DedupAction {
    match status.and_then(DocStatus::parse) {
        Some(DocStatus::Embedded | DocStatus::Pending | DocStatus::Parsing) => DedupAction::Reuse,
        _ => DedupAction::Reprocess,
    }
}

/// 同名不同内容命中的统一分派（覆盖上传）。
/// 进行态（pending/parsing）说明该文档的首入链还在跑——此时覆盖会与在跑的 `insert_chunks`
/// 互踩（那条链 `ON CONFLICT` 不重入），拒绝并要求稍后重试；其余状态一律 `Overwrite`
/// （影子构建原地重建，失败时旧版本原样保留；`failed` 文档由此获得「重传同名文件」的
/// 自然重试路径）。
async fn overwrite_dispatch(
    st: &OwnedStore,
    req: &UploadReq<'_>,
    existing: String,
) -> Result<Prepared, KbError> {
    let row = store::get_doc(st, &existing).await?;
    // 与 dedup_dispatch 同一个并发删除窗口：命中后文档没了直接 NotFound
    let Some(row) = row else {
        return Err(KbError::NotFound(format!("文档 {existing} 已不存在")));
    };
    match overwrite_action(Some(row.status.as_str())) {
        OverwriteAction::Conflict => Err(KbError::BadInput(format!(
            "同名文档「{}」正在入库中，请待其完成后重试",
            req.file_name
        ))),
        OverwriteAction::Overwrite => Ok(Prepared {
            doc_id: existing,
            job: Some(IngestJob::Overwrite { req: OwnedUploadReq::from_req(req) }),
            replaced: Some(req.file_name.to_string()),
        }),
    }
}

/// 同名命中文档的处置。进行态 `Conflict`；其余（含未知/缺失状态）一律 `Overwrite`——
/// 覆盖链是全量影子切换，对任何起始状态都安全。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverwriteAction {
    Overwrite,
    Conflict,
}

fn overwrite_action(status: Option<&str>) -> OverwriteAction {
    match status.and_then(DocStatus::parse) {
        Some(DocStatus::Pending | DocStatus::Parsing) => OverwriteAction::Conflict,
        _ => OverwriteAction::Overwrite,
    }
}

/// 对已存在文档原地重建。调用者先做 ACL 与原始文件读取；本函数只负责状态机与入库编排。
/// 表格物理表属于同一原文件，索引重建不重复灌数（已登记的数据源原样保留），故无返回值。
pub async fn reprocess(
    st: &OwnedStore,
    doc: &DocService,
    embed: &EmbedClient,
    viewer: &Viewer,
    cfg: &IngestCfg,
    req: UploadReq<'_>,
    doc_id: &str,
    image_ocr: Option<&dyn ImageOcr>,
) -> Result<(), KbError> {
    let kind = classify(req.file_name, req.bytes.len() as u64, cfg.max_bytes)?;
    // 落盘根目录在入口统一建一次（build_shadow 内不再重建）
    tokio::fs::create_dir_all(&cfg.root).await.map_err(io_err)?;
    // CAS 的 expected 文本必须按**库内** name/folder_path 生成（`replace_chunks` 用库内值复核）：
    // 用请求侧值时，改名/换目录后重传同内容文件会让 expected 全部失配、向量被迫补算
    let current = store::get_doc(st, doc_id)
        .await?
        .ok_or_else(|| KbError::NotFound(format!("文档 {doc_id} 不存在")))?;
    let stage_id = uuid::Uuid::new_v4().to_string();
    let staged_path = doc_path(cfg, &stage_id, req.file_name);
    let staged = build_shadow(doc, embed, &req, &current, &staged_path, image_ocr, kind).await;
    let built = match staged {
        Ok(v) => v,
        Err(e) => {
            remove_staged_file(&staged_path, doc_id).await;
            return Err(e);
        }
    };
    // 纯重建通常不换原件；但 failed 文档可能只剩 DB 元数据、原件已丢。此时同 hash
    // 重传的影子文件就是可恢复原件：先原子补回 live，再切索引；live 尚在则只清 stage。
    let live_path = doc_path(cfg, doc_id, &current.name);
    let _switch_guard = FILE_SWITCH_LOCK.lock().await;
    restore_missing_live_file(&staged_path, &live_path, doc_id).await?;
    store::replace_chunks(
        st,
        viewer,
        doc_id,
        &built.chunks,
        &built.embedding_texts,
        &built.embeddings,
        &built.spans,
        built.page_count,
        built.status,
        &built.error,
        &built.notice,
        // 纯重建：文件元数据三列（sha256/bytes/mime）原样保留
        None,
    )
    .await?;
    try_apply_inferred_version(st, viewer, doc_id, req.file_name).await;
    Ok(())
}

/// `reprocess` 的原件自愈：live 已存在时保持分毫不动；缺失时把同 hash 的影子文件原子
/// 提升为 live。任一文件操作失败都清 stage，且发生在 DB 影子切换前，故不改变切库语义。
async fn restore_missing_live_file(
    staged_path: &std::path::Path,
    live_path: &std::path::Path,
    doc_id: &str,
) -> Result<(), KbError> {
    match tokio::fs::metadata(live_path).await {
        Ok(_) => {
            remove_staged_file(staged_path, doc_id).await;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Err(e) = tokio::fs::rename(staged_path, live_path).await {
                remove_staged_file(staged_path, doc_id).await;
                return Err(io_err(e));
            }
            Ok(())
        }
        Err(e) => {
            remove_staged_file(staged_path, doc_id).await;
            Err(io_err(e))
        }
    }
}

/// staged 临时文件的收尾清理：文件可能根本没写出来（build_shadow 在写盘前就失败），
/// NotFound 不算事，其余留诊断。
async fn remove_staged_file(path: &std::path::Path, doc_id: &str) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(doc_id, error = %e, "staged 临时文件删除失败（会遗留 stage 文件）");
        }
    }
}

/// 文件 → DB 切换临界区的进程内互斥：Overwrite 恒换文件，Rebuild 在 live 缺失时补文件；
/// 两者任一与另一任务的 `replace_chunks` 交错，都会把文件和 DB 元数据/索引钉成两半状态。
/// 临界区只有一次文件收尾 + 一条 SQL +（覆盖表格时）建表灌数，上传闸本就只放 4 并发，
/// 全局串行的代价可忽略。
static FILE_SWITCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 同名覆盖的后台重建（`IngestJob::Overwrite`）：影子构建 → 换文件 → 一条语句切换索引与
/// 文件元数据 → 通道②随新内容重建。与 `reprocess` 同族的失败语义：影子切换之前任何一步
/// 失败，旧版本的文件/索引/数据源全都原样保留、保持可检索。
///
/// 与 `reprocess` 的差异全在「内容变了」：磁盘文件要换（同目录 rename 原子替换）、
/// sha256/bytes/mime 要随索引同语句切换、通道②物理表要按新内容重建。
/// 返回通道②新数据源（新内容无表时 `None`，旧数据源由 server 在任务收尾退役）。
async fn overwrite(
    st: &OwnedStore,
    doc: &DocService,
    embed: &EmbedClient,
    viewer: &Viewer,
    cfg: &IngestCfg,
    req: UploadReq<'_>,
    doc_id: &str,
    image_ocr: Option<&dyn ImageOcr>,
) -> Result<Option<TabularSource>, KbError> {
    let kind = classify(req.file_name, req.bytes.len() as u64, cfg.max_bytes)?;
    // 落盘根目录在入口统一建一次（build_shadow 内不再重建）
    tokio::fs::create_dir_all(&cfg.root).await.map_err(io_err)?;
    // CAS 口径同 reprocess：expected 按**库内** name/folder_path 生成（覆盖不动目录归属）
    let current = store::get_doc(st, doc_id)
        .await?
        .ok_or_else(|| KbError::NotFound(format!("文档 {doc_id} 不存在")))?;
    let stage_id = uuid::Uuid::new_v4().to_string();
    let staged_path = doc_path(cfg, &stage_id, req.file_name);
    let staged = build_shadow(doc, embed, &req, &current, &staged_path, image_ocr, kind).await;
    let built = match staged {
        Ok(v) => v,
        Err(e) => {
            // 影子构建失败：旧版本分毫未动，只欠临时文件清理
            remove_staged_file(&staged_path, doc_id).await;
            return Err(e);
        }
    };
    // 切换前两步都可以失败，失败后旧版本仍完整：sha 先算（此刻 live 文件还是旧内容，
    // 库内文件元数据也还是旧值，两半一致）；文件先换（旧原件暂存回滚副本）再切库——
    // 顺序不许反：先切库会把「新 sha + 旧文件」钉进库里，同内容重传被 sha 去重秒传弹回，
    // 旧文件永无自愈；先换文件时切换若失败（撤权/抖动），当场恢复回滚副本，
    // 不留下“旧索引 + 新原件”的错版。
    let sha = store::sha256_hex(st, req.bytes).await?;
    // 临界区全程互斥（并发覆盖同一文档的交错防护，见锁定义注释）；守卫随作用域/早退释放
    let _switch_guard = FILE_SWITCH_LOCK.lock().await;
    let live_path = doc_path(cfg, doc_id, req.file_name);
    let rollback_path = install_staged_file(&staged_path, &live_path, doc_id).await?;
    let meta = store::DocFileMeta {
        sha256: &sha,
        bytes: req.bytes.len() as i64,
        mime: req.mime,
    };
    let switched = store::replace_chunks(
        st,
        viewer,
        doc_id,
        &built.chunks,
        &built.embedding_texts,
        &built.embeddings,
        &built.spans,
        built.page_count,
        built.status,
        &built.error,
        &built.notice,
        Some(&meta),
    )
    .await;
    if let Err(e) = switched {
        if let Err(restore_error) =
            restore_live_file(&live_path, rollback_path.as_deref(), doc_id).await
        {
            return Err(KbError::Db(format!(
                "影子索引切换失败（{e}），且旧原件回滚失败：{restore_error}"
            )));
        }
        return Err(e);
    }
    if let Some(path) = rollback_path {
        remove_rollback_file(&path, doc_id).await;
    }
    // 通道② 排在索引切换之后：切换失败时旧数据源必须还在服役（旧版本保持可检索/可问数）。
    // 版本元数据不重新推断——覆盖保留原文档元数据，名字没变，推断结果也不会变。
    match overwrite_tabular(st, viewer, &current.space_id, doc_id, &built.sheets).await {
        Ok(source) => Ok(source),
        Err(e) => {
            // 文件 + chunks 已原子提交，这里已越过 commit boundary，不能再宣称
            // “旧版本保留”或走 last_ingest_error。通道②失败只降级提示，新文本版仍可检索。
            let msg = format!(
                "新版文档已可检索，但问数数据源重建失败：{}",
                dms_kernel::qalog::sanitize(&e.to_string())
            );
            if let Err(notice_error) = store::append_notice(st, viewer, doc_id, &msg).await {
                tracing::warn!(doc_id, error = %notice_error, "覆盖后问数降级提示落库失败");
            }
            tracing::warn!(doc_id, error = %e, "同名覆盖已切换新版，问数数据源重建失败");
            Ok(None)
        }
    }
}

/// 先把旧 live 移到同目录回滚副本，再将 stage 提升为 live。不直接覆盖是为了
/// 在紧随其后的 DB 影子切换失败时还能精确恢复旧原件，也兼容 Windows rename 不覆盖目标。
async fn install_staged_file(
    staged_path: &std::path::Path,
    live_path: &std::path::Path,
    doc_id: &str,
) -> Result<Option<std::path::PathBuf>, KbError> {
    let rollback_path = live_path.with_file_name(format!(
        ".{}.rollback-{}",
        live_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or(doc_id),
        uuid::Uuid::new_v4()
    ));
    let rollback = match tokio::fs::rename(live_path, &rollback_path).await {
        Ok(()) => Some(rollback_path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            remove_staged_file(staged_path, doc_id).await;
            return Err(io_err(e));
        }
    };
    if let Err(e) = tokio::fs::rename(staged_path, live_path).await {
        let switch_error = io_err(e);
        if let Some(path) = rollback.as_deref() {
            if let Err(restore_error) = tokio::fs::rename(path, live_path).await {
                return Err(KbError::Db(format!(
                    "新原件切换失败（{switch_error}），且旧原件恢复失败：{restore_error}"
                )));
            }
        }
        remove_staged_file(staged_path, doc_id).await;
        return Err(switch_error);
    }
    Ok(rollback)
}

/// DB 切换失败：先移除已提升的新 live，再把旧原件放回原位。恢复失败不能吞，
/// 否则日志会误报成普通 DB 抖动，而实际已变成文件/索引不一致的高危状态。
async fn restore_live_file(
    live_path: &std::path::Path,
    rollback_path: Option<&std::path::Path>,
    doc_id: &str,
) -> Result<(), KbError> {
    if let Err(e) = tokio::fs::remove_file(live_path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(io_err(e));
        }
    }
    if let Some(path) = rollback_path {
        tokio::fs::rename(path, live_path).await.map_err(|e| {
            KbError::Db(format!(
                "数据库切换失败后旧原件恢复失败（文档 {doc_id}）：{e}"
            ))
        })?;
    }
    Ok(())
}

async fn remove_rollback_file(path: &std::path::Path, doc_id: &str) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(doc_id, error = %e, "覆盖成功后旧原件回滚副本删除失败");
        }
    }
}

/// 覆盖的通道②收尾：旧物理表（schema `up_<doc_id>`）随旧版本退役——`drop_source` 幂等，
/// 旧版本非表格时是 no-op；新内容有 sheet 才重建（`tabular_channel` 自带写权限复核与
/// 「建表失败只降级」的 notice 语义，与 Fresh 同一条）。
async fn overwrite_tabular(
    st: &OwnedStore,
    viewer: &Viewer,
    space_id: &str,
    doc_id: &str,
    sheets: &[Sheet],
) -> Result<Option<TabularSource>, KbError> {
    if let Err(e) = tabular::drop_source(st, doc_id).await {
        // drop 失败不挡主链：后续建表撞同名表会走 tabular_channel 的降级 notice，
        // 孤儿 schema 留给运维（与 materialize 半途失败的清场同级）
        tracing::warn!(doc_id, error = %e, "同名覆盖：旧物理表清理失败（孤儿 schema 待回收）");
    }
    tabular_channel(st, viewer, space_id, doc_id, sheets).await
}

struct ShadowBuild {
    chunks: Vec<Chunk>,
    embedding_texts: Vec<String>,
    embeddings: Vec<Option<String>>,
    spans: Vec<Option<CharSpan>>,
    page_count: i32,
    status: DocStatus,
    error: String,
    notice: String,
    /// 解析出的表格（通道②素材）：`reprocess` 不用它（已登记数据源原样保留），
    /// `overwrite` 靠它按新内容重建物理表。
    sheets: Vec<Sheet>,
}

/// 重处理的影子构建：不改 `kb.doc/kb.chunk`，失败时线上版本完全不动。
/// `current` 是库内文档行：embedding_text 的 expected 口径（文档名/目录）必须按库内值生成
/// （`replace_chunks` 的 CAS 复核用库内 name/folder_path），`path` 由调用方算好传入（只算一次）。
async fn build_shadow(
    doc: &DocService,
    embed: &EmbedClient,
    req: &UploadReq<'_>,
    current: &store::DocRow,
    path: &std::path::Path,
    image_ocr: Option<&dyn ImageOcr>,
    kind: FileKind,
) -> Result<ShadowBuild, KbError> {
    tokio::fs::write(path, req.bytes).await.map_err(io_err)?;
    let parsed = parse_input(doc, path, req, image_ocr, kind).await?;
    let notice = parsed.notes.join("；");
    let mut blocks = parsed.blocks;
    let sheets = parsed.sheets;
    blocks.extend(tabular::sheet_blocks(&sheets));
    let spanned = chunk_with_preset(doc, resolve_preset(req.preset), &blocks).await?;
    if spanned.chunks.is_empty() {
        return Err(KbError::BadInput("文档里没有可索引的文本".into()));
    }
    let embedding_texts: Vec<String> = spanned
        .chunks
        .iter()
        .map(|c| store::chunk_embedding_text(&current.name, &current.folder_path, &c.heading_path, &c.text))
        .collect();
    let vecs = match embed.embed_batch(&embedding_texts, EmbedMode::Passage).await {
        Some(v) if v.len() == spanned.chunks.len() => v,
        got => {
            // 计数日志与 run 的同款 warn 对齐；两类失败文案分开（服务不可用 / 条数不符）
            tracing::warn!(
                chunks = spanned.chunks.len(),
                got_vecs = got.as_ref().map_or(0, |v| v.len()),
                "影子构建向量路失败，保留原版本未切换"
            );
            let msg = match got {
                Some(_) => "向量条数与块数不符，保留原版本未切换",
                None => "向量服务不可用，保留原版本未切换",
            };
            return Err(KbError::Upstream(msg.into()));
        }
    };
    let embeddings = vecs.iter().map(|v| Some(to_pgvector(v))).collect();
    Ok(ShadowBuild {
        chunks: spanned.chunks,
        embedding_texts,
        embeddings,
        spans: spanned.spans,
        page_count: parsed.page_count,
        status: DocStatus::Embedded,
        error: String::new(),
        notice,
        sheets,
    })
}

/// 落盘 → parse → chunk → 向量 →（表格）通道②。任一步 `Err` 由 `run_job` 统一记 `failed`；
/// 通道② 例外，它失败只记 `kb.doc.error` 不抹掉整次上传（见 `tabular_channel`）。
async fn run(
    st: &OwnedStore,
    doc: &DocService,
    embed: &EmbedClient,
    viewer: &Viewer,
    cfg: &IngestCfg,
    req: &UploadReq<'_>,
    doc_id: &str,
    image_ocr: Option<&dyn ImageOcr>,
    kind: FileKind,
) -> Result<Option<TabularSource>, KbError> {
    let path = doc_path(cfg, doc_id, req.file_name);
    store::set_status(st, viewer, doc_id, DocStatus::Parsing, "").await?;
    tokio::fs::write(&path, req.bytes).await.map_err(io_err)?;

    let parsed = parse_input(doc, &path, req, image_ocr, kind).await?;
    if !parsed.notes.is_empty() {
        store::set_notice(st, viewer, doc_id, &parsed.notes.join("；")).await?;
    }
    let mut blocks = parsed.blocks;
    // 通道①：每个 sheet 先渲染成 markdown 走同一条文本链（通道②在本函数末尾）
    blocks.extend(tabular::sheet_blocks(&parsed.sheets));
    // preset 通道：server 表单 `preset` 字段原样传入（kb_api 上传入口）
    let spanned = chunk_with_preset(doc, resolve_preset(req.preset), &blocks).await?;
    if spanned.chunks.is_empty() {
        return Err(KbError::BadInput("文档里没有可索引的文本".into()));
    }
    if !acl::space_writable(st, viewer, req.space_id).await? {
        return Err(KbError::Forbidden(format!("解析完成前已失去知识空间 {} 的写权限", req.space_id)));
    }
    let n = store::insert_chunks(st, viewer, doc_id, &spanned.chunks, &spanned.spans).await?;
    store::set_counts(st, viewer, doc_id, parsed.page_count, n as i32).await?;
    store::set_status(st, viewer, doc_id, DocStatus::Chunked, "").await?;

    let jobs = store::chunk_embedding_jobs(st, doc_id).await?;
    let texts: Vec<String> = jobs.iter().map(|j| j.text.clone()).collect();
    // 分批在 `EmbedClient::embed_batch` 里（64 一批，对齐 `embed_service.py` 的 KB_BATCH）——
    // 这里曾把全部块塞进一个请求，而语料侧和问句侧共用 3s 超时：275 块的 Word / 2500 块的 PDF
    // 必超时 ⇒ 文档永久停在 chunked。
    // 条数不符也走同一条降级（`ids` 是事实源）：`zip` 会静默只写前 k 个块的向量，
    // doc 却推到 embedded —— 那就是「界面显示已入库、其实一个字检索不到」。
    let vecs = match embed.embed_batch(&texts, EmbedMode::Passage).await {
        Some(v) if v.len() == jobs.len() => v,
        got => {
            if let Some(v) = &got {
                let (chunks, got_vecs) = (jobs.len(), v.len());
                tracing::warn!(doc_id, chunks, got_vecs, "向量条数与块数不符，本次不回写");
            }
            // 可接受降级：文本检索（tsvector/trgm）仍可用，向量由 `embed_service.py revec` 后补。
            // ⚠️ 这句文案是 revec 清 error 时的匹配串（那边的 DOWNGRADE_MSG），改字要一起改。
            store::set_status(
                st,
                viewer,
                doc_id,
                DocStatus::Chunked,
                "向量服务不可用，稍后可重建",
            )
            .await?;
            return tabular_channel(st, viewer, req.space_id, doc_id, &parsed.sheets).await;
        }
    };
    let rows = jobs.into_iter().zip(vecs.iter().map(|v| to_pgvector(v))).collect::<Vec<_>>();
    if !acl::space_writable(st, viewer, req.space_id).await? {
        return Err(KbError::Forbidden(format!("上传发布前已失去知识空间 {} 的写权限", req.space_id)));
    }
    let written = store::set_embeddings(st, doc_id, &rows, viewer).await?;
    if written == 0 {
        // CAS 全失配：并发重建把 embedding_text/配方改了——一行没写，留痕（文档不毕业，
        // 等 A9/embed_fill 按新文本补算）
        tracing::warn!(doc_id, "向量回写 0 行（CAS 全失配，疑似并发重建）");
    }
    // CAS 可能因目录并发移动而写入 0 行；只按库内实际缺口推进，不能无条件毕业。
    store::promote_doc_if_ready(st, doc_id, viewer).await?;
    tabular_channel(st, viewer, req.space_id, doc_id, &parsed.sheets).await
}

/// 图片可由运行时视觉模型提供文字，其余文件和视觉降级仍复用原解析器。
/// 这里只改变 ParsedDoc 的来源，后面的 chunk/embed/状态机保持同一条链。
/// `kind` 来自入口的 `classify`（同一张白名单查一次，这里不二次查表）。
async fn parse_input(
    doc: &DocService,
    path: &std::path::Path,
    req: &UploadReq<'_>,
    image_ocr: Option<&dyn ImageOcr>,
    kind: FileKind,
) -> Result<ParsedDoc, KbError> {
    let is_image = kind == FileKind::Image;
    if is_image {
        let recognized = match image_ocr {
            Some(ocr) => ocr.recognize(req.file_name, req.mime, req.bytes).await,
            None => None,
        };
        if let Some(text) = recognized.as_deref().map(str::trim).filter(|text| usable_image_ocr(text)) {
            // 图片含义与 OCR 正文并（OCR 成功的同一视觉通道才谈得上描述；describe 失败不挡入库）
            let caption = match image_ocr {
                Some(ocr) => ocr.describe(req.file_name, req.mime, req.bytes).await,
                None => None,
            };
            return Ok(image_parsed_doc(text, caption.as_deref()));
        }
    }

    let mut parsed = doc
        .parse(&path.to_string_lossy(), Some(req.mime))
        .await
        .map_err(|e| sanitize_doc_error(e, path))?;
    if is_image && image_ocr.is_some() {
        mark_image_ocr_fallback(&mut parsed);
    }
    Ok(parsed)
}

/// 文档服务的确定性错误变体会携带上游响应正文；知识库只保留可操作分类，
/// 防止正文经 `KbError::Display` 进入文档状态或 HTTP 响应。
/// `path` 只用于 NotFound 文案（运维定位），是我们自己落盘的路径，不是上游正文。
fn sanitize_doc_error(error: dms_connector::doc::DocError, path: &std::path::Path) -> KbError {
    use dms_connector::doc::DocError;
    match error {
        DocError::NoTextLayer => KbError::BadInput("该 PDF 没有文本层（扫描版），需先 OCR".into()),
        DocError::Unsupported(_) => KbError::BadInput("文档服务不支持该文件类型".into()),
        DocError::TooLarge(_) => KbError::BadInput("表格超出上限（20 万行 / 200 列）".into()),
        DocError::NotFound(_) => KbError::NotFound(format!("待解析文件不存在：{}", path.display())),
        other => {
            // 未分类变体：错误本体先落服务端日志（用户侧文案保持净化后的分类）
            tracing::warn!(error = %other, "文档服务错误未分类，按上游失败处理");
            KbError::Upstream("文档处理失败".into())
        }
    }
}

fn mark_image_ocr_fallback(parsed: &mut ParsedDoc) {
    if !parsed.notes.iter().any(|note| note == IMAGE_OCR_FALLBACK_NOTICE) {
        parsed.notes.push(IMAGE_OCR_FALLBACK_NOTICE.to_string());
    }
}

/// 「无法辨认」的归一化判定：去标点/空白/括号后等于「无法辨认」即失败信号——
/// `[无法辨认]。`、全角括号等变体同样不是有效 OCR 正文
fn usable_image_ocr(text: &str) -> bool {
    let normalized: String = text.chars().filter(|c| c.is_alphanumeric()).collect();
    !normalized.is_empty() && normalized != "无法辨认"
}

fn image_parsed_doc(text: &str, caption: Option<&str>) -> ParsedDoc {
    // 「图片含义」块在最前：导图章节树的第一个节点就是「这张图是什么」（检索同样命中它）
    let mut blocks: Vec<Block> = caption
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| Block {
            text: format!("图片含义：{c}"),
            page: Some(1),
            heading_path: "图片含义".to_string(),
        })
        .into_iter()
        .collect();
    blocks.push(Block {
        text: text.to_string(),
        page: Some(1),
        heading_path: "图片文字识别".to_string(),
    });
    ParsedDoc {
        blocks,
        page_count: 1,
        sheets: Vec::new(),
        notes: Vec::new(),
    }
}

/// 通道②（表格建物理表）。**失败不让整次上传失败**：文本检索已经可用，属可接受降级，
/// 失败文案落 `kb.doc.error` 让用户看得见（`done` 是文本链已经到达的状态，不许回退）。
/// 非表格文件（`sheets` 空）直接跳过——不能给 pdf 记一句「建表失败」。
async fn tabular_channel(
    st: &OwnedStore,
    viewer: &Viewer,
    space_id: &str,
    doc_id: &str,
    sheets: &[Sheet],
) -> Result<Option<TabularSource>, KbError> {
    if sheets.is_empty() {
        return Ok(None);
    }
    if !acl::space_writable(st, viewer, space_id).await? {
        return Err(KbError::Forbidden(format!(
            "表格发布前已失去知识空间 {space_id} 的写权限"
        )));
    }
    match tabular::materialize(st, doc_id, sheets).await {
        Ok(src) => {
            if !acl::space_writable(st, viewer, space_id).await? {
                if let Err(e) = tabular::drop_source(st, doc_id).await {
                    tracing::warn!(doc_id, error = %e, "失权回滚的物理表清理失败（孤儿表）");
                }
                return Err(KbError::Forbidden(format!(
                    "表格发布期间已失去知识空间 {space_id} 的写权限"
                )));
            }
            Ok(Some(src))
        }
        Err(e) => {
            if let Err(de) = tabular::drop_source(st, doc_id).await {
                tracing::warn!(doc_id, error = %de, "建表失败后的物理表清理也失败（孤儿表）");
            }
            // 错误文案进 notice 前过 sanitize：与「上游正文不进用户字段」同一纪律
            let msg = format!("表格已入知识库，建表失败：{}", dms_kernel::qalog::sanitize(&e.to_string()));
            if let Err(ne) = store::append_notice(st, viewer, doc_id, &msg).await {
                tracing::warn!(doc_id, error = %ne, "建表失败的降级提示落库失败");
            }
            Ok(None)
        }
    }
}

fn io_err(e: std::io::Error) -> KbError {
    KbError::Db(format!("落盘失败：{e}"))
}

// ==================== 分块 preset（B2）与字符偏移（B3） ====================
//
// 规则参考 yuxi `knowledge/chunking/ragflow_like/`，按我方现有 chunk 结构落地：
// - 选择只有两个输入：**显式上传参数**（`resolve_preset`）与**文档内容结构**
//   （执行层生效：qa 抽不到问答对、laws 识别不到条款时自动回退 general）。
// - 缺省/未知一律 `General`——默认口径与既有单一分块（`doc.chunk`）逐字节一致。
//   按文件名的启发猜测刻意不做：评测语料的分块口径不能因文件名碰巧含「办法」而静默改变。
// - 「标题路径注入」在我方架构里已由向量配方 v1 承担（`kb.chunk_embedding_text` 的章节行），
//   正文 `text` 保持引用真相，不再重复注入。

/// 分块 preset（与 yuxi `CHUNK_PRESETS` 同名对齐；separator 不搬——它等价于 overlap=0 的 general）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkPreset {
    General,
    Qa,
    Book,
    Laws,
    Semantic,
}

impl ChunkPreset {
    fn parse(value: &str) -> Option<ChunkPreset> {
        match value.trim().to_ascii_lowercase().as_str() {
            "general" => Some(ChunkPreset::General),
            "qa" => Some(ChunkPreset::Qa),
            "book" => Some(ChunkPreset::Book),
            "laws" => Some(ChunkPreset::Laws),
            "semantic" => Some(ChunkPreset::Semantic),
            _ => None,
        }
    }
}

/// 解析上传参数里的 preset 名（yuxi `normalize_chunk_preset_id` 同款：未知值回退 general）。
/// preset 通道：server 表单 `preset` 字段原样传入 `UploadReq.preset`（kb_api 上传入口）。
pub fn resolve_preset(explicit: Option<&str>) -> ChunkPreset {
    explicit.and_then(ChunkPreset::parse).unwrap_or(ChunkPreset::General)
}

/// 与 `tools/embed_service.py::est_tokens` 同口径：`ceil(chars / 1.6)` 的整数写法。
/// **全 crate 只此一份**（store 的 tokens 估算也用它，两处各写一份迟早漂移）。
pub(crate) fn est_tokens(chars: usize) -> i32 {
    ((chars * 5 + 7) / 8) as i32
}

/// `_fill` 的 char 域常量（`int(target*1.6)` / `int(overlap*1.6)` / `cap=target-overlap-1`，同 Python）
const TARGET_CHARS: usize = TARGET_TOKENS as usize * 8 / 5;
const OVERLAP_CHARS: usize = OVERLAP as usize * 8 / 5;
const UNIT_CAP: usize = TARGET_CHARS - OVERLAP_CHARS - 1;

/// 分块输入流：逐 block `trim` 后以 `\n` 连接（空块跳过）——**字符偏移的唯一坐标系**。
/// 偏移按 Unicode 字符计（Python `len` 口径），落库为 `start_char_pos`/`end_char_pos`。
struct SourceStream {
    text: String,
    /// char 下标 → byte 下标（末元素为 text.len()），切片 O(1)
    char_to_byte: Vec<usize>,
    /// 每个非空 block 在流中的 char 区间（顺序与文本一致、互不重叠）
    blocks: Vec<StreamBlock>,
}

struct StreamBlock {
    start: usize,
    end: usize,
    page: Option<i32>,
    heading: String,
}

impl SourceStream {
    fn build(blocks: &[Block]) -> SourceStream {
        let mut text = String::new();
        let mut out = Vec::new();
        let mut nchars = 0usize;
        for b in blocks {
            let t = b.text.trim();
            if t.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push('\n');
                nchars += 1;
            }
            let start = nchars;
            text.push_str(t);
            nchars += t.chars().count();
            out.push(StreamBlock { start, end: nchars, page: b.page, heading: b.heading_path.clone() });
        }
        let char_to_byte =
            text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
        SourceStream { text, char_to_byte, blocks: out }
    }

    fn len_chars(&self) -> usize {
        self.char_to_byte.len().saturating_sub(1)
    }

    fn slice(&self, start: usize, end: usize) -> &str {
        &self.text[self.char_to_byte[start]..self.char_to_byte[end]]
    }

    fn char_of_byte(&self, byte: usize) -> usize {
        self.char_to_byte.partition_point(|&b| b < byte)
    }
}

/// `_emit` 的 `strip()` 在区间上的对应物：两端去空白；全空白返回 None
fn trim_range(s: &SourceStream, start: usize, end: usize) -> Option<(usize, usize)> {
    let text = s.slice(start, end);
    let lead = text.chars().take_while(|c| c.is_whitespace()).count();
    if lead == end - start {
        return None;
    }
    let tail = text.chars().rev().take_while(|c| c.is_whitespace()).count();
    Some((start + lead, end - tail))
}

/// 合并单元/产出块的原文覆盖区间（char 域，半开 `[start, end)`）
#[derive(Clone, Copy)]
struct Unit {
    start: usize,
    end: usize,
    page: Option<i32>,
}

struct MergedChunk {
    text: String,
    heading: String,
    page: Option<i32>,
    start: usize,
    end: usize,
}

/// `_fill::one_page`：贡献页集合去重后只剩一个真实页才显示；跨页宁可 None（「不知道」比「说错」好）。
/// **全 crate 只此一份**（store 的合并页码也用它）。收迭代器：切片/映射链都能直接喂。
pub(crate) fn one_page(pages: impl Iterator<Item = Option<i32>>) -> Option<i32> {
    let mut real = pages.flatten();
    let first = real.next()?;
    if real.all(|p| p == first) {
        Some(first)
    } else {
        None
    }
}

/// `_split_long`：先按句末标点切（`(?<=[。！？；!?;\n])`，标点留在前段末尾、不丢字符），
/// 装箱不超 `cap`；单段仍超则硬切。返回流上的 char 区间（相对源流全局坐标）。
fn split_range(s: &SourceStream, start: usize, end: usize, cap: usize) -> Vec<(usize, usize)> {
    let text = s.slice(start, end);
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    let mut last = 0usize;
    let mut cur = 0usize;
    for c in text.chars() {
        cur += 1;
        if matches!(c, '。' | '！' | '？' | '；' | '!' | '?' | ';' | '\n') {
            pieces.push((last, cur));
            last = cur;
        }
    }
    if last < cur {
        pieces.push((last, cur));
    }
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut buf: Option<(usize, usize)> = None;
    for (ps, pe) in pieces {
        if let Some((bs, be)) = buf {
            if be - bs + (pe - ps) > cap {
                out.push((bs, be));
                buf = None;
            }
        }
        let mut seg = ps;
        while pe - seg > cap {
            out.push((seg, seg + cap));
            seg += cap;
        }
        if seg < pe {
            buf = Some(match buf {
                Some((bs, _)) => (bs, pe),
                None => (seg, pe),
            });
        }
    }
    if let Some((bs, be)) = buf {
        // Python `if buf.strip()`：尾部全空白段不进产出
        if !s.slice(start + bs, start + be).trim().is_empty() {
            out.push((bs, be));
        }
    }
    out.into_iter().map(|(a, b)| (start + a, start + b)).collect()
}

/// `embed_service.py::_fill` 的 Rust 复刻：同一组内按目标长度合并、块间重叠 `overlap`，
/// 单元超长先按句末标点切、仍超则硬切；`page` 取贡献页集合的唯一值（跨页 None）。
/// 唯一的语义增量：每块同时带原文流的 char 覆盖区间（B3 偏移因此精确）。
/// 注意 chunk 文本按 Python 口径在单元拼接处插入 `\n`，与流切片在句切边界可能差换行符——
/// 区间的语义是「该块内容来源的原文覆盖范围」，不是 `text == slice(start, end)`。
fn fill_units(s: &SourceStream, heading: &str, units: &[Unit], overlap: usize) -> Vec<MergedChunk> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut buf_chars = 0usize;
    let mut span: Option<(usize, usize)> = None;
    let mut pages: Vec<Option<i32>> = Vec::new();
    let mut fresh = false;
    for u in units {
        for (ps, pe) in split_range(s, u.start, u.end, UNIT_CAP) {
            let plen = pe - ps;
            if buf_chars > 0 && buf_chars + 1 + plen > TARGET_CHARS {
                flush_merged(&mut out, s, heading, &buf, span, &pages);
                // 重叠尾巴＝上一块末尾 overlap 个字符（Python `buf[len-overlap:]`）
                let (a, b) = span.expect("buf_chars>0 时 span 必存在");
                if overlap > 0 {
                    let tail = b.saturating_sub(overlap).max(a);
                    buf = s.slice(tail, b).to_string();
                    buf_chars = b - tail;
                    span = Some((tail, b));
                } else {
                    buf.clear();
                    buf_chars = 0;
                    span = None;
                }
                pages.clear();
            }
            if !pages.contains(&u.page) {
                pages.push(u.page);
            }
            if !buf.is_empty() {
                buf.push('\n');
                buf_chars += 1;
            }
            buf.push_str(s.slice(ps, pe));
            buf_chars += plen;
            span = Some((span.map(|(a, _)| a).unwrap_or(ps), pe));
            fresh = true;
        }
    }
    if fresh {
        flush_merged(&mut out, s, heading, &buf, span, &pages);
    }
    out
}

fn flush_merged(
    out: &mut Vec<MergedChunk>,
    s: &SourceStream,
    heading: &str,
    buf: &str,
    span: Option<(usize, usize)>,
    pages: &[Option<i32>],
) {
    let Some((a, b)) = span else { return };
    let text = buf.trim();
    if text.is_empty() {
        return;
    }
    // 首单元的前导空白/尾单元的尾部空白不算入覆盖范围
    let (a, b) = trim_range(s, a, b).unwrap_or((a, b));
    out.push(MergedChunk { text: text.to_string(), heading: heading.to_string(), page: one_page(pages.iter().copied()), start: a, end: b });
}

/// 按连续相同分组键切段，段内 `_fill` 合并（semantic/book/laws 共用的合并驱动）
fn merge_grouped(s: &SourceStream, units: Vec<(String, Unit)>, overlap: usize) -> Vec<MergedChunk> {
    let mut out = Vec::new();
    let mut group: Vec<Unit> = Vec::new();
    let mut group_heading = String::new();
    for (h, u) in units {
        if !group.is_empty() && h != group_heading {
            out.extend(fill_units(s, &group_heading, &group, overlap));
            group.clear();
        }
        if group.is_empty() {
            group_heading = h;
        }
        group.push(u);
    }
    if !group.is_empty() {
        out.extend(fill_units(s, &group_heading, &group, overlap));
    }
    out
}

/// 结构切分（semantic/book）：block 是解析器给出的结构单元，分组键＝标题路径
/// （semantic 取完整路径＝叶子；book 取顶级段＝允许同章跨小节合并），
/// 段内按长度合并但**不做字符重叠**——重叠尾巴会把上一节的话混进下一节标题下。
fn heading_chunks(s: &SourceStream, top_level: bool) -> Vec<MergedChunk> {
    let units = s
        .blocks
        .iter()
        .map(|b| {
            let key = if top_level {
                b.heading.split(" > ").next().unwrap_or("").to_string()
            } else {
                b.heading.clone()
            };
            (key, Unit { start: b.start, end: b.end, page: b.page })
        })
        .collect();
    merge_grouped(s, units, 0)
}

/// 源流逐行视图（block 内行间在流中只差一个 `\n`）
struct StreamLine {
    start: usize,
    end: usize,
    page: Option<i32>,
    /// 所属 block 下标（heading 用时再取，不逐行 clone 标题串）
    block: usize,
}

impl StreamLine {
    fn heading<'a>(&self, s: &'a SourceStream) -> &'a str {
        &s.blocks[self.block].heading
    }
}

fn stream_lines(s: &SourceStream) -> Vec<StreamLine> {
    let mut lines = Vec::new();
    for (bi, b) in s.blocks.iter().enumerate() {
        let mut pos = b.start;
        for piece in s.slice(b.start, b.end).split('\n') {
            let len = piece.chars().count();
            if !piece.trim().is_empty() {
                lines.push(StreamLine { start: pos, end: pos + len, page: b.page, block: bi });
            }
            pos += len + 1; // 跳过 '\n'；每 block 循环重置 pos，末尾多算无影响
        }
    }
    lines
}

/// `第[零一二三四五六七八九十百千万0-9]+<层级后缀>` 前缀判定
/// （yuxi `_ARTICLE_PATTERN` 的手写版——全仓无 regex 依赖是硬约束）。
fn law_marker(t: &str, suffixes: &[&str]) -> bool {
    let Some(rest) = t.strip_prefix('第') else { return false };
    let digits = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || "零一二三四五六七八九十百千万".contains(*c))
        .count();
    if digits == 0 {
        return false;
    }
    // char_indices 取第 digits 个字符的字节偏移后切片（中文数字 3 字节，digits 不是字节下标）
    let after = match rest.char_indices().nth(digits) {
        Some((i, _)) => &rest[i..],
        None => "",
    };
    suffixes.iter().any(|suf| after.starts_with(suf))
}

/// 法规条款合并（laws）：识别 `第X章/编/篇/部分`、`第X节`、`第X条` 层级，
/// 条款与其款/项续行并成一个单元，同章节路径内相邻条款按长度合并，标题路径随章/节栈推进。
/// 识别不到 ≥2 个条款返回 None——不是法规结构，回退 general（分块策略按内容结构生效）。
fn laws_chunks(s: &SourceStream) -> Option<Vec<MergedChunk>> {
    let mut units: Vec<(String, Unit)> = Vec::new();
    let mut top = String::new();
    let mut sub = String::new();
    let mut articles = 0usize;
    let mut cur: Option<(usize, usize, Option<i32>)> = None;
    let mut cur_heading = String::new();
    let heading_of = |top: &str, sub: &str| -> String {
        match (top.is_empty(), sub.is_empty()) {
            (true, true) => String::new(),
            (false, true) => top.to_string(),
            (true, false) => sub.to_string(),
            (false, false) => format!("{top} > {sub}"),
        }
    };
    let close = |cur: &mut Option<(usize, usize, Option<i32>)>,
                 heading: &str,
                 units: &mut Vec<(String, Unit)>| {
        if let Some((start, end, page)) = cur.take() {
            units.push((heading.to_string(), Unit { start, end, page }));
        }
    };
    for line in stream_lines(s) {
        let t = s.slice(line.start, line.end).trim();
        let is_chapter = law_marker(t, &["章", "编", "篇", "部分"]);
        let is_section = !is_chapter && law_marker(t, &["节"]);
        let is_article = !is_chapter && !is_section && law_marker(t, &["条"]);
        if is_chapter || is_section {
            close(&mut cur, &cur_heading, &mut units);
            if is_chapter {
                top = t.to_string();
                sub.clear();
            } else {
                sub = t.to_string();
            }
            cur_heading = heading_of(&top, &sub);
            cur = Some((line.start, line.end, line.page));
            continue;
        }
        if is_article {
            close(&mut cur, &cur_heading, &mut units);
            articles += 1;
            cur = Some((line.start, line.end, line.page));
            continue;
        }
        // 款/项/普通续行：并入当前单元；前言部分（尚无章条）自成单元
        cur = match cur {
            Some((start, _, page)) => Some((start, line.end, page)),
            None => {
                cur_heading = heading_of(&top, &sub);
                Some((line.start, line.end, line.page))
            }
        };
    }
    close(&mut cur, &cur_heading, &mut units);
    if articles < 2 {
        return None;
    }
    Some(merge_grouped(s, units, 0))
}

/// markdown 表格行解析：`| a | b |` → `["a","b"]`（非表格行返回空）
fn parse_md_row(t: &str) -> Vec<String> {
    let t = t.trim();
    if !(t.starts_with('|') && t.ends_with('|') && t.matches('|').count() >= 2) {
        return Vec::new();
    }
    let cells: Vec<String> =
        t.trim_start_matches('|').trim_end_matches('|').split('|').map(|c| c.trim().to_string()).collect();
    // `| | ` 这类全空单元行不是表格行（防被 qa 主循环当表格素材）
    if cells.iter().all(|c| c.is_empty()) {
        return Vec::new();
    }
    cells
}

/// `| --- | :-: |` 形分隔行（收已解析的 cells，不重复解析同一行）
fn is_md_separator_cells(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c: String = c.chars().filter(|ch| !ch.is_whitespace()).collect();
            c.contains('-') && c.chars().all(|ch| ch == '-' || ch == ':')
        })
}

/// `问：/Q:/Question：` 式前缀剥离（大小写不敏感；词后必须跟冒号才算命中）。
/// 零分配比较：`get(..w.len())` 在非字符边界返回 None，天然安全。
fn strip_prefix_ci(t: &str, words: &[&str]) -> Option<String> {
    for w in words {
        let Some(rest) = t.get(..w.len()).filter(|p| p.eq_ignore_ascii_case(w)).map(|_| &t[w.len()..])
        else {
            continue;
        };
        let rest = rest.trim_start();
        let mut chars = rest.chars();
        if matches!(chars.next(), Some(':') | Some('：')) {
            return Some(chars.as_str().trim().to_string());
        }
    }
    None
}

/// 大小写不敏感的词表命中（零分配，替代表头识别里的逐 cell `to_ascii_lowercase`）
fn eq_any_ci(s: &str, words: &[&str]) -> bool {
    words.iter().any(|w| s.eq_ignore_ascii_case(w))
}

const Q_PREFIXES: [&str; 4] = ["question", "问题", "问", "q"];
const A_PREFIXES: [&str; 5] = ["answer", "回答", "答案", "答", "a"];

#[allow(clippy::too_many_arguments)]
fn push_qa_pair(
    pairs: &mut Vec<MergedChunk>,
    seen: &mut std::collections::HashSet<(String, String)>,
    q: &str,
    a: &str,
    start: usize,
    end: usize,
    page: Option<i32>,
    heading: &str,
) {
    let q = q.trim();
    let a = a.trim();
    if q.is_empty() || a.is_empty() {
        return;
    }
    // QA 对多时是 O(n²) 线性去重的热面 → HashSet
    if !seen.insert((q.to_string(), a.to_string())) {
        return;
    }
    pairs.push(MergedChunk {
        text: format!("问题：{q}\n回答：{a}"),
        heading: heading.to_string(),
        page,
        start,
        end,
    });
}

/// 问答抽取（qa，yuxi `parsers/qa.py` 的我方落地）：两条路——
/// ① markdown 表格行（通道①已把 sheet 渲染成表格块，文档内嵌表格同形）：
///    `问/答` 表头定列位，否则首列问、其余列以「；」并入答；表头行与分隔行不产对。
/// ② `问：/Q：` … `答：/A：` 前缀行配对（问后的普通续行并入答案）。
/// 每对一块：`问题：{q}\n回答：{a}`（`_to_qa_chunk` 同款），偏移＝素材行在源流中的覆盖区间。
/// 一对都抽不到返回 None → 回退 general。
fn qa_chunks(s: &SourceStream) -> Option<Vec<MergedChunk>> {
    let lines = stream_lines(s);
    let mut pairs: Vec<MergedChunk> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    // 前缀配对状态：问题行区间 + 答案累积（答案可能跨多行，span 覆盖到最后一行答案）
    let mut cur_q: Option<(String, usize, Option<i32>, String)> = None;
    let mut cur_a: Vec<String> = Vec::new();
    let mut a_end = 0usize;
    // 表格列位（表头识别结果）：(问列, 答列)；答列 None = 首列问、其余并入答
    let mut cols: Option<(usize, Option<usize>)> = None;

    macro_rules! flush_prefix {
        () => {
            if let Some((q, q_start, page, heading)) = cur_q.take() {
                let a = cur_a.join("\n");
                push_qa_pair(&mut pairs, &mut seen, &q, &a, q_start, a_end, page, &heading);
                cur_a.clear();
            }
        };
    }

    let mut i = 0usize;
    while i < lines.len() {
        let line = &lines[i];
        let text = s.slice(line.start, line.end).trim();
        let cells = parse_md_row(text);
        if !cells.is_empty() {
            flush_prefix!(); // 表格结构打断前缀配对
            if is_md_separator_cells(&cells) {
                i += 1;
                continue;
            }
            // 表头＝表格行且下一行是分隔行：定列位，本身不产对
            if i + 1 < lines.len()
                && is_md_separator_cells(&parse_md_row(
                    s.slice(lines[i + 1].start, lines[i + 1].end).trim(),
                ))
            {
                let qi = cells.iter().position(|c| eq_any_ci(c, &["问", "问题", "q", "question"]));
                let ai =
                    cells.iter().position(|c| eq_any_ci(c, &["答", "回答", "答案", "a", "answer"]));
                cols = Some(match (qi, ai) {
                    (Some(qi), Some(ai)) if qi != ai => (qi, Some(ai)),
                    _ => (0, None),
                });
                i += 1;
                continue;
            }
            if cells.len() >= 2 {
                let (qi, ai) = cols.unwrap_or((0, None));
                let q = cells.get(qi).map(String::as_str).unwrap_or("");
                let a = match ai {
                    Some(ai) => cells.get(ai).cloned().unwrap_or_default(),
                    // 无答列表头：问列以外的全部列并入答案（多列不丢信息）
                    None => cells
                        .iter()
                        .enumerate()
                        .filter(|(ci, _)| *ci != qi)
                        .map(|(_, c)| c.as_str())
                        .collect::<Vec<_>>()
                        .join("；"),
                };
                push_qa_pair(
                    &mut pairs,
                    &mut seen,
                    q,
                    &a,
                    line.start,
                    line.end,
                    line.page,
                    line.heading(s),
                );
            }
            i += 1;
            continue;
        }
        cols = None; // 表格结束
        if let Some(q) = strip_prefix_ci(text, &Q_PREFIXES) {
            flush_prefix!();
            cur_q = Some((q, line.start, line.page, line.heading(s).to_string()));
            a_end = line.end;
        } else if let Some(a) = strip_prefix_ci(text, &A_PREFIXES) {
            if cur_q.is_some() {
                cur_a.push(a);
                a_end = line.end;
            }
        } else if cur_q.is_some() {
            cur_a.push(text.to_string());
            a_end = line.end;
        }
        i += 1;
    }
    flush_prefix!();
    if pairs.is_empty() {
        return None;
    }
    Some(pairs)
}

/// general 路径的偏移定位：chunk 是文档服务 `/chunk` 的产物（strip/句切/重叠），
/// 在原文流中**顺序**定位每块文本（游标单调推进，重叠尾巴允许下一块起点回退）；
/// 找不到时全流补找一次，仍找不到落 None——错位的偏移比没有更糟。
fn locate_offsets(s: &SourceStream, chunks: &[Chunk]) -> Vec<Option<CharSpan>> {
    let mut cursor = 0usize;
    chunks
        .iter()
        .map(|c| {
            let needle = c.text.trim();
            if needle.is_empty() {
                return None;
            }
            let hit = find_from(s, needle, cursor).or_else(|| find_from(s, needle, 0));
            match hit {
                Some((start, end)) => {
                    cursor = start + 1;
                    Some(CharSpan { start: start as i32, end: end as i32 })
                }
                None => None,
            }
        })
        .collect()
}

fn find_from(s: &SourceStream, needle: &str, from_char: usize) -> Option<(usize, usize)> {
    if from_char >= s.len_chars() {
        return None;
    }
    let byte_from = s.char_to_byte[from_char];
    let rel = s.text[byte_from..].find(needle)?;
    let start = s.char_of_byte(byte_from + rel);
    // end 用字节偏移换算（needle 起止都在字符边界上），不再 `needle.chars().count()` 全串重扫
    Some((start, s.char_of_byte(byte_from + rel + needle.len())))
}

/// 一条分块链路的产物：`chunks` 与等长平行的字符偏移（`insert_chunks`/`replace_chunks` 的入参形状）
struct SpannedChunks {
    chunks: Vec<Chunk>,
    spans: Vec<Option<CharSpan>>,
}

fn to_spanned(merged: Vec<MergedChunk>) -> SpannedChunks {
    let mut chunks = Vec::with_capacity(merged.len());
    let mut spans = Vec::with_capacity(merged.len());
    for m in merged {
        spans.push(Some(CharSpan { start: m.start as i32, end: m.end as i32 }));
        chunks.push(Chunk {
            tokens: est_tokens(m.text.chars().count()),
            text: m.text,
            heading_path: m.heading,
            page: m.page,
        });
    }
    SpannedChunks { chunks, spans }
}

/// preset 分派。Rust 侧结构策略（qa/laws/semantic/book）产不出块时回退文档服务通用分块——
/// 「按文档类型/内容结构选择」落在这一层：策略对结构不适用时绝不硬套。
async fn chunk_with_preset(
    doc: &DocService,
    preset: ChunkPreset,
    blocks: &[Block],
) -> Result<SpannedChunks, KbError> {
    let stream = SourceStream::build(blocks);
    let structured = match preset {
        ChunkPreset::Qa => qa_chunks(&stream),
        ChunkPreset::Laws => laws_chunks(&stream),
        ChunkPreset::Semantic => Some(heading_chunks(&stream, false)),
        ChunkPreset::Book => Some(heading_chunks(&stream, true)),
        ChunkPreset::General => None,
    };
    match structured.filter(|ms| !ms.is_empty()) {
        Some(merged) => Ok(to_spanned(merged)),
        None => {
            let chunks = doc.chunk(blocks, TARGET_TOKENS, OVERLAP).await?;
            Ok(SpannedChunks { spans: locate_offsets(&stream, &chunks), chunks })
        }
    }
}


/// sheet → markdown 表格块（通道①的唯一实现）。
/// `tabular::sheet_blocks` 是它的契约入口，留在这里是因为行上限与降级文案的单测在本文件。
pub(crate) fn sheet_block(s: &Sheet) -> Block {
    use std::fmt::Write as _;
    let mut text = format!("# {}\n\n", s.name);
    if !s.header.is_empty() {
        let _ = writeln!(text, "| {} |", s.header.iter().map(|h| md_cell(h)).collect::<Vec<_>>().join(" | "));
        let _ = writeln!(text, "|{}|", vec!["---"; s.header.len()].join("|"));
    }
    for row in s.rows.iter().take(SHEET_ROWS) {
        let _ = writeln!(text, "| {} |", row.iter().map(|c| md_cell(c)).collect::<Vec<_>>().join(" | "));
    }
    if s.rows.len() > SHEET_ROWS {
        // ponytail: 超长表只索引前 SHEET_ROWS 行，且把这句写进正文——降级要让读者看见。
        // 完整数据走 K4 的物理表通道。
        let _ = writeln!(
            text,
            "\n（本表共 {} 行，仅前 {SHEET_ROWS} 行进入文本索引）",
            s.rows.len()
        );
    }
    Block { text, page: None, heading_path: s.name.clone() }
}

/// 单元格渲染进 markdown 前的中和：`|`/换行会破坏表结构并干扰 qa 表格抽取
/// （与 answer 的 `table_cell` 同纪律：全角化/空格化，不丢内容）。无特殊字符零分配。
fn md_cell(c: &str) -> std::borrow::Cow<'_, str> {
    if c.contains(['|', '\n', '\r']) {
        std::borrow::Cow::Owned(c.replace('|', "｜").replace(['\n', '\r'], " "))
    } else {
        std::borrow::Cow::Borrowed(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u64 = 20 * 1_048_576;

    #[test]
    fn explicit_filename_versions_form_document_families_without_guessing() {
        assert_eq!(
            infer_doc_version("运营看板_各指标计算逻辑_v0.1.19.docx"),
            Some(InferredDocVersion {
                family: "运营看板_各指标计算逻辑".into(),
                revision: "v0.1.19".into(),
            })
        );
        assert_eq!(
            infer_doc_version("市场费用报销核销-第3版.pdf"),
            Some(InferredDocVersion {
                family: "市场费用报销核销".into(),
                revision: "第3版".into(),
            })
        );
        assert_eq!(
            infer_doc_version("经营制度（2026-08-06）.pdf"),
            Some(InferredDocVersion {
                family: "经营制度".into(),
                revision: "2026-08-06".into(),
            })
        );
        assert_eq!(infer_doc_version("设备v型说明书.pdf"), None);
        assert_eq!(infer_doc_version("经营制度最新版.pdf"), None);
    }

    #[test]
    fn whitelist_accepts_every_supported_extension() {
        for (name, want) in [
            ("制度.pdf", FileKind::Pdf),
            ("A.DOCX", FileKind::Docx),
            ("台账.xlsx", FileKind::Xlsx),
            ("宏表.xlsm", FileKind::Xlsx),
            ("x.csv", FileKind::Csv),
            ("y.pptx", FileKind::Pptx),
            ("z.md", FileKind::Text),
            ("z.markdown", FileKind::Text),
            ("z.txt", FileKind::Text),
            // 纯文本族新格式（json/log/html）
            ("配置.JSON", FileKind::Text),
            ("运行.log", FileKind::Text),
            ("页面.html", FileKind::Text),
            // 旧二进制 Office
            ("老制度.doc", FileKind::LegacyOffice),
            ("老台账.XLS", FileKind::LegacyOffice),
            ("老材料.ppt", FileKind::LegacyOffice),
            // 图片（运行时视觉模型优先，文档服务本地 OCR 降级）
            ("扫描件.png", FileKind::Image),
            ("拍的.jpg", FileKind::Image),
            ("拍的.JPEG", FileKind::Image),
            ("图.bmp", FileKind::Image),
            ("动图.gif", FileKind::Image),
            ("传真.tif", FileKind::Image),
            ("传真.tiff", FileKind::Image),
            ("图.webp", FileKind::Image),
        ] {
            assert_eq!(classify(name, 10, MAX).unwrap(), want, "{name}");
        }
    }

    #[test]
    fn ai_image_text_becomes_the_parsed_document_without_rewriting_it() {
        let text = "日期：2026-08-06\n金额：￥12,345.67\n| 商品 | 数量 |\n| A | 2 |";
        let parsed = image_parsed_doc(text, None);
        assert_eq!(parsed.page_count, 1);
        assert!(parsed.sheets.is_empty());
        assert_eq!(parsed.blocks.len(), 1);
        assert_eq!(parsed.blocks[0].text, text);
        assert_eq!(parsed.blocks[0].page, Some(1));
    }

    /// 图片含义块：有描述时置首块（导图首节点=「这张图是什么」），无描述/空白描述不产生块
    #[test]
    fn image_caption_block_comes_first_when_present() {
        let parsed = image_parsed_doc("OCR 正文", Some(" 门店陈列照片，烤肠专柜特写 "));
        assert_eq!(parsed.blocks.len(), 2);
        assert_eq!(parsed.blocks[0].heading_path, "图片含义");
        assert_eq!(parsed.blocks[0].text, "图片含义：门店陈列照片，烤肠专柜特写");
        assert_eq!(parsed.blocks[1].heading_path, "图片文字识别");
        let bare = image_parsed_doc("OCR 正文", Some("   "));
        assert_eq!(bare.blocks.len(), 1, "空白描述不许产空块");
        let none = image_parsed_doc("OCR 正文", None);
        assert_eq!(none.blocks.len(), 1);
    }

    #[test]
    fn local_image_ocr_notice_is_fixed_and_idempotent() {
        let mut parsed = ParsedDoc::default();
        mark_image_ocr_fallback(&mut parsed);
        mark_image_ocr_fallback(&mut parsed);
        assert_eq!(parsed.notes, vec![IMAGE_OCR_FALLBACK_NOTICE.to_string()]);
    }

    #[test]
    fn unusable_ai_image_text_falls_back_to_local_ocr() {
        assert!(!usable_image_ocr(""));
        assert!(!usable_image_ocr("[无法辨认]"));
        assert!(!usable_image_ocr("无法辨认"));
        // 变体同判：带句号/全角括号的失败信号也不是有效正文
        assert!(!usable_image_ocr("[无法辨认]。"));
        assert!(!usable_image_ocr("（无法辨认）"));
        assert!(!usable_image_ocr("！！"), "纯标点同样不是有效正文");
        assert!(usable_image_ocr("订单号：HJXH-001"));
    }

    /// 「第第3版」不是版本尾缀（各剥一次，不许多剥）；空单元格行不是表格行
    #[test]
    fn version_token_and_md_row_edge_cases() {
        assert!(!is_version_token("第第3版"));
        assert!(is_version_token("第3版"));
        assert!(parse_md_row("| | ").is_empty(), "全空单元行不许当表格行");
        assert!(parse_md_row("| | |").is_empty());
        assert_eq!(parse_md_row("| a | |"), vec!["a".to_string(), String::new()], "部分空单元仍是表格行");
    }

    /// 单元格里的 `|`/换行必须中和，否则渲染出的 markdown 表结构被破坏、qa 抽取被干扰
    #[test]
    fn sheet_cells_are_neutralized_before_rendering() {
        let s = Sheet {
            name: "t".into(),
            header: vec!["a|b".into()],
            rows: vec![vec!["x\ny".into()]],
        };
        let b = sheet_block(&s);
        assert!(b.text.contains("a｜b"), "竖线全角化: {}", b.text);
        assert!(b.text.contains("x y"), "换行空格化: {}", b.text);
        assert_eq!(b.text.matches("a｜b").count(), 1);
    }

    /// 🔴 **本表必须覆盖文档服务真支持的每一种扩展名。**
    ///
    /// 缺陷现场（实测）：这张表只有 7 项，而 `tools/embed_service.py` 的 `CAPS` 有 19 项，
    /// 容器 `/health` 的 `parse_caps` 也报 19 项全 `ok=true` ——
    /// 但用户上传 `.doc` / `.png` 在**产品唯一入口**就被 400 拒掉，那 12 项是死代码。
    /// 「解析器支持 PPT」与「用户能上传 PPT」是两件事，这张表就是中间那道门。
    ///
    /// 判据形状：从 Python 源里抠出扩展名，与 `EXTS` 对**集合相等**。
    /// 用 `include_str!` 读 Python 源是刻意的 —— 两张表在两种语言里，
    /// 没有类型系统能连起来，而 `include_str!` 至少让**改了一侧**当场红。
    /// （这不是「负向字面量断言」那种恒真形态：断的是集合相等，任一侧多/少一项都会红。）
    ///
    /// ⚠️ 抠源码这条路是**脆**的，本条判据自己就演示过一次：第一版只抠 `CAPS` 里的
    /// `'.xxx':` 字面量行，结果 7 个图片扩展名抠不到 —— 它们是
    /// `**{e: … for e in IMG_EXTS}` 展开进去的。所以两处都要抠，且**保留「抠出的项数够多」
    /// 这道自检**：抠法漂了会以「项数不足」当场红，而不是静默变成一条恒真的空集比较。
    #[test]
    fn exts_cover_the_doc_service_capabilities() {
        const PY: &str = include_str!("../../../tools/embed_service.py");
        let dots = |s: &str| -> Vec<String> {
            // 抠出所有 `'.xxx'` 形式的单引号字面量（扩展名一律以点开头）
            s.split('\'')
                .filter(|t| t.starts_with('.') && t.len() > 1 && t[1..].chars().all(|c| c.is_ascii_alphanumeric()))
                .map(|t| t[1..].to_string())
                .collect()
        };
        // ① `CAPS = { ... }` 那一段里的键
        let caps = PY.split("CAPS = {").nth(1).expect("embed_service.py 里找不到 CAPS 表");
        let mut py = dots(caps.split("\n}").next().unwrap());
        // ② 图片那族是 `**{e: … for e in IMG_EXTS}` 展开的，键不在 CAPS 的字面量里
        let img = PY.split("IMG_EXTS = (").nth(1).expect("找不到 IMG_EXTS");
        py.extend(dots(img.split(')').next().unwrap()));
        py.sort();
        py.dedup();
        assert!(py.len() >= 15, "只抠出 {} 项，抠法漂了：{py:?}", py.len());
        let mut rs: Vec<String> = EXTS.iter().map(|(e, _)| e.to_string()).collect();
        rs.sort();
        assert_eq!(rs, py, "EXTS 与 embed_service.py 的 CAPS 不一致（多/少的那几项就是死代码）");
    }

    #[test]
    fn unknown_and_missing_extension_rejected() {
        for name in ["a.exe", "a.pdf.exe", "noext", "a.", ".pdf.rar"] {
            assert!(matches!(classify(name, 10, MAX), Err(KbError::BadInput(_))), "{name}");
        }
        // 双扩展名不许因为「含 pdf」就放过
        assert!(classify("a.pdf.exe", 10, MAX).is_err());
        // 无扩展名的 .pdf 前缀文件确实是 pdf（rsplit_once 取最后一段）
        assert!(classify("report.final.pdf", 10, MAX).is_ok());
    }

    #[test]
    fn size_limits() {
        assert!(matches!(classify("a.pdf", 0, MAX), Err(KbError::BadInput(_))));
        assert!(classify("a.pdf", MAX, MAX).is_ok());
        assert!(matches!(classify("a.pdf", MAX + 1, MAX), Err(KbError::BadInput(_))));
    }

    #[test]
    fn tabular_only_for_sheets() {
        assert!(FileKind::Xlsx.is_tabular() && FileKind::Csv.is_tabular());
        assert!(!FileKind::Pdf.is_tabular() && !FileKind::Text.is_tabular());
    }

    /// 路径穿越：原名一个字都不许进磁盘路径
    #[test]
    fn disk_path_drops_original_name() {
        let cfg = IngestCfg { root: std::path::PathBuf::from("data/kb"), max_bytes: MAX };
        let evil = r"..\..\Windows\System32\evil.pdf";
        let p = doc_path(&cfg, "11111111-2222-3333-4444-555555555555", evil);
        let s = p.to_string_lossy().replace('\\', "/");
        assert!(s.ends_with("data/kb/11111111-2222-3333-4444-555555555555.pdf"), "{s}");
        assert!(!s.contains("evil") && !s.contains(".."));
    }

    #[test]
    fn sheet_renders_markdown_table_with_row_cap() {
        let s = Sheet {
            name: "一月".into(),
            header: vec!["日期".into(), "金额".into()],
            rows: (0..SHEET_ROWS + 3).map(|i| vec![format!("d{i}"), "1".into()]).collect(),
        };
        let b = sheet_block(&s);
        assert_eq!(b.heading_path, "一月");
        assert!(b.text.starts_with("# 一月"));
        assert!(b.text.contains("| 日期 | 金额 |"));
        assert!(b.text.contains("|---|---|"));
        assert!(b.text.contains(&format!("| d{} | 1 |", SHEET_ROWS - 1)));
        assert!(!b.text.contains(&format!("| d{SHEET_ROWS} |")));
        assert!(b.text.contains(&format!("本表共 {} 行", SHEET_ROWS + 3)));
    }

    // ==================== 分块 preset / 字符偏移 / 秒传去重 ====================

    fn blk(text: &str, heading: &str, page: Option<i32>) -> Block {
        Block { text: text.to_string(), page, heading_path: heading.to_string() }
    }

    fn mk_chunk(text: &str) -> Chunk {
        Chunk { text: text.to_string(), heading_path: String::new(), page: None, tokens: 0 }
    }

    /// B2：preset 由显式参数选择（大小写/空白容忍），未知与缺省一律回退 general
    #[test]
    fn preset_resolution_is_explicit_with_general_fallback() {
        assert_eq!(resolve_preset(Some("qa")), ChunkPreset::Qa);
        assert_eq!(resolve_preset(Some(" LAWS ")), ChunkPreset::Laws);
        assert_eq!(resolve_preset(Some("Semantic")), ChunkPreset::Semantic);
        assert_eq!(resolve_preset(Some("book")), ChunkPreset::Book);
        assert_eq!(resolve_preset(Some("general")), ChunkPreset::General);
        assert_eq!(resolve_preset(Some("naive")), ChunkPreset::General);
        assert_eq!(resolve_preset(Some("")), ChunkPreset::General);
        assert_eq!(resolve_preset(None), ChunkPreset::General);
    }

    /// 与 `embed_service.py::est_tokens` 的 `ceil(chars/1.6)` 逐点对拍
    #[test]
    fn est_tokens_matches_python_ceiling() {
        assert_eq!(est_tokens(0), 0);
        assert_eq!(est_tokens(8), 5);
        assert_eq!(est_tokens(9), 6);
        assert_eq!(est_tokens(640), 400);
    }

    /// B2-qa：markdown 表格（含 sheet 渲染块）按表头列位抽问答对，表头/分隔行不产对；
    /// `问：/答：` 前缀行配对且续行并入答案；重复对去重；无问答结构回退 None。
    #[test]
    fn qa_pairs_come_from_tables_and_prefix_lines() {
        let blocks = vec![
            blk("| 问题 | 答案 |\n| --- | --- |\n| 报销要发票吗 | 要 |\n| 发票抬头 | 公司名称 |", "FAQ", Some(1)),
            blk("问：住宿标准多少\n答：每晚 300\n旺季上浮两成", "制度", Some(2)),
        ];
        let s = SourceStream::build(&blocks);
        let pairs = qa_chunks(&s).unwrap();
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].text, "问题：报销要发票吗\n回答：要");
        assert_eq!(pairs[0].heading, "FAQ");
        assert_eq!(pairs[0].page, Some(1));
        assert_eq!(pairs[2].text, "问题：住宿标准多少\n回答：每晚 300\n旺季上浮两成");
        assert_eq!(pairs[2].page, Some(2));
        // 偏移＝素材行在源流中的覆盖区间
        assert_eq!(s.slice(pairs[0].start, pairs[0].end), "| 报销要发票吗 | 要 |");
        assert!(s.slice(pairs[2].start, pairs[2].end).starts_with("问：住宿标准多少"));

        // 无表头表格：首列问、其余列并入答；重复行去重
        let dup = vec![blk("| 年假几天 | 五天 |\n| 年假几天 | 五天 |", "", None)];
        let pairs = qa_chunks(&SourceStream::build(&dup)).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].text, "问题：年假几天\n回答：五天");

        // 普通段落没有问答结构 → None（回退 general）
        let plain = vec![blk("这是一段普通文本，没有任何问答结构。", "正文", None)];
        assert!(qa_chunks(&SourceStream::build(&plain)).is_none());
    }

    /// B2-laws：条款按层级合并（款项并入所属条），标题路径随章/节栈推进；
    /// 识别不到 ≥2 个条款回退 None。
    #[test]
    fn laws_articles_merge_under_chapter_headings() {
        let blocks = vec![blk(
            "第一章 总则\n第一条 为规范报销，制定本制度。\n第二条 本制度适用于全体员工。\n（一）正式员工；\n（二）实习生。\n第二章 附则\n第九条 本制度自发布之日起施行。",
            "",
            Some(3),
        )];
        let s = SourceStream::build(&blocks);
        let chunks = laws_chunks(&s).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "第一章 总则");
        assert!(chunks[0].text.contains("第一条"));
        assert!(chunks[0].text.contains("（二）实习生。"), "款项必须并入所属条款");
        assert_eq!(chunks[1].heading, "第二章 附则");
        assert!(chunks[1].text.contains("第九条"));
        let span0 = s.slice(chunks[0].start, chunks[0].end);
        assert!(span0.starts_with("第一章 总则"));
        assert!(span0.ends_with("（二）实习生。"));

        // 全文只有一处「第X条」不构成法规结构 → None（「一条」不带「第」不许误判）
        let plain = vec![blk("只有一条不算法规：第一条 孤独条款。", "", None)];
        assert!(laws_chunks(&SourceStream::build(&plain)).is_none());
    }

    /// B2-semantic/book：semantic 按完整标题路径（叶子）切；book 按顶级章节合并（允许跨小节），
    /// 跨页合并的页码按 one_page 规则置 None；偏移与源流切片逐字一致。
    #[test]
    fn semantic_splits_leaves_book_merges_top_level() {
        let blocks = vec![
            blk("第一节的内容", "某章 > 第一节", Some(1)),
            blk("第二节的内容", "某章 > 第二节", Some(2)),
        ];
        let s = SourceStream::build(&blocks);
        let sem = heading_chunks(&s, false);
        assert_eq!(sem.len(), 2);
        assert_eq!(sem[0].heading, "某章 > 第一节");
        assert_eq!(sem[0].page, Some(1));
        let book = heading_chunks(&s, true);
        assert_eq!(book.len(), 1);
        assert_eq!(book[0].heading, "某章");
        assert_eq!(book[0].page, None, "跨页合并宁可不显示页码");
        assert_eq!(book[0].text, "第一节的内容\n第二节的内容");
        assert_eq!(s.slice(book[0].start, book[0].end), "第一节的内容\n第二节的内容");
    }

    /// B3：合并器口径与 Python `_fill` 对拍——同组超长 flush、overlap 尾巴回退、
    /// 单 block 超 cap 句切；每块 span 都落在源流内且非空。
    #[test]
    fn merge_pipeline_matches_python_fill_and_spans_stay_in_stream() {
        let long_a = "甲".repeat(500);
        let long_b = "乙".repeat(500);
        let s = SourceStream::build(&[blk(&long_a, "H", Some(1)), blk(&long_b, "H", Some(2))]);
        // block1=[0,500)，块间 '\n' 在 500，block2=[501,1001)
        let units: Vec<Unit> = s.blocks.iter().map(|b| Unit { start: b.start, end: b.end, page: b.page }).collect();

        // 结构切分（overlap=0）：500+1+500 > 640 → 两块，各自独立页码与精确区间
        let merged = fill_units(&s, "H", &units, 0);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].page, Some(1));
        assert_eq!(merged[1].page, Some(2));
        assert_eq!(s.slice(merged[0].start, merged[0].end), long_a);
        assert_eq!(s.slice(merged[1].start, merged[1].end), long_b);
        assert_eq!(est_tokens(merged[0].text.chars().count()), 313);

        // 带重叠合并（general 形状）：第二块以第一块尾 OVERLAP_CHARS 个字符开头
        let merged = fill_units(&s, "H", &units, OVERLAP_CHARS);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].start, 500 - OVERLAP_CHARS);
        assert_eq!(merged[1].end, 1001);
        assert!(merged[1].text.starts_with(&"甲".repeat(10)));

        // 单 block 超 UNIT_CAP：按句末标点切，偏移区间与文本逐字对应（无插入换行差异）
        let sentences = "第一句。".repeat(300); // 1200 字符 > 543
        let s2 = SourceStream::build(&[blk(&sentences, "H", None)]);
        let merged = heading_chunks(&s2, false);
        assert!(merged.len() >= 2);
        for m in &merged {
            assert_eq!(s2.slice(m.start, m.end), m.text, "句切块的文本必须等于源流切片");
            assert!(m.text.chars().count() <= UNIT_CAP + "第一句。".chars().count());
        }
    }

    /// B3-general：文档服务产物按顺序游标定位；重叠块起点允许回退；
    /// 含插入换行的文本（句切产物）定位不到就落 None，绝不错位。
    #[test]
    fn general_offsets_locate_sequentially_or_null() {
        let s = SourceStream::build(&[blk("甲。乙。丙。丁。", "", None)]);
        let chunks = vec![mk_chunk("甲。乙。"), mk_chunk("乙。丙。"), mk_chunk("甲。\n乙。")];
        let spans = locate_offsets(&s, &chunks);
        assert_eq!(spans[0], Some(CharSpan { start: 0, end: 4 }));
        assert_eq!(spans[1], Some(CharSpan { start: 2, end: 6 }));
        assert_eq!(spans[2], None);
    }

    /// B7：同 hash 命中的处置分派——已入库/进行中复用（不重复扣解析与向量），
    /// 失败/半成功/未知状态一律影子重建（幂等，不重复建行）。
    #[test]
    fn dedup_action_reuses_live_docs_and_reprocesses_broken_ones() {
        assert_eq!(dedup_action(Some("embedded")), DedupAction::Reuse);
        assert_eq!(dedup_action(Some("pending")), DedupAction::Reuse);
        assert_eq!(dedup_action(Some("parsing")), DedupAction::Reuse);
        assert_eq!(dedup_action(Some("chunked")), DedupAction::Reprocess);
        assert_eq!(dedup_action(Some("failed")), DedupAction::Reprocess);
        assert_eq!(dedup_action(Some("未来新状态")), DedupAction::Reprocess);
        assert_eq!(dedup_action(None), DedupAction::Reprocess);
    }

    /// failed 文档同 hash 重传走 Rebuild；影子构建成功后必须先补回缺失原件，再切 DB。
    /// 构建失败仍只清 stage，线上文件/索引保持原样。
    #[test]
    fn failed_dedup_rebuild_restores_original_before_database_switch() {
        assert_eq!(dedup_action(Some("failed")), DedupAction::Reprocess);
        let src = include_str!("ingest.rs");
        let body = src.split("pub async fn reprocess").nth(1).unwrap();
        let body = body.split("/// `reprocess` 的原件自愈").next().unwrap();
        let build = body.find("build_shadow(").expect("Rebuild 必须走影子构建");
        let failed_cleanup = body
            .find("remove_staged_file(")
            .expect("影子构建失败必须清 stage");
        let restore = body
            .find("restore_missing_live_file(")
            .expect("Rebuild 必须自愈缺失原件");
        let switch = body
            .find("store::replace_chunks(")
            .expect("Rebuild 必须切换 DB 影子索引");
        let lock = body
            .find("FILE_SWITCH_LOCK")
            .expect("原件自愈与 DB 切换必须持互斥锁");
        assert!(
            build < failed_cleanup && failed_cleanup < lock && lock < restore && restore < switch,
            "顺序必须是 影子构建 → 失败清理/原件自愈 → DB 切换: {body}"
        );
        assert!(
            body.contains("doc_path(cfg, doc_id, &current.name)"),
            "live 扩展名必须按库内文档名生成，不能随重传文件名漂移: {body}"
        );
    }

    #[tokio::test]
    async fn missing_live_promotes_stage_and_existing_live_discards_stage() {
        let root = std::env::temp_dir().join(format!("dms-ai-rebuild-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let live = root.join("doc.pdf");
        let stage = root.join("stage.pdf");

        tokio::fs::write(&stage, b"recovered original")
            .await
            .unwrap();
        restore_missing_live_file(&stage, &live, "doc")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&live).await.unwrap(), b"recovered original");
        assert!(
            !tokio::fs::try_exists(&stage).await.unwrap(),
            "rename 后 stage 必须消失"
        );

        tokio::fs::write(&live, b"keep live").await.unwrap();
        tokio::fs::write(&stage, b"discard stage").await.unwrap();
        restore_missing_live_file(&stage, &live, "doc")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&live).await.unwrap(), b"keep live");
        assert!(
            !tokio::fs::try_exists(&stage).await.unwrap(),
            "live 已存在时必须清 stage"
        );

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    /// 同名覆盖命中分派：进行态（pending/parsing）的旧版本首入链还在跑，此时覆盖会与在跑的
    /// `insert_chunks` 互踩（那条链 `ON CONFLICT` 不重入）→ Conflict 拒绝；其余状态
    /// （含 failed/未知）一律 Overwrite——影子切换是全量替换，对任何起始状态都安全，
    /// failed 文档由此获得「重传同名文件」的自然重试路径。
    #[test]
    fn overwrite_action_conflicts_only_with_in_flight_ingest() {
        assert_eq!(overwrite_action(Some("embedded")), OverwriteAction::Overwrite);
        assert_eq!(overwrite_action(Some("chunked")), OverwriteAction::Overwrite);
        assert_eq!(overwrite_action(Some("failed")), OverwriteAction::Overwrite);
        assert_eq!(overwrite_action(Some("未来新状态")), OverwriteAction::Overwrite);
        assert_eq!(overwrite_action(None), OverwriteAction::Overwrite);
        assert_eq!(overwrite_action(Some("pending")), OverwriteAction::Conflict);
        assert_eq!(overwrite_action(Some("parsing")), OverwriteAction::Conflict);
    }

    /// 覆盖命中判定的分派顺序（源码钉住）：sha 去重 → 同名查找 → 新建。
    /// - 同内容（无论落在哪个目录/哪篇名下）先被 `find_by_sha` 秒传复用——
    ///   (space_id, sha256) 唯一约束下同内容全空间只能有一篇；
    /// - 同目录同名不同内容走 `overwrite_dispatch`（`IngestJob::Overwrite`）；
    /// - 目录口径是请求解析出的目标目录（空 = 根目录）——不同目录的同名文档查不到，
    ///   落到新建，不产生覆盖。
    #[test]
    fn prepare_dispatches_sha_dedup_then_name_overwrite_then_fresh() {
        let src = include_str!("ingest.rs");
        let prep = src.split("pub async fn prepare").nth(1).unwrap();
        let prep = prep.split("/// 入库重活分派").next().unwrap();
        let sha = prep.find("store::find_by_sha(").expect("prepare 缺 sha 去重");
        let name = prep.find("store::find_by_name_in_folder(").expect("prepare 缺同名覆盖分派");
        let insert = prep.find("store::insert_doc(").expect("prepare 缺新建");
        assert!(sha < name && name < insert, "分派顺序必须是 sha 去重 → 同名覆盖 → 新建: {prep}");
        assert!(prep.contains("overwrite_dispatch("), "同名命中必须走覆盖分派: {prep}");
        // 目录口径随请求解析值传入（folder_id 空 = 根目录），不是全空间查找
        assert!(prep.contains("folder_id.as_deref()"), "同名查找必须限定目标目录: {prep}");
        // 覆盖分派的产物：Overwrite 任务 + replaced 透出标记
        let od = src.split("async fn overwrite_dispatch").nth(1).unwrap();
        let od = od.split("\n}\n").next().unwrap();
        assert!(od.contains("IngestJob::Overwrite"), "覆盖分派必须产 Overwrite 任务: {od}");
        assert!(od.contains("replaced: Some("), "覆盖分派必须置 replaced 标记: {od}");
    }

    /// 覆盖链的失败方向（源码钉住）：先建后切——影子构建成功前旧版本的文件/索引/数据源
    /// 分毫不动；换文件（带回滚副本）必须排在切库（replace_chunks）之前（反了会把
    /// 「新 sha + 旧文件」钉死进库，秒传去重被毒化、旧文件永无自愈）；通道②退役/重建
    /// 排在索引切换之后（切换失败时旧数据源还在服役）。
    #[test]
    fn overwrite_switches_shadow_first_and_retires_tabular_last() {
        let src = include_str!("ingest.rs");
        let body = src.split("async fn overwrite(").nth(1).unwrap();
        let body = body.split("/// 覆盖的通道②收尾").next().unwrap();
        let build = body.find("build_shadow(").expect("覆盖必须走影子构建");
        let rename = body
            .find("install_staged_file(")
            .expect("覆盖必须保留旧原件回滚副本");
        let switch = body
            .find("store::replace_chunks(")
            .expect("覆盖必须走影子切换");
        let tabular = body.find("overwrite_tabular(").expect("覆盖必须重建通道②");
        assert!(
            build < rename && rename < switch && switch < tabular,
            "覆盖链顺序必须是 影子构建 → 换文件 → 切库 → 通道②: {body}"
        );
        assert!(
            !body.contains("insert_chunks("),
            "覆盖链不许走首入链落块（ON CONFLICT 不重入）: {body}"
        );
        assert!(
            body.contains("Some(&meta)"),
            "文件元数据必须随索引同语句切换: {body}"
        );
        assert!(
            body.contains("restore_live_file("),
            "DB 切换失败必须当场恢复旧原件: {body}"
        );
        // 并发覆盖同一文档：rename→切库→通道② 的临界区必须互斥（两半状态不自愈，见锁注释）
        let lock = body
            .find("FILE_SWITCH_LOCK")
            .expect("覆盖临界区必须持互斥锁");
        assert!(lock < rename, "锁必须在换文件之前拿: {body}");
        // 覆盖保留原文档元数据（需求语义）——名字没变，版本推断结果也不会变，不许顺手重推
        assert!(!body.contains("try_apply_inferred_version"), "覆盖保留既有版本元数据: {body}");
    }

    #[tokio::test]
    async fn overwrite_file_swap_can_restore_old_live_after_database_failure() {
        let root = std::env::temp_dir().join(format!("dms-ai-overwrite-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let live = root.join("doc.pdf");
        let stage = root.join("stage.pdf");
        tokio::fs::write(&live, b"old live").await.unwrap();
        tokio::fs::write(&stage, b"new staged").await.unwrap();

        let rollback = install_staged_file(&stage, &live, "doc").await.unwrap();
        assert_eq!(tokio::fs::read(&live).await.unwrap(), b"new staged");
        assert_eq!(
            tokio::fs::read(rollback.as_ref().unwrap()).await.unwrap(),
            b"old live"
        );

        // 模拟 replace_chunks Err：文件回滚后旧索引再次指向旧原件。
        restore_live_file(&live, rollback.as_deref(), "doc")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&live).await.unwrap(), b"old live");
        assert!(!tokio::fs::try_exists(&stage).await.unwrap());
        assert!(!tokio::fs::try_exists(rollback.as_ref().unwrap())
            .await
            .unwrap());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    /// 覆盖的通道②（源码钉住）：旧物理表先退役（drop 幂等，旧版本非表格时 no-op），
    /// 重建复用 Fresh 同一条 `tabular_channel`（写权限复核 + 建表失败只降级的语义不养第二份）。
    #[test]
    fn overwrite_tabular_drops_old_source_then_reuses_the_fresh_channel() {
        let src = include_str!("ingest.rs");
        let body = src.split("async fn overwrite_tabular").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let drop = body.find("tabular::drop_source(").expect("旧物理表必须先退役");
        let rebuild = body.find("tabular_channel(").expect("重建必须复用 tabular_channel");
        assert!(drop < rebuild, "先退役旧表再建新表: {body}");
        let overwrite = src.split("async fn overwrite(").nth(1).unwrap();
        let overwrite = overwrite.split("/// 覆盖的通道②收尾").next().unwrap();
        assert!(
            overwrite.contains("已越过 commit boundary"),
            "切库后的问数失败必须降级，不能冒充影子失败: {overwrite}"
        );
        assert!(
            overwrite.contains("store::append_notice("),
            "问数降级原因必须在 UI 可见: {overwrite}"
        );
    }

    /// 并发删除窗口（锚点）：find_by_sha 命中后 get_doc 为 None 时必须直接 NotFound，
    /// 不许走 Rebuild 落一个「写权限已失效」的误导文案
    #[test]
    fn dedup_after_concurrent_delete_is_not_found() {
        let src = include_str!("ingest.rs");
        let body = src.split("fn dedup_dispatch").nth(1).unwrap();
        let body = body.split("/// dedup 命中文档的处置").next().unwrap();
        assert!(body.contains("KbError::NotFound"), "并发删除必须报不存在而非权限错（若是改名请同步改本锚点）");
    }

    /// owning ↔ 借用两种形态必须逐字段同值：重活跨 spawn 边界走的就是这条换算
    #[test]
    fn owned_upload_req_roundtrips_into_borrowed_form() {
        let owned = OwnedUploadReq {
            space_id: "kb-hr".into(),
            folder_id: Some("f1".into()),
            file_name: "制度.pdf".into(),
            mime: "application/pdf".into(),
            bytes: b"%PDF-1".to_vec(),
            preset: Some("qa".into()),
        };
        let req = owned.as_req();
        assert_eq!(req.space_id, "kb-hr");
        assert_eq!(req.folder_id, Some("f1"));
        assert_eq!(req.file_name, "制度.pdf");
        assert_eq!(req.mime, "application/pdf");
        assert_eq!(req.bytes, b"%PDF-1");
        assert_eq!(req.preset, Some("qa"));
        // 缺省臂：None 不许变成 Some("")
        let bare = OwnedUploadReq::from_req(&UploadReq {
            space_id: "s", folder_id: None, file_name: "a.txt", mime: "text/plain",
            bytes: b"x", preset: None,
        });
        assert!(bare.folder_id.is_none() && bare.preset.is_none());
    }

    /// 快/慢路径切分契约（源码断言）：
    /// - `prepare` 只做快路径（不含 parse/chunk/embed 调用），且把状态推进到 parsing、完成落盘——
    ///   响应的进行态与「doc 行可见即文件可读」都靠这两行；
    /// - `run_job` 的 Fresh 臂失败必须落 `failed`（不许静默），Rebuild 臂必须走 `reprocess`
    ///   影子链、Overwrite 臂必须走 `overwrite` 覆盖链；两者失败另落最近入库错误。
    #[test]
    fn prepare_is_fast_path_and_run_job_owns_heavy_work() {
        let src = include_str!("ingest.rs");
        let prep = src.split("pub async fn prepare").nth(1).unwrap();
        let prep = prep.split("/// 入库重活分派").next().unwrap();
        for heavy in ["parse_input(", "chunk_with_preset(", "embed_batch(", "insert_chunks("] {
            assert!(!prep.contains(heavy), "快路径不许碰重活 {heavy}");
        }
        assert!(prep.contains("DocStatus::Parsing"), "快路径必须先推进到 parsing 再交还");
        assert!(prep.contains("tokio::fs::write"), "落盘必须在快路径完成");

        let job = src.split("pub async fn run_job").nth(1).unwrap();
        let job = job.split("\n}\n").next().unwrap();
        let fresh = job.split("IngestJob::Fresh").nth(1).unwrap();
        assert!(fresh.contains("DocStatus::Failed"), "Fresh 失败文案必须落库: {fresh}");
        let rebuild = job.split("IngestJob::Rebuild").nth(1).unwrap();
        assert!(rebuild.contains("reprocess("), "Rebuild 必须走影子重建链: {rebuild}");
        let overwrite = job.split("IngestJob::Overwrite").nth(1).unwrap();
        assert!(
            overwrite.contains("overwrite("),
            "Overwrite 必须走同名覆盖链: {overwrite}"
        );
        assert!(
            job.matches("persist_shadow_failure(").count() >= 2,
            "Rebuild/Overwrite 失败都必须持久化: {job}"
        );
        let persist = src.split("async fn persist_shadow_failure").nth(1).unwrap();
        let persist = persist.split("/// 版本元数据自动补全").next().unwrap();
        assert!(
            persist.contains("set_last_ingest_error"),
            "影子失败不许覆盖线上 status: {persist}"
        );
        assert!(
            !persist.contains("set_status"),
            "影子失败不许把可检索旧版改成 failed: {persist}"
        );
    }

    /// 落库形状契约：Rust preset 的 chunks 与 spans 等长平行，tokens 按统一口径估算
    #[test]
    fn rust_presets_emit_aligned_chunks_and_spans() {
        let blocks = vec![
            blk("甲段内容", "H1", Some(1)),
            blk("乙段内容", "H2", Some(2)),
        ];
        let spanned = to_spanned(heading_chunks(&SourceStream::build(&blocks), false));
        assert_eq!(spanned.chunks.len(), 2);
        assert_eq!(spanned.spans.len(), spanned.chunks.len());
        assert_eq!(spanned.chunks[0].tokens, est_tokens(4));
        assert_eq!(spanned.chunks[0].heading_path, "H1");
        assert_eq!(spanned.spans[1], Some(CharSpan { start: 5, end: 9 }));
    }
}
