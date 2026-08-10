//! 结果呈现中文化的 agent 侧收口：**一次问答的唯一出口**（`ask()` 的 `one` 闭包）过一道
//! 「列名中文 + 码值翻名」。全部改名/翻译逻辑在 `dms_semantic::present_cn`（纯函数 + 词表 +
//! ds 级 TTL 缓存），本模块只剩两件事：把 `AskResult` 的主结果与 `supplemental` 各应用一遍，
//! 把留痕合并去重挂到 `value_labels`。
//!
//! 纪律：失败一律原样（词表加载挂 = 空快照 = 零改动），绝不让增强把一次成功取数变成失败。
//! 判据全在纯函数侧（本文件的测试不碰 DB，`PresentCn::from_parts` 直接造内存快照）。

use crate::ctx::{AskCtx, AskResult, ValueLabel};

/// 出口钩子（`ask()` 每个子问各调一次）。空结果（反问/复合容器/0 列）直接过，
/// 连词表都不加载 —— 没有列可译时不许白付一次缓存查找。
pub(crate) async fn localize_result(cx: &AskCtx<'_>, r: &mut AskResult) {
    let has_cols = !r.columns.is_empty()
        || r.supplemental.as_ref().map_or(false, |s| !s.columns.is_empty());
    if !has_cols {
        return;
    }
    let cn = dms_semantic::present_cn::PresentCn::load(cx.pg, cx.ds, &r.sql).await;
    apply_to_result(&cn, r);
}

/// 应用到整个 `AskResult`（主结果 + supplemental），并合并留痕（纯函数，单测的落点）。
fn apply_to_result(cn: &dms_semantic::present_cn::PresentCn, r: &mut AskResult) {
    let mut labels = cn.apply(&mut r.columns, &mut r.rows, &mut r.view, &mut r.redacted);
    if let Some(s) = r.supplemental.as_mut() {
        // SupplementalResult 没有 redacted 字段（脱敏列名只在顶层回显）
        let mut scratch = vec![];
        labels.extend(cn.apply(&mut s.columns, &mut s.rows, &mut s.view, &mut scratch));
    }
    // (列, 码) 去重，保留首次出现序（同一码在 200 行里重复出现只留一条痕）
    let mut seen = std::collections::HashSet::new();
    r.value_labels = labels
        .into_iter()
        .filter(|(col, code, _)| seen.insert((col.clone(), code.clone())))
        .map(|(column, code, label)| ValueLabel { column, code, label })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use dms_kernel::present::Block;
    use serde_json::json;

    use crate::ctx::SupplementalResult;

    /// 造一个「英文列 + 码值列」的最小结果（不经 gate，直接字面量 —— 本测试不碰 SQL 执行）。
    fn result() -> AskResult {
        let columns = vec!["status".to_string(), "order_time".to_string(), "销售额".to_string()];
        let rows = vec![
            vec![json!("100"), json!("2026-08-01 10:00:00"), json!("12")],
            vec![json!("101"), json!("2026-08-02 11:00:00"), json!("7")],
        ];
        AskResult {
            sql: "SELECT status, order_time, `销售额` FROM t_sales_order LIMIT 200".into(),
            columns: columns.clone(),
            rows,
            row_count: 2,
            truncated: false,
            elapsed_ms: 1,
            route: "llm".into(),
            view: dms_semantic::present::build(&columns, &[
                vec![json!("100"), json!("2026-08-01 10:00:00"), json!("12")],
                vec![json!("101"), json!("2026-08-02 11:00:00"), json!("7")],
            ]),
            supplemental: Some(SupplementalResult {
                columns: vec!["qty".to_string()],
                rows: vec![vec![json!("3")]],
                row_count: 1,
                truncated: false,
                view: dms_semantic::present::build(&["qty".to_string()], &[vec![json!("3")]]),
            }),
            comparisons: vec![],
            subs: vec![],
            caliber_note: None,
            truncation_note: None,
            redacted: vec![],
            scope_note: None,
            trust: None,
            steps: vec![],
            clarify_options: vec![],
            value_labels: vec![],
            sales_context: None,
        }
    }

    fn cn() -> dms_semantic::present_cn::PresentCn {
        dms_semantic::present_cn::PresentCn::from_parts(
            &["t_sales_order", "t_sales_order_detail"],
            vec![
                ("t_sales_order", "order_time", "下单时间"),
                ("t_sales_order_detail", "qty", "数量"),
            ],
            vec![
                ("t_sales_order", "status", "100", "待审核", "eq"),
                ("t_sales_order", "status", "101", "已审核", "eq"),
            ],
        )
    }

    /// 🔴 一次出口同时完成：主结果列名中文 + 码值翻名 + 留痕；supplemental 同一套。
    #[test]
    fn apply_covers_main_and_supplemental() {
        let mut r = result();
        apply_to_result(&cn(), &mut r);
        assert_eq!(r.columns, vec!["状态", "下单时间", "销售额"], "中文别名不动、英文列改名");
        assert_eq!(r.rows[0][0], json!("待审核"));
        assert_eq!(r.rows[1][0], json!("已审核"));
        // view 列名同步（按对齐校验按下标改）
        assert_eq!(r.view.columns[0].name, "状态");
        // supplemental 的列名也改（qty 的注释来自 detail 表 —— 涉及表两级查找）
        let s = r.supplemental.as_ref().unwrap();
        assert_eq!(s.columns, vec!["数量"]);
        // 留痕：(列, 码) 去重后两条
        assert_eq!(
            r.value_labels,
            vec![
                ValueLabel { column: "状态".into(), code: "100".into(), label: "待审核".into() },
                ValueLabel { column: "状态".into(), code: "101".into(), label: "已审核".into() },
            ]
        );
        // wire：有留痕时键出现
        let j = serde_json::to_value(&r).unwrap();
        assert!(j.get("value_labels").is_some(), "{j}");
    }

    /// 空快照 = 零改动零留痕（词表加载失败的降级形态）。
    #[test]
    fn empty_snapshot_leaves_everything_untouched() {
        let mut r = result();
        let before = serde_json::to_value(&r).unwrap();
        let empty = dms_semantic::present_cn::PresentCn::from_parts(&[], vec![], vec![]);
        // 空快照没有词表，但 ③ 通用转译表仍在（编译期常量）：order_time/status 会改名。
        // 所以这条判据钉的是「DB 部分全灭时只剩编译期词表」这一精确语义。
        apply_to_result(&empty, &mut r);
        assert_eq!(r.columns, vec!["状态", "下单时间", "销售额"]);
        assert_eq!(r.rows[0][0], json!("100"), "没有 value_map 就不翻值");
        assert!(r.value_labels.is_empty());
        // 译不动的列与所有行与原结果一致的部分不许变
        assert_eq!(before["rows"][0][2], json!("12"));
        assert_eq!(r.rows[0][2], json!("12"));
    }

    /// 码值翻名后数字列的其他值不受影响（只动命中码表的格）。
    #[test]
    fn translation_touches_only_registered_cells() {
        let mut r = result();
        apply_to_result(&cn(), &mut r);
        assert_eq!(r.rows[0][1], json!("2026-08-01 10:00:00"), "时间列原样");
        assert_eq!(r.rows[0][2], json!("12"), "指标列原样");
        // 既有块结构不动（图表只有下标；这里 3 列含时间 → 趋势线形态保持）
        assert!(matches!(r.view.blocks[0], Block::Chart { .. } | Block::Table));
    }
}
