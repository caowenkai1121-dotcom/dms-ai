//! Dify 外部只读知识库客户端（`POST {base}/datasets/{dataset}/retrieve`）。
//! 依据 `docs/research/yuxi.json` B9「外部 KB 连接器」（external_kb_router.py：Dify/Notion 等
//! 只读 KB 的 files/retrieve/open/find）：本客户端只落**检索候选**这一路，不开文件面/写入面。
//!
//! 与 `rerank.rs` 同款的纪律：3s 超时（用户在等）+ send 失败熔断 300s + 响应形状不符不熔断；
//! **任何失败一律 `None`**，由调用方回退原有召回路 —— 外部 KB 永不许挂掉检索。
//! 配置只走环境变量（`from_env`）：`DMS_EXT_KB_BASE_URL` / `DMS_EXT_KB_DATASET` 缺一即关闭，
//! `DMS_EXT_KB_API_KEY` 可空（内网部署可省）。关闭时检索路径一行都不变。
//!
//! ACL 语义（与 `knowledge::retrieve` 的落地注释同源）：外部 KB 是**独立授权源，配置即授权** ——
//! 这三个变量配齐，等于部署方把这只数据集授给全体 KB 用户；它走不到 `kb.doc` 的 ACL 子查询，
//! 作为交换，每条记录都带 `source_uri` 标注来源，让用户能看穿「这条证据来自外部系统」。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 单次请求超时（对齐 embed/rerank 问句侧的 3s）：外部检索在检索关键路径上，用户在等。
const TIMEOUT_SECS: u64 = 3;
/// send 失败后的冷却期（对齐 embed/rerank 的 300s）：外部服务挂时每问白等一个 3s 超时才是事故。
const COOLDOWN_SECS: u64 = 300;

/// 一条远程检索记录。`source_uri` 由客户端按 base/dataset/segment 拼出 —— 来源标注在
/// connector 收口，调用方拿到的每条记录都自带可回看的出处。
#[derive(Debug, Clone, PartialEq)]
pub struct ExtKbRecord {
    /// 远程段 id（Dify `segment.id`）：记录的唯一身份，`source_uri` 与去重锚点都靠它。
    pub segment_id: String,
    /// 远程文档 id（可空：外部系统未必给出）。
    pub document_id: String,
    /// 远程文档名（缺失时回退 document_id，再没有则「外部文档」）。
    pub document_name: String,
    /// 段正文（远程文本块；本地没有对应 chunk 行）。
    pub content: String,
    /// 远程服务返回的相关度分。**只留作诊断**：进 RRF 后只看名次不看分。
    pub score: f64,
    /// `"{base}/datasets/{dataset}#segment-{segment_id}"` —— 数据集 + 段 id 是最小完整出处。
    pub source_uri: String,
}

#[derive(Clone)]
pub struct ExtKbClient {
    url: String,
    source_prefix: String,
    api_key: Option<String>,
    cooldown_until: Arc<AtomicU64>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl ExtKbClient {
    /// `base_url` 例 `http://dify.internal/v1`（请求打 `{base}/datasets/{dataset}/retrieve`）。
    pub fn new(base_url: &str, api_key: Option<&str>, dataset: &str) -> Self {
        let prefix = format!("{}/datasets/{}", base_url.trim_end_matches('/'), dataset);
        Self {
            url: format!("{prefix}/retrieve"),
            source_prefix: prefix,
            api_key: api_key.map(str::trim).filter(|k| !k.is_empty()).map(str::to_string),
            cooldown_until: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 环境变量入口。`DMS_EXT_KB_BASE_URL` / `DMS_EXT_KB_DATASET` 缺任一（或为空串）→ `None`＝关闭。
    pub fn from_env() -> Option<Self> {
        Self::from_vars(
            std::env::var("DMS_EXT_KB_BASE_URL").ok().as_deref(),
            std::env::var("DMS_EXT_KB_API_KEY").ok().as_deref(),
            std::env::var("DMS_EXT_KB_DATASET").ok().as_deref(),
        )
    }

    /// 与 `from_env` 同判据的纯函数核：单测不碰进程环境（并行测试改 env 会互相踩）。
    fn from_vars(base: Option<&str>, api_key: Option<&str>, dataset: Option<&str>) -> Option<Self> {
        let base = base?.trim();
        let dataset = dataset?.trim();
        if base.is_empty() || dataset.is_empty() {
            return None;
        }
        Some(Self::new(base, api_key, dataset))
    }

    /// 检索远程数据集，返回**响应原序**（即名次序）的记录。
    /// 空问句 / 熔断中 / 超时 / 服务挂 / 响应形状不符 → `None`（调用方回退原有召回路）。
    pub async fn retrieve(&self, query: &str, top_k: usize) -> Option<Vec<ExtKbRecord>> {
        let query = query.trim();
        if query.is_empty() || top_k == 0 {
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
        // Dify 数据集检索的请求形状：`retrieval_model` 必须给全（缺省字段服务端按各自默认填，
        // 而我们要的是确定行为 —— top_k 由调用方定，阈值/精排一律关，过滤全留在本地 RRF）。
        let body = serde_json::json!({
            "query": query,
            "retrieval_model": {
                "search_method": "hybrid_search",
                "reranking_enable": false,
                "top_k": top_k,
                "score_threshold_enabled": false,
            },
        });
        let mut req = client.post(&self.url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        match req.send().await {
            Ok(resp) => {
                let v: serde_json::Value = resp.json().await.ok()?;
                // 形状不符不熔断（对齐 embed/rerank）：服务活着只是回了别的，熔断解决不了它。
                parse_records(&v, &self.source_prefix)
            }
            Err(_) => {
                // send 失败（连接拒/超时）才熔断 300s（对齐 embed/rerank）。
                self.cooldown_until.store(now() + COOLDOWN_SECS, Ordering::Relaxed);
                None
            }
        }
    }
}

/// 响应 `{"records":[{"segment":{…},"score":s},…]}` → 记录流（保持响应原序 = 名次序）。
/// `records` 缺席/非数组 = 形状不符 → 整体 `None`；单条缺 `segment.id` 或正文为空 → 丢这一条
/// （候选路宁缺勿滥：没有稳定 id 的块造不出 `source_uri`，没有正文的块无可引用）。
fn parse_records(v: &serde_json::Value, source_prefix: &str) -> Option<Vec<ExtKbRecord>> {
    let arr = v["records"].as_array()?;
    let out: Vec<ExtKbRecord> = arr
        .iter()
        .filter_map(|item| {
            let segment = &item["segment"];
            let segment_id = segment["id"].as_str()?.trim();
            if segment_id.is_empty() {
                return None;
            }
            let content = segment["content"].as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let document_id = segment["document_id"].as_str().unwrap_or("").to_string();
            let name = segment["document"]["name"].as_str().unwrap_or("").trim();
            let document_name = if !name.is_empty() {
                name.to_string()
            } else if !document_id.is_empty() {
                document_id.clone()
            } else {
                "外部文档".to_string()
            };
            Some(ExtKbRecord {
                segment_id: segment_id.to_string(),
                document_id,
                document_name,
                content: content.to_string(),
                score: item["score"].as_f64().unwrap_or(0.0),
                source_uri: format!("{source_prefix}#segment-{segment_id}"),
            })
        })
        .collect();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_unless_base_and_dataset_are_both_set() {
        assert!(ExtKbClient::from_vars(None, Some("k"), Some("ds")).is_none());
        assert!(ExtKbClient::from_vars(Some("http://h:1"), Some("k"), None).is_none());
        assert!(ExtKbClient::from_vars(Some("  "), Some("k"), Some("ds")).is_none(), "空串 = 未配置");
        assert!(ExtKbClient::from_vars(Some("http://h:1"), Some("k"), Some("")).is_none());
        assert!(
            ExtKbClient::from_vars(Some("http://h:1"), None, Some("ds")).is_some(),
            "key 可空（内网部署可省；配了 base+dataset 即授权）"
        );
        let c = ExtKbClient::from_vars(Some("http://h:1"), Some("  "), Some("ds")).unwrap();
        assert!(c.api_key.is_none(), "空白 key 视为没配，不许发出 `Authorization: Bearer  `");
    }

    #[test]
    fn url_and_provenance_prefix_are_derived_from_base_and_dataset() {
        let c = ExtKbClient::new("http://h:8090/v1/", None, "ds1");
        assert_eq!(c.url, "http://h:8090/v1/datasets/ds1/retrieve");
        assert_eq!(c.source_prefix, "http://h:8090/v1/datasets/ds1");
    }

    #[test]
    fn parse_keeps_response_order_and_marks_provenance() {
        let v = serde_json::json!({"query": {"content": "q"}, "records": [
            {"segment": {"id": "s1", "document_id": "d1", "content": "甲",
                         "document": {"name": "制度.md"}}, "score": 0.9},
            {"segment": {"id": "s2", "document_id": "d1", "content": "乙"}, "score": 0.5},
        ]});
        let out = parse_records(&v, "http://h/datasets/ds1").unwrap();
        assert_eq!(
            out.iter().map(|r| r.segment_id.as_str()).collect::<Vec<_>>(),
            vec!["s1", "s2"],
            "响应序即名次序，不许重排"
        );
        assert_eq!(out[0].document_name, "制度.md");
        assert_eq!(out[0].content, "甲");
        assert_eq!(out[0].score, 0.9);
        assert_eq!(out[0].source_uri, "http://h/datasets/ds1#segment-s1", "每条记录自带出处");
        assert_eq!(out[1].document_name, "d1", "没有文档名时回退 document_id");
    }

    #[test]
    fn parse_drops_records_without_identity_or_text() {
        let v = serde_json::json!({"records": [
            {"segment": {"document_id": "d1", "content": "没有 id"}, "score": 0.9},
            {"segment": {"id": "s2", "content": "  "}, "score": 0.8},
            {"segment": {"id": "s3", "content": "留得下的"}},
        ]});
        let out = parse_records(&v, "p").unwrap();
        assert_eq!(out.len(), 1, "缺 id / 空正文的记录必须丢弃");
        assert_eq!(out[0].segment_id, "s3");
        assert_eq!(out[0].score, 0.0, "score 缺席按 0.0（诊断用，RRF 只看名次）");
        assert_eq!(out[0].document_name, "外部文档", "连 document_id 都没有时的兜底名");
        assert!(parse_records(&serde_json::json!({}), "p").is_none(), "records 缺席 = 形状不符");
        assert!(parse_records(&serde_json::json!({"records": {}}), "p").is_none());
        assert_eq!(
            parse_records(&serde_json::json!({"records": []}), "p"),
            Some(vec![]),
            "空 records 是合法的空路（远程真没命中），不是形状不符"
        );
    }

    /// 最小 HTTP 桩（与 rerank.rs 测试同款：不引新依赖）。记录每次请求的 body 与是否带 key，
    /// 固定回两条记录；`delay` 用来逼客户端 3s 超时。
    async fn stub(
        delay: std::time::Duration,
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
                    // 必须按 Content-Length 读满（rerank.rs/embed.rs 测试桩同款教训）：半个 body
                    // 喂给 serde_json 会 panic 在桩里，看起来像「客户端没发全」。
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
                    s.lock().unwrap().push((v, authed));
                    tokio::time::sleep(delay).await;
                    let body = serde_json::json!({"records": [
                        {"segment": {"id": "s1", "document_id": "d1", "content": "第一条",
                                     "document": {"name": "外部制度.md"}}, "score": 0.9},
                        {"segment": {"id": "s2", "document_id": "d2", "content": "第二条"},
                         "score": 0.4},
                    ]})
                    .to_string();
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
    async fn sends_dify_retrieve_shape_and_keeps_response_order() {
        let (base, seen) = stub(std::time::Duration::ZERO).await;
        let c = ExtKbClient::new(&base, Some("k1"), "ds1");
        let got = c.retrieve("报销上限", 4).await.unwrap();
        assert_eq!(got.iter().map(|r| r.segment_id.as_str()).collect::<Vec<_>>(), vec!["s1", "s2"]);
        assert_eq!(got[0].document_name, "外部制度.md");
        assert_eq!(got[0].source_uri, format!("{base}/datasets/ds1#segment-s1"));
        let (body, authed) = seen.lock().unwrap().pop().unwrap();
        assert!(authed, "配了 key 就要带 Bearer 头（配置即授权的那半只凭证）");
        assert_eq!(body["query"], "报销上限");
        assert_eq!(body["retrieval_model"]["top_k"], 4, "top_k 由调用方定，原样透传");
        assert_eq!(body["retrieval_model"]["search_method"], "hybrid_search");
        assert_eq!(body["retrieval_model"]["reranking_enable"], false);
        assert_eq!(body["retrieval_model"]["score_threshold_enabled"], false);
    }

    #[tokio::test]
    async fn conn_refused_degrades_and_breaker_shared_across_clone() {
        let c = ExtKbClient::new("http://127.0.0.1:1", None, "ds1");
        assert!(c.retrieve("q", 4).await.is_none()); // send 失败 → 熔断 300s
        let c2 = c.clone();
        // 「没发请求」用熔断状态断言，不用墙钟（机器负载高时墙钟会假红，embed.rs 测试同款教训）
        assert!(c2.cooldown_until.load(Ordering::Relaxed) > now(), "clone 应共享同一个熔断状态");
        assert!(c2.retrieve("q", 4).await.is_none(), "冷却期内直接 None，不再白发请求");
    }

    /// 服务挂住不写响应 → 3s 超时按 send 失败处理：返 None 且熔断（对齐 embed/rerank 语义）。
    #[tokio::test]
    async fn slow_response_times_out_and_trips_the_breaker() {
        let (base, _) = stub(std::time::Duration::from_secs(4)).await;
        let c = ExtKbClient::new(&base, None, "ds1");
        assert!(c.retrieve("q", 4).await.is_none(), "超过 3s 必须超时（用户在等）");
        assert!(c.cooldown_until.load(Ordering::Relaxed) > now(), "超时 = send 失败，熔断 300s");
    }

    #[tokio::test]
    async fn empty_query_short_circuits_without_touching_the_breaker() {
        let c = ExtKbClient::new("http://127.0.0.1:1", None, "ds1");
        assert!(c.retrieve("  ", 4).await.is_none());
        assert!(c.retrieve("q", 0).await.is_none());
        assert_eq!(c.cooldown_until.load(Ordering::Relaxed), 0, "空输入是调用方的事，不熔断");
    }
}
