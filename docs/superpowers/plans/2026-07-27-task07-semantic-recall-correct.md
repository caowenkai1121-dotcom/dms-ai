# Task 7：meta 解体② registry/ingest/recall 拆 + 五校正器 trait 链 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 meta.rs 的运行时资产按生命周期拆进 `dms-semantic`（registry/ingest/recall），把 corrector.rs 解体为 `correct/` 五文件并定义 `Corrector` trait 有序链，消灭 pipeline.rs:597-624 五段三种签名、各自 log_correction、各自 `chars().take(120)` 的样板。

**Architecture:** 对应 spec 迁移步 7 与 3.1/3.3 节。`dms-semantic` 新增四个模块：`registry/`（六注册表类型 + PG 读写 + element 统一体同步 + Registry 门面）、`ingest/`（information_schema ETL + autodiscover 拆 probe/match/register 三段）、`recall/`（六种召回统一 RecallCtx 入参 + map_filter 净化 + 卡片渲染）、`correct/`（Corrector trait + CorrectCtx + run_chain + 五校正器各一文件）。完成后 meta.rs / corrector.rs 两个上帝文件消失。

**Tech Stack:** Rust workspace、sqlx(PG/MySQL)、sqlparser、tracing。零新增第三方依赖；异步 trait 手写 BoxFut，不引 async-trait。

## Global Constraints

- **算法一字不改**：召回与校正的 SQL 文本、命中逻辑、阈值、门控、卡片文案全部按行号原样搬运，只改位置与统一签名。验收 = 原 corrector.rs 33 个 + meta.rs 13 个单测（合计 46，其中 4 个随 Task 2 在 kernel）全部原样通过。
- **命中逻辑不合并**：`correct_agg`（自带 `question.contains` 循环，无净化）与 `correct_caliber`（走 `recall_metric_hits`，有 map_filter 净化）是**两套不同命中**，禁止顺手统一——各按原样保留。
- **顺序不变**：校正链顺序 = 原 pipeline 605→621 的 GroupBy→Agg→Caliber→Value；每站输入 = 上站输出。`correction_log.kind` 取值保持 `groupby-fix/agg-fix/caliber-fix/value-fix/schema-fix` 不变（同错累计升格分析依赖该列）。
- **纯核函数签名不动**：`fix_group_by / link_values_with / normalize_agg / add_scope_filter / parse_agg_rule` 签名保持原样（33 个单测直接调用它们，测试一行不改是硬约束）。纯核内 `MySqlDialect{}` 硬编码 v1 保留（spec：v1 只 MySQL，不预造多数据源）。
- **依赖红线**：不新增第三方 crate；不开 sqlx 新 feature（唯一例外：Task 7.0 给 semantic 的 sqlx 补 `mysql`——Task 1 建 Cargo.toml 漏配，ingest 必须读 MySQL）。CorrectError 手写 enum，不引 thiserror。
- **仅两条有意行为变更**（见下「行为差异声明」），其余一切 observable 行为不变。
- Windows 构建须前缀 MinGW bin 路径（见文末备注），cargo 命令走 PowerShell。

## 行为差异声明（仅两条，已按定案写入本计划）

1. **OPT_OUT 词表入库**：`correct_caliber` 的反向问法词表从 const（corrector.rs:479）迁入 `meta.term`（`status='opt-out'`，播种进 `seeds/terms.sql` 追加 7 行）。词表查询失败时 CaliberCorrector 不出手（宁可不补也不误改）。`recall_terms`/`sync_elements` 均过滤 `status='active'`，不受污染。副作用：`meta.term` 表 +7 行，Task 6 种子对拍基线需同步豁免或重拍。
2. **correction_log.detail 文案统一**：run_chain 统一生成 `{旧sql take120} → {新sql take120}`（原 groupby 为「补 GROUP BY：{旧 take150}」、agg/caliber/value 各带中文前缀）。`kind` 列不变，日志分类连续性不受影响。

## 前置依赖核查（Task 7.0 执行时逐项打勾；缺失标注处理人）

| 需要 | 归属 | 缺失时处理 |
|---|---|---|
| `kernel::ast::{collect, Collector, split_top_and, collect_where_cols}`（+ collect×3、split_top_and×1 共 4 个测试） | 需 Task 2 已下沉 | 缺则由 Task 7 按原行号补搬到 `crates/kernel/src/ast.rs`（函数+测试同走） |
| `kernel::BoxFut<'a, T> = Pin<Box<dyn Future<Output=T> + Send + 'a>>` | 需 Task 2 | 缺则 Task 7 补（1 行 type 别名） |
| `kernel::sql::Dialect` trait + `MysqlDialect` + 便捷访问器 `dialect::mysql() -> &'static MysqlDialect` | 需 Task 2（spec 2.4） | 缺则 Task 7 按 spec 2.4 补 ~30 行 |
| `connector::embed::{embed_query, to_pgvector}`（熔断返回 None 语义不变） | 需 Task 4 | 缺则阻塞 7.3 的 recall_elements/retrieve 向量半，先 team-lead 协调 |
| `seeds/*.sql` + 幂等种子 runner + `meta::migrate/seed` 已从 main.rs 摘除 | 需 Task 6 | runner 接口名以 Task 6 plan 为准；本计划 7.5 只向其 `seeds/terms.sql` 追加 OPT_OUT |
| MySQL 采集通道：`ReadOnlyMySql::fixed` 只收 `&'static str`，但 autodiscover 抽样 SQL 是 format! 动态拼 | 需 Task 3 裁决 | 缺则 Task 7.0 在 connector 补 3 个采集方法（见 7.0 Step 3），SQL 拼接收进 connector（IO 唯一出口职责），semantic 不碰裸池 |

## 目标文件映射（搬移清单，行号对应当前 meta.rs / corrector.rs）

### meta.rs → dms-semantic

| 源行号 | 内容 | 目标文件 |
|---|---|---|
| 164-180 | `is_backup_table` | ingest/sync.rs（pub） |
| 183-188 | `is_sensitive_col` | ingest/sync.rs（pub；render_schema 跨模块引用） |
| 191-205 | `domain_of` | ingest/sync.rs（私有） |
| 208-282 | `sync_schema` | ingest/sync.rs（MySQL 通道见 7.0） |
| 414-453 | `sync_elements` | registry/element.rs（pub） |
| 456-492 | `upsert_element` | registry/element.rs（私有） |
| 495-502 | `log_correction` | correct/mod.rs（pub；run_chain 与 pipeline schema-fix/explain-fail 共用） |
| 505-513 | `log_failure` | recall/pitfall.rs（pub） |
| 562-574 | `save_lesson_candidate` | recall/pitfall.rs |
| 577-595 | `extract_tables` | recall/pitfall.rs |
| 600-627 | `recall_elements` | recall/cards.rs（入参改 RecallCtx；返回 `Vec<(String,String)>` 不变） |
| 738-758 | `recall_terms` | recall/cards.rs |
| 864-879 | `match_word` | recall/filter.rs（pub） |
| 890-918 | `map_filter` | recall/filter.rs（pub） |
| 921-955 | `MetricHit` + `recall_metric_hits` | recall/metric.rs |
| 958-972 | `metric_card` | recall/metric.rs |
| 974-976 | `recall_metrics` | recall/metric.rs |
| 1042-1071 | `recall_value_hints` | recall/cards.rs |
| 1075-1087 | `clean_dim_name` | ingest/autodiscover/register.rs |
| 1090-1092 | `dim_hit` | recall/filter.rs（死代码，仅测试引用；去留见裁决③） |
| 1095-1118 | `recall_dimensions` | recall/cards.rs |
| 1120-1190 | `TableCtx` + `retrieve` | recall/schema.rs |
| 1194-1220 | `recall_pitfalls` | recall/pitfall.rs |
| 1226-1386 | `autodiscover_dict_columns` | autodiscover/mod.rs 编排 + probe.rs / register.rs 拆三段 |
| 1388-1445 | `best_dict_match` | autodiscover/match_dict.rs（纯函数） |
| 1448-1455 | `name_aligns` | autodiscover/match_dict.rs（纯函数） |
| 1458-1485 | `render_schema` | recall/schema.rs |
| 测试 1491-1678 | 13 个测试随函数走 | filter.rs×7 / sync.rs×2 / match_dict.rs×3 / register.rs×1 |

### corrector.rs → dms-semantic

| 源行号 | 内容 | 目标文件 |
|---|---|---|
| 14-59 | `Collector` + `collect` | 【Task 2 已在 kernel】直接 `use dms_kernel::ast::collect` |
| 62-116 | `schema_check` | correct/schema.rs（独立 validator，**不进链**，见 7.5 说明） |
| 119-249 | `ValueMaps` + `Linker` + `link_values_with` | correct/value.rs |
| 252-273 | `correct_value` | correct/value.rs → `ValueCorrector::correct`（码表加载改调 `registry::types::ValueMap::load_for_tables`，SQL 逐表原样） |
| 276-308 | `correct_agg` | correct/agg.rs → `AggCorrector::correct`（3 列查询+contains 命中循环原样保留，经 `ctx.pg`） |
| 319-382 | `add_scope_filter` | correct/caliber.rs（签名不动） |
| 385-418 | `split_top_and` | 【Task 2 已在 kernel】 |
| 421-429 | `first_ident_of` | correct/caliber.rs（私有） |
| 432-474 | `collect_where_cols` | 【Task 2 已在 kernel】 |
| 477-493 | `correct_caliber` | correct/caliber.rs → `CaliberCorrector::correct`（OPT_OUT 改 `ctx.registry.opt_out_words()`） |
| 498-546 | `fix_group_by` | correct/groupby.rs（签名不动） |
| 549-563 | `expr_has_agg` | correct/groupby.rs（私有） |
| 566-599 | `AggRule` + `parse_agg_rule` + `last_ident` | correct/agg.rs |
| 606-765 | `normalize_agg` + `proj_has_func_over` + `rewrite_agg` | correct/agg.rs |
| 测试 767-1117 | 29 个测试随文件走 | groupby×4 / agg×10 / value×8 / caliber×7（另 4 个在 kernel） |

---

### Task 7.0: 依赖补齐与测试基线

**Files:**
- Modify: `crates/semantic/Cargo.toml`（sqlx features 补 `mysql`）
- Modify: `crates/kernel/src/lib.rs` / `crates/connector/src/...`（仅当依赖核查发现缺失）

**Interfaces:**
- Consumes: 前置依赖核查表全部条目
- Produces: semantic 可编 MySQL 采集代码；测试基线记录

- [ ] **Step 1: 记录全仓测试基线**

Run（PowerShell，前缀 MinGW，下同）:
```
cargo test 2>&1 | Select-String "test result:"
```
Expected: 各 crate 全 ok。把各 crate passed 数记入本任务提交信息（后续每步对比，总数只增不减）。

- [ ] **Step 2: 按「前置依赖核查」表逐项验证**

逐项 `Select-String` 或 `cargo doc` 确认存在性；缺失项按表中「缺失时处理」列补齐（kernel 缺 collect/split_top_and/collect_where_cols 则按 corrector.rs 原行号连函数带测试搬进 `crates/kernel/src/ast.rs`，MySqlDialect 硬编码原样保留）。

- [ ] **Step 3: semantic Cargo.toml 补 mysql feature；必要时 connector 补采集方法**

`crates/semantic/Cargo.toml` 的 sqlx features 数组加 `"mysql"`（Task 1 漏配，ingest 读 information_schema/字典必需）。
若 Task 3 未提供动态只读探针通道，在 `crates/connector/src/mysql.rs` 补三个方法（框架采集专用，内部拼 SQL，会话级 READ ONLY 由池保证）：
```rust
/// information_schema 表/列采集（字面量查询）
pub async fn fetch_information_schema(&self) -> Result<(Vec<(String, String, Option<i64>)>, Vec<(String, String, String, String, i64)>), ConnectorError>;
/// 生产字典全量（t_dict_key/value，字面量查询）
pub async fn fetch_dict_rows(&self) -> Result<Vec<(String, String, String, String)>, ConnectorError>;
/// 列值只读抽样（动态表/列名；调用方负责 10s 超时）
pub async fn probe_distinct_values(&self, table: &str, col: &str, where_deleted_flag: bool, limit: usize) -> Result<Vec<Option<String>>, ConnectorError>;
```
三条 SQL 文本从 meta.rs:209-222 / 1233-1241 / 1299-1300 原样移入 connector。

- [ ] **Step 4: 验证**

Run: `cargo build 2>&1 | Select-Object -Last 3`
Expected: `Finished dev profile`，无 error。

- [ ] **Step 5: 提交**

```bash
git add crates/semantic/Cargo.toml crates/kernel crates/connector
git commit -m "Task7.0: semantic sqlx 补 mysql feature；kernel/connector 依赖缺口补齐；测试基线=<各crate passed数>"
```

---

### Task 7.1: recall/filter.rs 纯函数先行（TDD 红→绿）

**Files:**
- Create: `crates/semantic/src/lib.rs`、`crates/semantic/src/recall/mod.rs`、`crates/semantic/src/recall/filter.rs`
- Modify: `crates/server/src/meta.rs`（删除已搬函数与测试）

**Interfaces:**
- Consumes: meta.rs:864-879 / 890-918 / 1090-1092
- Produces: `dms_semantic::recall::filter::{match_word, map_filter, dim_hit}`（pub）

- [ ] **Step 1: 测试先行（红）**

`crates/semantic/src/lib.rs`:
```rust
//! dms-semantic：业务知识全部落点（注册表/召回/组合器/校正器/列标注）。不依赖 policy。
pub mod registry;
pub mod ingest;
pub mod recall;
pub mod correct;
```
`recall/mod.rs` 先只写 `pub mod filter;`（后续子任务逐步追加）。
`recall/filter.rs`：先只写 `#[cfg(test)] mod tests`，把 meta.rs 的 7 个测试原样拷入——`dimension_hit_matching`(1511-1521)、`match_word_takes_longest_alias`(1663-1667)、`map_filter_longest_wins/dedups_same_name/drops_single_char/exact_beats_partial/keeps_unrelated`(1620-1660)，含 `hits`/`kept` 两个 helper(1621-1627)。
Run: `cargo test -p dms-semantic 2>&1 | Select-Object -Last 5`
Expected: 编译失败（match_word/map_filter/dim_hit 未定义）= 红。

- [ ] **Step 2: 搬实现（绿）**

把 meta.rs:864-879 `match_word`、890-918 `map_filter`、1090-1092 `dim_hit` 原文（含全部中文 doc 注释）搬进 filter.rs。
Run: `cargo test -p dms-semantic 2>&1 | Select-String "test result:"`
Expected: `test result: ok. 7 passed`。

- [ ] **Step 3: meta.rs 删除已搬函数与对应测试，全仓回归**

meta.rs 删 match_word/map_filter/dim_hit 及 7 个测试（其余保留）。此时 meta.rs 内部调用点（recall_terms:747,751、recall_metric_hits:943,947、recall_value_hints:1060、recall_dimensions:1107,1111）改为全限定路径 `dms_semantic::recall::filter::{match_word, map_filter}` 调用——**过渡期 server 反向调 semantic 是允许的**（Task 1 已挂 path 依赖），7.3 把召回函数搬走后这些临时引用自然消失。
Run: `cargo test 2>&1 | Select-String "test result:"`
Expected: 全仓 passed 总数 = 基线（7 个测试只是换了 crate）。

- [ ] **Step 4: 提交**

```bash
git add crates/semantic/src crates/server/src/meta.rs
git commit -m "Task7.1: match_word/map_filter/dim_hit 纯函数迁入 semantic::recall::filter（7 测试原样通过）"
```

---

### Task 7.2: registry 六类型 + Registry 门面 + element 统一体同步

**Files:**
- Create: `crates/semantic/src/registry/mod.rs`、`crates/semantic/src/registry/types.rs`、`crates/semantic/src/registry/element.rs`
- Modify: `crates/server/src/meta.rs`（删 414-492）

**Interfaces:**
- Consumes: meta.rs:414-492（sync_elements/upsert_element）
- Produces: `registry::Registry` 门面（CorrectCtx 字段类型、未来 AppState 成员、Task 8 compose/fastpath 读口）；`registry::types::{MetricDef, DimensionDef, ValueMap, TermDef, JoinEdge, TableScope}`；`registry::element::sync_elements`

本步无单测迁移（DB 包装层）；验收 = 全仓 build+test 绿。不编造冒烟测试。

- [ ] **Step 1: registry/types.rs——六注册表行类型 + load 实现**

每类型 = 结构体 + `load_active`/`load_all`/`load_for_tables`（`query_as` 元组 + 显式构造，**不用 FromRow derive**——避免开 sqlx derive feature，与现风格一致）：
```rust
/// 指标注册表行（口径单一事实源）
pub struct MetricDef {
    pub metric_code: String, pub name: String, pub aliases: Vec<String>,
    pub source_table: String, pub agg_expr: String, pub scope_filter: String,
    pub time_col: String, pub dedup_keys: String, pub description: String,
}
impl MetricDef {
    pub async fn load_active(pg: &sqlx::PgPool) -> anyhow::Result<Vec<Self>> {
        let rows: Vec<(String, String, Vec<String>, String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT metric_code, name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description
             FROM meta.metric WHERE status = 'active'",
        ).fetch_all(pg).await?;
        Ok(rows.into_iter().map(|(metric_code, name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description)|
            Self { metric_code, name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description }).collect())
    }
}
/// 码值行；校正器按涉及表加载（SQL 语义与 corrector.rs:260-271 逐表查询一致）
pub struct ValueMap { pub table_name: String, pub column_name: String, pub name: String, pub code: String, pub match_kind: String }
impl ValueMap {
    pub async fn load_for_tables(pg: &sqlx::PgPool, tables: &std::collections::HashSet<String>) -> anyhow::Result<Vec<Self>> {
        let mut out = vec![];
        for t in tables {
            let rows: Vec<(String, String, String, String)> = sqlx::query_as(
                "SELECT column_name, name, code, match_kind FROM meta.value_map WHERE lower(table_name) = $1",
            ).bind(t).fetch_all(pg).await?;
            out.extend(rows.into_iter().map(|(column_name, name, code, match_kind)|
                Self { table_name: t.clone(), column_name, name, code, match_kind }));
        }
        Ok(out)
    }
}
// DimensionDef / TermDef / JoinEdge / TableScope 同构：字段照 meta.rs:55-155 DDL 列定义，
// load_active 过滤 status='active'（value_map/table_scope 无 status 列则 load_all）。
```

- [ ] **Step 2: registry/element.rs——element 统一体同步**

meta.rs:414-492 `sync_elements` + `upsert_element` 原文搬入（内部对 metric/dimension/value_map/term 的 4 条 SELECT 一字不改）。`sync_elements` pub。

- [ ] **Step 3: registry/mod.rs——Registry 门面**

```rust
pub mod types;
pub mod element;

/// 注册表读口门面（CorrectCtx.registry / 未来 AppState 成员 / Task 8 compose·fastpath 共用）
pub struct Registry {
    pg: sqlx::PgPool,
}
impl Registry {
    pub fn new(pg: sqlx::PgPool) -> Self { Self { pg } }
    pub fn pg(&self) -> &sqlx::PgPool { &self.pg }
    /// 指标命中（委托 recall 统一实现：match_word + map_filter 净化）——caliber 校正器用
    pub async fn metric_hits(&self, question: &str) -> anyhow::Result<Vec<crate::recall::MetricHit>> {
        crate::recall::recall_metric_hits(&self.pg, &crate::recall::RecallCtx::new(question)).await
    }
    /// caliber 反向问法 opt-out 词表（meta.term status='opt-out'；播种 seeds/terms.sql）
    pub async fn opt_out_words(&self) -> Result<Vec<String>, sqlx::Error> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT term FROM meta.term WHERE status = 'opt-out'")
                .fetch_all(&self.pg).await?;
        Ok(rows.into_iter().map(|(t,)| t).collect())
    }
}
```

- [ ] **Step 4: meta.rs 删 414-492，切换 sync_elements 调用点**

调用点共两处：① 种子流程尾部（meta.rs:356 `sync_elements(pg).await?`——若 Task 6 已把 seed 外置为 runner，改 runner 内该调用为 `registry::element::sync_elements`；runner 归属以 Task 6 plan 为准）；② autodiscover 尾部（meta.rs:1376，7.4 才搬走，本步先把该处改为全限定路径 `dms_semantic::registry::element::sync_elements(pg).await?`）。
Run: `cargo test 2>&1 | Select-String "test result:"` + `cargo build 2>&1 | Select-Object -Last 3`
Expected: 全绿，passed 总数 = 基线。

- [ ] **Step 5: 提交**

```bash
git add crates/semantic/src crates/server/src
git commit -m "Task7.2: registry 六注册表类型+Registry 门面+element 统一体同步迁入 semantic"
```

---

### Task 7.3: recall 六种统一 RecallCtx + schema 三路召回

**Files:**
- Create: `crates/semantic/src/recall/metric.rs`、`recall/cards.rs`、`recall/pitfall.rs`、`recall/schema.rs`
- Modify: `crates/semantic/src/recall/mod.rs`（RecallCtx + re-export）
- Modify: `crates/server/src/meta.rs`（删 600-627/738-758/921-976/1042-1071/1095-1118/1120-1220/505-513/562-595/1458-1485）
- Modify: `crates/server/src/pipeline.rs`（233-289、684/705、732/734、930 调用点）、`crates/server/src/main.rs`（131/133）

**Interfaces:**
- Consumes: meta.rs 召回族全部行号（见映射表）；`connector::embed::{embed_query, to_pgvector}`（需 Task 4）
- Produces: 六种召回统一入参签名 `async fn recall_K(pg: &PgPool, q: &RecallCtx<'_>) -> ...`；`recall::{retrieve, TableCtx}`；`recall::{log_failure, save_lesson_candidate, extract_tables}`

`recall/mod.rs` 定稿内容（本步）：
```rust
pub mod filter;
pub mod metric;
pub mod cards;
pub mod pitfall;
pub mod schema;

/// 六种召回统一入参：问题 + 已召回表名（pitfalls 用）+ 条数上限（elements/pitfalls 用）
pub struct RecallCtx<'a> {
    pub question: &'a str,
    pub tables: &'a [String],
    pub limit: usize,
}
impl<'a> RecallCtx<'a> {
    pub fn new(question: &'a str) -> Self { Self { question, tables: &[], limit: 8 } }
    pub fn with_tables(mut self, tables: &'a [String]) -> Self { self.tables = tables; self }
    pub fn with_limit(mut self, limit: usize) -> Self { self.limit = limit; self }
}

pub use filter::{map_filter, match_word};
pub use metric::{metric_card, recall_metric_hits, recall_metrics, MetricHit};
pub use cards::{recall_dimensions, recall_elements, recall_terms, recall_value_hints};
pub use pitfall::{extract_tables, log_failure, recall_pitfalls, save_lesson_candidate};
pub use schema::{retrieve, TableCtx};
```

- [ ] **Step 1: 四文件搬运（入参改 RecallCtx；SQL/命中/净化/卡片文案一字不改）**

- `recall/metric.rs`：`MetricHit`(921-929) + `recall_metric_hits`(932-955) + `metric_card`(958-972) + `recall_metrics`(974-976)。签名改 `recall_metric_hits(pg, q: &RecallCtx<'_>)`，函数体 `question` 改 `q.question`，其余原样。
- `recall/cards.rs`：`recall_dimensions`(1095-1118) + `recall_terms`(738-758) + `recall_value_hints`(1042-1071) + `recall_elements`(600-627)。注意：`recall_elements` 保持**非 Result** 返回 `Vec<(String, String)>`（embed 熔断降级空 vec 语义不变），用 `q.limit`；embed 调用改 `dms_connector::embed::{embed_query, to_pgvector}`。四者卡片 format! 文案一字不改。
- `recall/pitfall.rs`：`recall_pitfalls`(1194-1220，用 `q.tables`/`q.limit`) + `save_lesson_candidate`(562-574) + `extract_tables`(577-595) + `log_failure`(505-513)。
- `recall/schema.rs`：`TableCtx`(1120-1125) + `retrieve`(1127-1190，签名保持 `(pg, question, k)` 不变——它是 schema 上下文召回，不在六种之列）+ `render_schema`(1458-1485，`is_sensitive_col` 改 `crate::ingest::sync::is_sensitive_col`，向量半 embed 调用同 cards.rs 改法）。

- [ ] **Step 2: meta.rs 删除已搬函数，切换全部调用点**

pipeline.rs 调用点逐处替换（顶部加 `use dms_semantic::recall;`）：
| 行号 | 原 | 新 |
|---|---|---|
| 233 | `meta::retrieve(pg, question, 6)` | `recall::retrieve(pg, question, 6)` |
| 235-237 | `meta::recall_metrics/dimensions/terms(pg, question)` | `recall::recall_*(pg, &recall::RecallCtx::new(question))` |
| 239 | `meta::recall_elements(pg, question, 8)` | `recall::recall_elements(pg, &recall::RecallCtx::new(question).with_limit(8))` |
| 249 | `meta::recall_pitfalls(pg, question, &table_names, 6)` | `recall::recall_pitfalls(pg, &recall::RecallCtx::new(question).with_tables(&table_names).with_limit(6))` |
| 289 | `meta::recall_value_hints(pg, question)` | `recall::recall_value_hints(pg, &recall::RecallCtx::new(question))` |
| 684/705 | `meta::log_failure(...)` | `recall::log_failure(...)` |
| 732/734 | `meta::extract_tables / save_lesson_candidate` | `recall::extract_tables / recall::save_lesson_candidate` |
| 930 | `meta::retrieve(pg, question, 6)` | `recall::retrieve(pg, question, 6)` |
main.rs:131/133 同法（pitfalls 用 `with_limit(5)`）。
corrector.rs:483 `crate::meta::recall_metric_hits(pg, question)` 暂改 `dms_semantic::recall::recall_metric_hits(pg, &RecallCtx::new(question))`（7.5 文件解体后改同 crate 调用）。

- [ ] **Step 3: 全仓回归**

Run: `cargo test 2>&1 | Select-String "test result:"` + `cargo build 2>&1 | Select-Object -Last 3`
Expected: 全绿；passed 总数 = 基线。

- [ ] **Step 4: 提交**

```bash
git add crates/semantic/src crates/server/src
git commit -m "Task7.3: 六种召回统一 RecallCtx 入参迁入 semantic::recall；schema 三路召回与渲染随迁"
```

---

### Task 7.4: ingest ETL + autodiscover 拆 probe/match/register 三段（TDD 红→绿）

**Files:**
- Create: `crates/semantic/src/ingest/mod.rs`、`ingest/sync.rs`、`ingest/autodiscover/mod.rs`、`autodiscover/probe.rs`、`autodiscover/match_dict.rs`、`autodiscover/register.rs`
- Modify: `crates/semantic/src/lib.rs` 已有 `pub mod ingest;`
- Modify: `crates/server/src/meta.rs`（删 164-282/1075-1087/1226-1455）、`crates/server/src/main.rs`（75、86 调用点）

**Interfaces:**
- Consumes: meta.rs:164-282、1075-1087、1226-1455；connector 采集三方法（7.0 Step 3）
- Produces: `ingest::sync::{sync_schema, is_backup_table, is_sensitive_col}`；`ingest::autodiscover::autodiscover_dict_columns`

- [ ] **Step 1: 测试先行（红）**

6 个测试按归属拷入：`backup_tables_skipped`/`sensitive_cols_filtered`(1491-1508) → sync.rs；`dict_match_basic`/`dict_match_rejects`/`dict_match_collision_guard`(1523-1618) → match_dict.rs；`clean_dim_name_cuts_at_separator`(1669-1678) → register.rs。
Run: `cargo test -p dms-semantic 2>&1 | Select-Object -Last 5`
Expected: 编译失败 = 红。

- [ ] **Step 2: 搬实现（绿）**

- `ingest/sync.rs`：`is_backup_table`(164-180, pub) + `is_sensitive_col`(183-188, pub) + `domain_of`(191-205, 私有) + `sync_schema`(208-282)。两条 information_schema 查询若 7.0 已建 `fetch_information_schema` 则改调它（行为等价）；否则保留 `&MySqlPool` 直查原样。
- `autodiscover/match_dict.rs`：`best_dict_match`(1396-1445) + `name_aligns`(1448-1455) + `pub type DictIndex = std::collections::HashMap<String, (String, Vec<(String, String)>)>;` 纯函数原样。
- `autodiscover/probe.rs`：拆出探测段——`load_dicts`(原 1233-1245，改调 connector `fetch_dict_rows` 或保留原样) + 候选枚举(1248-1257) + 人工覆盖集(1260-1270) + deleted_flag 表集(1273-1279) + 逐列抽样循环中「跳过与抽样」部分(1285-1320)。产出：
```rust
pub struct ProbedColumn { pub table: String, pub col: String, pub comment: String, pub values: Vec<String> }
pub struct ProbeOutcome { pub dicts: super::match_dict::DictIndex, pub probed: Vec<ProbedColumn>,
    pub candidates: usize, pub probed_count: usize, pub skipped_manual: usize }
pub async fn probe(mysql: &sqlx::MySqlPool, pg: &sqlx::PgPool) -> anyhow::Result<ProbeOutcome>;
// 注：MySqlPool 入参为过渡形态，以 Task 3 交付为准——Task 3 完成则一律改 &ReadOnlyMySql（见裁决①）
```
抽样超时/失败 warn 文案（1296-1313）一字不改。
- `autodiscover/register.rs`：`clean_dim_name`(1075-1087) + 注册段（原 1325-1372 的 value_map 注册 + dimension CASE 注册，含「码数 >60 仅注册值映射」门控、`dim_code` 截 80、注释首段取名逻辑，全原样）：
```rust
pub async fn register_match(pg: &sqlx::PgPool, table: &str, col: &str, comment: &str,
    dict_key: &str, dict_name: &str, pairs: &[(String, String)], coverage: f64) -> anyhow::Result<()>;
```
- `autodiscover/mod.rs`：编排三段 + 汇总 JSON（字段名一字不差）：
```rust
pub async fn autodiscover_dict_columns(mysql: &sqlx::MySqlPool, pg: &sqlx::PgPool) -> anyhow::Result<serde_json::Value> {
    // 注：MySqlPool 入参为过渡形态，以 Task 3 交付为准——Task 3 完成则改 &ReadOnlyMySql（见裁决①）
    let mut o = probe::probe(mysql, pg).await?;
    let mut registered = vec![];
    for c in o.probed.drain(..) {
        let Some((dk, dn, pairs, cov)) = match_dict::best_dict_match(&c.values, &o.dicts, &c.comment) else { continue };
        register::register_match(pg, &c.table, &c.col, &c.comment, &dk, &dn, &pairs, cov).await?;
        registered.push(serde_json::json!({"table": c.table, "column": c.col, "dict": dk, "dict_name": dn,
            "distinct_values": c.values.len(), "coverage": cov}));
    }
    crate::registry::element::sync_elements(pg).await?;
    Ok(serde_json::json!({"dict_keys": o.dicts.len(), "candidates": o.candidates, "probed": o.probed_count,
        "skipped_manual": o.skipped_manual, "registered_count": registered.len(), "registered": registered}))
}
```
注意：`is_backup_table`/`is_sensitive_col` 在 probe 内的跳过判定（原 1286-1287）改 `crate::ingest::sync::{...}`。
Run: `cargo test -p dms-semantic 2>&1 | Select-String "test result:"`
Expected: 累计 `42 - 29 = 13` 个 recall+ingest 测试中 ingest 6 个 passed（此时 correct 29 个还没进来，semantic 共 13 passed）。

- [ ] **Step 3: meta.rs 删除已搬段，切 main.rs 调用点**

main.rs:75 `meta::sync_schema(&mysql, &pg)` → `dms_semantic::ingest::sync::sync_schema(&mysql, &pg)`；main.rs:86 `meta::autodiscover_dict_columns(&mysql, &pg)` → `dms_semantic::ingest::autodiscover::autodiscover_dict_columns(&mysql, &pg)`。
Run: `cargo test 2>&1 | Select-String "test result:"` + `cargo build 2>&1 | Select-Object -Last 3`
Expected: 全绿，passed 总数 = 基线。

- [ ] **Step 4: 提交**

```bash
git add crates/semantic/src crates/server/src
git commit -m "Task7.4: sync_schema ETL + autodiscover 拆 probe/match/register 三段迁入 semantic::ingest（6 测试原样通过）"
```

---

### Task 7.5: correct/ trait 链 + 五校正器 + pipeline 接线（TDD 红→绿）

**Files:**
- Create: `crates/semantic/src/correct/mod.rs`、`correct/schema.rs`、`correct/groupby.rs`、`correct/agg.rs`、`correct/caliber.rs`、`correct/value.rs`
- Modify: `crates/semantic/seeds/terms.sql`（追加 OPT_OUT 7 行；路径以 Task 6 产物为准）
- Modify: `crates/server/src/pipeline.rs`（597-624 改造）、`crates/server/src/main.rs`（112 调用点）、`crates/server/src/corrector.rs`（清空待删）

**Interfaces:**
- Consumes: corrector.rs 全部（映射表）；kernel `BoxFut`/`Dialect`/`ast`（7.0）；`registry::Registry`
- Produces: `correct::{Corrector, CorrectCtx, CorrectError, run_chain, default_chain, log_correction, schema_check}`

**为什么 SchemaCorrector 不进链**：schema_check 的产物 hint 是 LLM `repair()` 的自修输入（pipeline.rs:597-603），「校验+LLM 自修」与四个确定性改写器不同构；repair 依赖 llm/principal/retrieve，是 pipeline（未来 agent）资产。Task 7 让 schema.rs 以独立 validator 存在、pipeline 597-603 段原样保留；spec 4.1「run_chain(5 校正器)」是 Task 9（AskRun Repair 轮）的终态口径，届时再并。

**三种签名如何统一进 trait（对照表）**：今天五校正器有三种签名形态，统一手法 = 入参全部上移 CorrectCtx 字段、函数体内嵌原逻辑、纯核函数签名一律不动（33 单测直接调用纯核，一行不改）：

| 校正器 | 旧签名（corrector.rs 行号） | 签名形态 | 统一后输入来源 | 壳内行为 |
|---|---|---|---|---|
| GroupBy | `fix_group_by(sql)` (498) | 纯函数 | 只需 `sql`（trait 固有参数） | `Ok(fix_group_by(sql))`，`ctx` 未使用 |
| Value | `correct_value(pg, sql)` (252) | pg | `pg` → `ctx.pg`（码表加载改调 `ValueMap::load_for_tables(ctx.pg, &tables)`，逐表 SQL 原样） | 内嵌 252-273 函数体，`collect` 改 kernel 路径 |
| Agg | `correct_agg(pg, question, sql)` (276) | pg+question | `pg` → `ctx.pg`；`question` → `ctx.question`（3 列查询+contains 命中循环原样，**不**改走 registry.metric_hits） | 内嵌 276-308 函数体 |
| Caliber | `correct_caliber(pg, question, sql)` (477) | pg+question | `question` → `ctx.question`（opt-out 判定）；`recall_metric_hits` → `ctx.registry.metric_hits(ctx.question)`（委托同一实现）；OPT_OUT const → `ctx.registry.opt_out_words()` | 内嵌 477-493 函数体，仅 opt-out 两处改库读 |
| Schema | `schema_check(pg, sql)` (62) | pg（产物喂 LLM repair） | **不进 trait**——独立 validator 保留 `schema_check(pg, sql)` 原签名 | correct/schema.rs 单文件承载，pipeline 597-603 原样调用 |

即：三种签名 `(sql)` / `(pg, sql)` / `(pg, question, sql)` 的差异参数全部被 `CorrectCtx{pg, question, ..}` 吸收，trait 统一为 `correct(&self, ctx, sql) -> BoxFut<Result<Option<String>, CorrectError>>`；壳层每校正器 ~15 行，逻辑零改写。

- [ ] **Step 1: 测试先行（红）**

29 个测试按归属原样拷入各文件 `#[cfg(test)]`（含 helper）：
- groupby.rs ×4：`adds_missing_group_by`/`keeps_existing_group_by`/`pure_aggregate_untouched`/`no_aggregate_untouched`(776-797) + `norm` helper(771-773)
- agg.rs ×10：`agg_rule_parsed`/`agg_distinct_filled`/`agg_func_normalized`/`agg_correct_untouched`/`agg_count_star_follows_metric_caliber`/`agg_occupied_rename_skipped`/`agg_subquery_untouched`/`agg_other_column_untouched`(824-902) + `count_star_normalized_to_distinct`/`count_star_untouched_when_ambiguous`(1097-1117)
- value.rs ×8：`value_eq_swapped`/`value_mirror_eq_swapped`/`value_like_rewritten`/`value_in_list_swapped`/`value_like_kind_in_list_skipped`/`value_bare_col_untouched`/`value_already_code_untouched`/`value_unknown_name_untouched`(926-1022) + `vmaps` helper(904-924)
- caliber.rs ×7：`caliber_adds_missing_status_filter`/`caliber_no_change_when_complete`/`caliber_respects_user_status_filter`/`caliber_qualifies_with_alias`/`caliber_skips_join_and_other_tables`/`caliber_skips_subquery_filter_and_empty`/`caliber_adds_where_when_absent`(1027-1087) + `ORDER_SCOPE` const(1025)
（`split_top_and_basics`(1089-1095) 与 collect 三测在 kernel，随 Task 2/7.0。）
Run: `cargo test -p dms-semantic 2>&1 | Select-Object -Last 5`
Expected: 编译失败 = 红。

- [ ] **Step 2: correct/mod.rs——trait + CorrectCtx + CorrectError + run_chain + log_correction（新代码全文）**

```rust
//! 确定性校正链（移植 SuperSonic 五 Corrector 的有序扁平表）：统一签名、统一日志/耗时/截断。
//! 链顺序与旧 pipeline 605→621 一字不差：GroupBy → Agg → Caliber → Value。

pub mod schema;
pub mod groupby;
pub mod agg;
pub mod caliber;
pub mod value;

/// 校正上下文：五个校正器的全部外部输入。
pub struct CorrectCtx<'a> {
    /// PG 元数据库：schema 校验读 column_doc、agg/value 各自原样查询、log_correction 写口
    pub pg: &'a sqlx::PgPool,
    /// 用户原始问句：agg 指标命中、caliber 反向问法 opt-out 判定
    pub question: &'a str,
    /// SQL 方言（kernel 契约，spec 2.4）：v1 纯核函数签名因单测原样约束不动、仍用 MySqlDialect，
    /// 本字段为 Task 8 compose / 多数据源占位（spec：不预造，留 trait 位）
    pub dialect: &'a dyn dms_kernel::sql::Dialect,
    /// 注册表读口门面：caliber 读 metric_hits / opt_out_words
    pub registry: &'a crate::registry::Registry,
}

/// 校正器错误（手写枚举，spec 4.2 不引 thiserror）
#[derive(Debug)]
pub enum CorrectError {
    Db(sqlx::Error),
    Internal(anyhow::Error),
}
impl std::fmt::Display for CorrectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "DB 访问失败: {e}"),
            Self::Internal(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for CorrectError {}
impl From<sqlx::Error> for CorrectError {
    fn from(e: sqlx::Error) -> Self { Self::Db(e) }
}
impl From<anyhow::Error> for CorrectError {
    fn from(e: anyhow::Error) -> Self { Self::Internal(e) }
}

pub trait Corrector: Send + Sync {
    /// 与 meta.correction_log.kind 现网取值一致（groupby-fix/agg-fix/caliber-fix/value-fix）
    fn name(&self) -> &'static str;
    /// Ok(Some(新SQL))=改写命中；Ok(None)=不动；Err=失败（run_chain warn 后跳过，同旧 if-let-Ok 吞错语义）
    fn correct<'a>(&'a self, ctx: &'a CorrectCtx<'a>, sql: &'a str)
        -> dms_kernel::BoxFut<'a, Result<Option<String>, CorrectError>>;
}

/// 有序链：逐站传递 SQL；日志/耗时/截断样板只写这一次（原 pipeline 604-624 四处拷贝）。
pub async fn run_chain(
    chain: &[std::sync::Arc<dyn Corrector>],
    ctx: &CorrectCtx<'_>,
    sql: String,
) -> (String, Vec<(&'static str, String)>) {
    let mut cur = sql;
    let mut changes = vec![];
    for c in chain {
        let t0 = std::time::Instant::now();
        match c.correct(ctx, &cur).await {
            Ok(Some(new)) if new != cur => {
                let detail = format!(
                    "{} → {}",
                    cur.chars().take(120).collect::<String>(),
                    new.chars().take(120).collect::<String>()
                );
                log_correction(ctx.pg, c.name(), ctx.question, &detail).await;
                tracing::info!(target: "correct_chain", name = c.name(), ms = t0.elapsed().as_millis() as u64, "校正命中");
                changes.push((c.name(), detail));
                cur = new;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(target: "correct_chain", name = c.name(), error = %e, "校正器失败，跳过"),
        }
    }
    (cur, changes)
}

/// 链顺序硬编码（spec 6：栈显式可 grep，顺序可 dump）
pub fn default_chain() -> Vec<std::sync::Arc<dyn Corrector>> {
    vec![
        std::sync::Arc::new(groupby::GroupByCorrector),
        std::sync::Arc::new(agg::AggCorrector),
        std::sync::Arc::new(caliber::CaliberCorrector),
        std::sync::Arc::new(value::ValueCorrector),
    ]
}

/// 纠错反哺日志（引擎 B+）：校正器出手即记录，供同错累计升格 pitfall（meta.rs:495-502 原样）
pub async fn log_correction(pg: &sqlx::PgPool, kind: &str, question: &str, detail: &str) {
    let _ = sqlx::query("INSERT INTO meta.correction_log(kind, question, detail) VALUES ($1,$2,$3)")
        .bind(kind)
        .bind(question.chars().take(200).collect::<String>())
        .bind(detail.chars().take(500).collect::<String>())
        .execute(pg)
        .await;
}
```

- [ ] **Step 3: 五校正器文件（纯核按映射表原样搬；壳层各 ~15 行）**

壳层样板（groupby.rs 示例，其余同构）：
```rust
pub struct GroupByCorrector;
impl crate::correct::Corrector for GroupByCorrector {
    fn name(&self) -> &'static str { "groupby-fix" }
    fn correct<'a>(&'a self, ctx: &'a crate::correct::CorrectCtx<'a>, sql: &'a str)
        -> dms_kernel::BoxFut<'a, Result<Option<String>, crate::correct::CorrectError>>
    {
        Box::pin(async move {
            let _ = ctx; // 纯 AST 改写，无外部输入
            Ok(fix_group_by(sql))
        })
    }
}
```
- `agg.rs`：`AggCorrector::correct` 内嵌原 correct_agg(276-308) 函数体——3 列 metric 查询经 `ctx.pg`、contains 命中循环原样、`question` 改 `ctx.question`。
- `value.rs`：`ValueCorrector::correct` 内嵌原 correct_value(252-273)——collect 改 `dms_kernel::ast::collect`，码表加载改 `crate::registry::types::ValueMap::load_for_tables(ctx.pg, &tables)` 后组 `ValueMaps`（组 map 逻辑原样）。
- `caliber.rs`：`CaliberCorrector::correct` 内嵌原 correct_caliber(477-493)，**唯一有意改动**——OPT_OUT const(479) 删除，改：
```rust
// 反向问法 opt-out：词表在 meta.term(status='opt-out')（seeds/terms.sql 播种）。
// 词表查询失败 → 本校正器不出手（宁可不补也不误改，fail-closed）
let opt_out = ctx.registry.opt_out_words().await.map_err(crate::correct::CorrectError::Db)?;
if opt_out.iter().any(|w| ctx.question.contains(w.as_str())) {
    return Ok(None);
}
let hits = ctx.registry.metric_hits(ctx.question).await.map_err(crate::correct::CorrectError::Internal)?;
// 其后 add_scope_filter 逐命中尝试补齐逻辑原样（changed/then_some 不变）
```
- `schema.rs`：`schema_check`(62-116) 原样搬（collect 改 kernel 路径），pub；**不实现 Corrector、不进 default_chain**，文件头注释说明原因与 Task 9 去向。
Run: `cargo test -p dms-semantic 2>&1 | Select-String "test result:"`
Expected: `test result: ok. 42 passed`（7 filter + 6 ingest + 29 correct）。

- [ ] **Step 4: seeds/terms.sql 追加 OPT_OUT 词表**

```sql
-- caliber 校正器反向问法 opt-out 词表（原 corrector.rs:479 const 迁入；status='opt-out' 不参与 active 召回与元素同步）
INSERT INTO meta.term(term, definition, aliases, status) VALUES
  ('全部状态', '[opt-out] 用户明确要全量状态，口径补全整体跳过', '{}', 'opt-out'),
  ('所有状态', '[opt-out] 用户明确要全量状态，口径补全整体跳过', '{}', 'opt-out'),
  ('包括已取消', '[opt-out] 用户明确要全量状态，口径补全整体跳过', '{}', 'opt-out'),
  ('含已取消', '[opt-out] 用户明确要全量状态，口径补全整体跳过', '{}', 'opt-out'),
  ('包含作废', '[opt-out] 用户明确要全量状态，口径补全整体跳过', '{}', 'opt-out'),
  ('含作废', '[opt-out] 用户明确要全量状态，口径补全整体跳过', '{}', 'opt-out'),
  ('不限状态', '[opt-out] 用户明确要全量状态，口径补全整体跳过', '{}', 'opt-out')
ON CONFLICT (term) DO UPDATE SET definition = EXCLUDED.definition, status = EXCLUDED.status;
```
注意：Task 6 种子对拍基线若覆盖 meta.term，需豁免此 7 行或重拍基线（对拍脚本：`SELECT count(*) FROM meta.term WHERE status='opt-out'` = 7）。

- [ ] **Step 5: pipeline.rs 597-624 改造（替换后全文）**

```rust
    // SchemaCorrector：幻觉列校验 + LLM 自修（与确定性改写器不同构，Task 9 并入 AskRun Repair 轮）
    if let Ok(Some(hint)) = dms_semantic::correct::schema_check(pg, &ensure_limit(&sql)).await {
        if let Ok(fixed) = repair(llm, pg, p, question, &sql, &hint).await {
            sql = fixed;
            route = "llm+schema-fix".into();
            dms_semantic::correct::log_correction(pg, "schema-fix", question, &hint).await;
        }
    }
    // 确定性校正链（GroupBy→Agg→Caliber→Value，顺序同原 604-624）：run_chain 统一日志/耗时/截断
    let registry = dms_semantic::registry::Registry::new(pg.clone());
    let cctx = dms_semantic::correct::CorrectCtx {
        pg,
        question,
        dialect: dms_kernel::sql::dialect::mysql(),
        registry: &registry,
    };
    let (fixed, _changes) = dms_semantic::correct::run_chain(&dms_semantic::correct::default_chain(), &cctx, sql).await;
    sql = fixed;
```
route 行为核对：原四校正器命中不改 route，改造后 `_changes` 弃置，route 只被 schema-fix/repair 改——一致。explain-fail 处（642）`meta::log_correction` 同步改 `dms_semantic::correct::log_correction`。main.rs:112 `corrector::schema_check` → `dms_semantic::correct::schema_check`。

- [ ] **Step 6: corrector.rs 清空，全仓回归**

corrector.rs 删除全部已搬内容（留空或本步直接删文件，main.rs 同步摘 `mod corrector;`——若 7.6 统一清场则本步留空壳）。
Run: `cargo test 2>&1 | Select-String "test result:"` + `cargo build 2>&1 | Select-Object -Last 3`
Expected: 全绿，passed 总数 = 基线。

- [ ] **Step 7: 连库行为验证（需可连 PG 的环境，人工或 CI 执行）**

① 播种后 `SELECT term FROM meta.term WHERE status='opt-out'` 返回 7 行；② 问「全部状态的订单有多少」口径补全不出手（opt-out 生效，correction_log 无 caliber-fix 新行）；③ 问「本月有多少个订单」口径补全正常出手（correction_log 新增 kind='caliber-fix' 行，detail 为 `{旧} → {新}` 形态）；④ 问「湖南省销售额」value-fix 换码正常。

- [ ] **Step 8: 提交**

```bash
git add crates/semantic crates/server/src
git commit -m "Task7.5: 五校正器解体入 semantic::correct + Corrector trait 有序链接线 pipeline（29 测试原样通过；OPT_OUT 迁 seeds/terms.sql）"
```

---

### Task 7.6: 清场——meta.rs/corrector.rs 删除 + 全仓验收

**Files:**
- Delete: `crates/server/src/meta.rs`、`crates/server/src/corrector.rs`（条件见 Step 1）
- Modify: `crates/server/src/main.rs`（摘 `mod meta;`/`mod corrector;`，清理残留 use）

- [ ] **Step 1: 删除两个上帝文件**

前置条件：meta.rs 内 Task 6 的资产（migrate DDL、seed 常量）已随 Task 6 外置清空。若 Task 6 未清场——**不删 meta.rs**，只保留其 Task 6 段，文件头加 `// TODO(Task6): DDL/种子外置后本文件删除`，并在提交信息标注「需 Task 6 补清场」。corrector.rs 7.5 已清空，直接删。

- [ ] **Step 2: main.rs 摘除 mod 声明与残留引用**

Run: `cargo build 2>&1 | Select-String "error"` 逐个修编译错误（预期只剩 use 路径类）。

- [ ] **Step 3: 全仓测试计数对比基线**

Run: `cargo test 2>&1 | Select-String "test result:"`
Expected: 全仓 passed 总数 = 7.0 基线（46 个迁移测试一个不落：semantic 42 + kernel 4）；零 failed。

- [ ] **Step 4: 依赖方向校验**

Run: `cargo tree -p dms-semantic --prefix none 2>&1 | Select-String "dms-policy|axum"`
Expected: **空**（semantic 不依赖 policy、不配 axum——spec 硬规则）。
Run: `cargo tree -p dms-semantic --prefix none 2>&1 | Select-String "dms-" | Select-Object -First 6`
Expected: 仅 dms-kernel、dms-connector。

- [ ] **Step 5: 文件粒度自检（spec 5.4 甜区 150-450 行）**

Run: `Get-ChildItem -Recurse crates/semantic/src -Filter *.rs | ForEach-Object { "$($_.FullName.Substring($_.FullName.IndexOf('src'))) $((Get-Content $_.FullName | Measure-Object -Line).Lines)" }`
Expected: 甜区豁免项（mod.rs 声明文件、RecallCtx 所在 recall/mod.rs）之外，无 >500 或 <80 且无独立测试的文件。semantic 全 crate 约 22 个 .rs，全仓总数逼近 60 上限——属预期，Task 8/9 消化 server 侧 direct/pipeline 后回落。

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "Task7.6: 删除 meta.rs/corrector.rs 上帝文件，meta 解体②完成（46 迁移测试全绿）"
```

---

## 自检（已执行）

- **spec 覆盖**：迁移步 7 全部内容（registry/ingest/recall 拆 + 五校正器 trait + CorrectCtx 保 caliber 读 question、value 读 pg）✓；3.1 目录四类 ✓；3.3 Corrector/run_chain 签名逐字遵守 ✓。
- **算法不变性核对**：全部 SQL 文本、阈值（0.35 余弦距离/80% 覆盖/10s 超时/take 截断）、命中逻辑（agg 与 caliber 两套不合并）、卡片 format! 文案、门控（保守跳过规则）均按行号原样搬运；仅两条有意行为变更已在「行为差异声明」列出 ✓。
- **测试账目**：46 = semantic 42（filter 7 + ingest 6 + correct 29）+ kernel 4（collect 3 + split_top_and 1），每步与基线对比 ✓。
- **签名统一边界**：六种召回统一 RecallCtx 入参；返回类型按消费形态保留三档（Vec<String> 卡片 / Vec<MetricHit> 结构化 / Vec<(String,String)> 元素对——pipeline 按元素名去重依赖元组）；retrieve 不在六种之列保持原签名 ✓。
- **占位符扫描**：无 TBD；新代码（mod.rs/run_chain/Registry/RecallCtx/壳层/种子 SQL/编排函数）均给全文；搬运代码给行号映射 ✓。
- **依赖红线**：零新增第三方 crate；sqlx 仅补 mysql feature（Task 1 漏配修正）；BoxFut 手写；CorrectError 手写 enum ✓。
- **已知留白（已在文中标注）**：① connector 采集三方法归属待 Task 3 确认；② seeds runner 接口名以 Task 6 plan 为准；③ embed 导出路径以 Task 4 为准；④ meta.rs 删除以 Task 6 清场为前置。

## 需 team-lead 裁决（阻塞前先确认）

1. **ingest MySQL 入参形态**：7.4 代码块按现状写 `&sqlx::MySqlPool`（过渡形态，文中已标注）。但 spec §1 红线「connector 不导出裸 MySqlPool」+ 7.0 Step 3 采集三方法入 connector 的终态是 `&ReadOnlyMySql`。请裁决：以 Task 3 交付为准（Task 3 完成则 7.4 签名一律改 `&ReadOnlyMySql`，main.rs 调用点零适配）；若 Task 3 未完，是否允许本任务暂持 `&MySqlPool` 过渡、Task 3 落地时回头收紧？
2. **OPT_OUT 入库的种子对拍配套**：行为差异①使 `meta.term` +7 行（status='opt-out'）。Task 6 种子对拍基线若覆盖 meta.term，选「对拍脚本豁免 status='opt-out' 行」还是「重拍基线」？（7.5 Step 4 已给豁免验证 SQL：`SELECT count(*) FROM meta.term WHERE status='opt-out'` = 7。）
3. **dim_hit 死代码去留**（meta.rs:1090-1092，仅 dimension_hit_matching 测试引用，生产路径用 match_word）：默认随 7 个 filter 测试原样搬入 recall/filter.rs 保留（守「13 测试一字不改」硬约束）；若想连测试一起删，需豁免该硬约束，请裁决。
4. **meta.rs 删除前置 = Task 6 清场**：7.6 Step 1 已按条件分支写好（Task 6 未清场则不删 meta.rs、留 Task 6 段并标注「需 Task 6 补清场」）。请确认此降级路径可接受。

## 备注（Windows 构建）

cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）

## 备注（测试数口径修正）

任务书「33 召回 + 13 校正单测」数字写反：实际 corrector.rs 33 个（groupby 4 + agg 10 + value 8 + caliber 7 + collect 3 + split_top_and 1）、meta.rs 13 个（filter 7 + ingest 6），合计 46。本计划按实际归属分配。
