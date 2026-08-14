//! 自评闭环（引擎 B+/C）：三类复核的 prompt + 三个 parse 纯函数 + 四个编排。
//! 变更原因＝「复核问什么、怎么判」。
//!
//! 逐行搬 `server/src/pipeline.rs:886-948/1023-1032`
//! （`review_failure` / `review_lessons` / `review_exemplar` / `review_all_pending`），
//! prompt 文案与判据逐字保留 —— 它们决定哪条语料进 few-shot、哪条教训进召回，
//! 改一个字就是改自进化回路的口径。
//!
//! **SQL 全走 `dms_semantic::registry::exemplar`**：agent 不许自己写 `meta.*` 的 SQL，
//! 否则 ds/visibility 两道总闸的漂移守卫（`semantic/tests/drift.rs` 扫源码）扫不到它。
//!
//! `review_lessons` / `review_all_pending` 是 CLI 与定时任务的入口，**签名形状不许变**
//! （`server/src/main.rs:165/174` 按 `(llm, pg, limit) -> Result<usize>` 在调）。

use sqlx::PgPool;

use dms_kernel::{ChatModel, ChatRequest, ModelTier};
use dms_semantic::registry::{exemplar, extract_tables, pitfall};

/// 失败复盘（引擎 C）的判词
const FAILURE_SYSTEM: &str = "你是资深数据工程师，复盘一条执行失败的取数 SQL。判断根因类别：\
                  ①表/列用错 ②口径错误（过滤条件/码值/去重）③权限注入冲突 ④性能超时 ⑤问题本身合理但无数据。\
                  若是①②③④且能给出可复用教训，输出一行 lesson=...（≤80字，「表X.列Y是…」式口径知识，禁止复述错误原文）；\
                  若是⑤或无法确定通用教训，只输出 lesson=NO_LESSON。";

/// 候选教训复核（对齐 MemoryReviewTask）的判词
const LESSON_SYSTEM: &str = "你是资深数据工程师，审核一条自动复盘产出的取数教训。\
                      判 enabled：口径合理、表述通用可复用、不是错误原文复述、不是一次性的具体问题细节。\
                      否则判 disabled。只输出一行 verdict=enabled 或 verdict=disabled。";

/// 记忆复核（SuperSonic MemoryReviewTask）的判词
const EXEMPLAR_SYSTEM: &str = "你是资深数据工程师，审核一条 SQL 是否正确回答了给定问题（口径合理、表/字段对、无明显错误）。\
                  日期过滤是否精确不必挑剔。只输出一行：opinion=POSITIVE 或 opinion=NEGATIVE。";

/// 三类复核共用的一次 fast 调用。`None` = 挂了/超时/没回内容，各调用方按自己的兜底处理。
/// 失败分支各留一条 debug：复核回路停转时不能零日志。温度 0.1 的出处见 `insight::LLM_TEMP`。
async fn fast(llm: &dyn ChatModel, system: &str, user: &str) -> Option<String> {
    let req = ChatRequest::text(ModelTier::Fast, system, user, Some(crate::insight::LLM_TEMP));
    match llm.chat(req).await {
        Ok(r) => {
            if r.content.is_none() {
                tracing::debug!("复核 fast 调用回空 content");
            }
            r.content
        }
        Err(e) => {
            tracing::debug!(err = %e, "复核 fast 调用失败");
            None
        }
    }
}

/// 失败复盘（引擎 C）：fast LLM 分析「问题+SQL+MySQL 错误」的根因，产出候选教训。
/// 教训格式对齐存量 pitfall（一句话口径知识）；判无教训（纯权限无数据/问题无解）则 NO_LESSON 不落。
pub async fn review_failure(
    llm: &dyn ChatModel,
    // `who` = (批次号, 操作者)：批次号是本轮 trace_id，管理员据此撤回这一轮学到的东西
    who: (&str, &str),
    pg: &PgPool,
    ds: &str,
    question: &str,
    sql: &str,
    error: &str,
) {
    // 不包 wrap_untrusted：这是离线复核回路，输入是本方已执行的 SQL 与引擎错误原文，
    // 不是外部文本；且 prompt 逐字保留是自进化口径（模块头），加包裹要过口径评审。
    let user = format!("问题：{question}\nSQL：\n{sql}\n执行错误：{error}");
    let Some(resp) = fast(llm, FAILURE_SYSTEM, &user).await else { return }; // fast 内已留痕
    let Some(lesson) = parse_lesson(&resp) else {
        tracing::debug!("复盘回复无有效 lesson（无前缀 / NO_LESSON / 空 / 过长）→ 不落");
        return;
    };
    let tables = extract_tables(sql);
    if !tables.is_empty()
        && !pitfall::save_lesson_candidate(pg, who, ds, &tables, lesson).await
    {
        tracing::warn!("候选教训落库失败（save_lesson_candidate 返回 false）");
    }
}

/// 候选教训复核（对齐 MemoryReviewTask 思想）：LLM 判候选教训是否正确通用 → active/disabled。
pub async fn review_lessons(llm: &dyn ChatModel, pg: &PgPool, limit: i64) -> anyhow::Result<usize> {
    // 跨源管理批处理（复核所有源的候选教训），按 id 逐条更新，不需要 ds 谓词（判据在 exemplar 侧）
    let rows = pitfall::candidate_lessons(pg, limit).await?;
    // 复核批的批次号：粒度是「这一次复核批」（与问答轮的 trace_id 同族，只是另一种事件源）。
    // 用 std 的秒级时间戳，零新增依赖。
    let batch = format!(
        "review-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let mut n = 0;
    let mut misses = 0;
    for (id, tables, lesson) in rows {
        let user = format!("锚定：{tables}\n教训：{lesson}");
        let Some(resp) = fast(llm, LESSON_SYSTEM, &user).await else {
            // LLM 挂掉时逐条继续 = 每条各烧一次必败的 fast 调用（各付一次超时）：
            // 连续 3 次失败即熔断，本轮剩余的下批再议
            misses += 1;
            if misses >= 3 {
                tracing::warn!("候选教训复核连续 {misses} 次失败（疑似 LLM 挂掉）→ 本轮剩余下批再议");
                break;
            }
            continue;
        };
        misses = 0;
        // 落库失败整批上抛是**有意的**（与 `review_all_pending` 的逐条容错不同）：
        // 复核结论只有写进库才有意义，PG 写不进时逐条继续也是各烧一次必败写，
        // 不如整批报错让人看见；已成功的计数 n 随 Err 丢弃可接受（下批会重扫）。
        pitfall::set_lesson_status(pg, (&batch, "review"), id, parse_verdict(&resp)).await?;
        n += 1;
    }
    Ok(n)
}

/// 记忆初筛（移植 SuperSonic MemoryReviewTask）：fast LLM 判断 SQL 是否值得进入人工验证队列。
/// POSITIVE→保持 pending，NEGATIVE→disabled。只有人工确认并真实只读执行通过才会 enabled。
///
/// 返回 `true` = 结论**真的落库了**。为什么要有返回值（二·AS2）：`set_status` 原先是
/// `let _ =` 吞错，PG 抖一下一条都没更新，`review_all_pending` 照样报「处理了 N 条」——
/// 而这条 UPDATE 是「被判 NEGATIVE 的语料不再当范例传播」的唯一出口（二·Q 投毒对策）。
/// 与兄弟函数 `review_lessons`（`?` 传播、成功后才 `n += 1`）现在同一种诚实度。
pub async fn review_exemplar(
    llm: &dyn ChatModel,
    // `who` = (批次号, 操作者)。AI 初筛会把语料打成 disabled —— 判错了得能整批撤回。
    who: (&str, &str),
    pg: &PgPool,
    ds: &str,
    question: &str,
    sql: &str,
) -> bool {
    // 不包 wrap_untrusted（立场同 `review_failure` 的注释）：离线批处理，输入非外部文本
    let user = format!("问题：{question}\nSQL：\n{sql}\n审核结论：");
    // 复核失败保持 pending，下次再议
    let Some(resp) = fast(llm, EXEMPLAR_SYSTEM, &user).await else { return false };
    if let Err(e) = exemplar::set_ai_review(pg, who, ds, question, parse_opinion(&resp)).await {
        // 带问句：批量复核一次扫 100 条，不带问句就查不出是哪条卡住
        // （问句截定长：整句塞结构化字段会让日志行膨胀到 KB 级）
        let q: String = question.chars().take(120).collect();
        tracing::warn!(question = %q, error = %e, "语料复核结论落库失败，保持 pending 下次再议");
        return false;
    }
    true
}

/// 批量复核 pending 语料（移植 SuperSonic MemoryReviewTask 定时扫 pending）。
/// 返回**真正落库**的条数（原先是 `rows.len()`，见 `count_reviewed`）。
pub async fn review_all_pending(
    llm: &dyn ChatModel,
    pg: &PgPool,
    limit: i64,
) -> anyhow::Result<usize> {
    // 跨源批处理：每行带着自己的 ds_id 回来（复核是逐条的，不需要 ds 谓词）
    let rows = exemplar::pending(pg, limit).await?;
    // 一次批量复核 = 一个批次（与 `review_lessons` 同一形态）
    let batch = format!(
        "screen-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let who = (batch.as_str(), "ai-screen");
    Ok(count_reviewed(rows, |(ds, q, sql)| async move {
        review_exemplar(llm, who, pg, &ds, &q, &sql).await
    })
    .await)
}

/// 逐条复核 → **只数真正成功的那些**。原形态是先 `let n = rows.len()` 再逐条干活，
/// 于是「取回了 N 行」被当成「处理了 N 条」上报（二·AS2）。
///
/// 🔴 抽成带闭包参数的小函数，唯一理由是**判据打不到原处**：`set_status` 吃 `&PgPool`，
/// 用 `connect_lazy` 能造出「写入必失败」的假池，却造不出「写入成功」的假池。
/// 没有这条缝，「写失败时条数 ≠ 行数」那一条判据把返回值写成恒 0 也全绿
/// （本仓已抓到 25+ 条这种恒真判据）。两个方向都判：全 false→0、全 true→3、混合→1。
/// 实测三次反向验证：改回 `total = rows.len()` → 两条判据同时红；
/// 改成恒 0 → 只 `success_is_counted` 红；把 `set_status` 的错吞回去 → 只
/// `write_failure_is_not_counted` 红（＝这两条判据各盯一半，没有互相顶替）。
async fn count_reviewed<R, F, Fut>(rows: Vec<R>, review: F) -> usize
where
    F: Fn(R) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut n = 0;
    for row in rows {
        if review(row).await {
            n += 1;
        }
    }
    n
}

/// 复盘回复 → 候选教训（**纯函数**）。取第一个 `lesson=` 前缀行（模型多印一行前言不该整篇丢弃）；
/// 空 / `NO_LESSON` / 过长一律不落。长度判据用**字符数**，与 prompt 的「≤80字」对齐
/// （原先按字节 200：80 个汉字 = 240 字节，按 prompt 合规的教训会被闸门丢掉 —— 两处阈值矛盾）。
fn parse_lesson(resp: &str) -> Option<&str> {
    let lesson = resp.lines().find_map(|line| line.trim().strip_prefix("lesson="))?.trim();
    if lesson.is_empty() || lesson == "NO_LESSON" || lesson.chars().count() > 80 {
        return None;
    }
    Some(lesson)
}

/// 教训复核回复 → 状态（**纯函数**）。定位**最后一个** `verdict=` 取其值精确比较：
/// `contains("verdict=enabled")` 会命中否定语境（「不应判 verdict=enabled，应判 verdict=disabled」
/// —— 结论在最后），而 enabled 是宽松侧（坏教训进后续所有 prompt），方向判反代价大。
/// 判不出一律 disabled —— 宁可漏启用不许误启用。
fn parse_verdict(resp: &str) -> &'static str {
    match resp.rfind("verdict=") {
        Some(i) if resp[i + "verdict=".len()..].starts_with("enabled") => "active",
        _ => "disabled",
    }
}

/// 语料初筛回复 → 意见。定位 `opinion=`（最后一个 = 结论位）取其值与 NEGATIVE 做 ASCII
/// 大小写无关比较：`to_uppercase().contains("NEGATIVE")` 会命中「not NEGATIVE」
/// 「opinion=POSITIVE 而非 NEGATIVE」这类否定语境。只有明确 NEGATIVE 才剔除，其余保留 pending。
fn parse_opinion(resp: &str) -> &'static str {
    match resp.rfind("opinion=").map(|i| &resp[i + "opinion=".len()..]) {
        // `get(..8)` 而不是硬切片：值以非 ASCII 开头时切片会切在多字节字符内部当场 panic
        Some(v) if v.get(..8).is_some_and(|head| head.eq_ignore_ascii_case("NEGATIVE")) => "negative",
        _ => "positive",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use dms_kernel::{BoxFut, ChatReply, LlmError};

    /// 假模型：一调就回同一句结论（这两条判据只关心「结论出来之后落不落得进库」）
    struct Fake(&'static str);

    impl ChatModel for Fake {
        fn chat<'a>(&'a self, _req: ChatRequest) -> BoxFut<'a, Result<ChatReply, LlmError>> {
            let r = self.0.to_string();
            Box::pin(async move { Ok(ChatReply { content: Some(r), usage: Default::default() }) })
        }
    }

    /// 「PG 抖一下 / 写不进去」的假件：lazy 池指到没人监听的 127.0.0.1:1（同 `connector/src/fixed.rs`
    /// 那几条测试的手法，无库无网）。**造池的那一行必须住在 connector** ——
    /// 架构门禁第①条「agent 不得造连接池」按 `PgPoolOptions` 判，本文件写它会当场 FAIL
    /// （实测 `[FAIL] agent 不得造连接池 → review.rs:191`）；改写类型路径绕 grep 是拿门禁换绿。
    /// 取连接超时压到 200ms —— sqlx 默认 30s，不压这条判据要跑半分钟。
    /// 200ms 那个值不是洁癖：我先试过不压超时的 `PgPool::connect_lazy`，
    /// 以为 ECONNREFUSED 会立刻返错 —— **实测 sqlx 会重连到默认的 30s**，
    /// 两条判据各等一轮 ⇒ `finished in 60.01s`。这条推断是被我自己加的耗时断言当场抓住的。
    fn dead_pool() -> PgPool {
        dms_connector::owned::dead_pg_pool_for_tests(std::time::Duration::from_millis(200))
    }

    /// `exemplar::pending` 的返回形状 `(ds, question, sql)`
    fn rows(n: usize) -> Vec<(String, String, String)> {
        (0..n).map(|i| ("dms".into(), format!("第{i}条问句"), "SELECT 1".into())).collect()
    }

    /// 🔴 二·AS2 本体：LLM 判了 NEGATIVE 但 UPDATE 写不进 PG → **一条都不许算处理过**。
    /// 原实现返回 `rows.len()`，于是「一条没 disable」与「全部 disable 成功」上报同一个数字，
    /// 而没 disable 掉的语料继续当 few-shot 范例喂给下一个问句。
    #[tokio::test]
    async fn write_failure_is_not_counted() {
        let (fake, pool) = (Fake("opinion=NEGATIVE"), dead_pool());
        // 先绑成引用再进闭包：`async move` 里直接用 fake/pool 会把它们移进 future，闭包退化成 FnOnce
        let (llm, pg) = (&fake as &dyn ChatModel, &pool);
        let rs = rows(2);
        let n = count_reviewed(rs.clone(), |(ds, q, sql): (String, String, String)| async move {
            review_exemplar(llm, ("t-batch", "test"), pg, &ds, &q, &sql).await
        })
        .await;
        assert_ne!(n, rs.len(), "写不进库还报「处理了 N 条」＝ 投毒语料继续当范例传播");
        assert_eq!(n, 0);
    }

    /// 🔴 死池必须**快速失败**，而不是等 sqlx 默认的 30s。
    /// 这条守的是「有人给 `dead_pool` 换了个会挂住的地址」——那样上面两条判据不会红，
    /// 只会让整支测试从 0.1s 变成一分钟，而慢测试最后总会被人 `#[ignore]` 掉。
    #[tokio::test]
    async fn dead_pool_fails_fast_not_after_the_default_timeout() {
        let t = std::time::Instant::now();
        let e = dead_pool().acquire().await.expect_err("127.0.0.1:1 不该连得上");
        let ms = t.elapsed().as_millis();
        assert!(ms < 3000, "取连接花了 {ms}ms —— 退化成了默认 30s 那条路：{e}");
    }

    /// 🔴 防恒真的另一半：没有这条，把返回值写成恒 0 上面那条也全绿。
    /// 混合那一例同时排除恒 0 与恒 `len`。
    /// ⚠️ 只判到 `count_reviewed` 这一层：造不出「PG 写入成功」的假池（见该函数注释），
    /// 所以 `review_exemplar` 返 `true` 的那一半**没有判据覆盖**。
    #[tokio::test]
    async fn success_is_counted() {
        let rs = rows(3);
        assert_eq!(count_reviewed(rs.clone(), |_| async { true }).await, 3, "全成功＝取回行数");
        assert_eq!(count_reviewed(rs.clone(), |_| async { false }).await, 0);
        let n = count_reviewed(rs, |(_, q, _): (String, String, String)| async move {
            q.contains('1')
        })
        .await;
        assert_eq!(n, 1, "三行里只有「第1条问句」那行成功");
    }

    /// 🔴 判据**必须打在 bug 站点上**。上面两条都只打在 `count_reviewed` 上，
    /// 而 二·AS2 的 bug 那一行在 `review_all_pending` 里 —— 交叉审实测：
    /// 把它改回 `let n = rows.len(); for … { review_exemplar(…).await; } Ok(n)`
    /// （`count_reviewed` 一个字不动），`-p dms-agent review` **5 passed / 0 failed 全绿**。
    /// 也就是说「取回 N 行当成处理 N 条」可以原样回来而判据不响。
    ///
    /// 无库单测覆盖不到这段 IO，所以照本仓既有形态（`gather.rs::gather_all_cards_actually_reads_the_registry`）
    /// 用**源码**守：函数体必须经 `count_reviewed`，且**不许**再出现 `rows.len()`。
    #[test]
    fn review_all_pending_counts_through_count_reviewed() {
        let src = include_str!("review.rs");
        let s = src
            .split("pub async fn review_all_pending")
            .nth(1)
            .expect("函数改名了 —— 顺手把这条判据一起改");
        // 函数体到下一个顶层文档注释为止（本文件每个顶层项前都有 `///`）
        let body = s.split("\n///").next().unwrap();
        assert!(body.contains("count_reviewed("), "回到了「先数行数再干活」的形态");
        assert!(
            !body.contains("rows.len()"),
            "`rows.len()` 又出现了 —— 那正是二·AS2：取回 N 行被当成处理 N 条上报"
        );
        // 防恒真两头钉：切出来的必须真的是那个函数体，不是整份源码
        assert!(body.contains("exemplar::pending"), "切段没切住：{body}");
        assert!(body.len() < 800, "切段没切住，body {} 字符 —— 断言会因为看的是整份源码而恒真", body.len());
    }

    /// 候选教训的准入：前缀、NO_LESSON、空、过长四道
    #[test]
    fn lesson_parsing() {
        assert_eq!(parse_lesson("lesson=表t_x.列y是含税额").unwrap(), "表t_x.列y是含税额");
        assert_eq!(parse_lesson("  lesson=  两边空白也认  ").unwrap(), "两边空白也认");
        assert!(parse_lesson("lesson=NO_LESSON").is_none(), "判无教训不许落库");
        assert!(parse_lesson("lesson=").is_none());
        assert!(parse_lesson("这条 SQL 用错了表").is_none(), "没有 lesson= 前缀一律不落");
        // 过长 = 大概率是把错误原文整段复述回来了（字符数判据，与 prompt 的「≤80字」对齐）
        assert!(parse_lesson(&format!("lesson={}", "x".repeat(81))).is_none());
        assert!(parse_lesson(&format!("lesson={}", "x".repeat(80))).is_some());
        // 80 个汉字合规：旧字节闸（>200）会把它误丢掉
        assert!(parse_lesson(&format!("lesson={}", "汉".repeat(80))).is_some());
        assert!(parse_lesson(&format!("lesson={}", "汉".repeat(81))).is_none());
        // 模型多印一行前言不该整篇丢弃：取第一个 lesson= 前缀行
        assert_eq!(
            parse_lesson("我来分析一下。\nlesson=表t_x.列y是含税额").unwrap(),
            "表t_x.列y是含税额"
        );
    }

    /// 教训复核：默认 disabled（判不出就别放进后续所有 prompt）
    #[test]
    fn verdict_defaults_to_disabled() {
        assert_eq!(parse_verdict("verdict=enabled"), "active");
        assert_eq!(parse_verdict("我认为 verdict=enabled，理由…"), "active");
        assert_eq!(parse_verdict("verdict=disabled"), "disabled");
        assert_eq!(parse_verdict("说不清"), "disabled", "判不出一律 disabled");
        assert_eq!(parse_verdict(""), "disabled");
        // 否定语境：结论在最后（contains("verdict=enabled") 会在这里判反方向）
        assert_eq!(
            parse_verdict("不应判 verdict=enabled，应判 verdict=disabled"),
            "disabled",
            "否定语境不许判反方向"
        );
    }

    /// 语料复核：默认 enabled，只有明确 NEGATIVE 才剔除（方向与教训相反，刻意的）
    #[test]
    fn opinion_defaults_to_enabled() {
        assert_eq!(parse_opinion("opinion=NEGATIVE"), "negative");
        assert_eq!(parse_opinion("opinion=negative"), "negative", "大小写不敏感");
        assert_eq!(parse_opinion("opinion=POSITIVE"), "positive");
        assert_eq!(parse_opinion("看不出问题"), "positive");
        // 否定语境不许误剔：取值位是 POSITIVE，后面提到 NEGATIVE 只是行文
        assert_eq!(parse_opinion("opinion=POSITIVE 而非 NEGATIVE"), "positive");
        assert_eq!(parse_opinion("not NEGATIVE"), "positive", "没有 opinion= 取值不参与判定");
        // 值以非 ASCII 开头不许 panic（`get(..8)` 守的就是这个）
        assert_eq!(parse_opinion("opinion=无法判断"), "positive");
    }
}
