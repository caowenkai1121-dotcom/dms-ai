//! T8 搬运：逐行迁自 `server/src/direct.rs`（**只搬不改**，一个字节的行为改动都会让
//! `evaluation.py` 的逐题结果集对拍失去意义）。顺序即行为，只提取不重排。

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;

use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};

use crate::compose::*;
use crate::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use crate::{DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation};

// 同批搬来的兄弟模块（原文件里是同一个作用域，拆文件后要显式引）
#[allow(unused_imports)]
use crate::compose::{assemble::*, metric::*, values::*};
#[allow(unused_imports)]
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*};

use crate::sales_fact;

/// 维度表达式是不是「按时间分组」。判据是**日期函数名**，不是列名 ——
/// 列名判不出来（`order_time` / `after_sales_time` / `created_time` 没有统一后缀，
/// 而 `DATE_FORMAT`/`YEAR`/`MONTH`/`QUARTER`/`DATE` 是 SQL 侧有限的几个）。
///
/// 判宽的代价：多拒一条本来能装的（回落 LLM，不出错数）。
/// 判窄的代价：装出一条按错表的时间列分组的 SQL，且确定性路不跑口径校验 —— 不可接受。
pub fn is_time_expr(expr: &str) -> bool {
    const F: &[&str] = &[
        "date_format", "year(", "month(", "quarter(", "week(", "date(",
        "to_char", "date_trunc", "extract(",
    ];
    let low = expr.to_lowercase();
    F.iter().any(|f| low.contains(f))
}


pub fn rank_direction(question: &str) -> &'static str {
    // 🔴「最差」必须在这里：认不出来就返回 DESC ——「卖得最差的 3 个商品」会**确定性地**
    // 给出卖得最好的三个。飘着的失败还能被用户发现，确定的答反不会（2026-08-13 审计）。
    if ["最少", "最小", "最低", "最差"].iter().any(|word| question.contains(word)) {
        "ASC"
    } else {
        "DESC"
    }
}


pub fn ranking_limit(question: &str) -> usize {
    // 「最低/最差」已进 `nl::time` 的最高级词表，不再需要在这里换词绕一道。
    detect_top_n(question)
}


/// 从注册表里挑**最具体**的那一条：命中词最长者胜，等长时按名字定序。
///
/// 为什么不能用原来的 `find`（第一条命中的）：`load_dimensions` 没有 `ORDER BY`，
/// 返回序就是 PG 的物理行序，一次种子重灌（UPSERT 重写行）或 VACUUM 都会改它。
/// 同一个问句常同时命中两条 ——「按客户分类」既命中维度 `客户` 也命中 `客户分类`：
/// 赢家是 `客户` 时残留「分类」被 `has_entity_residue` 拦下、回落 LLM；
/// 赢家是 `客户分类` 时才装配正确。**也就是说回归 E17 只是碰巧绿的**，
/// 无代码变更就可能翻红。同理「区域经理」会被业务员的别名「经理」遮蔽。
/// 长词更具体这条判据与 `kernel::nl::text::match_word` 同源（那边是同一元素内选别名）。
/// 返回值连同**命中词**一起带出（同一个 `match_word` 算的）——调用点拿它做减词，别再算一遍。
pub fn pick<'a, T>(
    question: &str,
    defs: &'a [T],
    of: impl Fn(&'a T) -> (&'a String, &'a Vec<String>),
) -> Option<(&'a T, String)> {
    // `taken` 空串 = 不减词，逐字等于本函数原来的行为（指标侧就是这么调的）
    pick_inner(question, defs, of, "")
}


/// `pick` 的**减词**版：`taken`（指标的命中词）已经消化掉的词不再算维度命中。
///
/// 🔴 实证错答（审计 二·AS1，用户零报错拿到 200 行客户名单）：
/// ```text
/// ✅ 本月成交客户数是多少   direct-agg  列=[成交客户数]        1 行   1625
/// ❌ 上周成交客户数是多少   direct-agg  列=[客户, 成交客户数] 200 行  发员工福利样品使用
/// ❌ 去年成交客户数是多少   direct-agg  列=[客户, 成交客户数] 200 行  线下-怀化市雪丰食品有限公司
/// ```
/// 根因：`pick(metrics)` 与 `pick(dims)` **各判一次、互不减词** —— 「成交客户**数**」里的
/// 「客户」被再次当成维度命中，而残留守卫剥完指标名+维度名后正好为空，于是一路绿灯。
/// route 仍是 `direct-agg`、`caliber_note` 为空，**只断言 route 的测试看不出来**。
///
/// 判据与 `value_filters` 那条子串门**同形**（只是这里的长词来自指标而不是注册表值名），
/// 且刻意收窄成两条同时成立：
/// ① 维度命中词是指标命中词的**真子串**（「本月销售额按客户」的「客户」不是「销售额」的
///    子串 → 真维度，不许误杀）；
/// ② 该词在问句里**只出现在指标命中词内部**（「各客户成交客户数」的「客户」在指标词外还有
///    一次 → 用户真要分组，照旧当维度）。
/// 减不掉时的失败方向是安全的：维度被减光 → 装配器走无维度模式或被残留守卫拦下回落 LLM。
pub fn pick_excluding<'a, T>(
    question: &str,
    defs: &'a [T],
    of: impl Fn(&'a T) -> (&'a String, &'a Vec<String>),
    taken: &str,
) -> Option<&'a T> {
    pick_inner(question, defs, of, taken).map(|(d, _)| d)
}


/// 选取核心：返回（选中的定义, 它的命中词）。`pick` 连词一起要（调用点拿去减词，
/// 不用对同一指标再算一遍 `match_word`）；`pick_excluding` 只要定义。
pub fn pick_inner<'a, T>(
    question: &str,
    defs: &'a [T],
    of: impl Fn(&'a T) -> (&'a String, &'a Vec<String>),
    taken: &str,
) -> Option<(&'a T, String)> {
    // `taken` 为空时 `contains` 恒 false（w 非空），`pseudo` 在第二条件就短路；
    // 整句 replace 只在这里做一次（原来闭包对每个维度候选词都重新分配一遍）
    let without_taken = question.replace(taken, "");
    let pseudo = |w: &str| w != taken && taken.contains(w) && !without_taken.contains(w);
    defs.iter()
        .filter_map(|d| {
            let (name, aliases) = of(d);
            dms_kernel::nl::text::match_word(question, name, aliases)
                .filter(|w| !pseudo(w))
                .map(|w| ((w.chars().count(), name.as_str()), w, d))
        })
        .max_by_key(|(k, _, _)| *k)
        .map(|(_, w, d)| (d, w))
}


/// 指标已消化的命中词。生产路径直接用 `pick` 带回来的词；本函数留给单测构造
/// `pick_excluding` 的 `taken`（与 `pick` 同一个 `match_word`，自己再判一遍就会漂）。
/// T8 搬运后本项被 `server/src/direct.rs` 的测试跨 crate 使用：`#[cfg(test)]` 在下游测试里
/// 不可见（它只在本 crate 自测时编译），故去掉该门。lib 的 `pub` 项不会触发 never-used 告警，
/// 原注释担心的那件事不会发生。
pub fn metric_word(question: &str, m: &MetricDef) -> String {
    dms_kernel::nl::text::match_word(question, &m.name, &m.aliases).unwrap_or_default()
}


pub fn metric_dimension_allowed(
    policies: &[crate::registry::model::MetricPolicy], metric: &str, dimension: &str,
) -> bool {
    dimension.is_empty() || policies.iter().find(|p| p.name == metric).is_some_and(|p| {
        p.allowed_dimensions.iter().any(|d| d == "*" || d == dimension)
    })
}


/// 表名比较的唯一判据。注册表/声明/拼出来的 FROM 串都可能带大小写漂移 ——
/// 一处用 `==`，漂移时就是「路径找不到 / 表级口径漏挂」（后者正是 41% 虚增的失败面）。
pub fn table_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}


/// BFS 找 metric 基表 → 维度驱动表 的最短 join 路径（≤3 跳）。返回 hop 序列。
pub fn find_path<'a>(
    from: &str,
    to: &str,
    edges: &'a [JoinEdge],
) -> Option<Vec<(String, String, String, bool)>> {
    // hop = (to_table, to_col, from_col, fanout)
    if table_eq(from, to) {
        return Some(vec![]);
    }
    let mut queue: std::collections::VecDeque<(String, Vec<(String, String, String, bool)>)> =
        std::collections::VecDeque::new();
    let mut visited = std::collections::HashSet::new();
    queue.push_back((from.to_string(), vec![]));
    visited.insert(from.to_string());
    while let Some((cur, path)) = queue.pop_front() {
        if path.len() >= 3 {
            continue;
        }
        // 注册表边数很小（几十条），每层全表扫 + 每点一次克隆足够，别为省克隆绕弯
        for e in edges {
            let (next, to_col, from_col, fanout) = if table_eq(&e.lt, &cur) {
                (e.rt.clone(), e.rc.clone(), e.lc.clone(), e.card == "1:N")
            } else if table_eq(&e.rt, &cur) {
                (e.lt.clone(), e.lc.clone(), e.rc.clone(), e.card == "N:1")
            } else {
                continue;
            };
            if visited.contains(&next) {
                continue;
            }
            let mut p = path.clone();
            p.push((next.clone(), to_col, from_col, fanout));
            if table_eq(&next, to) {
                return Some(p);
            }
            queue.push_back((next.clone(), p));
            visited.insert(next);
        }
    }
    None
}


/// 找两表间的直接边（时间桥用）
pub fn find_edge<'a>(a: &str, b: &str, edges: &'a [JoinEdge]) -> Option<(&'a JoinEdge, bool)> {
    // 返回 (edge, a_is_left)
    edges.iter().find_map(|e| {
        if table_eq(&e.lt, a) && table_eq(&e.rt, b) {
            Some((e, true))
        } else if table_eq(&e.rt, a) && table_eq(&e.lt, b) {
            Some((e, false))
        } else {
            None
        }
    })
}

