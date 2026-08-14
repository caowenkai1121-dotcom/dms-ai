//! # dms-semantic —— DMS 业务知识的全部落点
//!
//! 注册表（读写/播种/`ds_id` 作用域）→ 召回（六种命中 + 卡片）→ 装配（组合器/快路径）
//! → 校正（确定性四件链）→ 列标注与呈现算法。变更最频繁的 crate。
//!
//! ## 纪律
//! - **不依赖 dms-policy**：改口径碰不到权限内核，反之改权限也碰不到口径。
//! - **`meta.*` 的唯一读写口**：agent 不许自己写 `meta.*` 的 SQL（否则 `ds_id` 与 `visibility`
//!   两道总闸的漂移测试扫不到它）。
//! - **口径单一事实源**：有效订单状态码、时间列、去重键、省码表一律来自注册表或本 crate 的常量，
//!   不许在第二处内联。`tests/drift.rs` 把这条纪律变成会红的测试。
//! - 拆函数只许提取不许重排（`STRIP_WORDS` 的「上月」先于「上个月」、customer 段序等，顺序即行为）。
//!
//! 预算：≤38 个 `.rs`、单文件 ≤450 行。落点清单见 `docs/ARCHITECTURE.md` §4.4。

pub mod compose;
pub mod correct;
pub mod direct_types;
pub mod fastpath;
pub mod datamap_usage;
pub mod datamap;
pub mod lineage;
pub mod ddl;
pub mod document;
pub mod ingest;
pub mod ops_caliber;
pub mod present;
pub mod present_cn;
pub mod recall;
pub mod registry;
pub mod sales_fact;
pub mod seed;
pub mod seed_defs;
pub mod warehouse_catalog;

// T8-B5：确定性路径与 agent 的共享类型（ARCHITECTURE §4.4 的 lib.rs 行）
pub use direct_types::{
    DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation, ResolvedSlot,
};
