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
use dms_semantic::registry::{exemplar, extract_tables};

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
/// 温度 0.1 = 搬运前 `LlmClient::chat` 写死的那个值（`server/src/llm.rs:53`）。
async fn fast(llm: &dyn ChatModel, system: &str, user: &str) -> Option<String> {
    let req = ChatRequest::text(ModelTier::Fast, system, user, Some(0.1));
    llm.chat(req).await.ok()?.content
}

/// 失败复盘（引擎 C）：fast LLM 分析「问题+SQL+MySQL 错误」的根因，产出候选教训。
/// 教训格式对齐存量 pitfall（一句话口径知识）；判无教训（纯权限无数据/问题无解）则 NO_LESSON 不落。
pub async fn review_failure(
    llm: &dyn ChatModel,
    pg: &PgPool,
    ds: &str,
    question: &str,
    sql: &str,
    error: &str,
) {
    let user = format!("问题：{question}\nSQL：\n{sql}\n执行错误：{error}");
    let Some(resp) = fast(llm, FAILURE_SYSTEM, &user).await else { return };
    let Some(lesson) = parse_lesson(&resp) else { return };
    let tables = extract_tables(sql);
    if !tables.is_empty() {
        exemplar::save_lesson_candidate(pg, ds, &tables, lesson).await;
    }
}

/// 候选教训复核（对齐 MemoryReviewTask 思想）：LLM 判候选教训是否正确通用 → active/disabled。
pub async fn review_lessons(llm: &dyn ChatModel, pg: &PgPool, limit: i64) -> anyhow::Result<usize> {
    // 跨源管理批处理（复核所有源的候选教训），按 id 逐条更新，不需要 ds 谓词（判据在 exemplar 侧）
    let rows = exemplar::candidate_lessons(pg, limit).await?;
    let mut n = 0;
    for (id, trig, lesson) in rows {
        let user = format!("锚定：{trig}\n教训：{lesson}");
        let Some(resp) = fast(llm, LESSON_SYSTEM, &user).await else { continue };
        exemplar::set_lesson_status(pg, id, parse_verdict(&resp)).await?;
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
    pg: &PgPool,
    ds: &str,
    question: &str,
    sql: &str,
) -> bool {
    let user = format!("问题：{question}\nSQL：\n{sql}\n审核结论：");
    // 复核失败保持 pending，下次再议
    let Some(resp) = fast(llm, EXEMPLAR_SYSTEM, &user).await else { return false };
    if let Err(e) = exemplar::set_ai_review(pg, ds, question, parse_opinion(&resp)).await {
        // 带问句：批量复核一次扫 100 条，不带问句就查不出是哪条卡住
        tracing::warn!(question, error = %e, "语料复核结论落库失败，保持 pending 下次再议");
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
    Ok(count_reviewed(rows, |(ds, q, sql)| async move {
        review_exemplar(llm, pg, &ds, &q, &sql).await
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

/// 复盘回复 → 候选教训（**纯函数**）。必须以 `lesson=` 开头；空 / `NO_LESSON` / 过长一律不落。
/// 长度判据用字节（`str::len`，与搬运前一字不差）：这里只是防「把整段错误原文当教训」，
/// 不是显示宽度。
fn parse_lesson(resp: &str) -> Option<&str> {
    let lesson = resp.trim().strip_prefix("lesson=")?.trim();
    if lesson.is_empty() || lesson == "NO_LESSON" || lesson.len() > 200 {
        return None;
    }
    Some(lesson)
}

/// 教训复核回复 → 状态（**纯函数**）。只有明确 `verdict=enabled` 才启用：
/// 判不出一律 disabled —— 教训会进后续所有 prompt，宁可漏启用不许误启用。
fn parse_verdict(resp: &str) -> &'static str {
    if resp.contains("verdict=enabled") {
        "active"
    } else {
        "disabled"
    }
}

/// 语料初筛回复 → 意见。只有明确 NEGATIVE 才剔除，其余保留 pending 等人工验证。
fn parse_opinion(resp: &str) -> &'static str {
    if resp.to_uppercase().contains("NEGATIVE") {
        "negative"
    } else {
        "positive"
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

    /// 「PG 抖一下」的假件：lazy 池指到没人监听的 127.0.0.1:1（同 `connector/src/fixed.rs`
    /// 那几条测试的手法，无库无网）。取连接超时压到 200ms —— sqlx 默认 30s，
    /// 不压这条判据要跑半分钟。
    /// 「PG 写不进去」的假件。**造池的那一行必须住在 connector** ——
    /// 架构门禁第①条「agent 不得造连接池」按 `PgPoolOptions` 判，本文件写它会当场 FAIL
    /// （实测 `[FAIL] agent 不得造连接池 → review.rs:191`）；改写类型路径绕 grep 是拿门禁换绿。
    ///
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
            review_exemplar(llm, pg, &ds, &q, &sql).await
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
        // 过长 = 大概率是把错误原文整段复述回来了
        assert!(parse_lesson(&format!("lesson={}", "x".repeat(201))).is_none());
        assert!(parse_lesson(&format!("lesson={}", "x".repeat(200))).is_some());
    }

    /// 教训复核：默认 disabled（判不出就别放进后续所有 prompt）
    #[test]
    fn verdict_defaults_to_disabled() {
        assert_eq!(parse_verdict("verdict=enabled"), "active");
        assert_eq!(parse_verdict("我认为 verdict=enabled，理由…"), "active");
        assert_eq!(parse_verdict("verdict=disabled"), "disabled");
        assert_eq!(parse_verdict("说不清"), "disabled", "判不出一律 disabled");
        assert_eq!(parse_verdict(""), "disabled");
    }

    /// 语料复核：默认 enabled，只有明确 NEGATIVE 才剔除（方向与教训相反，刻意的）
    #[test]
    fn opinion_defaults_to_enabled() {
        assert_eq!(parse_opinion("opinion=NEGATIVE"), "negative");
        assert_eq!(parse_opinion("opinion=negative"), "negative", "大小写不敏感");
        assert_eq!(parse_opinion("opinion=POSITIVE"), "positive");
        assert_eq!(parse_opinion("看不出问题"), "positive");
    }
}
