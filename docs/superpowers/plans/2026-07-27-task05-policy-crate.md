# Task 5：policy crate 权限内核 IO 侧迁移实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把权限内核的 IO 侧（principal 加载 / scope 计算 / scope 缓存 / RuleSet 播种加载）从 server 原样迁入 dms-policy，RuleSet 从 OnceLock 改 `RwLock<Arc<RuleSet>>` 支持热更新，缓存补显式失效接口；46 个权限单测一个字不改地通过 = 硬验收。

**Architecture:** 纯算法已在 kernel（Task 2），三段 newtype 与 ReadOnlyMySql 已在 connector（Task 3），本任务只搬 IO 编排与进程状态，不碰任何算法语义。policy 依赖 kernel+connector，server 经 re-export 消费，依赖方向不变。

**Tech Stack:** Rust workspace、cargo、sqlx（PG 侧 &PgPool 签名不动；MySQL 侧走 connector FixedStmt 通道）。

## Global Constraints

- **语义 1:1 复刻 Java，一行不改，只换位置**：魔数 101/102/103、view_type 全值域、字典 key `payment_customer_for_inside/payment_customer_for_all`、哨兵 -1、落旗规则、SQL 文本形状全部保持原样。
- **46 权限单测硬验收**：31 scope + 15 inject。测试函数体（含全部断言）逐字节复制；仅允许两类适配：① `use super::*;` → 目标 crate 显式 use；② mod tests 内 helper（`s`/`sets`/`norm`）随测试文件复制（它们本就属于测试）。**不许改任何断言。**
- **有意的行为变化仅两处**（spec 迁移步 5 要求，除此之外一律不许变）：
  1. `load_rules` 重复调用：旧 = `REGISTRY.set` 静默失败（inject.rs:176）；新 = 原子换入热更新。
  2. scope 缓存：新增 `invalidate_scope(login, role)` 显式失效；当日过期语义（epoch_day 比对）不动。
- **RuleSet 并发铁律**：每请求入口 `rules::snapshot()` clone 一次 `Arc<RuleSet>` 并全程使用；禁止在请求中途重复 snapshot（保证单次请求内规则一致，哪怕中途热更新）。
- **MySQL 查询通道**：一律走 connector 的 `fixed(&'static str)` 字面量通道 / 占位符模板通道 + 参数绑定；**不得**在 policy 内持有或拼接收 LLM 串的裸池/String 查询入口。所需接口不存在 → 停下，标注「需 Task 2/3 补」，不自己重实现。
- **PG 侧不动**：`seed_rules`/`load_rules` 继续吃 `&sqlx::PgPool`（红线只锁 MySQL 池；policy 的 sqlx postgres feature 已在 Task 1 备好）。
- **不新增第三方依赖**；锁 poison 容错沿用现 `if let Ok` 风格（写失败加 tracing::warn，不 panic）。
- **其余 111 个 server 单测零改动零减少**（157 - 46 = 111）。
- Windows 构建须前缀 MinGW bin 路径（见文末备注），cargo 命令走 PowerShell 不走 Bash。

## 上游契约清单（Task 5.0 逐项核对；缺失即 blocker）

**需 Task 2（kernel）已 pub 交付：**
| # | 接口 | 用途 |
|---|---|---|
| K1 | `ScopeSets{employee_ids,employee_codes,customer_codes}` + `is_unrestricted()` + `SENTINEL` | scope IO 与测试 |
| K2 | `decide_base(&[i32]) -> anyhow::Result<BaseDecision>`，`BaseDecision` 三个变体 pub 且 `PartialEq+Debug` | scope_tests 8 断言直接匹配变体 |
| K3 | `merge_employee_ids` / `merge_customer_codes` / `expand_department_tree` | scope_tests 19 断言 |
| K4 | `Binding` / `OwnerKind` / `TableRule` / `builtin_rules() -> HashMap<String, TableRule>` | rules.rs 播种与回退 |
| K5 | `RuleSet`：包 `HashMap<String,TableRule>`，支持从 HashMap 构造（`From` 或等价），含 `rule_of(&self, &str) -> Option<TableRule>` | snapshot/热更新 |
| K6 | `inject(sql: &str, sets: &ScopeSets, rules: &RuleSet) -> anyhow::Result<String>`（三参）+ `build_condition` | 两参兼容包装与生产快照路径 |
| K7 | `dedup_i64` / `dedup_str` pub（compute_scope:253 在用） | scope IO |

**需 Task 3（connector）已交付：**
| # | 接口 | 用途 |
|---|---|---|
| C1 | `ReadOnlyMySql`（server AppState/CLI 已持有它而非裸 MySqlPool） | 全部 DMS 表查询入参 |
| C2 | `fixed(&'static str) -> FixedStmt`；FixedStmt 支持 bind(i64/&str) + `fetch_all`/`fetch_optional` 元组解码 | 固定 SQL 查询（principal 3 条 + scope 3 条） |
| C3 | 占位符模板通道：`fixed_in(template: &'static str, n: usize)`（模板内含 `{ph}` 标记，connector 内部只把 `{ph}` 展开为 `?,?,...`，其余原样） | scope 4 处动态 IN 查询（department_employee_ids / subordinate_ids / fetch_str_in 系 / group_customer_codes / manager_customer_codes） |
| C4 | server 侧 `&PgPool` 仍可拿到（现状 AppState.pg 即 PgPool） | seed/load_rules 签名不动 |

> C3 是守住红线的关键：模板是编译期字面量、展开只产 `?`，LLM 拼接串在类型上进不来；参数全走 bind。若 Task 3 未交付 C3，**本任务 blocker，升级 team-lead**（备选 String 查询入口会稀释「类型闸门」红线，不建议）。

---

### Task 5.0: 上游契约核对（gate，不写代码）

**Files:**
- 只读：`crates/kernel/src/**`、`crates/connector/src/**`

- [ ] **Step 1: 逐项 grep 核对 K1-K7 / C1-C4**

Run（PowerShell，前缀 MinGW，下同）:
```powershell
Select-String -Path crates/kernel/src/*.rs -Pattern "pub fn decide_base|pub enum BaseDecision|pub fn merge_employee_ids|pub fn merge_customer_codes|pub fn expand_department_tree|pub fn builtin_rules|pub struct RuleSet|pub fn inject|pub fn build_condition|pub fn dedup_str|pub const SENTINEL|pub struct ScopeSets"
Select-String -Path crates/connector/src/*.rs -Pattern "pub struct ReadOnlyMySql|pub fn fixed|fixed_in|struct FixedStmt"
```
Expected: 每条契约至少一处命中。

- [ ] **Step 2: 缺项处理**

任一契约缺失 → **停止本任务**，把缺项清单发回 team-lead（标注「需 Task 2 补 K 某」/「需 Task 3 补 C 某」），不在 policy 内重实现。全齐 → 继续。

---

### Task 5.1: policy 模块骨架 + rules.rs（RwLock 热更新）+ re-export 层 + 两参兼容包装

**Files:**
- Modify: `crates/policy/src/lib.rs`
- Create: `crates/policy/src/rules.rs`
- Create: `crates/policy/src/scope.rs`（本步只放 re-export 层，IO 在 5.4 进）
- Create: `crates/policy/src/cache.rs`（本步只放空壳声明，实现 5.4 进）

**Interfaces:**
- Consumes: K1/K4/K5/K6/K7（kernel）；`&sqlx::PgPool`（seed/load）
- Produces: `dms_policy::rules::{snapshot, load_rules, seed_rules}`；`dms_policy::inject`（两参兼容包装）；`dms_policy::scope::*` re-export 面

- [ ] **Step 1: 写 rules.rs 完整实现**

```rust
//! RuleSet 注册表：RwLock<Arc<RuleSet>> 热更新 + 每请求 Arc 快照。
//! 语义变化（spec 迁移步 5 有意为之）：load_rules 重复调用从「OnceLock 静默失败」
//! 改为「原子换入」。未 load 时回退内置种子（对齐旧 rule_of 的 BUILTIN 回退）。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use dms_kernel::{builtin_rules, RuleSet, TableRule, OwnerKind, Binding};

static REGISTRY: OnceLock<RwLock<Arc<RuleSet>>> = OnceLock::new();

fn cell() -> &'static RwLock<Arc<RuleSet>> {
    REGISTRY.get_or_init(|| RwLock::new(Arc::new(RuleSet::from(builtin_rules()))))
}

/// 每请求入口调用一次，全程持有该快照（请求内规则一致，不受中途热更新影响）。
pub fn snapshot() -> Arc<RuleSet> {
    match cell().read() {
        Ok(g) => Arc::clone(&g),
        Err(_) => Arc::new(RuleSet::from(builtin_rules())), // poison 容错，对齐旧 if let Ok 静默风格
    }
}

/// 原子换入（热更新）。pub(crate)：单测与 load_rules 用。
pub(crate) fn swap_registry(rs: RuleSet) {
    match cell().write() {
        Ok(mut g) => *g = Arc::new(rs),
        Err(e) => tracing::warn!("rules 注册表写锁 poison，热更新被跳过: {e}"),
    }
}
```

`seed_rules` 与 `load_rules` 从 `crates/server/src/inject.rs:104-178` **原样搬**（PG 行读取/upsert/unknown-mode warn 逐字不动），仅两处适配：
- `use crate::...` → 顶部 use 改为上面骨架；
- `load_rules` 末尾 `let _ = REGISTRY.set(m);` → `swap_registry(RuleSet::from(m));`（**唯一语义变化点**，热更新）。

- [ ] **Step 2: rules.rs 内新增热更新单测（src 内 #[cfg(test)]，不占 46 名额）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_defaults_to_builtin() {
        // 未 load：回退内置种子（对齐旧 BUILTIN 回退）
        let rs = snapshot();
        assert!(matches!(rs.rule_of("t_sales_order"), Some(TableRule::Scoped(_))));
        assert!(matches!(rs.rule_of("t_goods"), Some(TableRule::Global)));
    }

    #[test]
    fn hot_swap_replaces_registry() {
        // 重复换入生效（旧 OnceLock 第二次静默失败 → 新语义热更新）
        let mut m: HashMap<String, TableRule> = HashMap::new();
        m.insert("t_x".into(), TableRule::Global);
        swap_registry(RuleSet::from(m));
        let rs = snapshot();
        assert!(matches!(rs.rule_of("t_x"), Some(TableRule::Global)));
        assert!(rs.rule_of("t_sales_order").is_none(), "换入后旧表不存在");
        // 恢复内置，避免影响同进程其他测试
        swap_registry(RuleSet::from(builtin_rules()));
    }
}
```
> 注意：cargo test 默认多线程并行，两个测试都读写同一静态。给该 mod 的测试加 `#[serial]`？**不引新依赖**——改为把两断言合进一个 `#[test]`（先 builtin 断言、再 swap 断言、再复位），单测试内串行。按此修正：实际只落 **1 个** `registry_builtin_then_hot_swap` 测试。

- [ ] **Step 3: 写 scope.rs re-export 层（本步无 IO）**

```rust
//! 数据权限集合计算（IO 侧）。纯算法在 kernel，此处 re-export 供测试与调用方单点引用。
pub use dms_kernel::{
    decide_base, expand_department_tree, merge_customer_codes, merge_employee_ids,
    BaseDecision, ScopeSets, SENTINEL,
};
```

- [ ] **Step 4: 写 lib.rs（模块声明 + 顶层 re-export + 两参兼容包装）**

```rust
//! dms-policy：行级数据权限 IO 侧（语义 1:1 复刻 Java DMS，唯一「改错=越权」模块）。

pub mod cache;
pub mod principal;
pub mod rules;
pub mod scope;

pub use principal::{list_roles, load_principal, Principal};
pub use scope::{compute_scope, ScopeSets};

/// 兼容包装：自取一次 rules 快照后注入。仅供 46 个迁移测试与过渡调用点使用；
/// 生产请求路径必须「入口 rules::snapshot() 一次 + dms_kernel::inject(.., &rules)」。
pub fn inject(sql: &str, sets: &ScopeSets) -> anyhow::Result<String> {
    let rules = rules::snapshot();
    dms_kernel::inject(sql, sets, &rules)
}
```
`cache.rs`/`principal.rs` 本步先放空壳（`//!` 注释 + 后续步填充），`compute_scope`/`load_principal` 等签名可暂缺——lib.rs 相应 use 行先注释，5.4/5.5 填回。**或更省事**：5.1 的 lib.rs 只声明 `pub mod rules; pub mod scope;` + `inject` 包装，principal/cache 在各自子任务落地时再挂模块。按后者执行（编译随时绿）。

- [ ] **Step 5: 编译 + rules 新单测**

Run: `cargo test -p dms-policy 2>&1 | Select-Object -Last 8`
Expected: 编译过；`registry_builtin_then_hot_swap` passed。

- [ ] **Step 6: 提交**

```bash
git add crates/policy
git commit -m "policy: rules.rs RwLock<Arc<RuleSet>> 热更新 + snapshot + re-export 层 + 两参 inject 兼容包装"
```

---

### Task 5.2: 46 权限单测落位（硬验收①）

**Files:**
- Create: `crates/policy/tests/scope_tests.rs`（28 个）
- Create: `crates/policy/tests/inject_tests.rs`（15 个）
- Create: `crates/policy/tests/inject_e2e.rs`（3 个）

**Interfaces:**
- Consumes: `dms_policy::scope::*`（K1/K2/K3 re-export）、`dms_policy::inject`、`dms_policy::ScopeSets`
- Produces: 硬验收锁——后续任何改动打破语义立即红

- [ ] **Step 1: scope_tests.rs——从 server/src/scope.rs:454-649 复制 28 个测试**

复制范围：`mod tests` 内除最后 3 个 e2e（653-692 的 `sentinel_injects_reject_condition`/`all_view_injects_nothing`/`me_view_injects_own_id_only`）外的全部 28 个 `#[test]`：
- 基础档裁决 8（base_*）、merge_ids 7、merge_cust 6、dept_tree 6、ScopeSets 语义 1。
- 适配仅限：`use super::*;` → `use dms_policy::scope::*;`；helper `fn s(v: &[&str]) -> Vec<String>` 原样带上。
- **断言一字不改。**

- [ ] **Step 2: inject_tests.rs——从 server/src/inject.rs:379-547 复制 15 个测试**

- 适配仅限：`use super::*;` → `use dms_policy::{inject, ScopeSets};`；helper `sets`/`norm` 原样带上。
- 原测试里 `inject(...)` 裸调用经 use 解析到 `dms_policy::inject`（两参包装），**测试体零改动**。

- [ ] **Step 3: inject_e2e.rs——从 server/src/scope.rs:652-692 复制 3 个测试**

- 这 3 个原本反向依赖 `crate::inject`（scope→inject 跨模块）；迁到 policy/tests/ 后同时 `use dms_policy::scope::*; use dms_policy::inject;`，crate 循环问题消失（集成测试天然可跨模块）。
- 原 `crate::inject::inject(...)` 调用改为 `inject(...)`（去路径前缀，属 use 行适配的连带，断言不动）。

- [ ] **Step 4: 跑硬验收①**

Run: `cargo test -p dms-policy --test scope_tests --test inject_tests --test inject_e2e 2>&1 | Select-String "test result"`
Expected: 三行合计 **46 passed, 0 failed**。
任一红 → 不是本任务改测试，而是 **Task 2 交付缺口**（函数未 pub/形状不符）：停下，回 Task 2 补，再复跑。

- [ ] **Step 5: 提交**

```bash
git add crates/policy/tests
git commit -m "policy/tests: 46 权限单测原样落位（28 scope + 15 inject + 3 e2e），断言零改动"
```

---

### Task 5.3: principal.rs 原样搬（MySQL 通道换 FixedStmt）

**Files:**
- Create: `crates/policy/src/principal.rs`（93 行整体）
- Modify: `crates/policy/src/lib.rs`（挂 `pub mod principal;` + 顶层 re-export）

**Interfaces:**
- Consumes: C1 `ReadOnlyMySql`、C2 `fixed`+FixedStmt
- Produces: `dms_policy::{Principal, load_principal, list_roles}`

- [ ] **Step 1: 原样搬 93 行，仅三类适配**

1. 签名：`mysql: &MySqlPool` → `mysql: &ReadOnlyMySql`（3 处：load_principal / list_roles / 内部查询入参）。
2. 查询通道：`sqlx::query_as("SELECT ...", ).bind(x).fetch_optional(mysql)` → `mysql.fixed("SELECT ...").bind(x).fetch_optional_as::<(i64,String,String,Option<i8>,Option<i64>)>()`（确切方法名以 Task 3 交付的 FixedStmt 为准；缺 bind/元组解码 → 需 Task 3 补 C2）。
3. `use sqlx::MySqlPool;` → `use dms_connector::ReadOnlyMySql;`。

**不动**：两条 SQL 文本、多角色 fail-closed 三分支（Some 匹配/单角色/0 角色超管短路/多角色 bail 列可选角色）、`role_code.trim()`、错误消息逐字。

- [ ] **Step 2: 编译**

Run: `cargo build -p dms-policy 2>&1 | Select-Object -Last 3`
Expected: Finished，无 error（principal 无离线单测，连库行为由判官层验收，本步不加测试）。

- [ ] **Step 3: 提交**

```bash
git add crates/policy
git commit -m "policy: principal.rs 原样搬入（多角色 fail-closed 不动），MySQL 查询走 FixedStmt 通道"
```

---

### Task 5.4: cache.rs（RwLock+invalidate）+ scope.rs IO 搬入

**Files:**
- Modify: `crates/policy/src/cache.rs`（空壳 → 实现）
- Modify: `crates/policy/src/scope.rs`（re-export 层之上追加 IO）
- Modify: `crates/policy/src/lib.rs`（挂 compute_scope/compute_scope_cached re-export）

**Interfaces:**
- Consumes: C1/C2/C3（含 `fixed_in` 占位符模板）、K7 dedup、crate::principal::Principal
- Produces: `dms_policy::scope::{compute_scope, compute_scope_cached}`；`dms_policy::cache::invalidate_scope`

- [ ] **Step 1: cache.rs 完整实现**

```rust
//! scope 进程内缓存：RwLock<HashMap> + 当日过期（不动）+ 显式失效（新增）。
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use dms_connector::ReadOnlyMySql;
use dms_kernel::ScopeSets;

use crate::principal::Principal;

type CacheMap = HashMap<(String, String), (ScopeSets, u64)>;

static SCOPE_CACHE: OnceLock<RwLock<CacheMap>> = OnceLock::new();

fn cell() -> &'static RwLock<CacheMap> {
    SCOPE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn epoch_day() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86400)
        .unwrap_or(0)
}

/// 读命中（仅当日）：拆成纯函数便于离线单测。语义同旧 Mutex 版逐行对齐。
fn get_cached(key: &(String, String), today: u64) -> Option<ScopeSets> {
    if let Ok(map) = cell().read() {
        if let Some((sets, day)) = map.get(key) {
            if *day == today {
                return Some(sets.clone());
            }
        }
    }
    None
}

fn put_cached(key: (String, String), sets: &ScopeSets, today: u64) {
    if let Ok(mut map) = cell().write() {
        map.insert(key, (sets.clone(), today));
    }
}

/// 显式失效（新增接口）：权限档变更后由管理面/CLI 调用。
pub fn invalidate_scope(login: &str, role: &str) {
    if let Ok(mut map) = cell().write() {
        map.remove(&(login.to_string(), role.to_string()));
    }
}

/// 命中当日缓存直接返回；miss 重算并写入。编排同旧 compute_scope_cached 逐行对齐。
pub async fn compute_scope_cached(mysql: &ReadOnlyMySql, p: &Principal) -> anyhow::Result<ScopeSets> {
    let key = (p.login_name.clone(), p.role_code.clone());
    let today = epoch_day();
    if let Some(sets) = get_cached(&key, today) {
        return Ok(sets);
    }
    let sets = crate::scope::compute_scope(mysql, p).await?;
    put_cached(key, &sets, today);
    Ok(sets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sets_marker() -> ScopeSets {
        ScopeSets { employee_ids: vec![7], ..Default::default() }
    }

    #[test]
    fn cache_hit_miss_invalidate_expiry() {
        // 单测试内串行（静态共享，不引 serial 依赖）
        let key = ("u1".to_string(), "r1".to_string());
        let today = epoch_day();
        assert!(get_cached(&key, today).is_none(), "初始 miss");
        put_cached(key.clone(), &sets_marker(), today);
        assert_eq!(get_cached(&key, today).unwrap().employee_ids, vec![7], "当日命中");
        assert!(get_cached(&key, today + 1).is_none(), "隔日过期不命中");
        invalidate_scope("u1", "r1");
        assert!(get_cached(&key, today).is_none(), "显式失效后 miss");
    }
}
```

- [ ] **Step 2: scope.rs 追加 IO 部分——从 server/src/scope.rs 原样搬以下函数**

搬移清单（逐字，含全部 doc 注释与 Java 行号引用）：
- `compute_scope`（185-281）：开头超管短路、`t_role_data_scope` 查询（C2 固定通道）、101/102/103 分支、段落编排全部不动；`dedup_str` 改从 kernel（K7）经文件顶部 re-export 已可用。
- 7 类查询函数：`user_departments` / `self_and_children_departments` / `department_employee_ids` / `subordinate_ids` / `login_names_by_ids` / `actual_names_by_ids` / `customers_by_area_manager` / `common_customer_codes` / `group_customer_codes` / `manager_customer_codes` / `fetch_str_in`（284-436）。
- 辅助：`placeholders`（438-440）保留（内部拼 `?` 用，配合 C3）。

通道适配映射（SQL 文本逐字不动，只换执行方式）：
| 旧（sqlx） | 新（connector） |
|---|---|
| `sqlx::query_as("SELECT ...固定...").bind(x).fetch_all(mysql)` | `mysql.fixed("SELECT ...固定...").bind(x).fetch_all_as::<T>()` |
| 无 bind 固定查询（t_department 全表、common_customer_codes） | `mysql.fixed("...").fetch_all_as::<T>()` |
| `format!("...IN ({ph})")` + 循环 bind（department_employee_ids / subordinate_ids / fetch_str_in / group_customer_codes / manager_customer_codes） | SQL 模板改 `&'static str` 字面量（占位处写 `{ph}`），调 `mysql.fixed_in(TEMPLATE, ids.len())` 拿 statement，再按原顺序逐个 bind |

> fixed_in 用法示例（department_employee_ids）：
> ```rust
> const DEPT_EMP_SQL: &str = "SELECT DISTINCT t.employee_id FROM t_employee t
>      INNER JOIN t_employee_department td
>         ON td.employee_id = t.employee_id AND td.deleted_flag = 0 AND td.service_status = 0
>      WHERE t.department_id IN ({ph}) OR td.department_id IN ({ph})";
> // 两个 {ph} 同 n 展开；bind 顺序保持旧双循环：for _ in 0..2 { for d in dept_ids { q = q.bind(d) } }
> ```
> 若 Task 3 的 fixed_in 不支持多 `{ph}` 标记 → 需 Task 3 补（多标记同 n 展开），不在 policy 内自行 format! 出 String 查询。

- [ ] **Step 3: lib.rs 挂 re-export 并编译 + 全量 policy 测试**

`pub use scope::{compute_scope, ScopeSets};`（ScopeSets 经 scope re-export 透 kernel）；`pub use cache::compute_scope_cached;` 或挂 `scope::compute_scope_cached`——**二选一保持调用方路径稳定即可，建议**：cache.rs 的 `compute_scope_cached` 在 scope.rs 里 `pub use crate::cache::compute_scope_cached;` 再导出，让 server 调用点只认 `dms_policy::scope::{compute_scope, compute_scope_cached}`。

Run: `cargo test -p dms-policy 2>&1 | Select-String "test result"`
Expected: 46 迁移测试 + 2 新单测（rules 1 + cache 1）全 passed。

- [ ] **Step 4: 提交**

```bash
git add crates/policy
git commit -m "policy: cache.rs RwLock+invalidate_scope + scope IO 搬入（7 类 DMS 表查询走 fixed/fixed_in 通道）"
```

---

### Task 5.5: server 调用点适配 + 删旧三文件（硬验收②）

**Files:**
- Modify: `crates/server/src/main.rs`（删 3 个 mod 声明，9 处调用点换路径）
- Modify: `crates/server/src/pipeline.rs`（2 行 use + 4 处 inject 调用路径）
- Delete: `crates/server/src/principal.rs`、`crates/server/src/scope.rs`、`crates/server/src/inject.rs`

**Interfaces:**
- Consumes: `dms_policy::{load_principal, list_roles, Principal, ScopeSets}`、`dms_policy::scope::{compute_scope, compute_scope_cached}`、`dms_policy::rules::{seed_rules, load_rules}`、`dms_policy::inject`
- Produces: server 源码零 `crate::inject/scope/principal` 残留

- [ ] **Step 1: main.rs 适配**

- 删 `mod inject;`（:10）、`mod principal;`（:14）、`mod scope;`（:15）。
- 调用点路径替换（**只换前缀，参数不动**）：
  - :45-46 `inject::seed_rules(pg)` / `inject::load_rules(pg)` → `dms_policy::rules::seed_rules(pg)` / `dms_policy::rules::load_rules(pg)`
  - :153/:165/:189/:347 `principal::load_principal(...)` → `dms_policy::load_principal(...)`
  - :166/:190 `scope::compute_scope_cached/compute_scope` → `dms_policy::scope::compute_scope_cached/compute_scope`
  - :168/:191 `inject::inject(...)` → `dms_policy::inject(...)`
  - :389 `principal::list_roles` → `dms_policy::list_roles`
- 若 Task 3 已把 AppState.mysql 换成 `ReadOnlyMySql`，调用零改动；若仍是 MySqlPool，说明 Task 3 未完，**停下升级**（policy 函数只吃 &ReadOnlyMySql）。

- [ ] **Step 2: pipeline.rs 适配**

- `use crate::principal::Principal;` → `use dms_policy::Principal;`
- `use crate::scope::ScopeSets;` → `use dms_policy::ScopeSets;`
- :535 `scope::compute_scope_cached` → `dms_policy::scope::compute_scope_cached`
- :553/:561/:636/:845 `inject::inject` → `dms_policy::inject`（两参兼容包装，本步先求编译绿；快照接线在 5.6）

- [ ] **Step 3: 删文件并全量构建**

```powershell
Remove-Item crates/server/src/principal.rs, crates/server/src/scope.rs, crates/server/src/inject.rs
cargo build 2>&1 | Select-Object -Last 5
```
Expected: Finished，无 error、无 `crate::inject` 等残留报错。

- [ ] **Step 4: 硬验收②——全 workspace 测试**

Run: `cargo test --workspace 2>&1 | Select-String "test result"`
Expected: policy 46+2 全过；server **111 passed**（157-46，一个不少）；kernel/connector 既有测试不减少。

- [ ] **Step 5: 残留与依赖方向检查**

```powershell
Select-String -Path crates/server/src/*.rs -Pattern "crate::inject|crate::scope::|crate::principal"   # 期望空
cargo tree -p dms-policy --prefix none 2>&1 | Select-String "dms-"                                   # 见 kernel/connector，不见 server
```
Expected: 第一条空；第二条无反向边。

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "server: 权限 IO 三文件删除，调用点切 dms_policy（46 单测迁出，其余 111 不动）"
```

---

### Task 5.6: 每请求 Arc 快照接线（RuleSet 并发铁律落地）

**Files:**
- Modify: `crates/server/src/pipeline.rs`（:535 入口 + :553/:561/:636/:845）
- Modify: `crates/server/src/main.rs`（:166-168、:190-191 CLI 路径）

**Interfaces:**
- Consumes: `dms_policy::rules::snapshot() -> Arc<RuleSet>`、`dms_kernel::inject(sql, sets, &rules)`（K6 三参）
- Produces: 生产请求路径全程单快照；两参 `dms_policy::inject` 仅剩测试/过渡用途

- [ ] **Step 1: pipeline.rs 问答主入口**

:535 算完 sets 后紧接着：
```rust
let rules = dms_policy::rules::snapshot(); // 每请求一次，全程使用（请求内规则一致）
```
:553/:561/:636 三处改 `dms_kernel::inject(&candidate, &sets, &rules)`。
:845 所在函数判定：若处于同一请求路径 → 给该函数加 `rules: &RuleSet` 形参从入口传入；若为独立路径（无请求上下文）→ 函数内自行 snapshot 一次并注释原因。**禁止**在同一请求内混用两种规则来源。

> 若 Task 3 已把这些点改成 newtype 管道（`inject(CheckedSql, &ScopeSets, &RuleSet)`），本步等价：把同一快照传进已有的 rules 形参，形状服从当时代码。

- [ ] **Step 2: main.rs CLI 路径**

:166（exec-sql）与 :190（scope demo）算完 sets 后各加一次 snapshot，:168/:191 改三参调用。

- [ ] **Step 3: 验证 + 回归**

Run: `cargo test --workspace 2>&1 | Select-String "test result"`
Expected: 全绿（46+2+111 及 kernel/connector 既有）。
Run: `cargo build --release 2>&1 | Select-Object -Last 2`（确认发布形态可编译）

- [ ] **Step 4: 提交**

```bash
git add crates/server
git commit -m "server: 问答/CLI 入口每请求 rules::snapshot() 一次并全程传参，RuleSet 热更新并发铁律落地"
```

---

### Task 5.7: 终验 + 收尾

- [ ] **Step 1: 全量验收清单**

```powershell
cargo test --workspace 2>&1 | Select-String "test result"          # 46 迁移 + 2 新增 + 111 server + kernel/connector 全绿
cargo build 2>&1 | Select-Object -Last 3                            # 全 workspace 编译
Select-String -Path crates/policy/src/*.rs -Pattern "MySqlPool"     # 期望空（policy 不碰裸 MySQL 池）
Select-String -Path crates/policy/src/scope.rs -Pattern "101|102|103|payment_customer_for"  # 魔数/字典 key 原样在
```
Expected: 全过；policy 零 MySqlPool；魔数与字典 key 逐字保留。

- [ ] **Step 2: 判官对拍（可选加强验收，需真库环境）**

连库判官 `tools/judge_scope.py` 仍是 scope 语义最终验收。本 plan 不主动连库跑（需用户提供环境）；若用户给库，跑判官逐题比对，结果集必须逐字一致。

- [ ] **Step 3: 提交收尾（如有残余改动）**

---

## 自检（已执行）
- **spec 覆盖**：迁移步 5 全部要点（principal 原样搬 / scope IO+魔数不动 / cache RwLock+invalidate / RuleSet RwLock<Arc> 热更新 / 46 单测硬验收 / 每请求 Arc 快照）✓；3.2 policy 目录树五件套（principal/scope/cache/rules/tests）✓；5.1 测试金字塔纯单测层「46 一个字不动」✓。
- **46 数核验**：scope.rs 实测 31（8 基础档+7 merge_ids+6 merge_cust+6 dept_tree+1 ScopeSets+3 e2e）、inject.rs 实测 15，合计 46 ✓；e2e 3 个即 scope.rs:665/676/688 所在测试，落 inject_e2e.rs ✓。
- **行为变化白名单**：仅 load_rules 热更新 + invalidate 新增两处，其余逐行对齐 ✓。
- **红线**：policy 不持裸 MySqlPool（仅 &ReadOnlyMySql + fixed/fixed_in 字面量通道）；动态 IN 只经占位符模板（模板 &'static str、展开只产 `?`、参数全 bind）✓；零新增依赖 ✓。
- **TDD 节奏**：5.2 先把 46 锁立起来再搬 IO（5.3-5.4），5.5 删源文件后 111 剩余单测兜底，5.6 快照接线后全量回归 ✓。
- **占位符扫描**：FixedStmt 确切方法名/fixed_in 多标记支持标注「以 Task 3 交付为准，缺则需 Task 3 补」，非 TBD 留白 ✓。

## 需 team-lead 裁决（阻塞前先确认）
1. **46 测试唯一归属**：本 plan 假设 Task 2 下沉纯算法时**不搬测试**（server 原文件测试保留至 5.5 随删除下线）。若 plan-t2 已把测试写进 kernel，则 5.2 改为「从 kernel 挪出」，请与 plan-t2 对齐。
2. **kernel 内部类型 pub 化**（K2/K3/K7：decide_base/BaseDecision/merge_*/expand_department_tree/dedup_*）：46 断言零改必须跨 crate 可见，kernel API 面将多出这些内部类型，请确认可接受（或 #[doc(hidden)] pub 折中）。
3. **kernel::inject 三参签名**（K6：rules: &RuleSet）：spec 2.1 既定形状；若 Task 2/3 只交付两参全局查表版，快照无法落地，需 Task 2/3 补。
4. **connector 占位符模板通道**（C3 fixed_in，含多 {ph} 同 n 展开）：scope 5 个动态 IN 查询的唯一红线合规路径，需 plan-t3 纳入交付；否则本任务 5.4 blocker。
5. **invalidate_scope 生产接线点**：本任务只交付接口+单测；生产侧无现成权限变更写入口（DMS 主系统改库，dms-ai 只读），建议接线留 Task 10 管理面，请确认。

## 备注（Windows 构建）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）
