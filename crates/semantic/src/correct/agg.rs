//! AggCorrector：把 LLM 写的聚合函数归一成**指标注册表声明的默认聚合**
//! （`COUNT(code)` 漏 DISTINCT 这类）。规则来源是 `meta.metric.agg_expr`。
//!
//! 逐行搬运自 `server/src/corrector.rs`（T8 第二批）——只搬不改：一个字节的行为改动
//! 都会让 `evaluation.py` 的逐题结果集对拍失去意义。
//!
//! 🔴 入口的 `OPT_OUT` 与 `correct::caliber` 里那份是**两份同构词表**（各自语义不同，
//! 不能合并），加词时两边都要看一眼 —— 原注释在下方保留。

use std::collections::{HashMap, HashSet};

use sqlparser::ast::Expr;
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use sqlx::PgPool;

/// AggCorrector 入口：问句命中指标 → agg_expr 解析规则 → normalize_agg 归一。
pub async fn correct_agg(
    pg: &PgPool,
    ds: &str,
    question: &str,
    sql: &str,
) -> anyhow::Result<Option<String>> {
    // 🔴 **反向问法：问的是极值/均值就整体不归一**（形态照 `correct_caliber` 的 `OPT_OUT`）。
    //
    // 本校正器的立意是「模型没选对该指标的**默认**聚合」（`COUNT(code)` 漏 DISTINCT 这类）。
    // 但「本月单笔**最高**订单金额」问的是另一件事：LLM 老实写 `MAX(o.total_amount)`，
    // 而归一会把它换成销售额的默认聚合 `SUM` —— 用户看到一个标着「最高销售额」的
    // **全月合计**，数量级差几千倍。命中条件只看列名（下面 `rules.iter().find(|r| r.1 == col)`），
    // 而规则来源是问句含指标名/别名 —— 「最高销售额」含「销售额」，所以必然命中。
    //
    // 只删 `max|min` 那两个白名单项不够：`AVG` 同形（「本月**平均**销售额」被改成 SUM），
    // 而且反过来「问句问最高、LLM 写了 SUM」时校正器出手也救不了（SQL 本身就错了）。
    // 所以在**入口**整体退出：宁可少改一条，不许把一条正确的 SQL 改错（裁决 二·G 同族）。
    // ⚠️ 本名单与 `correct_caliber` 里的 `OPT_OUT` 是两份同构词表（各自语义不同，不能合并），
    // 今后加词时两边都要看一眼。
    const OPT_OUT: &[&str] =
        &["最高", "最低", "最大", "最小", "最多", "最少", "平均", "均值", "中位"];
    if OPT_OUT.iter().any(|w| question.contains(w)) {
        return Ok(None);
    }
    // 【K6-D】ds 限定：口径以**本源**注册表为单一事实源，别拿 DMS 的默认聚合归一别的库
    let rows: Vec<(String, Vec<String>, String)> = sqlx::query_as(&format!(
        // `ORDER BY name`：同 `recall::metric` 那条（缺它则顺序＝PG 物理行序，
        // 而种子每次启动都 UPDATE 一遍 meta.metric → 顺序会变、且没有测试会红）
        "SELECT name, aliases, agg_expr FROM meta.metric WHERE status = 'active'{ds_pred} ORDER BY name",
        ds_pred = crate::registry::ds_pred(1)
    ))
    .bind(ds)
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
        // 【A21】复合指标也要进得来：`parse_agg_rules` 抽**全部**聚合
        // （客单价那类此前整体跳过；单形态指标与旧路径逐字等价）
        for (func, col, distinct) in parse_agg_rules(agg) {
            match by_col.get(&col) {
                Some(prev) if prev.0 != func || prev.1 != distinct => {
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
    Ok(super::agg_rewrite::normalize_agg(sql, &rules))
}

/// 聚合归一规则：(目标函数 lower, 聚合列 lower, 是否 DISTINCT)。从指标注册表 agg_expr 解析而来。
pub type AggRule = (String, String, bool);

/// 解析指标 agg_expr → AggRule。只接受单聚合形态（SUM(x)/COUNT(DISTINCT x)）；
/// 客单价 SUM(x)/NULLIF(COUNT...,0) 这类复合表达式保守跳过（无法映射单一默认聚合）。
#[cfg(test)]
pub fn parse_agg_rule(agg_expr: &str) -> Option<AggRule> {
    // 恰好一条才给（复合表达式在本入口维持「保守跳过」的原语义 —— 多规则走
    // `parse_agg_rules`，见 A21；两条入口的对外行为因此都不变）
    let rules = parse_agg_rules(agg_expr);
    match &rules[..] {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// 【A21】复合表达式版：把 `agg_expr` 里的**全部**聚合抽成规则。
/// 客单价 `SUM(total_amount) / NULLIF(COUNT(DISTINCT sales_order_code), 0)` 这类
/// 复合指标从此进得了 `normalize_agg`（此前 `parse_agg_rule` 只收单聚合形态，
/// AggCorrector 对复合指标整体是死的）。
///
/// 保守面与 `normalize_agg` 同一条：**不进子查询** —— 退款占比等复合子查询
/// 复合子查询口径整体跳过（它们的列无法映射单一聚合，抽规则就是误抽）；
/// `COUNT(*)`、非单参数、非标识符入参一律不产规则（漏判方向）。
pub fn parse_agg_rules(agg_expr: &str) -> Vec<AggRule> {
    use sqlparser::ast::{SelectItem, SetExpr, Statement};
    let mut out = vec![];
    let Ok(stmts) = Parser::parse_sql(&MySqlDialect {}, &format!("SELECT {agg_expr}")) else {
        return out;
    };
    let Some(Statement::Query(q)) = stmts.into_iter().next() else { return out };
    let SetExpr::Select(sel) = *q.body else { return out };
    for item in sel.projection {
        let e = match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
            _ => continue,
        };
        collect_agg_rules(&e, &mut out);
    }
    out
}

/// 递归抽聚合（只下钻函数参数与二元/包装层，**不进子查询**）
fn collect_agg_rules(e: &Expr, out: &mut Vec<AggRule>) {
    use sqlparser::ast::{DuplicateTreatment, FunctionArg, FunctionArgExpr, FunctionArguments};
    match e {
        Expr::Function(f) => {
            let name = f.name.0.last().map(|p| p.value.to_lowercase()).unwrap_or_default();
            if let FunctionArguments::List(l) = &f.args {
                if matches!(name.as_str(), "sum" | "count" | "avg" | "max" | "min")
                    && l.args.len() == 1
                {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)) = &l.args[0] {
                        if let Some(col) = last_ident(arg) {
                            out.push((
                                name,
                                col,
                                matches!(l.duplicate_treatment, Some(DuplicateTreatment::Distinct)),
                            ));
                        }
                    }
                }
                // 继续下钻参数（`NULLIF(COUNT(DISTINCT code), 0)`：聚合在参数里）
                for a in &l.args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)) = a {
                        collect_agg_rules(arg, out);
                    }
                }
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_agg_rules(left, out);
            collect_agg_rules(right, out);
        }
        Expr::Nested(x) | Expr::UnaryOp { expr: x, .. } | Expr::Cast { expr: x, .. } => {
            collect_agg_rules(x, out)
        }
        _ => {} // 子查询 / 字面量 / 标识符 / CASE：停钻防误伤
    }
}

/// 取标识符末段（t.col→col）。非标识符表达式返回 None。
pub(crate) fn last_ident(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(p) => Some(p.value.to_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.to_lowercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::agg_rewrite::normalize_agg;

    /// 断言用归一（与旧址 corrector.rs 的同名 helper 逐字相同）
    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
    }

    /// 【A21】复合表达式抽全部聚合：客单价两条规则都抽到；
    /// 复合子查询口径整体跳过（不抽就是误抽）；
    /// 单形态入口 `parse_agg_rule` 对外语义一字不变（恰好一条才给）。
    #[test]
    fn parse_agg_rules_extracts_composite_but_skips_subqueries() {
        let rules = parse_agg_rules("SUM(total_amount) / NULLIF(COUNT(DISTINCT sales_order_code), 0)");
        assert_eq!(
            rules,
            [("sum".to_string(), "total_amount".to_string(), false),
             ("count".to_string(), "sales_order_code".to_string(), true)],
            "{rules:?}"
        );
        // 复合子查询口径：一条规则都不许抽（列无法映射单一聚合）
        assert!(parse_agg_rules("(SELECT SUM(x) FROM a) + (SELECT SUM(y) FROM b)").is_empty());
        // 单形态入口：恰好一条才给（复合 → None，与旧行为逐字一致）
        assert_eq!(parse_agg_rule("SUM(total_amount)"),
                   Some(("sum".into(), "total_amount".into(), false)));
        assert!(parse_agg_rule("SUM(x) / COUNT(y)").is_none());
        // COUNT(*) 不产规则
        assert!(parse_agg_rules("SUM(x) / COUNT(*)").iter().all(|r| r.0 != "count" || r.1 != "*"));
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
}
