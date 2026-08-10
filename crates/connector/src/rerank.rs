//! OpenAI 兼容 rerank 服务客户端（Jina / vLLM / TEI / SiliconFlow 同款 `POST {base}/rerank`）。
//! 依据 `docs/research/yuxi.json` B5「Reranker 可插拔 + 失败回退」：
//! 精排只是检索的加分项，**任何失败一律 `None`**，由调用方回退 RRF 原序 —— rerank 永不许挂掉检索。
//!
//! 与 `embed.rs` 同款的纪律：3s 超时（用户在等）+ send 失败熔断 300s + 响应形状不符不熔断。
//! 配置只走环境变量（`from_env`）：`DMS_RERANK_BASE_URL` / `DMS_RERANK_MODEL` 缺一即关闭，
//! `DMS_RERANK_API_KEY` 可空（内网自建 rerank 常不鉴权）。关闭时检索路径一行都不变。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 单次请求超时（对齐 embed 问句侧的 3s）：精排在检索关键路径上，用户在等。
const TIMEOUT_SECS: u64 = 3;
/// send 失败后的冷却期（对齐 embed 的 300s）：rerank 服务挂时每问白等一个 3s 超时才是事故。
const COOLDOWN_SECS: u64 = 300;

#[derive(Clone)]
pub struct RerankClient {
    url: String,
    api_key: Option<String>,
    model: String,
    cooldown_until: Arc<AtomicU64>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl RerankClient {
    /// `base_url` 例 `http://127.0.0.1:8090`（请求打 `{base}/rerank`）。
    pub fn new(base_url: &str, api_key: Option<&str>, model: &str) -> Self {
        Self {
            url: format!("{}/rerank", base_url.trim_end_matches('/')),
            api_key: api_key.map(str::trim).filter(|k| !k.is_empty()).map(str::to_string),
            model: model.to_string(),
            cooldown_until: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 环境变量入口。`DMS_RERANK_BASE_URL` / `DMS_RERANK_MODEL` 缺任一（或为空串）→ `None`＝功能关闭。
    pub fn from_env() -> Option<Self> {
        Self::from_vars(
            std::env::var("DMS_RERANK_BASE_URL").ok().as_deref(),
            std::env::var("DMS_RERANK_API_KEY").ok().as_deref(),
            std::env::var("DMS_RERANK_MODEL").ok().as_deref(),
        )
    }

    /// 与 `from_env` 同判据的纯函数核：单测不碰进程环境（并行测试改 env 会互相踩）。
    fn from_vars(base: Option<&str>, api_key: Option<&str>, model: Option<&str>) -> Option<Self> {
        let base = base?.trim();
        let model = model?.trim();
        if base.is_empty() || model.is_empty() {
            return None;
        }
        Some(Self::new(base, api_key, model))
    }

    /// 对 `docs` 逐条打相关度分，返回**与输入等长、按下标对齐**的分数。
    /// 空输入 / 熔断中 / 超时 / 服务挂 / 响应形状不符 → `None`（调用方回退原排序）。
    pub async fn rerank(&self, query: &str, docs: &[&str]) -> Option<Vec<f32>> {
        if docs.is_empty() {
            return None;
        }
        if now() < self.cooldown_until.load(Ordering::Relaxed) {
            return None;
        }
        // ponytail: 每次调用新建 Client＝与 embed.rs 同款的历史行为（丢连接复用）。
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .ok()?;
        // `top_n = 文档数`：要的是全量重排，不是截断 —— 截断由调用方按自己的窗口做。
        let body = serde_json::json!({
            "model": self.model,
            "query": query,
            "documents": docs,
            "top_n": docs.len(),
        });
        let mut req = client.post(&self.url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        match req.send().await {
            Ok(resp) => {
                let v: serde_json::Value = resp.json().await.ok()?;
                // 形状不符不熔断（对齐 embed）：服务活着只是回了别的，熔断解决不了它。
                parse_scores(&v, docs.len())
            }
            Err(_) => {
                // send 失败（连接拒/超时）才熔断 300s（对齐 embed）。
                self.cooldown_until.store(now() + COOLDOWN_SECS, Ordering::Relaxed);
                None
            }
        }
    }
}

/// 响应 `{"results":[{"index":i,"relevance_score":s},…]}` → 与输入等长的分数向量。
///
/// 条数/下标不齐即整体 `None`（对齐 embed「条数不符即整批降级」）：
/// 半份分数会让调用方把没打分的块当 0 分压底 —— 那不是降精度，是丢证据。
fn parse_scores(v: &serde_json::Value, n: usize) -> Option<Vec<f32>> {
    let arr = v["results"].as_array()?;
    if arr.len() != n {
        return None;
    }
    let mut out: Vec<Option<f32>> = vec![None; n];
    for item in arr {
        let i = usize::try_from(item["index"].as_u64()?).ok()?;
        let s = item["relevance_score"].as_f64()? as f32;
        if i >= n {
            return None;
        }
        out[i] = Some(s);
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_unless_base_and_model_are_both_set() {
        assert!(RerankClient::from_vars(None, None, Some("m")).is_none());
        assert!(RerankClient::from_vars(Some("http://h:1"), None, None).is_none());
        assert!(RerankClient::from_vars(Some("  "), None, Some("m")).is_none(), "空串 = 未配置");
        assert!(RerankClient::from_vars(Some("http://h:1"), None, Some("")).is_none());
        assert!(RerankClient::from_vars(Some("http://h:1"), None, Some("m")).is_some(), "key 可空（内网自建常不鉴权）");
        let c = RerankClient::from_vars(Some("http://h:1"), Some("  "), Some("m")).unwrap();
        assert!(c.api_key.is_none(), "空白 key 视为没配，不许发出 `Authorization: Bearer  `");
    }

    #[test]
    fn base_url_trailing_slash_tolerated() {
        assert_eq!(RerankClient::new("http://h:8090/", None, "m").url, "http://h:8090/rerank");
    }

    #[test]
    fn parse_requires_exactly_one_score_per_doc() {
        let v = serde_json::json!({"results": [
            {"index": 1, "relevance_score": 0.9},
            {"index": 0, "relevance_score": 0.1},
        ]});
        assert_eq!(parse_scores(&v, 2), Some(vec![0.1, 0.9]), "分数按下标归位，与返回顺序无关");
        assert!(parse_scores(&serde_json::json!({}), 2).is_none());
        assert!(parse_scores(&serde_json::json!({"results": [{"index": 0, "relevance_score": 0.1}]}), 2).is_none(), "少一条 = 半份分数，整体降级");
        assert!(parse_scores(&serde_json::json!({"results": [
            {"index": 0, "relevance_score": 0.1},
            {"index": 2, "relevance_score": 0.2}
        ]}), 2).is_none(), "下标越界 = 形状不符");
        assert!(parse_scores(&serde_json::json!({"results": [
            {"index": 0, "relevance_score": 0.1},
            {"index": 0, "relevance_score": 0.2},
            {"index": 1, "relevance_score": 0.3}
        ]}), 2).is_none(), "条数不符（重复下标顶掉了名额）");
    }

    /// 最小 HTTP 桩（与 embed.rs 测试同款：不引新依赖）。记录每次请求的 body 与是否带 key，
    /// 按 `score_of` 给第 i 篇文档打分。`hang` 时直接不写响应（逼客户端 3s 超时）。
    async fn stub(
        score_of: fn(usize) -> f32,
    ) -> (String, Arc<std::sync::Mutex<Vec<(serde_json::Value, bool)>>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen = Arc::new(std::sync::Mutex::new(Vec::<(serde_json::Value, bool)>::new()));
        let s0 = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else { return };
                let s = s0.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    let head = loop {
                        if let Some(h) = find(&buf, b"\r\n\r\n") {
                            if buf.len() >= h + 4 + content_len(&buf[..h]) {
                                break h;
                            }
                        }
                        let mut b = [0u8; 8192];
                        match sock.read(&mut b).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&b[..n]),
                        }
                    };
                    let head_text = String::from_utf8_lossy(&buf[..head]).to_lowercase();
                    let authed = head_text.contains("authorization: bearer ");
                    let v: serde_json::Value = serde_json::from_slice(&buf[head + 4..]).unwrap();
                    let n = v["documents"].as_array().unwrap().len();
                    s.lock().unwrap().push((v, authed));
                    let results: Vec<serde_json::Value> = (0..n)
                        .map(|i| serde_json::json!({"index": i, "relevance_score": score_of(i)}))
                        .collect();
                    let body = serde_json::json!({"results": results}).to_string();
                    let _ = sock
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                });
            }
        });
        (base, seen)
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    fn content_len(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .to_lowercase()
            .split("content-length:")
            .nth(1)
            .and_then(|t| t.split("\r\n").next())
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn sends_openai_compatible_shape_and_aligns_scores() {
        let (base, seen) = stub(|i| 0.5 + i as f32).await;
        let c = RerankClient::new(&base, Some("k1"), "bge-reranker");
        let got = c.rerank("报销上限", &["甲".into(), "乙".into()]).await.unwrap();
        assert_eq!(got, vec![0.5, 1.5]);
        let (body, authed) = seen.lock().unwrap().pop().unwrap();
        assert!(authed, "配了 key 就要带 Bearer 头");
        assert_eq!(body["model"], "bge-reranker");
        assert_eq!(body["query"], "报销上限");
        assert_eq!(body["documents"], serde_json::json!(["甲", "乙"]));
        assert_eq!(body["top_n"], 2, "top_n = 文档数：截断是调用方的事");
    }

    #[tokio::test]
    async fn conn_refused_degrades_and_breaker_shared_across_clone() {
        let c = RerankClient::new("http://127.0.0.1:1", None, "m");
        assert!(c.rerank("q", &["a"]).await.is_none()); // send 失败 → 熔断 300s
        let c2 = c.clone();
        // 「没发请求」用熔断状态断言，不用墙钟（机器负载高时墙钟会假红，embed.rs 测试同款教训）
        assert!(c2.cooldown_until.load(Ordering::Relaxed) > now(), "clone 应共享同一个熔断状态");
        assert!(c2.rerank("q", &["a"]).await.is_none(), "冷却期内直接 None，不再白发请求");
    }

    #[tokio::test]
    async fn empty_docs_short_circuits_without_touching_the_breaker() {
        let c = RerankClient::new("http://127.0.0.1:1", None, "m");
        assert!(c.rerank("q", &[]).await.is_none());
        assert_eq!(c.cooldown_until.load(Ordering::Relaxed), 0, "空输入是调用方的事，不熔断");
    }
}
