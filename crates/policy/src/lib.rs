//! # dms-policy —— 行级数据权限的 IO 侧
//!
//! 全仓唯一「改错 = 越权」的模块：加载 principal、算 `ScopeSets`（1:1 复刻 Java `DefaultEmployee`）、
//! 持有并热更新表权限档案 `RuleSet`、铸造 `UnrestrictedProof`。
//!
//! ## 纪律
//! - **语义 1:1 不动**。任何「顺手优化」都要在提交信息里点名，并给出对拍方式
//!   （`dms-ai-server.exe scope <login> [role]` 的 stdout JSON 字节级 diff + `judge_scope.py` 6/6）。
//! - **一切失败 fail-closed**：无角色拒、未登记表拒、权限计算失败拒、注入条件不可完整解析拒。
//! - 权限单测套件（scope / inject / fail_closed 三组）物理落 `tests/`，**断言一字不改**。
//! - 纯算法（`decide_base` / `merge_*` / `expand_department_tree`）在 kernel，
//!   带 DMS 表名列名的断言与语料在这里——这是「kernel 零 DMS 字符串」与「断言零改」同时成立的切法。
//! - SQL 全走 connector 的 `fixed(&'static str)` 字面量通道，本 crate 一行 `sqlx::query` 都没有。
//!
//! 预算：≤8 个 src `.rs` + 4 个 tests（**当前正好顶格 8+4**：新增文件前先考虑合并进既有文件）。
//! 落点清单见 `docs/ARCHITECTURE.md` §4.3。

pub mod builtin;
pub mod cache;
pub mod dms_tables;
pub mod principal;
pub mod proof;
pub mod rules;
pub mod scope;

// 路径一次性钉死（同 kernel/connector 的做法）：同一个符号只有一条 use 路径。
pub use builtin::builtin_rules;
pub use cache::{invalidate, invalidate_all};
pub use principal::{list_roles, load_principal, Principal};
pub use proof::for_principal;
pub use rules::{install, load_rules, seed_rules, snapshot};
pub use scope::{compute_scope, compute_scope_cached, Scope, ScopeSets};

// 字符串级注入的两个纯函数由 kernel 提供，从这里单点导出（fail-closed 测试与 T9 都只认这条路径）。
pub use dms_kernel::policy::inject::{build_condition, parse_full_expr};

/// 把权限条件注入 SQL（两参门面：档案走本 crate 的注册表，算法在 kernel）。
///
/// 15 个注入断言原样调它。**不走 newtype 闸门是刻意的**：`check()` 会补 `LIMIT 200`，
/// 走 `CheckedSql` 会让 `assert_eq!(out == in)` 假红（ARCHITECTURE §5「字符串级注入」）。
/// 生产路径请走 `dms_kernel::inject(CheckedSql, sets, &rules::snapshot())`。
///
/// 集合全空（超管/ALL）原样返回；受限用户 SQL 涉及未登记表 → Err（fail-closed 拒绝）。
pub fn inject(sql: &str, sets: &ScopeSets) -> anyhow::Result<String> {
    // `anyhow::Error::from` 保留错误源链（`anyhow!("{e}")` 会把 kernel 错误字符串化丢链；
    // 两者 Display 相同，喂给 repair 的文案逐字不变）
    dms_kernel::policy::inject::rewrite(sql, sets, &rules::snapshot(), &dms_kernel::MysqlDialect)
        .map_err(anyhow::Error::from)
}
