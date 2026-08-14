//! T8 搬运：逐行迁自 `server/src/direct.rs`（**只搬不改**，一个字节的行为改动都会让
//! `evaluation.py` 的逐题结果集对拍失去意义）。顺序即行为，只提取不重排。

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;

use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};

use crate::fastpath::*;
use crate::compose::*;
use crate::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use crate::{DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation};

// 同批搬来的兄弟模块（原文件里是同一个作用域，拆文件后要显式引）
#[allow(unused_imports)]
use crate::compose::{assemble::*, metric::*, path::*, values::*};
#[allow(unused_imports)]
use crate::fastpath::{finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*};

use crate::sales_fact;

/// 推导命中的 route 值。query_log 审计、前端徽标与可信凭证等级都认这一个字符串。
pub const DERIVE_ROUTE: &str = "direct-derive";


/// 推导候选表数上限：与 LLM 路径的表召回同一个 k。
pub const DERIVE_TOP_K: usize = 6;


/// 推导生成的温度：与 LLM 路径首轮同（0.1，确定性优先 —— 同一问句同一份候选该给同一条 SQL）。
pub const DERIVE_TEMP: f32 = 0.1;


/// 「不可计算」卡的唯一识别口径：销售维度/语义、开票、对账三张卡都是这个投影头
/// （与本文件既有测试断言同一个串）。
pub fn is_unavailable_card(hit: &DirectHit) -> bool {
    hit.sql.contains("'不可计算' AS `数据状态`")
}


/// 用表硬校验（纯函数）：SQL 引用的每张实表都必须落在候选集内（限定库名与目录库一致；
/// CTE 名不算实表，CTE 内部读的表照样校）。提示词里的「只用这些表」只是请求，这里才是闸 ——
/// LLM 写出候选集外的表 = 推导失败。AST 解析失败同样算越界：过不了解析的 SQL 留着也过不了
/// 闸门，早判早回落。
pub fn derive_tables_allowed(sql: &str, allowed: &[&str], d: &dyn dms_kernel::Dialect) -> bool {
    let Ok(refs) = dms_kernel::sql::ast::table_refs_of(sql, d) else {
        return false;
    };
    !refs.is_empty()
        && refs.iter().all(|parts| {
            let table = parts.last().map(String::as_str).unwrap_or_default();
            allowed.iter().any(|name| {
                let Some(asset) = crate::registry::warehouse_asset(name) else {
                    return false;
                };
                asset.table.eq_ignore_ascii_case(table)
                    && (parts.len() < 2
                        || parts[parts.len() - 2]
                            .eq_ignore_ascii_case(crate::warehouse_catalog::database_of(asset)))
            })
        })
}

// ── 两道语义闸（判官 E 系列裁决，2026-08-09）──
//
// 由来：derive 曾把 `t_sales_order_detail.amount`（明细金额）别名成「开票金额」（虚构指标，
// E05/E08/E15）、把 `created_by`（创建人）别名成「业务员」（码值劫走，E18）、用
// 置信度 0.35 的 joinable 边连 `t_winc_sale_report × t_goods`（未证实 JOIN 键，E09）。
// 两道闸都只作用于 direct-derive；直连合同路径不经过这里，一行未动。


/// derive SQL 的静态形状（sqlparser AST 只读遍历的产物；不改写 SQL、不参与执行）。
#[derive(Default)]
pub struct DeriveShape {
    /// (中文取数别名, 归属实表集合)。字面量投影（`'不可计算' AS 数据状态` 这类常数占位列）
    /// 与 ASCII 别名（列名形态）已剔除 —— 前者不取数，后者没有「改名」空间。
    pub labeled: Vec<(String, Vec<String>)>,
    /// JOIN ON 的跨表等值对：(左表集合, 左列, 右表集合, 右列)，已按本层别名图解析。
    /// 集合常态是单元素；派生子查询别名归到其子查询实表并集。
    pub join_pairs: Vec<(Vec<String>, String, Vec<String>, String)>,
    /// 没有跨表等值对的 JOIN 个数（USING/NATURAL/CROSS 或两端表解析不出 —— 没有可证的关联键）
    pub unevidenced_joins: usize,
    /// 时间桶别名（`DATE_FORMAT(stat_date,'%Y-%m') AS 月份` 这类）：时间词白名单 ∧ 表达式含
    /// 日期函数 才落这里。闸 1 跳过它们 —— 时间桶不可能虚构指标，但没这条豁免时
    /// 「各月/按周」类推导全被误判成「别名无出处」（实测：客户限定各月销售额被回落）。
    pub time_derived: Vec<String>,
}


/// 解析 derive SQL 的静态形状。`None` = AST 解析失败（调用方按推导失败回落原卡）。
pub fn analyze_derive_sql(sql: &str, d: &dyn dms_kernel::Dialect) -> Option<DeriveShape> {
    use sqlparser::ast::Statement;
    let stmts = sqlparser::parser::Parser::parse_sql(d.parser(), sql).ok()?;
    let mut shape = DeriveShape::default();
    for stmt in &stmts {
        if let Statement::Query(q) = stmt {
            analyze_query(q, &mut shape);
        }
    }
    Some(shape)
}


/// 递归分析一个查询；返回它（含子查询）读到的实表裸名集合。
pub fn analyze_query(q: &sqlparser::ast::Query, shape: &mut DeriveShape) -> Vec<String> {
    let mut tables = vec![];
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            tables.extend(analyze_query(&cte.query, shape));
        }
    }
    tables.extend(analyze_set_expr(&q.body, shape));
    tables.sort();
    tables.dedup();
    tables
}


pub fn analyze_set_expr(se: &sqlparser::ast::SetExpr, shape: &mut DeriveShape) -> Vec<String> {
    use sqlparser::ast::SetExpr;
    match se {
        SetExpr::Select(s) => analyze_select(s, shape),
        SetExpr::SetOperation { left, right, .. } => {
            let mut v = analyze_set_expr(left, shape);
            v.extend(analyze_set_expr(right, shape));
            v
        }
        SetExpr::Query(q) => analyze_query(q, shape),
        _ => vec![],
    }
}


/// 标识符归一：去反引号/双引号、小写。
pub fn ident_norm(value: &str) -> String {
    value.trim_matches(['`', '"']).to_lowercase()
}


/// 时间桶别名词表（精确匹配 —— 「月销售额」这种指标别名不在其列）。
pub const TIME_ALIAS_WORDS: &[&str] = &[
    "年", "年份", "月", "月份", "月度", "日", "日期", "天", "周", "周次", "星期", "周几", "季度", "小时", "时间",
];

/// 日期/时间函数白名单（MySQL/Doris 双方言常用集）。
pub const TIME_FNS: &[&str] = &[
    "DATE_FORMAT", "DATE_TRUNC", "YEAR", "MONTH", "QUARTER", "WEEK", "WEEKOFYEAR", "DAY",
    "DAYOFMONTH", "DAYOFWEEK", "HOUR", "MINUTE", "DATE", "LAST_DAY", "STR_TO_DATE",
    "FROM_UNIXTIME", "UNIX_TIMESTAMP", "TO_DAYS", "DATE_ADD", "DATE_SUB", "EXTRACT",
    "CURDATE", "CURRENT_DATE", "NOW",
];


/// 时间桶别名判定：别名是时间词 ∧ 表达式调用了日期函数。
/// 两个条件缺一不可 —— 光有时间词会把「指标改名」放过去，光有函数会把「给日期列起别名」卡掉。
pub fn is_time_bucket_alias(label: &str, expr: &sqlparser::ast::Expr) -> bool {
    TIME_ALIAS_WORDS.contains(&label) && expr_calls_time_fn(expr)
}


/// 表达式树里是否出现日期/时间函数调用（只读遍历）。
pub fn expr_calls_time_fn(e: &sqlparser::ast::Expr) -> bool {
    use sqlparser::ast::Expr;
    match e {
        Expr::Function(f) => {
            let name = f.name
                .0
                .last()
                .map(|p| p.value.to_uppercase())
                .unwrap_or_default();
            if TIME_FNS.contains(&name.as_str()) {
                return true;
            }
            if let sqlparser::ast::FunctionArguments::List(l) = &f.args {
                return l.args.iter().any(|a| match a {
                    sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(inner),
                    ) => expr_calls_time_fn(inner),
                    _ => false,
                });
            }
            false
        }
        Expr::BinaryOp { left, right, .. } => expr_calls_time_fn(left) || expr_calls_time_fn(right),
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::Cast { expr, .. } => {
            expr_calls_time_fn(expr)
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            operand.as_deref().map(expr_calls_time_fn).unwrap_or(false)
                || conditions.iter().any(expr_calls_time_fn)
                || results.iter().any(expr_calls_time_fn)
                || else_result.as_deref().map(expr_calls_time_fn).unwrap_or(false)
        }
        _ => false,
    }
}


/// 本层 FROM 的别名图：别名（小写）→ 实表集合。派生子查询先递归，
/// 其子查询实表并集就是派生别名的归属（`JOIN (SELECT ... FROM t) s` 的 `s` 归到 `t`）。
pub fn collect_from_factor(
    tf: &sqlparser::ast::TableFactor,
    shape: &mut DeriveShape,
    local: &mut std::collections::HashMap<String, Vec<String>>,
) {
    use sqlparser::ast::TableFactor;
    match tf {
        TableFactor::Table { name, alias, .. } => {
            let table = name.0.last().map(|p| ident_norm(&p.value)).unwrap_or_default();
            if table.is_empty() {
                return;
            }
            let key = alias
                .as_ref()
                .map(|a| ident_norm(&a.name.value))
                .unwrap_or_else(|| table.clone());
            local.entry(key).or_default().push(table);
        }
        TableFactor::Derived { subquery, alias, .. } => {
            let tables = analyze_query(subquery, shape);
            if let Some(a) = alias {
                local.insert(ident_norm(&a.name.value), tables);
            }
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            collect_from_factor(&table_with_joins.relation, shape, local);
            for j in &table_with_joins.joins {
                collect_from_factor(&j.relation, shape, local);
            }
        }
        _ => {}
    }
}


pub fn analyze_select(s: &sqlparser::ast::Select, shape: &mut DeriveShape) -> Vec<String> {
    use sqlparser::ast::SelectItem;
    let mut local: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for twj in &s.from {
        collect_from_factor(&twj.relation, shape, &mut local);
        for j in &twj.joins {
            collect_from_factor(&j.relation, shape, &mut local);
        }
    }
    let mut all_tables: Vec<String> = local.values().flatten().cloned().collect();
    all_tables.sort();
    all_tables.dedup();
    // ② 投影：中文取数别名 → 归属表集合（闸 1 的对账对象）
    for item in &s.projection {
        let SelectItem::ExprWithAlias { expr, alias } = item else { continue };
        let label = alias.value.trim_matches(['`', '"']).trim().to_string();
        if label.is_empty() || !label.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
            continue;
        }
        if is_literal_projection(expr) {
            continue; // 常数占位列不算取数别名
        }
        if is_time_bucket_alias(&label, expr) {
            shape.time_derived.push(label);
            continue; // 时间桶别名有独立豁免通道，不进闸 1 的对账清单
        }
        let mut tables: Vec<String> = vec![];
        let mut unresolved = false;
        for qualifier in expr_qualifier_refs(expr) {
            match local.get(&qualifier) {
                Some(ts) => tables.extend(ts.iter().cloned()),
                None => unresolved = true, // 外层/相关子查询引用：归属本层全部表
            }
        }
        // 无列引用（COUNT(*) 等）或有解析不出的限定符：归到本层全部表
        if tables.is_empty() || unresolved {
            tables = all_tables.clone();
        }
        tables.sort();
        tables.dedup();
        shape.labeled.push((label, tables));
    }
    // ③ JOIN ON 等值对（闸 2 的对账对象）
    for twj in &s.from {
        for j in &twj.joins {
            let mut pairs = vec![];
            if let Some(on) = join_on_expr(&j.join_operator) {
                collect_eq_pairs(on, &mut pairs);
            }
            let mut cross = 0;
            for (lq, lc, rq, rc) in pairs {
                let lt = local.get(&lq).cloned().unwrap_or_default();
                let rt = local.get(&rq).cloned().unwrap_or_default();
                if lt.is_empty() || rt.is_empty() {
                    continue; // 两端表解析不出 → 不算跨表键（下面按 cross==0 记无证据）
                }
                // 同表自连条件（两端同一实表）不是关联键
                if lt.iter().all(|t| rt.contains(t)) && rt.iter().all(|t| lt.contains(t)) {
                    continue;
                }
                shape.join_pairs.push((lt, lc, rt, rc));
                cross += 1;
            }
            if cross == 0 {
                shape.unevidenced_joins += 1;
            }
        }
    }
    all_tables
}


/// 常数占位列（`'不可计算' AS 数据状态` 这类纯字面量投影）不算取数别名。
pub fn is_literal_projection(e: &sqlparser::ast::Expr) -> bool {
    matches!(e, sqlparser::ast::Expr::Value(_))
}


/// 表达式里引用到的限定符（`d.amount` → `d`；`库.表.列` → `表`）。含子查询内部的。
pub fn expr_qualifier_refs(e: &sqlparser::ast::Expr) -> Vec<String> {
    use core::ops::ControlFlow;
    use sqlparser::ast::{Expr, Visit, Visitor};
    struct Qualifiers(Vec<String>);
    impl Visitor for Qualifiers {
        type Break = ();
        fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
            if let Expr::CompoundIdentifier(parts) = e {
                if parts.len() >= 2 {
                    self.0.push(ident_norm(&parts[parts.len() - 2].value));
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut v = Qualifiers(vec![]);
    let _ = e.visit(&mut v);
    v.0
}


/// JOIN 的 ON 条件（只认 Inner/Left/Right/Full 四类；USING/NATURAL/CROSS 返回 `None` ——
/// 没有可证的等值关联键，由调用侧记作无证据 JOIN）。
pub fn join_on_expr(op: &sqlparser::ast::JoinOperator) -> Option<&sqlparser::ast::Expr> {
    use sqlparser::ast::{JoinConstraint, JoinOperator};
    let constraint = match op {
        JoinOperator::Inner(c)
        | JoinOperator::LeftOuter(c)
        | JoinOperator::RightOuter(c)
        | JoinOperator::FullOuter(c) => c,
        _ => return None,
    };
    match constraint {
        JoinConstraint::On(e) => Some(e),
        _ => None,
    }
}


/// 收集 ON 条件里的 `限定符.列 = 限定符.列` 等值对（AND 合取与括号递归；
/// OR/函数包装里的等值不采信 —— 那不是干净的关联键）。
pub fn collect_eq_pairs(e: &sqlparser::ast::Expr, out: &mut Vec<(String, String, String, String)>) {
    use sqlparser::ast::{BinaryOperator, Expr};
    match e {
        Expr::Nested(inner) => collect_eq_pairs(inner, out),
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::And => {
                collect_eq_pairs(left, out);
                collect_eq_pairs(right, out);
            }
            BinaryOperator::Eq => {
                if let (Expr::CompoundIdentifier(l), Expr::CompoundIdentifier(r)) =
                    (left.as_ref(), right.as_ref())
                {
                    if l.len() >= 2 && r.len() >= 2 {
                        out.push((
                            ident_norm(&l[l.len() - 2].value),
                            ident_norm(&l[l.len() - 1].value),
                            ident_norm(&r[r.len() - 2].value),
                            ident_norm(&r[r.len() - 1].value),
                        ));
                    }
                }
            }
            _ => {}
        },
        _ => {}
    }
}


/// 闸 1 · 标签语义对账（E05/E08/E15/E18）：每个中文取数别名必须在其实际取数表的
/// 列名/列注释里有出处 —— 别名 ⊆ 列注释/列名，或列注释 ⊆ 别名（「销售额」⊂「销售额(元)」）。
/// 语料 = 候选表 schema 卡实际展示的列（与 LLM 所見逐字同源，不多查一遍库）。
/// `Some(别名)` = 第一个无出处的别名（虚构指标/码值劫走），调用方 warn 留痕后回落。
/// 核心销售口径词（用户裁决 2026-08-10：销售额/销量/成本/毛利/收入 允许从 ODS 度量列推导）。
/// 刻意不扩到「开票金额/专票金额」这类——它们在数仓里没有事实列，放行就是虚构（判官 E05/E08/E15）。
pub const CORE_SALES_METRIC_WORDS: &[&str] = &[
    "销售额", "销售金额", "销量", "销售数量", "毛利额", "毛利", "成本", "收入", "营收",
];


/// 度量列判定：列名或注释含度量词元（金额/数量/单价/成本/收入/毛利 或 amount/qty/price/cost/…）。
/// 知悉：`c.contains("cost")` 会把 `mat_costume`（服装）这类列误判成度量列 —— 通道③的
/// 放行面比注释写的宽。改成词元切分（`_` 分段判等）属闸语义改动，留待判官回归窗口再收。
pub fn is_measure_col(col: &str, cmt: &str) -> bool {
    let c = col.to_lowercase();
    ["amount", "qty", "quantity", "price", "cost", "revenue", "profit"].iter().any(|w| c.contains(w))
        || ["金额", "数量", "单价", "成本", "收入", "毛利", "价格"].iter().any(|w| cmt.contains(w))
}


/// 闸 1 · 标签语义对账。三条出路（按序）：
/// ① 别名在取数表的列名/列注释里有出处（防虚构的基本面）；
/// ② 别名是注册指标且其登记源表就是取数表（`meta.metric` 的同源映射 —— 运营指标回自己的表）；
/// ③ 别名是核心销售口径词且取数表有度量列（合同覆盖外的 ODS 推导映射，结果标注未经合同验证）。
pub fn derive_labels_ungrounded(
    shape: &DeriveShape,
    corpus: &[(String, Vec<(String, String)>)],
    metrics: &[(String, String)],
) -> Option<String> {
    for (label, tables) in &shape.labeled {
        let grounded = tables.iter().any(|table| {
            let cols_of = || {
                corpus
                    .iter()
                    .find(|(name, _)| name == table)
                    .map(|(_, cols)| cols.iter().collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            // ① 列名/列注释出处。`col.contains(label)` 今天恒 false（label 必含 CJK ——
            // 上面投影筛选保证 —— 而列名全 ASCII）：留着是防「未来出现 CJK 列名」的兜底。
            let by_comment = cols_of().iter().any(|(col, cmt)| {
                let cmt = cmt.trim();
                col.contains(label.as_str())
                    || (!cmt.is_empty() && (cmt.contains(label.as_str()) || label.contains(cmt)))
            });
            // ② 注册指标同源：源表可能带库名/UNION ALL，按裸表名判
            let by_metric = metrics.iter().any(|(name, source)| {
                name == label
                    && source
                        .split(|c: char| c.is_whitespace() || c == '/')
                        .any(|seg| seg.rsplit('.').next() == Some(table.as_str()))
            });
            // ③ 核心销售口径词 + 该表有度量列
            let by_core = CORE_SALES_METRIC_WORDS.contains(&label.as_str())
                && cols_of().iter().any(|(col, cmt)| is_measure_col(col, cmt));
            by_comment || by_metric || by_core
        });
        if !grounded {
            return Some(label.clone());
        }
    }
    None
}


/// 闸 2 · JOIN 证据闸（E09）：每条 JOIN 的每个跨表等值对都必须命中证据边
/// （取数侧已按「join_edge active 合同边 / datamap joinable 高置信或人工确认」过滤，
/// 这里只做双向匹配）。无等值关联键的 JOIN 直接算无证据。
/// `Some(描述)` = 第一条无证据的关联键，调用方 warn 留痕后回落。
pub fn derive_joins_unevidenced(
    shape: &DeriveShape,
    edges: &[crate::recall::JoinEvidenceRow],
) -> Option<String> {
    if shape.unevidenced_joins > 0 {
        return Some("存在无等值关联键的 JOIN（USING/NATURAL/CROSS 或两端表解析不出）".to_string());
    }
    for (lts, lc, rts, rc) in &shape.join_pairs {
        let hit = lts.iter().any(|lt| {
            rts.iter().any(|rt| {
                edges.iter().any(|e| {
                    let (el, er) = (bare_table(&e.left_table), bare_table(&e.right_table));
                    (el == *lt
                        && e.left_col.eq_ignore_ascii_case(lc)
                        && er == *rt
                        && e.right_col.eq_ignore_ascii_case(rc))
                        || (el == *rt
                            && e.left_col.eq_ignore_ascii_case(rc)
                            && er == *lt
                            && e.right_col.eq_ignore_ascii_case(lc))
                })
            })
        });
        if !hit {
            return Some(format!("{}.{} = {}.{}", lts.join("/"), lc, rts.join("/"), rc));
        }
    }
    None
}


/// 证据边表名归一：去库名限定、去引号、小写（join_edge 存裸名，datamap 可能存限定名）。
/// 先取最后一段再剥引号 —— 反过来的话 `` `db`.`tbl` `` 会先剥成 `` db`.`tbl ``、
/// 再切出 `` `tbl `` 这种带残留反引号的串，等值比较永不命中 = 证据全失效。
pub fn bare_table(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).trim_matches(['`', '"']).to_lowercase()
}


/// ODS 推导主流程。`Some` = 推导命中（route=direct-derive，经 `land` 过闸执行出答案）；
/// `None` = 推导不成，调用方把原「不可计算」卡原样返回。
/// 单轮推导的结果：命中（SQL）/ 空结果（试过的表，供剔除换轮）/ 失败（回落原卡）。
pub enum DeriveTry {
    Hit(String),
    Empty(Vec<String>),
    Failed,
}


/// 营销通/经销商上报专属表不进默认推导候选池：目录合同里的「禁止用本表推导」是写给 LLM
/// 看的文字，管不住表选择 —— 2026-08-11 实测「X客户本月销售额」被推导到 t_winc_sale_report，
/// 过滤一空就是单行全 NULL（同题不同答的根因之一）。用户点名 WinC/营销通/经销商上报/进销存
/// 时才放行；池被滤空 = 合同未覆盖语义不变，照旧回落原卡。纯函数，无库可单测。
pub fn derive_pool_winc_guard(pool: &mut Vec<&'static str>, question: &str) {
    const WINC_ONLY_TABLES: &[&str] = &[
        "t_winc_sale_report", "t_winc_stock_report", "t_winc_sale_transfer", "t_winc_stock_transfer",
    ];
    let winc_asked = ["winc", "WinC", "WINC", "营销通", "经销商上报", "进销存"]
        .iter()
        .any(|w| question.contains(w));
    if !winc_asked {
        pool.retain(|t| !WINC_ONLY_TABLES.contains(t));
    }
}


/// 剥掉指标词/时间词/通用虚词后的残留 = 候选客户名片段。至少两个汉字才值得探库。
pub fn customer_name_fragment(question: &str) -> Option<String> {
    let mut name = question.to_string();
    for (metric, _) in warehouse_sales_metrics(question) {
        name = name.replace(metric.name(), "");
        for alias in metric.aliases() {
            name = name.replace(alias, "");
        }
        // extras（「销售金额/收入/毛利」）也是同一个指标的说法：不剥的话片段带着指标词
        // （「恒众本月销售金额」剥出「恒众销售金额」），探库必空 = 漏接
        for extra in sales_fact_metric_extra_words(metric) {
            name = name.replace(extra, "");
        }
    }
    // 🔴 STRIP_WORDS 不许全局 replace：单字虚词（有/和/一/个…）是公司名肚子里的合法字
    // —— 「有」被剥掉，「…商贸有限公司」变成「…商贸限公司」，主档探库必空，整题跌进 ODS
    // 推导出单行 NULL（2026-08-11 实测「线下-潍坊程祥商贸有限公司本月销售额」）。名字在
    // 问句里是连续一段：只从两头剥虚词/标点，中间一个字都不动。
    let mut edge_words: Vec<&str> = dms_kernel::nl::lexicon::STRIP_WORDS.to_vec();
    // 「怎么样/如何」是纯语气尾词（answerable_tail_words 同一份），全局词表不收，边剥补上。
    edge_words.extend(["怎么样", "如何"]);
    edge_words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    let mut name = name.trim().to_string();
    loop {
        let before = name.clone();
        name = name
            .trim_matches(|c: char| c.is_whitespace() || RESIDUAL_PUNCT.contains(c))
            .to_string();
        for w in &edge_words {
            if let Some(rest) = name.strip_prefix(w) {
                name = rest.trim_start().to_string();
                break;
            }
            if let Some(rest) = name.strip_suffix(w) {
                name = rest.trim_end().to_string();
                break;
            }
        }
        // 渠道词（线下/线上）黏在实体名头尾时是**限定**不是名字，与虚词同轮边剥
        // （「…有限公司本月线下销售额」剥掉「线下」后「本月」才到边，必须同轮续剥——
        // 2026-08-12 生产实测归一重试两连不中）。护栏：剥完只剩渠道词本身时保留
        // （「本月线下销售额」的「线下」是渠道过滤本体）；带连字符的前缀（「线下-潍坊…」）
        // 是库内名称的一部分，不剥。
        for w in ["线下", "线上"] {
            // 剥后残余不许能被虚词表整个消化（「线下是多少」剥出「是多少」= 把渠道词本体剥没了）
            let junk_free_len = |s: &str| -> usize {
                let mut t = s.to_string();
                for ew in &edge_words {
                    t = t.replace(*ew, "");
                }
                t.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count()
            };
            if let Some(rest) = name.strip_suffix(w) {
                let rest = rest.trim_matches(|c: char| c.is_whitespace() || RESIDUAL_PUNCT.contains(c));
                if junk_free_len(rest) >= 2 {
                    name = rest.to_string();
                    break;
                }
            }
            if let Some(rest) = name.strip_prefix(w) {
                if !rest.starts_with('-') && !rest.starts_with('_') {
                    let rest = rest.trim_matches(|c: char| c.is_whitespace() || RESIDUAL_PUNCT.contains(c));
                    if junk_free_len(rest) >= 2 {
                        name = rest.to_string();
                        break;
                    }
                }
            }
        }
        if name == before {
            break;
        }
    }
    let name = name.as_str();
    let name = name.trim_matches(|c: char| c.is_whitespace() || RESIDUAL_PUNCT.contains(c));
    let hanzi = name.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
    if hanzi < 2 {
        return None;
    }
    // 类别/维度词不是名字：「线下客户」是客户分类（未验证维度），不是某个客户 —
    // 拿它去探主档会把分类问句错配成「名称含这两个字的客户」。
    const CLASS_WORDS: &[&str] = &["客户", "门店", "商品", "产品", "经销商", "分类", "类型", "类别", "省区", "省份", "战区"];
    if CLASS_WORDS.iter().any(|w| name.ends_with(w)) {
        return None;
    }
    // 领头的类别词同样不是名字：「客户董会琴」的「客户」是限定词，整词探库必空
    // （2026-08-11 实测漏接「线下-董会琴」）。剥完不足两个汉字 = 本来就是纯类别词，交回上面判 None。
    // 只剥客户系领头词：门店/商品领头的残词去探客户主档是跨域乱探，不剥。
    const CLASS_PREFIXES: &[&str] = &["客户", "经销商", "供应商"];
    let mut stripped = name;
    for prefix in CLASS_PREFIXES {
        if let Some(rest) = stripped.strip_prefix(prefix) {
            stripped = rest;
            break;
        }
    }
    if stripped.len() != name.len() {
        let rest_hanzi = stripped.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
        if rest_hanzi >= 2 {
            return Some(stripped.to_string());
        }
        return None;
    }
    Some(name.to_string())
}

