# Task 3：三段 newtype 类型闸门 + 执行器签名改造（最有价值/最高危）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 引入 `RawSql/CheckedSql/ScopedSql` 三段 newtype 与 `ReadOnlyMySql`，把「SQL 必过校验 → 权限注入 → 只读执行」从手工按序调用变成**编译期强制**：`fetch()` 类型上只收 `&ScopedSql`，而 `ScopedSql` 全仓唯一产出点是 `inject()`，`inject()` 只吃 `check()` 产物。所有执行点（LLM 路径/direct 快路径/KPI 环比/语义缓存回放/CLI exec-sql/scope 判官/框架自查）被编译器逼进同一管道。

**Architecture:**
- kernel 新增 `sql/gate.rs`：三段 newtype（字段全私有）+ `check()`（AST 单SELECT + 只读红线 + 敏感列 + 占位符幻觉 + LIMIT 护栏内化）+ `inject()`（吃 `CheckedSql`，产 `ScopedSql`）+ `GuardError/PolicyError`（手写 enum，不引 thiserror）。
- connector 新增 `mysql.rs`：`ReadOnlyMySql`（pool 私有、唯一 `connect()` 强制 `SET SESSION TRANSACTION READ ONLY`）+ `RowSet` + `FixedStmt`（框架自查字面量通道）+ `ConnectorError`；`cell_to_json` 从 pipeline.rs:395-436 原样迁入。**crate 不导出/不 re-export 裸 `MySqlPool`**。
- server 全部执行点改签名：`pipeline::execute(&MySqlPool, &str)` → `ReadOnlyMySql::fetch(&ScopedSql)`；框架自查走 `fixed()`/`fixed().expand()`；`db::mysql_pool` 删除。

**Tech Stack:** Rust workspace、cargo、sqlx 0.8、sqlparser 0.53。零新增第三方依赖。

## Global Constraints

- **依赖前置**：本计划假设 Task 2 已完成且 kernel 已存在：`is_safe_select`/`ensure_limit`/`strip_literals_and_comments`（pub 纯函数）、inject AST 注入算法（`Binding/TableRule/OwnerKind/builtin_rules/rule_of/REGISTRY OnceLock`）、`ScopeSets`（含 `is_unrestricted`）。**不得在本任务重实现这些算法**；发现缺口一律标注「需在 Task 2 补」（见 Task 3.1 Step 0）。
- **行为 1:1**：错误文案（如 `查询超时（>30s）`）、`AskResult.sql` 字段内容、exec-sql JSON 输出字段、repair prompt 吃的 bad_sql 语义（未注入 SQL）全部保持现状。
- **fail-closed 语义不松**：inject 未登记表拒绝、非 SELECT 拒绝等现状拒答行为全部保留；过渡开关只松「非兜底路径」的失败处理方式（见 Task 3.3 Step 4）。
- **kernel 纯净**：kernel 不读环境变量、不做 IO；`DMS_INJECT_STRICT` 环境判定只在 server 调用侧。
- **TDD**：每个子任务先写失败测试（kernel 纯单测）再实现；connector 的 IO 面（connect/fetch/explain）无法纯单测，靠 CLI 冒烟 + python 判官对拍验收，plan 中如实标注。
- Windows 构建统一 MinGW 前缀（见文末备注），cargo 命令走 PowerShell。
- 每子任务独立提交，可编译可跑，随时可停。

---

### Task 3.1: kernel 三段 newtype + check() + inject()

**Files:**
- Create: `crates/kernel/src/sql/gate.rs`
- Modify: `crates/kernel/src/sql/mod.rs`（挂 `pub mod gate;` 与 re-export）
- Modify: `crates/kernel/src/lib.rs`（如需要）

**Interfaces:**
- Consumes（Task 2 产物，kernel 内）: `is_safe_select`、`ensure_limit`、`strip_literals_and_comments`、inject AST 算法（`rule_of`/`TableRule`/`Binding`/`OwnerKind`）、`ScopeSets`
- Produces:
  ```rust
  pub struct RawSql(String);                       // LLM 输出/模板/缓存回放/CLI 入参都经此包装
  pub struct CheckedSql { text: String, tables: Vec<String> }
  pub struct ScopedSql  { text: String, unrestricted: bool }

  pub struct GuardConfig { pub max_rows: usize }   // v1 仅此一项；红线词/敏感列保持算法内常量
  impl Default for GuardConfig { fn default() -> Self { Self { max_rows: 200 } } }

  pub enum GuardError { Parse(String), MultiStatement, NonSelect, ExecutableComment,
                        WriteOp(String), SensitiveColumn, Placeholder(String), Limit }
  pub enum PolicyError { Parse(String), NonSelect, UnregisteredTable(String), ViaHeaderMissing(String) }

  impl RawSql { pub fn new(s: impl Into<String>) -> Self; pub fn as_str(&self) -> &str; }
  impl CheckedSql { pub fn text(&self) -> &str; pub fn tables(&self) -> &[String]; }
  impl ScopedSql {
      /// pub 是刻意的：connector fetch 要读字符串。纪律见 Global Constraints 与 Task 3.4 grep 守护。
      pub fn wire(&self) -> &str;
      pub fn is_unrestricted(&self) -> bool;
  }

  pub fn check(raw: RawSql, d: &dyn Dialect, g: &GuardConfig) -> Result<CheckedSql, GuardError>;
  pub fn inject(sql: CheckedSql, sets: &ScopeSets) -> Result<ScopedSql, PolicyError>;
  ```

- [ ] **Step 0: 核对 Task 2 产物清单（缺什么补什么，不重实现）**

逐一 `use` 试编译确认 kernel 已有：
1. `is_safe_select(&str) -> anyhow::Result<()>`（或等价错误类型）、`ensure_limit(&str) -> String`（**必须 pub**，server 的 `corrector::schema_check` 调用点还要单用）、`strip_literals_and_comments`。
2. inject AST 算法全套 + `ScopeSets`。
3. **需在 Task 2 补①**：inject 模块暴露一个 `pub(crate) fn collect_table_names(ast) -> Vec<String>`（或把现有 `collect_names` 提为 pub(crate)）——`check()` 要用它填 `CheckedSql.tables`（含子查询/JOIN/反引号实表，排除 CTE 名，语义与 inject 的遍历一致）。
4. **需在 Task 2 补②**：`kernel::sql::dialect::{Dialect trait, MysqlDialect}`。spec 2.4 有五方法，本步 check() 只用 `parser()`；若 Task 2 未建，本任务建**最小子集**（`fn name(&self) -> &'static str; fn parser(&self) -> &(dyn sqlparser::dialect::Dialect + Send + Sync);`），其余三方法（classify_column/time_fn/schema_probe）后续任务按需扩，不预造（YAGNI）。这是契约补建不是算法重实现，允许在 Task 3 内做，但须先查 Task 2 plan 是否已覆盖避免撞车。
5. 现状 inject 签名是 `inject(&str, &ScopeSets) -> anyhow::Result<String>`（server 侧字符串进字符串出）。Task 2 若已下沉，其 kernel 形态可能是字符串版——本步的 `gate::inject` 是**薄包装**：吃 `CheckedSql` → 调已有字符串算法 → 包 `ScopedSql`。AST 算法本体一行不改。

- [ ] **Step 1: 先写失败测试（gate.rs `#[cfg(test)]`，全部纯单测无需库）**

测试清单（行为锁 = 现状 is_safe_select/ensure_limit/inject 全部语义）：

```rust
// —— newtype 闸门 ——
check_appends_limit_when_missing        // check(RawSql("SELECT * FROM t_sales_order")).text 以 "LIMIT 200" 结尾
check_keeps_existing_limit              // "SELECT * FROM t LIMIT 5" 原样（不双 LIMIT）
check_limit_literal_not_fooled          // WHERE remark='limit' 仍追加 LIMIT 200
check_rejects_multi_statement           // GuardError::MultiStatement
check_rejects_non_select                // UPDATE/DELETE/DROP → GuardError::NonSelect
check_rejects_executable_comment        // "/*!" "/*+" → ExecutableComment
check_rejects_write_op_keywords         // delete/drop/update 词边界命中；deleted_flag/created_time 列名不误伤
check_rejects_sensitive_column          // login_pwd/password → SensitiveColumn
check_rejects_placeholder               // '__ORDER_CODE__' / 'X_PLACEHOLDER'
check_allows_literal_keywords           // LIKE '%update %'、REPLACE() 函数合法
check_extracts_tables                   // JOIN+子查询+反引号：tables 含全部实表；CTE 名不在 tables
check_then_inject_happy_path            // 受限用户：check→inject→wire() 含 owner_manager IN (...) 且含 LIMIT
inject_unrestricted_passthrough         // 全空 sets：wire() == check 后原文；is_unrestricted()==true
inject_scoped_or_condition              // ids+codes+cust 三段 or 括号（搬 inject.rs 现有断言）
inject_sentinel_rejects                 // [-1]/['-1'] 哨兵条件照常注入
inject_rejects_unregistered_table       // PolicyError::UnregisteredTable（fail-closed）
inject_via_exists_halfjoin              // 明细独查 EXISTS 借头表；头表在场跳过
inject_backtick_table_injected          // 反引号表名照常命中
inject_rejects_non_select_via_check     // check 层已拒，inject 不可能收到（类型保证，注释说明即可）
wire_is_only_reader                     // ScopedSql 无 pub 字段、唯一读取口 wire()（反射式注释断言，靠签名 review）
```

> 其中后 6 条 = inject.rs:379-547 现有 15 个测试的 newtype 适配版（`inject(sql,&sets)` → `inject(check(RawSql::new(sql)).unwrap(),&sets).unwrap().wire()`），断言文本原样保留。**pipeline.rs:943-1037 中锁 is_safe_select/ensure_limit 的 12 个测试若 Task 2 未迁入 kernel，本步顺带迁移**（它们锁的正是 check 的行为）；time_tokens/number_tokens/civil_from_days 的测试留在 server 不动。

- [ ] **Step 2: 运行测试确认全部失败**

Run: `cargo test -p dms-kernel gate 2>&1 | Select-Object -Last 5`
Expected: 编译失败（`gate` 模块不存在）——确认测试先行。

- [ ] **Step 3: 实现 gate.rs**

骨架（核心逻辑全部委托 Task 2 已有纯函数，本文件只做包装与状态推进）：

```rust
//! 三段 SQL newtype 类型闸门：只读+权限不可绕过的编译期保证。
//! ScopedSql 全仓唯一产出点是本模块 inject()；inject 只吃 check() 产物。

use super::dialect::Dialect;
use super::{ensure_limit, is_safe_select};         // Task 2 产物
use crate::inject_ast as inj;                       // Task 2 下沉的 AST 注入算法（名以 Task 2 实际为准）
use crate::scope::ScopeSets;                        // Task 2 下沉位置以实际为准

pub struct RawSql(String);
pub struct CheckedSql { text: String, tables: Vec<String> }
pub struct ScopedSql { text: String, unrestricted: bool }

pub fn check(raw: RawSql, d: &dyn Dialect, g: &GuardConfig) -> Result<CheckedSql, GuardError> {
    let limited = ensure_limit(raw.as_str());       // LIMIT 护栏内化：先于 AST 校验（现状 pipeline.rs:627 顺序）
    is_safe_select(&limited).map_err(|e| map_guard_err(&e))?;   // 逐条映射 anyhow 文案 → GuardError 变体
    let tables = /* parse + collect_table_names（Step 0 补①） */;
    Ok(CheckedSql { text: limited, tables })
}

pub fn inject(sql: CheckedSql, sets: &ScopeSets) -> Result<ScopedSql, PolicyError> {
    let unrestricted = sets.is_unrestricted();
    let text = inj::inject_str(&sql.text, sets).map_err(|e| map_policy_err(&e))?;  // 算法本体不动
    Ok(ScopedSql { text, unrestricted })
}
```

要点：
- `map_guard_err`/`map_policy_err`：按 anyhow 错误文案前缀映射到强类型变体（`未在权限档案登记` → UnregisteredTable 等）。**Display 文案与现状 anyhow 文案逐字一致**（repair prompt、判官脚本吃这些文本）。
- `GuardError/PolicyError` 手写 `impl Display + std::error::Error`，不引 thiserror（spec 4.2）。
- `CheckedSql.tables` 的收集复用 Step 0 补①的 pub(crate) 遍历，排除 CTE 名（与 inject 的 ctes 豁免同一语义）。

- [ ] **Step 4: 测试全绿 + 全仓回归**

Run: `cargo test -p dms-kernel 2>&1 | Select-Object -Last 5`
Expected: gate 全部新测试 + kernel 既有测试全过。
Run: `cargo build 2>&1 | Select-Object -Last 3`
Expected: server 不受影响（还没用它），编译通过。

- [ ] **Step 5: 提交**

```bash
git add crates/kernel
git commit -m "kernel: 三段 SQL newtype 闸门（RawSql/CheckedSql/ScopedSql + check/inject），纯单测锁定现状全部护栏语义"
```

---

### Task 3.2: connector ReadOnlyMySql + RowSet + FixedStmt + cell_to_json 迁入

**Files:**
- Create: `crates/connector/src/mysql.rs`
- Create: `crates/connector/src/error.rs`
- Modify: `crates/connector/src/lib.rs`

**Interfaces:**
- Consumes: `dms_kernel::sql::gate::{ScopedSql, RawSql, CheckedSql, check, inject, GuardConfig}`、`dms_kernel::scope::ScopeSets`
- Produces:
  ```rust
  pub struct ReadOnlyMySql { pool: sqlx::MySqlPool }   // 私有，全仓唯一造池入口
  pub struct RowSet { pub columns: Vec<String>, pub rows: Vec<Vec<serde_json::Value>> }
  pub enum ConnectorError { Timeout(u64), Db(String), Connect(String) }   // 手写 enum
  pub struct FixedStmt<'a> { /* pool: &'a MySqlPool, sql: String */ }

  impl ReadOnlyMySql {
      pub async fn connect(url: &str, max_conn: u32) -> Result<Self, ConnectorError>;  // after_connect 强制 READ ONLY
      pub async fn fetch(&self, sql: &ScopedSql, max: usize, t: std::time::Duration) -> Result<RowSet, ConnectorError>;
      pub async fn explain(&self, sql: &ScopedSql, t: std::time::Duration) -> Result<Option<String>, ConnectorError>;
      pub fn fixed(&self, sql: &'static str) -> FixedStmt<'_>;
  }
  impl<'a> FixedStmt<'a> {
      /// 可信占位符展开：模板含 `{in}` 标记处展开为 n 个 `?`（纯标点，数据仍走 bind）。
      /// 模板本身是 &'static str——LLM 拼接串类型上进不来。
      pub fn expand(self, n: usize) -> Self;
      // sqlx::query/query_as 的薄包装：.fetch_all::<T>() / .fetch_optional::<T>() / .bind(...)，
      // 签名对齐 sqlx 用法，server 侧改动最小。bind 泛型转发。
  }
  ```
  ** crate 不 re-export `sqlx::MySqlPool`；`lib.rs` 只 `pub use mysql::{ReadOnlyMySql, RowSet, FixedStmt}` 与 `error::ConnectorError`。**

- [ ] **Step 1: 先写失败测试**

纯单测可锁的（mysql.rs `#[cfg(test)]`）：
```rust
expand_in_marks_single        // fixed("... IN ({in})").expand(3) 内部 SQL == "... IN (?,?,?)"
expand_in_marks_multi         // 两处 {in} 各展开（scope.rs department_employee_ids 双 IN 形态）
expand_zero_marks_safe        // expand(0) → IN (NULL)（恒假防语法错；调用侧本已守非空，双保险）
expand_leaves_other_text      // 模板其余字节不动
connector_error_display       // Timeout(30).to_string() == "查询超时（>30s）"（现状文案兼容，repair/判官吃这文本）
```
IO 面无法纯单测，**如实标注**：connect 的 READ ONLY 强制、fetch 的 max 截断、explain 的超时返回 None——靠 Task 3.5 的 exec-sql 冒烟 + health 端点 + python 判官验收。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p dms-connector 2>&1 | Select-Object -Last 5`
Expected: 编译失败（模块不存在）。

- [ ] **Step 3: 实现**

- `connect`：搬 db.rs:50-62 的 `MySqlPoolOptions::new().max_connections(n).after_connect(SET SESSION TRANSACTION READ ONLY)`，一行不改，仅错误类型换 ConnectorError。
- `fetch`：搬 pipeline.rs:369-393 `execute` 本体（timeout + fetch_all + 首行取列名 + 逐行 cell_to_json + 满 max 即 break），`sql.wire()` 喂 `sqlx::query`。超时错误映射 `ConnectorError::Timeout(secs)`，Display = `查询超时（>{secs}s）`。
- `explain`：搬 pipeline.rs:354-366 `explain_check` 本体（`format!("EXPLAIN {}", sql.wire())`），语义 1:1：DB 明确报错 → `Ok(Some(错误文案))`；超时/连接故障 → `Ok(None)`（优化纯增益，抖动不触发改写）。
- `cell_to_json`：pipeline.rs:395-436 **原样迁入**（含 `use serde_json::Value` 与五个类型分派分支），改 `pub(crate)`。逻辑零改动——它锁的是线上 JSON 字节格式，靠回归对拍验收。
- `RowSet`：就是现状 `execute` 的返回元组结构化；**truncated 判定仍留调用侧**（`rows.len() >= max`，与现状 `row_count >= MAX_ROWS` 同语义，不内收）。
- `FixedStmt`：存 `&'a MySqlPool + String`；`expand(n)` 把每个 `{in}` 替换为 `?,?,...`（n=0 → `NULL`）；`fetch_all/fetch_optional/execute` 泛型转发 sqlx；`bind` 链式转发。

- [ ] **Step 4: 测试 + 构建**

Run: `cargo test -p dms-connector 2>&1 | Select-Object -Last 5`
Expected: 5 个纯测试全过。
Run: `cargo tree -p dms-connector --prefix none 2>&1 | Select-String "dms-kernel"`
Expected: 有边（connector→kernel）。

- [ ] **Step 5: 提交**

```bash
git add crates/connector
git commit -m "connector: ReadOnlyMySql 红线载体（pool 私有+强制 READ ONLY+fetch 只收 ScopedSql）+ FixedStmt 字面量通道 + cell_to_json 迁入"
```

---

### Task 3.3: server 执行点改造（pipeline 热路径 + CLI），编译器驱动逐个收编

**Files:**
- Modify: `crates/server/src/pipeline.rs`（LLM 主循环、direct 快路径、KPI 环比、语义缓存回放、explain 调用）
- Modify: `crates/server/src/main.rs`（exec-sql、scope 判官）
- Modify: `crates/server/Cargo.toml`（确认 dms-kernel/dms-connector path 依赖已存在——Task 1 已加）
- Delete（随改造完成）: `pipeline::execute`、`pipeline::explain_check`、`pipeline::is_safe_select`、`pipeline::ensure_limit`、`pipeline::cell_to_json`（若 Task 2 已下沉为 re-export，则删 re-export）

**Interfaces:**
- Consumes: `dms_kernel::sql::gate::{RawSql, check, inject, GuardConfig}`、`dms_kernel::sql::dialect::MysqlDialect`、`dms_connector::{ReadOnlyMySql, RowSet, ConnectorError}`
- Produces: 所有 AskResult 产出路径的 SQL 全部经 `check→inject→fetch`；`scoped.wire()` 填 `AskResult.sql`/日志/exec-sql JSON（字节级不变）

- [ ] **Step 1: 改 AppState 与函数签名（先让编译器列出所有点）**

`main.rs`：`AppState.mysql: MySqlPool` → `ReadOnlyMySql`；`db::mysql_pool(&cfg.mysql_url)` → `ReadOnlyMySql::connect(&cfg.mysql_url, 10).await?`。
`pipeline.rs`：`ask/ask_single/try_semantic_cache` 的 `mysql: &MySqlPool` → `&ReadOnlyMySql`。
Run: `cargo build 2>&1 | Select-String "error"`
Expected: 一批类型不匹配 error——**这就是编译器给出的执行点清单，逐个按下步收编**。

- [ ] **Step 2: LLM 主循环（pipeline.rs:626-714）**

现状：`ensure_limit → is_safe_select → inject → explain_check → execute`。
改造：
```rust
for attempt in 0..2 {
    let checked = match check(RawSql::new(sql.clone()), &MysqlDialect, &GuardConfig::default()) {
        Ok(c) => c,
        Err(e) => { /* 同现状：attempt==0 → repair+continue；否则 bail，文案保持 "SQL 安全校验未通过: {e}" */ }
    };
    let scoped = inject(checked, &sets)?;              // inject fail=bail（现状语义）；开关见 Step 4
    if attempt == 0 {
        if let Some(err) = mysql.explain(&scoped, Duration::from_secs(EXPLAIN_TIMEOUT_SECS)).await? {
            /* 同现状：log + repair + continue */
        }
    }
    match mysql.fetch(&scoped, MAX_ROWS, Duration::from_secs(EXEC_TIMEOUT_SECS)).await {
        Ok(rs) => { /* AskResult { sql: scoped.wire().to_string(), columns: rs.columns, rows: rs.rows, ... } */ }
        Err(e) => { /* 同现状：attempt==0 → repair；否则 log_failure + 复盘 spawn + bail */ }
    }
}
```
不变量核对（逐条对现状）：few-shot 回写 bind 的 candidate = **check 后 inject 前**文本（`checked.text()`，现状是 ensure_limit 后未注入串 ✓）；repair 的 bad_sql = 未注入串 ✓；`log_failure("zero-rows"/"exec-error")` 记 injected = `scoped.wire()` ✓；ConnectorError Display 与现状 anyhow 文案一致（repair prompt 无感）✓。

- [ ] **Step 3: direct 快路径 + KPI 环比 + 语义缓存回放（pipeline.rs:546-584, 843-846）**

- direct 主 SQL：
  ```rust
  if let Ok(checked) = check(RawSql::new(hit.sql), &MysqlDialect, &GuardConfig::default()) {
      let scoped = inject(checked, &sets)?;                 // inject fail 仍 bail（现状 :553 语义）
      if let Ok(rs) = mysql.fetch(&scoped, MAX_ROWS, ...).await { /* ... */ }
      // fetch fail → 静默回落 LLM（现状 :582 语义）
  }
  // check fail → 跳过 direct（现状 is_safe_select fail 语义），加 warn trace 可观测
  ```
  ⚠️ **行为变化点（记录进提交说明）**：direct SQL 现在会被 `check` 追加 `LIMIT 200`（现状 direct 漏 ensure_limit——历史绕过点①收口）。try_direct/try_compose 产物多为聚合/分组（行数 ≪200），预期无感；回归对拍确认。
- KPI 环比 prev_sql（现状 :558-568，**只 inject 不过 check**——历史绕过点②）：
  ```rust
  let prev_scoped = check(RawSql::new(prev_sql.clone()), &MysqlDialect, &GuardConfig::default())
      .and_then(|c| inject(c, &sets).map_err(|e| /* PolicyError→统一 */ e.into()));
  if let (Some(cur), Ok(prev)) = (rows.first()..., prev_scoped) {
      if let Ok(prs) = mysql.fetch(&prev, MAX_ROWS, ...).await { /* patch_kpi_delta */ }
  }
  // check/inject/fetch 任一 fail → warn trace + 跳过环比（现状 inject fail 静默跳过语义，可观测性增强）
  ```
- 语义缓存回放：三关现状已齐（ensure_limit+is_safe_select+inject+execute），改走闸门后失败语义**保持 `.ok()?` 回落**，但每个失败点补 `tracing::warn!(target:"sql_gate", route="semantic-cache", ...)`（现状静默，观测增强）。

- [ ] **Step 4: 过渡开关 DMS_INJECT_STRICT（spec 5.2 步 3 验收要求）**

**设计**（kernel 不读 env，判定只在 server 侧）：
```rust
// pipeline.rs 顶部
fn inject_strict() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("DMS_INJECT_STRICT").map(|v| v != "0").unwrap_or(true))
}
```
- `=1`（默认，生产）：inject fail-closed 一律按现状语义（direct/LLM 路径 bail；prev/缓存路径本就回落）。
- `=0`（观察期）：**非兜底路径**（direct 主 SQL）的 inject fail → `tracing::warn!(target:"sql_gate", strict_bypass=true, ...)` + 回落 LLM 而非 bail。**LLM 兜底路径无处可回，永远 bail**（fail-closed 铁律不动摇，开关只影响「还有得退」的分支）。
- 观察期用法（写进提交说明与运维备注）：生产跑一周，`grep sql_gate` 日志；零误伤后开关保持默认 1 长期存在（不删，成本一行）。
- prev/缓存路径本就回落，开关不影响它们——它们的 warn trace 恒定开启。

- [ ] **Step 5: CLI exec-sql 与 scope 判官（main.rs:161-209）**

exec-sql（评测判官，三道防线一个不少的注释保持）：
```rust
let checked = check(RawSql::new(args[3].clone()), &MysqlDialect, &GuardConfig::default())?;
let scoped = inject(checked, &sets)?;
let rs = mysql.fetch(&scoped, 200, Duration::from_secs(30)).await?;
// JSON 输出字段一字不动：sql=scoped.wire(), columns, rows, row_count, elapsed_ms
```
⚠️ **行为变化点**：gold SQL 无 LIMIT 时现追加 `LIMIT 200`（历史绕过点③收口）。evaluation.py 38 题 exec-only 对拍验收；若某 gold 结果集 >200 行，给 gold SQL 补 LIMIT 而非绕过闸门。

scope 判官（:191-194）：demo SQL 是编译期字面量，走：
```rust
let demo = inject(check(RawSql::new("SELECT COUNT(*) AS cnt FROM t_sales_order so WHERE so.deleted_flag = 0"), ...)?, &sets)?;
// 输出 demo_sql = demo.wire()；不执行（现状就不执行）
```

- [ ] **Step 6: 删旧函数，验证全仓**

`pipeline::execute/explain_check/cell_to_json` 删除（无调用者）；`is_safe_select/ensure_limit` 的 server 侧壳删除（调用点已进 check；`corrector::schema_check` 处改 `dms_kernel::sql::ensure_limit(&sql)`）。
Run: `cargo build 2>&1 | Select-Object -Last 3` —— Expected: 通过，无 unused import warning。
Run: `cargo test 2>&1 | Select-Object -Last 8` —— Expected: 全绿（server 现存测试里锁 is_safe_select/ensure_limit 的已迁 kernel；graph/civil/main 测试不受影响）。

- [ ] **Step 7: 提交**

```bash
git add crates/server
git commit -m "server: 全部业务执行点收编 check→inject→fetch 类型闸门；direct/exec-sql 补 LIMIT 护栏；KPI 环比过 check；DMS_INJECT_STRICT 过渡开关"
```

---

### Task 3.4: 框架自查全部走 fixed 通道 + 裸池封堵 + grep 守护

**Files:**
- Modify: `crates/server/src/db.rs`（删 `mysql_pool`）
- Modify: `crates/server/src/principal.rs`、`scope.rs`、`wework.rs`（签名 `&MySqlPool` → `&ReadOnlyMySql`，查询走 `fixed()`）
- Modify: `crates/server/src/meta.rs`（sync_schema、autodiscover 的 MySQL 查询）
- Modify: `crates/server/src/graph.rs`（sync 的聚合边查询）
- Modify: `crates/server/src/main.rs`（health、graph sync 定时任务、meta sync/autodiscover 子命令）
- Create: `scripts/check-readonly-gate.ps1`

**Interfaces:**
- Consumes: `ReadOnlyMySql::{fixed, fetch}`、`FixedStmt::{bind, expand, fetch_all, fetch_optional, execute}`
- Produces: server 全文件零 `MySqlPool` 出现；grep 守护脚本可重复执行

**框架自查查询分类处置**（逐点，全部从「裸 pool + sqlx::query」收编）：

| 点位 | 形态 | 通道 |
|---|---|---|
| principal.rs:32-49, 83-91 | 静态 SQL + bind | `fixed("...").bind(x).fetch_*()` |
| wework.rs:92-97 | 静态 + bind | 同上 |
| scope.rs user_departments/self_and_children/common_customer_codes | 静态 + bind | 同上 |
| scope.rs department_employee_ids/subordinate_ids/group_customer_codes/manager_customer_codes/fetch_str_in | 静态模板 + **动态个数 `?`** | `fixed("... IN ({in}) ...").expand(n).bind(...)`（模板 `&'static str`，动态的只是标点） |
| meta.rs sync_schema information_schema ×2 | 静态无 bind | `fixed("...").fetch_all()` |
| graph.rs sync 聚合边 | 静态无 bind | 同上 |
| main.rs health ×2（`SELECT 1` / `SELECT @@session.transaction_read_only`） | 静态无 bind | 同上 |
| meta.rs autodiscover DISTINCT 抽样 | **标识符动态**（表/列名来自 PG 注册表，运行时拼） | **不开后门**：`RawSql::new(抽样SQL) → check → inject(unrestricted sets) → fetch`。CLI 工具内部查询同样过闸，类型统一；抽的是码列无敏感列，check 必过 |

- [ ] **Step 1: scope.rs 改造（最大的一块）**

10 个函数签名统一 `mysql: &ReadOnlyMySql`；动态 IN 处模板改 `&'static str` 常量 + `.expand(ids.len())`，bind 循环不变。注意 `department_employee_ids` 的双 IN（模板含两个 `{in}`，bind 顺序=展开后占位符顺序，与现状 `for _ in 0..2 { for d in dept_ids }` 一致——expand 保序所以 bind 顺序不变）。
⚠️ 语义红线：本步**只改通道不改 SQL 文本**——每个查询的 SQL 字符串逐字保持（46 权限单测/policy 语义的地基，Task 5 才迁）。

- [ ] **Step 2: 其余框架自查点改造 + 删 db::mysql_pool**

principal/wework/meta sync_schema/graph sync/health 按上表改造。`db.rs` 删 `mysql_pool`（pg_pool 保留——PG 不受只读闸门约束）。main.rs 各子命令与定时任务的 `db::mysql_pool(...)` 全部换 `ReadOnlyMySql::connect(...)`。

- [ ] **Step 3: autodiscover 抽样查询过闸**

抽样 SQL 构造处（`SELECT DISTINCT CAST({col} AS CHAR) FROM {table} LIMIT 61` 形态）：
```rust
let scoped = inject(check(RawSql::new(sample_sql), &MysqlDialect, &GuardConfig::default())?,
                    &ScopeSets::default())?;   // 空 sets = unrestricted，CLI 管理员工具语境
let rs = mysql.fetch(&scoped, 61, Duration::from_secs(30)).await?;
```
RowSet 读出替换现状 `query_as` 取值（row[0] 取字符串）。fail → 该列跳过 + warn（现状某列抽样失败本就不该拖死全量 autodiscover）。

- [ ] **Step 4: grep 守护脚本（替代 compile-fail 测试的纪律载体）**

`ScopedSql::wire()` 必须 pub（connector 要读），真正的封堵 = 「connector 不导出裸池」约定 + 本脚本 + code review。试过 rustdoc `compile_fail` doctest 不可行（doctest 在 connector 包内编译，模拟不了下游视角；引 trybuild 违反零新增依赖红线）——故用脚本：

```powershell
# scripts/check-readonly-gate.ps1 —— 只读红线守护：以下模式只允许出现在 crates/connector
$bad = Select-String -Path "crates\server\src\*.rs","crates\agent\src\*.rs","crates\policy\src\*.rs","crates\semantic\src\*.rs" `
       -Pattern "MySqlPool","MySqlPoolOptions","sqlx::mysql","\.wire\(\)" 2>$null
if ($bad) { $bad | ForEach-Object { "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" }; exit 1 }
"OK: 裸 MySqlPool / wire() 未泄漏出 connector"
```
（`.wire()` 例外：server 取注入后 SQL 字符串填 AskResult/日志是**读**，合法——脚本中对 `.wire()` 的命中需人工核对其用途仅为读字符串展示，不得喂给任何执行通道。脚本对 wire 只告警不失败：拆两轮，MySqlPool 系 exit 1，wire 系打印提醒。）

- [ ] **Step 5: 验证**

Run: `cargo build 2>&1 | Select-Object -Last 3` —— Expected: 通过。
Run: `powershell -File scripts\check-readonly-gate.ps1` —— Expected: `OK`（wire 提醒逐条人工核对）。
Run: `cargo test 2>&1 | Select-Object -Last 8` —— Expected: 全绿。
Run: `cargo tree -p dms-ai-server --prefix none 2>&1 | Select-String "dms-"` —— Expected: kernel/connector 在树中。

- [ ] **Step 6: 提交**

```bash
git add crates/server crates/connector scripts
git commit -m "server: 框架自查全走 fixed 字面量通道（scope 动态 IN 用 expand 展开）；autodiscover 抽样过闸；删裸池出口 + grep 守护脚本"
```

---

### Task 3.5: 全量验收（回归对拍 + 判官冒烟）

- [ ] **Step 1: 全量测试**

Run: `cargo test 2>&1 | Select-Object -Last 10`
Expected: kernel（含 gate 新测试）+ connector + server 全绿；**46 权限相关断言（inject/scope 系）一个字未改地通过**。

- [ ] **Step 2: exec-sql 冒烟（三关合一实证）**

Run（PowerShell）:
```
cargo run -q -- exec-sql <受限账号> "SELECT COUNT(*) AS cnt FROM t_sales_order so WHERE so.deleted_flag = 0" <role_code>
```
Expected: JSON 输出 `sql` 字段含注入条件且带 LIMIT；受限账号查 `t_role_data_scope` 直接报「未在权限档案登记」。

- [ ] **Step 3: health 端点实证 READ ONLY**

起服务后 GET /api/health，Expected: `mysql.session_read_only == true`（after_connect 强制未丢）。

- [ ] **Step 4: 回归题集 + 评测对拍（有库环境时）**

Run: `python tools\regression.py`（51 题）与 `python tools\evaluation.py`（38 exec-only）
Expected: 结果集与基线一致（比结果集不比 SQL 文本）。**特别关注**：direct 命中题（LIMIT 200 追加）、gold SQL 结果集接近 200 行的 exec-only 题。

- [ ] **Step 5: 提交验收记录 + 收尾**

若对拍有差异：逐题判定是「LIMIT 收口预期内」还是真回归；预期内差异更新基线并写进提交说明。
```bash
git commit -m "验收: Task 3 回归/评测对拍记录；sql_gate warn 观察项留档" --allow-empty
```

---

## 历史绕过点清单（本任务逐条收口）

| # | 点位 | 现状 | 处置 |
|---|---|---|---|
| ① | direct 快路径（pipeline.rs:546-584） | `is_safe_select→inject→execute`，**漏 ensure_limit** | check 内化 LIMIT，自动收口；行为变化=direct SQL 追加 LIMIT 200，回归对拍确认 |
| ② | KPI 环比 prev_sql（pipeline.rs:558-568） | **只 inject 不过任何校验**直接 execute | 过 check→inject→fetch；失败=warn+跳过环比（与现状 inject fail 静默跳过同语义） |
| ③ | CLI exec-sql（main.rs:166-170） | `is_safe_select→inject→execute`，**漏 ensure_limit** | 同①；gold SQL >200 行的补 LIMIT，不开绕过 |
| ④ | autodiscover DISTINCT 抽样（meta.rs:1226+） | 标识符动态拼接，裸 pool 执行 | 走 RawSql→check→inject(unrestricted)→fetch 全管道，不开后门 |
| ⑤ | explain_check（pipeline.rs:354-366） | 注入后 SQL 拼 EXPLAIN 直接 `sqlx::query`，旁路执行通道 | `ReadOnlyMySql::explain(&ScopedSql)`，语义 1:1（超时/抖动=None 跳过预检） |
| ⑥ | 框架自查 10+ 处（principal/scope/wework/meta sync/graph sync/health） | 裸 `MySqlPool` 任意查询 | fixed() 字面量通道 + expand() 标点展开；pool 私有化后类型上不可达 |
| ⑦ | 语义缓存回放（pipeline.rs:843-846） | 三关已齐但失败全静默 `.ok()?` | 闸门化 + 每失败点 warn trace（route=semantic-cache），回落语义不变 |

**单列说明——图快路径（pipeline.rs:878-920）不是绕过点**：try_graph 只查 PG（AGE cypher），类型上够不到 MySQL 闸门；其权限护栏是 `sets.is_unrestricted()` 门禁（pipeline.rs:538，限权用户回落 LLM 走注入）。本任务对它**零改动**；cypher 侧的 `esc()` 拼接与权限收敛属 Task 9（Answerer 化）范畴，届时统一审。

## 需 team-lead 裁决点

1. **scope.rs 动态 IN 与 spec `fixed(&'static str)` 的冲突**：spec 字面量通道容不下「占位符个数动态」的权限查询（scope.rs 5 处）。本 plan 方案 = `FixedStmt::expand(n)`（模板仍 `&'static str`，`{in}` 标记展开为纯标点 `?`，数据全走 bind），保住「LLM 拼接串类型上进不来」的设计意图。备选 = 改 FIND_IN_SET（SQL 全静态但丢索引）或逐 id 单查（性能回归）。**请裁决：expand 方案是否认可。**
2. **autodiscover 抽样走 unrestricted 全管道**：CLI 管理员工具以空 ScopeSets（=unrestricted）过 inject。语义自洽（工具语境无用户身份），但意味着「unrestricted 注入」存在第二个调用场景。备选 = 给 connector 开 `trusted_dyn` 后门（不推荐，破坏闸门纯度）。
3. **DMS_INJECT_STRICT 长期留存**：观察期后开关保留（默认 1）还是删除？plan 按保留写（成本一行，回退保险丝）。
4. **CheckedSql.tables 的消费**：v1 仅 trace/调试展示用，inject 内部仍自行 AST 遍历（与 Task 2 算法零冲突）。若认为 v1 无消费者属过度设计，可裁掉 tables 字段。

## 自检

- **spec 覆盖**：迁移步 3 全项（三段 newtype ✓、执行器签名 ✓、fail-closed 先 warn 过渡 ✓）；2.1/2.2 签名对齐（inject 的 RuleSet 参数留 Task 5 引入，v1 经 kernel OnceLock 注册表，与现状语义 1:1）；5.3 风险③（本步无迁移无关）与「编译器报所有执行点逐个改」门禁 ✓。
- **占位符扫描**：无 TBD；关键代码骨架完整给出。
- **类型一致**：`check(raw, &dyn Dialect, &GuardConfig)` 对齐 spec 2.1；`fetch(&ScopedSql, max, Duration)` 对齐 spec 2.2。
- **零新增依赖**：全程无新 crate；compile-fail 用 grep 脚本替代的理由已写明。
- **TDD 诚实标注**：kernel 闸门全纯单测；connector IO 面与 cell_to_json 靠判官对拍，未伪装成单测覆盖。

## 备注（Windows 构建）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）
