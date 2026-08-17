//! 语义缓存成员：向量找近义已复核语料，护栏全等则**按当轮用户重新注入**后复用其 SQL（0-LLM）。
//! 变更原因＝「什么样的历史问答可以当本问的答案」。
//!
//! 搬运源 `server/src/pipeline.rs:953-975`（两张词表）+ `980-1021`（`try_semantic_cache`），逐行搬运。
//!
//! 🔴 **回放必须按当轮用户重新注入**（不变量 I4：缓存不跨用户）：语料里存的是**注入前**的
//! SQL 原文（`exemplar::save` 存的是 `candidate`），所以回放时重走一遍 `gate_on`。
//! 直接复用注入后的串就是把甲的行级条件端给乙。

use std::collections::BTreeSet;
use std::time::Instant;

use dms_connector::embed::{to_pgvector, EmbedClient};
use dms_kernel::BoxFut;
use dms_semantic::registry::exemplar;

use crate::answerers::Answerer;
use crate::ctx::{table_answer, AskCtx, AskResult};
use crate::gate::{gate_on, is_guard_err, EXEC_TIMEOUT, MAX_ROWS};

/// 余弦距离上限：超过就不算「近义」。
const MAX_DIST: f64 = 0.12;

pub struct CacheAnswerer {
    /// 实例式（connector 侧禁全局单例）；`Clone` 共享熔断状态，wire 侧传 `AppState` 那一份的克隆。
    embed: EmbedClient,
    /// 追问判据。函数本体按迁移表落 `ask.rs`（`rewrite_followup` 也用它），那不是本文件；
    /// 用 `fn` 指针注入＝零开销，且不复制第二份词表。wire 时传 `crate::ask::is_followup`。
    is_followup: fn(&str) -> bool,
}

impl CacheAnswerer {
    pub fn new(embed: EmbedClient, is_followup: fn(&str) -> bool) -> Self {
        Self { embed, is_followup }
    }

    /// `p` 只为闸门（`gate_on` 的放行支要身份铸 proof），命中的 SQL 仍按**当轮**用户重新注入（I4）。
    async fn replay(&self, cx: &AskCtx<'_>, t0: Instant) -> anyhow::Result<Option<AskResult>> {
        let Some(vec) = self.embed.embed_query(cx.question).await else { return Ok(None) };
        let vlit = to_pgvector(&vec);
        // 最近义的一条 enabled 语料 + 余弦距离（ds 谓词不可省：复用别的源的 SQL 必答错表）
        let Some((hit_q, hit_sql, dist)) = exemplar::nearest(cx.pg, cx.ds, &vlit, cx.question).await
        else {
            return Ok(None);
        };
        if !passes_guards(cx.question, &hit_q, dist) {
            return Ok(None);
        }
        let intent_coverage = crate::intent::sql_coverage(cx.intent, &hit_sql, cx.source.dialect());
        if !intent_coverage.complete() {
            tracing::warn!(
                ?intent_coverage,
                "语义缓存 SQL 未覆盖本轮结构化意图 → 回落 LLM"
            );
            return Ok(None);
        }
        // 命中：复用 SQL（数据实时查、权限按**当轮**用户重新注入，I4）。
        let scoped = match gate_on(cx.p, &hit_sql, cx.scope, cx.ds_global, cx.source.dialect()) {
            Ok(s) => s,
            // 只读红线不过（语料里存了一条今天已不合格的 SQL）→ 回落，但**必须留痕**：
            // 拆分前是 `.ok()?` 一声不响，于是「缓存一直在静默失效」看起来和「缓存没命中」一样。
            Err(e) if is_guard_err(&e) => {
                tracing::warn!("语义缓存回放被红线拒（{e}）→ 回落 LLM；语料问句：{hit_q}");
                return Ok(None);
            }
            // 🔴 权限注入失败（`ConditionParse` / `UnregisteredTable`）**不许吞**：那两个是
            // fail-closed 信号，静默回落等于「本用户不该看这张表」变成「换条路给他查」。
            Err(e) => return Err(e),
        };
        let Ok(rs) = cx.source.fetch(&scoped, MAX_ROWS, EXEC_TIMEOUT).await else {
            return Ok(None);
        };
        // 复用的是已复核通过的语料，故不再自判口径（`hits::land` 的注释解释了为什么）
        Ok(Some(table_answer(&scoped, rs, "semantic-cache", t0)))
    }
}

impl Answerer for CacheAnswerer {
    fn route(&self) -> &'static str {
        "semantic-cache"
    }

    /// 追问不许命中缓存（`pipeline.rs:707`）：追问的字面很短、语义靠上一轮撑着，
    /// 向量近邻会稳定地把它认成别的问题。
    fn accept(&self, cx: &AskCtx<'_>) -> bool {
        cx.intent_attempt.is_data_executable() && !(self.is_followup)(cx.question)
    }

    fn answer<'a>(&'a self, cx: &'a AskCtx<'a>) -> BoxFut<'a, anyhow::Result<Option<AskResult>>> {
        Box::pin(self.replay(cx, cx.t0))
    }
}

/// 回放前的**四关护栏**（纯函数，一关都不许少）：距离、时间词、数字词、**限定词**。
/// 前三关都是「语义近似 ≠ 问的是同一件事」的补丁：向量把「上月销售额」和「本月销售额」
/// 认成 0.03 的距离，直接复用就是给用户一个月份错的数字，而它长得完全正常。
fn passes_guards(question: &str, hit_q: &str, dist: f64) -> bool {
    if dist > MAX_DIST {
        return false; // 不够近
    }
    // 护栏：时间词、数字词集合必须全等（否则语义近似会把上月命中本月）
    if time_tokens(question) != time_tokens(hit_q) || number_tokens(question) != number_tokens(hit_q)
    {
        return false;
    }
    // 🔴 第四关：本问句里的**范围限定词**必须在被命中的那句里逐字出现（2026-08-17）。
    //
    // 前三关只管时间和数字，管不住「谁/哪个/哪里」这一维：
    // 「京东仓的库存量是多少」与「现在库存量是多少」时间词都空、数字词都空、
    // 向量距离很近 —— 于是前者会 replay 后者的全仓总量 SQL，答出 1.06 亿，
    // **收据还全绿**（回放走的是 `table_answer`，绕开了覆盖闸）。
    // 用户看不出这个数少了一个 WHERE。这正是本仓最不能接受的一类错答。
    //
    // 判据是**单向**的：本问的限定必须在命中问句里出现，反过来不要求 ——
    // 命中问句更窄不会让答案变宽（它的 SQL 带着更严的 WHERE，最多是答不全，
    // 而距离门已经把差太远的挡在外面）。
    // 判据钉「问句里出现过的实词」而不是某张实体名单：名单永远有下一个漏项，
    // 而这里要防的恰恰是「没人登记过的那个词」。
    scope_tokens(question)
        .iter()
        .all(|token| hit_q.contains(token.as_str()))
}

/// 问句里的**范围限定词**：剥掉时间词、数字、以及纯功能词之后剩下的实词片段。
///
/// 不追求分词精确 —— 它只需要回答一个问题：「这两句话限定的范围是不是同一个」。
/// 宁可多留几个词（缓存少命中一次，代价是慢一点），也不能少留（代价是答错一个数）。
fn scope_tokens(question: &str) -> Vec<String> {
    // 功能词/句式词：出现在几乎每一句里，留着它们等于要求两句话逐字相同
    const NOISE: &[&str] = &[
        "是", "多少", "的", "了", "吗", "呢", "有", "在", "和", "与", "及", "或",
        "查", "查询", "看", "看看", "统计", "一下", "请", "帮我", "我", "你", "现在",
        "目前", "当前", "总", "共", "全部", "所有", "合计", "汇总", "整体", "累计",
        "怎么样", "如何", "什么", "哪些", "几", "个", "、", "，", ",", "。", "?", "？",
    ];
    let mut stripped = question.to_string();
    for word in time_tokens(question) {
        stripped = stripped.replace(word, " ");
    }
    for word in NOISE {
        stripped = stripped.replace(word, " ");
    }
    stripped
        .split(|c: char| c.is_whitespace() || c.is_ascii_digit() || c.is_ascii_punctuation())
        .map(str::trim)
        .filter(|piece| piece.chars().filter(|c| !c.is_ascii()).count() >= 2)
        .map(str::to_string)
        .collect()
}

/// 时间词集合（护栏：命中缓存的问题时间词必须与本问全等，"上月"≠"本月"）。
/// 搬运源 `pipeline.rs:953`，逻辑一字未改。
///
/// 🔴 **单一事实源**：`pub` 只为 `triage::time_hit` 复用这张词表（判据是「集合非空」）。
/// 词表在这里有第二个消费者了：改它会同时影响缓存护栏与分诊，抄第二份就等于埋一处会漂的表。
/// 断言 `cache_time_guard` 随消费者落在 `triage.rs`（那边逐字搬的），本文件不抄第二份。
pub fn time_tokens(q: &str) -> BTreeSet<&'static str> {
    ["今天", "昨天", "前天", "本月", "上月", "上个月", "这个月", "本周", "上周", "今年", "去年", "本季度"]
        .into_iter()
        .filter(|t| q.contains(t))
        .collect()
}

/// 数字词集合（护栏："前5"≠"前10"）
fn number_tokens(q: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let mut cur = String::new();
    for c in q.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            set.insert(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        set.insert(cur);
    }
    set
}

#[cfg(test)]
mod tests {

    /// 第四关：范围限定不同的两句话不许互相回放（2026-08-17）。
    #[test]
    fn a_narrower_question_never_replays_a_broader_ones_answer() {
        // 🔴 这一对是本关的存在理由：时间词都空、数字词都空、向量距离很近，
        // 前三关全过 —— 回放出来就是全仓 1.06 亿，少了一个 WHERE，收据还全绿。
        assert!(
            !super::passes_guards("京东仓的库存量是多少", "现在库存量是多少", 0.02),
            "带仓库限定的问句不许回放全量答案"
        );
        for (q, hit) in [
            ("华东区的销售额", "销售额是多少"),
            ("烤肠的库存量", "库存量是多少"),
            ("线上渠道的订单数", "订单数是多少"),
        ] {
            assert!(!super::passes_guards(q, hit, 0.02), "{q} 不许回放 {hit}");
        }
        // 正向：同一件事的不同说法照旧命中（不然缓存就废了）
        for (q, hit) in [
            ("本月销售额", "本月销售额是多少"),
            ("本月销售额是多少", "查一下本月销售额"),
        ] {
            assert!(super::passes_guards(q, hit, 0.02), "{q} 应当能命中 {hit}");
        }
        // 单向：命中问句更窄不拦（它的 SQL 带更严的 WHERE，不会把答案变宽）
        assert!(
            super::passes_guards("库存量是多少", "京东仓的库存量是多少", 0.02),
            "反方向不要求 —— 更窄的命中不会让答案变宽"
        );
        // 前三关一个字没松
        assert!(!super::passes_guards("本月销售额", "上月销售额", 0.02), "时间词那关还在");
        assert!(!super::passes_guards("前5名商品", "前10名商品", 0.02), "数字词那关还在");
        assert!(!super::passes_guards("本月销售额", "本月销售额", 0.9), "距离那关还在");
    }
    use super::*;

    // `cache_time_guard`（时间词护栏）随词表落在 `triage.rs`，不在这里抄第二份断言；
    // 本文件的 `three_guards_each_reject_one_case` 连同距离与数字词一起再守一遍。

    #[test]
    fn cache_number_guard() {
        assert_ne!(number_tokens("前5的省份"), number_tokens("前10的省份"));
        assert_eq!(number_tokens("销售额"), number_tokens("营业额")); // 都无数字
    }

    /// 🔴 三关护栏各一条该拒的 + 一条该放的。三关全在这一个纯函数里，
    /// 少任何一关这里就绿不了 —— 而线上的症状是「数字看着正常但月份/条数不对」。
    #[test]
    fn three_guards_each_reject_one_case() {
        // ① 距离：刚好等于阈值放行，超一点就拒（`dist > 0.12` 逐字保留）
        assert!(passes_guards("本月销售额", "本月销售额是多少", MAX_DIST));
        assert!(!passes_guards("本月销售额", "本月销售额是多少", MAX_DIST + 0.001));
        // ② 时间词：距离再近也不许把上月的 SQL 当本月的答案
        assert!(!passes_guards("本月销售额", "上月销售额", 0.01));
        // ③ 数字词：前5 ≠ 前10
        assert!(!passes_guards("销售额前5的省份", "销售额前10的省份", 0.01));
    }

    /// wire 形态能编译 + 追问门禁真的会否决（`accept` 只读 `cx.question`，但 `AskCtx` 要池，
    /// 故这里直接验注入进来的判据本身：漏传 `is_followup` 就是让追问去命中别人的 SQL）。
    #[test]
    fn wire_shape_compiles_and_followup_is_rejected() {
        let a = CacheAnswerer::new(EmbedClient::new("http://127.0.0.1:8077"), |q| q == "那上月呢");
        assert!((a.is_followup)("那上月呢"));
        assert!(!(a.is_followup)("本月销售额是多少"));
        let b: Box<dyn Answerer> = Box::new(a);
        assert_eq!(b.route(), "semantic-cache");
        assert!(crate::ROUTE_LABELS.contains(&b.route()));
    }
}
