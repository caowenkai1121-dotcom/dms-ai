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
    (["市场费用", "营销费用", "销售费用"].iter().any(|w| question.contains(w)))
        .then(|| warehouse_market_cost(question))
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

