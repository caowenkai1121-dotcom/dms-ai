//! 生产 DMS 业务库轻查询兜底。
//!
//! 这里不是第二套 NL2SQL：只识别登记单号和显式客户/商品代码。
//! 每条 SQL 都是单表点查，先过 `gate_dms_lookup`，再走 connector 固定 2 秒超时与 50 行上限。

use std::collections::HashMap;

use dms_connector::source::RowSet;
use dms_kernel::present::{Block, ViewSpec};
use dms_kernel::sql::dms_lookup::DmsLookupPolicy;
use dms_kernel::BoxFut;
use dms_semantic::document::{resolve_document, DocumentFamily, DocumentKind};

use crate::answerers::Answerer;
use crate::ctx::{AskCtx, AskResult, SupplementalResult};
use crate::gate::gate_dms_lookup;

const SALES: DmsLookupPolicy = DmsLookupPolicy::new("t_sales_order", &["sales_order_code"]);
const SALES_BY_CUSTOMER: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_sales_order", &["customer_code"]);
const SALES_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_sales_order_detail", &["sales_order_code"]);
const AFTER: DmsLookupPolicy =
    DmsLookupPolicy::new("t_after_sales_order_header", &["after_sales_code"]);
const AFTER_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_after_sales_order_detail", &["after_sales_code"]);
const BILL: DmsLookupPolicy = DmsLookupPolicy::new("t_account_bill_header", &["bill_code"]);
const BILL_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_account_bill_detail", &["bill_code"]);
const DEVICE: DmsLookupPolicy =
    DmsLookupPolicy::new("t_device_requisition", &["requisition_code"]);
const DEVICE_RECEIVE: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_device_receive_item", &["requisition_code"]);
const DEVICE_DELIVERY: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_device_delivery_item", &["requisition_code"]);
const INVOICE: DmsLookupPolicy =
    DmsLookupPolicy::new("t_invoice_apply_header", &["invoice_code"]);
const INVOICE_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_invoice_apply_detail", &["invoice_code"]);
const INVOICE_NEW: DmsLookupPolicy =
    DmsLookupPolicy::new("t_invoice_new_apply_header", &["invoice_code"]);
const INVOICE_NEW_DETAIL: DmsLookupPolicy =
    DmsLookupPolicy::indexed("t_invoice_new_apply_detail", &["invoice_code"]);
const CUSTOMER: DmsLookupPolicy = DmsLookupPolicy::new("t_customer", &["customer_code"]);
const GOODS: DmsLookupPolicy = DmsLookupPolicy::new("t_goods", &["goods_code"]);

/// `table_result` 的「永不截断」档：SQL 自带 LIMIT 1 时行数判据只是兜底形状（usize::MAX 太魔法）。
const NO_TRUNC: usize = usize::MAX;

pub struct BusinessLookupAnswerer;

impl BusinessLookupAnswerer {
    pub fn new() -> Self {
        Self
    }
}

impl Answerer for BusinessLookupAnswerer {
    fn route(&self) -> &'static str {
        "business-lookup"
    }

    fn accept(&self, cx: &AskCtx<'_>) -> bool {
        // 数仓模式用于补未同步单据；生产 MySQL 模式由 ask_single 独占调用，禁止其它分析路由。
        cx.source.is_warehouse() || cx.ds == dms_semantic::registry::datasource::DMS_DS_ID
    }

    fn answer<'a>(&'a self, cx: &'a AskCtx<'a>) -> BoxFut<'a, anyhow::Result<Option<AskResult>>> {
        Box::pin(async move {
            if let Some(doc) = resolve_document(cx.question, false) {
                return document(cx, doc.family, &doc.code).await;
            }
            if let Some((kind, value)) = entity_query(cx.question) {
                return entity(cx, kind, &value).await;
            }
            Ok(None)
        })
    }
}

async fn document(
    cx: &AskCtx<'_>,
    family: &'static DocumentFamily,
    code: &str,
) -> anyhow::Result<Option<AskResult>> {
    let Some(source) = family.production else {
        return Ok(Some(document_registry_answer(
            cx,
            family,
            code,
            "已识别单据类型；当前没有通过生产轻点查安全核验，未访问业务库。",
        )));
    };
    let Some(header_policy) = header_policy(family.kind) else {
        return Ok(Some(document_registry_answer(
            cx,
            family,
            code,
            "已识别单据类型；当前未证明主表索引与数据权限，未访问业务库。",
        )));
    };
    let Some(header_code_col) = source.header_code_cols.first() else { return Ok(None) };
    let code = exact_literal(code)?; // 注册表识别的单号形状保证不可达（纯防御，fail-closed 不回落）
    let deleted = source.header_deleted_flag.then_some(" AND deleted_flag = 0").unwrap_or("");
    let header_sql = format!(
        "SELECT {} FROM {} WHERE {} = '{}'{} LIMIT 1",
        source.header_projection, source.header_table, header_code_col, code, deleted
    );
    let header = lookup(cx, &header_sql, header_policy).await?;
    if header.rows.is_empty() || !document_row_visible(cx, family.kind, &header).await? {
        return Ok(Some(document_registry_answer(
            cx,
            family,
            code.as_str(),
            "已准确识别单据类型、主表与明细表；当前账号下未返回记录。为避免泄露数据存在性，系统不区分单号不存在与无查看权限。",
        )));
    }

    let mut executed = vec![header_sql];
    // 明细族并发（彼此无依赖，串行时每族白付一个 RTT）：`join_all` 保序，`executed` 与串行同序。
    // 注册表声明了明细却没有点查策略的表：跳过必须留痕（原来是静默 continue）。
    let jobs: Vec<_> = source
        .details
        .iter()
        .filter_map(|detail| match detail_policy(detail.table) {
            Some(policy) => {
                let deleted = detail.deleted_flag.then_some(" AND deleted_flag = 0").unwrap_or("");
                let sql = format!(
                    "SELECT {} FROM {} WHERE {} = '{code}'{} LIMIT 50",
                    detail.projection, detail.table, detail.code_col, deleted
                );
                Some((detail.table, sql, policy))
            }
            None => {
                tracing::warn!(table = %detail.table, "明细表无点查策略，跳过");
                None
            }
        })
        .collect();
    let results =
        futures::future::join_all(jobs.iter().map(|(_, sql, policy)| lookup(cx, sql, policy))).await;
    let mut detail_sets = Vec::new();
    for ((table, sql, _), rows) in jobs.into_iter().zip(results) {
        let rows = rows?;
        executed.push(sql);
        if !rows.rows.is_empty() {
            detail_sets.push((table, rows));
        }
    }
    let detail_truncated = detail_sets.iter().any(|(_, rs)| rs.rows.len() >= 50);
    let details = merge_rowsets(detail_sets);
    Ok(Some(document_answer(
        cx,
        family,
        code.as_str(),
        executed.join(";\n"),
        header,
        details,
        detail_truncated,
    )))
}

#[derive(Clone, Copy)]
enum EntityKind {
    Customer,
    Goods,
}

async fn entity(
    cx: &AskCtx<'_>,
    kind: EntityKind,
    value: &str,
) -> anyhow::Result<Option<AskResult>> {
    let value = exact_literal(value)?; // 上游 `entity_query` 已 `valid_entity_value` 过滤，此行纯防御（fail-closed）
    let sql = match kind {
        EntityKind::Customer => format!(
            "SELECT customer_code, customer_name, customer_short_name, province, city, district, \
                    channel_category, customer_level, customer_type, business_type, customer_group, \
                    customer_status, customer_channel, business_channel, owner_master, belong_company, \
                    department_id, is_enable, area_manager_id, sales_channel, sales_group \
             FROM t_customer WHERE customer_code = '{value}' AND deleted_flag = 0 LIMIT 1"
        ),
        EntityKind::Goods => format!(
            "SELECT goods_code, goods_name, goods_short_name, goods_category_name, goods_category_code, \
                    brand_code, brand_name, on_sale, bom_code, bom_name, is_bom, bom_type, frozen_state, \
                    invoice_unit, group_number, materialtype, sku_group, sku_class, new_product_tag, net_content \
             FROM t_goods WHERE goods_code = '{value}' AND deleted_flag = 0 LIMIT 1"
        ),
    };
    let policy = match kind {
        EntityKind::Customer => &CUSTOMER,
        EntityKind::Goods => &GOODS,
    };
    let mut rows = lookup(cx, &sql, policy).await?;
    if matches!(kind, EntityKind::Customer) {
        // 可见集合一次问答算一次（主档行过滤 + 订单补充过滤两回；Goods 路径不构建）
        let sets = VisibleSets::of(cx);
        retain_visible_rows(cx, &sets, &mut rows, Visibility::CustomerOrEmployee("area_manager_id"));
        if rows.rows.len() == 1 {
            return Ok(Some(
                customer_answer(cx, &sets, sql, rows, "已识别客户主档；下方补充该客户的订单概览。")
                    .await?,
            ));
        }
    }
    if rows.rows.is_empty() {
        return Ok(None);
    }
    let mut answer = table_result(cx, sql, rows, 1); // SQL 自带 LIMIT 1，limit 只是兜底形状
    if matches!(kind, EntityKind::Goods) {
        answer.view.insight = Some(
            "已识别商品主档。生产 DMS 只允许商品编码单表点查；销售、客户与订单关联请在 Doris 数仓中查询。".into(),
        );
    }
    // （两类实体 SQL 都 LIMIT 1，`row_count > 1` 不可达 —— 原来的多候选分支是死代码，已删；
    //  多候选形态走 entity.rs 的 candidate_card，别在这里加回来。）
    Ok(Some(answer))
}

async fn customer_answer(
    cx: &AskCtx<'_>,
    sets: &VisibleSets,
    main_sql: String,
    customer_rows: RowSet,
    insight: &str,
) -> anyhow::Result<AskResult> {
    let customer_code = cell_by_name(&customer_rows, 0, "customer_code")
        .and_then(value_text)
        .map(str::to_string);
    let mut answer = table_result(cx, main_sql, customer_rows, NO_TRUNC);
    answer.view.insight = Some(insight.into());
    let Some(customer_code) = customer_code.filter(|code| valid_entity_value(code)) else {
        return Ok(answer);
    };
    let customer_scope_allows_orders = cx.scope.unrestricted_by_role()
        || sets
            .customers
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&customer_code));
    if !customer_scope_allows_orders {
        return Ok(answer);
    }
    let sql = format!(
        "SELECT sales_order_code, order_time, order_type, customer_code, customer_name, owner_manager, total_quantity, total_amount, order_status \
         FROM t_sales_order WHERE customer_code = '{customer_code}' AND deleted_flag = 0 LIMIT 20"
    );
    let mut orders = lookup(cx, &sql, &SALES_BY_CUSTOMER).await?;
    retain_visible_rows(cx, sets, &mut orders, Visibility::CustomerOrEmployee("owner_manager"));
    if !orders.rows.is_empty() {
        answer.supplemental = Some(supplemental(orders, 20));
    }
    // main_sql 已 move 进 answer.sql：就地拼上补充查询，不再克隆第二回
    answer.sql = format!("{};\n{sql}", std::mem::take(&mut answer.sql));
    Ok(answer)
}

async fn lookup(
    cx: &AskCtx<'_>,
    sql: &str,
    policy: &DmsLookupPolicy,
) -> anyhow::Result<RowSet> {
    let checked = gate_dms_lookup(sql, policy)?;
    Ok(cx.auth_source.fetch_dms_lookup(&checked).await?)
}

#[derive(Clone, Copy)]
enum Visibility {
    /// 纯客户维度可见性（仅单测构造；生产族没有只用 customer 单列裁决的档案）
    #[allow(dead_code)]
    Customer,
    CustomerOrEmployee(&'static str),
    Employee(&'static str),
    AccountBillManager(&'static str),
    FailClosed,
}

fn document_visibility(kind: DocumentKind) -> Visibility {
    match kind {
        DocumentKind::SalesOrder | DocumentKind::AfterSales => {
            Visibility::CustomerOrEmployee("owner_manager")
        }
        DocumentKind::InvoiceApply => Visibility::Employee("manager"),
        DocumentKind::InvoiceApplyNew => Visibility::CustomerOrEmployee("manager"),
        DocumentKind::AccountBill => Visibility::AccountBillManager("manager"),
        DocumentKind::DeviceRequisition => Visibility::FailClosed,
        DocumentKind::WarehouseShipment
        | DocumentKind::PurchaseTransfer
        | DocumentKind::ShopRequisition
        | DocumentKind::ShopShipment
        | DocumentKind::ShopReturn
        | DocumentKind::Voucher
        | DocumentKind::StockAdjustment => Visibility::FailClosed,
    }
}

async fn document_row_visible(
    cx: &AskCtx<'_>,
    kind: DocumentKind,
    header: &RowSet,
) -> anyhow::Result<bool> {
    if cx.scope.unrestricted_by_role()
        || (kind == DocumentKind::DeviceRequisition && cx.scope.device_unrestricted_by_role())
    {
        return Ok(true);
    }
    // 可见集合一次问答算一次（原先 `row_visible`/`retain_visible_rows` 各自重复构建）
    let sets = VisibleSets::of(cx);
    if kind != DocumentKind::DeviceRequisition {
        return Ok(row_visible(cx, &sets, header, 0, document_visibility(kind)));
    }
    let Some(customer_code) = cell_by_name(header, 0, "customer_code")
        .and_then(value_text)
        .filter(|value| valid_entity_value(value))
    else {
        return Ok(false);
    };
    let sql = format!(
        "SELECT customer_code, area_manager_id FROM t_customer \
         WHERE customer_code = '{}' AND deleted_flag = 0 LIMIT 1",
        exact_literal(customer_code)?
    );
    let customer = lookup(cx, &sql, &CUSTOMER).await?;
    if customer.rows.is_empty() {
        return Ok(false);
    }
    Ok(row_visible(
        cx,
        &sets,
        &customer,
        0,
        Visibility::CustomerOrEmployee("area_manager_id"),
    ))
}

/// 一次问答算一次的可见集合：`visible_customer_codes`/`visible_employee_ids` 各自要
/// filter+clone 一遍整个集合，一轮问答在多个函数里重复构建太浪费（权限判据不变，只是算一次）。
struct VisibleSets {
    customers: Vec<String>,
    employees: Vec<i64>,
}

impl VisibleSets {
    fn of(cx: &AskCtx<'_>) -> Self {
        Self { customers: visible_customer_codes(cx), employees: visible_employee_ids(cx) }
    }
}

fn row_visible(cx: &AskCtx<'_>, sets: &VisibleSets, rows: &RowSet, row: usize, visibility: Visibility) -> bool {
    if cx.scope.unrestricted_by_role() {
        return true;
    }
    match visibility {
        Visibility::AccountBillManager(column) => manager_visible(
            cell_by_name(rows, row, column).and_then(value_text),
            &sets.employees,
            cx.scope.manager_names(),
        ),
        Visibility::Customer
        | Visibility::CustomerOrEmployee(_)
        | Visibility::Employee(_)
        | Visibility::FailClosed => {
            // customer 只在这一族里用：AccountBillManager 臂不白取白扫
            let customer = cell_by_name(rows, row, "customer_code").and_then(value_text);
            let employee = match visibility {
                Visibility::CustomerOrEmployee(column) | Visibility::Employee(column) => {
                    cell_by_name(rows, row, column).and_then(value_i64)
                }
                _ => None,
            };
            scope_visible(false, customer, employee, &sets.customers, &sets.employees, visibility)
        }
    }
}

fn manager_visible(manager: Option<&str>, allowed_ids: &[i64], allowed_names: &[String]) -> bool {
    let Some(manager) = manager.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    manager
        .parse::<i64>()
        .ok()
        .is_some_and(|id| id != -1 && allowed_ids.contains(&id))
        || allowed_names
            .iter()
            .any(|name| name.as_str() != "-1" && name.eq_ignore_ascii_case(manager))
}

fn retain_visible_rows(cx: &AskCtx<'_>, sets: &VisibleSets, rows: &mut RowSet, visibility: Visibility) {
    let unrestricted = cx.scope.unrestricted_by_role();
    // 列下标在闭包外各算一次：原来每行 `position` 线性找，O(行×列)
    let columns = rows.columns.clone();
    let index_of = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let customer_i = index_of("customer_code");
    let employee_i = match visibility {
        Visibility::CustomerOrEmployee(column) | Visibility::Employee(column) => index_of(column),
        _ => None,
    };
    rows.rows.retain(|row| {
        let customer = customer_i.and_then(|i| row.get(i)).and_then(value_text);
        let employee = employee_i.and_then(|i| row.get(i)).and_then(value_i64);
        scope_visible(unrestricted, customer, employee, &sets.customers, &sets.employees, visibility)
    });
}

fn scope_visible(
    unrestricted: bool,
    customer: Option<&str>,
    employee: Option<i64>,
    allowed_customers: &[String],
    allowed_employees: &[i64],
    visibility: Visibility,
) -> bool {
    if unrestricted {
        return true;
    }
    let customer_ok = customer.is_some_and(|code| {
        code != "-1"
            && allowed_customers.iter().any(|allowed| {
                allowed.as_str() != "-1" && allowed.eq_ignore_ascii_case(code)
            })
    });
    match visibility {
        Visibility::Customer => customer_ok,
        Visibility::CustomerOrEmployee(_) => {
            customer_ok
                || employee.is_some_and(|id| id != -1 && allowed_employees.contains(&id))
        }
        Visibility::Employee(_) => {
            employee.is_some_and(|id| id != -1 && allowed_employees.contains(&id))
        }
        // 只对未来误用者生效的两臂（fail-closed）：AccountBillManager 在 `row_visible` 上层
        // 已分流去 `manager_visible`；FailClosed 族在 `document_row_visible` 单独判
        Visibility::AccountBillManager(_) => false,
        Visibility::FailClosed => false,
    }
}

fn value_text(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(value) => Some(value),
        _ => None,
    }
}

fn value_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(value) => value.as_i64(),
        serde_json::Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn value_at<'a>(
    columns: &[String],
    row: &'a [serde_json::Value],
    name: &str,
) -> Option<&'a serde_json::Value> {
    let column = columns.iter().position(|column| column.eq_ignore_ascii_case(name))?;
    row.get(column)
}

fn cell_by_name<'a>(rows: &'a RowSet, row: usize, name: &str) -> Option<&'a serde_json::Value> {
    value_at(&rows.columns, rows.rows.get(row)?, name)
}

fn visible_customer_codes(cx: &AskCtx<'_>) -> Vec<String> {
    cx.scope
        .sets()
        .customer_codes
        .iter()
        .filter(|code| code.as_str() != "-1" && valid_entity_value(code))
        .cloned()
        .collect()
}

fn visible_employee_ids(cx: &AskCtx<'_>) -> Vec<i64> {
    cx.scope.sets().employee_ids.iter().copied().filter(|id| *id != -1).collect()
}

fn entity_query(question: &str) -> Option<(EntityKind, String)> {
    // 客套前缀/尾巴词表与 `entity.rs` 的 LEADING_INTENT/TRAILING_INTENT 大比例重叠
    // （那边多「看看」族）。两份是有意的（各自词法门独立演化），但改动时两边都要看一眼 —— 互指防漂移。
    let mut q = question.trim().trim_matches(|c: char| matches!(c, '，' | ',' | '。' | '?' | '？'));
    loop {
        let Some(rest) = [
            "请帮我查询一下", "请帮我查一下", "帮我查询一下", "帮我查一下", "请查询一下",
            "请查一下", "查询一下", "查一下", "请查询", "请查", "查询", "查", "请问",
        ]
        .iter()
        .find_map(|prefix| q.strip_prefix(prefix))
        else {
            break;
        };
        q = rest.trim();
    }
    for (prefix, kind) in [
        ("客户编码", EntityKind::Customer),
        ("客户代码", EntityKind::Customer),
        ("客户编号", EntityKind::Customer),
        ("商品编码", EntityKind::Goods),
        ("商品代码", EntityKind::Goods),
        ("产品编码", EntityKind::Goods),
        ("产品代码", EntityKind::Goods),
        ("SKU编码", EntityKind::Goods),
        ("SKU代码", EntityKind::Goods),
    ] {
        if let Some(value) = q
            .strip_prefix(prefix)
            .map(clean_value)
            .filter(|value| valid_entity_value(value))
        {
            return Some((kind, value.to_string()));
        }
    }
    None
}

fn clean_value(value: &str) -> &str {
    let mut value = value
        .trim()
        .trim_start_matches(|c: char| matches!(c, '：' | ':' | ' ' | '='))
        .trim();
    loop {
        let Some(stripped) = [
            "的订单明细", "的下单信息", "的订单信息", "的销售表现", "的销售情况",
            "的详细信息", "详细信息", "的信息", "是什么", "资料", "信息",
        ]
        .iter()
        .find_map(|suffix| value.strip_suffix(suffix))
        else {
            return value;
        };
        value = stripped.trim();
    }
}

fn valid_entity_value(value: &str) -> bool {
    (2..=60).contains(&value.chars().count())
        && !value
            .chars()
            .any(|c| matches!(c, '\'' | '"' | '%' | ';' | '\\') || c.is_control())
}

fn exact_literal(value: &str) -> anyhow::Result<String> {
    if !valid_entity_value(value) {
        anyhow::bail!("生产 DMS 点查值不合法")
    }
    Ok(value.to_string())
}

fn header_policy(kind: DocumentKind) -> Option<&'static DmsLookupPolicy> {
    // 注册表说明“这是什么单、表和字段是什么”；此白名单另说明“生产索引已核验、允许点查”。
    // 门店/凭证/调整单尚未进入 connector 的物理最左索引核验目录，必须在访问业务库前失败关闭。
    Some(match kind {
        DocumentKind::WarehouseShipment
        | DocumentKind::ShopRequisition
        | DocumentKind::ShopShipment
        | DocumentKind::ShopReturn
        | DocumentKind::Voucher
        | DocumentKind::StockAdjustment => return None,
        DocumentKind::SalesOrder => &SALES,
        DocumentKind::AfterSales => &AFTER,
        DocumentKind::AccountBill => &BILL,
        DocumentKind::DeviceRequisition => &DEVICE,
        DocumentKind::InvoiceApply => &INVOICE,
        DocumentKind::InvoiceApplyNew => &INVOICE_NEW,
        DocumentKind::PurchaseTransfer => return None,
    })
}

fn detail_policy(table: &str) -> Option<&'static DmsLookupPolicy> {
    Some(match table {
        "t_sales_order_detail" => &SALES_DETAIL,
        "t_after_sales_order_detail" => &AFTER_DETAIL,
        "t_account_bill_detail" => &BILL_DETAIL,
        "t_device_receive_item" => &DEVICE_RECEIVE,
        "t_device_delivery_item" => &DEVICE_DELIVERY,
        "t_invoice_apply_detail" => &INVOICE_DETAIL,
        "t_invoice_new_apply_detail" => &INVOICE_NEW_DETAIL,
        _ => return None,
    })
}

fn merge_rowsets(parts: Vec<(&str, RowSet)>) -> RowSet {
    if parts.is_empty() {
        return RowSet::default();
    }
    let mut columns = vec!["来源表".to_string()];
    let mut seen = std::collections::HashSet::from(["来源表"]);
    for (_, rows) in &parts {
        for column in &rows.columns {
            if seen.insert(column.as_str()) {
                columns.push(column.clone());
            }
        }
    }
    let mut data = Vec::new();
    let mut redacted = Vec::new();
    for (table, rows) in parts {
        let positions: HashMap<&str, usize> =
            rows.columns.iter().enumerate().map(|(i, c)| (c.as_str(), i)).collect();
        for row in rows.rows {
            let mut merged = vec![serde_json::Value::String(table.to_string())];
            merged.extend(columns.iter().skip(1).map(|column| {
                positions
                    .get(column.as_str())
                    .and_then(|i| row.get(*i))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            }));
            data.push(merged);
        }
        redacted.extend(rows.redacted);
    }
    redacted.sort();
    redacted.dedup();
    RowSet { columns, rows: data, redacted }
}

fn supplemental(rows: RowSet, limit: usize) -> SupplementalResult {
    let RowSet { columns, rows, .. } = rows;
    let row_count = rows.len();
    let view = dms_semantic::present::build(&columns, &rows);
    SupplementalResult {
        columns,
        rows,
        row_count,
        truncated: row_count >= limit,
        view,
    }
}

fn document_answer(
    cx: &AskCtx<'_>,
    family: &DocumentFamily,
    code: &str,
    header_sql: String,
    mut header: RowSet,
    mut details: RowSet,
    detail_truncated: bool,
) -> AskResult {
    let pairs = header_pairs(document_identity_pairs(family, code), &header);
    let has_details = !details.rows.is_empty();
    if has_details {
        details.redacted.extend(header.redacted.drain(..));
        details.redacted.sort();
        details.redacted.dedup();
    }
    let rows = if has_details { details } else { header };
    // build 只调一次：原先先算一遍 blocks 又被整体覆盖，白做一次全量决策
    let mut view = dms_semantic::present::build(&rows.columns, &rows.rows);
    let mut blocks = vec![Block::Entity { pairs }];
    if has_details {
        blocks.extend(std::mem::take(&mut view.blocks));
    }
    view.blocks = blocks;
    view.insight = Some(format!(
        "已识别{}并按当前 DMS 账号权限核验；主表与明细按同一单号分别执行轻量点查。",
        family.name
    ));
    // 截断按「任一单表明细顶到 LIMIT 50」判：合并后行数对单表上限判会误报（两个 30 行 → 60 ≥ 50）
    let truncated = has_details && detail_truncated;
    result(cx, header_sql, rows, view, truncated)
}

fn document_identity_pairs(
    family: &DocumentFamily,
    code: &str,
) -> Vec<(String, serde_json::Value)> {
    vec![
        ("单据类型".into(), serde_json::Value::String(family.name.into())),
        ("单号".into(), serde_json::Value::String(code.into())),
        ("主表".into(), serde_json::Value::String(family.header_table.into())),
        (
            "明细表".into(),
            serde_json::Value::String(if family.details.is_empty() {
                "（无）".to_string() // 空清单不拼出空串（头卡「明细表：（空）」是坏展示）
            } else {
                family.details.iter().map(|(table, _)| *table).collect::<Vec<_>>().join("、")
            }),
        ),
    ]
}

fn document_registry_answer(
    cx: &AskCtx<'_>,
    family: &DocumentFamily,
    code: &str,
    note: &str,
) -> AskResult {
    let view = ViewSpec {
        columns: vec![],
        blocks: vec![Block::Entity { pairs: document_identity_pairs(family, code) }],
        interact: dms_kernel::present::Interact { drill: vec![] },
        insight: Some(note.into()),
    };
    result(cx, String::new(), RowSet::default(), view, false)
}

/// 头卡 pairs = 身份四件 + header 投影行。header 投影里若含与身份同 label 的列（如「单号」），
/// 先过滤 —— 同一标签在头卡出现两次是展示事故。
fn header_pairs(
    identity: Vec<(String, serde_json::Value)>,
    header: &RowSet,
) -> Vec<(String, serde_json::Value)> {
    let mut pairs = identity;
    // 先收集再并入：filter 闭包对 pairs 的不可变借用止于 collect，不与 extend 打架
    let extra = header
        .columns
        .iter()
        .cloned()
        .zip(header.rows.first().cloned().unwrap_or_default())
        .filter(|(label, value)| !value.is_null() && !pairs.iter().any(|(l, _)| l == label))
        .collect::<Vec<_>>();
    pairs.extend(extra);
    pairs
}

fn table_result(cx: &AskCtx<'_>, sql: String, rows: RowSet, limit: usize) -> AskResult {
    let truncated = rows.rows.len() >= limit;
    let view = dms_semantic::present::build(&rows.columns, &rows.rows);
    result(cx, sql, rows, view, truncated)
}

fn result(
    cx: &AskCtx<'_>,
    sql: String,
    rows: RowSet,
    view: ViewSpec,
    truncated: bool,
) -> AskResult {
    let RowSet { columns, rows, redacted } = rows;
    let row_count = rows.len();
    AskResult {
        sql,
        columns,
        rows,
        row_count,
        truncated,
        elapsed_ms: cx.t0.elapsed().as_millis(),
        route: "business-lookup".into(),
        view,
        supplemental: None,
        comparisons: vec![],
        subs: vec![],
        caliber_note: None,
        reinterpret_note: None,
        truncation_note: None,
        redacted,
        scope_note: (!cx.scope.unrestricted_by_role())
            .then(|| "已按当前 DMS 账号权限执行生产轻查询".into()),
        trust: None,
        steps: vec![],
        clarify_options: vec![],
        value_labels: vec![],
        sales_context: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_entities_enter_production_lookup() {
        assert!(matches!(entity_query("客户编码 C001"), Some((EntityKind::Customer, _))));
        assert!(matches!(
            entity_query("商品编码 SKU_001 的详细信息"),
            Some((EntityKind::Goods, value)) if value == "SKU_001"
        ));
        assert!(matches!(
            entity_query("请帮我查一下 SKU编码 SKU_001 的订单明细信息"),
            Some((EntityKind::Goods, value)) if value == "SKU_001"
        ));
        assert!(matches!(
            entity_query("查客户编号 C001 的下单信息"),
            Some((EntityKind::Customer, value)) if value == "C001"
        ));
        assert!(entity_query("商品名称 可颂香肠卷").is_none());
        assert!(entity_query("客户 线下-长沙某客户").is_none());
        assert!(entity_query("本月销售额是多少").is_none());
        assert!(entity_query("客户编码 %").is_none());
    }

    #[test]
    fn header_pairs_drop_columns_duplicating_identity_labels() {
        let family = dms_semantic::document::DOCUMENT_FAMILIES
            .iter()
            .find(|f| f.kind == DocumentKind::SalesOrder)
            .unwrap();
        let header = RowSet {
            columns: vec!["单号".into(), "客户".into()],
            rows: vec![vec![serde_json::json!("X1"), serde_json::json!("恒众")]],
            redacted: vec![],
        };
        let pairs = header_pairs(document_identity_pairs(family, "X1"), &header);
        // 「单号」只在身份四件里出现一次（header 投影的同名列被滤掉）
        assert_eq!(pairs.iter().filter(|(l, _)| l == "单号").count(), 1, "{pairs:?}");
        assert!(pairs.iter().any(|(l, _)| l == "客户"), "{pairs:?}");
    }

    #[test]
    fn production_lookup_only_exposes_physically_verified_families() {
        for family in dms_semantic::document::DOCUMENT_FAMILIES {
            let enabled = header_policy(family.kind).is_some();
            let expected = matches!(
                family.kind,
                DocumentKind::SalesOrder
                    | DocumentKind::AfterSales
                    | DocumentKind::AccountBill
                    | DocumentKind::DeviceRequisition
                    | DocumentKind::InvoiceApply
                    | DocumentKind::InvoiceApplyNew
            );
            assert_eq!(enabled, expected, "{}", family.header_table);
            if enabled {
                for detail in family.production.into_iter().flat_map(|source| source.details) {
                    assert!(detail_policy(detail.table).is_some(), "{}", detail.table);
                }
            }
        }
    }

    #[test]
    fn registered_document_codes_keep_exact_header_and_detail_families() {
        for (code, kind, detail_tables) in [
            ("HJXH-DXO2026072300384", DocumentKind::SalesOrder, &["t_sales_order_detail"][..]),
            ("HJXH-DRO2026072300047", DocumentKind::AfterSales, &["t_after_sales_order_detail"][..]),
            ("HJXH-DZD20261230000261", DocumentKind::AccountBill, &["t_account_bill_detail"][..]),
            (
                "HJXH_XQ20260804100098",
                DocumentKind::DeviceRequisition,
                &["t_device_receive_item", "t_device_delivery_item"][..],
            ),
            ("IO2025123456", DocumentKind::InvoiceApply, &["t_invoice_apply_detail"][..]),
            ("SQ2026052345", DocumentKind::InvoiceApplyNew, &["t_invoice_new_apply_detail"][..]),
        ] {
            let resolved = resolve_document(code, false).expect(code);
            assert_eq!(resolved.family.kind, kind, "{code}");
            let source = resolved.family.production.expect(code);
            assert!(header_policy(kind).is_some(), "{code}");
            assert_eq!(
                source.details.iter().map(|detail| detail.table).collect::<Vec<_>>(),
                detail_tables,
                "{code}"
            );
            assert!(source.details.iter().all(|detail| detail_policy(detail.table).is_some()));
        }
    }

    #[test]
    fn recognized_but_unverified_documents_still_return_exact_registry_identity() {
        for (code, kind, header, details) in [
            ("CG2603090123", DocumentKind::PurchaseTransfer, "t_winc_purchase_transfer", "（无）"),
            ("SHOP_PH20260805100005", DocumentKind::ShopShipment, "t_shop_shipment_order", "t_shop_order_header、t_shop_order_detail"),
            ("PZ20260805100003", DocumentKind::Voucher, "t_voucher_header", "t_voucher_detail"),
        ] {
            let resolved = resolve_document(code, false).expect(code);
            assert_eq!(resolved.family.kind, kind, "{code}");
            assert!(resolved.family.production.is_none(), "{code} 不得误开生产查询");
            let pairs = document_identity_pairs(resolved.family, code);
            assert!(pairs.iter().any(|(label, value)| label == "主表" && value.as_str() == Some(header)), "{code}: {pairs:?}");
            assert!(pairs.iter().any(|(label, value)| label == "明细表" && value.as_str() == Some(details)), "{code}: {pairs:?}");
        }
    }

    #[test]
    fn recognized_documents_keep_identity_when_data_is_absent_or_hidden() {
        let src = include_str!("business_lookup.rs");
        let document = src
            .split("async fn document(")
            .nth(1)
            .unwrap()
            .split("enum EntityKind")
            .next()
            .unwrap();
        assert!(document.contains("不区分单号不存在与无查看权限"));
        assert!(document.contains("document_registry_answer("));
    }

    #[test]
    fn production_queries_remain_indexed_single_table_points() {
        let src = include_str!("business_lookup.rs");
        let document = src
            .split("async fn document(")
            .nth(1)
            .unwrap()
            .split("enum EntityKind")
            .next()
            .unwrap();
        let customer = src
            .split("async fn customer_answer(")
            .nth(1)
            .unwrap()
            .split("async fn lookup(")
            .next()
            .unwrap();
        for forbidden in [" JOIN ", " GROUP BY ", " ORDER BY ", " LIKE ", "COUNT(", "SUM("] {
            assert!(!document.contains(forbidden), "生产单据点查出现重查询：{forbidden}");
            assert!(!customer.contains(forbidden), "生产客户补充查询出现重查询：{forbidden}");
        }
        assert!(document.contains("LIMIT 1"));
        assert!(document.contains("LIMIT 50"));
        assert!(customer.contains("WHERE customer_code = '{customer_code}'"));
        assert!(customer.contains("LIMIT 20"));
    }

    #[test]
    fn dms_or_visibility_matches_mapper_contracts_and_filters_sentinels() {
        let customers = vec!["-1".to_string(), "C1".to_string()];
        let employees = vec![-1, 7];
        assert!(scope_visible(
            false, Some("C1"), None, &customers, &employees,
            Visibility::CustomerOrEmployee("owner_manager")
        ));
        assert!(scope_visible(
            false, Some("C2"), Some(7), &customers, &employees,
            Visibility::CustomerOrEmployee("owner_manager")
        ));
        assert!(!scope_visible(
            false, Some("C2"), Some(-1), &customers, &employees,
            Visibility::CustomerOrEmployee("owner_manager")
        ));
        assert!(!scope_visible(
            false, Some("-1"), None, &customers, &employees, Visibility::Customer
        ));
        assert!(!scope_visible(
            false, Some("C2"), Some(8), &customers, &employees, Visibility::Customer
        ));
        assert!(!scope_visible(
            false, Some("C1"), Some(7), &customers, &employees, Visibility::FailClosed
        ));
        assert!(!scope_visible(
            false, Some("C1"), Some(8), &customers, &employees, Visibility::Employee("manager")
        ));
        assert!(scope_visible(
            false, Some("C2"), Some(7), &customers, &employees, Visibility::Employee("manager")
        ));
        assert!(manager_visible(Some("7"), &employees, &[]));
        assert!(manager_visible(Some("张三"), &[], &["张三".into()]));
        assert!(!manager_visible(Some("李四"), &employees, &["张三".into()]));
    }

    #[test]
    fn device_visibility_matches_customer_or_area_manager_scope() {
        let customers = vec!["C1".into()];
        let employees = vec![7];
        assert!(scope_visible(
            false,
            Some("C1"),
            Some(8),
            &customers,
            &employees,
            Visibility::CustomerOrEmployee("area_manager_id"),
        ));
        assert!(scope_visible(
            false,
            Some("C2"),
            Some(7),
            &customers,
            &employees,
            Visibility::CustomerOrEmployee("area_manager_id"),
        ));
        assert!(!scope_visible(
            false,
            Some("C2"),
            Some(8),
            &customers,
            &employees,
            Visibility::CustomerOrEmployee("area_manager_id"),
        ));
    }
}
