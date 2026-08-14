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
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*};

use crate::sales_fact;

/// 残留文本/实体名片段共用的标点白名单（`residual_text` 与 `customer_name_fragment` 同一份）——
/// 各写一份会漂出「带书名号/括号的问句一边算残留、一边不算」。
pub const RESIDUAL_PUNCT: &str = "，。？?、,.~～!！:：;；「」『』()（）";




pub fn residual_text(question: &str, consumed: &[&str]) -> String {
    let mut s = question.to_string();
    let mut words = consumed.to_vec();
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    for w in words {
        s = s.replace(w, "");
    }
    for w in dms_kernel::nl::lexicon::STRIP_WORDS {
        s = s.replace(w, "");
    }
    s.chars()
        .filter(|c| !c.is_ascii_digit() && !c.is_whitespace() && !RESIDUAL_PUNCT.contains(*c))
        .collect::<String>()
        .trim()
        .to_string()
}




/// 16 臂 order_status 中文状态映射。形参是**列引用**（`o.order_status` / 裸 `order_status`），
/// 订单明细、单据卡、设备订单三处共用 —— 各抄一份，两处状态文案必漂。
pub fn sales_status_sql(col: &str) -> String {
    format!(
        "CASE {col} \
         WHEN '0' THEN '暂存 (0)' WHEN '100' THEN '未支付 (100)' \
         WHEN '101' THEN '待备货 (101)' WHEN '102' THEN '备货中 (102)' \
         WHEN '103' THEN '等待配送 (103)' WHEN '104' THEN '交易完成 (104)' \
         WHEN '105' THEN '待核销 (105)' WHEN '106' THEN '售后中 (106)' \
         WHEN '107' THEN '已退款 (107)' WHEN '108' THEN '已取消 (108)' \
         WHEN '109' THEN '部分收货 (109)' WHEN '110' THEN '待收货 (110)' \
         WHEN '111' THEN '部分发货 (111)' WHEN '150' THEN '取消中 (150)' \
         WHEN '151' THEN '取消失败-退款失败 (151)' WHEN '199' THEN '已删除 (199)' \
         ELSE CONCAT('未知状态 (', {col}, ')') END"
    )
}




pub fn sales_detail_sql(where_sql: &str, joins: &str, dedup_join: bool) -> String {
    // 数仓对账表可能同一外部单号对应多条核对记录；按业务明细主键分组只消掉 JOIN 放大，
    // 不会把内容相同但 id 不同的真实订单行合并。
    let group = if dedup_join {
        " GROUP BY d.id, d.item_code, d.sku_code, d.sku_name, d.box_gauge, d.price, \
                    d.box_quantity, d.bag_quantity, d.goods_amount, d.amount, \
                    d.actual_delivery_quantity, d.actual_receive_quantity, \
                    d.delivery_time, d.receive_time"
    } else {
        ""
    };
    format!(
        "SELECT d.item_code AS `商品编码`, d.sku_code AS `SKU编码`, d.sku_name AS `商品名称`, \
                d.box_gauge AS `箱规`, d.price AS `单价`, d.box_quantity AS `箱数`, \
                d.bag_quantity AS `袋数`, d.goods_amount AS `商品金额`, d.amount AS `明细金额`, \
                d.actual_delivery_quantity AS `实发数量`, d.actual_receive_quantity AS `实收数量`, \
                d.delivery_time AS `发货时间`, d.receive_time AS `收货时间` \
           FROM {joins} WHERE {where_sql} AND d.deleted_flag = 0 AND d.item_type = '1' \
          {group} ORDER BY d.id LIMIT 200"
    )
}




pub fn warehouse_order_hit(code: &str) -> DirectHit {
    let status = sales_status_sql("o.order_status");
    let sql = format!(
        "SELECT '数仓发货拆单映射' AS `单据类型`, 'sales_dw.dws_fin_shipment_check_dnf' AS `主表`, \
                'dms_ods.t_sales_order_detail' AS `明细表`, r.ywzt_order AS `中台单号`, r.base_ref_order AS `基础系统单号`, \
                r.dms_order_code AS `DMS销售单号`, r.ship_at AS `发货日期`, r.order_type AS `订单类型`, \
                r.store_name AS `门店`, r.dms_lines AS `DMS行数`, r.dms_amount AS `DMS金额`, \
                r.ywzt_lines AS `中台行数`, r.ywzt_amount AS `中台金额`, \
                r.base_lines AS `基础系统行数`, r.base_amount AS `基础系统金额`, \
                r.lines_difference AS `行数差异`, r.amount_difference AS `金额差异`, \
                r.change_type AS `差异类型`, o.customer_name AS `客户`, {status} AS `DMS订单状态`, \
                o.order_time AS `DMS下单时间` \
           FROM sales_dw.dws_fin_shipment_check_dnf r \
           JOIN dms_ods.t_sales_order o ON o.sales_order_code = r.dms_order_code AND o.deleted_flag = 0 \
          WHERE r.ywzt_order = '{code}' OR r.base_ref_order = '{code}' \
          ORDER BY r.ship_at DESC LIMIT 1"
    );
    let detail = sales_detail_sql(
        &format!("(r.ywzt_order = '{code}' OR r.base_ref_order = '{code}')"),
        "sales_dw.dws_fin_shipment_check_dnf r \
         JOIN dms_ods.t_sales_order o ON o.sales_order_code = r.dms_order_code AND o.deleted_flag = 0 \
         JOIN dms_ods.t_sales_order_detail d ON d.sales_order_code = o.sales_order_code",
        true,
    );
    DirectHit { detail: Some(detail), ..hit(sql, "direct-doc") }
}




pub fn document_detail_sql(
    source: &crate::document::DocumentSource,
    code: &str,
) -> Option<String> {
    let detail = source.details.first()?;
    let deleted = detail.deleted_flag.then_some(" AND deleted_flag = 0").unwrap_or("");
    Some(format!(
        "SELECT {} FROM {} WHERE {} = '{}'{} LIMIT 50",
        detail.projection, detail.table, detail.code_col, code, deleted
    ))
}




/// 生产与数仓共用 semantic 的严格扫描器和同一份字段白名单。
pub fn sniff_doc_code(question: &str, warehouse: bool) -> Option<DirectHit> {
    use crate::document::{resolve_document, DocumentKind};

    let resolved = resolve_document(question, warehouse)?;
    let safe = resolved.code.replace('\'', "''");
    if resolved.family.kind == DocumentKind::WarehouseShipment {
        return Some(warehouse_order_hit(&safe));
    }
    let source = resolved.family.source(warehouse)?;
    let column = source.header_code_cols.first()?;
    let deleted = source.header_deleted_flag.then_some(" AND deleted_flag = 0").unwrap_or("");
    let sql = format!(
        "SELECT {} FROM {} WHERE {} = '{}'{} LIMIT 1",
        source.header_projection, source.header_table, column, safe, deleted
    );
    let detail = document_detail_sql(source, &safe);
    Some(DirectHit { detail, ..hit(sql, "direct-doc") })
}




/// `agg_template` 的剥词表 ＝ 本模板消化的**业务**词 + `kernel::nl::lexicon::STRIP_WORDS`。
///
/// 🔴 时间词与通用虚词**只有一份**（STRIP_WORDS 是单一事实源）。这里原先有**第二份内联
/// 时间词表**（11 个时间词），而 STRIP_WORDS 里的时间词有 23 个（多出「上周/去年/上半年/
/// 下半年/近/最近/季度/至今」与「天/周/月/年」这些单字单位词）—— **差集精确地就是曝光面**：
/// 「上周」「去年」在 STRIP_WORDS 里 ⇒ 组合器那侧的残留守卫剥得掉、**不拦**；不在这份内联表里
/// ⇒ `agg_template` 返 None ⇒ `compose_hit` 的让路门开 ⇒ 「上周成交客户数是多少」被装配成
/// 按客户分组的 **200 行**（实测，审计 二·AS1；而「本季度」两边都没有，被残留守卫拦下回落 LLM
/// 反而答对了 —— 也就是说这条 bug 的分布完全由两份词表的差集决定）。
///
/// ⚠️ 收词只影响本模板的**入口宽度**：剥得掉但 `time_predicate` 解析不了的词照旧返 None
/// （`let time_pred = time_window(question)?`）。`agg_template_time_words_come_from_the_single_source`
/// 逐词钉住「剥词表与 time_predicate 一致」这件事。
pub fn agg_strip_words() -> Vec<&'static str> {
    // 本模板消化的订单口径指标词。默认销售额由 DWS 事实快路径单独处理。
    // 顺序即行为：长词先于子串
    // （「成交客户数」先于「成交客户」先于「客户数」）。
    const AGG_WORDS: &[&str] = &[
        "订单数", "多少单", "几单", "客单价",
        "成交客户数", "成交客户", "客户数", "多少客户",
        // 语气词：`STRIP_WORDS` 今天还没收这五个，而它是全仓共用表（加词要连它自己的守卫
        // 一起改，属 kernel 侧）。留在本模板里 = 只放宽本模板，不放宽组合器的残留守卫。
        "呢", "吗", "总共", "一共", "了",
    ];
    AGG_WORDS.iter().copied().chain(dms_kernel::nl::lexicon::STRIP_WORDS.iter().copied()).collect()
}




/// 本文件专用薄包装：规则时间解析（kernel）+ 填本表时间列
pub fn time_window(q: &str) -> Option<String> {
    time_predicate(q).map(|tpl| fill_time_col(&tpl, "order_time"))
}

// ─────────── T9 wire：Router 两个 `HitAnswerer` 成员的产出方 ───────────
// 见文件头的 ponytail：这两个函数随 T8 一起删除。
// 必须是**具名 `fn`**（不是闭包）：`crate::HitFn` 是一条 HRTB（返回的 future 借着入参的
// 生命周期），闭包在那上面的推断很脆。`detect_relation` 本身已经就是 `crate::DetectFn`，
// 不需要包装 —— 那也是它与 agent 共用同一个 `Relation` 换来的。




