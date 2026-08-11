//! bge 向量服务客户端：批量 + query/passage 双模式 + 超时按条数算 + 300s 熔断。
//! 搬运源 `crates/server/src/embed.rs` 全文，语义逐字保留（超时/冷却/请求形状/`to_pgvector` 格式）；
//! 唯一结构改动是**实例化**（ARCHITECTURE §4.2「实例而非全局单例」）——
//! 熔断状态放 `Arc<AtomicU64>` 随 `Clone` 共享，等价于历史的全局 `static COOLDOWN_UNTIL`。
//! 服务挂时静默降级返回 `None`，调用方回落词典召回。
//! 请求形状 `{"texts":[...],"query":bool}` 与 `tools/embed_service.py` 的 `/embed` 一一对应。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::now;

/// 单次请求超时（对齐历史 3s）。**问句侧就这么多**：3s 是用户在等的预算。
/// 与 rerank/external_kb 同档常量已上提 crate 根（`HTTP_CALL_TIMEOUT_SECS`）。
const TIMEOUT_SECS: u64 = crate::HTTP_CALL_TIMEOUT_SECS;
/// 失败后冷却期（对齐历史 300s）：防 embed 服务挂时每问白等一个超时（crate 根共享常量）
const COOLDOWN_SECS: u64 = crate::COOLDOWN_SECS;
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

/// 熔断槽按模式分开：语料侧一次批超时不能把问句侧也熔断 300s
/// （一次后台重建失败影响在线问答 5 分钟是不行的）。`Arc` 随 `Clone` 共享，同历史全局语义。
#[derive(Default)]
struct Breaker {
    query: AtomicU64,
    passage: AtomicU64,
}

impl Breaker {
    fn slot(&self, mode: EmbedMode) -> &AtomicU64 {
        match mode {
            EmbedMode::Query => &self.query,
            EmbedMode::Passage => &self.passage,
        }
    }
}

#[derive(Clone)]
pub struct EmbedClient {
    url: String,
    cooldown_until: Arc<Breaker>,
    /// 实例级复用的 HTTP 客户端（`Clone` 共享同一连接池）。曾经每次调用新建一个
    /// Client＝每次都重付 TCP 握手、丢 keep-alive：问句侧一问 2~3 次调用，全是白付。
    /// 超时仍在**每次请求**上单独设（`RequestBuilder::timeout` 覆盖语义与
    /// `Client::builder().timeout` 逐字相同），不同条数/模式的超时预算不变。
    http: reqwest::Client,
    /// 问句向量的单槽 memo（问句原文 → 向量）：一轮问答里同一问句至少被取三次
    /// （首轮 `gather` 的表召回、回炉 `gather_all_cards` 的 schema 召回与经验召回），
    /// 而向量对（embed 服务, 文本）是确定的 —— 重复 HTTP 是纯浪费（单次几十~几百 ms）。
    /// 单槽而不是 Map：一次问答只有一个问句，跨请求命中同文本同样安全；
    /// **只缓存成功**（None 不缓存）：服务恢复后必须能重试，冷却/熔断语义全在 `embed`，
    /// 本字段不加第二道。`Clone` 共享（与熔断状态同一处理）。
    query_memo: Arc<std::sync::Mutex<Option<(String, Vec<f32>)>>>,
}


impl EmbedClient {
    /// `base_url` 例 `http://127.0.0.1:8077`（文档服务同进程同端口，见 `doc.rs`）
    pub fn new(base_url: &str) -> Self {
        Self {
            url: format!("{}/embed", base_url.trim_end_matches('/')),
            cooldown_until: Arc::new(Breaker::default()),
            // build() 只在 TLS 后端缺失这类部署事故时失败：启动即崩是刻意取舍
            // （静默降级需要一个能用的客户端，造不出来就没有降级对象）
            http: reqwest::Client::builder().build().expect("embed http client"),
            query_memo: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 批量取向量。空输入 / 服务不可用 / 熔断中一律 `None`。
    pub async fn embed(&self, texts: &[String], mode: EmbedMode) -> Option<Vec<Vec<f32>>> {
        self.embed_within(texts, mode, timeout_for(texts.len(), mode)).await
    }

    async fn embed_within(
        &self,
        texts: &[String],
        mode: EmbedMode,
        budget: std::time::Duration,
    ) -> Option<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return None;
        }
        let slot = self.cooldown_until.slot(mode);
        if now() < slot.load(Ordering::Relaxed) {
            return None;
        }
        // 客户端在 `new` 时建好复用（连接池/keep-alive 跨调用生效）；超时逐请求设，
        // 与改动前「每次新建带 timeout 的 Client」同语义（reqwest 的请求级 timeout 即全程超时）。
        let req = self.http.post(&self.url).json(&build_body(texts, mode)).timeout(budget);
        match req.send().await {
            Ok(resp) => {
                // 5xx 计入熔断（服务持续 500 时每问白付一次 HTTP）；4xx 不熔断（对齐历史）
                let resp = match resp.error_for_status() {
                    Ok(r) => r,
                    Err(e) => {
                        if e.status().is_some_and(|s| s.is_server_error()) {
                            tracing::warn!(err = %e, "embed 服务 5xx，熔断 {COOLDOWN_SECS}s");
                            slot.store(now() + COOLDOWN_SECS, Ordering::Relaxed);
                        }
                        return None;
                    }
                };
                let v: serde_json::Value = match resp.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!(err = %e, "embed 响应解析失败，降级");
                        return None;
                    }
                };
                let m = parse_embeddings(&v)?;
                // 条数不符即整批降级（不触发熔断，同形状不符）：少返几行会让 `ingest.rs` 的
                // `ids.zip(vecs)` 静默只写前 k 个块的向量，而 doc 照样推到 embedded ——
                // 「界面显示已入库、其实检索不到」正是本仓反复抓的那一族。
                (m.len() == texts.len()).then_some(m)
            }
            Err(e) => {
                // 熔断 300s（仅 send 失败/5xx 触发；解析失败不触发——对齐历史）
                tracing::warn!(err = %e, "embed 服务不可达，熔断 {COOLDOWN_SECS}s");
                slot.store(now() + COOLDOWN_SECS, Ordering::Relaxed);
                None
            }
        }
    }

    /// 单条查询向量（维度由服务端模型定，本层不写死）。历史 `embed_query` 语义：Query 模式取首行。
    pub async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        // 先撞 memo（锁只护一读后写，不跨 await；锁中毒恢复取值即可，memo 无部分态）：
        // 同一问句在一轮问答里被取多次，见 `query_memo` 字段注释。未命中才走 HTTP，命中结果回填。
        // 两个并发 miss 会重复发一次 HTTP（last-writer-wins，无害），刻意不加 in-flight 锁。
        if let Some((t, v)) = self.query_memo.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if t == text {
                return Some(v.clone());
            }
        }
        let v = self.embed(&[text.to_string()], EmbedMode::Query).await?.into_iter().next()?;
        *self.query_memo.lock().unwrap_or_else(|e| e.into_inner()) = Some((text.to_string(), v.clone()));
        Some(v)
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
        self.embed_batched(texts, EmbedMode::Passage).await
    }

    /// 问句侧批量（Query 模式）：向量自愈补**语料问句**用（`embed_service.py` 的
    /// `embed(texts, is_query=True)` 同款 —— 同一列不许混两种模式的向量）。
    /// 与 `embed_passages` 同分批、同「任一批失败整篇 None」；超时同样按条数预算 ——
    /// 调用方是后台批任务（server/src/embed_fill.rs，一批 64 条），3s 恒额必超时熔断，
    /// 正是上面 4s 批那族问题在 query 侧的复刻。用户侧单条 `embed_query` 仍保持 3s。
    pub async fn embed_queries(&self, texts: &[String]) -> Option<Vec<Vec<f32>>> {
        self.embed_batched(texts, EmbedMode::Query).await
    }

    /// 批处理公共形态：按 `BATCH` 分批、任一批失败整篇 `None`、批量预算按条数。
    async fn embed_batched(&self, texts: &[String], mode: EmbedMode) -> Option<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            out.extend(self.embed_within(batch, mode, batch_timeout(batch.len())).await?);
        }
        Some(out)
    }
}

/// 超时预算。问句侧单条恒 3s（用户在等）；批量走 `batch_timeout` 按条数算 —— 一批 64 块的活
/// 不可能在 3s 里干完，而「按条数算」不是「3s × N」：N 是**一批**的条数，整篇的总预算随批次
/// 线性增长，任何一批卡住都在 22s 内落地，不会攒成一个几百秒的单请求。
fn timeout_for(n: usize, mode: EmbedMode) -> std::time::Duration {
    match mode {
        EmbedMode::Query => std::time::Duration::from_secs(TIMEOUT_SECS),
        EmbedMode::Passage => batch_timeout(n),
    }
}

/// 批量（语料/语料问句）预算：基础 3s + 每条 300ms。
fn batch_timeout(n: usize) -> std::time::Duration {
    std::time::Duration::from_secs(TIMEOUT_SECS)
        + std::time::Duration::from_millis(PASSAGE_MS_PER_TEXT * n as u64)
}

/// 请求体。`#[derive(Serialize)]` 零拷贝（wire 形状 `{"texts":[...],"query":bool}` 逐字不变，
/// 有测试守）；`json!` 宏会把 64×640 字全量 clone 成 `Value` 再序列化，两道分配。
#[derive(serde::Serialize)]
struct Body<'a> {
    texts: &'a [String],
    query: bool,
}

fn build_body(texts: &[String], mode: EmbedMode) -> Body<'_> {
    Body { texts, query: mode == EmbedMode::Query }
}

/// 响应 `{"embeddings": [[...], ...]}` → 矩阵；形状不符返回 `None`（不触发熔断，对齐历史）。
/// 行维度不齐 / 空行 / 含非数值或超范围（→inf）元素都算形状不符：短维度或 inf 向量
/// 写进 pgvector 只会换来一个指向 SQL 层的谜之错误。
fn parse_embeddings(v: &serde_json::Value) -> Option<Vec<Vec<f32>>> {
    let arr = v["embeddings"].as_array()?;
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(arr.len());
    let mut dim: Option<usize> = None;
    for row in arr {
        let r = row.as_array()?;
        if r.is_empty() || dim.is_some_and(|d| d != r.len()) {
            return None;
        }
        dim.get_or_insert(r.len());
        let mut parsed = Vec::with_capacity(r.len());
        for x in r {
            let f = x.as_f64()? as f32;
            if !f.is_finite() {
                return None;
            }
            parsed.push(f);
        }
        out.push(parsed);
    }
    Some(out)
}

/// f32 向量 → pgvector 字面量 `'[...]'`（原样迁自 `server/embed.rs`，6 位小数不许变——
/// `tools/embed_service.py` 回写的字面量是同一格式）。
/// 调用方保证元素有限（`parse_embeddings` 已过滤 NaN/inf；直接喂 NaN 会产出 `NaN` 字面量，
/// pgvector 拒收且错误指向 SQL 层）。
pub fn to_pgvector(v: &[f32]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(v.len() * 9 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        write!(&mut s, "{x:.6}").expect("写 String 不会失败");
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
        // wire 形状钉死：{"texts":[...],"query":bool}，与 embed_service.py 一一对应
        let q = serde_json::to_value(build_body(&texts, EmbedMode::Query)).unwrap();
        assert_eq!(q["query"], true);
        assert_eq!(q["texts"][0], "a");
        let p = serde_json::to_value(build_body(&texts, EmbedMode::Passage)).unwrap();
        assert_eq!(p["query"], false);
    }

    #[test]
    fn parse_matrix() {
        let v = serde_json::json!({"embeddings": [[1.0, 0.5], [0.25, 0.75]]});
        let m = parse_embeddings(&v).unwrap();
        assert_eq!(m, vec![vec![1.0f32, 0.5], vec![0.25, 0.75]]);
        assert!(parse_embeddings(&serde_json::json!({})).is_none());
        assert!(parse_embeddings(&serde_json::json!({"embeddings": [1]})).is_none());
        // 行维度不齐 / 空行 / 非数值元素 / 超范围值：一律整批 None（形状不符）
        assert!(parse_embeddings(&serde_json::json!({"embeddings": [[1.0, 0.5], [0.25]]})).is_none());
        assert!(parse_embeddings(&serde_json::json!({"embeddings": [[]]})).is_none());
        assert!(parse_embeddings(&serde_json::json!({"embeddings": [[1.0, "x"]]})).is_none());
        assert!(parse_embeddings(&serde_json::json!({"embeddings": [[1e300]]})).is_none());
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
        assert!(c2.cooldown_until.query.load(Ordering::Relaxed) > now(), "clone 应共享同一个熔断状态");
        assert!(c2.embed_query("q").await.is_none()); // 冷却期内直接 None
    }

    #[tokio::test]
    async fn empty_texts_short_circuits() {
        let c = EmbedClient::new("http://127.0.0.1:1");
        assert!(c.embed(&[], EmbedMode::Query).await.is_none());
        assert!(c.embed_passages(&[]).await.is_none());
    }

    /// 问句向量 memo：同文本第二次不再发 HTTP（一轮问答里同一问句被取多次），
    /// 换文本照常发；**失败不缓存**（服务恢复后必须能重试，冷却语义在 `embed`）。
    #[tokio::test]
    async fn query_embedding_is_memoized_per_text_and_failures_are_not() {
        let (base, seen) = stub(std::time::Duration::ZERO, |n| n).await;
        let c = EmbedClient::new(&base);
        let a = c.embed_query("同一个问题").await.expect("桩在线必须取向量");
        let b = c.embed_query("同一个问题").await.expect("memo 命中");
        assert_eq!(a, b, "memo 必须返回同一份向量");
        assert_eq!(seen.lock().unwrap().len(), 1, "第二次必须命中 memo，不许再发 HTTP");
        // clone 共享 memo（与熔断状态同一语义）：另一个句柄也命中
        let c2 = c.clone();
        let _ = c2.embed_query("同一个问题").await;
        assert_eq!(seen.lock().unwrap().len(), 1, "clone 必须共享同一份 memo");
        // 单槽只记**最近**一个文本：换文本照常发（并把槽顶掉 —— 一轮问答只有一个问句）
        let _ = c.embed_query("另一个问题").await;
        assert_eq!(seen.lock().unwrap().len(), 2, "换文本必须照常发请求");
        // 失败侧：服务不可达 → None，且**不进** memo（下一轮重试权不许被缓存毒掉）
        let down = EmbedClient::new("http://127.0.0.1:1");
        assert!(down.embed_query("q").await.is_none());
        assert!(down.query_memo.lock().unwrap().is_none(), "None 不许进 memo");
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
                    use tokio::io::AsyncWriteExt;
                    let Some((_, raw)) = crate::test_stub::read_request(&mut sock).await else {
                        return;
                    };
                    let v: serde_json::Value = serde_json::from_slice(&raw)
                        .expect("stub 收到坏 JSON（客户端没发全或没发对）");
                    let n = v["texts"].as_array().expect("stub 收到非 texts 请求").len();
                    s.lock().unwrap().push(n);
                    tokio::time::sleep(delay).await;
                    let body =
                        serde_json::json!({"embeddings": vec![vec![0.5f32, 0.25]; rows(n)]})
                            .to_string();
                    let _ = sock.write_all(&crate::test_stub::json_response(&body)).await;
                });
            }
        });
        (base, seen)
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
        assert_eq!(c.cooldown_until.passage.load(Ordering::Relaxed), 0, "慢一次不许把全 app 熔断 300s");
        // 问句侧的 3s 预算一个字没动（p50/p95 靠它）
        assert_eq!(timeout_for(1, EmbedMode::Query).as_secs(), TIMEOUT_SECS);
    }

    /// 服务少返一行 → 整批 `None`（宁可后补，不许写半份向量再把 doc 推到 embedded）。
    #[tokio::test]
    async fn short_response_degrades_instead_of_writing_half_the_vectors() {
        let (base, _) = stub(std::time::Duration::ZERO, |n| n - 1).await;
        let c = EmbedClient::new(&base);
        assert!(c.embed_passages(&texts(3)).await.is_none());
        assert_eq!(c.cooldown_until.passage.load(Ordering::Relaxed), 0, "形状不符不熔断（对齐历史）");
    }

    /// 恒 500 的桩：5xx 必须计入熔断，且只熔断本模式槽位
    #[tokio::test]
    async fn server_error_trips_only_the_query_breaker() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else { return };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .await;
                });
            }
        });
        let c = EmbedClient::new(&base);
        assert!(c.embed_query("q").await.is_none());
        assert!(c.cooldown_until.query.load(Ordering::Relaxed) > now(), "5xx 必须计入熔断");
        assert_eq!(c.cooldown_until.passage.load(Ordering::Relaxed), 0, "熔断按模式分槽");
    }
}
