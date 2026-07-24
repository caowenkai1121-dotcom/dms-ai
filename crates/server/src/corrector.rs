//! SchemaCorrector：执行前字段白名单校验（移植 SuperSonic SchemaCorrector.correctFieldName）。
//! LLM 生成的 SQL 里，凡「真实表.列」形式的列引用，若该列不在 meta.column_doc 记录的真实列清单里，
//! 判为幻觉列 → 携精确「可用列清单」自修一次（比执行报 1054 更早、给 LLM 更准的纠正依据）。
//! 只校验带表前缀且前缀映射到 meta 已知物理表的列——派生表/CTE 别名列、裸列、中文别名跳过，防误伤。

use std::collections::{HashMap, HashSet};

use core::ops::ControlFlow;
use sqlparser::ast::{Expr, TableFactor, Visit, VisitMut, Visitor, VisitorMut};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use sqlx::PgPool;

#[derive(Default)]
struct Collector {
    /// (别名或表名 lower, 真实表名 lower)
    aliases: Vec<(String, String)>,
    /// (前缀 lower, 列名 lower)
    cols: Vec<(String, String)>,
}

impl Visitor for Collector {
    type Break = ();

    fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table { name, alias, .. } = tf {
            let table = name.0.last().map(|p| p.value.to_lowercase()).unwrap_or_default();
            let key = alias
                .as_ref()
                .map(|a| a.name.value.to_lowercase())
                .unwrap_or_else(|| table.clone());
            self.aliases.push((key, table));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        if let Expr::CompoundIdentifier(parts) = e {
            if parts.len() >= 2 {
                let prefix = parts[parts.len() - 2].value.to_lowercase();
                let col = parts[parts.len() - 1].value.to_lowercase();
                self.cols.push((prefix, col));
            }
        }
        ControlFlow::Continue(())
    }
}

/// 提取 (别名→表, 带前缀列引用)。纯函数，可单测。
fn collect(sql: &str) -> anyhow::Result<(HashMap<String, String>, Vec<(String, String)>)> {
    let stmts = Parser::parse_sql(&MySqlDialect {}, sql)?;
    let mut c = Collector::default();
    for s in &stmts {
        let _ = s.visit(&mut c);
    }
    // 后出现的别名覆盖（同别名罕见）；派生表别名不会进 aliases（TableFactor::Derived 无 name）
    let amap: HashMap<String, String> = c.aliases.into_iter().collect();
    Ok((amap, c.cols))
}

/// 执行前字段校验。返回 Some(自修提示) 表示发现幻觉列，None 表示通过。
pub async fn schema_check(pg: &PgPool, sql: &str) -> anyhow::Result<Option<String>> {
    let (amap, cols) = collect(sql)?;
    if cols.is_empty() {
        return Ok(None);
    }
    // 涉及的真实表 → 从 meta.column_doc 取真实列集合（只对 meta 已知表校验）
    let real_tables: HashSet<String> = amap.values().cloned().collect();
    let mut table_cols: HashMap<String, HashSet<String>> = HashMap::new();
    for t in &real_tables {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT lower(column_name) FROM meta.column_doc WHERE lower(table_name) = $1")
                .bind(t)
                .fetch_all(pg)
                .await?;
        if !rows.is_empty() {
            table_cols.insert(t.clone(), rows.into_iter().map(|(c,)| c).collect());
        }
    }
    if table_cols.is_empty() {
        return Ok(None); // 没有一张表在 meta 里（纯派生/未采集），不校验
    }

    // 找幻觉列：前缀映射到 meta 已知表，但列不在该表列集
    let mut bad: Vec<(String, String)> = vec![]; // (表, 幻觉列)
    let mut seen = HashSet::new();
    for (prefix, col) in &cols {
        if let Some(table) = amap.get(prefix) {
            if let Some(known) = table_cols.get(table) {
                if !known.contains(col) && seen.insert((table.clone(), col.clone())) {
                    bad.push((table.clone(), col.clone()));
                }
            }
        }
    }
    if bad.is_empty() {
        return Ok(None);
    }

    // 组织自修提示：幻觉列 + 该表真实可用列清单（给 LLM 精确纠正依据）
    let mut hint = String::from("SQL 引用了不存在的列（幻觉列），请改用下方真实列名重写：\n");
    let mut listed: HashSet<String> = HashSet::new();
    for (table, col) in &bad {
        hint.push_str(&format!("- 列 {table}.{col} 不存在。"));
        if listed.insert(table.clone()) {
            if let Some(known) = table_cols.get(table) {
                let mut names: Vec<&String> = known.iter().collect();
                names.sort();
                let list = names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                hint.push_str(&format!("{table} 的真实列有：{list}"));
            }
        }
        hint.push('\n');
    }
    Ok(Some(hint))
}

/// 值链接码表：(表,列) → [(中文名, 码, match_kind)]。match_kind: eq=等值换码 / like=组合值列须 LIKE '%码%'
pub type ValueMaps = HashMap<(String, String), Vec<(String, String, String)>>;

/// ValueLinker（移植 SuperSonic 值链接纠正）：编码列上「中文名直写」确定性换码。
/// 真坑：invoice_status='已开票' 必返 0 行（库存码 2）；paid_way='可开票余额支付' 等值必返 0 行（组合值须 LIKE）。
/// 门控：带前缀列且前缀映射到 meta 已知物理表（裸列/派生表不碰）；eq 列换码值，like 列 '=' 改写 LIKE '%码%'；
/// IN 列表逐项换（like 列跳过）；已是码值/无名命中不动。
struct Linker<'a> {
    aliases: &'a HashMap<String, String>,
    maps: &'a ValueMaps,
    changed: bool,
}

impl<'a> Linker<'a> {
    /// 「前缀.列」解析出 (表,列)。裸列不解析（防误伤）。
    fn resolve_key(&self, e: &Expr) -> Option<(String, String)> {
        let Expr::CompoundIdentifier(parts) = e else { return None };
        if parts.len() < 2 {
            return None;
        }
        let prefix = parts[parts.len() - 2].value.to_lowercase();
        let col = parts[parts.len() - 1].value.to_lowercase();
        let table = self.aliases.get(&prefix)?;
        Some((table.clone(), col))
    }

    /// 在 (表,列) 的码表里按中文名找 (码,kind)
    fn find(&self, key: &(String, String), name: &str) -> Option<(String, String)> {
        self.maps
            .get(key)?
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, c, k)| (c.clone(), k.clone()))
    }
}

impl<'a> VisitorMut for Linker<'a> {
    type Break = ();

    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<()> {
        match e {
            // col = '中文名'（及镜像 '中文名' = col）→ 换码；like 列改 LIKE '%码%'
            Expr::BinaryOp { left, op: sqlparser::ast::BinaryOperator::Eq, right } => {
                // 找出哪一侧是列、哪一侧是字符串字面量
                let (col_side, lit_side_is_right) = if matches!(left.as_ref(), Expr::CompoundIdentifier(_))
                    && matches!(right.as_ref(), Expr::Value(sqlparser::ast::Value::SingleQuotedString(_)))
                {
                    (true, true)
                } else if matches!(right.as_ref(), Expr::CompoundIdentifier(_))
                    && matches!(left.as_ref(), Expr::Value(sqlparser::ast::Value::SingleQuotedString(_)))
                {
                    (false, false)
                } else {
                    return ControlFlow::Continue(());
                };
                let _ = col_side;
                let (col_expr, lit_expr) = if lit_side_is_right {
                    (left.as_ref(), right.as_ref())
                } else {
                    (right.as_ref(), left.as_ref())
                };
                let Some(key) = self.resolve_key(col_expr) else { return ControlFlow::Continue(()) };
                let Expr::Value(sqlparser::ast::Value::SingleQuotedString(name)) = lit_expr else {
                    return ControlFlow::Continue(());
                };
                let Some((code, kind)) = self.find(&key, name) else {
                    return ControlFlow::Continue(());
                };
                if kind == "like" {
                    // 组合值列：'=' 改写 LIKE '%码%'（等值必返 0 行）
                    let new_expr = Expr::Like {
                        negated: false,
                        any: false,
                        expr: Box::new(col_expr.clone()),
                        pattern: Box::new(Expr::Value(sqlparser::ast::Value::SingleQuotedString(
                            format!("%{code}%"),
                        ))),
                        escape_char: None,
                    };
                    *e = new_expr;
                } else if lit_side_is_right {
                    *right = Box::new(Expr::Value(sqlparser::ast::Value::SingleQuotedString(code)));
                } else {
                    *left = Box::new(Expr::Value(sqlparser::ast::Value::SingleQuotedString(code)));
                }
                self.changed = true;
            }
            // col IN ('名1','名2') → 逐项换码（like 列跳过）
            Expr::InList { expr, list, negated: false } => {
                let Some(key) = self.resolve_key(expr) else { return ControlFlow::Continue(()) };
                let mut replaced: Vec<(usize, String)> = vec![];
                for (i, item) in list.iter().enumerate() {
                    if let Expr::Value(sqlparser::ast::Value::SingleQuotedString(name)) = item {
                        if let Some((code, kind)) = self.find(&key, name) {
                            if kind == "eq" {
                                replaced.push((i, code));
                            }
                        }
                    }
                }
                for (i, code) in replaced {
                    list[i] = Expr::Value(sqlparser::ast::Value::SingleQuotedString(code));
                    self.changed = true;
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// ValueLinker 纯核（可单测）：解析别名 → VisitMut 换码。
pub fn link_values_with(
    sql: &str,
    aliases: &HashMap<String, String>,
    maps: &ValueMaps,
) -> Option<String> {
    let mut stmts = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
    if maps.is_empty() {
        return None;
    }
    let mut linker = Linker { aliases, maps, changed: false };
    for s in &mut stmts {
        // VisitMut 节点 trait 方法名同为 visit；Linker 只实现 VisitorMut，解析唯一
        let _ = VisitMut::visit(s, &mut linker);
    }
    if linker.changed {
        Some(stmts.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(";\n"))
    } else {
        None
    }
}

/// ValueLinker 入口：加载涉及表的码表 → 换码。
pub async fn correct_value(pg: &PgPool, sql: &str) -> anyhow::Result<Option<String>> {
    let (amap, _) = collect(sql)?;
    let tables: HashSet<String> = amap.values().cloned().collect();
    if tables.is_empty() {
        return Ok(None);
    }
    let mut maps: ValueMaps = HashMap::new();
    for t in &tables {
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT column_name, name, code, match_kind FROM meta.value_map WHERE lower(table_name) = $1",
        )
        .bind(t)
        .fetch_all(pg)
        .await?;
        for (col, name, code, kind) in rows {
            maps.entry((t.clone(), col.to_lowercase()))
                .or_insert_with(Vec::new)
                .push((name, code, kind));
        }
    }
    Ok(link_values_with(sql, &amap, &maps))
}

/// AggCorrector 入口：问句命中指标 → agg_expr 解析规则 → normalize_agg 归一。
pub async fn correct_agg(pg: &PgPool, question: &str, sql: &str) -> anyhow::Result<Option<String>> {
    let rows: Vec<(String, Vec<String>, String)> =
        sqlx::query_as("SELECT name, aliases, agg_expr FROM meta.metric WHERE status = 'active'")
            .fetch_all(pg)
            .await?;
    // 列唯一命中一个指标才建规则（同列多指标歧义保守跳过）
    let mut by_col: HashMap<String, (String, bool)> = HashMap::new();
    let mut ambiguous: HashSet<String> = HashSet::new();
    for (name, aliases, agg) in &rows {
        let hit = question.contains(name.as_str())
            || aliases.iter().any(|a| question.contains(a.as_str()));
        if !hit {
            continue;
        }
        if let Some((func, col, distinct)) = parse_agg_rule(agg) {
            match by_col.get(&col) {
                Some(prev) if *prev != (func.clone(), distinct) => {
                    by_col.remove(&col);
                    ambiguous.insert(col);
                }
                Some(_) => {}
                None => {
                    if !ambiguous.contains(&col) {
                        by_col.insert(col, (func, distinct));
                    }
                }
            }
        }
    }
    let rules: Vec<AggRule> =
        by_col.into_iter().map(|(col, (func, d))| (func, col, d)).collect();
    Ok(normalize_agg(sql, &rules))
}

/// GroupByCorrector（移植 SuperSonic）：select 同时含聚合列和裸维度列却漏 GROUP BY 时，
/// 用裸维度列补上 GROUP BY（MySQL only_full_group_by 下漏 group by 直接报错）。纯 AST，确定性。
/// 保守门控：单表非复杂 SQL、已有 group by 不动、无聚合或无裸列不动。
pub fn fix_group_by(sql: &str) -> Option<String> {
    use sqlparser::ast::{
        GroupByExpr, Query, Select, SelectItem, SetExpr, Statement,
    };
    let mut stmts = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
    if stmts.len() != 1 {
        return None;
    }
    let Statement::Query(q) = &mut stmts[0] else {
        return None;
    };
    // 只处理顶层单 Select（子查询/union 跳过，防误伤）
    let Query { body, .. } = q.as_mut();
    let SetExpr::Select(sel) = body.as_mut() else {
        return None;
    };
    let sel: &mut Select = sel.as_mut();
    // 已有 group by 不动
    if let GroupByExpr::Expressions(v, _) = &sel.group_by {
        if !v.is_empty() {
            return None;
        }
    } else {
        return None; // GROUP BY ALL 等不处理
    }
    // 分离聚合项与裸维度项
    let mut has_agg = false;
    let mut dims: Vec<sqlparser::ast::Expr> = vec![];
    for item in &sel.projection {
        let expr = match item {
            SelectItem::UnnamedExpr(e) => Some(e),
            SelectItem::ExprWithAlias { expr, .. } => Some(expr),
            _ => return None, // 有 * 通配等，不处理
        };
        if let Some(e) = expr {
            if expr_has_agg(e) {
                has_agg = true;
            } else {
                dims.push(e.clone());
            }
        }
    }
    // 需同时有聚合和裸维度才补
    if !has_agg || dims.is_empty() {
        return None;
    }
    sel.group_by = GroupByExpr::Expressions(dims, vec![]);
    Some(stmts[0].to_string())
}

/// 表达式是否含聚合函数
fn expr_has_agg(e: &sqlparser::ast::Expr) -> bool {
    use sqlparser::ast::Expr;
    const AGG: &[&str] = &["sum", "count", "avg", "max", "min", "group_concat"];
    match e {
        Expr::Function(f) => f
            .name
            .0
            .last()
            .map(|p| AGG.contains(&p.value.to_lowercase().as_str()))
            .unwrap_or(false),
        Expr::BinaryOp { left, right, .. } => expr_has_agg(left) || expr_has_agg(right),
        Expr::Nested(e) | Expr::UnaryOp { expr: e, .. } | Expr::Cast { expr: e, .. } => expr_has_agg(e),
        _ => false,
    }
}

/// 聚合归一规则：(目标函数 lower, 聚合列 lower, 是否 DISTINCT)。从指标注册表 agg_expr 解析而来。
pub type AggRule = (String, String, bool);

/// 解析指标 agg_expr → AggRule。只接受单聚合形态（SUM(x)/COUNT(DISTINCT x)）；
/// 客单价 SUM(x)/NULLIF(COUNT...,0) 这类复合表达式保守跳过（无法映射单一默认聚合）。
pub fn parse_agg_rule(agg_expr: &str) -> Option<AggRule> {
    use sqlparser::ast::{FunctionArg, FunctionArguments, SelectItem, SetExpr, Statement};
    let stmts = Parser::parse_sql(&MySqlDialect {}, &format!("SELECT {agg_expr}")).ok()?;
    let Statement::Query(q) = &stmts[0] else { return None };
    let SetExpr::Select(sel) = q.body.as_ref() else { return None };
    let [SelectItem::UnnamedExpr(Expr::Function(f))] = &sel.projection[..] else { return None };
    let FunctionArguments::List(l) = &f.args else { return None };
    if l.args.len() != 1 {
        return None;
    }
    let col = match &l.args[0] {
        FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(e)) => last_ident(e)?,
        _ => return None, // COUNT(*) 等
    };
    let func = f.name.0.last()?.value.to_lowercase();
    let distinct = matches!(
        l.duplicate_treatment,
        Some(sqlparser::ast::DuplicateTreatment::Distinct)
    );
    Some((func, col, distinct))
}

/// 取标识符末段（t.col→col）。非标识符表达式返回 None。
fn last_ident(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(p) => Some(p.value.to_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.to_lowercase()),
        _ => None,
    }
}

/// AggCorrector（移植 SuperSonic correctAggFunction）：命中指标的聚合列归一到注册表默认聚合。
/// 问「订单数」LLM 写 COUNT(sales_order_code) → COUNT(DISTINCT sales_order_code)；
/// 问「销售额」写 AVG(total_amount) → SUM(total_amount)。口径以注册表为单一事实源。
/// 保守门控：仅顶层 SELECT 投影（子查询/WHERE 不碰）、列唯一命中一个指标、
/// 目标函数已被同列其他聚合占用则不改（防改出重复列）、COUNT(*) 不碰。
pub fn normalize_agg(sql: &str, rules: &[AggRule]) -> Option<String> {
    use sqlparser::ast::{Query, SelectItem, SetExpr, Statement};
    if rules.is_empty() {
        return None;
    }
    let mut stmts = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
    if stmts.len() != 1 {
        return None;
    }
    let Statement::Query(q) = &mut stmts[0] else { return None };
    let Query { body, .. } = q.as_mut();
    let SetExpr::Select(sel) = body.as_mut() else { return None };
    let sel = sel.as_mut();

    // 占用集：同列已被目标函数占用（如 SELECT SUM(x), AVG(x) 对比问法），改名会撞出重复列 → 该规则停用改名
    let occupied: HashSet<(String, String)> = rules
        .iter()
        .filter(|r| {
            sel.projection.iter().any(|item| {
                let e = match item {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
                    _ => return false,
                };
                proj_has_func_over(e, &r.0, &r.1)
            })
        })
        .map(|r| (r.0.clone(), r.1.clone()))
        .collect();
    let mut changed = false;
    for item in &mut sel.projection {
        let e = match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
            _ => continue,
        };
        rewrite_agg(e, rules, &occupied, &mut changed);
    }
    if changed {
        Some(stmts[0].to_string())
    } else {
        None
    }
}

/// 投影表达式中是否已存在 func(col) 形态（只下钻安全包装层，不进子查询）
fn proj_has_func_over(e: &Expr, func: &str, col: &str) -> bool {
    use sqlparser::ast::FunctionArguments;
    match e {
        Expr::Function(f) => {
            let name_ok = f
                .name
                .0
                .last()
                .map(|p| p.value.eq_ignore_ascii_case(func))
                .unwrap_or(false);
            let col_ok = match &f.args {
                FunctionArguments::List(l) if l.args.len() == 1 => match &l.args[0] {
                    sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(a)) => {
                        last_ident(a).map(|c| c == col).unwrap_or(false)
                    }
                    _ => false,
                },
                _ => false,
            };
            (name_ok && col_ok)
                || match &f.args {
                    FunctionArguments::List(l) => l.args.iter().any(|a| {
                        matches!(a, sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(x)) if proj_has_func_over(x, func, col))
                    }),
                    _ => false,
                }
        }
        Expr::BinaryOp { left, right, .. } => {
            proj_has_func_over(left, func, col) || proj_has_func_over(right, func, col)
        }
        Expr::Nested(x) | Expr::UnaryOp { expr: x, .. } | Expr::Cast { expr: x, .. } => {
            proj_has_func_over(x, func, col)
        }
        _ => false,
    }
}

/// 归一改写（只下钻安全包装层；子查询/Case 等停钻防误伤）
fn rewrite_agg(
    e: &mut Expr,
    rules: &[AggRule],
    occupied: &HashSet<(String, String)>,
    changed: &mut bool,
) {
    use sqlparser::ast::{DuplicateTreatment, FunctionArguments};
    match e {
        Expr::Function(f) => {
            let FunctionArguments::List(l) = &mut f.args else { return };
            if l.args.len() != 1 {
                return;
            }
            let col = match &l.args[0] {
                sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(a)) => {
                    match last_ident(a) {
                        Some(c) => c,
                        None => return,
                    }
                }
                _ => return, // COUNT(*) 不碰
            };
            let Some(rule) = rules.iter().find(|r| r.1 == col) else { return };
            let node_func = f
                .name
                .0
                .last()
                .map(|p| p.value.to_lowercase())
                .unwrap_or_default();
            let node_distinct = matches!(l.duplicate_treatment, Some(DuplicateTreatment::Distinct));
            if node_func == rule.0 {
                // 函数已对，补 DISTINCT（COUNT(code)→COUNT(DISTINCT code)）
                if rule.2 && !node_distinct {
                    l.duplicate_treatment = Some(DuplicateTreatment::Distinct);
                    *changed = true;
                }
            } else if matches!(node_func.as_str(), "sum" | "count" | "avg" | "max" | "min")
                && !occupied.contains(&(rule.0.clone(), rule.1.clone()))
            {
                // 函数名归一到指标默认聚合（目标形态未占用才改），并采用规则的 DISTINCT 形态
                if let Some(p) = f.name.0.last_mut() {
                    p.value = rule.0.to_uppercase();
                }
                l.duplicate_treatment =
                    if rule.2 { Some(DuplicateTreatment::Distinct) } else { None };
                *changed = true;
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            rewrite_agg(left, rules, occupied, changed);
            rewrite_agg(right, rules, occupied, changed);
        }
        Expr::Nested(x) | Expr::UnaryOp { expr: x, .. } | Expr::Cast { expr: x, .. } => {
            rewrite_agg(x, rules, occupied, changed);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
    }

    #[test]
    fn adds_missing_group_by() {
        // 省份 + SUM(金额) 漏 GROUP BY → 补
        let out = fix_group_by("SELECT province, SUM(total_amount) FROM t_sales_order").unwrap();
        assert!(norm(&out).contains("groupbyprovince"), "{out}");
    }

    #[test]
    fn keeps_existing_group_by() {
        assert!(fix_group_by("SELECT province, SUM(x) FROM t GROUP BY province").is_none());
    }

    #[test]
    fn pure_aggregate_untouched() {
        // 纯聚合无维度 → 不补
        assert!(fix_group_by("SELECT SUM(total_amount) FROM t_sales_order").is_none());
    }

    #[test]
    fn no_aggregate_untouched() {
        // 明细查询无聚合 → 不补
        assert!(fix_group_by("SELECT a, b FROM t").is_none());
    }

    #[test]
    fn collects_alias_and_cols() {
        let (amap, cols) = collect(
            "SELECT o.customer_code, o.receiver_name FROM t_sales_order o WHERE o.deleted_flag = 0",
        )
        .unwrap();
        assert_eq!(amap.get("o"), Some(&"t_sales_order".to_string()));
        assert!(cols.contains(&("o".into(), "customer_code".into())));
        assert!(cols.contains(&("o".into(), "receiver_name".into())));
    }

    #[test]
    fn no_alias_uses_table_name() {
        let (amap, _) = collect("SELECT t_sales_order.receiver FROM t_sales_order").unwrap();
        assert_eq!(amap.get("t_sales_order"), Some(&"t_sales_order".to_string()));
    }

    #[test]
    fn derived_alias_not_real_table() {
        // 派生表别名 dd 不进 aliases（不会误判其列为幻觉）
        let (amap, _) = collect("SELECT dd.amount FROM (SELECT amount FROM t_x) dd").unwrap();
        assert!(!amap.contains_key("dd"));
    }

    #[test]
    fn agg_rule_parsed() {
        assert_eq!(
            parse_agg_rule("SUM(total_amount)"),
            Some(("sum".into(), "total_amount".into(), false))
        );
        assert_eq!(
            parse_agg_rule("COUNT(DISTINCT sales_order_code)"),
            Some(("count".into(), "sales_order_code".into(), true))
        );
        // 复合表达式（客单价）保守跳过
        assert!(parse_agg_rule("SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0)").is_none());
    }

    #[test]
    fn agg_distinct_filled() {
        // 问订单数：COUNT(sales_order_code) → COUNT(DISTINCT sales_order_code)
        let rules = vec![("count".into(), "sales_order_code".into(), true)];
        let out = normalize_agg(
            "SELECT COUNT(o.sales_order_code) AS `订单数` FROM t_sales_order o",
            &rules,
        )
        .unwrap();
        assert!(norm(&out).contains("count(distincto.sales_order_code)"), "{out}");
    }

    #[test]
    fn agg_func_normalized() {
        // 问销售额：AVG(total_amount) → SUM(total_amount)
        let rules = vec![("sum".into(), "total_amount".into(), false)];
        let out = normalize_agg("SELECT AVG(o.total_amount) FROM t_sales_order o", &rules).unwrap();
        assert!(norm(&out).contains("sum(o.total_amount)"), "{out}");
    }

    #[test]
    fn agg_correct_untouched() {
        let rules = vec![("sum".into(), "total_amount".into(), false)];
        assert!(normalize_agg("SELECT SUM(o.total_amount) FROM t_sales_order o", &rules).is_none());
    }

    #[test]
    fn agg_count_star_untouched() {
        let rules = vec![("count".into(), "sales_order_code".into(), true)];
        assert!(normalize_agg("SELECT COUNT(*) FROM t_sales_order", &rules).is_none());
    }

    #[test]
    fn agg_occupied_rename_skipped() {
        // 同列已有 SUM 占用（对比问法）→ AVG 不改名，防撞出重复 SUM 列
        let rules = vec![("sum".into(), "total_amount".into(), false)];
        assert!(normalize_agg(
            "SELECT SUM(o.total_amount), AVG(o.total_amount) FROM t_sales_order o",
            &rules,
        )
        .is_none());
    }

    #[test]
    fn agg_subquery_untouched() {
        // 子查询内的聚合不碰（保守）
        let rules = vec![("sum".into(), "total_amount".into(), false)];
        assert!(normalize_agg(
            "SELECT t.c FROM (SELECT AVG(o.total_amount) AS c FROM t_sales_order o) t",
            &rules,
        )
        .is_none());
    }

    #[test]
    fn agg_other_column_untouched() {
        // 规则列不匹配 → 不动
        let rules = vec![("sum".into(), "total_amount".into(), false)];
        assert!(normalize_agg("SELECT AVG(o.refund_amount) FROM t_x o", &rules).is_none());
    }

    fn vmaps() -> (HashMap<String, String>, ValueMaps) {
        let mut aliases = HashMap::new();
        aliases.insert("h".to_string(), "t_invoice_apply_header".to_string());
        aliases.insert("o".to_string(), "t_sales_order".to_string());
        let mut maps: ValueMaps = HashMap::new();
        maps.insert(
            ("t_invoice_apply_header".into(), "invoice_status".into()),
            vec![
                ("已开票".into(), "2".into(), "eq".into()),
                ("开票失败".into(), "5".into(), "eq".into()),
            ],
        );
        maps.insert(
            ("t_sales_order".into(), "paid_way".into()),
            vec![
                ("在线支付".into(), "ZX01".into(), "eq".into()),
                ("可开票余额支付".into(), "ZZ05".into(), "like".into()),
            ],
        );
        (aliases, maps)
    }

    #[test]
    fn value_eq_swapped() {
        // 中文名直写必返 0 行 → 换码
        let (a, m) = vmaps();
        let out = link_values_with(
            "SELECT * FROM t_invoice_apply_header h WHERE h.invoice_status = '已开票'",
            &a,
            &m,
        )
        .unwrap();
        assert!(norm(&out).contains("invoice_status='2'"), "{out}");
    }

    #[test]
    fn value_mirror_eq_swapped() {
        // 镜像形态 '名' = col 也换
        let (a, m) = vmaps();
        let out = link_values_with(
            "SELECT * FROM t_invoice_apply_header h WHERE '已开票' = h.invoice_status",
            &a,
            &m,
        )
        .unwrap();
        assert!(norm(&out).contains("'2'=h.invoice_status"), "{out}");
    }

    #[test]
    fn value_like_rewritten() {
        // 组合值列：'=' 改写 LIKE '%码%'（等值必返 0 行）
        let (a, m) = vmaps();
        let out = link_values_with(
            "SELECT * FROM t_sales_order o WHERE o.paid_way = '可开票余额支付'",
            &a,
            &m,
        )
        .unwrap();
        assert!(norm(&out).contains("like'%zz05%'"), "{out}");
    }

    #[test]
    fn value_in_list_swapped() {
        let (a, m) = vmaps();
        let out = link_values_with(
            "SELECT * FROM t_invoice_apply_header h WHERE h.invoice_status IN ('已开票','开票失败')",
            &a,
            &m,
        )
        .unwrap();
        assert!(norm(&out).contains("in('2','5')"), "{out}");
    }

    #[test]
    fn value_like_kind_in_list_skipped() {
        // like 列在 IN 列表里跳过（语义须 OR LIKE，保守不动）
        let (a, m) = vmaps();
        assert!(link_values_with(
            "SELECT * FROM t_sales_order o WHERE o.paid_way IN ('可开票余额支付')",
            &a,
            &m,
        )
        .is_none());
    }

    #[test]
    fn value_bare_col_untouched() {
        // 裸列无前缀 → 不解析不动
        let (a, m) = vmaps();
        assert!(link_values_with(
            "SELECT * FROM t_invoice_apply_header h WHERE invoice_status = '已开票'",
            &a,
            &m,
        )
        .is_none());
    }

    #[test]
    fn value_already_code_untouched() {
        let (a, m) = vmaps();
        assert!(link_values_with(
            "SELECT * FROM t_invoice_apply_header h WHERE h.invoice_status = '2'",
            &a,
            &m,
        )
        .is_none());
    }

    #[test]
    fn value_unknown_name_untouched() {
        // 码表无名（非编码值或正常字符串）→ 不动
        let (a, m) = vmaps();
        assert!(link_values_with(
            "SELECT * FROM t_invoice_apply_header h WHERE h.invoice_status = '进行中'",
            &a,
            &m,
        )
        .is_none());
    }
}
