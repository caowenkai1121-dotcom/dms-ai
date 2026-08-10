//! AST 遍历的搬运断言（`corrector.rs:799-821` 三个，**一字不改**，只补 dialect 形参）
//! + `table_names_of` 三个新增用例。
//!
//! 在 `tests/` 而非 `ast.rs` 的 `#[cfg(test)]`：同 `sql_guard.rs` 的理由——用例带 DMS 表名，
//! 而门禁对 `crates/kernel/src` grep `t_[a-z_]{3,}` 判红。

use dms_kernel::sql::ast::{
    collect as collect_with, function_names_of as functions_with, table_names_of as names_with,
    table_refs_of as refs_with,
};
use dms_kernel::MysqlDialect;

fn collect(
    sql: &str,
) -> Result<
    (
        std::collections::HashMap<String, String>,
        Vec<(String, String)>,
    ),
    dms_kernel::GuardError,
> {
    collect_with(sql, &MysqlDialect)
}

fn table_names_of(sql: &str) -> Vec<String> {
    names_with(sql, &MysqlDialect).unwrap()
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
fn table_names_multi_join() {
    let sql = "SELECT 1 FROM t_sales_order o JOIN t_goods g ON g.id = o.goods_id";
    assert_eq!(table_names_of(sql), vec!["t_goods", "t_sales_order"]);
}

#[test]
fn table_names_include_subquery() {
    let sql = "SELECT 1 FROM t_sales_order o WHERE o.id IN (SELECT d.order_id FROM t_order_detail d)";
    assert_eq!(table_names_of(sql), vec!["t_order_detail", "t_sales_order"]);
}

#[test]
fn table_names_exclude_cte() {
    // CTE 名是虚拟表：出现在 FROM 里也不能算实表（否则按表取档案/取列清单会查一个不存在的表）
    let sql = "WITH mine AS (SELECT id FROM t_sales_order) SELECT m.id FROM mine m";
    assert_eq!(table_names_of(sql), vec!["t_sales_order"]);
}

#[test]
fn table_refs_keep_schema_and_exclude_cte_alias() {
    let sql = "WITH mine AS (SELECT id FROM up_a.orders) \
               SELECT m.id FROM mine m JOIN \"up_a\".details d ON d.id=m.id";
    assert_eq!(
        refs_with(sql, &dms_kernel::PostgresDialect).unwrap(),
        vec![
            vec!["up_a".to_string(), "details".to_string()],
            vec!["up_a".to_string(), "orders".to_string()],
        ]
    );
}

#[test]
fn table_refs_include_postgres_table_command() {
    assert_eq!(
        refs_with("(TABLE up_a.sheet1)", &dms_kernel::PostgresDialect).unwrap(),
        vec![vec!["up_a".to_string(), "sheet1".to_string()]]
    );
}

#[test]
fn function_names_keep_qualification() {
    assert_eq!(
        functions_with(
            "SELECT sum(amount), pg_catalog.query_to_xml('SELECT 1', true, false, '') FROM t_x",
            &dms_kernel::PostgresDialect,
        )
        .unwrap(),
        vec![
            vec!["pg_catalog".to_string(), "query_to_xml".to_string()],
            vec!["sum".to_string()],
        ]
    );
}

#[test]
fn cte_names_are_scoped_and_nonrecursive_definitions_do_not_hide_tables() {
    let nested = "SELECT * FROM (WITH pg_class AS (SELECT 1) SELECT * FROM pg_class) x, pg_class";
    assert_eq!(
        refs_with(nested, &dms_kernel::PostgresDialect).unwrap(),
        vec![vec!["pg_class".to_string()]],
    );

    let same_name = "WITH pg_class AS (SELECT * FROM pg_class) SELECT * FROM pg_class";
    assert_eq!(
        refs_with(same_name, &dms_kernel::PostgresDialect).unwrap(),
        vec![vec!["pg_class".to_string()]],
    );

    let ordered = "WITH a AS (SELECT * FROM sheet1), b AS (SELECT * FROM a) SELECT * FROM b";
    assert_eq!(
        refs_with(ordered, &dms_kernel::PostgresDialect).unwrap(),
        vec![vec!["sheet1".to_string()]],
    );
}
