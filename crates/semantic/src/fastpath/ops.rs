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
use crate::fastpath::{derive::*, finance::*, graph_rows::*, relation::*, sales::*, stock::*, template::*};

use crate::sales_fact;

/// 问句里的「{省名}战区 / {省名}省区」限定值 → (省名词干, 原词)。
/// Ok(None)=没有区域限定；Err=有区域词但兑现不了（多个限定值、或词干不在省名表，
/// 如「华北战区」「直营战区」「各省区」）—— 调用方必须不接，不许静默丢限定。
/// 值形态已探库（2026-08-11）：数仓 region 列与 ODS province_department_name 列
/// 都用「山东省区/山东战区」这类「省名+后缀」存储。
pub fn province_region_qualifier(question: &str) -> Result<Option<(&'static str, String)>, ()> {
    let mut hit: Option<(&'static str, String)> = None;
    for &(_code, name) in crate::present::PROVINCE_LABELS {
        for suffix in ["省区", "战区"] {
            let phrase = format!("{name}{suffix}");
            if question.contains(&phrase) {
                if hit.as_ref().is_some_and(|(_, p)| *p != phrase) {
                    return Err(()); // 多个区域限定值，等值谓词表达不了
                }
                hit = Some((name, phrase));
            }
        }
    }
    if hit.is_none() && ["战区", "省区", "大区"].iter().any(|w| question.contains(w)) {
        return Err(());
    }
    Ok(hit)
}



/// 小程序下单事实：`sales_dw.dws_mkt_app_place_order_dnf`（统计日×客户的当日/当月累计快照）。
/// t_sales_order.source_platform_code 全表只有 'DMS'，小程序订单不在里面 —— 2026-08-11 实测
/// 「按客户展示山东战区本月小程序的下单数量和金额」被 `sales_order_rows` 劫走，
/// 「山东战区」「小程序」两个限定一个条件都没进 SQL。本模板拦在它前面。
/// 合同纪律（warehouse_catalog 钉着）：必须按 data_date 取最新快照；同行 tomonth_* 已是
/// 当月累计，禁止跨 data_date SUM 累计列、禁止混加当日与月累计 —— 所以一条 SQL 只选一个
/// 时间列族，快照日用「数据日期」列透出。
pub fn mini_program_order_agg(question: &str) -> Option<DirectHit> {
    if !question.contains("小程序")
        || (!question.contains("下单") && !question.contains("订单"))
        || question.contains("设备订单")
    {
        return None;
    }
    // 时间 → 列族：今天/今日 = 当日列；本月/这个月/当月 = 当月累计列；缺省 = 当月累计。
    // 其他时间词（昨天/上月/今年…）这张「当日+当月累计」快照表没有对应列 → 不接，让位 LLM。
    let monthly = if ["今天", "今日"].iter().any(|w| question.contains(w)) {
        false
    } else if ["本月", "这个月", "当月"].iter().any(|w| question.contains(w)) {
        true
    } else if time_predicate(question).is_some() {
        return None;
    } else {
        true
    };
    let wants_wx = question.contains("微信");
    let wants_zy = question.contains("账余");
    let wants_cancel = question.contains("取消");
    let wants_count = ["数量", "单量", "多少单", "几单", "订单数", "单数"]
        .iter()
        .any(|w| question.contains(w));
    let wants_amount = question.contains("金额");
    let vague = question.contains("多少"); // 只说「多少」= 数量金额都要
    if !(wants_wx || wants_zy || wants_cancel || wants_count || wants_amount || vague) {
        return None;
    }
    // 取消只有单数列、没有金额列：问「取消的金额」兑现不了 → 不接
    if wants_cancel && wants_amount {
        return None;
    }
    // 列族（同行同周期，绝不混加当日与月累计；今日账余列的物理拼写就是 todaty_，原样照抄）
    let p = if monthly { "本月" } else { "今日" };
    let (count, amount) = if monthly {
        ("tomonth_order_count", "tomonth_amount")
    } else {
        ("today_order_count", "today_amount")
    };
    let (wx_count, wx_amount) = if monthly {
        ("tomonth_wxorder_count", "tomonth_wxorder_amount")
    } else {
        ("today_wxorder_count", "today_wxorder_amount")
    };
    let (zy_count, zy_amount) = if monthly {
        ("tomonth_zyorder_count", "tomonth_zyorder_amount")
    } else {
        ("todaty_zyorder_count", "todaty_zyorder_amount")
    };
    let cancel = if monthly { "tomonth_cancel_order" } else { "today_cancel_order" };
    let mut cols: Vec<(String, String)> = vec![]; // (聚合表达式, 中文列名)
    let mut push = |col: &str, label: String| cols.push((format!("SUM({col})"), label));
    if wants_wx || wants_zy || wants_cancel {
        if wants_count {
            push(count, format!("{p}下单数量"));
        }
        if wants_wx {
            if wants_count || !wants_amount {
                push(wx_count, format!("{p}微信下单数量"));
            }
            if wants_amount || !wants_count {
                push(wx_amount, format!("{p}微信下单金额"));
            }
        }
        if wants_zy {
            if wants_count || !wants_amount {
                push(zy_count, format!("{p}账余下单数量"));
            }
            if wants_amount || !wants_count {
                push(zy_amount, format!("{p}账余下单金额"));
            }
        }
        if wants_cancel {
            push(cancel, format!("{p}取消订单数"));
        }
    } else if wants_count && !wants_amount && !vague {
        push(count, format!("{p}下单数量"));
    } else if wants_amount && !wants_count && !vague {
        push(amount, format!("{p}下单金额"));
    } else {
        // 数量+金额都要（或只说「多少」）：两列都给，微信/账余分列透出构成
        push(count, format!("{p}下单数量"));
        push(amount, format!("{p}下单金额"));
        push(wx_count, format!("{p}微信下单数量"));
        push(wx_amount, format!("{p}微信下单金额"));
        push(zy_count, format!("{p}账余下单数量"));
        push(zy_amount, format!("{p}账余下单金额"));
    }
    // 区域限定：认得出省名词干就按该表 region 列的存储形态写谓词；认不出/多个 → 不接
    let region = match province_region_qualifier(question) {
        Ok(v) => v,
        Err(()) => return None,
    };
    // 残留守卫：只剥本模板兑现了的词；剥完还有实义残留（商品/门店/渠道/实体名…）→ 让位
    let mut consumed: Vec<&str> = vec![
        "小程序", "下单", "订单", "数量", "单量", "多少单", "几单", "订单数", "单数", "金额",
        "取消", "微信支付", "微信", "账余", "支付", "客户", "当月", "情况", "进行", "展示",
    ];
    if let Some((_, phrase)) = &region {
        consumed.push(phrase);
    }
    if !residual_text(question, &consumed).is_empty() {
        return None;
    }
    let region_sql = match region {
        Some((stem, _)) => {
            let region = crate::warehouse_catalog::shop_business_region_for_province(stem)?;
            if region == format!("{stem}省区") {
                // 普通省份保持已探值的兼容候选；非字面映射只走生产权威值。
                format!(" AND region IN ('{stem}省区','{stem}战区','{stem}大区','{stem}')")
            } else {
                format!(" AND region = '{}'", rel_quote(region))
            }
        }
        None => String::new(),
    };
    let snapshot = "data_date = (SELECT MAX(data_date) FROM sales_dw.dws_mkt_app_place_order_dnf)";
    // 问句点名「战区」必须明示口径：该表没有 war_zone 列，region 是省区口径 ——
    // 不许静默拿 region 冒充战区（「战区/大区」只是 region 列的存储形态探值，不是层级）。
    let war_zone_note =
        if question.contains("战区") { "该表无「战区」字段，按省区（region）统计；" } else { "" };
    let note = format!(
        "-- 小程序下单口径（{war_zone_note}最新快照 data_date，快照日见「数据日期」列；\
         同行当月/当日累计，禁止跨快照日求和）\n"
    );
    let select_cols =
        cols.iter().map(|(expr, label)| format!("{expr} AS `{label}`")).collect::<Vec<_>>().join(", ");
    if ["按客户", "各客户"].iter().any(|w| question.contains(w)) {
        // ORDER BY 取金额列（没有金额列就取首个指标列），与「按金额 DESC」的约定一致
        let order = cols
            .iter()
            .find(|(_, label)| label.contains("金额"))
            .unwrap_or_else(|| &cols[0]);
        return Some(hit(
            format!(
                "{note}SELECT store_code AS `客户编码`, store_name AS `客户`, {select_cols}, \
                        MAX(data_date) AS `数据日期` \
                 FROM sales_dw.dws_mkt_app_place_order_dnf \
                 WHERE {snapshot}{region_sql} \
                 GROUP BY store_code, store_name ORDER BY `{}` DESC LIMIT 200",
                order.1
            ),
            "direct-agg",
        ));
    }
    Some(hit(
        format!(
            "{note}SELECT {select_cols}, MAX(data_date) AS `数据日期` \
             FROM sales_dw.dws_mkt_app_place_order_dnf WHERE {snapshot}{region_sql}"
        ),
        "direct-agg",
    ))
}



/// 销售订单列表：明细问法直接返回业务列与中文状态，不让 LLM 自由挑表/列。
pub fn sales_order_rows(question: &str) -> Option<DirectHit> {
    let wants_rows = ["订单明细", "订单清单", "销售单明细", "销售订单明细"]
        .iter()
        .any(|w| question.contains(w));
    // contains 语义下的冗余项已删：「有下单/有过下单」都含「下单」；「有那些客户」含「那些客户」。
    // ⚠️ 「下过单」不是冗余项（「下」与「单」不相连），删了「昨天都有谁下过单啊」会漏接。
    let explicit_order_customers = ["下单", "下过单"]
        .iter()
        .any(|w| question.contains(w))
        && ["客户", "谁", "哪家", "哪些", "那些"]
            .iter()
            .any(|w| question.contains(w));
    let temporal_customer_list = ["哪些客户", "那些客户", "哪几家客户"]
        .iter()
        .any(|w| question.contains(w))
        && time_predicate(question).is_some()
        && !["新增", "拜访", "投诉", "售后", "回款", "欠款", "余额", "流失"]
            .iter()
            .any(|w| question.contains(w));
    let wants_customers = explicit_order_customers || temporal_customer_list;
    if (!wants_rows && !wants_customers) || question.contains("设备订单") {
        return None;
    }
    // 🔴 「小程序」限定本模板兑现不了：t_sales_order.source_platform_code 全表只有 'DMS'，
    // 小程序订单不在里面，两个分支都没有渠道过滤能力 —— 接了就是静默丢限定
    // （2026-08-11 实测：「山东战区+小程序」双限定一个条件都没进 SQL）。让位：
    // 数仓侧由 mini_program_order_agg（dws_mkt_app_place_order_dnf）接，其余落 LLM。
    if question.contains("小程序") {
        return None;
    }
    // 战区/省区限定：province_department_name（省区部门名称）已探值含「山东战区/山东省区」
    // 形态，认得出就补等值谓词；认不出（多值/非省名词干）就不接 —— 同一纪律：
    // 识别到却兑现不了的限定词，不许静默丢。
    let region = match province_region_qualifier(question) {
        Ok(v) => v,
        Err(()) => return None,
    };
    let region_sql = match region {
        Some((stem, phrase)) => {
            let business_region = crate::warehouse_catalog::shop_business_region_for_province(stem)?;
            let conventional_region = format!("{stem}省区");
            let value = if business_region == conventional_region {
                phrase.as_str()
            } else {
                business_region
            };
            format!(" AND o.province_department_name = '{}'", rel_quote(value))
        }
        None => String::new(),
    };
    let pred = time_predicate(question)?;
    let time = fill_time_col(&pred, "o.order_time");
    if wants_customers {
        return Some(hit(
            format!(
                "SELECT o.customer_name AS `客户`, COUNT(DISTINCT o.sales_order_code) AS `订单数`, \
                        SUM(o.total_amount) AS `订单金额`, MAX(o.order_time) AS `最近下单时间` \
                 FROM t_sales_order o \
                 WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199') AND {time}{region_sql} \
                 GROUP BY o.customer_name ORDER BY `订单金额` DESC, `订单数` DESC LIMIT 200"
            ),
            "direct-doc",
        ));
    }
    Some(hit(
        format!(
            "SELECT o.sales_order_code AS `订单号`, o.order_time AS `下单时间`, \
                    o.customer_name AS `客户`, COALESCE(o.shop_name,'') AS `门店`, \
                    COALESCE(e.actual_name,o.owner_manager) AS `业务员`, \
                    COALESCE(d.value_name,o.order_type) AS `订单类型`, \
                    {} AS `订单状态`, o.total_quantity AS `订单数量`, \
                    o.total_amount AS `订单金额`, o.actual_paid_amount AS `实付金额` \
             FROM t_sales_order o \
             LEFT JOIN t_employee e ON e.employee_id = o.owner_manager AND e.deleted_flag = 0 \
             LEFT JOIN t_dict_value d ON d.dict_key_id = '67' AND d.value_code = o.order_type \
             WHERE o.deleted_flag = 0 AND {time}{region_sql} ORDER BY o.order_time DESC LIMIT 200",
            sales_status_sql("o.order_status")
        ),
        "direct-doc",
    ))
}



/// DMS「设备订单」不是泛指设备名称，而是销售订单体系里的 SO04 单据。
/// 这里直接绑定后端源码确认过的业务语义，避免裸名词被 need-intent 抢走。
pub fn device_orders(question: &str) -> Option<DirectHit> {
    if !["设备订单", "设备销售单"].iter().any(|w| question.contains(w)) {
        return None;
    }
    // 时间谓词只解析一次：单表分支填裸列，两个 JOIN 分支各自填 `o.` 限定
    let time_pred = time_predicate(question);
    let time = time_pred
        .as_deref()
        .map(|p| format!(" AND {}", fill_time_col(p, "order_time")))
        .unwrap_or_default();
    let where_sql = format!("deleted_flag = 0 AND order_type = 'SO04'{time}");
    // 与订单明细/单据卡同一份 16 臂状态映射（`sales_status_sql`）：抄第二份两处文案必漂
    let status_sql = sales_status_sql("order_status");
    let sql = if question.contains("按设备类型") || question.contains("设备构成") {
        format!("SELECT COALESCE(NULLIF(TRIM(dv.class3), ''), NULLIF(TRIM(dv.class2), ''), \
                        NULLIF(TRIM(dv.class1), ''), x.sku_name) AS `设备类型`, \
                        SUM(x.box_quantity) AS `设备数量` \
                 FROM t_sales_order o \
                 JOIN (SELECT DISTINCT sales_order_code, sku_code, sku_name, box_quantity \
                       FROM t_sales_order_detail WHERE deleted_flag = 0 AND item_type = '1') x \
                   ON x.sales_order_code = o.sales_order_code \
                 LEFT JOIN dim.dim_device dv ON dv.sku_code = x.sku_code \
                 WHERE o.deleted_flag = 0 AND o.order_type = 'SO04'{} \
                 GROUP BY COALESCE(NULLIF(TRIM(dv.class3), ''), NULLIF(TRIM(dv.class2), ''), \
                          NULLIF(TRIM(dv.class1), ''), x.sku_name) \
                 ORDER BY `设备数量` DESC LIMIT 200", time_pred
                    .as_deref()
                    .map(|p| format!(" AND {}", fill_time_col(p, "o.order_time")))
                    .unwrap_or_default())
    } else if question.contains("按设备名称") || question.contains("各设备") {
        format!("SELECT x.sku_name AS `设备名称`, SUM(x.box_quantity) AS `设备数量` \
                 FROM t_sales_order o \
                 JOIN (SELECT DISTINCT sales_order_code, sku_code, sku_name, box_quantity \
                       FROM t_sales_order_detail WHERE deleted_flag = 0 AND item_type = '1') x \
                   ON x.sales_order_code = o.sales_order_code \
                 WHERE o.deleted_flag = 0 AND o.order_type = 'SO04'{} \
                 GROUP BY x.sku_name ORDER BY `设备数量` DESC LIMIT 200", time_pred
                    .as_deref()
                    .map(|p| format!(" AND {}", fill_time_col(p, "o.order_time")))
                    .unwrap_or_default())
    } else if question.contains("按客户") || question.contains("各客户") {
        format!("SELECT customer_name AS `客户`, COUNT(DISTINCT sales_order_code) AS `设备订单数` \
                 FROM t_sales_order WHERE {where_sql} GROUP BY customer_name \
                 ORDER BY `设备订单数` DESC LIMIT 200")
    } else if question.contains("按状态") || question.contains("各状态") {
        format!("SELECT {status_sql} AS `状态`, COUNT(DISTINCT sales_order_code) AS `设备订单数` \
                 FROM t_sales_order WHERE {where_sql} GROUP BY order_status \
                 ORDER BY `设备订单数` DESC LIMIT 200")
    } else if question.contains("按小时") || question.contains("各小时") {
        format!("SELECT DATE_FORMAT(order_time, '%H:00') AS `小时`, \
                 COUNT(DISTINCT sales_order_code) AS `设备订单数` \
                 FROM t_sales_order WHERE {where_sql} GROUP BY DATE_FORMAT(order_time, '%H:00') \
                 ORDER BY `小时` LIMIT 200")
    } else if ["多少", "数量", "几单", "总数"].iter().any(|w| question.contains(w)) {
        format!("SELECT COUNT(DISTINCT sales_order_code) AS `设备订单数` FROM t_sales_order WHERE {where_sql}")
    } else {
        format!("SELECT sales_order_code AS `单号`, order_time AS `下单时间`, \
                 customer_name AS `客户`, source_code AS `设备需求单号`, \
                 total_amount AS `押金金额`, {status_sql} AS `状态` \
                 FROM t_sales_order WHERE {where_sql} ORDER BY order_time DESC LIMIT 200")
    };
    Some(hit(sql, "direct-doc"))
}


