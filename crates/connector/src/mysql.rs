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
    gate_dms_scoped_with, DmsIndexKind, DmsLookupSql, DMS_LOOKUP_MAX_ROWS,
};
use dms_kernel::{BoxFut, Dialect, DsId, MysqlDialect, ScopedSql};

use crate::error::ConnectorError;
use crate::dms_lookup::{registered_lookup_keys, registered_lookup_kind, REGISTRY};
use crate::fixed::FixedStmt;
use crate::source::{ColumnInfo, DsPolicy, RowSet, SchemaSnapshot, SourceKind, SqlSource, TableInfo};

pub const DMS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);
pub const DMS_LOOKUP_MAX_CONCURRENCY: u32 = 2;
/// 生产点查索引的启动核验预算：一次性、逐表 SHOW INDEX，与用户点查的 2s 红线分开。
const LOOKUP_INDEX_VERIFY_TIMEOUT: Duration = Duration::from_secs(30);
/// 会话只读位核验的启动预算。**一次性**，同样与用户点查的 2s 红线分开（同 `LOOKUP_INDEX_VERIFY_TIMEOUT`）。
///
/// 🔴 由来（2026-08-14）：这条核验原来借用 `DMS_LOOKUP_TIMEOUT`（2s），而公网链路上
/// 一次 RTT 就 1.1s、连接池获取偶尔 2.4s —— 于是核验超时 → 按「无法核验」拒绝 → **服务起不来**
/// （实测三次重建里两次挂在这里）。判据本身仍 fail-closed：超时照旧拒绝，只是别把
/// 「链路慢」误判成「不是只读」。
const SESSION_READ_ONLY_VERIFY_TIMEOUT: Duration = Duration::from_secs(15);
// 数仓目录探针只读 information_schema。公网链路实测单条 ~27s（2026-08-08 切换公网后），
// 10s 在内网够用、公网必超时 —— 探针失败是启动硬失败，超时要按链路预算给。
const WAREHOUSE_CATALOG_TIMEOUT: Duration = Duration::from_secs(60);

// 进程级全局信号量是有意的：今天全进程只有一个生产源，2 槽阀门对所有点查统一生效。
// 若未来多生产源共存，必须改成按源各一把，否则多源共享 2 槽会互相饿死。
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

    /// 错误文案里的能力名：拒绝原因写出实际能力，不再硬编码 "production_lookup"
    fn label(self) -> &'static str {
        match self {
            Self::ProductionLookup => "production_lookup",
            Self::IdentityPermission => "identity_permission",
            Self::Warehouse => "warehouse",
        }
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
        self.pool.read().unwrap_or_else(|e| e.into_inner()).pool.clone()
    }

    /// 【A8】策略快照（`Mutex` 不跨 await：拷贝完即放锁）。
    fn ds_policy(&self) -> DsPolicy {
        *self.ds_policy.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 查询池和目标类型必须在同一把锁内取快照：避免刚判定为 Doris，热切换后却从生产
    /// MySQL 新池执行未套轻查询闸门的 SQL。
    fn execution_target(
        &self,
    ) -> (sqlx::MySqlPool, MysqlCapability, Option<std::sync::Arc<LookupIndexes>>) {
        let state = self.pool.read().unwrap_or_else(|e| e.into_inner());
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
        // 生产能力在 connect_read_only 内已逐连接核验会话只读，这里只补数仓的账号三级
        // 权限核验 —— 生产路径不再多付一次重复核验 RTT。
        if capability.is_warehouse() && !pool_read_only(&new, capability).await {
            return Err(ConnectorError::config(
                self.ds.as_str(),
                "目标库不是只读会话或只读授权账号 —— 拒绝换入可写库",
            ));
        }
        *self.pool.write().unwrap_or_else(|e| e.into_inner()) = PoolState {
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
        let state = self.pool.read().unwrap_or_else(|e| e.into_inner());
        (state.target.clone(), state.capability.is_warehouse())
    }

    /// 仅用于启动时：连接已按运行时配置建立，补上该配置在目录中的名字。
    pub fn set_target_name(&self, target: &str) {
        self.pool.write().unwrap_or_else(|e| e.into_inner()).target = target.to_string();
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
        // 【A8】与 fetch 同口径：ds 级策略只许进一步收紧点查红线
        let (max, t) =
            effective_limits(false, self.ds_policy(), DMS_LOOKUP_MAX_ROWS, DMS_LOOKUP_TIMEOUT);
        let deadline = tokio::time::Instant::now() + t;
        let _slot = self.acquire_lookup_slot(deadline).await?;
        let started = std::time::Instant::now();
        tokio::time::timeout_at(deadline, async {
            let rows = sqlx::raw_sql(sql.wire())
                .fetch_all(&pool)
                .await
                .map_err(|e| sqlx_err(at, e))?;
            // 生产点查慢查询留痕（与 fixed.rs 的 500ms 慢日志同档，只加日志）
            let elapsed = started.elapsed();
            if elapsed > Duration::from_millis(500) {
                tracing::warn!(ds = at, elapsed_ms = elapsed.as_millis(), "生产业务点查偏慢");
            }
            let (columns, mut data, truncated) = to_table(&rows, max);
            let redacted = redact(self.sensitive, &columns, &mut data);
            Ok(RowSet { columns, rows: data, redacted, truncated })
        })
        .await
        .map_err(|_| ConnectorError::timeout(at, DMS_LOOKUP_TIMEOUT))?
    }

    async fn acquire_lookup_slot(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<tokio::sync::SemaphorePermit<'static>, ConnectorError> {
        tokio::time::timeout_at(deadline, production_lookup_slots().acquire())
            .await
            // 报「实际愿意等多久」（deadline 剩余量），不是满额预算
            .map_err(|_| {
                ConnectorError::timeout(
                    self.ds.as_str(),
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                )
            })?
            // 防御分支：进程级信号量从不 close，正常不可达
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
                format!("{} 禁止 raw 静态分析查询", capability.label()),
            ));
        }
        // 数仓静态查询也必须有上限：公网链路挂住不能等到 TCP 层才死
        let rows = tokio::time::timeout(
            WAREHOUSE_CATALOG_TIMEOUT,
            sqlx::raw_sql(sql).fetch_all(&pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(at, WAREHOUSE_CATALOG_TIMEOUT))?
        .map_err(|e| sqlx_err(at, e))?;
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
                format!("{} 禁止全库 schema 探针；仅允许静态业务键索引核验", capability.label()),
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
                let (database, table, comment, row_estimate, name, data_type, col_comment, ordinal_text): WarehouseCatalogRow =
                    FromRow::from_row(row).map_err(|e| sqlx_err(at, e))?;
                let ordinal = ordinal_text.parse::<i64>().map_err(|e| {
                    sqlx_err(at, sqlx::Error::Decode(Box::new(e)))
                })?;
                Ok((
                    database,
                    table,
                    comment,
                    row_estimate.and_then(|v| v.parse::<i64>().ok()),
                    name,
                    data_type,
                    col_comment,
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
                format!("{} 禁止读取数仓字段映射", capability.label()),
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
            Err(e) => {
                tracing::info!(reason = "warehouse_mapping_unavailable", err = %e, "DMS 数仓字段映射不可用，保留原生 schema 注释");
                return Ok(0);
            }
        };
        let mapped: std::collections::HashMap<(String, String), String> = rows
            .into_iter()
            .map(|(table, column, comment)| ((table.to_ascii_lowercase(), column.to_ascii_lowercase()), comment))
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
                format!("{} 禁止日期模板分析查询", capability.label()),
            ));
        }
        // 与 raw_all 同理：数仓日期模板查询也要包上限
        let rows = tokio::time::timeout(
            WAREHOUSE_CATALOG_TIMEOUT,
            sqlx::raw_sql(&sql).fetch_all(&pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(at, WAREHOUSE_CATALOG_TIMEOUT))?
        .map_err(|e| sqlx_err(at, e))?;
        rows.iter().map(|row| T::from_row(row).map_err(|e| sqlx_err(at, e))).collect()
    }

    pub async fn ping(&self) -> bool {
        tokio::time::timeout(
            DMS_LOOKUP_TIMEOUT,
            sqlx::raw_sql("SELECT 1").fetch_one(&self.pool()),
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
    tracing::info!(
        backend = if capability.is_warehouse() { "warehouse" } else { "production_lookup" },
        ds = ds.as_str(),
        max_conn,
        "{}",
        if capability.is_warehouse() { "创建数仓只读连接池" } else { "创建只读业务连接池" }
    );
    let out = if capability.is_warehouse() {
        let opts = MySqlConnectOptions::from_str(url)
            .map_err(|_| ConnectorError::config(ds.as_str(), "数据库连接地址格式无效"))?
            .pipes_as_concat(false)
            .no_engine_substitution(false)
            .timezone(None)
            .set_names(false);
        // 🔴 服务端也要有超时闸。客户端 `EXEC_TIMEOUT`（30s）只是**本地放弃等待**：
        // 连接被丢回池里之前那条查询在 Doris 上仍然跑到底，白烧数仓资源，几条并发大查询
        // 会互相拖垮（生产点查那条分支一直有 `MAX_EXECUTION_TIME=2000`，数仓这条一条都没设）。
        // 上限取客户端超时 + 余量：先让客户端超时报出可读文案，服务端兜底收尸。
        // 两条 SET 都**失败不阻断建连**：`query_timeout` 是 Doris 原生（秒），
        // `max_execution_time` 是 MySQL 兼容口（毫秒），同构数仓未必两个都认 ——
        // `after_connect` 返 Err 会让整条连接建不起来，那是把「少一道兜底」升级成「连不上」。
        pool.after_connect(|conn, _| {
            Box::pin(async move {
                use sqlx::Executor;
                for stmt in [
                    "SET query_timeout = 45",
                    "SET SESSION MAX_EXECUTION_TIME=45000",
                ] {
                    if let Err(e) = conn.execute(stmt).await {
                        tracing::debug!(stmt, err = %e, "数仓会话超时设置未生效（该方言不认，忽略）");
                    }
                }
                Ok(())
            })
        })
        .connect_with(opts)
        .await
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
    let pool = out.map_err(|e| {
        // 建池失败保留底层错误留痕（认证失败 vs 网络不可达要能分清），对外仍归一为连接不可用
        tracing::warn!(reason = "mysql_pool_connect_failed", ds = ds.as_str(), err = %e, "建只读池失败");
        connection_unavailable(ds.as_str())
    })?;
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

/// 静态日期模板的 `?` 替换。**约束**：`?` 计数包含模板字符串字面量内的 `?`
/// （`'a?b'` 这类字面量会吃掉一个槽位）——静态模板由代码评审保证不把 `?` 写进字面量。
fn render_date_template(template: &str, dates: &[chrono::NaiveDate]) -> Option<String> {
    if template.matches('?').count() != dates.len() {
        return None;
    }
    let mut sql = String::with_capacity(template.len() + dates.len() * 12);
    // 计数已校验：split 段数恒 == dates.len() + 1，zip 不会提前耗尽
    let mut parts = template.split('?');
    sql.push_str(parts.next().expect("split 恒产出首段"));
    for (date, head) in dates.iter().zip(parts) {
        sql.push('\'');
        sql.push_str(&date.format("%F").to_string());
        sql.push('\'');
        sql.push_str(head);
    }
    Some(sql)
}

/// 【测试连通性】建一次性池验证 DSN：连得上 + 会话只读 + `SELECT 1`。
/// 返回 (延迟毫秒, 服务端版本)。**不产生** `SqlSource`（它只用于页面「测试」按钮 ——
/// 与 `swap_pool` 的验证同构但不换任何东西）。
/// 注意：生产能力下延迟包含完整索引核验（30s 预算），页面按钮的等待远超一次 ping 是预期。
pub async fn test_pool(
    ds: &DsId,
    url: &str,
    capability: MysqlCapability,
) -> Result<(u128, String), ConnectorError> {
    let t0 = std::time::Instant::now();
    let (pool, _) = connect_read_only(ds, url, 1, capability).await?;
    if !pool_read_only(&pool, capability).await {
        pool.close().await;
        return Err(ConnectorError::config(
            ds.as_str(),
            "目标库不是只读会话或只读授权账号 —— 可写库不进本系统",
        ));
    }
    // 数仓走公网链路（单条探针实测 ~27s），版本探测不能套生产点查的 2s 预算
    let budget =
        if capability.is_warehouse() { WAREHOUSE_CATALOG_TIMEOUT } else { DMS_LOOKUP_TIMEOUT };
    let rows = tokio::time::timeout(
        budget,
        sqlx::raw_sql("SELECT CAST(VERSION() AS CHAR)").fetch_all(&pool),
    )
        .await
        .map_err(|_| ConnectorError::timeout(ds.as_str(), budget))?
        .map_err(|e| sqlx_err(ds.as_str(), e))?;
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
    // 一次迭代同时收 registered 映射与 tables 集合（不调两遍 registered_lookup_keys）
    let mut registered: HashMap<(String, String), DmsIndexKind> = HashMap::new();
    let mut tables = std::collections::BTreeSet::new();
    for (table, column, kind) in registered_lookup_keys() {
        registered.insert((table.to_ascii_lowercase(), column.to_ascii_lowercase()), kind);
        tables.insert(table);
    }
    // 启动/热切换时的一次性核验（逐表查 information_schema.STATISTICS，不是逐表 SHOW INDEX）。
    // 不是用户查询：2s 的用户点查预算在公网链路上连十几张表的 RTT 都不够
    // （2026-08-08 实测公网核验必超时 → 点查整关）。
    let deadline = tokio::time::Instant::now() + LOOKUP_INDEX_VERIFY_TIMEOUT;
    let mut verified: LookupIndexes = HashMap::new();
    for table in tables {
        if !table.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            return Err(ConnectorError::config(
                at,
                format!("生产点查索引登记表名不合法: {table}"),
            ));
        }
        let table_lc = table.to_ascii_lowercase();
        // 全列 CAST AS CHAR：SHOW INDEX 的列型随 MySQL 版本漂移（8.0.28 实测按名解码
        // 直接失败），information_schema + 全文本投影是唯一可移植形态（探针同款）。
        // IS_VISIBLE 只存在 8.0+，不查它：不可见索引是 DBA 的显式动作，按可见处理
        // （与旧代码 Visible 解码失败时 unwrap_or(true) 同语义）。
        let stmt = format!(
            "SELECT CAST(INDEX_NAME AS CHAR), CAST(SEQ_IN_INDEX AS CHAR),                     CAST(COLUMN_NAME AS CHAR), CAST(NON_UNIQUE AS CHAR),                     CAST(SUB_PART AS CHAR), CAST(INDEX_TYPE AS CHAR)             FROM information_schema.STATISTICS             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = '{table}'"
        );
        let rows = tokio::time::timeout_at(deadline, sqlx::raw_sql(&stmt).fetch_all(pool))
            .await
            .map_err(|_| ConnectorError::timeout(at, LOOKUP_INDEX_VERIFY_TIMEOUT))?
            .map_err(|e| sqlx_err(at, e))?;
        let mut indexes: BTreeMap<String, Vec<(i64, String, i64, Option<i64>, String)>> =
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
            // IS_VISIBLE 不查（见上注释）：不可见索引按可见处理，故元组不带 visible 字段
            indexes.entry(index).or_default().push((seq, column, non_unique, sub_part, index_type));
        }
        for columns in indexes.values_mut() {
            // or_default 只产出非空 Vec：first 的 None 分支不可达；只要 SEQ 最小者，用
            // min_by_key 免全排序（稳定排序的首元素 == 首个最小值，语义同）
            let (seq, column, non_unique, sub_part, index_type) =
                columns.iter().min_by_key(|column| column.0).expect("or_default 保证非空");
            if *seq != 1
                || sub_part.is_some()
                || !(index_type.eq_ignore_ascii_case("BTREE") || index_type.eq_ignore_ascii_case("HASH"))
            {
                continue;
            }
            let key = (table_lc.clone(), column.to_ascii_lowercase());
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
    if sql.lookup_cols().is_empty() {
        return Err(ConnectorError::config(
            at,
            "生产业务点查缺少业务键条件（空键集），拒绝生产点查",
        ));
    }
    let table_lc = sql.table().to_ascii_lowercase();
    if sql.lookup_cols().iter().all(|column| {
        let required = registered_lookup_kind(sql.table(), column);
        let found = verified
            .get(&(table_lc.clone(), column.to_ascii_lowercase()))
            .copied();
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
        SESSION_READ_ONLY_VERIFY_TIMEOUT,
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
    // 数仓走公网链路（单条探针实测 ~27s）：账号权限核验不能用生产点查的 2s 预算
    let rows = match tokio::time::timeout(
        WAREHOUSE_CATALOG_TIMEOUT,
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

/// 全分区扫描判据的下限：分区数少于它一律不判（小表全扫是正常的，不该逼用户加时间条件）。
const FULL_SCAN_PARTITION_FLOOR: u32 = 8;

/// EXPLAIN 结果行 → 一段纯文本。Doris 的计划是单列多行；列名各版本不同，故按**位置**取第 0 列，
/// 取不出来的行跳过（计划里混进非文本列不该让整条判据失效）。
fn plan_text(rows: &[sqlx::mysql::MySqlRow]) -> String {
    rows.iter()
        .filter_map(|row| row.try_get::<String, _>(0).ok())
        .collect::<Vec<_>>()
        .join("
")
}

impl SqlSource for ReadOnlyMySql {
    fn ds_id(&self) -> &DsId {
        &self.ds
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Mysql
    }

    fn is_warehouse(&self) -> bool {
        self.pool.read().unwrap_or_else(|e| e.into_inner()).capability.is_warehouse()
    }

    fn dialect(&self) -> &'static dyn Dialect {
        &MysqlDialect
    }

    fn set_ds_policy(&self, policy: DsPolicy) {
        *self.ds_policy.lock().unwrap_or_else(|e| e.into_inner()) = policy;
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
                Some(self.acquire_lookup_slot(deadline).await?)
            };
            let started = std::time::Instant::now();
            let rows = tokio::time::timeout_at(deadline, sqlx::raw_sql(wire).fetch_all(&pool))
                .await
                .map_err(|_| ConnectorError::timeout(at, t))?
                .map_err(|e| sqlx_err(at, e))?;
            // scoped fetch 慢查询留痕（与 fixed.rs 的 500ms 慢日志同档，只加日志）
            let elapsed = started.elapsed();
            if elapsed > Duration::from_millis(500) {
                tracing::warn!(ds = at, elapsed_ms = elapsed.as_millis(), warehouse, "scoped fetch 偏慢");
            }
            let (columns, mut data, truncated) = to_table(&rows, max);
            // Doris 的 0 行结果补列名；生产点查禁止额外 DESCRIBE 往返。
            let columns = match columns.is_empty() {
                true if warehouse => describe_columns(&pool, wire, at, t).await?,
                true => vec![],
                false => columns,
            };
            let redacted = redact(self.sensitive, &columns, &mut data);
            Ok(RowSet { columns, rows: data, redacted, truncated })
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
            // 【A8】与 fetch 同口径：ds 级策略只许收紧 explain 预算（行上限无意义，给满）
            let (_, t) = effective_limits(true, self.ds_policy(), usize::MAX, t);
            // 非数仓已在上面早退（Ok(None)），这里只剩 Doris 的通用分析 EXPLAIN。
            let stmt = format!("EXPLAIN {wire}");
            match tokio::time::timeout(t, sqlx::raw_sql(&stmt).fetch_all(&pool)).await {
                // 计划文本已经付过这次往返了 —— 里面写着 partitions=N/N 的就别再让用户
                // 等满 EXEC_TIMEOUT 才拿到一句「超时」。判据是纯函数（`source::scan_verdict`），
                // 判词走的是与「数据库明确报错」同一个 `Some` 口子：进 repair 回炉，不 fail-closed。
                Ok(Ok(rows)) => Ok(crate::source::scan_verdict(&plan_text(&rows), FULL_SCAN_PARTITION_FLOOR)),
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
                    format!("{} 禁止全库 schema autodiscover", capability.label()),
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
    // 包超时：公网链路挂住时不能一路等到 TCP 层（目录探针的 60s 同款预算）。
    let table_rows = tokio::time::timeout(
        WAREHOUSE_CATALOG_TIMEOUT,
        sqlx::raw_sql(dialect.table_probe()).fetch_all(pool),
    )
    .await
    .map_err(|_| ConnectorError::timeout(at, WAREHOUSE_CATALOG_TIMEOUT))?
    .map_err(|e| sqlx_err(at, e))?;
    let tables = table_rows
        .iter()
        .map(|row| {
            let (name, comment, rows): (String, String, Option<String>) =
                FromRow::from_row(row).map_err(|e| sqlx_err(at, e))?;
            Ok((name, comment, rows.and_then(|v| v.parse::<i64>().ok())))
        })
        .collect::<Result<Vec<_>, ConnectorError>>()?;
    let column_rows = tokio::time::timeout(
        WAREHOUSE_CATALOG_TIMEOUT,
        sqlx::raw_sql(dialect.column_probe()).fetch_all(pool),
    )
    .await
    .map_err(|_| ConnectorError::timeout(at, WAREHOUSE_CATALOG_TIMEOUT))?
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
            // 列去重是 O(n²)：单表列数十级，规模假设成立，不值得上 HashSet
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
    for mut table in found.into_values() {
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
    let checked = gate_dms_scoped_with(wire, &MysqlDialect, sensitive, &REGISTRY)
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

/// sqlx 错误 → `ConnectorError`，与 `fixed::classify` **变体**同口径（只有 `Database` 归 `Query`，
/// 因为只有它是「数据库明确判定语句有问题」= 可拿去 repair；文案分工见 classify 注释）。
/// `postgres.rs`/`owned.rs` 共用。
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
    tracing::warn!(reason = "sensitive_columns_redacted", columns = ?names, "结果集敏感列已置空");
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
/// 整段包在 `budget` 内：数仓链路挂住时 fetch 不能因回填失去上限。
async fn describe_columns(
    pool: &sqlx::MySqlPool,
    sql: &str,
    at: &str,
    budget: Duration,
) -> Result<Vec<String>, ConnectorError> {
    use sqlx::Executor as _;
    async fn inner(
        pool: &sqlx::MySqlPool,
        sql: &str,
        at: &str,
    ) -> Result<Vec<String>, ConnectorError> {
        // acquire 失败同样降级空列（与 describe 失败同口径，不让回填步骤有两种失败口径）
        let mut conn = match pool.acquire().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!(reason = "describe_acquire_failed", ds = at, err = %e, "DESCRIBE 取连接失败（空列返回）");
                return Ok(vec![]);
            }
        };
        match conn.describe(sql).await {
            Ok(d) => Ok(d.columns().iter().map(|c| c.name().to_string()).collect()),
            Err(e) => {
                // 错误与 SQL 指纹都得带上：裸「空列返回」告警看不出是哪条查询、什么原因
                // （实测 Doris 对含子查询/中文别名的语句 describe 不稳），刷屏却无从归因。
                tracing::warn!(reason = "describe_columns_failed", ds = at, err = %e,
                    sql = %sql.chars().take(160).collect::<String>(),
                    "DESCRIBE 回填列名失败（空列返回）");
                Ok(vec![])
            }
        }
    }
    match tokio::time::timeout(budget, inner(pool, sql, at)).await {
        Ok(res) => res,
        Err(_) => {
            tracing::warn!(reason = "describe_columns_timeout", ds = at, "DESCRIBE 回填列名超时（空列返回）");
            Ok(vec![])
        }
    }
}

/// 行集 → (列名, JSON 行)。列名取自首行（无行则空列，与拆分前逐行等价）；`max` 行即截断不报错。
/// 返回 `(列名, 行, 是否在上限处截断)`。第三位是 2026-08-14 补的：截断此前没有出口，
/// 调用方只能拿 `row_count >= MAX_ROWS` 反推，而 ds 策略把上限压到 50 时那条恒为假。
fn to_table(rows: &[sqlx::mysql::MySqlRow], max: usize) -> (Vec<String>, Vec<Vec<Value>>, bool) {
    let mut columns: Vec<String> = vec![];
    let mut data: Vec<Vec<Value>> = vec![];
    for (i, row) in rows.iter().enumerate() {
        // 先判后收：max=0（DsPolicy 最紧档，source.rs 契约「恒空结果」）一行都不能返
        if data.len() >= max {
            break;
        }
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
    }
    // 「取回来的行数 > 允许放进结果的行数」＝这次确实被截断了
    let truncated = rows.len() > data.len();
    (columns, data, truncated)
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
        | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED" | "BIGINT UNSIGNED"
        | "YEAR" => Cell::Int,
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
            })
            // MySQL TIME 型 sqlx 解为 NaiveTime（按 sqlx 类型名表补的第三回落，未经带库验证）
            .or_else(|| {
                row.try_get::<Option<sqlx::types::chrono::NaiveTime>, _>(i)
                    .ok()
                    .flatten()
                    .map(|t| Value::from(t.format("%H:%M:%S").to_string()))
            })),
        Cell::Text => get(row.try_get::<Option<String>, _>(i).ok().flatten().map(Value::from)),
    }
}

/// 无库无网：脱敏与类型映射都是纯函数。
#[cfg(test)]
mod tests {

    /// 🔴 截断必须有出口：`to_table` 到上限就 break，而「截断了」此前只能靠
    /// `row_count >= MAX_ROWS` 反推 —— ds 策略把上限压到 50/20 时那条**恒为假**，
    /// 于是几十行的结果被当成全量呈现（2026-08-14 审计）。
    #[test]
    fn to_table_reports_truncation_at_the_cap() {
        // `MySqlRow` 造不出来，判据打在源码形状上：截断标志必须来自「取回行数 > 放行行数」，
        // 不能是别的推断（比如 `data.len() == max` —— 恰好等于上限但没被截断时会假报）。
        let src = include_str!("mysql.rs");
        let body = src.split("fn to_table(").nth(1).expect("to_table 改名了");
        let head = body.split("
}
").next().unwrap();
        assert!(
            head.contains("let truncated = rows.len() > data.len();"),
            "截断判据不对或没了：{head}"
        );
        assert!(head.contains("(columns, data, truncated)"), "截断标志没交出去：{head}");
    }
    /// 🔴 数仓连接必须带**服务端**超时闸：客户端 `EXEC_TIMEOUT` 只是本地放弃等待，
    /// 连接归池前那条查询在 Doris 上仍跑到底 —— 几条并发大查询互相拖垮（2026-08-13 审计）。
    /// 判据打在源码上：这段要连库才能跑，而「有没有设」是形状问题。
    #[test]
    fn warehouse_pool_sets_a_server_side_timeout() {
        let src = include_str!("mysql.rs");
        let body = src
            .split("let out = if capability.is_warehouse() {")
            .nth(1)
            .expect("建池分支改名了 —— 顺手把这条判据一起改")
            .split("} else {")
            .next()
            .unwrap();
        assert!(body.contains("after_connect"), "数仓分支没有 after_connect：{body}");
        assert!(body.contains("query_timeout"), "缺 Doris 原生超时闸：{body}");
        // 失败不阻断建连：返 Err 会把「少一道兜底」升级成「连不上」
        assert!(
            body.contains("if let Err(e) = conn.execute(stmt).await"),
            "方言不认这两个变量时必须忽略而不是让整条连接建不起来：{body}"
        );
    }

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
        assert_eq!(cell_kind("MEDIUMINT UNSIGNED"), Cell::Int, "sqlx 类型名表里的无符号中整型");
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
        // mapped 的键已全小写（enrich_dms_snapshot 建表时统一过小写化）；
        // 每空白列一次 (table, col) 小写分配做查找 —— 空白列数量有限，不为省分配换键类型
        if col.comment.trim().is_empty() {
            if let Some(comment) =
                mapped.get(&(table.to_ascii_lowercase(), col.name.to_ascii_lowercase()))
            {
                col.comment = comment.clone();
                filled += 1;
            }
        }
    }
    filled
}
