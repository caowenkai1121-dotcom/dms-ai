//! 39 张 DMS/数仓辅助表的内置权限档案（scoped 17 / via 7 / global 15）。
//!
//! 表名与列名是 DMS 语料，**故意不在 kernel**（ARCHITECTURE §5「builtin_rules」行、
//! 门禁 `kernel 不得含 DMS 表名`）。代码即种子真相：`rules::seed_rules` 据此 upsert
//! `meta.scope_binding`，PG 不可用时 `rules::snapshot()` 也回落到这里。
//!（源头是已删除的 server/src/inject.rs 表清单，逐行搬来后在此维护。）

use std::collections::HashMap;

use dms_kernel::{Binding, CustomerKind, OwnerKind, TableRule};

/// 内置种子（代码即种子真相，随 seed_rules 灌表；PG 不可用时兜底）。
/// 口径来源：DMS Java @DataScope joinSql 逐条核对（2026-07-26 复核 15 个 mapper）。
pub fn builtin_rules() -> HashMap<String, TableRule> {
    use OwnerKind::*;
    let b = |c: &str, o: Option<&str>, k: OwnerKind| {
        TableRule::Scoped(Binding {
            customer_col: Some(c.to_string()),
            customer_kind: CustomerKind::Codes,
            owner_col: o.map(|s| s.to_string()),
            owner_kind: k,
        })
    };
    let owner_only = |o: &str, k: OwnerKind| {
        TableRule::Scoped(Binding {
            customer_col: None,
            customer_kind: CustomerKind::Codes,
            owner_col: Some(o.to_string()),
            owner_kind: k,
        })
    };
    let shop_b = |c: &str| {
        TableRule::Scoped(Binding {
            customer_col: Some(c.to_string()),
            customer_kind: CustomerKind::ShopCodes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
        })
    };
    let via = |t: &str, l: &str, r: &str| TableRule::Via {
        table: t.to_string(),
        local_col: l.to_string(),
        remote_col: r.to_string(),
    };
    let mut m: HashMap<String, TableRule> = HashMap::new();
    // —— scoped：Java joinSql 权威模板 → 列绑定
    m.insert("t_sales_order".into(), b("customer_code", Some("owner_manager"), Ids));
    m.insert("t_sales_order_his".into(), b("customer_code", Some("owner_manager"), Ids));
    m.insert("t_customer".into(), b("customer_code", Some("area_manager_id"), Ids));
    m.insert("t_after_sales_order_header".into(), b("customer_code", Some("owner_manager"), Ids));
    m.insert("t_activity_main".into(), b("customer_code", Some("created_id"), Ids));
    m.insert(
        "t_market_activity_promoter_expense".into(),
        b("customer_code", Some("created_by"), Codes),
    );
    // 旧开票页由 service 将当前数据范围员工 ID 写入 managers，XML 只过滤 invoice.manager。
    // 不能套用新开票的 customer OR manager 注解，否则会扩大旧流程可见客户面。
    m.insert("t_invoice_apply_header".into(), owner_only("manager", Ids));
    m.insert("t_invoice_new_apply_header".into(), b("customer_code", Some("manager"), Ids));
    // 对账单页面按 `manager`（历史兼容员工 ID/姓名）裁决，无法用通用 AST 单字段绑定
    // 精确表达。禁止登记成 customer/created_by 的近似规则，避免通用 NL2SQL 扩大可见面；
    // 精确单号由 business-lookup 在头表点查后按 manager 双形态单独裁决。
    m.insert("t_device_inspection_header".into(), b("customer_code", Some("manager_code"), Codes));
    m.insert("t_long_promotion_person".into(), b("customer_code", Some("manager_id"), Ids));
    // 无 owner 维度的表（Java 模板只有 customer 段）
    for t in ["t_customer_balance", "t_customer_device_ledger", "t_device_disposal_order", "t_shop_inspection_records"] {
        m.insert(t.into(), b("customer_code", None, Ids));
    }
    // 设备列表的普通范围是 customer_code OR area_manager_id，但两个专职角色还有设备专属
    // 全量例外。静态 Binding 无法携带该角色证明，因此设备头/明细不进通用 NL2SQL 档案，
    // 只由精确单号通道先裁决头表，再按同一单号读取明细。
    // 数仓市场费用按客户汇总；`store_code` 实际承载 DMS customer_code。
    m.insert("ads_off_sales_cost_customer_dnf".into(), b("store_code", None, Ids));
    // BI 线下销售宽表没有稳定员工 ID，只按 DMS customer_codes → storecode 隔离。
    // 受限身份若映射不到客户编码，RequiredCodes 会注入恒假条件。
    m.insert(
        "dws_off_offline_sale_dfn".into(),
        TableRule::Scoped(Binding {
            customer_col: Some("storecode".into()),
            customer_kind: CustomerKind::RequiredCodes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
        }),
    );
    // DMS 三种特殊策略分别返回门店集合；shop_contact 只能看绑定门店，不能按客户放大。
    m.insert("t_master_shop".into(), shop_b("shop_code"));
    // —— via：明细/从表独查借头表条件（Java 场景恒 JOIN 头表吃 tso.* 条件）
    m.insert("t_sales_order_detail".into(), via("t_sales_order", "sales_order_code", "sales_order_code"));
    m.insert("t_sales_order_logistics".into(), via("t_sales_order", "sales_order_code", "sales_order_code"));
    m.insert("t_after_sales_order_detail".into(), via("t_after_sales_order_header", "after_sales_code", "after_sales_code"));
    m.insert("t_activity_promoter_fee".into(), via("t_activity_main", "activity_id", "id"));
    m.insert("t_invoice_apply_detail".into(), via("t_invoice_apply_header", "invoice_code", "invoice_code"));
    m.insert("t_invoice_new_apply_detail".into(), via("t_invoice_new_apply_header", "invoice_code", "invoice_code"));
    // 数仓对账表只负责跨系统单号映射，行权限借 DMS 订单头表。
    m.insert("dws_fin_shipment_check_dnf".into(), via("t_sales_order", "dms_order_code", "sales_order_code"));
    // —— global：Java 无 @DataScope（维表/字典/主数据/全局报表），1:1 全量可见
    for t in [
        "t_goods", "t_goods_category", "t_employee", "t_department", "t_employee_department",
        "t_dict_key", "t_dict_value", "t_warehouse", "t_warehouse_manage",
        "t_winc_stock_report", "t_winc_sale_report", "t_market_total_expense",
        "t_customer_price",
        // 地区码表（region_code UNI 不扇出）：ship_dim 省份解码把它 JOIN 进模板 ——
        // 不登记 = 受限用户的省份维度查询被 fail-closed 整批拒（与 t_dict_value 同一条）
        "t_regions",
        // Doris 数仓设备维表：只含设备分类与 SKU 主数据，无用户归属，供设备订单构成下钻。
        "dim_device",
    ] {
        m.insert(t.into(), TableRule::Global);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三类档案的条数是契约（39 表 / scoped 17 / via 7 / global 15）。
    /// 少一张 scoped 表 = 该表对受限用户从「注入条件」变成「fail-closed 拒绝」或反之，静默改权限面。
    #[test]
    fn thirty_nine_tables_by_kind() {
        let m = builtin_rules();
        assert_eq!(m.len(), 39);
        assert_eq!(m.values().filter(|r| matches!(r, TableRule::Scoped(_))).count(), 17);
        assert_eq!(m.values().filter(|r| matches!(r, TableRule::Via { .. })).count(), 7);
        assert_eq!(m.values().filter(|r| matches!(r, TableRule::Global)).count(), 15);
        // 省份解码字典必须在 global（ship_dim 省份 JOIN 它；缺了受限用户整批被拒）
        assert!(matches!(m.get("t_regions"), Some(TableRule::Global)));
        assert!(matches!(m.get("dim_device"), Some(TableRule::Global)));
        for table in [
            "t_device_requisition",
            "t_device_receive_item",
            "t_device_delivery_item",
            "t_account_bill_header",
            "t_account_bill_detail",
        ] {
            assert!(!m.contains_key(table), "{table} 必须只走精确单号权限裁决");
        }
        assert!(!m.contains_key("t_winc_purchase_transfer"), "采购调拨没有已证明的数据范围，必须失败关闭");
        assert!(matches!(m.get("dws_fin_shipment_check_dnf"), Some(TableRule::Via { .. })));
        assert!(matches!(m.get("ads_off_sales_cost_customer_dnf"), Some(TableRule::Scoped(_))));
        let Some(TableRule::Scoped(sales)) = m.get("dws_off_offline_sale_dfn") else {
            panic!("DWS 线下销售事实必须按客户编码受限");
        };
        assert_eq!(sales.customer_col.as_deref(), Some("storecode"));
        assert!(sales.customer_kind == CustomerKind::RequiredCodes);
        assert!(sales.owner_col.is_none(), "不得用 manger 名称模拟稳定员工 ID");
        let Some(TableRule::Via { table, local_col, remote_col }) = m.get("t_activity_promoter_fee") else {
            panic!("t_activity_promoter_fee 必须经活动头表继承权限");
        };
        assert_eq!((table.as_str(), local_col.as_str(), remote_col.as_str()), ("t_activity_main", "activity_id", "id"));
        let Some(TableRule::Scoped(expense)) = m.get("t_market_activity_promoter_expense") else {
            panic!("t_market_activity_promoter_expense 必须按客户和创建人受限");
        };
        assert_eq!(expense.customer_col.as_deref(), Some("customer_code"));
        assert!(expense.customer_kind == CustomerKind::Codes);
        assert_eq!(expense.owner_col.as_deref(), Some("created_by"));
        assert!(expense.owner_kind == OwnerKind::Codes);
        let Some(TableRule::Scoped(shop)) = m.get("t_master_shop") else {
            panic!("t_master_shop 必须按门店编码受限");
        };
        assert!(shop.customer_kind == CustomerKind::ShopCodes);
        assert_eq!(shop.customer_col.as_deref(), Some("shop_code"));

        let Some(TableRule::Scoped(old_invoice)) = m.get("t_invoice_apply_header") else {
            panic!("旧开票必须登记负责人权限");
        };
        assert!(old_invoice.customer_col.is_none(), "旧开票不得按客户权限放大");
        assert_eq!(old_invoice.owner_col.as_deref(), Some("manager"));
        assert!(old_invoice.owner_kind == OwnerKind::Ids);
    }
}
