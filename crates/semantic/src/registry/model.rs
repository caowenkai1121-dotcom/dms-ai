//! 装配侧的注册表行类型与读取：指标 / 维度 / JOIN 边 / 表级标准口径。
//! 变更原因＝装配侧读到的形状。
//!
//! 搬运源：行类型 `server/src/direct.rs:30-57`（字段逐字不变，`DimDef` 更名 `DimensionDef`）、
//! 四条加载 SQL `server/src/direct.rs:65-101`（SQL 文本与绑定序号原样，谓词仍走 `ds_pred`）。
//!
//! 返回 `Result` 而非 `Option`：`try_compose` 今天写的是 `.ok()?` / `.unwrap_or_default()`，
//! 调用点保留那两个后缀即零行为变化。

use crate::registry::{
    catalog_allows_dimension, catalog_allows_metric_record, catalog_allows_table,
    join_asset_live_pred_at, scoped_pred_1, source_asset_live_pred_at, table_asset_live_pred_at,
};
use sqlx::PgPool;

/// `meta.metric` 装配投影行（11 列）：Vec 标注与 query_as 共用这一份。
type MetricRow = (
    String,
    Vec<String>,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

/// `meta.metric` 治理投影行（13 列）：同上。
type MetricPolicyRow = (
    String,
    String,
    Vec<String>,
    String,
    Vec<String>,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

/// 指标定义（meta.metric 行）
#[derive(Debug)]
pub struct MetricDef {
    pub name: String,
    pub aliases: Vec<String>,
    pub source_table: String,
    pub agg_expr: String,
    pub scope_filter: String,
    /// 去重键（逗号分隔列）：该来源表含系统级重复行时必填，聚合前须按这些列 DISTINCT。
    /// 空=表无重复问题。t_sales_order_detail 实测 100.7 万行原始 vs 83.2 万去重后。
    pub dedup_keys: String,
    /// 该指标的时间语义钉在哪一列（`meta.metric.time_col`）。空 = 未声明。
    ///
    /// 🔴 装配侧此前**不读它**：`compose_sql_with` 的时间窗写死 `t_sales_order` / `order_time`
    /// （在 FROM 里找不到就试着桥一条边，桥不到就整条不装配）。于是任何时间语义不在订单头上的
    /// 指标 —— 售后单数（`after_sales_time`）、开票金额、动销商品数 —— **一律放不下时间窗、
    /// 一律回落 LLM**，而声明里明明写着该用哪一列。这是「声明在那儿、装配器不读」的又一处。
    pub time_col: String,
}

/// 指标治理元数据：版本用于审计，允许维度用于阻止未经验证的指标×维度自动组合。
#[derive(Debug)]
pub struct MetricPolicy {
    pub metric_code: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub version: String,
    pub allowed_dimensions: Vec<String>,
}

/// 维度定义（meta.dimension 行）
#[derive(Debug)]
pub struct DimensionDef {
    pub name: String,
    pub aliases: Vec<String>,
    pub source_table: String,
    pub expr: String,
}

/// JOIN 边（meta.join_edge 行）
#[derive(Debug)]
pub struct JoinEdge {
    pub lt: String,
    pub lc: String,
    pub rt: String,
    pub rc: String,
    pub card: String, // lt→rt: "1:N"(扇出) / "N:1"(收敛)
}

pub async fn load_metrics(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<MetricDef>> {
    let ds_pred = scoped_pred_1(source_asset_live_pred_at);
    let rows: Vec<MetricRow> = sqlx::query_as(
        &format!(
            "SELECT name, aliases, source_table, agg_expr, scope_filter, dedup_keys, time_col,
                    description, unit, time_cap, version
             FROM meta.metric WHERE status = 'active'{ds_pred} ORDER BY name",
        ),
    )
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(name, _, source, expr, scope, dedup, time_col, description, unit, time_cap, version)| {
            catalog_allows_metric_record(
                ds, name, source, expr, scope, time_col, dedup, description, unit, time_cap,
                version,
            )
        })
        .map(
            |(
                name,
                aliases,
                source_table,
                agg_expr,
                scope_filter,
                dedup_keys,
                time_col,
                _,
                _,
                _,
                _,
            )| {
                MetricDef {
                    name,
                    aliases,
                    source_table,
                    agg_expr,
                    scope_filter,
                    dedup_keys,
                    time_col,
                }
            },
        )
        .collect())
}

pub async fn load_metric_policies(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<MetricPolicy>> {
    let ds_pred = scoped_pred_1(source_asset_live_pred_at);
    let rows: Vec<MetricPolicyRow> = sqlx::query_as(&format!(
        "SELECT metric_code, name, aliases, version, allowed_dimensions, source_table, agg_expr,
                scope_filter, time_col, dedup_keys, description, unit, time_cap
         FROM meta.metric WHERE status = 'active'{ds_pred} ORDER BY name, metric_code",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, name, _, version, _, source, expr, scope, time_col, dedup, description, unit, time_cap)| {
            catalog_allows_metric_record(
                ds, name, source, expr, scope, time_col, dedup, description, unit, time_cap,
                version,
            )
        })
        .map(
            |(
                metric_code,
                name,
                aliases,
                version,
                allowed_dimensions,
                source_table,
                _,
                _,
                _,
                _,
                _,
                _,
                _,
            )| {
                // source 解析一次，逐维度判定（原来每个维度都重跑一遍 source_refs）
                let checker = crate::registry::metric_dimension_checker(ds, &source_table);
                MetricPolicy {
                    metric_code,
                    name,
                    aliases,
                    version,
                    allowed_dimensions: allowed_dimensions
                        .into_iter()
                        .filter(|dimension| checker(dimension))
                        .collect(),
                }
            },
        )
        .collect())
}

pub async fn load_dimensions(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<DimensionDef>> {
    let ds_pred = scoped_pred_1(source_asset_live_pred_at);
    let rows: Vec<(String, Vec<String>, String, String)> = sqlx::query_as(&format!(
        "SELECT name, aliases, source_table, expr FROM meta.dimension WHERE status = 'active'{ds_pred}
         ORDER BY name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(name, _, source_table, expr)| {
            catalog_allows_dimension(ds, name, source_table, expr)
        })
        .map(|(name, aliases, source_table, expr)| DimensionDef {
            name,
            aliases,
            source_table,
            expr,
        })
        .collect())
}

pub async fn load_join_edges(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<JoinEdge>> {
    let ds_pred = scoped_pred_1(join_asset_live_pred_at);
    // ORDER BY 钉死输出序：下游按序组规则/提示，不随 PG 物理行序漂
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT left_table, left_col, right_table, right_col, card
         FROM meta.join_edge WHERE status = 'active'{ds_pred}
         ORDER BY left_table, right_table, left_col, right_col",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(lt, _, rt, _, _)| {
            catalog_allows_table(ds, lt) && catalog_allows_table(ds, rt)
        })
        .map(|(lt, lc, rt, rc, card)| JoinEdge { lt, lc, rt, rc, card })
        .collect())
}

/// 表级标准口径（meta.table_scope 行）。`note` 是**人话**那一句（LLM 会逐字读），
/// 口径校验违规时原样回吐给它，故不能只留 `filter`。
#[derive(Debug)]
pub struct TableScope {
    pub table_name: String,
    pub filter: String,
    pub note: String,
}

/// 快照/流水表声明（meta.table_snapshot 行）：同一分区键有多条历史行，取数须只留最新一条。
#[derive(Debug)]
pub struct TableSnapshot {
    pub table_name: String,
    /// 分区键（逗号分隔列）：`(客户, 余额类型)` 这类「一个业务实体一条最新行」的键
    pub partition_cols: String,
    /// 取最新的排序（原样一句 `created_time DESC, id DESC`，含方向）
    pub order_cols: String,
    /// 该表恒需的额外过滤（如只算生效行）；空=无
    pub extra_filter: String,
    pub note: String,
}

/// 码值一行：`名字 → (表, 列, 码)`。装配侧用它把问句里的值过滤**按声明**装进 WHERE。
///
/// 与 `registry::caliber::load_code_values` 的区别：那个带 `length(code) >= 3` 早筛
/// （它服务的是「码写在了哪一列」那条判据，短码没有区分度）；装配侧**不能筛** ——
/// 「退货 → after_sales_type = 1」正是一位码，筛掉它就等于这条声明对装配器不存在。
///
/// （与 `lexicon::ValueMap` 同表同字段的另一份行类型：字段名不同（table/column vs
/// table_name/column_name），合并要动 server 侧消费点 —— 欠账，两处注释互指。）
#[derive(Debug)]
pub struct ValueRef {
    pub table: String,
    pub column: String,
    pub name: String,
    pub code: String,
    /// 匹配方式（`meta.value_map.match_kind`）：`eq` = `列 = '码'`；`like` = 该列是**多值串**
    /// （`t_sales_order.paid_way` 一单多种支付方式），须 `LIKE '%码%'`。
    ///
    /// 🔴 装配侧**只认 `eq`**（实测 931 eq / 5 like）。不筛在 SQL 里、而是原样带出来交给
    /// `value_filters` 挡：判据要能被单测枪测，筛在 SQL 里就只剩「装配器看不见这几行」这一句话。
    pub match_kind: String,
}

/// 装配侧码值全量加载。
/// 🔴 与 `lexicon::load_value_maps` 同表两份加载：过滤口径（`catalog_allows_table` vs
/// `catalog_allows_column`）、`ORDER BY` 有无都不同 —— 各自服务不同判据，改一边先看另一边。
pub async fn load_value_map(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<ValueRef>> {
    let ds_pred = scoped_pred_1(table_asset_live_pred_at);
    let rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, name, code, match_kind FROM meta.value_map
         WHERE 1 = 1{ds_pred} ORDER BY name, table_name, column_name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table, ..)| catalog_allows_table(ds, table))
        .map(|(table, column, name, code, match_kind)| ValueRef {
            table,
            column,
            name,
            code,
            // DDL 是 `NOT NULL DEFAULT 'eq'`：NULL 实际不存在；兜底与 DDL 默认值同口径
            // （下游 `value_filters` 只认 "eq" 精确值，空串会被静默丢弃）
            match_kind: match_kind.unwrap_or_else(|| "eq".to_string()),
        })
        .collect())
}

/// 表级标准口径全量行（含 `note`）。`load_table_scopes` 是它的二元组投影——**同一条 SQL**，
/// 不许各写一份（口径出现第二处真相就是漂移的开始）。
pub async fn load_table_scope_rows(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<TableScope>> {
    let ds_pred = scoped_pred_1(table_asset_live_pred_at);
    // ORDER BY 钉死行序：caliber 侧「note 首次登记者胜出」（merge_cols），同表多条 scope
    // 时人话取哪条不许随 PG 物理行序漂
    let rows: Vec<(String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, filter, note FROM meta.table_scope WHERE 1 = 1{ds_pred}
         ORDER BY table_name, note",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table_name, ..)| catalog_allows_table(ds, table_name))
        .map(|(table_name, filter, note)| TableScope { table_name, filter, note })
        .collect())
}

/// 表级标准口径（SuperSonic model filter）：JOIN 到的表恒需附加的过滤 `(table_name, filter)`。
/// 刻意保持二元组：装配侧（`compose_sql_with`）与它的断言吃的就是 `&[(String, String)]`。
pub async fn load_table_scopes(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<(String, String)>> {
    Ok(load_table_scope_rows(pg, ds).await?.into_iter().map(|s| (s.table_name, s.filter)).collect())
}

pub async fn load_table_snapshots(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<TableSnapshot>> {
    let ds_pred = scoped_pred_1(table_asset_live_pred_at);
    // ORDER BY 钉死行序：caliber 侧按序 push RequireLatest，规则序不随物理行序漂
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, partition_cols, order_cols, extra_filter, note
         FROM meta.table_snapshot WHERE 1 = 1{ds_pred} ORDER BY table_name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table_name, ..)| catalog_allows_table(ds, table_name))
        .map(|(table_name, partition_cols, order_cols, extra_filter, note)| TableSnapshot {
            table_name,
            partition_cols,
            order_cols,
            extra_filter,
            note,
        })
        .collect())
}

/// 【T8-B9】指标名 + 物理表（推导闸 1 通道②的语料）。
///
/// 逐字带走 `server/src/direct.rs` 里那条 `SELECT name, source_table FROM meta.metric …` ——
/// 它随 `ods_derive` 迁到了 agent，而**agent 不许写 SQL**（`check-arch.ps1` 的 FAIL 级红线：
/// 每一条 `meta.*` 查询都必须落在本 crate）。刻意不复用 `load_metrics`：那条带 `ORDER BY name`、
/// 另一套 ds 谓词与 `catalog_allows_metric_record` 过滤，行集与行序都不同 —— 复用就不是纯搬运了。
pub async fn load_metric_sources(pg: &PgPool, ds: &str) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as("SELECT name, source_table FROM meta.metric WHERE ds_id IN ($1, '*') AND status='active'")
        .bind(ds)
        .fetch_all(pg)
        .await
}
