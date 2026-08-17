//! 一次问答的上下文 + 表格答案的构造。变更原因＝「一次问答需要哪些外部句柄」与「`RowSet` 怎么变答案」。
//!
//! 搬运源 `server/src/pipeline.rs:38-83`（`AskResult`/`SubResult`/`compound`）与
//! `pipeline.rs:671-701`（`ask_single` 里那段表格答案构造）。serde 形状**字节不变**：
//! 字段声明顺序即 JSON 键顺序，新字段一律 `skip_serializing_if`——前端、`tools/regression.py`、
//! `tools/evaluation.py` 三处都在解析这份 JSON，多一个恒在的字段就是一次形状破坏。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::PgPool;

use dms_connector::mysql::ReadOnlyMySql;
use dms_connector::source::{RowSet, SqlSource};
use dms_kernel::{llm::Usage, ChatModel, ChatRequest, ModelTier, ScopedSql};
use dms_policy::{scope::Scope, Principal};

use crate::gate::MAX_ROWS;
use crate::intent::{ExecutionEvidence, IntentAttempt, IntentSummary, IntentV1};

/// 一次问答（单问或复合的一个子问）的全部外部句柄。**只读上下文，不装状态**：
/// 轮次、候选 SQL、route 这些会变的东西留在 `run.rs` 的显式循环里。
///
/// 收 `&PgPool` 是允许的（自有库句柄由 server 装配后传入），**造池**才违规
/// （全仓唯一能造池的是 connector，见 ARCHITECTURE §1 门禁第 1 条）。
pub struct AskCtx<'a> {
    pub p: &'a Principal,
    pub scope: &'a Scope,
    pub question: &'a str,
    /// Fast 模型提取的表面槽位合同；仅 `Ready` 可开放缓存与自由 SQL。
    pub intent_attempt: &'a IntentAttempt,
    /// 本轮**裁决**出来的路由（`AskPlan.route`）。收据报它，不报合同的意见 ——
    /// 用户点了 chip 时两者会不一致，报合同就等于把用户的表态当没发生。
    pub decided_route: crate::intent::IntentRoute,
    pub intent: Option<&'a IntentV1>,
    pub ds: &'a str,
    /// 用户可见的实际查询目标名。主逻辑源 `dms` 可能热切到 `doris_warehouse`；
    /// 可信凭证必须写物理目标，不能让用户误以为仍在查原 DMS MySQL。
    pub source_name: &'a str,
    /// **`&dyn SqlSource` 而非具名 MySQL**：这是 ds_id 断链的头号修法 ——
    /// 具名类型会让「换个源问数」在类型层就走不通（ARCHITECTURE §4.6）。
    pub source: &'a dyn SqlSource,
    /// DMS 生产身份/业务库的只读连接。只允许 `business-lookup` 通过专用 AST gate
    /// 做单表、索引点查；通用分析仍只走 `source`，不得借此绕回生产库。
    pub auth_source: &'a ReadOnlyMySql,
    pub pg: &'a PgPool,
    /// `&Arc<dyn ChatModel>` 而非 `&dyn ChatModel`：复核与失败复盘要 `tokio::spawn`
    /// （`pipeline.rs:839/876`），spawn 进去的东西必须能 clone 成 `'static`（ARCHITECTURE §5 契约表）。
    pub llm: &'a Arc<dyn ChatModel>,
    /// 该源在注册表里是 `policy_kind='global'`（整源不做行级过滤，见 `gate::gate_on` 的文档）
    pub ds_global: bool,
    /// 本次**单问**的起点。**由调用方给，成员不许自取** ——
    /// `elapsed_ms` 要覆盖整次单问（含前面几个成员没接住所花的时间）。
    /// 成员各自 `Instant::now()` 会让排在后面的成员把自己之前的耗时全丢掉，
    /// 快路径与缓存那两处实测差十几毫秒：读日志的人会以为缓存比实际更快。
    pub t0: Instant,
    /// 🔴 本轮**不许生成新 SQL 形态**（合同没就绪 / 资料问句的问数臂）。
    ///
    /// `ask_data_arm` 的 `members.retain(|m| m.route() != "llm")` 只摘掉 Router 末位的
    /// `LlmAnswerer` —— 而 `direct-doc` 成员内部的 ODS 推导（`fastpath_intent::ods_derive`）
    /// 本身就是一次 Precise 模型自由写 SQL 并执行。两臂并行让资料问句的问数臂也跑起来之后，
    /// 只摘 Router 末位就关不住这条路了（2026-08-14 自审发现）。
    pub deterministic_only: bool,
    /// 一次问答的关联键（子问题共用父的）。透传到 `correction_log` / `failure_log` / `query_log` ——
    /// 三张表各记一段，没有这个键就拼不回同一次问答（`chat.rs:117` 已吃过一次这个亏）。
    /// **写入侧失败不许让问答失败**：观测掉了不算故障（`query_log.rs` 的纪律 1）。
    pub trace_id: String,
    /// 一次会话的关联键。CLI 没有会话概念时与 `trace_id` 相同；
    /// HTTP 聊天有 `conv_id` 时由调用方传进来（`query_log` 用它拿回本会话上一轮）。
    pub conv_id: String,
    /// 每次 precise LLM 调用后的 token 用量回调。
    ///
    /// 为什么是回调而不是返回值：落点 `Trace` 住在 `server/src/query_log.rs` 且带 axum，
    /// agent 引它就是反向依赖边（门禁 `agent 不得引 axum` + `依赖方向单向无环`）。
    /// 放在 `AskCtx` 而不是 `LlmDeps` 里，是为了让 `LlmAnswerer` 能**作为普通成员进 Router**：
    /// 它此前拿不到真回调，只能挂一个 no-op，于是 `ask_single` 必须绕过 Router 直调 `run_llm`
    /// —— 「加一种能力＝加一个 Answerer」那时只有 4/5 成立。
    pub on_usage: &'a (dyn Fn(&Usage) + Send + Sync),
}

#[derive(Serialize)]
pub struct AskResult {
    pub sql: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub truncated: bool,
    pub elapsed_ms: u128,
    pub route: String,
    pub view: dms_kernel::present::ViewSpec,
    /// 主查询之外、已经经过同一权限闸门执行的结构或明细数据。
    ///
    /// 聚合答案的顶层 `columns/rows` 是 API、CSV 与评测的主结果契约，不能再被下钻明细覆盖；
    /// 深度模式与前端从本字段读取补充数据。单据查询仍沿用“头卡 + 顶层明细”的既有契约，
    /// 因此只有聚合快路径会填它。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplemental: Option<SupplementalResult>,
    /// 同一主指标的已执行基期比较（环比/同比）。完整原值在这里，旧 `view.kpi.delta`
    /// 继续保留第一项，避免破坏精简模式和历史前端。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comparisons: Vec<KpiComparison>,
    /// 复合问题的子结果（deepagents 拆解-合并）；单结果时为空
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subs: Vec<SubResult>,
    /// 口径复核未通过的标注（回炉预算用尽仍违反声明）：结果照返，但显式说明数字不可信。
    /// `None` 时**不出现**在 JSON 里（`skip_serializing_if`）：前端、`tools/regression.py`、
    /// `tools/evaluation.py` 都在解析这份 JSON，多一个恒在的字段就是一次形状破坏。
    /// 有断言锁（`caliber_note_is_omitted_when_absent`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caliber_note: Option<String>,
    /// 命中行上限时的截断三件套（原因 / 范围 / 续读参数），见 `truncation_note`。
    /// 同样 `skip_serializing_if`：老前端不改也不崩。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation_note: Option<String>,
    /// 被敏感列防线**整列置空**的列名（`RowSet.redacted`）。
    ///
    /// 🔴 不带出来的后果是实测过的：那几列在界面上就是一片空值，**用户把脱敏当成故障**
    /// （「这个字段怎么查不出来」），而系统其实正确地拒绝了它。
    /// 内容必须与 `columns` 里的列名**逐字相同** —— 前端按字符串相等定位列。
    /// 同样 `skip_serializing_if`：没有脱敏列时这个键不出现，老前端与两个 runner 的形状不变。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub redacted: Vec<String>,
    /// 行级权限**生效了**的回显。`None` = 这次没有注入任何权限过滤（超管/ALL 档）。
    ///
    /// 🔴 为什么这是正确性而不是产品面：受限用户看到的是**子集**，而界面上没有一个字说明
    /// 「这不是全量」—— 于是他拿着被过滤的数去下结论（「我们本月只有 12 个客户？」）。
    /// 那件事不会报错，也不会被任何判据抓到。
    ///
    /// 值从 `ScopedSql::is_unrestricted()` 来 —— 那个方法此前是**零生产调用方的死代码**
    /// （全仓 `is_unrestricted()` 的命中全是 `ScopeSets` 那个同名方法或测试）：
    /// 回显要的那个 bit 早就算好了，只是没人取。
    /// 具体注入了哪些条件不在这里说（那是 `insight.rs` 的 `caliber` 干的活，它读已执行的 SQL）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_note: Option<String>,
    /// 「已按理解为你想问：X」的透出（`ask.rs` 的 AI 重新理解层重试**命中**时）：
    /// 问句先出了「不可计算」卡，fast 归一成标准问法后重试命中 —— 这个字段说明
    /// 答案对应的是归一后的问法，不是用户原句。
    /// 🔴 不借道 `caliber_note`：ResultPanel 对数据类 route 的 caliber_note 首屏只显示
    /// 固定句「当前结果未通过业务口径复核」（web/src/ResultPanel.vue:534，原文折叠进
    /// 核查详情）—— 那会把一次成功的归一命中误报成口径违规。
    /// 同样 `skip_serializing_if`：不重理解整键不上线，前端与两个判官脚本的形状不变。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reinterpret_note: Option<String>,
    /// 本轮实际生效的问句（追问改写/日期继承/归一重试之后的形态，与用户原句不同才填）：
    /// 会话持久化靠它让下一轮追问继承到**完整问句**而不是用户上一句碎片（「上月呢？」
    /// 本身是碎片，链式追问会因此丢实体——2026-08-12 实测）。None = 整键不上线，wire 不变。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_question: Option<String>,
    /// 本次结果的可核查可信凭证。等级只依据路由、口径判据、截断与权限事实计算，
    /// 不接受 LLM 自评概率。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<TrustEnvelope>,
    /// 全链路分步留痕（A6）：Router 各成员依次出手的结果，一条成员一步。
    /// 空 = 没走 Router（need-intent 反问、复合容器）—— `skip_serializing_if`
    /// 保证老前端与两个判官脚本的 JSON 形状不变。命中那步的真实口径看 `route`，
    /// 不看 `steps` 末位的表标签（llm 路径命中时 route 会是 `llm+repair` 等）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// 意图反问的结构化候选（need-intent 增强）：fast LLM 给的 2~4 个最可能问法。
    /// 空 = 降级为纯文本反问（与引入前逐字等价），整键不上线 —— 老客户端零影响。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clarify_options: Vec<ClarifyOption>,
    /// 码值翻译留痕（`present_cn`）：单元格显示中文名，原始码在这里保留
    /// （前端 tooltip / 对数用）。空 = 本轮没有翻译，整键不上线。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub value_labels: Vec<ValueLabel>,
    /// 销售类单指标 KPI 的**同窗补充**（裁决：销售额/销量/毛利额等单值答案
    /// 顺带成本/收入/毛利额/毛利率）。与主查询同一时间窗、同一权限闸门；
    /// 取数失败/为空 = `None` 整键不上线，主回答一个字符不变（含 `sql` 展示串——
    /// 金标把它逐字钉死）。只有 sales_fact 标量命中会填它。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sales_context: Option<SalesContextResult>,
    /// 面向回归和用户诊断的非敏感意图收据；不含 prompt、SQL AST 或内部实体 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_summary: Option<IntentSummary>,
    /// **知识库半**（两臂并行的产物）。`None` = 本轮资料侧没有实质内容，整键不上线，
    /// 老消费者逐字节不变。
    ///
    /// 🔴 为什么是一个附加字段而不是把两半折成 `compound`：`compound` 容器把顶层
    /// `sql`/`columns`/`rows`/`row_count`/`view` 全部清空（`row_count` 恒 0），于是
    /// 「导出 CSV」「AI 解读」两个按钮消失、收据变空、`tools/regression.py` 的 15 个断言里
    /// 有 13 个读的正是这几个顶层字段（实测 79 题会红 68 题）。
    ///
    /// 键名 `kb` 是**既有协议**：混合问句的 `hybrid_payload` 早就手工塞
    /// `v["kb"]`，前端 `App.vue` 的 `t.result?.kb` 分支已经在渲染它（含引用角标与
    /// 「下载原件」按钮）—— 所以这条改动前端零改。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb: Option<dms_kernel::Answer>,
}

/// 反问候选（need-intent）：`label` 是短标签（≤6 字预期），`question` 是可直接重发的完整问句。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ClarifyOption {
    pub label: String,
    pub question: String,
}

/// 码值翻译留痕：`column` 列里原始码 `code` 显示成了 `label`。
/// `column` 与翻译后的 `columns` 逐字一致（前端按名定位列）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ValueLabel {
    pub column: String,
    pub code: String,
    pub label: String,
}

#[derive(Serialize)]
pub struct SupplementalResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub truncated: bool,
    pub view: dms_kernel::present::ViewSpec,
}

/// 销售单指标 KPI 的同窗补充：恒单行五值（销售额/不含税成本/不含税收入/毛利额/毛利率），
/// 列名即合同中文别名（无需 present_cn 翻译，locale 出口对它本就零改动）。
/// 不带 view/row_count：前端按固定四格小卡渲染，形状刻意比 `SupplementalResult` 窄。
#[derive(Serialize)]
pub struct SalesContextResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

#[derive(Serialize, Clone)]
pub struct KpiComparison {
    pub label: String,
    pub current: f64,
    pub baseline: f64,
    pub change: f64,
    pub pct: f64,
    pub dir: &'static str,
}

/// 用户可核查的答案凭证。只放现有链路已经能证明的事实；指标版本与数据截至时间
/// 等语义层落地后再以可选字段扩展，今天不编造一个看似完整的值。
#[derive(Serialize)]
pub struct TrustEnvelope {
    /// `verified` = 确定性/已复核路径；`high` = LLM SQL 经全闸门执行；`review` = 有明确风险标注。
    pub level: &'static str,
    pub trace_id: String,
    pub source: String,
    pub route: String,
    /// 当前答案采用的权限边界，不暴露具体客户/员工集合。
    pub access: String,
    /// `实时执行` 或 `图谱快照`；语义缓存只缓存 SQL，不缓存结果，仍属于实时执行。
    pub execution: &'static str,
    /// 已执行 SQL/Cypher 描述的稳定短指纹，用于复核同一计算，不回显全文。
    pub fingerprint: String,
    pub checks: Vec<String>,
}

/// 分步留痕的一步（A6）：Router 一个成员的出手结果。
/// 只记 {表标签, 结果, 耗时} —— 问句与 SQL 原文 `query_log` 已存，这里再带一份
/// 就是每行答案多扛几百字节（计划 90 行那条纪律）。
#[derive(Serialize)]
pub struct Step {
    /// 成员的表标签（`Answerer::route()`）
    pub stage: &'static str,
    /// `hit` 接住 / `miss` 没接住交下一个 / `skip` 门禁没放行（`accept` 为假，
    /// 如受限用户过不了 graph、追问过不了缓存）—— skip 是「为什么没走这条路」的唯一记录
    pub kind: &'static str,
    pub ms: u128,
}

/// 复合子问题结果（deepagents SubAgent 收敛：每子问题一句题目 + 完整结果）
#[derive(Serialize)]
pub struct SubResult {
    pub question: String,
    pub result: AskResult,
}

impl AskResult {
    /// 复合容器：主体空，subs 装各子结果，前端分面板渲染
    pub fn compound(subs: Vec<SubResult>, elapsed_ms: u128) -> Self {
        AskResult {
            sql: "[复合问题拆解]".into(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            truncated: false,
            elapsed_ms,
            route: "compound".into(),
            view: dms_semantic::present::build(&[], &[]),
            supplemental: None,
            comparisons: vec![],
            reinterpret_note: None,
        resolved_question: None,
            subs,
            // 容器本身没有 SQL 可判；每个子结果各自带着自己的标注
            caliber_note: None,
            truncation_note: None,
            redacted: vec![],
            // 容器自己不产 SQL，权限回显在每个子结果上
            scope_note: None,
            trust: None,
            // 复合容器没走 Router（子问句各自的 steps 在各自 `result` 里）
            steps: vec![],
            clarify_options: vec![],
            value_labels: vec![],
            sales_context: None,
            intent_summary: None,
            kb: None,
        }
    }
}

/// `RowSet` → 表格答案（**`view` 恒 Some**：`ViewSpec` 不是 `Option`，前端不当可选，裁决 T9-1）。
/// 逐行搬 `ask_single`/`try_semantic_cache` 里那段构造：三条确定性路径、语义缓存、LLM 路径
/// 共用同一份，`route` 由**命中方**给（`Answerer::route()` 只是表标签）。
///
/// KPI 环比与口径标注在构造之后由调用方补（`patch_kpi_delta(&mut r.view, ..)` /
/// `r.caliber_note = ..`）：视图本来就是先按 columns/rows 建好再打补丁的，顺序一字不差。
pub fn table_answer(
    scoped: &ScopedSql,
    rs: RowSet,
    route: impl Into<String>,
    t0: Instant,
) -> AskResult {
    // `rs.redacted` 曾长期无消费者（裁决 二·D T4-5 记的那笔债），于是被脱敏的列在界面上
    // 只是一片空值 —— 用户把它当故障。现在带出去，前端按列名打「已脱敏」角标。
    let RowSet {
        columns,
        rows,
        redacted,
        truncated: cut,
    } = rs; // 列全字段不写 `..`：RowSet 再加字段时编译期强制决策
    let row_count = rows.len();
    let view = dms_semantic::present::build(&columns, &rows);
    // wire() 取一次复用：sql 字段与 truncation_note 各要一份
    let wire = scoped.wire();
    AskResult {
        sql: wire.to_string(),
        columns,
        // 🔴 两个来源取或：`cut` 是取数侧「我在上限处截断了」的**事实**，
        // `row_count >= MAX_ROWS` 是行数撞上限的**推断**。ds 策略把上限压到 50/20 时
        // 后者恒为假 —— 几十行的结果会被当成全量呈现，脚注一个字不提（2026-08-14 审计）。
        truncated: cut || row_count >= MAX_ROWS,
        row_count,
        rows,
        elapsed_ms: t0.elapsed().as_millis(),
        route: route.into(),
        view,
        supplemental: None,
        comparisons: vec![],
        reinterpret_note: None,
        resolved_question: None,
        subs: vec![],
        caliber_note: None,
        truncation_note: truncation_note(wire, row_count),
        redacted,
        scope_note: (!scoped.is_unrestricted())
            .then(|| "结果已按你的数据权限过滤：看到的是你有权访问的那部分，不是全量".to_string()),
        // `ask_single` 在知道 ds/trace/完整 Router 结果后统一补，构造器不猜这些事实。
        trust: None,
        // 由 `ask_single` 的分派循环在命中后补上（这里不知道前面几个成员出过手）
        steps: vec![],
        clarify_options: vec![],
        value_labels: vec![],
        sales_context: None,
        intent_summary: None,
        kb: None,
    }
}

/// 「单据类型」列名：columns 与 Entity pairs 两处判据共用，改名只动这里。
const DOC_TYPE_COL: &str = "单据类型";

/// 可信级别判级（纯函数，可单测）：direct-derive 恒为 review —— 推导口径未经合同验证，
/// 与「口径复核未通过/截断」同一档；它既不是确定性路径（不进 verified），也不是普通
/// LLM 答案（不止 high），凭证上必须一眼可分。
fn trust_level(route: &str, risk: bool, deterministic: bool) -> &'static str {
    if route == "direct-derive" || risk {
        "review"
    } else if deterministic {
        "verified"
    } else {
        "high"
    }
}

/// 在 Router 命中出口统一补可信凭证。放在这里避免七条 Answerer 各写一份等级和权限文案。
pub(crate) fn attach_trust(cx: &AskCtx<'_>, r: &mut AskResult) {
    attach_intent_summary(cx, r, &ExecutionEvidence::default());
    attach_result_verification(cx, r);
    if r.sql.trim().is_empty() || matches!(r.route.as_str(), "need-intent" | "compound") {
        return;
    }
    // 主查询与补充明细任一截断都算风险（checks 文案用同一个 bit，等级与凭证不自相矛盾）
    let any_truncated = r.truncated
        || r.supplemental
            .as_ref()
            .is_some_and(|detail| detail.truncated);
    let intent_unverified = !cx.intent_attempt.is_data_executable();
    let receipt_blocked = r
        .intent_summary
        .as_ref()
        .is_some_and(|summary| summary.coverage.status != "complete");
    let risk = r.caliber_note.is_some() || any_truncated || intent_unverified || receipt_blocked;
    let deterministic = matches!(
        r.route.as_str(),
        "direct-agg"
            | "direct-doc"
            | "entity-card"
            | "business-lookup"
            | "graph"
            | "semantic-cache"
    );
    let level = trust_level(&r.route, risk, deterministic);
    let derived = r.route == "direct-derive";
    let business_lookup = r.route == "business-lookup";
    let access = if business_lookup {
        if cx.scope.unrestricted_by_role() {
            "当前 DMS 角色全量权限".to_string()
        } else {
            "DMS 账号行级权限".to_string()
        }
    } else if cx.ds_global {
        "数据源级授权".to_string()
    } else if cx.scope.unrestricted_by_role() {
        "当前角色全量权限".to_string()
    } else {
        "DMS 账号行级权限".to_string()
    };
    let mut checks = vec!["只读执行通道".to_string(), "当前身份权限已校验".to_string()];
    if intent_unverified {
        checks.push("结构化意图未获有效合同；仅确定性路径执行，本结果需复核".to_string());
    }
    if receipt_blocked {
        checks.push("执行结果未通过完整性或值级核验，请勿作为最终口径".to_string());
    } else if cx.intent.is_some() {
        checks.push("已核对返回行形状与请求指标的实际值".to_string());
    }
    if business_lookup {
        checks.push("生产 DMS 单表轻查询：索引条件、小 LIMIT、2 秒超时".to_string());
    }
    checks.push(if derived {
        "推导口径：合同未覆盖，由 ODS 明细推导，未经合同验证".to_string()
    } else if deterministic {
        if r.route == "graph" {
            "图关系仅对具备全量权限的身份开放".to_string()
        } else {
            "确定性业务路径或已复核 SQL".to_string()
        }
    } else {
        "模型 SQL 已通过安全闸门、权限注入与执行校验".to_string()
    });
    checks.push(if derived {
        "未经合同口径复核，数字仅作排查参考".to_string()
    } else if r.caliber_note.is_none() {
        "未发现口径规则冲突".to_string()
    } else {
        "存在口径复核提醒，请人工确认".to_string()
    });
    checks.push(if any_truncated {
        "结果已截断，不能视为完整明细".to_string()
    } else {
        "结果未触发行数截断".to_string()
    });
    if !r.redacted.is_empty() {
        checks.push("敏感列已按策略脱敏".to_string());
    }
    let has_document_evidence = r.columns.iter().any(|c| c == DOC_TYPE_COL)
        || r.view.blocks.iter().any(|b| match b {
            dms_kernel::present::Block::Entity { pairs } =>
                pairs.iter().any(|(key, _)| key == DOC_TYPE_COL),
            _ => false,
        });
    if matches!(r.route.as_str(), "direct-doc" | "business-lookup") && has_document_evidence {
        checks.push("单号已匹配源码单据族，主表与明细表映射已返回".to_string());
    }
    r.trust = Some(TrustEnvelope {
        level,
        trace_id: cx.trace_id.clone(),
        source: if business_lookup {
            "DMS 生产只读轻查询".to_string()
        } else {
            cx.source_name.to_string()
        },
        route: r.route.clone(),
        access,
        execution: if r.route == "graph" { "图谱快照" } else { "实时执行" },
        fingerprint: sql_fingerprint(&r.sql),
        checks,
    });
}

/// 执行后的最终闸：SQL 覆盖只证明“查询计划表达了什么”，不能证明引擎真的返回了
/// 可用值。这里复用 `IntentSummary.coverage` 作为唯一收据，把实际行集、指标值、比较值
/// 和补充结果一起核验。任何问题都只会把 trust 降为 review，不伪造新的状态协议。
fn attach_result_verification(cx: &AskCtx<'_>, r: &mut AskResult) {
    let issues = result_verification_issues(cx.intent, r);
    if issues.is_empty() {
        return;
    }
    let Some(summary) = r.intent_summary.as_mut() else {
        return;
    };
    summary.coverage.status = "blocked";
    for issue in issues {
        if !summary.coverage.issues.contains(&issue) {
            summary.coverage.issues.push(issue);
        }
    }
}

fn result_verification_issues(intent: Option<&IntentV1>, r: &AskResult) -> Vec<String> {
    let mut issues = Vec::new();
    let mut add = |issue: String| {
        if !issues.contains(&issue) {
            issues.push(issue);
        }
    };

    if r.row_count != r.rows.len() {
        add("result:row-count-mismatch".into());
    }
    if r.rows.iter().any(|row| row.len() != r.columns.len()) {
        add("result:column-shape-mismatch".into());
    }
    if let Some(detail) = &r.supplemental {
        if detail.row_count != detail.rows.len() {
            add("result:supplemental-row-count-mismatch".into());
        }
        if detail
            .rows
            .iter()
            .any(|row| row.len() != detail.columns.len())
        {
            add("result:supplemental-column-shape-mismatch".into());
        }
    }
    if let Some(context) = &r.sales_context {
        if context
            .rows
            .iter()
            .any(|row| row.len() != context.columns.len())
        {
            add("result:sales-context-shape-mismatch".into());
        }
    }

    let Some(intent) = intent else { return issues };
    if intent.route() != crate::intent::IntentRoute::Data {
        return issues;
    }
    let has_primary_value = r.rows.iter().flatten().any(cell_has_value);
    if !has_primary_value {
        add("result:empty".into());
    }
    for metric in &intent.metrics {
        if !metric_has_actual_value(metric, r) {
            add(format!("result:metric-unverified:{metric}"));
        }
    }
    if !intent.comparisons.is_empty() {
        let structured = r
            .comparisons
            .iter()
            .filter(|comparison| comparison_is_valid(comparison))
            .count();
        if structured != r.comparisons.len() {
            add("result:comparison-invalid".into());
        }
        let inline = r
            .columns
            .iter()
            .enumerate()
            .filter(|(index, column)| {
                comparison_column(column)
                    && r.rows
                        .iter()
                        .any(|row| row.get(*index).is_some_and(cell_is_number))
            })
            .count();
        if structured + inline < intent.comparisons.len() {
            add("result:comparison-incomplete".into());
        }
    }
    if intent.requested_detail {
        let supplemental = r
            .supplemental
            .as_ref()
            .is_some_and(|detail| detail.rows.iter().flatten().any(cell_has_value));
        let primary_detail = has_primary_value && (r.columns.len() > 1 || r.rows.len() > 1);
        if !primary_detail && !supplemental {
            add("result:detail-empty".into());
        }
    }
    if let Some(current) = primary_scalar(r) {
        if r.comparisons
            .iter()
            .any(|comparison| (comparison.current - current).abs() > comparison_tolerance(current))
        {
            add("result:comparison-current-mismatch".into());
        }
    }
    issues
}

fn cell_has_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(value) => !value.trim().is_empty(),
        serde_json::Value::Array(values) => values.iter().any(cell_has_value),
        serde_json::Value::Object(values) => values.values().any(cell_has_value),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
    }
}

fn cell_is_number(value: &serde_json::Value) -> bool {
    cell_number(value).is_some()
}

fn cell_number(value: &serde_json::Value) -> Option<f64> {
    let number = match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(value) => value
            .trim()
            .trim_end_matches('%')
            .replace(',', "")
            .parse()
            .ok(),
        _ => None,
    };
    number.filter(|value| value.is_finite())
}

fn primary_scalar(r: &AskResult) -> Option<f64> {
    (r.columns.len() == 1 && r.rows.len() == 1)
        .then(|| r.rows.first()?.first().and_then(cell_number))?
}

fn comparison_tolerance(value: f64) -> f64 {
    value.abs().max(1.0) * 1e-9
}

fn comparison_is_valid(comparison: &KpiComparison) -> bool {
    if ![
        comparison.current,
        comparison.baseline,
        comparison.change,
        comparison.pct,
    ]
    .into_iter()
    .all(f64::is_finite)
        || (comparison.change - (comparison.current - comparison.baseline)).abs()
            > comparison_tolerance(comparison.current)
    {
        return false;
    }

    let expected_pct = if comparison.baseline.abs() >= 1e-6 {
        comparison.change / comparison.baseline * 100.0
    } else {
        0.0
    };
    (comparison.pct - (expected_pct * 10.0).round() / 10.0).abs() <= 0.051
}

fn metric_has_actual_value(metric: &str, r: &AskResult) -> bool {
    // 🔴 量词前缀不改变指标本身：`总库存量` 要按 `库存量` 查别名表。
    // 别名表是**硬编码的键匹配**（`_ => return false`），`总库存量` 不在键里 ——
    // 2026-08-17 生产实测：答出 1.06 亿，收据却挂 `result:metric-unverified:总库存量`，
    // 给了数字又说证不出来。词表在 `dms_semantic::direct_types`，全仓唯一一份。
    let metric = dms_semantic::direct_types::strip_quantifier_prefix(metric)
        .filter(|stripped| *stripped != metric)
        .unwrap_or(metric);
    let aliases: &[&str] = match metric {
        "销售额" | "销售总额" | "销售金额" | "营业额" => {
            &[
                "销售额",
                "销售总额",
                "销售金额",
                "营业额",
                "sales_amount",
                "amount",
            ]
        }
        "销量" | "销售量" | "销售数量" => {
            &[
                "销量",
                "销售量",
                "销售数量",
                "sales_qty",
                "sales_quantity",
                "qty",
                "quantity",
            ]
        }
        "库存量" | "库存数量" | "库存总量" => {
            &["库存量", "库存数量", "库存总量", "in_stock_quantity"]
        }
        "订单数" | "订单数量" => &["订单数", "订单数量", "order_count"],
        "毛利额" | "毛利润" => &["毛利额", "毛利润", "gross_profit"],
        "毛利率" => &["毛利率", "gross_margin"],
        "不含税成本" => &["不含税成本", "cost_excluding_tax"],
        "不含税收入" => &["不含税收入", "revenue_excluding_tax"],
        _ => return false,
    };
    let indices = r
        .columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| {
            aliases
                .iter()
                .any(|alias| metric_column_matches_alias(column, alias))
                .then_some(index)
        })
        .collect::<Vec<_>>();
    indices.into_iter().any(|index| {
        r.rows
            .iter()
            .any(|row| row.get(index).is_some_and(cell_is_number))
    })
}

/// Result columns are evidence, not a hint: a numeric scalar only verifies a metric when its
/// column carries that metric's controlled name. In particular, substring matching would let
/// `discount_amount` satisfy `amount` and `account_count` satisfy `count`.
fn metric_column_matches_alias(column: &str, alias: &str) -> bool {
    let column = normalized_metric_column(column);
    let alias = normalized_metric_column(alias);
    column == alias
}

fn normalized_metric_column(value: &str) -> String {
    let mut value = value
        .trim()
        .trim_matches(|ch| matches!(ch, '`' | '"' | '\''))
        .to_lowercase();
    loop {
        let Some((head, suffix)) = metric_unit_suffix(&value) else {
            break;
        };
        if !matches!(
            suffix,
            "元" | "万元" | "亿元" | "个" | "件" | "箱" | "单" | "笔" | "%" | "百分比"
        ) {
            break;
        }
        value = head.trim_end_matches(['_', '-', ' ']).trim().to_string();
    }
    value
}

fn metric_unit_suffix(value: &str) -> Option<(&str, &str)> {
    if let Some(head) = value.strip_suffix('）') {
        let (head, suffix) = head.rsplit_once('（')?;
        return Some((head, suffix.trim()));
    }
    if let Some(head) = value.strip_suffix(')') {
        let (head, suffix) = head.rsplit_once('(')?;
        return Some((head, suffix.trim()));
    }
    value
        .rsplit_once(['_', '-', ' '])
        .map(|(head, suffix)| (head, suffix.trim()))
}

fn comparison_column(column: &str) -> bool {
    let folded = column.to_lowercase();
    ["同比", "环比", "较上", "change", "delta", "growth", "pct"]
        .iter()
        .any(|word| folded.contains(word))
}

pub(crate) fn attach_intent_summary(
    cx: &AskCtx<'_>,
    r: &mut AskResult,
    evidence: &ExecutionEvidence,
) {
    if r.intent_summary.is_some() {
        return;
    }
    let coverage = if r.sql.trim().is_empty()
        || matches!(
            r.route.as_str(),
            "need-intent" | "compound" | "graph" | "entity-card"
        ) {
        let mut report = crate::intent::CoverageReport::default();
        if cx.intent.is_some() {
            report
                .unverifiable
                .push(format!("route:{}:coverage-unverifiable", r.route));
        }
        Some(report)
    } else {
        Some(crate::intent::sql_coverage(
            cx.intent,
            &r.sql,
            cx.source.dialect(),
        ))
    };
    r.intent_summary = Some(cx.intent_attempt.summary(coverage.as_ref(), evidence, cx.decided_route));
}

/// FNV-1a 64 位偏移基（`sql_fingerprint` 与 `summary_cache_key` 共用同一算法起点）。
const FNV_OFFSET: u64 = 0xcbf29ce484222325;

/// FNV-1a 64 位：零依赖、跨进程稳定，足够作为复核短指纹；它不是安全哈希，也不承担签名。
fn sql_fingerprint(sql: &str) -> String {
    let mut h = FNV_OFFSET;
    for token in sql.split_whitespace() {
        fnv1a_feed(&mut h, token.as_bytes());
        fnv1a_feed(&mut h, b" ");
    }
    format!("{h:016x}")
}

// ──────────────────────── 长会话两级摘要（Y10）与 D7 落账暂存 ────────────────────────
//
// 【Y10 两级摘要】会话历史超过 `SUMMARY_KEEP_RECENT` 轮时，早期轮先由 fast LLM 压成一段摘要
// 再进上下文，最近 N 轮保留原文 —— 比「超预算硬截」多保住早期轮语义。摘要失败/超时一律
// 回退原硬截语义（早期轮整段省略 + 一行明说），每处降级都 warn 留痕，绝不让装配失败。
//
// 本层只有纯函数 + 一次 fast 调用，刻意不做两件 IO：
// - **不读 `chat.msg`**：会话消息的读写口在 server `chat.rs`（本 crate 门禁：不得 `sqlx::query`，
//   check-arch 硬 FAIL；且「同一信任边界两份实现必然漂」）；
// - **不读写 `meta.kv`**：摘要缓存的 KV 两行用 server 侧既有 `admin_api::KV_GET_SQL/KV_SET_SQL`，
//   本层只产出**键**（`summary_cache_key`，含轮次指纹 —— 缓存正确性的全部难点都在这里，由单测守）。
//
// 调用方流程（server 侧装配历史进 prompt 时）：
//   let (early, recent) = split_turns(&turns);
//   if needs_summary(turns.len()) {
//       let key = summary_cache_key(conv_id, early);   // kv 命中直接用；
//       // 未命中 → summarize_early_turns(llm, early).await → 写 kv；
//       let block = render_history_block(summary.as_deref(), early.len(), recent);
//   }
// 超长工具结果（大表格/长 SQL）走 `externalize_*`：上下文里只留「指针 + 头部」。

/// 一轮对话的摘要素材：问句 + 那轮**实际执行**的 SQL + 结果行数。
///
/// SQL/行数是 `Option` 而不是必填：知识库轮/反问轮没有可执行 SQL
/// （与 `ask::PrevTurn` 的第二位同一约定）—— 那一档照样参与计数与指纹，
/// 摘要输入里只写问句，不编造一个不存在的口径。
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub question: String,
    pub sql: Option<String>,
    pub row_count: Option<usize>,
}

/// 保留原文的最近轮数 N。历史轮 > N 才触发摘要（`needs_summary`）。
pub const SUMMARY_KEEP_RECENT: usize = 6;
/// 表格结果超过 M 行时外置：上下文里只留「[表格 N 行，前 5 行：…]」的指针形态。
pub const TABLE_EXTERNAL_ROWS: usize = 50;
/// SQL 超过 K 字符时外置：只留「[SQL 共 N 字符，前 300：…]」。
pub const SQL_EXTERNAL_CHARS: usize = 800;
/// 外置表格带进上下文的头部行数（附件区形态：指针 + 头部）。
const EXTERNAL_HEAD_ROWS: usize = 5;
/// 外置 SQL 带进上下文的头部字符数（**字符**，按字节截会把中文切成半个字 —— 本仓踩过三次）。
const EXTERNAL_SQL_HEAD_CHARS: usize = 300;
/// 外置单元格渲染上限：指针形态要的是「看得出是什么」，不是把附件搬进上下文。
const EXTERNAL_CELL_CHARS: usize = 40;

// 指针分支隐含不变量：上限必须大于头部行数，否则「前 5 行 / 其余 N-5 行」对不上
const _: () = assert!(TABLE_EXTERNAL_ROWS > EXTERNAL_HEAD_ROWS);
/// fast 摘要的等待上限：超时 = 失败 = 回退硬截（与 `triage`/`ask` 的 fast 调用同族降级）。
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(5);

/// 触发判据（纯函数）：历史轮 > N 时早期轮才压摘要；≤ N 时全量原文，一个字符都不动。
pub fn needs_summary(n_turns: usize) -> bool {
    n_turns > SUMMARY_KEEP_RECENT
}

/// 拆出「要压摘要的早期轮」与「保留原文的最近 N 轮」（纯函数）。
/// 轮数 ≤ N 时 early 恒空 —— 调用方无条件拆，不需要先判 `needs_summary`。
pub fn split_turns(turns: &[Turn]) -> (&[Turn], &[Turn]) {
    let keep = SUMMARY_KEEP_RECENT.min(turns.len());
    turns.split_at(turns.len() - keep)
}

/// 摘要缓存键（纯函数）：`ctxsum:{conv_id}:{指纹}`。
///
/// 🔴 正确性全在指纹的覆盖面上：它对**被摘要的那一段**（early 全体、逐轮 问句+SQL+行数）
/// 做长度前缀 FNV —— 新轮进来时 early 变长/内容变 → 指纹变 → 键变 → 旧摘要自然失效重算，
/// 不存在「摘要漏了新消息」的窗口。长度前缀防拼接歧义（("ab","c") 与 ("a","bc") 必须不同键）；
/// `None` 的行数与 `Some(0)` 必须可分（「那轮没出数」≠「那轮出了 0 行」）。
pub fn summary_cache_key(conv_id: &str, early: &[Turn]) -> String {
    let mut h = FNV_OFFSET;
    fnv1a_feed(&mut h, &(early.len() as u64).to_le_bytes());
    for t in early {
        fnv1a_feed(&mut h, &(t.question.len() as u64).to_le_bytes());
        fnv1a_feed(&mut h, t.question.as_bytes());
        let sql = t.sql.as_deref().unwrap_or("");
        fnv1a_feed(&mut h, &(sql.len() as u64).to_le_bytes());
        fnv1a_feed(&mut h, sql.as_bytes());
        fnv1a_feed(&mut h, &t.row_count.map(|n| n as u64).unwrap_or(u64::MAX).to_le_bytes());
    }
    format!("ctxsum:{conv_id}:{h:016x}")
}

/// FNV-1a 64 位的流式喂法（与 `sql_fingerprint` 同一族：零依赖、跨进程稳定，不是安全哈希）。
fn fnv1a_feed(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100000001b3);
    }
}

/// 长 SQL 外置（纯函数）：≤ K 字符原样返回；超过则压成单行指针形态
/// （空白含换行归一成单个空格 —— 附件区只留头部，多行原文不进上下文）。
pub fn externalize_sql(sql: &str) -> String {
    let total = sql.chars().count();
    if total <= SQL_EXTERNAL_CHARS {
        return sql.to_string();
    }
    let head = sql
        .chars()
        .take(EXTERNAL_SQL_HEAD_CHARS)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[SQL 共 {total} 字符，仅前 {EXTERNAL_SQL_HEAD_CHARS} 字符进上下文：{head}…]"
    )
}

/// 大表格外置（纯函数）：≤ M 行渲染全量文本（调用方大小表共用这一个入口）；
/// 超过则指针形态「[表格 N 行，前 5 行：…]」—— 表头行 + 前 `EXTERNAL_HEAD_ROWS` 行
/// 进上下文，其余明说去结果区看。单元格按字符截 `EXTERNAL_CELL_CHARS`。
pub fn externalize_table(columns: &[String], rows: &[Vec<serde_json::Value>]) -> String {
    // 列名与单元格同口径按字符截：100 列长列名的小表会把整个表头灌进上下文
    let header = if columns.is_empty() {
        String::new()
    } else {
        let names = columns
            .iter()
            .map(|c| c.chars().take(EXTERNAL_CELL_CHARS).collect::<String>())
            .collect::<Vec<_>>()
            .join(" | ");
        format!("{names}\n")
    };
    if rows.len() <= TABLE_EXTERNAL_ROWS {
        let mut s = header;
        for r in rows {
            s.push_str(&render_external_row(r));
            s.push('\n');
        }
        return s.trim_end().to_string();
    }
    let kept = rows
        .iter()
        .take(EXTERNAL_HEAD_ROWS)
        .map(|r| render_external_row(r))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[表格 {} 行，前 {EXTERNAL_HEAD_ROWS} 行：\n{header}{kept}\n…其余 {} 行在结果区，不进上下文]",
        rows.len(),
        rows.len() - EXTERNAL_HEAD_ROWS
    )
}

fn render_external_row(row: &[serde_json::Value]) -> String {
    row.iter()
        .map(|v| match v {
            // 字符串单元格直接截断收集：先整串克隆再 take，长单元格白拷贝一份
            serde_json::Value::String(s) => s.chars().take(EXTERNAL_CELL_CHARS).collect::<String>(),
            serde_json::Value::Null => "NULL".to_string(),
            other => {
                let s = other.to_string();
                s.chars().take(EXTERNAL_CELL_CHARS).collect::<String>()
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 历史段的最终渲染（纯函数）：摘要段（或硬截标记）+ 最近 N 轮原文。
///
/// `early_summary = None` 就是**降级语义 = 原硬截**：早期轮整段不进上下文，
/// 但留一行「（更早的 K 轮对话已省略）」—— 静默省略会让模型把「没有更早轮次」当事实。
/// `early_count = 0` 时一个标记都不出（与「未触发」逐字等价）。
pub fn render_history_block(early_summary: Option<&str>, early_count: usize, recent: &[Turn]) -> String {
    let mut s = String::new();
    if early_count > 0 {
        match early_summary {
            Some(sum) => s.push_str(&format!("## 更早的 {early_count} 轮对话（摘要）\n{sum}\n")),
            None => s.push_str(&format!("（更早的 {early_count} 轮对话已省略）\n")),
        }
    }
    if !recent.is_empty() {
        if early_count > 0 {
            s.push_str("## 最近对话（原文）\n");
        }
        for (i, t) in recent.iter().enumerate() {
            // 编号从 early_count + 1 起：有早期轮被省略/摘要时，「最近对话」的第 1 轮
            // 实际是全历史的第 K+1 轮，从 1 编会让模型引用轮号时歧义
            s.push_str(&format!("{}. 问：{}\n", early_count + i + 1, t.question));
            if let Some(sql) = t.sql.as_deref().filter(|s| !s.trim().is_empty()) {
                s.push_str(&format!("   SQL：{}\n", externalize_sql(sql)));
            }
            if let Some(n) = t.row_count {
                s.push_str(&format!("   结果：{n} 行\n"));
            }
        }
    }
    s
}

/// 早期轮 → 一段摘要：**fast 一发**，失败/超时/回空一律 `None`（调用方回退硬截，各分支 warn）。
pub async fn summarize_early_turns(llm: &dyn ChatModel, early: &[Turn]) -> Option<String> {
    summarize_with_timeout(llm, early, SUMMARY_TIMEOUT).await
}

/// 超时可参数化的一半：单测要拿小预算去戳「超时回退」那条路（5s 的常量等不起）。
async fn summarize_with_timeout(llm: &dyn ChatModel, early: &[Turn], budget: Duration) -> Option<String> {
    if early.is_empty() {
        return None;
    }
    let system = "#角色：你是数据问答会话的整理员。\n\
                  #任务：把早期对话轮次压缩成一段摘要，保住：问过的指标、维度、时间口径、筛选条件与每轮结果量级。\n\
                  #规则：1. 只输出摘要正文，不要解释、不要小节标题；2. 不超过 400 字；\
                  3. 没出现过的口径一个字都不编；4. 不写 SQL。";
    let mut user = String::from("#早期对话：\n");
    for (i, t) in early.iter().enumerate() {
        user.push_str(&format!("{}. 问：{}\n", i + 1, t.question));
        if let Some(sql) = t.sql.as_deref().filter(|s| !s.trim().is_empty()) {
            user.push_str(&format!("   SQL：{}\n", externalize_sql(sql)));
        }
        if let Some(n) = t.row_count {
            user.push_str(&format!("   结果：{n} 行\n"));
        }
    }
    user.push_str("#摘要：");
    let req = ChatRequest::text(ModelTier::Fast, system, &user, Some(0.1));
    let reply = match tokio::time::timeout(budget, llm.chat(req)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(err = %e, "早期轮摘要 fast 调用失败 → 回退硬截");
            return None;
        }
        Err(_) => {
            tracing::warn!("早期轮摘要 fast 调用超时 → 回退硬截");
            return None;
        }
    };
    // 摘要是给 LLM 看的外部文本：只剥控制字符（排版版权只在模板），不再人工截断模型输出。
    let cleaned = reply
        .content
        .map(|c| c.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|c| c.trim().to_string())
        .unwrap_or_default();
    if cleaned.is_empty() {
        tracing::warn!("早期轮摘要 fast 回空 → 回退硬截");
        return None;
    }
    Some(cleaned)
}

// —— D7 上下文落账：类型与进程内暂存 ——

/// 【D7】落 `meta.query_log.context_summary` 的一行：本轮实际进 prompt 的上下文摘要。
///
/// 脱敏口径：只有结构 / 尺寸 / 表名 / 注册表口径名。码值、值域专名、few-shot 问句、经验正文
/// 这些含**用户数据值**的卡只记 kind+chars（组装侧 `gather::build_context_summary` 的函数头
/// 守着这条红线，并有单测）。尺寸与预算护栏同口径：UTF-8 字节（`len()`）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ContextSummary {
    /// 预算裁剪后的实际进 prompt 总量（字节）
    pub prompt_chars: usize,
    pub cards: Vec<ContextCard>,
    /// 本轮**召回降级**的项（未触发 = 空数组）。
    ///
    /// 换掉了这里原先的 `trimmed: Vec<TrimNote>` 与 `summary_used: bool` —— 两个都是死件：
    /// `BudgetReport.notes` 恒 `vec![]`、`summary_used` 恒 `false`（历史摘要装配点在 server 侧），
    /// 于是审计面板上那两行**永远不出现**。留着死件比没有更糟：读的人以为「没裁 = 一切正常」。
    /// 现在这一格装真正会发生的事：口径卡缺席（见 `PromptCtx::degraded`）。
    pub degraded: Vec<&'static str>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ContextCard {
    pub kind: &'static str,
    /// 表名 / 注册表口径名；含数据值的卡种恒 None（不出现这个键）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub chars: usize,
}


/// gather 装配出的摘要按 `trace_id` 暂存在进程内（纯内存 —— 本 crate 门禁不许 `sqlx::query`），
/// server 的 `query_log::finish` 在同一个 fire-and-forget 任务里取走落库，主链一个 `.await` 都不多。
/// 正常路径每 stash 必随一次 finish 的 take；`STASH_CAP` 只防「装配后崩在半途」的孤儿条目。
const STASH_CAP: usize = 256;
static CONTEXT_SUMMARIES: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn context_summaries() -> &'static Mutex<HashMap<String, String>> {
    &CONTEXT_SUMMARIES
}

/// `pub(crate)`：写口只有 gather.rs 一个；读口在 server（跨 crate，故 `take` 是 pub）。
pub(crate) fn stash_context_summary(trace_id: &str, json: String) {
    if trace_id.is_empty() {
        tracing::debug!("空 trace_id，跳过摘要暂存");
        return;
    }
    let mut map = context_summaries().lock().unwrap_or_else(|p| p.into_inner());
    // 爆帽 = 前面有孤儿（finish 没跑到）。观测件宁可丢一条也不涨内存：HashMap 无序，
    // `keys().next()` 丢的是**任意一条** —— 可能踢掉刚 stash、finish 还没 take 的新条目；
    // 正常路径（stash 后必有 take）根本到不了帽，故维持「丢任意一条」不换 FIFO。
    if map.len() >= STASH_CAP && !map.contains_key(trace_id) {
        if let Some(k) = map.keys().next().cloned() {
            map.remove(&k);
        }
    }
    map.insert(trace_id.to_string(), json);
}

/// 取走即清：一行 query_log 只贴一份摘要。复合问题的并行子问共用 `trace_id`，后写覆盖先写
/// （审计粒度是「这一轮问答」，不是每个子问）。
pub fn take_context_summary(trace_id: &str) -> Option<String> {
    context_summaries().lock().unwrap_or_else(|p| p.into_inner()).remove(trace_id)
}

/// 截断三件套的渲染（**纯函数**）：命中行上限时必须带出三件事 ——
/// ① **原因**（命中 200 行上限，不是「后面没有数据了」）；② **范围**（已返回前 200 行，按当前排序）；
/// ③ **续读参数**（把 `LIMIT 200 OFFSET 200` 接在原 SQL 上）。
///
/// 在这之前是**静默截断**：用户看到 200 行却不知道后面还有，「各分类销量」被截成前 200 个分类
/// 而毫无提示（同类事故在 `kernel::nl::time.rs` 的 `detect_top_n` 注释里记过一次）。
///
/// 判据是 `row_count >= MAX_ROWS`：这会有假阳（结果恰好 200 行时多提示一句），
/// **假阳的代价是多一句提示，假阴的代价是静默少数据** —— 所以取假阳一侧。
pub fn truncation_note(sql: &str, row_count: usize) -> Option<String> {
    if row_count < MAX_ROWS {
        return None;
    }
    Some(format!(
        "已命中 {MAX_ROWS} 行上限：本次只返回前 {MAX_ROWS} 行（按当前排序），后面可能还有数据。\
         续读下一页：{}",
        next_page_sql(sql)
    ))
}

/// 续读 SQL：剥掉尾部的 `LIMIT n [OFFSET m]`（或 MySQL 的 `LIMIT m, n`）再接 `LIMIT 200 OFFSET 200`。
/// ponytail: 只认「尾部是纯 limit 子句」这一种形态 —— `check()` 保证每条执行的 SQL 都带 LIMIT，
/// 而它补的正是这一种。形态不认时原样追加：产出的是给人看的提示串，不是拿去执行的 SQL。
fn next_page_sql(sql: &str) -> String {
    let body = strip_trailing_limit(sql.trim().trim_end_matches(';').trim_end());
    format!("{body} LIMIT {MAX_ROWS} OFFSET {MAX_ROWS}")
}

fn strip_trailing_limit(sql: &str) -> &str {
    // 按字节反向找 `limit`（大小写不敏感）。**不用 `to_lowercase().rfind()`**：那是在另一个
    // 字符串里取偏移，遇到会变长的字符（如 `İ`）就会拿着错位的下标切原串 —— 非字符边界当场 panic。
    // `limit` 全是 ASCII，而 ASCII 字节不可能出现在多字节 UTF-8 序列内部，故这里的下标一定是字符边界。
    let b = sql.as_bytes();
    let Some(pos) = (0..b.len().saturating_sub(b"limit".len() - 1)).rev().find(|&i| b[i..i + 5].eq_ignore_ascii_case(b"limit"))
    else {
        return sql;
    };
    // 词边界：`limit` 前必须是空白，否则可能是 `xlimit` 这类标识符的尾巴
    if !sql[..pos].ends_with(|c: char| c.is_whitespace()) {
        return sql;
    }
    // 尾部形态：数字 | 数字,数字（MySQL `LIMIT m, n`）| 数字 OFFSET 数字 —— 成对消费：
    // 孤 `offset`（`LIMIT 200 OFFSET` 没有第二个数字）不算纯 limit 子句，不剥，原样追加。
    let tokens: Vec<&str> = sql[pos + 5..]
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    let is_num = |t: &str| t.chars().all(|c| c.is_ascii_digit());
    let tail_is_pure_limit = match tokens.as_slice() {
        [] => true, // 裸 `LIMIT`（check() 补的 limit 必带数字，这只是防御分支）
        [n] => is_num(n),
        [a, b] => is_num(a) && is_num(b),
        [n, off, m] => is_num(n) && off.eq_ignore_ascii_case("offset") && is_num(m),
        _ => false,
    };
    if tail_is_pure_limit {
        sql[..pos].trim_end()
    } else {
        sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use dms_policy::scope::ScopeSets;

    /// 造一条能执行的 SQL：只能经 `gate()`（本 crate 不许自己铸 `UnrestrictedProof`）。
    fn scoped(sql: &str) -> ScopedSql {
        let p = crate::gate::anyone();
        crate::gate::gate(&p, sql, &Scope::new(ScopeSets::default(), true), &dms_kernel::MysqlDialect).unwrap()
    }

    fn rowset(n: usize) -> RowSet {
        RowSet {
            columns: vec!["分类".into()],
            rows: (0..n).map(|i| vec![serde_json::Value::from(i)]).collect(),
            redacted: vec![], truncated: false }
    }

    /// 🔴 serde 向后兼容：`caliber_note` 缺席时**不许出现在 JSON 里**。
    /// 前端与 `tools/regression.py` / `tools/evaluation.py` 都在解析这份 JSON，
    /// 少了 `skip_serializing_if` 就是 53 题回归里所有断言 JSON 形状的题一起红。
    #[test]
    fn caliber_note_is_omitted_when_absent() {
        let mut r = AskResult {
            sql: "SELECT 1".into(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            truncated: false,
            elapsed_ms: 1,
            route: "llm".into(),
            view: dms_semantic::present::build(&[], &[]),
            reinterpret_note: None,
        resolved_question: None,
            supplemental: None,
            comparisons: vec![],
            subs: vec![],
            caliber_note: None,
            truncation_note: None,
            redacted: vec![],
            scope_note: None,
            trust: None,
            steps: vec![],
            clarify_options: vec![],
            value_labels: vec![],
            sales_context: None,
            intent_summary: None,
            kb: None,
        };
        let j = serde_json::to_value(&r).unwrap();
        assert!(j.get("caliber_note").is_none(), "{j}");
        assert!(
            j.get("subs").is_none(),
            "既有形状不许变（空 subs 同样不出现）：{j}"
        );
        assert!(
            j.get("route").is_some() && j.get("row_count").is_some(),
            "{j}"
        );
        // 本轮新增的字段同样必须缺席（老前端不改也不崩）
        assert!(j.get("truncation_note").is_none(), "{j}");
        assert!(
            j.get("redacted").is_none(),
            "空 redacted 不许出现在 JSON 里：{j}"
        );
        assert!(
            j.get("trust").is_none(),
            "未补凭证时 trust 不许占老 JSON 形状：{j}"
        );
        assert!(
            j.get("supplemental").is_none(),
            "无补充结果时不许改变老 JSON 形状：{j}"
        );
        // 呈现中文化与反问候选同理：空 = 整键不上线
        assert!(j.get("clarify_options").is_none(), "空 clarify_options 不许上线：{j}");
        assert!(j.get("value_labels").is_none(), "空 value_labels 不许上线：{j}");
        // 销售同窗补充同理：None = 整键不上线（非销售 KPI 答案形状一字不变）
        assert!(j.get("sales_context").is_none(), "无同窗补充时 sales_context 不许上线：{j}");
        // AI 重新理解的透出同理：未重理解 = 整键不上线
        assert!(j.get("reinterpret_note").is_none(), "未重理解时 reinterpret_note 不许上线：{j}");
        // 命中时形状 = {columns, rows}（单行五值，列序＝合同 CONTEXT_METRICS）
        r.sales_context = Some(SalesContextResult {
            columns: vec!["销售额".into(), "不含税成本".into(), "不含税收入".into(), "毛利额".into(), "毛利率".into()],
            rows: vec![vec![serde_json::json!("100.00"), serde_json::json!("80.00"),
                            serde_json::json!("90.00"), serde_json::json!("10.00"), serde_json::json!("0.1111")]],
        });
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["sales_context"]["columns"][1], "不含税成本", "{j}");
        assert_eq!(j["sales_context"]["rows"][0][4], serde_json::json!("0.1111"), "{j}");
        assert!(j["sales_context"].get("view").is_none() && j["sales_context"].get("row_count").is_none(),
                "同窗补充刻意比 supplemental 窄，不带 view/row_count：{j}");
        r.sales_context = None;
        // reinterpret_note 非空才上线，且是原文（与 caliber_note 同一 wire 纪律）
        r.reinterpret_note = Some("已按理解为你想问：「销售额按省份按商品」".into());
        assert_eq!(
            serde_json::to_value(&r).unwrap()["reinterpret_note"],
            "已按理解为你想问：「销售额按省份按商品」"
        );
        r.reinterpret_note = None;
        // 有标注时才出现，且是原文（前端按它显示「数字不可信」的提示）
        r.caliber_note = Some("口径复核未通过：下方结果不可信".into());
        assert_eq!(
            serde_json::to_value(&r).unwrap()["caliber_note"],
            "口径复核未通过：下方结果不可信"
        );
        // 🔴 非空时必须出现、且是**列名原文**（前端按字符串相等定位列，改成索引或大写就定位不到）
        r.redacted = vec!["login_pwd".into(), "id_card".into()];
        assert_eq!(
            serde_json::to_value(&r).unwrap()["redacted"],
            serde_json::json!(["login_pwd", "id_card"])
        );
        // steps 同理：空（need-intent 反问 / 复合容器）不出现，非空原样带出
        assert!(serde_json::to_value(&r).unwrap().get("steps").is_none());
        r.steps = vec![Step { stage: "graph", kind: "miss", ms: 3 },
                       Step { stage: "direct-agg", kind: "hit", ms: 412 }];
        assert_eq!(
            serde_json::to_value(&r).unwrap()["steps"],
            serde_json::json!([{"stage":"graph","kind":"miss","ms":3},
                               {"stage":"direct-agg","kind":"hit","ms":412}])
        );
    }

    #[test]
    fn fingerprint_normalizes_whitespace_but_not_token_boundaries() {
        assert_eq!(sql_fingerprint("SELECT   1\nFROM t"), sql_fingerprint("SELECT 1 FROM t"));
        assert_ne!(sql_fingerprint("SELECT ab c"), sql_fingerprint("SELECT a bc"));
    }

    /// 🔴 新字段的 wire 形态：非空才出现，且是 `{label, question}` / `{column, code, label}`。
    /// 两个判官脚本与老前端不认识这两个键 —— 它们必须只在真有内容时才上线。
    #[test]
    fn clarify_options_and_value_labels_on_wire_only_when_present() {
        let mut r = AskResult {
            sql: "SELECT 1".into(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            truncated: false,
            elapsed_ms: 1,
            route: "need-intent".into(),
            view: dms_semantic::present::build(&[], &[]),
            reinterpret_note: None,
        resolved_question: None,
            supplemental: None,
            comparisons: vec![],
            subs: vec![],
            caliber_note: None,
            truncation_note: None,
            redacted: vec![],
            scope_note: None,
            trust: None,
            steps: vec![],
            clarify_options: vec![ClarifyOption { label: "销售表现".into(), question: "本月销售额是多少".into() }],
            value_labels: vec![ValueLabel { column: "状态".into(), code: "100".into(), label: "待审核".into() }],
            sales_context: None,
            intent_summary: None,
            kb: None,
        };
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["clarify_options"], serde_json::json!([{"label": "销售表现", "question": "本月销售额是多少"}]));
        assert_eq!(j["value_labels"], serde_json::json!([{"column": "状态", "code": "100", "label": "待审核"}]));
        r.clarify_options = vec![];
        r.value_labels = vec![];
        let j = serde_json::to_value(&r).unwrap();
        assert!(j.get("clarify_options").is_none() && j.get("value_labels").is_none(), "{j}");
    }

    /// direct-derive 的凭证判级：恒 review（未经合同验证），绝不进 verified/high ——
    /// 凭证上「推导答案」与「合同答案」必须一眼可分。其他 route 的判级一字不变。
    #[test]
    fn derived_route_is_always_review_level() {
        assert_eq!(trust_level("direct-derive", false, false), "review");
        assert_eq!(trust_level("direct-derive", false, true), "review", "即使被误判成确定性路径也必须压回 review");
        assert_eq!(trust_level("direct-agg", false, true), "verified");
        assert_eq!(trust_level("llm", false, false), "high");
        assert_eq!(trust_level("llm", true, false), "review");
        // attach_trust 里两条推导专用文案必须还在（透出「未经合同验证」的唯一位置）
        let src = include_str!("ctx.rs");
        let body = src
            .split("fn attach_trust(")
            .nth(1)
            .expect("attach_trust 没了")
            .split("/// FNV-1a")
            .next()
            .expect("attach_trust 边界没了");
        assert!(body.contains("推导口径：合同未覆盖，由 ODS 明细推导，未经合同验证"), "{body}");
        assert!(body.contains("未经合同口径复核"), "{body}");
    }

    #[test]
    fn result_verifier_requires_actual_metric_values_after_sql_execution() {
        let intent = IntentV1 {
            mode: crate::intent::IntentMode::Data,
            metrics: vec!["销售额".into()],
            ..Default::default()
        };
        let mut result = table_answer(
            &scoped("SELECT SUM(amount) AS 销售额 FROM t_sales_order"),
            RowSet {
                columns: vec!["销售额".into()],
                rows: vec![],
                redacted: vec![], truncated: false },
            "llm",
            Instant::now(),
        );
        let issues = result_verification_issues(Some(&intent), &result);
        assert!(issues.contains(&"result:empty".to_string()), "{issues:?}");
        assert!(
            issues.contains(&"result:metric-unverified:销售额".to_string()),
            "SQL 别名不能冒充执行值：{issues:?}"
        );

        result.rows = vec![vec![serde_json::json!("203000000.00")]];
        result.row_count = 1;
        let issues = result_verification_issues(Some(&intent), &result);
        assert!(
            !issues
                .iter()
                .any(|issue| issue.starts_with("result:metric-unverified")),
            "{issues:?}"
        );
        assert!(!issues.contains(&"result:empty".to_string()), "{issues:?}");
    }

    #[test]
    fn result_verifier_binds_scalar_values_to_exact_metric_columns() {
        let result = |column: &str| {
            table_answer(
                &scoped("SELECT SUM(amount) AS metric_value FROM t_sales_order LIMIT 200"),
                RowSet {
                    columns: vec![column.into()],
                    rows: vec![vec![serde_json::json!(100)]],
                    redacted: vec![], truncated: false },
                "direct-agg",
                Instant::now(),
            )
        };
        let issues_for = |metric: &str, column: &str| {
            result_verification_issues(
                Some(&IntentV1 {
                    mode: crate::intent::IntentMode::Data,
                    metrics: vec![metric.into()],
                    ..Default::default()
                }),
                &result(column),
            )
        };

        for (metric, wrong_column) in [
            ("销售额", "discount_amount"),
            ("订单数", "account_count"),
            ("毛利率", "库存量"),
        ] {
            let issues = issues_for(metric, wrong_column);
            assert!(
                issues.contains(&format!("result:metric-unverified:{metric}")),
                "{wrong_column} 不能冒充 {metric}: {issues:?}"
            );
        }

        for (metric, column) in [
            ("销售额", "amount"),
            ("销售额", "销售额（万元）"),
            ("订单数", "order_count"),
            ("库存量", "in_stock_quantity"),
            ("毛利率", "gross_margin"),
        ] {
            let issues = issues_for(metric, column);
            assert!(
                !issues
                    .iter()
                    .any(|issue| issue.starts_with("result:metric-unverified")),
                "合法标量列 {column} 应验证 {metric}: {issues:?}"
            );
        }
    }

    #[test]
    fn result_verifier_blocks_incomplete_or_inconsistent_comparisons() {
        let intent = IntentV1 {
            mode: crate::intent::IntentMode::Data,
            metrics: vec!["销售额".into()],
            comparisons: vec!["同比".into()],
            ..Default::default()
        };
        let mut result = table_answer(
            &scoped("SELECT SUM(amount) AS 销售额 FROM t_sales_order"),
            RowSet {
                columns: vec!["销售额".into()],
                rows: vec![vec![serde_json::json!(100.0)]],
                redacted: vec![], truncated: false },
            "direct-agg",
            Instant::now(),
        );
        let issues = result_verification_issues(Some(&intent), &result);
        assert!(
            issues.contains(&"result:comparison-incomplete".to_string()),
            "{issues:?}"
        );

        result.comparisons.push(KpiComparison {
            label: "同比".into(),
            current: 99.0,
            baseline: 80.0,
            change: 19.0,
            pct: 23.8,
            dir: "up",
        });
        let issues = result_verification_issues(Some(&intent), &result);
        assert!(
            issues.contains(&"result:comparison-current-mismatch".to_string()),
            "{issues:?}"
        );
        assert!(
            !issues.contains(&"result:comparison-incomplete".to_string()),
            "{issues:?}"
        );
    }

    #[test]
    fn result_verifier_does_not_treat_scalar_as_requested_detail() {
        let intent = IntentV1 {
            mode: crate::intent::IntentMode::Data,
            metrics: vec!["销售额".into()],
            requested_detail: true,
            ..Default::default()
        };
        let mut result = table_answer(
            &scoped("SELECT SUM(amount) AS 销售额 FROM t_sales_order"),
            RowSet {
                columns: vec!["销售额".into()],
                rows: vec![vec![serde_json::json!(100)]],
                redacted: vec![], truncated: false },
            "direct-agg",
            Instant::now(),
        );
        assert!(
            result_verification_issues(Some(&intent), &result)
                .contains(&"result:detail-empty".to_string()),
            "主 KPI 不能冒充逐笔明细"
        );
        result.supplemental = Some(SupplementalResult {
            columns: vec!["单号".into(), "金额".into()],
            rows: vec![vec![serde_json::json!("SO-1"), serde_json::json!(100)]],
            row_count: 1,
            truncated: false,
            view: dms_semantic::present::build(
                &["单号".into(), "金额".into()],
                &[vec![serde_json::json!("SO-1"), serde_json::json!(100)]],
            ),
        });
        assert!(
            !result_verification_issues(Some(&intent), &result)
                .contains(&"result:detail-empty".to_string())
        );
    }

    #[test]
    fn result_verifier_rejects_non_finite_numeric_strings_and_comparisons() {
        for value in ["NaN", "inf", "-Infinity"] {
            assert!(!cell_is_number(&serde_json::json!(value)), "{value}");
        }
        let intent = IntentV1 {
            mode: crate::intent::IntentMode::Data,
            metrics: vec!["销售额".into()],
            comparisons: vec!["同比".into()],
            ..Default::default()
        };
        let mut result = table_answer(
            &scoped("SELECT SUM(amount) AS 销售额 FROM t_sales_order"),
            RowSet {
                columns: vec!["销售额".into()],
                rows: vec![vec![serde_json::json!(100)]],
                redacted: vec![], truncated: false },
            "direct-agg",
            Instant::now(),
        );
        result.comparisons.push(KpiComparison {
            label: "同比".into(),
            current: 100.0,
            baseline: 80.0,
            change: f64::NAN,
            pct: 25.0,
            dir: "up",
        });
        let issues = result_verification_issues(Some(&intent), &result);
        assert!(issues.contains(&"result:comparison-invalid".to_string()), "{issues:?}");
        assert!(issues.contains(&"result:comparison-incomplete".to_string()), "{issues:?}");

        result.comparisons[0] = KpiComparison {
            label: "同比".into(),
            current: 100.0,
            baseline: 80.0,
            change: 20.0,
            pct: 999.0,
            dir: "up",
        };
        let issues = result_verification_issues(Some(&intent), &result);
        assert!(issues.contains(&"result:comparison-invalid".to_string()), "{issues:?}");
        assert!(issues.contains(&"result:comparison-incomplete".to_string()), "{issues:?}");
    }

    #[test]
    fn result_verifier_rejects_wire_shape_drift() {
        let mut result = table_answer(
            &scoped("SELECT goods_type, SUM(amount) FROM t_sales_order GROUP BY goods_type"),
            RowSet {
                columns: vec!["品类".into(), "销售额".into()],
                rows: vec![vec![serde_json::json!("烤肠"), serde_json::json!(100)]],
                redacted: vec![], truncated: false },
            "llm",
            Instant::now(),
        );
        result.rows[0].pop();
        result.row_count = 2;
        let issues = result_verification_issues(None, &result);
        assert!(
            issues.contains(&"result:row-count-mismatch".to_string()),
            "{issues:?}"
        );
        assert!(
            issues.contains(&"result:column-shape-mismatch".to_string()),
            "{issues:?}"
        );
    }

    /// 🔴 `table_answer` 必须把 `RowSet.redacted` **原样带出去**。
    /// 这个字段曾长期无消费者（裁决 二·D T4-5），于是被脱敏的列在界面上只是一片空值 ——
    /// 用户把「系统正确地拒绝了敏感列」当成「系统查不出来」。
    /// 断言打在「值真的从 RowSet 流到 AskResult」上，不是打在字段声明上：
    /// 声明加了而 `let RowSet { .. }` 里仍旧丢掉，就是一段永远不显示的前端分支冒充已修。
    #[test]
    fn table_answer_carries_redacted_columns_through() {
        // 用既有的 `scoped()` 助手（本 crate 不许自己铸 UnrestrictedProof）。
        // SQL 里不点名敏感列 —— `is_safe_select` 会拒；脱敏是**执行侧整列置空**后回报的，
        // 所以这里模拟的是「查了 t_employee 的普通列、connector 侧把某列判成敏感并置空」。
        let s = scoped("SELECT login_name FROM t_employee LIMIT 1");
        let rs = RowSet {
            columns: vec!["login_name".into()],
            rows: vec![vec![serde_json::Value::Null]],
            redacted: vec!["login_name".into()], truncated: false };
        let r = table_answer(&s, rs, "llm", Instant::now());
        assert_eq!(
            r.redacted,
            vec!["login_name".to_string()],
            "RowSet.redacted 又被 `..` 丢掉了"
        );
        assert_eq!(
            r.redacted[0], r.columns[0],
            "必须与 columns 逐字相同（前端按名定位列）"
        );
        // 这条 SQL 走的是 `ScopeSets::default()` + admin（`scoped()` 助手），即**不限制** →
        // 不许出现权限回显（否则超管每次都被告知「你看到的不是全量」）
        assert!(
            r.scope_note.is_none(),
            "无限制却报了权限回显：{:?}",
            r.scope_note
        );
    }

    /// 🔴 行级权限**生效时必须回显**。
    ///
    /// 受限用户看到的是子集，而界面上此前没有一个字说明「这不是全量」——
    /// 他会拿着被过滤的数下结论（「我们本月只有 12 个客户？」），而这件事不报错、
    /// 也不被任何判据抓到。值取自 `ScopedSql::is_unrestricted()`，那个方法在此之前
    /// 是**零生产调用方的死代码** —— bit 早就算好了，只是没人取。
    ///
    /// 断言打在「值真的从 `ScopedSql` 流到 `AskResult`」上（两个方向都验），
    /// 不是打在字段声明上：只声明字段而 `table_answer` 不填，就是一段永远不显示的前端分支。
    #[test]
    fn scope_note_appears_only_when_row_permission_was_injected() {
        use dms_policy::scope::ScopeSets;
        let p = crate::gate::anyone();
        // 受限：非空 customer_codes ⇒ `is_unrestricted()` 为假 ⇒ 注入生效
        let sets = ScopeSets {
            customer_codes: ["C1".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let limited = crate::gate::gate(
            &p,
            "SELECT customer_code FROM t_sales_order",
            &Scope::new(sets, false),
            &dms_kernel::MysqlDialect,
        )
        .unwrap();
        assert!(
            !limited.is_unrestricted(),
            "这条应当是受限的，测试前提就不成立了"
        );
        let r = table_answer(&limited, rowset(3), "llm", Instant::now());
        let note = r.scope_note.as_deref().expect("行权限生效却没有回显");
        assert!(note.contains("数据权限"), "{note}");
        assert!(
            note.contains("不是全量"),
            "必须说清「这不是全量」，否则等于没说：{note}"
        );
        // JSON 形状：有值才出现（老前端与两个 runner 不受影响）
        let j = serde_json::to_value(&r).unwrap();
        assert!(j.get("scope_note").is_some(), "{j}");
        let r2 = table_answer(
            &scoped("SELECT 1 FROM t_sales_order"),
            rowset(3),
            "llm",
            Instant::now(),
        );
        assert!(
            serde_json::to_value(&r2)
                .unwrap()
                .get("scope_note")
                .is_none(),
            "无限制时不许出现这个键"
        );
    }

    /// 截断三件套：**未截断一句都不说，截断必须说全三件**。
    /// 少了它就是回到静默截断——用户拿前 200 行当全量做决策，而这件事不会报错。
    #[test]
    fn truncation_note_renders_reason_scope_and_next_page() {
        // ① 未截断（含 0 行）→ 没有提示
        assert!(truncation_note("SELECT a FROM t LIMIT 200", MAX_ROWS - 1).is_none());
        assert!(truncation_note("SELECT a FROM t LIMIT 200", 0).is_none());
        // 超过上限（connector 某天返回 >MAX_ROWS）：truncated 与 note 判据同为 >=，不许互相矛盾
        assert!(truncation_note("SELECT a FROM t LIMIT 200", MAX_ROWS + 1).is_some());
        // ② 命中上限 → 原因 + 范围 + 续读 SQL（尾部的 LIMIT 200 被换成 LIMIT 200 OFFSET 200）
        let n = truncation_note("SELECT a FROM t ORDER BY a DESC LIMIT 200", MAX_ROWS).unwrap();
        assert!(n.contains("命中 200 行上限"), "缺原因：{n}");
        assert!(n.contains("只返回前 200 行（按当前排序）"), "缺范围：{n}");
        assert!(
            n.contains("SELECT a FROM t ORDER BY a DESC LIMIT 200 OFFSET 200"),
            "缺续读参数：{n}"
        );
        // ③ MySQL 的 `LIMIT m, n` 与已有 OFFSET 同样只保留一份 limit 子句
        assert!(next_page_sql("SELECT a FROM t LIMIT 0, 200").ends_with("t LIMIT 200 OFFSET 200"));
        assert!(
            next_page_sql("SELECT a FROM t LIMIT 200 OFFSET 0").ends_with("t LIMIT 200 OFFSET 200")
        );
        // 孤 OFFSET（没有第二个数字）不是纯 limit 子句：不剥，原样追加
        assert!(next_page_sql("SELECT a FROM t LIMIT 200 OFFSET")
            .contains("OFFSET LIMIT 200 OFFSET 200"));
        // ④ 尾部不是纯 limit 子句就不乱剥（提示串宁可多一段，也不能把 SQL 切坏）
        assert_eq!(
            next_page_sql("SELECT a FROM t WHERE b = 'limit 3'"),
            "SELECT a FROM t WHERE b = 'limit 3' LIMIT 200 OFFSET 200"
        );
    }

    /// `table_answer` 的形状：`route` 取调用方给的值、`view` 恒 Some（非 Option）、
    /// 截断字段随行数自动带出。
    #[test]
    fn table_answer_shape_and_truncation() {
        let s = scoped("SELECT goods_type FROM t_sales_order");
        let r = table_answer(&s, rowset(MAX_ROWS), "direct-agg", Instant::now());
        assert_eq!(
            r.route, "direct-agg",
            "route 必须取命中方给的值，不是表标签"
        );
        assert_eq!((r.row_count, r.truncated), (MAX_ROWS, true));
        assert!(r.sql.ends_with("LIMIT 200"), "{}", r.sql);
        assert!(r.truncation_note.as_deref().unwrap().contains("OFFSET 200"));
        assert!(serde_json::to_value(&r)
            .unwrap()
            .get("truncation_note")
            .is_some());
        // 未满一页：既不算截断，也不带提示
        let r2 = table_answer(&s, rowset(3), "semantic-cache", Instant::now());
        assert_eq!((r2.row_count, r2.truncated), (3, false));
        assert!(r2.truncation_note.is_none());
        assert!(serde_json::to_value(&r2)
            .unwrap()
            .get("truncation_note")
            .is_none());
    }

    /// `semantic::present::ROW_CAP` 复刻本常量（agent→semantic 单向依赖只能复刻），两侧不许漂。
    #[test]
    fn row_cap_matches_agent_max_rows() {
        assert_eq!(dms_semantic::present::ROW_CAP, MAX_ROWS);
    }
}

#[cfg(test)]
mod y10_tests {
    //! Y10 两级摘要与 D7 暂存的判据：触发/拆分/缓存键/外置/渲染降级/fast 调用全分支。
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use dms_kernel::{BoxFut, ChatReply, LlmError};

    fn turn(q: &str, sql: Option<&str>, rows: Option<usize>) -> Turn {
        Turn { question: q.into(), sql: sql.map(Into::into), row_count: rows }
    }

    fn turns(n: usize) -> Vec<Turn> {
        (1..=n)
            .map(|i| turn(&format!("第{i}轮问句"), Some(&format!("SELECT {i}")), Some(i)))
            .collect()
    }

    /// 触发与拆分：≤ N 全量原文（early 恒空）；> N 时 early = 去掉最近 N 轮，
    /// 且**最早的一轮在 early 头部**（它最该被压成摘要）。
    #[test]
    fn summary_triggers_only_beyond_keep_recent() {
        for n in 0..=SUMMARY_KEEP_RECENT {
            assert!(!needs_summary(n), "{n} 轮不该触发摘要");
            let ts = turns(n);
            let (early, recent) = split_turns(&ts);
            assert!(early.is_empty(), "{n} 轮不该有 early");
            assert_eq!(recent.len(), n);
        }
        assert!(needs_summary(SUMMARY_KEEP_RECENT + 1));
        let ts = turns(SUMMARY_KEEP_RECENT + 3);
        let (early, recent) = split_turns(&ts);
        assert_eq!(early.len(), 3);
        assert_eq!(recent.len(), SUMMARY_KEEP_RECENT);
        assert_eq!(early[0].question, "第1轮问句", "最早的一轮必须进 early");
        assert_eq!(recent[0].question, "第4轮问句", "recent 从第 4 轮开始");
        assert_eq!(recent.last().unwrap().question, format!("第{}轮问句", SUMMARY_KEEP_RECENT + 3));
    }

    /// 🔴 缓存键失效判据：新轮进入被摘要段 → 键必须变（「摘要漏了新消息」就是这条红了没被抓）。
    #[test]
    fn cache_key_invalidates_when_a_new_turn_enters_the_summarized_prefix() {
        let ts = turns(8);
        let (early7, _) = split_turns(&ts[..7]); // 7 轮时 early = 第1轮
        let (early8, _) = split_turns(&ts); //      8 轮时 early = 第1..2轮
        assert_ne!(
            summary_cache_key("c1", early7),
            summary_cache_key("c1", early8),
            "新轮进入 early 前缀后键没变 —— 缓存会漏掉这轮新消息"
        );
        // 同一段相同内容 → 同一个键（缓存命中的前提）
        assert_eq!(summary_cache_key("c1", early8), summary_cache_key("c1", &ts[..2]));
        // 会话不同 → 键不同（摘要绝不许跨会话串）
        assert_ne!(summary_cache_key("c1", early8), summary_cache_key("c2", early8));
        assert!(summary_cache_key("c1", early8).starts_with("ctxsum:c1:"));
        // 拼接歧义：("ab","c") 与 ("a","bc") 必须不同键（长度前缀防的就是这个）
        let a = [turn("ab", Some("c"), None)];
        let b = [turn("a", Some("bc"), None)];
        assert_ne!(summary_cache_key("c1", &a), summary_cache_key("c1", &b));
        // 行数 None ≠ Some(0)（「那轮没出数」≠「那轮出了 0 行」）；SQL 改动也改键
        assert_ne!(
            summary_cache_key("c1", &[turn("q", None, None)]),
            summary_cache_key("c1", &[turn("q", None, Some(0))])
        );
        assert_ne!(
            summary_cache_key("c1", &[turn("q", Some("SELECT 1"), Some(1))]),
            summary_cache_key("c1", &[turn("q", Some("SELECT 2"), Some(1))])
        );
    }

    /// 外置 SQL：短的原样、K 整好不外置、长的指针形态（共 N 字符 + 前 300 + 压成单行）；
    /// 中文按**字符**截不 panic（按字节截会切出半个字 —— 本仓踩过三次的那个坑）。
    #[test]
    fn externalize_sql_keeps_short_and_points_long() {
        let short = "SELECT 1";
        assert_eq!(externalize_sql(short), short);
        let boundary = "x".repeat(SQL_EXTERNAL_CHARS);
        assert_eq!(externalize_sql(&boundary), boundary, "K 整好不外置");
        let long = format!("SELECT {}\nFROM t", "汉".repeat(SQL_EXTERNAL_CHARS));
        let out = externalize_sql(&long);
        assert!(
            out.starts_with(&format!(
                "[SQL 共 {} 字符，仅前 {EXTERNAL_SQL_HEAD_CHARS} 字符进上下文：",
                long.chars().count()
            )),
            "{out}"
        );
        assert!(!out.contains('\n'), "指针形态必须压成单行：{out}");
        assert!(out.chars().count() < long.chars().count());
        let cn_head = "汉".repeat(SQL_EXTERNAL_CHARS + 10);
        let out2 = externalize_sql(&cn_head);
        assert!(out2.contains(&"汉".repeat(EXTERNAL_SQL_HEAD_CHARS)), "{out2}");
        assert!(!out2.contains(&"汉".repeat(EXTERNAL_SQL_HEAD_CHARS + 1)), "头部超截了");
    }

    /// 外置表格：≤ M 行全量渲染；> M 行指针形态带前 5 行，第 6 行的值绝不出现；
    /// 长单元格按字符截；NULL/数字渲染形态钉住。
    #[test]
    fn externalize_table_renders_full_below_cap_and_points_above() {
        let cols = vec!["分类".to_string(), "销量".to_string()];
        let mk = |n: usize| {
            (0..n)
                .map(|i| vec![serde_json::Value::from(format!("分类{i}")), serde_json::Value::from(i)])
                .collect::<Vec<_>>()
        };
        let small = externalize_table(&cols, &mk(3));
        assert!(small.contains("分类 | 销量") && small.contains("分类2 | 2"), "{small}");
        assert!(!small.contains("…其余"), "小表不许出指针形态");
        let big_rows = mk(TABLE_EXTERNAL_ROWS + 10);
        let out = externalize_table(&cols, &big_rows);
        assert!(
            out.starts_with(&format!("[表格 {} 行，前 {EXTERNAL_HEAD_ROWS} 行：", big_rows.len())),
            "{out}"
        );
        assert!(out.contains("分类4 | 4"), "前 5 行必须带头：{out}");
        assert!(!out.contains("分类5"), "第 6 行的值绝不许进上下文：{out}");
        assert!(
            out.contains(&format!("…其余 {} 行在结果区", big_rows.len() - EXTERNAL_HEAD_ROWS)),
            "{out}"
        );
        let wide = vec![vec![serde_json::Value::from("长".repeat(EXTERNAL_CELL_CHARS + 20))]];
        let out2 = externalize_table(&cols, &wide);
        assert!(
            out2.contains(&"长".repeat(EXTERNAL_CELL_CHARS))
                && !out2.contains(&"长".repeat(EXTERNAL_CELL_CHARS + 1)),
            "{out2}"
        );
        let nulls = vec![vec![serde_json::Value::Null, serde_json::Value::from(1.5)]];
        assert!(externalize_table(&cols, &nulls).contains("NULL | 1.5"));
    }

    /// 渲染：有摘要 → 摘要段 + 最近原文逐字；无摘要（降级）→ 硬截标记 + 最近原文；
    /// early=0 零标记；长 SQL 在渲染层也被外置。
    #[test]
    fn render_history_block_summary_fallback_and_zero_marker() {
        let ts = turns(9);
        let (early, recent) = split_turns(&ts);
        let with = render_history_block(Some("用户先问了销量，再问了分类排行。"), early.len(), recent);
        assert!(with.contains("## 更早的 3 轮对话（摘要）\n用户先问了销量，再问了分类排行。"), "{with}");
        assert!(with.contains("## 最近对话（原文）"), "{with}");
        assert!(
            with.contains(&format!("问：第{}轮问句", SUMMARY_KEEP_RECENT + 3)),
            "最近一轮原文必须逐字在：{with}"
        );
        // 轮号从 early_count + 1 起：「最近对话」的第 1 轮是全历史的第 4 轮，不许从 1 编
        assert!(with.contains("4. 问：第4轮问句"), "{with}");
        assert!(!with.contains("1. 问："), "{with}");
        // 降级 = 原硬截：早期轮一个不进上下文，但留一行明说
        // （静默省略会让模型把「没有更早轮次」当事实）
        let without = render_history_block(None, early.len(), recent);
        assert!(without.contains("（更早的 3 轮对话已省略）"), "{without}");
        assert!(!without.contains("第1轮问句"), "硬截语义：早期轮不进上下文");
        assert!(without.contains(&format!("问：第{}轮问句", SUMMARY_KEEP_RECENT + 3)));
        // 未触发（early=0）：一个标记都不出
        let plain = render_history_block(None, 0, &ts[..2]);
        assert!(!plain.contains("省略") && !plain.contains("## "), "{plain}");
        assert!(plain.contains("问：第1轮问句"));
        // 长 SQL 在渲染层也被外置
        let long_sql_turn = vec![turn("追问", Some(&"x".repeat(SQL_EXTERNAL_CHARS + 1)), None)];
        assert!(render_history_block(None, 0, &long_sql_turn).contains("[SQL 共"));
    }

    /// 假模型：`reply` 是 fast 回复（None = 调用即失败），`delay` 模拟慢调用，`calls` 计数。
    struct FakeLlm {
        reply: Option<String>,
        delay: Duration,
        calls: AtomicUsize,
    }

    impl ChatModel for FakeLlm {
        fn chat<'a>(&'a self, _req: ChatRequest) -> BoxFut<'a, Result<ChatReply, LlmError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (r, d) = (self.reply.clone(), self.delay);
            Box::pin(async move {
                tokio::time::sleep(d).await;
                match r {
                    Some(content) => Ok(ChatReply { content: Some(content), usage: Default::default() }),
                    None => Err(LlmError::Transport("模型挂了".into())),
                }
            })
        }
    }

    fn fake(reply: Option<String>, delay: Duration) -> FakeLlm {
        FakeLlm { reply, delay, calls: AtomicUsize::new(0) }
    }

    /// fast 摘要的全分支：成功 → Some（剥控制字符但不截断）；失败/超时/回空 → None（回退硬截）；
    /// 空 early 一次 LLM 都不调。
    #[tokio::test]
    async fn summarize_falls_back_on_failure_timeout_and_empty() {
        let early = turns(3);
        // 成功：控制字符（含换行）剥掉、内容保留
        let ok = fake(Some("用户先问销量。\n再问排行。".into()), Duration::ZERO);
        let got = summarize_with_timeout(&ok, &early, Duration::from_millis(500)).await;
        assert_eq!(got.as_deref(), Some("用户先问销量。再问排行。"), "控制字符必须剥掉：{got:?}");
        assert_eq!(ok.calls.load(Ordering::SeqCst), 1, "fast 只许一发");
        // 失败 → None
        let err = fake(None, Duration::ZERO);
        assert!(summarize_with_timeout(&err, &early, Duration::from_millis(500)).await.is_none());
        // 超时 → None（小预算戳超时路，不等 5s 常量）
        let slow = fake(Some("太晚".into()), Duration::from_millis(200));
        assert!(summarize_with_timeout(&slow, &early, Duration::from_millis(20)).await.is_none());
        // 回空 → None
        let empty = fake(Some("   ".into()), Duration::ZERO);
        assert!(summarize_with_timeout(&empty, &early, Duration::from_millis(500)).await.is_none());
        let huge = "摘".repeat(1200);
        let full = summarize_with_timeout(
            &fake(Some(huge.clone()), Duration::ZERO),
            &early,
            Duration::from_millis(500),
        )
        .await
        .unwrap();
        assert_eq!(full, huge, "摘要输出不应被应用层截断");
        // 空 early：一次 LLM 都不调
        let never = fake(Some("不该出现".into()), Duration::ZERO);
        assert!(summarize_with_timeout(&never, &[], Duration::from_millis(500)).await.is_none());
        assert_eq!(never.calls.load(Ordering::SeqCst), 0, "空 early 不许调 LLM");
    }

    /// D7 暂存：stash → take 取走即清；同键覆盖（复合子问共用 trace_id）；
    /// 空 trace_id 不收；爆帽踢旧条目而不是涨内存或拒收。
    /// 全部断言收在**一个** test 里：暂存是进程内全局，拆多条并行跑会互相踢键。
    #[test]
    fn context_summary_stash_take_overwrite_and_cap() {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let uniq = |tag: &str| format!("y10-test-{tag}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst));
        let k = uniq("roundtrip");
        assert_eq!(take_context_summary(&k), None, "没 stash 过不该有东西");
        stash_context_summary(&k, "{\"v\":1}".into());
        assert_eq!(take_context_summary(&k).as_deref(), Some("{\"v\":1}"));
        assert_eq!(take_context_summary(&k), None, "take 必须取走即清（一行只贴一份）");
        stash_context_summary(&k, "old".into());
        stash_context_summary(&k, "new".into());
        assert_eq!(take_context_summary(&k).as_deref(), Some("new"), "同键后写覆盖先写");
        stash_context_summary("", "x".into());
        assert_eq!(take_context_summary(""), None, "空 trace_id 不许收");
        // 灌到帽之外：条目数不许超 STASH_CAP，且最新一批必须还在（踢旧，不是拒新）
        let keys: Vec<String> = (0..STASH_CAP + 40).map(|i| uniq(&format!("cap{i}"))).collect();
        for k in &keys {
            stash_context_summary(k, "x".into());
        }
        let len = context_summaries().lock().unwrap().len();
        assert!(len <= STASH_CAP, "爆帽没踢人：{len}");
        let last = keys.last().unwrap();
        assert_eq!(take_context_summary(last).as_deref(), Some("x"), "爆帽把新条目也拒了");
        for k in &keys {
            let _ = take_context_summary(k); // 清残渣，别污染同进程后续判据
        }
    }

    /// D7 落账 JSON 的契约形状：{prompt_chars, cards, degraded}。
    /// （2026-08-14：`trimmed` 与 `summary_used` 两个恒空/恒 false 的死件换成了真会发生的
    /// `degraded` —— 死件比没有更糟，读的人以为「没裁 = 一切正常」。）
    #[test]
    fn context_summary_json_shape_is_stable() {
        let cs = ContextSummary {
            prompt_chars: 42,
            cards: vec![ContextCard { kind: "metric", name: Some("销售额".into()), chars: 10 }],
            degraded: vec!["指标召回失败 → 指标卡缺席"],
        };
        let j = serde_json::to_value(&cs).unwrap();
        assert_eq!(j["prompt_chars"], 42);
        assert_eq!(j["cards"][0]["kind"], "metric");
        assert_eq!(j["degraded"][0], "指标召回失败 → 指标卡缺席");
        assert!(j.get("trimmed").is_none() && j.get("summary_used").is_none(), "死件不许回来");
    }
}
