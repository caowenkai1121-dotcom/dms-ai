# Task 9：pipeline 解体入 agent（AskRun 状态机 + Answerer 路由 + Answer 协议）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 pipeline.rs(1037 行) 解体进 dms-agent：AskRun sans-IO 状态机（kernel）装所有「决策」、五条确定性路径包成五个 Answerer 走有序路由表、ask_single 收敛为 ~20 行驱动循环、AskResult 换 Answer 协议（Table 变体 serde flatten 保前端字段字节级不变）、prompt 外置 *.md、补复合问题的汇总步。

**Architecture:** kernel 落 AskRun 状态机（零 IO、纯决策、可纯单测）+ Answer 协议类型；agent 落 Answerer trait + Router + 五个实现 + run 驱动循环 + prompt/review；server 删 pipeline.rs，handler 改调 agent::ask。依赖方向 agent→{kernel,connector,policy,semantic}，agent 不配 axum（HTTP/CLI/定时任务三入口共用）。

**Tech Stack:** Rust workspace、cargo、serde/serde_json。零新增第三方依赖。

**前置依赖（必须先落地）：** Task 1（骨架）、Task 2（kernel 纯算法）、Task 3（三段 newtype + ReadOnlyMySql）、Task 4（ChatModel/ChatRequest/ChatReply/LlmError::is_transient）、Task 5（policy compute_scope/rules::snapshot）、Task 7（semantic Corrector trait + run_chain + recall）。需要的接口若缺失，标注「需 Task N 补」，不自己重实现。

## Global Constraints

- **行为不变（除两处有意修复）**：路由顺序、校正链顺序、自修两轮、KPI 环比、语义缓存、图快路径、失败复盘全部与今天逐行对齐。仅有的两处有意变化：① 补复合问题汇总步（今天 `AskResult::compound` 主体全空，pipeline.rs:40-52）；② Answer 比 AskResult 多 `kind`/`column_meta`/`trace` 字段（新增不破坏）。
- **三个硬门禁（spec 迁移步 9）**：
  - **路由对拍**：同一批问句（tools/regression_cases.json 全部 question）在新代码下的 route 必须与今天逐条相同——顺序错一位就让问句改走别的路径（compose 与 fastpath 互换会让「销售额按省份」SQL 完全不同）。
  - **serde golden**：Answer 序列化的 Table 变体顶层字段与旧 AskResult 字节级一致（新增 kind/column_meta/trace 除外），写 golden JSON 测试。
  - **prompt 外置一字不差**：prompts/*.md 与今天 format! 字面量逐字一致（含结尾换行），prompt 微小变化会让 LLM 输出漂移，给逐字比对脚本。
- **agent 不配 axum**：`cargo tree -p dms-agent` 不得出现 axum/tokio-stream。
- **不新增第三方依赖**；异步 trait 手写 BoxFut 不引 async-trait。
- **既有测试**：server 111 单测（157-46 权限已迁 policy）一个不改地通过；校正器 13 单测随 semantic；AskRun 新增纯单测。
- Windows 构建须前缀 MinGW bin 路径（见文末备注），cargo 命令走 PowerShell。

## 上游契约清单（Task 9.0 逐项核对；缺失即标注「需 Task N 补」）

| # | 契约 | 来自 | 用途 |
|---|---|---|---|
| A1 | `dms_kernel::llm::{ChatModel, ChatRequest, ChatReply, ModelTier, LlmError::is_transient}` | Task 4 | LLM 调用 |
| A2 | `dms_kernel::sql::gate::{RawSql, check, inject, GuardConfig}` + `sql::dialect::MysqlDialect` | Task 3 | 校验+注入 |
| A3 | `dms_connector::{ReadOnlyMySql, RowSet}` + `fetch(&ScopedSql,..)` / `explain(&ScopedSql,..)` | Task 3 | 执行 |
| A4 | `dms_policy::{Principal, ScopeSets}` + `scope::compute_scope_cached` + `rules::snapshot() -> Arc<RuleSet>` | Task 5 | 权限 |
| A5 | `dms_semantic::correct::{Corrector, CorrectCtx, run_chain, default_chain, schema_check, log_correction}` + `recall::*` | Task 7 | 校正链+召回 |
| A6 | `dms_semantic::fastpath::{try_compose, try_direct, detect_relation}` + `graph::{try_graph}` | Task 8 | 确定性路径 |
| A7 | `dms_kernel::present::{build as viewspec_build, patch_kpi_delta, ViewSpec}` | Task 2 | 呈现 |
| A8 | `dms_kernel::nl::lexicon::{FOLLOWUP_MARKS, TIME_GUARDS}` + `is_followup` 逻辑 | Task 2 | 缓存护栏 |

---

### Task 9.1: kernel AskRun sans-IO 状态机（纯决策，可纯单测）

**Files:**
- Create: `crates/kernel/src/run.rs`
- Modify: `crates/kernel/src/lib.rs`（挂 `pub mod run;`）

**Interfaces:**
- Consumes: `dms_kernel::sql::gate::{RawSql, ScopedSql}`（Task 3）、`dms_kernel::llm::{ChatReply}`（Task 4）
- Produces: `dms_kernel::run::{AskRun, Step, Budget, Stage, ExecFailure, SqlTrace}`

**设计意图**：把 ask_single(pipeline.rs:527-716) 里所有「决策」——轮次计数、重试上限、校正后是否重验、失败是否自修、终态构造——抽进一个**不做任何 IO** 的状态机。驱动侧（agent/run.rs）只剩 ~20 行 `match run.next()`。收益：整条流水线逻辑可纯同步单测（不需 mock LLM/起库），Rust 穷尽匹配保证新增 Stage 时每个分支都被处理。**不做** Serialize/可恢复（HITL 需求，YAGNI）。

- [ ] **Step 1: 写失败测试（kernel/run.rs `#[cfg(test)]`，全纯单测）**

```rust
// 锁定决策语义（无需库、不需 mock）：
first_step_is_generate            // new 后 next() == Step::Generate
on_generated_produces_sql_goes_validate   // 喂 ChatReply{content:"SELECT..."} → next()==Validate
validate_fail_first_goes_repair   // on_failed(校验错) round=0 → Step::Repair{round:1}
validate_fail_second_bails        // round 已达 max_repair → Err(AskError)
execute_ok_goes_finish            // on_executed(rows) → next()==Step::Finish
execute_fail_first_goes_repair    // 执行错 round=0 → Repair
execute_fail_second_bails         // 超预算 → Err
budget_enforced                   // Budget{max_repair_rounds:2} 第 3 次失败即终止
trace_accumulates                 // trace() 记录 generated/corrected/injected/stage_ms
```

- [ ] **Step 2: 运行确认失败** `cargo test -p dms-kernel run 2>&1 | Select-Object -Last 5`（模块不存在=红）

- [ ] **Step 3: 实现 run.rs**

```rust
//! AskRun：NL2SQL 流水线 sans-IO 状态机。只装「决策」，不做任何 IO。
//! 驱动侧（dms-agent::run）match next() 喂 IO 结果。决策可纯同步单测。

use crate::llm::ChatReply;
use crate::sql::gate::ScopedSql;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage { Generate, Validate, Execute, Repair, Done }

#[derive(Clone, Copy, Debug)]
pub struct Budget { pub max_repair_rounds: u8 }
impl Default for Budget { fn default() -> Self { Self { max_repair_rounds: 2 } } } // 对齐 pipeline.rs:626 for attempt in 0..2

#[derive(Debug)]
pub enum Step {
    Generate { system: String, user: String },
    Validate { sql: String },                 // 待 check→inject
    Execute { sql: Box<ScopedSql> },          // 已注入，待 fetch
    Repair { sql: String, reason: String, round: u8 },
    Finish(Box<crate::answer::Answer>),
}

#[derive(Debug)]
pub enum ExecFailure { Guard(String), Policy(String), Explain(String), Exec(String) }

#[derive(Debug, Default)]
pub struct SqlTrace {
    pub generated: Option<String>,
    pub corrected: Vec<(String, String)>,   // (校正器名, detail)
    pub injected: Option<String>,
    pub repair_rounds: u8,
}

pub struct AskRun {
    budget: Budget,
    round: u8,
    question: String,
    sql: String,                            // 当前候选（未注入）
    route: String,
    stage: Stage,
    trace: SqlTrace,
}

#[derive(Debug)]
pub enum AskError { RepairExhausted, Llm(String) }
impl std::fmt::Display for AskError { /* LLM 生成失败 / 自修后仍不可用（对齐 pipeline.rs:715 文案） */ fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::RepairExhausted=>write!(f,"生成失败（自修后仍不可用）"),Self::Llm(e)=>write!(f,"{e}")}} }
impl std::error::Error for AskError {}

impl AskRun {
    pub fn new(question: String, budget: Budget) -> Self { /* round:0, stage:Generate, route:"llm" */ }
    pub fn next(&self) -> Step { /* 按 stage 返回当前该做的动作 */ }
    pub fn on_generated(&mut self, reply: ChatReply) -> Result<(), AskError> { /* content 缺→Err(Llm)；有→sql=内容, trace.generated, stage=Validate */ }
    pub fn on_corrected(&mut self, sql: String, changes: Vec<(String,String)>) { /* sql 更新+trace.corrected+=changes，stage 保持 Validate */ }
    pub fn on_validated(&mut self, scoped: ScopedSql) { /* trace.injected=scoped.wire()，stage=Execute */ }
    pub fn on_executed(&mut self, answer: crate::answer::Answer) { /* stage=Done，存 answer */ }
    pub fn on_failed(&mut self, f: ExecFailure) -> Result<(), AskError> {
        // round<max → round+=1, stage=Repair, Ok(())；否则 Err(RepairExhausted)
    }
    pub fn trace(&self) -> &SqlTrace { &self.trace }
    pub fn route(&self) -> &str { &self.route } // repair 发生时内部已置 "llm+repair"/"llm+schema-fix"
}
```

- [ ] **Step 4: 测试全绿** `cargo test -p dms-kernel run 2>&1 | Select-Object -Last 5`

- [ ] **Step 5: 提交** `git commit -m "kernel: AskRun sans-IO 状态机（轮次/重试/终止决策），纯单测锁定"`

---

### Task 9.2: kernel Answer 协议 + serde golden 门禁

**Files:**
- Create: `crates/kernel/src/answer.rs`
- Modify: `crates/kernel/src/lib.rs`（挂 `pub mod answer;`）

**Interfaces:**
- Produces: `dms_kernel::answer::{Answer, AnswerBody, ColumnMeta, Citation, StepRecord}`

- [ ] **Step 1: 写 serde golden 测试（红）**

固定一份旧 AskResult 的 JSON（从生产一次真实「本月销售额」响应抓的 golden），断言新 Answer::table(...) 序列化后**顶层键集合 ⊇ golden 键集合**且共有键的值逐字节相等（新增 kind/column_meta/trace 除外）：

```rust
#[test]
fn table_body_matches_legacy_ask_result() {
    let ans = Answer::table("SELECT ... LIMIT 200".into(), cols(), rows(), false, 3);
    let v = serde_json::to_value(&ans).unwrap();
    // 旧 AskResult 顶层键：sql/columns/rows/row_count/truncated/elapsed_ms/route/view/subs
    for k in ["sql","columns","rows","row_count","truncated","route"] {
        assert!(v.get(k).is_some(), "缺旧字段 {k}");
    }
    assert_eq!(v["kind"], "table"); // 新增
    assert_eq!(v["sql"], "SELECT ... LIMIT 200");
}
```

- [ ] **Step 2: 实现 answer.rs（spec 2.5 形状）**

```rust
//! 统一回答协议：破「回答=一张表+一条SQL」不变量。Table 变体 serde flatten 保前端字节级兼容。
use serde::Serialize;

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerBody {
    Table {
        sql: String,
        columns: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        column_meta: Vec<ColumnMeta>,
        rows: Vec<Vec<serde_json::Value>>,
        row_count: usize,
        truncated: bool,
    },
    Text { markdown: String, citations: Vec<Citation> },
    Steps { steps: Vec<StepRecord> },
    Composite { subs: Vec<Answer>, summary: Option<String> },
}

#[derive(Serialize, Debug, Clone)]
pub struct Answer {
    pub route: String,
    #[serde(flatten)]
    pub body: AnswerBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<crate::present::ViewSpec>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subs: Vec<Answer>,               // 旧 AskResult.subs 兼容
    #[serde(skip_serializing_if = "crate::answer::trace_is_empty")]
    pub trace: crate::run::SqlTrace,
    pub elapsed_ms: u128,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct ColumnMeta { pub role: String, pub semantic: String, #[serde(skip_serializing_if="Option::is_none")] pub unit: Option<String>, #[serde(skip_serializing_if="Option::is_none")] pub code_map: Option<serde_json::Value> }
#[derive(Serialize, Debug, Clone)]
pub struct Citation { pub source: String, pub locator: String }
#[derive(Serialize, Debug, Clone)]
pub struct StepRecord { pub name: String, pub detail: String, pub ms: u64 }

pub fn trace_is_empty(t: &crate::run::SqlTrace) -> bool { t.generated.is_none() && t.corrected.is_empty() && t.injected.is_none() }

impl Answer {
    pub fn table(sql: String, columns: Vec<String>, rows: Vec<Vec<serde_json::Value>>, truncated: bool, row_count: usize) -> Self { /* route:"llm", body:Table{..}, view:None, subs:vec![], trace:default, elapsed_ms:0 */ }
}
```

> 说明：旧 `AskResult` 的 `view` 是必填（非 Option）。golden 测试时若前端把 `view` 当必填，Answer.view 在 Table 路径必须始终 `Some(viewspec_build(..))`——驱动侧（agent/run.rs）负责填，serde 层 `skip_none` 仅为非 Table 路径。这点在 Task 9.5 的前端联调核。

- [ ] **Step 3: 验证** `cargo test -p dms-kernel answer 2>&1 | Select-Object -Last 5`（golden 过）

- [ ] **Step 4: 提交** `git commit -m "kernel: Answer 统一协议（Table/Text/Steps/Composite），serde flatten 保旧字段"`

---

### Task 9.3: agent Answerer trait + Router + 五个实现

**Files:**
- Create: `crates/agent/src/answerers/mod.rs`、`compose.rs`、`fastpath.rs`、`graph.rs`、`cache.rs`、`llm.rs`
- Create: `crates/agent/src/ctx.rs`
- Modify: `crates/agent/src/lib.rs`

**Interfaces:**
- Consumes: A1-A8 全部
- Produces: `dms_agent::answerers::{Answerer, Router, default_router}`；`dms_agent::ctx::AskCtx`

- [ ] **Step 1: 定义 Answerer + AskCtx + Router**

```rust
//! 路由分诊：替代 pipeline.rs:538-591 的写死 if 链。顺序=优先级，可 dump 可 trace。
use std::sync::Arc;
use dms_kernel::answer::Answer;
use crate::ctx::AskCtx;

pub trait Answerer: Send + Sync {
    fn route(&self) -> &'static str;
    /// 便宜门禁，不做 IO
    fn accept(&self, ctx: &AskCtx<'_>) -> bool;
    /// Ok(None)=我没接住，交给下一个
    fn answer<'a>(&'a self, ctx: &'a AskCtx<'a>)
        -> dms_kernel::llm::BoxFut<'a, anyhow::Result<Option<Answer>>>;
}

pub struct Router { answerers: Vec<Arc<dyn Answerer>> }
impl Router {
    pub fn routes(&self) -> Vec<&'static str> { self.answerers.iter().map(|a| a.route()).collect() }
    pub async fn dispatch(&self, ctx: &AskCtx<'_>) -> anyhow::Result<Answer> {
        for a in &self.answerers {
            if a.accept(ctx) {
                if let Some(ans) = a.answer(ctx).await? { return Ok(ans); }
            }
        }
        anyhow::bail!("无可用回答路径（llm 兜底缺失）")
    }
}
```

`ctx.rs`：`pub struct AskCtx<'a> { pub question:&'a str, pub principal:&'a Principal, pub sets:&'a ScopeSets, pub rules:&'a dms_kernel::policy::RuleSet, pub mysql:&'a ReadOnlyMySql, pub pg:&'a sqlx::PgPool, pub llm:&'a dyn ChatModel, pub started: std::time::Instant }`

- [ ] **Step 2: 五个实现，顺序与 pipeline.rs:538-591 一字不差**

| 序 | Answerer | accept | 行为（对齐现状行号） |
|---|---|---|---|
| 1 | `ComposeAnswerer` | `sets.is_unrestricted() && detect_relation(q).is_none()` 且 try_compose 命中 | :547 try_compose；SQL→check→inject→fetch；KPI 环比(:558-568) |
| 2 | `FastpathAnswerer` | try_direct 命中 | :549 单号/下钻/聚合模板；同上的 check→inject→fetch+环比 |
| 3 | `GraphAnswerer` | `sets.is_unrestricted() && detect_relation(q).is_some()` | :538-544 try_graph（限权回落由 accept=false 保证） |
| 4 | `CacheAnswerer` | `!is_followup(q)` | :587-590 try_semantic_cache |
| 5 | `LlmAnswerer` | 恒 true（兜底） | :593-715 生成→run_chain 五校正→AskRun 循环 |

> ⚠️ **路由对拍的关键**：现状 graph 在最前(:538)但只在 `sets.is_unrestricted()` 且 `detect_relation` 命中时走；compose(:547) 在 fastpath(:549) 前。五个 Answerer 的 `accept` 必须把「graph 仅全权限」「graph 仅关系问句」「compose 优先于 fastpath」这三个判断**逐字搬进各自的 accept**，而不是靠 Router 顺序隐含。Router 顺序只是 tie-break。

- [ ] **Step 3: 路由对拍 harness（硬门禁）**

写一个临时 binary `crates/agent/examples/route_diff.rs`（或 `#[cfg(test)]` 用 ignore 标注的连库测试）：
1. 读 `tools/regression_cases.json` 全部 question + 一个代表性受限账号 + 超管账号。
2. 对每个 (账号, question)：用**旧逻辑**（临时保留的 pipeline.rs 副本或 git stash 旧版）记录 route；用**新 Router** 记录 `router.dispatch` 实际命中的 `answerer.route()`。
3. 输出不一致清单。期望：逐条相同。
> 因新旧无法同进程共存，实操 = 先在旧代码跑一遍 `ask` 记录 route 落盘 JSON，再切新代码跑 route_diff 比对。给出 `tools/route_capture.py`（subprocess 调旧 exe `ask` 存 route）+ `route_diff` 的配对。差异→回 Step 2 修 accept，不许合并。

- [ ] **Step 4: 验证 + 提交** `cargo test -p dms-agent` + 路由对拍零差异 → `git commit -m "agent: Answerer 有序路由表 + 五实现，路由对拍逐条相同"`

---

### Task 9.4: agent run 驱动循环 + prompt 外置 + review 迁入

**Files:**
- Create: `crates/agent/src/run.rs`、`crates/agent/src/prompt.rs`、`crates/agent/src/review.rs`
- Create: `crates/agent/prompts/{system,repair,split,rewrite,review_failure}.md`
- Modify: `crates/agent/src/lib.rs`

**Interfaces:**
- Consumes: kernel::run::AskRun、semantic::correct::run_chain、A1
- Produces: `dms_agent::run::ask_single(&AskCtx) -> Result<Answer>`；`dms_agent::ask(&AskCtx, prev_question) -> Result<Answer>`

- [ ] **Step 1: run.rs 驱动循环（LlmAnswerer 的内核，~20 行 match）**

```rust
//! 驱动 kernel::AskRun 的 IO 循环：全仓唯一 loop。决策全在 AskRun，这里只做 IO。
pub async fn drive(ctx: &AskCtx<'_>, system: String, user: String) -> anyhow::Result<Answer> {
    let mut run = AskRun::new(ctx.question.to_string(), Budget::default());
    loop {
        match run.next() {
            Step::Generate { .. } => {
                let reply = ctx.llm.chat(ChatRequest::text(ModelTier::Precise, &system, &user, Some(0.1))).await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let sql = crate::prompt::extract_sql(reply.content.as_deref().unwrap_or(""))
                    .ok_or_else(|| anyhow::anyhow!("LLM 未产出 SQL"))?;
                let mut r = ChatReply::default(); r.content = Some(sql);
                run.on_generated(r)?;
            }
            Step::Validate { sql } => {
                let checked = dms_kernel::sql::gate::check(dms_kernel::sql::gate::RawSql::new(sql.clone()), &MysqlDialect, &GuardConfig::default())
                    .map_err(|e| { let _ = run.on_failed(ExecFailure::Guard(e.to_string())); e }).ok();
                match checked {
                    Some(c) => { let scoped = dms_kernel::sql::gate::inject(c, ctx.sets, ctx.rules)?; run.on_validated(scoped); }
                    None => { run.on_failed(ExecFailure::Guard("校验失败".into()))?; }
                }
            }
            Step::Execute { sql } => {
                match ctx.mysql.fetch(&sql, MAX_ROWS, std::time::Duration::from_secs(30)).await {
                    Ok(rs) => { /* few-shot 回写 + zero-rows 记录 + viewspec_build → Answer，run.on_executed(ans) */ }
                    Err(e) => run.on_failed(ExecFailure::Exec(e.to_string()))?,
                }
            }
            Step::Repair { sql, reason, round } => { /* 调 repair(prompt) 拿新 SQL，run.on_corrected */ }
            Step::Finish(ans) => return Ok(*ans),
        }
    }
}
```
> 校正链（五校正器）在 `Step::Validate` 之前、Generate 之后跑一次（对齐 pipeline.rs:597-624 在循环外的位置）：`let (sql, changes) = run_chain(&default_chain(), &cctx, sql).await; run.on_corrected(sql, changes)`。schema_check 的「幻觉列携真实列清单自修」是 Repair 的一种 reason。

- [ ] **Step 2: prompt.rs + 外置 *.md（一字不差门禁）**

`prompts/system.md` = `build_system_prompt`（pipeline.rs:187-204）的 format! 模板，把 `{}` 占位换成 `{date}`/`{identity}` 命名占位，运行期 `str::replace` 填。`prompts/repair.md`/`split.md`/`rewrite.md`/`review_failure.md` 同理（对应 :923-941/:479/:453/:721）。
`prompt.rs`：`pub fn build_system(date:&str, identity:&str) -> String { include_str!("../prompts/system.md").replace("{date}",date).replace("{identity}",identity) }` + `extract_sql`（已在 Task 4.4）+ `build_user`（8 段组装，pipeline.rs:227-320 迁入）。
**一字不差比对**：写 `tools/prompt_diff.py`——对同一 (date,identity)，调旧代码打印 system prompt 存盘，与新 `build_system` 输出 `difflib.unified_diff` 比对，**必须零 diff**（含结尾换行）。这是硬门禁。

- [ ] **Step 3: review.rs 迁入**（review_failure:720-736 / review_lessons:739-762 / review_exemplar:766-785 / review_all_pending:863-875），签名 `llm: &dyn ChatModel`，调用点（run.rs 的 spawn、server CLI review-pending/review-lessons）改 agent::review。

- [ ] **Step 4: 复合问题汇总步（有意修复①）**

`ask()`（对齐 pipeline.rs:495-525）拆出 subs 后，新增：`let summary = summarize(ctx.llm, &subs).await`——用 fast tier 把各子结果 columns+首几行喂 LLM 产一段中文总结，填 `AnswerBody::Composite{subs, summary: Some(summary)}`。今天主体全空，这是补全不是变更。summarize 失败（LLM 抖动）→ `summary: None` 不阻断（对齐今天「subs 原样丢前端」的兜底）。

- [ ] **Step 5: 验证 + 提交** `cargo test -p dms-agent` + prompt_diff 零 diff → `git commit -m "agent: run 驱动循环 + prompt 外置 *.md + review 迁入 + 复合汇总步"`

---

### Task 9.5: server 切换 + 删 pipeline.rs + serde golden 终验

**Files:**
- Modify: `crates/server/src/main.rs`（删 `mod pipeline;`，handler 改调 `dms_agent::ask`，CLI ask 同理）
- Delete: `crates/server/src/pipeline.rs`

**Interfaces:**
- Consumes: `dms_agent::{ask, AskCtx}`；AppState 提供 mysql/pg/llm/rules
- Produces: server 问答全部经 agent；HTTP 响应字段与今天字节级一致

- [ ] **Step 1: AppState 装配 Router + handler 改 agent::ask**

`api/ask` handler：认证得 Principal → `compute_scope_cached` → `rules::snapshot()` → 组 `AskCtx` → `dms_agent::ask(&ctx, prev_question).await` → 返回 `Json(answer)`。AskResult 全删。

- [ ] **Step 2: 删 pipeline.rs + 全仓构建**

`cargo build 2>&1 | Select-Object -Last 5`：无 error、无 `crate::pipeline` 残留。

- [ ] **Step 3: serde golden 终验（连库或录制回放）**

用 Task 4.5 的 MockChatModel 录制回放，对「本月销售额」「前五省份销售额」「单号直查」三个代表问句，比对新旧 HTTP 响应 JSON：共有键值逐字节相等，新增键（kind/column_meta/trace）存在。`tools/golden_diff.py` 给出。
**前端联调核**：起服务 + 前端，确认 KPI 卡/图表/表格三形态渲染不崩（view 字段在 Table 路径始终 Some）。

- [ ] **Step 4: 全量回归** `cargo test --workspace` 全绿 + `python tools/regression.py` 结果集不变。

- [ ] **Step 5: 提交** `git commit -m "server: pipeline.rs 删除，问答切 dms_agent::ask；Answer 协议 serde golden 终验"`

---

## 自检
- **spec 覆盖**：迁移步 9 全项（AskRun 状态机 ✓ 9.1、五 Answerer 顺序同今 ✓ 9.3、Answer 协议 ✓ 9.2、prompt 外置 ✓ 9.4、汇总步 ✓ 9.4）；三个风险点各落门禁（路由对拍 9.3 Step 3、serde golden 9.2/9.5、prompt 逐字 9.4 Step 2）。
- **占位符扫描**：AskRun/answer/Answerer/run 骨架完整；prompt_diff/route_diff/golden_diff 三脚本做法具体。
- **类型一致**：ChatRequest::text 对齐 Task 4.1；check/inject 对齐 Task 3；run_chain/CorrectCtx 对齐 Task 7；snapshot 对齐 Task 5。
- **依赖方向**：agent→kernel/connector/policy/semantic，不配 axum（9.0 核对）。

## 需 team-lead 裁决
1. **Answer.view 在 Table 路径**：旧 AskResult.view 是必填，新 Answer.view 是 Option（非 Table 路径跳过）。建议 Table 路径始终 Some（驱动侧填），serde 层 skip_none 仅为非 Table——请确认前端不把 view 当可选。
2. **路由对拍的旧 route 捕获**：新旧无法同进程共存，方案 = 旧 exe `ask` 先存 route JSON、新代码 route_diff 比对。若接受「git 出旧 commit 编一个旧 exe」的笨办法，请确认；更省的是信任 accept 逐字搬 + 回归题集 route 断言（regression.py 本就断言 route 字段）。
3. **汇总步的 LLM 成本**：复合问题新增一次 fast-tier summarize 调用。若在意成本/延迟，可降级为「模板拼接各子结果标题」不调 LLM——请定。

## 备注（Windows 构建）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）
