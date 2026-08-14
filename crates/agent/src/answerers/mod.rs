//! `Answerer` trait + Router 有序表。变更原因＝「谁有资格出手、按什么顺序」。
//!
//! 逐条转写 `server/src/pipeline.rs:643-711`（`ask_single` 里 graph / compose / fastpath /
//! cache / llm 五支内联 if）。**加一种能力＝加一个 `Answerer`**，不是往那 258 行（拆分时点行数）里再塞一支 if。
//!
//! ## 纪律（全部来自 crate 文档，逐条落实）
//! - **顺序即行为**：`[graph, compose, fastpath, cache, llm]` 一位都不许换。compose 与 fastpath
//!   互换会让「销售额按省份」走另一条装配、生成完全不同的 SQL。
//! - **权限门禁在 `accept`，不在 `answer`**：`compose`/`fastpath` 的 `accept` 恒真（今天无权限门禁，
//!   裁决 二·C 推翻了给它们加 `is_unrestricted` 的想法 —— `regression_cases.json` D01/D03
//!   断言 `route=direct-agg`，加门禁当场红），只有 `graph` 带 unrestricted 门禁。
//! - **知识库 answerer 不进 Router**：由 triage 直接分派。进链会让文档问句在没命中时
//!   回落到 SQL 生成，破不变量 I5（外部文本永不成为指令）。
//! - `route()` 只是**表标签**（日志与本文件的自检用）；真正写进 `AskResult.route` 的是**命中方**给的值
//!   （`hit.route` 可能是 `direct-agg` 也可能是 `direct-doc`，llm 路径还会变成 `llm+repair`/`llm+schema-fix`）。
//!   混用即 26 题 `direct-agg` + 3 题 `graph` 的回归断言全红。

// 七个成员。真 Router 在 `ask::router()`（它要注入 embed 与三个确定性命中回调）。
pub mod cache;
pub mod business_lookup;
pub mod entity;
pub mod fastpath_intent;
#[cfg(test)]
pub mod fastpath_tests;
pub mod graph;
pub mod hits;
pub mod knowledge;

use dms_kernel::BoxFut;

use crate::ctx::{AskCtx, AskResult};

/// 路由表的一个成员。
///
/// 异步方法手写 `BoxFut` 而非引 `async-trait`（铁律 3 零新增依赖）：原生 `async fn in trait`
/// 至今**不是 dyn 兼容的**（lint `async_fn_in_dyn_trait`；toolchain 是浮动 stable，版本论断无法复核），
/// 而路由表就是
/// `Vec<Box<dyn Answerer>>`。同一写法已在 `dms_connector::source::SqlSource` 与
/// `dms_kernel::ChatModel` 用过两次，这里第三次沿用。
///
/// 调用约定（`ask.rs` 的分派循环照此写，别把 `accept` 漏掉 —— 那等于把权限门禁绕过）：
/// ```text
/// for a in router {
///     if !a.accept(&cx) { continue; }
///     if let Some(r) = a.answer(&cx).await? { return Ok(r); }   // Err 原样上抛：fail-closed 不许降级成下一路
/// }
/// ```
pub trait Answerer: Send + Sync {
    /// 表标签（**只用于日志与 Router 自检**，真正写进 `AskResult.route` 的是命中方给的值）
    fn route(&self) -> &'static str;

    /// 该成员是否有资格出手。**权限门禁在这里，不在 `answer` 里**。
    /// 同步且不做 IO：graph/cache/entity/business-lookup 等成员只做资格判断，
    /// compose/fastpath 恒真；没有一个门禁需要 await。
    fn accept(&self, cx: &AskCtx<'_>) -> bool;

    /// `Ok(None)` = 我没接住，交给下一个；`Err` = **原样上抛**
    /// （权限注入失败 fail-closed 绝不降级成「换下一路重试」，见 `gate::is_guard_err` 的分类）。
    fn answer<'a>(&'a self, cx: &'a AskCtx<'a>) -> BoxFut<'a, anyhow::Result<Option<AskResult>>>;
}

/// `AskResult.route` 的全部合法取值。26 题断言 `direct-agg`、3 题断言 `graph`，混用即全红。
pub const ROUTE_LABELS: &[&str] = &[
    "direct-agg",
    "direct-doc",
    // 合同未覆盖时的 ODS 推导降级（server direct.rs 的 direct-doc 成员产出）：不是 Router
    // 成员标签，但会写进 AskResult.route / query_log.route —— 审计靠它把「推导口径·
    // 未经合同验证」的答案与合同答案分开。
    "direct-derive",
    "entity-card",
    "business-lookup",
    "graph",
    "semantic-cache",
    "llm",
    "llm+repair",
    "llm+schema-fix",
    "compound",
];

/// Router 的成员顺序（各成员的 `route()` 表标签）。**顺序即行为，一位都不许换。**
/// `entity-card` 在 direct-doc 之后、semantic-cache 之前：裸名称先吃确定性快路径
/// （单号直查可能恰好是名字形），被实体卡接住之前不许被缓存抢走（缓存复用别人的 SQL）。
pub const ROUTER_ORDER: &[&str] = &[
    "graph",
    "direct-agg",
    "direct-doc",
    "entity-card",
    "business-lookup",
    "semantic-cache",
    "llm",
];

// 这里曾有一个 `default_router() -> vec![]`：地基阶段的占位，成员逐个落地后**没人再用它**
// （真 Router 是 `ask::router()`，它要注入 embed 与三个确定性命中回调，造不出无参版本）。
// 删掉它是因为 `route_label_map` 当时拿它取标签 —— 空表让那条子序列断言**恒真**，
// 守着空气比没有守卫更坏。顺序契约现由 `ask::router_is_the_contract_in_full` 逐字守
// （它断言七个标签与 `ROUTER_ORDER` 全等，`llm` 也在表内）。

#[cfg(test)]
mod tests {
    use super::*;

    /// `labels` 是否为 `ROUTER_ORDER` 的**子序列**：一次同时守住三件事 ——
    /// 标签在白名单内（不在则不可能出现在 ROUTER_ORDER 里）、无重复（ROUTER_ORDER 无重复项，
    /// 重复标签必然匹配不上第二次）、顺序不变（换位即失配）。
    /// 成员逐个落地的过程中它也成立，所以这条断言从今天起就有效，不用等都到齐。
    fn is_subsequence(labels: &[&str]) -> bool {
        let mut it = ROUTER_ORDER.iter();
        labels.iter().all(|l| it.any(|o| o == l))
    }

    /// 🔴 「加成员忘了对齐标签」的唯一防线。
    #[test]
    fn route_label_map() {
        // ① 表标签全在白名单内，且 ROUTER_ORDER 自身无重复
        for l in ROUTER_ORDER {
            assert!(ROUTE_LABELS.contains(l), "表标签不在白名单内：{l}");
            assert_eq!(ROUTER_ORDER.iter().filter(|x| *x == l).count(), 1, "表标签重复：{l}");
        }
        // ② 顺序是契约（compose 必须先于 fastpath，graph 必须最先，llm 必须兜底；
        //    entity-card 在 doc 后、cache 前 —— 裸名称不许被缓存抢走）
        assert_eq!(
            ROUTER_ORDER,
            &["graph", "direct-agg", "direct-doc", "entity-card", "business-lookup", "semantic-cache", "llm"]
        );
        // ③ 真 Router 的标签由 `ask::router_is_the_contract_in_full` 逐字守
        //    （它能造出带 embed 与三个回调的真表；本文件造不出，原先拿一个恒返空表的
        //    `default_router()` 顶替，那让子序列断言恒真 —— 已删，见文件中段的说明）。
        // ④ 判据不是哑测试：漏项可以，换位与重复必须红（守卫搬家最容易变成「永远绿」，裁决 二·F F2）
        assert!(is_subsequence(&["graph", "llm"]));
        assert!(!is_subsequence(&["llm", "graph"]), "换位必须红");
        assert!(!is_subsequence(&["graph", "graph"]), "重复必须红");
        assert!(!is_subsequence(&["knowledge"]), "白名单外的标签必须红");
    }
}

#[cfg(test)]
mod order_scope_drift {
    /// 🔴 有效订单口径不许出现第 9 处**不同形**的散写。
    ///
    /// 由来：这串过滤在手工模板里散写了 8 处，而装配器路径读的是 `meta.table_scope`。
    /// 运营侧新增一个作废状态码时，装配器当天自愈、手工模板继续把作废单算进订单数 ——
    /// 同一个「订单数」两个答案（2026-08-13 审计）。
    ///
    /// 这条判据只保证**所有副本逐字相同**（漂了就红）；让模板真正去读声明是 T8 之后的
    /// 独立一笔（要改行为，必须连库跑 evaluation.py）。
    #[test]
    fn no_inline_order_scope_literals() {
        const SOURCES: &[(&str, &str)] = &[
            ("entity.rs", include_str!("entity.rs")),
            ("fastpath_intent.rs", include_str!("fastpath_intent.rs")),
        ];
        let canon = dms_semantic::sales_fact::ORDER_SCOPE;
        let core = canon.split("NOT IN ").nth(1).expect("口径常量形状变了");
        for (name, src) in SOURCES {
            for (i, line) in src.lines().enumerate() {
                if !line.contains("order_status NOT IN") {
                    continue;
                }
                assert!(
                    line.contains(core),
                    "{name}:{} 的有效订单口径与 sales_fact::ORDER_SCOPE 漂了：{line}",
                    i + 1
                );
            }
        }
    }
}
