//! 商品分类实体卡。闭集优先精确命中；只有用户显式写“商品分类/类型/品类”时才允许模糊找分类。

use dms_kernel::present::{Kpi, Semantic};

use super::{
    build_card, candidate_card, esc, fetch_rows, prefix_hint, AskCtx, AskResult, Candidate, Kind,
};

pub(super) async fn card(cx: &AskCtx<'_>, name: &str) -> anyhow::Result<Option<AskResult>> {
    // 分类**经营指标**仍未进默认销售合同（事实内分类列未验收，fail-closed 不变）；
    // 分类主档（商品数/清单）与事实合同无关：生产读 `t_goods.goods_category_name`，
    // 数仓镜像该列为空，改读已验证的 `DW.dim_sku.class2`（与商品卡同一来源）。
    let warehouse = cx.source.is_warehouse();
    let explicit = prefix_hint(cx.question) == Some(Kind::Category);
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
            .is_some_and(|value| value.trim() == name.trim())
    });
    if exact_row.is_none() && found.rows.len() > 1 {
        let candidates = found
            .rows
            .iter()
            .filter_map(|row| {
                let name = row.first()?.as_str()?.trim().to_string();
                (!name.is_empty()).then(|| Candidate {
                    kind: Kind::Category,
                    code: row.get(1).and_then(|value| value.as_str()).unwrap_or_default().to_string(),
                    name,
                })
            })
            .collect::<Vec<_>>();
        if candidates.len() > 1 {
            return Ok(Some(candidate_card(cx, name, candidates)));
        }
    }
    let selected = exact_row.unwrap_or(&found.rows[0]);
    let category = selected[0].as_str().unwrap_or_default().trim().to_string();
    if category.is_empty() {
        return Ok(None);
    }
    let goods_n = selected
        .get(2)
        .and_then(crate::answerers::hits::cell_num)
        .unwrap_or(0.0);
    let others: Vec<String> = found.rows
        .iter()
        .filter(|row| row.first().and_then(|value| value.as_str()).is_some_and(|value| value.trim() != category))
        .map(|r| format!("试试：商品分类{}", r[0].as_str().unwrap_or_default().trim()))
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
    let Some(goods) = fetch_rows(cx, &goods_sql).await? else { return Ok(None) };
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
    Ok(Some(build_card(
        &format!("{found_sql}; 商品分类总览卡"),
        &category,
        items,
        goods,
        drill,
        cx,
    )))
}
