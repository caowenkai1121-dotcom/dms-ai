//! 四种「命中即渲染」的卡片召回：维度口径卡 / 业务术语 / 取值编码提示 / 元素向量近邻卡。
//! 变更原因＝这四张卡的命中判据与渲染文案。
//!
//! 搬运源 `server/src/meta.rs:1169-1207`（`dim_hit` / `recall_dimensions`）、
//! `server/src/meta.rs:869-892`（`recall_terms`）、`server/src/meta.rs:1124-1164`
//! （`recall_value_hints`）、`server/src/meta.rs:722-760`（`recall_elements`）——
//! SQL 文本、命中判据、`map_filter` 净化位置、渲染文案逐字保留。
//!
//! 术语与码表的加载走 `registry::lexicon::{load_terms, load_value_maps}`（那两条 SQL 已在上游
//! 落地，此处**不再写第二份**：两份 SQL 漂移守卫都扫得到，但「单一事实源」会当场破）。
//! 维度与元素的投影与 `registry::model` 那份不同（卡片要 `description`），故各自保留。

use crate::recall::RecallCtx;
// 【A19】术语递归要调同族三路召回（指标在隔壁 `metric` 模块）
use super::metric::recall_metrics;
use crate::registry::{
    catalog_allows_column, catalog_allows_dimension, catalog_allows_metric_record,
    element_asset_live_pred_at, source_asset_live_pred_at, warehouse_qualified_source,
};
use crate::registry::lexicon::{
    load_domain_values, load_terms, load_value_domains, load_value_maps, longest_value_hit,
    ValueDomain, ValueMap,
};
use dms_kernel::nl::text::{map_filter, match_word};
use sqlx::PgPool;

/// 维度命中判定（问句含维度名或别名）。
/// allow：生产调用点在 T2 迁移时随 `recall_dimensions` 改走 `match_word` + `map_filter` 没了，
/// 但裁决 T7-3 明令保留它与它的搬运断言。
#[allow(dead_code)]
fn dim_hit(question: &str, name: &str, aliases: &[String]) -> bool {
    if question.contains(name) {
        return personnel_dimension_allowed(question, name, name);
    }
    aliases
        .iter()
        .find(|alias| question.contains(alias.as_str()))
        .map(|hit| personnel_dimension_allowed(question, name, hit))
        .unwrap_or(false)
}

/// 人员维度必须带明确业务事实语境；“区域经理业绩”不能自动变成 DWS 销售业务员维度。
fn personnel_dimension_allowed(question: &str, name: &str, hit_word: &str) -> bool {
    let is_personnel = ["业务员", "经理", "负责人", "人员"]
        .iter()
        .any(|word| name.contains(word) || hit_word.contains(word));
    !is_personnel
        || ["订单", "下单", "售后", "费用", "活动", "巡店", "促销"]
            .iter()
            .any(|context| question.contains(context))
}

fn ambiguous_sales_personnel(question: &str) -> bool {
    ["区域经理", "大区经理", "销售经理", "销售负责人"]
        .iter()
        .any(|word| question.contains(word))
        && !["订单", "下单", "售后", "费用", "活动", "巡店", "促销"]
            .iter()
            .any(|context| question.contains(context))
}

/// 召回命中的维度口径卡（问句含维度名或别名）→ 注入 prompt 让 LLM 按此分组取数
pub async fn recall_dimensions(pg: &PgPool, cx: &RecallCtx<'_>) -> anyhow::Result<Vec<String>> {
    if ambiguous_sales_personnel(cx.question) {
        return Ok(vec![
            "【人员维度需澄清】“区域经理/大区经理”不是已验证的 DWS 销售维度，禁止拆成“区域”+“经理”或默认映射为业务员。请确认要看订单大区经理、订单所属经理，还是按省区/战区分析销售。"
                .to_string(),
        ]);
    }
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        source_asset_live_pred_at("", 1)
    );
    let rows: Vec<(String, Vec<String>, String, String, String)> = sqlx::query_as::<
        _,
        (String, Vec<String>, String, String, String),
    >(&format!(
        // `ORDER BY` 不是洁癖：`map_filter` 的 R2 是「同名保留**首个**」，而 autodiscover
        // 把同一个列注释灌成了多条同名维度（实测「所属公司编码」11 条）。没有 ORDER BY 时
        // 「首个」＝PG 物理行序 → **同一个问句在不同部署上拿到不同的口径卡文本**。
        // 严格说这次只影响卡片里那句「来源 X」（那 11 条的 `expr` 实测完全相同，
        // count(DISTINCT expr)=1），不改变分组表达式；但本仓的纪律是「prompt 的字节就是行为」，
        // 不可复现的 prompt 本身就是缺陷。`source_table` 进排序键才能在同名时也定死。
        "SELECT name, aliases, source_table, expr, description
         FROM meta.dimension WHERE status = 'active'{ds_pred} ORDER BY name, source_table",
    ))
    .bind(cx.ds)
    .fetch_all(pg)
    .await?
    .into_iter()
    .filter(|(name, _, source_table, expr, _)| {
        catalog_allows_dimension(cx.ds, name, source_table, expr)
    })
    .collect();
    // 命中 + MapFilter 净化。维度表被 autodiscover 灌入过列注释原文（同名重复 10 条、
    // 名字带码值说明），不净化会重复注入同一张卡并淹没真正的维度口径。
    let matched: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, (name, aliases, ..))| {
            let hit = match_word(cx.question, name, aliases)?;
            personnel_dimension_allowed(cx.question, name, &hit).then_some((i, hit))
        })
        .collect();
    let pairs: Vec<(String, String)> =
        matched.iter().map(|(i, w)| (rows[*i].0.clone(), w.clone())).collect();
    Ok(map_filter(&pairs)
        .into_iter()
        .map(|k| {
            let (name, _a, src, expr, desc) = rows[matched[k].0].clone();
            format!(
                "【{name}】分组取值 {expr}，来源 {}。说明：{desc}",
                warehouse_qualified_source(cx.ds, &src)
            )
        })
        .collect())
}

/// 召回命中的业务术语（问句含术语名/别名）→ 注入 prompt DomainTerms 段
pub async fn recall_terms(pg: &PgPool, cx: &RecallCtx<'_>) -> anyhow::Result<Vec<String>> {
    let rows = load_terms(pg, cx.ds).await?;
    let matched: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, t)| match_word(cx.question, &t.term, &t.aliases).map(|w| (i, w)))
        .collect();
    let pairs: Vec<(String, String)> =
        matched.iter().map(|(i, w)| (rows[*i].term.clone(), w.clone())).collect();
    Ok(map_filter(&pairs)
        .into_iter()
        .map(|k| {
            let t = &rows[matched[k].0];
            format!("{} = {}", t.term, t.definition)
        })
        .collect())
}

/// 【A19】术语定义递归 mapping（SuperSonic `TermDescMapper`，**一层即止**）：
/// 命中术语后拿它的 `definition` 当新问句再跑一遍 指标/维度/值域 三路召回，
/// 与已有卡按卡名去重后只返回**增量**（问「复购率」命中术语，定义里的
/// 「客户/订单」再去召回真表 —— 否则术语的解释与可取的表之间断着一层）。
///
/// 两条防线（计划点名）：① 一层即止 —— mapping 的产出不再 mapping，递归没有第二条路；
/// ② 增量过名字去重 —— 「维度卡 78 行淹没真口径」（`gather.rs` 的账）与
/// 「同一批取值双渲」（下面 `value_hint_cards` 的账）都发生在没这道去重的时候。
pub async fn recall_term_mapped(
    pg: &PgPool,
    cx: &RecallCtx<'_>,
    existing: &[&[String]],
) -> anyhow::Result<Vec<String>> {
    let terms = load_terms(pg, cx.ds).await?;
    let mut out: Vec<String> = vec![];
    for t in &terms {
        if match_word(cx.question, &t.term, &t.aliases).is_none() {
            continue;
        }
        // 一层即止：definition 只当**召回问句**用，不再对它的结果递归
        let dc = RecallCtx {
            question: &t.definition,
            tables: &[],
            limit: 3,
            ds: cx.ds,
            embed: None,
            embed_slices: &[],
        };
        out.extend(recall_metrics(pg, &dc).await?);
        out.extend(recall_dimensions(pg, &dc).await?);
        out.extend(recall_value_domains(pg, &dc).await?);
    }
    Ok(dedup_new_cards(out, existing))
}

/// 卡名（`【名字】…` 前缀里的那一段）；不是这个形态的一律 None（不参与去重，漏判方向）
fn card_name(card: &str) -> Option<&str> {
    let s = card.strip_prefix('【')?;
    let end = s.find('】')?;
    Some(&s[..end])
}

/// 增量去重（**纯函数**）：卡名已出现在任何一张已有卡里的不再重复摆
/// （名字**精确**相等 —— 包含判据会把「销量占比」当成「销量」误删）。
fn dedup_new_cards(new: Vec<String>, existing: &[&[String]]) -> Vec<String> {
    let names: std::collections::HashSet<String> = existing
        .iter()
        .flat_map(|g| g.iter())
        .filter_map(|s| card_name(s).map(|n| n.to_string()))
        .collect();
    let mut seen = names;
    new.into_iter()
        .filter(|c| match card_name(c) {
            Some(n) => seen.insert(n.to_string()),
            None => true,
        })
        .collect()
}

/// 码值提示：问句里出现的中文值若是某编码列的码名 → 直接告诉 LLM 该列存码及对应码值。
/// ValueLinker（correct_value）只能在 LLM **已写出** `col='中文名'` 时换码；
/// 问「湖南省销售额」LLM 压根不知道 province 存的是 '430000'，实测直接漏掉省份过滤答成全量。
/// 这一层把「值→列→码」在生成前就摆给 LLM，是确定性的（不依赖向量召回）。
pub async fn recall_value_hints(pg: &PgPool, cx: &RecallCtx<'_>) -> anyhow::Result<Vec<String>> {
    let domains = load_value_domains(pg, cx.ds).await?;
    Ok(value_hint_cards(
        cx.ds,
        &load_value_maps(pg, cx.ds).await?,
        &domains,
        cx.question,
    ))
}

/// `recall_value_hints` 的纯判据部分（拆出来只为无库可断言）。
///
/// **名称型值域列在这里被排除**：那批取值的 `name = code`，编码提示会渲染成
/// 「『手抓饼』的编码是『手抓饼』」——纯噪声，且与值域命中卡对同一批取值出两张卡
/// （一张说「取值编码提示」一张说「过滤必须用这一列」），白占 prompt 还互相打架。
fn value_hint_cards(
    ds: &str,
    rows: &[ValueMap],
    domains: &[ValueDomain],
    question: &str,
) -> Vec<String> {
    let is_domain = |v: &ValueMap| {
        domains.iter().any(|d| {
            d.table_name.eq_ignore_ascii_case(&v.table_name)
                && d.column_name.eq_ignore_ascii_case(&v.column_name)
        })
    };
    let matched: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter(|(_, v)| !is_domain(v))
        .filter(|(_, v)| v.name.chars().count() >= 2 && question.contains(v.name.as_str()))
        .map(|(i, v)| (i, v.name.clone()))
        .collect();
    // 同名多列（如"货架店铺"既是 customer_class 又是 customer_type）全部保留——
    // 由 LLM 结合问句选列；MapFilter 仅做包含关系净化（"线下客户" 压过 "客户"）
    let pairs: Vec<(String, String)> = matched
        .iter()
        .map(|(i, w)| {
            let v = &rows[*i];
            (format!("{}.{}:{}", v.table_name, v.column_name, v.name), w.clone())
        })
        .collect();
    map_filter(&pairs)
        .into_iter()
        .map(|k| {
            let ValueMap { table_name: t, column_name: c, name, code, match_kind: kind } =
                &rows[matched[k].0];
            if kind == "like" {
                let table = warehouse_qualified_source(ds, t);
                format!("「{name}」在 {table}.{c} 列的码是 '{code}'，该列是逗号组合值，必须用 {c} LIKE '%{code}%'")
            } else {
                // 🔴 **不许把 LIKE 形态写进卡里**：反例会被照抄（SALE17 实测：卡里有
                // `LIKE '%湖南%'`，模型无视「必返 0 行」的警告把反例当答案抄了 ——
                // 「LLM 照抄的是示例」那本账的又一次）。只给正确写法，禁止形态一概不出现。
                let table = warehouse_qualified_source(ds, t);
                format!("「{name}」在 {table}.{c} 列存的是编码 '{code}'，过滤**只许**写 {c} = '{code}' —— 该列存码不存名，任何名称写法都必返 0 行")
            }
        })
        .collect()
}

/// 值域命中卡（纯函数）：告诉 LLM 这个词是**哪一列的取值**，过滤必须用那一列。
/// `note` 承担「误用哪一列会怎样」那半句（种子里带实测数字），故卡片本体只有骨架。
///
/// 🔴 `code`：命中名在码表里有**不同的码**时必须带上 —— 该列存码不存名
/// （SALE17 实测：province 被 autodiscover 登记成名称型值域，域卡只写「用这一列」，
/// 模型就写 `province LIKE '%湖南%'` —— 码列上一个字都匹不到，0 行）。
pub fn value_domain_card(d: &ValueDomain, hit: &str, code: Option<&str>) -> String {
    value_domain_card_for("", d, hit, code)
}

fn value_domain_card_for(ds: &str, d: &ValueDomain, hit: &str, code: Option<&str>) -> String {
    let trap = match code {
        // 🔴 同 `value_hint_cards` 那条：禁止形态（含 LIKE 的字符串）一概不进卡 —— 反例会被照抄
        Some(c) => format!(
            "该列存码不存名：过滤请写 {} = '{c}'，名称写法必返 0 行。",
            d.column_name
        ),
        None => String::new(),
    };
    format!(
        "【值域命中】「{hit}」是 {}.{} 的取值 → 过滤必须用这一列。{}{}",
        warehouse_qualified_source(ds, &d.table_name), d.column_name, trap, d.note
    )
}
/// 值域命中召回：问句命中某实体名值域列的取值 → 渲染卡片。
///
/// 取值自取（`load_domain_values`）：`meta.value_domain` 登记「哪一列是值域」，取值由
/// `meta autodiscover` 的名称型探针灌进 `meta.value_map`（name=code），重跑即自适应。
/// 未登记的列一律零卡片 —— 声明缺失绝不判、也绝不瞎渲染。
///
/// 命中名在 `meta.value_map` 的等值码表里有**不同的码**时，卡片必须带码
/// （province 那类「长得像名称列的码列」就靠这一句 —— 缺了它模型必写 LIKE/名称等值）。
pub async fn recall_value_domains(pg: &PgPool, cx: &RecallCtx<'_>) -> anyhow::Result<Vec<String>> {
    let domains = load_value_domains(pg, cx.ds).await?;
    let values = load_domain_values(pg, cx.ds).await?;
    let maps = load_value_maps(pg, cx.ds).await?;
    Ok(domains
        .iter()
        .filter_map(|d| {
            let same = |t: &String, c: &String| {
                t.eq_ignore_ascii_case(&d.table_name) && c.eq_ignore_ascii_case(&d.column_name)
            };
            let hit = longest_value_hit(
                cx.question,
                values.iter().filter(|(t, c, _)| same(t, c)).map(|(_, _, v)| v.as_str()),
            )?;
            // 命中名在码表里的码与它**不同** → 该列是码列，卡片必须带码
            let code = maps
                .iter()
                .find(|m| {
                    m.table_name.eq_ignore_ascii_case(&d.table_name)
                        && m.column_name.eq_ignore_ascii_case(&d.column_name)
                        && m.name == hit && m.code != hit
                })
                .map(|m| m.code.as_str());
            Some(value_domain_card_for(cx.ds, d, hit, code))
        })
        .collect())
}

/// 元素级向量召回（移植 SuperSonic SchemaMapper）：问句 embed → ANN 近邻元素。
/// 返回 (元素名, 渲染卡) 供 pipeline 与 substring 命中去重合并——口语化问法的语义双保险。
/// embed 服务缺席自动降级为空（`cx.embed == None`，熔断在 embed 客户端内）。
///
/// 【A8】切片向量：`embed_slices` 非空时按「任一片最近」取 MIN 距离（整句在首位）——
/// 整句向量被长问句稀释时，专名片段照样打得中。只有整句向量时包成单片走同一条路：
/// 查询恒为一条，「降级留痕」的判据（下面那条 `map_err + unwrap_or_default`）只数一处。
pub async fn recall_elements(pg: &PgPool, cx: &RecallCtx<'_>) -> Vec<(String, String)> {
    let slices: Vec<String> = if !cx.embed_slices.is_empty() {
        cx.embed_slices.to_vec()
    } else {
        match cx.embed {
            Some(lit) => vec![lit.to_string()],
            None => return vec![],
        }
    };
    let ds_pred = format!(
        "{}{}",
        // 必须带 `e.`：本查询 LEFT JOIN 了 metric/dimension 两张带 ds_id 的副表，
        // 裸 `ds_id` 在 PG 是 ambiguous（实测报错 → 元素卡整路静默缺席 → 组合器断粮）
        crate::registry::ds_pred_at("e", 3),
        element_asset_live_pred_at("e", 3)
    );
    type ElementRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        f64,
    );
    let rows: Vec<ElementRow> = sqlx::query_as::<_, ElementRow>(&format!(
        "SELECT e.element_id, e.kind, e.name,
                CASE
                  WHEN e.kind = 'metric' THEN COALESCE(m.agg_expr, e.ref_expr)
                  WHEN e.kind = 'dimension' THEN COALESCE(d.expr, e.ref_expr)
                  ELSE e.ref_expr
                END AS ref_expr,
                CASE
                  WHEN e.kind = 'metric' THEN COALESCE(m.source_table, '')
                  WHEN e.kind = 'dimension' THEN COALESCE(d.source_table, '')
                  WHEN e.kind = 'value' THEN regexp_replace(
                    split_part(e.element_id, ':', 2), '[.][^.]+$', ''
                  )
                  ELSE ''
                END AS source_table,
                COALESCE(m.scope_filter, ''), COALESCE(m.time_col, ''),
                COALESCE(m.dedup_keys, ''), COALESCE(m.description, ''),
                COALESCE(m.unit, ''), COALESCE(m.time_cap, ''), COALESCE(m.version, ''),
                (SELECT MIN(e.embedding <=> v.vec) FROM (SELECT unnest($1::text[])::vector AS vec) v) AS dist
         FROM meta.element e
         LEFT JOIN meta.metric m ON e.kind = 'metric' AND m.status = 'active'
           AND m.ds_id = e.ds_id AND e.element_id = 'metric:' || m.metric_code
         LEFT JOIN meta.dimension d ON e.kind = 'dimension' AND d.status = 'active'
           AND d.ds_id = e.ds_id AND e.element_id = 'dimension:' || d.dim_code
         WHERE e.status = 'active' AND e.embedding IS NOT NULL{ds_pred}
         ORDER BY dist LIMIT $2",
    ))
    .bind(slices)
    .bind(cx.limit as i64)
    .bind(cx.ds)
    .fetch_all(pg)
    .await
    // 🔴 **降级必须留痕**（本函数签名不返 Result，`?` 压根写不出来；就算能，让整轮问答
    // 因为少几张卡而失败也是过度反应 —— 裁决 二·G 同族）。这一处的静默遮的是
    // 「元素向量路是不是活的」：2026-07-28 查库 `meta.element` 1033 行 embedding **全 NULL**，
    // 这条 SQL 天天正常返 0 行，与「PG 抖了 / 谓词写错 / 列没了」在日志里完全无法区分。
    .map_err(|e| tracing::warn!(err = %e, "元素向量召回失败 → 元素卡缺席"))
    .unwrap_or_default()
    .into_iter()
    .filter(|(id, kind, name, ref_expr, source, scope, time_col, dedup, description, unit, time_cap, version, _)| match kind.as_str() {
        "metric" => catalog_allows_metric_record(
            cx.ds, name, source, ref_expr, scope, time_col, dedup, description, unit, time_cap,
            version,
        ),
        "dimension" => catalog_allows_dimension(cx.ds, name, source, ref_expr),
        "value" => id
            .split(':')
            .nth(1)
            .and_then(|target| target.rsplit_once('.'))
            .is_some_and(|(_, column)| catalog_allows_column(cx.ds, source, column)),
        _ => true,
    })
    .collect();
    // 【A7】两档阈值：严格档全空才放宽一档（SuperSonic「一个都没命中就把阈值折半」的
    // 对偶 —— 那边是降相似度要求，这边是放大可接受的余弦距离）。**不是全局调阈**：
    // 严格档有命中时宽松档一次都不看，噪声进不来；0.5 的天花板与选源阈值
    // `DS_MAX_DIST` 的实测距离表同源（真实命中 ≤0.43，错源 ≥0.56）—— 再宽就是噪声区。
    // 重召零额外往返：SQL 一次取回 `limit` 行，放宽只是换个阈值再滤一遍。
    const STRICT: f64 = 0.35; // 余弦距离阈值：语义相关才入（实测校准值）
    const LOOSE: f64 = 0.5; // A7：只在严格档全空时启用
    let render = |max_dist: f64| {
        rows
            .iter()
            .filter(|(_, _, _, _, _, _, _, _, _, _, _, _, dist)| *dist < max_dist)
            .map(|(id, kind, name, ref_expr, source, _, _, _, _, _, _, _, _)| {
                let source = warehouse_qualified_source(cx.ds, source);
                let card = match kind.as_str() {
                    "metric" => format!("【指标·{name}】= {ref_expr}，来源 {source}"),
                    "dimension" => {
                        format!("【维度·{name}】分组取值 {ref_expr}，来源 {source}")
                    }
                    "value" => format!("【码值·{name}】编码列码值（{id}，来源 {source}）"),
                    _ => format!("【术语·{name}】{ref_expr}"),
                };
                (name.clone(), card)
            })
            .collect::<Vec<_>>()
    };
    let strict = render(STRICT);
    if !strict.is_empty() {
        return strict;
    }
    let loose = render(LOOSE);
    if !loose.is_empty() {
        // 「靠放宽救回来」的频次是召回质量的调参依据：多 = 严格档定太死 / 向量该重灌
        tracing::info!(hits = loose.len(), "元素召回严格档全空 → 放宽一档命中");
    }
    loose
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【A19】卡名提取与增量去重：**名字精确相等**才删（包含判据会把「销量占比」
    /// 当成「销量」误删 —— 那是两个口径）；非【】形态的一律保留（漏判方向）。
    #[test]
    fn dedup_new_cards_drops_exact_name_duplicates_only() {
        let existing: Vec<String> = vec!["【销量】= SUM(qty)".into(), "【销售额】= SUM(amount)".into()];
        let new = vec![
            "【销量】= SUM(qty)".into(),            // 同名：删（已有卡里已有它）
            "【销量占比】= x/y".into(),             // 名字不同（不是子串误删）：留
            "【省份】分组取值 province".into(),      // 新卡：留
            "纯文本没有卡名".to_string(),           // 非【】形态：留（漏判方向）
        ];
        let out = dedup_new_cards(new, &[&existing]);
        assert_eq!(out, ["【销量占比】= x/y", "【省份】分组取值 province", "纯文本没有卡名"], "{out:?}");
        // new 内部互重也删（两个术语的定义召回到同一个指标）
        let dup = vec!["【销量】= a".to_string(), "【销量】= b".to_string()];
        assert_eq!(dedup_new_cards(dup, &[&[]]).len(), 1);
        // 卡名提取的形状
        assert_eq!(card_name("【退款占比】= x"), Some("退款占比"));
        assert_eq!(card_name("没有前缀"), None);
        assert_eq!(card_name("【不闭合"), None);
    }

    /// 值域卡必须把「用哪一列」与「别用哪一列」都摆出来：只说前半句 LLM 照旧写 sku_name LIKE
    #[test]
    fn value_domain_card_names_the_column_and_the_trap() {
        let d = ValueDomain {
            table_name: "t_goods_category".into(),
            column_name: "category_name".into(),
            note: "不要写 d.sku_name LIKE".into(),
        };
        let card = value_domain_card(&d, "手抓饼", None);
        assert!(card.contains("「手抓饼」是 t_goods_category.category_name 的取值"), "{card}");
        assert!(card.contains("不要写 d.sku_name LIKE"), "{card}");
        // 命中名在码表里有不同的码 → 卡片必须带码与「写名称必 0 行」的陷阱句
        let coded = value_domain_card(&d, "湖南", Some("430000"));
        assert!(coded.contains("category_name = '430000'") && coded.contains("必返 0 行"), "{coded}");
        // 🔴 禁止形态**永远不进卡**（反例会被照抄，SALE17 实测）：
        // 两类卡都不许出现 `LIKE '%<名称>%'` 的完整形态（「LIKE」一词本身留着是合法的）
        assert!(!coded.contains("'%湖南%'"), "反例形态进卡了：{coded}");
        let hint = value_hint_cards(
            "",
            &[ValueMap { table_name: "t_customer".into(), column_name: "province".into(),
                         name: "湖南".into(), code: "430000".into(), match_kind: "eq".into() }],
            &[],
            "本月湖南省的销售额是多少",
        );
        assert_eq!(hint.len(), 1, "{hint:?}");
        assert!(hint[0].contains("province = '430000'"), "{:?}", hint[0]);
        assert!(!hint[0].contains("'%湖南%'"), "反例形态进提示卡了：{:?}", hint[0]);
        // 名码相同的名称列（name=code）一个字的码都不多给
        assert!(!card.contains("存码不存名"), "{card}");
    }

    /// 🔴 名称型值域列不许再出「取值编码提示」卡：name=code 让它读成
    /// 「『手抓饼』的编码是『手抓饼』」，且与值域命中卡对同一批取值双渲。码型卡照旧要出。
    #[test]
    fn value_hints_exclude_name_domain_columns() {
        let vm = |t: &str, c: &str, n: &str, code: &str| ValueMap {
            table_name: t.into(),
            column_name: c.into(),
            name: n.into(),
            code: code.into(),
            match_kind: "eq".into(),
        };
        let rows = [
            vm("t_goods_category", "category_name", "手抓饼", "手抓饼"),
            vm("t_sales_order", "order_status", "已开票", "108"),
        ];
        let domains = [ValueDomain {
            table_name: "T_GOODS_CATEGORY".into(), // 大小写无关（meta.* 里两种写法都有）
            column_name: "Category_Name".into(),
            note: String::new(),
        }];
        let cards = value_hint_cards("", &rows, &domains, "手抓饼分类已开票的销量");
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert!(cards[0].contains("已开票") && cards[0].contains("'108'"), "{cards:?}");
        // 未登记为值域时那张卡照旧出（声明缺失不改行为）
        assert_eq!(value_hint_cards("", &rows, &[], "手抓饼分类已开票的销量").len(), 2);
    }

    #[test]
    fn dimension_hit_matching() {
        let aliases = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 名命中
        assert!(dim_hit("本月销售额按省份", "省份", &aliases(&["各省"])));
        // 无订单/售后/费用等事实语境时，区域经理不得自动解释成销售业务员维度
        assert!(!dim_hit("各区域经理业绩", "业务员", &aliases(&["经理", "负责人"])));
        assert!(dim_hit("订单额按业务员", "订单业务员", &aliases(&["业务员"])));
        assert!(ambiguous_sales_personnel("各区域经理业绩"));
        assert!(!ambiguous_sales_personnel("订单额按大区经理"));
        assert!(dim_hit("销售额按品类", "商品分类", &aliases(&["品类", "类别"])));
        // 未命中
        assert!(!dim_hit("本月销售额", "省份", &aliases(&["各省"])));
        assert!(!dim_hit("库存量", "门店", &aliases(&["店铺", "终端"])));
    }

    /// 🔴 元素向量召回读失败必须**留痕**：`meta.element` 1033 行 embedding 全 NULL，
    /// 这条 SQL 天天正常返 0 行 —— 静默降级把「读失败」和「本来没命中」压成同一种日志
    /// （都是没有日志）。形态与 `agent::gather::gather_warns_on_every_recall_degradation`
    /// 同族（条数相等）。无库单测覆盖不到这段 IO，故源码守。
    #[test]
    fn element_recall_degradation_is_logged() {
        let src = include_str!("cards.rs");
        // 本函数是文件里最后一个顶层项，**必须先切掉 `#[cfg(test)]`**：否则 body 会一路吃到
        // EOF 把本测试自己的源码算进去，而这段文本里就有 `.unwrap_or_default()` 与
        // `tracing::warn!` 两个字面量 → 计数判据当场变成看自己（第一版写法就是这个坑）。
        let body = src
            .split("pub async fn recall_elements(")
            .nth(1)
            .expect("函数改名了 —— 顺手把这条判据一起改")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap()
            .split("\n///")
            .next()
            .unwrap();
        // 锚点咬新投影（带 `e.` 别名：该查询 JOIN 了 metric/dimension 副表）。
        // 防恒真，两头都钉（同 schema.rs 那条：不拿字节长度当上限，中文注释必假红）。
        // 锚点故意用投影列而不是那句 FROM：`drift.rs` 的 ds 守卫按**源码行**扫「FROM + meta 点」，
        // 判据（连注释）里出现那个串就会把本测试自己当成一条漏了 ds 限定的召回 SQL —— 实测判红两次，
        // 第二次就是这行注释自己引起的。
        assert!(body.contains("e.element_id, e.kind, e.name,"), "切段没切住：{body}");
        assert!(!body.contains("mod tests"), "切过头了，吃进了测试模块：{body}");
        let degraded = body.matches(".unwrap_or_default()").count();
        assert_eq!(degraded, 1, "只数到 {degraded} 处降级 —— 元素向量那一路哪去了？");
        assert_eq!(
            degraded,
            body.matches("tracing::warn!").count(),
            "静默降级又回来了：{body}"
        );
    }

    /// 【A7】严格档全空才放宽一档 —— 钉住「两档 + 顺序」，防有人把阈值改回单档
    /// （那是**全局放宽**，噪声稀释 prompt 的实测账见整合计划 A7 节）。
    #[test]
    fn element_recall_loosens_only_when_strict_is_empty() {
        let src = include_str!("cards.rs");
        let body = src
            .split("pub async fn recall_elements(")
            .nth(1)
            .expect("函数改名了 —— 顺手把这条判据一起改")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap();
        assert!(body.contains("STRICT") && body.contains("LOOSE"), "两档阈值没了");
        let early = body.find("if !strict.is_empty()").expect("严格档早退没了");
        let loose = body.find("render(LOOSE)").expect("宽松档没了");
        assert!(early < loose, "宽松档跑在严格档前面了 —— 那就是全局放宽");
    }

    /// 【A8】切片向量：元素召回按「任一片最近」取 MIN 距离；只有整句时包成单片走同一条路。
    /// 钉的是查询形态，防「优化」回单向量（长问句稀释问题就回来了）。
    #[test]
    fn element_recall_takes_min_distance_over_slices() {
        let src = include_str!("cards.rs");
        let body = src
            .split("pub async fn recall_elements(")
            .nth(1)
            .expect("函数改名了 —— 顺手把这条判据一起改")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap();
        assert!(body.contains("unnest($1::text[])::vector"), "多向量 unnest 没了：{body}");
        assert!(body.contains("MIN(e.embedding <=>"), "MIN 距离没了：{body}");
        assert!(body.contains("embed_slices"), "切片入口没了：{body}");
    }
}
