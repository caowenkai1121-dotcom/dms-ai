//! T8 搬运：逐行迁自 `server/src/direct.rs`（**只搬不改**，一个字节的行为改动都会让
//! `evaluation.py` 的逐题结果集对拍失去意义）。顺序即行为，只提取不重排。

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;

use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};

use crate::fastpath::*;
use crate::compose::*;
use crate::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use crate::{DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation};

// 同批搬来的兄弟模块（原文件里是同一个作用域，拆文件后要显式引）
#[allow(unused_imports)]
use crate::compose::{assemble::*, metric::*, path::*, values::*};
#[allow(unused_imports)]
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, stock::*, template::*};

use crate::sales_fact;

pub fn sales_fact_metric_extra_words(metric: crate::sales_fact::Metric) -> &'static [&'static str] {
    use crate::sales_fact::Metric;
    match metric {
        Metric::SalesAmount => &["销售金额"],
        Metric::RevenueExcludingTax => &["收入"],
        Metric::GrossProfit => &["毛利"],
        _ => &[],
    }
}





pub fn sales_fact_dimension_extra_words(
    dimension: crate::sales_fact::Dimension,
) -> &'static [&'static str] {
    use crate::sales_fact::Dimension;
    match dimension {
        Dimension::Month => &["趋势", "走势"],
        _ => &[],
    }
}





pub fn longest_sales_fact_word(
    question: &str,
    name: &'static str,
    aliases: &'static [&'static str],
    extras: &'static [&'static str],
) -> Option<&'static str> {
    std::iter::once(name)
        .chain(aliases.iter().copied())
        .chain(extras.iter().copied())
        .filter(|word| question.contains(*word))
        .max_by_key(|word| word.chars().count())
}





pub fn warehouse_sales_metrics(
    question: &str,
) -> Vec<(crate::sales_fact::Metric, &'static str)> {
    use crate::sales_fact;
    let mut candidates = sales_fact::METRICS
        .iter()
        .copied()
        .filter_map(|metric| {
            longest_sales_fact_word(
                question,
                metric.name(),
                metric.aliases(),
                sales_fact_metric_extra_words(metric),
            )
            .map(|word| (metric, word))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, word)| std::cmp::Reverse(word.chars().count()));

    let mut selected: Vec<(sales_fact::Metric, &'static str)> = vec![];
    for candidate in candidates {
        if selected.iter().any(|(_, word)| word.contains(candidate.1)) {
            continue;
        }
        selected.push(candidate);
    }
    // word 上游（`longest_sales_fact_word`）已按 `contains` 筛过，`find` 恒 Some；
    // 兜底 MAX 是防御写法（上游筛法若改，未知位置排最后），不会真取到
    selected.sort_by_key(|(_, word)| question.find(*word).unwrap_or(usize::MAX));
    selected
}





pub fn warehouse_sales_dimensions(
    question: &str,
) -> Vec<(crate::sales_fact::Dimension, &'static str)> {
    use crate::sales_fact;
    const RELIABLE: &[sales_fact::Dimension] = &[
        sales_fact::Dimension::OrderDate,
        sales_fact::Dimension::CustomerCode,
        sales_fact::Dimension::Customer,
        sales_fact::Dimension::SkuCode,
        sales_fact::Dimension::Goods,
        sales_fact::Dimension::WarZone,
        sales_fact::Dimension::Region,
        sales_fact::Dimension::Month,
    ];
    let mut candidates = RELIABLE
        .iter()
        .copied()
        .filter_map(|dimension| {
            longest_sales_fact_word(
                question,
                dimension.name(),
                dimension.aliases(),
                sales_fact_dimension_extra_words(dimension),
            )
            .map(|word| (dimension, word))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, word)| std::cmp::Reverse(word.chars().count()));

    let mut selected: Vec<(sales_fact::Dimension, &'static str)> = vec![];
    for candidate in candidates {
        if selected.iter().any(|(_, word)| word.contains(candidate.1)) {
            continue;
        }
        selected.push(candidate);
    }
    selected.sort_by_key(|(dimension, word)| {
        let time_axis = !matches!(dimension, sales_fact::Dimension::Month | sales_fact::Dimension::OrderDate);
        // 同上：`find` 恒 Some（word 上游按 `contains` 筛过），MAX 只是防御兜底
        (time_axis, question.find(*word).unwrap_or(usize::MAX))
    });
    selected
}





pub fn warehouse_order_count_question(question: &str) -> bool {
    const WORDS: &[&str] = &[
        "订单数", "订单量", "单量", "多少单", "多少个订单", "多少订单", "几个订单",
        "几单", "订单笔数", "客单价", "订单号", "单号",
    ];
    WORDS.iter().any(|word| question.contains(word))
}





pub fn warehouse_sales_question(question: &str) -> bool {
    !warehouse_sales_metrics(question).is_empty() || warehouse_order_count_question(question)
}





pub const WAREHOUSE_SALES_UNSUPPORTED: &[&str] = &[
    "品牌", "牌子", "门店", "店铺", "终端", "店号", "门店编码", "门店名称",
    "客户分类", "客户类别", "客户类型", "业务员", "销售员", "负责人", "区域经理",
    // 「manger」是当年拼错的收录；补上正确的「manager」同档拦（多拦一类问句进失败关闭卡）
    // 「省份」已从本清单移除：业务确认它=省区（region 字段），现由 Region 别名接管
    // （2026-08-11 裁决：「销售额按省份」必须答，不许再跌进 ODS 推导被营销通表截胡）。
    "大区经理", "大区负责人", "经理", "manger", "manager", "商品分类",
    "商品类型", "二级分类", "末级分类", "品类", "TYPE", "销售类型", "城市",
    "价格组", "来源订单类型", "订单类型", "订单", "退货", "发货", "出库", "物流", "应收",
    "损益", "财务",
];





pub fn warehouse_sales_unsupported_semantic(question: &str) -> Option<&'static str> {
    if warehouse_order_count_question(question) {
        return Some("订单口径");
    }
    WAREHOUSE_SALES_UNSUPPORTED
        .iter()
        .copied()
        .find(|word| question.contains(word))
        .or_else(|| {
            ((question.contains("最近") || question.contains("过去") || question.contains('近'))
                && question.contains("季度"))
                .then_some("滚动季度")
        })
}





pub fn warehouse_sales_has_unsupported_semantics(question: &str) -> bool {
    warehouse_sales_unsupported_semantic(question).is_some()
}





pub fn sales_fact_unavailable(
    question: &str,
    unsupported: Option<&'static str>,
    reason: &str,
    advice: &str,
) -> Option<DirectHit> {
    let metrics = warehouse_sales_metrics(question);
    if metrics.is_empty() {
        return None;
    }
    let names = metrics
        .iter()
        .map(|(metric, _)| metric.name())
        .collect::<Vec<_>>()
        .join("、");
    let requested = unsupported.unwrap_or("当前业务数据源");
    Some(hit(
        format!(
            "SELECT '不可计算' AS `数据状态`, '{names}' AS `指标`, \
                    '{requested}' AS `未确认范围`, '{reason}' AS `原因`, \
                    '{advice}' AS `处理建议` \n             FROM dms_ods.t_dict_value LIMIT 1"
        ),
        "direct-doc",
    ))
}





pub fn warehouse_sales_time_bounds(question: &str) -> Option<(String, String)> {
    crate::sales_fact::question_time_bounds(question)
}





pub fn sales_fact_sql(
    metrics: &[crate::sales_fact::Metric],
    dimensions: &[crate::sales_fact::Dimension],
    begin: &str,
    end: &str,
    predicates: &[crate::sales_fact::Predicate],
    sort: Option<crate::sales_fact::Sort>,
    limit: Option<u32>,
) -> String {
    use crate::sales_fact::{self, QueryOptions};
    sales_fact::aggregate_sql_with_options(
        metrics,
        dimensions,
        begin,
        end,
        QueryOptions { predicates, sort, limit },
    )
}





pub fn intent_time_surface(question: &str) -> Option<String> {
    if let Some(surface) = dms_kernel::nl::time::time_phrase_of(question) {
        return Some(surface.to_string());
    }
    let dates = question
        .as_bytes()
        .windows(10)
        .enumerate()
        .filter_map(|(at, bytes)| {
            // 🔴 两头必须是数字（2026-08-14 实测）：chrono 的 `%Y` 会**跳过前导空白**，
            // 于是 `" 2026-08-1"` 也解析成功 —— 窗口整体左移一位，取出来的跨度少一个字符。
            // 「2026-08-10 至 2026-08-11」被截成「2026-08-10 至 2026-08-1」，
            // 带前缀时更是只剩「 2026-08-10」（B01W 的时间槽因此永远兑现不了）。
            if !bytes[0].is_ascii_digit() || !bytes[9].is_ascii_digit() {
                return None;
            }
            let value = std::str::from_utf8(bytes).ok()?;
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .map(|_| (at, value))
        })
        .take(2)
        .collect::<Vec<_>>();
    if let [(start, _), (end, _)] = dates.as_slice() {
        return question.get(*start..end + 10).map(str::to_string);
    }
    // 「2026年6月」「2026年6月15日」这一族：`time_phrase_of` 见到 "20" 就返 None（它只认相对词），
    // 两个 ISO 日期那支也不匹配 —— 而这正是运营看板最常见的问法。
    // 🔴 必须在**整句兜底之前**：兜底返回的是整个问句，拿它当「已消化」会把
    // 「长沙」这种没处理的限定一起吞掉（实测把一道该拒的题变成了绿灯）。
    if let Some(surface) = year_month_surface(question) {
        return Some(surface);
    }
    dms_kernel::nl::time::time_predicate(question).map(|_| question.to_string())
}

/// 显式年月/年月日的表面词（纯函数）。`2026年6月` / `2026年6月15日`；没有则 `None`。
fn year_month_surface(question: &str) -> Option<String> {
    let chars: Vec<char> = question.chars().collect();
    let year_at = (0..chars.len().saturating_sub(4))
        .find(|&i| chars[i..i + 4].iter().all(char::is_ascii_digit) && chars.get(i + 4) == Some(&'年'))?;
    let mut end = year_at + 5;
    let mut take_digits_then = |unit: char, end: &mut usize| {
        let digits = (*end..chars.len()).take_while(|&i| chars[i].is_ascii_digit()).count();
        if digits > 0 && chars.get(*end + digits) == Some(&unit) {
            *end += digits + 1;
            true
        } else {
            false
        }
    };
    if !take_digits_then('月', &mut end) {
        return None; // 只有年份不算一个时间窗表面词（「2026年」交给既有分支）
    }
    take_digits_then('日', &mut end);
    Some(chars[year_at..end].iter().collect())
}





/// 单省问法（周报主查询形态：`山东省 2026-08-10 至 2026-08-11 销售额`）
/// → sales_fact.region 的已知存储形态。
///
/// 只认唯一省名；省名若只是已探明客户名的一部分则不解释为区域。无法唯一解释时返回 Err，
/// 调用方必须 fail closed，不能把省份限定丢掉后查询全国数据。
pub fn sales_fact_province_filter(
    question: &str,
    customer: Option<&str>,
) -> Result<Option<(String, crate::sales_fact::Predicate)>, ()> {
    use crate::sales_fact::{Dimension, Predicate};

    // 用户直接说出 region 的取值本身（「西北大区」「线下私域」…）：这一支在省名扫描
    // **之前**，因为「川渝藏大区」里含「川」这类省名字样，先扫省名会把它拆错。
    // 出现两个不同取值 = 多区域比较，与多省同一纪律：不猜、不静默放宽成全国。
    {
        let mut direct: Option<&'static str> = None;
        for value in crate::warehouse_catalog::DIRECT_REGION_VALUES {
            if !question.contains(*value) {
                continue;
            }
            if direct.is_some_and(|existing| existing != *value) {
                return Err(());
            }
            direct = Some(value);
        }
        if let Some(value) = direct {
            let predicate = Predicate::one_of(Dimension::Region, &[value]).expect("固定非空");
            return Ok(Some((value.to_string(), predicate)));
        }
    }

    let mut hit: Option<(&'static str, String)> = None;
    for &(_code, name) in crate::present::PROVINCE_LABELS {
        if !question.contains(name) || customer.is_some_and(|entity| entity.contains(name)) {
            continue;
        }
        // 长形态在前，确保「广西壮族自治区」不会只消化成「广西」。同时容忍用户常见的
        // “省/市/自治区”写法；裸省名仅在残留守卫确认其余限定都已兑现后才会放行。
        let phrases = [
            format!("{name}壮族自治区"),
            format!("{name}回族自治区"),
            format!("{name}维吾尔自治区"),
            format!("{name}特别行政区"),
            format!("{name}自治区"),
            format!("{name}省"),
            format!("{name}市"),
            name.to_string(),
        ];
        let Some(phrase) = phrases.into_iter().find(|phrase| question.contains(phrase)) else {
            continue;
        };
        if hit.as_ref().is_some_and(|(existing, _)| *existing != name) {
            return Err(()); // 多省比较不是单省过滤；不猜、不静默放宽成全国。
        }
        hit = Some((name, phrase));
    }

    let Some((name, consumed)) = hit else {
        return Ok(None);
    };
    let business_region = crate::warehouse_catalog::shop_business_region_for_province(name)
        .ok_or(())?;
    let conventional_region = format!("{name}省区");
    if business_region != conventional_region {
        // 🔴 省区与行政省**不是一回事**（2026-08-15 生产直打逮到的一整族倍数级错答）：
        //   海南省 → region='广东省区'（含广东 494.8 万 + 海南 46.1 万）→ 高估 11.7 倍
        //   上海市 → region='浙江省区'                                  → 高估 3.8 倍
        //   西藏   → region='川渝藏大区'（真值 0）                      → 凭空 419 万
        //   新疆   → region='西北大区'                                  → 高估 4.3 倍
        // 而且全都 trust=verified、caliber_note 为空 —— 用户没有任何途径察觉。
        //
        // 映射**不是 1:1** 的那一档，region 是行政省的**超集**，拿它当过滤必然多算。
        // 这一档改用 `state`（38 个官方全称取值）精确过滤。
        //
        // 1:1 的那一档（山东省 → 山东省区）**照旧走 region 四形候选**：
        // 那是 2026-08-11 业务裁决的口径（「省份」≡「省区」≡ region），实测两侧同值
        // （山东省本月 7,126,980.40 = 山东省区本月），B01W 钉的也是那个形态。
        // 本改动只治「超集冒充精确值」，不动分组口径。
        //
        // `contains` 而不是 `eq`：`PROVINCE_LABELS` 给的是短名（海南/新疆/内蒙古），
        // 而 state 存官方全称（海南省/新疆维吾尔自治区/内蒙古自治区）。短名是全称的
        // 唯一前缀，省名之间互不为子串（河南≠海南≠湖南、山西≠陕西），INSTR 不会误中。
        let predicate = Predicate::contains(Dimension::State, name);
        return Ok(Some((consumed, predicate)));
    }
    let values = vec![
        format!("{name}省区"),
        format!("{name}战区"),
        format!("{name}大区"),
        name.to_string(),
    ];
    let refs = values.iter().map(String::as_str).collect::<Vec<_>>();
    let predicate = Predicate::one_of(Dimension::Region, &refs).expect("省区候选固定非空");
    Ok(Some((consumed, predicate)))
}







#[cfg(test)]
mod range_surface_tests {
    use super::*;

    /// 用户直接说出 region 的取值本身：确定性路必须接住，不许掉进自由 SQL。
    /// 「川渝藏大区」含「川」这类省名字样，判据必须在省名扫描**之前**。
    #[test]
    fn a_region_value_spoken_verbatim_is_a_region_filter() {
        for (q, want) in [
            ("本月西北大区销售额", "西北大区"),
            ("川渝藏大区本月销售额", "川渝藏大区"),
            ("线下私域本月销售额", "线下私域"),
            ("海外事业部本月销售额", "海外事业部"),
        ] {
            let (consumed, predicate) =
                sales_fact_province_filter(q, None).expect("不该 Err").expect("该认出来");
            assert_eq!(consumed, want, "{q}");
            let sql = format!("{predicate:?}");
            assert!(sql.contains(want), "{q} → {sql}");
        }
        // 省名照旧走四形候选（不许被这条改动带偏）
        let (_, p) = sales_fact_province_filter("山东省本月销售额", None).unwrap().unwrap();
        let sql = format!("{p:?}");
        assert!(sql.contains("山东省区") && sql.contains("山东战区"), "{sql}");
        // 两个不同大区 = 多区域比较，不猜
        assert!(sales_fact_province_filter("西北大区和线下私域本月销售额", None).is_err());
        // 未登记的大区名照旧不认（让它去走「未确认限定」）
        assert!(sales_fact_province_filter("华东区本月销售额", None).unwrap().is_none());
    }

    /// 两个 ISO 日期的区间：跨度必须**整段**取出来。
    /// chrono 的 `%Y` 跳过前导空白 → `" 2026-08-1"` 也解析成功，窗口左移一位，
    /// 此前把「2026-08-10 至 2026-08-11」截成「…至 2026-08-1」，带中文前缀时
    /// 更是只剩「 2026-08-10」—— 时间槽因此永远兑现不了（生产回归 B01W）。
    #[test]
    fn an_explicit_date_range_surface_covers_both_ends() {
        for q in [
            "2026-08-10 至 2026-08-11 销售额",
            "山东省 2026-08-10 至 2026-08-11 销售额",
        ] {
            assert_eq!(
                intent_time_surface(q).as_deref(),
                Some("2026-08-10 至 2026-08-11"),
                "{q}"
            );
        }
        // 单个日期不走这一支（交给相对词/年月/整句兜底），不许被这条改动带偏
        assert_eq!(intent_time_surface("本月销售额").as_deref(), Some("本月"));
        assert_eq!(intent_time_surface("2026年6月销售额").as_deref(), Some("2026年6月"));
    }
}
