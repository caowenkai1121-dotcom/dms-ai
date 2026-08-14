//! # dms-agent —— 一次问答的循环语义与路由分诊
//!
//! 全仓唯一持有 loop 的 crate：多轮改写 → 意图分诊（data/knowledge/hybrid）→ 选源 →
//! Router 有序表 `[graph, compose, fastpath, cache, llm]` → LLM 路径的显式 repair 循环（≤2 轮）。
//! HTTP / CLI / 定时任务三入口共用同一个 `ask()`。
//!
//! ## 纪律
//! - **不配 axum**：协议层归 server。
//! - **不自己写 `meta.*` 的 SQL**：语料、教训、缓存语料一律经 `dms_semantic::registry`。
//! - **不造连接池**：`AskCtx` 只**持** `&PgPool`（server 装配后传入），造池是 connector 的事。
//! - `Answerer::route()` 只是表标签，**`Answer.route` 取命中方给的值**
//!   （`direct-agg`/`direct-doc`/`graph`/`semantic-cache`/`llm[+repair|+schema-fix]`/`compound`）——
//!   混用即 26 题 `direct-agg` 与 3 题 `graph` 的回归断言全红。
//! - `compose`/`fastpath` 的 `accept` 恒真（今天无权限门禁），只有 `graph` 带 unrestricted 门禁；
//!   知识库 answerer 不进 Router，由 triage 直接分派（进链会让文档问句回落到 SQL 生成）。
//! - `correction_log` **九个** kind 一个不少：`run.rs` 的六个字面量（含 `schema-fix` 与 `explain-fail`）
//!   + `guard::Verdict::log_kind()` 的两个（`caliber-retry`/`caliber-unresolved`）。
//!   少一个＝一类自进化数据静默断供（`run.rs` 的 `correction_kinds_all_present` 守着）。
//!
//! 预算：≤15 个 `.rs` + 2 个外置 prompt 模板。落点清单见 `docs/ARCHITECTURE.md` §4.6。

pub mod answerers;
pub mod analysis;
pub mod answer_contract;
pub mod ask;
pub mod compound;
pub mod ctx;
pub mod entity_resolver;
pub mod gate;
pub mod gather;
pub mod guard;
pub mod hybrid;
pub mod insight;
pub mod intent;
pub mod localize;
pub mod prompt;
pub mod review;
pub mod run;
pub mod source;
pub mod triage;

// 路径一次性钉死（同 kernel/connector/policy 的做法）：同一个符号只有一条 use 路径。
pub use answerers::{Answerer, ROUTER_ORDER, ROUTE_LABELS};
pub use analysis::{AnalysisKind, AnalysisPlan, AnalysisShape, ReportSpec};
// HTTP / CLI / 定时任务三入口共用的唯一入口。`AskDeps` 里的三个 `fn` 指针与 `correctors`
// 是 T8/T10 的临时入参（实现仍在 `server/src/{direct,corrector}.rs`），那两处各有一行 ponytail 记账。
pub use ask::{
    ask, ask_prepared, ask_prepared_data_only, is_followup, prepare_question, AskDeps, DetectFn,
    HitFn, PreparedQuestion,
};
pub use ctx::{
    table_answer, truncation_note, AskCtx, AskResult, ClarifyOption, Step, SubResult, SupplementalResult,
    TrustEnvelope, ValueLabel,
};
pub use intent::{
    ExecutionEvidence, IntentCoverageSummary, IntentRoute, IntentSlotKind, IntentSlotState,
    IntentSlotSummary, IntentSummary, ResolvedIntent, RoutedQuestion,
};
// 问答参数常量与三段闸门：server 的 `exec-sql` 判官子命令与服务走同一条管道、同一组参数。
pub use gate::{ensure_limit, gate, gate_on, is_guard_err, EXEC_TIMEOUT, GUARD, MAX_ROWS};
// prompt 是纯渲染面（零 IO）；`gather` 是它的素材装配，两者一起构成 LLM 路径的输入。
pub use gather::gather;
// 「这个结果说明了什么」：口径说明（确定性）+ fast LLM 解读。按需端点 `/api/analysis` 的实现面。
pub use insight::Reading;
pub use prompt::{build_system_prompt, extract_sql, today_cn, PromptCtx};
// 自评闭环的四个入口：HTTP 路径异步 spawn 两个，CLI/定时任务调另两个（`main.rs:165/174`）。
pub use review::{review_all_pending, review_exemplar, review_failure, review_lessons};
// LLM 路径：`run_llm` 是 wire 侧的直调入口（能传真的用量回调），`LlmAnswerer` 是 Router 末位成员。
pub use run::{generate_sql, repair, run_llm, LlmAnswerer, LlmDeps};
pub use source::select_source;
