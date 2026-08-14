//! 46 张 DMS/数仓辅助表的内置权限档案（scoped 21 / via 10 / global 15）。
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
    // 无 owner 维度兜底的表只能用它：空客户集合 → 恒假条件，而不是「一个段都不注入」。
    let required_b = |c: &str| {
        TableRule::Scoped(Binding {
            customer_col: Some(c.to_string()),
            customer_kind: CustomerKind::RequiredCodes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
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
    let mut m: HashMap<String, TableRule> = HashMap::with_capacity(46);
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
    // t_account_bill_header（已退役，故不在本文件的登记清单里）：对账单页面按 `manager`
    //（历史兼容员工 ID/姓名）裁决，无法用通用 AST 单字段绑定精确表达。禁止登记成
    // customer/created_by 的近似规则，避免通用 NL2SQL 扩大可见面；
    // 精确单号由 business-lookup 在头表点查后按 manager 双形态单独裁决。
    // Java: `h.customer_code in (#customerCodes) #or h.created_by in (#employeeCodes)`
    // （DeviceInspectionHeaderMapper.java:25）—— 此前写的 `manager_code` 是错列，
    // 于是 owner 段恒不命中，巡检单只按客户集合可见（2026-08-14 对拍订正）。
    m.insert("t_device_inspection_header".into(), b("customer_code", Some("created_by"), Codes));
    m.insert("t_long_promotion_person".into(), b("customer_code", Some("manager_id"), Ids));
    // 🔴 2026-08-14 第 5 轮：Java 有 @DataScope、我们**一条档案都没有**的三张。
    // 未登记 ≠ 全量可见 —— 是 `UnregisteredTable` fail-closed：受限账号问到它们时整句被拒。
    // 方向是「答少了」（DMS 页面里看得见，问数说没权限），照样是与 DMS 不一致。
    // 旧开票列表（ApplicationListHeaderMapper.java:21，别名 invoice = t_application_list_header，
    // 见同名 XML:47）。注意与 `t_invoice_apply_header` 不是一张表：后者无注解，
    // 由 service 把数据范围员工写进 `p.managers`，故仍是 owner_only（上面那条）。
    m.insert("t_application_list_header".into(), b("customer_code", Some("manager"), Ids));
    // 设备调拨单（DeviceTransferOrderMapper.java:20）：只有客户段，且列名是 out_customer_code
    //（调出方）。与 device_ledger/disposal 同族用 `Codes` —— 空客户集合不注入，与 Java 等价
    //（见 fail_closed_tests.rs 的 `empty_segments_allows_today`）。
    m.insert("t_device_transfer_order".into(), b("out_customer_code", None, Ids));
    // 对账申请（StatementApplicationMapper.java:23）。表名是 `t_statement_apply`
    //（`@TableName` 在 StatementApplicationDO.java:17，别名 t 见 XML:31），不是类名那个。
    // owner 段用 `#employeeCodes` → 登录名族，故 `Codes` 不是 `Ids`。
    m.insert("t_statement_apply".into(), b("customer_code", Some("created_by"), Codes));
    // 🔴 Java 有注解：`@DataScope(joinSql = "t_employee.employee_id in (#employeeIds)")`
    // （EmployeeDao.java:35）。此前登记成 global —— **任何受限账号都能拿到全量花名册**
    // （姓名/登录名/部门归属），而 SENSITIVE_COLS 那 9 词只挡凭据列。
    // `t_employee_department` / `t_department` 是纯组织维表、Java 无注解，保持 global。
    m.insert("t_employee".into(), owner_only("employee_id", Ids));
    // 无 owner 维度的表（Java 模板只有 customer 段）
    for t in [
        "t_customer_device_ledger", "t_device_disposal_order", "t_shop_inspection_records",
    ] {
        m.insert(t.into(), b("customer_code", None, Ids));
    }
    // 🔴 `t_customer_balance` 不在上面那组：Java 的 joinSql 是
    // `c.customer_code in (#customerCodes) #or c.area_manager_id in (#employeeIds)`
    // （CustomerBalanceMapper.java:36），而其中的 `c` 是 XML 里
    // `LEFT JOIN t_customer c ON b.customer_code = c.customer_code` 带进来的 —— 
    // `area_manager_id` **不在 balance 表上**，所以不能写成 owner_col。
    // 登记成 via：t_customer 档案本身就是 `customer_code IN codes OR area_manager_id IN ids`，
    // EXISTS 半连接与 Java 逐字等价。此前只按 balance 自己的 customer_code 过滤，
    // 丢了 area_manager 分支 —— 区域经理看不到本该可见的余额行（答少了，2026-08-14 对拍）。
    m.insert("t_customer_balance".into(), via("t_customer", "customer_code", "customer_code"));
    // 设备**明细**（receive_item / delivery_item）仍不登记：它们挂在 t_device_requisition 下，
    // 而 via 的头表必须是 Scoped（`via_head_without_scoped_rule_is_rejected`），
    // 链式 via 表达不了；仍只由精确单号通道按同一单号读取。
    // 数仓市场费用按客户汇总；`store_code` 实际承载 DMS customer_code。
    // 🔴 必须是 `RequiredCodes` 而不是 `b()` 的 `Codes`：`Codes` 臂在客户集合为空时
    // **一个段都不 push**（`kernel/policy/inject.rs:450-465`），而这里没有 owner 维度兜底
    // → segs 空 → 不注入 → 受限身份看到**整表**。两行注释此前都写着「fail-closed」，
    // 代码却是 fail-open（2026-08-13 审计抓出）。这两张是数仓自建表、不在 Java 对拍面，
    // 收紧不会与 `judge_scope.py` 分叉。
    m.insert("ads_off_sales_cost_customer_dnf".into(), required_b("store_code"));
    // 小程序下单快照（2026-08-11 接入）：粒度统计日×客户，`store_code` 同样承载
    // DMS customer_code —— 与上面市场费用同一先例；受限身份映射不到客户编码时恒假（fail-closed）。
    m.insert("dws_mkt_app_place_order_dnf".into(), required_b("store_code"));
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
    m.insert("t_application_list_detail".into(), via("t_application_list_header", "invoice_code", "invoice_code"));
    // 设备申请单的 Java 条件挂在 **JOIN 进来的 t_customer** 上
    //（`INNER JOIN t_customer tc ON tc.customer_code = tdr.customer_code`，
    // DeviceRequisitionMapper.xml:201 + .java:31 `tc.customer_code ... #or tc.area_manager_id ...`）
    // —— 与 `t_customer_balance` 同一形态：借 t_customer 的档案，不能写成本表的 owner_col。
    // ⚠️ `xiaoyunbp`/`shebeiyy` 两个设备专职角色的全量例外**不在这条档案里**，
    // 它是 `Scope::device_unrestricted_by_role` 的布尔证明，只对精确单号通道生效
    //（business_lookup.rs:286）；通用 NL2SQL 对这两个角色仍按客户集合收窄（偏严，不越权）。
    m.insert("t_device_requisition".into(), via("t_customer", "customer_code", "customer_code"));
    // 数仓对账表只负责跨系统单号映射，行权限借 DMS 订单头表。
    m.insert("dws_fin_shipment_check_dnf".into(), via("t_sales_order", "dms_order_code", "sales_order_code"));
    // —— global：Java 无 @DataScope（维表/字典/主数据/全局报表），1:1 全量可见
    for t in [
        "t_goods", "t_goods_category", "t_department", "t_employee_department",
        "t_dict_key", "t_dict_value", "t_warehouse", "t_warehouse_manage",
        "t_winc_stock_report", "t_winc_sale_report", "t_market_total_expense",
        "t_customer_price",
        // 地区码表（region_code UNI 不扇出）：ship_dim 省份解码把它 JOIN 进模板 ——
        // 不登记 = 受限用户的省份维度查询被 fail-closed 整批拒（与 t_dict_value 同一条）
        "t_regions",
        // Doris 数仓设备维表：只含设备分类与 SKU 主数据，无用户归属，供设备订单构成下钻。
        "dim_device",
        // 业务中台 WMS 现行库存（ywzt_ods）：公司仓库存是无归属运营数据，
        // 与 t_winc_stock_report（门店进销存）同档全量可见（2026-08-11 用户指定默认库存源）。
        "scm_warehous_manage",
    ] {
        m.insert(t.into(), TableRule::Global);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三类档案的条数与**分类**是契约。少一张 scoped = 该表对受限用户从「注入条件」变成
    /// 「fail-closed 拒绝」或反之，静默改权限面。
    ///
    /// 函数名里不写数字（原名 `thirty_nine_tables_by_kind` 在 39→41 那次就已经腐烂成谎话）。
    #[test]
    fn builtin_table_counts_by_kind() {
        let m = builtin_rules();
        let scoped = m.values().filter(|r| matches!(r, TableRule::Scoped(_))).count();
        let via = m.values().filter(|r| matches!(r, TableRule::Via { .. })).count();
        let global = m.values().filter(|r| matches!(r, TableRule::Global)).count();
        assert_eq!((m.len(), scoped, via, global), (46, 21, 10, 15), "档案分类漂了");
        // 🔴 与 Java @DataScope 逐条对拍的三条（2026-08-14）：
        // ① t_employee 有注解（EmployeeDao.java:35）→ 必须 scoped，不许退回 global
        let Some(TableRule::Scoped(emp)) = m.get("t_employee") else {
            panic!("t_employee 必须 scoped —— 退回 global 就是全员花名册对受限账号可见");
        };
        assert_eq!(emp.owner_col.as_deref(), Some("employee_id"));
        assert!(emp.customer_col.is_none(), "Java 只按 employee_id，不许加客户段");
        // ② t_customer_balance 的 area_manager 分支在 t_customer 上 → 必须 via
        assert!(
            matches!(m.get("t_customer_balance"), Some(TableRule::Via { table, .. }) if table == "t_customer"),
            "余额表必须借 t_customer 的档案，否则丢掉 Java 的 area_manager 分支"
        );
        // ④ 2026-08-14 第 5 轮补登记的五张：Java 有注解而我们没档案 = 受限账号被整句拒。
        //    每条都附 Java 出处，改动必须同步改 Java 侧证据。
        for (table, owner) in [
            ("t_application_list_header", Some("manager")),      // ApplicationListHeaderMapper.java:21
            ("t_device_transfer_order", None),                   // DeviceTransferOrderMapper.java:20
            ("t_statement_apply", Some("created_by")),           // StatementApplicationMapper.java:23
        ] {
            let Some(TableRule::Scoped(bind)) = m.get(table) else {
                panic!("{table} 有 Java @DataScope，必须 scoped —— 缺档案 = 受限账号问它必被拒");
            };
            assert_eq!(bind.owner_col.as_deref(), owner, "{table} 的 owner 列与 Java joinSql 不符");
        }
        // 设备申请单的条件挂在 JOIN 进来的 t_customer 上（XML:201），与余额表同形态
        assert!(
            matches!(m.get("t_device_requisition"), Some(TableRule::Via { table, .. }) if table == "t_customer"),
            "设备申请单必须借 t_customer 的档案（Java 条件在 tc.* 上，不在本表列上）"
        );
        // ③ 巡检单的 owner 列是 created_by（DeviceInspectionHeaderMapper.java:25），不是 manager_code
        let Some(TableRule::Scoped(insp)) = m.get("t_device_inspection_header") else {
            panic!("巡检单必须 scoped");
        };
        assert_eq!(insp.owner_col.as_deref(), Some("created_by"));
        // 2026-08-11 新增：小程序下单快照按客户编码 scoped；中台库存表 global（无归属运营数据）
        let Some(TableRule::Scoped(app)) = m.get("dws_mkt_app_place_order_dnf") else {
            panic!("小程序下单快照必须按客户编码受限");
        };
        assert_eq!(app.customer_col.as_deref(), Some("store_code"));
        assert!(matches!(m.get("scm_warehous_manage"), Some(TableRule::Global)));
        // 省份解码字典必须在 global（ship_dim 省份 JOIN 它；缺了受限用户整批被拒）
        assert!(matches!(m.get("t_regions"), Some(TableRule::Global)));
        assert!(matches!(m.get("dim_device"), Some(TableRule::Global)));
        for table in [
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
        assert!(sales.owner_col.is_none(), "不得用 manager 名称模拟稳定员工 ID");
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
