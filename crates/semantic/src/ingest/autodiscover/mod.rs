//! A1 自动发现引擎的**编排**：探测（`probe`）→ 三闸对码（`match_dict`）→ 注册（`register`）。
//! 变更原因＝三段的先后与统计口径。
//!
//! 搬运源 `server/src/meta.rs:1344-1531`（`autodiscover_dict_columns` 160 行按三段拆开，
//! 分支顺序与 `continue` 位置逐条对齐原行号：备份表/敏感列 1424、人工已覆盖 1428、
//! 探针失败 1439/1452、对码不中 1464）。

pub mod match_dict;
pub mod probe;
pub mod register;

use std::collections::HashSet;

use dms_connector::mysql::ReadOnlyMySql;
use sqlx::PgPool;

use crate::registry::datasource::DMS_DS_ID;
use crate::registry::element::sync_elements;
use crate::registry::lexicon::load_value_domains;
use crate::registry::{is_backup_table, is_sensitive_col};
use match_dict::best_dict_match;
use probe::{ColRef, ProbeGate};
use register::DictHit;

/// A1 自动发现引擎：字典码列自动对码（数据驱动注册——字典变了重跑即自适应，不再需要手工播种）。
/// 候选=码型后缀列(*_code/_type/_status/_class/_mode/_way/_level)+小表(row_estimate<100万)；
/// 只读 DISTINCT 抽样(≤61 值)；值集 ⊆ 某 dict key 码集(覆盖≥80% 且 ≥2 值)→
/// 自动注册 value_map(eq 换码,字典全码)+dimension(CASE 翻名)。人工种子优先：已覆盖 (表,列) 跳过。
/// 收尾再跑 `discover_domain_values`（名称型值域取值 → 同一张 value_map，见该函数）。
pub async fn autodiscover_dict_columns(
    mysql: &ReadOnlyMySql,
    pg: &PgPool,
    gate: &ProbeGate<'_>,
) -> anyhow::Result<serde_json::Value> {
    // 只吃 `&ReadOnlyMySql`（= DMS 主源）→ 读写都固定在 'dms' 那一格（与 `sync_schema` 同）
    let ds = DMS_DS_ID;
    let dicts = probe::load_dicts(mysql).await?;
    let cands = probe::candidate_columns(pg, ds).await?;
    let manual = probe::manual_covered(pg, ds).await?;
    let del_tables = probe::del_flag_tables(pg, ds).await?;

    let mut probed = 0usize;
    let mut skipped_manual = 0usize;
    let mut registered: Vec<serde_json::Value> = vec![];

    for (table, col, comment) in &cands {
        if is_backup_table(table) || is_sensitive_col(col) {
            continue;
        }
        if manual.covers(table, col) {
            skipped_manual += 1;
            continue;
        }
        let c = ColRef { table, col, has_del: del_tables.contains(table) };
        let Some(values) = probe::sample_values(mysql, gate, &c).await else {
            continue;
        };
        probed += 1;
        let Some((dict_key, dict_name, pairs, coverage)) =
            best_dict_match(&values, &dicts, comment)
        else {
            continue;
        };
        let hit = DictHit {
            table,
            col,
            comment,
            dict_key,
            dict_name,
            pairs,
            coverage,
            distinct: values.len(),
        };
        registered.push(register::register_match(pg, ds, &hit).await?);
    }

    let domains = discover_domain_values(mysql, pg, gate, &del_tables).await?;

    // 新注册的维度/码值同步进元素注册表（向量化召回原子单位）
    sync_elements(pg).await?;

    Ok(serde_json::json!({
        "dict_keys": dicts.len(),
        "candidates": cands.len(),
        "probed": probed,
        "skipped_manual": skipped_manual,
        "registered_count": registered.len(),
        "registered": registered,
        "domain_values": domains,
    }))
}

/// 名称型值域取值入库：`meta.value_domain` 声明的每个 (表,列) → DISTINCT 取值 →
/// `meta.value_map`（name=code）。
///
/// 为什么码型那一段吃不到这些列：候选靠 `_code|_type|_status|…` 后缀发现，而「分类名」这种
/// **实体名**列后缀不匹配 → 从未被发现 → LLM 只能猜成「商品名 LIKE」，把名字含该词却不属该
/// 分类的商品算进来（实测「手抓饼这个分类卖了多少箱」156847 vs 正确 115175，虚高 36%）。
/// 显式声明过的列不走三闸（三闸防的是码列误配），也不派生维度，也**不查 `manual.covers`**：
/// 声明本身就是人工意图（`t_goods_category.category_name` 已被一条 dimension 的 expr 提及，
/// 拿 covers 一挡这段就整段空转）。
async fn discover_domain_values(
    mysql: &ReadOnlyMySql,
    pg: &PgPool,
    gate: &ProbeGate<'_>,
    del_tables: &HashSet<String>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut out = vec![];
    for d in load_value_domains(pg, DMS_DS_ID).await? {
        let (table, col) = (&d.table_name, &d.column_name);
        let c = ColRef { table, col, has_del: del_tables.contains(table) };
        let Some(values) = probe::sample_domain_values(mysql, gate, &c).await else {
            continue;
        };
        // 探针空转（表为空/列被脱敏置空）时不许清库：旧取值比零取值有用
        if values.is_empty() {
            tracing::warn!("值域探针零取值，保留旧词典 {table}.{col}");
            continue;
        }
        let n = register::register_domain_values(pg, DMS_DS_ID, table, col, &values).await?;
        out.push(serde_json::json!({ "table": table, "column": col, "values": n }));
    }
    Ok(out)
}
