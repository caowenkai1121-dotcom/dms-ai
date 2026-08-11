//! SchemaCorrector：执行前表/字段白名单校验（移植 SuperSonic SchemaCorrector.correctFieldName）。
//! LLM 生成的 SQL 里，表不在当前源或「真实表.列」不在 meta.column_doc 记录的真实列清单里，
//! 判为 schema 幻觉 → 携精确可用表/列清单自修一次（比执行报 1051/1054 更早）。
//! 只校验带表前缀且前缀映射到 meta 已知物理表的列——派生表/CTE 别名列、裸列、中文别名跳过，防误伤。

use std::collections::{HashMap, HashSet};

use core::ops::ControlFlow;
use sqlparser::ast::{Expr, VisitMut, VisitorMut};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use sqlx::PgPool;

// AST 遍历与词法已迁 `dms_kernel::sql::{ast,lex}`（逐行搬运）；`collect` 多一个 dialect 形参，
// 这里的薄包装把 DMS 侧的方言钉死并把 GuardError 转回 anyhow（文案不变）。
pub use dms_kernel::sql::ast::collect_where_cols;
pub use dms_kernel::sql::lex::{first_ident_of, split_top_and};

// ⚠️ ponytail: 【T9 留下的临时接线，消掉它的时机＝T8/T10】
// 下面的 `DmsCorrectors` 是本文件五个校正器给 `dms_agent::run` 的**入参形态**：
// `dms_agent` 引 server 是反向依赖边，所以「LLM 那一路调哪五个校正器」只能由 server 注入。
// T8/T10 把它们迁进 `dms_semantic::correct/*` 之后，这个 impl 与 `dms_agent::Correctors`
// 一起删掉，`run_llm` 直接调 semantic 的实现。**顺序即行为**：链的先后由 `run::correct_chain` 定，
// 本 impl 只做转发，一行判据都不许加在这里。
pub struct DmsCorrectors;

impl dms_agent::Correctors for DmsCorrectors {
    fn schema_check<'a>(
        &'a self,
        cx: &'a dms_agent::AskCtx<'a>,
        sql: &'a str,
    ) -> dms_agent::Fix<'a> {
        Box::pin(schema_check(cx.pg, cx.ds, sql))
    }

    fn fix_select_fields(&self, sql: &str) -> Option<String> {
        fix_select_fields(sql)
    }

    fn dedup_select_fields(&self, sql: &str) -> Option<String> {
        dedup_select_fields(sql)
    }

    fn fix_group_by(&self, sql: &str) -> Option<String> {
        fix_group_by(sql)
    }

    fn correct_agg<'a>(&'a self, cx: &'a dms_agent::AskCtx<'a>, sql: &'a str) -> dms_agent::Fix<'a> {
        Box::pin(correct_agg(cx.pg, cx.ds, cx.question, sql))
    }

    fn correct_caliber<'a>(
        &'a self,
        cx: &'a dms_agent::AskCtx<'a>,
        sql: &'a str,
    ) -> dms_agent::Fix<'a> {
        Box::pin(correct_caliber(cx.pg, cx.ds, cx.question, sql))
    }

    fn correct_value<'a>(
        &'a self,
        cx: &'a dms_agent::AskCtx<'a>,
        sql: &'a str,
    ) -> dms_agent::Fix<'a> {
        Box::pin(correct_value(cx.pg, cx.ds, sql))
    }

    fn fix_time_lower_bound(&self, sql: &str) -> Option<String> {
        fix_time_lower_bound(sql)
    }
}

/// 提取 (别名→表, 带前缀列引用)。纯函数，可单测。
fn collect(sql: &str) -> anyhow::Result<(HashMap<String, String>, Vec<(String, String)>)> {
    dms_kernel::sql::ast::collect(sql, &dms_kernel::MysqlDialect).map_err(anyhow::Error::from)
}

/// 自修提示里候选表清单的截断上限（提示是给模型看的，全量表名会淹没关键行）
const TABLE_HINT_CAP: usize = 20;

/// 执行前字段校验。返回 Some(自修提示) 表示发现幻觉列，None 表示通过。
pub async fn schema_check(pg: &PgPool, ds: &str, sql: &str) -> anyhow::Result<Option<String>> {
    let (amap, cols) = collect(sql)?;
    let real_tables: HashSet<String> = dms_kernel::sql::ast::table_names_of(
        sql,
        &dms_kernel::MysqlDialect,
    )?
    .into_iter()
    .collect();

    // 无实表（`SELECT 1` 这类）：后面两个分支必然走到 Ok(None)，不白跑 table_doc 查询
    if real_tables.is_empty() {
        return Ok(None);
    }

    // 先校验物理表。`table_doc` 为空表示该源尚未完成 schema 采集，保持原有 fail-open；
    // 一旦有当前源 schema，SQL 里的每张实表都必须在启用清单中。
    let known_tables: Vec<(String,)> = sqlx::query_as(&format!(
        "SELECT lower(table_name) FROM meta.table_doc WHERE enabled{ds_pred} ORDER BY table_name",
        ds_pred = dms_semantic::registry::ds_pred(1)
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    let known_tables: Vec<String> = known_tables.into_iter().map(|(t,)| t).collect();
    if !known_tables.is_empty() {
        // 两侧都已小写（known 来自 `SELECT lower(table_name)`，real_tables 出自 AST 收集），
        // 一次建集直接查，不再逐对 `eq_ignore_ascii_case` 双重扫描
        let known_set: HashSet<&str> = known_tables.iter().map(String::as_str).collect();
        let missing: Vec<&String> = real_tables
            .iter()
            .filter(|t| !known_set.contains(t.as_str()))
            .collect();
        if !missing.is_empty() {
            let mut hint = String::from(
                "SQL 引用了当前业务数据源不存在的表，请只使用真实表结构重写：\n",
            );
            for table in missing {
                hint.push_str(&format!("- 表 {table} 不存在或已停用。\n"));
            }
            let mut ranked: Vec<String> = known_tables
                .iter()
                .filter(|t| {
                    let hay = t.as_str();
                    real_tables.iter().any(|bad| {
                        bad.split('_')
                            .filter(|part| part.len() >= 4)
                            .any(|part| hay.contains(part))
                    })
                })
                .take(TABLE_HINT_CAP)
                .cloned()
                .collect();
            if ranked.is_empty() {
                ranked.extend(known_tables.iter().take(TABLE_HINT_CAP).cloned());
            }
            hint.push_str(&format!("当前源可用表候选：{}\n", ranked.join(", ")));
            return Ok(Some(hint));
        }
    }
    if cols.is_empty() {
        return Ok(None);
    }
    // 涉及的真实表 → 从 meta.column_doc 取真实列集合（只对 meta 已知表校验）
    // 【K6-D】ds 限定：列白名单是**每个源自己的**，拿 DMS 的列清单校别的库会把真列判成幻觉列
    // 【性能③】一次 `= ANY($1)` 取回全部涉及表（原来按表循环是 N+1 次往返），内存按表分组。
    // 谓词仍是 `lower(table_name)` 与逐表版逐字相同：行能返回 ⇔ 分组键等于某个 `real_tables`
    // 元素本身，所以按 `t` 查回分组与逐表版**逐个等价**（含「t 带大写则查不到」这个边角）。
    let q = format!(
        "SELECT lower(table_name), lower(column_name) FROM meta.column_doc WHERE lower(table_name) = ANY($1){ds_pred}",
        ds_pred = dms_semantic::registry::ds_pred(2)
    );
    let tables: Vec<String> = real_tables.iter().cloned().collect();
    let rows: Vec<(String, String)> =
        sqlx::query_as(&q).bind(&tables).bind(ds).fetch_all(pg).await?;
    let mut grouped: HashMap<String, HashSet<String>> = HashMap::new();
    for (t, c) in rows {
        grouped.entry(t).or_insert_with(HashSet::new).insert(c);
    }
    let mut table_cols: HashMap<String, HashSet<String>> = HashMap::new();
    for t in &real_tables {
        // remove 移动语义：`grouped` 之后不再使用，不整份克隆 HashSet
        if let Some(cols) = grouped.remove(t) {
            table_cols.insert(t.clone(), cols);
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
                if !known.contains(col) {
                    let pair = (table.clone(), col.clone());
                    if seen.insert(pair.clone()) {
                        bad.push(pair);
                    }
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
        // 别名表键本就小写：先按原值查（多数命中，零分配），不中再 lower 查一次
        let raw = &parts[parts.len() - 2].value;
        let table = match self.aliases.get(raw) {
            Some(t) => t,
            None => self.aliases.get(&raw.to_lowercase())?,
        };
        let col = parts[parts.len() - 1].value.to_lowercase();
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
                let lit_side_is_right = if matches!(left.as_ref(), Expr::CompoundIdentifier(_))
                    && matches!(right.as_ref(), Expr::Value(sqlparser::ast::Value::SingleQuotedString(_)))
                {
                    true
                } else if matches!(right.as_ref(), Expr::CompoundIdentifier(_))
                    && matches!(left.as_ref(), Expr::Value(sqlparser::ast::Value::SingleQuotedString(_)))
                {
                    false
                } else {
                    return ControlFlow::Continue(());
                };
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
    // 码表为空时换码必然无命中，不白解析一次
    if maps.is_empty() {
        return None;
    }
    let mut stmts = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
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
pub async fn correct_value(pg: &PgPool, ds: &str, sql: &str) -> anyhow::Result<Option<String>> {
    let (amap, _) = collect(sql)?;
    let tables: HashSet<String> = amap.values().cloned().collect();
    if tables.is_empty() {
        return Ok(None);
    }
    // 【K6-D】ds 限定：码表是每个源自己的（DMS 的 invoice_status=2 换到别的库就是错值）
    // 【性能③】一次 `= ANY($1)` 取回全部涉及表（原来按表循环是 N+1 次往返），内存按表分组。
    // 分组键用 `lower(table_name)`：逐表版的谓词是 `lower(table_name) = t`，行能返回 ⇔
    // 分组键与 `t` 逐字相等 —— 所以码表的 (表,列) 键与逐表版**逐个等价**，大小写边角同形。
    let q = format!(
        "SELECT lower(table_name), column_name, name, code, match_kind FROM meta.value_map WHERE lower(table_name) = ANY($1){ds_pred}",
        ds_pred = dms_semantic::registry::ds_pred(2)
    );
    let tables_vec: Vec<String> = tables.iter().cloned().collect();
    let rows: Vec<(String, String, String, String, String)> =
        sqlx::query_as(&q).bind(&tables_vec).bind(ds).fetch_all(pg).await?;
    let mut maps: ValueMaps = HashMap::new();
    for (t, col, name, code, kind) in rows {
        maps.entry((t, col.to_lowercase()))
            .or_insert_with(Vec::new)
            .push((name, code, kind));
    }
    Ok(link_values_with(sql, &amap, &maps))
}

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
        ds_pred = dms_semantic::registry::ds_pred(1)
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
    Ok(normalize_agg(sql, &rules))
}

/// 在 FROM/JOIN 链里定位指标来源表 → (补条件用的限定前缀, **该表**已被约束的列名)。
/// 恰好出现一次才返回 Some：0 次说明这条 SQL 与该指标无关，≥2 次是自连接（补给谁全靠猜）。
///
/// 原先这里是「FROM 恰一张表，有 JOIN 直接 return None」——而销量的来源表是明细表，
/// 任何真实问法都必须 JOIN 订单头拿时间与有效状态，于是校正器恰好在最需要它的查询上放弃
/// （评测 GOODS15 虚高 69%、SALE15 TOP10 错位都是这道门造成的）。
fn locate_target(
    sel: &sqlparser::ast::Select,
    source_table: &str,
) -> Option<(Option<String>, HashSet<String>)> {
    use sqlparser::ast::TableFactor;
    // 顶层 FROM/JOIN 链上的 (表名, 别名)。派生表/CTE 内部的同名表不登记——那不是本层能限定的东西
    let mut chain: Vec<(String, Option<String>)> = vec![];
    for twj in &sel.from {
        for r in std::iter::once(&twj.relation).chain(twj.joins.iter().map(|j| &j.relation)) {
            if let TableFactor::Table { name, alias, .. } = r {
                // 空表名实际不可达，但用 `?` 会把整函数提前返回 —— 显式跳过该项
                let Some(t) = name.0.last().map(|p| p.value.trim_matches('`').to_lowercase()) else {
                    continue;
                };
                chain.push((t, alias.as_ref().map(|a| a.name.value.trim_matches('`').to_string())));
            }
        }
    }
    let target = source_table.to_lowercase();
    let mut hits = chain.iter().filter(|(t, _)| *t == target);
    let alias = hits.next()?.1.clone();
    if hits.next().is_some() {
        return None; // 自连接：别名归属靠猜，不补
    }
    // 无别名时：多表链必须用表名限定（裸列在 MySQL 多表下是 1052 歧义），单表沿用裸列写法
    let prefix = alias.clone().or_else(|| (chain.len() > 1).then(|| target.clone()));
    // 「已约束」判定范围＝WHERE + 所有 JOIN ON：本仓 gold 常把口径写在 ON 上
    // （`JOIN t_sales_order o ON … AND o.deleted_flag = 0`），只看 WHERE 会重复补一遍。
    // 且只认 `目标别名.列`：JOIN 到的订单头 `o.deleted_flag` 不算明细表已约束，
    // 否则明细口径永远补不上（GOODS15 正是这个形态）。
    let mut frag: String = sel.from.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(" ");
    if let Some(w) = &sel.selection {
        frag.push(' ');
        frag.push_str(&w.to_string());
    }
    let refs = dms_kernel::sql::lex::base_col_refs(&frag, alias.as_deref().unwrap_or(&target));
    let mut present: HashSet<String> = refs.into_iter().collect();
    if chain.len() == 1 {
        // 单表：裸列（无前缀）写法也算已约束——既有行为。多表下裸列本身是歧义，不认
        if let Some(w) = &sel.selection {
            collect_where_cols(w, &mut present);
        }
    }
    Some((prefix, present))
}

/// scope_filter 含子查询的判定：按独立词元找 `SELECT`（大小写不敏感）。
/// 原来是 `to_uppercase().contains("SELECT")`：列名含 `selected` 之类会被误伤，
/// 整条口径静默不补 —— 子串不是词。
fn has_select_token(s: &str) -> bool {
    s.split(|c: char| !c.is_ascii_alphabetic())
        .any(|w| w.eq_ignore_ascii_case("select"))
}

/// 口径过滤补全（移植 SuperSonic 语义层「指标 filter 恒生效」）：问句命中指标、
/// SQL 里就有该指标来源表，却漏了注册表的 scope_filter 条件 → AND 补上。
/// 直击评测抓到的真缺陷：问「本月有多少个订单」LLM 漏 order_status 有效订单过滤，数字虚高 17%。
///
/// 保守门控（宁可不补也不误伤）：
/// - 反向问法（含"全部/所有状态/包括已取消/含作废"）整体跳过——用户明确要全量
/// - 仅顶层单 Select、无 WITH；scope_filter 含子查询（库存快照类）跳过
/// - source_table 在 FROM/JOIN 链里恰好出现 1 次（见 `locate_target`）
/// - 逐个原子条件比对：该表已含该列的任何条件就不补（用户可能在查特定状态）
///
/// 注意这是铁律 3 的唯一例外（保守补全）：JOIN 打开后，被 JOIN 进来当维表的指标来源表
/// 也会被补上它自己的口径——那正是「口径恒生效」的意思，但它确实会收窄结果集。
pub fn add_scope_filter(sql: &str, source_table: &str, scope_filter: &str) -> Option<String> {
    use sqlparser::ast::{Query, SetExpr, Statement};
    if scope_filter.trim().is_empty() || has_select_token(scope_filter) {
        return None;
    }
    let mut stmts = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
    if stmts.len() != 1 {
        return None;
    }
    let Statement::Query(q) = &mut stmts[0] else { return None };
    if q.with.is_some() {
        // 🔴 放弃**不许静默**：这条 SQL 里 `source_table` 恒需的 `scope_filter` 因此没被补上，
        // 而「口径没生效」的症状是数悄悄虚高，没有任何报错。三题诊断（wf_c921b918）
        // 列出的三处补留痕之一就是这里 —— 打不出这条 SQL 的形状时，连「为什么会漏」都查不出来。
        tracing::warn!(
            source_table,
            "带 WITH 的 SQL 跳过口径补全（scope_filter 未补）：{}",
            sql.chars().take(160).collect::<String>()
        );
        return None;
    }
    let Query { body, .. } = q.as_mut();
    let SetExpr::Select(sel) = body.as_mut() else { return None };
    let sel = sel.as_mut();
    let (prefix, present) = locate_target(sel, source_table)?;
    // 逐个原子条件（按顶层 AND 切）补齐缺失的
    let mut added: Vec<String> = vec![];
    for cond in split_top_and(scope_filter) {
        let Some(col) = first_ident_of(&cond) else { continue };
        if present.contains(&col) {
            continue;
        }
        let qualified = match &prefix {
            Some(p) => format!("{p}.{cond}"),
            None => cond.clone(),
        };
        added.push(qualified);
    }
    if added.is_empty() {
        return None;
    }
    let extra = added.join(" AND ");
    let expr = Parser::new(&MySqlDialect {})
        .try_with_sql(&extra)
        .and_then(|mut p| p.parse_expr())
        .ok()?;
    sel.selection = Some(match sel.selection.take() {
        Some(existing) => Expr::BinaryOp {
            left: Box::new(Expr::Nested(Box::new(existing))),
            op: sqlparser::ast::BinaryOperator::And,
            right: Box::new(expr),
        },
        None => expr,
    });
    Some(stmts[0].to_string())
}

/// 口径过滤补全的 DB 包装：命中的指标逐个尝试补
/// `ds` 不可省：指标口径（如「有效订单剔除 0/108/199」）是 DMS 的语义，
/// 补到别的源的 SQL 上就是拿一个库的口径改另一个库的查询。
/// 漂移守卫**抓不到这一条**（本函数不内联 SQL，没有 `FROM meta.` 字面量），只能靠签名带着 ds 走。
pub async fn correct_caliber(
    pg: &PgPool,
    ds: &str,
    question: &str,
    sql: &str,
) -> anyhow::Result<Option<String>> {
    // 反向问法：用户明确要全量/含无效状态 → 整体不补
    // ⚠️ 与 `correct_agg` 的 `OPT_OUT` 是两份同构词表，加词时两边都要看一眼。
    const OPT_OUT: &[&str] = &["全部状态", "所有状态", "包括已取消", "含已取消", "包含作废", "含作废", "不限状态"];
    if OPT_OUT.iter().any(|w| question.contains(w)) {
        return Ok(None);
    }
    // 指标命中只吃 `(ds, question)`；`tables`/`limit`/`embed` 三项本召回不读（形状见 `RecallCtx`）
    let cx = dms_semantic::recall::RecallCtx {
        question,
        tables: &[],
        limit: 0,
        ds,
        embed: None,
        embed_slices: &[],
    };
    let hits = dms_semantic::recall::recall_metric_hits(pg, &cx).await?;
    let mut cur = sql.to_string();
    let mut changed = false;
    // 每个命中都全量重 parse 一次（N 命中 = N 次解析）：命中数是个位数的指标量级、
    // SQL 是 KB 级，无害；「循环外解析一次、循环内累积改同一 AST」是结构性改动，真有成百命中再做。
    for m in &hits {
        if let Some(next) = add_scope_filter(&cur, base_table(&m.source_table), &m.scope_filter) {
            cur = next;
            changed = true;
        }
    }
    // 表级标准口径（`meta.table_scope`，SuperSonic 的 model filter）：与指标口径同等强制。
    //
    // 为什么必须在**这里**也补一遍：这些声明此前**只有校验器在读**
    // （`registry::caliber::build_rules` → `kernel::check_caliber`），补全器不读。
    // 于是「明细表漏 deleted_flag」这类完全能确定性补上的问题，只能靠回炉让 LLM 重写整条 SQL，
    // 而重写会连带改坏与违规无关的正确部分 —— 实测评测 GOODS17：判词只要 `deleted_flag`，
    // LLM 却把真实的分类 JOIN 换成 `LEFT(sku_name,2)` 编了一个「分类」（184616 vs 正确 141502），
    // 一条本来过的题被打红。**能确定性补的就不该回炉。**
    // 读失败降级成「本轮不补表级口径」而不是让整轮失败（补全器返错会打死整条问答，过度），
    // 但**必须吼一声** —— 照 `gather.rs::gather_all_cards` 的形态。此前是裸 `unwrap_or_default()`：
    // 校验器随后仍会判违规 → 回炉，于是表面看只是「慢一轮」，
    // 而日志里查不到「补全器为什么没补」，把一次 PG 抖动误诊成「口径声明没配」（裁决 二·AS4）。
    let scopes = dms_semantic::registry::model::load_table_scopes(pg, ds)
        .await
        .map_err(|e| tracing::warn!(err = %e, ds, "表级口径读失败 → 本轮不补表级过滤，交回炉兜"))
        .unwrap_or_default();
    for (table, filter) in &scopes {
        if let Some(next) = add_scope_filter(&cur, table, filter) {
            cur = next;
            changed = true;
        }
    }
    Ok(changed.then_some(cur))
}

/// 注册表的 `source_table` 带人类注解（`t_sales_order_detail(JOIN t_sales_order 有效订单)`、
/// `t_invoice_apply_header UNION ALL t_invoice_new_apply_header`），取首个标识符即基表。
///
/// 不能用 `strip_annotations`：那个函数刻意**保留半角括号**（否则会切坏 `COUNT(x)` 这类 SQL，
/// 有断言 `strip_annotations_keeps_sql_parens` 锁着）。
/// 此前这里直接把带注解的整串喂给 `locate_target`，与真实表名永不相等 ——
/// **指标级口径补全对销量类指标一直是死的**（`sales_qty` 的 `item_type='1'` 从未被补过）。
fn base_table(src: &str) -> &str {
    let end = src
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(src.len());
    &src[..end]
}

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
fn expr_key(e: &sqlparser::ast::Expr) -> String {
    e.to_string().replace('`', "").to_lowercase()
}

/// 投影项是可枚举的表达式（`*` 通配/qualified wildcard 不可枚举）
fn is_expr_item(i: &sqlparser::ast::SelectItem) -> bool {
    matches!(
        i,
        sqlparser::ast::SelectItem::UnnamedExpr(_) | sqlparser::ast::SelectItem::ExprWithAlias { .. }
    )
}

/// 顶层单 Select 的共用门控（三个校正器与 `fix_group_by` 同一条）：子查询/UNION/多语句
/// 一律跳过（防误伤方向），返回可变的 Select 引用给调用方继续判。
fn top_select<'s>(
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
fn expr_has_agg(e: &sqlparser::ast::Expr) -> bool {
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
fn last_ident(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(p) => Some(p.value.to_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.to_lowercase()),
        _ => None,
    }
}

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

    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
    }

    /// 🔴【性能③】两处按表取数必须是**一次 `= ANY($1)`**，逐表循环（N+1）不许回来。
    /// 无库单测覆盖不到这段 IO，照本仓既有形态（`gather.rs` 的接线判据）用源码守。
    /// 同时钉住【K6-D】：ds 限定不许在改造中丢掉（拿 DMS 的列/码校别的库就是错判）。
    #[test]
    fn schema_and_value_lookups_are_single_any_queries() {
        let src = include_str!("corrector.rs");
        let body = |marker: &str, tail: &str| {
            let s = src.split(marker).nth(1).expect("函数改名了 —— 顺手把这条判据一起改");
            let b = s.split("\n///").next().unwrap();
            assert!(b.contains(tail), "切段没切住：{b}");
            b
        };
        // schema_check：column_doc 一次 ANY 取回 + 内存分组
        //（断言用 contains 不用条数：函数体内的注释里也出现了同一字面量，数条数会恒红）
        let sc = body("pub async fn schema_check", "Ok(Some(hint))");
        assert!(sc.contains("= ANY($1)"), "列清单必须一次 ANY 取回：{sc}");
        assert!(!sc.contains(".bind(t)"), "逐表循环的 bind 回来了：{sc}");
        assert!(sc.contains("ds_pred(2)"), "K6-D 的 ds 限定丢了：{sc}");
        // correct_value：value_map 一次 ANY 取回 + 内存分组
        let cv = body("pub async fn correct_value", "link_values_with");
        assert!(cv.contains("= ANY($1)"), "码表必须一次 ANY 取回：{cv}");
        assert!(!cv.contains(".bind(t)"), "逐表循环的 bind 回来了：{cv}");
        assert!(cv.contains("ds_pred(2)"), "K6-D 的 ds 限定丢了：{cv}");
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
    fn adds_missing_group_by() {
        // 省份 + SUM(金额) 漏 GROUP BY → 补
        let out = fix_group_by("SELECT province, SUM(total_amount) FROM t_sales_order").unwrap();
        assert!(norm(&out).contains("groupbyprovince"), "{out}");
    }

    // ─────────────────────── 【A12】三个补缺校正器 ───────────────────────

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

    /// 子查询闸门按独立词元认 `SELECT`：列名含 select 子串（user_selected）不再被误伤
    #[test]
    fn caliber_select_gate_matches_word_token_not_substring() {
        let out = add_scope_filter("SELECT 1 FROM t_sales_order", "t_sales_order", "user_selected = 0")
            .expect("列名含 select 子串不该被当成子查询");
        assert!(out.to_lowercase().replace(' ', "").contains("user_selected=0"), "{out}");
        // 真子查询仍跳过（`caliber_skips_subquery_filter_and_empty` 守着）
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

    // collect 的三个断言已随算法搬去 `crates/kernel/tests/sql_ast.rs`（一字不改，只补 dialect 形参）。

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

    // ── 口径过滤补全（correct_caliber 的纯函数核心）──
    const ORDER_SCOPE: &str = "deleted_flag = 0 AND order_status NOT IN ('0','108','199')";

    #[test]
    fn caliber_adds_missing_status_filter() {
        // 评测抓获的真缺陷：LLM 只写 deleted_flag，漏有效订单状态过滤
        let sql = "SELECT COUNT(*) FROM t_sales_order WHERE deleted_flag = 0 AND order_time >= '2026-07-01'";
        let out = add_scope_filter(sql, "t_sales_order", ORDER_SCOPE).unwrap();
        let n = out.to_lowercase().replace(' ', "");
        assert!(n.contains("order_statusnotin('0','108','199')"), "{out}");
        // 已有的 deleted_flag 不重复补
        assert_eq!(n.matches("deleted_flag").count(), 1, "{out}");
    }

    #[test]
    fn caliber_no_change_when_complete() {
        let sql = "SELECT COUNT(*) FROM t_sales_order WHERE deleted_flag = 0 AND order_status NOT IN ('0','108','199')";
        assert!(add_scope_filter(sql, "t_sales_order", ORDER_SCOPE).is_none());
    }

    #[test]
    fn caliber_respects_user_status_filter() {
        // 用户显式查某状态 → 该列已出现，不覆盖用户意图
        let sql = "SELECT COUNT(*) FROM t_sales_order WHERE order_status = '104'";
        let out = add_scope_filter(sql, "t_sales_order", ORDER_SCOPE).unwrap();
        let n = out.to_lowercase().replace(' ', "");
        assert!(!n.contains("notin('0','108','199')"), "不得覆盖用户状态条件: {out}");
        assert!(n.contains("deleted_flag=0"), "缺失的 deleted_flag 仍要补: {out}");
    }

    #[test]
    fn caliber_qualifies_with_alias() {
        let sql = "SELECT COUNT(*) FROM t_sales_order o WHERE o.deleted_flag = 0";
        let out = add_scope_filter(sql, "t_sales_order", ORDER_SCOPE).unwrap();
        assert!(out.to_lowercase().replace(' ', "").contains("o.order_statusnotin"), "{out}");
    }

    #[test]
    fn caliber_skips_join_and_other_tables() {
        // 语义变更（K-3）：多表 JOIN 不再整体跳过——source_table 在 FROM/JOIN 链里唯一出现即按其别名补。
        // 原断言是 `.is_none()`，那道门恰好在最需要口径的 JOIN 查询上放弃（GOODS15 虚高 69% 的根因）。
        let out = add_scope_filter(
            "SELECT COUNT(*) FROM t_sales_order o JOIN t_customer c ON c.customer_code = o.customer_code WHERE o.deleted_flag = 0",
            "t_sales_order", ORDER_SCOPE).unwrap();
        assert!(out.to_lowercase().replace(' ', "").contains("o.order_statusnotin"), "{out}");
        // 主表非该指标来源表 → 不碰
        assert!(add_scope_filter("SELECT COUNT(*) FROM t_customer", "t_sales_order", ORDER_SCOPE).is_none());
    }

    // ── K-3：JOIN 下的口径补全 ──
    const DETAIL_SCOPE: &str = "item_type = '1' AND deleted_flag = 0";

    /// 🔴 注册表的 `source_table` 带人类注解，此前整串喂给 `locate_target` 与真实表名永不相等
    /// —— 指标级口径补全对销量类指标一直是死的（`item_type='1'` 从未被补过）。
    #[test]
    fn caliber_source_table_strips_registry_annotation() {
        assert_eq!(base_table("t_sales_order_detail(JOIN t_sales_order 有效订单)"), "t_sales_order_detail");
        assert_eq!(base_table("t_invoice_apply_header UNION ALL t_invoice_new_apply_header"), "t_invoice_apply_header");
        assert_eq!(base_table("t_after_sales_order_header / t_sales_order"), "t_after_sales_order_header");
        assert_eq!(base_table("t_sales_order"), "t_sales_order");
        // 剥完的名字必须真能定位到 FROM 链（否则等于没修）
        let sql = "SELECT SUM(d.box_quantity) FROM t_sales_order_detail d JOIN t_sales_order o ON o.sales_order_code = d.sales_order_code WHERE o.deleted_flag = 0";
        let out = add_scope_filter(sql, base_table("t_sales_order_detail(JOIN t_sales_order 有效订单)"), "item_type = '1'").unwrap();
        assert!(out.contains("d.item_type = '1'"), "{out}");
    }

    /// 问赠品（`item_type = '2'`）时不许把口径改成 `'1'`：该列已被约束就整条跳过。
    /// 评测 GOODS14「六月赠品箱数」靠这条守着 —— 值不比对是判据本身的一部分。
    #[test]
    fn caliber_does_not_override_user_chosen_code() {
        let sql = "SELECT SUM(box_quantity) FROM t_sales_order_detail WHERE item_type = '2'";
        assert!(add_scope_filter(sql, "t_sales_order_detail", "item_type = '1'").is_none());
        // 但同一声明里**没被约束**的那半条仍要补
        let out = add_scope_filter(sql, "t_sales_order_detail", "item_type = '1' AND deleted_flag = 0").unwrap();
        assert!(out.contains("deleted_flag = 0") && out.contains("item_type = '2'"), "{out}");
        assert!(!out.contains("item_type = '1'"), "{out}");
    }

    #[test]
    fn caliber_adds_to_joined_detail_table() {
        // GOODS15/SALE15 的真实形态：销量来源表是明细表，必须 JOIN 订单头拿时间与有效状态。
        // 头表的 o.deleted_flag 不算明细表已约束 → d.item_type 与 d.deleted_flag 都要补。
        let sql = "SELECT COUNT(DISTINCT d.sku_code) FROM t_sales_order_detail d \
                   JOIN t_sales_order o ON d.sales_order_code = o.sales_order_code \
                   WHERE o.deleted_flag = 0 AND o.order_time >= '2026-06-01'";
        let out = add_scope_filter(sql, "t_sales_order_detail", DETAIL_SCOPE).unwrap();
        let n = out.to_lowercase().replace(' ', "");
        assert!(n.contains("d.item_type='1'"), "{out}");
        assert!(n.contains("d.deleted_flag=0"), "明细表口径不能被头表同名列顶掉: {out}");
    }

    #[test]
    fn caliber_counts_on_clause_conditions() {
        // 口径写在 ON 上（本仓 gold 的常见写法）→ 已齐，不重复补
        let sql = "SELECT COUNT(*) FROM t_customer c JOIN t_sales_order o \
                   ON o.customer_code = c.customer_code AND o.deleted_flag = 0 \
                   AND o.order_status NOT IN ('0','108','199')";
        assert!(add_scope_filter(sql, "t_sales_order", ORDER_SCOPE).is_none());
    }

    #[test]
    fn caliber_skips_self_join_and_absent_table() {
        // 自连接：source_table 出现 2 次，补给谁靠猜 → 不补
        assert!(add_scope_filter(
            "SELECT COUNT(*) FROM t_sales_order a JOIN t_sales_order b ON a.parent_code = b.sales_order_code",
            "t_sales_order", ORDER_SCOPE).is_none());
        // source_table 不在 FROM/JOIN 链里 → 不补
        assert!(add_scope_filter(
            "SELECT COUNT(*) FROM t_customer c JOIN t_goods g ON g.customer_code = c.customer_code",
            "t_sales_order", ORDER_SCOPE).is_none());
        // 派生表内部的同名表不算「在链里」（那不是本层能限定的东西）
        assert!(add_scope_filter(
            "SELECT x.c FROM (SELECT COUNT(*) AS c FROM t_sales_order) x",
            "t_sales_order", ORDER_SCOPE).is_none());
    }

    #[test]
    fn caliber_qualifies_with_table_name_in_join() {
        // 多表链里目标表无别名：必须用表名限定，补裸列在 MySQL 多表下是 1052 歧义
        let sql = "SELECT COUNT(*) FROM t_sales_order JOIN t_customer c \
                   ON c.customer_code = t_sales_order.customer_code WHERE t_sales_order.deleted_flag = 0";
        let out = add_scope_filter(sql, "t_sales_order", ORDER_SCOPE).unwrap();
        let n = out.to_lowercase().replace(' ', "");
        assert!(n.contains("t_sales_order.order_statusnotin"), "{out}");
        assert_eq!(n.matches("deleted_flag").count(), 1, "已有的 deleted_flag 不重复补: {out}");
    }

    #[test]
    fn caliber_skips_subquery_filter_and_empty() {
        // 快照类 scope_filter 含子查询 → 跳过
        assert!(add_scope_filter(
            "SELECT SUM(stock_quantity) FROM t_winc_stock_report",
            "t_winc_stock_report",
            "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)").is_none());
        assert!(add_scope_filter("SELECT 1 FROM t_sales_order", "t_sales_order", "").is_none());
    }

    #[test]
    fn caliber_adds_where_when_absent() {
        let sql = "SELECT COUNT(*) FROM t_sales_order";
        let out = add_scope_filter(sql, "t_sales_order", ORDER_SCOPE).unwrap();
        let n = out.to_lowercase().replace(' ', "");
        assert!(n.contains("wheredeleted_flag=0andorder_statusnotin"), "{out}");
    }

    // split_top_and_basics 已随算法搬去 `kernel/src/sql/lex.rs`（断言一字不改）。

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
