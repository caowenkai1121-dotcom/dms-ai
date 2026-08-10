//! # dms-kernel —— 纯契约 + 纯算法底座
//!
//! 依赖树的根：不依赖任何 workspace crate，也不依赖 sqlx / reqwest / axum / tokio / chrono。
//!
//! ## 收纳判据（四条全中才进 kernel，任一不中就留 policy/semantic/agent）
//! 1. **纯**：纯函数或纯状态转移；不 await、不碰 IO、不读时钟、无全局可变态（时间/随机一律参数注入）。
//! 2. **零 DMS 语料**：表名/列名/码值魔数/中文业务名词一律参数化。只允许 SQL 方言字符串与通用中文虚词。
//! 3. **≥2 个上游消费者**，或因编译方向只能落这里的跨 crate 契约类型。
//! 4. **无库无网可单测**（`cargo test -p dms-kernel`）。
//!
//! 反例（明确不进 kernel）：`builtin_rules` 的 32 张 DMS 表、指标/维度中文词表、prompt 文案、
//! `extract_tables`（按 `t_` 前缀的 DMS 命名约定）、`Corrector`/`Answerer` trait。
//!
//! 预算：≤21 个 `.rs`、单文件 ≤450 行。落点清单见 `docs/ARCHITECTURE.md` §4.1。
//! `sql/gate.rs`（三段 newtype）属 T3，`llm.rs`/`answer.rs`/`run.rs` 属 T4/T9，本阶段不建。

pub mod answer;
pub mod ds;
pub mod errors;
pub mod llm;
pub mod nl;
pub mod policy;
pub mod present;
pub mod qalog;
pub mod sql;

// 路径一次性钉死：同一个类型只有一条 use 路径，杜绝 dms_kernel::X 与 dms_kernel::a::b::X 并存。
pub use answer::{Answer, AnswerBody, Citation};
pub use ds::DsId;
pub use errors::{GuardError, PolicyError};
pub use llm::{BoxFut, ChatModel, ChatReply, ChatRequest, LlmError, Message, ModelTier};
pub use sql::caliber::{check_caliber, keeps_output_shape, output_shape, CaliberRule, Violation};
pub use sql::gate::{check, inject, CheckedSql, RawSql, ScopedSql, UnrestrictedProof};
pub use policy::rules::{Binding, CustomerKind, OwnerKind, RuleSet, TableRule};
pub use policy::scope::{ScopeSets, SENTINEL};
pub use sql::dialect::{by_name, Dialect, MysqlDialect, PostgresDialect};
