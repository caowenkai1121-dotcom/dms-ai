//! AGE 图关系问答：客户-购买-商品图。
//! 关系只从已验证的 Doris 销售 DWS 构建；生产业务库绝不承担图谱聚合任务。
//!
//! 逐行搬自 `server/src/graph.rs:1-198`（T9-A1）：整块是 IO（AGE/Cypher + MySQL 读），
//! 留在 server 会让 agent 的图 Answerer 产生 agent → server 的反向依赖边。
//! Cypher 文本 / `SET search_path` / agtype 解析 / 批量参数化写法一字未改，
//! 故意**不**改走 `fixed(&'static str)` 通道：Cypher 串带动态量进不去那条通道，
//! 且改通道会让「逐行对拍」失效。

use crate::mysql::ReadOnlyMySql;
use crate::source::SqlSource;
// 滑窗在 kernel（纯函数）：SQL 路径的切片向量召回（A8）与本文件的实体抽取共用同一份
use dms_kernel::nl::text::candidate_windows;
use sqlx::{PgPool, Row};

const GRAPH: &str = "dms_graph";
const GRAPH_EDGE_LIMIT: usize = 250_000;
const GRAPH_SOURCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// 图查询 TOP N 的防御上限：limit 直接拼进 Cypher，调用方传多大就查多大可不行
const GRAPH_QUERY_MAX_LIMIT: usize = 1000;
// 主边 SQL 字面量写死 LIMIT 250001（raw_all 要 &'static str 不能 format!）—— 与常量钉死 +1 关系
const _: () = assert!(250001 == GRAPH_EDGE_LIMIT + 1);

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphAvailability {
    desired_target: Option<String>,
    graph_target: Option<String>,
    generation: u64,
}

impl Default for GraphAvailability {
    fn default() -> Self {
        Self { desired_target: None, graph_target: None, generation: 0 }
    }
}

impl GraphAvailability {
    fn invalidate(&mut self, target: &str) -> GraphInvalidation {
        let previous = self.clone();
        self.generation = self.generation.wrapping_add(1);
        self.desired_target = Some(target.to_string());
        self.graph_target = None;
        GraphInvalidation { previous, invalidated_generation: self.generation }
    }

    fn begin_sync(&mut self, target: &str) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.desired_target = Some(target.to_string());
        // 从 drop/create 开始，旧图物理内容不再可信；失败也不能重新放行。
        self.graph_target = None;
        self.generation
    }

    fn mark_ready(&mut self, target: &str, generation: u64) -> bool {
        if self.generation != generation || self.desired_target.as_deref() != Some(target) {
            return false;
        }
        self.graph_target = Some(target.to_string());
        true
    }

    /// 跨进程接管：持久化标记证明图内容就是该目标的那份时，把本进程状态直接置就绪。
    fn adopt(&mut self, target: &str) {
        self.generation = self.generation.wrapping_add(1);
        self.desired_target = Some(target.to_string());
        self.graph_target = Some(target.to_string());
    }

    fn lease(&self, target: &str) -> Option<GraphLease> {
        (self.desired_target.as_deref() == Some(target)
            && self.graph_target.as_deref() == Some(target))
        .then(|| GraphLease {
            target: target.to_string(),
            generation: self.generation,
        })
    }
}

/// 图查询租约：查询前获取、返回结果前复核。切库或重建会推进 generation，旧结果随即失效。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphLease {
    target: String,
    generation: u64,
}

/// 切库失效令牌。只有状态未被并发同步推进时，失败回滚才允许恢复旧状态。
pub struct GraphInvalidation {
    previous: GraphAvailability,
    invalidated_generation: u64,
}

fn availability() -> &'static std::sync::RwLock<GraphAvailability> {
    static STATE: std::sync::OnceLock<std::sync::RwLock<GraphAvailability>> =
        std::sync::OnceLock::new();
    STATE.get_or_init(|| std::sync::RwLock::new(GraphAvailability::default()))
}

fn sync_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 在连接池真正换入前立即关闭图快路径。目标名不含凭据，可以安全留在进程状态中。
pub fn invalidate_for_target(target: &str) -> GraphInvalidation {
    availability().write().unwrap_or_else(|e| e.into_inner()).invalidate(target)
}

/// 切库失败时恢复旧状态；若期间已有同步开始，拒绝覆盖并发状态。
pub fn restore_after_failed_switch(token: GraphInvalidation) -> bool {
    let mut state = availability().write().unwrap_or_else(|e| e.into_inner());
    if state.generation != token.invalidated_generation {
        return false;
    }
    *state = token.previous;
    true
}

pub fn ready_lease(target: &str) -> Option<GraphLease> {
    availability().read().unwrap_or_else(|e| e.into_inner()).lease(target)
}

pub fn lease_is_current(lease: &GraphLease) -> bool {
    availability()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .lease(&lease.target)
        .is_some_and(|current| current == *lease)
}

pub fn is_ready_for(target: &str) -> bool {
    ready_lease(target).is_some()
}

/// 跨进程接管图就绪状态：CLI 等短生命周期进程不重建图，只核验「图内容就是当前目标
/// 建的」—— 标记只由完整重建成功写入，重建中的 drop_graph 会先抹掉它（缺席 = 不可信，
/// 直接回落到非图路径，与未同步行为一致）。
pub async fn adopt_if_current(pg: &PgPool, target: &str) -> bool {
    let read = async {
        let mut conn = age_conn(pg).await?;
        let cy = format!(
            "SELECT target::text FROM (SELECT * FROM cypher('{GRAPH}', $$ MATCH (m:GraphMeta) RETURN m.target LIMIT 1 $$) AS (target agtype)) sub"
        );
        let rows = sqlx::query(&cy).fetch_all(&mut *conn).await?;
        let row = rows.first().ok_or_else(|| anyhow::anyhow!("图无就绪标记"))?;
        let value: Option<String> = row.try_get(0).ok();
        let value = value.ok_or_else(|| anyhow::anyhow!("图就绪标记读不出"))?;
        anyhow::Ok(unquote(&value))
    };
    match read.await {
        Ok(marker) if marker == target => {
            availability().write().unwrap_or_else(|e| e.into_inner()).adopt(target);
            true
        }
        Ok(_) => false, // 标记与目标不符：图是别的目标建的，不可信
        Err(e) => {
            // 真实 DB 错误（断网/权限）与「无标记」都落这里：留痕但不放行
            tracing::debug!(err = %e, "图就绪标记读取失败，按不可信处理");
            false
        }
    }
}

/// AGE 连接准备：每连接需 LOAD age + search_path（放 fetch 前）
async fn age_conn(pg: &PgPool) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
    let mut conn = pg.acquire().await?;
    // 两条独立 `query()`（两次 RTT）是刻意的：`raw_sql` 合并版（simple protocol 一句多命令）
    // 的 future 形状会让下游 `JoinSet::spawn`（knowledge 图谱构建流水）过不了 Send 的 HRTB
    // 证明（实测 rustc 报 "not general enough"，整仓编不过）——别为了省一次 RTT 换回去。
    sqlx::query("LOAD 'age'").execute(&mut *conn).await?;
    sqlx::query("SET search_path = ag_catalog, public")
        .execute(&mut *conn)
        .await?;
    Ok(conn)
}

/// Cypher 单引号字面量清洗，单趟成型：
/// - 剥 `\`：防 AGE 转义歧义的刻意取舍（实体名里的 `\n` 会变 `n`——数据损毁而非转义，
///   但图名来自主档业务数据，可接受）；
/// - 剥控制字符：换行等可撑破 Cypher 单引号字面量；
/// - `'` → `\'`。
/// 与 doc_graph.rs 的 esc 必须同文（两侧测试互守）。
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => {}
            '\'' => out.push_str("\\'"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// 插值进 `=~` 正则前：在 esc 之上再转义正则元字符 —— 业务词里的 `.`/`*`/`[`（"C++"、
/// "A.B"）不该改变匹配语义。
fn esc_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => {}
            '\'' => out.push_str("\\'"),
            c if "^$.*+?()[]{}|".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// 客户 → 省区/省份维度边。省区取统一合同 `region`；省份取客户主档 `t_customer.province`
/// （行政编码）经 `t_regions` 解码（事实里的省级列未确认，漂移守卫禁读，不走捷径）。
/// 分类等未确认字段仍不进入图构建，相关问句安全回落。
struct DimEdges {
    /// (customer_code, 省区名)；统一合同 `region=省区`。
    customer_sales_region: Vec<(String, String)>,
    /// (customer_code, 省份名)；事实内 `state`。
    customer_province: Vec<(String, String)>,
}

type PurchaseEdge = (String, String, String, String, String, f64, f64);

/// 由上层业务注册表投影进 AGE 的单据族。connector 只负责通用图写入，
/// 不持有 DMS 单据前缀与表名，避免在 IO 层复制业务真相源。
#[derive(Debug, Clone)]
pub struct DocumentGraphSpec {
    pub code: String,
    pub name: String,
    pub header_table: String,
    pub detail_tables: Vec<String>,
}

/// 静态数仓资产目录投影。所有字段由上层语义目录提供；connector 不发现表、
/// 不查询生产库，也不推断表间血缘或 JOIN。
#[derive(Debug, Clone)]
pub struct WarehouseAssetGraphSpec {
    pub code: String,
    pub table: String,
    pub database: String,
    pub name: String,
    pub layer: String,
    pub domain: String,
    pub grain: String,
    pub time_rule: String,
    pub metrics: String,
    pub forbidden: String,
    pub comparison: String,
    pub default_sales: bool,
}

/// 从 Doris DWS 聚合客户-商品购买边，重建 AGE 图（幂等：先 drop 再建）。
/// 返回值仍是 `(客户数, 商品数, 购买边数)` 三元组 —— 维度节点/边的计数走 `tracing::info!`，
/// 不为两个数字改签名和它的调用点。
pub async fn sync(
    mysql: &ReadOnlyMySql,
    pg: &PgPool,
    documents: &[DocumentGraphSpec],
    warehouse_assets: &[WarehouseAssetGraphSpec],
) -> anyhow::Result<(usize, usize, usize)> {
    let _sync = sync_lock().lock().await;
    anyhow::ensure!(mysql.is_warehouse(), "关系图谱只允许从显式数仓目标构建");
    let target = mysql.target_name();
    let generation = availability()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .begin_sync(&target);
    // DWS 已统一正向销售和负向退货；图上的金额、销量与问数使用同一事实合同。
    // 目录中没有“客户×商品”同粒度且同口径的 ADS，不能拿省区月汇总替代购买关系。
    // 保留全期准确口径，但给源查询明确执行预算和聚合边硬上限；超限整轮失败，不截半张图。
    let edges: Vec<PurchaseEdge> = tokio::time::timeout(
        GRAPH_SOURCE_TIMEOUT,
        mysql.raw_all(
             "SELECT /*+ SET_VAR(query_timeout=120) */
                    sf.storecode, COALESCE(MAX(sf.storename),''),
                    sf.skucode, COALESCE(MAX(sf.skuname),''),
                    COALESCE(NULLIF(TRIM(sf.region),''),''),
                    CAST(COALESCE(SUM(sf.amount),0) AS DOUBLE),
                    CAST(COALESCE(SUM(sf.qty),0) AS DOUBLE)
             FROM sales_dw.dws_off_offline_sale_dfn sf
             WHERE sf.storecode IS NOT NULL AND TRIM(sf.storecode) <> ''
               AND sf.skucode IS NOT NULL AND TRIM(sf.skucode) <> ''
             GROUP BY sf.storecode, sf.skucode,
                      COALESCE(NULLIF(TRIM(sf.region),''),'')
             LIMIT 250001",
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("graph source query exceeded 120s budget"))??;
    anyhow::ensure!(
        edges.len() <= GRAPH_EDGE_LIMIT,
        "graph source exceeded {GRAPH_EDGE_LIMIT} aggregate edges: got {}",
        edges.len()
    );
    anyhow::ensure!(
        mysql.is_warehouse() && mysql.target_name() == target,
        "graph source target changed during aggregation"
    );
    // 省份对取客户主档 + 行政区字典（`t_customer.province` 是行政编码；事实里的省级列
    // 未确认、漂移守卫禁读，不走捷径）；主边查询按省区聚合，不为它加一维。
    let province_pairs: Vec<(String, String)> = tokio::time::timeout(
        GRAPH_SOURCE_TIMEOUT,
        mysql.raw_all(
             "SELECT /*+ SET_VAR(query_timeout=120) */ c.customer_code, r.region_name               FROM dms_ods.t_customer c               JOIN dms_ods.t_regions r ON r.region_code = c.province AND r.deleted_flag = 0               WHERE c.deleted_flag = 0 AND c.province IS NOT NULL AND TRIM(c.province) <> ''               LIMIT 250001",
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("graph province query exceeded 120s budget"))??;
    anyhow::ensure!(
        province_pairs.len() <= GRAPH_EDGE_LIMIT,
        "graph province query exceeded {GRAPH_EDGE_LIMIT} rows: got {}",
        province_pairs.len()
    );
    let dims = DimEdges {
        // 先借用去重再 clone：≤250k 行不为每个幸存者白付一次 String 分配
        customer_sales_region: edges
            .iter()
            .filter(|(_, _, _, _, region, _, _)| !region.is_empty())
            .map(|(code, _, _, _, region, _, _)| (code.as_str(), region.as_str()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .map(|(code, region)| (code.to_string(), region.to_string()))
            .collect(),
        customer_province: province_pairs
            .into_iter()
            .filter(|(_, state)| !state.is_empty())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect(),
    };

    // 去重节点（25 万行级：命中也白 clone key 的 entry(...).or_insert_with 形态不用）
    use std::collections::HashMap;
    let mut customers: HashMap<String, String> = HashMap::new();
    let mut goods: HashMap<String, String> = HashMap::new();
    for (cc, cn, gc, gn, _, _, _) in &edges {
        if !customers.contains_key(cc) {
            customers.insert(cc.clone(), cn.clone());
        }
        if !goods.contains_key(gc) {
            goods.insert(gc.clone(), gn.clone());
        }
    }

    let mut conn = age_conn(pg).await?;
    // 重建图
    // 首跑「图不存在」是预期；其余错误留痕但不阻断 —— 紧接着 create_graph 会重建
    if let Err(e) =
        sqlx::query(&format!("SELECT drop_graph('{GRAPH}', true)")).execute(&mut *conn).await
    {
        tracing::debug!(err = %e, "drop_graph 失败（首跑图不存在属预期）");
    }
    sqlx::query(&format!("SELECT create_graph('{GRAPH}')")).execute(&mut *conn).await?;

    // 批量建节点（UNWIND inline）
    batch_nodes(&mut conn, "Customer", &customers).await?;
    batch_nodes(&mut conn, "Goods", &goods).await?;

    // 节点属性索引（建边 MATCH 提速）；索引缺失会让每条建边 MATCH 全表扫，失败必须 warn
    for label in ["Customer", "Goods"] {
        if let Err(e) = sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS {}_code_idx ON {GRAPH}.\"{label}\" \
             USING btree (agtype_access_operator(VARIADIC ARRAY[properties, '\"code\"'::agtype]))",
            label.to_lowercase()
        ))
        .execute(&mut *conn)
        .await
        {
            tracing::warn!(err = %e, label, "图节点索引创建失败，建边 MATCH 将退化为全表扫");
        }
    }

    // 批量建边
    batch_edges(&mut conn, &edges).await?;

    // ── 省区节点与边。**建在购买边之后**：它们只挂到已存在的
    //    Customer 上，图里没有的 code 静默跳过（`MATCH` 匹配不到就不建边）。
    //    这一段失败不让整次同步失败（warn-and-continue）—— 购买图（本体）已经建好，维度边是增强。
    let (sales_regions, provinces, n) = match batch_dim_edges(&mut conn, &dims).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(err = %e, "图维度边构建失败，购买图本体已建好，继续");
            (0, 0, 0)
        }
    };
    // 计数口径：dim_edges 是「尝试条数」，MATCH 不到的维度边静默跳过，实际建成可能更少
    tracing::info!(sales_regions, provinces, dim_edges = n, "图维度边已建（SalesRegion + Province）");

    let schema_edges = batch_document_schema(&mut conn, documents).await?;
    tracing::info!(families = documents.len(), schema_edges, "单据族主明细关系已写入图谱");

    let catalog_edges = batch_warehouse_catalog(&mut conn, warehouse_assets).await?;
    tracing::info!(
        assets = warehouse_assets.len(),
        catalog_edges,
        "静态数仓资产目录已写入图谱"
    );

    anyhow::ensure!(
        mysql.is_warehouse() && mysql.target_name() == target,
        "graph source target changed during AGE rebuild"
    );
    // 先落持久化就绪标记再置内存态：标记写失败时进程返回 Err 且内存态未 ready，
    // 两半不会出现「内存 ready 但标记缺席」的不一致。标记只由完整重建成功写入 ——
    // 下次重建的 drop_graph 会先抹掉它，「没有标记」就是「不可信」（见 `adopt_if_current`）。
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ CREATE (:GraphMeta {{target:'{}'}}) $$) AS (v agtype)",
        esc(&target)
    );
    sqlx::query(&cy).execute(&mut *conn).await?;
    let ready = availability()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .mark_ready(&target, generation);
    anyhow::ensure!(ready, "graph target generation changed during AGE rebuild");
    Ok((customers.len(), goods.len(), edges.len()))
}

/// 零分配的「code == db.table」校验（每资产一次 format! 是纯浪费）
fn is_qualified_code(code: &str, db: &str, table: &str) -> bool {
    code.len() == db.len() + 1 + table.len()
        && code.starts_with(db)
        && code.as_bytes()[db.len()] == b'.'
        && code.ends_with(table)
}

/// 静态目录只投影资产自身、分层、业务域和默认销售合同。
/// 没有经过目录明确声明的表间血缘与 JOIN 一律不进入 AGE。
async fn batch_warehouse_catalog(
    conn: &mut sqlx::PgConnection,
    assets: &[WarehouseAssetGraphSpec],
) -> anyhow::Result<usize> {
    let mut codes = std::collections::HashSet::new();
    for asset in assets {
        anyhow::ensure!(
            is_qualified_code(&asset.code, &asset.database, &asset.table),
            "warehouse asset code must be fully qualified"
        );
        anyhow::ensure!(
            matches!(asset.layer.as_str(), "ODS" | "DWD" | "DWS" | "ADS" | "OTHER"),
            "warehouse asset has unsupported data layer"
        );
        anyhow::ensure!(codes.insert(asset.code.as_str()), "duplicate warehouse asset code");
    }
    anyhow::ensure!(
        assets.iter().filter(|asset| asset.default_sales).count() == 1,
        "warehouse catalog must declare exactly one default-sales asset"
    );

    for chunk in assets.chunks(100) {
        let list = chunk
            .iter()
            .map(|asset| {
                format!(
                    "{{code:'{}',table:'{}',database:'{}',name:'{}',layer:'{}',domain:'{}',grain:'{}',time_rule:'{}',metrics:'{}',forbidden:'{}',comparison:'{}',default_sales:{}}}",
                    esc(&asset.code),
                    esc(&asset.table),
                    esc(&asset.database),
                    esc(&asset.name),
                    esc(&asset.layer),
                    esc(&asset.domain),
                    esc(&asset.grain),
                    esc(&asset.time_rule),
                    esc(&asset.metrics),
                    esc(&asset.forbidden),
                    esc(&asset.comparison),
                    asset.default_sales,
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let cy = format!(
            "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS r \
             MERGE (a:WarehouseAsset {{code:r.code}}) \
             SET a.table=r.table, a.database=r.database, a.name=r.name, a.layer=r.layer, \
                 a.domain=r.domain, a.grain=r.grain, a.time_rule=r.time_rule, \
                 a.metrics=r.metrics, a.forbidden=r.forbidden, a.comparison=r.comparison, \
                 a.default_sales=r.default_sales $$) AS (v agtype)"
        );
        sqlx::query(&cy).execute(&mut *conn).await?;
    }

    // 分层节点就是固定的 5 层（上面已校验 ∈ 5 层，资产侧 insert 永远插不进新值，不收）
    let layers = ["ODS", "DWD", "DWS", "ADS", "OTHER"]
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let mut domains = std::collections::HashSet::new();
    for asset in assets {
        domains.insert(asset.domain.as_str());
    }
    batch_named_catalog_nodes(conn, "DataLayer", layers).await?;
    batch_named_catalog_nodes(conn, "BusinessDomain", domains).await?;
    // MetricContract 节点不在此预建：下面 USES_AS_DEFAULT 的 MERGE + SET name 一次到位

    let list = assets
        .iter()
        .map(|asset| {
            format!(
                "{{code:'{}',layer:'{}',domain:'{}'}}",
                esc(&asset.code),
                esc(&asset.layer),
                esc(&asset.domain)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS r \
         MATCH (a:WarehouseAsset {{code:r.code}}), \
               (l:DataLayer {{code:r.layer}}), \
               (d:BusinessDomain {{code:r.domain}}) \
         MERGE (a)-[:IN_LAYER]->(l) \
         MERGE (a)-[:IN_DOMAIN]->(d) $$) AS (v agtype)"
    );
    sqlx::query(&cy).execute(&mut *conn).await?;

    let default_code =
        esc(&assets.iter().find(|asset| asset.default_sales).expect("已 ensure 恰好一个").code);
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ \
         MATCH (a:WarehouseAsset {{code:'{default_code}'}}) \
         MERGE (c:MetricContract {{code:'default_sales'}}) \
         SET c.name='默认销售额' \
         MERGE (c)-[:USES_AS_DEFAULT]->(a) $$) AS (v agtype)"
    );
    sqlx::query(&cy).execute(&mut *conn).await?;

    // 索引必须建在**所有标签都已存在**之后：AGE 里 `CREATE INDEX ON graph."Label"` 对还不存在的
    // 标签恒报 relation does not exist（实测：MetricContract 的 MERGE 在上面才首次发生，
    // 索引循环放它前面 = 每轮同步稳定失败一次、告警噪音 + 该标签永远没索引）。
    for label in ["WarehouseAsset", "DataLayer", "BusinessDomain", "MetricContract"] {
        if let Err(e) = sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS {}_code_idx ON {GRAPH}.\"{label}\" \
             USING btree (agtype_access_operator(VARIADIC ARRAY[properties, '\"code\"'::agtype]))",
            label.to_lowercase()
        ))
        .execute(&mut *conn)
        .await
        {
            tracing::warn!(err = %e, label, "图目录索引创建失败，MATCH 将退化为全表扫");
        }
    }

    // 返回「尝试数」而非实建数（IN_LAYER/IN_DOMAIN/USES_AS_DEFAULT 各一 + 资产数），仅作日志口径
    Ok(assets.len() * 2 + 1)
}

async fn batch_named_catalog_nodes<'a>(
    conn: &mut sqlx::PgConnection,
    label: &str,
    values: std::collections::HashSet<&'a str>,
) -> anyhow::Result<()> {
    let values = values.into_iter().collect::<Vec<_>>();
    for chunk in values.chunks(200) {
        let list = chunk
            .iter()
            .map(|value| {
                let v = esc(value);
                format!("{{code:'{v}',name:'{v}'}}")
            })
            .collect::<Vec<_>>()
            .join(",");
        let cy = format!(
            "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS r \
             MERGE (n:{label} {{code:r.code}}) SET n.name=r.name $$) AS (v agtype)"
        );
        sqlx::query(&cy).execute(&mut *conn).await?;
    }
    Ok(())
}

/// 单据族 → 主表/明细表。节点属性沿用 `code/name`，可复用已有批量建点函数。
async fn batch_document_schema(
    conn: &mut sqlx::PgConnection,
    documents: &[DocumentGraphSpec],
) -> anyhow::Result<usize> {
    use std::collections::HashMap;

    let mut families: HashMap<String, String> = HashMap::new();
    for d in documents {
        // 与目录侧（duplicate warehouse asset code）同口径：重复 code 显式拒绝，不静默 last-wins
        anyhow::ensure!(
            families.insert(d.code.clone(), d.name.clone()).is_none(),
            "duplicate document family code"
        );
    }
    let mut tables: HashMap<String, String> = HashMap::new();
    for d in documents {
        for t in std::iter::once(&d.header_table).chain(d.detail_tables.iter()) {
            if !tables.contains_key(t) {
                tables.insert(t.clone(), t.clone());
            }
        }
    }
    batch_nodes(conn, "DocumentFamily", &families).await?;
    batch_nodes(conn, "BusinessTable", &tables).await?;

    let mut count = 0;
    for (rel, pairs) in [
        (
            "HEADER_TABLE",
            documents
                .iter()
                .map(|d| (&d.code, &d.header_table))
                .collect::<Vec<_>>(),
        ),
        (
            "DETAIL_TABLE",
            documents
                .iter()
                .flat_map(|d| d.detail_tables.iter().map(|t| (&d.code, t)))
                .collect::<Vec<_>>(),
        ),
    ] {
        for chunk in pairs.chunks(200) {
            let list = chunk
                .iter()
                .map(|(family, table)| format!("{{f:'{}',t:'{}'}}", esc(family), esc(table)))
                .collect::<Vec<_>>()
                .join(",");
            let cy = format!(
                "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS e \
                 MATCH (f:DocumentFamily {{code:e.f}}), (t:BusinessTable {{code:e.t}}) \
                 CREATE (f)-[:{rel}]->(t) $$) AS (v agtype)"
            );
            sqlx::query(&cy).execute(&mut *conn).await?;
            // 口径：尝试条数（CREATE 全量成功时 == 实建数）；日志 schema_edges 按此解读
            count += chunk.len();
        }
    }
    Ok(count)
}

/// 建 SalesRegion 节点与客户省区边。
///
/// 只对**图里已有**的 Goods/Customer 建边：`MATCH` 匹配不到就什么也不做。
/// 那是刻意的 —— `t_goods` 有 4 万多个 SKU，而购买图里只有 455 个真的被买过；
/// 为没被买过的 SKU 建节点只会让图变大而一条查询也用不上。
async fn batch_dim_edges(
    conn: &mut sqlx::PgConnection,
    dims: &DimEdges,
) -> anyhow::Result<(usize, usize, usize)> {
    use std::collections::HashSet;
    let mut n = 0usize;
    for (label, rel, pairs) in [
        ("SalesRegion", "IN_SALES_REGION", &dims.customer_sales_region),
        ("Province", "IN_PROVINCE", &dims.customer_province),
    ] {
        let names: HashSet<&str> = pairs.iter().map(|(_, name)| name.as_str()).collect();
        let items: Vec<&str> = names.into_iter().collect();
        // chunk 尺寸（节点 1000 / 目录 200 / 边 500）是按各语句文本长度的经验值，无更深层依据
        for chunk in items.chunks(1000) {
            let list: String = chunk
                .iter()
                .map(|name| format!("{{name:'{}'}}", esc(name)))
                .collect::<Vec<_>>()
                .join(",");
            let cy = format!(
                "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS r \
                 CREATE (:{label} {{name:r.name}}) $$) AS (v agtype)"
            );
            sqlx::query(&cy).execute(&mut *conn).await?;
        }
        if let Err(e) = sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS {}_name_idx ON {GRAPH}.\"{}\" \
             USING btree (agtype_access_operator(VARIADIC ARRAY[properties, '\"name\"'::agtype]))",
            label.to_lowercase(), label
        ))
        .execute(&mut *conn)
        .await
        {
            tracing::warn!(err = %e, label, "图维度索引创建失败，MATCH 将退化为全表扫");
        }
        for chunk in pairs.chunks(500) {
            let list: String = chunk
                .iter()
                .map(|(code, name)| format!("{{c:'{}',d:'{}'}}", esc(code), esc(name)))
                .collect::<Vec<_>>()
                .join(",");
            let cy = format!(
                "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS e \
                 MATCH (s:Customer {{code:e.c}}), (d:{label} {{name:e.d}}) \
                 CREATE (s)-[:{rel}]->(d) $$) AS (v agtype)"
            );
            sqlx::query(&cy).execute(&mut *conn).await?;
            n += chunk.len();
        }
    }
    let sales_regions =
        dims.customer_sales_region.iter().map(|(_, r)| r).collect::<HashSet<_>>().len();
    let provinces =
        dims.customer_province.iter().map(|(_, p)| p).collect::<HashSet<_>>().len();
    Ok((sales_regions, provinces, n))
}

async fn batch_nodes(
    conn: &mut sqlx::PgConnection,
    label: &str,
    nodes: &std::collections::HashMap<String, String>,
) -> anyhow::Result<()> {
    let items: Vec<(&String, &String)> = nodes.iter().collect();
    for chunk in items.chunks(1000) {
        let list: String = chunk
            .iter()
            .map(|(code, name)| format!("{{code:'{}',name:'{}'}}", esc(code), esc(name)))
            .collect::<Vec<_>>()
            .join(",");
        let cy = format!(
            "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS r CREATE (:{label} {{code:r.code, name:r.name}}) $$) AS (v agtype)"
        );
        sqlx::query(&cy).execute(&mut *conn).await?;
    }
    Ok(())
}

async fn batch_edges(
    conn: &mut sqlx::PgConnection,
    edges: &[PurchaseEdge],
) -> anyhow::Result<()> {
    for chunk in edges.chunks(500) {
        let list: String = chunk
            .iter()
            .map(|(cc, _, gc, _, region, amt, qty)| {
                format!(
                    "{{c:'{}',g:'{}',r:'{}',a:{:.4},q:{:.4}}}",
                    esc(cc), esc(gc), esc(region), amt, qty
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let cy = format!(
            "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS e \
             MATCH (c:Customer {{code:e.c}}), (g:Goods {{code:e.g}}) \
             CREATE (c)-[:BOUGHT {{region:e.r, amount:e.a, qty:e.q}}]->(g) $$) AS (v agtype)"
        );
        sqlx::query(&cy).execute(&mut *conn).await?;
    }
    Ok(())
}

/// agtype 值去引号（AGE 返回 "xxx" 带引号字符串）。
/// 只剥一层成对外引号：`trim_matches('"')` 会把名字里真·首尾引号全剥掉，读写口径按一层对齐。
fn unquote(s: &str) -> String {
    let s = s.trim();
    match s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\""),
        None => s.to_string(),
    }
}

pub struct GraphRow {
    pub code: String,
    pub name: String,
    pub amount: f64,
}

/// 买过某商品（名称模糊）的客户 TOP N，按购买额降序
pub async fn buyers_of_goods(pg: &PgPool, goods_name: &str, limit: usize) -> anyhow::Result<Vec<GraphRow>> {
    let limit = limit.clamp(1, GRAPH_QUERY_MAX_LIMIT);
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ \
         MATCH (c:Customer)-[b:BOUGHT]->(g:Goods) WHERE g.name =~ '.*{}.*' \
         RETURN c.code, c.name, sum(b.amount) ORDER BY sum(b.amount) DESC LIMIT {limit} \
         $$) AS (code agtype, name agtype, amount agtype)",
        esc_regex(goods_name)
    );
    fetch_graph_rows(pg, &cy).await
}

/// 某客户（名称模糊）买过的商品 TOP N，按购买额降序
pub async fn goods_of_customer(pg: &PgPool, customer_name: &str, limit: usize) -> anyhow::Result<Vec<GraphRow>> {
    let limit = limit.clamp(1, GRAPH_QUERY_MAX_LIMIT);
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ \
         MATCH (c:Customer)-[b:BOUGHT]->(g:Goods) WHERE c.name =~ '.*{}.*' \
         RETURN g.code, g.name, sum(b.amount) ORDER BY sum(b.amount) DESC LIMIT {limit} \
         $$) AS (code agtype, name agtype, amount agtype)",
        esc_regex(customer_name)
    );
    fetch_graph_rows(pg, &cy).await
}

/// 买过 X 商品的客户还买了什么（共购推荐）：两跳
pub async fn copurchase(pg: &PgPool, goods_name: &str, limit: usize) -> anyhow::Result<Vec<GraphRow>> {
    let limit = limit.clamp(1, GRAPH_QUERY_MAX_LIMIT);
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ \
         MATCH (c:Customer)-[:BOUGHT]->(g1:Goods) WHERE g1.name =~ '.*{}.*' \
         WITH DISTINCT c \
         MATCH (c)-[b2:BOUGHT]->(g2:Goods) WHERE NOT g2.name =~ '.*{}.*' \
         RETURN g2.code, g2.name, sum(b2.amount) ORDER BY sum(b2.amount) DESC LIMIT {limit} \
         $$) AS (code agtype, name agtype, amount agtype)",
        esc_regex(goods_name), esc_regex(goods_name)
    );
    fetch_graph_rows(pg, &cy).await
}

/// 一个词在图里**是什么**。`None` = 图里没有这个东西。
///
/// 🔴 这是「消化了词却不装过滤」那一族的解药。今天 `strip_relation_words` 剥完
/// 「湖南省买过烤肠的客户」剩下 `湖南省烤肠`，系统把它整体当商品名去模糊匹配 →
/// 0 行 → 回落。问题不在剥词，在**剥完之后没人问过「剩下的这坨到底是什么」**。
///
/// 解析而不是猜：候选词只对已验证的 `SalesRegion.name`、`Province.name`（事实 `state`）
/// 与 `Goods.name` 查询。分类等未验证维度不进入类型系统；解析不出来就回落。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entity {
    Goods(String),
    SalesRegion(String),
    Province(String),
}

/// 闭集标签先于开集 Goods：省区、省份都是精确匹配，商品是模糊确认。
const RESOLVABLE_LABELS: [&str; 3] = ["SalesRegion", "Province", "Goods"];

/// 把一段残留文本切成候选词并逐个解析。返回**互不重叠**的解析结果，按在原文里出现的位置排序。
///
/// 切法：长词优先的滑窗（2..=8 字），命中即占位，后续候选不许与已占区间重叠 ——
/// 那样「湖南省烤肠」可解析成 `[SalesRegion(湖南省), Goods(烤肠)]`，而不是把重叠短词也算上。
/// 上限 8 字由共用滑窗函数控制；未验证维度不会因此获得图查询能力。
pub async fn resolve_entities(pg: &PgPool, text: &str) -> anyhow::Result<Vec<Hit>> {
    let mut taken = vec![false; text.chars().count()];
    let mut out: Vec<Hit> = vec![];
    for (start, w) in candidate_windows(text) {
        let end = start + w.chars().count();
        if taken[start..end].iter().any(|t| *t) {
            continue;
        }
        if let Some(entity) = resolve_one(pg, &w).await? {
            taken[start..end].iter_mut().for_each(|t| *t = true);
            out.push(Hit { start, window: w, entity });
        }
    }
    // 必须按 start 排序：candidate_windows 按「长度外层、起点内层」产出，本就不按位置有序；
    // 重叠占位判据（taken）与输出契约都依赖位置序 —— 这行看着冗余，删不得。
    out.sort_by_key(|h| h.start);
    Ok(out)
}

/// 一次命中：**匹配到的窗口**（原文里的那几个字）+ 解析出的实体。
///
/// 🔴 `window` 是承重字段，不是调试信息。覆盖率自检必须按**窗口**算，不能按实体名算 ——
/// 实测的静默错答就出在这里：「湖南省烤肠」里窗口「烤肠」模糊匹到了
/// `皇家小虎黑猪肉烤肠（原味）0500G00`，按实体名算 covered=13 ≥ 5，
/// 覆盖率判据当场被绕过，「湖南省」整个被丢掉，用户拿到**全国** 27 个客户，
/// route 还是 `graph`、行数看着很正常。按窗口算则 covered=2 < 5 → 拒 → 回落。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// 窗口在原文里的起始字符下标
    pub start: usize,
    /// 原文里被这次命中吃掉的那几个字
    pub window: String,
    pub entity: Entity,
}

/// 单个词的解析。省区闭集精确匹配优先，商品开集模糊匹配最后。
async fn resolve_one(pg: &PgPool, word: &str) -> anyhow::Result<Option<Entity>> {
    // 一次解析复用同一条连接：逐 label 重新 age_conn = 每窗口 3 倍的 acquire+LOAD+SET
    let mut conn = age_conn(pg).await?;
    for label in RESOLVABLE_LABELS {
        let exact = label != "Goods";
        let cond = if exact {
            format!("x.name = '{}'", esc(word))
        } else {
            format!("x.name =~ '.*{}.*'", esc_regex(word))
        };
        let cy = format!(
            "SELECT * FROM cypher('{GRAPH}', $$ MATCH (x:{label}) WHERE {cond} \
             RETURN x.name LIMIT 1 $$) AS (name agtype)"
        );
        let wrapped = format!("SELECT name::text FROM ({cy}) AS sub");
        // 标签还不存在（旧图没跑过新版 sync）时 AGE 会报错 —— 那不是解析失败，是图没建全，
        // 所以 `Err` 一律当「这个标签没有」继续试下一个，而不是让整条问答失败。
        let rows = match sqlx::query(&wrapped).fetch_all(&mut *conn).await {
            Ok(r) => r,
            Err(e) => {
                // 标签不存在与真实 DB 错误都落这里：留痕后继续试下一个标签
                tracing::debug!(err = %e, label, "图标签查询失败，按「该标签没有」继续");
                continue;
            }
        };
        if let Some(r) = rows.first() {
            let name: String = unquote(&cell_text(r, 0));
            if name.is_empty() {
                continue;
            }
            return Ok(Some(match label {
                // 闭集：解析出的精确省区/省份名就是要装入过滤的值。
                "SalesRegion" => Entity::SalesRegion(name),
                "Province" => Entity::Province(name),
                // 🔴 开集：存**用户说的那个词**，不是图里匹到的那条商品名。
                //
                // 解析对 Goods 的作用只是「确认这个词在商品名里出现过」，**不是定位到某个商品**。
                // 存图里的名字会把问句悄悄缩窄：实测「湖南省买过烤肠的客户」里的「烤肠」
                // 被 `LIMIT 1` 取到了 `合马双层烤肠机`（一台机器），
                // 于是「买过烤肠的客户」变成「买过那台烤肠机的客户」，行数从 50 掉到 1。
                // 取到哪一条还取决于图的物理行序 —— 同一个问句在不同部署上答案不同。
                _ => Entity::Goods(word.to_string()),
            }));
        }
    }
    Ok(None)
}

/// 带限定的「买过 X 的客户」：当前只支持商品名 + 省区，全部是 AND。
///
/// 至少要有一个商品侧限定，否则这就是「列出所有客户」——
/// 那不是关系问句，返回 `Ok(vec![])` 让调用方回落。
pub async fn buyers_filtered(
    pg: &PgPool,
    goods: Option<&str>,
    sales_region: Option<&str>,
    province: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<GraphRow>> {
    match buyers_cypher(goods, sales_region, province, limit) {
        Some(cy) => fetch_graph_rows(pg, &cy).await,
        None => Ok(vec![]),
    }
}

/// `buyers_filtered` 的 Cypher 组装。抽成纯函数是为了让「限定词真的装上了」可测 ——
/// 那条不变量的反面（装了一半）是**静默给出全国名单**，不报错、route 还是 `graph`。
fn buyers_cypher(
    goods: Option<&str>,
    sales_region: Option<&str>,
    province: Option<&str>,
    limit: usize,
) -> Option<String> {
    if goods.is_none() {
        return None; // 没有商品侧限定 = 「列出所有客户」，不是关系问句
    }
    let limit = limit.clamp(1, GRAPH_QUERY_MAX_LIMIT);
    let mut matches = vec!["MATCH (c:Customer)-[b:BOUGHT]->(g:Goods)"];
    let mut wheres: Vec<String> = vec![];
    // 商品名是开集 → 模糊（正则元字符先转义）；省区/省份是闭集且解析时已精确命中 → 相等。
    if let Some(g) = goods {
        wheres.push(format!("g.name =~ '.*{}.*'", esc_regex(g)));
    }
    if let Some(r) = sales_region {
        wheres.push(format!("b.region = '{}'", esc(r)));
    }
    if let Some(p) = province {
        matches.push("MATCH (c)-[:IN_PROVINCE]->(pv:Province)");
        wheres.push(format!("pv.name = '{}'", esc(p)));
    }
    Some(format!(
        "SELECT * FROM cypher('{GRAPH}', $$ {} WHERE {} \
         RETURN c.code, c.name, sum(b.amount) ORDER BY sum(b.amount) DESC LIMIT {limit} \
         $$) AS (code agtype, name agtype, amount agtype)",
        matches.join(" "),
        wheres.join(" AND ")
    ))
}

/// 图行文本列解码：失败留痕（debug）并按空串兜底 —— AGE ::text 形态漂移时不该静默变「空名字」
fn cell_text(r: &sqlx::postgres::PgRow, i: usize) -> String {
    match r.try_get::<Option<String>, _>(i) {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => {
            tracing::debug!(err = %e, col = i, "图行文本列解码失败，按空串兜底");
            String::new()
        }
    }
}

async fn fetch_graph_rows(pg: &PgPool, cypher: &str) -> anyhow::Result<Vec<GraphRow>> {
    let mut conn = age_conn(pg).await?;
    // agtype 类型 sqlx 不识别，外层包一层 ::text（string→带引号JSON、number→裸数字）
    let wrapped = format!("SELECT code::text, name::text, amount::text FROM ({cypher}) AS sub");
    let rows = sqlx::query(&wrapped).fetch_all(&mut *conn).await?;
    Ok(rows
        .iter()
        .map(|r| {
            let amt_s = cell_text(r, 2);
            GraphRow {
                code: unquote(&cell_text(r, 0)),
                name: unquote(&cell_text(r, 1)),
                amount: amt_s
                    .trim()
                    .parse()
                    .unwrap_or_else(|_| {
                        tracing::debug!(raw = %amt_s, "图行金额解析失败，按 0.0 兜底");
                        0.0
                    }),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_amount_is_null_safe() {
        let src = include_str!("graph.rs");
        assert!(src.contains("sales_dw.dws_off_offline_sale_dfn"));
        assert!(src.contains("CAST(COALESCE(SUM(sf.amount),0) AS DOUBLE)"));
        assert!(src.contains("CAST(COALESCE(SUM(sf.qty),0) AS DOUBLE)"));
        assert!(src.contains("LIMIT 250001"));
        assert!(src.contains("GRAPH_SOURCE_TIMEOUT"));
        assert!(!src.contains(concat!("JOIN ", "t_sales_order_detail")));
        assert!(!src.contains(concat!("sf.", "class2")));
        assert!(!src.contains(concat!("sf.", "state")));
    }

    #[test]
    fn graph_source_only_reads_confirmed_sales_fact_columns() {
        let src = include_str!("graph.rs");
        let body = src
            .split("pub async fn sync(")
            .nth(1)
            .expect("sync missing")
            .split("// 去重节点")
            .next()
            .expect("sync source section missing");
        let allowed = [
            "order_date",
            "storecode",
            "storename",
            "skucode",
            "skuname",
            "war_zone",
            "region",
            "qty",
            "amount",
            "cost_excluding_tax",
            "revenue_excluding_tax",
            "gross_profit",
        ];
        for access in body.match_indices("sf.").map(|(at, _)| {
            body[at + 3..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
        }) {
            assert!(allowed.contains(&access.as_str()), "unconfirmed sales fact field: {access}");
        }
    }

    #[test]
    fn graph_availability_is_target_and_generation_bound() {
        let mut state = GraphAvailability::default();
        let generation = state.begin_sync("warehouse_a");
        assert!(state.lease("warehouse_a").is_none());
        assert!(state.mark_ready("warehouse_a", generation));
        let lease = state.lease("warehouse_a").expect("同步成功后该可用");
        assert!(state.lease("warehouse_b").is_none());
        state.invalidate("warehouse_b");
        assert!(state.lease("warehouse_a").is_none());
        assert_ne!(state.generation, lease.generation);
    }

    #[test]
    fn entity_resolution_only_reads_confirmed_graph_labels() {
        assert_eq!(RESOLVABLE_LABELS, ["SalesRegion", "Province", "Goods"]);
    }

    #[test]
    fn esc_quotes() {
        assert_eq!(esc("O'Brien"), "O\\'Brien");
    }

    #[test]
    fn esc_strips_backslash_and_control_chars() {
        assert_eq!(esc("a\\nb"), "anb", "剥除是防 AGE 转义歧义的刻意取舍");
        assert_eq!(esc("a\nb"), "ab", "控制字符可撑破 Cypher 字面量，必须剥");
    }

    #[test]
    fn esc_regex_escapes_metacharacters() {
        assert_eq!(esc_regex("A.B"), "A\\.B");
        assert_eq!(esc_regex("C++"), "C\\+\\+");
        assert_eq!(esc_regex("O'Brien"), "O\\'Brien", "引号仍按字面量转义");
        assert_eq!(esc_regex("烤肠"), "烤肠");
        let q = buyers_cypher(Some("A.B"), None, None, 50).unwrap();
        assert!(q.contains("g.name =~ '.*A\\.B.*'"), "{q}");
    }

    #[test]
    fn unquote_agtype() {
        assert_eq!(unquote("\"恒众\""), "恒众");
        assert_eq!(unquote("恒众"), "恒众");
    }

    #[test]
    fn document_graph_spec_keeps_header_and_details_separate() {
        let d = DocumentGraphSpec {
            code: "sales_order".into(),
            name: "销售订单".into(),
            header_table: "t_sales_order".into(),
            detail_tables: vec!["t_sales_order_detail".into()],
        };
        assert_ne!(d.header_table, d.detail_tables[0]);
    }

    /// 🔴 长词优先的判据在 kernel 随函数走（`nl::text::windows_try_long_words_first`）——
    /// 函数已上提到 `dms_kernel::nl::text::candidate_windows`，与 SQL 路径切片召回共用。
    #[test]
    fn windows_contract_lives_in_kernel() {
        // 本文件只钉「用的是 kernel 那份」：行为判据（长词优先/边界/覆盖）不抄第二遍
        let ws = candidate_windows("湖南省烤肠");
        assert!(ws.first().map(|(_, w)| w == "湖南省烤肠").unwrap_or(false), "长词优先没了");
    }

    /// 限定词必须**真的装进 Cypher**。反面（装了一半）会静默给出全国名单。
    #[test]
    fn cypher_carries_every_filter() {
        // ① 只有商品：一个 MATCH、模糊匹配
        let g = buyers_cypher(Some("烤肠"), None, None, 50).unwrap();
        assert!(g.contains("g.name =~ '.*烤肠.*'"), "{g}");
        assert_eq!(g.matches("MATCH").count(), 1, "{g}");
        // ② 商品 + 省区：同一购买边上的省区属性。
        let gr = buyers_cypher(Some("烤肠"), Some("湘北省区"), None, 50).unwrap();
        assert!(gr.contains("b.region = '湘北省区'"), "{gr}");
        assert!(gr.contains(" AND "), "两个限定必须 AND 连：{gr}");
        // ②b 商品 + 省份：Province 节点上的 AND 过滤，与省区互不占位
        let gp = buyers_cypher(Some("烤肠"), None, Some("湖南省"), 50).unwrap();
        assert!(gp.contains("IN_PROVINCE") && gp.contains("pv.name = '湖南省'"), "{gp}");
        assert!(gp.contains("g.name =~ '.*烤肠.*' AND pv.name"), "{gp}");
        // ③ 没有商品侧限定 = 「列出所有客户」，不是关系问句 → 不许产 SQL
        assert!(buyers_cypher(None, Some("湘北省区"), None, 50).is_none());
        assert!(buyers_cypher(None, None, Some("湖南省"), 50).is_none());
        assert!(buyers_cypher(None, None, None, 50).is_none());
        // ④ 单引号必须被转义（实体名来自图，但图里的名字是业务数据，不是可信输入）
        let q = buyers_cypher(Some("O'Brien"), None, None, 50).unwrap();
        assert!(q.contains("O\\'Brien"), "{q}");
    }
}
