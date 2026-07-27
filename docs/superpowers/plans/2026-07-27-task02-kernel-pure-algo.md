# Task 2：纯算法下沉 dms-kernel + 词表合并（低风险）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 server 里「纯算法、零 IO」的代码物理下沉到 `dms-kernel`，server 侧全部改为 re-export / 薄门面，**所有调用点与全部现有单测一行不改**。同步完成五份中文词表向 `kernel::nl::lexicon` 的收敛（并集 + 开关 + 回归验证 + 收开关）。

**Architecture:** kernel 新增 `sql::{lex, guard, ast}`、`policy::{scope, inject}`、`nl::{time, lexicon}`、`present` 四组模块；server 六个文件（pipeline/direct/corrector/inject/scope/viewspec）删除被搬函数本体，改 `pub use dms_kernel::...` 或同名薄包装。IO 代码（compute_scope 七个连库查询、rule_of/builtin_rules/load_rules/seed_rules、schema_check/correct_* 的 PG 加载、has_residue 等组合器守卫、time_window）**全部留 server**。

**Tech Stack:** Rust workspace、cargo、sqlparser 0.53、serde/serde_json。

**前置依赖：** Task 1（6-crate 骨架）必须已落地——本 plan 所有 `dms-kernel` 引用都依赖它。**注意：截至本 plan 撰写时 Task 1 尚未执行**（根 `Cargo.toml` 仍只有 `members = ["crates/server"]`），若执行时仍未落地，先按 `2026-07-27-task01-workspace-skeleton.md` 完成 Task 1。

## Global Constraints

- **零行为变化**：搬移的函数体逐行复制，纯算法逻辑一字不改；只允许三类签名微调：① `anyhow::Result` → kernel 手写错误 enum；② DMS 词表/码表/常量参数化（敏感列、MAX_ROWS、PresentLexicon）；③ 私有改 `pub`。
- **错误文案原样保留**：`GuardError`/`PolicyError` 的 `Display` 文案与现 anyhow 消息逐字一致（repair 流程把 `e.to_string()` 喂 LLM；inject 测试断言 `contains("未在权限档案登记")`）。
- **现有单测一行不改（硬验收）**：server 侧 157 个单测的函数体与断言**一个字符都不动**——靠 re-export 让它们继续编译并锁定 kernel 实现。只允许**新增**门面一致性测试。（spec 3.2 已把 31 scope + 15 inject 单测的终态划给 policy crate，故 Task 2 不做物理搬移，避免 Task 5 二次搬运。）
- **kernel 硬规则**：零 IO、零 sqlx/reqwest/axum、非测试代码零 DMS 字符串。`builtin_rules`（27 张 DMS 表绑定）、`login_pwd` 敏感列、`order_time` 默认时间列、DIM_POOL、34 省码表、中文业务词表**一律不进 kernel**，由 server 以参数/词表结构注入。
- **依赖红线**：kernel 只用 Task 1 已配的 serde/serde_json/sqlparser(visitor)/chrono。已核对够用（chrono 本任务暂未被 kernel 代码引用，属多配不属缺配，不动 Task 1 配置）。**不得新增任何第三方 crate**；错误类型手写 enum + Display + Error，不引 thiserror。
- **TDD 统一节奏**（每个子任务都走）：① 先在 server 写「门面一致性测试」（调尚不存在的 `dms_kernel::...`，编译失败 = 红）→ ② kernel 实现（逐行搬）→ ③ server 改门面/re-export → ④ 测试转绿 → ⑤ `cargo test` 全量（157+N 全绿）→ ⑥ 独立 commit。
- Windows 构建须前缀 MinGW bin 路径（见文末备注），cargo 命令走 PowerShell 不走 Bash。

---

### Task 2.1: kernel `sql::lex`——七个 SQL 词法工具

**Files:**
- Create: `crates/kernel/src/lib.rs`
- Create: `crates/kernel/src/sql/mod.rs`
- Create: `crates/kernel/src/sql/lex.rs`（约 330 行含测试）
- Modify: `crates/server/src/pipeline.rs`、`crates/server/src/direct.rs`、`crates/server/src/corrector.rs`

**Interfaces:**
- Consumes: 无（首个 kernel 模块）
- Produces: `dms_kernel::sql::lex::{strip_literals_and_comments, ensure_limit_with, split_top_and, first_ident_of, from_table_aliases, base_col_refs, qualify_cols}`

搬移映射（函数体逐行复制，出处行号为现状）：

| kernel 目标 | server 出处 | 签名变化 |
|---|---|---|
| `strip_literals_and_comments(sql: &str) -> String` | pipeline.rs:119-169 | 无 |
| `ensure_limit_with(sql: &str, max_rows: usize) -> String` | pipeline.rs:171-185 | 写死的 `MAX_ROWS` 改参数 |
| `split_top_and(filter: &str) -> Vec<String>` | corrector.rs:385-418 | 无 |
| `first_ident_of(cond: &str) -> Option<String>` | corrector.rs:421-429 | 无 |
| `from_table_aliases(from: &str) -> Vec<(String, String)>` | direct.rs:362-396 | 无 |
| `base_col_refs(frag: &str, alias: &str) -> Vec<String>` | direct.rs:399-424 | 无 |
| `qualify_cols(expr: &str, alias: &str) -> String` | direct.rs:467-523 | 无（KEYWORDS 是通用 SQL 词，随函数进 kernel） |

`crates/kernel/src/lib.rs`:
```rust
//! dms-kernel：纯契约 + 纯算法底座（零 IO，禁 sqlx/reqwest/axum，零 DMS 字符串）。

pub mod sql;
pub mod policy;
pub mod nl;
pub mod present;
```

`crates/kernel/src/sql/mod.rs`:
```rust
//! SQL 词法/AST 工具与只读护栏（三段 newtype 在 Task 3 加入本模块）。

pub mod lex;
pub mod guard;
pub mod ast;
```

`crates/kernel/src/sql/lex.rs` 骨架（函数体从上表出处逐行复制）：
```rust
//! SQL 纯文本词法工具（零 IO、零 DMS 字符串）。函数体自 server 逐行搬移，逻辑一字不改。

/// 词法剥离：去字符串字面量与注释。复制自 server/pipeline.rs:119-169。
pub fn strip_literals_and_comments(sql: &str) -> String { /* 逐行复制 */ }

/// LIMIT 护栏：max_rows 参数化（server 传 200）。复制自 pipeline.rs:171-185。
pub fn ensure_limit_with(sql: &str, max_rows: usize) -> String { /* 逐行复制，MAX_ROWS→max_rows */ }

pub fn split_top_and(filter: &str) -> Vec<String> { /* 复制自 corrector.rs:385-418 */ }
pub fn first_ident_of(cond: &str) -> Option<String> { /* 复制自 corrector.rs:421-429 */ }
pub fn from_table_aliases(from: &str) -> Vec<(String, String)> { /* 复制自 direct.rs:362-396 */ }
pub fn base_col_refs(frag: &str, alias: &str) -> Vec<String> { /* 复制自 direct.rs:399-424 */ }
pub fn qualify_cols(expr: &str, alias: &str) -> String { /* 复制自 direct.rs:467-523，含 KEYWORDS */ }

#[cfg(test)]
mod tests {
    use super::*;
    // kernel 自守测试（泛化名，新写）：strip 三种注释/引号转义、ensure_limit_with 追加与不重复追加、
    // split_top_and 括号不切、qualify_cols 裸列限定/引号跳过/已限定跳过、from_table_aliases 子查询跳过、
    // base_col_refs 前缀误命中防护。server 侧原有针对这些函数的单测一行不动，继续经门面锁定行为。
}
```

- [ ] **Step 1: 先写门面一致性测试（红）**

在 `crates/server/src/pipeline.rs` 的 `mod tests` 末尾**新增**（不动既有测试）：
```rust
#[test]
fn facade_ensure_limit_delegates_to_kernel() {
    // 证明 server 门面调的就是 kernel 实现（现阶段编译失败 = 红）
    assert_eq!(
        ensure_limit("SELECT a FROM t"),
        dms_kernel::sql::lex::ensure_limit_with("SELECT a FROM t", MAX_ROWS)
    );
}
```

- [ ] **Step 2: 建 kernel 三个文件，逐行搬入七个函数**

- [ ] **Step 3: server 改门面/改用**

`pipeline.rs`：删 `strip_literals_and_comments`/`ensure_limit` 本体，改同名薄包装：
```rust
fn strip_literals_and_comments(sql: &str) -> String {
    dms_kernel::sql::lex::strip_literals_and_comments(sql)
}
fn ensure_limit(sql: &str) -> String {
    dms_kernel::sql::lex::ensure_limit_with(sql, MAX_ROWS)
}
```
`corrector.rs`：删 `split_top_and`/`first_ident_of` 本体，顶部加：
```rust
use dms_kernel::sql::lex::{first_ident_of, split_top_and};
```
`direct.rs`：删 `from_table_aliases`/`base_col_refs`/`qualify_cols` 本体，顶部加：
```rust
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};
```

- [ ] **Step 4: 验证（PowerShell，前缀 MinGW，下同）**

```
cargo test -p dms-kernel 2>&1 | Select-Object -Last 5
cargo test -p dms-ai-server 2>&1 | Select-Object -Last 8
```
Expected: kernel 新测试绿；server **157+1** 全绿（原测试零改动）。

- [ ] **Step 5: 提交** `git commit -m "kernel: sql::lex 七词法工具下沉，server 改门面/改用，调用点与既有单测零改动"`

---

### Task 2.2: kernel `sql::guard` + `sql::ast`——只读护栏与 AST 收集器

**Files:**
- Create: `crates/kernel/src/sql/guard.rs`（约 180 行）
- Create: `crates/kernel/src/sql/ast.rs`（约 200 行）
- Modify: `crates/server/src/pipeline.rs`、`crates/server/src/corrector.rs`

**Interfaces:**
- Produces: `dms_kernel::sql::guard::{GuardError, is_safe_select_with}`；`dms_kernel::sql::ast::{collect, collect_where_cols}`

`guard.rs` 骨架：
```rust
//! 只读安全护栏（纯判定）。is_safe_select 本体自 server/pipeline.rs:61-105 逐行搬移；
//! 敏感列词表（login_pwd 是 DMS 列名）参数化，由 server 注入。

use sqlparser::ast::Statement;
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

#[derive(Debug)]
pub enum GuardError {
    Parse(String),
    NotSingleStatement,
    NotSelect,
    ExecutableComment,
    WriteOp(String),
    SensitiveColumn,
    UnfilledPlaceholder(String),
    PlaceholderHallucination,
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "{e}"),
            Self::NotSingleStatement => write!(f, "只允许单条语句"),
            Self::NotSelect => write!(f, "只允许 SELECT"),
            Self::ExecutableComment => write!(f, "只读红线：禁止可执行注释"),
            Self::WriteOp(tok) => write!(f, "只读红线：禁止写操作 [{tok}]"),
            Self::SensitiveColumn => write!(f, "SQL 含敏感列"),
            Self::UnfilledPlaceholder(frag) => write!(f, "SQL 含未填充占位符: {frag}"),
            Self::PlaceholderHallucination => write!(f, "SQL 含占位符幻觉"),
        }
    }
}
impl std::error::Error for GuardError {}

/// 原 is_safe_select 本体（pipeline.rs:61-105 逐行搬），三处映射：
/// bail!("只允许单条语句"/"只允许 SELECT"/...) → Err(GuardError::...)；
/// `sql.contains("login_pwd") || sql.contains("password")` →
/// `sensitive.iter().any(|w| stripped.contains(w))`（stripped 小写，server 传小写词表）。
pub fn is_safe_select_with(sql: &str, sensitive: &[&str]) -> Result<(), GuardError> { /* 逐行搬 */ }
```

`ast.rs` 骨架：
```rust
//! SQL AST 收集器（sqlparser visitor）。自 server/corrector.rs 逐行搬移。

use std::collections::{HashMap, HashSet};
use core::ops::ControlFlow;
use sqlparser::ast::{Expr, TableFactor, Visit, Visitor};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use crate::sql::guard::GuardError;

/// Collector 结构体与 impl Visitor：复制自 corrector.rs:14-47（保持私有）。
/// collect：复制自 corrector.rs:50-59，anyhow::Error → GuardError::Parse。
pub fn collect(sql: &str) -> Result<(HashMap<String, String>, Vec<(String, String)>), GuardError> { /* 逐行搬 */ }

/// collect_where_cols：复制自 corrector.rs:432-474，签名不变。
pub fn collect_where_cols(e: &Expr, out: &mut HashSet<String>) { /* 逐行搬 */ }
```

- [ ] **Step 1: 门面一致性测试（红）**，追加到 pipeline.rs `mod tests`：
```rust
#[test]
fn facade_is_safe_select_delegates_to_kernel() {
    for sql in ["SELECT a FROM b", "DELETE FROM t", "SELECT login_pwd FROM t_employee",
                "SELECT * FROM t WHERE note = 'please delete me'"] {
        assert_eq!(
            is_safe_select(sql).is_ok(),
            dms_kernel::sql::guard::is_safe_select_with(sql, &["login_pwd", "password"]).is_ok(),
            "{sql}"
        );
    }
}
```

- [ ] **Step 2: 建 kernel guard.rs / ast.rs，逐行搬入**

- [ ] **Step 3: server 改门面/改用**

`pipeline.rs` 删 `is_safe_select` 本体，改门面（pub 签名不变，main.rs:167 调用点不动）：
```rust
pub fn is_safe_select(sql: &str) -> anyhow::Result<()> {
    Ok(dms_kernel::sql::guard::is_safe_select_with(sql, &["login_pwd", "password"])?)
}
```
`corrector.rs` 删 `Collector`/`collect`/`collect_where_cols` 本体，顶部加：
```rust
use dms_kernel::sql::ast::{collect, collect_where_cols};
```
（`schema_check`/`correct_value` 里 `collect(sql)?` 经 `GuardError: std::error::Error` 自动转 anyhow，一行不改。）

- [ ] **Step 4: 验证**：`cargo test -p dms-kernel` + `cargo test -p dms-ai-server`（157+2 全绿；pipeline 的 safe_select/literal/comment 系列与 corrector 的 collects_alias/no_alias/derived_alias 测试走的就是 kernel 实现）。

- [ ] **Step 5: 提交** `git commit -m "kernel: sql::guard 只读护栏 + sql::ast 收集器下沉，敏感列词表参数化"`

---

### Task 2.3: kernel `policy::scope`——权限纯裁决

**Files:**
- Create: `crates/kernel/src/policy/mod.rs`（约 50 行，含 PolicyError）
- Create: `crates/kernel/src/policy/scope.rs`（约 280 行含测试）
- Modify: `crates/server/src/scope.rs`

**Interfaces:**
- Produces: `dms_kernel::policy::{PolicyError}`；`dms_kernel::policy::scope::{SENTINEL, ScopeSets, BaseDecision, decide_base, merge_employee_ids, merge_customer_codes, expand_department_tree}`

`policy/mod.rs`：
```rust
//! 权限纯算法（零 IO）：ScopeSets 裁决与 AST 注入本体。IO 侧（连库算集合、档案加载）留 server/policy crate。

pub mod scope;
pub mod inject;

#[derive(Debug)]
pub enum PolicyError {
    Parse(String),
    NonSelect,
    BadViewType(i32),
    UnregisteredTable(String),
    ViaHeaderNotScoped { table: String, head: String },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "{e}"),
            Self::NonSelect => write!(f, "只允许 SELECT 语句"),
            Self::BadViewType(v) => write!(f, "角色数据权限配置错误 view_type={v}"),
            // 文案与现 anyhow 消息逐字一致（含运维指引；inject 测试断言子串「未在权限档案登记」）
            Self::UnregisteredTable(t) => write!(f, "表 {t} 未在权限档案登记（meta.scope_binding），已按 fail-closed 拒绝；请核实 Java @DataScope 口径后登记 scoped/global/via"),
            Self::ViaHeaderNotScoped { table, head } => write!(f, "表 {table} 的 via 头表 {head} 未登记 scoped 档案，fail-closed 拒绝"),
        }
    }
}
impl std::error::Error for PolicyError {}
```

`policy/scope.rs` 搬移映射（出处 server/scope.rs）：

| kernel 目标 | 出处 | 变化 |
|---|---|---|
| `pub const SENTINEL: i64 = -1` | :20 | 无 |
| `ScopeSets` + `is_unrestricted` | :22-37 | 无（Serialize derive 保留） |
| `BaseView`（pub(crate)）+ `from_value` | :39-59 | 无 |
| `BaseDecision`（pub） | :63-71 | 私→pub（server compute_scope 要 match） |
| `decide_base` | :73-86 | `anyhow::Result` → `Result<_, PolicyError>`；anyhow! → `PolicyError::BadViewType(max_v)` |
| `merge_employee_ids` / `merge_customer_codes` | :89-129 | 私→pub；`dedup_i64`/`dedup_str` 一并搬入（kernel 内私有） |
| `expand_department_tree` | :132-153 | 私→pub |

kernel 自守测试（新写，泛化）：decide_base 全值域/max 取大/未知 fail-closed、merge 哨兵 vs 空集、部门树环保护。

- [ ] **Step 1: 门面一致性测试（红）**，追加到 scope.rs `mod tests`：
```rust
#[test]
fn facade_decide_base_delegates_to_kernel() {
    // re-export 的 decide_base 与 kernel 直调返回同一结果（现阶段红）
    assert_eq!(decide_base(&[0, 2]).unwrap(), dms_kernel::policy::scope::decide_base(&[0, 2]).unwrap());
}
```

- [ ] **Step 2: 建 kernel policy/mod.rs + policy/scope.rs，逐行搬入**

- [ ] **Step 3: server/scope.rs 删纯函数本体，改 re-export**

删除：`SENTINEL`/`ScopeSets`/`BaseView`/`BaseDecision`/`decide_base`/`merge_employee_ids`/`merge_customer_codes`/`expand_department_tree`/`dedup_i64` 本体，文件顶部加：
```rust
pub use dms_kernel::policy::scope::{
    decide_base, expand_department_tree, merge_customer_codes, merge_employee_ids,
    BaseDecision, ScopeSets, SENTINEL,
};
```
注意：
- `dedup_str` 被 `compute_scope`（现 :253）使用——**server 侧保留一份私有 `dedup_str`**（逻辑与 kernel 内那份相同，各归各 crate，不冲突）。
- `compute_scope` 里 `decide_base(&base_rows)?` 经 `PolicyError: std::error::Error` 自动转 anyhow，一行不改。
- 既有 31 个 scope 测试（含末尾 3 个调 `crate::inject::inject` 的跨模块语义锁）**一行不动**。

- [ ] **Step 4: 验证**：`cargo test -p dms-kernel` + `cargo test -p dms-ai-server`（157+3 全绿）。

- [ ] **Step 5: 提交** `git commit -m "kernel: policy::scope 权限纯裁决下沉（decide_base/merge*/dept_tree），server re-export"`

---

### Task 2.4: kernel `policy::inject`——AST 注入本体

**Files:**
- Create: `crates/kernel/src/policy/inject.rs`（约 350 行含测试）
- Modify: `crates/server/src/inject.rs`

**Interfaces:**
- Consumes: `dms_kernel::policy::scope::ScopeSets`、`dms_kernel::policy::PolicyError`（Task 2.3）
- Produces: `dms_kernel::policy::inject::{Binding, OwnerKind, TableRule, inject_with, build_condition}`

kernel 版核心签名（档案解析器注入——`rule_of`/OnceLock 注册表/`builtin_rules` 27 张 DMS 表绑定全部留 server）：
```rust
//! 权限条件 SQL AST 注入器（纯算法本体）。表档案由调用方以闭包注入；kernel 不持有任何 DMS 表名。

use std::collections::HashSet;
use sqlparser::ast::{Expr, Query, Select, SetExpr, Statement, TableFactor};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use crate::policy::scope::ScopeSets;
use crate::policy::PolicyError;

#[derive(Clone)]
pub struct Binding { /* 逐行搬 server/inject.rs:20-24 */ }
#[derive(PartialEq, Clone, Copy)]
pub enum OwnerKind { Ids, Codes }
#[derive(Clone)]
pub enum TableRule { Scoped(Binding), Global, Via { table: String, local_col: String, remote_col: String } }

/// 原 inject() 本体（server/inject.rs:182-199 逐行搬），两点适配：
/// - rule_of(&table) → rules(&table)（闭包参数）
/// - anyhow::bail! → PolicyError::{NonSelect, UnregisteredTable, ViaHeaderNotScoped}
pub fn inject_with(
    sql: &str,
    sets: &ScopeSets,
    rules: &dyn Fn(&str) -> Option<TableRule>,
) -> Result<String, PolicyError> { /* 逐行搬 */ }

// inject_query / inject_set_expr / inject_select / collect_names / collect_table_conds
// （server/inject.rs:201-335）逐行搬，签名加 rules 透传，保持私有。
// build_condition（:338-369）逐行搬，pub。quote_list（:371-377）逐行搬，私有。
```

kernel 自守测试（新写，泛化表名 t_order/t_cust + 测试用闭包档案）：双维 or 注入、别名缺省用表名、子查询递归、CTE 豁免、via 独查 EXISTS/头表在场跳过、未登记 fail-closed、非 SELECT 拒绝、引号转义。

- [ ] **Step 1: 门面一致性测试（红）**，追加到 inject.rs `mod tests`：
```rust
#[test]
fn facade_inject_delegates_to_kernel() {
    let s = sets(&[7], &[], &["C1"]);
    let sql = "SELECT COUNT(*) FROM t_sales_order so WHERE so.deleted_flag = 0";
    assert_eq!(
        inject(sql, &s).unwrap(),
        dms_kernel::policy::inject::inject_with(sql, &s, &rule_of).unwrap()
    );
}
```

- [ ] **Step 2: 建 kernel/policy/inject.rs，逐行搬入**

- [ ] **Step 3: server/inject.rs 删本体，改 re-export + 门面**

删除：`Binding`/`OwnerKind`/`TableRule` 类型本体与 `inject`/`inject_query`/`inject_set_expr`/`inject_select`/`collect_names`/`collect_table_conds`/`build_condition`/`quote_list` 函数本体。保留：`builtin_rules`/`rule_of`/`seed_rules`/`load_rules`（OnceLock 注册表与 PG IO）。顶部加：
```rust
pub use dms_kernel::policy::inject::{build_condition, inject_with, Binding, OwnerKind, TableRule};

/// 把权限条件注入 SQL。门面：档案解析走 server 注册表（PG 加载回退内置种子），算法在 kernel。
pub fn inject(sql: &str, sets: &ScopeSets) -> anyhow::Result<String> {
    Ok(dms_kernel::policy::inject::inject_with(sql, sets, &rule_of)?)
}
```
`use crate::scope::ScopeSets;` 一行不动（scope.rs 已 re-export）。既有 15 个 inject 测试一行不动。

- [ ] **Step 4: 验证**：`cargo test -p dms-kernel` + `cargo test -p dms-ai-server`（157+4 全绿；15 inject 测试 + scope 末尾 3 个跨模块锁全走 kernel）。

- [ ] **Step 5: 提交** `git commit -m "kernel: policy::inject AST 注入本体下沉，档案闭包注入，server 门面接 rule_of"`

---

### Task 2.5: kernel `nl::time`——中文 NLP 时间/数量基元

**Files:**
- Create: `crates/kernel/src/nl/mod.rs`
- Create: `crates/kernel/src/nl/time.rs`（约 300 行含测试）
- Modify: `crates/server/src/direct.rs`

**Interfaces:**
- Produces: `dms_kernel::nl::time::{time_predicate, fill_time_col, cn_num, recent_n, detect_top_n_with}`

`nl/mod.rs`:
```rust
//! 中文 NLP 基元（规则解析，零 IO 零 LLM）。

pub mod time;
pub mod lexicon;
```

搬移映射（出处 server/direct.rs）：

| kernel 目标 | 出处 | 变化 |
|---|---|---|
| `cn_num(s: &str) -> Option<u32>` | :843-861 | 私→pub |
| `recent_n(q: &str) -> Option<(u32, &'static str)>` | :864-893 | 私→pub |
| `time_predicate(q: &str) -> Option<String>` | :898-981 | 无（词表是通用中文时间词，随函数进 kernel） |
| `fill_time_col(tpl: &str, col: &str) -> String` | :984-986 | 无 |
| `detect_top_n_with(q: &str, default_n: usize) -> usize` | :787-820 | 写死默认 200 → 参数 `default_n`（200 是 server MAX_ROWS 对齐值） |

kernel 自守测试（新写）：把 server/direct.rs 的 `time_recent_n_with_cn_numbers`/`time_quarter_and_half_year`/`time_explicit_month`/`time_relative_words`/`time_col_is_parameterized`/`cn_num_parses`/`top_n_detect` 七个测试**语义等价复制**（断言里的问句是通用中文，可原样；`top_n_detect` 里默认值断言改走 `detect_top_n_with(q, 200)`）。server 原测试不动。

- [ ] **Step 1: 门面一致性测试（红）**，追加到 direct.rs `mod tests`：
```rust
#[test]
fn facade_time_predicate_delegates_to_kernel() {
    assert_eq!(time_predicate("近7天销售额"), dms_kernel::nl::time::time_predicate("近7天销售额"));
    assert_eq!(detect_top_n("销售额前十的客户"), dms_kernel::nl::time::detect_top_n_with("销售额前十的客户", 200));
}
```

- [ ] **Step 2: 建 kernel nl/mod.rs + nl/time.rs，逐行搬入**（`time_window` 留 server——它填死 `order_time`，是 DMS 列名）

- [ ] **Step 3: server/direct.rs 删本体，改 re-export + 薄包装**

删除 `cn_num`/`recent_n`/`time_predicate`/`fill_time_col`/`detect_top_n` 本体，顶部加：
```rust
pub use dms_kernel::nl::time::{cn_num, fill_time_col, recent_n, time_predicate};

/// "前N/topN" → 限制条数（中文数字支持），默认对齐全局 MAX_ROWS
fn detect_top_n(q: &str) -> usize {
    dms_kernel::nl::time::detect_top_n_with(q, 200)
}
```
（`pub use` 使 `crate::direct::time_predicate` 路径对 pipeline.rs:281 保持有效；`time_window` 原样保留，经 pub use 继续可见 `time_predicate`/`fill_time_col`。）

- [ ] **Step 4: 验证**：`cargo test -p dms-kernel` + `cargo test -p dms-ai-server`（157+5 全绿）。

- [ ] **Step 5: 提交** `git commit -m "kernel: nl::time 中文时间/TopN 规则基元下沉，默认行数参数化"`

---

### Task 2.6: kernel `nl::lexicon`——五份词表合并（本任务风险点，独立子任务 + 回归门禁）

**Files:**
- Create: `crates/kernel/src/nl/lexicon.rs`（约 160 行）
- Modify: `crates/server/src/pipeline.rs`（is_followup/time_tokens）、`crates/server/src/direct.rs`（has_residue/strip_relation_words/agg_template）

**Interfaces:**
- Produces: `dms_kernel::nl::lexicon::{FOLLOWUP_MARKS, TIME_GUARDS, STRIP_WORDS, residue_words, relation_words, agg_strip_words}`（验证后回收为 `{FOLLOWUP_MARKS, TIME_GUARDS, STRIP_WORDS}`）

**词表盘点（五份出处与语义）：**

| # | 出处 | 用途语义 | 合并策略 |
|---|---|---|---|
| L1 | pipeline.rs:445-448 `MARK` | is_followup 命中集（any contains） | 原样搬，命名 `FOLLOWUP_MARKS`，不并集 |
| L2 | pipeline.rs:789 数组 | time_tokens 时间词集（BTreeSet 全等护栏） | 原样搬，命名 `TIME_GUARDS`，不并集 |
| L3 | direct.rs:441-447 | has_residue 通用剥词表 | 并入 `STRIP_WORDS` |
| L4 | direct.rs:554-558 | strip_relation_words 剥词表 | 并入 `STRIP_WORDS` |
| L5 | direct.rs:748-753 | agg_template 剥词表 | 并入 `STRIP_WORDS` |

**⚠️ 关键排序约束（读码实证，必须遵守）：** L3/L5 原数组里 `"上月"` 排在 `"上个月"` **之前**——子串先剥会把「上个月」剥成「个月」残留，这是现状行为的一部分（例如「上个月销售额」今天因此在 agg_template 不命中而走 LLM）。所以：
- `STRIP_WORDS` = **L3 原序 + L4 去重后原序 + L5 去重后原序**（三段拼接，段内保持原数组顺序），**绝不做全局长词降序排序**——排序会修复子串问题但同时改变路由行为，超出本任务「零行为变化」边界。
- 每个调用点先剥完自己原表对应的段，新增词只在后段补剥——原表内的剥除顺序与今天逐字节一致。

`lexicon.rs` 完整骨架：
```rust
//! 中文词表单一事实源：原散在 pipeline.rs:445 / pipeline.rs:789 / direct.rs:441 / direct.rs:554 /
//! direct.rs:748 的五份收敛于此。L1/L2 是命中集合语义不并集；L3+L4+L5 剥词表先并集（STRIP_WORDS），
//! 三调用点各持编译期开关——跑通 tools/regression.py 后收掉开关与 *_ORIG，全部直连 STRIP_WORDS。

/// L1 追问指代词（pipeline.rs:445 原样）
pub const FOLLOWUP_MARKS: &[&str] = &[
    "那", "再", "呢", "按", "换", "上个", "下个", "它", "这个", "这张", "该", "此",
    "前", "后", "同比", "环比", "拆", "分开", "对比", "上月", "下月", "去年",
];

/// L2 缓存护栏时间词（pipeline.rs:789 原样）
pub const TIME_GUARDS: &[&str] = &[
    "今天", "昨天", "前天", "本月", "上月", "上个月", "这个月", "本周", "上周", "今年", "去年", "本季度",
];

/// L3+L4+L5 剥词并集 = L3 原序 + L4 去重原序 + L5 去重原序（禁止全局重排，见模块注释）。
pub const STRIP_WORDS: &[&str] = &[
    // —— L3 段（direct.rs:441-447 原序原样）——
    "今天", "今日", "昨天", "昨日", "本月", "这个月", "上月", "上个月", "本周", "这周", "今年",
    "上周", "去年", "近", "最近", "天", "周", "月", "年", "季度", "至今",
    "按", "各", "的", "是多少", "多少", "有", "查", "查询", "统计", "看看", "帮我", "我", "一下",
    "排行", "排名", "前", "第", "名", "top", "TOP", "对比", "和", "与", "分别",
    "一", "二", "三", "四", "五", "六", "七", "八", "九", "十", "百",
    // —— L4 段（direct.rs:554-558 去重后原序）——
    "还买过什么", "还买什么", "还买了什么", "还购买", "还买", "关联购买", "一起买",
    "买过什么", "买了什么", "买过哪些", "买了哪些", "购买清单", "购买过", "买过", "买了",
    "的客户", "哪些客户", "哪些门店", "哪些", "客户", "门店", "商品", "是", "什么", "都", "买",
    // —— L5 段（direct.rs:748-753 去重后原序）——
    "销售额", "销售总额", "营业额", "订单数", "多少单", "几单", "客单价", "卖了多少",
    "成交客户数", "成交客户", "客户数", "多少客户", "呢", "吗", "总共", "一共", "了",
];

// —— 合并期回退开关与原始子集（回归验证通过后整块删除）——
pub const RESIDUE_USES_MERGED: bool = true;
pub const RELATION_USES_MERGED: bool = true;
pub const AGG_STRIP_USES_MERGED: bool = true;

const RESIDUE_WORDS_ORIG: &[&str] = &[ /* L3 原样（direct.rs:441-447） */ ];
const RELATION_WORDS_ORIG: &[&str] = &[ /* L4 原样（direct.rs:554-558） */ ];
const AGG_STRIP_WORDS_ORIG: &[&str] = &[ /* L5 原样（direct.rs:748-753） */ ];

pub fn residue_words() -> &'static [&'static str] {
    if RESIDUE_USES_MERGED { STRIP_WORDS } else { RESIDUE_WORDS_ORIG }
}
pub fn relation_words() -> &'static [&'static str] {
    if RELATION_USES_MERGED { STRIP_WORDS } else { RELATION_WORDS_ORIG }
}
pub fn agg_strip_words() -> &'static [&'static str] {
    if AGG_STRIP_USES_MERGED { STRIP_WORDS } else { AGG_STRIP_WORDS_ORIG }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merged_is_superset_and_prefix_order_preserved() {
        // 并集包含三份原表全部词
        for w in RESIDUE_WORDS_ORIG.iter().chain(RELATION_WORDS_ORIG).chain(AGG_STRIP_WORDS_ORIG) {
            assert!(STRIP_WORDS.contains(w), "{w}");
        }
        // L3 段必须是 STRIP_WORDS 的严格前缀（剥除顺序逐字节保持现状）
        assert_eq!(&STRIP_WORDS[..RESIDUE_WORDS_ORIG.len()], RESIDUE_WORDS_ORIG);
    }
}
```

- [ ] **Step 1: 建 kernel/nl/lexicon.rs**（`RESIDUE_WORDS_ORIG` 等三份从出处原样复制；`STRIP_WORDS` 按「L3 原序 + L4 去重原序 + L5 去重原序」拼装——去重指该词已在前面段落出现则跳过）

- [ ] **Step 2: server 五调用点切换**（红→绿一体：先追加下方门面测试再改实现）

`pipeline.rs`：
```rust
fn is_followup(q: &str) -> bool {
    let n = q.chars().count();
    if n > 14 { return false; }
    dms_kernel::nl::lexicon::FOLLOWUP_MARKS.iter().any(|m| q.contains(m))
}
fn time_tokens(q: &str) -> std::collections::BTreeSet<&'static str> {
    dms_kernel::nl::lexicon::TIME_GUARDS.iter().copied().filter(|t| q.contains(t)).collect()
}
```
`direct.rs`：
- `has_residue` 内嵌数组 → `for w in dms_kernel::nl::lexicon::residue_words()`
- `strip_relation_words` 内嵌数组 → `for w in dms_kernel::nl::lexicon::relation_words()`
- `agg_template` 内嵌数组 → `for w in dms_kernel::nl::lexicon::agg_strip_words()`

门面一致性测试（追加到 pipeline.rs tests）：
```rust
#[test]
fn lexicon_matches_original_word_sets() {
    // L1/L2 与原数组逐字全等（防搬错字）
    assert_eq!(dms_kernel::nl::lexicon::FOLLOWUP_MARKS.len(), 22);
    assert_eq!(dms_kernel::nl::lexicon::TIME_GUARDS.len(), 12);
    assert!(is_followup("那上个月呢"));
    assert_ne!(time_tokens("本月销售额"), time_tokens("上月销售额"));
}
```

- [ ] **Step 3: 验证**：`cargo test -p dms-kernel` + `cargo test -p dms-ai-server`（157+6 全绿；direct 的 has_residue_basics/breakdown_rejects_value_filtered_question/breakdown_accepts_clean_questions/agg_* 系列即并集行为回归）。

- [ ] **Step 4: 提交（开关态）** `git commit -m "kernel: nl::lexicon 五份中文词表收敛，剥词并集+三开关（回归验证前）"`

- [ ] **Step 5: 连库门禁回归**

```
python tools/regression.py
```
（需 MySQL/PG/Docker 环境与已构建的 `target\debug\dms-ai-server.exe`；该判官逐题断言 route/SQL/视图/权限/红线，剥词并集若改变任何问句的路由或结果集会在此暴露。）

- [ ] **Step 6a: 无差异 → 收开关（独立提交）**

删除 `RESIDUE_USES_MERGED`/`RELATION_USES_MERGED`/`AGG_STRIP_USES_MERGED` 三个开关、`RESIDUE_WORDS_ORIG`/`RELATION_WORDS_ORIG`/`AGG_STRIP_WORDS_ORIG` 三份原子集、`residue_words`/`relation_words`/`agg_strip_words` 三个 fn；direct.rs 三调用点直连 `dms_kernel::nl::lexicon::STRIP_WORDS`。重跑 Step 3 验证后提交：
`git commit -m "kernel: 词表并集回归无差异，收掉合并期开关与原始子集"`

- [ ] **Step 6b: 有差异 → 按调用点回退（替代路径）**

把导致差异的调用点开关拔为 `false`（该点退回原始子集，行为与今天逐字节一致），其余保持并集；`cargo test` 复绿后提交，并把「差异问句清单 + 归属调用点 + 预期 vs 实际」写入 commit body，留待 Task 8（direct 解体）裁决是否放行该行为变化。**禁止**在回归有差异时执行 6a。

- [ ] **Step 6c: 无库环境降级（仅当 Step 5 无法执行时）**

写一个临时 `crates/server/examples/lexicon_diff.rs`：读 `tools/regression_cases.json` 全部 question，对每个问句分别用 MERGED 与三份 ORIG 跑三调用点的剥词逻辑，输出残留串 diff；人工逐条判定「新增剥词是否改变该题应有的路由」。判定全可接受才允许走 6a，且必须在下一次有库环境时补跑 Step 5。判完删除该 examples 文件。

---

### Task 2.7: kernel `present`——ViewSpec 呈现决策树（剥离 DMS 词表/码表）

**Files:**
- Create: `crates/kernel/src/present/mod.rs`（约 230 行：类型 + build_with + patch_kpi_delta）
- Create: `crates/kernel/src/present/infer.rs`（约 320 行含测试）
- Modify: `crates/server/src/viewspec.rs`

**Interfaces:**
- Produces: `dms_kernel::present::{ViewSpec, ColumnSpec, Block, ChartKind, Kpi, Delta, Interact, Role, Semantic, WordRule, PresentLexicon, build_with, patch_kpi_delta}`

**剥离清单（留 server/semantic，不进 kernel）：** `DIM_POOL`（viewspec.rs:108）、`infer_semantic`/`infer_role` 全部中文词表、`province_cn` 34 省码表（viewspec.rs:294-307）。判定顺序/阈值/树结构是算法，进 kernel。

`present/infer.rs` 核心结构（完整）：
```rust
//! 列语义/角色推断与结论洞察（算法本体）。全部中文业务词表由 PresentLexicon 注入。

use serde_json::Value;
use crate::present::{ColumnSpec, Role, Semantic};

/// 一组「包含即中 / 后缀即中」的词规则
#[derive(Clone, Copy, Default)]
pub struct WordRule {
    pub contains: &'static [&'static str],
    pub suffix: &'static [&'static str],
}
impl WordRule {
    pub fn matches(&self, name: &str) -> bool {
        self.contains.iter().any(|w| name.contains(w)) || self.suffix.iter().any(|s| name.ends_with(s))
    }
}

/// 呈现决策树的全部业务词表/码表注入点（DMS 实例在 server/viewspec.rs 装配）
#[derive(Clone, Copy, Default)]
pub struct PresentLexicon {
    pub drill_dims: &'static [&'static str],
    pub percent: WordRule,
    pub money: WordRule,
    pub geo: WordRule,
    pub customer: WordRule,
    pub goods: WordRule,
    pub count: WordRule,
    pub order: WordRule,
    pub time: WordRule,
    pub id: WordRule,
    /// 纯数值列兜底判 metric 时的排除词（原名含"年"/"月"不算指标）
    pub bare_metric_excludes: &'static [&'static str],
    /// (省级区划码, 省名)——insight 里 geo 列翻名
    pub province_names: &'static [(&'static str, &'static str)],
}
```

`infer_semantic` 参数化版（判定顺序与原 if-else 链逐项对应，注释里的「Count 必须先于 Order」保留）：
```rust
pub fn infer_semantic(name: &str, lex: &PresentLexicon) -> Semantic {
    if lex.percent.matches(name) { Semantic::Percent }
    else if lex.money.matches(name) { Semantic::Money }
    else if lex.geo.matches(name) { Semantic::Geo }
    else if lex.customer.matches(name) { Semantic::Customer }
    else if lex.goods.matches(name) { Semantic::Goods }
    else if lex.count.matches(name) { Semantic::Count }   // 必须先于 Order
    else if lex.order.matches(name) { Semantic::Order }
    else { Semantic::None }
}
```

server/viewspec.rs 装配（DMS 词表全量在此，唯一事实源）：
```rust
pub use dms_kernel::present::{
    build_with, patch_kpi_delta, Block, ChartKind, ColumnSpec, Delta, Interact, Kpi,
    PresentLexicon, Role, Semantic, ViewSpec, WordRule,
};
use serde_json::Value;

const WR: fn(&'static [&'static str], &'static [&'static str]) -> WordRule =
    |contains, suffix| WordRule { contains, suffix };

/// DMS 呈现词表（原 viewspec.rs:108 DIM_POOL、infer_semantic/infer_role 内嵌词表、
/// :294-307 三十四省码表逐字搬到这里，一个字不改）
static DMS_LEXICON: PresentLexicon = PresentLexicon {
    drill_dims: &["省份", "商品分类", "业务员", "客户", "门店", "月份"],
    percent: WR(&["率", "占比", "%"], &[]),
    money: WR(&["金额", "销售额", "营业额", "客单价", "余额", "费用"], &["额"]),
    geo: WR(&["省", "市", "区县", "地区"], &[]),
    customer: WR(&["客户"], &[]),
    goods: WR(&["商品", "SKU", "sku"], &[]),
    count: WR(&["数", "销量", "笔数"], &[]),
    order: WR(&["单号", "订单"], &[]),
    time: WR(&["时间", "日期", "月份", "季度", "年月"], &["date", "time"]),
    id: WR(&["编码", "单号", "编号"], &["code", "_id"]),
    bare_metric_excludes: &["年", "月"],
    province_names: &[
        ("110000", "北京"), ("120000", "天津"), /* …34 条逐字搬自 viewspec.rs:295-306… */ ("820000", "澳门"),
    ],
};

/// 组装 ViewSpec：门面签名与今天完全一致（pipeline.rs 六处调用点一行不改）
pub fn build(columns: &[String], rows: &[Vec<Value>]) -> ViewSpec {
    build_with(columns, rows, &DMS_LEXICON)
}
```

kernel 侧映射（出处 viewspec.rs，函数体逐行搬，词表引用改 `lex.*`）：
- 类型全部搬：`Role`/`Semantic`/`ColumnSpec`/`Block`/`ChartKind`/`Delta`/`Kpi`/`Interact`/`ViewSpec`（:8-101，含 serde 属性与 `is_none_sem`/`drill_empty` 辅助）。
- `infer_drill`（:110-124）：`DIM_POOL` → `lex.drill_dims`。
- `infer_semantic`（:127-149）→ 上方参数化版。
- `is_numeric_col`（:152-168）原样。
- `infer_role`（:171-194）：时间/Id 词表 → `lex.time`/`lex.id`；`!n.contains("年") && !n.contains("月")` → `!lex.bare_metric_excludes.iter().any(|w| n.contains(w))`。
- `patch_kpi_delta`（:202-213）原样（无词表，pub 直出）。
- `mk`（:216-221）、`compute_insight`（:224-283）：`province_cn(&raw)` → `lex.province_names.iter().find(|(c, _)| *c == raw).map(|(_, n)| *n)`；「未知」占位、¥、万/亿 `compress`、insight 文案模板为通用输出文案，随算法进 kernel。
- `val_str`/`cell_f64`/`compress`（:285-291, :403-421）原样。
- `build`（:310-401）→ `build_with(columns, rows, lex)`，决策树本体一字不改；`PIE_MAX`/`BAR_MAX`/`BAR_TOP`/`ENTITY_MIN_COLS` 阈值是通用呈现常量，随 kernel。

- [ ] **Step 1: 门面一致性测试（红）**，追加到 viewspec.rs `mod tests`：
```rust
#[test]
fn facade_build_matches_kernel_lexicon_call() {
    let rows = vec![vec![serde_json::json!("广东")], vec![serde_json::json!("100")]];
    let via_facade = serde_json::to_value(build(&cols(&["省份"]), &rows)).unwrap();
    let via_kernel = serde_json::to_value(
        dms_kernel::present::build_with(&cols(&["省份"]), &rows, &DMS_LEXICON)
    ).unwrap();
    assert_eq!(via_facade, via_kernel);
}
```

- [ ] **Step 2: 建 kernel present/mod.rs + present/infer.rs，逐行搬入**（`mod.rs` 里 `pub mod infer;` + 类型 + `build_with`；`build_with` 调 `infer::{infer_role, infer_semantic, infer_drill, compute_insight}`）

- [ ] **Step 3: server/viewspec.rs 删本体，按上方骨架改 re-export + DMS_LEXICON + build 门面**。既有 10 个 viewspec 测试（含 kpi_delta_up_down_and_zero）一行不动。

- [ ] **Step 4: 验证**：`cargo test -p dms-kernel` + `cargo test -p dms-ai-server`（157+7 全绿）+ `cargo build 2>&1 | Select-Object -Last 3`（前端协议字段零变化由 serde 属性原样保证）。

- [ ] **Step 5: 提交** `git commit -m "kernel: present 呈现决策树下沉，DIM_POOL/中文词表/34省码表参数化为 PresentLexicon"`

---

### Task 2.8: 收尾全量验证

- [ ] **Step 1: 全量测试**
```
cargo test 2>&1 | Select-Object -Last 15
```
Expected: workspace 全绿；server 侧原 157 个测试一个未改地通过 + 7 个新增门面测试；kernel 新测试全绿。

- [ ] **Step 2: 依赖方向与红线自查**
```
cargo tree -p dms-kernel --prefix none 2>&1 | Select-String "sqlx|reqwest|axum|tokio"
```
Expected: **空**（kernel 零 IO 依赖）。
```
cargo tree -p dms-ai-server --prefix none 2>&1 | Select-String "dms-kernel" | Select-Object -First 3
```
Expected: server 依赖 dms-kernel。

- [ ] **Step 3: 行为不变自查**
```
cargo build 2>&1 | Select-Object -Last 3
```
Expected: `Finished dev profile`；`target\debug\dms-ai-server.exe` 正常产出。若 Task 2.6 已跑过 `python tools/regression.py`，本步无需重跑；若当时无环境，此处记录待补。

- [ ] **Step 4: 提交收尾**（若有未提交改动）`git commit -m "task2 收尾: 全量验证绿，kernel 依赖方向自查通过"`

---

## 自检（已执行）
- **spec 覆盖**：对应迁移步 2（纯算法下沉 kernel + 词表合并；验收「调用点 re-export 不改；词表先并集+开关，跑回归再收」逐项落在 Task 2.6）。✓
- **占位符扫描**：词表内容、`province_names` 34 条、`*_ORIG` 子集标注「从出处逐字搬」的，执行时按行号复制，非 TBD；其余签名/结构/门面均给完整代码。✓
- **类型一致**：`GuardError`/`PolicyError` 手写 enum（spec 4.2）；`inject_with` 闭包注入与 spec 2.1 `rules: &RuleSet` 的终态兼容（Task 3/5 再把闭包收敛为 RuleSet 类型）；`ScopeSets`/`TableRule`/`Binding`/`OwnerKind` 语义 1:1 未动（spec 第 2 节首行）。✓
- **后续任务依赖**：Task 3 三段 newtype 落 `sql/mod.rs`（GuardError 已就位）；Task 5 policy crate 直接复用 `kernel::policy` + server 的 builtin_rules/注册表（spec 3.2 终态，46 权限单测届时随注册表迁移，本任务未物理搬动它们）。✓
- **157 单测零改动**：已逐文件核对 re-export/门面后 `use super::*` 解析路径不变（scope 测试经 `pub use` 拿 `decide_base` 等；inject 测试走 server 门面 inject；viewspec 测试经 `pub use` 拿类型与 `build`/`patch_kpi_delta`）。✓

## 备注（Windows 构建）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）
