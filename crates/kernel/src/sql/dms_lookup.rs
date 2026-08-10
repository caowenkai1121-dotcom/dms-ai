//! 生产 DMS MySQL 的专用轻查询闸门。
//!
//! 该闸门同时服务“业务点查 answerer”和 connector 的通用 `ScopedSql` 生产库兜底，
//! 不约束登录、角色和数据权限加载使用的静态 `FixedStmt`。类型私有字段保证 connector
//! 只能执行经过本文件校验且数据库侧 LIMIT 不超过 50 的 SQL。

use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::{error::Error, fmt};

use sqlparser::ast::{
    BinaryOperator, Expr, GroupByExpr, Query, SelectItem, SetExpr, Statement, TableFactor, Value,
    Visit, Visitor,
};
use sqlparser::parser::Parser;

use crate::errors::GuardError;
use crate::sql::dialect::Dialect;
use crate::sql::guard::is_safe_select_with;

pub const DMS_LOOKUP_MAX_ROWS: usize = 50;
pub const DMS_LOOKUP_MAX_IN_ITEMS: usize = 5;

/// 生产点查额外敏感目录。这里覆盖凭据、手机号、邮箱、地址、银行、税务和证件类字段；
/// 即使上层词表漏项，生产业务库也不会把这些列投影出来。
const DMS_SENSITIVE_FRAGMENTS: &[&str] = &[
    "password", "passwd", "login_pwd", "credential", "secret", "token", "phone", "mobile",
    "telephone", "email", "address", "bank_account", "bank_card", "bank_name", "invoice_bank",
    "tax_no", "tax_number", "taxpayer", "social_credit", "identity_card", "id_card",
];

/// 尚无可核对 DMS 行权限合同的生产表。即使调用方自行构造 `DmsLookupPolicy`，也必须
/// 在共享 gate 内 fail-closed，不能只依赖通用 `ScopedSql` 白名单。
const UNCONTRACTED_PRODUCTION_TABLES: &[&str] = &["t_winc_purchase_transfer"];

/// 通用 `ScopedSql` 在生产 DMS MySQL 上的执行白名单。未知表一律拒绝；这里只登记
/// 由单列主键/唯一索引承载的头表业务键。明细外键只能由专用业务点查通道声明，
/// 并在 connector 侧核验为物理索引的最左列。
const SCOPED_LOOKUP_POLICIES: &[DmsLookupPolicy] = &[
    DmsLookupPolicy::new("t_sales_order", &["sales_order_code"]),
    DmsLookupPolicy::new("t_after_sales_order_header", &["after_sales_code"]),
    DmsLookupPolicy::new("t_account_bill_header", &["bill_code"]),
    DmsLookupPolicy::new("t_device_requisition", &["requisition_code"]),
    DmsLookupPolicy::new("t_invoice_apply_header", &["invoice_code"]),
    DmsLookupPolicy::new("t_invoice_new_apply_header", &["invoice_code"]),
    DmsLookupPolicy::new("t_customer", &["customer_code"]),
    DmsLookupPolicy::new("t_goods", &["goods_code"]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmsIndexKind {
    /// 主表/主档点查必须由单列主键或单列唯一索引承载。
    Unique,
    /// 一对多补充明细允许普通索引，但过滤列必须位于索引第一列。
    Leading,
}

const REGISTERED_LOOKUP_KEYS: &[(&str, &str, DmsIndexKind)] = &[
    ("t_sales_order", "sales_order_code", DmsIndexKind::Unique),
    ("t_sales_order", "customer_code", DmsIndexKind::Leading),
    ("t_sales_order_detail", "sales_order_code", DmsIndexKind::Leading),
    ("t_after_sales_order_header", "after_sales_code", DmsIndexKind::Unique),
    ("t_after_sales_order_detail", "after_sales_code", DmsIndexKind::Leading),
    ("t_account_bill_header", "bill_code", DmsIndexKind::Unique),
    ("t_account_bill_detail", "bill_code", DmsIndexKind::Leading),
    ("t_device_requisition", "requisition_code", DmsIndexKind::Unique),
    ("t_device_receive_item", "requisition_code", DmsIndexKind::Leading),
    ("t_device_delivery_item", "requisition_code", DmsIndexKind::Leading),
    ("t_invoice_apply_header", "invoice_code", DmsIndexKind::Unique),
    ("t_invoice_apply_detail", "invoice_code", DmsIndexKind::Leading),
    ("t_invoice_new_apply_header", "invoice_code", DmsIndexKind::Unique),
    ("t_invoice_new_apply_detail", "invoice_code", DmsIndexKind::Leading),
    ("t_customer", "customer_code", DmsIndexKind::Unique),
    ("t_goods", "goods_code", DmsIndexKind::Unique),
];

/// 连接器启动/热切换时核验这组代码登记键。运行期必须同时满足登记类型和物理索引，
/// 任一条件缺失都 fail-closed。
pub fn registered_lookup_keys(
) -> impl Iterator<Item = (&'static str, &'static str, DmsIndexKind)> {
    REGISTERED_LOOKUP_KEYS.iter().copied()
}

pub fn registered_lookup_kind(table: &str, column: &str) -> Option<DmsIndexKind> {
    REGISTERED_LOOKUP_KEYS
        .iter()
        .find(|(registered_table, registered_column, _)| {
            table.eq_ignore_ascii_case(registered_table)
                && column.eq_ignore_ascii_case(registered_column)
        })
        .map(|(_, _, kind)| *kind)
}

/// 服务端代码登记的单表索引点查策略。请求参数只能提供值，不能提供表、键或索引类型。
pub struct DmsLookupPolicy {
    table: &'static str,
    lookup_cols: &'static [&'static str],
    index_kind: DmsIndexKind,
}

impl DmsLookupPolicy {
    pub const fn new(table: &'static str, unique_cols: &'static [&'static str]) -> Self {
        Self { table, lookup_cols: unique_cols, index_kind: DmsIndexKind::Unique }
    }

    pub const fn indexed(table: &'static str, lookup_cols: &'static [&'static str]) -> Self {
        Self { table, lookup_cols, index_kind: DmsIndexKind::Leading }
    }
}

/// 只有本模块的两个严格 gate 能构造；下游除规范化 SQL 外，还能读取本次查询实际使用的
/// 登记键，以便与连接建立时核验的单列唯一索引/最左索引逐项核对。
pub struct DmsLookupSql {
    wire: String,
    table: String,
    lookup_cols: Vec<String>,
    index_kind: DmsIndexKind,
}

impl DmsLookupSql {
    pub fn wire(&self) -> &str {
        &self.wire
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn lookup_cols(&self) -> &[String] {
        &self.lookup_cols
    }

    pub fn index_kind(&self) -> DmsIndexKind {
        self.index_kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmsLookupError {
    Common(GuardError),
    Shape(String),
}

impl fmt::Display for DmsLookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Common(err) => write!(f, "{err}"),
            Self::Shape(reason) => write!(f, "生产 DMS 点查拒绝：{reason}"),
        }
    }
}

impl Error for DmsLookupError {}

impl From<GuardError> for DmsLookupError {
    fn from(value: GuardError) -> Self {
        Self::Common(value)
    }
}

fn reject(reason: impl Into<String>) -> DmsLookupError {
    DmsLookupError::Shape(reason.into())
}

/// 严格生产点查 gate。`sensitive` 与通用 guard 共用业务侧词表，但查询形状单独判定。
pub fn gate_dms_lookup_with(
    sql: &str,
    d: &'static dyn Dialect,
    sensitive: &[&str],
    policy: &DmsLookupPolicy,
) -> Result<DmsLookupSql, DmsLookupError> {
    let mut query = parse_query(sql, d, sensitive)?;
    let lookup_cols = validate_query(&query, policy)?;
    let table = query_table_name(&query)?.to_ascii_lowercase();
    normalize_limit(&mut query);
    Ok(DmsLookupSql {
        wire: query.to_string(),
        table,
        lookup_cols,
        index_kind: policy.index_kind,
    })
}

/// connector 的通用 `ScopedSql` 生产库兜底。先识别唯一物理表，再应用固定登记的业务键；
/// 调用方不能自行声明表或“索引列”，避免把任意等值条件伪装成轻查询。
pub fn gate_dms_scoped_with(
    sql: &str,
    d: &'static dyn Dialect,
    sensitive: &[&str],
) -> Result<DmsLookupSql, DmsLookupError> {
    let mut query = parse_query(sql, d, sensitive)?;
    let table = query_table_name(&query)?.to_ascii_lowercase();
    let policy = SCOPED_LOOKUP_POLICIES
        .iter()
        .find(|policy| table.eq_ignore_ascii_case(policy.table))
        .ok_or_else(|| reject(format!("表 {table} 未登记为生产 DMS 轻查询表")))?;
    let lookup_cols = validate_query(&query, policy)?;
    normalize_limit(&mut query);
    Ok(DmsLookupSql {
        wire: query.to_string(),
        table,
        lookup_cols,
        index_kind: policy.index_kind,
    })
}

fn parse_query(
    sql: &str,
    d: &'static dyn Dialect,
    sensitive: &[&str],
) -> Result<Box<Query>, DmsLookupError> {
    is_safe_select_with(sql, d, sensitive).map_err(safe_guard_error)?;
    let mut stmts = Parser::parse_sql(d.parser(), sql)
        .map_err(|_| reject("SQL 语法不合法"))?;
    match stmts.pop() {
        Some(Statement::Query(query)) if stmts.is_empty() => Ok(query),
        _ => Err(reject("只允许单条 SELECT")),
    }
}

fn safe_guard_error(err: GuardError) -> DmsLookupError {
    match err {
        GuardError::Parse(_) => reject("SQL 语法不合法"),
        GuardError::UnfilledPlaceholder(_) => reject("SQL 含未填充占位符"),
        other => DmsLookupError::Common(other),
    }
}

fn normalize_limit(query: &mut Query) {
    let limit = query
        .limit
        .as_ref()
        .and_then(|e| match e {
            Expr::Value(Value::Number(n, false)) => n.parse::<usize>().ok(),
            _ => None,
        })
        .unwrap_or(DMS_LOOKUP_MAX_ROWS)
        .min(DMS_LOOKUP_MAX_ROWS);
    query.limit = Some(Expr::Value(Value::Number(limit.to_string(), false)));
}

fn validate_query(query: &Query, policy: &DmsLookupPolicy) -> Result<Vec<String>, DmsLookupError> {
    // ── 查询级附加子句（与表形/投影无关，最先判）──
    if query.with.is_some() {
        return Err(reject("禁止 CTE"));
    }
    if query.order_by.is_some() {
        return Err(reject("禁止 ORDER BY，避免生产库排序扫描"));
    }
    if !query.locks.is_empty() {
        return Err(reject("禁止行锁"));
    }
    if query.offset.is_some() || query.fetch.is_some() || !query.limit_by.is_empty() {
        return Err(reject("禁止 OFFSET/FETCH/LIMIT BY"));
    }
    if query.for_clause.is_some() || query.settings.is_some() || query.format_clause.is_some() {
        return Err(reject("禁止非 MySQL 点查附加子句"));
    }
    if let Some(limit) = &query.limit {
        let Expr::Value(Value::Number(n, false)) = limit else {
            return Err(reject("LIMIT 必须是整数常量"));
        };
        let limit = n.parse::<usize>().map_err(|_| reject("LIMIT 必须是整数常量"))?;
        if limit > DMS_LOOKUP_MAX_ROWS {
            return Err(reject(format!("LIMIT 不得超过 {DMS_LOOKUP_MAX_ROWS}")));
        }
    }

    // ── 查询体与单表形状（先于投影判定：UNION/多表/派生表/子查询是更根本的拒绝理由）──
    let SetExpr::Select(select) = &*query.body else {
        return Err(reject("禁止 UNION/INTERSECT/EXCEPT、括号子查询及非 SELECT 查询体"));
    };
    if select.distinct.is_some() {
        return Err(reject("禁止 DISTINCT，业务点查应直接按索引键定位"));
    }
    if select.into.is_some() {
        return Err(reject("禁止 SELECT INTO"));
    }
    if select.top.is_some() || !select.lateral_views.is_empty() || select.prewhere.is_some() {
        return Err(reject("禁止 TOP/LATERAL/PREWHERE"));
    }
    if !matches!(&select.group_by, GroupByExpr::Expressions(items, _) if items.is_empty())
        || select.having.is_some()
    {
        return Err(reject("禁止 GROUP BY/HAVING"));
    }
    if !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || !select.named_window.is_empty()
        || select.qualify.is_some()
        || select.connect_by.is_some()
    {
        return Err(reject("禁止分析型查询子句"));
    }
    let (actual_table, alias) = query_table_ref(query)?;
    if UNCONTRACTED_PRODUCTION_TABLES
        .iter()
        .any(|table| actual_table.eq_ignore_ascii_case(table))
    {
        return Err(reject(format!("表 {actual_table} 缺少可核对的 DMS 行权限合同")));
    }
    if !actual_table.eq_ignore_ascii_case(policy.table) {
        return Err(reject(format!("该通道只允许查询登记表 {}", policy.table)));
    }

    // ── 全语句结构扫描（子查询/窗口/聚合/限定符/敏感列；函数调用推迟到谓词判定之后）──
    let mut shape = Shape::new(alias.unwrap_or(actual_table));
    let _ = query.visit(&mut shape);
    if shape.queries != 1 || shape.tables != 1 {
        return Err(reject("禁止子查询，且必须恰好引用一张物理表"));
    }
    if shape.has_window {
        return Err(reject("禁止窗口函数"));
    }
    if let Some(name) = shape.aggregate {
        return Err(reject(format!("禁止聚合函数 {name}")));
    }
    if let Some(reference) = shape.foreign_qualifier {
        return Err(reject(format!("字段限定符 {reference} 与 FROM 表不一致")));
    }
    if let Some(name) = shape.sensitive {
        return Err(reject(format!("禁止读取敏感字段 {name}")));
    }

    // ── WHERE 点查谓词（先于函数/投影判定：WHERE 里的函数按「WHERE 只允许…」拒绝）──
    let where_expr = select.selection.as_ref().ok_or_else(|| reject("必须提供可索引 WHERE 点查条件"))?;
    if policy.lookup_cols.is_empty() {
        return Err(reject("调用方未声明该表允许的索引点查列"));
    }
    let mut lookup_cols = BTreeSet::new();
    validate_lookup_predicate(where_expr, policy, &mut lookup_cols)?;
    if lookup_cols.is_empty() {
        return Err(reject("WHERE 缺少调用方声明的索引点查列"));
    }

    // ── 函数调用与投影形状（最后：投影里的表达式按各自专类消息拒绝）──
    if let Some(name) = shape.function {
        return Err(reject(format!("生产点查禁止函数调用 {name}")));
    }
    if select.projection.iter().any(|item| {
        matches!(item, SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _))
    }) {
        return Err(reject("生产业务库禁止 SELECT *，必须使用显式最小投影"));
    }
    if select.projection.iter().any(|item| {
        !matches!(
            item,
            SelectItem::UnnamedExpr(Expr::Identifier(_) | Expr::CompoundIdentifier(_))
        )
    }) {
        return Err(reject("生产业务库投影只允许显式字段，不允许表达式或别名"));
    }
    Ok(lookup_cols.into_iter().collect())
}

fn query_table_name(query: &Query) -> Result<&str, DmsLookupError> {
    query_table_ref(query).map(|(table, _)| table)
}

fn query_table_ref(query: &Query) -> Result<(&str, Option<&str>), DmsLookupError> {
    let SetExpr::Select(select) = &*query.body else {
        return Err(reject("禁止 UNION/INTERSECT/EXCEPT、括号子查询及非 SELECT 查询体"));
    };
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(reject("必须且只能查询一张物理表，禁止 JOIN"));
    }
    let relation = &select.from[0].relation;
    if !matches!(
        relation,
        TableFactor::Table {
            args: None,
            with_hints,
            version: None,
            with_ordinality: false,
            partitions,
            json_path: None,
            ..
        } if with_hints.is_empty() && partitions.is_empty()
    ) {
        return Err(reject("FROM 只能是物理表，禁止派生表和表函数"));
    }
    let TableFactor::Table { name, alias, .. } = relation else { unreachable!() };
    if name.0.len() != 1 {
        return Err(reject("禁止跨库/跨 schema 点查"));
    }
    if alias.as_ref().is_some_and(|alias| !alias.columns.is_empty()) {
        return Err(reject("禁止通过表别名列清单重命名字段"));
    }
    Ok((
        name.0.last().map(|part| part.value.as_str()).unwrap_or_default(),
        alias.as_ref().map(|alias| alias.name.value.as_str()),
    ))
}

struct Shape<'a> {
    allowed_qualifier: &'a str,
    queries: usize,
    tables: usize,
    has_window: bool,
    aggregate: Option<String>,
    function: Option<String>,
    foreign_qualifier: Option<String>,
    sensitive: Option<String>,
}

impl<'a> Shape<'a> {
    fn new(allowed_qualifier: &'a str) -> Self {
        Self {
            allowed_qualifier,
            queries: 0,
            tables: 0,
            has_window: false,
            aggregate: None,
            function: None,
            foreign_qualifier: None,
            sensitive: None,
        }
    }
}

impl Visitor for Shape<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<Self::Break> {
        self.queries += 1;
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<Self::Break> {
        if matches!(tf, TableFactor::Table { .. }) {
            self.tables += 1;
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::CompoundIdentifier(ids) = expr {
            if ids.len() != 2
                || !ids[0].value.eq_ignore_ascii_case(self.allowed_qualifier)
            {
                self.foreign_qualifier
                    .get_or_insert_with(|| ids.iter().map(|id| id.value.as_str()).collect::<Vec<_>>().join("."));
            }
        }
        let column = match expr {
            Expr::Identifier(id) => Some(id.value.as_str()),
            Expr::CompoundIdentifier(ids) => ids.last().map(|id| id.value.as_str()),
            _ => None,
        };
        if let Some(column) = column {
            let lower = column.to_ascii_lowercase();
            if DMS_SENSITIVE_FRAGMENTS.iter().any(|fragment| lower.contains(fragment)) {
                self.sensitive.get_or_insert(lower);
            }
        }
        if let Expr::Function(fun) = expr {
            self.has_window |= fun.over.is_some();
            let name = fun.name.to_string().to_ascii_lowercase();
            if is_aggregate(&name) {
                self.aggregate.get_or_insert_with(|| name.clone());
            }
            self.function.get_or_insert(name);
        }
        ControlFlow::Continue(())
    }
}

fn is_aggregate(name: &str) -> bool {
    matches!(
        name.rsplit('.').next().unwrap_or(name),
        "avg"
            | "bit_and"
            | "bit_or"
            | "bit_xor"
            | "count"
            | "group_concat"
            | "json_arrayagg"
            | "json_objectagg"
            | "max"
            | "min"
            | "std"
            | "stddev"
            | "stddev_pop"
            | "stddev_samp"
            | "sum"
            | "variance"
            | "var_pop"
            | "var_samp"
    )
}

/// WHERE 只允许登记索引键叶子通过 AND 组合；唯一例外是固定软删除条件
/// `deleted_flag = 0/false`，它只负责业务有效性，不能单独取得执行资格。
fn validate_lookup_predicate(
    expr: &Expr,
    policy: &DmsLookupPolicy,
    found: &mut BTreeSet<String>,
) -> Result<(), DmsLookupError> {
    match unnest(expr) {
        Expr::BinaryOp { left, op: BinaryOperator::And, right } => {
            validate_lookup_predicate(left, policy, found)?;
            validate_lookup_predicate(right, policy, found)
        }
        Expr::BinaryOp { op: BinaryOperator::Or | BinaryOperator::Xor, .. } => {
            Err(reject("禁止 OR/XOR 条件"))
        }
        Expr::BinaryOp { left, op: BinaryOperator::Eq, right }
            if (is_column(left) && is_literal(right)) || (is_literal(left) && is_column(right)) =>
        {
            if let Some(column) = indexed_side(left, right, policy.lookup_cols) {
                let literal = if is_column(left) { right } else { left };
                require_registered_key_literal(policy.table, column, literal)?;
                found.insert(column.to_ascii_lowercase());
                return Ok(());
            }
            if is_soft_delete_predicate(left, right) {
                return Ok(());
            }
            Err(reject("WHERE 条件列必须是登记的索引键；仅额外允许 deleted_flag = 0"))
        }
        Expr::InList { expr, list, negated: false }
            if is_column(expr)
                && !list.is_empty()
                && list.len() <= DMS_LOOKUP_MAX_IN_ITEMS
                && list.iter().all(is_literal) =>
        {
            if let Some(column) = indexed_column(expr, policy.lookup_cols) {
                for literal in list {
                    require_registered_key_literal(policy.table, column, literal)?;
                }
                found.insert(column.to_ascii_lowercase());
                return Ok(());
            }
            Err(reject("IN 只允许用于登记的索引键"))
        }
        Expr::InList { list, .. } if list.len() > DMS_LOOKUP_MAX_IN_ITEMS => Err(reject(format!(
            "IN 项数不得超过 {DMS_LOOKUP_MAX_IN_ITEMS}"
        ))),
        _ => Err(reject("WHERE 只允许索引键 = 常量或索引键 IN 小集合，并用 AND 连接")),
    }
}

/// 已登记的生产业务键均为字符编码。若允许 `code = 123`，MySQL 可能把字符列转换为
/// 数值再比较，已有 BTREE 也会退化为扫描；未登记的本地测试策略不承担该合同。
fn require_registered_key_literal(
    table: &str,
    column: &str,
    literal: &Expr,
) -> Result<(), DmsLookupError> {
    if registered_lookup_kind(table, column).is_some()
        && !matches!(unnest(literal), Expr::Value(Value::SingleQuotedString(_)))
    {
        return Err(reject("生产业务编码必须使用字符串常量，禁止隐式类型转换扫描"));
    }
    Ok(())
}

fn unnest(mut expr: &Expr) -> &Expr {
    while let Expr::Nested(inner) = expr {
        expr = inner;
    }
    expr
}

fn is_column(expr: &Expr) -> bool {
    matches!(unnest(expr), Expr::Identifier(_) | Expr::CompoundIdentifier(_))
}

fn is_literal(expr: &Expr) -> bool {
    matches!(
        unnest(expr),
        Expr::Value(
            Value::Number(_, false)
                | Value::SingleQuotedString(_)
                | Value::Boolean(_)
        )
    )
}

fn is_soft_delete_predicate(left: &Expr, right: &Expr) -> bool {
    let (column, value) = if is_column(left) { (left, right) } else { (right, left) };
    if !column_name(column).is_some_and(|name| name.eq_ignore_ascii_case("deleted_flag")) {
        return false;
    }
    matches!(
        unnest(value),
        Expr::Value(Value::Number(value, false)) if value == "0"
    ) || matches!(unnest(value), Expr::Value(Value::Boolean(false)))
}

fn column_name(expr: &Expr) -> Option<&str> {
    match unnest(expr) {
        Expr::Identifier(id) => Some(id.value.as_str()),
        Expr::CompoundIdentifier(ids) => ids.last().map(|id| id.value.as_str()),
        _ => None,
    }
}

fn indexed_column<'a>(expr: &'a Expr, lookup_cols: &[&str]) -> Option<&'a str> {
    column_name(expr).filter(|column| {
        lookup_cols.iter().any(|allowed| column.eq_ignore_ascii_case(allowed))
    })
}

fn indexed_side<'a>(
    left: &'a Expr,
    right: &'a Expr,
    lookup_cols: &[&str],
) -> Option<&'a str> {
    if is_column(left) {
        indexed_column(left, lookup_cols)
    } else {
        indexed_column(right, lookup_cols)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MysqlDialect;

    fn gate(sql: &str) -> Result<DmsLookupSql, DmsLookupError> {
        const ORDERS: DmsLookupPolicy =
            DmsLookupPolicy::new("orders", &["id", "order_code", "code"]);
        gate_dms_lookup_with(
            sql,
            &MysqlDialect,
            &["login_pwd", "password"],
            &ORDERS,
        )
    }

    fn gate_scoped(sql: &str) -> Result<DmsLookupSql, DmsLookupError> {
        gate_dms_scoped_with(sql, &MysqlDialect, &["login_pwd", "password"])
    }

    fn rejected(sql: &str, fragment: &str) {
        let err = gate(sql).err().expect("应拒绝").to_string();
        assert!(err.contains(fragment), "{sql}\n{err}");
    }

    #[test]
    fn accepts_exact_and_small_in_and_rejects_large_limit() {
        let a = gate("SELECT id, order_code FROM orders WHERE order_code = 'SO-1'").unwrap();
        assert!(a.wire().ends_with("LIMIT 50"), "{}", a.wire());
        rejected("SELECT id FROM orders WHERE code IN ('C1','C2') LIMIT 999", "不得超过 50");
        let c = gate("SELECT id, name FROM orders WHERE code IN ('C1','C2') AND deleted_flag = 0 LIMIT 3").unwrap();
        assert!(c.wire().ends_with("LIMIT 3"), "{}", c.wire());
    }

    #[test]
    fn rejects_every_expensive_query_shape() {
        for (sql, why) in [
            ("SELECT orders.id FROM orders a JOIN detail b ON a.id=b.id WHERE a.id=1", "一张物理表"),
            ("SELECT * FROM orders, detail WHERE orders.id=1", "一张物理表"),
            ("SELECT * FROM orders WHERE id=1 UNION SELECT * FROM detail WHERE id=1", "UNION"),
            ("SELECT * FROM orders WHERE id=1 INTERSECT SELECT * FROM detail WHERE id=1", "INTERSECT"),
            ("SELECT * FROM orders WHERE id=1 EXCEPT SELECT * FROM detail WHERE id=1", "EXCEPT"),
            ("WITH x AS (SELECT * FROM orders) SELECT * FROM x WHERE id=1", "CTE"),
            ("SELECT * FROM (SELECT * FROM orders) x WHERE id=1", "物理表"),
            ("SELECT * FROM orders WHERE id IN (SELECT id FROM detail)", "子查询"),
            ("SELECT COUNT(*) FROM orders WHERE id=1", "聚合函数"),
            ("SELECT LOWER(name) FROM orders WHERE id=1", "禁止函数调用"),
            ("SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM orders WHERE id=1", "窗口函数"),
            ("SELECT id FROM orders WHERE id=1 GROUP BY id", "GROUP BY"),
            ("SELECT id FROM orders WHERE id=1 HAVING id=1", "HAVING"),
            ("SELECT DISTINCT id FROM orders WHERE id=1", "DISTINCT"),
            ("SELECT * FROM orders WHERE name LIKE '长沙%' ORDER BY id", "ORDER BY"),
            ("SELECT id INTO OUTFILE '/tmp/x' FROM orders WHERE id=1", "只读红线"),
            ("SELECT * FROM orders WHERE id=1 FOR UPDATE", "行锁"),
            ("SELECT * FROM orders WHERE id=1 LIMIT 1+1", "LIMIT 必须"),
            ("SELECT * FROM orders WHERE id=1 LIMIT -1", "LIMIT 必须"),
            ("SELECT * FROM orders WHERE id=1 LIMIT 1 OFFSET 1", "OFFSET"),
        ] {
            rejected(sql, why);
        }
    }

    #[test]
    fn rejects_unbounded_or_non_indexable_predicates() {
        rejected("SELECT id FROM orders", "WHERE");
        rejected("SELECT id FROM orders WHERE id=1 OR 1=1", "OR");
        rejected("SELECT name FROM orders WHERE name LIKE '%长沙%'", "WHERE 只允许");
        rejected("SELECT name FROM orders WHERE name LIKE '长_沙%'", "WHERE 只允许");
        rejected("SELECT id FROM orders WHERE created_at >= '2026-01-01'", "WHERE 只允许");
        rejected("SELECT id FROM orders WHERE LOWER(code) = 'x'", "WHERE 只允许");
        rejected("SELECT id FROM orders WHERE deleted_flag = 0", "缺少调用方声明");
        rejected(
            "SELECT id FROM orders WHERE order_code = 'SO-1' AND tenant_id = 7",
            "条件列必须是登记的索引键",
        );
        rejected(
            "SELECT id FROM orders WHERE order_code = 'SO-1' AND name IN ('长沙')",
            "IN 只允许用于登记的索引键",
        );
        rejected(
            "SELECT id FROM orders WHERE id IN (1,2,3,4,5,6)",
            "IN 项数",
        );
    }

    #[test]
    fn keeps_common_guard_protections() {
        rejected("SELECT password FROM orders WHERE id=1", "敏感列");
        rejected("SELECT * FROM orders WHERE id=1; SELECT * FROM orders WHERE id=2", "单条");
        rejected("DELETE FROM orders WHERE id=1", "SELECT");
        rejected("SELECT id FROM customer WHERE id=1", "登记表 orders");
        rejected("SELECT 1 FROM orders WHERE id=1", "投影只允许显式字段");
        rejected("SELECT id AS code FROM orders WHERE id=1", "投影只允许显式字段");
        rejected("SELECT ghost.id FROM orders WHERE ghost.id=1", "字段限定符");
        assert!(gate("SELECT x FROM orders AS o(x) WHERE x=1").is_err());

        const ORDERS: DmsLookupPolicy = DmsLookupPolicy::new("orders", &["id"]);
        let hard_denied = gate_dms_lookup_with(
            "SELECT access_token FROM orders WHERE id=1",
            &MysqlDialect,
            &[],
            &ORDERS,
        );
        assert!(hard_denied.is_err(), "凭据字段不得依赖调用方词表才被拒绝");

        let malformed = gate("SELECT id FROM orders WHERE id = 'credential-secret")
            .err()
            .expect("非法 SQL 应拒绝");
        assert!(!malformed.to_string().contains("credential-secret"));
    }

    #[test]
    fn scoped_gate_uses_fixed_table_and_business_key_registry() {
        let exact = gate_scoped(
            "SELECT customer_code, customer_name FROM t_customer \
             WHERE customer_code = 'C1' AND deleted_flag = 0 LIMIT 50",
        )
        .unwrap();
        assert!(exact.wire().ends_with("LIMIT 50"), "{}", exact.wire());
        for sql in [
            "SELECT customer_code FROM t_customer WHERE customer_code = 123 LIMIT 1",
            "SELECT customer_code FROM t_customer WHERE customer_code IN ('C1', 2) LIMIT 2",
        ] {
            assert!(gate_scoped(sql).is_err(), "生产字符业务键不应接受数值常量: {sql}");
        }

        for sql in [
            "SELECT * FROM t_customer WHERE deleted_flag = 0 LIMIT 10",
            "SELECT * FROM t_customer WHERE customer_name = '长沙客户' LIMIT 10",
            "SELECT * FROM t_goods WHERE goods_name LIKE '长才%' LIMIT 10",
            "SELECT * FROM t_goods WHERE goods_short_name = '长才' LIMIT 10",
            "SELECT * FROM t_employee WHERE employee_id = 1 LIMIT 1",
            "SELECT * FROM t_sales_order WHERE sales_order_code = 'SO-1' ORDER BY id",
            "SELECT COUNT(*) FROM t_sales_order WHERE sales_order_code = 'SO-1'",
            "SELECT * FROM t_sales_order a JOIN t_sales_order_detail b ON a.sales_order_code=b.sales_order_code WHERE a.sales_order_code='SO-1'",
            "SELECT * FROM t_sales_order WHERE sales_order_code = 'SO-1' LIMIT 51",
            "SELECT * FROM other_db.t_sales_order WHERE sales_order_code = 'SO-1' LIMIT 1",
        ] {
            assert!(gate_scoped(sql).is_err(), "生产通用 ScopedSql 不应放行: {sql}");
        }
    }

    #[test]
    fn purchase_transfer_is_denied_even_with_a_caller_policy() {
        const TRANSFER: DmsLookupPolicy =
            DmsLookupPolicy::new("t_winc_purchase_transfer", &["bill_code"]);
        let sql = "SELECT bill_code FROM t_winc_purchase_transfer WHERE bill_code = 'PT-1' LIMIT 1";
        let err = gate_dms_lookup_with(sql, &MysqlDialect, &[], &TRANSFER)
            .err()
            .expect("缺少行权限合同的生产表必须拒绝")
            .to_string();
        assert!(err.contains("行权限合同"), "{err}");
        assert!(gate_scoped(sql).is_err());
        assert!(registered_lookup_keys().all(|(table, _, _)| table != "t_winc_purchase_transfer"));
    }

    #[test]
    fn indexed_policy_preserves_physical_index_requirement() {
        const DETAILS: DmsLookupPolicy =
            DmsLookupPolicy::indexed("order_detail", &["order_code"]);
        let checked = gate_dms_lookup_with(
            "SELECT order_code, goods_code FROM order_detail WHERE order_code = 'SO-1' LIMIT 50",
            &MysqlDialect,
            &[],
            &DETAILS,
        )
        .unwrap();
        assert_eq!(checked.index_kind(), DmsIndexKind::Leading);
    }
}
