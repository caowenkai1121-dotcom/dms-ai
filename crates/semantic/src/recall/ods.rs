//! direct-derive（合同未覆盖 → ODS 推导降级）的候选表召回。
//!
//! 候选池 = 静态目录里 layer 为 ODS/DIM 的资产，按 `warehouse_catalog::scored_assets`
//! 的问句相关性排序。血缘边（`meta.datamap_edge` kind='lineage'，DWS/ADS←ODS）是**可选
//! 增强**：血缘作业还没跑 / 表为空 / 读失败，都只按目录打分序返回 —— 降级路径自身
//! 绝不因为增强缺席而失败（warn 留痕）。
//!
//! 调用方（server `direct.rs` 的 direct-derive）拿到的只是候选表名：LLM 只看见这些表的
//! schema 卡，生成的 SQL 仍过与直连完全相同的三段闸门与行上限。

use std::collections::HashSet;

use sqlx::PgPool;

use crate::registry::{ds_pred, warehouse_asset};
use crate::warehouse_catalog::{self, detail_layer};

/// 血缘加权的锚点表数上限：问句最相关的几张合同层（非明细）表。
/// 血缘边以它们为端点找 ODS 对端；锚点取得越多，加权越接近「全目录平推」，失去意义。
const LINEAGE_ANCHORS: usize = 3;

/// datamap 推断边进 JOIN 证据的置信下限：低于它只信人工确认（status='accepted'）。
/// （两处 `status <> 'rejected'` 依赖两张边表的 status 均为 NOT NULL —— DDL 钉着，无 NULL 漏网。）
const JOIN_MIN_CONFIDENCE: f64 = 0.9;

/// 推导候选表（裸表名，目录内唯一），按「血缘命中优先、问句相关性次之」排序，最多 `limit` 张。
///
/// 血缘读不到（表空/未跑/读失败）= 零加权，照常返回目录打分序 —— 本函数因此不返回 `Result`：
/// 它的任何失败形态都收敛成「纯目录序」，没有需要调用方区分的错误。
pub async fn ods_candidate_tables(
    pg: &PgPool,
    ds: &str,
    question: &str,
    limit: usize,
) -> Vec<&'static str> {
    // 一遍循环同时产出候选池（明细层）与血缘锚点（合同层前 LINEAGE_ANCHORS 张，两谓词互补）
    let mut pool: Vec<(usize, &'static str)> = Vec::new();
    let mut anchors: Vec<String> = Vec::new();
    let mut anchor_tables = 0usize;
    for (score, asset) in warehouse_catalog::scored_assets(question) {
        if detail_layer(asset.layer) {
            pool.push((score, asset.table));
        } else if anchor_tables < LINEAGE_ANCHORS {
            // 裸名 + 限定名两种形态都喂：归一化靠 `warehouse_asset`（血缘写入侧用哪种都能中）
            anchor_tables += 1;
            anchors.push(asset.table.to_string());
            anchors.push(format!("{}.{}", warehouse_catalog::database_of(asset), asset.table));
        }
    }
    if pool.is_empty() {
        return Vec::new();
    }
    let boosted = lineage_boost(pg, ds, &anchors).await;
    let pool_n = pool.len();
    let out: Vec<&'static str> = apply_boost(pool, &boosted)
        .into_iter()
        .take(limit)
        .map(|(_, table)| table)
        .collect();
    tracing::debug!(pool = pool_n, boosted = boosted.len(), taken = out.len(), "ODS 推导候选");
    out
}

/// 血缘命中者排前，其余维持目录打分序（分数 desc → 表名 asc）。
/// 纯函数：排序逻辑无库可单测，PG 读的那半薄到只有一条 SQL。
fn apply_boost(
    pool: Vec<(usize, &'static str)>,
    boosted: &HashSet<String>,
) -> Vec<(usize, &'static str)> {
    let mut pool = pool;
    // 全序键（boosted desc → score desc → 表名 asc）：每元素算一次，行为与多段比较器全等
    pool.sort_by_key(|(score, table)| {
        (std::cmp::Reverse(boosted.contains(*table)), std::cmp::Reverse(*score), *table)
    });
    pool
}

/// 一条 JOIN 证据边（两源统一形状）：左表.左列 = 右表.右列。
/// 表名原样带出（可能裸名也可能限定名），归一化在匹配侧（server `direct.rs` 的纯函数）。
#[derive(Debug)]
pub struct JoinEvidenceRow {
    pub left_table: String,
    pub left_col: String,
    pub right_table: String,
    pub right_col: String,
}

/// 「查得到就用、读失败留痕返空集」的统一形态：血缘/JOIN 证据都是可选增强，
/// 读失败不许炸召回（两个调用点的 warn 文案自带后果说明）。
async fn fetch_or_empty<T>(
    pg: &PgPool,
    q: sqlx::query::QueryAs<'_, sqlx::Postgres, T, sqlx::postgres::PgArguments>,
    warn: &str,
) -> Vec<T>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Send + Unpin,
{
    match q.fetch_all(pg).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(err = %e, "{warn}");
            Vec::new()
        }
    }
}

/// direct-derive 的 JOIN 证据边集：**一次** PG 查询取两源 ——
/// ① `meta.join_edge` 的 active 合同边；② `meta.datamap_edge` 里 kind='joinable' 且
///    高置信（>=0.9，rejected 除外）或人工确认（status='accepted'，不看置信度）的边。
/// 两端表都限制在候选集内（裸名/限定名两种形态都喂）。读失败 → 空集（warn 留痕）：
/// 有 JOIN 的推导自然因「无证据」被拒，无 JOIN 的单表推导不受影响 —— fail-closed。
pub async fn join_evidence_edges(pg: &PgPool, ds: &str, tables: &[&str]) -> Vec<JoinEvidenceRow> {
    if tables.is_empty() {
        return Vec::new();
    }
    // 裸名 + 目录限定名两种形态都喂；先过 `catalog_ident` 归一（去空白/反引号），与
    // `warehouse_asset` 的判定同一标准；去重防重名表让 `ANY($1)` 数组白膨胀
    let mut forms: Vec<String> = tables
        .iter()
        .map(|t| crate::registry::catalog_ident(t))
        .flat_map(|table| {
            let qualified = warehouse_asset(table)
                .map(|asset| format!("{}.{}", warehouse_catalog::database_of(asset), asset.table));
            [table.to_string()].into_iter().chain(qualified)
        })
        .collect();
    forms.sort();
    forms.dedup();
    let rows: Vec<(String, String, String, String)> = fetch_or_empty(
        pg,
        sqlx::query_as(&format!(
            "SELECT left_table, left_col, right_table, right_col FROM meta.join_edge \
             WHERE status = 'active' AND left_table = ANY($1) AND right_table = ANY($1){ds_pred} \
             UNION ALL \
             SELECT left_table, left_col, right_table, right_col FROM meta.datamap_edge \
             WHERE kind = 'joinable' AND status <> 'rejected' \
               AND (confidence >= {JOIN_MIN_CONFIDENCE} OR status = 'accepted') \
               AND left_table = ANY($1) AND right_table = ANY($1){ds_pred}",
            ds_pred = ds_pred(2)
        ))
        .bind(&forms)
        .bind(ds),
        "JOIN 证据边读取失败 → 带 JOIN 的推导将因无证据被拒",
    )
    .await;
    rows.into_iter()
        .map(|(left_table, left_col, right_table, right_col)| JoinEvidenceRow {
            left_table,
            left_col,
            right_table,
            right_col,
        })
        .collect()
}

/// 血缘边里落在目录明细层的对端表名集合。**方向不敏感**：血缘边的左/右哪端是 ODS
/// 由写入侧（`semantic::lineage`，并行任务）决定，这里只认「锚点表的对端落在明细层」。
async fn lineage_boost(pg: &PgPool, ds: &str, anchors: &[String]) -> HashSet<String> {
    if anchors.is_empty() {
        return HashSet::new();
    }
    let rows: Vec<(String, String)> = fetch_or_empty(
        pg,
        sqlx::query_as(&format!(
            "SELECT left_table, right_table FROM meta.datamap_edge \
             WHERE kind = 'lineage' AND status <> 'rejected' \
               AND (left_table = ANY($1) OR right_table = ANY($1)){ds_pred}",
            ds_pred = ds_pred(2)
        ))
        .bind(anchors)
        .bind(ds),
        // 血缘是可选增强：表还没建 / 作业还没跑 / PG 抖动，都只留痕，不影响候选序
        "血缘边读取失败 → 推导候选按目录打分序（零加权）",
    )
    .await;
    rows.iter()
        .flat_map(|(left, right)| [left, right])
        // 端点归一（裸名/限定名两种形态都中）靠 `warehouse_asset` 内部的 `warehouse_table_parts` 剥库名
        .filter_map(|endpoint| warehouse_asset(endpoint))
        .filter(|asset| detail_layer(asset.layer))
        .map(|asset| asset.table.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 候选池只含明细层：合同层（DWS/ADS）一张都不许进来 —— 否则推导就成了
    /// 「换一张合同表猜」，fail-closed 顺序被颠倒。
    #[test]
    fn candidate_pool_is_detail_layer_only() {
        let pool: Vec<&str> = warehouse_catalog::scored_assets("本月销售额按门店")
            .iter()
            .filter(|(_, asset)| detail_layer(asset.layer))
            .map(|(_, asset)| asset.table)
            .collect();
        assert!(!pool.is_empty(), "门店问句必须召到明细层候选");
        for table in &pool {
            let asset = warehouse_asset(table).expect("候选必须能解析回目录资产");
            assert!(
                detail_layer(asset.layer),
                "合同层表混进了推导候选：{table}（{}）",
                asset.layer
            );
        }
        // 销售×门店：候选来自 ODS 明细（经销商上报/门店主数据），DWS 事实不许在
        assert!(pool.contains(&"t_master_shop"), "{pool:?}");
        assert!(!pool.contains(&crate::sales_fact::TABLE_NAME), "{pool:?}");
        // 订单口径问句：订单头必须排在候选首位（domain 命中 32 分）
        let order_pool: Vec<&str> = warehouse_catalog::scored_assets("本月订单销售额")
            .iter()
            .filter(|(_, asset)| detail_layer(asset.layer))
            .map(|(_, asset)| asset.table)
            .collect();
        assert_eq!(order_pool.first(), Some(&"t_sales_order"), "{order_pool:?}");
    }

    /// 空锚点早退钉住：`anchors.is_empty()` 的 return 必须在血缘查询之前
    /// （早退丢失 = 每轮白付一次空集 PG 往返）。
    #[test]
    fn empty_anchors_short_circuit_is_pinned() {
        let src = include_str!("ods.rs");
        let body = &src[src.find("async fn lineage_boost").unwrap()..];
        let early = body.find("anchors.is_empty()").unwrap();
        let query = body.find("SELECT left_table, right_table").unwrap();
        assert!(early < query, "空锚点早退必须在血缘查询之前");
    }

    /// 血缘加权只把命中者提到前面，不打乱其余打分序；空集合 = 恒等（血缘缺席的降级形态）。
    #[test]
    fn boost_moves_lineage_hits_first_and_keeps_scored_order() {
        let pool: Vec<(usize, &'static str)> =
            vec![(30, "t_sales_order"), (20, "t_master_shop"), (10, "t_goods")];
        let none = HashSet::new();
        let unchanged = apply_boost(pool.clone(), &none);
        assert_eq!(
            unchanged.iter().map(|(_, t)| *t).collect::<Vec<_>>(),
            vec!["t_sales_order", "t_master_shop", "t_goods"],
            "零加权必须维持目录打分序"
        );
        let boosted: HashSet<String> = ["t_goods".to_string()].into_iter().collect();
        let reordered = apply_boost(pool, &boosted);
        assert_eq!(
            reordered.iter().map(|(_, t)| *t).collect::<Vec<_>>(),
            vec!["t_goods", "t_sales_order", "t_master_shop"],
            "血缘命中者排前，其余维持原序"
        );
    }
}
