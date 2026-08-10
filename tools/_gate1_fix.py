# -*- coding: utf-8 -*-
"""闸 1 修正：metrics 用 cx.pg、闭包按表判定、测试调用点补第三参。"""
import io

p = 'crates/server/src/direct.rs'
s = io.open(p, encoding='utf-8', newline='').read()

# 1) ods_derive 壳：owned → cx.pg（AskCtx 没有 owned 字段）
old = """    let metrics: Vec<(String, String)> = cx
        .owned
        .fixed("SELECT name, source_table FROM meta.metric WHERE ds_id IN ($1, '*') AND status='active'")
        .bind(cx.ds)
        .fetch_all::<(String, String)>()
        .await
        .unwrap_or_default();"""
new = """    let metrics: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, source_table FROM meta.metric WHERE ds_id IN ($1, '*') AND status='active'",
    )
    .bind(cx.ds)
    .fetch_all(cx.pg)
    .await
    .unwrap_or_default();"""
assert s.count(old) == 1, 'metrics load'
s = s.replace(old, new)

# 2) 闸 1 闭包：按表逐判（①注释 ②指标同源 ③核心口径词+度量列）
old = """    for (label, tables) in &shape.labeled {
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
new = """    for (label, tables) in &shape.labeled {
        let grounded = tables.iter().any(|table| {
            let cols_of = || {
                corpus
                    .iter()
                    .find(|(name, _)| name == table)
                    .map(|(_, cols)| cols.iter().collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            // ① 列名/列注释出处
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
}"""
assert s.count(old) == 1, 'gate body'
s = s.replace(old, new)

# 3) 测试调用点补第三参
n = s.count("derive_labels_ungrounded(&s, &")
s = s.replace("derive_labels_ungrounded(&s, &corpus)", "derive_labels_ungrounded(&s, &corpus, &[])")
s = s.replace("derive_labels_ungrounded(&s, &[])", "derive_labels_ungrounded(&s, &[], &[])")
n2 = s.count("derive_labels_ungrounded(&s, &corpus, &[])") + s.count("derive_labels_ungrounded(&s, &[], &[])")
print('call sites updated:', n, '->', n2)
io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('done')
