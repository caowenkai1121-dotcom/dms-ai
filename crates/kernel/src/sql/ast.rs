//! sqlparser 只读遍历。变更原因＝sqlparser 的 AST 形态。**只读**：不产 SQL、不改 AST。
//!
//! 搬运源：`corrector.rs:14-59`（`Collector` + `collect`，anyhow → `GuardError::Parse`，
//! 新增 dialect 形参）、`corrector.rs:432-474`（`collect_where_cols`）。
//! `table_names_of` 是新增契约，复用同一次遍历（不写第二套 Visitor）。

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

use sqlparser::ast::{Expr, Query, SetExpr, TableFactor, Visit, Visitor};
use sqlparser::parser::Parser;

use crate::errors::GuardError;
use crate::sql::dialect::Dialect;

#[derive(Default)]
struct Collector {
    /// (别名或表名 lower, 真实表名 lower)
    aliases: Vec<(String, String)>,
    /// 排除当前查询作用域 CTE 后的真实表名。
    real_tables: Vec<String>,
    /// 实表完整限定名（逐段 lower）。上传 PG 源靠它拒绝跨 schema 引用。
    table_refs: Vec<Vec<String>>,
    /// 函数完整限定名（逐段 lower）。上传 PG 源靠它拒绝动态 SQL / 服务端文件函数。
    functions: Vec<Vec<String>>,
    /// (前缀 lower, 列名 lower)
    cols: Vec<(String, String)>,
    /// 当前查询可见的 CTE 名。栈顶是完整有效集合，离开嵌套 Query 即恢复外层。
    cte_scopes: Vec<HashSet<String>>,
    /// CTE 定义子查询的外层可见集合；非递归 WITH 只允许看见此前定义。
    cte_query_outer: HashMap<usize, HashSet<String>>,
}

impl Visitor for Collector {
    type Break = ();

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        let key = q as *const Query as usize;
        // 每个嵌套 Query 克隆一次外层 CTE 集：SQL 嵌套层级浅（LLM 产出 ≤ 几层）、
        // CTE 数量个位数，整集克隆量级可忍；真深化了再换 Rc 作用域栈。
        let outer = self
            .cte_query_outer
            .remove(&key)
            .unwrap_or_else(|| self.cte_scopes.last().cloned().unwrap_or_default());
        let mut local = Vec::new();
        if let Some(with) = &q.with {
            for cte in &with.cte_tables {
                local.push(cte.alias.name.value.to_lowercase());
            }
            if with.recursive {
                let mut visible = outer.clone();
                visible.extend(local.iter().cloned());
                for cte in &with.cte_tables {
                    self.cte_query_outer.insert(
                        cte.query.as_ref() as *const Query as usize,
                        visible.clone(),
                    );
                }
            } else {
                let mut visible = outer.clone();
                for (cte, name) in with.cte_tables.iter().zip(&local) {
                    self.cte_query_outer.insert(
                        cte.query.as_ref() as *const Query as usize,
                        visible.clone(),
                    );
                    visible.insert(name.clone());
                }
            }
        }
        let mut effective = outer;
        effective.extend(local);
        self.cte_scopes.push(effective);
        let current = self.cte_scopes.last().expect("刚压入 CTE 作用域");
        collect_table_commands(&q.body, current, &mut self.table_refs);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _q: &Query) -> ControlFlow<()> {
        self.cte_scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table { name, alias, .. } = tf {
            let parts: Vec<String> = name.0.iter().map(|p| p.value.to_lowercase()).collect();
            if parts.is_empty() {
                return ControlFlow::Continue(()); // 理论不可达（Table 必带名），不押 panic
            }
            let table = parts.last().cloned().unwrap_or_default();
            let key = alias
                .as_ref()
                .map(|a| a.name.value.to_lowercase())
                .unwrap_or_else(|| table.clone());
            self.aliases.push((key, table));
            let is_cte = parts.len() == 1
                && self.cte_scopes.last().is_some_and(|scope| scope.contains(&parts[0]));
            if !is_cte {
                // 限定名按**首段**（库名/schema 名）入 real_tables：`db.t_x` 收的是 `db`。
                // 下游已实测踩坑并绕开（server direct.rs 有注释）；改语义会动权限登记，不动。
                self.real_tables.push(parts[0].clone());
                self.table_refs.push(parts);
            }
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
        if let Expr::Function(f) = e {
            self.functions.push(f.name.0.iter().map(|p| p.value.to_lowercase()).collect());
        }
        ControlFlow::Continue(())
    }
}

/// PostgreSQL 的 `TABLE schema.name` 是 `SetExpr::Table`，不会触发 `TableFactor` visitor。
fn collect_table_commands(
    body: &SetExpr,
    ctes: &HashSet<String>,
    out: &mut Vec<Vec<String>>,
) {
    match body {
        SetExpr::Table(t) => {
            let Some(table) = t.table_name.as_deref() else { return };
            let mut parts = Vec::with_capacity(2);
            if let Some(schema) = t.schema_name.as_deref() {
                parts.push(schema.trim_matches('"').to_lowercase());
            }
            parts.push(table.trim_matches('"').to_lowercase());
            if !(parts.len() == 1 && ctes.contains(&parts[0])) {
                out.push(parts);
            }
        }
        // 嵌套 Query 会触发自己的 pre_visit_query，那里有正确的 CTE 作用域。
        SetExpr::Query(_) => {}
        SetExpr::SetOperation { left, right, .. } => {
            collect_table_commands(left, ctes, out);
            collect_table_commands(right, ctes, out);
        }
        // 未来新增 SetExpr 变体默认识漏（漏判方向：少收一个表引用，不是多收）
        _ => {}
    }
}

/// 唯一一次遍历：`collect` 与 `table_names_of` 都从这里取视图。
fn walk(sql: &str, d: &dyn Dialect) -> Result<Collector, GuardError> {
    let stmts = Parser::parse_sql(d.parser(), sql).map_err(|e| GuardError::Parse(e.to_string()))?;
    let mut c = Collector::default();
    for s in &stmts {
        let _ = s.visit(&mut c);
    }
    Ok(c)
}

/// 提取 (别名→表, 带前缀列引用)。纯函数，可单测。
pub fn collect(
    sql: &str,
    d: &dyn Dialect,
) -> Result<(HashMap<String, String>, Vec<(String, String)>), GuardError> {
    let c = walk(sql, d)?;
    // 后出现的别名覆盖（同别名罕见）；派生表别名不进 aliases——TableFactor::Derived
    // 分支不匹配本 visitor 的 `Table` 臂（它有 alias 但没有**表名**）
    let amap: HashMap<String, String> = c.aliases.into_iter().collect();
    Ok((amap, c.cols))
}

/// 语句涉及的实表名（去重、字典序）。CTE 名不算实表——它在 FROM 里与实表同形，
/// 当成实表会让「按表取权限档案/取列清单」对着一个不存在的表查。
/// 排序+去重是为了输出规范化：与遍历顺序无关（下游 diff/断言才能稳定）。
pub fn table_names_of(sql: &str, d: &dyn Dialect) -> Result<Vec<String>, GuardError> {
    let c = walk(sql, d)?;
    let mut out = c.real_tables;
    out.retain(|t| !t.is_empty());
    out.sort();
    out.dedup();
    Ok(out)
}

/// 语句涉及的实表完整限定名（如 `schema.table` → `["schema", "table"]`）。
/// 单段 CTE 引用会被排除；CTE 内部真正读取的表仍保留。
pub fn table_refs_of(sql: &str, d: &dyn Dialect) -> Result<Vec<Vec<String>>, GuardError> {
    let c = walk(sql, d)?;
    let mut out = c.table_refs;
    out.retain(|parts| !parts.is_empty());
    out.sort();
    out.dedup();
    Ok(out)
}

/// 语句调用的函数名（完整限定名，去重、字典序）。
pub fn function_names_of(sql: &str, d: &dyn Dialect) -> Result<Vec<Vec<String>>, GuardError> {
    let c = walk(sql, d)?;
    let mut out = c.functions;
    out.retain(|parts| !parts.is_empty());
    out.sort();
    out.dedup();
    Ok(out)
}

/// 一次遍历同时取函数名与实表限定名（两个视图各自的 pub 入口仍在；两个都要的调用方
/// 走这个，少 parse 一遍 —— connector 上传源的执行期 schema 闸就是两个都要的）。
pub fn functions_and_table_refs_of(
    sql: &str,
    d: &dyn Dialect,
) -> Result<(Vec<Vec<String>>, Vec<Vec<String>>), GuardError> {
    let c = walk(sql, d)?;
    let mut functions = c.functions;
    functions.retain(|parts| !parts.is_empty());
    functions.sort();
    functions.dedup();
    let mut refs = c.table_refs;
    refs.retain(|parts| !parts.is_empty());
    refs.sort();
    refs.dedup();
    Ok((functions, refs))
}

/// 收集 WHERE 中出现的列名（末段小写）。
/// 递归无深度上限：输入都是已过 gate 的 AST，深度受 parser 限制，理论爆栈不接受为现实风险。
/// 已知边界（刻意，漏收方向）：`InSubquery` 只收左端列，子查询内部不进结果；
/// `Function` 只收参数列表里的 `Unnamed` 参数（`Named` 参数列不收）；
/// `Expr::Case` 分支内的列不收 —— 消费者（修复提示的列覆盖判断）按「宁可少收」取值。
pub fn collect_where_cols(e: &Expr, out: &mut HashSet<String>) {
    match e {
        // sqlparser 的 Ident.value 本就去引号（trim_matches 是死防御，全仓统一说明在 caliber.rs）
        Expr::Identifier(i) => {
            out.insert(i.value.to_lowercase());
        }
        Expr::CompoundIdentifier(parts) => {
            if let Some(p) = parts.last() {
                out.insert(p.value.to_lowercase());
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_where_cols(left, out);
            collect_where_cols(right, out);
        }
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::Cast { expr, .. } => {
            collect_where_cols(expr, out)
        }
        Expr::InList { expr, .. } | Expr::InSubquery { expr, .. } => collect_where_cols(expr, out),
        Expr::Between { expr, low, high, .. } => {
            collect_where_cols(expr, out);
            collect_where_cols(low, out);
            collect_where_cols(high, out);
        }
        Expr::IsNull(e) | Expr::IsNotNull(e) => collect_where_cols(e, out),
        Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
            collect_where_cols(expr, out);
            collect_where_cols(pattern, out);
        }
        Expr::Function(f) => {
            if let sqlparser::ast::FunctionArguments::List(l) = &f.args {
                for a in &l.args {
                    if let sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(e),
                    ) = a
                    {
                        collect_where_cols(e, out);
                    }
                }
            }
        }
        _ => {}
    }
}
