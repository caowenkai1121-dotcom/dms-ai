//! Python 文档服务客户端（与 embed 同进程同端口，裁决 V1）：
//! `POST /parse` → `{blocks, page_count, sheets}`、`POST /chunk` → `{chunks}`、`GET /health`。
//! 服务端契约见 `tools/embed_service.py` 的模块 doc。
//!
//! 三条纪律：
//! 1. 解析给 120s（几十 MB 的 PDF 走 pymupdf 是真的慢），分块 30s；
//! 2. **确定性失败不重试也不熔断**（扫描版 PDF / 不支持的类型 → 换一份文件才有意义，
//!    一份坏文件不该让后续 5 分钟的上传全废）；
//! 3. 网络类失败沿用 embed 同款 300s 熔断——就地一个 `AtomicU64`，不造通用中间件。

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 大文件解析超时
const PARSE_TIMEOUT_SECS: u64 = 120;
/// 分块超时（纯文本切分，不该慢）
const CHUNK_TIMEOUT_SECS: u64 = 30;
/// 健康检查超时
const HEALTH_TIMEOUT_SECS: u64 = 3;
/// 网络失败后冷却期（与 `embed.rs` 同款）
const COOLDOWN_SECS: u64 = 300;

/// 解析结果。`sheets` 非空即表格类文件（xlsx/csv），走双通道的那一半。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ParsedDoc {
    pub blocks: Vec<Block>,
    pub page_count: i32,
    pub sheets: Vec<Sheet>,
    /// OCR 补页、部分页面未识别等解析提示。不是失败，但必须让用户看见。
    pub notes: Vec<String>,
}

/// 解析出的文本块（`/parse` 的产物，也是 `/chunk` 的入参）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Block {
    pub text: String,
    pub page: Option<i32>,
    /// 标题层级路径，例 `第三章 > 3.2 报销标准`
    pub heading_path: String,
}

/// 分块结果（一块＝一条 `kb.chunk`）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Chunk {
    pub text: String,
    pub heading_path: String,
    pub page: Option<i32>,
    pub tokens: i32,
}

/// 表格 sheet（i32/i64 对齐 PG 的 int，省掉到处 cast）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Sheet {
    pub name: String,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug)]
pub enum DocError {
    /// 熔断冷却期内直接失败，不发请求
    Cooldown,
    Transport(String),
    Api { status: u16, body: String },
    /// 扫描版 PDF 无文本层——确定性失败，重试无意义
    NoTextLayer,
    /// 服务端不认这个类型——确定性失败
    Unsupported(String),
    /// 路径不存在（Python 侧 404 not_found）——确定性失败
    NotFound(String),
    /// 单 sheet 超行/列上限（Python 侧 422 too_large，报错不截断）——确定性失败
    TooLarge(String),
}

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocError::Cooldown => write!(f, "文档服务近期连续失败，冷却中（{COOLDOWN_SECS}s）"),
            DocError::Transport(m) => write!(f, "文档服务不可达：{m}"),
            DocError::Api { status, body } => write!(f, "文档服务返回 {status}：{body}"),
            DocError::NoTextLayer => write!(f, "该 PDF 没有文本层（扫描版）"),
            DocError::Unsupported(m) => write!(f, "文档服务不支持该类型：{m}"),
            DocError::NotFound(m) => write!(f, "文档服务找不到文件：{m}"),
            DocError::TooLarge(m) => write!(f, "表格超出上限（20 万行 / 200 列）：{m}"),
        }
    }
}

impl std::error::Error for DocError {}

#[derive(Clone)]
pub struct DocService {
    base: String,
    cooldown_until: Arc<AtomicU64>,
    /// 实例级复用的 HTTP 客户端（与 `EmbedClient` 同一条修法）：超时逐请求设。
    http: reqwest::Client,
}

impl DocService {
    /// `base_url` 例 `http://127.0.0.1:8077`（与 `EmbedClient` 同一个）
    pub fn new(base_url: &str) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            cooldown_until: Arc::new(AtomicU64::new(0)),
            http: reqwest::Client::builder().build().expect("doc http client"),
        }
    }

    /// 解析文件（`path` 是服务端可见的绝对路径——同机部署，不传字节流）
    pub async fn parse(&self, path: &str, mime: Option<&str>) -> Result<ParsedDoc, DocError> {
        let body = serde_json::json!({ "path": path, "mime": mime.unwrap_or("") });
        self.post("/parse", &body, PARSE_TIMEOUT_SECS).await
    }

    /// 分块（`target_tokens`/`overlap` 由调用方给，本层不设默认值）
    pub async fn chunk(
        &self,
        blocks: &[Block],
        target_tokens: i32,
        overlap: i32,
    ) -> Result<Vec<Chunk>, DocError> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Resp {
            chunks: Vec<Chunk>,
        }
        let body = serde_json::json!({
            "blocks": blocks, "target_tokens": target_tokens, "overlap": overlap
        });
        let r: Resp = self.post("/chunk", &body, CHUNK_TIMEOUT_SECS).await?;
        Ok(r.chunks)
    }

    /// 健康检查：原样透传服务端 JSON（含各解析器是否装上的 `parse_ok`），进 `/api/health`
    pub async fn health(&self) -> Option<serde_json::Value> {
        let resp = self
            .http
            .get(format!("{}/health", self.base))
            .timeout(std::time::Duration::from_secs(HEALTH_TIMEOUT_SECS))
            .send()
            .await
            .ok()?;
        resp.json().await.ok()
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
        timeout: u64,
    ) -> Result<T, DocError> {
        if now() < self.cooldown_until.load(Ordering::Relaxed) {
            return Err(DocError::Cooldown);
        }
        let url = format!("{}{path}", self.base);
        let resp = match self
            .http
            .post(&url)
            .json(body)
            .timeout(std::time::Duration::from_secs(timeout))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // 网络类失败：熔断 300s（与 embed 同款语义）
                self.cooldown_until.store(now() + COOLDOWN_SECS, Ordering::Relaxed);
                return Err(DocError::Transport(e.to_string()));
            }
        };
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| DocError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            // 注意：HTTP 错误不进熔断——它是这一份文件的问题，不是服务的问题
            return Err(api_error(status, text));
        }
        serde_json::from_str(&text).map_err(|e| DocError::Transport(format!("响应不是预期 JSON：{e}")))
    }
}

/// 错误码 → 确定性失败变体；其余原样带上状态码与 body（可查）。
/// Python 侧四个码（tools/embed_service.py）：no_text_layer / unsupported / too_large 走 422，
/// not_found 走 404。落成确定性变体才能在 KbError 里映成 400/404 给用户看，
/// 否则会被 `other => Upstream` 吞成 500「文档服务不可用」——把用户的文件问题报成我们的故障。
fn api_error(status: u16, body: String) -> DocError {
    if status == 422 {
        if body.contains("no_text_layer") {
            return DocError::NoTextLayer;
        }
        if body.contains("unsupported") {
            return DocError::Unsupported(body);
        }
        if body.contains("too_large") {
            return DocError::TooLarge(body);
        }
    }
    if status == 404 && body.contains("not_found") {
        return DocError::NotFound(body);
    }
    DocError::Api { status, body }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_failures_mapped_from_422() {
        assert!(matches!(
            api_error(422, r#"{"error":"no_text_layer"}"#.into()),
            DocError::NoTextLayer
        ));
        assert!(matches!(
            api_error(422, r#"{"error":"unsupported: .rar"}"#.into()),
            DocError::Unsupported(_)
        ));
        // 同样的 body 换个状态码就不是确定性失败（500 是服务端崩，不是文件的错）
        assert!(matches!(
            api_error(500, r#"{"error":"no_text_layer"}"#.into()),
            DocError::Api { status: 500, .. }
        ));
        assert!(matches!(api_error(422, "boom".into()), DocError::Api { status: 422, .. }));
    }

    #[test]
    fn parse_tolerates_missing_fields() {
        let d: ParsedDoc = serde_json::from_str(r#"{"blocks":[{"text":"a"}]}"#).unwrap();
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.page_count, 0);
        assert!(d.sheets.is_empty());
        assert!(d.notes.is_empty());
        assert!(d.blocks[0].page.is_none());
    }

    #[test]
    fn parse_keeps_nonfatal_notes() {
        let d: ParsedDoc = serde_json::from_str(
            r#"{"blocks":[{"text":"a"}],"notes":["第 2 页已 OCR","第 5 页未识别"]}"#,
        )
        .unwrap();
        assert_eq!(d.notes, ["第 2 页已 OCR", "第 5 页未识别"]);
    }

    #[tokio::test]
    async fn conn_refused_then_cooldown() {
        let s = DocService::new("http://127.0.0.1:1/");
        assert!(matches!(s.parse("x.pdf", None).await, Err(DocError::Transport(_))));
        // `Cooldown` 变体本身就是「没发请求」的确定性证据；墙钟断言（<500ms）在负载高时会假红，不要。
        assert!(matches!(s.clone().chunk(&[], 400, 60).await, Err(DocError::Cooldown)));
    }
}
