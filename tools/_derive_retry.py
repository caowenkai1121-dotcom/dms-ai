# -*- coding: utf-8 -*-
"""ods_derive 两轮重试改造（空结果换候选表再来一轮）。"""
import io

p = 'crates/server/src/direct.rs'
s = io.open(p, encoding='utf-8', newline='').read()

old_head = '''async fn ods_derive(cx: &dms_agent::AskCtx<'_>) -> Option<DirectHit> {
    if !derive_eligible(cx) {
        return None;
    }
    let tables =
        dms_semantic::recall::ods_candidate_tables(cx.pg, cx.ds, cx.question, DERIVE_TOP_K).await;
    if tables.is_empty() {
        tracing::info!(question = %cx.question, "推导无候选 ODS 表 → 回落「不可计算」卡");
        return None;
    }
    // 仅候选表的 schema 卡：LLM 只看得到这些表（卡头即目录合同，粒度/时间/禁用规则随卡给出）。
    // 列语料与卡文本同一次取数 —— 闸 1 的「出处」语料就是 LLM 实际看见的列，一张都不多。
    let mut schema = String::from(
        "（推导口径：合同层未覆盖本问题，以下全部是 ODS 明细表，只允许用这些表推导；\\
         禁止引用任何 DWS/ADS 汇总表。结果会标注「未经合同验证」。）\\n",
    );
    let mut usable: Vec<&str> = vec![];
    let mut corpus: Vec<(String, Vec<(String, String)>)> = vec![];
    for table in &tables {
        match dms_semantic::recall::schema_card_with_columns(cx.pg, cx.ds, table).await {
            Ok(Some(card)) => {
                schema.push_str(&card.text);
                corpus.push(((*table).to_string(), card.columns));
                usable.push(table);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(err = %e, table = %table, "推导候选表 schema 卡读取失败，跳过该表"),
        }
    }
    if usable.is_empty() {
        return None;
    }
    let raw = derive_compose(cx, &schema).await?;'''

new_head = '''/// 单轮推导的结果：命中（SQL）/ 空结果（试过的表，供剔除换轮）/ 失败（回落原卡）。
enum DeriveTry {
    Hit(String),
    Empty(Vec<String>),
    Failed,
}

async fn ods_derive(cx: &dms_agent::AskCtx<'_>) -> Option<DirectHit> {
    if !derive_eligible(cx) {
        return None;
    }
    let pool =
        dms_semantic::recall::ods_candidate_tables(cx.pg, cx.ds, cx.question, DERIVE_TOP_K).await;
    if pool.is_empty() {
        tracing::info!(question = %cx.question, "推导无候选 ODS 表 → 回落「不可计算」卡");
        return None;
    }
    // 空结果换一轮：候选表「有表无数据」（实测：客户 183507 在 t_winc_sale_report 零行、
    // 在 t_sales_order 4950 行）不等于问题答不出。每轮把试过的表剔出候选池，最多两轮
    // （一轮直连 + 一轮换表，推导是降级路，成本到这里为止）。
    let mut tried: Vec<String> = vec![];
    for _ in 0..2 {
        let remaining: Vec<&String> = pool.iter().filter(|t| !tried.contains(t)).collect();
        if remaining.is_empty() {
            break;
        }
        // 仅候选表的 schema 卡：LLM 只看得到这些表（卡头即目录合同，粒度/时间/禁用规则随卡给出）。
        // 列语料与卡文本同一次取数 —— 闸 1 的「出处」语料就是 LLM 实际看见的列，一张都不多。
        let mut schema = String::from(
            "（推导口径：合同层未覆盖本问题，以下全部是 ODS 明细表，只允许用这些表推导；\\
             禁止引用任何 DWS/ADS 汇总表。结果会标注「未经合同验证」。）\\n",
        );
        let mut usable: Vec<&str> = vec![];
        let mut corpus: Vec<(String, Vec<(String, String)>)> = vec![];
        for table in &remaining {
            match dms_semantic::recall::schema_card_with_columns(cx.pg, cx.ds, table).await {
                Ok(Some(card)) => {
                    schema.push_str(&card.text);
                    corpus.push(((**table).to_string(), card.columns));
                    usable.push(table);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(err = %e, table = %table, "推导候选表 schema 卡读取失败，跳过该表"),
            }
        }
        if usable.is_empty() {
            return None;
        }
        match derive_attempt(cx, &schema, &corpus, &usable).await {
            DeriveTry::Hit(sql) => {
                tracing::info!(question = %cx.question, tables = ?usable, "ODS 推导命中（direct-derive）");
                return Some(DirectHit {
                    sql,
                    route: DERIVE_ROUTE.into(),
                    prev: None,
                    comparisons: vec![],
                    detail: None,
                });
            }
            DeriveTry::Empty(used) => {
                tracing::info!(question = %cx.question, tables = ?used, "推导 SQL 合法但零行，换候选表再来一轮");
                tried.extend(used);
            }
            DeriveTry::Failed => return None,
        }
    }
    None
}

/// 一轮推导尝试（组 SQL → 用表校验 → 双语义闸 → 闸门 → 预执行）。
async fn derive_attempt(
    cx: &dms_agent::AskCtx<'_>,
    schema: &str,
    corpus: &[(String, Vec<(String, String)>)],
    usable: &[&str],
) -> DeriveTry {
    let Some(raw) = derive_compose(cx, schema).await else {
        return DeriveTry::Failed;
    };'''

assert s.count(old_head) == 1, ('head', s.count(old_head))
s = s.replace(old_head, new_head)

old_tail = '''    // 目录限定名规范化（与组合器同一个出口）：LLM 写裸表名也补成 库.表
    let sql = dms_semantic::registry::warehouse_qualified_source(cx.ds, &raw);
    if !derive_tables_allowed(&sql, &usable, cx.source.dialect()) {
        tracing::warn!(question = %cx.question, sql = %sql, "推导 SQL 用表越出候选集 → 回落「不可计算」卡");
        return None;
    }
    // 两道语义闸（判官 E 系列裁决）：只作用于 direct-derive，直连合同路径不经过这里。
    let Some(shape) = analyze_derive_sql(&sql, cx.source.dialect()) else {
        tracing::warn!(question = %cx.question, sql = %sql, "推导 SQL 解析失败 → 回落「不可计算」卡");
        return None;
    };
    // 闸 1 · 标签语义对账：中文取数别名必须在取数表的列名/列注释里有出处
    if let Some(label) = derive_labels_ungrounded(&shape, &corpus) {
        tracing::warn!(question = %cx.question, alias = %label, sql = %sql,
            "推导别名在取数表列名/列注释里无出处（虚构指标/码值劫走）→ 回落「不可计算」卡");
        return None;
    }
    // 闸 2 · JOIN 证据闸：每条跨表等值关联键都要命中合同边或高置信/人工确认的 joinable 边
    if !shape.join_pairs.is_empty() || shape.unevidenced_joins > 0 {
        let edges = dms_semantic::recall::join_evidence_edges(cx.pg, cx.ds, &usable).await;
        if let Some(join) = derive_joins_unevidenced(&shape, &edges) {
            tracing::warn!(question = %cx.question, join = %join, sql = %sql,
                "推导 JOIN 关联键无证据 → 回落「不可计算」卡");
            return None;
        }
    }
    let candidate = dms_agent::ensure_limit(&sql, cx.source.dialect());
    // 与直连完全同一个闸门：check（只读红线/敏感列/LIMIT）→ 行级权限注入。
    // 红线拒（GuardError）与权限拒（PolicyError，如候选表对受限身份不可证）都回落原卡 ——
    // 回落目标是 fail-closed 占位卡本身，不放大任何可见面。
    let scoped = match dms_agent::gate_on(cx.p, &candidate, cx.scope, cx.ds_global, cx.source.dialect()) {
        Ok(scoped) => scoped,
        Err(e) => {
            tracing::warn!(err = %e, question = %cx.question, "推导 SQL 未过闸门 → 回落「不可计算」卡");
            return None;
        }
    };
    // 预执行一次（行上限/超时与直连相同）：执行失败（列漂移/超时）必须回落原卡，
    // 而不是把失败交给 `land` 跌进后面的 LLM 全目录路径。
    // 代价是命中时同一条 SQL 在 `land` 再执行一次 —— 推导是降级路，
    // 这笔重复执行换的是「回落语义不漂」。
    match cx.source.fetch(&scoped, dms_agent::MAX_ROWS, dms_agent::EXEC_TIMEOUT).await {
        Ok(_) => {
            tracing::info!(question = %cx.question, tables = ?usable, "ODS 推导命中（direct-derive）");
            Some(DirectHit {
                sql: candidate,
                route: DERIVE_ROUTE.into(),
                prev: None,
                comparisons: vec![],
                detail: None,
            })
        }
        Err(e) => {
            tracing::warn!(err = %e, question = %cx.question, "推导 SQL 执行失败 → 回落「不可计算」卡");
            None
        }
    }
}'''

new_tail = '''    // 目录限定名规范化（与组合器同一个出口）：LLM 写裸表名也补成 库.表
    let sql = dms_semantic::registry::warehouse_qualified_source(cx.ds, &raw);
    if !derive_tables_allowed(&sql, usable, cx.source.dialect()) {
        tracing::warn!(question = %cx.question, sql = %sql, "推导 SQL 用表越出候选集 → 回落「不可计算」卡");
        return DeriveTry::Failed;
    }
    // 两道语义闸（判官 E 系列裁决）：只作用于 direct-derive，直连合同路径不经过这里。
    let Some(shape) = analyze_derive_sql(&sql, cx.source.dialect()) else {
        tracing::warn!(question = %cx.question, sql = %sql, "推导 SQL 解析失败 → 回落「不可计算」卡");
        return DeriveTry::Failed;
    };
    // 闸 1 · 标签语义对账：中文取数别名必须在取数表的列名/列注释里有出处
    if let Some(label) = derive_labels_ungrounded(&shape, corpus) {
        tracing::warn!(question = %cx.question, alias = %label, sql = %sql,
            "推导别名在取数表列名/列注释里无出处（虚构指标/码值劫走）→ 回落「不可计算」卡");
        return DeriveTry::Failed;
    }
    // 闸 2 · JOIN 证据闸：每条跨表等值关联键都要命中合同边或高置信/人工确认的 joinable 边
    if !shape.join_pairs.is_empty() || shape.unevidenced_joins > 0 {
        let edges = dms_semantic::recall::join_evidence_edges(cx.pg, cx.ds, usable).await;
        if let Some(join) = derive_joins_unevidenced(&shape, &edges) {
            tracing::warn!(question = %cx.question, join = %join, sql = %sql,
                "推导 JOIN 关联键无证据 → 回落「不可计算」卡");
            return DeriveTry::Failed;
        }
    }
    let candidate = dms_agent::ensure_limit(&sql, cx.source.dialect());
    // 与直连完全同一个闸门：check（只读红线/敏感列/LIMIT）→ 行级权限注入。
    // 红线拒（GuardError）与权限拒（PolicyError，如候选表对受限身份不可证）都回落原卡 ——
    // 回落目标是 fail-closed 占位卡本身，不放大任何可见面。
    let scoped = match dms_agent::gate_on(cx.p, &candidate, cx.scope, cx.ds_global, cx.source.dialect()) {
        Ok(scoped) => scoped,
        Err(e) => {
            tracing::warn!(err = %e, question = %cx.question, "推导 SQL 未过闸门 → 回落「不可计算」卡");
            return DeriveTry::Failed;
        }
    };
    // 预执行一次（行上限/超时与直连相同）：执行失败（列漂移/超时）必须回落原卡，
    // 而不是把失败交给 `land` 跌进后面的 LLM 全目录路径。
    // 零行不报错但报「空」—— 调用方换候选表再来一轮（有表无数据 ≠ 答不出）。
    match cx.source.fetch(&scoped, dms_agent::MAX_ROWS, dms_agent::EXEC_TIMEOUT).await {
        Ok(rs) if rs.rows.is_empty() => {
            DeriveTry::Empty(dms_kernel::sql::ast::table_names_of(&sql, cx.source.dialect()).unwrap_or_default())
        }
        Ok(_) => DeriveTry::Hit(candidate),
        Err(e) => {
            tracing::warn!(err = %e, question = %cx.question, "推导 SQL 执行失败 → 回落「不可计算」卡");
            DeriveTry::Failed
        }
    }
}'''

assert s.count(old_tail) == 1, ('tail', s.count(old_tail))
s = s.replace(old_tail, new_tail)

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('patched')
