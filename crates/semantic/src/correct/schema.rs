//! SchemaCorrector：执行前表/字段白名单校验（移植 SuperSonic SchemaCorrector.correctFieldName）。
//! LLM 生成的 SQL 里，表不在当前源或「真实表.列」不在 meta.column_doc 记录的真实列清单里，
//! 判为 schema 幻觉 → 携精确可用表/列清单自修一次（比执行报 1051/1054 更早）。
//! 只校验带表前缀且前缀映射到 meta 已知物理表的列——派生表/CTE 别名列、裸列、中文别名跳过，防误伤。
//!
//! 逐行搬运自 `server/src/corrector.rs`（T8 第二批），只搬不改。

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;

use super::collect;

/// 自修提示里候选表清单的截断上限（提示是给模型看的，全量表名会淹没关键行）
const TABLE_HINT_CAP: usize = 20;

/// 执行前字段校验。返回 Some(自修提示) 表示发现幻觉列，None 表示通过。
pub async fn schema_check(pg: &PgPool, ds: &str, sql: &str) -> anyhow::Result<Option<String>> {
    let (amap, cols) = collect(sql)?;
    let real_tables: HashSet<String> = dms_kernel::sql::ast::table_names_of(
        sql,
        &dms_kernel::MysqlDialect,
    )?
    .into_iter()
    .collect();

    // 无实表（`SELECT 1` 这类）：后面两个分支必然走到 Ok(None)，不白跑 table_doc 查询
    if real_tables.is_empty() {
        return Ok(None);
    }

    // 先校验物理表。`table_doc` 为空表示该源尚未完成 schema 采集，保持原有 fail-open；
    // 一旦有当前源 schema，SQL 里的每张实表都必须在启用清单中。
    let known_tables: Vec<(String,)> = sqlx::query_as(&format!(
        "SELECT lower(table_name) FROM meta.table_doc WHERE enabled{ds_pred} ORDER BY table_name",
        ds_pred = crate::registry::ds_pred(1)
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    let known_tables: Vec<String> = known_tables.into_iter().map(|(t,)| t).collect();
    if !known_tables.is_empty() {
        // 两侧都已小写（known 来自 `SELECT lower(table_name)`，real_tables 出自 AST 收集），
        // 一次建集直接查，不再逐对 `eq_ignore_ascii_case` 双重扫描
        let known_set: HashSet<&str> = known_tables.iter().map(String::as_str).collect();
        let missing: Vec<&String> = real_tables
            .iter()
            .filter(|t| !known_set.contains(t.as_str()))
            .collect();
        if !missing.is_empty() {
            let mut hint = String::from(
                "SQL 引用了当前业务数据源不存在的表，请只使用真实表结构重写：\n",
            );
            for table in missing {
                hint.push_str(&format!("- 表 {table} 不存在或已停用。\n"));
            }
            let mut ranked: Vec<String> = known_tables
                .iter()
                .filter(|t| {
                    let hay = t.as_str();
                    real_tables.iter().any(|bad| {
                        bad.split('_')
                            .filter(|part| part.len() >= 4)
                            .any(|part| hay.contains(part))
                    })
                })
                .take(TABLE_HINT_CAP)
                .cloned()
                .collect();
            if ranked.is_empty() {
                ranked.extend(known_tables.iter().take(TABLE_HINT_CAP).cloned());
            }
            hint.push_str(&format!("当前源可用表候选：{}\n", ranked.join(", ")));
            return Ok(Some(hint));
        }
    }
    if cols.is_empty() {
        return Ok(None);
    }
    // 涉及的真实表 → 从 meta.column_doc 取真实列集合（只对 meta 已知表校验）
    // 【K6-D】ds 限定：列白名单是**每个源自己的**，拿 DMS 的列清单校别的库会把真列判成幻觉列
    // 【性能③】一次 `= ANY($1)` 取回全部涉及表（原来按表循环是 N+1 次往返），内存按表分组。
    // 谓词仍是 `lower(table_name)` 与逐表版逐字相同：行能返回 ⇔ 分组键等于某个 `real_tables`
    // 元素本身，所以按 `t` 查回分组与逐表版**逐个等价**（含「t 带大写则查不到」这个边角）。
    let q = format!(
        "SELECT lower(table_name), lower(column_name) FROM meta.column_doc WHERE lower(table_name) = ANY($1){ds_pred}",
        ds_pred = crate::registry::ds_pred(2)
    );
    let tables: Vec<String> = real_tables.iter().cloned().collect();
    let rows: Vec<(String, String)> =
        sqlx::query_as(&q).bind(&tables).bind(ds).fetch_all(pg).await?;
    let mut grouped: HashMap<String, HashSet<String>> = HashMap::new();
    for (t, c) in rows {
        grouped.entry(t).or_insert_with(HashSet::new).insert(c);
    }
    let mut table_cols: HashMap<String, HashSet<String>> = HashMap::new();
    for t in &real_tables {
        // remove 移动语义：`grouped` 之后不再使用，不整份克隆 HashSet
        if let Some(cols) = grouped.remove(t) {
            table_cols.insert(t.clone(), cols);
        }
    }
    if table_cols.is_empty() {
        return Ok(None); // 没有一张表在 meta 里（纯派生/未采集），不校验
    }

    // 找幻觉列：前缀映射到 meta 已知表，但列不在该表列集
    let mut bad: Vec<(String, String)> = vec![]; // (表, 幻觉列)
    let mut seen = HashSet::new();
    for (prefix, col) in &cols {
        if let Some(table) = amap.get(prefix) {
            if let Some(known) = table_cols.get(table) {
                if !known.contains(col) {
                    let pair = (table.clone(), col.clone());
                    if seen.insert(pair.clone()) {
                        bad.push(pair);
                    }
                }
            }
        }
    }
    if bad.is_empty() {
        return Ok(None);
    }

    // 组织自修提示：幻觉列 + 该表真实可用列清单（给 LLM 精确纠正依据）
    let mut hint = String::from("SQL 引用了不存在的列（幻觉列），请改用下方真实列名重写：\n");
    let mut listed: HashSet<String> = HashSet::new();
    for (table, col) in &bad {
        hint.push_str(&format!("- 列 {table}.{col} 不存在。"));
        if listed.insert(table.clone()) {
            if let Some(known) = table_cols.get(table) {
                let mut names: Vec<&String> = known.iter().collect();
                names.sort();
                let list = names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                hint.push_str(&format!("{table} 的真实列有：{list}"));
            }
        }
        hint.push('\n');
    }
    Ok(Some(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴【性能③】两处按表取数必须是**一次 `= ANY($1)`**，逐表循环（N+1）不许回来。
    /// 无库单测覆盖不到这段 IO，照本仓既有形态（`gather.rs` 的接线判据）用源码守。
    /// 同时钉住【K6-D】：ds 限定不许在改造中丢掉（拿 DMS 的列/码校别的库就是错判）。
    #[test]
    fn schema_and_value_lookups_are_single_any_queries() {
        // T8 搬运后两个函数分居两个文件（schema_check 在本文件、correct_value 在 value.rs）：
        // 判据跟着符号走，锚点也跟着换 —— 源码扫描类判据的锚点是它的**输入**，
        // 输入指错地方 = 判据恒真（本仓反复抓到的那类缺陷）。
        fn body<'a>(src: &'a str, marker: &str, tail: &str) -> &'a str {
            let s = src.split(marker).nth(1).expect("函数改名了 —— 顺手把这条判据一起改");
            let b = s.split("\n///").next().unwrap();
            assert!(b.contains(tail), "切段没切住：{b}");
            b
        }
        // schema_check：column_doc 一次 ANY 取回 + 内存分组
        //（断言用 contains 不用条数：函数体内的注释里也出现了同一字面量，数条数会恒红）
        let sc = body(include_str!("schema.rs"), "pub async fn schema_check", "Ok(Some(hint))");
        assert!(sc.contains("= ANY($1)"), "列清单必须一次 ANY 取回：{sc}");
        assert!(!sc.contains(".bind(t)"), "逐表循环的 bind 回来了：{sc}");
        assert!(sc.contains("ds_pred(2)"), "K6-D 的 ds 限定丢了：{sc}");
        // correct_value：value_map 一次 ANY 取回 + 内存分组
        let cv = body(include_str!("value.rs"), "pub async fn correct_value", "link_values_with");
        assert!(cv.contains("= ANY($1)"), "码表必须一次 ANY 取回：{cv}");
        assert!(!cv.contains(".bind(t)"), "逐表循环的 bind 回来了：{cv}");
        assert!(cv.contains("ds_pred(2)"), "K6-D 的 ds 限定丢了：{cv}");
    }
}
