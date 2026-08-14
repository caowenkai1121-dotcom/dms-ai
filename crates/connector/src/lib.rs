//! # dms-connector —— 全部对外 IO 的唯一出口
//!
//! 九类外部资源包成类型受控的客户端：只读取数源 / 自有可写 PG / LLM / embed / rerank /
//! 文档服务 / AGE 图 / 外部只读 KB（Dify 数据集检索，Yuxi B9）/ URL 安全抓取。
//! 其余 crate 一行 sqlx / reqwest 都不写。
//!
//! ## 两条红线（结构性，不是纪律性）
//! 1. **全仓唯一能造连接池**，且不导出裸池。业务 SQL 只能经 `SqlSource::fetch(&ScopedSql)`——
//!    `ScopedSql` 的产出点只有 `kernel::inject()` 与 `ScopedSql::unrestricted(_, &UnrestrictedProof)`。
//! 2. **`OwnedStore` 永不接受 LLM 产物**：自有 PG 的写入只走 `fixed(&'static str) + bind`
//!    （事务/会话锁同样只接字面量）与 `create_upload_table(&UploadTableSpec)`
//!    （标识符经 `SafeIdent` 白名单，DDL 由代码渲染）。
//!
//! 框架自查走字面量通道 `fixed()`；动态 `IN` 只有一条路 `FixedStmt::expand(n)`。
//! 敏感列在 `fetch` 组装 `RowSet` 时整列置空——这是 `SELECT *` 的唯一收口。
//!
//! 预算：≤16 个 `.rs`（URL 安全抓取边界 +1，`docs/ARCHITECTURE.md` §4.2 清单待同步）。
//! 落点清单见 `docs/ARCHITECTURE.md` §4.2。
//! 本阶段（T4）落齐四个池与它们的语句通道；`llm` 仍属 T10。
//! `graph` 已于 T9-A1 逐行搬入（AGE/Cypher 是 IO，留 server 会让 agent 反向依赖 server）。

pub mod ddl;
pub mod dms_lookup;
pub mod doc;
pub mod doc_graph;
pub mod embed;
pub mod error;
pub mod external_kb;
pub mod fixed;
pub mod graph;
pub mod mysql;
pub mod owned;
pub mod postgres;
pub mod registry;
pub mod rerank;
pub mod source;
pub mod url_fetch;

/// UNIX 秒（熔断/冷却计时共用）：时钟异常返 0 → `now() < cooldown_until` 恒真 = 永久冷却，
/// fail-closed 是刻意取舍（服务在烧时不该继续打）。
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 面向用户的外呼统一超时（3s：用户在等）。embed/rerank/external_kb 共用一档。
pub(crate) const HTTP_CALL_TIMEOUT_SECS: u64 = 3;
/// 外呼服务熔断冷却期（300s）：服务挂时每问白等一个超时才是事故。各客户端共用一档。
pub(crate) const COOLDOWN_SECS: u64 = 300;

/// embed / rerank / external_kb 测试共用的最小 HTTP 桩件（不引新依赖）。
#[cfg(test)]
pub(crate) mod test_stub {
    pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    pub fn content_len(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .to_lowercase()
            .split("content-length:")
            .nth(1)
            .and_then(|t| t.split("\r\n").next())
            .and_then(|t| t.trim().parse().ok())
            .expect("stub 需要 Content-Length，缺头会提前 break 喂空 body")
    }

    /// 按 Content-Length 读满一个请求（半个 body 喂 serde_json 会 panic 在桩里，
    /// 看起来像「客户端没发全」）。返回 (head 小写文本, body)；对端断开 → None。
    pub async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let head = loop {
            if let Some(h) = find(&buf, b"\r\n\r\n") {
                if buf.len() >= h + 4 + content_len(&buf[..h]) {
                    break h;
                }
            }
            let mut b = [0u8; 8192];
            match sock.read(&mut b).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => buf.extend_from_slice(&b[..n]),
            }
        };
        Some((String::from_utf8_lossy(&buf[..head]).to_lowercase(), buf[head + 4..].to_vec()))
    }

    /// 200 JSON 响应（Connection: close）
    pub fn json_response(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }
}

// 路径一次性钉死（同 kernel 的做法）：同一个类型只有一条 use 路径。
// ARCHITECTURE §5 与后续计划写的都是 `dms_connector::OwnedStore`，照文档写不能撞 E0433。
pub use error::ConnectorError;
pub use external_kb::{ExtKbClient, ExtKbRecord};
pub use fixed::{FixedStmt, PgStmt};
pub use graph::GraphRow;
pub use mysql::ReadOnlyMySql;
pub use owned::OwnedStore;
pub use postgres::PostgresSource;
pub use registry::{DsSpec, SourceRegistry};
pub use source::{DsPolicy, RowSet, SchemaSnapshot, SourceKind, SqlSource};
