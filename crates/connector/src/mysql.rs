//! 生产 MySQL 只读源 —— 只读红线的**结构性**载体。
//!
//! 红线不是纪律而是形状：
//! 1. `connect()` 是唯一构造入口，它给池装了 `after_connect`——**每条**连接建立时
//!    `SET SESSION TRANSACTION READ ONLY`。代码层再有疏漏，写语句也会被 MySQL 自己拒掉。
//! 2. `pool` 字段私有且**没有任何返回 `&MySqlPool` 的方法**：拿不到裸池就写不出第二条取数路径。
//!    框架自查走 `fixed(&'static str)`；分析取数走 `SqlSource::fetch(&ScopedSql)`，切到生产
//!    MySQL 时会在本边界再次套严格轻查询闸门；业务点查也可走 `fetch_dms_lookup`，两条路径
//!    的 AST 形状、索引列与 LIMIT 都由 kernel 锁死。
//! 3. 生产点查必须显式最小投影；结果列仍执行敏感列置空，作为最后一道防线。
//!
//! 取值/脱敏/探针三段判定逐行搬自 `server/src/pipeline.rs` 与 `server/src/meta.rs`
//! （DECIMAL→字符串保精度、日期格式化、`max` 行截断、`CAST(... AS CHAR)` 两个坑一处不改）。

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::time::Duration;

use serde_json::Value;
use sqlx::mysql::MySqlConnectOptions;
use sqlx::{Column, FromRow, Row, TypeInfo};

use dms_kernel::sql::dms_lookup::{
    gate_dms_scoped_with, registered_lookup_keys, registered_lookup_kind, DmsIndexKind,
    DmsLookupSql, DMS_LOOKUP_MAX_ROWS,
};
use dms_kernel::{BoxFut, Dialect, DsId, MysqlDialect, ScopedSql};

use crate::error::ConnectorError;
use crate::fixed::FixedStmt;
use crate::source::{ColumnInfo, DsPolicy, RowSet, SchemaSnapshot, SourceKind, SqlSource, TableInfo};

pub const DMS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);
pub const DMS_LOOKUP_MAX_CONCURRENCY: u32 = 2;
/// 生产点查索引的启动核验预算：一次性、逐表 SHOW INDEX，与用户点查的 2s 红线分开。
const LOOKUP_INDEX_VERIFY_TIMEOUT: Duration = Duration::from_secs(30);
// 数仓目录探针只读 information_schema。公网链路实测单条 ~27s（2026-08-08 切换公网后），
// 10s 在内网够用、公网必超时 —— 探针失败是启动硬失败，超时要按链路预算给。
const WAREHOUSE_CATALOG_TIMEOUT: Duration = Duration::from_secs(60);

fn production_lookup_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(DMS_LOOKUP_MAX_CONCURRENCY as usize))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MysqlCapability {
    ProductionLookup,
    /// 仅限 DMS 登录、角色与数据权限静态 SQL。它仍按生产 MySQL 建立只读、2 秒会话，
    /// 但允许 `fixed()`；业务点查仍必须走 `DmsLookupSql`。
    IdentityPermission,
    Warehouse,
}

impl MysqlCapability {
    pub fn is_warehouse(self) -> bool {
        matches!(self, Self::Warehouse)
    }
}

/// 数仓跨库元数据白名单项。字段保持私有，调用方只能通过静态构造器登记库表名；
/// 实际探针前还会再次执行 ASCII 标识符校验，不能把请求参数拼进 information_schema SQL。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarehouseAsset {
    database: &'static str,
    table: &'static str,
}

impl WarehouseAsset {
    pub const fn new(database: &'static str, table: &'static str) -> Self {
        Self { database, table }
    }

    pub const fn database(self) -> &'static str {
        self.database
    }

    pub const fn table(self) -> &'static str {
        self.table
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WarehouseCatalogStats {
    pub requested: usize,
    pub tables: usize,
    pub columns: usize,
    pub missing: usize,
}

pub struct ReadOnlyMySql {
    /// 🔴 业务库热切换（`/api/admin/db-target`）：池在内置锁里，`swap_pool` 验证后整个换掉。
    /// `MySqlPool` 本身是 Arc 克隆 —— 在飞的查询握旧克隆自然收尾，新查询拿新池，无中断窗口。
    pool: std::sync::Arc<std::sync::RwLock<PoolState>>,
    ds: DsId,
    sensitive: &'static [&'static str],
    /// 【A8】数据源级查询策略。独立于 `PoolState`：热切换换的是池，策略跟着源走。
    ds_policy: std::sync::Mutex<DsPolicy>,
}

impl ReadOnlyMySql {
    /// 唯一构造入口。`sensitive` = 敏感列词表（`dms_kernel::nl::lexicon::SENSITIVE_COLS`），
    /// `&'static` 是为了让它只能来自代码里写死的词表，不可能来自请求参数。
    pub async fn connect(
        ds: DsId,
        url: &str,
        max_conn: u32,
        sensitive: &'static [&'static str],
        capability: MysqlCapability,
    ) -> Result<Self, ConnectorError> {
        let (pool, lookup_indexes) =
            connect_read_only(&ds, url, max_conn, capability).await?;
        Ok(Self {
            pool: std::sync::Arc::new(std::sync::RwLock::new(PoolState {
                pool,
                capability,
                lookup_indexes,
                target: ds.as_str().to_string(),
            })),
            ds,
            sensitive,
            ds_policy: std::sync::Mutex::new(DsPolicy::default()),
        })
    }

    /// 当前池的克隆（Arc 廉价拷贝 —— 拿着它查完这次，换池不影响在飞查询）
    fn pool(&self) -> sqlx::MySqlPool {
        self.pool.read().expect("mysql pool 锁中毒").pool.clone()
    }

    /// 【A8】策略快照（`Mutex` 不跨 await：拷贝完即放锁）。
    fn ds_policy(&self) -> DsPolicy {
        *self.ds_policy.lock().expect("mysql 策略锁中毒")
    }

    /// 查询池和目标类型必须在同一把锁内取快照：避免刚判定为 Doris，热切换后却从生产
    /// MySQL 新池执行未套轻查询闸门的 SQL。
    fn execution_target(
        &self,
    ) -> (sqlx::MySqlPool, MysqlCapability, Option<std::sync::Arc<LookupIndexes>>) {
        let state = self.pool.read().expect("mysql pool 锁中毒");
        (state.pool.clone(), state.capability, state.lookup_indexes.clone())
    }

    /// 【业务库热切换】换到另一个 DSN：**先建先验、后换** ——
    /// 新池连不上 / 会话不是只读就返回 Err，旧池原样保留（不许换一半）。
    /// 只读是 DMS 生产库的最后一道物理防线，可写库**永远进不来**（F8 同一条）。
    pub async fn swap_pool(
        &self,
        url: &str,
        max_conn: u32,
        capability: MysqlCapability,
    ) -> Result<(), ConnectorError> {
        let target = self.target_name();
        self.swap_pool_named(url, max_conn, &target, capability).await
    }

    /// 热切换连接池与用户可见目标名必须在同一把锁内提交，避免可信凭证短暂标错来源。
    pub async fn swap_pool_named(
        &self,
        url: &str,
        max_conn: u32,
        target: &str,
        capability: MysqlCapability,
    ) -> Result<(), ConnectorError> {
        let (new, lookup_indexes) =
            connect_read_only(&self.ds, url, max_conn, capability).await?;
        if !pool_read_only(&new, capability).await {
            return Err(ConnectorError::config(
                self.ds.as_str(),
                "目标库不是只读会话或只读授权账号 —— 拒绝换入可写库",
            ));
        }
        *self.pool.write().expect("mysql pool 锁中毒") = PoolState {
            pool: new,
            capability,
            lookup_indexes,
            target: target.to_string(),
        };
        Ok(())
    }

    pub fn target_name(&self) -> String {
        self.target_snapshot().0
    }

    /// 目标名与能力必须原子读取，供深度报告在热切换期间做 fail-closed 判断。
    pub fn target_snapshot(&self) -> (String, bool) {
        let state = self.pool.read().expect("mysql pool 锁中毒");
        (state.target.clone(), state.capability.is_warehouse())
    }

    /// 仅用于启动时：连接已按运行时配置建立，补上该配置在目录中的名字。
    pub fn set_target_name(&self, target: &str) {
        self.pool.write().expect("mysql pool 锁中毒").target = target.to_string();
    }

    /// 框架自查语句通道（裁决 C1）：SQL 只能是 `&'static str`，数据全走 bind。
    pub fn fixed(&self, sql: &'static str) -> FixedStmt<'_> {
        let (pool, capability, _) = self.execution_target();
        FixedStmt::new_owned(
            pool,
            self.ds.as_str(),
            sql,
            matches!(capability, MysqlCapability::IdentityPermission),
        )
    }

    /// 新增“生产 DMS 业务点查”执行通道。只接受 kernel 严格 gate 产出的类型，SQL 本身已被
    /// 限定为 `LIMIT <= 50`，因此 `fetch_all` 最多接收 50 行，不存在先拉大结果再内存截断。
    /// 固定静态 `fixed()` 仍供登录/角色/权限加载使用，刻意不自动套本 gate，避免破坏鉴权。
    pub async fn fetch_dms_lookup(&self, sql: &DmsLookupSql) -> Result<RowSet, ConnectorError> {
        let at = self.ds.as_str();
        let (pool, capability, indexes) = self.execution_target();
        if capability.is_warehouse() {
            return Err(ConnectorError::config(at, "生产业务点查不能在数仓能力连接上执行"));
        }
        ensure_verified_lookup(sql, indexes.as_deref(), at)?;
        let deadline = tokio::time::Instant::now() + DMS_LOOKUP_TIMEOUT;
        let _slot = self.acquire_lookup_slot(deadline, DMS_LOOKUP_TIMEOUT).await?;
        tokio::time::timeout_at(deadline, async {
            let rows = sqlx::raw_sql(sql.wire())
                .fetch_all(&pool)
                .await
                .map_err(|e| sqlx_err(at, e))?;
            let (columns, mut data) = to_table(&rows, DMS_LOOKUP_MAX_ROWS);
            let redacted = redact(self.sensitive, &columns, &mut data);
            Ok(RowSet { columns, rows: data, redacted })
        })
        .await
        .map_err(|_| ConnectorError::timeout(at, DMS_LOOKUP_TIMEOUT))?
    }

    async fn acquire_lookup_slot(
        &self,
        deadline: tokio::time::Instant,
        timeout: Duration,
    ) -> Result<tokio::sync::SemaphorePermit<'static>, ConnectorError> {
        tokio::time::timeout_at(deadline, production_lookup_slots().acquire())
            .await
            .map_err(|_| ConnectorError::timeout(self.ds.as_str(), timeout))?
            .map_err(|_| ConnectorError::config(self.ds.as_str(), "生产业务点查并发阀门已关闭"))
    }

    /// Doris 2.1 的预编译响应与 sqlx 不兼容；无参数的框架静态查询走文本协议。
    /// 只接受 `&'static str`，不提供动态拼接入口。
    pub async fn raw_all<T>(&self, sql: &'static str) -> Result<Vec<T>, ConnectorError>
    where
        T: for<'r> FromRow<'r, sqlx::mysql::MySqlRow> + Send + Unpin,
    {
        let at = self.ds.as_str();
        let (pool, capability, _) = self.execution_target();
        if !capability.is_warehouse() {
            return Err(ConnectorError::config(
                at,
                "production_lookup 禁止 raw 静态分析查询",
            ));
        }
        let rows = sqlx::raw_sql(sql).fetch_all(&pool).await.map_err(|e| sqlx_err(at, e))?;
        rows.iter().map(|row| T::from_row(row).map_err(|e| sqlx_err(at, e))).collect()
    }

    /// 采集当前库 schema，并在同一连接池快照上补齐已登记的 Doris 跨库资产。
    ///
    /// 跨库部分只读 `information_schema`，WHERE 精确限定代码内白名单的 `(库, 表)`，
    /// 不枚举整库、不读取任何业务数据行。普通 MySQL 只返回当前库探针结果。
    pub async fn probe_schema_with_warehouse_catalog(
        &self,
        assets: &[WarehouseAsset],
    ) -> Result<(SchemaSnapshot, WarehouseCatalogStats), ConnectorError> {
        let at = self.ds.as_str();
        let (pool, capability, _) = self.execution_target();
        if !capability.is_warehouse() {
            return Err(ConnectorError::config(
                at,
                "production_lookup 禁止全库 schema 探针；仅允许静态业务键索引核验",
            ));
        }
        let mut snap = probe_mysql_schema(&pool, at, self.dialect()).await?;
        if assets.is_empty() {
            return Ok((snap, WarehouseCatalogStats::default()));
        }

        let sql = render_warehouse_catalog_probe(assets)
            .map_err(|e| ConnectorError::config(at, e))?;
        let rows = tokio::time::timeout(
            WAREHOUSE_CATALOG_TIMEOUT,
            sqlx::raw_sql(&sql).fetch_all(&pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(at, WAREHOUSE_CATALOG_TIMEOUT))?
        .map_err(|e| sqlx_err(at, e))?;
        let rows = rows
            .iter()
            .map(|row| {
                let decoded: WarehouseCatalogRow =
                    FromRow::from_row(row).map_err(|e| sqlx_err(at, e))?;
                let ordinal = decoded.7.parse::<i64>().map_err(|e| {
                    sqlx_err(at, sqlx::Error::Decode(Box::new(e)))
                })?;
                Ok((
                    decoded.0,
                    decoded.1,
                    decoded.2,
                    decoded.3.and_then(|v| v.parse::<i64>().ok()),
                    decoded.4,
                    decoded.5,
                    decoded.6,
                    ordinal,
                ))
            })
            .collect::<Result<Vec<_>, ConnectorError>>()?;
        let stats = merge_warehouse_catalog_rows(&mut snap, assets, rows);
        Ok((snap, stats))
    }

    /// Doris 数仓提供的 DMS 字段血缘映射可补齐 ODS 的空注释。
    /// 只读静态 SQL、只填空白注释；普通 MySQL 或没有该维表时静默跳过。
    pub async fn enrich_dms_snapshot(&self, snap: &mut SchemaSnapshot) -> Result<usize, ConnectorError> {
        let (_, capability, _) = self.execution_target();
        if !capability.is_warehouse() {
            return Err(ConnectorError::config(
                self.ds.as_str(),
                "production_lookup 禁止读取数仓字段映射",
            ));
        }
        let rows: Vec<(String, String, String)> = match self
            .raw_all(
                "SELECT table_name, column_name, CAST(column_comment AS CHAR)
                 FROM dim.dim_dms_table_mapping
                 WHERE column_comment IS NOT NULL AND TRIM(column_comment) <> ''",
            )
            .await
        {
            Ok(rows) => rows,
            Err(_) => {
                tracing::info!(reason = "warehouse_mapping_unavailable", "DMS 数仓字段映射不可用，保留原生 schema 注释");
                return Ok(0);
            }
        };
        let mapped: std::collections::HashMap<(String, String), String> = rows
            .into_iter()
            .map(|(table, column, comment)| ((table.to_lowercase(), column.to_lowercase()), comment))
            .collect();
        Ok(fill_blank_comments(snap, &mapped))
    }

    /// 静态模板里的 `?` 仅允许替换为服务端生成的 ISO 日期。
    /// 日期先强类型解析再加引号，调用方拿不到任意字符串拼 SQL 的入口。
    pub async fn raw_dates_all<T>(
        &self,
        template: &'static str,
        dates: &[chrono::NaiveDate],
    ) -> Result<Vec<T>, ConnectorError>
    where
        T: for<'r> FromRow<'r, sqlx::mysql::MySqlRow> + Send + Unpin,
    {
        let sql = render_date_template(template, dates).ok_or_else(|| {
            ConnectorError::config(self.ds.as_str(), "日期参数数量与静态模板不一致")
        })?;
        let at = self.ds.as_str();
        let (pool, capability, _) = self.execution_target();
        if !capability.is_warehouse() {
            return Err(ConnectorError::config(
                at,
                "production_lookup 禁止日期模板分析查询",
            ));
        }
        let rows = sqlx::raw_sql(&sql).fetch_all(&pool).await.map_err(|e| sqlx_err(at, e))?;
        rows.iter().map(|row| T::from_row(row).map_err(|e| sqlx_err(at, e))).collect()
    }

    pub async fn ping(&self) -> bool {
        tokio::time::timeout(
            DMS_LOOKUP_TIMEOUT,
            sqlx::raw_sql("SELECT 1").fetch_all(&self.pool()),
        )
        .await
        .is_ok_and(|result| result.is_ok())
    }

    /// `/api/health` 用：真从池里取一条连接问 MySQL「这个会话是只读吗」。
    /// 连不上/列类型漂移一律 `false`（fail-closed：报不出只读就当没只读）。
    /// 变量名认 5.7+/8.x 的 `transaction_read_only`（5.6 那个 `tx_read_only` 已随 5.6 停服作废）。
    pub async fn session_read_only(&self) -> bool {
        let (pool, capability, _) = self.execution_target();
        pool_read_only(&pool, capability).await
    }
}

/// 建只读池（connect 与 swap 共用一段 —— `after_connect` 的只读设定只有一份）。
/// 🔴 逐行搬 server/src/db.rs:72-84：只读会话是 DMS 生产库的最后一道物理防线
async fn connect_read_only(
    ds: &DsId,
    url: &str,
    max_conn: u32,
    capability: MysqlCapability,
) -> Result<(sqlx::MySqlPool, Option<std::sync::Arc<LookupIndexes>>), ConnectorError> {
    // 生产库即使每条都很轻，也不能靠高并发把压力重新叠回业务库；身份/权限池同样限 2。
    // Doris 分析维持调用方配置，不进入生产限流。
    let max_conn = effective_max_connections(max_conn, capability);
    let pool = sqlx::mysql::MySqlPoolOptions::new().max_connections(max_conn);
    tracing::info!(backend = if capability.is_warehouse() { "warehouse" } else { "production_lookup" }, "创建只读业务连接池");
    let out = if capability.is_warehouse() {
        let opts = MySqlConnectOptions::from_str(url)
            .map_err(|_| ConnectorError::config(ds.as_str(), "数据库连接地址格式无效"))?
            .pipes_as_concat(false)
            .no_engine_substitution(false)
            .timezone(None)
            .set_names(false);
        pool.connect_with(opts).await
    } else {
        pool.after_connect(|conn, _| {
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute("SET SESSION TRANSACTION READ ONLY").await?;
                conn.execute("SET SESSION MAX_EXECUTION_TIME=2000").await?;
                Ok(())
            })
        })
        .connect(url)
        .await
    };
    let pool = out.map_err(|_| connection_unavailable(ds.as_str()))?;
    if !capability.is_warehouse() && !mysql_session_read_only(&pool).await {
        pool.close().await;
        return Err(ConnectorError::config(
            ds.as_str(),
            "生产 MySQL 会话未进入只读模式，按 fail-closed 拒绝",
        ));
    }
    let lookup_indexes = if capability.is_warehouse() {
        None
    } else {
        match verify_lookup_indexes(&pool, ds.as_str()).await {
            Ok(indexes) => Some(std::sync::Arc::new(indexes)),
            Err(e) => {
                tracing::warn!(reason = "lookup_index_verification_failed", err = %e, "生产业务点查索引核验失败，点查保持关闭");
                None
            }
        }
    };
    Ok((pool, lookup_indexes))
}

fn effective_max_connections(requested: u32, capability: MysqlCapability) -> u32 {
    match capability {
        MysqlCapability::Warehouse => requested.max(1),
        MysqlCapability::ProductionLookup | MysqlCapability::IdentityPermission => {
            requested.clamp(1, DMS_LOOKUP_MAX_CONCURRENCY)
        }
    }
}

/// 【A8】`fetch` 入口的有效 (行上限, 超时) = min(调用方, ds 级策略, 生产能力红线)。
/// 纯函数：「ds 级策略放宽不了 production_lookup / dms-auth 的 2s·50 行」由单测钉死。
fn effective_limits(warehouse: bool, policy: DsPolicy, max: usize, t: Duration) -> (usize, Duration) {
    let (max, t) = policy.clamp(max, t);
    if warehouse { (max, t) } else { (max.min(DMS_LOOKUP_MAX_ROWS), t.min(DMS_LOOKUP_TIMEOUT)) }
}

struct PoolState {
    pool: sqlx::MySqlPool,
    capability: MysqlCapability,
    lookup_indexes: Option<std::sync::Arc<LookupIndexes>>,
    target: String,
}

type LookupIndexes = HashMap<(String, String), DmsIndexKind>;

fn render_date_template(template: &str, dates: &[chrono::NaiveDate]) -> Option<String> {
    if template.matches('?').count() != dates.len() {
        return None;
    }
    let mut sql = String::with_capacity(template.len() + dates.len() * 12);
    let mut rest = template;
    for date in dates {
        let (head, tail) = rest.split_once('?')?;
        sql.push_str(head);
        sql.push('\'');
        sql.push_str(&date.format("%F").to_string());
        sql.push('\'');
        rest = tail;
    }
    sql.push_str(rest);
    Some(sql)
}

/// 【测试连通性】建一次性池验证 DSN：连得上 + 会话只读 + `SELECT 1`。
/// 返回 (延迟毫秒, 服务端版本)。**不产生** `SqlSource`（它只用于页面「测试」按钮 ——
/// 与 `swap_pool` 的验证同构但不换任何东西）。
pub async fn test_pool(
    ds: &DsId,
    url: &str,
    capability: MysqlCapability,
) -> Result<(u128, String), ConnectorError> {
    let t0 = std::time::Instant::now();
    let (pool, _) = connect_read_only(ds, url, 1, capability).await?;
    if !pool_read_only(&pool, capability).await {
        return Err(ConnectorError::config(
            ds.as_str(),
            "目标库不是只读会话或只读授权账号 —— 可写库不进本系统",
        ));
    }
    let rows = tokio::time::timeout(
        DMS_LOOKUP_TIMEOUT,
        sqlx::raw_sql("SELECT CAST(VERSION() AS CHAR)").fetch_all(&pool),
    )
        .await
        .map_err(|_| ConnectorError::timeout(ds.as_str(), DMS_LOOKUP_TIMEOUT))?
        .map_err(|_| connection_unavailable(ds.as_str()))?;
    let ver = rows
        .first()
        .and_then(|row| row.try_get::<String, _>(0).ok())
        .ok_or_else(|| ConnectorError::decode(ds.as_str(), "数据库版本返回为空"))?;
    Ok((t0.elapsed().as_millis(), ver))
}

async fn verify_lookup_indexes(
    pool: &sqlx::MySqlPool,
    at: &str,
) -> Result<LookupIndexes, ConnectorError> {
    let registered: HashMap<(String, String), DmsIndexKind> = registered_lookup_keys()
        .map(|(table, column, kind)| {
            ((table.to_ascii_lowercase(), column.to_ascii_lowercase()), kind)
        })
        .collect();
    let tables = registered_lookup_keys()
        .map(|(table, _, _)| table)
        .collect::<std::collections::BTreeSet<_>>();
    // 启动/热切换时的一次性核验（逐表 SHOW INDEX）。不是用户查询：2s 的用户点查预算
    // 在公网链路上连十几张表的 RTT 都不够（2026-08-08 实测公网核验必超时 → 点查整关）。
    let deadline = tokio::time::Instant::now() + LOOKUP_INDEX_VERIFY_TIMEOUT;
    let mut verified: LookupIndexes = HashMap::new();
    for table in tables {
        if !table.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            return Err(ConnectorError::config(at, "生产点查索引登记表名不合法"));
        }
        // 全列 CAST AS CHAR：SHOW INDEX 的列型随 MySQL 版本漂移（8.0.28 实测按名解码
        // 直接失败），information_schema + 全文本投影是唯一可移植形态（探针同款）。
        // IS_VISIBLE 只存在 8.0+，不查它：不可见索引是 DBA 的显式动作，按可见处理
        // （与旧代码 Visible 解码失败时 unwrap_or(true) 同语义）。
        let stmt = format!(
            "SELECT CAST(INDEX_NAME AS CHAR), CAST(SEQ_IN_INDEX AS CHAR),                     CAST(COLUMN_NAME AS CHAR), CAST(NON_UNIQUE AS CHAR),                     CAST(SUB_PART AS CHAR), CAST(INDEX_TYPE AS CHAR)              FROM information_schema.STATISTICS              WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = '{table}'"
        );
        let rows = tokio::time::timeout_at(deadline, sqlx::raw_sql(&stmt).fetch_all(pool))
            .await
            .map_err(|_| ConnectorError::timeout(at, LOOKUP_INDEX_VERIFY_TIMEOUT))?
            .map_err(|e| sqlx_err(at, e))?;
        let mut indexes: BTreeMap<String, Vec<(i64, String, i64, Option<i64>, String, bool)>> =
            BTreeMap::new();
        for row in rows {
            let index: String = row.try_get(0).map_err(|e| sqlx_err(at, e))?;
            let seq_text: String = row.try_get(1).map_err(|e| sqlx_err(at, e))?;
            let seq: i64 = seq_text.parse().map_err(|_| {
                sqlx_err(at, sqlx::Error::Decode("SEQ_IN_INDEX 非数字".into()))
            })?;
            let column: String = row.try_get(2).map_err(|e| sqlx_err(at, e))?;
            let non_unique_text: String = row.try_get(3).map_err(|e| sqlx_err(at, e))?;
            let non_unique: i64 = non_unique_text.parse().map_err(|_| {
                sqlx_err(at, sqlx::Error::Decode("NON_UNIQUE 非数字".into()))
            })?;
            let sub_part: Option<i64> = row
                .try_get::<Option<String>, _>(4)
                .map_err(|e| sqlx_err(at, e))?
                .and_then(|value| value.parse().ok());
            let index_type: String = row.try_get(5).map_err(|e| sqlx_err(at, e))?;
            let visible = true;
            indexes.entry(index).or_default().push((
                seq,
                column,
                non_unique,
                sub_part,
                index_type,
                visible,
            ));
        }
        for columns in indexes.values_mut() {
            columns.sort_by_key(|column| column.0);
            let Some((seq, column, non_unique, sub_part, index_type, visible)) = columns.first()
            else {
                continue;
            };
            if *seq != 1
                || sub_part.is_some()
                || !*visible
                || !matches!(index_type.to_ascii_uppercase().as_str(), "BTREE" | "HASH")
            {
                continue;
            }
            let key = (table.to_ascii_lowercase(), column.to_ascii_lowercase());
            if registered.contains_key(&key) {
                let found = if *non_unique == 0 && columns.len() == 1 {
                    DmsIndexKind::Unique
                } else {
                    DmsIndexKind::Leading
                };
                verified
                    .entry(key)
                    .and_modify(|kind| {
                        if found == DmsIndexKind::Unique {
                            *kind = found;
                        }
                    })
                    .or_insert(found);
            }
        }
    }
    Ok(verified)
}

fn ensure_verified_lookup(
    sql: &DmsLookupSql,
    verified: Option<&LookupIndexes>,
    at: &str,
) -> Result<(), ConnectorError> {
    let verified = verified.ok_or_else(|| {
        ConnectorError::config(at, "生产业务点查索引尚未完成核验，按 fail-closed 拒绝")
    })?;
    if !sql.lookup_cols().is_empty()
        && sql.lookup_cols().iter().all(|column| {
            let required = registered_lookup_kind(sql.table(), column);
            let found = verified.get(&(
                sql.table().to_ascii_lowercase(),
                column.to_ascii_lowercase(),
            )).copied();
            matches!(
                (required, sql.index_kind(), found),
                (
                    Some(DmsIndexKind::Unique),
                    DmsIndexKind::Unique,
                    Some(DmsIndexKind::Unique)
                ) | (
                    Some(DmsIndexKind::Leading),
                    DmsIndexKind::Leading,
                    Some(DmsIndexKind::Unique)
                ) | (
                    Some(DmsIndexKind::Leading),
                    DmsIndexKind::Leading,
                    Some(DmsIndexKind::Leading)
                )
            )
        })
    {
        return Ok(());
    }
    Err(ConnectorError::config(
        at,
        "查询键未通过要求的单列唯一索引或最左索引核验，拒绝生产点查",
    ))
}

/// MySQL 用会话只读位；Doris 没有可用的事务只读位，改验当前账号在全局/库/表三级
/// 都没有写权限。两条路径都 fail-closed，SQL 语法闸门仍只允许单条查询。
async fn mysql_session_read_only(pool: &sqlx::MySqlPool) -> bool {
    if tokio::time::timeout(
        DMS_LOOKUP_TIMEOUT,
        sqlx::query_scalar::<_, i64>("SELECT @@SESSION.transaction_read_only").fetch_one(pool),
    )
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|v| v == 1)
    {
        return true;
    }
    tracing::warn!(reason = "mysql_session_read_only_not_verified", "生产 MySQL 会话只读状态无法核验，按非只读拒绝");
    false
}

async fn pool_read_only(pool: &sqlx::MySqlPool, capability: MysqlCapability) -> bool {
    if !capability.is_warehouse() {
        return mysql_session_read_only(pool).await;
    }
    let sql = "SELECT UPPER(PRIVILEGE_TYPE) FROM (\
         SELECT PRIVILEGE_TYPE FROM information_schema.USER_PRIVILEGES WHERE GRANTEE = CURRENT_USER() \
         UNION ALL SELECT PRIVILEGE_TYPE FROM information_schema.SCHEMA_PRIVILEGES WHERE GRANTEE = CURRENT_USER() \
         UNION ALL SELECT PRIVILEGE_TYPE FROM information_schema.TABLE_PRIVILEGES WHERE GRANTEE = CURRENT_USER()\
         ) p";
    let rows = match tokio::time::timeout(
        DMS_LOOKUP_TIMEOUT,
        sqlx::raw_sql(sql).fetch_all(pool),
    )
    .await
    {
        Ok(Ok(rows)) => rows,
        Err(_) => {
            tracing::warn!(reason = "read_only_verification_timeout", "数据库账号权限核验超时，按非只读拒绝");
            return false;
        }
        Ok(Err(_)) => {
            tracing::warn!(reason = "warehouse_privilege_verification_failed", "无法核验数据库账号权限，按非只读拒绝");
            return false;
        }
    };
    !rows.is_empty()
        && rows.iter().all(|row| {
            row.try_get::<String, _>(0).is_ok_and(|privilege| {
                matches!(privilege.as_str(), "SELECT" | "SHOW VIEW" | "USAGE")
            })
        })
}

impl SqlSource for ReadOnlyMySql {
    fn ds_id(&self) -> &DsId {
        &self.ds
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Mysql
    }

    fn is_warehouse(&self) -> bool {
        self.pool.read().expect("mysql pool 锁中毒").capability.is_warehouse()
    }

    fn dialect(&self) -> &'static dyn Dialect {
        &MysqlDialect
    }

    fn set_ds_policy(&self, policy: DsPolicy) {
        *self.ds_policy.lock().expect("mysql 策略锁中毒") = policy;
    }

    fn fetch<'a>(
        &'a self,
        sql: &'a ScopedSql,
        max: usize,
        t: Duration,
    ) -> BoxFut<'a, Result<RowSet, ConnectorError>> {
        Box::pin(async move {
            let at = self.ds.as_str();
            let (pool, capability, indexes) = self.execution_target();
            let checked = guard_scoped_wire(
                capability,
                indexes.as_deref(),
                sql.wire(),
                at,
                self.sensitive,
            )?;
            let wire = checked.wire();
            let warehouse = capability.is_warehouse();
            // 【A8】入口先与 ds 级策略取 min，再套生产能力红线 —— 两级都只许更紧
            let (max, t) = effective_limits(warehouse, self.ds_policy(), max, t);
            let deadline = tokio::time::Instant::now() + t;
            let _slot = if warehouse {
                None
            } else {
                Some(self.acquire_lookup_slot(deadline, t).await?)
            };
            let rows = tokio::time::timeout_at(deadline, sqlx::raw_sql(wire).fetch_all(&pool))
                .await
                .map_err(|_| ConnectorError::timeout(at, t))?
                .map_err(|e| sqlx_err(at, e))?;
            let (columns, mut data) = to_table(&rows, max);
            // Doris 的 0 行结果补列名；生产点查禁止额外 DESCRIBE 往返。
            let columns = match columns.is_empty() {
                true if warehouse => describe_columns(&pool, wire, at).await?,
                true => vec![],
                false => columns,
            };
            let redacted = redact(self.sensitive, &columns, &mut data);
            Ok(RowSet { columns, rows: data, redacted })
        })
    }

    fn explain<'a>(
        &'a self,
        sql: &'a ScopedSql,
        t: Duration,
    ) -> BoxFut<'a, Result<Option<String>, ConnectorError>> {
        Box::pin(async move {
            let at = self.ds.as_str();
            let (pool, capability, indexes) = self.execution_target();
            if !capability.is_warehouse() {
                return Ok(None);
            }
            let checked = match guard_scoped_wire(
                capability,
                indexes.as_deref(),
                sql.wire(),
                at,
                self.sensitive,
            ) {
                Ok(wire) => wire,
                Err(err) => return Ok(Some(err.to_string())),
            };
            let wire = checked.wire();
            let warehouse = capability.is_warehouse();
            let t = if warehouse { t } else { t.min(DMS_LOOKUP_TIMEOUT) };
            // 生产 MySQL 到这里已经过单表业务键点查闸门；Doris 仍保留通用分析 EXPLAIN。
            let stmt = format!("EXPLAIN {wire}");
            match tokio::time::timeout(t, sqlx::raw_sql(&stmt).fetch_all(&pool)).await {
                Ok(Ok(_)) => Ok(None),
                // 只有数据库**明确判定**语句有问题才给 Some（可拿去 repair）
                Ok(Err(e)) => Ok(e.as_database_error().map(|db| db.message().to_string())),
                // 超时/抖动 = None：不触发改写（可能把本来对的 SQL 改坏，还多花一次 LLM）
                Err(_) => Ok(None),
            }
        })
    }

    fn probe_schema<'a>(&'a self) -> BoxFut<'a, Result<SchemaSnapshot, ConnectorError>> {
        Box::pin(async move {
            let at = self.ds.as_str();
            let (pool, capability, _) = self.execution_target();
            if !capability.is_warehouse() {
                return Err(ConnectorError::config(
                    at,
                    "production_lookup 禁止全库 schema autodiscover",
                ));
            }
            probe_mysql_schema(&pool, at, self.dialect()).await
        })
    }
}

type WarehouseCatalogRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
);

type DecodedWarehouseCatalogRow = (
    String,
    String,
    String,
    Option<i64>,
    String,
    String,
    String,
    i64,
);

async fn probe_mysql_schema(
    pool: &sqlx::MySqlPool,
    at: &str,
    dialect: &'static dyn Dialect,
) -> Result<SchemaSnapshot, ConnectorError> {
    // 两条探针来自 `Dialect`（kernel 已逐字收录 meta.rs 的形态，含 CAST 两个坑）。
    let table_rows = sqlx::raw_sql(dialect.table_probe())
        .fetch_all(pool)
        .await
        .map_err(|e| sqlx_err(at, e))?;
    let tables = table_rows
        .iter()
        .map(|row| {
            let (name, comment, rows): (String, String, Option<String>) =
                FromRow::from_row(row).map_err(|e| sqlx_err(at, e))?;
            Ok((name, comment, rows.and_then(|v| v.parse::<i64>().ok())))
        })
        .collect::<Result<Vec<_>, ConnectorError>>()?;
    let column_rows = sqlx::raw_sql(dialect.column_probe())
        .fetch_all(pool)
        .await
        .map_err(|e| sqlx_err(at, e))?;
    let cols = column_rows
        .iter()
        .map(|row| {
            let (table, name, data_type, comment, ordinal):
                (String, String, String, String, String) =
                FromRow::from_row(row).map_err(|e| sqlx_err(at, e))?;
            let ordinal = ordinal
                .parse::<i64>()
                .map_err(|e| sqlx_err(at, sqlx::Error::Decode(Box::new(e))))?;
            Ok((table, name, data_type, comment, ordinal))
        })
        .collect::<Result<Vec<_>, ConnectorError>>()?;
    Ok(snapshot(tables, cols))
}

const WAREHOUSE_CATALOG_PROBE_PREFIX: &str =
    "SELECT CAST(t.TABLE_SCHEMA AS CHAR), CAST(t.TABLE_NAME AS CHAR), \
            CAST(COALESCE(t.TABLE_COMMENT, '') AS CHAR), CAST(t.TABLE_ROWS AS CHAR), \
            CAST(c.COLUMN_NAME AS CHAR), CAST(c.DATA_TYPE AS CHAR), \
            CAST(COALESCE(c.COLUMN_COMMENT, '') AS CHAR), CAST(c.ORDINAL_POSITION AS CHAR) \
     FROM information_schema.TABLES t \
     JOIN information_schema.COLUMNS c \
       ON c.TABLE_SCHEMA = t.TABLE_SCHEMA AND c.TABLE_NAME = t.TABLE_NAME \
     WHERE t.TABLE_TYPE = 'BASE TABLE' AND (";

fn render_warehouse_catalog_probe(assets: &[WarehouseAsset]) -> Result<String, String> {
    if assets.is_empty() {
        return Err("数仓元数据白名单不能为空".into());
    }
    let mut qualified = std::collections::HashSet::new();
    let mut base_names = std::collections::HashMap::new();
    let mut predicates = Vec::with_capacity(assets.len());
    for asset in assets {
        if !valid_metadata_ident(asset.database) || !valid_metadata_ident(asset.table) {
            return Err(format!(
                "非法数仓元数据白名单标识符 {}.{}",
                asset.database, asset.table
            ));
        }
        let q = format!("{}.{}", asset.database.to_ascii_lowercase(), asset.table.to_ascii_lowercase());
        if !qualified.insert(q.clone()) {
            return Err(format!("重复数仓元数据白名单资产 {q}"));
        }
        if let Some(previous) = base_names.insert(asset.table.to_ascii_lowercase(), q.clone()) {
            if previous != q {
                return Err(format!(
                    "数仓元数据白名单存在跨库同名表 {}：{} 与 {}",
                    asset.table, previous, q
                ));
            }
        }
        predicates.push(format!(
            "(t.TABLE_SCHEMA = '{}' AND t.TABLE_NAME = '{}')",
            asset.database, asset.table
        ));
    }
    Ok(format!(
        "{WAREHOUSE_CATALOG_PROBE_PREFIX}{}) ORDER BY t.TABLE_SCHEMA, t.TABLE_NAME, c.ORDINAL_POSITION",
        predicates.join(" OR ")
    ))
}

fn valid_metadata_ident(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn merge_warehouse_catalog_rows(
    snap: &mut SchemaSnapshot,
    assets: &[WarehouseAsset],
    rows: Vec<DecodedWarehouseCatalogRow>,
) -> WarehouseCatalogStats {
    struct FoundTable {
        database: String,
        table: String,
        comment: String,
        row_estimate: i64,
        columns: Vec<ColumnInfo>,
    }

    let mut found = std::collections::BTreeMap::<String, FoundTable>::new();
    for (database, table, comment, row_estimate, name, data_type, col_comment, ordinal) in rows {
        let key = table.to_ascii_lowercase();
        let entry = found.entry(key).or_insert_with(|| FoundTable {
            database,
            table,
            comment,
            row_estimate: row_estimate.unwrap_or(0),
            columns: Vec::new(),
        });
        if !entry.columns.iter().any(|column| column.name.eq_ignore_ascii_case(&name)) {
            entry.columns.push(ColumnInfo { name, data_type, comment: col_comment, ordinal });
        }
    }

    let expected_names = assets
        .iter()
        .map(|asset| asset.table.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>();
    // SchemaSnapshot 的持久化键仍是 base table，兼容现有 source_table 正则与活性校验。
    // 所有白名单基础表名先从默认库快照删除：即使跨库表缺失/不可见，也不能让默认库同名表
    // 冒充已验证资产；实际 information_schema 返回后才回填，缺失即 fail-closed。
    snap.tables.retain(|table| !expected_names.contains(&table.name.to_ascii_lowercase()));
    snap.columns.retain(|(table, _)| !expected_names.contains(&table.to_ascii_lowercase()));

    let mut stats = WarehouseCatalogStats {
        requested: expected_names.len(),
        missing: expected_names.len().saturating_sub(found.len()),
        ..WarehouseCatalogStats::default()
    };
    for (_, mut table) in found {
        table.columns.sort_by_key(|column| column.ordinal);
        stats.tables += 1;
        stats.columns += table.columns.len();
        let physical = format!("{}.{}", table.database, table.table);
        let comment = if table.comment.trim().is_empty() {
            format!("数仓物理表 {physical}；生成 SQL 必须使用完整库表名")
        } else {
            format!(
                "数仓物理表 {physical}；生成 SQL 必须使用完整库表名。{}",
                table.comment
            )
        };
        snap.tables.push(TableInfo {
            name: table.table.clone(),
            comment,
            row_estimate: table.row_estimate,
        });
        snap.columns.extend(table.columns.into_iter().map(|column| (table.table.clone(), column)));
    }
    stats
}

/// 只约束 AI `ScopedSql` 执行路径。`fixed/raw/probe` 是框架内部静态 SQL，不经过这里。
enum GuardedWire<'a> {
    Borrowed(&'a str),
    Checked(DmsLookupSql),
}

impl GuardedWire<'_> {
    fn wire(&self) -> &str {
        match self {
            Self::Borrowed(wire) => wire,
            Self::Checked(sql) => sql.wire(),
        }
    }
}

fn guard_scoped_wire<'a>(
    capability: MysqlCapability,
    indexes: Option<&LookupIndexes>,
    wire: &'a str,
    at: &str,
    sensitive: &[&str],
) -> Result<GuardedWire<'a>, ConnectorError> {
    if capability.is_warehouse() {
        return Ok(GuardedWire::Borrowed(wire));
    }
    let checked = gate_dms_scoped_with(wire, &MysqlDialect, sensitive)
        .map_err(|e| ConnectorError::query(at, e))?;
    ensure_verified_lookup(&checked, indexes, at)?;
    Ok(GuardedWire::Checked(checked))
}

/// 探针两组元组 → `SchemaSnapshot`。`TABLE_ROWS` 可为 NULL（视图/新表）→ 0。
/// 列序与 PG 版必须一致（下游是同一个 `SchemaSnapshot`）。
pub(crate) fn snapshot(
    tables: Vec<(String, String, Option<i64>)>,
    cols: Vec<(String, String, String, String, i64)>,
) -> SchemaSnapshot {
    SchemaSnapshot {
        tables: tables
            .into_iter()
            .map(|(name, comment, rows)| TableInfo {
                name,
                comment,
                row_estimate: rows.unwrap_or(0),
            })
            .collect(),
        columns: cols
            .into_iter()
            .map(|(t, name, data_type, comment, ordinal)| {
                (t, ColumnInfo { name, data_type, comment, ordinal })
            })
            .collect(),
    }
}

/// sqlx 错误 → `ConnectorError`，与 `fixed::classify` 同口径（只有 `Database` 归 `Query`，
/// 因为只有它是「数据库明确判定语句有问题」= 可拿去 repair）。`postgres.rs`/`owned.rs` 共用。
pub(crate) fn sqlx_err(at: &str, e: sqlx::Error) -> ConnectorError {
    match e {
        sqlx::Error::Database(db) => {
            let message = db.message();
            let lower = message.to_ascii_lowercase();
            if lower.contains("access denied") || lower.contains("permission denied") {
                ConnectorError::query(at, "数据库权限不足")
            } else {
                ConnectorError::query(at, message)
            }
        }
        sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Decode(_)
        | sqlx::Error::ColumnNotFound(_) => {
            ConnectorError::decode(at, "数据库返回数据无法解码")
        }
        _ => connection_unavailable(at),
    }
}

fn connection_unavailable(at: &str) -> ConnectorError {
    ConnectorError::connect(at, "数据库连接不可用")
}

/// 结果列脱敏（F5）：命中的列整列置 `Null`，返回被置空的列名（进 `RowSet.redacted`，
/// 调用方据此提示用户「该列已脱敏」而不是当成没数据）。`postgres.rs` 共用同一份。
pub(crate) fn redact(
    sensitive: &[&str],
    columns: &[String],
    data: &mut [Vec<Value>],
) -> Vec<String> {
    let hit: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            let n = c.to_lowercase();
            sensitive.iter().any(|k| n.contains(k))
        })
        .map(|(i, _)| i)
        .collect();
    if hit.is_empty() {
        return vec![];
    }
    let names: Vec<String> = hit.iter().map(|i| columns[*i].clone()).collect();
    tracing::warn!("结果集敏感列已置空: {names:?}");
    for row in data.iter_mut() {
        for i in &hit {
            if let Some(cell) = row.get_mut(*i) {
                *cell = Value::Null;
            }
        }
    }
    names
}

/// 0 行结果的列名回填：`DESCRIBE SELECT` 只解析不取数（只读会话允许，见 fetch 调用点）。
/// describe 失败**降级为空列**（warn 留痕）—— 元数据拿不到不是让取数失败的理由。
async fn describe_columns(
    pool: &sqlx::MySqlPool,
    sql: &str,
    at: &str,
) -> Result<Vec<String>, ConnectorError> {
    use sqlx::Executor as _;
    let mut conn = pool.acquire().await.map_err(|e| sqlx_err(at, e))?;
    match conn.describe(sql).await {
        Ok(d) => Ok(d.columns().iter().map(|c| c.name().to_string()).collect()),
        Err(_) => {
            tracing::warn!(reason = "describe_columns_failed", "DESCRIBE 回填列名失败（空列返回）");
            Ok(vec![])
        }
    }
}

/// 行集 → (列名, JSON 行)。列名取自首行（无行则空列，与拆分前逐行等价）；`max` 行即截断不报错。
fn to_table(rows: &[sqlx::mysql::MySqlRow], max: usize) -> (Vec<String>, Vec<Vec<Value>>) {    let mut columns: Vec<String> = vec![];
    let mut data: Vec<Vec<Value>> = vec![];
    for (i, row) in rows.iter().enumerate() {
        if i == 0 {
            columns = row.columns().iter().map(|c| c.name().to_string()).collect();
        }
        data.push(
            row.columns()
                .iter()
                .enumerate()
                .map(|(ci, col)| cell_to_json(row, ci, col.type_info().name()))
                .collect(),
        );
        if data.len() >= max {
            break;
        }
    }
    (columns, data)
}

/// 列类型名 → 取值路径。抽成纯函数**只为无库可单测**（`MySqlRow` 造不出来），
/// 分支与拆分前 `pipeline::cell_to_json` 的 match 臂一字不差。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Cell {
    Int,
    Float,
    /// DECIMAL 走字符串：f64 会吃掉分位精度（金额列必现）
    Dec,
    Time,
    Text,
}

pub(crate) fn cell_kind(ty: &str) -> Cell {
    match ty {
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "TINYINT UNSIGNED"
        | "SMALLINT UNSIGNED" | "INT UNSIGNED" | "BIGINT UNSIGNED" | "YEAR" => Cell::Int,
        "FLOAT" | "DOUBLE" => Cell::Float,
        "DECIMAL" => Cell::Dec,
        "DATE" | "DATETIME" | "TIMESTAMP" | "TIME" => Cell::Time,
        _ => Cell::Text,
    }
}

fn cell_to_json(row: &sqlx::mysql::MySqlRow, i: usize, ty: &str) -> Value {
    use sqlx::types::chrono::{NaiveDate, NaiveDateTime};
    let get = |v: Option<Value>| v.unwrap_or(Value::Null);
    match cell_kind(ty) {
        Cell::Int => get(row.try_get::<Option<i64>, _>(i).ok().flatten().map(Value::from)),
        Cell::Float => get(row.try_get::<Option<f64>, _>(i).ok().flatten().map(Value::from)),
        Cell::Dec => get(row
            .try_get::<Option<sqlx::types::Decimal>, _>(i)
            .ok()
            .flatten()
            .map(|d| Value::from(d.to_string()))),
        Cell::Time => get(row
            .try_get::<Option<NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|d| Value::from(d.format("%Y-%m-%d %H:%M:%S").to_string()))
            .or_else(|| {
                row.try_get::<Option<NaiveDate>, _>(i)
                    .ok()
                    .flatten()
                    .map(|d| Value::from(d.format("%Y-%m-%d").to_string()))
            })),
        Cell::Text => get(row.try_get::<Option<String>, _>(i).ok().flatten().map(Value::from)),
    }
}

/// 无库无网：脱敏与类型映射都是纯函数。
#[cfg(test)]
mod tests {
    use super::*;

    /// F5：`SELECT *` 拿不到列名文本，靠结果列脱敏兜底（断言取自 pipeline.rs 同名测试）
    #[test]
    fn redacts_sensitive_result_columns() {
        let sensitive = dms_kernel::nl::lexicon::SENSITIVE_COLS;
        let cols = vec!["actual_name".to_string(), "login_pwd".to_string(), "id_card".to_string()];
        let mut rows = vec![vec![
            Value::from("张三"),
            Value::from("hash"),
            Value::from("4301..."),
        ]];
        let hit = redact(sensitive, &cols, &mut rows);
        assert_eq!(rows[0][0], Value::from("张三"));
        assert_eq!(rows[0][1], Value::Null);
        assert_eq!(rows[0][2], Value::Null);
        // redacted 必须回报列名：调用方要能提示「已脱敏」而不是「无数据」
        assert_eq!(hit, ["login_pwd", "id_card"]);
        // 不命中时不动数据、不回报
        let mut ok = vec![vec![Value::from(1)]];
        assert!(redact(sensitive, &["amount".to_string()], &mut ok).is_empty());
        assert_eq!(ok[0][0], Value::from(1));
    }

    #[test]
    fn cell_kind_maps_every_family() {
        assert_eq!(cell_kind("BIGINT UNSIGNED"), Cell::Int);
        assert_eq!(cell_kind("YEAR"), Cell::Int);
        assert_eq!(cell_kind("DOUBLE"), Cell::Float);
        // 🔴 DECIMAL 必须走字符串保精度，不能落进 Float
        assert_eq!(cell_kind("DECIMAL"), Cell::Dec);
        assert_eq!(cell_kind("DATETIME"), Cell::Time);
        assert_eq!(cell_kind("TIME"), Cell::Time);
        // 未列举的一律 Text（JSON/BLOB/ENUM/新类型）——不猜、不 panic
        for t in ["VARCHAR", "JSON", "LONGBLOB", "ENUM", "BIT", ""] {
            assert_eq!(cell_kind(t), Cell::Text, "{t}");
        }
    }

    /// 探针元组 → 快照：`TABLE_ROWS` 为 NULL 归 0（视图/未 ANALYZE 的新表）
    #[test]
    fn snapshot_keeps_null_row_estimate_at_zero() {
        let s = snapshot(
            vec![("orders".into(), "订单".into(), None), ("goods".into(), "".into(), Some(7))],
            vec![("orders".into(), "id".into(), "bigint".into(), "主键".into(), 1)],
        );
        assert_eq!(s.tables[0].row_estimate, 0);
        assert_eq!(s.tables[1].row_estimate, 7);
        assert_eq!(s.columns[0].0, "orders");
        assert_eq!(s.columns[0].1.ordinal, 1);
    }

    #[test]
    fn production_mysql_pool_and_execution_have_a_hard_concurrency_ceiling() {
        assert_eq!(DMS_LOOKUP_MAX_CONCURRENCY, 2);
        assert_eq!(effective_max_connections(10, MysqlCapability::ProductionLookup), 2);
        assert_eq!(effective_max_connections(5, MysqlCapability::IdentityPermission), 2);
        assert_eq!(effective_max_connections(10, MysqlCapability::Warehouse), 10);
        let analytical = "SELECT region, SUM(amount) FROM sales_dw.dws_off_offline_sale_dfn GROUP BY region";
        assert_eq!(
            guard_scoped_wire(
                MysqlCapability::Warehouse,
                None,
                analytical,
                "warehouse",
                &[],
            )
            .unwrap()
            .wire(),
            analytical,
        );
    }

    #[test]
    fn warehouse_mapping_only_fills_blank_native_comments() {
        let mut s = snapshot(
            vec![("t_goods".into(), "商品".into(), Some(1))],
            vec![
                ("t_goods".into(), "sku_group".into(), "varchar".into(), "".into(), 1),
                ("t_goods".into(), "goods_name".into(), "varchar".into(), "原生商品名".into(), 2),
            ],
        );
        let mapped = std::collections::HashMap::from([
            (("t_goods".into(), "sku_group".into()), "物料分组(SKU组)".into()),
            (("t_goods".into(), "goods_name".into()), "映射商品名".into()),
        ]);
        assert_eq!(fill_blank_comments(&mut s, &mapped), 1);
        assert_eq!(s.columns[0].1.comment, "物料分组(SKU组)");
        assert_eq!(s.columns[1].1.comment, "原生商品名", "已有注释不得被数仓映射覆盖");
    }

    #[test]
    fn warehouse_catalog_probe_is_exact_and_rejects_ambiguous_names() {
        let assets = [
            WarehouseAsset::new("sales_dw", "dws_off_offline_sale_dfn"),
            WarehouseAsset::new("sales_ads", "ads_off_offline_region_sale_dfn"),
        ];
        let sql = render_warehouse_catalog_probe(&assets).unwrap();
        assert!(sql.contains("FROM information_schema.TABLES t"));
        assert!(sql.contains("t.TABLE_SCHEMA = 'sales_dw' AND t.TABLE_NAME = 'dws_off_offline_sale_dfn'"));
        assert!(sql.contains("t.TABLE_SCHEMA = 'sales_ads' AND t.TABLE_NAME = 'ads_off_offline_region_sale_dfn'"));
        assert!(!sql.contains("DATABASE()"));
        assert!(!sql.contains("SELECT *"));

        assert!(render_warehouse_catalog_probe(&[
            WarehouseAsset::new("sales_dw", "same_name"),
            WarehouseAsset::new("sales_ads", "same_name"),
        ])
        .is_err());
        assert!(render_warehouse_catalog_probe(&[
            WarehouseAsset::new("sales_dw; DROP", "x"),
        ])
        .is_err());
    }

    #[test]
    fn warehouse_catalog_replaces_same_base_table_and_keeps_base_key() {
        let mut snap = snapshot(
            vec![("dws_fact".into(), "默认库同名表".into(), Some(9))],
            vec![("dws_fact".into(), "wrong_col".into(), "int".into(), "".into(), 1)],
        );
        let assets = [WarehouseAsset::new("sales_dw", "dws_fact")];
        let stats = merge_warehouse_catalog_rows(
            &mut snap,
            &assets,
            vec![
                (
                    "sales_dw".into(),
                    "dws_fact".into(),
                    "已验证事实".into(),
                    Some(88),
                    "order_date".into(),
                    "date".into(),
                    "业务日期".into(),
                    1,
                ),
                (
                    "sales_dw".into(),
                    "dws_fact".into(),
                    "已验证事实".into(),
                    Some(88),
                    "amount".into(),
                    "decimal".into(),
                    "销售额".into(),
                    2,
                ),
            ],
        );
        assert_eq!(
            stats,
            WarehouseCatalogStats { requested: 1, tables: 1, columns: 2, missing: 0 }
        );
        assert_eq!(snap.tables.len(), 1);
        assert_eq!(snap.tables[0].name, "dws_fact", "meta 活性键保持 base table");
        assert!(snap.tables[0].comment.contains("sales_dw.dws_fact"));
        assert!(snap.tables[0].comment.contains("必须使用完整库表名"));
        assert_eq!(snap.columns.iter().map(|(_, c)| c.name.as_str()).collect::<Vec<_>>(), ["order_date", "amount"]);
    }

    #[test]
    fn missing_cross_database_asset_drops_default_database_namesake() {
        let mut snap = snapshot(
            vec![("dws_fact".into(), "错误同名表".into(), Some(9))],
            vec![("dws_fact".into(), "wrong_col".into(), "int".into(), "".into(), 1)],
        );
        let assets = [WarehouseAsset::new("sales_dw", "dws_fact")];
        let stats = merge_warehouse_catalog_rows(&mut snap, &assets, vec![]);
        assert_eq!(stats.requested, 1);
        assert_eq!(stats.missing, 1);
        assert!(snap.tables.is_empty());
        assert!(snap.columns.is_empty());
    }

    #[test]
    fn date_template_only_accepts_exact_typed_slots() {
        let a = chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let b = chrono::NaiveDate::from_ymd_opt(2026, 8, 4).unwrap();
        assert_eq!(
            render_date_template("x >= ? AND x < ?", &[a, b]).as_deref(),
            Some("x >= '2026-08-01' AND x < '2026-08-04'")
        );
        assert!(render_date_template("x >= ?", &[a, b]).is_none());
        assert!(render_date_template("SELECT 1", &[a]).is_none());
    }

    #[test]
    fn scoped_execution_gate_only_restricts_production_mysql() {
        let sensitive = dms_kernel::nl::lexicon::SENSITIVE_COLS;
        let analytical = "SELECT a.id, COUNT(*) FROM a JOIN b ON a.id=b.id GROUP BY a.id";
        assert_eq!(
            guard_scoped_wire(
                MysqlCapability::Warehouse,
                None,
                analytical,
                "dms",
                sensitive,
            )
            .unwrap()
            .wire(),
            analytical
        );

        let indexes = HashMap::from([(
            ("t_sales_order".to_string(), "sales_order_code".to_string()),
            DmsIndexKind::Unique,
        )]);
        let safe = guard_scoped_wire(
            MysqlCapability::ProductionLookup,
            Some(&indexes),
            "SELECT sales_order_code FROM t_sales_order WHERE sales_order_code='SO-1' LIMIT 50",
            "dms",
            sensitive,
        )
        .unwrap();
        assert!(safe.wire().ends_with("LIMIT 50"), "{}", safe.wire());

        for bad in [
            "SELECT * FROM t_sales_order",
            "SELECT COUNT(*) FROM t_sales_order WHERE sales_order_code='SO-1'",
            "SELECT * FROM t_sales_order WHERE sales_order_code LIKE '%SO%'",
            "SELECT * FROM t_sales_order WHERE sales_order_code='SO-1' ORDER BY id",
            "SELECT * FROM t_employee WHERE employee_id=1",
            "SELECT * FROM t_sales_order WHERE sales_order_code='SO-1' LIMIT 51",
            "SELECT * FROM other_db.t_sales_order WHERE sales_order_code='SO-1' LIMIT 1",
        ] {
            assert!(
                guard_scoped_wire(
                    MysqlCapability::ProductionLookup,
                    Some(&indexes),
                    bad,
                    "dms",
                    sensitive,
                )
                .is_err(),
                "生产 MySQL 不应放行: {bad}"
            );
        }
    }

    /// 🔴 A8：ds 级策略只许更紧 —— production_lookup / dms-auth 的 2s·50 行红线任何配置都放宽不了
    #[test]
    fn ds_policy_never_relaxes_production_red_line() {
        let global = (200usize, Duration::from_secs(30)); // 全局两档（dms_agent::MAX_ROWS / EXEC_TIMEOUT）
        // 默认策略（全 None）= 存量行为逐字节不变
        assert_eq!(
            effective_limits(false, DsPolicy::default(), global.0, global.1),
            (DMS_LOOKUP_MAX_ROWS, DMS_LOOKUP_TIMEOUT)
        );
        assert_eq!(effective_limits(true, DsPolicy::default(), global.0, global.1), global);
        // ds 配得更松（5000 行 / 120s）：生产仍是红线，数仓仍是调用方值
        let loose = DsPolicy { max_rows: Some(5000), timeout: Some(Duration::from_secs(120)) };
        assert_eq!(
            effective_limits(false, loose, global.0, global.1),
            (DMS_LOOKUP_MAX_ROWS, DMS_LOOKUP_TIMEOUT),
            "ds 级策略不许放宽 production_lookup 红线"
        );
        assert_eq!(effective_limits(true, loose, global.0, global.1), global);
        // ds 配得更紧：生产与数仓两条通道都收紧
        let tight = DsPolicy { max_rows: Some(20), timeout: Some(Duration::from_millis(800)) };
        assert_eq!(effective_limits(false, tight, global.0, global.1), (20, Duration::from_millis(800)));
        assert_eq!(effective_limits(true, tight, global.0, global.1), (20, Duration::from_millis(800)));
        // 只配一个维度：另一维不受影响
        let rows_only = DsPolicy { max_rows: Some(10), timeout: None };
        assert_eq!(effective_limits(false, rows_only, global.0, global.1), (10, DMS_LOOKUP_TIMEOUT));
    }
}

fn fill_blank_comments(
    snap: &mut SchemaSnapshot,
    mapped: &std::collections::HashMap<(String, String), String>,
) -> usize {
    let mut filled = 0;
    for (table, col) in &mut snap.columns {
        if col.comment.trim().is_empty() {
            if let Some(comment) = mapped.get(&(table.to_lowercase(), col.name.to_lowercase())) {
                col.comment = comment.clone();
                filled += 1;
            }
        }
    }
    filled
}
