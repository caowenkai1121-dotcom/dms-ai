//! 权限条件 SQL AST 注入器（纯算法本体，零 IO、零 DMS 语料）。
//! 语义对齐 Java CustomerDataScopeStrategy：每张已绑定表生成
//! `(alias.customer_col IN (..) or alias.owner_col IN (..))`，各段集合为空则丢弃，
//! 整体括号包住后 AND 进该表所在 (子)查询的 WHERE。
//! 段顺序同 Java getCondition：employeeIds → employeeCodes → customerCodes。
//!
//! 逐行搬自 server/src/inject.rs:180-385，两处签名适配：
//! ① 档案查表从全局 OnceLock 改吃 `&RuleSet`（裁决 C2 的三参 inject）；
//! ② 方言由调用方注入（`rewrite` 必须 pub —— 46 权限断言零改的硬前提，见 ARCHITECTURE §5）。

use std::collections::HashSet;

use sqlparser::ast::{Expr, Query, Select, SetExpr, Statement, TableFactor};
use sqlparser::parser::Parser;

use crate::errors::PolicyError;
use crate::policy::rules::{Binding, CustomerKind, OwnerKind, RuleSet, TableRule};
use crate::policy::scope::ScopeSets;
use crate::sql::dialect::Dialect;

/// 注入全程只读的三件共享状态（D4：拆函数同时拆参数，不把 7 个形参连排）
struct InjectCtx<'a> {
    sets: &'a ScopeSets,
    rules: &'a RuleSet,
    dialect: &'a dyn Dialect,
}

/// 当前表在 SQL 里的位置：真表名（fail-closed 文案用）+ 别名前缀（条件拼接用）
struct TableAt<'a> {
    table: &'a str,
    prefix: &'a str,
}

/// via 档案的三列：头表 / 本地列 / 远端列
struct ViaCols<'a> {
    table: &'a str,
    local_col: &'a str,
    remote_col: &'a str,
}

/// 把权限条件注入 SQL。集合全空（超管/ALL）原样返回。
/// 受限用户 SQL 涉及未登记表 → Err（fail-closed 拒绝，绝不静默放行）。
pub fn rewrite(
    sql: &str,
    sets: &ScopeSets,
    rules: &RuleSet,
    d: &dyn Dialect,
) -> Result<String, PolicyError> {
    if sets.is_unrestricted() {
        return Ok(sql.to_string());
    }
    let cx = InjectCtx { sets, rules, dialect: d };
    let mut stmts =
        Parser::parse_sql(d.parser(), sql).map_err(|e| PolicyError::Parse(e.to_string()))?;
    for stmt in &mut stmts {
        match stmt {
            Statement::Query(q) => inject_query(q, &cx, &HashSet::new())?,
            _ => return Err(PolicyError::NotSelect),
        }
    }
    Ok(stmts
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; "))
}

fn inject_query(
    q: &mut Query,
    cx: &InjectCtx<'_>,
    outer_ctes: &HashSet<String>,
) -> Result<(), PolicyError> {
    // CTE 也要注入；CTE 名是虚拟表，加入豁免集（可引用先声明的 CTE）
    let mut ctes = outer_ctes.clone();
    if let Some(with) = &mut q.with {
        for cte in &mut with.cte_tables {
            inject_query(&mut cte.query, cx, &ctes)?;
            ctes.insert(cte.alias.name.value.to_lowercase());
        }
    }
    inject_set_expr(&mut q.body, cx, &ctes)
}

fn inject_set_expr(
    body: &mut SetExpr,
    cx: &InjectCtx<'_>,
    ctes: &HashSet<String>,
) -> Result<(), PolicyError> {
    match body {
        SetExpr::Select(sel) => inject_select(sel, cx, ctes),
        SetExpr::Query(q) => inject_query(q, cx, ctes),
        SetExpr::SetOperation { left, right, .. } => {
            inject_set_expr(left, cx, ctes)?;
            inject_set_expr(right, cx, ctes)
        }
        _ => Ok(()),
    }
}

fn inject_select(
    sel: &mut Select,
    cx: &InjectCtx<'_>,
    ctes: &HashSet<String>,
) -> Result<(), PolicyError> {
    // 先收集本层 FROM/JOIN 的实表名（via 判断头表是否在场）
    let mut present: HashSet<String> = HashSet::new();
    for twj in &sel.from {
        collect_names(&twj.relation, &mut present);
        for j in &twj.joins {
            collect_names(&j.relation, &mut present);
        }
    }
    let mut conds: Vec<String> = vec![];
    for twj in &mut sel.from {
        collect_table_conds(&mut twj.relation, cx, ctes, &present, &mut conds)?;
        for j in &mut twj.joins {
            collect_table_conds(&mut j.relation, cx, ctes, &present, &mut conds)?;
        }
    }
    // 🔴 **FROM 之外的子查询也必须注入**。原来这里只遍历 `sel.from` ——
    // 于是 `SELECT (SELECT SUM(a) FROM t_x WHERE …) / (SELECT SUM(b) FROM t_y WHERE …)`
    // 这种形态外层 `sel.from` 为空 ⇒ `present` 空 ⇒ `conds` 空 ⇒ **一条权限条件都不注入**，
    // 而 `UnregisteredTable` 那道 fail-closed 也不触发（它只在表出现在 FROM 里才判）。
    // 受限用户拿到全公司数据，`route` 正常、形状正常、零报错。
    //
    // 这个形态**在库里就有**：`meta.metric` 的「退款占比」`agg_expr` 正是两个标量子查询相除
    // （派生指标按设计只走 LLM 路，于是它就是 LLM 会照抄的那份口径卡）。
    subqueries_of(sel, cx, ctes)?;
    for cond in conds {
        // 🔴 fail-closed：条件解析失败绝不能静默丢弃——丢掉的是权限条件，查询照跑 = 越权出数。
        // 也不能只看 parse_expr 成功：sqlparser 不要求吃到 EOF，`x.owner manager in (1)` 会前缀
        // 解析成功（只吃到 x.owner），产出语法合法而语义被截断的条件 = 比原缺陷更隐蔽的越权。
        let expr = parse_expr_eof(&cond, cx.dialect.parser())
            .ok_or_else(|| PolicyError::ConditionParse(cond.clone()))?;
        sel.selection = Some(match sel.selection.take() {
            Some(existing) => Expr::BinaryOp {
                left: Box::new(expr),
                op: sqlparser::ast::BinaryOperator::And,
                right: Box::new(Expr::Nested(Box::new(existing))),
            },
            None => expr,
        });
    }
    Ok(())
}

/// 对 `SELECT` 的**投影 / WHERE / GROUP BY / HAVING** 里的每个子查询递归注入。
///
/// 🔴 **量器自检**是这个函数的核心，不是装饰。`walk_expr_subqueries` 是手写 match，
/// 而 sqlparser 的 `Expr` 有几十个变体、且随版本增加 —— 漏一个变体的后果是
/// **那个子查询一条权限条件都不注入**（静默越权读）。所以不指望手写 match 完备：
/// 先用 sqlparser **自己的** `Visit`（它的遍历是全变体的、由库保证）数出子查询总数，
/// 再与递归器实际走到的数对拍，不等就 `Err`。误伤方向是「多拒一条查询」。
///
/// 只扫这四处，**不扫 `sel.from`**：FROM 那条路已由 `collect_table_conds` 的
/// `TableFactor::Derived` 臂递归处理，两边都走会把同一个条件 AND 两遍。
fn subqueries_of(
    sel: &mut Select,
    cx: &InjectCtx<'_>,
    ctes: &HashSet<String>,
) -> Result<(), PolicyError> {
    let counted = count_expr_subqueries(sel);
    if counted == 0 {
        return Ok(());
    }
    let mut walked = 0usize;
    let mut visit = |q: &mut Query| -> Result<(), PolicyError> {
        walked += 1;
        inject_query(q, cx, ctes)
    };
    for item in &mut sel.projection {
        for e in projection_exprs(item) {
            walk_expr_subqueries(e, &mut visit)?;
        }
    }
    if let Some(e) = &mut sel.selection {
        walk_expr_subqueries(e, &mut visit)?;
    }
    if let Some(e) = &mut sel.having {
        walk_expr_subqueries(e, &mut visit)?;
    }
    if let sqlparser::ast::GroupByExpr::Expressions(es, _) = &mut sel.group_by {
        for e in es {
            walk_expr_subqueries(e, &mut visit)?;
        }
    }
    if counted != walked {
        return Err(PolicyError::SubqueryNotCovered { counted, walked });
    }
    Ok(())
}

/// 投影项里的表达式（`SELECT expr` / `SELECT expr AS x`；通配符没有表达式）
fn projection_exprs(item: &mut sqlparser::ast::SelectItem) -> Vec<&mut Expr> {
    use sqlparser::ast::SelectItem as SI;
    match item {
        SI::UnnamedExpr(e) | SI::ExprWithAlias { expr: e, .. } => vec![e],
        _ => vec![],
    }
}

/// 子查询计数器：用 sqlparser **自己的** `Visit`，遍历完备性由库保证。
/// 只数**表达式里**的（`sel.from` 的派生表由 `collect_table_conds` 那条路处理）。
fn count_expr_subqueries(sel: &Select) -> usize {
    use core::ops::ControlFlow;
    use sqlparser::ast::{Visit, Visitor};
    struct C(usize);
    impl Visitor for C {
        type Break = ();
        fn pre_visit_query(&mut self, _q: &Query) -> ControlFlow<()> {
            self.0 += 1;
            ControlFlow::Continue(())
        }
    }
    let mut c = C(0);
    // 逐段访问而不是访问整个 `sel`：访问 `sel` 会把 FROM 里的派生表也数进来。
    for item in &sel.projection {
        let _ = item.visit(&mut c);
    }
    if let Some(e) = &sel.selection {
        let _ = e.visit(&mut c);
    }
    if let Some(e) = &sel.having {
        let _ = e.visit(&mut c);
    }
    if let sqlparser::ast::GroupByExpr::Expressions(es, _) = &sel.group_by {
        for e in es {
            let _ = e.visit(&mut c);
        }
    }
    c.0
}

/// 递归找 `Expr` 里的子查询并交给 `f`。**手写 match**，完备性由 `subqueries_of` 的对拍保证。
///
/// 找到一个子查询就交给 `f` 并**不再深入它内部** —— `f` 是 `inject_query`，它自己会递归。
/// 深入的话内层会被注入两遍（语义相同、SQL 变丑，且计数对不上）。
fn walk_expr_subqueries(
    e: &mut Expr,
    f: &mut impl FnMut(&mut Query) -> Result<(), PolicyError>,
) -> Result<(), PolicyError> {
    use Expr as E;
    match e {
        // ── 子查询本体：交给 f，不再深入 ──
        E::Subquery(q) => return f(q),
        E::Exists { subquery, .. } => return f(subquery),
        E::InSubquery { expr, subquery, .. } => {
            walk_expr_subqueries(expr, f)?;
            return f(subquery);
        }
        // ── 容器变体：继续往下找 ──
        E::BinaryOp { left, right, .. } => {
            walk_expr_subqueries(left, f)?;
            walk_expr_subqueries(right, f)?;
        }
        E::UnaryOp { expr, .. }
        | E::Nested(expr)
        | E::Cast { expr, .. }
        | E::IsNull(expr)
        | E::IsNotNull(expr)
        | E::IsTrue(expr)
        | E::IsNotTrue(expr)
        | E::IsFalse(expr)
        | E::IsNotFalse(expr) => walk_expr_subqueries(expr, f)?,
        E::Between { expr, low, high, .. } => {
            walk_expr_subqueries(expr, f)?;
            walk_expr_subqueries(low, f)?;
            walk_expr_subqueries(high, f)?;
        }
        E::InList { expr, list, .. } => {
            walk_expr_subqueries(expr, f)?;
            for x in list {
                walk_expr_subqueries(x, f)?;
            }
        }
        E::Like { expr, pattern, .. } | E::ILike { expr, pattern, .. } => {
            walk_expr_subqueries(expr, f)?;
            walk_expr_subqueries(pattern, f)?;
        }
        E::Case { operand, conditions, results, else_result, .. } => {
            if let Some(x) = operand {
                walk_expr_subqueries(x, f)?;
            }
            for x in conditions.iter_mut().chain(results.iter_mut()) {
                walk_expr_subqueries(x, f)?;
            }
            if let Some(x) = else_result {
                walk_expr_subqueries(x, f)?;
            }
        }
        E::Function(fun) => {
            if let sqlparser::ast::FunctionArguments::List(args) = &mut fun.args {
                for a in &mut args.args {
                    if let sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(x),
                    ) = a
                    {
                        walk_expr_subqueries(x, f)?;
                    }
                }
            }
        }
        // 其余变体不含子查询；真含了就会被 `subqueries_of` 的对拍抓成 `SubqueryNotCovered`
        _ => {}
    }
    Ok(())
}

/// 解析一个**完整**表达式：解析成功且吃到 EOF 才算数（截断式条件视为失败）。纯函数可单测。
/// 默认 MySQL 词法；注入主路径用调用方传入的方言（`parse_expr_eof`）。
pub fn parse_full_expr(cond: &str) -> Option<Expr> {
    parse_expr_eof(cond, &sqlparser::dialect::MySqlDialect {})
}

fn parse_expr_eof(cond: &str, d: &dyn sqlparser::dialect::Dialect) -> Option<Expr> {
    let mut p = Parser::new(d).try_with_sql(cond).ok()?;
    let expr = p.parse_expr().ok()?;
    matches!(p.peek_token().token, sqlparser::tokenizer::Token::EOF).then_some(expr)
}

fn collect_names(rel: &TableFactor, out: &mut HashSet<String>) {
    match rel {
        TableFactor::Table { name, .. } => {
            if let Some(p) = name.0.last() {
                out.insert(p.to_string().trim_matches('`').to_lowercase());
            }
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            collect_names(&table_with_joins.relation, out);
            for j in &table_with_joins.joins {
                collect_names(&j.relation, out);
            }
        }
        _ => {}
    }
}

fn collect_table_conds(
    rel: &mut TableFactor,
    cx: &InjectCtx<'_>,
    ctes: &HashSet<String>,
    present: &HashSet<String>,
    out: &mut Vec<String>,
) -> Result<(), PolicyError> {
    match rel {
        TableFactor::Table { name, alias, .. } => {
            let table = name
                .0
                .last()
                .map(|p| p.to_string().trim_matches('`').to_lowercase())
                .unwrap_or_default();
            // CTE 虚拟表：内部已注入，跳过
            if ctes.contains(&table) {
                return Ok(());
            }
            let prefix = alias
                .as_ref()
                .map(|a| a.name.value.clone())
                .unwrap_or_else(|| table.clone());
            match cx.rules.rule_of(&table) {
                Some(TableRule::Scoped(binding)) => {
                    if let Some(c) = build_condition(&binding, &prefix, cx.sets) {
                        out.push(c);
                    }
                }
                Some(TableRule::Global) => {}
                Some(TableRule::Via { table: vt, local_col, remote_col }) => {
                    // 头表同 SELECT 在场 → 由头表自身条件覆盖；独查 → EXISTS 借头表条件
                    if !present.contains(&vt) {
                        let at = TableAt { table: &table, prefix: &prefix };
                        let via = ViaCols {
                            table: &vt,
                            local_col: &local_col,
                            remote_col: &remote_col,
                        };
                        if let Some(c) = via_exists_cond(cx, &at, &via)? {
                            out.push(c);
                        }
                    }
                }
                None => return Err(PolicyError::UnregisteredTable(table)),
            }
        }
        // 子查询（派生表）内部递归注入
        TableFactor::Derived { subquery, .. } => inject_query(subquery, cx, ctes)?,
        TableFactor::NestedJoin { table_with_joins, .. } => {
            collect_table_conds(&mut table_with_joins.relation, cx, ctes, present, out)?;
            for j in &mut table_with_joins.joins {
                collect_table_conds(&mut j.relation, cx, ctes, present, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// via 明细表独查：借头表条件拼 EXISTS 半连接（自 `collect_table_conds` 的 Via 臂原样提取）。
/// 头表未登记 scoped 档案 → fail-closed 拒绝；头表条件为空 → 不注入（与原分支一致）。
fn via_exists_cond(
    cx: &InjectCtx<'_>,
    at: &TableAt<'_>,
    via: &ViaCols<'_>,
) -> Result<Option<String>, PolicyError> {
    let (vt, local_col, remote_col) = (via.table, via.local_col, via.remote_col);
    let prefix = at.prefix;
    let Some(TableRule::Scoped(hb)) = cx.rules.rule_of(vt) else {
        return Err(PolicyError::ViaHeadUnregistered {
            table: at.table.to_string(),
            via: vt.to_string(),
        });
    };
    let Some(hc) = build_condition(&hb, "__ds_h", cx.sets) else {
        return Ok(None);
    };
    Ok(Some(format!(
        "exists (select 1 from {vt} __ds_h where __ds_h.{remote_col} = {prefix}.{local_col} and {hc})"
    )))
}

/// 单表条件：段顺序同 Java（employeeIds → employeeCodes → customerCodes），空段丢弃，or 连接，括号包住。
pub fn build_condition(binding: &Binding, alias: &str, sets: &ScopeSets) -> Option<String> {
    let mut segs: Vec<String> = vec![];
    if let Some(owner) = binding.owner_col.as_deref() {
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
            OwnerKind::Login if !sets.login_names.is_empty() => {
                let codes = quote_list(&sets.login_names);
                let col = if owner.contains("{alias}") {
                    owner.replace("{alias}", alias)
                } else {
                    format!("{alias}.{owner}")
                };
                segs.push(format!("{col} in ({codes})"));
            }
            _ => {}
        }
    }
    if let Some(cc) = binding.customer_col.as_deref() {
        let customers = match binding.customer_kind {
            CustomerKind::Codes | CustomerKind::RequiredCodes => &sets.customer_codes,
            CustomerKind::ManagerCodes => &sets.manager_customer_codes,
            CustomerKind::ShopCodes => &sets.shop_codes,
        };
        if binding.customer_kind == CustomerKind::RequiredCodes && customers.is_empty() {
            return Some("(1 = 0)".into());
        }
        if !customers.is_empty() {
            let codes = quote_list(customers);
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

/// kernel 自守测试：**泛化表名**（DMS 真表名与权限档案在 IO 侧）。
/// 15 个 DMS 注入断言原地留在 server/src/inject.rs，T5 才迁 policy/tests（裁决 C3）。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::dialect::MysqlDialect;
    use std::collections::HashMap;

    fn rules() -> RuleSet {
        let scoped = |c: &str, o: Option<&str>, k: OwnerKind| {
            TableRule::Scoped(Binding {
                customer_col: Some(c.to_string()),
                customer_kind: CustomerKind::Codes,
                owner_col: o.map(|s| s.to_string()),
                owner_kind: k,
            })
        };
        let mut m: HashMap<String, TableRule> = HashMap::new();
        m.insert("orders".into(), scoped("cust_code", Some("owner_id"), OwnerKind::Ids));
        m.insert("bills".into(), scoped("cust_code", Some("created_by"), OwnerKind::Codes));
        m.insert("goods".into(), TableRule::Global);
        m.insert(
            "order_line".into(),
            TableRule::Via {
                table: "orders".into(),
                local_col: "order_code".into(),
                remote_col: "order_code".into(),
            },
        );
        RuleSet::from(m)
    }

    fn sets(ids: &[i64], codes: &[&str], cust: &[&str]) -> ScopeSets {
        ScopeSets {
            employee_ids: ids.to_vec(),
            employee_codes: codes.iter().map(|s| s.to_string()).collect(),
            customer_codes: cust.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn go(sql: &str, s: &ScopeSets) -> Result<String, PolicyError> {
        rewrite(sql, s, &rules(), &MysqlDialect)
    }

    /// sqlparser 回渲染会大写关键字/补 AS/加空格——归一化后按语义断言
    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
    }

    #[test]
    fn unrestricted_passthrough() {
        let sql = "SELECT * FROM orders o WHERE o.deleted_flag = 0";
        assert_eq!(go(sql, &sets(&[], &[], &[])).unwrap(), sql);
    }

    #[test]
    fn injects_both_dims_with_or() {
        let out = go("SELECT COUNT(*) FROM orders o WHERE o.deleted_flag = 0", &sets(&[1, 2], &[], &["C001"]))
            .unwrap();
        let n = norm(&out);
        assert!(n.contains("o.owner_idin(1,2)"), "{out}");
        assert!(n.contains("o.cust_codein('c001')"), "{out}");
        assert!(n.contains("or"), "{out}");
        // 原条件被括号保护
        assert!(n.contains("(o.deleted_flag=0)"), "{out}");
    }

    #[test]
    fn codes_kind_ignores_ids_and_no_alias_uses_table_name() {
        let out = go("SELECT * FROM bills", &sets(&[9], &["zhangsan"], &[])).unwrap();
        let n = norm(&out);
        assert!(n.contains("bills.created_byin('zhangsan')"), "{out}");
        assert!(!n.contains("in(9)"), "codes 档案不吃 employee_ids: {out}");
    }

    #[test]
    fn global_table_untouched_and_quotes_escaped() {
        let out = go("SELECT * FROM goods g", &sets(&[1], &[], &["C1"])).unwrap();
        assert!(!norm(&out).contains("in("), "global 免注入: {out}");
        let esc = go("SELECT * FROM orders", &sets(&[], &[], &["C'1"])).unwrap();
        assert!(esc.contains("'C''1'"), "{esc}");
    }

    #[test]
    fn subquery_and_cte_injected_cte_name_exempt() {
        let sub = go(
            "SELECT a.total FROM (SELECT SUM(amount) AS total FROM orders o) a",
            &sets(&[7], &[], &[]),
        )
        .unwrap();
        assert!(norm(&sub).contains("o.owner_idin(7)"), "{sub}");
        let cte = go(
            "WITH x AS (SELECT owner_id, amount FROM orders) SELECT SUM(amount) FROM x",
            &sets(&[3], &[], &[]),
        )
        .unwrap();
        assert!(norm(&cte).contains("owner_idin(3)"), "{cte}");
    }

    #[test]
    fn via_standalone_gets_exists_but_skips_when_head_present() {
        let alone = go("SELECT SUM(qty) FROM order_line d", &sets(&[7], &[], &["C1"])).unwrap();
        let n = norm(&alone);
        assert!(n.contains("exists(select1fromorders"), "{alone}");
        assert!(n.contains("__ds_h.order_code=d.order_code"), "{alone}");
        assert!(n.contains("__ds_h.owner_idin(7)"), "{alone}");
        let joined = go(
            "SELECT SUM(d.qty) FROM order_line d JOIN orders o ON o.order_code = d.order_code",
            &sets(&[7], &[], &[]),
        )
        .unwrap();
        assert!(!norm(&joined).contains("exists("), "{joined}");
    }

    #[test]
    fn unregistered_table_and_non_select_fail_closed() {
        let err = go("SELECT * FROM nowhere", &sets(&[1], &[], &[])).unwrap_err();
        assert!(err.to_string().contains("未在权限档案登记"), "{err}");
        assert!(go("DELETE FROM orders", &sets(&[1], &[], &[])).is_err());
        // 超管（全部权限维度为空）不受限
        assert!(go("SELECT * FROM nowhere", &sets(&[], &[], &[])).is_ok());
    }

    #[test]
    fn full_expr_required() {
        // 正常条件：解析完整
        assert!(parse_full_expr("(x.owner_id in (1,2) or x.cust_code in ('c1'))").is_some());
        // 截断式：`owner id` 中间有空格，parse_expr 只吃到 x.owner 就返回成功 → 必须判失败
        assert!(parse_full_expr("(x.owner id in (1))").is_none());
        assert!(parse_full_expr("(").is_none());
        assert!(parse_full_expr("").is_none());
    }

    #[test]
    fn empty_segments_inject_nothing() {
        // 现状语义（与 Java 一致）：该表只有 customer 段而客户集合为空 → 不注入 = 放行全部。
        let only_cust = TableRule::Scoped(Binding {
            customer_col: Some("cust_code".into()),
            customer_kind: CustomerKind::Codes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
        });
        let mut m: HashMap<String, TableRule> = HashMap::new();
        m.insert("balances".into(), only_cust);
        let out = rewrite("SELECT * FROM balances b", &sets(&[7], &[], &[]), &RuleSet::from(m), &MysqlDialect)
            .unwrap();
        assert!(!norm(&out).contains("in("), "现状=不注入（Java 同语义）: {out}");
    }

    #[test]
    fn shop_kind_uses_shop_codes_without_customer_widening() {
        let binding = Binding {
            customer_col: Some("shop_code".into()),
            customer_kind: CustomerKind::ShopCodes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
        };
        let sets = ScopeSets {
            customer_codes: vec!["C001".into()],
            shop_codes: vec!["S001".into()],
            ..Default::default()
        };
        let condition = build_condition(&binding, "s", &sets).expect("门店集合非空必须注入");
        assert!(condition.contains("s.shop_code in ('S001')"), "{condition}");
        assert!(!condition.contains("C001"), "不得按客户编码扩大门店权限: {condition}");
    }

    #[test]
    fn required_customer_codes_fail_closed_when_empty() {
        let binding = Binding {
            customer_col: Some("storecode".into()),
            customer_kind: CustomerKind::RequiredCodes,
            owner_col: None,
            owner_kind: OwnerKind::Ids,
        };
        let restricted = ScopeSets { employee_ids: vec![7], ..Default::default() };
        assert_eq!(build_condition(&binding, "s", &restricted).as_deref(), Some("(1 = 0)"));

        let visible = ScopeSets {
            customer_codes: vec!["C001".into(), "C'02".into()],
            ..Default::default()
        };
        assert_eq!(
            build_condition(&binding, "s", &visible).as_deref(),
            Some("(s.storecode in ('C001','C''02'))")
        );
    }
}
