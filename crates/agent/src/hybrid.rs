//! 混合问句（一句话里既有问数又有知识库）的**唯一编排点**。
//!
//! 为什么在 agent：此前整套住在 `server/src/main.rs`（`hybrid_payload`），而 `ask_prepared`
//! 开头一句「route 不是 Data 就返回澄清卡」把 Hybrid 挡在门外 —— 于是
//! **CLI/判官链路与 HTTP 服务链路对同一个合同行为相反**：判官问混合问句永远得到澄清卡，
//! 回归题集结构上覆盖不到这条路，而线上走的是另一套代码。第二套编排器是 ARCHITECTURE
//! 明令禁止的（「禁止再造平行编排器」），这里把它收回来。
//!
//! 边界：**编排在这里，协议在 server**。本模块返回类型化的 [`HybridOutcome`]，
//! wire 形状（`v["kb"]` / `view.insight` 那几个键）仍由 server 塑 —— 那是协议层的事，
//! 且三端（Web / 小程序 / MCP）的应答包各不相同。
//!
//! 失败语义与 `compound` 同族：**一路挂了不拖死另一路**，退化成单路答案并留 warn；
//! 两路都挂才整体失败。

use dms_kernel::Answer;
use dms_policy::principal::Principal;

use crate::ask::{ask_data_arm, AskDeps, PreparedQuestion};
use crate::ctx::AskResult;
use crate::intent::{IntentRoute, RoutedQuestion};

/// 知识库那一路的依赖。`None` = 调用方不提供 KB（深度报告子问、定时任务等），
/// 此时 Hybrid 合同只执行问数半并在收据里留缺口，而不是整轮澄清 ——
/// 「拿不到 KB」是调用方的能力边界，不是用户问错了。
pub struct KbArm<'a> {
    pub owned: &'a dms_connector::owned::OwnedStore,
    pub weights: &'a dms_knowledge::retrieve::RrfWeights,
    /// 显式知识空间；`None` = 不限空间（小程序恒 None）
    pub space: Option<&'a str>,
}

/// 两路并行的类型化产物。三个字段各自可缺席，调用方据此决定 wire 形状。
pub struct HybridOutcome {
    pub data: Option<AskResult>,
    pub knowledge: Option<Answer>,
    /// AI 综合（fast 档）。两路都在才生成；失败降级为 `None`，不拖垮主结果。
    pub summary: Option<String>,
    /// 归属不唯一等无法执行的原因；非空时 `data`/`knowledge` 都为 `None`。
    pub clarification: Option<AskResult>,
    /// 问数臂**答不了的原因**（它出了卡但没实质内容时）。资料半单独上屏时把这句带上，
    /// 否则用户会以为数据侧没意见 —— 而他问的本来就是数据。
    pub data_note: Option<String>,
}

/// typed subgoal → (问数半 N 条, 知识库半 1 条)。
///
/// 此前这里要求**恰好两条**（一数一知），于是「本月销售额和毛利各多少？另外退货政策怎么规定」
/// 这种再普通不过的问法直接吃澄清卡 —— 用户看到的就是业主截图里那张「先问清再查」。
/// 问数半多条并不需要新载体：复合问句本来就走 `AskResult::compound(subs)`，wire 与前端零改动。
///
/// **知识库半仍限 1 条**，这是载体上限不是懒：`Answer` 的角标 = `citations` 下标 + 1，
/// 合并两份答案得整体重编号，编错就是「点开引用跳到别的原文」——比澄清卡更伤。
/// 触发条件写进澄清文案（下面 `cardinality_note`），用户拆一次就能问到。
///
/// 任何 `Unknown` 子任务照旧一票否决：归属都没证明，谈不上并行执行。
pub fn split<'a>(routed: &'a [RoutedQuestion]) -> Option<(Vec<&'a RoutedQuestion>, &'a RoutedQuestion)> {
    if routed.iter().any(|item| item.route == IntentRoute::Unknown) {
        return None;
    }
    let data: Vec<&RoutedQuestion> =
        routed.iter().filter(|item| item.route == IntentRoute::Data).collect();
    let mut kb = routed.iter().filter(|item| item.route == IntentRoute::Knowledge);
    let knowledge = kb.next()?;
    if data.is_empty() || kb.next().is_some() {
        return None;
    }
    Some((data, knowledge))
}

/// 澄清卡上那句话：**说清是几条、为什么执行不了**，而不是笼统「请说得更具体」。
/// 用户据此知道该拆成几次问，而不是换个说法再撞一次同一堵墙。
pub fn cardinality_note(routed: &[RoutedQuestion]) -> String {
    let count = |r: IntentRoute| routed.iter().filter(|item| item.route == r).count();
    let (data, knowledge) = (count(IntentRoute::Data), count(IntentRoute::Knowledge));
    let unknown = routed.len().saturating_sub(data + knowledge);
    if unknown > 0 {
        return format!("我识别到 {unknown} 个子任务归属仍不明确（数据 {data} 个、资料 {knowledge} 个），先确认这几个再查。");
    }
    if knowledge > 1 {
        return format!("我识别到 {knowledge} 个资料子任务；一次混合回答只能带 1 个资料问题（引用角标要保证点得开），请把资料部分拆成 {knowledge} 次问。");
    }
    format!("我识别到数据 {data} 个、资料 {knowledge} 个子任务，这个组合无法一次可靠回答，请拆开问。")
}

/// 执行一份 Hybrid 合同。`d.kb` 缺席时只跑问数半（见 [`KbArm`] 的文档）。
pub async fn run(
    d: &AskDeps<'_>,
    p: &Principal,
    prepared: &PreparedQuestion,
    explicit_ds: Option<&str>,
) -> anyhow::Result<HybridOutcome> {
    let routed = prepared.routed_questions();
    let Some((data_qs, kb_q)) = split(&routed) else {
        let mut card = prepared.clarification_result();
        card.caliber_note = Some(cardinality_note(&routed));
        return Ok(HybridOutcome {
            data: None,
            knowledge: None,
            summary: None,
            clarification: Some(card),
            data_note: None,
        });
    };
    let data_prepared: Vec<_> = data_qs.iter().map(|q| prepared.project(q)).collect();
    //  → 本函数 →  是递归 async，必须装箱（编译器要求）。
    // 递归只有一层：投影后的子问 route 恒为 Data，不会再进 Hybrid 分支。
    // 多条问数子问**彼此也并行**：它们各自打一次库，串行等于白等（与 compound 同一条）。
    let data_fut = async {
        let mut out = Vec::with_capacity(data_prepared.len());
        // 走**问数臂**而不是 `ask_prepared`：后者现在自己会并行点一次知识库，
        // typed 子问再各点一次就是 N+1 次检索 + N+1 次生成（合同已经把资料半单独拆出来了）。
        for r in futures::future::join_all(
            data_prepared.iter().map(|q| Box::pin(ask_data_arm(d, p, q, explicit_ds, false))),
        )
        .await
        {
            out.push(r?);
        }
        anyhow::Ok(out)
    };
    let kb_question = kb_q.question.clone();
    let kb_fut = async {
        let Some(kb) = d.kb.as_ref() else { return None };
        match crate::answerers::knowledge::answer(
            kb.owned,
            d.embed,
            &**d.llm,
            p,
            kb.space,
            &kb_question,
            kb.weights,
        )
        .await
        {
            Ok(a) => Some(a),
            Err(e) => {
                // 与 compound 子问同一条纪律：单路失败留痕但不拖垮整轮
                tracing::warn!(err = %e, question = %kb_question, "混合查询知识库路失败 → 退化纯问数");
                None
            }
        }
    };
    // 两路**并行**：总耗时 = 两路较大者，不相加（公网 Doris + 向量检索各自都是秒级）
    let (data_r, knowledge) = tokio::join!(data_fut, kb_fut);
    let data = match data_r {
        Ok(rs) => fold_data(rs, prepared, &data_qs),
        Err(e) => {
            if knowledge.is_none() {
                // 两路皆挂：原样上抛（fail-closed，不伪造半个答案）
                return Err(e);
            }
            tracing::warn!(err = %e, "混合查询问数路失败 → 退化纯知识库");
            None
        }
    };
    let summary = match (&data, &knowledge) {
        (Some(r), Some(a)) => {
            crate::compound::hybrid_summary(&**d.llm, &prepared.effective_question, r, a).await
        }
        _ => None,
    };
    Ok(HybridOutcome { data, knowledge, summary, clarification: None, data_note: None })
}


/// ★ **两臂并行**：一句问句同时走问数与知识库，两边都有实质内容时合成一份答案。
///
/// ## 它替换了什么
///
/// 此前 `ask_prepared` 顶上是一段五选一的分派：判 `Knowledge` 就**只**问知识库，
/// 判 `Data` 就**只**问数。分类器错一次，用户整轮拿不到本来存在的答案 ——
/// 业主 2026-08-14 实测「线下-浏阳品元商贸有限公司」被判资料问句，知识库如实答
/// 「知识库里没有这家公司的规定」，而这家公司在业务库里有客户卡，一个字没提。
///
/// ## 合成纪律（保守，不改既有形状）
///
/// | 两臂状态 | 产物 |
/// |---|---|
/// | 都有实质 | `compound` 容器（问数半 + 资料半）+ AI 综合落 `view.insight` |
/// | 只有问数 | **问数结果原样返回** —— 与改造前逐字节相同 |
/// | 只有资料 | 资料结果（route = `knowledge`） |
/// | 都没有 | 问数臂那张卡（它至少解释了为什么答不了）；问数臂报错才澄清 |
///
/// 「只有问数时原样返回」是刻意的：绝大多数问数题知识库里本来就没有对应资料，
/// 这一档必须与改造前逐字节一致，否则 79 条回归判据和前端渲染要跟着一起改。
///
/// ## 代价
///
/// 每一问多一次知识库检索。知识库那边模型给不出带角标的结论时会返回
/// [`dms_knowledge::answer::NO_HIT`]，这一档**不进合成**、也不影响主答案。
pub async fn dual(
    d: &AskDeps<'_>,
    p: &Principal,
    prepared: &PreparedQuestion,
    explicit_ds: Option<&str>,
    deterministic_fallback: bool,
) -> anyhow::Result<AskResult> {
    let outcome = dual_outcome(d, p, prepared, explicit_ds, deterministic_fallback).await?;
    Ok(fuse(outcome, prepared))
}

/// 两臂产物 → 一份 `AskResult`。**问数半在就以它为主体**，资料半挂 [`AskResult::kb`]。
///
/// 🔴 不套 `compound` 壳（2026-08-14 实测过代价）：容器会把顶层
/// `sql`/`columns`/`rows`/`row_count`/`view` 全清空，于是「导出 CSV」「AI 解读」消失、
/// 收据变空，`tools/regression.py` 79 题里 68 题当场红。
/// 真正的复合问句（Hybrid 合同 typed 拆出多条问数子问）仍走 [`into_ask_result`] ——
/// 那一档本来就有多个子结果，容器是它的正确载体。
fn fuse(outcome: HybridOutcome, prepared: &PreparedQuestion) -> AskResult {
    if let Some(card) = outcome.clarification {
        return card;
    }
    let HybridOutcome { data, knowledge, summary, data_note, .. } = outcome;
    let Some(mut r) = data else {
        let mut out = into_ask_result(
            HybridOutcome { data: None, knowledge, summary, clarification: None, data_note: None },
            prepared,
        );
        // 问数臂答不了的**原因**跟着上屏：用户问的是数据，只端一份资料答案会让他以为
        // 数据侧没意见。`into_ask_result` 已经把资料摘要写进 caliber_note，这里接在前面。
        if let Some(note) = data_note {
            out.caliber_note = Some(match out.caliber_note.take() {
                Some(kb) => format!("{note}
{kb}"),
                None => note,
            });
        }
        return out;
    };
    if knowledge.is_some() {
        r.kb = knowledge;
        // AI 综合落 `view.insight`，与混合问句同一个键位。已有洞察（SuperSonic textSummary）
        // 不覆盖：那是对**数据**的解读，综合是对两侧的解读，覆盖掉等于丢一条。
        if r.view.insight.is_none() {
            r.view.insight = summary;
        }
    }
    r
}

/// [`dual`] 的**类型化**出口：wire 形状归协议层。
///
/// HTTP 面把资料半塞进 `v["kb"]`（引用角标要能点开，`Answer` 必须整份过去），
/// CLI/判官那条路只需要「资料半答了什么」。两条路必须来自**同一次执行** ——
/// 这正是 `hybrid::run` 当初收口的理由，`dual` 不许再破一次。
pub async fn dual_outcome(
    d: &AskDeps<'_>,
    p: &Principal,
    prepared: &PreparedQuestion,
    explicit_ds: Option<&str>,
    deterministic_fallback: bool,
) -> anyhow::Result<HybridOutcome> {
    let data_fut = ask_data_arm(d, p, prepared, explicit_ds, deterministic_fallback);
    let kb_fut = async {
        let kb = d.kb.as_ref()?;
        crate::answerers::knowledge::answer(
            kb.owned,
            d.embed,
            &**d.llm,
            p,
            kb.space,
            &prepared.effective_question,
            kb.weights,
        )
        .await
        // 一路挂了不拖死另一路（与 `run` 同族纪律）
        .map_err(|e| tracing::warn!(err = %e, "两臂并行：知识库路失败 → 只用问数半"))
        .ok()
    };
    // 两路**并行**：总耗时 = 较大者，不是相加
    let (data_r, knowledge) = tokio::join!(data_fut, kb_fut);
    let knowledge = knowledge.filter(kb_has_substance);

    let only_kb = |a| HybridOutcome {
        data: None,
        knowledge: Some(a),
        summary: None,
        clarification: None,
        data_note: None,
    };
    let data = match data_r {
        Ok(r) => r,
        Err(e) => {
            // 🔴 权限类失败**一律上抛**，资料半有没有答出来都不算数（2026-08-14 自审）：
            // 「无权访问数据源」「权限注入失败」是本仓的 fail-closed 信号，降级成一句 warn
            // 就变成「用户拿到 200 + 一份资料答案」，而他其实**没有权限**看这份数据。
            // 其余失败（取数超时、SQL 执行错）才允许退化成单臂。
            let msg = e.to_string();
            let permission_failure = ["无权", "权限", "scope", "Scope", "principal", "Principal"]
                .iter()
                .any(|word| msg.contains(word));
            let Some(a) = knowledge.filter(|_| !permission_failure) else { return Err(e) };
            tracing::warn!(err = %e, "两臂并行：问数路失败 → 只用资料半");
            return Ok(only_kb(a));
        }
    };
    let Some(a) = knowledge else {
        return Ok(HybridOutcome {
            data: Some(data),
            knowledge: None,
            summary: None,
            clarification: None,
            data_note: None,
        });
    };
    // 问数臂只会说「我答不了」时不并排展示：一张反问卡配一份真资料，
    // 合成出来的「综合结论」会让用户以为数据侧也确认了什么。
    //
    // 🔴 但那张卡上的**解释**不许跟着丢（2026-08-14 自审）：用户问的是数据，
    // 「为什么算不出来」正是他要的信息之一；只把资料答案端上去，他会以为数据侧没意见。
    if !data_has_substance(&data) {
        let mut out = only_kb(a);
        out.data_note = data.caliber_note.clone().or_else(|| {
            (data.route == crate::ask::NEED_INTENT).then(|| "数据侧未能确定查询口径，以上只是资料侧的回答。".to_string())
        });
        return Ok(out);
    }
    let summary =
        crate::compound::hybrid_summary(&**d.llm, &prepared.effective_question, &data, &a).await;
    Ok(HybridOutcome { data: Some(data), knowledge: Some(a), summary, clarification: None, data_note: None })
}

/// 问数臂**答出东西了**吗。反问卡、出界卡、不可计算卡与空结果都不算 ——
/// 它们是「我答不了」的四种说法，不该和一份真资料并排展示成「综合结论」。
pub fn data_has_substance(r: &AskResult) -> bool {
    if matches!(r.route.as_str(), crate::ask::NEED_INTENT | crate::ask::NO_TOPIC)
        || crate::ask::is_unavailable_card_result(r)
    {
        return false;
    }
    // 🔴 「有块」不等于「有内容」：确定性视图对空结果同样兜底成 `[Table]`，
    // 于是一个 0 行的失败查询会被判成「有实质」，把真正答出东西的资料半挤成侧栏
    // （2026-08-14 生产实测：`route=llm+repair, rows=0` 却当了主答案）。
    // 只有**自带内容**的块（实体卡 / KPI 卡）才算数，表格必须有行。
    r.row_count > 0
        || !r.subs.is_empty()
        || r.view.blocks.iter().any(|block| {
            matches!(block, dms_kernel::present::Block::Entity { .. } | dms_kernel::present::Block::Kpis { .. })
        })
}

/// 资料臂**答出东西了**吗。判据只有一条：模型给不出任何带角标的结论时，
/// `finalize_markdown` 会把整份答案换成 [`dms_knowledge::answer::NO_HIT`]。
/// 拿它当界比任何相关度阈值都可靠 —— RRF 融合分不是标定过的相关度，阈值切不准。
pub fn kb_has_substance(a: &Answer) -> bool {
    match &a.body {
        dms_kernel::AnswerBody::Text { markdown, citations } => {
            !citations.is_empty() && !markdown.trim().starts_with(dms_knowledge::answer::NO_HIT)
        }
        _ => true,
    }
}

/// 类型化产物 → `AskResult`。**CLI/判官那条路的出口**：HTTP 侧另有自己的 wire 形状
/// （`v["kb"]` 等键位由 server 塑），但两条路必须来自**同一次执行**，这正是收口的意义。
///
/// 形状选 `compound` 容器：问数半原样进 `subs[0]`，知识库半包成文本结果进 `subs[1]`，
/// AI 综合落 `view.insight` —— 与既有复合问句的前端渲染同构，前端零改动。
pub fn into_ask_result(outcome: HybridOutcome, prepared: &PreparedQuestion) -> AskResult {
    if let Some(card) = outcome.clarification {
        return card;
    }
    let HybridOutcome { data, knowledge, summary, .. } = outcome;
    // 问数半可能**已经是**多子问的 compound（`fold_data`）：此时把它的 subs 直接抬上来，
    // 不再套第二层 —— 嵌套 compound 前端只渲染第一层，第二层的表格就这么消失了
    // （AX115 那次「深度轮嵌套产物」是同一个坑）。
    let mut subs = Vec::with_capacity(2);
    match data {
        Some(r) if !r.subs.is_empty() => subs.extend(r.subs),
        Some(r) => subs
            .push(crate::ctx::SubResult { question: prepared.effective_question.clone(), result: r }),
        None => {}
    }
    // 问数半缺席（纯资料问句）时耗时取本轮真实用时 —— 子结果没有就写 0，
    // 收据上会显示「0ms 答完」，那是明摆着的假数。
    let ms = if subs.is_empty() { prepared.started_at().elapsed().as_millis() } else { data_ms(&subs) };
    let mut out = AskResult::compound(subs, ms);
    if out.subs.is_empty() {
        // 路由标签要说实话：没有问数半的那次就不是 compound
        out.route = "knowledge".into();
    }
    // 知识库半不折成表格：它的载体是 `AnswerBody::Text{markdown, citations}`，硬塞进
    // 行列会把引用角标丢掉（角标 = citations 下标 + 1，丢了就点不开原文）。
    // CLI/判官这条路只需要「知识库半答了什么」，故落 `caliber_note`；
    // HTTP 侧仍由 server 把整份 `Answer` 塞进 `v["kb"]`（协议归 server）。
    if let Some(a) = knowledge {
        if let dms_kernel::AnswerBody::Text { markdown, citations } = &a.body {
            out.caliber_note = Some(format!(
                "知识库：{}（引用 {} 条）",
                markdown.chars().take(400).collect::<String>(),
                citations.len()
            ));
        }
        // 🔴 整份 `Answer` **也**带出去：`caliber_note` 只是 CLI 的 400 字摘要，
        // 引用角标全丢了。HTTP 侧要按 `kind:"text"` + `citations` 重新塑形（角标点得开
        // 才叫有引用），拿不到原件就只能退回自己再查一次知识库 —— 那就又是两条链路
        // 对同一问句各查各的，`hybrid::run` 当初收口要根治的正是这个。
        out.kb = Some(a);
    }
    out.view.insight = summary;
    out
}

/// N 条问数子结果 → 一个可承载的 `AskResult`。
///
/// 一条时**原样返回**（不套壳）：套一层 compound 会让原本直出表格的单问句多一层子结果，
/// 前端渲染与收据都跟着变形 —— 这是「一条也走通用路径」最常见的回归。
/// 多条时折进既有 compound 容器：子问题名用**投影后的子问句**，不是父问句
/// （父问句在每个 sub 上重复一遍，用户根本分不清哪块是哪块）。
fn fold_data(
    mut rs: Vec<AskResult>,
    prepared: &PreparedQuestion,
    qs: &[&RoutedQuestion],
) -> Option<AskResult> {
    match rs.len() {
        0 => None,
        1 => rs.pop(),
        _ => {
            let subs: Vec<crate::ctx::SubResult> = rs
                .into_iter()
                .zip(qs.iter())
                .map(|(result, q)| crate::ctx::SubResult { question: q.question.clone(), result })
                .collect();
            let ms = subs.iter().map(|s| s.result.elapsed_ms).max().unwrap_or(0);
            let _ = prepared; // 容器本身不带问句字段；子问句已逐条落在 SubResult 上
            Some(AskResult::compound(subs, ms))
        }
    }
}

/// 复合容器的耗时取子结果里最大的那个（两路并行，总耗时 = 较大者，不是相加）。
fn data_ms(subs: &[crate::ctx::SubResult]) -> u128 {
    subs.iter().map(|s| s.result.elapsed_ms).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(route: &str, sql: &str, blocks: Vec<dms_kernel::present::Block>, rows: usize) -> AskResult {
        let mut r = crate::ctx::AskResult::compound(vec![], 0);
        r.route = route.into();
        r.sql = sql.into();
        r.row_count = rows;
        r.view.blocks = blocks;
        r.subs = vec![];
        r
    }

    /// 🔴「有块」不等于「有内容」：确定性视图对空结果同样兜底成 `[Table]`，
    /// 于是 0 行的失败查询曾被判成「有实质」，把真正答出东西的资料半挤成侧栏
    /// （生产实测 `route=llm+repair, rows=0` 却当了主答案）。
    #[test]
    fn empty_table_is_not_substance_but_cards_are() {
        use dms_kernel::present::Block;
        assert!(!data_has_substance(&card("llm+repair", "SELECT 1", vec![Block::Table], 0)));
        assert!(data_has_substance(&card("llm", "SELECT 1", vec![Block::Table], 3)));
        // 卡片自带内容，0 行也算
        let entity = Block::Entity { pairs: vec![("单号".into(), serde_json::Value::from("X"))] };
        assert!(data_has_substance(&card("direct-doc", "SELECT 1", vec![entity], 0)));
        // 「我答不了」的四种说法一个都不算
        for route in [crate::ask::NEED_INTENT, crate::ask::NO_TOPIC] {
            assert!(!data_has_substance(&card(route, "", vec![Block::Table], 0)), "{route}");
        }
        let unavailable = card("direct-agg", "SELECT '不可计算' AS `数据状态`", vec![Block::Table], 1);
        assert!(!data_has_substance(&unavailable), "不可计算卡不算答出东西");
    }

    /// 资料臂的实质判据：模型给不出带角标的结论时 `finalize_markdown` 会整份换成 `NO_HIT`。
    #[test]
    fn kb_substance_needs_citations_and_not_no_hit() {
        use dms_kernel::{Answer, Citation};
        // 只需要「有没有引用」，字段值不重要
        let cite = || Citation { doc_id: "d".into(), doc_name: "n".into(), chunk_id: 1, ..Citation::default() };
        assert!(kb_has_substance(&Answer::text("有结论[^1]".into(), vec![cite()], 1)));
        assert!(!kb_has_substance(&Answer::text("有结论[^1]".into(), vec![], 1)), "没引用不算");
        assert!(
            !kb_has_substance(&Answer::text(dms_knowledge::answer::NO_HIT.into(), vec![cite()], 1)),
            "NO_HIT 不算"
        );
    }

    /// 四档合成：问数为主 + 资料挂 `kb`；只有一边时不套壳、不丢内容。
    #[test]
    fn fuse_covers_all_four_branches() {
        use dms_kernel::present::Block;
        let prepared = crate::ask::prepared_for_test("本月销售额");
        let data = || card("direct-agg", "SELECT 1", vec![Block::Table], 2);
        let answer = || dms_kernel::Answer::text("资料结论[^1]".into(), vec![], 1);

        // ① 两边都有 → 问数主体 + kb 挂件 + 综合落 insight
        let both = fuse(
            HybridOutcome { data: Some(data()), knowledge: Some(answer()), summary: Some("综合".into()), clarification: None, data_note: None },
            &prepared,
        );
        assert_eq!(both.route, "direct-agg", "主体必须还是问数结果");
        assert_eq!(both.row_count, 2, "行数不许被容器清零");
        assert!(both.kb.is_some(), "资料半必须挂在 kb 上");
        assert_eq!(both.view.insight.as_deref(), Some("综合"));

        // ② 只有问数 → 逐字原样（wire 与改造前一致）
        let only_data = fuse(
            HybridOutcome { data: Some(data()), knowledge: None, summary: None, clarification: None, data_note: None },
            &prepared,
        );
        assert_eq!(only_data.route, "direct-agg");
        assert!(only_data.kb.is_none());

        // ③ 只有资料 → route=knowledge，且**整份 Answer 带出去**（HTTP 要靠它重塑形状）
        let only_kb = fuse(
            HybridOutcome { data: None, knowledge: Some(answer()), summary: None, clarification: None, data_note: None },
            &prepared,
        );
        assert_eq!(only_kb.route, "knowledge");
        assert!(only_kb.kb.is_some(), "纯资料答案必须把原件带出去，否则角标点不开");

        // ④ 澄清卡原样返回
        let mut clar = card(crate::ask::NEED_INTENT, "", vec![], 0);
        clar.caliber_note = Some("请补充".into());
        let out = fuse(
            HybridOutcome { data: None, knowledge: None, summary: None, clarification: Some(clar), data_note: None },
            &prepared,
        );
        assert_eq!(out.route, crate::ask::NEED_INTENT);
        assert_eq!(out.caliber_note.as_deref(), Some("请补充"));
    }

    fn rq(route: IntentRoute, q: &str) -> RoutedQuestion {
        RoutedQuestion { route, question: q.to_string() }
    }

    /// 🔴 可执行的边界：**N 条问数 + 恰好 1 条资料**。
    ///
    /// 「两个数据问题 + 一个资料问题」以前吃澄清卡 —— 而它是最普通的问法之一
    /// （业主截图里那张「先问清再查」就是这么来的）。资料半仍限 1 条是载体上限（角标重编号）。
    #[test]
    fn split_takes_many_data_but_exactly_one_knowledge() {
        let one = [rq(IntentRoute::Data, "本月销售额"), rq(IntentRoute::Knowledge, "保修期")];
        let (data, kb) = split(&one).expect("一数一知");
        assert_eq!(data.len(), 1);
        assert_eq!(kb.question, "保修期");
        let many = [
            rq(IntentRoute::Data, "本月销售额"),
            rq(IntentRoute::Data, "本月毛利"),
            rq(IntentRoute::Knowledge, "退货政策"),
        ];
        assert_eq!(split(&many).expect("多数一知可执行").0.len(), 2);
        // 没有问数半 / 资料半 2 条 / 带 Unknown → 都不可执行
        assert!(split(&[rq(IntentRoute::Knowledge, "a")]).is_none());
        assert!(split(&[rq(IntentRoute::Data, "a"), rq(IntentRoute::Data, "b")]).is_none());
        assert!(split(&[
            rq(IntentRoute::Data, "a"),
            rq(IntentRoute::Knowledge, "b"),
            rq(IntentRoute::Knowledge, "c")
        ])
        .is_none());
        assert!(split(&[rq(IntentRoute::Data, "a"), rq(IntentRoute::Unknown, "b")]).is_none());
    }

    /// 澄清文案必须说清**是几条、卡在哪**：笼统一句「请说得更具体」用户只会换个说法再撞一次。
    #[test]
    fn cardinality_note_says_which_side_overflowed() {
        let two_kb = [
            rq(IntentRoute::Data, "a"),
            rq(IntentRoute::Knowledge, "b"),
            rq(IntentRoute::Knowledge, "c"),
        ];
        let note = cardinality_note(&two_kb);
        assert!(note.contains("2 个资料子任务") && note.contains("拆成 2 次"), "{note}");
        let unknown = [rq(IntentRoute::Data, "a"), rq(IntentRoute::Unknown, "b")];
        assert!(cardinality_note(&unknown).contains("归属仍不明确"), "未知子任务要单独说");
    }
}
