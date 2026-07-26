//! 权限条件 SQL AST 注入器。
//! 语义对齐 Java CustomerDataScopeStrategy：每张已绑定表生成
//! `(alias.customer_col IN (..) or alias.owner_col IN (..))`，各段集合为空则丢弃，
//! 整体括号包住后 AND 进该表所在 (子)查询的 WHERE。
//! 段顺序同 Java getCondition：employeeIds → employeeCodes → customerCodes。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use sqlparser::ast::{
    Expr, Query, Select, SetExpr, Statement, TableFactor,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use crate::scope::ScopeSets;

/// 表绑定：该表用哪些列吃权限条件（对应 Java @DataScope joinSql 模板，逐条探库核实）
#[derive(Clone)]
pub struct Binding {
    pub customer_col: Option<String>,
    pub owner_col: Option<String>,
    pub owner_kind: OwnerKind,
}

#[derive(PartialEq, Clone, Copy)]
pub enum OwnerKind {
    /// 数字 employee_id（#employeeIds）
    Ids,
    /// 登录名字符串（#employeeCodes）
    Codes,
}

/// 表权限档案（fail-closed 三态）：
/// - Scoped：注入 Java joinSql 等价条件
/// - Global：Java 无 @DataScope，1:1 审定全量可见，免注入
/// - Via：明细/从表独查时借头表条件（EXISTS 半连接）；头表同 SELECT 在场则跳过
/// 未登记的表对受限用户一律拒绝（fail-closed）。
#[derive(Clone)]
pub enum TableRule {
    Scoped(Binding),
    Global,
    Via { table: String, local_col: String, remote_col: String },
}

/// 内置种子（代码即种子真相，随 seed_rules 灌表；PG 不可用时兜底）。
/// 口径来源：DMS Java @DataScope joinSql 逐条核对（2026-07-26 复核 15 个 mapper）。
pub fn builtin_rules() -> HashMap<String, TableRule> {
    use OwnerKind::*;
    let b = |c: &str, o: Option<&str>, k: OwnerKind| {
        TableRule::Scoped(Binding {
            customer_col: Some(c.to_string()),
            owner_col: o.map(|s| s.to_string()),
            owner_kind: k,
        })
    };
    let via = |t: &str, l: &str, r: &str| TableRule::Via {
        table: t.to_string(),
        local_col: l.to_string(),
        remote_col: r.to_string(),
    };
    let mut m: HashMap<String, TableRule> = HashMap::new();
    // —— scoped：Java joinSql 权威模板 → 列绑定
    m.insert("t_sales_order".into(), b("customer_code", Some("owner_manager"), Ids));
    m.insert("t_sales_order_his".into(), b("customer_code", Some("owner_manager"), Ids));
    m.insert("t_customer".into(), b("customer_code", Some("area_manager_id"), Ids));
    m.insert("t_after_sales_order_header".into(), b("customer_code", Some("owner_manager"), Ids));
    m.insert("t_activity_main".into(), b("customer_code", Some("created_id"), Ids));
    m.insert("t_invoice_apply_header".into(), b("customer_code", Some("manager"), Ids));
    m.insert("t_invoice_new_apply_header".into(), b("customer_code", Some("manager"), Ids));
    m.insert("t_account_bill_header".into(), b("customer_code", Some("created_by"), Codes));
    m.insert("t_device_inspection_header".into(), b("customer_code", Some("manager_code"), Codes));
    m.insert("t_long_promotion_person".into(), b("customer_code", Some("manager_id"), Ids));
    // 无 owner 维度的表（Java 模板只有 customer 段）
    for t in ["t_customer_balance", "t_customer_device_ledger", "t_device_disposal_order", "t_shop_inspection_records"] {
        m.insert(t.into(), b("customer_code", None, Ids));
    }
    // —— via：明细/从表独查借头表条件（Java 场景恒 JOIN 头表吃 tso.* 条件）
    m.insert("t_sales_order_detail".into(), via("t_sales_order", "sales_order_code", "sales_order_code"));
    m.insert("t_sales_order_logistics".into(), via("t_sales_order", "sales_order_code", "sales_order_code"));
    m.insert("t_after_sales_order_detail".into(), via("t_after_sales_order_header", "after_sales_code", "after_sales_code"));
    // —— global：Java 无 @DataScope（维表/字典/主数据/全局报表），1:1 全量可见
    for t in [
        "t_goods", "t_goods_category", "t_employee", "t_department", "t_employee_department",
        "t_dict_key", "t_dict_value", "t_warehouse", "t_warehouse_manage",
        "t_winc_stock_report", "t_winc_sale_report", "t_market_total_expense",
        "t_customer_price", "t_device_requisition", "t_master_shop",
    ] {
        m.insert(t.into(), TableRule::Global);
    }
    m
}

static BUILTIN: OnceLock<HashMap<String, TableRule>> = OnceLock::new();
static REGISTRY: OnceLock<HashMap<String, TableRule>> = OnceLock::new();

/// 查表权限档案：优先 PG 加载的注册表，回退内置种子（测试/CLI 无 PG 场景）
pub fn rule_of(table: &str) -> Option<TableRule> {
    let map = REGISTRY.get().unwrap_or_else(|| BUILTIN.get_or_init(builtin_rules));
    map.get(table).cloned()
}

/// 内置种子灌入 meta.scope_binding（upsert，代码为种子真相；管理员手工加的行保留）
pub async fn seed_rules(pg: &sqlx::PgPool) -> anyhow::Result<()> {
    for (t, rule) in builtin_rules() {
        let (mode, cc, oc, ok, vt, vl, vr) = match &rule {
            TableRule::Scoped(b) => (
                "scoped",
                b.customer_col.clone(),
                b.owner_col.clone(),
                Some(if b.owner_kind == OwnerKind::Ids { "ids" } else { "codes" }.to_string()),
                None, None, None,
            ),
            TableRule::Global => ("global", None, None, None, None, None, None),
            TableRule::Via { table, local_col, remote_col } => (
                "via", None, None, None,
                Some(table.clone()), Some(local_col.clone()), Some(remote_col.clone()),
            ),
        };
        sqlx::query(
            "INSERT INTO meta.scope_binding(table_name, mode, customer_col, owner_col, owner_kind, via_table, via_local_col, via_remote_col)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT (table_name) DO UPDATE SET mode=$2, customer_col=$3, owner_col=$4, owner_kind=$5, via_table=$6, via_local_col=$7, via_remote_col=$8",
        )
        .bind(&t).bind(mode).bind(cc).bind(oc).bind(ok).bind(vt).bind(vl).bind(vr)
        .execute(pg)
        .await?;
    }
    Ok(())
}

/// 从 meta.scope_binding 全量加载权限档案到进程注册表（服务启动调用一次）
pub async fn load_rules(pg: &sqlx::PgPool) -> anyhow::Result<usize> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT table_name, mode, customer_col, owner_col, owner_kind, via_table, via_local_col, via_remote_col FROM meta.scope_binding",
    )
    .fetch_all(pg)
    .await?;
    let mut m: HashMap<String, TableRule> = HashMap::new();
    for r in &rows {
        let t: String = r.get("table_name");
        let mode: String = r.get("mode");
        let rule = match mode.as_str() {
            "global" => TableRule::Global,
            "scoped" => {
                let kind = match r.get::<Option<String>, _>("owner_kind").as_deref() {
                    Some("codes") => OwnerKind::Codes,
                    _ => OwnerKind::Ids,
                };
                TableRule::Scoped(Binding {
                    customer_col: r.get("customer_col"),
                    owner_col: r.get("owner_col"),
                    owner_kind: kind,
                })
            }
            "via" => {
                let (Some(vt), Some(vl), Some(vr)) = (
                    r.get::<Option<String>, _>("via_table"),
                    r.get::<Option<String>, _>("via_local_col"),
                    r.get::<Option<String>, _>("via_remote_col"),
                ) else {
                    tracing::warn!("scope_binding {t} via 缺列，跳过（该表将 fail-closed 拒绝）");
                    continue;
                };
                TableRule::Via { table: vt, local_col: vl, remote_col: vr }
            }
            other => {
                tracing::warn!("scope_binding {t} 未知 mode={other}，跳过（该表将 fail-closed 拒绝）");
                continue;
            }
        };
        m.insert(t, rule);
    }
    let n = m.len();
    let _ = REGISTRY.set(m);
    Ok(n)
}

/// 把权限条件注入 SQL。集合全空（超管/ALL）原样返回。
/// 受限用户 SQL 涉及未登记表 → Err（fail-closed 拒绝，绝不静默放行）。
pub fn inject(sql: &str, sets: &ScopeSets) -> anyhow::Result<String> {
    if sets.is_unrestricted() {
        return Ok(sql.to_string());
    }
    let dialect = MySqlDialect {};
    let mut stmts = Parser::parse_sql(&dialect, sql)?;
    for stmt in &mut stmts {
        match stmt {
            Statement::Query(q) => inject_query(q, sets, &HashSet::new())?,
            _ => anyhow::bail!("只允许 SELECT 语句"),
        }
    }
    Ok(stmts
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; "))
}

fn inject_query(q: &mut Query, sets: &ScopeSets, outer_ctes: &HashSet<String>) -> anyhow::Result<()> {
    // CTE 也要注入；CTE 名是虚拟表，加入豁免集（可引用先声明的 CTE）
    let mut ctes = outer_ctes.clone();
    if let Some(with) = &mut q.with {
        for cte in &mut with.cte_tables {
            inject_query(&mut cte.query, sets, &ctes)?;
            ctes.insert(cte.alias.name.value.to_lowercase());
        }
    }
    inject_set_expr(&mut q.body, sets, &ctes)
}

fn inject_set_expr(body: &mut SetExpr, sets: &ScopeSets, ctes: &HashSet<String>) -> anyhow::Result<()> {
    match body {
        SetExpr::Select(sel) => inject_select(sel, sets, ctes),
        SetExpr::Query(q) => inject_query(q, sets, ctes),
        SetExpr::SetOperation { left, right, .. } => {
            inject_set_expr(left, sets, ctes)?;
            inject_set_expr(right, sets, ctes)
        }
        _ => Ok(()),
    }
}

fn inject_select(sel: &mut Select, sets: &ScopeSets, ctes: &HashSet<String>) -> anyhow::Result<()> {
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
        collect_table_conds(&mut twj.relation, sets, ctes, &present, &mut conds)?;
        for j in &mut twj.joins {
            collect_table_conds(&mut j.relation, sets, ctes, &present, &mut conds)?;
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
    Ok(())
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
    sets: &ScopeSets,
    ctes: &HashSet<String>,
    present: &HashSet<String>,
    out: &mut Vec<String>,
) -> anyhow::Result<()> {
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
            match rule_of(&table) {
                Some(TableRule::Scoped(binding)) => {
                    if let Some(c) = build_condition(&binding, &prefix, sets) {
                        out.push(c);
                    }
                }
                Some(TableRule::Global) => {}
                Some(TableRule::Via { table: vt, local_col, remote_col }) => {
                    // 头表同 SELECT 在场 → 由头表自身条件覆盖；独查 → EXISTS 借头表条件
                    if !present.contains(&vt) {
                        let Some(TableRule::Scoped(hb)) = rule_of(&vt) else {
                            anyhow::bail!("表 {table} 的 via 头表 {vt} 未登记 scoped 档案，fail-closed 拒绝");
                        };
                        if let Some(hc) = build_condition(&hb, "__ds_h", sets) {
                            out.push(format!(
                                "exists (select 1 from {vt} __ds_h where __ds_h.{remote_col} = {prefix}.{local_col} and {hc})"
                            ));
                        }
                    }
                }
                None => anyhow::bail!(
                    "表 {table} 未在权限档案登记（meta.scope_binding），已按 fail-closed 拒绝；请核实 Java @DataScope 口径后登记 scoped/global/via"
                ),
            }
        }
        // 子查询（派生表）内部递归注入
        TableFactor::Derived { subquery, .. } => inject_query(subquery, sets, ctes)?,
        TableFactor::NestedJoin { table_with_joins, .. } => {
            collect_table_conds(&mut table_with_joins.relation, sets, ctes, present, out)?;
            for j in &mut table_with_joins.joins {
                collect_table_conds(&mut j.relation, sets, ctes, present, out)?;
            }
        }
        _ => {}
    }
    Ok(())
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
            _ => {}
        }
    }
    if let Some(cc) = binding.customer_col.as_deref() {
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
    fn unregistered_table_rejected() {
        // fail-closed：受限用户查未登记表必须拒绝（防"忘登记即放行"）
        let s = sets(&[1], &[], &[]);
        let err = inject("SELECT * FROM t_role_data_scope", &s).unwrap_err();
        assert!(err.to_string().contains("未在权限档案登记"), "{err}");
        // 超管不受限
        let all = sets(&[], &[], &[]);
        assert!(inject("SELECT * FROM t_role_data_scope", &all).is_ok());
    }

    #[test]
    fn detail_standalone_gets_exists() {
        // 明细独查：借头表条件 EXISTS 半连接（堵标注遗漏的独查泄漏）
        let s = sets(&[7], &[], &["C1"]);
        let out = inject(
            "SELECT SUM(box_quantity) FROM t_sales_order_detail d WHERE d.item_type = '1'",
            &s,
        )
        .unwrap();
        let n = norm(&out);
        assert!(n.contains("exists(select1fromt_sales_order"), "{out}");
        assert!(n.contains("__ds_h.sales_order_code=d.sales_order_code"), "{out}");
        assert!(n.contains("__ds_h.owner_managerin(7)"), "{out}");
        assert!(n.contains("__ds_h.customer_codein('c1')"), "{out}");
    }

    #[test]
    fn detail_with_header_skips_exists() {
        // 头表同 SELECT 在场：条件由头表承担，明细不再加 EXISTS（避免双重注入拖慢）
        let s = sets(&[7], &[], &[]);
        let out = inject(
            "SELECT SUM(d.box_quantity) FROM t_sales_order_detail d JOIN t_sales_order so ON so.sales_order_code = d.sales_order_code",
            &s,
        )
        .unwrap();
        let n = norm(&out);
        assert!(n.contains("so.owner_managerin(7)"), "{out}");
        assert!(!n.contains("exists("), "{out}");
    }

    #[test]
    fn cte_name_exempt_and_inner_injected() {
        // CTE 名是虚拟表不得误拦；CTE 内部照常注入
        let s = sets(&[3], &[], &[]);
        let out = inject(
            "WITH x AS (SELECT owner_manager, total_amount FROM t_sales_order) SELECT SUM(total_amount) FROM x",
            &s,
        )
        .unwrap();
        let n = norm(&out);
        assert!(n.contains("owner_managerin(3)"), "{out}");
    }

    #[test]
    fn logistics_via_header() {
        // Java SalesOrderLogisticsMapper @DataScope 全部绑在 tso.* → 独查借头表
        let s = sets(&[], &[], &["C9"]);
        let out = inject("SELECT * FROM t_sales_order_logistics", &s).unwrap();
        let n = norm(&out);
        assert!(n.contains("exists(select1fromt_sales_order"), "{out}");
        assert!(n.contains("__ds_h.customer_codein('c9')"), "{out}");
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
