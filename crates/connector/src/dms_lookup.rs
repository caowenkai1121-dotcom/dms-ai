//! DMS 生产点查的业务策略目录。
//!
//! kernel 只负责通用 SQL AST 与安全形状；具体表、业务键、索引要求和行权限合同属于
//! connector 的生产数据源适配层。调用方只能使用本目录产生的策略，不能从请求注入元数据。

use dms_kernel::sql::dms_lookup::{
    DmsIndexKind, DmsLookupKey, DmsLookupPolicy, DmsLookupRegistry,
};

pub const SALES: DmsLookupPolicy =
    DmsLookupPolicy::new("t_sales_order", &["sales_order_code"]);
pub const SALES_BY_CUSTOMER: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_sales_order", &["customer_code"]);
pub const SALES_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_sales_order_detail", &["sales_order_code"]);
pub const AFTER: DmsLookupPolicy =
    DmsLookupPolicy::new("t_after_sales_order_header", &["after_sales_code"]);
pub const AFTER_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_after_sales_order_detail", &["after_sales_code"]);
pub const BILL: DmsLookupPolicy =
    DmsLookupPolicy::new("t_account_bill_header", &["bill_code"]);
pub const BILL_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_account_bill_detail", &["bill_code"]);
pub const DEVICE: DmsLookupPolicy =
    DmsLookupPolicy::new("t_device_requisition", &["requisition_code"]);
pub const DEVICE_RECEIVE: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_device_receive_item", &["requisition_code"]);
pub const DEVICE_DELIVERY: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_device_delivery_item", &["requisition_code"]);
pub const INVOICE: DmsLookupPolicy =
    DmsLookupPolicy::new("t_invoice_apply_header", &["invoice_code"]);
pub const INVOICE_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_invoice_apply_detail", &["invoice_code"]);
pub const INVOICE_NEW: DmsLookupPolicy =
    DmsLookupPolicy::new("t_invoice_new_apply_header", &["invoice_code"]);
pub const INVOICE_NEW_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_invoice_new_apply_detail", &["invoice_code"]);
pub const CUSTOMER: DmsLookupPolicy =
    DmsLookupPolicy::new("t_customer", &["customer_code"]);
pub const GOODS: DmsLookupPolicy = DmsLookupPolicy::new("t_goods", &["goods_code"]);

const SCOPED_POLICIES: &[DmsLookupPolicy] = &[
    SALES,
    AFTER,
    BILL,
    DEVICE,
    INVOICE,
    INVOICE_NEW,
    CUSTOMER,
    GOODS,
];

const REGISTERED_KEYS: &[DmsLookupKey] = &[
    DmsLookupKey::new("t_sales_order", "sales_order_code", DmsIndexKind::Unique),
    DmsLookupKey::new("t_sales_order", "customer_code", DmsIndexKind::Leading),
    DmsLookupKey::new("t_sales_order_detail", "sales_order_code", DmsIndexKind::Leading),
    DmsLookupKey::new("t_after_sales_order_header", "after_sales_code", DmsIndexKind::Unique),
    DmsLookupKey::new("t_after_sales_order_detail", "after_sales_code", DmsIndexKind::Leading),
    DmsLookupKey::new("t_account_bill_header", "bill_code", DmsIndexKind::Unique),
    DmsLookupKey::new("t_account_bill_detail", "bill_code", DmsIndexKind::Leading),
    DmsLookupKey::new("t_device_requisition", "requisition_code", DmsIndexKind::Unique),
    DmsLookupKey::new("t_device_receive_item", "requisition_code", DmsIndexKind::Leading),
    DmsLookupKey::new("t_device_delivery_item", "requisition_code", DmsIndexKind::Leading),
    DmsLookupKey::new("t_invoice_apply_header", "invoice_code", DmsIndexKind::Unique),
    DmsLookupKey::new("t_invoice_apply_detail", "invoice_code", DmsIndexKind::Leading),
    DmsLookupKey::new("t_invoice_new_apply_header", "invoice_code", DmsIndexKind::Unique),
    DmsLookupKey::new("t_invoice_new_apply_detail", "invoice_code", DmsIndexKind::Leading),
    DmsLookupKey::new("t_customer", "customer_code", DmsIndexKind::Unique),
    DmsLookupKey::new("t_goods", "goods_code", DmsIndexKind::Unique),
];

const UNCONTRACTED_TABLES: &[&str] = &["t_winc_purchase_transfer"];

pub static REGISTRY: DmsLookupRegistry =
    DmsLookupRegistry::new(SCOPED_POLICIES, REGISTERED_KEYS, UNCONTRACTED_TABLES);

pub fn registered_lookup_keys(
) -> impl Iterator<Item = (&'static str, &'static str, DmsIndexKind)> {
    REGISTRY.registered_lookup_keys()
}

pub fn registered_lookup_kind(table: &str, column: &str) -> Option<DmsIndexKind> {
    REGISTRY.registered_lookup_kind(table, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scoped_policy_key_is_registered() {
        for policy in REGISTRY.scoped_policies() {
            for column in policy.lookup_cols() {
                assert!(
                    registered_lookup_kind(policy.table(), column).is_some(),
                    "点查策略 ({}, {}) 未登记物理索引要求",
                    policy.table(),
                    column
                );
            }
        }
    }

    #[test]
    fn uncontracted_table_is_not_registered() {
        assert!(registered_lookup_keys()
            .all(|(table, _, _)| table != "t_winc_purchase_transfer"));
    }
}
