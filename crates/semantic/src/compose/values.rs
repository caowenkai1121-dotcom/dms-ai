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
use crate::compose::{assemble::*, metric::*, path::*};
#[allow(unused_imports)]
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*};

use crate::sales_fact;

/// 问句里**能被唯一一条码值声明解释**的值过滤。
///
/// 为什么必须「唯一」：`meta.value_map` 实测 936 行 / 82 列，其中 **109 个名字跨 ≥2 个
/// (表, 列)** —— 「湖南虎家食品科技有限公司」这类公司名在十几张表上各有一份 `company_code`。
/// 名字歧义时装配器**不猜**（先例：`code_rules` 对跨两列的码就是跳过）。跳过 = 那个词
/// 没被消化 = 残留守卫照旧把整条拦下回落 LLM，与本特性上线前完全同形。
///
/// 三道早筛：
/// - 名字 < 2 字：单字码值名（「男」）会在任意问句里命中。
/// - 码含 `'` / `\` 或为空：拼进 SQL 字面量会破引号。声明是管理员写的，但破引号这件事
///   不该靠「写声明的人小心」来保证。
/// - 长名吃掉短名：问句「湖南虎家…的销售额」同时含公司名与「湖南」，取最长那个 ——
///   短的是长的一部分，不是另一个限定。
pub fn value_filters<'a>(question: &str, vals: &'a [ValueRef], words: &[String]) -> Vec<&'a ValueRef> {
    // 🔴 歧义判据必须打在**只按名字命中、未经任何其它过滤**的集合上。
    // 若拿下面 `cand`（已被 `match_kind` / 子串门筛过）去判，一个「eq 落在 A 列、like 落在 B 列」
    // 的名字会因为 like 那行被筛掉而**看起来无歧义**，于是装配器挑了 A 列 —— 那正是在猜。
    // （实测当前没有混合 `match_kind` 的同名行，所以这一刀今天不改变任何行为；
    // 它防的是下一条声明写进来的时候。）
    let hits: Vec<&ValueRef> = vals
        .iter()
        // `contains` 才是选择性条件，放前面短路（936 行逐行数字数再 contains 是反的）
        .filter(|v| question.contains(v.name.as_str()) && v.name.chars().count() >= 2)
        .collect();
    let unambiguous = |v: &ValueRef| {
        hits.iter()
            .filter(|o| o.name == v.name)
            .all(|o| o.table == v.table && o.column == v.column && o.code == v.code)
    };
    let cand: Vec<&ValueRef> = hits
        .iter()
        .copied()
        .filter(|v| {
            // 🔴 只认 `eq`。`like` 那 5 行是 `t_sales_order.paid_way`（一单多种支付方式，
            // 列里存的是多值串）—— 对它写 `= '码'` 是**确定性地取错集合**。
            // 拼 `LIKE '%码%'` 也不是顺手能对的事（`ZZ01` 会撞 `ZZ010` 这类前缀），
            // 而当前没有一道题需要它。认不了的 match_kind 就不认 = 那个词照旧是残留 = 回落 LLM。
            unambiguous(v)
                && v.match_kind == "eq"
                && !v.code.trim().is_empty()
                && !v.code.contains('\'')
                && !v.code.contains('\\')
                // 🔴 已被指标/维度消化的词里**包含**这个值名（含相等）→ 它不是值过滤。
                // 实测两条（扫全部 92 道题面得到的**唯一**两个危险命中）：
                // ① 「本月各**业务**员的销售额」：`业务` 唯一命中
                //    `t_customer_contacts_account.contact_type = 1`，而它是维度名「业务员」的子串
                //    —— 认下来就会给一道现在全绿的题桥一张联系人表、加一条毫无关系的过滤；
                // ② 「今年**市场费用**…」：`市场费用` 同时是**指标名**和
                //    `t_customer_balance.balance_type = 3` 的码值名 —— 相等也必须让给指标。
                // 与残留剥离那边「长词先于子串」是同一条原则，只是这里的长词来自注册表。
                && !words.iter().any(|w| w.contains(v.name.as_str()) && question.contains(w.as_str()))
        })
        .collect();
    cand.iter()
        // 不能是另一个命中名字的真子串（长名吃短名：「湖南虎家…」在问句里时不要再单独加「湖南」）
        .filter(|v| !cand.iter().any(|o| o.name != v.name && o.name.contains(v.name.as_str())))
        .copied()
        // 同名同码可能在 value_map 里重复行，去一次重
        .fold(Vec::new(), |mut acc: Vec<&ValueRef>, v| {
            if !acc.iter().any(|o| o.name == v.name) {
                acc.push(v);
            }
            acc
        })
}


/// 值名的**位置性同位语**：紧跟在已命中值名之后的行政区划后缀（湖南**省** / 长沙**市**）。
///
/// 🔴 为什么不进 `STRIP_WORDS`：那张表是全仓共用的**无位置**虚词表，全局剥「省」会吃掉
/// 实体名里的字，而那正是 E16「线下客户被静默丢弃」那类翻车的形态（`lexicon.rs` 里
/// 「只加实测挡住过的、且无实体名风险的词」那条纪律说的就是这个）。而在**紧跟一条已被
/// 声明唯一解释的值名之后**这个位置上，「省」表达不出任何额外限定 —— 它是地名的一部分，
/// `t_customer.province = '430000'` 已经把它兑现完了。位置性 = 不可能放宽全局守卫。
pub const VALUE_APPOSITIVES: &[&str] = &["省", "市", "区", "县"];


pub fn consumed_phrase(question: &str, name: &str) -> String {
    let Some(i) = question.find(name) else {
        return name.to_string();
    };
    let rest = &question[i + name.len()..];
    match VALUE_APPOSITIVES.iter().find(|s| rest.starts_with(**s)) {
        Some(s) => format!("{name}{s}"),
        None => name.to_string(),
    }
}


/// 注册表侧的消化词：指标名/别名 + 维度名/别名。`value_filters` 与残留守卫**共用同一份** ——
/// 各写一份就会漂出「值过滤认下了一个残留守卫按指标消化的词」。
pub fn registry_words(m: &MetricDef, d: &DimDef) -> Vec<String> {
    let mut w: Vec<String> = vec![m.name.clone(), d.name.clone()];
    w.extend(m.aliases.iter().cloned());
    w.extend(d.aliases.iter().cloned());
    w
}


/// 组合器专用：消化词 = 指标名/别名 + 维度名/别名 + 已认下的值过滤名（含位置性同位语）
pub fn has_entity_residue(question: &str, m: &MetricDef, d: &DimDef, vfs: &[&ValueRef]) -> bool {
    let mut words = registry_words(m, d);
    words.push("最低".into());
    words.extend(vfs.iter().map(|v| consumed_phrase(question, &v.name)));
    // 🔴 **不要在这里补「消化显式年份」**：`has_residue_with` 已经把所有 ASCII 数字
    // 过滤掉了（`!c.is_ascii_digit()`），阿拉伯年份**从来就不是残留**。
    // 我加过一段「消化 `explicit_year` 认下的年份」，枪测当场证明它是**死代码**
    // （关掉它测试仍全绿）—— 而死代码比没有更坏：它让读者以为这里有一层保护。
    // 顺带订正 `_DECISIONS` 二·O5a 里那句「STRIP_WORDS 认不出阿拉伯年份 → 残留守卫拦」：
    // 那句是错的。真正会成为残留的是**单位词**（「…是多少**箱**」的「箱」）与实体名。
    has_residue(question, &words)
}

