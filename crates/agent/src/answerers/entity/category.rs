//! 商品分类实体卡。闭集优先精确命中；只有用户显式写“商品分类/类型/品类”时才允许模糊找分类。

use dms_kernel::present::{Kpi, Semantic};

use super::{build_card, candidate_card, esc, fetch_rows, AskCtx, AskResult, Candidate, Kind};

/// 「精确行」的判定与 SQL 侧 `=`（ci 校对）同口径：trim + ASCII 大小写不敏感。
fn name_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

pub(super) async fn card(cx: &AskCtx<'_>, name: &str, explicit: bool) -> anyhow::Result<Option<AskResult>> {
    // 分类**经营指标**仍未进默认销售合同（事实内分类列未验收，fail-closed 不变）；
    // 分类主档（商品数/清单）与事实合同无关：生产读 `t_goods.goods_category_name`，
    // 数仓镜像该列为空，改读已验证的 `DW.dim_sku.class2`（与商品卡同一来源）。
    // `explicit` 由调用方透传（它手里就有 parsed.kind）—— 别再第三次完整 parse 问句。
    let warehouse = cx.source.is_warehouse();
    let n = esc(name);
    let found_sql = if warehouse {
        let pred = if explicit {
            format!("class2 LIKE '%{n}%'")
        } else {
            format!("class2 = '{n}'")
        };
        format!(
            "SELECT class2 AS `商品分类`, 'class2' AS `分类层级`, \
                    COUNT(DISTINCT sku_code) AS `商品数` \
             FROM DW.dim_sku WHERE class2 <> '' AND {pred} \
             GROUP BY class2 ORDER BY (class2 = '{n}') DESC, `商品数` DESC LIMIT 10"
        )
    } else {
        let pred = if explicit {
            format!("goods_category_name LIKE '%{n}%'")
        } else {
            format!("goods_category_name = '{n}'")
        };
        format!(
            "SELECT goods_category_name AS `商品分类`, 'goods_category_name' AS `分类层级`, \
                    COUNT(DISTINCT goods_code) AS `商品数` \
             FROM t_goods WHERE deleted_flag = 0 AND group_number = 'CHJZFL05-SYS' \
               AND goods_category_name <> '' AND {pred} \
             GROUP BY goods_category_name ORDER BY (goods_category_name = '{n}') DESC, `商品数` DESC LIMIT 10"
        )
    };
    let Some(found) = fetch_rows(cx, &found_sql).await? else { return Ok(None) };
    if found.rows.is_empty() {
        return Ok(None);
    }
    let exact_row = found.rows.iter().find(|row| {
        row.first()
            .and_then(|value| value.as_str())
            // SQL 侧 `=`（ci 校对）大小写不敏感，Rust 侧同口径：否则 SQL 认为精确命中的行
            // 在这里认不出，落进候选分支
            .is_some_and(|value| name_eq(value, name))
    });
    if exact_row.is_none() && found.rows.len() > 1 {
        let candidates = found
            .rows
            .iter()
            .filter_map(|row| {
                let name = row.first()?.as_str()?.trim().to_string();
                // 候选卡的「编码」列不塞字段名（'class2'/'goods_category_name' 是给引擎看的，
                // 不是给用户看的编码）—— 展示语义错位，给空串
                (!name.is_empty()).then(|| Candidate { kind: Kind::Category, code: String::new(), name })
            })
            .collect::<Vec<_>>();
        if candidates.len() > 1 {
            return Ok(Some(candidate_card(cx, name, candidates)));
        }
    }
    let selected = exact_row.unwrap_or(&found.rows[0]);
    let category = selected.first().and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
    if category.is_empty() {
        return Ok(None);
    }
    let goods_n = selected
        .get(2)
        .and_then(crate::answerers::hits::cell_num)
        .unwrap_or(0.0);
    // 同分类多行理论上去重过，但 dim 脏数据可重复 —— 去重；全空白名字不出坏建议
    let mut seen_others = std::collections::HashSet::new();
    let others: Vec<String> = found.rows
        .iter()
        .filter_map(|r| r.first()?.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty() && *v != category.as_str())
        .filter(|v| seen_others.insert(v.to_string()))
        .map(|v| format!("试试：商品分类{v}"))
        .collect();
    let c = esc(&category);
    let goods_sql = if warehouse {
        format!(
            "SELECT sku_name AS `商品`, sku_code AS `货号` FROM DW.dim_sku \
             WHERE class2 = '{c}' ORDER BY sku_name LIMIT 20"
        )
    } else {
        format!(
            "SELECT goods_name AS `商品`, goods_code AS `货号` FROM t_goods \
             WHERE deleted_flag = 0 AND group_number = 'CHJZFL05-SYS' \
               AND goods_category_name = '{c}' ORDER BY goods_name LIMIT 20"
        )
    };
    // 清单查询失败不丢整卡：分类名与商品数已在手，降级出无清单卡（空 RowSet 走正常拼装）
    let goods = fetch_rows(cx, &goods_sql)
        .await?
        .unwrap_or_else(|| dms_connector::source::RowSet { columns: vec![], rows: vec![], redacted: vec![], truncated: false });
    let items = vec![
        Kpi { label: "商品分类".into(), value: serde_json::Value::from(category.clone()), semantic: Semantic::Goods, delta: None },
        Kpi { label: "分类商品数".into(), value: serde_json::Value::from(goods_n), semantic: Semantic::Count, delta: None },
    ];
    let mut drill = others;
    drill.extend([
        format!("{category}今年各月销售额"),
        format!("{category}销售额按省区"),
        format!("买过{category}的客户有哪些"),
    ]);
    // 展示 SQL 两条都留（分类命中 + 商品清单 —— hits.rs 的「头查询；明细」同族）
    Ok(Some(build_card(
        &format!("{found_sql};\n\n-- 商品清单\n{goods_sql}"),
        items,
        goods,
        drill,
        cx,
    )))
}

#[cfg(test)]
mod tests {
    /// 「精确行」与 SQL 侧 `=`（ci 校对）同口径：trim + ASCII 大小写不敏感
    #[test]
    fn exact_row_matching_is_case_insensitive_like_mysql_ci() {
        assert!(super::name_eq(" 烤肠 ", "烤肠"));
        assert!(super::name_eq("SKU", "sku"));
        assert!(!super::name_eq("烤肠", "烤肠王"));
    }
}
