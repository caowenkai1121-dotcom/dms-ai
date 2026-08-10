//! bge 向量服务客户端：批量 + query/passage 双模式 + 超时按条数算 + 300s 熔断。
//! 搬运源 `crates/server/src/embed.rs` 全文，语义逐字保留（超时/冷却/请求形状/`to_pgvector` 格式）；
//! 唯一结构改动是**实例化**（ARCHITECTURE §4.2「实例而非全局单例」）——
//! 熔断状态放 `Arc<AtomicU64>` 随 `Clone` 共享，等价于历史的全局 `static COOLDOWN_UNTIL`。
//! 服务挂时静默降级返回 `None`，调用方回落词典召回。
//! 请求形状 `{"texts":[...],"query":bool}` 与 `tools/embed_service.py` 的 `/embed` 一一对应。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 单次请求超时（对齐历史 3s）。**问句侧就这么多**：3s 是用户在等的预算。
const TIMEOUT_SECS: u64 = 3;
/// 失败后冷却期（对齐历史 300s）：防 embed 服务挂时每问白等一个超时
const COOLDOWN_SECS: u64 = 300;
/// 语料侧一批的条数。对齐 `tools/embed_service.py` 的 `KB_BATCH = 64`（CLI 重建路径早就是
/// 这么分的，只有入库这条服务路径没分）。
const BATCH: usize = 64;
/// 语料侧每条给的超时预算。实测（`embed_service.py` KB_BATCH 那段注释，同一台机器）：
/// 64 块 ≈ 9675 字一次 embed **2.21s**，即 ~35ms/块 —— 300ms/块是 8x 余量，
/// 一批 64 块上限 22.2s。
const PASSAGE_MS_PER_TEXT: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedMode {
    /// 查询侧（服务端 `query=true`，会加 BGE query 指令前缀）
    Query,
    /// 语料侧（入库向量）
    Passage,
}

#[derive(Clone)]
pub struct EmbedClient {
    url: String,
    cooldown_until: Arc<AtomicU64>,
    /// 实例级复用的 HTTP 客户端（`Clone` 共享同一连接池）。曾经每次调用新建一个
    /// Client＝每次都重付 TCP 握手、丢 keep-alive：问句侧一问 2~3 次调用，全是白付。
    /// 超时仍在**每次请求**上单独设（`RequestBuilder::timeout` 覆盖语义与
    /// `Client::builder().timeout` 逐字相同），不同条数/模式的超时预算不变。
    http: reqwest::Client,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl EmbedClient {
    /// `base_url` 例 `http://127.0.0.1:8077`（文档服务同进程同端口，见 `doc.rs`）
    pub fn new(base_url: &str) -> Self {
        Self {
            url: format!("{}/embed", base_url.trim_end_matches('/')),
            cooldown_until: Arc::new(AtomicU64::new(0)),
            http: reqwest::Client::builder().build().expect("embed http client"),
        }
    }

    /// 批量取向量。空输入 / 服务不可用 / 熔断中一律 `None`。
    pub async fn embed(&self, texts: &[String], mode: EmbedMode) -> Option<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return None;
        }
        if now() < self.cooldown_until.load(Ordering::Relaxed) {
            return None;
        }
        // 客户端在 `new` 时建好复用（连接池/keep-alive 跨调用生效）；超时逐请求设，
        // 与改动前「每次新建带 timeout 的 Client」同语义（reqwest 的请求级 timeout 即全程超时）。
        let req = self
            .http
            .post(&self.url)
            .json(&build_body(texts, mode))
            .timeout(timeout_for(texts.len(), mode));
        match req.send().await {
            Ok(resp) => {
                let v: serde_json::Value = resp.json().await.ok()?;
                let m = parse_embeddings(&v)?;
                // 条数不符即整批降级（不触发熔断，同形状不符）：少返几行会让 `ingest.rs` 的
                // `ids.zip(vecs)` 静默只写前 k 个块的向量，而 doc 照样推到 embedded ——
                // 「界面显示已入库、其实检索不到」正是本仓反复抓的那一族。
                (m.len() == texts.len()).then_some(m)
            }
            Err(_) => {
                // 熔断 300s（仅 send 失败触发；解析失败不触发——对齐历史）
                self.cooldown_until.store(now() + COOLDOWN_SECS, Ordering::Relaxed);
                None
            }
        }
    }

    /// 单条查询向量（512 维）。历史 `embed_query` 语义：Query 模式取首行。
    pub async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        self.embed(&[text.to_string()], EmbedMode::Query).await?.into_iter().next()
    }

    /// 语料批量（Passage 模式），知识库入库用。**按 `BATCH` 分批**。
    ///
    /// 🔴 缺陷现场：这里原先是一次 HTTP 发全部块，而超时是 query/passage 共用的 3s。
    /// 块大小 640 字（400 token × 1.6）、`kb_max_mb = 20` ⇒ 一篇 100 页 Word ≈ 275 块、
    /// 500 页 PDF ≈ 2500 块全塞进同一个 3s 请求 → 必超时 → doc 停在 `chunked` +
    /// 「向量服务不可用，稍后可重建」，**该文档永不进向量路**，还顺带把进程级熔断打到 300s。
    /// 没早暴露只是因为夹具小：`kb.chunk` 32 行、单篇最多 5 块、最长 529 字。
    ///
    /// 任一批失败即整篇返 `None`（调用方降级到文本检索，向量由 `embed_service.py revec` 后补）——
    /// 返回半份向量会让 `ingest.rs` 只写前几批却把 doc 推到 embedded。
    pub async fn embed_passages(&self, texts: &[String]) -> Option<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            out.extend(self.embed(batch, EmbedMode::Passage).await?);
        }
        Some(out)
    }

    /// 问句侧批量（Query 模式）：向量自愈补**语料问句**用（`embed_service.py` 的
    /// `embed(texts, is_query=True)` 同款 —— 同一列不许混两种模式的向量）。
    /// 与 `embed_passages` 同分批、同「任一批失败整篇 None」。
    pub async fn embed_queries(&self, texts: &[String]) -> Option<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            out.extend(self.embed(batch, EmbedMode::Query).await?);
        }
        Some(out)
    }
}

/// 超时预算。问句侧恒 3s（用户在等）；语料侧按条数算 —— 一批 64 块的活不可能在 3s 里干完，
/// 而「按条数算」不是「3s × N」：N 是**一批**的条数，整篇的总预算随批次线性增长，
/// 任何一批卡住都在 22s 内落地，不会攒成一个几百秒的单请求。
fn timeout_for(n: usize, mode: EmbedMode) -> std::time::Duration {
    match mode {
        EmbedMode::Query => std::time::Duration::from_secs(TIMEOUT_SECS),
        EmbedMode::Passage => std::time::Duration::from_millis(
            TIMEOUT_SECS * 1000 + PASSAGE_MS_PER_TEXT * n as u64,
        ),
    }
}

fn build_body(texts: &[String], mode: EmbedMode) -> serde_json::Value {
    serde_json::json!({ "texts": texts, "query": mode == EmbedMode::Query })
}

/// 响应 `{"embeddings": [[...], ...]}` → 矩阵；形状不符返回 `None`（不触发熔断，对齐历史）
fn parse_embeddings(v: &serde_json::Value) -> Option<Vec<Vec<f32>>> {
    let arr = v["embeddings"].as_array()?;
    arr.iter()
        .map(|row| {
            let r = row.as_array()?;
            Some(r.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
        })
        .collect()
}

/// f32 向量 → pgvector 字面量 `'[...]'`（原样迁自 `server/embed.rs`，6 位小数不许变——
/// `tools/embed_service.py` 回写的字面量是同一格式）
pub fn to_pgvector(v: &[f32]) -> String {
    let mut s = String::from("[");
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{x:.6}"));
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_query_flag_by_mode() {
        let texts = vec!["a".to_string()];
        assert_eq!(build_body(&texts, EmbedMode::Query)["query"], true);
        assert_eq!(build_body(&texts, EmbedMode::Passage)["query"], false);
        assert_eq!(build_body(&texts, EmbedMode::Query)["texts"][0], "a");
    }

    #[test]
    fn parse_matrix() {
        let v = serde_json::json!({"embeddings": [[1.0, 0.5], [0.25]]});
        let m = parse_embeddings(&v).unwrap();
        assert_eq!(m, vec![vec![1.0f32, 0.5], vec![0.25]]);
        assert!(parse_embeddings(&serde_json::json!({})).is_none());
        assert!(parse_embeddings(&serde_json::json!({"embeddings": [1]})).is_none());
    }

    #[test]
    fn pgvector_literal_format() {
        assert_eq!(to_pgvector(&[1.0, 0.5]), "[1.000000,0.500000]");
    }

    #[test]
    fn base_url_trailing_slash_tolerated() {
        assert_eq!(EmbedClient::new("http://h:8077/").url, "http://h:8077/embed");
    }

    #[tokio::test]
    async fn conn_refused_degrades_and_breaker_shared_across_clone() {
        let c = EmbedClient::new("http://127.0.0.1:1");
        assert!(c.embed_query("q").await.is_none()); // send 失败 → 熔断 300s
        let c2 = c.clone(); // Clone 共享熔断（对齐历史全局 static 语义）
        // 「没发请求」用熔断状态断言，不用墙钟：机器负载高时 <500ms 会假红（并行跑 9 个 crate 时实测踩到）
        assert!(c2.cooldown_until.load(Ordering::Relaxed) > now(), "clone 应共享同一个熔断状态");
        assert!(c2.embed_query("q").await.is_none()); // 冷却期内直接 None
    }

    #[tokio::test]
    async fn empty_texts_short_circuits() {
        let c = EmbedClient::new("http://127.0.0.1:1");
        assert!(c.embed(&[], EmbedMode::Query).await.is_none());
        assert!(c.embed_passages(&[]).await.is_none());
    }

    // ===== 分批：桩服务记每次请求的 texts 条数 =====

    /// 最小 HTTP 桩。**不引新依赖**（tokio 已是 `features = ["full"]`，reqwest 也已在）：
    /// 一个 TcpListener + 手写 26 字节响应头，比拉 hyper/axum 进 dev-deps 便宜。
    /// 返回 `(base_url, 每次请求里的 texts 条数)`。`rows` 决定回几行向量（用来造「条数不符」）。
    async fn stub(
        delay: std::time::Duration,
        rows: fn(usize) -> usize,
    ) -> (String, Arc<std::sync::Mutex<Vec<usize>>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let s0 = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else { return };
                let s = s0.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // 必须按 Content-Length 读满：64 块 × 640 字 ≈ 40KB，一次 read 收不完，
                    // 半个 body 喂给 serde_json 会 panic 在桩里而看起来像「客户端没发全」。
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
                    let v: serde_json::Value = serde_json::from_slice(&buf[head + 4..]).unwrap();
                    let n = v["texts"].as_array().unwrap().len();
                    s.lock().unwrap().push(n);
                    tokio::time::sleep(delay).await;
                    let body =
                        serde_json::json!({"embeddings": vec![vec![0.5f32, 0.25]; rows(n)]})
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

    fn texts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("块 {i}：报销标准与住宿费上限")).collect()
    }

    /// 200 块必须发 **4** 次（64 一批），且回来的向量条数与块数一致。
    #[tokio::test]
    async fn passages_are_batched_by_64() {
        let (base, seen) = stub(std::time::Duration::ZERO, |n| n).await;
        let c = EmbedClient::new(&base);
        let got = c.embed_passages(&texts(200)).await.expect("200 块必须能入库");
        assert_eq!(got.len(), 200, "少返一条就等于那些块永远没有向量");
        assert_eq!(*seen.lock().unwrap(), vec![64, 64, 64, 8], "200 块 → 64/64/64/8 共 4 次");
    }

    /// 反面：小批不许被拆成「每条一发」——那样上面那条也是绿的，但 275 块会发 275 次 HTTP。
    #[tokio::test]
    async fn ten_passages_go_out_in_one_request() {
        let (base, seen) = stub(std::time::Duration::ZERO, |n| n).await;
        let c = EmbedClient::new(&base);
        assert_eq!(c.embed_passages(&texts(10)).await.unwrap().len(), 10);
        assert_eq!(*seen.lock().unwrap(), vec![10], "10 块只该发 1 次");
    }

    /// 一批真花 4s 时不许降级、不许熔断 —— 语料侧的 3s 就是「文档永不进向量路」的成因。
    #[tokio::test]
    async fn a_four_second_batch_neither_degrades_nor_trips_the_breaker() {
        let (base, seen) = stub(std::time::Duration::from_secs(4), |n| n).await;
        let c = EmbedClient::new(&base);
        assert!(c.embed_passages(&texts(10)).await.is_some(), "4s 就返 None = doc 停在 chunked");
        assert_eq!(*seen.lock().unwrap(), vec![10]);
        assert_eq!(c.cooldown_until.load(Ordering::Relaxed), 0, "慢一次不许把全 app 熔断 300s");
        // 问句侧的 3s 预算一个字没动（p50/p95 靠它）
        assert_eq!(timeout_for(1, EmbedMode::Query).as_secs(), TIMEOUT_SECS);
    }

    /// 服务少返一行 → 整批 `None`（宁可后补，不许写半份向量再把 doc 推到 embedded）。
    #[tokio::test]
    async fn short_response_degrades_instead_of_writing_half_the_vectors() {
        let (base, _) = stub(std::time::Duration::ZERO, |n| n - 1).await;
        let c = EmbedClient::new(&base);
        assert!(c.embed_passages(&texts(3)).await.is_none());
        assert_eq!(c.cooldown_until.load(Ordering::Relaxed), 0, "形状不符不熔断（对齐历史）");
    }
}
