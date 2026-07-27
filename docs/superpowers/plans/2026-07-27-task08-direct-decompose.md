# Task 8：direct 解体 + 口径单一事实源（最高危）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 server/direct.rs（1385 行）解体迁入 `dms-semantic`（`compose/{mod,path,guard}.rs` + `fastpath.rs`），组合器/快路径算法逐字不动，只改三处口径来源：① 组合器时间桥改读 `metric.time_col`（修「设计意图与实现自相矛盾」）；② 有效订单状态码 `'0','108','199'` 运行时三处内联统一读 `meta.table_scope`；③ `doc_binding` 单号→表硬编码 match 迁注册表 `meta.doc_binding`。**必须拆三个独立提交**（A 时间桥 / B 状态码 / C doc_binding），各自单独跑 regression.py(51) + evaluation.py(38 exec-only)，结果集变化逐题人工判定修复还是回归。

**Architecture:** 组合器与快路径是 semantic 的「注册表驱动装配」组件（spec 3.1）；时间基元已在 kernel（Task 2 `nl::time`）；注册表读口已在 semantic（Task 7 `registry`）；本任务只做「搬位置 + 换口径来源」，不碰装配算法、门控顺序、路由协议（`DirectHit` 形状不变，pipeline 消费点仅改路径前缀）。

**Tech Stack:** Rust workspace、cargo、sqlx（PG 侧读注册表）；判官 python tools/regression.py + tools/evaluation.py（需真实库环境）。

## Global Constraints

- **最高危红线（spec 5.3 第 2 条）**：提交A 会改变一批线上问句的答案数值（售后/费用/活动类带时间词的问句从「回落 LLM」变「组合器按 time_col 装配」）。**上线前必须产出对照清单**：哪些问句的数会变、变成什么、为什么现在的错（模板见 Task 8.D）。未完成对照清单 + 逐题人工判定前，三个提交一律不得合并。
- **比结果集不比 SQL 文本**：SQL 允许因修 bug 变（桥接别名改名、时间列换人），结果集不许无理由变。evaluation.py exec-only 是主门禁；regression.py 的 SQL contains 断言因修复变红时，人工判定为「修复」后更新 case，判定为「回归」一律回滚。
- **算法逻辑逐字不动**：`compose_sql_with` 的 FROM 装配/扇出检查/去重子查询/表级口径附加、`find_path`/`find_edge` BFS、`has_residue` 守卫、`sales_breakdown`/`agg_template`/`sniff_doc_code` 的装配与剥词——全部原样搬，仅允许本 plan 点名的适配（字段名、use 路径、签名换 registry、三处口径来源）。
- **口径单一事实源**：运行时禁止再出现 `'0','108','199'` 字面量拼进业务 SQL（graph.rs:30、direct.rs:592、direct.rs:774 三处）。`meta.table_scope` 是唯一事实源；`meta.metric.scope_filter` 与 `corrector` 的 `ORDER_SCOPE` 的处理见「需 team-lead 裁决」1/2。
- **fail-closed / 宁缺毋滥**：注册表查询失败或缺行 → 快路径/组合器失命中回落（`None` + `tracing::warn`），绝不拿残缺口径装配；graph 建图（管理操作）缺 `t_sales_order` 行 → 直接 bail。
- **基线前提**：本 plan 以 Task 1/2/6/7 落地后的代码形态为基线（workspace 六 crate、`dms_kernel::nl::time` facade、`seeds/*.sql` 外置、`registry` 门面已建）。任一前提缺失 → 8.0 gate 拦下，标「需 Task N 补」，不在本任务内重实现。
- **不新增第三方依赖**。Windows 构建须前缀 MinGW bin 路径（见文末备注），cargo 命令走 PowerShell 不走 Bash。
- **既有单测总账**：direct.rs 内 30+ 测试随模块搬走（断言除点名适配外一字不改）；其中时间族 7 测试（`time_recent_n_with_cn_numbers`/`time_quarter_and_half_year`/`time_explicit_month`/`time_relative_words`/`time_col_is_parameterized`/`cn_num_parses`/`top_n_detect`）Task 2 已在 kernel 语义等价复制，随 direct.rs 删除下线，不算测试真空。

## 上游契约清单（Task 8.0 逐项核对；缺失即 blocker）

**需 Task 2（kernel）已 pub 交付：**
| # | 接口 | 用途 |
|---|---|---|
| T2-1 | `dms_kernel::nl::time::{time_predicate, fill_time_col}` | 提交A 时间桥：谓词模板 + 填列（不再经 `time_window` 的 order_time 写死中转） |

**需 Task 6（meta 解体①）已交付：**
| # | 接口 | 用途 |
|---|---|---|
| T6-1 | `crates/semantic/migrations/0001_*.sql` 起版本化 DDL 目录存在，编号连续 | 提交C 新增 `meta.doc_binding` 迁移文件接末位编号 |
| T6-2 | `crates/semantic/seeds/metrics.sql`、`seeds/table_scopes.sql` 外置（含 time_col 列值与 table_scope 三行） | 提交A 改销量 time_col 种子、提交B 种子一致性守护 |

**需 Task 7（semantic registry）已交付：**
| # | 接口 | 用途 |
|---|---|---|
| T7-1 | `dms_semantic::registry::Registry::new(pg)` + `pg()` 访问器 | compose/fastpath 读口载体 |
| T7-2 | `registry::types::MetricDef::load_active(&PgPool)`，结构含 `time_col`/`dedup_keys` 字段 | 提交A 组合器指标源（现状 direct.rs:58-63 的 SELECT 不查 time_col，必须改走此类型） |
| T7-3 | `registry::types::DimensionDef::load_active` / `JoinEdge::load_active` / `TableScope::load_all` | 组合器维度/边/表级口径源 |
| T7-4 | `DimensionDef` 字段含 `name/aliases/source_table/expr`；`JoinEdge` 字段按 DDL 列名（`left_table/left_col/right_table/right_col/card`）；`TableScope` 字段含 `table_name/filter` | compose/path 字段访问适配（direct.rs 本地 `lt/lc/rt/rc` → DDL 列名） |

> T7-2/3/4 以 Task 7 plan「registry/types.rs 六类型同构交付」为准。若实跑发现某类型缺 `load_active`/`load_all` 或字段名不符 → **停止本任务**，缺项清单发回 team-lead（标「需 Task 7 补 T7-某」），不在 compose 内自己写 SQL 重查。

**server 侧现状（本任务直接消费，无需补）：**
| # | 接口 | 用途 |
|---|---|---|
| S1 | `pipeline.rs:539` `crate::direct::detect_relation(question)`；`:547-549` `try_compose(pg,question)` / `try_direct(question)`；`:885-887` `Relation` 三变体消费 | 调用点适配清单 |
| S2 | `graph.rs:22` `sync(mysql: &MySqlPool, pg: &PgPool)`（pg 已在手） | 提交B 读 table_scope 无需改签名 |

---

### Task 8.0: 上游契约核对 + 判官前态基线（gate，不写代码）

**Files:**
- 只读：`crates/kernel/src/nl/**`、`crates/semantic/src/registry/**`、`crates/semantic/migrations/**`、`crates/semantic/seeds/**`

- [ ] **Step 1: 逐项 grep 核对 T2-1 / T6-1 / T6-2 / T7-1~4**

Run（PowerShell，前缀 MinGW，下同）:
```powershell
Select-String -Path crates/kernel/src/nl/*.rs -Pattern "pub fn time_predicate|pub fn fill_time_col"
Select-String -Path crates/semantic/src/registry/types.rs -Pattern "pub struct MetricDef|pub struct DimensionDef|pub struct JoinEdge|pub struct TableScope|load_active|load_all|time_col"
Select-String -Path crates/semantic/src/registry/mod.rs -Pattern "pub struct Registry|pub fn new|pub fn pg"
Get-ChildItem crates/semantic/migrations, crates/semantic/seeds | Select-Object Name
```
Expected: 每条契约至少一处命中；migrations/seeds 目录非空且记录 migrations 末位编号（提交C 用）。

- [ ] **Step 2: 缺项处理**

任一契约缺失 → **停止本任务**，缺项清单发回 team-lead（标「需 Task 2/6/7 补 某」），不在本任务内重实现。全齐 → 继续。

- [ ] **Step 3: 判官环境确认 + 前态基线归档（三个提交共用的对照起点）**

确认 docker `dms-ai-pg` 在跑、MySQL 可达、当前工作区干净（基线 commit = Task 7 末态）：
```powershell
cargo build 2>&1 | Select-Object -Last 2
python tools/evaluation.py --baseline
python tools/regression.py | Out-File -Encoding utf8 tools\reg_pretask8.txt
```
Expected: build 绿；eval 基线行写入 `tools/eval_baseline.csv`（记录 rate 与逐题通过面）；`reg_pretask8.txt` 51 题全绿（若前态已有红题，逐题记录题名，后续提交对比时排除该既有红）。**前态基线不达标 → 停下报 team-lead，不在烂基线上动刀。**

---

### Task 8.A: 【提交A】组合器迁 semantic::compose + 时间桥改读 metric.time_col

> 本任务 = 一个 git 提交。Step 1-2 纯搬移（零行为变化，用全量单测锁死）；Step 3-5 TDD 做时间桥修复；Step 6 判官逐题判定；全部过了才 commit。

**Files:**
- Create: `crates/semantic/src/compose/mod.rs`、`crates/semantic/src/compose/path.rs`、`crates/semantic/src/compose/guard.rs`
- Modify: `crates/semantic/src/lib.rs`（挂 `pub mod compose;` + `DirectHit` 根部定义）
- Modify: `crates/server/src/direct.rs`（删组合器段，留 fastpath 段 + 测试）
- Modify: `crates/server/src/pipeline.rs`（:547 调用点路径）
- Modify: `crates/semantic/seeds/metrics.sql`（销量 time_col → `t_sales_order.order_time`；若 Task 6 未外置则改 `meta.rs` seed_metrics 常量，行号以当时为准）

**Interfaces:**
- Consumes: T2-1、T7-1~4；direct.rs:53-107（try_compose）、:109-141（strip_annotations）、:143-197（find_path/find_edge）、:199-358（compose_sql/compose_sql_with）、:360-424（from_table_aliases/base_col_refs）、:426-463（has_residue/has_entity_residue）、:465-523（qualify_cols）
- Produces: `dms_semantic::DirectHit`；`dms_semantic::compose::{try_compose, qualify_cols}`；`dms_semantic::compose::path::*`（crate 内用）；时间桥读 `MetricDef.time_col`

- [ ] **Step 1: 纯搬移——compose 三文件落位（零行为变化）**

搬移映射（逐字，含全部 doc 注释与行内 Java/评测抓获引用）：

| direct.rs 行号 | 符号 | 落点 |
|---|---|---|
| :6-11 | `DirectHit` | `semantic/src/lib.rs` 根部（pub；compose/fastpath 共用，pipeline 消费） |
| :53-107 | `try_compose` | `compose/mod.rs`（pub；查询改走下述 registry 读口） |
| :109-141 | `strip_annotations` | `compose/mod.rs`（私有） |
| :143-183 | `find_path` | `compose/path.rs`（`pub(crate)`） |
| :185-197 | `find_edge` | `compose/path.rs`（`pub(crate)`） |
| :199-203 | `compose_sql`（#[cfg(test)] 简化入口） | `compose/mod.rs`（测试用，原样） |
| :205-358 | `compose_sql_with` | `compose/mod.rs`（私有；本步原样，Step 4 才动时间桥段） |
| :360-396 | `from_table_aliases` | `compose/path.rs`（`pub(crate)`） |
| :398-424 | `base_col_refs` | `compose/path.rs`（`pub(crate)`） |
| :426-455 | `has_residue` | `compose/guard.rs`（`pub(crate)`；fastpath 的 sales_breakdown 也在用 → 提交B 时 fastpath `use crate::compose::guard::has_residue`，保持 pub(crate) 即可） |
| :457-463 | `has_entity_residue` | `compose/guard.rs`（`pub(crate)`） |
| :465-523 | `qualify_cols` | `compose/mod.rs`（**pub**——提交B graph.rs 要复用做 table_scope filter 别名限定） |

搬移适配（仅三类，其余一字不改）：
1. 本地 `MetricDef/DimDef/JoinEdge` 结构体（:25-51）**删除**，改用 `crate::registry::types::{MetricDef, DimensionDef, JoinEdge}`；字段访问适配：direct 的 `e.lt/e.lc/e.rt/e.rc` → `e.left_table/e.left_col/e.right_table/e.right_col`（find_path/find_edge 内全量替换）；`DimDef` → `DimensionDef`（字段同名）。`card` 字段同名不动。
2. `try_compose` 签名与查询：`pub async fn try_compose(pg: &sqlx::PgPool, question: &str)` → `pub async fn try_compose(registry: &crate::registry::Registry, question: &str)`；函数体三段 `sqlx::query_as(...).fetch_all(pg)` 换成 `MetricDef::load_active(registry.pg()).await.ok()?` / `DimensionDef::load_active(...)` / `JoinEdge::load_active(...)`；table_scope 段换 `TableScope::load_all(registry.pg()).await.unwrap_or_default()` 后 `.into_iter().map(|s| (s.table_name, s.filter)).collect::<Vec<_>>()`。命中闭包 `hit` 与 `metrics.iter().find` / `dims.iter().find` 逻辑原样。
3. use 行：顶部 `use dms_kernel::nl::time::{time_predicate, fill_time_col};`（本步 time_window 仍留在 server direct.rs，compose 暂时 `use crate::…` 不可行——time_window 是 server 私有 fn。**处理**：本步在 compose/mod.rs 内就地补一个私有 `fn time_window(q: &str) -> Option<String> { time_predicate(q).map(|tpl| fill_time_col(&tpl, "order_time")) }`（与 direct.rs:988-990 逐字同），Step 4 删除它。direct.rs 原 time_window 保留给 fastpath 用，提交B 随 fastpath 搬走。）

- [ ] **Step 2: 搬移验证（零行为变化锁）**

direct.rs 删已搬段；direct.rs 顶部对残留 fastpath 段加 `use dms_semantic::compose::guard;` 不需要（has_residue 还在 direct.rs 内被 sales_breakdown 用——has_residue 已搬走，direct.rs 残留段临时 `use dms_semantic::compose::guard::has_residue;`… guard 是 pub(crate) 跨 crate 不可见。**处理**：has_residue 改 `pub`（guard.rs 内），它是纯函数守卫，跨 crate 可见无安全问题。server Cargo.toml 已有 dms-semantic path 依赖（Task 1 备好；缺则停下找 team-lead）。）

pipeline.rs:547 适配：
```rust
let registry = dms_semantic::registry::Registry::new(pg.clone()); // 薄包装零成本；AppState 化留 Task 9/10
let direct_hit = match dms_semantic::compose::try_compose(&registry, question).await {
    Some(h) => Some(h),
    None => crate::direct::try_direct(question),
};
```
（`DirectHit` 类型路径随 use 适配；`try_direct` 本步仍在 server。）

Run:
```powershell
cargo test --workspace 2>&1 | Select-String "test result"
```
Expected: 全绿；compose 全部测试（compose_province/compose_entity_question_skipped/compose_topn_and_no_time/compose_skips_mismatch/compose_fanout_rejected_for_sum/compose_qty_province_cross_base/compose_qty_category_time_bridge/dedup_subquery_for_detail_metric/dedup_skipped_when_col_not_in_keys/no_dedup_metric_unchanged/base_col_refs_extracts/table_scope_applied_to_bridge/table_scope_not_duplicated_for_metric_base/from_table_aliases_parses/qualify_bare_cols 共 15 个）随文件落在 compose 模块内，断言**一字未改**全过。**本步不 commit**（中间态，与 Step 3-6 同属提交A）。

- [ ] **Step 3: TDD 红——时间桥新语义测试先写**

compose/mod.rs tests 内改/新写（**先改测试，跑出红**）：

1. 适配 2 个既有测试（桥接别名 `o_time` → `t_time`，语义不变）：
   - `compose_qty_category_time_bridge`（:1213-1219）：断言串里 `JOIN t_sales_order o_time ON o_time.sales_order_code = d.sales_order_code` → `JOIN t_sales_order t_time ON t_time.sales_order_code = d.sales_order_code`；`o_time.order_time >=` → `t_time.order_time >=`。
   - `table_scope_applied_to_bridge`（:1267-1273）：`o_time.order_status NOT IN...` / `o_time.deleted_flag = 0` → `t_time.` 前缀两条。
   - 同时把这两个测试的 `qty_metric()` helper（:1104-1113）字段补 `time_col: "t_sales_order.order_time".into()`；`sales_metric()`（:1094-1103）补 `time_col: "order_time".into()`；`stock` 测试结构（:1184-1191）补 `time_col: String::new()`（registry MetricDef 含 time_col 字段，测试构造必须带）。
2. 新增 3 个测试（锁新语义）：
```rust
fn aftersales_metric() -> MetricDef {
    MetricDef {
        metric_code: "aftersales_count".into(),
        name: "售后单数".into(), aliases: vec![],
        source_table: "t_after_sales_order_header".into(),
        agg_expr: "COUNT(DISTINCT after_sales_code)".into(),
        scope_filter: "deleted_flag = 0".into(),
        time_col: "after_sales_time".into(),
        dedup_keys: String::new(), description: String::new(),
    }
}
fn aftersales_dim() -> DimensionDef {
    DimensionDef {
        dim_code: "as_shop".into(),
        name: "门店".into(), aliases: vec![],
        source_table: "t_after_sales_order_header h".into(),
        expr: "COALESCE(h.shop_name,'未知')".into(),
        description: String::new(),
    }
}

#[test]
fn time_bridge_reads_metric_time_col_on_base_table() {
    // 售后指标（time_col=after_sales_time，宿主=基表）带时间词：
    // 旧行为=桥不到 t_sales_order 返回 None 回落 LLM；新行为=基表自身时间列装配（修 bug）
    let sql = compose_sql(&aftersales_metric(), &aftersales_dim(), "本月售后单数按门店", &edges()).unwrap();
    assert!(sql.contains("h.after_sales_time >="), "{sql}");
    assert!(!sql.contains("order_time"), "{sql}");
    assert!(!sql.contains("t_sales_order"), "{sql}");
}

#[test]
fn time_bridge_empty_time_col_rejects_timed_question() {
    // 无时间语义指标（time_col 空）带时间词 → 不装配回落，绝不瞎填列
    let m = MetricDef { time_col: String::new(), ..aftersales_metric() };
    assert!(compose_sql(&m, &aftersales_dim(), "本月售后单数按门店", &edges()).is_none());
}

#[test]
fn time_bridge_unqualified_time_col_uses_base_alias() {
    // 基表=t_sales_order 的指标（time_col='order_time' 无限定）：行为与旧版完全一致（回归锁）
    let sql = compose_sql(&sales_metric(), &dim("省份", "COALESCE(NULLIF(cus.province,''),'未知')"), "本月销售额按省份", &edges()).unwrap();
    assert!(sql.contains("o.order_time >="), "{sql}");
}
```
（`dim()`/`edges()` helper 原样沿用；`DimensionDef`/`MetricDef` 若含 status 等更多字段，构造处按 Task 7 实际结构补齐——测试体其余不动。）

Run: `cargo test -p dms-semantic compose 2>&1 | Select-Object -Last 15`
Expected: 新测试 3 个红（`time_bridge_reads_metric_time_col_on_base_table` 因现状返回 None 而 unwrap panic；另两个按现状行为红/绿不定——`time_bridge_empty_time_col_rejects` 现状对售后桥不到也是 None 可能直接绿，`time_bridge_unqualified` 现状第一分支 FROM 无 t_sales_order 且 dim 基表=订单…按实际跑出的红为准记录），别名适配 2 个红（断言找 o_time 找不到）。**全红确认后进 Step 4。**

- [ ] **Step 4: 时间桥实现——compose_sql_with 时间窗段整体替换**

把 compose/mod.rs 内时间窗段（原 direct.rs:274-291）**整段替换**为：

```rust
    // 时间窗：列取 metric.time_col（注册表口径单一事实源，不再写死 order_time 桥 t_sales_order）。
    // time_col 语法：空=该指标无时间语义（带时间词不装配回落）；`col`=宿主为指标基表；
    // `table.col`=宿主为指定表（如销量 detail 指标的时间列在订单主表，须桥接）。
    let time_and = match time_predicate(question) {
        Some(tpl) => {
            let tc = m.time_col.trim();
            if tc.is_empty() {
                return None; // 快照类无时间语义，带时间词 → 回落 LLM，绝不瞎填列
            }
            let (host, col) = match tc.split_once('.') {
                Some((t, c)) => (t.to_string(), c.to_string()),
                None => (m_src.clone(), tc.to_string()),
            };
            let alias = if let Some((_, a)) = table_aliases.iter().find(|(t, _)| *t == host) {
                a.clone()
            } else if let Some((e, base_is_left)) = find_edge(&m_src, &host, edges) {
                let (c_base, c_host) = if base_is_left { (&e.lc, &e.rc) } else { (&e.rc, &e.lc) };
                from.push_str(&format!(
                    " JOIN {host} t_time ON t_time.{c_host} = {base_alias}.{c_base}"
                ));
                "t_time".to_string()
            } else {
                return None;
            };
            format!(" AND {}", fill_time_col(&tpl, &format!("{alias}.{col}")))
        }
        None => String::new(),
    };
```

连带点（本段替换的自洽所需，其余一律不动）：
- 删除 Step 1 临时补的私有 `time_window`（compose 不再使用）；`time_predicate`/`fill_time_col` 改从 `dms_kernel::nl::time` use（Step 1 已加则保留）。
- 排序判断 `d.expr.contains("DATE_FORMAT") || d.expr.contains("order_time")`（原 :349）**不动**——时间维度 expr 均为 `DATE_FORMAT(...)` 形态已覆盖，order_time 分支冗余但无害，改动无收益。
- 去重子查询安全门控（原 :298-305）天然覆盖新 time_and：`base_col_refs(&time_and, &base_alias)` 会把基表时间列纳入 refs 检查，列不在 dedup_keys 即拒绝装配——销量场景 time_and 引用桥表别名 `t_time.` 不受影响，售后场景基表无 dedup（dedup 空不进门控），行为自洽，无需改。

Run: `cargo test -p dms-semantic compose 2>&1 | Select-Object -Last 8`
Expected: 全绿（15 旧 + 3 新）。再跑 `cargo test --workspace 2>&1 | Select-String "test result"` 确认无连带红。

- [ ] **Step 5: 种子变更——销量 time_col 限定形式 + 生产库 UPDATE**

`seeds/metrics.sql` 中 `sales_qty` 行 `time_col` 值 `'order_time'` → `'t_sales_order.order_time'`（宿主显式化；其余 12 个指标 time_col 不动：order_time×3 宿主即基表、after_sales_time×2、created_time×3、空×2、apply_time 属 UNION 源 :220 已拒）。

生产库同步（种子幂等重播之外的即时生效手段；由执行者在判官前跑，PS 连 PG 或用现成 psql 通道）：
```sql
UPDATE meta.metric SET time_col = 't_sales_order.order_time' WHERE metric_code = 'sales_qty';
```
Run（判官自检）：`cargo test --workspace 2>&1 | Select-String "test result"` 再绿一次。

- [ ] **Step 6: 提交A 判官门禁——逐题人工判定（最高危红线执行点）**

```powershell
cargo build 2>&1 | Select-Object -Last 2
python tools/evaluation.py --baseline
python tools/regression.py | Out-File -Encoding utf8 tools\reg_commit_a.txt
python tools/evaluation.py | Out-File -Encoding utf8 tools\eval_commit_a.txt
```

逐题对照 `tools\reg_pretask8.txt` / 前态 eval 基线行：
1. **结果集变绿/不变的题** → 通过。
2. **结果集变化题**（eval 红 + regression 红）：逐题人工判定——
   - 判定「修复」：该题属「售后/费用/活动类带时间词」或「销量类 SQL 形状变化（别名 o_time→t_time）但结果集等价」。regression 的 SQL contains 断言若因别名改名变红 → 更新 case 文本（记录题号）；结果集等价性用 `evaluation.py --filter <题名>` 复核。
   - 判定「回归」：任何订单域指标（销售额/订单数/客单价/成交客户数）结果集变化 → **一律回滚本任务全部改动**，排查后重来（这些题新旧逻辑应逐字节等价，变了就是搬移事故）。
3. 把变化题填进 Task 8.D 的对照清单（题号/问句/旧路由/新路由/旧数/新数/判定）。

Expected: 无「回归」判定；全部「修复」判定有逐题记录。**有任何拿不准的题 → 停，报 team-lead 裁决，不猜。**

- [ ] **Step 7: 提交A commit**

```bash
git add crates/semantic crates/server/src/direct.rs crates/server/src/pipeline.rs crates/semantic/seeds tools/reg_commit_a.txt tools/eval_commit_a.txt
git commit -m "semantic: direct 组合器段迁 compose/{mod,path,guard} + 时间桥改读 metric.time_col（售后/费用/活动类时间口径修复，对照清单见 plan Task8.D）"
```

---

### Task 8.B: 【提交B】fastpath 迁 semantic + 有效订单状态码统一读 meta.table_scope

> 本任务 = 一个 git 提交。Step 1 纯搬移（fastpath 段 + direct.rs 删除）；Step 2-4 TDD 换状态码来源（3 处运行时内联 + graph.rs）；Step 5 判官；全过才 commit。

**Files:**
- Create: `crates/semantic/src/fastpath.rs`
- Modify: `crates/semantic/src/lib.rs`（挂 `pub mod fastpath;`）
- Modify: `crates/semantic/src/registry/mod.rs`（Registry 追加 `table_scope_filter` 读口）
- Delete: `crates/server/src/direct.rs`（搬空后删除）
- Modify: `crates/server/src/pipeline.rs`（:539/:549/:885-887 调用点路径）
- Modify: `crates/server/src/graph.rs`（:24-33 聚合 SQL 状态码段改读 table_scope）

**Interfaces:**
- Consumes: direct.rs 残留段（提交A 后）：`try_direct`(:564-568)、`sniff_doc_code`(:714-735)、`doc_binding`(:695-711)、`sales_breakdown`(:572-641)、`SalesDim`+`detect_sales_dim`+`consumed_words`(:643-692)、`agg_template`(:738-784)、`prev_window`(:823-839)、`time_window`(:988-990)、`Relation`+`detect_relation`+`strip_relation_words`(:14-22/:526-562)、`detect_top_n` facade；T7-1 Registry；kernel `detect_top_n_with`（Task 2 facade 已在）
- Produces: `dms_semantic::fastpath::{try_direct, detect_relation, Relation}`；`Registry::table_scope_filter`；运行时 `'0','108','199'` 仅存在于 seeds/测试/文档，业务 SQL 全从注册表读

- [ ] **Step 1: 纯搬移——fastpath.rs 落位 + direct.rs 删除（零行为变化）**

搬移映射（逐字）：

| direct.rs 行号 | 符号 | 落点 |
|---|---|---|
| :14-22 | `Relation` 枚举 | `fastpath.rs`（pub；pipeline.rs:885-887 消费三变体） |
| :526-549 | `detect_relation` | `fastpath.rs`（pub；pipeline.rs:539 消费） |
| :552-562 | `strip_relation_words` | `fastpath.rs`（私有） |
| :564-568 | `try_direct` | `fastpath.rs`（本步原样同步签名，Step 3 才改 async+Registry） |
| :572-641 | `sales_breakdown` | `fastpath.rs`（私有） |
| :643-651 | `SalesDim` | `fastpath.rs`（私有） |
| :653-668 | `consumed_words` | `fastpath.rs`（私有） |
| :670-692 | `detect_sales_dim` | `fastpath.rs`（私有） |
| :694-711 | `doc_binding` | `fastpath.rs`（私有；本步原样硬编码，提交C 才迁注册表） |
| :713-735 | `sniff_doc_code` | `fastpath.rs`（私有） |
| :737-784 | `agg_template` | `fastpath.rs`（私有） |
| :786-820 | `detect_top_n` | **不搬**——Task 2 已下沉 kernel 为 `detect_top_n_with(q, 200)` + facade；fastpath 内调用点改 `dms_kernel::nl::time::detect_top_n_with(q, 200)`，原 facade 函数随 direct.rs 删除 |
| :822-839 | `prev_window` | `fastpath.rs`（私有） |
| :988-990 | `time_window` | `fastpath.rs`（私有；sales_breakdown/agg_template 的 order_time 填充是「基表恒 t_sales_order」的正确口径，**不属于时间桥 bug**，不动） |

适配（仅 use 行）：`use dms_semantic::DirectHit;`→ 同 crate 直接 `use crate::DirectHit;`；`has_residue` 从 `crate::compose::guard::has_residue` use（提交A 已 pub）；时间族 `use dms_kernel::nl::time::{time_predicate, fill_time_col, detect_top_n_with};`。

direct.rs 内测试随函数搬：fastpath 族 14 个（doc_prefixes/sniff_in_sentence/agg_hits_month_sales/agg_order_count/agg_skips_dimension/agg_needs_time_and_metric/top_n_detect/sales_breakdown_top_n/sales_breakdown_dims/relation_detect/breakdown_rejects_value_filtered_question/breakdown_accepts_clean_questions/has_residue_basics + qualify_bare_cols 已随 compose 走过）；时间族 7 测试（time_recent_n_with_cn_numbers/time_quarter_and_half_year/time_explicit_month/time_relative_words/time_col_is_parameterized/cn_num_parses）**不搬直接删**——kernel 已有语义等价锁（Task 2.5）；`top_n_detect` 已在 kernel，server 侧删。

pipeline.rs 调用点：
- :539 `crate::direct::detect_relation(question)` → `dms_semantic::fastpath::detect_relation(question)`
- :549 `crate::direct::try_direct(question)` → `dms_semantic::fastpath::try_direct(question)`（本步同步签名）
- :885-887 `Relation::BuyersOfGoods(...)` 三变体 → `dms_semantic::fastpath::Relation::...`（use 行适配）

Run: `cargo test --workspace 2>&1 | Select-String "test result"`
Expected: 全绿；direct.rs 消失，测试净减 7（时间族，kernel 已有等价）；其余断言一字未改。**不 commit。**

- [ ] **Step 2: Registry 追加 table_scope_filter 读口 + TDD 红**

`registry/mod.rs` 追加（Task 7 门面之上的本任务新增，不需 Task 7 补）：
```rust
/// 表级标准口径读口（fastpath/graph 统一状态码来源）。Ok(None)=该表无登记行。
pub async fn table_scope_filter(&self, table: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT filter FROM meta.table_scope WHERE table_name = $1")
        .bind(table)
        .fetch_optional(&self.pg)
        .await
}
```

TDD 红：fastpath.rs tests 改 3 处签名断言（`sales_breakdown`/`agg_template` 即将加 `order_scope: &str` 参数——纯函数依赖注入，测试把原内联串当参数传入，**断言一字不改**）：
```rust
// 测试文件顶部加 helper：
const TEST_ORDER_SCOPE: &str = "deleted_flag = 0 AND order_status NOT IN ('0','108','199')";
// 调用点：sales_breakdown("本月销售额前5的省份") → sales_breakdown("本月销售额前5的省份", TEST_ORDER_SCOPE)
//         agg_template("本月销售额是多少") → agg_template("本月销售额是多少", TEST_ORDER_SCOPE)
```
（`agg_hits_month_sales` 的 `assert!(h.sql.contains("NOT IN ('0','108','199')"))` 等断言不动——注入的串与旧内联逐字同。）

Run: `cargo test -p dms-semantic fastpath 2>&1 | Select-Object -Last 6`
Expected: 编译错（参数数量不符）= 红。进 Step 3。

- [ ] **Step 3: try_direct 改 async+Registry；sales_breakdown/agg_template 换注入参数**

```rust
/// 确定性快路径（0-LLM）：单号直查 + 高频销售聚合模板。
/// 有效订单口径统一读 meta.table_scope（t_sales_order 行）；查询失败/缺行 → 模板族失命中
/// 回落 LLM+校正器兜底（宁缺毋滥，不拿残缺口径装配）；单号直查不依赖口径，不连坐。
pub async fn try_direct(registry: &crate::registry::Registry, question: &str) -> Option<DirectHit> {
    if let Some(h) = sniff_doc_code(question) {
        return Some(h);
    }
    let order_scope = match registry.table_scope_filter("t_sales_order").await {
        Ok(Some(f)) => f,
        other => {
            tracing::warn!("meta.table_scope 读 t_sales_order 失败/缺行({other:?})，快路径模板失命中回落");
            return None;
        }
    };
    sales_breakdown(question, &order_scope).or_else(|| agg_template(question, &order_scope))
}
```

`sales_breakdown`/`agg_template` 换签名（函数体仅一处适配）：
- `fn sales_breakdown(question: &str, order_scope: &str)`；`base_where` 由
  `format!("o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199'){time_and}")` 改为
  `format!("{}{time_and}", crate::compose::qualify_cols(order_scope, "o"))`。
  （qualify_cols 把裸列限定到 o.——读出的 filter 文本与旧内联裸列逐字同，限定后 SQL 形状与旧版逐字节等价。）
- `fn agg_template(question: &str, order_scope: &str)`；`base` 闭包由
  `"... WHERE deleted_flag = 0 AND order_status NOT IN ('0','108','199') AND {pred}"` 改为
  `"... WHERE {order_scope} AND {pred}"`（agg_template 的 SQL 无表别名，裸列直接用，与旧版逐字节等价）。

pipeline.rs:549 适配：`dms_semantic::fastpath::try_direct(&registry, question).await`（registry 在 :547 已构造，复用）。

Run: `cargo test --workspace 2>&1 | Select-String "test result"`
Expected: 全绿。

- [ ] **Step 4: graph.rs:30 聚合 SQL 改读 table_scope（fail-closed）**

graph.rs `sync`（:22-35）内，聚合边 SQL 的 `WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199')` 段改为运行时读：
```rust
// 有效订单口径统一读 meta.table_scope（建图是管理操作：缺行/查询失败 → bail，不建错图）
let order_scope: String = sqlx::query_scalar(
    "SELECT filter FROM meta.table_scope WHERE table_name = 't_sales_order'",
)
.fetch_optional(pg)
.await?
.ok_or_else(|| anyhow::anyhow!("meta.table_scope 缺 t_sales_order 行，fail-closed 拒绝建图"))?;
let order_scope = dms_semantic::compose::qualify_cols(&order_scope, "o");
```
SQL 字符串里 `WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199')` 替换为 `WHERE {order_scope}`（`AND d.deleted_flag = 0 AND ...` 后续段原样保留；format! 拼接，注意原 SQL 是多行 Rust 字符串，把该段换成 `{order_scope}` 占位并 format）。graph.rs 顶部 `use sqlx::{MySqlPool, PgPool, Row};` 已具备；pg 参数已在签名。

Run: `cargo build 2>&1 | Select-Object -Last 3` + `cargo test --workspace 2>&1 | Select-String "test result"`
Expected: 全绿（graph 无离线单测，连库行为由判官覆盖）。

- [ ] **Step 5: 提交B 判官门禁**

```powershell
cargo build 2>&1 | Select-Object -Last 2
python tools/evaluation.py --baseline
python tools/regression.py | Out-File -Encoding utf8 tools\reg_commit_b.txt
python tools/evaluation.py | Out-File -Encoding utf8 tools\eval_commit_b.txt
```

预期**零结果集变化**（读出的 filter 与旧内联逐字同、SQL 形状等价）。逐题对照前态：
- regression_cases.json 里含 `order_status NOT IN ('0','108','199')` SQL contains 断言的题应照常过（文本一致）。
- 任何题结果集变化 → 唯一合法解释是「table_scope 读到了与内联不同的库值」→ 查库确认 `SELECT * FROM meta.table_scope` 是否被人工改过；若库值与内联不一致，**停下报 team-lead**（说明种子与库已漂移，先对齐再谈统一），不擅自以哪边为准。

- [ ] **Step 6: 提交B commit**

```bash
git add crates/semantic crates/server tools/reg_commit_b.txt tools/eval_commit_b.txt
git commit -m "semantic: fastpath 迁入 + 有效订单状态码运行时三处内联统一读 meta.table_scope（graph 建图 fail-closed）；direct.rs 删除"
```

---

### Task 8.C: 【提交C】doc_binding 迁注册表 meta.doc_binding

> 本任务 = 一个 git 提交。新注册表 DDL+种子、Registry 读口、`sniff_doc_code` 匹配改库读；匹配语义与硬编码逐字等价（命中集不变 → 预期零判官波动）。

**Files:**
- Create: `crates/semantic/migrations/000N_doc_binding.sql`（N = Task 6 末位+1，8.0 Step 1 已记录）
- Create: `crates/semantic/seeds/doc_binding.sql`
- Modify: `crates/semantic/src/registry/types.rs`（DocBinding 行类型）、`registry/mod.rs`（doc_bindings 读口）
- Modify: `crates/semantic/src/fastpath.rs`（doc_binding/sniff_doc_code/try_direct 换注册表数据源）
- Modify: 种子编排（Task 6 的 seed runner 挂 doc_binding.sql；runner 归属以 Task 6 落地形态为准）

**Interfaces:**
- Consumes: T6-1 迁移编号、T7-1 Registry、fastpath.rs（提交B 后形态）
- Produces: `meta.doc_binding` 注册表（9 行种子）；`registry::types::DocBinding`；`Registry::doc_bindings`

- [ ] **Step 1: 迁移 + 种子**

`000N_doc_binding.sql`：
```sql
-- 单据前缀→(表, 主号列)绑定注册表（替代 direct.rs:695-711 硬编码 match）
-- match_kind: prefix=单号以 tag+'-' 开头（如 SPC）；hjxh_tag=HJXH- 后的单据类型字母段
CREATE TABLE IF NOT EXISTS meta.doc_binding(
  tag text PRIMARY KEY,
  table_name text NOT NULL,
  pk_col text NOT NULL,
  match_kind text NOT NULL,
  note text NOT NULL DEFAULT ''
);
```

`seeds/doc_binding.sql`（9 行，与硬编码一一对应）：
```sql
INSERT INTO meta.doc_binding(tag, table_name, pk_col, match_kind, note) VALUES
 ('SPC','t_winc_purchase_transfer','bill_code','prefix','winc 采购调拨单'),
 ('DXO','t_sales_order','sales_order_code','hjxh_tag','销售订单'),
 ('DSO','t_sales_order','sales_order_code','hjxh_tag','销售订单'),
 ('XO','t_sales_order','sales_order_code','hjxh_tag','销售订单'),
 ('SO','t_sales_order','sales_order_code','hjxh_tag','销售订单'),
 ('DRO','t_after_sales_order_header','after_sales_code','hjxh_tag','售后单'),
 ('RO','t_after_sales_order_header','after_sales_code','hjxh_tag','售后单'),
 ('DZD','t_account_bill_header','bill_code','hjxh_tag','对账单'),
 ('ZD','t_account_bill_header','bill_code','hjxh_tag','对账单')
ON CONFLICT (tag) DO UPDATE SET table_name=EXCLUDED.table_name, pk_col=EXCLUDED.pk_col,
 match_kind=EXCLUDED.match_kind, note=EXCLUDED.note;
```
种子编排挂接 + 生产库执行迁移与播种（执行者跑，语句即上面两文件内容）。

- [ ] **Step 2: DocBinding 类型 + Registry 读口 + TDD 红**

`registry/types.rs` 追加：
```rust
/// 单据前缀绑定行（meta.doc_binding）
pub struct DocBinding {
    pub tag: String, pub table_name: String, pub pk_col: String, pub match_kind: String,
}
```
`registry/mod.rs` 追加：
```rust
pub async fn doc_bindings(&self) -> Result<Vec<types::DocBinding>, sqlx::Error> {
    sqlx::query_as("SELECT tag, table_name, pk_col, match_kind FROM meta.doc_binding")
        .fetch_all(&self.pg)
        .await
        .map(|rows: Vec<(String, String, String, String)>| {
            rows.into_iter()
                .map(|(tag, table_name, pk_col, match_kind)| types::DocBinding { tag, table_name, pk_col, match_kind })
                .collect()
        })
}
```

TDD 红——fastpath.rs tests 的 `doc_prefixes`/`sniff_in_sentence` 适配为注册表驱动（**断言一字不改**，只换数据入口）：
```rust
fn test_bindings() -> Vec<DocBinding> {
    // 与 seeds/doc_binding.sql 9 行逐字对应
    [
        ("SPC", "t_winc_purchase_transfer", "bill_code", "prefix"),
        ("DXO", "t_sales_order", "sales_order_code", "hjxh_tag"),
        ("DSO", "t_sales_order", "sales_order_code", "hjxh_tag"),
        ("XO", "t_sales_order", "sales_order_code", "hjxh_tag"),
        ("SO", "t_sales_order", "sales_order_code", "hjxh_tag"),
        ("DRO", "t_after_sales_order_header", "after_sales_code", "hjxh_tag"),
        ("RO", "t_after_sales_order_header", "after_sales_code", "hjxh_tag"),
        ("DZD", "t_account_bill_header", "bill_code", "hjxh_tag"),
        ("ZD", "t_account_bill_header", "bill_code", "hjxh_tag"),
    ]
    .iter()
    .map(|(t, tb, pk, mk)| DocBinding { tag: t.into(), table_name: tb.into(), pk_col: pk.into(), match_kind: mk.into() })
    .collect()
}
// doc_binding("HJXH-DXO2026072300384") → doc_binding("HJXH-DXO2026072300384", &test_bindings())
// sniff_doc_code("帮我查下 HJXH-DXO2026072300384 这张单") → sniff_doc_code(..., &test_bindings())
```

Run: `cargo test -p dms-semantic fastpath 2>&1 | Select-Object -Last 6`
Expected: 编译错（签名不符）= 红。

- [ ] **Step 3: doc_binding/sniff_doc_code 换注册表匹配（语义逐字等价）**

```rust
/// 单据前缀 → (表, 主号列)。注册表驱动（meta.doc_binding），匹配语义与旧硬编码等价：
/// 先 prefix 族（tag+'-' 开头，如 SPC-），再 HJXH- 后字母段（hjxh_tag 族）。
fn doc_binding<'a>(code: &str, bindings: &'a [DocBinding]) -> Option<(&'a str, &'a str)> {
    let up = code.to_uppercase();
    for b in bindings.iter().filter(|b| b.match_kind == "prefix") {
        if up.starts_with(&format!("{}-", b.tag)) {
            return Some((b.table_name.as_str(), b.pk_col.as_str()));
        }
    }
    if let Some(rest) = up.strip_prefix("HJXH-") {
        let tag: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        return bindings
            .iter()
            .find(|b| b.match_kind == "hjxh_tag" && b.tag == tag)
            .map(|b| (b.table_name.as_str(), b.pk_col.as_str()));
    }
    None
}

/// 从问句抽单号（HJXH-字母+数字 / 注册表 prefix 族），命中即出单据卡（SELECT * 单行）。
fn sniff_doc_code(question: &str, bindings: &[DocBinding]) -> Option<DirectHit> {
    // 函数体与旧版逐字同，仅 doc_binding(t) 调用改 doc_binding(t, bindings)
}
```

`try_direct` 头部适配（bindings 与 order_scope 同门拿取；查询失败/空表 → 单号族失命中不连坐模板族）：
```rust
pub async fn try_direct(registry: &crate::registry::Registry, question: &str) -> Option<DirectHit> {
    match registry.doc_bindings().await {
        Ok(b) if !b.is_empty() => {
            if let Some(h) = sniff_doc_code(question, &b) {
                return Some(h);
            }
        }
        other => tracing::warn!("meta.doc_binding 读取失败/空表({other:?})，单号直查失命中"),
    }
    // order_scope 段与提交B 逐字同
}
```

Run: `cargo test --workspace 2>&1 | Select-String "test result"`
Expected: 全绿（doc_prefixes/sniff_in_sentence 断言未动）。

- [ ] **Step 4: 提交C 判官门禁**

```powershell
cargo build 2>&1 | Select-Object -Last 2
python tools/evaluation.py --baseline
python tools/regression.py | Out-File -Encoding utf8 tools\reg_commit_c.txt
python tools/evaluation.py | Out-File -Encoding utf8 tools\eval_commit_c.txt
```
Expected: 零结果集变化（命中集与硬编码逐字同）。单据类题（regression 含 HJXH 单号题）路由仍 `direct-doc`。任何单号失命中 → 先查库 `SELECT * FROM meta.doc_binding` 是否 9 行已播，再查 warn 日志。

- [ ] **Step 5: 提交C commit**

```bash
git add crates/semantic tools/reg_commit_c.txt tools/eval_commit_c.txt
git commit -m "semantic: doc_binding 单号→表绑定迁 meta.doc_binding 注册表（9 行种子，匹配语义与硬编码等价）"
```

---

### Task 8.D: 终验 + 「数值变化对照清单」定稿（spec 5.3 第 2 条硬要求）

**本清单是上线前置条件**：提交A 改变了售后/费用/活动类带时间词问句的答案来源与数值。三个提交的判官产物汇总后，按下表逐题填全，交用户确认后才算 Task 8 完成。

- [ ] **Step 1: 口径变化对照清单（模板 + 两个已坐实的 SQL 对照示例）**

| 题号 | 问句 | 旧路由/旧时间列 | 新路由/新时间列 | 旧数 | 新数 | 判定(修复/回归) | 为什么现在的错 |
|---|---|---|---|---|---|---|---|
| 示例① | 本月售后单数按门店 | llm（组合器桥不到 t_sales_order 返回 None） | direct-agg / `h.after_sales_time` | 填实测 | 填实测 | 修复 | 售后单的时间语义是售后发生/申请时间（after_sales_time）；LLM 路径易拿订单 order_time 过滤或漏 `deleted_flag`，把「本月售后」错算成「本月订单的售后」 |
| 示例② | 本月市场费用按月份 | llm（同上 None） | direct-agg / `created_time` | 填实测 | 填实测 | 修复 | 费用口径以费用单创建时间（created_time）为准，与订单时间无关 |

示例①的 SQL 对照（修复前后同一句问句）：
```sql
-- 旧（LLM 路径曾被抓到的典型错法）：拿订单时间过滤售后，且强制 JOIN 丢掉无订单关联的售后单
SELECT h.shop_name, COUNT(DISTINCT h.after_sales_code) FROM t_after_sales_order_header h
JOIN t_sales_order o ON o.sales_order_code = h.sales_order_code
WHERE o.order_time >= DATE_FORMAT(CURDATE(),'%Y-%m-01') GROUP BY h.shop_name;
-- 新（组合器按 metric.time_col 装配）：时间列=售后表自身的 after_sales_time，表级口径 deleted_flag=0 由 metric.scope_filter 带入
SELECT COALESCE(h.shop_name,'未知') AS `门店`, COUNT(DISTINCT h.after_sales_code) AS `售后单数`
FROM t_after_sales_order_header h
WHERE h.deleted_flag = 0 AND h.after_sales_time >= DATE_FORMAT(CURDATE(),'%Y-%m-01')
  AND h.after_sales_time < DATE_ADD(DATE_FORMAT(CURDATE(),'%Y-%m-01'), INTERVAL 1 MONTH)
GROUP BY COALESCE(h.shop_name,'未知') ORDER BY `售后单数` DESC LIMIT 200;
```

填写规则：从 `tools\eval_commit_a.txt` / `reg_commit_a.txt` 与前态基线的逐题 diff 中取数；凡「结果集变化」的题必须入表，一行不漏；判定为「回归」的题不允许存在（有则回滚提交A 重来）。

- [ ] **Step 2: 全量验收清单**

```powershell
cargo test --workspace 2>&1 | Select-String "test result"          # 全绿（compose 15+3、fastpath 14、时间族净减 7 有 kernel 等价锁）
cargo build 2>&1 | Select-Object -Last 3                            # 全 workspace 编译
Select-String -Path crates/semantic/src/compose/*.rs, crates/semantic/src/fastpath.rs, crates/server/src/graph.rs -Pattern "108"   # 期望：仅测试构造/helper 注释命中，业务 SQL 拼装路径零命中
Select-String -Path crates/server/src/*.rs -Pattern "crate::direct" # 期望空（direct.rs 已删除，无残留引用）
cargo tree -p dms-semantic --prefix none 2>&1 | Select-String "dms-" # 见 kernel/connector，不见 server/policy（依赖方向不变）
Test-Path crates/server/src/direct.rs                               # 期望 False
```

- [ ] **Step 3: 文件粒度对照 spec 5.4 甜区**

compose/mod.rs 预计 ~450 行（含测试）、path.rs ~230、guard.rs ~60、fastpath.rs ~430（含测试）。guard.rs 低于 80 行但守卫逻辑有独立测试且被两模块共用，保留独立文件（spec 5.4「<80 且无独立测试则并回」——它有测试，不并）。若实测 mod.rs 超 500，把 tests 段拆 `compose/tests.rs`（`#[cfg(test)] mod tests;` 引入），不在本 plan 预设。

- [ ] **Step 4: 收尾提交（对照清单/判官产物归档）**

```bash
git add tools docs/superpowers/plans/2026-07-27-task08-direct-decompose.md
git commit -m "Task8 终验：三提交判官产物 + 数值变化对照清单定稿"
```

---

## 自检（已执行）
- **spec 覆盖**：迁移步 8 三子件齐全——direct.rs 解体 compose/{mod,path,guard}.rs+fastpath.rs（8.A/8.B Step 1 搬移映射表逐行落号）✓；时间桥读 metric.time_col（8.A Step 4 新逻辑全文 + 8.A Step 5 种子限定形式）✓；状态码统一读 table_scope（8.B 运行时 3 处 + graph.rs）✓；doc_binding 迁注册表（8.C DDL/种子/读口/匹配等价）✓；三个独立提交各自跑 regression+evaluation 逐题人工判定（8.A/8.B/8.C 判官 Step）✓；5.3 第 2 条对照清单（8.D 模板+SQL 对照示例）✓；3.1 fastpath「单号→下钻→聚合」与 compose「注册表驱动组合器+BFS+残留守卫」目录形态一致 ✓。
- **6 处内联核验**（grep `108` 全仓）：运行时业务 SQL 内联 = graph.rs:30、direct.rs:592、direct.rs:774 三处（8.B 全覆盖）；meta.rs:365 是 table_scope 种子=事实源本体；meta.rs:768/775/781 是 metric 种子 scope_filter（处理见裁决 1）；corrector.rs:1025 `ORDER_SCOPE` 是 grep 出的第 7 处（Task 7 已迁 correct/caliber.rs，处理见裁决 2）；测试断言/regression_cases/eval gold_sql/文档中的出现属锁或文档，不动 ✓。
- **时间桥行为对照核验**（逐指标过一遍）：order_time×3（宿主=基表，新旧逐字节等价）✓；销量（限定形式 host=t_sales_order，同基表场景仅别名 o_time→t_time，跨基表场景 FROM 已有 order 用其别名，等价）✓；after_sales_time×2/created_time×3（旧 None 回落 → 新装配，= 对照清单填写对象）✓；库存×2（子查询口径早已 :217 拒，不变）✓；开票（UNION 源 :220 拒，不变）✓；无时间词问句（不加过滤，不变）✓。
- **等价性论证**：提交B 读出的 filter 文本与旧内联逐字同（种子同源），qualify_cols 限定后 SQL 形状逐字节等价 → 预期零判官波动，任何波动即库值漂移信号（8.B Step 5 处置）✓；提交C 匹配算法分支顺序（prefix 族先、HJXH 字母段后）与硬编码逐字同，9 行种子与 9 个 match 臂一一对应 ✓。
- **TDD 节奏**：每提交均「纯搬移零变化锁 → 改测试出红 → 实现转绿 → 判官逐题判定」；46 权限单测与本任务无交集（不动 policy）✓。
- **红线**：不新增第三方依赖 ✓；组合器/快路径算法逐字搬（仅点名字段名/use/签名适配）✓；fail-closed（graph 缺行 bail、fastpath 缺行回落+warn）✓；DirectHit 形状不变、Answer 协议不碰（Task 9 领域）✓。
- **占位符扫描**：migrations 编号 000N 以 8.0 实测为准（非 TBD，是 gate 产物）；MetricDef/DimensionDef 结构字段以 Task 7 交付为准（T7-2/3/4 已列核对命令）；种子文件路径两态指引（Task 6 外置前后）已写明 ✓。

## 需 team-lead 裁决（阻塞前先确认）
1. **metric 种子 scope_filter 双写**：seeds/metrics.sql 的 sales_amount/order_count/avg_order_value 三行 scope_filter 与 table_scope 的 t_sales_order 行文本重复。本 plan 取「**保留双写 + 8.B Step 5 判官核库防漂移**」（改动最小；metric.scope_filter 是指标级口径设计位，LLM 口径卡与组合器都在消费）。备选「清空由 table_scope 派生」要动组合器装配 + prompt 口径卡 + corrector 三处消费点，面大不建议。请确认。
2. **corrector 的 ORDER_SCOPE（第 7 处内联）**：caliber 校正器向 LLM SQL 补有效订单口径的常量（现 correct/caliber.rs，Task 7 迁）。本 plan 未动它（属 Task 7 领域、且校正器语义是「补 t_sales_order 的表级口径」与 table_scope 行完全同义）。是否随提交B 一并改读 `ctx.registry.table_scope_filter("t_sales_order")`？建议改（否则口径仍双写一处），但会多动一个 Task 7 交付物，请裁决。
3. **time_col 点分限定语法**（`t_sales_order.order_time`，8.A Step 4/5）：零 DDL 变更、只改一行种子值即表达「时间列宿主表」。备选是 `meta.metric` 加 `time_table` 列（DDL 迁移 + types 加字段 + 种子 12 行回填），更重。本 plan 取前者，请确认。
4. **Relation/detect_relation 归属**：spec 3.1 的 fastpath 注释只写「单号→下钻→聚合」未含图关系识别；本 plan 把它随 fastpath.rs 走（0-LLM 识别族同源，留 server 会产生 direct.rs 残骸）。若 team-lead 认为图识别该留 server 靠 graph.rs，请指出（改动仅 8.B Step 1 映射表两行）。
5. **meta.doc_binding 迁移编号衔接**：8.C 新增 `000N_doc_binding.sql` 接 Task 6 末位。若 Task 6 的迁移编排是「单文件 0001_init 全量」而非逐文件递增，doc_binding 该追加进 0001 还是开 0002，以 Task 6 落地形态为准——请确认 Task 6 的迁移文件策略。

## 备注（Windows 构建 + 判官环境）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）

判官前置：docker `dms-ai-pg` 在跑（PG+AGE）、MySQL 可达、`cargo build` 已出 `target/debug/dms-ai-server.exe`。判官连生产只读库，执行者确认环境后手动跑，plan 不自动连库。`evaluation.py` 题间自带 2s 节流、抖动退避重试 3 次；`--baseline` 只归档汇总行（rate/p50/p95），逐题对照用各提交留存的 `reg_commit_*.txt`/`eval_commit_*.txt` 文本 diff。



