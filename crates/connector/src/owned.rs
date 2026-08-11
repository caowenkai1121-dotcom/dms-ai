//! 自有 PG（meta/kb/chat/上传表）—— 全仓**唯一可写**通道。
//!
//! ## 红线的形状：这里刻意缺席的东西比存在的东西重要
//! - **不实现 `SqlSource`**：`fetch(&ScopedSql)` 是「LLM 产物过闸门后可执行」的通道，
//!   自有库不该有这条路。类型上 `OwnedStore` 就不是一个 `dyn SqlSource`，塞不进 `AskCtx`。
//! - **没有 `execute(&str)`**：写只有两条路 —— `fixed(&'static str) + bind`（值全走占位符）
//!   与 `create_upload_table(&UploadTableSpec)`（标识符经 `SafeIdent` 白名单，DDL 由代码渲染）。
//! - **没有 `From<ScopedSql>` / `From<RawSql>` / 任何吃 `String` 的语句入口**：
//!   LLM 产物在**类型上**到不了这里，不依赖任何人记得「别把生成的 SQL 往自有库送」。
//!
//! 唯二的例外是 `pool()` 与 `dead_pg_pool_for_tests()`：前者是迁移期过渡口，后者对外交出
//! 裸 `PgPool` 但只给单测用（见其文档注释与 `#[doc(hidden)]`）。

use crate::ddl::{SafeIdent, UploadTableSpec};
use crate::error::ConnectorError;
use crate::fixed::PgStmt;
use crate::mysql::sqlx_err;

/// 自有库的错误标识：只有一个自有库，故是字面量而非 `DsId`（`DsId` 是**只读源**的身份）。
const AT: &str = "owned-pg";

/// 灌数据的单批行数。bind 数恒等于**列数**（每列一个数组），与行数无关，故这个数只影响
/// 单条语句的数组长度与内存，碰不到 PG 的 65535 参数上限。
const INSERT_BATCH: usize = 500;

/// 上传表的只读组角色名。**刻意硬编码而不进 settings**：多一个配置项就多一种
/// 「DSN 配了、角色名忘了配」的半可用状态，而这个角色名本来就该与部署脚本一致。
/// `pg_ro_url` 指向的角色要么就是它，要么是它的成员（`GRANT dms_ai_ro TO <角色>`）。
const RO_ROLE: &str = "dms_ai_ro";

/// 「PG 写不进去」的假件：指向没人监听的端口、取连接**快速失败**的池。
///
/// 🔴 为什么住在 connector：架构门禁第①条是「kernel/policy/agent **不得造连接池**」，
/// 按 `PgPoolOptions` 这个词判。上层 crate 要造死池就会当场 FAIL，而绕 grep
///（改写类型路径）是拿门禁换绿。池的构造集中在唯一允许造池的 crate ——
/// 那正是那条纪律的本意，不是绕过它。
///
/// 🔴 为什么必须压 `acquire_timeout`：`PgPool::connect_lazy` 看着够用，**实测不够** ——
/// 127.0.0.1:1 是 ECONNREFUSED，我以为内核会立刻拒、sqlx 会立刻返错；
/// 实际 sqlx 的 `acquire` 会**一直重连到 `acquire_timeout`**，默认 30s。
/// 两条判据各等 30s ⇒ 那一支单测从 0.1s 变成 `finished in 60.01s`（实测），
/// 而慢测试最后总会被人 `#[ignore]` 掉。
///
/// 只给上层 crate 的**单测**用。生产路径一律 `OwnedStore::connect`。
#[doc(hidden)] // 非生产 API：不进文档，新代码不该发现它
pub fn dead_pg_pool_for_tests(acquire: std::time::Duration) -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(acquire)
        .connect_lazy("postgres://u:p@127.0.0.1:1/db")
        .expect("固定 DSN，解析不会失败")
}

/// `Clone` 共享同一连接池（`PgPool` 是 Arc）：spawn 落账这类要 `'static` 所有权的
/// 观测写入靠它过任务边界（knowledge `qa_log` 的 fire-and-forget 形态与 server 同款）。
#[derive(Clone)]
pub struct OwnedStore {
    pool: sqlx::PgPool,
}

impl OwnedStore {
    pub async fn connect(url: &str, max_conn: u32) -> Result<Self, ConnectorError> {
        // max_conn=0 与 MySQL 侧（effective_max_connections）对齐钳到 ≥1；
        // application_name 让 pg_stat_activity 能把写通道连接与其他客户端区分开
        let options: sqlx::postgres::PgConnectOptions = std::str::FromStr::from_str(url)
            .map_err(|e| ConnectorError::connect(AT, e))?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(max_conn.max(1))
            .connect_with(options.application_name("dms-ai-owned"))
            .await
            .map_err(|e| ConnectorError::connect(AT, e))?;
        Ok(Self { pool })
    }

    /// 字面量语句通道：SQL 只能是 `&'static str`，值全走 `bind`，动态 `IN` 只能 `expand(n)`。
    pub fn fixed(&self, sql: &'static str) -> PgStmt<'_> {
        PgStmt::new(&self.pool, AT, sql)
    }

    /// 迁移期过渡口：semantic 的 30+ 个 `&PgPool` 签名与 server 的既有查询暂时还需要它。
    /// ponytail: 它不是给拼串 SQL 用的；T7/T10 收口后删掉，届时全仓只剩 `fixed()` 一条路。
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    /// 建上传表：`CREATE SCHEMA` → `CREATE TABLE` → 逐列 `COMMENT ON`。
    /// 入参是 `UploadTableSpec`（`SafeIdent` 白名单 + 代码渲染 DDL），**没有**吃裸串的重载。
    ///
    /// ponytail: 三段不在一个事务里 —— PG 的 DDL 可事务化，但半途失败的残留是
    /// 「空表 / 缺注释」，重跑幂等（`IF NOT EXISTS` + `COMMENT ON` 覆盖写）。
    /// 真需要原子性时把这里换成 `BEGIN`/`COMMIT`。
    pub async fn create_upload_table(&self, spec: &UploadTableSpec) -> Result<(), ConnectorError> {
        let what = || format!("上传表 {}.{}", spec.schema.as_str(), spec.table.as_str());
        self.create_upload_schema(&spec.schema).await.map_err(|e| e.context(&what()))?;
        self.ddl(&crate::ddl::render_create_table(spec))
            .await
            .map_err(|e| e.context(&what()))?;
        let comments = crate::ddl::render_column_comments(spec);
        if !comments.is_empty() {
            // 逐列一条 COMMENT ON = 每列一次 RTT（百列上传表被 RT 放大）；拼成一条
            // multi-statement 走 simple protocol 一次往返。语句全由 ddl.rs 从 SafeIdent 渲染。
            let joined = comments.join(";");
            sqlx::raw_sql(&joined)
                .execute(&self.pool)
                .await
                .map_err(|e| sqlx_err(AT, e).context(&what()))?;
        }
        self.grant_readonly(&spec.schema).await.map_err(|e| e.context(&what()))
    }

    /// 把上传 schema 授权给只读组角色（角色不存在 = no-op）。
    ///
    /// 为什么必须有这一步：`pg_ro_url` 按 CONFIG 要求是**看不见 `meta`/`kb`/`chat` 的角色**
    /// （`PostgresSource::connect` 的 F3 自检会拒掉 owner 角色），那个角色对刚建好的 `up_*`
    /// schema 同样没有 `USAGE` —— 不授权则 V2「上传即可问数」这条通道恒
    /// `permission denied for schema`：知识库检索与建表都正常，只有问数死掉，
    /// 是最难归因的那种半可用。建表时授一次，是唯一不需要 DBA 每次跟进的位置。
    async fn grant_readonly(&self, schema: &SafeIdent) -> Result<(), ConnectorError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_roles WHERE rolname = $1)")
                .bind(RO_ROLE)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| sqlx_err(AT, e))?;
        if !exists {
            return Ok(());
        }
        let s = schema.as_str();
        // GRANT 的角色名不加双引号：RO_ROLE 常量因此必须保持全小写形态
        // （哪天改常量引入大写，这里会静默失配 —— 要么引号要么 SafeIdent 渲染）
        self.ddl(&format!("GRANT USAGE ON SCHEMA \"{s}\" TO {RO_ROLE}")).await?;
        self.ddl(&format!("GRANT SELECT ON ALL TABLES IN SCHEMA \"{s}\" TO {RO_ROLE}")).await
    }

    /// 建 schema（幂等）。schema 名是 `SafeIdent`，渲染时再双引号包裹（双保险，与 ddl.rs 同纪律）。
    pub async fn create_upload_schema(&self, schema: &SafeIdent) -> Result<(), ConnectorError> {
        self.ddl(&format!("CREATE SCHEMA IF NOT EXISTS \"{}\"", schema.as_str())).await
    }

    /// 批量灌上传表：每列一个 `text[]`，一条 `INSERT … SELECT … FROM unnest(…)` 写一批
    /// （≤500 行）。**值全走 bind**，SQL 里不出现任何字面量；返回真正写入的行数。
    /// 批间无事务：中途失败留下前 N 批残留，由调用方（kb_api 作废即 drop schema）清理。
    ///
    /// 空串按 `NULL` 灌：缺格与空单元格在 numeric/timestamptz 列上是 `''::numeric` 语法错，
    /// 而「这一格没填」正是 NULL 的语义。脏值（numeric 列里的 `abc`）不猜不丢，让 PG 报错。
    pub async fn insert_upload_rows(
        &self,
        spec: &UploadTableSpec,
        rows: &[Vec<String>],
    ) -> Result<usize, ConnectorError> {
        if spec.columns.is_empty() {
            return Err(ConnectorError::config(AT, "零列上传表 spec 是编程错误，不是正常输入"));
        }
        if rows.is_empty() {
            return Ok(0);
        }
        let sql = render_insert_unnest(spec);
        let mut written = 0u64;
        for batch in rows.chunks(INSERT_BATCH) {
            let cols: Vec<Vec<Option<&str>>> = (0..spec.columns.len())
                .map(|i| batch.iter().map(|r| cell(r, i)).collect())
                .collect();
            let mut q = sqlx::query(&sql);
            for c in &cols {
                q = q.bind(c);
            }
            let done = q.execute(&self.pool).await.map_err(|e| sqlx_err(AT, e))?;
            written += done.rows_affected();
        }
        Ok(usize::try_from(written).unwrap_or(usize::MAX))
    }

    /// 删整个上传 schema（上传作废/租户清理）。`CASCADE`：schema 里只有我们建的上传表。
    pub async fn drop_upload_schema(&self, schema: &SafeIdent) -> Result<(), ConnectorError> {
        self.ddl(&format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE", schema.as_str()))
            .await
    }

    /// **私有**：唯一执行渲染后 DDL 的地方。入参虽是 `&str`，但两个调用点的串
    /// 全部由 `ddl.rs` 的纯函数从 `SafeIdent` 渲染而来 —— 这是它不能变 `pub` 的全部理由。
    async fn ddl(&self, stmt: &str) -> Result<(), ConnectorError> {
        // DDL 全静态无业务值，可以安全记日志；成功路径留 debug 痕（建表/授权/删 schema 不再全静默）
        let r = sqlx::query(stmt)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|e| sqlx_err(AT, e));
        if r.is_ok() {
            tracing::debug!(stmt = &stmt[..stmt.len().min(80)], "owned DDL 执行成功");
        }
        r
    }
}

/// 第 `i` 格的值：行比表头短时 `get` 返 `None`；空串与全空白串都当 `NULL`
/// （见 `insert_upload_rows`；与 ddl.rs `infer_col_type` 先 trim 再判空的口径一致）。
/// 行比表头长时多出来的格直接丢 —— 表头是列数的唯一真相源。
fn cell(row: &[String], i: usize) -> Option<&str> {
    row.get(i).map(String::as_str).filter(|s| !s.trim().is_empty())
}

/// `INSERT INTO "s"."t" ("a","b") SELECT u.c1::numeric,u.c2::text FROM unnest($1::text[],$2::text[]) AS u(c1,c2)`
///
/// **为什么不是 `ddl::render_insert`**（单行 `VALUES ($1,$2)`）：值按 text bind 时 sqlx 会在
/// `Parse` 里报出 text 的 OID，而 PG 没有 text→numeric 的**赋值**转换 —— 单行形态会在第一个
/// 金额列上直接报「column is of type numeric but expression is of type text」。故转换必须显式
/// 写进 SQL，类型只从 `ColType::pg_type()` 取（一行映射，不是第二套按类型分发的代码）。
/// 副产物：往返次数从「每行一次」降到「每 500 行一次」。
///
/// 安全论证与 `render_create_table` 同款：标识符在类型上只能是 `SafeIdent`，渲染时再双引号
/// 包裹；`$n` 之外不产生任何值。
/// ponytail: 这个渲染器该住在 `ddl.rs`（与另两个渲染器同处一个 review 面）；本轮 ddl.rs 不在
/// 改动范围，下次动它时把它和无消费者的 `render_insert` 一起收拢过去。
fn render_insert_unnest(spec: &UploadTableSpec) -> String {
    use std::fmt::Write as _;
    let mut names = String::new();
    let mut casts = String::new();
    let mut arrays = String::new();
    let mut alias = String::new();
    for (i, c) in spec.columns.iter().enumerate() {
        let n = i + 1;
        let sep = if i > 0 { "," } else { "" };
        write!(names, "{sep}\"{}\"", c.name.as_str()).expect("写 String 不会失败");
        write!(casts, "{sep}u.c{n}::{}", c.ty.pg_type()).expect("写 String 不会失败");
        write!(arrays, "{sep}${n}::text[]").expect("写 String 不会失败");
        write!(alias, "{sep}c{n}").expect("写 String 不会失败");
    }
    format!(
        "INSERT INTO \"{}\".\"{}\" ({}) SELECT {} FROM unnest({}) AS u({})",
        spec.schema.as_str(),
        spec.table.as_str(),
        names,
        casts,
        arrays,
        alias
    )
}

// 本文件的单测只覆盖 `render_insert_unnest` 这一个纯函数：其余全是 IO，断言 `format!` 的
// 输出等于自己会长成重言式测试（改代码必须同步改断言，什么都拦不住），执行语义归连库验收。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::{build_columns, ColType};

    /// 灌数据语句的两条安全属性：① 值只有 `$n`，一个字面量都没有；② 每个标识符都带双引号。
    #[test]
    fn insert_renders_only_placeholders_and_quoted_idents() {
        let spec = UploadTableSpec {
            schema: SafeIdent::parse("up_1").unwrap(),
            table: SafeIdent::parse("t0_sales").unwrap(),
            columns: build_columns(&[("金额", ColType::Numeric), ("日期", ColType::Timestamptz)]),
        };
        assert_eq!(
            render_insert_unnest(&spec),
            "INSERT INTO \"up_1\".\"t0_sales\" (\"c0\",\"c1\") \
             SELECT u.c1::numeric,u.c2::timestamptz \
             FROM unnest($1::text[],$2::text[]) AS u(c1,c2)"
        );
    }

    /// 空串与缺格都灌 NULL；多出表头的格丢弃
    #[test]
    fn cell_maps_blank_and_missing_to_null() {
        let row = vec!["1".to_string(), String::new()];
        assert_eq!(cell(&row, 0), Some("1"));
        assert_eq!(cell(&row, 1), None, "空串当 NULL");
        assert_eq!(cell(&row, 2), None, "缺格当 NULL");
        let blank = vec!["   ".to_string()];
        assert_eq!(cell(&blank, 0), None, "全空白串当 NULL（与 infer_col_type 口径一致）");
    }

    /// 三列（含 Text）：casts 与 alias 编号必须一一对齐，列错位无断言守会静默写错列
    #[test]
    fn insert_aligns_casts_and_aliases_for_three_columns() {
        let spec = UploadTableSpec {
            schema: SafeIdent::parse("up_1").unwrap(),
            table: SafeIdent::parse("t1").unwrap(),
            columns: build_columns(&[
                ("金额", ColType::Numeric),
                ("名称", ColType::Text),
                ("日期", ColType::Timestamptz),
            ]),
        };
        assert_eq!(
            render_insert_unnest(&spec),
            "INSERT INTO \"up_1\".\"t1\" (\"c0\",\"c1\",\"c2\") \
             SELECT u.c1::numeric,u.c2::text,u.c3::timestamptz \
             FROM unnest($1::text[],$2::text[],$3::text[]) AS u(c1,c2,c3)"
        );
    }

    /// 零列 spec 是编程错误：发请求之前就该 Config 报错（死池证明没碰 DB）
    #[tokio::test]
    async fn zero_column_spec_is_config_error_before_touching_db() {
        let store = OwnedStore {
            pool: crate::owned::dead_pg_pool_for_tests(std::time::Duration::from_millis(50)),
        };
        let spec = UploadTableSpec {
            schema: SafeIdent::parse("up_1").unwrap(),
            table: SafeIdent::parse("t0").unwrap(),
            columns: vec![],
        };
        let rows = vec![vec!["1".to_string()]];
        let err = store.insert_upload_rows(&spec, &rows).await.unwrap_err();
        assert!(matches!(err, ConnectorError::Config(_)), "{err}");
    }
}
