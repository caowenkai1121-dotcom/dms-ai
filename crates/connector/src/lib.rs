//! # dms-connector —— 全部对外 IO 的唯一出口
//!
//! 八类外部资源包成类型受控的客户端：只读取数源 / 自有可写 PG / LLM / embed / rerank /
//! 文档服务 / AGE 图 / 外部只读 KB（Dify 数据集检索，Yuxi B9）。
//! 其余 crate 一行 sqlx / reqwest 都不写。
//!
//! ## 两条红线（结构性，不是纪律性）
//! 1. **全仓唯一能造连接池**，且不导出裸池。业务 SQL 只能经 `SqlSource::fetch(&ScopedSql)`——
//!    `ScopedSql` 的产出点只有 `kernel::inject()` 与 `ScopedSql::unrestricted(_, &UnrestrictedProof)`。
//! 2. **`OwnedStore` 永不接受 LLM 产物**：自有 PG 的写入只走 `fixed(&'static str) + bind` 与
//!    `create_upload_table(&UploadTableSpec)`（标识符经 `SafeIdent` 白名单，DDL 由代码渲染）。
//!
//! 框架自查走字面量通道 `fixed()`；动态 `IN` 只有一条路 `FixedStmt::expand(n)`。
//! 敏感列在 `fetch` 组装 `RowSet` 时整列置空——这是 `SELECT *` 的唯一收口。
//!
//! 预算：≤15 个 `.rs`（B9 外部只读 KB 接入 +1，`docs/ARCHITECTURE.md` §4.2 清单待同步）。
//! 落点清单见 `docs/ARCHITECTURE.md` §4.2。
//! 本阶段（T4）落齐四个池与它们的语句通道；`llm` 仍属 T10。
//! `graph` 已于 T9-A1 逐行搬入（AGE/Cypher 是 IO，留 server 会让 agent 反向依赖 server）。

pub mod ddl;
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
