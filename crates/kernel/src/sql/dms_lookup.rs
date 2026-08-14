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
/// 判据是**子串匹配**（contains），宁宽勿漏：`tokenizer_ver`（含 token）、`phone_ext`
/// 这类列名一并拒 —— 多拒方向是刻意的。
const DMS_SENSITIVE_FRAGMENTS: &[&str] = &[
    "password", "passwd", "login_pwd", "credential", "secret", "token", "phone", "mobile",
    "telephone", "email", "address", "bank_account", "bank_card", "bank_name", "invoice_bank",
    "tax_no", "tax_number", "taxpayer", "social_credit", "identity_card", "id_card",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmsIndexKind {
    /// 主表/主档点查必须由单列主键或单列唯一索引承载。
    Unique,
    /// 一对多补充明细允许普通索引，但过滤列必须位于索引第一列。
    Leading,
}

/// 服务端代码登记的单表索引点查策略。请求参数只能提供值，不能提供表、键或索引类型。
pub struct DmsLookupPolicy {
    table: &'static str,
    lookup_cols: &'static [&'static str],
    index_kind: DmsIndexKind,
}

impl DmsLookupPolicy {
    pub const fn new(table: &'static str, lookup_cols: &'static [&'static str]) -> Self {
        Self { table, lookup_cols, index_kind: DmsIndexKind::Unique }
    }

    pub const fn indexed(table: &'static str, lookup_cols: &'static [&'static str]) -> Self {
        Self { table, lookup_cols, index_kind: DmsIndexKind::Leading }
    }

    pub const fn table(&self) -> &'static str {
        self.table
    }

    pub const fn lookup_cols(&self) -> &'static [&'static str] {
        self.lookup_cols
    }
}

/// 业务层注入的点查键元数据。kernel 只解释这些约束，不保存任何业务表或字段目录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmsLookupKey {
    table: &'static str,
    column: &'static str,
    kind: DmsIndexKind,
}

impl DmsLookupKey {
    pub const fn new(
        table: &'static str,
        column: &'static str,
        kind: DmsIndexKind,
    ) -> Self {
        Self { table, column, kind }
    }
}

/// 业务层的点查策略快照。具体 DMS 表、业务键、索引要求与拒绝目录必须由上层注入；
/// kernel 只保留通用 SQL AST、安全形状与 LIMIT 验证，避免业务目录反向固化进内核。
pub struct DmsLookupRegistry {
    scoped_policies: &'static [DmsLookupPolicy],
    registered_keys: &'static [DmsLookupKey],
    uncontracted_tables: &'static [&'static str],
}

impl DmsLookupRegistry {
    pub const fn new(
        scoped_policies: &'static [DmsLookupPolicy],
        registered_keys: &'static [DmsLookupKey],
        uncontracted_tables: &'static [&'static str],
    ) -> Self {
        Self { scoped_policies, registered_keys, uncontracted_tables }
    }

    pub fn scoped_policies(&self) -> &[DmsLookupPolicy] {
        self.scoped_policies
    }

    pub fn registered_lookup_keys(
        &self,
    ) -> impl Iterator<Item = (&'static str, &'static str, DmsIndexKind)> + '_ {
        self.registered_keys.iter().map(|key| (key.table, key.column, key.kind))
    }

    pub fn registered_lookup_kind(&self, table: &str, column: &str) -> Option<DmsIndexKind> {
        self.registered_keys
            .iter()
            .find(|key| {
                table.eq_ignore_ascii_case(key.table)
                    && column.eq_ignore_ascii_case(key.column)
            })
            .map(|key| key.kind)
    }

    fn rejects_table(&self, table: &str) -> bool {
        self.uncontracted_tables
            .iter()
            .any(|denied| table.eq_ignore_ascii_case(denied))
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

/// 标识符小写化：本身全小写（DMS 表名常态）零分配借用，含大写才落一份拷贝
fn lower_ident(s: &str) -> std::borrow::Cow<'_, str> {
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(s.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// 严格生产点查 gate。`sensitive` 与通用 guard 共用业务侧词表，但查询形状单独判定。
pub fn gate_dms_lookup_with(
    sql: &str,
    d: &'static dyn Dialect,
    sensitive: &[&str],
    policy: &DmsLookupPolicy,
) -> Result<DmsLookupSql, DmsLookupError> {
    let mut query = parse_query(sql, d, sensitive)?;
    let (lookup_cols, table) = validate_query(&query, policy, None)?;
    normalize_limit(&mut query);
    Ok(DmsLookupSql {
        wire: query.to_string(),
        table,
        lookup_cols,
        index_kind: policy.index_kind,
    })
}

/// 带业务注册表的严格点查 gate。`registry` 负责业务键类型与禁用表合同；不传时仅执行
/// 调用方策略和通用 SQL 安全验证，供与具体业务无关的 kernel 调用方与单测使用。
pub fn gate_dms_lookup_registered_with(
    sql: &str,
    d: &'static dyn Dialect,
    sensitive: &[&str],
    policy: &DmsLookupPolicy,
    registry: &DmsLookupRegistry,
) -> Result<DmsLookupSql, DmsLookupError> {
    let mut query = parse_query(sql, d, sensitive)?;
    let (lookup_cols, table) = validate_query(&query, policy, Some(registry))?;
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
    registry: &DmsLookupRegistry,
) -> Result<DmsLookupSql, DmsLookupError> {
    let mut query = parse_query(sql, d, sensitive)?;
    let table = lower_ident(query_table_name(&query)?).into_owned();
    // policy.table 是代码内全小写常量，table 已小写化：`==` 即可，不必 eq_ignore_ascii_case
    let policy = registry
        .scoped_policies()
        .iter()
        .find(|policy| table == policy.table)
        .ok_or_else(|| reject(format!("表 {table} 未登记为生产 DMS 轻查询表")))?;
    let (lookup_cols, _) = validate_query(&query, policy, Some(registry))?;
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

/// LIMIT 常量 → usize（validate 的校验与 normalize_limit 的 clamp 共用一份）
fn const_limit_usize(e: &Expr) -> Option<usize> {
    match e {
        Expr::Value(Value::Number(n, false)) => n.parse::<usize>().ok(),
        _ => None,
    }
}

fn normalize_limit(query: &mut Query) {
    // validate 已保证 limit ≤ 50，这里的 min 是纵深第二道（双保险，别删）
    let limit = query
        .limit
        .as_ref()
        .and_then(|e| const_limit_usize(e))
        .unwrap_or(DMS_LOOKUP_MAX_ROWS)
        .min(DMS_LOOKUP_MAX_ROWS);
    query.limit = Some(Expr::Value(Value::Number(limit.to_string(), false)));
}

fn validate_query(
    query: &Query,
    policy: &DmsLookupPolicy,
    registry: Option<&DmsLookupRegistry>,
) -> Result<(Vec<String>, String), DmsLookupError> {
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
        match const_limit_usize(limit) {
            Some(v) if v <= DMS_LOOKUP_MAX_ROWS => {}
            Some(_) => return Err(reject(format!("LIMIT 不得超过 {DMS_LOOKUP_MAX_ROWS}"))),
            None => {
                // 「不是整数常量」与「整数超出可表示范围」（如 20 位 9 溢出）是两种事故
                let overflow = matches!(limit, Expr::Value(Value::Number(_, false)));
                return Err(reject(if overflow {
                    "LIMIT 整数超出可表示范围"
                } else {
                    "LIMIT 必须是整数常量"
                }));
            }
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
    if registry.is_some_and(|registry| registry.rejects_table(actual_table)) {
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
    validate_lookup_predicate(where_expr, policy, registry, &mut lookup_cols)?;
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
    // 顺带返回实际表名（小写）：调用方不必再走一遍 query_table_name
    Ok((lookup_cols.into_iter().collect(), lower_ident(actual_table).into_owned()))
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
    let (name, alias) = match relation {
        TableFactor::Table {
            args: None,
            with_hints,
            version: None,
            with_ordinality: false,
            partitions,
            json_path: None,
            name,
            alias,
            ..
        } if with_hints.is_empty() && partitions.is_empty() => (name, alias),
        _ => return Err(reject("FROM 只能是物理表，禁止派生表和表函数")),
    };
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
            // to_string + 原地小写化：一次分配（不是 to_string().to_ascii_lowercase() 两次）
            let mut name = fun.name.to_string();
            name.make_ascii_lowercase();
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
        // rsplit 恒产出至少一段，无 fallback 分支
        name.rsplit('.').next().expect("rsplit 恒非空"),
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
    registry: Option<&DmsLookupRegistry>,
    found: &mut BTreeSet<String>,
) -> Result<(), DmsLookupError> {
    match unnest(expr) {
        Expr::BinaryOp { left, op: BinaryOperator::And, right } => {
            validate_lookup_predicate(left, policy, registry, found)?;
            validate_lookup_predicate(right, policy, registry, found)
        }
        Expr::BinaryOp { op: BinaryOperator::Or | BinaryOperator::Xor, .. } => {
            Err(reject("禁止 OR/XOR 条件"))
        }
        Expr::BinaryOp { left, op: BinaryOperator::Eq, right }
            if (is_column(left) && is_literal(right)) || (is_literal(left) && is_column(right)) =>
        {
            if let Some(column) = indexed_side(left, right, policy.lookup_cols) {
                let literal = if is_column(left) { right } else { left };
                require_registered_key_literal(registry, policy.table, column, literal)?;
                found.insert(column.to_ascii_lowercase());
                return Ok(());
            }
            if is_soft_delete_predicate(left, right) {
                return Ok(());
            }
            Err(reject("WHERE 条件列必须是登记的索引键；仅额外允许 deleted_flag = 0（数值 0）"))
        }
        Expr::InList { expr, list, negated: false }
            if is_column(expr)
                && !list.is_empty()
                && list.len() <= DMS_LOOKUP_MAX_IN_ITEMS
                && list.iter().all(is_literal) =>
        {
            if let Some(column) = indexed_column(expr, policy.lookup_cols) {
                for literal in list {
                    require_registered_key_literal(registry, policy.table, column, literal)?;
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
/// 只认**单引号**字符串字面量：MySQL 默认下 `"SO-1"` 也是字符串，但这里不放行
/// DoubleQuotedString（安全方向，双引号形态按拒绝处理，未见真实需求）。
fn require_registered_key_literal(
    registry: Option<&DmsLookupRegistry>,
    table: &str,
    column: &str,
    literal: &Expr,
) -> Result<(), DmsLookupError> {
    if registry
        .and_then(|registry| registry.registered_lookup_kind(table, column))
        .is_some()
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

    const SCOPED_POLICIES: &[DmsLookupPolicy] = &[
        DmsLookupPolicy::new("customer", &["customer_code"]),
        DmsLookupPolicy::new("goods", &["goods_code"]),
    ];
    const REGISTERED_KEYS: &[DmsLookupKey] = &[
        DmsLookupKey::new("customer", "customer_code", DmsIndexKind::Unique),
        DmsLookupKey::new("goods", "goods_code", DmsIndexKind::Unique),
    ];
    const UNCONTRACTED: &[&str] = &["purchase_transfer"];
    const REGISTRY: DmsLookupRegistry =
        DmsLookupRegistry::new(SCOPED_POLICIES, REGISTERED_KEYS, UNCONTRACTED);

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
        gate_dms_scoped_with(
            sql,
            &MysqlDialect,
            &["login_pwd", "password"],
            &REGISTRY,
        )
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
            "SELECT customer_code, customer_name FROM customer \
             WHERE customer_code = 'C1' AND deleted_flag = 0 LIMIT 50",
        )
        .unwrap();
        assert!(exact.wire().ends_with("LIMIT 50"), "{}", exact.wire());
        for sql in [
            "SELECT customer_code FROM customer WHERE customer_code = 123 LIMIT 1",
            "SELECT customer_code FROM customer WHERE customer_code IN ('C1', 2) LIMIT 2",
        ] {
            assert!(gate_scoped(sql).is_err(), "生产字符业务键不应接受数值常量: {sql}");
        }

        for sql in [
            "SELECT * FROM customer WHERE deleted_flag = 0 LIMIT 10",
            "SELECT * FROM customer WHERE customer_name = '长沙客户' LIMIT 10",
            "SELECT * FROM goods WHERE goods_name LIKE '长才%' LIMIT 10",
            "SELECT * FROM goods WHERE goods_short_name = '长才' LIMIT 10",
            "SELECT * FROM employee WHERE employee_id = 1 LIMIT 1",
            "SELECT * FROM sales_order WHERE sales_order_code = 'SO-1' ORDER BY id",
            "SELECT COUNT(*) FROM sales_order WHERE sales_order_code = 'SO-1'",
            "SELECT * FROM sales_order a JOIN sales_order_detail b ON a.sales_order_code=b.sales_order_code WHERE a.sales_order_code='SO-1'",
            "SELECT * FROM sales_order WHERE sales_order_code = 'SO-1' LIMIT 51",
            "SELECT * FROM other_db.sales_order WHERE sales_order_code = 'SO-1' LIMIT 1",
        ] {
            assert!(gate_scoped(sql).is_err(), "生产通用 ScopedSql 不应放行: {sql}");
        }
    }

    #[test]
    fn purchase_transfer_is_denied_even_with_a_caller_policy() {
        const TRANSFER: DmsLookupPolicy =
            DmsLookupPolicy::new("purchase_transfer", &["bill_code"]);
        let sql = "SELECT bill_code FROM purchase_transfer WHERE bill_code = 'PT-1' LIMIT 1";
        let err = gate_dms_lookup_registered_with(sql, &MysqlDialect, &[], &TRANSFER, &REGISTRY)
            .err()
            .expect("缺少行权限合同的生产表必须拒绝")
            .to_string();
        assert!(err.contains("行权限合同"), "{err}");
        assert!(gate_scoped(sql).is_err());
        assert!(REGISTRY
            .registered_lookup_keys()
            .all(|(table, _, _)| table != "purchase_transfer"));
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

    /// 策略表与登记键表是两份平行常量：漂移防线 = 策略里每个 (表,键) 必在登记键表里
    #[test]
    fn every_scoped_policy_key_is_a_registered_lookup_key() {
        for policy in SCOPED_POLICIES {
            for col in policy.lookup_cols() {
                assert!(
                    REGISTRY.registered_lookup_kind(policy.table(), col).is_some(),
                    "SCOPED_POLICIES 的 ({}, {}) 未在 REGISTERED_KEYS 登记",
                    policy.table(),
                    col
                );
            }
        }
    }
}
