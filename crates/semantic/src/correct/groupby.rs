//! 确定性校正链的**补缺**一族：漏 GROUP BY、漏投影维度列、重复投影、时间窗只有上界。
//!
//! 全部是纯 sqlparser AST 操作：零 IO、零注册表、零 `AskCtx`，判据只看 SQL 自身形状。
//! 逐行搬运自 `server/src/corrector.rs:642-911`（T8 第一批）——**只搬不改**：
//! 一个字节的行为改动都会让 `evaluation.py` 的逐题结果集对拍失去意义。
//!
//! 🔴 `expr_has_agg` 的六函数名单（含 `group_concat`）与 `correct::agg` 的
//! `collect_agg_rules` 五函数名单**刻意不同**（原注释在下方保留）：前者判「含不含聚合」，
//! 后者判「能不能归一成单列默认聚合」。不许趁重构合并成一份常量。

use std::collections::HashSet;

use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

/// GroupByCorrector（移植 SuperSonic）：select 同时含聚合列和裸维度列却漏 GROUP BY 时，
/// 用裸维度列补上 GROUP BY（MySQL only_full_group_by 下漏 group by 直接报错）。纯 AST，确定性。
/// 保守门控：单表非复杂 SQL、已有 group by 不动、无聚合或无裸列不动。
pub fn fix_group_by(sql: &str) -> Option<String> {
    use sqlparser::ast::{GroupByExpr, Query, Select, SelectItem, SetExpr, Statement};
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
        let e = match item {
            SelectItem::UnnamedExpr(e) => e,
            SelectItem::ExprWithAlias { expr, .. } => expr,
            _ => return None, // 有 * 通配等，不处理
        };
        if expr_has_agg(e) {
            has_agg = true;
        } else {
            dims.push(e.clone());
        }
    }
    // 需同时有聚合和裸维度才补
    if !has_agg || dims.is_empty() {
        return None;
    }
    sel.group_by = GroupByExpr::Expressions(dims, vec![]);
    Some(stmts[0].to_string())
}

// ─────────────────────────── 【A12】三个补缺校正器（纯 AST）───────────────────────────

/// 表达式相等判据：sqlparser 的 Display 输出本就是归一形态，再去反引号、小写化。
/// 与 `kernel::caliber` 的「比列不比字节」同一条纪律。
pub(crate) fn expr_key(e: &sqlparser::ast::Expr) -> String {
    e.to_string().replace('`', "").to_lowercase()
}

/// 投影项是可枚举的表达式（`*` 通配/qualified wildcard 不可枚举）
pub(crate) fn is_expr_item(i: &sqlparser::ast::SelectItem) -> bool {
    matches!(
        i,
        sqlparser::ast::SelectItem::UnnamedExpr(_) | sqlparser::ast::SelectItem::ExprWithAlias { .. }
    )
}

/// 顶层单 Select 的共用门控（三个校正器与 `fix_group_by` 同一条）：子查询/UNION/多语句
/// 一律跳过（防误伤方向），返回可变的 Select 引用给调用方继续判。
pub fn top_select<'s>(
    stmts: &'s mut [sqlparser::ast::Statement],
) -> Option<&'s mut sqlparser::ast::Select> {
    use sqlparser::ast::{Query, Select, SetExpr, Statement};
    if stmts.len() != 1 {
        return None;
    }
    let Statement::Query(q) = &mut stmts[0] else {
        return None;
    };
    let Query { body, .. } = q.as_mut();
    let SetExpr::Select(sel) = body.as_mut() else {
        return None;
    };
    let sel: &mut Select = sel.as_mut();
    // 有 * 通配不处理（投影不可枚举）
    if sel.projection.iter().any(|i| !is_expr_item(i)) {
        return None;
    }
    Some(sel)
}

/// SelectCorrector（移植 SuperSonic 同名）：**GROUP BY 有的列、SELECT 没有 ⇒ 补进投影最前**。
/// 不补的代价不是报错是**图表没有分类轴**：「销售额按省份」只出一列合计，
/// present 按输出列建视图，缺了维度列就是一张单值 KPI（那族混轴问题在 `present.rs` 记过账）。
///
/// 保守门控（全偏漏判）：顶层单 Select、无 *、GROUP BY 为 Expressions、投影含聚合
/// （纯维度查询是 DISTINCT 风格不是漏列）、**缺失项必须全是带前缀的列引用** ——
/// `GROUP BY 月份`（别名）与 `GROUP BY 1`（位置）补进去就是 1054/1055，一律不动。
pub fn fix_select_fields(sql: &str) -> Option<String> {
    use sqlparser::ast::{Expr, GroupByExpr, SelectItem};
    let mut stmts = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
    let sel = top_select(&mut stmts)?;
    let GroupByExpr::Expressions(gb, _) = &sel.group_by else { return None };
    if gb.is_empty() {
        return None;
    }
    let has_agg = sel.projection.iter().any(|i| match i {
        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => expr_has_agg(e),
        _ => false,
    });
    if !has_agg {
        return None;
    }
    let have: HashSet<String> = sel
        .projection
        .iter()
        .map(|i| match i {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => expr_key(e),
            _ => String::new(),
        })
        .collect();
    let missing: Vec<Expr> = gb.iter().filter(|e| !have.contains(&expr_key(e))).cloned().collect();
    if missing.is_empty() {
        return None;
    }
    // 只补带前缀的列引用（`o.province`）；别名/位置/函数形式的 group by 一项都不碰
    if !missing.iter().all(|e| matches!(e, Expr::CompoundIdentifier(_))) {
        return None;
    }
    let mut proj: Vec<SelectItem> = missing.into_iter().map(SelectItem::UnnamedExpr).collect();
    proj.extend(sel.projection.iter().cloned());
    sel.projection = proj;
    Some(stmts[0].to_string())
}

/// removeSameFieldFromSelect（移植 SuperSonic 同名）：投影里**逐字重复**的项只留第一份。
/// 重复列的代价是前端表格出两列一模一样的列、AI 解读按列名定位打架。
///
/// 🔴 只去**整项逐字相同**（表达式与别名都一样）的重复：`SUM(x) AS a, SUM(x) AS b`
/// 两个都留 —— `ORDER BY b` 还指着它，删了就是把能跑的 SQL 改挂（漏判方向）。
pub fn dedup_select_fields(sql: &str) -> Option<String> {
    use sqlparser::ast::SelectItem;
    let mut stmts = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
    let sel = top_select(&mut stmts)?;
    let item_key = |i: &SelectItem| match i {
        SelectItem::UnnamedExpr(e) => expr_key(e),
        SelectItem::ExprWithAlias { expr, alias } => {
            format!("{} AS {}", expr_key(expr), alias.value.trim_matches('`').to_lowercase())
        }
        _ => String::new(),
    };
    let mut seen = HashSet::new();
    let mut out = vec![];
    let mut changed = false;
    for item in &sel.projection {
        if seen.insert(item_key(item)) {
            out.push(item.clone());
        } else {
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    sel.projection = out;
    Some(stmts[0].to_string())
}

/// TimeCorrector 的「只有上界补下界」半边（**只做防全表扫** —— 缺时间自动补默认窗
/// 是 X3 裁决明令禁止的，别顺手一起做）。WHERE 里时间列只有 `<`/`<=`、
/// 没有 `>=`/`>`/`=`/`BETWEEN` ⇒ 追加 `AND col >= '1970-01-01'`（语义中性：
/// DMS 数据都在 2022 年之后；索引能少走下界扫描）。
/// 时间列词法谓词与 caliber `time_ish_conds` 同一条（含 time/date/_at）。顶层单 Select。
pub fn fix_time_lower_bound(sql: &str) -> Option<String> {
    use sqlparser::ast::{BinaryOperator as B, Expr, Value};
    let mut stmts = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
    let sel = top_select(&mut stmts)?;
    // 已知假阳：子串匹配，`menddate` 这类含 date 的列名也会被当时间列。
    // 谓词与 caliber `time_ish_conds` 同一条，单边收紧会让两处口径漂移，故保持子串并在此记账。
    let ish = |c: &str| c.contains("time") || c.contains("date") || c.contains("_at");
    // (列键(末段小写), 列引用原文(留限定符，补下界时带回), 有上界, 有下界/等值)。
    // 只沿 WHERE 的 AND 链收集：OR 分支里的时间约束是条件性的，不能当顶层约束
    // （`A OR B` 下 B 分支的上界若算数，补出的下界会把 A 分支也收窄）。
    let mut cols: Vec<(String, Expr, bool, bool)> = vec![];
    fn walk<'e>(e: &'e Expr, ish: &impl Fn(&str) -> bool, cols: &mut Vec<(String, Expr, bool, bool)>) {
        if let Expr::BinaryOp { left, op, right } = e {
            let col = match left.as_ref() {
                Expr::Identifier(i) => Some(i.value.trim_matches('`').to_lowercase()),
                Expr::CompoundIdentifier(p) => {
                    p.last().map(|i| i.value.trim_matches('`').to_lowercase())
                }
                _ => None,
            };
            match (col, op) {
                (Some(c), B::Lt | B::LtEq) if ish(&c) => {
                    cols.push((c, left.as_ref().clone(), true, false))
                }
                (Some(c), B::Gt | B::GtEq | B::Eq) if ish(&c) => {
                    cols.push((c, left.as_ref().clone(), false, true))
                }
                _ => {}
            }
            if matches!(op, B::And) {
                walk(left, ish, cols);
                walk(right, ish, cols);
            }
        } else if let Expr::Between { expr, .. } | Expr::InList { expr, .. } = e {
            // 与比较分支同形：裸列与限定列（取末段做键）都认
            let c = match expr.as_ref() {
                Expr::Identifier(i) => Some(i.value.trim_matches('`').to_lowercase()),
                Expr::CompoundIdentifier(p) => {
                    p.last().map(|i| i.value.trim_matches('`').to_lowercase())
                }
                _ => None,
            };
            if let Some(c) = c {
                if ish(&c) {
                    cols.push((c, expr.as_ref().clone(), false, true));
                }
            }
        }
    }
    let selection = sel.selection.as_ref()?;
    walk(selection, &ish, &mut cols);
    // 有上界且无下界的时间列（去重、稳定序 —— 日志文案逐次一致）
    let mut targets: Vec<(String, Expr)> = cols
        .iter()
        .filter(|(c, _, up, _low)| *up && !cols.iter().any(|(c2, _, _, low2)| c2 == c && *low2))
        .map(|(c, e, _, _)| (c.clone(), e.clone()))
        .collect();
    targets.sort_by(|a, b| a.0.cmp(&b.0));
    targets.dedup_by(|a, b| a.0 == b.0);
    if targets.is_empty() {
        return None;
    }
    // 补回时保留原限定符（`o.order_time >= …`）：多表 JOIN 下裸列是 MySQL 1052 歧义
    let extra: Vec<Expr> = targets
        .iter()
        .map(|(_, e)| Expr::BinaryOp {
            left: Box::new(e.clone()),
            op: B::GtEq,
            right: Box::new(Expr::Value(Value::SingleQuotedString("1970-01-01".into()))),
        })
        .collect();
    let mut cond = selection.clone();
    for e in extra {
        // 左操作数不包 `Nested` 是安全的：`walk` 只沿 AND 链收集（见上），顶层若是 `Or`
        // 则 cols 必空、走不到这里 —— AND 链上续接 AND 结合律同形，不需要括号保护。
        cond = Expr::BinaryOp { left: Box::new(cond), op: B::And, right: Box::new(e) };
    }
    sel.selection = Some(cond);
    Some(stmts[0].to_string())
}

/// 表达式是否含聚合函数
pub fn expr_has_agg(e: &sqlparser::ast::Expr) -> bool {
    use sqlparser::ast::Expr;
    // 判定用名单 ≠ 归一用名单：这里判「含不含聚合」要收 group_concat（它也是聚合）；
    // `collect_agg_rules` 只收五函数，是因为 group_concat 映射不了「单列默认聚合」的归一形态。
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言用归一（与旧址 corrector.rs 的同名 helper 逐字相同 —— 纯搬运不许改判据）
    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
    }

    #[test]
    fn adds_missing_group_by() {
        // 省份 + SUM(金额) 漏 GROUP BY → 补
        let out = fix_group_by("SELECT province, SUM(total_amount) FROM t_sales_order").unwrap();
        assert!(norm(&out).contains("groupbyprovince"), "{out}");
    }

    /// SelectCorrector：GROUP BY 有、SELECT 没有 → 补进投影最前（维度在前是本仓 gold 的形制）
    #[test]
    fn select_fields_adds_missing_group_cols_first() {
        let out = fix_select_fields(
            "SELECT SUM(o.total_amount) AS `订单额` FROM t_sales_order o WHERE o.deleted_flag = 0 GROUP BY o.province",
        )
        .unwrap();
        assert!(norm(&out).starts_with("selecto.province"), "{out}");
        // 已在投影里的不重复补
        assert!(fix_select_fields(
            "SELECT o.province, SUM(o.total_amount) FROM t_sales_order o GROUP BY o.province"
        )
        .is_none());
    }

    /// SelectCorrector 的漏判侧（全是防误伤）：别名 group by / 位置 group by / 纯维度查询 / 无聚合
    #[test]
    fn select_fields_skips_alias_positional_and_dim_only() {
        // GROUP BY 别名（`月份`）——补进去就是 1054，不动
        assert!(fix_select_fields(
            "SELECT DATE_FORMAT(o.order_time, '%Y-%m') AS `月份`, SUM(o.total_amount) FROM t_sales_order o GROUP BY `月份`"
        )
        .is_none());
        // GROUP BY 位置序号
        assert!(fix_select_fields("SELECT o.province, SUM(o.x) FROM t_sales_order o GROUP BY 1").is_none());
        // 纯维度查询（DISTINCT 风格，不是漏列）
        assert!(fix_select_fields("SELECT o.province FROM t_sales_order o GROUP BY o.province").is_none());
        // 无 GROUP BY
        assert!(fix_select_fields("SELECT SUM(o.x) FROM t_sales_order o").is_none());
    }

    /// removeSameFieldFromSelect：逐字重复的只留第一份；不同别名的一个不动（ORDER BY 可能指着它）
    #[test]
    fn dedup_select_removes_exact_duplicates_only() {
        let out = dedup_select_fields(
            "SELECT o.province, o.province, SUM(o.total_amount) AS `订单额` FROM t_sales_order o",
        )
        .unwrap();
        assert_eq!(norm(&out).matches("o.province").count(), 1, "{out}");
        // 同表达式不同别名：都留（`ORDER BY b` 还指着它）
        assert!(dedup_select_fields(
            "SELECT SUM(o.x) AS a, SUM(o.x) AS b FROM t_sales_order o ORDER BY b"
        )
        .is_none());
        // 无重复：None
        assert!(dedup_select_fields("SELECT o.a, o.b FROM t_sales_order o").is_none());
    }

    /// 只有上界补下界：补 `'1970-01-01'`（语义中性）；
    /// 已有下界 / 等值 / BETWEEN / 无 WHERE / 非时间列 一律不动。
    #[test]
    fn time_lower_bound_only_when_upper_alone() {
        let out = fix_time_lower_bound(
            "SELECT SUM(o.total_amount) FROM t_sales_order o WHERE o.order_time < '2026-08-01'",
        )
        .unwrap();
        assert!(norm(&out).contains("order_time>='1970-01-01'"), "{out}");
        for skip in [
            "SELECT SUM(o.x) FROM t_sales_order o WHERE o.order_time >= '2026-07-01' AND o.order_time < '2026-08-01'",
            "SELECT SUM(o.x) FROM t_sales_order o WHERE DATE(o.order_time) = '2026-07-31'",
            "SELECT SUM(o.x) FROM t_sales_order o WHERE o.order_time BETWEEN '2026-07-01' AND '2026-07-31'",
            "SELECT SUM(o.x) FROM t_sales_order o",
            "SELECT SUM(o.x) FROM t_sales_order o WHERE o.amount < 100",
        ] {
            assert!(fix_time_lower_bound(skip).is_none(), "不该动：{skip}");
        }
    }

    /// OR 分支里的时间上界不算顶层约束：不许补下界（补了会把 OR 另一支也收窄）
    #[test]
    fn time_lower_bound_ignores_bounds_inside_or_branches() {
        assert!(fix_time_lower_bound(
            "SELECT SUM(o.x) FROM t_sales_order o WHERE o.amount > 100 OR o.order_time < '2026-08-01'"
        )
        .is_none());
    }

    /// 多表 JOIN 下补下界必须保留原限定符：裸列是 MySQL 1052 歧义
    #[test]
    fn time_lower_bound_keeps_qualifier_in_joins() {
        let out = fix_time_lower_bound(
            "SELECT SUM(d.box_quantity) FROM t_sales_order_detail d \
             JOIN t_sales_order o ON o.sales_order_code = d.sales_order_code \
             WHERE o.order_time < '2026-08-01'",
        )
        .unwrap();
        assert!(norm(&out).contains("o.order_time>='1970-01-01'"), "{out}");
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

}
