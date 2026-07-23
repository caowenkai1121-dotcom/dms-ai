//! SchemaCorrector：执行前字段白名单校验（移植 SuperSonic SchemaCorrector.correctFieldName）。
//! LLM 生成的 SQL 里，凡「真实表.列」形式的列引用，若该列不在 meta.column_doc 记录的真实列清单里，
//! 判为幻觉列 → 携精确「可用列清单」自修一次（比执行报 1054 更早、给 LLM 更准的纠正依据）。
//! 只校验带表前缀且前缀映射到 meta 已知物理表的列——派生表/CTE 别名列、裸列、中文别名跳过，防误伤。

use std::collections::{HashMap, HashSet};

use core::ops::ControlFlow;
use sqlparser::ast::{Expr, TableFactor, Visit, Visitor};
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
