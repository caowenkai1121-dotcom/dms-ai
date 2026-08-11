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
use match_dict::best_dict_match_ix;
use probe::{ColRef, ProbeGate};
use register::DictHit;

/// A1 自动发现引擎：字典码列自动对码（数据驱动注册——字典变了重跑即自适应，不再需要手工播种）。
/// 候选=码型后缀列（后缀清单以 `probe::candidate_columns` 的正则为准，这里不复制）+小表
/// (row_estimate<100万)；只读 DISTINCT 抽样(≤61 值)；值集 ⊆ 某 dict key 码集(覆盖≥80% 且 ≥2 值)→
/// 自动注册 value_map(eq 换码,字典全码)+dimension(CASE 翻名)。人工种子优先：已覆盖 (表,列) 跳过。
/// 收尾再跑 `discover_domain_values`（名称型值域取值 → 同一张 value_map，见该函数）。
pub async fn autodiscover_dict_columns(
    mysql: &ReadOnlyMySql,
    pg: &PgPool,
    gate: &ProbeGate<'_>,
) -> anyhow::Result<serde_json::Value> {
    // 只吃 `&ReadOnlyMySql`（= DMS 主源）→ 读写都固定在 'dms' 那一格
    let ds = DMS_DS_ID;
    // 四条加载互不依赖（1×MySQL + 3×PG），一次并发取齐（原来串行白加四段启动延迟）
    let (dicts, cands, manual, del_tables) = tokio::try_join!(
        probe::load_dicts(mysql),
        probe::candidate_columns(pg, ds),
        probe::manual_covered(pg, ds),
        probe::del_flag_tables(pg, ds),
    )?;

    let mut probed = 0usize;
    let mut probe_failed = 0usize;
    let mut skipped_manual = 0usize;
    let mut skipped_backup = 0usize;
    let mut skipped_sensitive = 0usize;
    let mut failed = 0usize;
    let mut registered: Vec<serde_json::Value> = vec![];
    // 一轮对码的预建视图（小写键/码集只建一次，全部候选列复用；key 排序 → 跨轮可复现）
    let dict_index = match_dict::DictIndex::build(&dicts);

    for (i, cand) in cands.iter().enumerate() {
        let (table, col, comment) = (&cand.table, &cand.col, &cand.comment);
        // 进度留痕：单探针最坏 10s × N 候选，跑半小时外面不能一动不动
        if i > 0 && i % 50 == 0 {
            tracing::info!(done = i, total = cands.len(), registered = registered.len(), "自动发现进度");
        }
        if is_backup_table(table) {
            skipped_backup += 1;
            continue;
        }
        if is_sensitive_col(col) {
            skipped_sensitive += 1;
            continue;
        }
        if manual.covers(table, col) {
            skipped_manual += 1;
            continue;
        }
        let c = ColRef { table, col, has_del: del_tables.contains(table) };
        let Some(values) = probe::sample_values(mysql, gate, &c).await else {
            // 闸门拒/抽样失败两种原因已在 warn 日志里，这里计数进输出
            probe_failed += 1;
            continue;
        };
        // 空抽样（空表/全 NULL 列）不算探到（probed 口径 = 拿到非空抽样）
        if values.is_empty() {
            continue;
        }
        probed += 1;
        let Some((dict_key, dict_name, pairs, coverage)) =
            best_dict_match_ix(&values, &dict_index, comment)
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
            // 不同值个数（trim 后去重；不是抽样行数）
            distinct: values.iter().collect::<HashSet<_>>().len(),
        };
        match register::register_match(pg, ds, &hit).await {
            Ok(v) => registered.push(v),
            Err(e) => {
                // 单次注册失败不中止整轮：前面已注册的保留、后面候选继续
                failed += 1;
                tracing::warn!(err = %e, "注册失败（继续后续候选）：{table}.{col}");
            }
        }
    }

    // 收尾分段容错：字典段已落库的注册成果不因值域/元素同步失败而整轮 Err（部分成果可见）
    let domains = match discover_domain_values(mysql, pg, gate, &del_tables, ds).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(err = %e, "值域取值入库失败（字典段成果保留）");
            vec![]
        }
    };

    // 新注册的维度/码值同步进元素注册表（向量化召回原子单位）
    if let Err(e) = sync_elements(pg).await {
        tracing::warn!(err = %e, "元素注册表同步失败（下轮启动补齐）");
    }

    Ok(serde_json::json!({
        "dict_keys": dicts.len(),
        "candidates": cands.len(),
        "probed": probed,
        "probe_failed": probe_failed,
        "skipped_manual": skipped_manual,
        "skipped_backup": skipped_backup,
        "skipped_sensitive": skipped_sensitive,
        "registered_count": registered.len(),
        "registered": registered,
        "failed": failed,
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
    ds: &str, // 与外层同一事实源（原来内部两处直接写 DMS_DS_ID，改 ds 语义只改一半）
) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut out = vec![];
    for d in load_value_domains(pg, ds).await? {
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
        let n = register::register_domain_values(pg, ds, table, col, &values).await?;
        out.push(serde_json::json!({ "table": table, "column": col, "values": n }));
    }
    Ok(out)
}
