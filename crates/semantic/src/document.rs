//! DMS 单据注册表：单号形状、主明细关系和固定查询投影的唯一事实源。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    WarehouseShipment,
    SalesOrder,
    AfterSales,
    AccountBill,
    DeviceRequisition,
    InvoiceApply,
    InvoiceApplyNew,
    PurchaseTransfer,
    ShopRequisition,
    ShopShipment,
    ShopReturn,
    Voucher,
    StockAdjustment,
}

#[derive(Debug, Clone, Copy)]
pub struct DocumentDetail {
    pub table: &'static str,
    pub code_col: &'static str,
    pub projection: &'static str,
    pub deleted_flag: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct DocumentSource {
    pub header_table: &'static str,
    pub header_code_cols: &'static [&'static str],
    pub header_projection: &'static str,
    pub header_deleted_flag: bool,
    pub details: &'static [DocumentDetail],
}

#[derive(Debug)]
pub struct DocumentFamily {
    pub kind: DocumentKind,
    pub code: &'static str,
    pub name: &'static str,
    pub prefixes: &'static [&'static str],
    // Compatibility fields consumed by seed/graph. Runtime SQL must use production/warehouse;
    // `warehouse_available` is only a derived persistence mirror, never a routing decision.
    pub header_table: &'static str,
    pub header_code_col: &'static str,
    pub details: &'static [(&'static str, &'static str)],
    pub evidence: &'static str,
    pub warehouse_available: bool,
    pub production: Option<&'static DocumentSource>,
    pub warehouse: Option<&'static DocumentSource>,
}

impl DocumentFamily {
    pub fn source(&self, warehouse: bool) -> Option<&'static DocumentSource> {
        if warehouse { self.warehouse } else { self.production }
    }
}

const SALES_DETAIL: DocumentDetail = DocumentDetail {
    table: "t_sales_order_detail",
    code_col: "sales_order_code",
    projection: "sales_order_code, item_code, sku_code, sku_name, goods_specification_name, item_type, price, sales_price, actual_delivery_quantity, actual_receive_quantity, goods_amount, amount, delivery_time, receive_time, is_gift",
    deleted_flag: true,
};
const SALES_DORIS_DETAIL: DocumentDetail = DocumentDetail { table: "dms_ods.t_sales_order_detail", ..SALES_DETAIL };
const AFTER_DETAIL: DocumentDetail = DocumentDetail {
    table: "t_after_sales_order_detail",
    code_col: "after_sales_code",
    projection: "after_sales_code, item_code, sales_order_code, sales_order_item, sku_code, sku_name, goods_specification_name, price, actual_delivery_quantity, actual_receive_quantity, requested_return_qty_box, requested_return_qty_bag, returned_qty_box, returned_qty_bag, goods_amount, refund_amount, amount, is_gift",
    deleted_flag: true,
};
const AFTER_DORIS_DETAIL: DocumentDetail = DocumentDetail { table: "dms_ods.t_after_sales_order_detail", ..AFTER_DETAIL };
const BILL_DETAIL: DocumentDetail = DocumentDetail {
    table: "t_account_bill_detail", code_col: "bill_code",
    projection: "bill_code, bill_item_code, delivery_customer_code, delivery_customer_name, receipt_date, ref_code, ref_item_code, amount_type, business_type, sku_code, sku_name, sku_unit, sku_spec, price, quantity, balance, income, expenditure, order_status, order_type, source_order_code",
    deleted_flag: true,
};
const DEVICE_DETAILS: &[DocumentDetail] = &[
    DocumentDetail {
        table: "t_device_receive_item", code_col: "requisition_code",
        projection: "requisition_code, warehouse_name, sku_code, sku_name, price, quantity, amount, actual_deduct_amount, amount_invoice, amount_no_invoice, wms_code, sales_order_code",
        deleted_flag: true,
    },
    DocumentDetail {
        table: "t_device_delivery_item", code_col: "requisition_code",
        // 生产实表核验（2026-08-08，MySQL 8.0.28）：无 receive_item_index/ledger_id/serial_number
        projection: "requisition_code, receive_item_id, sku_code, sku_name, quantity, amount, store_code",
        deleted_flag: true,
    },
];
const INVOICE_DETAIL: DocumentDetail = DocumentDetail {
    table: "t_invoice_apply_detail", code_col: "invoice_code",
    projection: "invoice_code, invoice_item_code, bill_code, bill_item_code, invoice_amount, tax_rate, tax_amount, discount_amount, erp_id",
    deleted_flag: true,
};
const INVOICE_NEW_DETAIL: DocumentDetail = DocumentDetail {
    table: "t_invoice_new_apply_detail", code_col: "invoice_code",
    projection: "invoice_code, invoice_item_code, row_num, ref_order_code, ref_line_num, item_code, item_name, actual_delivery_quantity, price, goods_amount, pay_invoicable_amount, pay_market_amount, tax_rate, tax_amount, erp_id",
    deleted_flag: true,
};
const SALES_PROD: DocumentSource = DocumentSource {
    header_table: "t_sales_order", header_code_cols: &["sales_order_code"],
    header_projection: "sales_order_code, order_time, order_type, customer_code, customer_name, owner_manager, total_quantity, total_amount, actual_paid_amount, paid_status, order_status, after_sales_status, source_code, delivery_warehouse_code, order_remark",
    header_deleted_flag: true, details: &[SALES_DETAIL],
};
const SALES_DORIS: DocumentSource = DocumentSource {
    header_table: "dms_ods.t_sales_order", header_code_cols: &["sales_order_code"],
    header_projection: SALES_PROD.header_projection, header_deleted_flag: true,
    details: &[SALES_DORIS_DETAIL],
};
const AFTER_PROD: DocumentSource = DocumentSource {
    header_table: "t_after_sales_order_header", header_code_cols: &["after_sales_code"],
    header_projection: "after_sales_code, sales_order_code, after_sales_time, after_sales_type, customer_code, customer_name, owner_manager, total_quantity, total_amount, refund_total_quantity, refund_amount, actual_refund_amount, after_sales_status",
    header_deleted_flag: true, details: &[AFTER_DETAIL],
};
const AFTER_DORIS: DocumentSource = DocumentSource {
    header_table: "dms_ods.t_after_sales_order_header", header_code_cols: &["after_sales_code"],
    header_projection: AFTER_PROD.header_projection, header_deleted_flag: true,
    details: &[AFTER_DORIS_DETAIL],
};
const BILL_PROD: DocumentSource = DocumentSource {
    header_table: "t_account_bill_header", header_code_cols: &["bill_code"],
    header_projection: "bill_code, bill_date, customer_code, customer_name, manager, bill_status, status, balance, can_invoice_amount, invoice_status, account_mode, period_id",
    header_deleted_flag: true, details: &[BILL_DETAIL],
};
const DEVICE_PROD: DocumentSource = DocumentSource {
    header_table: "t_device_requisition", header_code_cols: &["requisition_code"],
    header_projection: "requisition_code, request_date, customer_code, customer_name, customer_mode, requisition_status, device_type, delivery_type, deposit_pay_type, deposit_amount, actual_deduct_amount, returned_deposit_amount, purchase_amount, doc_status, approval_status, actual_delivery_time",
    header_deleted_flag: true, details: DEVICE_DETAILS,
};
const INVOICE_PROD: DocumentSource = DocumentSource {
    header_table: "t_invoice_apply_header", header_code_cols: &["invoice_code"],
    header_projection: "invoice_code, invoice_status, invoice_amount, invoice_type, apply_time, apply_by, invoice_time, tax_amount, customer_code, manager, red_invoice_code, discount_amount, belong_company",
    header_deleted_flag: true, details: &[INVOICE_DETAIL],
};
const INVOICE_DORIS: DocumentSource = DocumentSource {
    header_table: "dms_ods.t_invoice_apply_header", header_code_cols: &["invoice_code"],
    header_projection: INVOICE_PROD.header_projection, header_deleted_flag: true, details: &[],
};
const INVOICE_NEW_PROD: DocumentSource = DocumentSource {
    header_table: "t_invoice_new_apply_header", header_code_cols: &["invoice_code"],
    header_projection: "erp_invoice_code, invoice_code, invoice_status, invoice_amount, invoice_type, apply_time, apply_by, invoice_time, tax_amount, customer_code, manager, red_invoice_code, discount_amount, belong_company, belong_company_name",
    header_deleted_flag: true, details: &[INVOICE_NEW_DETAIL],
};
/// Doris 只有头表镜像（明细表结构与生产不同、未核验）：明细留空，
/// 由 `needs_production_detail_fallback` 走生产注册表明细点查。
const DEVICE_DORIS: DocumentSource = DocumentSource {
    header_table: "dms_ods.t_device_requisition", header_code_cols: &["requisition_code"],
    header_projection: DEVICE_PROD.header_projection, header_deleted_flag: true,
    details: &[],
};
const INVOICE_NEW_DORIS: DocumentSource = DocumentSource {
    header_table: "dms_ods.t_invoice_new_apply_header", header_code_cols: &["invoice_code"],
    header_projection: INVOICE_NEW_PROD.header_projection, header_deleted_flag: true, details: &[],
};
const SHIPMENT_DORIS: DocumentSource = DocumentSource {
    header_table: "sales_dw.dws_fin_shipment_check_dnf",
    header_code_cols: &["ywzt_order", "base_ref_order"],
    header_projection: "ywzt_order, base_ref_order, dms_order_code, ship_at, order_type, store_name, dms_lines, dms_amount, ywzt_lines, ywzt_amount, base_lines, base_amount, lines_difference, amount_difference, change_type",
    header_deleted_flag: false, details: &[],
};

const SALES_BIND: &[(&str, &str)] = &[("t_sales_order_detail", "sales_order_code")];
const AFTER_BIND: &[(&str, &str)] = &[("t_after_sales_order_detail", "after_sales_code")];
const BILL_BIND: &[(&str, &str)] = &[("t_account_bill_detail", "bill_code")];
const DEVICE_BIND: &[(&str, &str)] = &[("t_device_receive_item", "requisition_code"), ("t_device_delivery_item", "requisition_code")];
const INVOICE_BIND: &[(&str, &str)] = &[("t_invoice_apply_detail", "invoice_code")];
const INVOICE_NEW_BIND: &[(&str, &str)] = &[("t_invoice_new_apply_detail", "invoice_code")];
const SHOP_ORDER_BIND: &[(&str, &str)] = &[("t_shop_order_detail", "order_no")];
const SHOP_SHIPMENT_BIND: &[(&str, &str)] = &[("t_shop_order_header", "shipment_no"), ("t_shop_order_detail", "order_no")];
const SHOP_RETURN_BIND: &[(&str, &str)] = &[("t_shop_order_return_detail", "order_no")];
const VOUCHER_BIND: &[(&str, &str)] = &[("t_voucher_detail", "voucher_code")];
const ADJ_BIND: &[(&str, &str)] = &[("t_wms_adj_detail", "adjust_code")];
const NONE: &[(&str, &str)] = &[];

macro_rules! family {
    ($kind:ident, $code:literal, $name:literal, $prefixes:expr, $table:literal, $key:literal,
     $details:expr, $evidence:literal, $production:expr, $warehouse:expr) => {
        DocumentFamily {
            kind: DocumentKind::$kind, code: $code, name: $name, prefixes: $prefixes,
            header_table: $table, header_code_col: $key, details: $details, evidence: $evidence,
            warehouse_available: has_source($warehouse), production: $production, warehouse: $warehouse,
        }
    };
}

const fn has_source(source: Option<&DocumentSource>) -> bool {
    source.is_some()
}

pub const DOCUMENT_FAMILIES: &[DocumentFamily] = &[
    family!(WarehouseShipment, "warehouse_shipment_check", "数仓发货拆单映射", &["HJXH-DSO", "HJXH-DXO", "HJXH-SO", "HJXH-XO"], "sales_dw.dws_fin_shipment_check_dnf", "ywzt_order|base_ref_order", SALES_BIND, "Doris dws_fin_shipment_check_dnf 拆单映射；DMS 订单只引用 dms_ods 完整表名", None, Some(&SHIPMENT_DORIS)),
    family!(SalesOrder, "sales_order", "销售订单", &["HJXH-DXO", "HJXH-DSO", "HJXH-XO", "HJXH-SO"], "t_sales_order", "sales_order_code", SALES_BIND, "SalesOrderHeaderDO + SalseOrderDetailMapper.xml", Some(&SALES_PROD), Some(&SALES_DORIS)),
    family!(AfterSales, "after_sales_order", "售后订单", &["HJXH-DRO", "HJXH-RO"], "t_after_sales_order_header", "after_sales_code", AFTER_BIND, "AfterSalesOrderDO/AfterSalesOrderDetailDO", Some(&AFTER_PROD), Some(&AFTER_DORIS)),
    family!(AccountBill, "account_bill", "客户对账单", &["HJXH-DZD", "HJXH-ZD"], "t_account_bill_header", "bill_code", BILL_BIND, "AccountBillHeaderDO/AccountBillDetailDO", Some(&BILL_PROD), None),
    family!(DeviceRequisition, "device_requisition", "设备需求单", &["HJXH_XQ", "DEV_XQ"], "t_device_requisition", "requisition_code", DEVICE_BIND, "DeviceRequisition/DeviceReceiveItem/DeviceDeliveryItem", Some(&DEVICE_PROD), Some(&DEVICE_DORIS)),
    family!(InvoiceApply, "invoice_apply", "开票申请单（旧流程）", &["IO"], "t_invoice_apply_header", "invoice_code", INVOICE_BIND, "InvoiceApplyHeaderDo/InvoiceApplyDetailDo", Some(&INVOICE_PROD), Some(&INVOICE_DORIS)),
    family!(InvoiceApplyNew, "invoice_apply_new", "开票申请单（新流程）", &["SQ"], "t_invoice_new_apply_header", "invoice_code", INVOICE_NEW_BIND, "InvoiceApplyNewHeaderDo/InvoiceApplyNewDetailDo", Some(&INVOICE_NEW_PROD), Some(&INVOICE_NEW_DORIS)),
    family!(PurchaseTransfer, "purchase_transfer", "采购调拨单", &["CG", "SPC-"], "t_winc_purchase_transfer", "bill_code", NONE, "WincPurchaseTransferDO.bill_code；生产权限未证明", None, None),
    family!(ShopRequisition, "shop_requisition", "门店要货单", &["SHOP_YH"], "t_shop_order_header", "order_no", SHOP_ORDER_BIND, "SHOP_YH 流水号；生产权限未证明", None, None),
    family!(ShopShipment, "shop_shipment", "门店配送单", &["SHOP_PH"], "t_shop_shipment_order", "shipment_no", SHOP_SHIPMENT_BIND, "SHOP_PH 流水号；生产权限未证明", None, None),
    family!(ShopReturn, "shop_return", "门店退货单", &["SHOP_TH"], "t_shop_order_return_header", "order_no", SHOP_RETURN_BIND, "SHOP_TH 流水号；生产权限未证明", None, None),
    family!(Voucher, "voucher", "库存凭证单", &["PZ"], "t_voucher_header", "voucher_code", VOUCHER_BIND, "PZ 流水号；生产权限未证明", None, None),
    family!(StockAdjustment, "stock_adjustment", "库存调整单", &["SHOP_TZ"], "t_wms_adj_header", "adjust_code", ADJ_BIND, "SHOP_TZ 流水号；生产权限未证明", None, None),
];

#[derive(Debug)]
pub struct ResolvedDocument {
    pub code: String,
    pub family: &'static DocumentFamily,
}

pub fn resolve_document(question: &str, warehouse: bool) -> Option<ResolvedDocument> {
    ascii_candidates(question).find_map(|candidate| resolve_code(candidate, warehouse))
}

pub fn resolve_code(raw: &str, warehouse: bool) -> Option<ResolvedDocument> {
    let code = raw.to_ascii_uppercase();
    if code.len() < 6 || !code.bytes().all(valid_code_byte) {
        return None;
    }
    if warehouse_alias_base(&code).is_some() {
        return warehouse.then(|| resolved(code, DocumentKind::WarehouseShipment)).flatten();
    }
    let kind = if sales_shape(&code) { DocumentKind::SalesOrder }
    else if dated_or_legacy(&code, "HJXH-DRO", "HJXH-RO") { DocumentKind::AfterSales }
    else if dated_or_legacy(&code, "HJXH-DZD", "HJXH-ZD") { DocumentKind::AccountBill }
    else if dated_serial(&code, "HJXH_XQ", 3, 12) || numeric(&code, "DEV_XQ", 3, 20) { DocumentKind::DeviceRequisition }
    else if numeric(&code, "IO", 6, 24) { DocumentKind::InvoiceApply }
    else if numeric(&code, "SQ", 6, 24) { DocumentKind::InvoiceApplyNew }
    else if numeric(&code, "CG", 6, 24) || spc_shape(&code) { DocumentKind::PurchaseTransfer }
    else if local_shape(&code, "SHOP_YH") { DocumentKind::ShopRequisition }
    else if local_shape(&code, "SHOP_PH") { DocumentKind::ShopShipment }
    else if local_shape(&code, "SHOP_TH") { DocumentKind::ShopReturn }
    else if local_shape(&code, "PZ") { DocumentKind::Voucher }
    else if local_shape(&code, "SHOP_TZ") { DocumentKind::StockAdjustment }
    else { return None };
    resolved(code, kind)
}

fn resolved(code: String, kind: DocumentKind) -> Option<ResolvedDocument> {
    DOCUMENT_FAMILIES.iter().find(|family| family.kind == kind)
        .map(|family| ResolvedDocument { code, family })
}

fn ascii_candidates(question: &str) -> impl Iterator<Item = &str> {
    question.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '*')))
        .filter(|candidate| !candidate.is_empty())
}

fn valid_code_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'*')
}

fn numeric(code: &str, prefix: &str, min: usize, max: usize) -> bool {
    code.strip_prefix(prefix).is_some_and(|rest| {
        (min..=max).contains(&rest.len()) && rest.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn dated_serial(code: &str, prefix: &str, min: usize, max: usize) -> bool {
    code.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() >= 8 + min && rest.len() <= 8 + max
            && valid_date(&rest[..8]) && rest[8..].bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn dated_or_legacy(code: &str, dated: &str, legacy: &str) -> bool {
    dated_serial(code, dated, 3, 12) || numeric(code, legacy, 3, 20)
}

fn sales_shape(code: &str) -> bool {
    dated_serial(code, "HJXH-DXO", 3, 12) || dated_serial(code, "HJXH-DSO", 3, 12)
        || numeric(code, "HJXH-XO", 3, 20) || numeric(code, "HJXH-SO", 3, 20)
}

fn local_shape(code: &str, prefix: &str) -> bool {
    dated_serial(code, prefix, 5, 19)
}

fn spc_shape(code: &str) -> bool {
    let Some(rest) = code.strip_prefix("SPC-") else { return false };
    let Some((date, serial)) = rest.split_once('-') else { return false };
    valid_date(date) && (1..=12).contains(&serial.len())
        && serial.bytes().all(|byte| byte.is_ascii_digit())
}

fn warehouse_alias_base(code: &str) -> Option<&str> {
    let (base, suffix) = code.rsplit_once('*').or_else(|| code.rsplit_once('_'))?;
    if base.contains('*') || base.contains('_') || suffix.is_empty()
        || suffix.len() > 6 || !suffix.bytes().all(|byte| byte.is_ascii_digit())
        || !sales_shape(base)
    {
        return None;
    }
    Some(base)
}

fn valid_date(date: &str) -> bool {
    if date.len() != 8 || !date.bytes().all(|byte| byte.is_ascii_digit()) { return false; }
    let year = date[..4].parse::<u32>().unwrap_or_default();
    let month = date[4..6].parse::<u32>().unwrap_or_default();
    let day = date[6..].parse::<u32>().unwrap_or_default();
    if !(2000..=2999).contains(&year) || !(1..=12).contains(&month) { return false; }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max = match month { 2 if leap => 29, 2 => 28, 4 | 6 | 9 | 11 => 30, _ => 31 };
    (1..=max).contains(&day)
}

pub fn detail_table_names(family: &DocumentFamily) -> String {
    family.details.iter().map(|(table, _)| *table).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_handles_chinese_adjacency_and_all_registered_families() {
        for (code, kind) in [
            ("HJXH-DXO2026072300384", DocumentKind::SalesOrder),
            ("HJXH-DRO2026072300047", DocumentKind::AfterSales),
            ("HJXH-DZD20261230000261", DocumentKind::AccountBill),
            ("HJXH_XQ20260804100098", DocumentKind::DeviceRequisition),
            ("IO2025123456", DocumentKind::InvoiceApply),
            ("SQ2026052345", DocumentKind::InvoiceApplyNew),
            ("SPC-20260718-8", DocumentKind::PurchaseTransfer),
            ("SHOP_YH20260805100001", DocumentKind::ShopRequisition),
            ("SHOP_PH20260805100005", DocumentKind::ShopShipment),
            ("SHOP_TH20260805100002", DocumentKind::ShopReturn),
            ("PZ20260805100003", DocumentKind::Voucher),
            ("SHOP_TZ20260805100004", DocumentKind::StockAdjustment),
        ] {
            assert_eq!(resolve_document(&format!("查{code}这单"), false).map(|x| x.family.kind), Some(kind));
        }
    }

    #[test]
    fn malformed_known_prefixes_are_rejected() {
        for code in ["SPC-", "SPC-20261301-1", "HJXH-DXO202602300001", "DEV_XQ_IDEM_001",
                     "SHOP_YH20261301100001", "PZ202608051234"] {
            assert!(resolve_code(code, false).is_none(), "{code}");
        }
        assert!(resolve_code("HJXH-DSO2026080400071_2", false).is_none());
        assert_eq!(resolve_code("HJXH-DSO2026080400071_2", true).unwrap().family.kind,
                   DocumentKind::WarehouseShipment);
    }

    #[test]
    fn source_contracts_are_explicit_and_projection_only() {
        for family in DOCUMENT_FAMILIES {
            assert_eq!(family.warehouse_available, family.warehouse.is_some());
            for source in [family.production, family.warehouse].into_iter().flatten() {
                assert!(!source.header_projection.contains('*'));
                if family.warehouse.is_some_and(|candidate| std::ptr::eq(candidate, source)) {
                    assert!(source.header_table.contains('.'));
                    assert!(source.details.iter().all(|detail| detail.table.contains('.')));
                }
                assert!(source.details.iter().all(|detail| !detail.projection.contains('*')));
            }
        }
    }

    #[test]
    fn production_sources_exist_only_for_proven_permission_families() {
        for family in DOCUMENT_FAMILIES {
            let expected = matches!(
                family.kind,
                DocumentKind::SalesOrder
                    | DocumentKind::AfterSales
                    | DocumentKind::AccountBill
                    | DocumentKind::DeviceRequisition
                    | DocumentKind::InvoiceApply
                    | DocumentKind::InvoiceApplyNew
            );
            assert_eq!(family.production.is_some(), expected, "{}", family.header_table);
        }
    }
}
