//! ValueCorrector：把 LLM 写在 SQL 里的**取值名**换成注册表登记的**码值**
//! （`status = '已完成'` → `status = '2'`）。命中判据是 (表, 列, 名字) 三元组。
//!
//! 逐行搬运自 `server/src/corrector.rs`（T8 第二批），只搬不改。

use std::collections::{HashMap, HashSet};

use core::ops::ControlFlow;
use sqlparser::ast::{Expr, VisitMut, VisitorMut};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use sqlx::PgPool;

use super::collect;

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

/// ValueLinker 纯核（可单测）：解析别名 → VisitMut 换码。
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
        ds_pred = crate::registry::ds_pred(2)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言用归一（与旧址 corrector.rs 的同名 helper 逐字相同）
    fn norm(s: &str) -> String {
        s.to_lowercase().replace(' ', "")
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
}
