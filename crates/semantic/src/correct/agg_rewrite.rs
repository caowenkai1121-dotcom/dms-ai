//! 聚合归一的 **AST 改写半**（变更原因＝sqlparser 与 SELECT 形态；判据与命中在 `agg.rs`）。
//!
//! 逐行搬运自 `server/src/corrector.rs`（T8 第二批），只搬不改。

use std::collections::HashSet;

use sqlparser::ast::Expr;
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use super::agg::{last_ident, AggRule};

/// AggCorrector（移植 SuperSonic correctAggFunction）：命中指标的聚合列归一到注册表默认聚合。
/// 问「订单数」LLM 写 COUNT(sales_order_code) → COUNT(DISTINCT sales_order_code)；
/// 问「订单额」写 AVG(total_amount) → SUM(total_amount)。口径以注册表为单一事实源；
/// 默认销售额使用 DWS `amount`，不会与订单额共用列规则。
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
    // 存规则引用而不是克隆出的 String 对（占用集只读）
    let occupied: HashSet<(&str, &str)> = rules
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
        .map(|r| (r.0.as_str(), r.1.as_str()))
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
                        last_ident(a).is_some_and(|c| c == col)
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
    occupied: &HashSet<(&str, &str)>,
    changed: &mut bool,
) {
    use sqlparser::ast::{DuplicateTreatment, FunctionArguments};
    match e {
        Expr::Function(f) => {
            let FunctionArguments::List(l) = &mut f.args else { return };
            if l.args.len() != 1 {
                return;
            }
            let node_name = f.name.0.last().map(|p| p.value.to_lowercase()).unwrap_or_default();
            let col = match &l.args[0] {
                sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(a)) => {
                    match last_ident(a) {
                        Some(c) => c,
                        None => return,
                    }
                }
                // COUNT(*) → 命中「计数类去重指标」时按口径改写为 COUNT(DISTINCT 主键)。
                // 头表一单一行时两者数值相同，但一旦 JOIN 明细就会按行数虚增——口径以注册表为准。
                // 仅在恰有一条 count+DISTINCT 规则时改（多指标歧义保守跳过）。
                sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Wildcard)
                    if node_name == "count" =>
                {
                    let mut cnt = rules.iter().filter(|r| r.0 == "count" && r.2);
                    if let (Some(rule), None) = (cnt.next(), cnt.next()) {
                        l.args[0] = sqlparser::ast::FunctionArg::Unnamed(
                            sqlparser::ast::FunctionArgExpr::Expr(Expr::Identifier(
                                sqlparser::ast::Ident::new(rule.1.clone()),
                            )),
                        );
                        l.duplicate_treatment = Some(DuplicateTreatment::Distinct);
                        *changed = true;
                    }
                    return;
                }
                _ => return,
            };
            let Some(rule) = rules.iter().find(|r| r.1 == col) else { return };
            let node_distinct = matches!(l.duplicate_treatment, Some(DuplicateTreatment::Distinct));
            if node_name == rule.0 {
                // 函数已对，补 DISTINCT（COUNT(code)→COUNT(DISTINCT code)）
                if rule.2 && !node_distinct {
                    l.duplicate_treatment = Some(DuplicateTreatment::Distinct);
                    *changed = true;
                }
            // 🔴 `max`/`min` **不在**可归一之列：它们不是「选错了默认聚合」，是**另一个问题**。
            // 上面入口那道 `OPT_OUT` 挡的是「问句写了最高/平均」；这一道挡的是
            // 「问句没写、但 LLM 自己写了 `MAX`」—— 那种情况归一同样把语义换掉了。
            // `avg` 保留：它确实是 LLM 对「销售额」误写默认聚合的高频形态，
            // 而「平均」那一族已被入口的 `OPT_OUT` 拦在外面。
            } else if matches!(node_name.as_str(), "sum" | "count" | "avg")
                && !occupied.contains(&(rule.0.as_str(), rule.1.as_str()))
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
    use super::super::agg::parse_agg_rules;

    /// 断言用归一（与旧址 corrector.rs 的同名 helper 逐字相同）
    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
    }

    /// 复合指标的归一真的打到嵌套里：除法里的 `COUNT(code)` 补 DISTINCT，
    /// 而已占用的 `SUM(total_amount)` 不被改名（occupied 只管函数归一那一支）。
    #[test]
    fn normalize_agg_reaches_inside_composite_expressions() {
        let rules = parse_agg_rules("SUM(total_amount) / NULLIF(COUNT(DISTINCT sales_order_code), 0)");
        let out = normalize_agg(
            "SELECT SUM(total_amount) / COUNT(sales_order_code) AS `客单价` FROM t_sales_order",
            &rules,
        )
        .unwrap();
        assert!(norm(&out).contains("count(distinctsales_order_code)"), "{out}");
        assert!(norm(&out).contains("sum(total_amount)"), "{out}");
    }

    #[test]
    fn agg_func_normalized() {
        // 问订单额：AVG(total_amount) → SUM(total_amount)
        let rules = vec![("sum".into(), "total_amount".into(), false)];
        let out = normalize_agg("SELECT AVG(o.total_amount) FROM t_sales_order o", &rules).unwrap();
        assert!(norm(&out).contains("sum(o.total_amount)"), "{out}");
    }

    #[test]
    fn agg_correct_untouched() {
        let rules = vec![("sum".into(), "total_amount".into(), false)];
        assert!(normalize_agg("SELECT SUM(o.total_amount) FROM t_sales_order o", &rules).is_none());
    }

    /// 🔴 **MAX/MIN 不许被归一成默认聚合**（二·AU3）。
    ///
    /// 修前：问「本月单笔最高订单金额」，LLM 老实写 `MAX(o.total_amount) AS 最高订单金额`，
    /// 校正器按列名命中销售额规则（`sum`）把它换成 `SUM` ——
    /// 用户看到一个标着「最高销售额」的**全月合计**，数量级差几千倍。
    /// 命中是必然的：规则来源是问句含指标名，而「最高销售额」含「销售额」。
    ///
    /// 判据两侧都钉：MAX/MIN 必须**原样不动**，而 AVG 仍要被归一
    /// （它是「LLM 对销售额误写默认聚合」的高频形态，删掉那条会丢真收益）。
    #[test]
    fn extremum_aggregates_are_never_normalized() {
        let rules = vec![("sum".into(), "total_amount".into(), false)];
        for f in ["MAX", "MIN"] {
            let sql = format!("SELECT {f}(o.total_amount) AS `x` FROM t_sales_order o");
            assert!(
                normalize_agg(&sql, &rules).is_none(),
                "{f} 被归一成了 SUM —— 「最高订单金额」会变成全月合计"
            );
        }
        // 反面（防恒真）：AVG 那一族仍归一，否则把 `normalize_agg` 写成恒 None 上面也全绿
        let out = normalize_agg("SELECT AVG(o.total_amount) FROM t_sales_order o", &rules)
            .expect("AVG 仍应被归一");
        assert!(norm(&out).contains("sum(o.total_amount)"), "{out}");
    }

    #[test]
    fn agg_count_star_follows_metric_caliber() {
        // 语义变更（回归 E13）：COUNT(*) 命中唯一「计数+去重」指标时按口径归一为 COUNT(DISTINCT 主键)。
        // 原先一律不碰——头表一单一行时数值虽同，但 JOIN 明细后 COUNT(*) 按行数虚增。
        let rules = vec![("count".into(), "sales_order_code".into(), true)];
        let out = normalize_agg("SELECT COUNT(*) FROM t_sales_order", &rules).unwrap();
        assert!(out.to_uppercase().replace(' ', "").contains("COUNT(DISTINCTSALES_ORDER_CODE)"), "{out}");
        // 非去重计数规则不触发（COUNT(*) 与 COUNT(col) 在 NULL 上语义不同，不擅改）
        let plain = vec![("count".into(), "sales_order_code".into(), false)];
        assert!(normalize_agg("SELECT COUNT(*) FROM t_sales_order", &plain).is_none());
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

    #[test]
    fn count_star_normalized_to_distinct() {
        // 回归 E13：问「售后单数」LLM 写 COUNT(*)，口径要求 COUNT(DISTINCT after_sales_code)。
        // 头表一单一行时数值相同，但 JOIN 明细后 COUNT(*) 会按行数虚增 → 按注册表口径归一。
        let rules = vec![("count".to_string(), "after_sales_code".to_string(), true)];
        let out = normalize_agg("SELECT COUNT(*) FROM t_after_sales_order_header", &rules).unwrap();
        assert!(out.to_uppercase().replace(' ', "").contains("COUNT(DISTINCTAFTER_SALES_CODE)"), "{out}");
    }

    #[test]
    fn count_star_untouched_when_ambiguous() {
        // 两条计数去重规则 → 不知该用哪个主键，保守不改
        let rules = vec![
            ("count".to_string(), "after_sales_code".to_string(), true),
            ("count".to_string(), "sales_order_code".to_string(), true),
        ];
        assert!(normalize_agg("SELECT COUNT(*) FROM t", &rules).is_none());
        // 无计数去重规则（只有 SUM 类指标）→ 不碰
        let sum_only = vec![("sum".to_string(), "total_amount".to_string(), false)];
        assert!(normalize_agg("SELECT COUNT(*) FROM t", &sum_only).is_none());
    }
}
