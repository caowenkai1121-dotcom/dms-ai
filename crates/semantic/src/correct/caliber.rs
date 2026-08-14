//! CaliberCorrector：把注册表声明的**口径过滤**补进 SQL（漏了它数值会虚高）。
//!
//! 逐行搬运自 `server/src/corrector.rs`（T8 第二批），只搬不改。
//!
//! 🔴 入口的 `OPT_OUT` 与 `correct::agg` 里那份是两份同构词表（各自语义不同，不能合并）。

use std::collections::HashSet;

use dms_kernel::sql::ast::collect_where_cols;
use dms_kernel::sql::lex::{first_ident_of, split_top_and};
use sqlparser::ast::Expr;
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use sqlx::PgPool;

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
    let cx = crate::recall::RecallCtx {
        question,
        tables: &[],
        limit: 0,
        ds,
        embed: None,
        embed_slices: &[],
    };
    let hits = crate::recall::recall_metric_hits(pg, &cx).await?;
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
    let scopes = crate::registry::model::load_table_scopes(pg, ds)
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── 口径过滤补全（correct_caliber 的纯函数核心）──
    const ORDER_SCOPE: &str = "deleted_flag = 0 AND order_status NOT IN ('0','108','199')";
    // ── K-3：JOIN 下的口径补全 ──
    const DETAIL_SCOPE: &str = "item_type = '1' AND deleted_flag = 0";

    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
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
}
