//! 字面量语句通道（裁决 C1）：框架自查用的 SQL **只能是 `&'static str`**，数据全走 bind，
//! 动态 `IN` 只有一条路 —— 模板里写 `{in}`，调用方 `expand(n)`。
//!
//! 它解决的是一个具体的历史形态：`scope.rs` 的 7 处 `format!("... IN ({})", placeholders(n))`
//! —— 拼串本身安全（只拼 `?`），但那条路一旦存在，下一个人往 `format!` 里塞列名/条件是零成本的。
//! 这里的 `Cow<'static, str>` 起点是 `&'static str`：**编译期就没有「把 LLM 产物变成 SQL」的入口**
//! （不变量 I2 / `OwnedStore` 红线）。
//!
//! 构造入口不在本文件：`FixedStmt` 只能由 `ReadOnlyMySql::fixed()` 产出、`PgStmt` 只能由
//! `OwnedStore::fixed()` 产出（T4-A2），故两个 `new` 是 `pub(crate)` —— 别人拿不到池，也就造不出语句。
//!
//! MySQL 与 PG 两份实现刻意不抽泛型：sqlx 的 `Database` 泛型要拖一串 `for<'r> FromRow` /
//! `Executor` 边界，读起来比两份具体实现难得多，而这里只有两个数据库、永远也就两个。

use std::borrow::Cow;
use std::time::{Duration, Instant};

// `Arguments` trait 必须 in scope：`MySqlArguments::add` 的公开形态是 trait 方法
use sqlx::Arguments;

use crate::error::ConnectorError;

/// 模板里的动态 `IN` 标记。**唯一**的动态占位符形态。
const MARK: &str = "{in}";
const MYSQL_PH: &str = "?";
const PG_PH: &str = "$";
// 身份/角色/权限静态查询的超时红线：2s 是局域网时代的值。DMS 身份库如今走公网（部署现实），
// 2s 会把登录态与身份核验打成间歇性硬失败（实测「超时 [dms-auth] 等待 2.0s 未返回」把
// 判官 CLI 直接打死）。8s 只抬天花板不改正常路径耗时；生产业务点查的 2s·50 行红线
// （DMS_LOOKUP_TIMEOUT / fetch_dms_lookup）不动。
const MYSQL_FIXED_TIMEOUT: Duration = Duration::from_secs(8);
const MYSQL_FIXED_SLOW: Duration = Duration::from_millis(500);
/// 自有 PG 静态语句与 MySQL 侧同档（8s 超时 + 500ms 慢日志）：本地 PG 挂死时
/// handler 的 await 不能没有上限。
const PG_FIXED_TIMEOUT: Duration = Duration::from_secs(8);
const PG_FIXED_SLOW: Duration = Duration::from_millis(500);
/// `expand(n)` 的上界：n 极大时拼出超长 SQL 顶爆 max_allowed_packet，错误在远端才暴露
const MAX_EXPAND: usize = 10_000;

/// 慢查询 warn：静态 SQL 本身无数据（值全在 bind 里），可安全记前 80 字节指纹
fn warn_if_slow(at: &str, sql: &str, started: Instant, slow: Duration) {
    let elapsed = started.elapsed();
    if elapsed >= slow {
        tracing::warn!(
            source = at,
            sql = &sql[..sql.len().min(80)],
            elapsed_ms = elapsed.as_millis(),
            "静态语句偏慢"
        );
    }
}

/// `{in}` → n 个占位符（逗号分隔）。纯函数，本文件的可单测核心。
///
/// - `ph == "?"`：MySQL，n 个 `?`。
/// - `ph == "$"`：PG，**编号必须与模板里已有的 `$k` 接续**（多个 `{in}` 连续编号），
///   否则 bind 顺序与占位符编号错位 —— 这是本文件最容易错的一处。
///   前提：模板里的固定 `$k` 都写在 `{in}` 之前（按最大值接续，位置颠倒会撞号）。
pub(crate) fn render_in(tpl: &str, n: usize, ph: &str) -> String {
    if !tpl.contains(MARK) {
        return tpl.to_string(); // 无标记：原样返回（expand 侧已记 config 错，这里仍保持纯函数全定义）
    }
    // doc 约定（本函数上方）：固定 `$k` 都写在 `{in}` 之前，违反会撞号 —— debug 构建当场炸
    if let Some(d) = tpl.rfind('$') {
        debug_assert!(d < tpl.find(MARK).expect("已短路"), "固定 $k 必须写在 {{in}} 之前: {tpl}");
    }
    let numbered = ph == PG_PH;
    let mut next = if numbered { max_dollar(tpl) + 1 } else { 0 };
    let mut out = String::with_capacity(tpl.len() + n * 4);
    let mut rest = tpl;
    while let Some(i) = rest.find(MARK) {
        out.push_str(&rest[..i]);
        for k in 0..n {
            if k > 0 {
                out.push(',');
            }
            if numbered {
                use std::fmt::Write as _;
                write!(out, "${next}").expect("写 String 不会失败");
                next += 1;
            } else {
                out.push_str(ph);
            }
        }
        rest = &rest[i + MARK.len()..];
    }
    out.push_str(rest);
    out
}

/// 模板里已有的最大 `$k`（没有则 0）。约束：模板**字符串字面量**里的 `$k`（如 `'US$5'`）
/// 也会被计入最大值 —— 静态模板由评审保证不在字面量里玩 `$`。
fn max_dollar(tpl: &str) -> usize {
    tpl.match_indices('$')
        .filter_map(|(i, _)| {
            let digits: String =
                tpl[i + 1..].chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<usize>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// sqlx 错误 → `ConnectorError`。分类依据是**调用方要做的决定**：
/// 只有「数据库明确判定语句有问题」才是 `Query`（可拿去 repair），其余都不该触发改写。
/// 与 mysql.rs 的 `sqlx_err` **变体同口径、文案不同**：这里保留 sqlx 原始文案，
/// 那边把 access-denied/decode 归一为固定话术（点查面向用户，文案要稳）。
fn classify(at: &str, e: sqlx::Error) -> ConnectorError {
    let msg = e.to_string();
    match e {
        sqlx::Error::Database(_) => ConnectorError::query(at, msg),
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) | sqlx::Error::ColumnNotFound(_) => {
            ConnectorError::decode(at, msg)
        }
        _ => ConnectorError::connect(at, msg),
    }
}

/// MySQL 侧字面量语句。`at` 是错误文案里的源标识（`DsId`）。
///
/// 该通道承载登录、角色和权限加载等服务端静态 SQL，刻意不套 `DmsLookupSql` 的单表限制；
/// 新增面向用户的生产业务点查必须走 `ReadOnlyMySql::fetch_dms_lookup`，不能借用这里绕过。
/// pool 是**持有克隆**（`MySqlPool` 内部是 Arc）：`ReadOnlyMySql` 热切换业务库后，
/// 这条语句仍在旧池上自然收尾（克隆即「在飞查询的保鲜期」），而新语句拿新池。
pub struct FixedStmt<'a> {
    pool: sqlx::MySqlPool,
    at: &'a str,
    sql: Cow<'static, str>,
    args: sqlx::mysql::MySqlArguments,
    /// 首个失败（bind 编码失败 / `expand(0)`），到 fetch 时才返回 —— 建造器链上没地方 `?`
    err: Option<ConnectorError>,
}

impl<'a> FixedStmt<'a> {
    pub(crate) fn new_owned(
        pool: sqlx::MySqlPool,
        at: &'a str,
        tpl: &'static str,
        identity_permission: bool,
    ) -> Self {
        let err = (!identity_permission).then(|| {
            ConnectorError::config(
                at,
                "MySQL fixed 通道仅允许 DMS 身份、角色与权限静态查询",
            )
        });
        Self { pool, at, sql: Cow::Borrowed(tpl), args: Default::default(), err }
    }

    /// 把模板里**每个** `{in}` 展开成 n 个 `?`（多个 `{in}` 用同一个 n —— `scope.rs` 的双 IN 场景）。
    /// `n == 0` 记错：空 `IN ()` 是语法错，调用方本该先判空集短路。
    /// 模板没有 `{in}` 或 n 超上限同样记错：那是调用方误用，不该到数据库才报 bind 数不匹配。
    pub fn expand(mut self, n: usize) -> Self {
        if n == 0 {
            self.err.get_or_insert_with(|| {
                ConnectorError::config(self.at, "expand(0)：空 IN 列表，调用方应先判空集短路")
            });
            return self;
        }
        if n > MAX_EXPAND {
            self.err.get_or_insert_with(|| {
                ConnectorError::config(
                    self.at,
                    format!("expand({n}) 超上限 {MAX_EXPAND}：调用方应先分批"),
                )
            });
            return self;
        }
        if !self.sql.contains(MARK) {
            self.err.get_or_insert_with(|| {
                ConnectorError::config(self.at, "模板没有 {in} 标记，expand 是调用方误用")
            });
            return self;
        }
        self.sql = Cow::Owned(render_in(&self.sql, n, MYSQL_PH));
        self
    }

    /// bind 顺序 = 展开后的占位符顺序（裁决 C1）。编码失败暂存首错，不 panic。
    pub fn bind<'v, T>(mut self, v: T) -> Self
    where
        T: 'v + sqlx::Encode<'v, sqlx::MySql> + sqlx::Type<sqlx::MySql>,
    {
        if self.err.is_some() {
            return self; // 已置错：结果必然丢弃，不再白编码后续参数
        }
        if let Err(e) = self.args.add(v) {
            let at = self.at;
            self.err.get_or_insert_with(|| ConnectorError::decode(at, e));
        }
        self
    }

    fn into_parts(
        self,
    ) -> Result<(sqlx::MySqlPool, &'a str, Cow<'static, str>, sqlx::mysql::MySqlArguments), ConnectorError>
    {
        let Self { pool, at, sql, args, err } = self;
        match err {
            Some(e) => Err(e),
            None => Ok((pool, at, sql, args)),
        }
    }

    pub async fn fetch_all<T>(self) -> Result<Vec<T>, ConnectorError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::mysql::MySqlRow> + Send + Unpin,
    {
        let (pool, at, sql, args) = self.into_parts()?;
        let started = Instant::now();
        let out = tokio::time::timeout(
            MYSQL_FIXED_TIMEOUT,
            sqlx::query_as_with::<sqlx::MySql, T, _>(&sql, args).fetch_all(&pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(at, MYSQL_FIXED_TIMEOUT))?
        .map_err(|e| classify(at, e));
        warn_if_slow(at, &sql, started, MYSQL_FIXED_SLOW);
        out
    }

    pub async fn fetch_optional<T>(self) -> Result<Option<T>, ConnectorError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::mysql::MySqlRow> + Send + Unpin,
    {
        let (pool, at, sql, args) = self.into_parts()?;
        let started = Instant::now();
        let out = tokio::time::timeout(
            MYSQL_FIXED_TIMEOUT,
            sqlx::query_as_with::<sqlx::MySql, T, _>(&sql, args).fetch_optional(&pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(at, MYSQL_FIXED_TIMEOUT))?
        .map_err(|e| classify(at, e));
        warn_if_slow(at, &sql, started, MYSQL_FIXED_SLOW);
        out
    }
}

/// 自有 PG 侧字面量语句。与 `FixedStmt` 同形，另有 `execute()`：自有库要写
/// （`OwnedStore` 的唯一可写通道就是它 + `create_upload_table`）。
pub struct PgStmt<'a> {
    pool: &'a sqlx::PgPool,
    at: &'a str,
    sql: Cow<'static, str>,
    args: sqlx::postgres::PgArguments,
    err: Option<ConnectorError>,
}

impl<'a> PgStmt<'a> {
    pub(crate) fn new(pool: &'a sqlx::PgPool, at: &'a str, tpl: &'static str) -> Self {
        Self { pool, at, sql: Cow::Borrowed(tpl), args: Default::default(), err: None }
    }

    /// 同 `FixedStmt::expand`，但渲染成 `$k` 且**编号接续模板里已有的参数**。
    pub fn expand(mut self, n: usize) -> Self {
        if n == 0 {
            self.err.get_or_insert_with(|| {
                ConnectorError::config(self.at, "expand(0)：空 IN 列表，调用方应先判空集短路")
            });
            return self;
        }
        if n > MAX_EXPAND {
            self.err.get_or_insert_with(|| {
                ConnectorError::config(
                    self.at,
                    format!("expand({n}) 超上限 {MAX_EXPAND}：调用方应先分批"),
                )
            });
            return self;
        }
        if !self.sql.contains(MARK) {
            self.err.get_or_insert_with(|| {
                ConnectorError::config(self.at, "模板没有 {in} 标记，expand 是调用方误用")
            });
            return self;
        }
        self.sql = Cow::Owned(render_in(&self.sql, n, PG_PH));
        self
    }

    pub fn bind<'v, T>(mut self, v: T) -> Self
    where
        T: 'v + sqlx::Encode<'v, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    {
        if self.err.is_some() {
            return self; // 已置错：结果必然丢弃，不再白编码后续参数
        }
        if let Err(e) = self.args.add(v) {
            let at = self.at;
            self.err.get_or_insert_with(|| ConnectorError::decode(at, e));
        }
        self
    }

    fn into_parts(
        self,
    ) -> Result<(&'a sqlx::PgPool, &'a str, Cow<'static, str>, sqlx::postgres::PgArguments), ConnectorError>
    {
        let Self { pool, at, sql, args, err } = self;
        match err {
            Some(e) => Err(e),
            None => Ok((pool, at, sql, args)),
        }
    }

    pub async fn fetch_all<T>(self) -> Result<Vec<T>, ConnectorError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Send + Unpin,
    {
        let (pool, at, sql, args) = self.into_parts()?;
        let started = Instant::now();
        let out = tokio::time::timeout(
            PG_FIXED_TIMEOUT,
            sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, args).fetch_all(pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(at, PG_FIXED_TIMEOUT))?
        .map_err(|e| classify(at, e));
        warn_if_slow(at, &sql, started, PG_FIXED_SLOW);
        out
    }

    pub async fn fetch_optional<T>(self) -> Result<Option<T>, ConnectorError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Send + Unpin,
    {
        let (pool, at, sql, args) = self.into_parts()?;
        let started = Instant::now();
        let out = tokio::time::timeout(
            PG_FIXED_TIMEOUT,
            sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, args).fetch_optional(pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(at, PG_FIXED_TIMEOUT))?
        .map_err(|e| classify(at, e));
        warn_if_slow(at, &sql, started, PG_FIXED_SLOW);
        out
    }

    /// 写入（INSERT/UPDATE/DELETE），返回受影响行数 —— F8 的「越权删返回假成功」要靠它判 403。
    pub async fn execute(self) -> Result<u64, ConnectorError> {
        let (pool, at, sql, args) = self.into_parts()?;
        let started = Instant::now();
        let out = tokio::time::timeout(
            PG_FIXED_TIMEOUT,
            sqlx::query_with::<sqlx::Postgres, _>(&sql, args).execute(pool),
        )
        .await
        .map_err(|_| ConnectorError::timeout(at, PG_FIXED_TIMEOUT))?
        .map(|r| r.rows_affected())
        .map_err(|e| classify(at, e));
        warn_if_slow(at, &sql, started, PG_FIXED_SLOW);
        out
    }
}

/// 全部无库无网：`connect_lazy` 只解析 DSN 不建连接，`expand` 的守卫在发请求之前就短路。
#[cfg(test)]
mod tests {
    use super::*;

    /// `scope.rs` 的双 IN 形态（表名泛化：DMS 语料归 policy/semantic）
    const DOUBLE_IN: &str = "SELECT DISTINCT t.id FROM staff t
         WHERE t.dept_id IN ({in}) OR td.dept_id IN ({in})";

    fn lazy_mysql() -> sqlx::MySqlPool {
        sqlx::MySqlPool::connect_lazy("mysql://u:p@127.0.0.1:1/db").unwrap()
    }

    fn lazy_pg() -> sqlx::PgPool {
        sqlx::PgPool::connect_lazy("postgres://u:p@127.0.0.1:1/db").unwrap()
    }

    #[test]
    fn mysql_expands_every_mark_with_same_n() {
        let out = render_in(DOUBLE_IN, 3, MYSQL_PH);
        assert_eq!(out.matches("?,?,?").count(), 2, "{out}");
        assert!(!out.contains(MARK), "{out}");
        assert_eq!(render_in("a IN ({in})", 1, MYSQL_PH), "a IN (?)");
        assert_eq!(render_in("SELECT 1", 5, MYSQL_PH), "SELECT 1");
    }

    /// 本文件最容易错的一处：PG 编号必须接着模板里已有的固定参数往下排。
    #[test]
    fn pg_numbering_continues_after_fixed_params() {
        assert_eq!(
            render_in("SELECT a FROM t WHERE ds_id = $1 AND x IN ({in}) AND y IN ({in})", 2, PG_PH),
            "SELECT a FROM t WHERE ds_id = $1 AND x IN ($2,$3) AND y IN ($4,$5)"
        );
        // 无固定参数时从 $1 起
        assert_eq!(render_in("x IN ({in})", 2, PG_PH), "x IN ($1,$2)");
        // 两位数编号（`$1` 不能被当成最大值）
        assert_eq!(max_dollar("a=$9 b=$10 c=$2"), 10);
        assert_eq!(render_in("a=$10 AND x IN ({in})", 1, PG_PH), "a=$10 AND x IN ($11)");
        assert_eq!(max_dollar("no params"), 0);
    }

    /// 身份通道超时 ≥ 生产点查红线（公网身份库现实）；再收紧到 2s 就是登录态间歇性自杀。
    #[test]
    fn fixed_timeout_never_tighter_than_lookup_redline() {
        assert!(MYSQL_FIXED_TIMEOUT >= crate::mysql::DMS_LOOKUP_TIMEOUT);
        assert_eq!(MYSQL_FIXED_TIMEOUT, Duration::from_secs(8));
    }

    /// `#[tokio::test]`：`connect_lazy` 自己要 spawn 池的维护任务（不建连接，但要 runtime）
    #[tokio::test]
    async fn expand_swaps_template_in_place() {
        let pool = lazy_mysql();
        let s = FixedStmt::new_owned(pool.clone(), "dms", "x IN ({in})", true).expand(2);
        assert_eq!(s.sql, "x IN (?,?)");
        assert!(s.err.is_none());
    }

    /// `expand(0)` 必须在发请求**之前**短路（否则拼出 `IN ()` 让生产库报语法错）
    #[tokio::test]
    async fn expand_zero_fails_before_touching_db() {
        let pool = lazy_mysql();
        let err = FixedStmt::new_owned(pool.clone(), "dms", "x IN ({in})", true)
            .expand(0)
            .bind(1i64)
            .fetch_all::<(i64,)>()
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ConnectorError::Config(_)), "{err}");
        assert!(err.to_string().contains("expand(0)"), "{err}");
    }

    #[tokio::test]
    async fn mysql_fixed_rejects_non_identity_sources_before_touching_db() {
        let pool = lazy_mysql();
        let err = FixedStmt::new_owned(pool, "analysis", "SELECT 1", false)
            .fetch_all::<(i64,)>()
            .await
            .unwrap_err();
        assert!(matches!(err, ConnectorError::Config(_)), "{err}");
        assert!(err.to_string().contains("身份、角色与权限"), "{err}");
    }

    /// bind 接受借用值与拥有值两类（`scope.rs` 传 `&i64` / `&String`，也有字面量）；
    /// 固定参数写在 `{in}` 之前，bind 顺序 = 占位符编号顺序。
    #[tokio::test]
    async fn bind_accepts_borrowed_and_owned() {
        let pool = lazy_pg();
        let ids = vec![7i64, 8];
        let name = String::from("tanlibo");
        let mut s = PgStmt::new(&pool, "owned-pg", "b = $1 AND a IN ({in})").expand(2);
        assert_eq!(s.sql, "b = $1 AND a IN ($2,$3)");
        s = s.bind(name.as_str());
        for id in &ids {
            s = s.bind(id);
        }
        assert!(s.err.is_none(), "bind 不该报错");
    }

    /// 无 `{in}` 标记 / n 超上限：都是调用方误用，必须在发请求前记 config 错
    /// （否则到数据库才报 bind 数不匹配 / 顶爆 max_allowed_packet，错误归类全错）。
    #[tokio::test]
    async fn expand_misuse_is_config_error_before_touching_db() {
        let pool = lazy_mysql();
        let err = FixedStmt::new_owned(pool.clone(), "dms", "SELECT 1", true)
            .expand(2)
            .fetch_all::<(i64,)>()
            .await
            .unwrap_err();
        assert!(matches!(err, ConnectorError::Config(_)), "{err}");
        assert!(err.to_string().contains("{in}"), "{err}");
        let err = FixedStmt::new_owned(pool.clone(), "dms", "x IN ({in})", true)
            .expand(20_000)
            .fetch_all::<(i64,)>()
            .await
            .unwrap_err();
        assert!(matches!(err, ConnectorError::Config(_)), "{err}");
        assert!(err.to_string().contains("超上限"), "{err}");
    }
}
