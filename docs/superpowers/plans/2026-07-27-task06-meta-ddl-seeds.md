# Task 6：meta 解体① DDL 版本化 + 种子外置 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 meta.rs 按 ';' 朴素切分的 DDL（:11-161，25 条语句）拆成 dms-semantic/migrations/0001-0008 版本化迁移（sqlx 已有 migrate feature，零新依赖），把 9 组种子 const（meta.rs:286-1036）+ scope_binding 32 表（inject.rs:48-92）外置成 dms-semantic/seeds/*.sql 幂等灌入；**种子对拍全等 = 合并硬门禁**；生产库先插 baseline 再部署。

**Architecture:** meta.rs 解体第一刀（spec 迁移步 6）。新建 dms-semantic crate（kernel+connector 依赖，不依赖 policy）；migrations/ 与 seeds/ 落其 crate 根，src 只留 ddl.rs（migrate 接线）+ seed.rs（编排 + sync_elements 搬入）。server 经 path 依赖消费；meta.rs 删除 migrate/seed/seed_*/sync_elements/upsert_element，其余（sync_schema/recall_*）留 Task 7。

**Tech Stack:** Rust workspace、cargo、sqlx 0.8（migrate feature + `sqlx::raw_sql` 多语句执行）、psql/createdb、Python 3.13 + psycopg（仅 tools 对拍脚本，不入 Cargo 依赖）。

## Global Constraints

- **种子对拍全等才可合并**（spec 5.2 门禁原话）：10 张种子表 + meta.element 逐行逐列比对，含 NULL vs 空串、text[] 数组顺序。不全等不许合并，没有例外。
- **生产库 baseline 前置**（spec 5.3 第 3 条）：16 张 meta.* 已在生产 PG 存在。部署新二进制前必须先跑 `tools/baseline_sqlx_migrations.py` 插入 1-8 版本记录（含正确 SHA-384 checksum），否则首启动重跑全部迁移——虽多为 IF NOT EXISTS，但 meta.rs:53/73/75/113 的 ALTER TABLE ADD COLUMN 在旧 PG 版本不带 IF NOT EXISTS 会报错致启动失败。
- **种子即真相，转换逐条核对**：漏条目/中文标点/单引号转义错不编译报错，只在召回时静默降级。条数核对表（6.3 Step 1）逐组打钩 + 对拍兜底，双保险。条数一律以源码实数为准，不抄任何文档数字（任务书口径与源码实测有出入：实测 WARNS 23/KW_FORCE 36/METRICS 12/DIMENSIONS 9/scope_binding 32）。
- **SQL 语义逐字保留**：ON CONFLICT 子句、WHERE NOT EXISTS 形态、UPDATE-only（warns 不插新行）、value_map 105 条含重复 PK（后写覆盖 = 旧循环覆盖）全部原样转写；aliases 用 `ARRAY['a','b']` 字面量；空串保持 `''` 不得变 NULL；scoped/global 的可空列保持 NULL 不得变 `''`。
- **种子文件不做分号切分**：seed.rs 一律 `sqlx::raw_sql(include_str!("../seeds/X.sql")).execute(pg)` 整文件多语句执行（PG simple query 协议原生支持），从根上消灭 meta.rs:157 的 `split(';')` 脆弱性。
- **seed 编排顺序逐字不动**：warns → kw_force → metrics → dimensions → value_maps → join_edges → table_scopes → pitfalls → terms → sync_elements → scope_binding（对齐 meta.rs:312-357 + main.rs:45）。
- **scope_binding 双真相源受控**：灌库真相迁到 seeds/scope_binding.sql；kernel::builtin_rules 保留作 PG 缺席兜底（rule_of/snapshot 回退），两者漂移由 6.3 的 drift 单测锁死。
- **不新增第三方依赖**：sqlx 只加已有 feature `migrate`；tools/*.py 的 psycopg 是开发机一次性 pip 安装，不进任何 manifest。
- **既有 157 server 单测零改动零减少**；kernel/policy/connector 既有测试不减少。
- Windows 构建须前缀 MinGW bin 路径（见文末备注），cargo 命令走 PowerShell 不走 Bash。

## 上游契约清单（Task 6.0 逐项核对；缺失即降级/升级）

| # | 契约 | 现状 | 缺失处置 |
|---|---|---|---|
| S1 | dms-semantic crate 骨架（Cargo.toml + src/lib.rs，sqlx features 含 postgres+migrate，path 依赖 kernel/connector） | **不存在**（workspace 目前仅 crates/server，Task 1 pending） | 6.2 Step 1 自建最小骨架（属 Task 1 范围，见裁决 1） |
| S2 | `dms_kernel::builtin_rules() -> HashMap<String, TableRule>` + `TableRule`/`Binding`/`OwnerKind` pub | Task 2 交付物（plan-t5 K4 已锁定） | 缺失则从 server inject.rs 现位置读真相转换，drift 单测改为对 inject.rs 文本 grep，标注「Task 2 合并后换回类型比对」 |
| S3 | seed_rules 当前落点（inject::seed_rules 或 dms_policy::rules::seed_rules，取决于 Task 5 是否合并） | 实测仍在 `crates/server/src/inject.rs:104-130` | 6.3 按 grep 实点删除并接线，两态都给了命令 |
| S4 | Python 3 + psycopg（对拍/baseline 脚本） | Python 3.13.5 在；psycopg 缺 | `pip install "psycopg[binary]"` 一次性安装（6.1 Step 1） |
| S5 | 本地 PG 环境（A/B 对拍库，版本尽量对齐生产） | 未核对 | 6.1 Step 2 二选一：docker postgres 或本机已装 PG 建两个 database |
| S6 | server CLI 入口形态 | `main.rs` 手工 args 匹配（`meta sync`/:71、`meta autodiscover`/:82） | 6.1 新增 `meta seed-only` 同风格插入 |

---

### Task 6.0: 上游契约核对（gate，不写代码）

**Files:**
- 只读：`crates/`、`crates/server/src/inject.rs`、`Cargo.toml`

- [ ] **Step 1: 逐项核对 S1-S6**

Run（PowerShell，前缀 MinGW，下同）:
```powershell
Test-Path crates/semantic/Cargo.toml                                                # S1
Select-String -Path crates/kernel/src/*.rs -Pattern "pub fn builtin_rules"          # S2
Select-String -Path crates/server/src/inject.rs,crates/policy/src/rules.rs -Pattern "pub async fn seed_rules" -ErrorAction SilentlyContinue  # S3 实点
python -c "import psycopg; print(psycopg.__version__)"                              # S4
Get-Command psql,createdb -ErrorAction SilentlyContinue                             # S5
```
Expected: S1=False 走自建；S3 命中 inject.rs:104；S4/S5 任一缺失先补（Step 2）。

- [ ] **Step 2: 缺项处理**

S4 缺 → `pip install "psycopg[binary]"`。S5 缺 → 6.1 Step 2 用 docker（`docker run -d --name dms-pg -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres:16`，版本以生产 `SELECT version();` 为准调整）。S2 缺失且 inject.rs 也已删 → 停止，升级 team-lead（Task 2/5 状态不明）。

---

### Task 6.1: 对拍 harness 先行（seed-only 子命令 + seed_diff.py + A 库旧种子快照）

**Files:**
- Modify: `crates/server/src/main.rs`（:70 前插入 seed-only 分支）
- Create: `tools/seed_diff.py`

**Interfaces:**
- Consumes: 现状 bootstrap_meta（main.rs:42-49）
- Produces: `meta seed-only [pg_url]` 子命令；对拍脚本可比对 A/B 两库

**动机（TDD）:** 先建安全网再动刀。改动前用旧代码灌 A 库并 pg_dump 存档；之后每一步转换都能立刻对拍。seed-only 同时是长期部署资产（重置种子不碰 MySQL）。

- [ ] **Step 1: main.rs 加 seed-only 子命令**

在 :70 `// M2 子命令：meta sync` 注释之前插入（风格对齐邻近分支；第三参数可显式传 PG URL，免改生产 settings.json）：

```rust
// 子命令：meta seed-only [pg_url] —— 只连 PG 跑元数据引导（migrate+seed+rules），
// 供种子对拍/部署重置种子用；不碰 MySQL。
if args.len() >= 3 && args[1] == "meta" && args[2] == "seed-only" {
    let pg = if let Some(url) = args.get(3) {
        db::pg_pool(url).await?
    } else {
        db::pg_pool(&cfg.pg_url).await?
    };
    bootstrap_meta(&pg).await?;
    println!("{}", serde_json::json!({ "ok": true }));
    return Ok(());
}
```

Run: `cargo build -p dms-ai-server 2>&1 | Select-Object -Last 2`
Expected: Finished 无 error。

- [ ] **Step 2: 备 A/B 两个空库**

```powershell
# 同一 PG 实例建两个库（实例版本尽量对齐生产；docker 见 6.0 Step 2）
createdb -h localhost -U postgres seed_a
createdb -h localhost -U postgres seed_b
```
连接串：`postgres://postgres:postgres@localhost:5432/seed_a`（密码按本机实际）。

- [ ] **Step 3: 写 tools/seed_diff.py（完整脚本，一次写全）**

用法 `python tools/seed_diff.py <pg_url_a> <pg_url_b> [--prepare]`；`--prepare` 先向两库 table_doc 插 warns 覆盖的 23 个表名占位行（WARNS 是 UPDATE-only，table_doc 为空则两边 warn 全空、比对失效），随后做 schema 比对 + 种子全量比对：

```python
#!/usr/bin/env python3
"""种子/模式对拍：旧 seed 灌 A 库 vs 新 seed 灌 B 库，逐表全量比对（含 NULL vs 空串）。
用法: python tools/seed_diff.py <pg_url_a> <pg_url_b> [--prepare]
依赖: pip install "psycopg[binary]"
退出码: 0=全等 1=有差异（差异打到行级） 2=用法/连接错误
"""
import sys
import psycopg

# warns.sql 覆盖的表（与 meta.rs WARNS 23 条一一对应；占位行只为让 UPDATE 生效）
WARN_TABLES = [
    "t_sales_order_detail", "t_sales_order_his_detai", "t_marketing_goods", "t_goods",
    "t_sales_order_import", "t_customer_balance", "t_winc_stock_report", "t_warehouse_manage",
    "t_device_requisition", "t_device_receive_item", "t_sales_order_short", "t_market_claim_header",
    "t_market_activity_promoter_expense", "t_winc_sale_transfer", "t_winc_stock_transfer",
    "t_market_marketing_expense", "t_device_demand_apply_detail", "t_marketing_zone_product",
    "t_new_market_product", "t_customer_price", "t_activity_promoter_fee", "t_master_shop",
    "t_invoice_apply_header",
]

# (表, 主键列)：10 张种子表 + element（sync_elements 派生，同逻辑应全等）
SEED_TABLES = [
    ("table_doc", "table_name"),      # 只比 warn 非空行 + 占位行（全表比即可，两边同起点）
    ("kw_force", "keyword"),
    ("metric", "metric_code"),
    ("dimension", "dim_code"),
    ("value_map", "table_name, column_name, name"),
    ("term", "term"),
    ("pitfall", "id"),
    ("table_scope", "table_name"),
    ("join_edge", "left_table, left_col, right_table, right_col"),
    ("scope_binding", "table_name"),
    ("element", "element_id"),
]

def prepare(conn):
    with conn.cursor() as cur:
        for t in WARN_TABLES:
            cur.execute(
                "INSERT INTO meta.table_doc(table_name) VALUES (%s) ON CONFLICT (table_name) DO NOTHING",
                (t,),
            )
    conn.commit()

def fetch(conn, sql, params=()):
    with conn.cursor() as cur:
        cur.execute(sql, params)
        return cur.fetchall()

def norm(v):
    # psycopg 把 text[] 解为 list、NULL 解为 None、空串为 ''——repr 级比对天然区分三者
    if isinstance(v, list):
        return tuple(v)
    return v

def compare_schema(a, b):
    q = """
      SELECT table_schema, table_name, column_name, data_type, is_nullable,
             COALESCE(column_default, '<NULL>'), ordinal_position
      FROM information_schema.columns
      WHERE table_schema = 'meta'
      ORDER BY table_name, ordinal_position
    """
    ra, rb = fetch(a, q), fetch(b, q)
    if ra == rb:
        print("[schema] meta.* 列级全等")
        return True
    sa, sb = set(ra), set(rb)
    for r in sorted(sa - sb):
        print(f"[schema] 仅 A: {r}")
    for r in sorted(sb - sa):
        print(f"[schema] 仅 B: {r}")
    return False

def compare_tables(a, b):
    ok = True
    for table, pk in SEED_TABLES:
        q = f"SELECT * FROM meta.{table} ORDER BY {pk}"
        ra = [tuple(norm(v) for v in row) for row in fetch(a, q)]
        rb = [tuple(norm(v) for v in row) for row in fetch(b, q)]
        if ra == rb:
            print(f"[seed] meta.{table}: {len(ra)} 行全等")
            continue
        ok = False
        print(f"[seed] meta.{table}: 不等！A={len(ra)} 行 B={len(rb)} 行")
        for i, (x, y) in enumerate(zip(ra, rb)):
            if x != y:
                print(f"  首处差异 行{i}: A={x!r} B={y!r}")
                break
        if len(ra) != len(rb):
            sa, sb = set(ra), set(rb)
            for r in sorted(sa - sb)[:5]:
                print(f"  仅 A: {r!r}")
            for r in sorted(sb - sa)[:5]:
                print(f"  仅 B: {r!r}")
    return ok

def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    do_prepare = "--prepare" in sys.argv
    with psycopg.connect(sys.argv[1]) as a, psycopg.connect(sys.argv[2]) as b:
        if do_prepare:
            prepare(a)
            prepare(b)
            print("[prepare] 两库 table_doc 已插 23 个占位行")
        ok = compare_schema(a, b) & compare_tables(a, b)
    print("RESULT:", "ALL EQUAL" if ok else "DIFF FOUND")
    return 0 if ok else 1

if __name__ == "__main__":
    sys.exit(main())
```

> 注意 pitfall 主键 bigserial：A/B 必须同顺序插入 id 才一致——seeds/pitfalls.sql 条目顺序必须 = LESSONS const 顺序（6.3 核对表锁）。element.embedding 两边均 NULL 一致。

- [ ] **Step 4: 改动前灌 A 库并存档**

> 顺序陷阱：warns 是 UPDATE-only，table_doc 为空时首轮打不上；但占位行又需要 migrate 先建表。故必须「建表 → 插占位 → 再灌」三段：

```powershell
# 当前 HEAD 还是旧代码（meta.rs 未动）
$A = "postgres://postgres:postgres@localhost:5432/seed_a"
# 1) 空库先跑一次 seed-only 建表（此时 warns 因 table_doc 空而打不上，属预期）
cargo run -p dms-ai-server -- meta seed-only $A
# 2) 插 23 个占位行
python tools/seed_diff.py $A $A --prepare
# 3) 再跑一次 seed-only（幂等：warns 这次打上，其余 ON CONFLICT 刷新同值）
cargo run -p dms-ai-server -- meta seed-only $A
# 4) 存档防污染
pg_dump -h localhost -U postgres -d seed_a -f tools/seed_a_snapshot.sql
```
Expected: 第 3 步后 `SELECT count(*) FROM meta.table_doc WHERE warn <> ''` = 23。B 库在 6.3 Step 6 用完全相同的四步灌（那时代码已换新实现）。

- [ ] **Step 5: 提交**

```bash
git add crates/server/src/main.rs tools/seed_diff.py
git commit -m "对拍 harness：meta seed-only 子命令 + tools/seed_diff.py（schema+种子全量比对，含 NULL vs 空串）"
```

---

### Task 6.2: DDL 版本化（semantic 骨架 + migrations 8 文件 + sqlx migrate 接线）

**Files:**
- Create: `crates/semantic/Cargo.toml`、`crates/semantic/src/lib.rs`、`crates/semantic/src/ddl.rs`
- Create: `crates/semantic/migrations/0001_init.sql` … `0008_join_edge_scope_binding.sql`
- Modify: `Cargo.toml`（members 追加）、`crates/server/Cargo.toml`（path 依赖）、`crates/server/src/main.rs`（3 处 migrate 调用点）
- Modify: `crates/server/src/meta.rs`（删 migrate():10-161）

**Interfaces:**
- Consumes: meta.rs:11-161 的 25 条 DDL（逐字搬移，含全部 `--` 注释）
- Produces: `dms_semantic::ddl::migrate(&PgPool)`

- [ ] **Step 1: 建 dms-semantic 最小骨架（S1 降级：Task 1 未交付，本步自建，见裁决 1）**

根 `Cargo.toml` members 改 `["crates/server", "crates/semantic"]`。

`crates/semantic/Cargo.toml`：
```toml
[package]
name = "dms-semantic"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = { workspace = true }
tracing = { workspace = true }
sqlx = { version = "0.8", default-features = false, features = [
    "runtime-tokio",
    "tls-rustls",
    "postgres",
    "migrate",
] }

[dev-dependencies]
dms-kernel = { path = "../kernel" }
```
> dms-kernel 仅 6.3 drift 单测用，挂 dev-dependencies 保持语义层运行时零 policy/kernel 耦合（spec 45 行 semantic=kernel+connector 是目标态；Task 2 未合并时该 path 不存在——则先删掉 dev-dependencies 整段，drift 单测走 6.3 Step 5 的文本 grep 降级）。`migrate` 是 sqlx 已有 feature，非新依赖。

`crates/semantic/src/lib.rs`：
```rust
//! dms-semantic：业务知识语义层（meta.rs 解体落点）。本任务先交付 DDL 版本化 + 种子外置。
pub mod ddl;
pub mod seed;
```

`crates/server/Cargo.toml` [dependencies] 追加：`dms-semantic = { path = "../semantic" }`。

`crates/semantic/src/seed.rs` 本步先空壳（`//! 种子编排，6.3 填充`）。

- [ ] **Step 2: 拆 8 个迁移文件（按 DDL 注释可见的演进批次；内容 = meta.rs 行号区间逐字照抄，含注释行）**

| 文件 | meta.rs 行区间 | 含语句 |
|---|---|---|
| `0001_init.sql` | :12-51 | CREATE SCHEMA meta；CREATE EXTENSION pg_trgm / vector；table_doc + idx_table_doc_trgm；column_doc；kw_force；pitfall；sql_exemplar |
| `0002_exemplar_review_status.sql` | :52-53 | ALTER sql_exemplar ADD status（复核态注释随行） |
| `0003_term_and_metric.sql` | :54-71 | term；metric |
| `0004_metric_time_col_dedup_keys.sql` | :72-75 | ALTER metric ADD time_col；ADD dedup_keys |
| `0005_scope_dimension_value_map.sql` | :76-101 | table_scope；dimension；value_map |
| `0006_element.sql` | :102-113 | element；ALTER element ADD embedding |
| `0007_correction_failure_logs.sql` | :114-132 | correction_log + idx；failure_log + idx |
| `0008_join_edge_scope_binding.sql` | :133-155 | join_edge；scope_binding |

规则：① 语句逐字，连 `--` 注释一起搬；② 每文件末尾不留孤立分号；③ 文件名 `<4位版本>_<描述>.sql`，sqlx 解析 description 时把 `_` 转空格（baseline 脚本同规则，必须一致）。

- [ ] **Step 3: ddl.rs（migrate 接线）**

```rust
//! 版本化 DDL：替代 meta.rs 按 ';' 朴素切分（:157）。sqlx migrate 管版本与幂等，
//! 每文件单事务执行；已应用版本按 SHA-384 checksum 校验（生产 baseline 见 6.4）。
pub async fn migrate(pg: &sqlx::PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pg).await?;
    Ok(())
}
```

- [ ] **Step 4: main.rs 换调用点 + meta.rs 删 migrate()**

- main.rs :43、:74、:85 `meta::migrate(pg|` 三处 → `dms_semantic::ddl::migrate(...)`（参数不动）。
- meta.rs 删 :10-161 整个 `pub async fn migrate`（含 `let ddl = r#"..."#;` 与 for 循环），其余函数一个不动。

Run: `cargo build 2>&1 | Select-Object -Last 3`
Expected: Finished 无 error。

- [ ] **Step 5: 空库迁移 + A/B schema 对拍（本任务第一次对拍：migrate 换装零差异）**

```powershell
$A = "postgres://postgres:postgres@localhost:5432/seed_a"
$B = "postgres://postgres:postgres@localhost:5432/seed_b"
# B 库用新代码跑完整引导（migrate 已是新版，seed 仍是旧 Rust const）
cargo run -p dms-ai-server -- meta seed-only $B
python tools/seed_diff.py $B $B --prepare   # B 库补 23 占位行
cargo run -p dms-ai-server -- meta seed-only $B
python tools/seed_diff.py $A $B             # A=旧 migrate+旧 seed；B=新 migrate+旧 seed
```
Expected: `[schema] meta.* 列级全等` + 11 张表 `全等` + `RESULT: ALL EQUAL`。另验证版本表：`psql $B -c "SELECT version, description, success FROM _sqlx_migrations ORDER BY version"` = 8 行全 true。
任一差异 → 拆分照抄有误，逐文件对 meta.rs 原文核对，不许带伤进入 6.3。

- [ ] **Step 6: 既有库幂等验证（模拟生产：无 baseline 重跑必现风险，仅本地确认行为）**

```powershell
# 在已建表的 B 库直接再跑 migrate（未插 baseline）：IF NOT EXISTS 语句幂等跳过；
# 若本机 PG 版本对某条 ALTER 报错，即复现 spec 5.3 第 3 条——这正是 baseline 存在的理由，记录现象即可。
cargo run -p dms-ai-server -- meta seed-only $B
```
Expected: 本地 PG ≥ 9.6 时成功（ADD COLUMN IF NOT EXISTS 受支持）；无论成败都在提交信息里记录本机 PG 版本与现象，佐证 6.4 baseline 不可省。

- [ ] **Step 7: 提交**

```bash
git add Cargo.toml crates/semantic crates/server
git commit -m "semantic: DDL 版本化 8 迁移文件 + sqlx migrate 接线（A/B 对拍全等）；meta.rs 删分号切分 migrate"
```

---

### Task 6.3: 种子外置（10 seeds/*.sql + seed.rs 编排 + 对拍全等硬门禁）

**Files:**
- Create: `crates/semantic/seeds/{warns,kw_force,metrics,dimensions,value_maps,terms,pitfalls,table_scopes,join_edges,scope_binding}.sql`（10 个，文件名即 spec 3.1 定名）
- Modify: `crates/semantic/src/seed.rs`（空壳 → 编排 + sync_elements/upsert_element 搬入）
- Modify: `crates/server/src/main.rs`（bootstrap_meta + meta sync 两处 seed 调用点）
- Modify: `crates/server/src/meta.rs`（删 seed/seed_*/sync_elements/upsert_element）
- Modify: `crates/server/src/inject.rs` 或 `crates/policy/src/rules.rs`（删 seed_rules，按 S3 实点）

**Interfaces:**
- Consumes: 9 组 Rust const + inject.rs builtin_rules 32 表（逐条转换）
- Produces: `dms_semantic::seed::run_seeds(&PgPool)`（内含 scope_binding 灌入）

- [ ] **Step 1: 转换规则（每组转换前后对照此表打钩；条数全部以源码实数为准）**

通用规则：
1. Rust `"..."` → SQL `'...'`；**串内每个 `'` 双写为 `''`**（重灾区：metrics.scope_filter 的 `IN ('0','108','199')`、dimensions 的 CASE WHEN '01'、month 的 `'%Y-%m'`、value_hints 相关码值）。
2. Rust `&["a","b"]`（aliases）→ `ARRAY['a','b']`；空数组 `&[]` → `'{}'`（对齐列 DEFAULT '{}'）。
3. Rust `""` → SQL `''`（NOT NULL DEFAULT '' 列保持空串，**不得写 NULL**）；Rust `None`（scope_binding 可空列）→ SQL `NULL`（**不得写 ''**）。
4. 每行一条 INSERT/UPDATE（便于 diff 与 drift 单测解析）；条目顺序 = const 声明顺序（pitfall bigserial id 依赖顺序一致）。
5. 中文标点（【】⚠️『』，；）逐字照抄，文件 UTF-8 无 BOM。
6. ON CONFLICT / WHERE NOT EXISTS 子句逐字保留（下表）。

| 文件 | 源（meta.rs/inject.rs） | 实测条数 | SQL 形态 |
|---|---|---|---|
| warns.sql | WARNS :287-310 | 23 | `UPDATE meta.table_doc SET warn='...' WHERE table_name='...';` |
| kw_force.sql | KW_FORCE :321-337 | 36 | `INSERT INTO meta.kw_force(keyword,table_name) VALUES('..','..') ON CONFLICT (keyword) DO UPDATE SET table_name='..';` |
| metrics.sql | METRICS :765-839 | 12 | `INSERT INTO meta.metric(metric_code,name,aliases,source_table,agg_expr,scope_filter,time_col,dedup_keys,description) VALUES(...) ON CONFLICT (metric_code) DO UPDATE SET name=..,aliases=..,source_table=..,agg_expr=..,scope_filter=..,time_col=..,dedup_keys=..,description=..;`（9 列全列名，对齐 :842-845） |
| dimensions.sql | DIMENSIONS :981-1018 | 9 | `INSERT INTO meta.dimension(...) VALUES(...) ON CONFLICT (dim_code) DO UPDATE SET name=..,aliases=..,source_table=..,expr=..,description=..;`（对齐 :1021-1024） |
| value_maps.sql | MAPS :632-694 | 105（含重复 PK，去重后 93） | `INSERT INTO meta.value_map(table_name,column_name,name,code,match_kind) VALUES(...) ON CONFLICT (table_name,column_name,name) DO UPDATE SET code=..,match_kind=..;`（重复条目全保留=旧循环覆盖语义） |
| terms.sql | TERMS :716-722 | 5 | `INSERT INTO meta.term(term,definition,aliases) VALUES(...) ON CONFLICT (term) DO UPDATE SET definition=..,aliases=..;` |
| pitfalls.sql | LESSONS :519-546 | 8 | `INSERT INTO meta.pitfall(kind,trigger_words,lesson,status) SELECT 'pitfall','..','..','active' WHERE NOT EXISTS (SELECT 1 FROM meta.pitfall WHERE trigger_words='..' AND lesson='..');`（同一条目 trigger/lesson 出现两次，转义同步两处） |
| table_scopes.sql | SCOPES :363-369 | 3 | `INSERT INTO meta.table_scope(table_name,filter,note) VALUES(...) ON CONFLICT (table_name) DO UPDATE SET filter=..,note=..;` |
| join_edges.sql | EDGES :384-391 | 5 | `INSERT INTO meta.join_edge(left_table,left_col,right_table,right_col,card,note) VALUES(...) ON CONFLICT (left_table,left_col,right_table,right_col) DO UPDATE SET card=..,note=..;` |
| scope_binding.sql | builtin_rules inject.rs:64-90 | 32 | 8 列 `INSERT INTO meta.scope_binding(table_name,mode,customer_col,owner_col,owner_kind,via_table,via_local_col,via_remote_col) VALUES(...) ON CONFLICT (table_name) DO UPDATE SET mode=..,customer_col=..,owner_col=..,owner_kind=..,via_table=..,via_local_col=..,via_remote_col=..;`（列与 SET 对齐 inject.rs:121-123，不写 note 列） |

scope_binding 展开规则（对齐 inject.rs:106-118 的 match）：
- Scoped：`('表','scoped','customer_code','owner_manager','ids',NULL,NULL,NULL)`；owner_kind Codes → `'codes'`；无 owner 维度 4 表 owner_col 也是 `NULL`（owner_kind 仍为 `'ids'`，对齐 b() 闭包）。
- Via：`('表','via',NULL,NULL,NULL,'头表','local_col','remote_col')`。
- Global：`('表','global',NULL,NULL,NULL,NULL,NULL,NULL)`。
- 计数自检：scoped 10 + scoped 无 owner 4 + via 3 + global 15 = 32 行。

示例（metrics.sql 首条，注意单引号双写）：
```sql
INSERT INTO meta.metric(metric_code,name,aliases,source_table,agg_expr,scope_filter,time_col,dedup_keys,description)
VALUES('sales_amount','销售额',ARRAY['销售总额','营业额','销售业绩','业绩','卖了多少'],'t_sales_order','SUM(total_amount)','deleted_flag = 0 AND order_status NOT IN (''0'',''108'',''199'')','order_time','','有效订单销售金额（剔除暂存0/无效108/作废199）')
ON CONFLICT (metric_code) DO UPDATE SET name='销售额',aliases=ARRAY['销售总额','营业额','销售业绩','业绩','卖了多少'],source_table='t_sales_order',agg_expr='SUM(total_amount)',scope_filter='deleted_flag = 0 AND order_status NOT IN (''0'',''108'',''199'')',time_col='order_time',dedup_keys='',description='有效订单销售金额（剔除暂存0/无效108/作废199）');
```
> ON CONFLICT 的 SET 子句值与 VALUES 相同——为降低手转两份出错面，允许用 excluded：`DO UPDATE SET name=excluded.name, aliases=excluded.aliases, ...`（语义等价且更短）。**选定一种风格 10 文件统一**，建议 excluded 风格。

- [ ] **Step 2: 逐组转换 10 个文件（转一组核一组条数：`Select-String -Path crates/semantic/seeds/X.sql -Pattern "^(INSERT|UPDATE)" | Measure-Object | % Count` 与上表条数一致）**

- [ ] **Step 3: seed.rs 编排 + sync_elements/upsert_element 搬入**

```rust
//! 种子编排：顺序对齐旧 meta::seed + bootstrap_meta（spec 迁移步 6）。
//! 整文件多语句执行（sqlx::raw_sql 走 simple query 协议），不切分不解析。

async fn run(pg: &sqlx::PgPool, sql: &'static str) -> anyhow::Result<()> {
    sqlx::raw_sql(sql).execute(pg).await?;
    Ok(())
}

pub async fn run_seeds(pg: &sqlx::PgPool) -> anyhow::Result<()> {
    run(pg, include_str!("../seeds/warns.sql")).await?;
    run(pg, include_str!("../seeds/kw_force.sql")).await?;
    run(pg, include_str!("../seeds/metrics.sql")).await?;
    run(pg, include_str!("../seeds/dimensions.sql")).await?;
    run(pg, include_str!("../seeds/value_maps.sql")).await?;
    run(pg, include_str!("../seeds/join_edges.sql")).await?;
    run(pg, include_str!("../seeds/table_scopes.sql")).await?;
    run(pg, include_str!("../seeds/pitfalls.sql")).await?;
    run(pg, include_str!("../seeds/terms.sql")).await?;
    sync_elements(pg).await)?;
    run(pg, include_str!("../seeds/scope_binding.sql")).await?;
    Ok(())
}
```

`sync_elements`（meta.rs:414-453）与 `upsert_element`（:456-492）**逐字搬**到本文件（含 doc 注释）；`use` 行适配（去掉 `crate::` 前缀）。它们是 PG 派生逻辑（读四注册表 upsert element），不属于 const 种子，故留 Rust 不外置。

- [ ] **Step 4: server 删除与接线**

- main.rs bootstrap_meta（:42-49）改：
  ```rust
  dms_semantic::ddl::migrate(pg).await?;
  dms_semantic::seed::run_seeds(pg).await?;
  let n = inject::load_rules(pg).await?;   // 或 dms_policy::rules::load_rules，按 S3 实点
  ```
  （删 `meta::seed` 与 `inject::seed_rules` 两行调用；`meta sync` 分支 :76 同样换成 `run_seeds`。）
- meta.rs 删：`pub async fn seed`（:285-358）、`seed_table_scopes`（:362-380）、`seed_join_edges`（:382-409）、`seed_pitfalls`（:517-559）、`seed_terms`（:715-735）、`seed_metrics`（:761-860）、`seed_dimensions`（:979-1036）、`seed_value_maps`（:630-712）、`pub async fn sync_elements`（:414-453）、`upsert_element`（:456-492）。其余（sync_schema/is_backup_table/is_sensitive_col/domain_of/recall_*/match_word/map_filter/MetricHit/metric_card/extract_tables/save_lesson_candidate/log_*）一个不动，留 Task 7。
- seed_rules 删除（S3 实点二选一）：
  ```powershell
  Select-String -Path crates/server/src/inject.rs,crates/policy/src/rules.rs -Pattern "pub async fn seed_rules"
  ```
  命中哪个删哪个（inject.rs:104-130 整段；policy 则删对应段 + lib.rs re-export）。builtin_rules/rule_of/load_rules **全保留**（兜底回退与启动加载仍需要）。

Run: `cargo build 2>&1 | Select-Object -Last 3`
Expected: Finished 无 error、无「未使用 import」以外的警告。

- [ ] **Step 5: scope_binding 双源漂移单测（`crates/semantic/tests/seed_drift.rs`）**

灌库真相（SQL）与兜底真相（builtin_rules）必须同步变更，漂移即红：

```rust
//! scope_binding.sql 与 kernel::builtin_rules 双真相源一致性（漂移即红）。
//! S2 缺失态（Task 2 未合并）：改为读 crates/server/src/inject.rs 文本 grep m.insert 表名集合比对。
use std::collections::{HashMap, HashSet};

fn parse_seed() -> HashMap<String, Vec<String>> {
    let sql = include_str!("../seeds/scope_binding.sql");
    let mut out = HashMap::new();
    for line in sql.lines().map(str::trim).filter(|l| l.starts_with("INSERT")) {
        let vals = line.split("VALUES(").nth(1).and_then(|s| s.split(')').next()).unwrap();
        // 8 个字段：'str' 或 NULL，列值均为标识符无内嵌逗号/括号
        let fields: Vec<String> = vals.split(',').map(|f| f.trim().trim_matches('\'').replace("''", "'")).collect();
        assert_eq!(fields.len(), 8, "行字段数异常: {line}");
        out.insert(fields[0].clone(), fields);
    }
    out
}

#[test]
fn scope_binding_seed_matches_builtin_rules() {
    let seed = parse_seed();
    let builtin = dms_kernel::builtin_rules();
    let seed_keys: HashSet<_> = seed.keys().collect();
    let rust_keys: HashSet<_> = builtin.keys().collect();
    assert_eq!(seed_keys, rust_keys, "表名集合漂移");
    for (t, f) in &seed {
        let rule = &builtin[t];
        let mode = f[1].as_str();
        match rule {
            dms_kernel::TableRule::Scoped(b) => {
                assert_eq!(mode, "scoped", "{t}");
                assert_eq!(f[2], b.customer_col.clone().unwrap(), "{t} customer_col");
                assert_eq!(f[3], b.owner_col.clone().unwrap_or("NULL".into()), "{t} owner_col");
                let kind = if b.owner_kind == dms_kernel::OwnerKind::Ids { "ids" } else { "codes" };
                assert_eq!(f[4], kind, "{t} owner_kind");
            }
            dms_kernel::TableRule::Global => assert_eq!(mode, "global", "{t}"),
            dms_kernel::TableRule::Via { table, local_col, remote_col } => {
                assert_eq!(mode, "via", "{t}");
                assert_eq!(&f[5], table, "{t} via_table");
                assert_eq!(&f[6], local_col, "{t} via_local_col");
                assert_eq!(&f[7], remote_col, "{t} via_remote_col");
            }
        }
    }
}
```
> NULL 字段解析后是字符串 `"NULL"`，owner_col None 比对处相应写成 `"NULL"`（上面已体现）。若 Task 2 未交付 pub 类型：同文件改读 `inject.rs` 源码，正则 `m\.insert\("([^"]+)"` 抽 32 个表名只比集合，并在测试注释标「Task 2 合并后恢复类型级比对」。

Run: `cargo test -p dms-semantic 2>&1 | Select-Object -Last 5`
Expected: `scope_binding_seed_matches_builtin_rules` passed。

- [ ] **Step 6: B 库重灌 + 对拍全等（合并硬门禁）**

```powershell
$A = "postgres://postgres:postgres@localhost:5432/seed_a"
$B = "postgres://postgres:postgres@localhost:5432/seed_b"
# B 库清库重建（混入 6.2 的旧 seed 数据，必须从零灌新实现）
dropdb -h localhost -U postgres seed_b; createdb -h localhost -U postgres seed_b
cargo run -p dms-ai-server -- meta seed-only $B    # 1) 新 migrate + 新 seed（warns 首轮空打）
python tools/seed_diff.py $B $B --prepare          # 2) 占位行
cargo run -p dms-ai-server -- meta seed-only $B    # 3) warns 打上
python tools/seed_diff.py $A $B                    # A=旧实现存档库 vs B=全新实现
```
Expected: `[schema] meta.* 列级全等` + 11 张表全 `全等` + `RESULT: ALL EQUAL`。
**不等 → 禁止合并**：按差异行回 Step 1 规则逐条修（首查单引号双写、NULL/空串、ARRAY 元素顺序、pitfall 两处转义同步），修到全等为止。

- [ ] **Step 7: 全量回归 + 提交**

Run: `cargo test --workspace 2>&1 | Select-String "test result"`
Expected: server 157 一个不少；kernel/policy/connector 既有不减少；semantic +1（drift）。

```bash
git add crates/semantic crates/server
git commit -m "semantic: 10 种子 SQL 外置 + run_seeds 编排 + scope_binding 双源 drift 单测；A/B 种子对拍全等（硬门禁达成）"
```

---

### Task 6.4: 生产 baseline 操作手册 + 终验

**Files:**
- Create: `tools/baseline_sqlx_migrations.py`

**原则：** 本节是**手册**——agent 交付脚本与步骤，生产库操作一律由用户明确指示后人工/受控执行（不主动连生产服务器）。

- [ ] **Step 1: 写 tools/baseline_sqlx_migrations.py（完整脚本，一次写全）**

背景（sqlx 0.8 机制）：首次 `Migrator::run` 会自建 `_sqlx_migrations(version BIGINT PK, description TEXT, installed_on TIMESTAMPTZ DEFAULT now(), success BOOLEAN, checksum BYTEA, execution_time BIGINT)`；每次启动对已应用版本**重算 SHA-384 checksum 并比对**，不一致直接 `VersionMismatch` 拒绝启动。故 baseline 必须写入与部署文件逐字节一致的 checksum——脚本直接从本仓库 migrations/ 计算，杜绝手抄。

```python
#!/usr/bin/env python3
"""生产库 sqlx baseline：把已存在于生产的 16 张 meta.* 标记为迁移 1-8 已应用。
用法: python tools/baseline_sqlx_migrations.py <pg_url> [--apply]
  默认 dry-run：只打印将插入的 8 条记录；--apply 才真正写入。
前置: 必须在「将部署二进制」的同一 commit 下运行（migrate! 宏编译期内嵌 SQL，
      checksum 按当前 crates/semantic/migrations/*.sql 逐字节 SHA-384 计算）。
"""
import hashlib
import re
import sys
from pathlib import Path

import psycopg

MIGRATIONS_DIR = Path(__file__).resolve().parent.parent / "crates" / "semantic" / "migrations"

DDL = """
CREATE TABLE IF NOT EXISTS _sqlx_migrations (
  version BIGINT PRIMARY KEY,
  description TEXT NOT NULL,
  installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
  success BOOLEAN NOT NULL,
  checksum BYTEA NOT NULL,
  execution_time BIGINT NOT NULL
)
"""

def migrations():
    out = []
    for p in sorted(MIGRATIONS_DIR.glob("*.sql")):
        m = re.fullmatch(r"(\d+)_(.+)\.sql", p.name)
        if not m:
            raise SystemExit(f"文件名不合 <version>_<desc>.sql 规范: {p.name}")
        version = int(m.group(1))
        description = m.group(2).replace("_", " ")  # sqlx 同款规则：下划线转空格
        checksum = hashlib.sha384(p.read_bytes()).digest()
        out.append((version, description, checksum))
    if not out:
        raise SystemExit(f"未找到迁移文件: {MIGRATIONS_DIR}")
    return out

def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    url, apply = sys.argv[1], "--apply" in sys.argv
    rows = migrations()
    print(f"{'APPLY' if apply else 'DRY-RUN'}: {len(rows)} 个版本")
    for v, d, c in rows:
        print(f"  version={v} description={d!r} checksum=sha384:{c.hex()[:16]}...")
    if not apply:
        print("（dry-run，未写库；确认后加 --apply）")
        return 0
    with psycopg.connect(url) as conn:
        with conn.cursor() as cur:
            cur.execute(DDL)
            for v, d, c in rows:
                cur.execute(
                    "INSERT INTO _sqlx_migrations(version, description, success, checksum, execution_time)"
                    " VALUES (%s, %s, true, %s, 0) ON CONFLICT (version) DO NOTHING",
                    (v, d, c),
                )
        conn.commit()
        with conn.cursor() as cur:
            cur.execute("SELECT version, description, success FROM _sqlx_migrations ORDER BY version")
            for r in cur.fetchall():
                print("  已记录:", r)
    print("baseline 完成")
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: 本地预演（在 B 库全流程走一遍手册，验证脚本与顺序可靠）**

```powershell
$B = "postgres://postgres:postgres@localhost:5432/seed_b"
# B 库已是全量新实现（含 _sqlx_migrations 8 行）——先清掉版本表模拟「生产旧库」
psql $B -c "DROP TABLE _sqlx_migrations"
python tools/baseline_sqlx_migrations.py $B            # dry-run 看 8 条
python tools/baseline_sqlx_migrations.py $B --apply    # 写入
cargo run -p dms-ai-server -- meta seed-only $B        # 首启动：应 0 迁移应用、直接成功
```
Expected: seed-only 成功且无 migrate 报错；`SELECT count(*) FROM _sqlx_migrations WHERE success` = 8。若报 VersionMismatch → checksum 算错（多半是文件换行/BOM 被动过），修文件后重算，**禁止**手改数据库 checksum 绕过。

- [ ] **Step 3: 生产操作手册（交给用户/运维按序执行；每步有验证）**

1. 生产库确认版本与现状：`SELECT version();`、`SELECT count(*) FROM information_schema.tables WHERE table_schema='meta';`（应 = 16）。
2. 在**将部署 commit** 的工作机上：`python tools/baseline_sqlx_migrations.py <生产PG_URL>`（dry-run 核对 8 条）。
3. `--apply` 写入；验证：`SELECT version, description, success FROM _sqlx_migrations ORDER BY version;` = 8 行全 true。
4. 部署新二进制；首启动日志应无 `Applying` 字样（0 迁移应用），`元数据引导完成` 正常打出。
5. 抽验种子幂等：`SELECT count(*) FROM meta.metric;`（12）、`SELECT count(*) FROM meta.scope_binding;`（32）与种子条数核对表一致。
回滚：`_sqlx_migrations` 对旧二进制完全透明（旧代码不读它），回退旧二进制无需任何数据库动作；要彻底还原则 `DROP TABLE _sqlx_migrations`（不影响 meta.* 数据）。

- [ ] **Step 4: 终验清单（本地全绿才可提请合并）**

```powershell
cargo test --workspace 2>&1 | Select-String "test result"     # 157 server + 既有 + drift 全绿
cargo build 2>&1 | Select-Object -Last 3                       # 全 workspace 编译
Select-String -Path crates/server/src/meta.rs -Pattern "pub async fn migrate|pub async fn seed|sync_elements|seed_metrics|seed_dimensions|seed_value_maps|seed_terms|seed_pitfalls|seed_table_scopes|seed_join_edges"   # 期望空
Select-String -Path crates/server/src/*.rs,crates/policy/src/*.rs -Pattern "seed_rules" -ErrorAction SilentlyContinue   # 期望空
Select-String -Path crates/server/src/meta.rs -Pattern "split\(';'\)|split\(\";\"\)"      # 期望空（朴素切分已消灭）
Get-ChildItem crates/semantic/migrations/*.sql | Measure-Object | % Count                 # 期望 8
Get-ChildItem crates/semantic/seeds/*.sql | Measure-Object | % Count                      # 期望 10
```
Expected: 全过；对拍脚本最近一次运行 `RESULT: ALL EQUAL`（6.3 Step 6 输出留存）。

- [ ] **Step 5: 提交收尾**

```bash
git add tools/baseline_sqlx_migrations.py docs/superpowers/plans/2026-07-27-task06-meta-ddl-seeds.md
git commit -m "tools: sqlx baseline 脚本 + 生产操作手册（B 库预演通过）；Task 6 终验全绿"
```

---

## 自检（已执行）
- **spec 覆盖**：迁移步 6 两要件（DDL 版本化 + 种子外置）✓；门禁「种子对拍全等才可合并」落为 6.3 Step 6 硬步骤 ✓；「先插生产库 baseline」落为 6.4 脚本+手册 ✓；3.1 目录树 migrations/seeds 10 文件名逐字对齐 spec ✓；5.1 种子对拍层「全等（含 NULL vs 空串）」由 seed_diff.py 的 repr 级比对实现 ✓。
- **5.3 第 3 条正面处理**：baseline 脚本按 sqlx 0.8 真实机制写（_sqlx_migrations 表结构、SHA-384 checksum 重算比对、description 下划线转空格规则）；checksum 从部署同 commit 文件现算，禁手改绕过；ALTER 旧 PG 风险在 6.2 Step 6 本地复现记录 ✓。
- **条数核验（源码实测，非任务书口径）**：WARNS 23（meta.rs:287-310）/ KW_FORCE 36 / METRICS 12 / DIMENSIONS 9 / TERMS 5 / LESSONS 8 / SCOPES 3 / EDGES 5 / MAPS 105 条（去重后 93 行）/ scope_binding 32（10 scoped+4 无 owner+3 via+15 global）/ DDL 25 条 → 8 迁移 ✓。任务书写的 26/38/13/10/30 与源码不符，一律以源码+对拍为准。
- **TDD 节奏**：6.1 安全网先行（A 库旧种子存档于动刀前）→ 6.2 换 migrate 即对拍 → 6.3 换 seed 再对拍（硬门禁）→ 6.4 手册预演 ✓。
- **依赖红线**：零新第三方依赖（migrate 为 sqlx feature；psycopg 仅 tools 脚本）✓；依赖方向 server→semantic，semantic 运行时不依赖 server/policy（dms-kernel 仅 dev-dependencies 供 drift 单测）✓；sync_elements/upsert_element 随 seed 搬入 semantic 是方向所迫（seed 编排调用），recall_elements 依赖 embed 留 server ✓。
- **种子语义**：UPDATE-only warns、ON CONFLICT/WHERE NOT EXISTS 形态、value_map 重复 PK 覆盖语义、空串 vs NULL 区分、编排顺序（含 sync_elements 位置、scope_binding 最后）逐字锁定 ✓。
- **占位符扫描**：S3 seed_rules 落点两态命令都给出；S2 缺失时 drift 单测降级方案写明；无 TBD 留白 ✓。

## 需 team-lead 裁决（阻塞前先确认）
1. **semantic 骨架归属**：Task 1 仍 pending（workspace 仅 crates/server）。本 plan 6.2 Step 1 自建最小骨架（Cargo.toml+lib.rs+ddl.rs/seed.rs），与 plan-t1 范围重叠——建议 Task 6 先行自建、Task 1 合并时以本骨架为准不重复建，请裁决。
2. **条数口径**：任务书 WARNS 26/KW_FORCE 38/METRICS 13/DIMENSIONS 10/scope_binding 30 与源码实测 23/36/12/9/32 不符。plan 全按源码实测+对拍全等兜底，请确认（若任务书口径另有来源，需指出差异条目）。
3. **scope_binding 双真相源折中**：灌库真相迁 seeds/scope_binding.sql，kernel::builtin_rules 保留作 PG 缺席兜底（rule_of/snapshot 回退路径仍需要），drift 单测锁漂移。彻底单源化（kernel 解析 SQL）会让 kernel 碰文件/SQL 解析，不建议；请确认此折中。
4. **seed_rules 删除的连带**：policy（Task 5 合并后）将只剩 load_rules，「重置权限档案」入口归 dms_semantic::seed::run_seeds——请知会 plan-t5/plan-t10，避免 Task 10 管理面重复造种子入口。
5. **生产 baseline 执行人**：按「不主动碰服务器」惯例，6.4 Step 3 全程用户/运维执行，agent 只交付脚本+B 库预演证据；请确认。

## 备注（Windows 构建）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）
对拍/B 库相关命令里的连接串密码按本机 PG 实际替换；`createdb/dropdb/pg_dump/psql` 走 PG 自带 bin（docker 场景用 `docker exec dms-pg ...` 等价替换）。
