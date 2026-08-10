//! embed 客户端薄包装：实现整块搬到 `dms_connector::embed`（实例式 `EmbedClient`）。
//! 这里只留进程内单例 + 两个原签名 free fn，让 `meta.rs`/`pipeline.rs` 的调用点一行不改。
//! （connector 侧禁全局单例；server 是装配层，全局在这一层是允许的。）

use dms_connector::embed::EmbedClient;
use std::sync::OnceLock;

/// ponytail: base_url 先写死。配置化（`config.rs` 的单一 `service_url`，embed 与文档服务同端口）由 B3 接管。
const BASE_URL: &str = "http://127.0.0.1:8077";

fn client() -> &'static EmbedClient {
    static C: OnceLock<EmbedClient> = OnceLock::new();
    C.get_or_init(|| EmbedClient::new(BASE_URL))
}

/// 查询向量（512维）。服务不可用/熔断中返回 None，调用方降级到词典召回。
pub async fn embed_query(text: &str) -> Option<Vec<f32>> {
    client().embed_query(text).await
}

/// f32 向量 → pgvector 字面量 '[...]'
pub use dms_connector::embed::to_pgvector;
