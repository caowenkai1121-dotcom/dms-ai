//! 权限条件 SQL AST 注入器。
//! 语义对齐 Java CustomerDataScopeStrategy：每张已绑定表生成
//! `(alias.customer_col IN (..) or alias.owner_col IN (..))`，各段集合为空则丢弃，
//! 整体括号包住后 AND 进该表所在 (子)查询的 WHERE。
//! 段顺序同 Java getCondition：employeeIds → employeeCodes → customerCodes。

use sqlparser::ast::{
    Expr, Query, Select, SetExpr, Statement, TableFactor,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use crate::scope::ScopeSets;

/// 表绑定：该表用哪些列吃权限条件（对应 Java @DataScope joinSql 模板，逐条探库核实）
pub struct Binding {
    pub customer_col: Option<&'static str>,
    pub owner_col: Option<&'static str>,
    pub owner_kind: OwnerKind,
}

#[derive(PartialEq, Clone, Copy)]
pub enum OwnerKind {
    /// 数字 employee_id（#employeeIds）
    Ids,
    /// 登录名字符串（#employeeCodes）
    Codes,
}

/// 绑定注册表（M1 硬编码，M2 迁 PG 元数据）。列存在性已连库核实。
pub fn binding_of(table: &str) -> Option<Binding> {
    let b = |c: Option<&'static str>, o: Option<&'static str>, k: OwnerKind| Binding {
        customer_col: c,
        owner_col: o,
        owner_kind: k,
    };
    match table {
        // @DataScope joinSql 权威模板 → 列绑定
        "t_sales_order" => Some(b(Some("customer_code"), Some("owner_manager"), OwnerKind::Ids)),
        "t_sales_order_his" => Some(b(Some("customer_code"), Some("owner_manager"), OwnerKind::Ids)),
        "t_customer" => Some(b(Some("customer_code"), Some("area_manager_id"), OwnerKind::Ids)),
        "t_after_sales_order_header" => {
            Some(b(Some("customer_code"), Some("owner_manager"), OwnerKind::Ids))
        }
        "t_activity_main" => Some(b(Some("customer_code"), Some("created_id"), OwnerKind::Ids)),
        "t_invoice_apply_header" => Some(b(Some("customer_code"), Some("manager"), OwnerKind::Ids)),
        "t_account_bill_header" => {
            Some(b(Some("customer_code"), Some("created_by"), OwnerKind::Codes))
        }
        // 无 owner 维度的表（Java 模板只有 customer 段）
        "t_customer_balance" | "t_customer_device_ledger" | "t_device_disposal_order"
        | "t_shop_inspection_records" => Some(b(Some("customer_code"), None, OwnerKind::Ids)),
        _ => None,
    }
}

/// 把权限条件注入 SQL。集合全空（超管/ALL）原样返回。
pub fn inject(sql: &str, sets: &ScopeSets) -> anyhow::Result<String> {
    if sets.is_unrestricted() {
        return Ok(sql.to_string());
    }
    let dialect = MySqlDialect {};
    let mut stmts = Parser::parse_sql(&dialect, sql)?;
    for stmt in &mut stmts {
        match stmt {
            Statement::Query(q) => inject_query(q, sets),
            _ => anyhow::bail!("只允许 SELECT 语句"),
        }
    }
    Ok(stmts
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; "))
}

fn inject_query(q: &mut Query, sets: &ScopeSets) {
    // CTE 也要注入
    if let Some(with) = &mut q.with {
        for cte in &mut with.cte_tables {
            inject_query(&mut cte.query, sets);
        }
    }
    inject_set_expr(&mut q.body, sets);
}

fn inject_set_expr(body: &mut SetExpr, sets: &ScopeSets) {
    match body {
        SetExpr::Select(sel) => inject_select(sel, sets),
        SetExpr::Query(q) => inject_query(q, sets),
        SetExpr::SetOperation { left, right, .. } => {
            inject_set_expr(left, sets);
            inject_set_expr(right, sets);
        }
        _ => {}
    }
}

fn inject_select(sel: &mut Select, sets: &ScopeSets) {
    let mut conds: Vec<String> = vec![];
    for twj in &mut sel.from {
        collect_table_conds(&mut twj.relation, sets, &mut conds);
        for j in &mut twj.joins {
            collect_table_conds(&mut j.relation, sets, &mut conds);
        }
    }
    for cond in conds {
        let dialect = MySqlDialect {};
        if let Ok(expr) = Parser::new(&dialect)
            .try_with_sql(&cond)
            .and_then(|mut p| p.parse_expr())
        {
            sel.selection = Some(match sel.selection.take() {
                Some(existing) => Expr::BinaryOp {
                    left: Box::new(expr),
                    op: sqlparser::ast::BinaryOperator::And,
                    right: Box::new(Expr::Nested(Box::new(existing))),
                },
                None => expr,
            });
        }
    }
}

fn collect_table_conds(rel: &mut TableFactor, sets: &ScopeSets, out: &mut Vec<String>) {
    match rel {
        TableFactor::Table { name, alias, .. } => {
            let table = name
                .0
                .last()
                .map(|p| p.to_string().trim_matches('`').to_lowercase())
                .unwrap_or_default();
            if let Some(binding) = binding_of(&table) {
                let prefix = alias
                    .as_ref()
                    .map(|a| a.name.value.clone())
                    .unwrap_or_else(|| table.clone());
                if let Some(c) = build_condition(&binding, &prefix, sets) {
                    out.push(c);
                }
            }
        }
        // 子查询（派生表）内部递归注入
        TableFactor::Derived { subquery, .. } => inject_query(subquery, sets),
        TableFactor::NestedJoin { table_with_joins, .. } => {
            collect_table_conds(&mut table_with_joins.relation, sets, out);
            for j in &mut table_with_joins.joins {
                collect_table_conds(&mut j.relation, sets, out);
            }
        }
        _ => {}
    }
}

/// 单表条件：段顺序同 Java（employeeIds → employeeCodes → customerCodes），空段丢弃，or 连接，括号包住。
pub fn build_condition(binding: &Binding, alias: &str, sets: &ScopeSets) -> Option<String> {
    let mut segs: Vec<String> = vec![];
    if let Some(owner) = binding.owner_col {
        match binding.owner_kind {
            OwnerKind::Ids if !sets.employee_ids.is_empty() => {
                let ids = sets
                    .employee_ids
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                segs.push(format!("{alias}.{owner} in ({ids})"));
            }
            OwnerKind::Codes if !sets.employee_codes.is_empty() => {
                let codes = quote_list(&sets.employee_codes);
                segs.push(format!("{alias}.{owner} in ({codes})"));
            }
            _ => {}
        }
    }
    if let Some(cc) = binding.customer_col {
        if !sets.customer_codes.is_empty() {
            let codes = quote_list(&sets.customer_codes);
            segs.push(format!("{alias}.{cc} in ({codes})"));
        }
    }
    if segs.is_empty() {
        None
    } else {
        Some(format!("({})", segs.join(" or ")))
    }
}

fn quote_list(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sets(ids: &[i64], codes: &[&str], cust: &[&str]) -> ScopeSets {
        ScopeSets {
            employee_ids: ids.to_vec(),
            employee_codes: codes.iter().map(|s| s.to_string()).collect(),
            customer_codes: cust.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// sqlparser 回渲染会大写关键字/补 AS/加空格——归一化后按语义断言
    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
    }

    #[test]
    fn unrestricted_passthrough() {
        let s = sets(&[], &[], &[]);
        let sql = "SELECT * FROM t_sales_order so WHERE so.deleted_flag = 0";
        assert_eq!(inject(sql, &s).unwrap(), sql);
    }

    #[test]
    fn injects_both_dims_with_or() {
        let s = sets(&[1, 2], &[], &["C001", "C002"]);
        let out = inject("SELECT COUNT(*) FROM t_sales_order so WHERE so.deleted_flag = 0", &s).unwrap();
        let n = norm(&out);
        assert!(n.contains("so.owner_managerin(1,2)"), "{out}");
        assert!(n.contains("so.customer_codein('c001','c002')"), "{out}");
        assert!(n.contains("or"), "{out}");
        // 原条件被括号保护
        assert!(n.contains("(so.deleted_flag=0)"), "{out}");
    }

    #[test]
    fn sentinel_rejects() {
        let s = sets(&[-1], &[], &["-1"]);
        let out = inject("SELECT * FROM t_customer cus", &s).unwrap();
        let n = norm(&out);
        assert!(n.contains("cus.area_manager_idin(-1)"), "{out}");
        assert!(n.contains("cus.customer_codein('-1')"), "{out}");
    }

    #[test]
    fn codes_dim_for_account_bill() {
        let s = sets(&[9], &["zhangsan"], &["C1"]);
        let out = inject("SELECT * FROM t_account_bill_header t", &s).unwrap();
        let n = norm(&out);
        assert!(n.contains("t.created_byin('zhangsan')"), "{out}");
        assert!(!n.contains("in(9)"), "created_by 表不吃 employee_ids: {out}");
    }

    #[test]
    fn no_alias_uses_table_name() {
        let s = sets(&[5], &[], &[]);
        let out = inject("SELECT * FROM t_sales_order", &s).unwrap();
        assert!(norm(&out).contains("t_sales_order.owner_managerin(5)"), "{out}");
    }

    #[test]
    fn subquery_injected() {
        let s = sets(&[7], &[], &[]);
        let out = inject(
            "SELECT a.total FROM (SELECT SUM(order_amount) AS total FROM t_sales_order so) a",
            &s,
        )
        .unwrap();
        assert!(norm(&out).contains("so.owner_managerin(7)"), "{out}");
    }

    #[test]
    fn unbound_table_untouched() {
        let s = sets(&[1], &[], &["C1"]);
        let sql = "SELECT * FROM t_goods g WHERE g.deleted_flag = 0";
        let out = inject(sql, &s).unwrap();
        assert!(!norm(&out).contains("in("), "未绑定表不得注入: {out}");
    }

    #[test]
    fn quote_escaped() {
        let s = sets(&[], &[], &["C'1"]);
        let out = inject("SELECT * FROM t_customer c", &s).unwrap();
        assert!(out.contains("'C''1'"), "{out}");
    }

    #[test]
    fn rejects_non_select() {
        let s = sets(&[1], &[], &[]);
        assert!(inject("DELETE FROM t_sales_order", &s).is_err());
    }

    #[test]
    fn backtick_table_injected() {
        // 🔴 M3 e2e 抓获的真实翻车：LLM 生成反引号表名，注入器必须照样命中
        let s = sets(&[42], &[], &["C9"]);
        let out = inject(
            "SELECT SUM(`total_goods_amount`) AS `销售额` FROM `t_sales_order` WHERE `deleted_flag` = 0",
            &s,
        )
        .unwrap();
        assert!(norm(&out).contains("owner_managerin(42)"), "{out}");
    }
}
