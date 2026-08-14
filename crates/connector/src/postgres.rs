//! PG 只读源（多源阶段：客户自有 PG，或指向自有库的**只读角色**）。
//!
//! 连库实测记录（2026-07-28，K4 上传源首次真问数）：`probe_schema` 的两条探针、`attnum` 的
//! int2 解码、类型映射三处**均已在真 PG 上跑通** —— 一份中文表头 CSV 上传后采出
//! `tables=1 columns=4`，列注释（部门/员工姓名/月度销量/入职日期）与类型
//! （text/text/numeric/timestamptz）逐个正确。`SchemaSnapshot` 与 MySQL 版同结构，列数列序一致。
//!
//! 未 ANALYZE 的新表 `reltuples = -1`（原样入 `row_estimate`，不伪造成 0）：唯一做比较的
//! 消费者是 autodiscover 的 `row_estimate < 100 万`，那是手动 CLI，`-1` 在那里无害。
//!
//! ## F3 启动期自检（ARCHITECTURE §3）——本文件存在的主要理由
//! 只读源与 `OwnedStore` 指向**同一个物理 PG** 时，一条合法 SELECT 就能读到
//! `kb.chunk`（全员文档原文）与 `meta.sql_exemplar`（他人问句与 SQL）——权限体系在 SQL 层被绕开，
//! 且看起来完全正常。故 `connect()` 结束前必须确认只读源角色**看不见** meta/kb/chat，
//! 看得见就拒绝启动（fail-closed，不降级成 warn）。

use std::collections::HashSet;
use std::time::Duration;

use serde_json::Value;
use sqlx::{Column, Row, TypeInfo};

use dms_kernel::{BoxFut, Dialect, DsId, PostgresDialect, ScopedSql};

use crate::error::ConnectorError;
use crate::mysql::{redact, snapshot, sqlx_err, Cell};
use crate::source::{DsPolicy, RowSet, SchemaSnapshot, SourceKind, SqlSource};

/// F3 探针。**刻意不用** `has_schema_privilege('meta','usage')` 的字面形态：
/// 那个函数在 schema 不存在时直接报 3F000（未部署自有库的纯只读源会连不上），
/// 走 `pg_namespace` 既不报错，语义又完全一样（三个 schema 任一可见即为真）。
const OWNED_VISIBLE: &str = "SELECT coalesce(bool_or(has_schema_privilege(n.nspname, 'usage')), false)
     FROM pg_namespace n WHERE n.nspname IN ('meta', 'kb', 'chat')";

/// 建连期/健康检查探针预算：公网或慢链路下不许悬挂（mysql.rs 的目录探针 60s 先例的 PG 档）。
const CONNECT_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct PostgresSource {
    pool: sqlx::PgPool,
    ds: DsId,
    sensitive: &'static [&'static str],
    /// 上传源的 (schema, 真实关系名白名单)：两者必须同有同无，故收进同一个 Option。
    /// 普通客户 PG 源为 None。schema 闸与表白名单一起阻断 pg_catalog/public 回退。
    schema_tables: Option<(String, HashSet<String>)>,
    /// 【A8】数据源级查询策略（`fetch` 入口与调用方值取 min，只许更紧）。
    ds_policy: std::sync::Mutex<DsPolicy>,
}

impl PostgresSource {
    /// 唯一构造入口：会话级只读 + 可选 `search_path` + F3 自检。自检不过 → `Err`，服务起不来。
    ///
    /// `schema`：`Some` 时每条连接置 `search_path`。上传表格源（K4）必须给它 ——
    /// 那些源共用一条 `pg_ro_url`，schema 却一份一个，不置则 `probe_schema()`
    /// （探针按 `current_schema()` 过滤）一张表都采不到。`None` = 用库的默认。
    pub async fn connect(
        ds: DsId,
        url: &str,
        max_conn: u32,
        sensitive: &'static [&'static str],
        schema: Option<&str>,
    ) -> Result<Self, ConnectorError> {
        let schema_tables: Option<(String, HashSet<String>)> = match schema {
            Some(s) => Some((s.to_string(), HashSet::new())),
            None => None,
        };
        let set_path =
            search_path_stmt(ds.as_str(), schema_tables.as_ref().map(|(s, _)| s.as_str()))?;
        // max_conn=0 与 MySQL 侧（mysql.rs effective_max_connections）对齐钳到 ≥1
        let options: sqlx::postgres::PgConnectOptions = std::str::FromStr::from_str(url)
            .map_err(|e| ConnectorError::connect(ds.as_str(), e))?;
        // application_name 带 ds_id：运维在 pg_stat_activity 能归因连接属于哪个源
        let options = options.application_name(&format!("dms-ai-ro:{}", ds.as_str()));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(max_conn.max(1))
            .after_connect(move |conn, _| {
                let set_path = set_path.clone();
                Box::pin(async move {
                    use sqlx::Executor;
                    // 会话级只读：等价于 MySQL 侧的 SET SESSION TRANSACTION READ ONLY
                    conn.execute("SET default_transaction_read_only = on").await?;
                    if let Some(stmt) = &set_path {
                        conn.execute(stmt.as_str()).await?;
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|e| ConnectorError::connect(ds.as_str(), e))?;
        let schema_tables = match schema_tables {
            Some((s, _)) => Some((
                s.clone(),
                // 建连期探针：公网/慢链路下不能悬挂（mysql.rs 的目录探针给了 60s 先例）
                tokio::time::timeout(
                    CONNECT_PROBE_TIMEOUT,
                    sqlx::query_scalar::<_, String>(
                        "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
                         WHERE n.nspname=$1 AND c.relkind IN ('r','p')",
                    )
                    .bind(s)
                    .fetch_all(&pool),
                )
                .await
                .map_err(|_| ConnectorError::timeout(ds.as_str(), CONNECT_PROBE_TIMEOUT))?
                .map_err(|e| sqlx_err(ds.as_str(), e))?
                .into_iter()
                .map(|t| t.to_lowercase())
                .collect(),
            )),
            None => None,
        };
        let src = Self {
            pool,
            ds,
            sensitive,
            schema_tables,
            ds_policy: std::sync::Mutex::new(DsPolicy::default()),
        };
        let visible = src.owned_schema_visible().await?;
        if let Err(e) = deny_if_owned_visible(visible, src.ds.as_str()) {
            // F3 失败显式关池（与 mysql.rs:426 同口径），不靠 Drop 隐式收尾
            src.pool.close().await;
            return Err(e);
        }
        Ok(src)
    }

    /// 静态 SQL 通道（`&'static str`，数据全走 bind）：自有库上的框架自查语句。
    /// 仍受会话只读约束（`default_transaction_read_only = on`），不是写通道。
    pub fn fixed(&self, sql: &'static str) -> crate::fixed::PgStmt<'_> {
        crate::fixed::PgStmt::new(&self.pool, self.ds.as_str(), sql)
    }

    /// `/api/health` 用：授权可以在启动**之后**被改坏（谁给只读角色 GRANT 了 USAGE），
    /// 只在 connect 查一次等于只防第一天。带超时：PG 挂起时健康检查不许跟着悬挂
    /// （超时按 Err 上报 —— connect 路径 fail-closed，health 路径报不健康）。
    pub async fn owned_schema_visible(&self) -> Result<bool, ConnectorError> {
        tokio::time::timeout(
            CONNECT_PROBE_TIMEOUT,
            sqlx::query_scalar::<_, bool>(OWNED_VISIBLE).fetch_one(&self.pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(self.ds.as_str(), CONNECT_PROBE_TIMEOUT))?
        .map_err(|e| sqlx_err(self.ds.as_str(), e))
    }
}

/// `search_path` 语句的渲染 + 校验。纯函数，故「非法 schema 名当场拒」有单测守。
///
/// 🔴 schema 名进的是**语句文本**（`SET` 不吃 bind 参数），所以必须过 `SafeIdent`：
/// 这是全仓唯一一份标识符白名单（ARCHITECTURE §5），过不了就 `Err`，不许清洗后放行 ——
/// 清洗会把「配错的 schema」变成「连上了另一个 schema」，那比连不上危险。
/// 不挂 `public`：上传源缺表时绝不能回落到共享 schema；系统函数仍由隐式 `pg_catalog` 提供。
fn search_path_stmt(at: &str, schema: Option<&str>) -> Result<Option<String>, ConnectorError> {
    let Some(s) = schema else { return Ok(None) };
    if crate::ddl::SafeIdent::parse(s).is_none() {
        return Err(ConnectorError::config(at, format!("schema 名非法（不过标识符白名单）: {s}")));
    }
    // pg_catalog 即使不显式列出也会被 PostgreSQL 自动搜索；不追加 public，避免无前缀表名
    // 在上传 schema 缺表时回落到共享对象。
    Ok(Some(format!("SET search_path = \"{s}\"")))
}

/// 上传源的执行期 schema 闸。无前缀表名由唯一 search_path 解析；显式前缀只能是当前 schema。
fn enforce_schema(
    at: &str,
    allowed: Option<&str>,
    tables: Option<&HashSet<String>>,
    sql: &str,
) -> Result<(), ConnectorError> {
    let Some(allowed) = allowed else { return Ok(()) };
    let tables = tables.ok_or_else(|| ConnectorError::config(at, "上传数据源缺表白名单"))?;
    let (functions, refs) =
        dms_kernel::sql::ast::functions_and_table_refs_of(sql, &PostgresDialect)
            .map_err(|e| ConnectorError::query(at, e))?;
    if let Some(name) = functions.iter().filter_map(|parts| parts.last()).find(|name| {
        forbidden_upload_function(name)
    }) {
        return Err(ConnectorError::query(
            at,
            format!("上传数据源不允许调用动态 SQL 或服务端函数 {name}"),
        ));
    }
    for parts in refs {
        let valid_schema = match parts.as_slice() {
            [_table] => true,
            [schema, _table] => schema.eq_ignore_ascii_case(allowed),
            _ => false,
        };
        let table = match parts.last() {
            Some(t) => t,
            None => continue, // AST 实表名非空由 kernel 侧 retain 保证；这里不押 panic
        };
        if !valid_schema || !tables.contains(table) {
            return Err(ConnectorError::query(
                at,
                format!("上传数据源只允许访问 schema {allowed} 内已登记的表"),
            ));
        }
    }
    Ok(())
}

fn forbidden_upload_function(name: &str) -> bool {
    name.starts_with("dblink")
        || name.starts_with("pg_read_")
        || name.starts_with("pg_ls_")
        || name.starts_with("pg_advisory_")
        || name.starts_with("lo_")
        || name.contains("_to_xml")
        || matches!(
            name,
            "pg_stat_file"
                | "pg_logdir_ls"
                | "pg_sleep"
                | "pg_notify"
                | "pg_reload_conf"
                | "pg_rotate_logfile"
                | "pg_terminate_backend"
                | "pg_cancel_backend"
                | "set_config"
                | "nextval"
                | "setval"
                // 管理/复制函数（需特权，纵深防御缺口补齐）
                | "pg_create_restore_point"
                | "pg_switch_wal"
                | "pg_backup_start"
                | "pg_backup_stop"
                | "pg_promote"
        )
}

/// F3 判定单独成函数：可无库单测，且「可见即拒」这条不许被 warn 化。
pub(crate) fn deny_if_owned_visible(visible: bool, at: &str) -> Result<(), ConnectorError> {
    if visible {
        return Err(ConnectorError::config(
            at,
            "只读源角色可见自有库 schema（meta/kb/chat）——请 REVOKE USAGE 后再启动",
        ));
    }
    Ok(())
}

impl SqlSource for PostgresSource {
    fn ds_id(&self) -> &DsId {
        &self.ds
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Postgres
    }

    fn dialect(&self) -> &'static dyn Dialect {
        &PostgresDialect
    }

    fn set_ds_policy(&self, policy: DsPolicy) {
        // DsPolicy 是 Copy、持锁段无 panic 点：中毒直接恢复，口径与 registry.rs 一致
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
            // 【A8】入口先与 ds 级策略取 min（只许更紧），再进 schema 闸与执行
            let (max, t) =
                self.ds_policy.lock().unwrap_or_else(|e| e.into_inner()).clamp(max, t);
            let (schema, tables) = match &self.schema_tables {
                Some((s, t)) => (Some(s.as_str()), Some(t)),
                None => (None, None),
            };
            enforce_schema(at, schema, tables, sql.wire())?;
            let rows = tokio::time::timeout(t, sqlx::query(sql.wire()).fetch_all(&self.pool))
                .await
                .map_err(|_| ConnectorError::timeout(at, t))?
                .map_err(|e| sqlx_err(at, e))?;
            let (columns, mut data, truncated) = to_table(&rows, max);
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
            // 【A8】与 fetch 同口径：ds 级策略只许收紧 explain 预算（行上限对 explain 无意义）
            let (_, t) =
                self.ds_policy.lock().unwrap_or_else(|e| e.into_inner()).clamp(usize::MAX, t);
            let (schema, tables) = match &self.schema_tables {
                Some((s, t)) => (Some(s.as_str()), Some(t)),
                None => (None, None),
            };
            enforce_schema(at, schema, tables, sql.wire())?;
            let stmt = format!("EXPLAIN {}", sql.wire());
            // 只要「数据库判没判错」，计划行本身用不上：execute 不物化计划行
            match tokio::time::timeout(t, sqlx::query(&stmt).execute(&self.pool)).await {
                Ok(Ok(_)) => Ok(None),
                Ok(Err(e)) => Ok(e.as_database_error().map(|db| db.message().to_string())),
                Err(_) => Ok(None),
            }
        })
    }

    fn probe_schema<'a>(&'a self) -> BoxFut<'a, Result<SchemaSnapshot, ConnectorError>> {
        Box::pin(async move {
            let at = self.ds.as_str();
            // 两条探针并行 + 统一超时：串行无超时会让慢链路把 schema 采集挂死
            let (tables, cols) = tokio::join!(
                tokio::time::timeout(
                    CONNECT_PROBE_TIMEOUT,
                    sqlx::query_as::<_, (String, String, Option<i64>)>(self.dialect().table_probe())
                        .fetch_all(&self.pool),
                ),
                tokio::time::timeout(
                    CONNECT_PROBE_TIMEOUT,
                    // ⚠️ `pg_attribute.attnum` 是 **int2**：解成 i64 会直接类型不匹配报错
                    // （同 MySQL 侧 LONGBLOB 必须 CAST 的那类坑）。这里解 i16 再抬成契约的 i64。
                    sqlx::query_as::<_, (String, String, String, String, i16)>(
                        self.dialect().column_probe(),
                    )
                    .fetch_all(&self.pool),
                ),
            );
            let tables = tables
                .map_err(|_| ConnectorError::timeout(at, CONNECT_PROBE_TIMEOUT))?
                .map_err(|e| sqlx_err(at, e))?;
            let cols = cols
                .map_err(|_| ConnectorError::timeout(at, CONNECT_PROBE_TIMEOUT))?
                .map_err(|e| sqlx_err(at, e))?;
            let cols = cols
                .into_iter()
                .map(|(t, n, ty, c, ord)| (t, n, ty, c, ord as i64))
                .collect();
            Ok(snapshot(tables, cols))
        })
    }
}

/// 第三位 = 是否在上限处截断（与 mysql.rs 同一条：截断此前没有出口）。
fn to_table(rows: &[sqlx::postgres::PgRow], max: usize) -> (Vec<String>, Vec<Vec<Value>>, bool) {
    let Some(first) = rows.first() else { return (vec![], vec![], false) };
    let columns: Vec<String> = first.columns().iter().map(|c| c.name().to_string()).collect();
    // 类型名 → 取值路径只算一次（逐行逐 cell 重算 type_info().name() 是纯浪费）
    let kinds: Vec<Cell> =
        first.columns().iter().map(|c| pg_cell_kind(c.type_info().name())).collect();
    // 先判后收：max=0（DsPolicy 最紧档，source.rs 契约「恒空结果」）一行都不能返
    let mut data: Vec<Vec<Value>> = Vec::with_capacity(rows.len().min(max));
    for row in rows.iter().take(max) {
        data.push(row.columns().iter().enumerate().map(|(ci, _)| cell_to_json(row, ci, &kinds[ci])).collect());
    }
    let truncated = rows.len() > data.len();
    (columns, data, truncated)
}

/// PG 类型名 → 取值路径（`Cell` 与 MySQL 版共用一个枚举：下游 JSON 形态必须一致）。
/// NUMERIC 同样走字符串保精度。未列举一律 Text。
pub(crate) fn pg_cell_kind(ty: &str) -> Cell {
    match ty {
        // sqlx 的 type_info().name() 对 serial 列返回底层 INT4/INT8（serial 是建表语法不是
        // 类型），SMALLSERIAL/SERIAL/BIGSERIAL 是死分支，不收
        "INT2" | "INT4" | "INT8" => Cell::Int,
        "FLOAT4" | "FLOAT8" => Cell::Float,
        "NUMERIC" | "MONEY" => Cell::Dec,
        "DATE" | "TIMESTAMP" | "TIMESTAMPTZ" => Cell::Time,
        _ => Cell::Text,
    }
}

/// fmt_dt 的口径：秒级精度（丢毫秒）、TIMESTAMPTZ 按 UTC 渲染且不标时区 —— 与 MySQL 侧
/// 一致的既定形态，改精度/时区要两侧同改。
fn fmt_dt(d: sqlx::types::chrono::NaiveDateTime) -> Value {
    Value::from(d.format("%Y-%m-%d %H:%M:%S").to_string())
}

fn cell_to_json(row: &sqlx::postgres::PgRow, i: usize, kind: &Cell) -> Value {
    use sqlx::types::chrono::{DateTime, NaiveDate, Utc};
    match kind {
        // INT2/INT4/INT8 是三个不同的 PG 类型，i64 只解 INT8——必须逐级回落
        Cell::Int => try_get(row, i, |v: i64| Value::from(v))
            .or_else(|| try_get(row, i, |v: i32| Value::from(v)))
            .or_else(|| try_get(row, i, |v: i16| Value::from(v)))
            .unwrap_or(Value::Null),
        Cell::Float => try_get(row, i, |v: f64| Value::from(v))
            .or_else(|| try_get(row, i, |v: f32| Value::from(v)))
            .unwrap_or(Value::Null),
        Cell::Dec => try_get(row, i, |v: sqlx::types::Decimal| Value::from(v.to_string()))
            .unwrap_or(Value::Null),
        Cell::Time => try_get(row, i, fmt_dt)
            .or_else(|| try_get(row, i, |v: DateTime<Utc>| fmt_dt(v.naive_utc())))
            .or_else(|| try_get(row, i, |v: NaiveDate| Value::from(v.format("%Y-%m-%d").to_string())))
            .unwrap_or(Value::Null),
        // BOOL 列 sqlx 拒解 String，逐级回落到 bool（UUID/JSONB 等仍落 Null —— 未开 uuid
        // feature 的类型不猜，与「未列举一律 Text」的保守口径一致）
        Cell::Text => try_get(row, i, |v: String| Value::from(v))
            .or_else(|| try_get(row, i, |v: bool| Value::from(v)))
            .unwrap_or(Value::Null),
    }
}

/// 取一列：解不出（类型不符）→ `None` 交给下一个候选，SQL NULL → `Some(Value::Null)`。
/// 两者必须分开，否则 NULL 会被误判成「类型不符」一路回落到 Text。
fn try_get<'r, T, F>(row: &'r sqlx::postgres::PgRow, i: usize, f: F) -> Option<Value>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    F: FnOnce(T) -> Value,
{
    match row.try_get::<Option<T>, _>(i) {
        Ok(Some(v)) => Some(f(v)),
        Ok(None) => Some(Value::Null),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 F3：可见即拒，且文案必须带 REVOKE 指引（运维照着做就能修）
    #[test]
    fn f3_rejects_when_owned_schemas_visible() {
        let e = deny_if_owned_visible(true, "ds-7").err().unwrap();
        assert!(matches!(e, ConnectorError::Config(_)), "{e}");
        assert!(e.to_string().contains("REVOKE USAGE"), "{e}");
        assert!(e.to_string().contains("[ds-7]"), "{e}");
        assert!(deny_if_owned_visible(false, "ds-7").is_ok());
    }

    /// 🔴 `search_path` 只认过白名单的 schema 名，且**不清洗后放行**。
    #[test]
    fn search_path_is_rendered_only_for_safe_idents() {
        assert_eq!(search_path_stmt("ds-7", None).unwrap(), None);
        assert_eq!(
            search_path_stmt("ds-7", Some("up_1111_2222")).unwrap().as_deref(),
            Some("SET search_path = \"up_1111_2222\"")
        );
        for bad in ["up_x; DROP SCHEMA meta CASCADE", "public\"", "上传", "", "a-b"] {
            let e = search_path_stmt("ds-7", Some(bad)).err().unwrap_or_else(|| {
                panic!("非法 schema 名被放行：{bad:?}");
            });
            assert!(matches!(e, ConnectorError::Config(_)), "{e}");
        }
    }

    #[test]
    fn upload_source_rejects_cross_schema_table_refs() {
        let tables: HashSet<String> = ["sheet1"].into_iter().map(str::to_string).collect();
        for ok in [
            "SELECT * FROM sheet1",
            "SELECT * FROM up_a.sheet1",
            "(TABLE up_a.sheet1)",
            "SELECT sum(1), to_char(current_date, 'YYYY-MM-DD') FROM sheet1",
            "WITH x AS (SELECT * FROM up_a.sheet1) SELECT * FROM x",
        ] {
            assert!(enforce_schema("upload_d1", Some("up_a"), Some(&tables), ok).is_ok(), "{ok}");
        }
        for bad in [
            "SELECT * FROM up_b.sheet1",
            "SELECT * FROM public.sheet1",
            "SELECT * FROM app.up_a.sheet1",
            "(TABLE up_b.sheet1)",
            "(TABLE up_a.missing_table)",
            "WITH x AS (SELECT * FROM up_b.sheet1) SELECT * FROM x",
            "SELECT * FROM pg_class",
            "SELECT * FROM missing_table",
        ] {
            let e = enforce_schema("upload_d1", Some("up_a"), Some(&tables), bad).unwrap_err();
            assert!(matches!(e, ConnectorError::Query(_)), "{e}");
            assert!(e.to_string().contains("schema up_a 内已登记的表"), "{e}");
        }
        for bad in [
            "SELECT query_to_xml('SELECT * FROM up_b.sheet1', true, false, '')",
            "SELECT dblink('host=elsewhere', 'SELECT 1')",
            "SELECT pg_read_file('/etc/passwd')",
            "SELECT pg_promote()",
            "SELECT pg_switch_wal()",
        ] {
            let e = enforce_schema("upload_d1", Some("up_a"), Some(&tables), bad).unwrap_err();
            assert!(matches!(e, ConnectorError::Query(_)), "{e}");
            assert!(e.to_string().contains("不允许调用动态 SQL 或服务端函数"), "{e}");
        }
        assert!(enforce_schema("customer-pg", None, None, "SELECT * FROM any_schema.t").is_ok());
    }

    #[test]
    fn pg_cell_kind_maps_families() {
        for t in ["INT2", "INT4", "INT8"] {
            assert_eq!(pg_cell_kind(t), Cell::Int, "{t}");
        }
        assert_eq!(pg_cell_kind("FLOAT8"), Cell::Float);
        assert_eq!(pg_cell_kind("NUMERIC"), Cell::Dec);
        assert_eq!(pg_cell_kind("TIMESTAMPTZ"), Cell::Time);
        for t in ["TEXT", "VARCHAR", "JSONB", "BOOL", "UUID", ""] {
            assert_eq!(pg_cell_kind(t), Cell::Text, "{t}");
        }
    }

    /// 🔴 A8：fetch 入口必须套 ds 级策略 —— 删掉 clamp 这行，管理端配的收紧就静默失效。
    /// （min 语义由 `DsPolicy::clamp` 的单测守着，这里钉的是「接线没断」。）
    #[test]
    fn fetch_entry_clamps_with_ds_policy() {
        // include_str! 自指是故意脆的接线守卫：改名/拆文件即 panic，提醒同步这里
        let src = include_str!("postgres.rs");
        let body = src.split("impl SqlSource for PostgresSource").nth(1).expect("impl 不见了");
        let fetch = body.split("fn fetch<'a>").nth(1).expect("fetch 不见了");
        let head = fetch.split("tokio::time::timeout").next().unwrap();
        assert!(head.contains(".clamp(max, t)"), "fetch 入口（执行之前）必须与 ds 级策略取 min");
        assert!(body.contains("fn set_ds_policy"), "策略登记入口不许丢");
    }
}
