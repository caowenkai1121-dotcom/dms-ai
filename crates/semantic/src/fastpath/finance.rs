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
use crate::fastpath::{derive::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*};

use crate::sales_fact;

pub const MARKET_COST_GROUPS: &[(&str, &[&str])] = &[
    ("长促督导", &["sup_staff_cost"]),
    ("客户赔偿", &["comp_pkg", "comp_cust_complaint", "comp_logistics", "comp_out_stock", "comp_other"]),
    ("营销物料", &["mat_costume", "mat_card_sign", "mat_freight_install", "mat_ip_goods", "mat_sticker", "mat_tasting_table", "mat_stand_banner", "mat_kt_board", "mat_lightbox", "mat_other", "mat_insulated_bag", "mat_tent"]),
    ("营销设备", &["eq_baking", "eq_other", "eq_freight", "eq_fridge", "eq_sausage"]),
    ("终端费用", &["term_adv_fee", "term_other", "term_entry_barcode", "term_display", "term_display_material"]),
    ("广告费用", &["offline_adv", "brand_adv"]),
    ("活动执行", &["act_other", "act_outsource", "act_tasting_sample", "act_logistics", "act_venue", "act_material_build"]),
    ("客户返利", &["rebate_key_cust", "rebate_fresh_food", "rebate_other"]),
    ("非活动样品", &["not_act_tasting_sample"]),
    ("其他", &["other"]),
];


pub fn market_cost_expr(alias: &str, cols: &[&str]) -> String {
    cols.iter().map(|c| format!("COALESCE({alias}.{c},0)")).collect::<Vec<_>>().join(" + ")
}


pub fn market_cost_where(question: &str) -> String {
    time_predicate(question)
        .map(|p| fill_time_col(&p, "f.data_month"))
        .unwrap_or_else(|| "1 = 1".into())
}


pub fn warehouse_market_cost(question: &str) -> DirectHit {
    let pred = market_cost_where(question);
    let top_n = detect_top_n(question);
    // 裸「前」不是触发词（「目前市场费用」会误中）：「前+N/前十」形态由 `detect_top_n`
    // 带时间单位黑名单判；「top」归一小写后判一次（原来 "top"/"TOP" 两枚举还漏 "Top"）
    let rank = ["最多", "最高", "排行", "排名"].iter().any(|word| question.contains(word))
        || question.to_lowercase().contains("top")
        || top_n < 200;
    let detail = MARKET_COST_GROUPS
        .iter()
        .map(|(name, cols)| format!(
            "SELECT '{name}' AS `费用分类`, COALESCE(SUM({}),0) AS `市场费用` \
             FROM sales_ads.ads_off_sales_cost_customer_dnf f WHERE {pred}",
            market_cost_expr("f", cols),
        ))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let detail = format!("SELECT * FROM ({detail}) x ORDER BY `市场费用` DESC LIMIT {top_n}");
    if rank {
        // 排行问法的主结果就是分类明细：总额/上期不再先拼出来再丢
        return hit(detail, "direct-agg");
    }
    let all_cols: Vec<&str> = MARKET_COST_GROUPS.iter().flat_map(|(_, cols)| cols.iter().copied()).collect();
    let total = format!(
        "SELECT COALESCE(SUM({}),0) AS `市场费用` \
         FROM sales_ads.ads_off_sales_cost_customer_dnf f WHERE {pred}",
        market_cost_expr("f", &all_cols),
    );
    let prev = prev_window(question).map(|(p, label)| {
        let p = fill_time_col(p, "f.data_month");
        (format!(
            "SELECT COALESCE(SUM({}),0) AS `市场费用` \
             FROM sales_ads.ads_off_sales_cost_customer_dnf f WHERE {p}",
            market_cost_expr("f", &all_cols),
        ), label.to_string())
    });
    DirectHit { prev, detail: Some(detail), ..hit(total, "direct-agg") }
}


pub fn warehouse_invoice_unavailable() -> DirectHit {
    hit(
        "SELECT '不可计算' AS `数据状态`, '开票金额' AS `指标`, \
                     '当前业务数仓未同步DMS开票事实表，不能安全计算' AS `原因`, \
                     '请切换到含开票申请事实的业务库，或先补齐数仓同步' AS `处理建议` \
              FROM dms_ods.t_dict_value LIMIT 1".into(),
        "direct-doc",
    )
}


pub fn warehouse_account_bill_unavailable() -> DirectHit {
    hit(
        "SELECT '不可计算' AS `数据状态`, '待确认对账单' AS `指标`, \
                     '当前业务数仓未同步DMS对账单事实表，无法安全计算张数与金额' AS `原因`, \
                     '请先补齐对账单事实同步，禁止用费用报销或其他相似表替代' AS `处理建议` \
              FROM dms_ods.t_dict_value LIMIT 1".into(),
        "direct-doc",
    )
}


pub fn warehouse_finance(question: &str) -> Option<DirectHit> {
    let invoice = ["开票", "专票", "普票"].iter().any(|w| question.contains(w))
        && !question.contains("可开票")
        && !question.contains("未开票")
        && !question.contains("开票余额")
        && ["金额", "多少", "额"].iter().any(|w| question.contains(w));
    if invoice {
        return Some(warehouse_invoice_unavailable());
    }
    if question.contains("对账单") || (question.contains("对账") && question.contains("待确认")) {
        return Some(warehouse_account_bill_unavailable());
    }
    // 🔴 十个分类名也是触发词（2026-08-15 生产直打 + 复验）：
    // 「6月营销物料费用」「本月客户返利费用」此前全落 need-intent —— 而这些分类名
    // 正是本模板自己拼明细时用的那十个（`MARKET_COST_GROUPS`）。
    // 模板既然算得出「营销物料」这一类的合计，就不该在用户点名它时装作不认识。
    let named_group = market_cost_group(question);
    if named_group.is_some()
        || ["市场费用", "营销费用", "销售费用"].iter().any(|w| question.contains(w))
    {
        // 🔴 残留守卫（2026-08-15 生产直打逮到，两条都很重）：
        //
        //   湖南省区市场费用       → 地域限定**一个字都没进 SQL**（`WHERE 1 = 1`），
        //                            照样出数：真值 111 万，答 1.04 亿，约 94 倍；
        //   市场费用核销需要哪些材料 → 政策问句被答成一个 1.04 亿的合计（换了个问题回答）。
        //
        // 本模板只兑现「时间窗 + 费用分类」两样东西，此前却是个裸 `contains("市场费用")`：
        // 剩下的限定一个都表达不了、也一个都不检查。与销售事实那条路同一条纪律 ——
        // **识别到却兑现不了的限定，不许静默丢**：有残留就不接，让它去走资料臂/自由 SQL。
        if !market_cost_residue(question).is_empty() {
            return None;
        }
        return Some(match named_group {
            Some((name, cols)) => warehouse_market_cost_group(question, name, cols),
            None => warehouse_market_cost(question),
        });
    }
    None
}

/// 市场费用模板消化不掉的残留。空 = 这条问句它全兑现得了。
///
/// 消化词只列**模板真的兑现了的**：指标本名、时间窗（`market_cost_where` 真的填进 WHERE）、
/// 排行词与分类词（`warehouse_market_cost` 的 rank 分支与 `MARKET_COST_GROUPS` 明细）。
/// 地域、客户、材料、核销、流程这些一个都不在里面 —— 它们出现就是残留。
pub fn market_cost_residue(question: &str) -> String {
    let mut consumed: Vec<&str> = vec![
        "市场费用", "营销费用", "销售费用", "费用总额", "推广费",
        // 排行分支真的落 ORDER BY + LIMIT
        "最多", "最高", "排行", "排名", "top", "Top", "TOP",
        // 明细分支真的按 `MARKET_COST_GROUPS` 出分类
        "分类", "构成", "明细", "各项", "分项", "项",
        // 「花/花了/花费/支出」是费用问句的口语动词，不是限定（「本月市场费用花了多少」）
        "花费", "花了", "花", "支出",
    ];
    // 时间：相对词用 `time_phrase_of`，显式年月（「6月」「2026年6月」）用 `intent_time_surface`。
    // 后者带**整句兜底**（`time_predicate(q).map(|_| q)`），拿整句当消化词会把真限定一起吞掉 ——
    // 所以只在它 != 整句时才用（与 `ops_caliber` 那处同一条守卫）。
    // `owned` 持有显式年月那一支的表面词（`intent_time_surface` 返 String）——
    // 借用它的 `&str` 进 `consumed`，不用 `Box::leak`（那是每次调用泄一次）。
    let owned_time = crate::fastpath::intent_time_surface(question)
        .filter(|surface| surface != question);
    if let Some(phrase) = dms_kernel::nl::time::time_phrase_of(question) {
        consumed.push(phrase);
    } else if let Some(surface) = owned_time.as_deref() {
        consumed.push(surface);
    }
    // 分类名是模板真的兑现得了的（它就是按这十个分类拼明细的）——点名哪一类就消化哪一类。
    // 「费用」本身也随之消化：「营销物料费用」= 分类名 + 通用尾词，不是两个限定。
    if let Some((name, _)) = market_cost_group(question) {
        consumed.push(name);
        consumed.push("费用");
    }
    crate::fastpath::residual_text(question, &consumed)
}

/// 问句点名了哪一个市场费用分类。`None` = 没点名（走全量口径）。
///
/// 只认**唯一**一个：点了两类（「营销物料和客户返利费用」）本模板一条 SQL 表达不了，
/// 按残留守卫的同一条纪律 fail-closed，不许挑一个答。
pub fn market_cost_group(question: &str) -> Option<(&'static str, &'static [&'static str])> {
    let mut hit: Option<(&'static str, &'static [&'static str])> = None;
    for (name, cols) in MARKET_COST_GROUPS {
        // 「其他」太泛，不做触发词（「其他费用」不是一个业务分类问法）
        if *name == "其他" || !question.contains(name) {
            continue;
        }
        if hit.is_some() {
            return None;
        }
        hit = Some((name, cols));
    }
    hit
}

/// 点名某一个分类时的合计（与全量口径同一张表、同一个时间窗，只换求和列）。
pub fn warehouse_market_cost_group(
    question: &str,
    name: &'static str,
    cols: &'static [&'static str],
) -> DirectHit {
    let pred = market_cost_where(question);
    hit(
        format!(
            "SELECT COALESCE(SUM({}),0) AS `{name}费用`              FROM sales_ads.ads_off_sales_cost_customer_dnf f WHERE {pred}",
            market_cost_expr("f", cols),
        ),
        "direct-agg",
    )
}


/// 账户余额排行是滚动快照，必须先按 (客户,余额类型) 取最新再聚合。
/// 该问法的“客户”维度属于客户主档，不允许经销售订单表绕路造成扇出。
pub fn balance_ranking(question: &str) -> Option<DirectHit> {
    // 裸「前」不是触发词（「之前的账户余额」会误中）：「前+N/前十」由 `detect_top_n`
    // 带时间单位黑名单判；「top」归一小写后判一次
    let top_n = detect_top_n(question);
    if !question.contains("账户余额")
        || !(["最高", "最多", "排行", "排名"].iter().any(|word| question.contains(word))
            || question.to_lowercase().contains("top")
            || top_n < 200)
        || !question.contains("客户")
    {
        return None;
    }
    let has_province_code = crate::present::PROVINCE_LABELS
        .iter()
        .any(|(code, _)| question.contains(code));
    if time_predicate(question).is_some()
        || has_province_code
        || !residual_text(question, &["账户余额", "客户"]).is_empty()
    {
        return None;
    }
    Some(hit(
        format!(
            "SELECT c.customer_name AS `客户`, SUM(t.balance) AS `账户余额` \
             FROM (SELECT customer_code, balance_type, balance, \
                          ROW_NUMBER() OVER (PARTITION BY customer_code, balance_type \
                                             ORDER BY created_time DESC, id DESC) AS rn \
                   FROM t_customer_balance \
                   WHERE deleted_flag = 0 AND balance_status = '4' AND balance_type IN ('8','9')) t \
             JOIN t_customer c ON c.customer_code = t.customer_code AND c.deleted_flag = 0 \
             WHERE t.rn = 1 \
             GROUP BY t.customer_code, c.customer_name \
             ORDER BY `账户余额` DESC LIMIT {top_n}"
        ),
        "direct-agg",
    ))
}


#[cfg(test)]
mod market_cost_guard_tests {
    use super::*;

    /// 模板兑现得了的照旧接；兑现不了的一律不接（不许静默丢限定）。
    ///
    /// 🔴 由来（2026-08-15 生产直打）：
    ///   湖南省区市场费用        → 地域一个字没进 SQL（`WHERE 1 = 1`），真值 111 万答成 1.04 亿；
    ///   市场费用核销需要哪些材料 → 政策问句被答成一个 1.04 亿的合计。
    #[test]
    fn market_cost_only_answers_what_it_can_actually_honour() {
        for q in [
            "本月市场费用", "本月市场费用是多少", "市场费用排行",
            "本月市场费用各分类构成", "本月市场费用花了多少", "本月市场费用最高的5项",
        ] {
            assert_eq!(market_cost_residue(q), "", "{q} 该被模板全兑现");
            assert!(warehouse_finance(q).is_some(), "{q} 该接");
        }
        // 点名分类的照旧接，且只求该类的列（2026-08-15：这十个分类名此前既不是触发词、
        // 也不在消化词里，「6月营销物料费用」「本月客户返利费用」全落 need-intent）
        for (q, name, col) in [
            ("6月营销物料费用", "营销物料", "mat_costume"),
            ("本月客户返利费用", "客户返利", "rebate_key_cust"),
            ("本月终端费用", "终端费用", "term_adv_fee"),
        ] {
            let h = warehouse_finance(q).unwrap_or_else(|| panic!("{q} 该接"));
            assert!(h.sql.contains(col), "{q} 该只求 {name} 那几列：{}", h.sql);
            assert!(h.sql.contains(&format!("`{name}费用`")), "{q} 列名该点明分类：{}", h.sql);
            // 全量口径的列不许混进来
            assert!(!h.sql.contains("sup_staff_cost + ") || name == "长促督导", "{q} 混进了全量列：{}", h.sql);
        }
        // 点了两类一条 SQL 表达不了 → 不接（与残留守卫同一条纪律）
        assert!(warehouse_finance("本月营销物料和客户返利费用").is_none());

        for (q, why) in [
            ("湖南省区市场费用", "地域限定模板表达不了"),
            ("山东省区本月市场费用", "同上"),
            ("市场费用核销需要哪些材料", "这是政策问句，不是金额问句"),
            ("市场费用报销流程是什么", "同上"),
            ("恒众餐饮本月市场费用", "客户限定模板表达不了"),
        ] {
            assert!(!market_cost_residue(q).is_empty(), "{q}：{why}（残留不该为空）");
            assert!(warehouse_finance(q).is_none(), "{q}：{why}（不该接）");
        }
    }
}
