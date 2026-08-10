# -*- coding: utf-8 -*-
"""闸 1 加口径词通道：注册指标同源放行 + 核心销售口径词映射度量列放行。"""
import io

p = 'crates/server/src/direct.rs'
s = io.open(p, encoding='utf-8', newline='').read()

# 1) 闸 1 函数重写
old = """fn derive_labels_ungrounded(
    shape: &DeriveShape,
    corpus: &[(String, Vec<(String, String)>)],
) -> Option<String> {
    for (label, tables) in &shape.labeled {
        let grounded = tables.iter().any(|table| {
            corpus
                .iter()
                .find(|(name, _)| name == table)
                .is_some_and(|(_, cols)| {
                    cols.iter().any(|(col, cmt)| {
                        let cmt = cmt.trim();
                        col.contains(label.as_str())
                            || (!cmt.is_empty()
                                && (cmt.contains(label.as_str()) || label.contains(cmt)))
                    })
                })
        });
        if !grounded {
            return Some(label.clone());
        }
    }
    None
}"""

new = """/// 核心销售口径词（用户裁决 2026-08-10：销售额/销量/成本/毛利/收入 允许从 ODS 度量列推导）。
/// 刻意不扩到「开票金额/专票金额」这类——它们在数仓里没有事实列，放行就是虚构（判官 E05/E08/E15）。
const CORE_SALES_METRIC_WORDS: &[&str] = &[
    "销售额", "销售金额", "销量", "销售数量", "毛利额", "毛利", "成本", "收入", "营收",
];

/// 度量列判定：列名或注释含度量词元（金额/数量/单价/成本/收入/毛利 或 amount/qty/price/cost/…）。
fn is_measure_col(col: &str, cmt: &str) -> bool {
    let c = col.to_lowercase();
    ["amount", "qty", "quantity", "price", "cost", "revenue", "profit"].iter().any(|w| c.contains(w))
        || ["金额", "数量", "单价", "成本", "收入", "毛利", "价格"].iter().any(|w| cmt.contains(w))
}

/// 闸 1 · 标签语义对账。三条出路（按序）：
/// ① 别名在取数表的列名/列注释里有出处（防虚构的基本面）；
/// ② 别名是注册指标且其登记源表就是取数表（`meta.metric` 的同源映射 —— 运营指标回自己的表）；
/// ③ 别名是核心销售口径词且取数表有度量列（合同覆盖外的 ODS 推导映射，结果标注未经合同验证）。
fn derive_labels_ungrounded(
    shape: &DeriveShape,
    corpus: &[(String, Vec<(String, String)>)],
    metrics: &[(String, String)],
) -> Option<String> {
    for (label, tables) in &shape.labeled {
        let grounded = tables.iter().any(|table| {
            corpus
                .iter()
                .find(|(name, _)| name == table)
                .is_some_and(|(_, cols)| {
                    cols.iter().any(|(col, cmt)| {
                        let cmt = cmt.trim();
                        col.contains(label.as_str())
                            || (!cmt.is_empty()
                                && (cmt.contains(label.as_str()) || label.contains(cmt)))
                    })
                })
        }) || metrics.iter().any(|(name, source)| {
            // ② 注册指标同源：源表可能带库名/UNION ALL，按裸表名包含判
            name == label && source.split(|c: char| c.is_whitespace() || c == '/')
                .any(|seg| seg.split('.').next_back() == Some(table.as_str()))
        }) || (CORE_SALES_METRIC_WORDS.contains(&label.as_str())
            && tables.iter().any(|table| {
                corpus
                    .iter()
                    .find(|(name, _)| name == table)
                    .is_some_and(|(_, cols)| cols.iter().any(|(col, cmt)| is_measure_col(col, cmt)))
            }));
        if !grounded {
            return Some(label.clone());
        }
    }
    None
}"""
assert s.count(old) == 1, 'gate1'
s = s.replace(old, new)

# 2) derive_attempt 签名与调用点加 metrics
old = """async fn derive_attempt(
    cx: &dms_agent::AskCtx<'_>,
    schema: &str,
    corpus: &[(String, Vec<(String, String)>)],
    usable: &[&str],
) -> DeriveTry {"""
new = """async fn derive_attempt(
    cx: &dms_agent::AskCtx<'_>,
    schema: &str,
    corpus: &[(String, Vec<(String, String)>)],
    metrics: &[(String, String)],
    usable: &[&str],
) -> DeriveTry {"""
assert s.count(old) == 1, 'attempt sig'
s = s.replace(old, new)

old = """    if let Some(label) = derive_labels_ungrounded(&shape, corpus) {"""
new = """    if let Some(label) = derive_labels_ungrounded(&shape, corpus, metrics) {"""
assert s.count(old) == 1, 'gate1 call'
s = s.replace(old, new)

# 3) ods_derive 壳：加载一次注册指标（ds 作用域），随两轮共用
old = """    // 空结果换一轮：候选表「有表无数据」（实测：客户 183507 在 t_winc_sale_report 零行、"""
new = """    // 注册指标清单（闸 1 的通道②语料）：name + source_table，ds 作用域。
    let metrics: Vec<(String, String)> = cx
        .owned
        .fixed("SELECT name, source_table FROM meta.metric WHERE ds_id IN ($1, '*') AND status='active'")
        .bind(cx.ds)
        .fetch_all::<(String, String)>()
        .await
        .unwrap_or_default();
    // 空结果换一轮：候选表「有表无数据」（实测：客户 183507 在 t_winc_sale_report 零行、"""
assert s.count(old) == 1, 'shell metrics'
s = s.replace(old, new)

old = """        match derive_attempt(cx, &schema, &corpus, &usable).await {"""
new = """        match derive_attempt(cx, &schema, &corpus, &metrics, &usable).await {"""
assert s.count(old) == 1, 'attempt call'
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('patched')
