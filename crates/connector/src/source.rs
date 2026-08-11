//! 只读取数源的契约：`SqlSource` + 它进出的三组纯数据（`RowSet` / `SchemaSnapshot` / `SourceKind`）。
//! 实现在 `mysql.rs`（生产 MySQL）与 `postgres.rs`（自有 PG 的只读角色）—— 两个实现，故 trait 成立（D7）。
//!
//! 三条契约要点，改签名前先读：
//! 1. **`fetch` 只收 `&ScopedSql`**（不变量 I1）：想执行一个 `String`，编译不过。
//! 2. **`explain` 返 `Option`**（ARCHITECTURE §5）：`Some` = 数据库明确判定 SQL 有问题（可拿去 repair）；
//!    超时/连接抖动 = `None`，**不触发改写** —— 抖动触发的改写可能把本来对的 SQL 改坏，还多花一次 LLM。
//! 3. **`RowSet.redacted`**（F5）：敏感列在组装 `RowSet` 时整列置空，这是 `SELECT *` 的唯一收口
//!    （SQL 文本层的词表挡不住 `SELECT *`，而单号直查恒是 `SELECT *`）。
//!
//! 异步 trait 手写 `dms_kernel::BoxFut`，不引 `async-trait`（D6）。

use std::time::Duration;

// `ScopedSql` 只读不造：本 crate 拿不到它的构造器（唯二产出点都在 kernel）
use dms_kernel::{BoxFut, Dialect, DsId, ScopedSql};

use crate::error::ConnectorError;

/// 源的方言族。`Dialect` 管「怎么 parse / 怎么采 schema」，这个枚举管「按源分派的那几处 match」
/// （类型映射、启动自检）；实现 ≤3 个前不引注册中心（ARCHITECTURE §10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Mysql,
    Postgres,
}

/// 日志用小写源名（`{:?}` 的 PascalCase 不进日志）
impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mysql => f.write_str("mysql"),
            Self::Postgres => f.write_str("postgres"),
        }
    }
}

/// 一次取数的结果。列名与行分开存（前端契约是 `columns` + `rows` 两个数组，不是对象数组）。
#[derive(Default, Clone)]
pub struct RowSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    /// 被敏感列防线整列置空的列名（F5：`SELECT *` 的唯一收口）。
    /// 空 = 没有命中，非空 = 这些列的值全是 `Null`，调用方据此提示用户而不是当成没数据。
    pub redacted: Vec<String>,
}

// 手写 Debug：derive 会把全部行数据（业务值）打进任何 `{:?}` 日志
impl std::fmt::Debug for RowSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowSet")
            .field("columns", &self.columns)
            .field("rows", &format_args!("{} 行（业务值不进 Debug）", self.rows.len()))
            .field("redacted", &self.redacted)
            .finish()
    }
}

/// 一列的元信息。`ordinal` 是 `i64` 而非 `usize`：探针直接给的是数据库的序号列，
/// 转换要么在这里要么在 ETL 侧，放这里等于每个实现都写一遍 `as`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub comment: String,
    pub ordinal: i64,
}

/// 一张表的元信息。`row_estimate` 是**估算值**（MySQL `TABLE_ROWS` / PG `reltuples`），
/// 用于召回排序，不做业务口径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    pub comment: String,
    pub row_estimate: i64,
}

/// 一次 schema 采集的全量快照。`columns` 的 `String` 是**表名**（一张表多行），
/// 不做 `HashMap<String, Vec<ColumnInfo>>`：ETL 侧是逐行 upsert，分组只会先合再拆。
/// 已知浪费：每列重复克隆一份表名（千列大库 = 千份重复分配）—— 量级可忍，暂不动。
#[derive(Debug, Default, Clone)]
pub struct SchemaSnapshot {
    pub tables: Vec<TableInfo>,
    pub columns: Vec<(String, ColumnInfo)>,
}

/// 【A8】数据源级查询策略：`fetch` 入口处与调用方传入值取 min —— **只许更紧，从不放宽**。
/// 两个字段都 `None`（默认）= 不收紧：与全局两档取 min 恒等，存量行为逐字节不变。
/// 配置面在 semantic 注册表的数据源策略配置（JSON 字段 `max_rows` / `timeout_ms`），
/// 这里只留执行形态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DsPolicy {
    /// 单次取数行上限（超出截断）。`Some(0)` 是合法的最紧档（恒空结果）。
    pub max_rows: Option<usize>,
    /// 单次查询超时。
    pub timeout: Option<Duration>,
}

impl DsPolicy {
    /// min 语义的唯一收口：任一侧 `None` = 该维度不收紧；两侧都有值取小者。
    #[must_use = "clamp 返回新值，丢掉返回值 = 策略静默失效"]
    pub fn clamp(self, max: usize, t: Duration) -> (usize, Duration) {
        (
            self.max_rows.map_or(max, |cap| max.min(cap)),
            self.timeout.map_or(t, |cap| t.min(cap)),
        )
    }
}

/// 只读取数源。`&'a self` + `BoxFut<'a, _>`：实现侧持私有连接池，调用侧可放 `&dyn SqlSource`
/// （`AskCtx.source` 就是这个形状：按 `ds_id` 从注册表取源，而不是把具名 MySQL 硬写进各
/// 结构体 —— 具名字段一旦换源/改名就与 `ds_id` 断链，是历史上的头号断链成因）。
pub trait SqlSource: Send + Sync {
    fn ds_id(&self) -> &DsId;

    fn kind(&self) -> SourceKind;

    /// 当前连接是否指向 Doris 数仓。默认 false；热切 MySQL 池时由连接器同步更新。
    /// （PostgresSource 不 override 本方法 —— PG 没有数仓形态，走的就是这个默认 false。）
    fn is_warehouse(&self) -> bool {
        false
    }

    /// 本源的方言：`check()` 要用它 parse，`CheckedSql` 会带着它走到 `inject()`。
    fn dialect(&self) -> &'static dyn Dialect;

    /// 【A8】登记数据源级查询策略：其后每次 `fetch` 在入口与调用方值取 min（只许更紧）。
    /// 实现检查清单：必须真的存下它（内部可变性）—— 空操作实现等于把管理端配的收紧
    /// 静默丢地上（本文件 tests 里有 A8 断言，新实现照着补一条 set→生效断言）。
    fn set_ds_policy(&self, policy: DsPolicy);

    /// 取数。`max` = 行上限（超出即截断，不报错 —— 截断发生在实现侧内存：数仓路径
    /// 拉全量后内存截断，无 DB 端 LIMIT 注入，大结果集的内存峰值要有数）；`t` = 单次查询超时。
    fn fetch<'a>(
        &'a self,
        sql: &'a ScopedSql,
        max: usize,
        t: Duration,
    ) -> BoxFut<'a, Result<RowSet, ConnectorError>>;

    /// 预翻译验证（`EXPLAIN <SQL>`，只解析优化不取数）。`Ok(Some)` / `Ok(None)` 的
    /// 抖动语义见文件头第 2 条（单一表述在那里，这里不重复抄）。
    fn explain<'a>(
        &'a self,
        sql: &'a ScopedSql,
        t: Duration,
    ) -> BoxFut<'a, Result<Option<String>, ConnectorError>>;

    /// schema 采集的**唯一**入口（ARCHITECTURE §5）：两条探针由 `Dialect` 提供，
    /// 不给 semantic 开「只要表 / 只要列 / 只要某表」三个专用方法。
    fn probe_schema<'a>(&'a self) -> BoxFut<'a, Result<SchemaSnapshot, ConnectorError>>;
}

/// 契约自守：trait 必须是 object-safe（`&dyn SqlSource` 是 agent 侧的持有形态），
/// 且 `redacted` 能原样穿过。无库无网 —— 假实现直接返回内存里的 `RowSet`。
#[cfg(test)]
mod tests {
    use super::*;
    use dms_kernel::policy::scope::ScopeSets;
    use dms_kernel::sql::guard::GuardConfig;
    use dms_kernel::{check, MysqlDialect, RawSql, UnrestrictedProof};

    struct Fake(DsId, std::sync::Mutex<DsPolicy>);

    impl SqlSource for Fake {
        fn ds_id(&self) -> &DsId {
            &self.0
        }
        fn kind(&self) -> SourceKind {
            SourceKind::Mysql
        }
        fn dialect(&self) -> &'static dyn Dialect {
            &MysqlDialect
        }
        fn set_ds_policy(&self, policy: DsPolicy) {
            // 与生产实现同口径：锁中毒用 into_inner 恢复（mysql.rs/postgres.rs 同款）
            *self.1.lock().unwrap_or_else(|e| e.into_inner()) = policy;
        }
        fn fetch<'a>(
            &'a self,
            _sql: &'a ScopedSql,
            _max: usize,
            _t: Duration,
        ) -> BoxFut<'a, Result<RowSet, ConnectorError>> {
            Box::pin(async move {
                // 契约自守：Fake 也按「超出即截断不报错」截断，max=0 恒空
                let rows = vec![
                    vec![1.into(), serde_json::Value::Null],
                    vec![2.into(), serde_json::Value::Null],
                ];
                Ok(RowSet {
                    columns: vec!["id".into(), "login_pwd".into()],
                    rows: rows.into_iter().take(_max).collect(),
                    redacted: vec!["login_pwd".into()],
                })
            })
        }
        fn explain<'a>(
            &'a self,
            _sql: &'a ScopedSql,
            _t: Duration,
        ) -> BoxFut<'a, Result<Option<String>, ConnectorError>> {
            Box::pin(async move { Ok(None) })
        }
        fn probe_schema<'a>(&'a self) -> BoxFut<'a, Result<SchemaSnapshot, ConnectorError>> {
            Box::pin(async move { Ok(SchemaSnapshot::default()) })
        }
    }

    /// 只能这么造 `ScopedSql`：`check()` → `unrestricted(_, &proof)`（唯二产出点之一）
    fn scoped(sql: &str) -> ScopedSql {
        // 200 与 agent 侧 MAX_ROWS 同档（跨 crate 引用仅作注释，无编译期联动）
        const G: GuardConfig = GuardConfig::new(200, &[]);
        let c = check(RawSql::new(sql), &MysqlDialect, &G).unwrap();
        let proof = UnrestrictedProof::new(&ScopeSets::default(), true).unwrap();
        ScopedSql::unrestricted(c, &proof)
    }

    #[tokio::test]
    async fn trait_is_object_safe_and_carries_redacted() {
        let f = Fake(DsId::new("dms"), std::sync::Mutex::new(DsPolicy::default()));
        let s: &dyn SqlSource = &f; // object-safe：这一行是本测试的主张
        assert_eq!(s.ds_id().as_str(), "dms");
        assert_eq!(s.kind(), SourceKind::Mysql);
        assert_eq!(s.dialect().name(), "MySQL");

        let sql = scoped("SELECT id FROM orders");
        let rs = s.fetch(&sql, 200, Duration::from_secs(1)).await.unwrap();
        assert_eq!(rs.columns, ["id", "login_pwd"]);
        assert_eq!(rs.redacted, ["login_pwd"]);
        assert!(rs.rows[0][1].is_null());

        // 「超出即截断不报错」契约：Fake 也必须截断（max=1 只返 1 行，max=0 恒空）
        let rs = s.fetch(&sql, 1, Duration::from_secs(1)).await.unwrap();
        assert_eq!(rs.rows.len(), 1, "max=1 只许返 1 行");
        let rs = s.fetch(&sql, 0, Duration::from_secs(1)).await.unwrap();
        assert!(rs.rows.is_empty(), "max=0 是合法最紧档：恒空结果");

        // 抖动语义：None 表示「别改写」，不是「SQL 没问题」
        assert!(s.explain(&sql, Duration::from_secs(1)).await.unwrap().is_none());
        assert!(s.probe_schema().await.unwrap().tables.is_empty());

        // A8：策略登记必须真的落进实现 —— 空操作实现 = 管理端配的收紧被静默丢地上
        let p = DsPolicy { max_rows: Some(20), timeout: Some(Duration::from_millis(800)) };
        s.set_ds_policy(p);
        assert_eq!(*f.1.lock().unwrap_or_else(|e| e.into_inner()), p);
    }

    /// 🔴 A8：min 语义 —— 只许更紧；默认（全 `None`）与调用方值恒等（存量行为不变）
    #[test]
    fn ds_policy_clamp_only_tightens() {
        let (max, t) = (200usize, Duration::from_secs(30)); // 全局两档（agent 侧行上限/超时）
        assert_eq!(DsPolicy::default().clamp(max, t), (max, t), "默认策略必须是恒等");
        // 各维度独立收紧
        assert_eq!(DsPolicy { max_rows: Some(20), timeout: None }.clamp(max, t), (20, t));
        assert_eq!(
            DsPolicy { max_rows: None, timeout: Some(Duration::from_millis(800)) }.clamp(max, t),
            (max, Duration::from_millis(800))
        );
        // 配得比全局更松：不放宽任何东西
        assert_eq!(
            DsPolicy { max_rows: Some(5000), timeout: Some(Duration::from_secs(120)) }.clamp(max, t),
            (max, t)
        );
        // 调用方更紧时，ds 配置同样放宽不了
        assert_eq!(
            DsPolicy { max_rows: Some(200), timeout: Some(Duration::from_secs(30)) }
                .clamp(50, Duration::from_secs(2)),
            (50, Duration::from_secs(2))
        );
        // 0 是合法的最紧档（恒空结果 / 立即超时）
        assert_eq!(
            DsPolicy { max_rows: Some(0), timeout: Some(Duration::ZERO) }.clamp(max, t),
            (0, Duration::ZERO)
        );
    }

    /// `{:?}` 不许把业务值打进日志：只出列名/行数/redacted
    #[test]
    fn rowset_debug_never_prints_business_values() {
        let rs = RowSet {
            columns: vec!["secret_col".into()],
            rows: vec![vec![serde_json::Value::from("业务值不该出现")]],
            redacted: vec![],
        };
        let dbg = format!("{rs:?}");
        assert!(dbg.contains("secret_col"), "{dbg}");
        assert!(dbg.contains("1 行"), "{dbg}");
        assert!(!dbg.contains("业务值不该出现"), "{dbg}");
    }
}
