//! 多源连接注册中心：`DsSpec` → 已建池的 `Arc<dyn SqlSource>`（懒建 + 复用）。
//!
//! 两条纪律：
//! 1. **明文 DSN 只在配置里**。`DsSpec` 只带 `dsn_ref`（键名），映射表在 registry 内部私有 ——
//!    这样 `DsSpec` 可以随便进日志/接口/缓存 key，口令不会跟着走。
//! 2. **不做池数上限**（ARCHITECTURE §8 已判删 cap）：真实源数是个位数，
//!    LRU 淘汰只会带来「淘汰了正在用的源」这种难查故障。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::Connection;

use dms_kernel::nl::lexicon::SENSITIVE_COLS;
use dms_kernel::DsId;

use crate::error::ConnectorError;
use crate::mysql::{sqlx_err, ReadOnlyMySql};
use crate::postgres::PostgresSource;
use crate::source::{DsPolicy, SourceKind, SqlSource};

/// 「测试连接」探针预算：黑洞主机不许让按钮永远转圈
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 一个数据源的配置形态。**无口令**：`dsn_ref` 是配置里那张映射表的键名。
#[derive(Debug, Clone)]
pub struct DsSpec {
    pub ds_id: DsId,
    pub kind: SourceKind,
    pub dsn_ref: String,
    pub max_conn: u32,
    /// PG 源的 `search_path`（`None` = 用库默认）。**上传表格源必须给**：
    /// 它们共用一条 `pg_ro_url`、schema 一份一个，`None` 会让 schema 采集空手而归。
    /// 取值一律来自 `dms_knowledge::tabular::upload_schema_of_ds(ds_id)` —— 别手拼。
    pub schema: Option<String>,
}

pub struct SourceRegistry {
    /// dsn_ref → 明文 DSN。私有且无 getter：拿不到它就没人能在别处建第二个池。
    dsns: HashMap<String, String>,
    /// 已建池。`std::sync::Mutex` 而非 tokio 的：锁**从不跨 await**（见 `get`）。
    pools: Mutex<HashMap<DsId, Arc<dyn SqlSource>>>,
    /// 【A8】数据源级策略账本：未建池的源先记账（`get` 建池后带上），已建池的当场收紧。
    policies: Mutex<HashMap<DsId, DsPolicy>>,
}

impl SourceRegistry {
    pub fn new(dsns: HashMap<String, String>) -> Self {
        // 启动零 DSN 多半是配置事故：留痕键数，排障时第一眼要看的就是它
        tracing::debug!(dsn_keys = dsns.len(), "数据源注册表已建");
        Self {
            dsns,
            pools: Mutex::new(HashMap::new()),
            policies: Mutex::new(HashMap::new()),
        }
    }

    /// 预置一个已建好的源（DMS 主源在启动时就建好，注册进来复用 —— 全程只有一个池）。
    pub fn preload(&self, src: Arc<dyn SqlSource>) {
        if let Some(old) = self.lock().insert(src.ds_id().clone(), src) {
            tracing::debug!(ds = %old.ds_id(), "preload 覆盖了同名源的已建池");
        }
    }

    /// 【A8】登记/更新数据源级策略：先记账（`policies`），再对已建池的源当场生效 ——
    /// 这个顺序让「`set_policy` 与首次 `get` 建池并发」最终收敛到账本里那份。
    /// `close` 不摘账：策略属于 ds_id（配置），不属于某一代池。
    pub fn set_policy(&self, ds: &DsId, policy: DsPolicy) {
        self.policies_lock().insert(ds.clone(), policy);
        if let Some(src) = self.lock().get(ds).cloned() {
            src.set_ds_policy(policy);
        }
    }

    /// 懒建 + 复用。先查缓存，miss 才建池；**建池期间不持锁**（否则一个连不上的源会
    /// 把所有源的取数一起堵死）。缓存命中不校验 spec：dsn_ref/max_conn/schema 改了
    /// 必须先 `close` 再 `get` 才重建 —— 复用的是「建池那一刻的配置」。
    ///
    /// ponytail: 并发首访同一个源可能各建一个池，先到者入表、后到者的池随 Arc 落地即释放。
    /// 代价是偶发一次多余握手；换成「按 ds 排队」要引一张 in-flight 表，不值。
    pub async fn get(&self, spec: &DsSpec) -> Result<Arc<dyn SqlSource>, ConnectorError> {
        if let Some(hit) = self.lock().get(&spec.ds_id).cloned() {
            return Ok(hit);
        }
        let built = self.build(spec).await?;
        let src = self.lock().entry(spec.ds_id.clone()).or_insert(built).clone();
        // 【A8】建池前登记过的策略在这里带上（与 `set_policy` 的并发收敛序见其注释）
        if let Some(p) = self.policies_lock().get(&spec.ds_id).copied() {
            src.set_ds_policy(p);
        }
        Ok(src)
    }

    /// 连通性测试（新增数据源时的「测试连接」按钮）：一条**独立短连接**问版本号，
    /// 不进池 —— 配错的 DSN 不该在注册表里留下一个坏池。
    /// 整段包超时：黑洞主机不能让按钮永远转圈。
    pub async fn probe(&self, spec: &DsSpec) -> Result<String, ConnectorError> {
        tokio::time::timeout(PROBE_TIMEOUT, self.probe_inner(spec))
            .await
            .map_err(|_| ConnectorError::timeout(spec.ds_id.as_str(), PROBE_TIMEOUT))?
    }

    async fn probe_inner(&self, spec: &DsSpec) -> Result<String, ConnectorError> {
        let at = spec.ds_id.as_str();
        let dsn = self.dsn(spec)?;
        match spec.kind {
            SourceKind::Mysql => {
                let mut c = sqlx::MySqlConnection::connect(dsn)
                    .await
                    .map_err(|e| ConnectorError::connect(at, e))?;
                let v: String = sqlx::query_scalar("SELECT VERSION()")
                    .fetch_one(&mut c)
                    .await
                    .map_err(|e| sqlx_err(at, e))?;
                let _ = c.close().await;
                Ok(v)
            }
            SourceKind::Postgres => {
                let mut c = sqlx::PgConnection::connect(dsn)
                    .await
                    .map_err(|e| ConnectorError::connect(at, e))?;
                let v: String = sqlx::query_scalar("SELECT version()")
                    .fetch_one(&mut c)
                    .await
                    .map_err(|e| sqlx_err(at, e))?;
                // 配了 schema 就校验其存在性：schema 配错也能「测通」，之后 schema 采集才空手
                if let Some(s) = spec.schema.as_deref() {
                    let exists: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)",
                    )
                    .bind(s)
                    .fetch_one(&mut c)
                    .await
                    .map_err(|e| sqlx_err(at, e))?;
                    if !exists {
                        let _ = c.close().await;
                        return Err(ConnectorError::config(at, format!("schema 不存在: {s}")));
                    }
                }
                let _ = c.close().await;
                Ok(v)
            }
        }
    }

    /// 摘掉一个源（DSN 改了/源下线）。池随最后一个 `Arc` 落地关闭 ——
    /// 正在跑的查询握着 `Arc`，不会被拔线，这正是要的语义。
    pub async fn close(&self, ds: &DsId) {
        if self.lock().remove(ds).is_some() {
            tracing::info!("数据源已摘除: {ds}");
        }
    }

    async fn build(&self, spec: &DsSpec) -> Result<Arc<dyn SqlSource>, ConnectorError> {
        let dsn = self.dsn(spec)?.to_string();
        tracing::info!(
            "建池 {} -> {} (kind={}, max_conn={})",
            spec.ds_id,
            redact_dsn(&dsn),
            spec.kind,
            spec.max_conn
        );
        let ds = spec.ds_id.clone();
        Ok(match spec.kind {
            SourceKind::Mysql => Arc::new(
                ReadOnlyMySql::connect(
                    ds,
                    &dsn,
                    spec.max_conn,
                    SENSITIVE_COLS,
                    crate::mysql::MysqlCapability::ProductionLookup,
                )
                .await?,
            ) as Arc<dyn SqlSource>,
            SourceKind::Postgres => Arc::new(
                PostgresSource::connect(
                    ds,
                    &dsn,
                    spec.max_conn,
                    SENSITIVE_COLS,
                    spec.schema.as_deref(),
                )
                .await?,
            ),
        })
    }

    fn dsn(&self, spec: &DsSpec) -> Result<&str, ConnectorError> {
        self.dsns.get(&spec.dsn_ref).map(String::as_str).ok_or_else(|| {
            // 只报键名，不报值：这条错误会进日志。带上已配置键数，省一遍翻配置
            ConnectorError::config(
                spec.ds_id.as_str(),
                format!("dsn_ref 未配置: {}（已配置 {} 键）", spec.dsn_ref, self.dsns.len()),
            )
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<DsId, Arc<dyn SqlSource>>> {
        // 持锁段只有 HashMap 操作，中毒本不可能；恢复分支是纯防御（真中毒也不拖垮注册表）
        self.pools.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn policies_lock(&self) -> std::sync::MutexGuard<'_, HashMap<DsId, DsPolicy>> {
        self.policies.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 日志用：口令段换成 `***`。**纯函数**，DSN 一进日志就再也收不回来，故必须单测。
///
/// 覆盖两种漏法：`scheme://user:pass@host/db` 的 userinfo，与 `?password=` / `?pwd=` 查询参数。
/// 覆盖边界：无 `://` 的 `user:pass@host` 裸形态不进 userinfo 遮蔽分支（无 scheme 时
/// userinfo 与路径不可区分）——那种形态只有 `password=`/`pwd=` 键会被 mask。
pub fn redact_dsn(dsn: &str) -> String {
    let Some((scheme, rest)) = dsn.split_once("://") else {
        return mask_params(dsn);
    };
    // 口令里含 `@` 是常见形态（`p@ss`），故按**最后**一个 `@` 切 userinfo/host
    let Some((userinfo, host)) = rest.rsplit_once('@') else {
        return mask_params(dsn);
    };
    match userinfo.split_once(':') {
        // 无口令段就别凭空加一个 `:***`（那会让人以为配了口令）
        None => mask_params(dsn),
        Some((user, _)) => format!("{scheme}://{user}:***@{}", mask_params(host)),
    }
}

/// `password=xxx` / `pwd=xxx` 参数值换成 `***`（PG 的 URI 与 keyword/value DSN 都这么写口令）
fn mask_params(s: &str) -> String {
    // ascii 小写化不改字节长度：全程复用这一份小写找键，下标直接切回原串
    let low = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    while let Some(i) = find_secret_key(&low[at..]).map(|i| at + i) {
        out.push_str(&s[at..i]);
        let eq = i + low[i..].find('=').expect("find_secret_key 保证有 =") + 1;
        out.push_str(&s[i..eq]);
        out.push_str("***");
        // 值到下一个分隔符为止（`&`/`?` 查询参数、` ` keyword-value、`;` ADO 风格）
        at = match low[eq..].find(['&', ' ', ';']) {
            Some(j) => eq + j,
            None => s.len(),
        };
    }
    out.push_str(&s[at..]);
    out
}

/// 下一个 `password` / `pwd` 键的起点（键名必须紧跟 `=`，避免误伤库名里的 `pwd`）。
/// 入参必须是**已小写化**的串（调用方在 `mask_params` 里只 `to_ascii_lowercase` 一次）。
/// 键边界：前一字符必须是起始 / `&` / `?` / 空格 / `;` —— `expwd=x`、路径 `/expwd=1` 不算键。
fn find_secret_key(low: &str) -> Option<usize> {
    // match_indices 而非 find：首个命中被键边界过滤掉时，后面合法的命中不能丢
    ["password=", "pwd=", "passwd="]
        .iter()
        .flat_map(|k| low.match_indices(k).map(|(i, _)| i).collect::<Vec<_>>())
        .filter(|i| *i == 0 || matches!(low.as_bytes()[i - 1], b'&' | b'?' | b' ' | b';'))
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 口令一进日志就收不回来：四种真实形态都必须遮掉
    #[test]
    fn redact_dsn_hides_every_password_form() {
        assert_eq!(
            redact_dsn("mysql://root:s3cret@10.0.0.7:3306/dms"),
            "mysql://root:***@10.0.0.7:3306/dms"
        );
        // 口令含 @：按最后一个 @ 切，host 段不能被吃掉
        assert_eq!(
            redact_dsn("mysql://root:p@ss@10.0.0.7:3306/dms"),
            "mysql://root:***@10.0.0.7:3306/dms"
        );
        // 查询参数形态（PG URI）
        assert_eq!(
            redact_dsn("postgres://dms@pg:5432/app?password=s3cret&sslmode=require"),
            "postgres://dms@pg:5432/app?password=***&sslmode=require"
        );
        // keyword/value DSN（无 scheme）
        assert_eq!(
            redact_dsn("host=pg user=dms password=s3cret dbname=app"),
            "host=pg user=dms password=*** dbname=app"
        );
        // 无口令：原样（别凭空加一个 :***）
        assert_eq!(redact_dsn("postgres://dms@pg:5432/app"), "postgres://dms@pg:5432/app");
        assert_eq!(redact_dsn("not a dsn"), "not a dsn");
        // 库名里含 pwd 不该被当成键（键名必须紧跟 =）
        assert_eq!(redact_dsn("mysql://u:p@h/pwd_db"), "mysql://u:***@h/pwd_db");
        // ADO 风格分号终止符：剩余参数不被吃掉
        assert_eq!(redact_dsn("Password=s3cret;User ID=u"), "Password=***;User ID=u");
        // 键边界：expwd/xpwd 这类前缀不是键，不被误遮
        assert_eq!(redact_dsn("mysql://u:p@h/db?expwd=1"), "mysql://u:***@h/db?expwd=1");
        assert_eq!(redact_dsn("host=h;expwd=1;pwd=s3cret"), "host=h;expwd=1;pwd=***");
    }

    fn spec(dsn_ref: &str) -> DsSpec {
        DsSpec {
            ds_id: DsId::new("ds-7"),
            kind: SourceKind::Mysql,
            dsn_ref: dsn_ref.into(),
            max_conn: 4,
            schema: None,
        }
    }

    /// dsn_ref 认不出 → 在**发起连接之前**就 Config 失败（且文案带源标识与键名）
    #[tokio::test]
    async fn unknown_dsn_ref_fails_before_any_io() {
        let reg = SourceRegistry::new(HashMap::new());
        for e in [
            reg.get(&spec("dms-main")).await.err().unwrap(),
            reg.probe(&spec("dms-main")).await.err().unwrap(),
        ] {
            assert!(matches!(e, ConnectorError::Config(_)), "{e}");
            assert_eq!(
                e.to_string(),
                "配置错误 [ds-7] dsn_ref 未配置: dms-main（已配置 0 键）"
            );
        }
        // 摘一个从没建过的源不 panic
        reg.close(&DsId::new("ds-7")).await;
    }

    /// A8：已建池（含 `preload` 的主源）当场收到策略；未建池的只记账、不报错；
    /// 重复登记者覆盖（管理端改配置的形态）。策略只许更紧的语义在 `DsPolicy::clamp`。
    #[tokio::test]
    async fn set_policy_reaches_live_sources_and_records_lazy_ones() {
        use std::time::Duration;

        use dms_kernel::{BoxFut, Dialect, MysqlDialect, ScopedSql};

        use crate::source::{RowSet, SchemaSnapshot};

        struct Fake(DsId, Mutex<Option<DsPolicy>>);

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
                *self.1.lock().unwrap_or_else(|e| e.into_inner()) = Some(policy);
            }
            fn fetch<'a>(
                &'a self,
                _sql: &'a ScopedSql,
                _max: usize,
                _t: Duration,
            ) -> BoxFut<'a, Result<RowSet, ConnectorError>> {
                Box::pin(async move { Ok(RowSet::default()) })
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

        let reg = SourceRegistry::new(HashMap::new());
        let p = DsPolicy { max_rows: Some(20), timeout: Some(Duration::from_millis(800)) };
        // 未建池：只记账，不 panic（源还没建起来不算错误）
        reg.set_policy(&DsId::new("ds-lazy"), p);
        // 已 preload：当场生效
        let fake = Arc::new(Fake(DsId::new("ds-7"), Mutex::new(None)));
        reg.preload(fake.clone());
        reg.set_policy(&DsId::new("ds-7"), p);
        assert_eq!(*fake.1.lock().unwrap_or_else(|e| e.into_inner()), Some(p));
        // 重复登记：后者覆盖（管理端改配置）
        let p2 = DsPolicy { max_rows: Some(10), timeout: None };
        reg.set_policy(&DsId::new("ds-7"), p2);
        assert_eq!(*fake.1.lock().unwrap_or_else(|e| e.into_inner()), Some(p2));
    }
}
