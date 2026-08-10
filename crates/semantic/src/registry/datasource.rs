//! 【K3-A】数据源注册表：`meta.datasource` 的 CRUD + ds 级可见性 + 向量选源候选。
//! 变更原因＝多源注册与 ds 级可见性。搬运源 `server/src/meta.rs:1659-1881`。

use sqlx::PgPool;

// ─────────────────────────── 【K3-A】数据源注册表 ───────────────────────────
// 变更原因＝多源注册与 ds 级可见性。T7 随 semantic 整块迁到 `registry/datasource.rs`。

/// 一个数据源的登记形态。**无口令**：`dsn_ref` 是 settings.json 里那张映射表的键名，
/// 所以这一行可以随便进接口响应、日志与缓存 key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsSpecRow {
    pub ds_id: String,
    pub name: String,
    pub kind: String,
    pub dialect: String,
    pub dsn_ref: String,
    pub policy_kind: String,
    pub description: String,
    pub status: String,
}

/// 【A8】数据源级查询策略 —— ds 配置 JSON 的可选字段。两个字段都缺省 = 不收紧：
/// connector 的 `fetch` 在入口与调用方值取 min，缺省即与全局两档（200 行 / 30s）恒等；
/// 配得比全局更松也不会放宽任何东西（min 语义由 `DsPolicy::clamp` 守着）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
pub struct DsPolicyConfig {
    /// 单次取数行上限（超出截断）。`0` 是合法的最紧档（恒空结果），不做下限校验。
    #[serde(default)]
    pub max_rows: Option<u32>,
    /// 单次查询超时（毫秒）。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl DsPolicyConfig {
    /// → connector 的执行形态（`usize` 行上限 / `Duration` 超时）。
    pub fn to_ds_policy(self) -> dms_connector::source::DsPolicy {
        dms_connector::source::DsPolicy {
            max_rows: self.max_rows.map(|v| v as usize),
            timeout: self.timeout_ms.map(std::time::Duration::from_millis),
        }
    }
}

const DS_COLS: &str = "ds_id, name, kind, dialect, dsn_ref, policy_kind, description, status";

/// 8 列元组 → `DsSpecRow`（workspace 的 sqlx 没开 `derive` feature，故不 `#[derive(FromRow)]`）
type DsTuple = (String, String, String, String, String, String, String, String);

fn ds_row(t: DsTuple) -> DsSpecRow {
    let (ds_id, name, kind, dialect, dsn_ref, policy_kind, description, status) = t;
    DsSpecRow { ds_id, name, kind, dialect, dsn_ref, policy_kind, description, status }
}

/// 注册表里的 `kind` 字符串 → connector 的 `SourceKind`。**全仓唯一一处映射**
/// （ds_api 的登记校验与 pipeline 的取数建池共用；两份必然漂出一个能过校验却建不出池的组合）。
pub fn source_kind(kind: &str) -> Option<dms_connector::source::SourceKind> {
    use dms_connector::source::SourceKind;
    match kind {
        "mysql" => Some(SourceKind::Mysql),
        "postgres" => Some(SourceKind::Postgres),
        _ => None,
    }
}

/// DMS 主源的固定标识（错误文案、缓存 key、registry 与本表都用它）
pub const DMS_DS_ID: &str = "dms";

/// 上传表格建出的数据源统一用这个 `dsn_ref`：指向**无 meta/kb/chat 权限**的 PG 只读角色（F3）。
/// settings.json 里要有同名键，否则 `probe`/建池会在发起连接前就报「dsn_ref 未配置」。
///
/// 消费者（三处，都在 server）：`kb_api.rs:137`（上传源的 `DsSpecRow.dsn_ref`）、
/// 本文件 `register_upload_datasource`、`db.rs:95`（把 `pg_ro_url` 塞进 dsn 映射表）。
/// 原来这里挂着 `#[allow(dead_code)] // 消费者＝K4 的 tabular 落库`：K4 早落了，
/// 而 `pub` 项在 lib crate 里本来就不触发 dead_code —— 那个 allow 是空操作 + 假注释
/// （读的人会以为这东西还没人用）。
pub const UPLOAD_DSN_REF: &str = "pg_ro_url";

/// 全量列表（管理端用；`GET /api/ds` 必须再与 `visible_datasources` 取交集）
pub async fn list_datasources(pg: &PgPool) -> anyhow::Result<Vec<DsSpecRow>> {
    let rows: Vec<DsTuple> =
        sqlx::query_as(&format!("SELECT {DS_COLS} FROM meta.datasource ORDER BY ds_id"))
            .fetch_all(pg)
            .await?;
    Ok(rows.into_iter().map(ds_row).collect())
}

pub async fn get_datasource(pg: &PgPool, ds_id: &str) -> anyhow::Result<Option<DsSpecRow>> {
    let row: Option<DsTuple> =
        sqlx::query_as(&format!("SELECT {DS_COLS} FROM meta.datasource WHERE ds_id = $1"))
            .bind(ds_id)
            .fetch_optional(pg)
            .await?;
    Ok(row.map(ds_row))
}

/// 登记/更新。`description` 变了就清 embedding（等 K3-B 的 embed build 重建）——
/// 否则选源会一直按旧描述命中。
pub async fn upsert_datasource(pg: &PgPool, d: &DsSpecRow) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO meta.datasource(ds_id, name, kind, dialect, dsn_ref, policy_kind, description, status)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT (ds_id) DO UPDATE SET
           name=$2, kind=$3, dialect=$4, dsn_ref=$5, policy_kind=$6, description=$7, status=$8,
           embedding = CASE WHEN meta.datasource.description = $7 THEN meta.datasource.embedding END",
    )
    .bind(&d.ds_id)
    .bind(&d.name)
    .bind(&d.kind)
    .bind(&d.dialect)
    .bind(&d.dsn_ref)
    .bind(&d.policy_kind)
    .bind(&d.description)
    .bind(&d.status)
    .execute(pg)
    .await?;
    Ok(())
}

/// 注销。ds 级授权一并删除（留着就是「重建同名 ds_id 时旧授权复活」的越权面）。
pub async fn delete_datasource(pg: &PgPool, ds_id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM meta.datasource WHERE ds_id = $1").bind(ds_id).execute(pg).await?;
    sqlx::query("DELETE FROM kb.acl WHERE scope = 'ds' AND target_id = $1")
        .bind(ds_id)
        .execute(pg)
        .await?;
    Ok(())
}

/// 上传表格文档的可用状态与其物化数据源同步。停用、待生效或已失效的文档不能继续被问数。
pub async fn set_upload_datasource_active(
    pg: &PgPool,
    ds_id: &str,
    active: bool,
) -> anyhow::Result<u64> {
    let status = if active { "active" } else { "disabled" };
    Ok(sqlx::query(
        "UPDATE meta.datasource SET status=$2 WHERE ds_id=$1 AND dsn_ref=$3",
    )
    .bind(ds_id)
    .bind(status)
    .bind(UPLOAD_DSN_REF)
    .execute(pg)
    .await?
    .rows_affected())
}

/// 向量选源：按 description 的 embedding 取最近邻 `(ds_id, 余弦距离)`。
/// 没有一行有 embedding（embed 服务缺席 / 还没跑过 build）→ 返回空，
/// 调用方（`pipeline::select_source`）降级到主源。
pub async fn nearest_datasources(
    pg: &PgPool,
    qvec: &str,
    k: i64,
) -> anyhow::Result<Vec<(String, f64)>> {
    Ok(sqlx::query_as(
        "SELECT ds_id, (embedding <=> $1::vector) AS dist FROM meta.datasource
         WHERE status = 'active' AND embedding IS NOT NULL
         ORDER BY embedding <=> $1::vector LIMIT $2",
    )
    .bind(qvec)
    .bind(k)
    .fetch_all(pg)
    .await?)
}

/// 【K4】上传表格建库后登记。可见性不复制第二套授权，直接动态继承来源文档的空间 owner/ACL。
///
/// 消费者：`server/src/kb_api.rs:103`（上传表格落库后的通道②，那里也有一条同款注释钉着
/// 「必须是本函数一个调用」）。原来挂的 `#[allow(dead_code)] // 消费者＝K4 的 tabular 落库`
/// 与上面 `UPLOAD_DSN_REF` 那个同一批，都是空操作＋假注释，已删（实测删掉后
/// `-p dms-semantic` 一条警告都没冒）。
pub async fn register_upload_datasource(
    pg: &PgPool,
    ds_id: &str,
    name: &str,
    description: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO meta.datasource(ds_id, name, kind, dialect, dsn_ref, policy_kind, description)
         VALUES ($1,$2,'postgres','postgres',$3,'global',$4)
         ON CONFLICT (ds_id) DO UPDATE SET name=$2, description=$4, status='active'",
    )
    .bind(ds_id)
    .bind(name)
    .bind(UPLOAD_DSN_REF)
    .bind(description)
    .execute(pg)
    .await?;
    Ok(())
}

/// ds 级可见性谓词，内联进 SQL（**不做「查完再过滤」**，与 knowledge 的 ACL 同一纪律）。
/// 占位符：`$1` = login（text）、`$2` = 角色码（text[]）。判据三条取并集：
/// ① `policy_kind='dms_datascope'` 的源对所有认证用户可见——行级权限由 `inject` 兜着；
/// ② 上传源动态继承来源文档所在空间 owner、space/doc ACL 与文档生命周期；
/// ③ 其他外部源才使用独立 `kb.acl(scope='ds')`。
const DS_VISIBLE_PRED: &str = "d.status = 'active' \
  AND (\
       d.policy_kind = 'dms_datascope' \
    OR (d.dsn_ref = 'pg_ro_url' AND EXISTS (SELECT 1 FROM kb.doc kd \
         WHERE d.ds_id = 'upload_' || kd.doc_id AND kd.enabled=true \
           AND kd.status IN ('chunked','embedded') \
           AND (kd.effective_from IS NULL OR kd.effective_from <= CURRENT_DATE) \
           AND (kd.effective_to IS NULL OR kd.effective_to >= CURRENT_DATE) \
           AND (EXISTS (SELECT 1 FROM kb.space ks \
                        WHERE ks.space_id=kd.space_id AND ks.owner=$1) \
             OR EXISTS (SELECT 1 FROM kb.acl ka \
                        WHERE ka.perm IN ('read','write') \
                          AND ((ka.scope='space' AND ka.target_id=kd.space_id) \
                            OR (ka.scope='doc' AND ka.target_id=kd.doc_id)) \
                          AND ((ka.grantee_kind='login' AND ka.grantee=$1) \
                            OR (ka.grantee_kind='role' AND ka.grantee=ANY($2::text[]))))))) \
    OR (d.dsn_ref <> 'pg_ro_url' AND EXISTS (SELECT 1 FROM kb.acl a \
                WHERE a.scope = 'ds' AND a.target_id = d.ds_id \
                  AND a.perm IN ('read','write') \
                  AND ((a.grantee_kind = 'login' AND a.grantee = $1) \
                    OR (a.grantee_kind = 'role'  AND a.grantee = ANY($2::text[]))))))";

/// 该 viewer 可见的数据源 id。判据全在 SQL 里（见 `DS_VISIBLE_PRED`）。
pub async fn visible_datasources(
    pg: &PgPool,
    login: &str,
    roles: &[String],
) -> anyhow::Result<Vec<String>> {
    // 🔴 **必须调 `visible_datasources_sql()`，不许在这里再 `format!` 一份。**
    //
    // 原来这里自己拼一份、单测读另一份。谓词本身确实不会分叉（两处插值的是同一个
    // `DS_VISIBLE_PRED` const，往里加 `OR true` 会被下面那条断言抓到）——
    // 但**谓词之外**的部分测试读不到：把这一行改成
    // `format!("... WHERE {DS_VISIBLE_PRED} OR d.owner_login = $1 ...")`
    // 就是一条越权放行，而单测照旧全绿（它读的是另一个函数返回的串）。
    // 让生产与判据读**同一个字符串**，这个缝就不存在了；`#[allow(dead_code)]` 也随之去掉。
    let sql = visible_datasources_sql();
    let rows: Vec<(String,)> =
        sqlx::query_as(&sql).bind(login).bind(roles).fetch_all(pg).await?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

/// ds 可见性的**唯一** SQL 来源：生产（`visible_datasources`）与单测都读这一份。
pub fn visible_datasources_sql() -> String {
    format!("SELECT d.ds_id FROM meta.datasource d WHERE {DS_VISIBLE_PRED} ORDER BY d.ds_id")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 可见性必须在 SQL 内 JOIN `kb.acl` 并按 grantee 过滤。
    /// 有人日后改成「查完再按 Rust 过滤」，或漏掉 grantee/perm 任一条，这里就红。
    #[test]
    fn visible_datasources_filters_acl_in_sql() {
        let s = visible_datasources_sql();
        assert!(s.contains("kb.acl"), "ACL 必须在 SQL 内 JOIN，不许后过滤：{s}");
        assert!(s.contains("a.grantee = $1"));
        assert!(s.contains("a.grantee = ANY($2::text[])"));
        assert!(s.contains("a.scope = 'ds'"));
        assert!(s.contains("a.perm IN ('read','write')"));
        // ① DMS 主库对所有认证用户可见（行级权限由 inject 兜）
        assert!(s.contains("d.policy_kind = 'dms_datascope'"));
        // ③ 没有「所有人可见 upload 源」的兜底：谓词里不许出现恒真项
        assert!(!s.contains("1=1") && !s.contains("OR true"), "不许有兜底放行：{s}");
        assert!(s.contains("d.status = 'active'"));
        assert!(s.contains("d.ds_id = 'upload_' || kd.doc_id"));
        assert!(s.contains("ks.space_id=kd.space_id AND ks.owner=$1"));
        assert!(s.contains("ka.scope='space'") && s.contains("ka.scope='doc'"));
        assert!(s.contains("ka.grantee=$1") && s.contains("ka.grantee=ANY($2::text[])"));
        assert!(s.contains("d.dsn_ref <> 'pg_ro_url' AND EXISTS"));
        assert!(s.contains("kd.enabled=true"));
        assert!(s.contains("kd.status IN ('chunked','embedded')"));
        assert!(s.contains("kd.effective_from IS NULL OR kd.effective_from <= CURRENT_DATE"));
        assert!(s.contains("kd.effective_to IS NULL OR kd.effective_to >= CURRENT_DATE"));
        // 🔴 **判据读的必须是生产那一份字符串**。
        // 这条断言存在的理由：以前 `visible_datasources` 自己 `format!` 一份、本测试读另一份，
        // 于是「在谓词之外接一个 `OR d.owner_login = $1`」这种越权放行**测不到**
        // （谓词本身有上面几条守着，缝在谓词外面）。现在两边共用 `visible_datasources_sql()`。
        // 形状锁：整条 SQL 必须**逐字**等于「模板 + 那个 const」，谓词之外多一个字都红。
        // （不能按 `" WHERE "` 计数 —— const 内部的 EXISTS 子查询自带一个 WHERE，实测是 2 个。）
        assert_eq!(
            s,
            format!("SELECT d.ds_id FROM meta.datasource d WHERE {DS_VISIBLE_PRED} ORDER BY d.ds_id"),
            "生产那条 SQL 的形状变了：谓词之外接了别的条件？{s}"
        );
    }

    #[test]
    fn upload_datasource_state_is_explicitly_switchable() {
        let src = include_str!("datasource.rs");
        let body = src.split("pub async fn set_upload_datasource_active").nth(1).unwrap();
        assert!(body.contains("status=$2") && body.contains("UPLOAD_DSN_REF"));
    }

    /// A8：配置 JSON 的缺省/取值 → connector 策略；min 语义只许更紧、默认恒等
    #[test]
    fn ds_policy_config_defaults_to_no_tightening_and_only_tightens() {
        use std::time::Duration;
        let global = (200usize, Duration::from_secs(30)); // 全局两档（dms_agent::MAX_ROWS / EXEC_TIMEOUT）
        // 缺省（字段整个不写）= 全 None = 与全局取 min 恒等
        let c: DsPolicyConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, DsPolicyConfig::default());
        assert_eq!(c.to_ds_policy().clamp(global.0, global.1), global);
        // 显式更紧 → 生效
        let c: DsPolicyConfig = serde_json::from_str(r#"{"max_rows":50,"timeout_ms":1500}"#).unwrap();
        assert_eq!(c.max_rows, Some(50));
        assert_eq!(c.timeout_ms, Some(1500));
        assert_eq!(c.to_ds_policy().clamp(global.0, global.1), (50, Duration::from_millis(1500)));
        // 显式更松 → 不放宽（调用方值原样穿过）
        let c: DsPolicyConfig =
            serde_json::from_str(r#"{"max_rows":5000,"timeout_ms":120000}"#).unwrap();
        assert_eq!(c.to_ds_policy().clamp(global.0, global.1), global);
        // 只配一个维度：另一维不收紧
        let c: DsPolicyConfig = serde_json::from_str(r#"{"timeout_ms":900}"#).unwrap();
        assert_eq!(c.max_rows, None);
        assert_eq!(c.to_ds_policy().clamp(global.0, global.1), (200, Duration::from_millis(900)));
    }
}
