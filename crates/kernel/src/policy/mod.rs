//! 权限层的**纯算法**：集合裁决（scope）、档案类型（rules）、AST 注入（inject）。
//! IO 侧（连库算 ScopeSets、加载档案、铸造 UnrestrictedProof）在 dms-policy crate。
//!
//! 「kernel 零 DMS 字符串」与「46 权限单测断言一字不改」同时成立的切法 = 数值 vs 字符串：
//! 这里只有 i32/i64/String 集合运算与 SENTINEL=-1（view_type 的 101/102/103 是数值不是字符串），
//! 而带 DMS 表名列名的断言与 builtin_rules 归 dms-policy。

pub mod inject;
pub mod rules;
pub mod scope;
