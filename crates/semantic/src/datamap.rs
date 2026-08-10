//! 数仓数据地图（DataLink 移植）：只读小样画像 → 四类列间推断 → `meta.datamap_edge` 落库。
//!
//! 调研源 `docs/research/datafoundry.json` DataLink 节 +
//! `services/datalink/src/datalink/{profiler,inferrer,graph}`（joinable/synonym/distribution
//! 三类推断与置信度形态逐条对齐，差异处在各函数注释里写明；第四类 correlated 按
//! DataLink「同 DataFrame 成对数值列相关」的精神落成同表两列联合采样，见 ②b 节）。
//!
//! ## 纪律（与本 crate 全局纪律同色）
//! - **只画像已验证目录**（`warehouse_catalog::ASSETS` 编译期白名单）内的表；目录外的表不碰。
//! - **只读数仓**：入口对连接能力 fail-closed（非数仓能力直接 Err，绝不对生产 MySQL 采样）；
//!   每条采样 SQL 照走 `RawSql → check → unrestricted → fetch` 全管道（同 autodiscover 的
//!   `probe.rs`，不开后门），标识符先过 `ident()` 白名单才进反引号。
//! - **禁全表扫**：行数取 information_schema 探针的估算值（`row_estimate`，不另发 COUNT(*)），
//!   空值率/基数/Top 值全部来自 `LIMIT SAMPLE_ROWS` 的小样。证据 JSON 记录样本量，调用方
//!   据此知道这是「样本内」口径而不是全量统计。
//! - **推断边 status 一律 `pending`**：落库只是「待验收候选」，**绝不直接进装配/召回**
//!   （只有验收过的口径进合同）。验收/驳回是人工动作（直接改 status 列），本模块只写
//!   pending，且重跑 upsert 不覆盖既有 status —— 人工结论不被下一轮推断冲掉。
//!
//! ## 接线说明（本模块不改动 main.rs / retrieve.rs / direct.rs）
//!
//! 典型调用点是一个 CLI 管理任务（与 `meta autodiscover` 同款的管理员上下文）：
//! ```ignore
//! let snapshot = mysql.probe_schema_with_warehouse_catalog(&warehouse_catalog::metadata_assets()).await?;
//! let report = dms_semantic::datamap::build(pg, mysql, &gate, ds_id, &snapshot.0).await?;
//! ```
//! `UnrestrictedProof` 由调用方铸造（铸造点必须留在能判定「这是管理任务、没有以谁的身份查」
//! 的那一层，同 `ingest::autodiscover::probe::ProbeGate` 的先例）。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use dms_connector::mysql::ReadOnlyMySql;
use dms_connector::source::{ColumnInfo, SchemaSnapshot, SqlSource};
use dms_kernel::sql::guard::GuardConfig;
use dms_kernel::UnrestrictedProof;
use sqlx::PgPool;

use crate::warehouse_catalog::{self, Asset};

/// 单表采样行上限：小 LIMIT 采样，500 行足够判空值率/基数/Top 值，又不给数仓加压。
const SAMPLE_ROWS: usize = 500;
/// 单表采样超时：对齐 autodiscover 单探针 10s 的既有护栏（`probe.rs::fetch_distinct`）。
/// 悬挂采样跳过该表、不拖全局（与探针同一语义）。
const PROFILE_TIMEOUT: Duration = Duration::from_secs(10);
/// Top 值条数（DataLink `top_n=10`）。
const TOP_VALUES: usize = 10;
/// 值重叠率下限（DataLink `overlap_threshold=0.1`）：低于它连「弱」档都不发边。
const OVERLAP_FLOOR: f64 = 0.1;
/// 分布相似度下限（DataLink `similarity_threshold=0.5`）。
const DISTRIBUTION_FLOOR: f64 = 0.5;
/// 相关系数绝对值下限（correlated）：|r| 低于它连「弱」档都不发边（小样噪音区）。
const CORRELATION_FLOOR: f64 = 0.5;
/// 相关判定的最小成对样本量：两列**同行同非空**的行数低于它，系数没有判据意义。
const CORRELATION_MIN_PAIRS: usize = 30;
/// 每表参与相关性配对的数值列上限（按 ordinal 取前 N）：两两配对 O(n²) 且每对一条
/// 联合采样 SQL，8 列 = 28 对封顶 —— 防宽表把一轮建图拖成几百条采样（护栏）。
const CORRELATION_MAX_NUMERIC_COLS: usize = 8;

/// 采样闸门的两件凭证（形状照抄 `probe::ProbeGate`）：proof 由调用方铸造，guard 提供
/// 只读红线词表与行上限（本模块 SQL 自带 LIMIT，`ensure_limit` 不会改写）。
pub struct MapGate<'a> {
    pub proof: &'a UnrestrictedProof,
    pub guard: &'a GuardConfig,
}

/// 一列的样本画像。`counts` 是样本内非空取值 → 次数（基数 = `counts.len()`）；
/// 全部统计都是「样本内」口径，`sampled`/`row_estimate` 让消费方分得清。
#[derive(Debug, Clone)]
pub struct ColumnProfile {
    pub database: String,
    pub table: String,
    pub column: String,
    pub data_type: String,
    /// 已过 `ingest::sanitize_comment`（与进 schema/prompt 的是同一份净化）。
    pub comment: String,
    /// information_schema 估算行数（不做业务口径，只进证据）。
    pub row_estimate: i64,
    /// 实际采样行数（≤ `SAMPLE_ROWS`，被 guard/fetch 行上限收紧时更小）。
    pub sampled: usize,
    /// 样本内 NULL + 空串行数（Doris 常用 '' 代 NULL，一并算空）。
    pub nulls: usize,
    counts: HashMap<String, u64>,
    /// 样本内 Top-`TOP_VALUES`（次数降序、同次按取值升序，确定性）。
    pub top_values: Vec<(String, u64)>,
}

impl ColumnProfile {
    /// `db.table.column` 全小写规范形态：边 src/dst 与证据都用它。
    pub fn id(&self) -> String {
        format!(
            "{}.{}.{}",
            self.database.to_ascii_lowercase(),
            self.table.to_ascii_lowercase(),
            self.column.to_ascii_lowercase()
        )
    }

    pub fn null_rate(&self) -> f64 {
        if self.sampled == 0 {
            0.0
        } else {
            self.nulls as f64 / self.sampled as f64
        }
    }

    pub fn cardinality(&self) -> usize {
        self.counts.len()
    }

    fn non_null(&self) -> usize {
        self.sampled.saturating_sub(self.nulls)
    }
}

/// 四类推断边。`as_str` 的取值就是落库 `kind` 列与 DDL CHECK 的唯一事实源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    Joinable,
    Synonym,
    DistributionSimilar,
    Correlated,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Joinable => "joinable",
            Self::Synonym => "synonym",
            Self::DistributionSimilar => "distribution_similar",
            Self::Correlated => "correlated",
        }
    }
}

/// 一条待落库的推断边。src/dst 已按字典序规范化（`a < b`），同一对列无论从哪侧先发现
/// 都收敛到同一行 —— upsert 幂等的前提。
#[derive(Debug, Clone)]
pub struct DataEdge {
    pub kind: EdgeKind,
    pub src: String,
    pub dst: String,
    pub confidence: f64,
    pub evidence: serde_json::Value,
}

impl DataEdge {
    fn new(kind: EdgeKind, a: &ColumnProfile, b: &ColumnProfile, confidence: f64, evidence: serde_json::Value) -> Self {
        Self::with_refs(kind, a.id(), b.id(), confidence, evidence)
    }

    /// 引用版构造（correlated 用：配对来自两列联合采样，手里是列元信息不是画像）。
    /// 与 `new` 同一条规范化纪律：src/dst 按字典序，同一对列反向发现收敛同一行。
    fn with_refs(kind: EdgeKind, a: String, b: String, confidence: f64, evidence: serde_json::Value) -> Self {
        let (src, dst) = if a <= b { (a, b) } else { (b, a) };
        Self { kind, src, dst, confidence, evidence }
    }
}

/// 一次建图的产出汇总（CLI/日志透出用）。
#[derive(Debug)]
pub struct DataMapReport {
    pub ds_id: String,
    pub tables_profiled: usize,
    pub columns_profiled: usize,
    /// （表名， 跳过原因）：采样被拒/超时/无可采样列，逐表记录不静默。
    pub tables_skipped: Vec<(String, String)>,
    /// upsert 进 `meta.datamap_edge` 的边数（全部 status=pending）。
    pub edges: usize,
    pub joinable: usize,
    pub synonym: usize,
    pub distribution_similar: usize,
    pub correlated: usize,
}

// ── ① 画像 ─────────────────────────────────────────────────────────────

/// 反引号标识符白名单：`[A-Za-z0-9_$]`、≤64 字符（MySQL 上限）。
/// 与 `probe.rs::ident` 同一规则（那一个是私有的；判定逻辑保持逐字一致，散开两处是
/// 文件边界所迫，改规则要两边一起改）。含反引号的列名闭合引号 = 一条带 unrestricted
/// 放行的任意读 —— 拼不出 SQL 才是正确结果。
fn ident(s: &str) -> Option<&str> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    ok.then_some(s)
}

/// 可采样列：Doris 聚合态/草图类型（HLL/BITMAP/…）直接 SELECT 会报错或读出二进制，
/// 跳过；敏感列不投影（`registry::is_sensitive_col` 与 `is_safe_select` 共用词表与
/// contains 语义 —— 列名含敏感词的 SQL 根本过不了 `check()`，这里先剔掉省一次往返）。
fn samplable(c: &ColumnInfo) -> bool {
    let t = c.data_type.to_ascii_lowercase();
    if ["hll", "bitmap", "quantile_state", "agg_state"].iter().any(|x| t.contains(x)) {
        return false;
    }
    !crate::registry::is_sensitive_col(&c.name)
}

/// 采样 SQL（纯函数，可单测）：`SELECT \`c1\`,… FROM \`db\`.\`table\` LIMIT 500`。
/// 位置参数拼接（`drift.rs` 的命名插值白名单只放既有文件）；库/表/列名全部先过 `ident()`，
/// `None` = 标识符非法 → 不采样（fail-closed）。
fn sample_sql(db: &str, table: &str, cols: &[&ColumnInfo]) -> Option<String> {
    let db = ident(db)?;
    let table = ident(table)?;
    if cols.is_empty() {
        return None;
    }
    let projection = cols
        .iter()
        .map(|c| ident(&c.name).map(|c| format!("`{c}`")))
        .collect::<Option<Vec<_>>>()?
        .join(", ");
    Some(format!(
        "SELECT {} FROM `{}`.`{}` LIMIT {}",
        projection, db, table, SAMPLE_ROWS
    ))
}

/// 动态采样 SQL 的闸门（同 `probe::probe_scoped` 的裁决：管理任务动态 SQL 走同一条全管道）。
fn map_scoped(sql: &str, gate: &MapGate<'_>) -> anyhow::Result<dms_kernel::ScopedSql> {
    let checked =
        dms_kernel::check(dms_kernel::RawSql::new(sql), &dms_kernel::MysqlDialect, gate.guard)?;
    Ok(dms_kernel::ScopedSql::unrestricted(checked, gate.proof))
}

/// 单元格 → 取值：NULL 与去空白后的空串都算「空」（Doris 常用 '' 代 NULL）；
/// 字符串按去空白原文，数值/布尔走 JSON 规范形态（DECIMAL 在连接层已是字符串，保精度）。
fn cell_value(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        other => Some(other.to_string()),
    }
}

/// 由一列的样本单元格构建画像（纯函数，可单测）。
fn profile_column(
    database: &str,
    table: &str,
    col: &ColumnInfo,
    row_estimate: i64,
    cells: &[Option<String>],
) -> ColumnProfile {
    let nulls = cells.iter().filter(|c| c.is_none()).count();
    let mut counts: HashMap<String, u64> = HashMap::new();
    for v in cells.iter().flatten() {
        *counts.entry(v.clone()).or_default() += 1;
    }
    let mut top: Vec<(String, u64)> = counts.iter().map(|(v, n)| (v.clone(), *n)).collect();
    // 次数降序、同次按取值升序：同一份样本必得同一份 Top，证据可复现。
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top.truncate(TOP_VALUES);
    ColumnProfile {
        database: database.to_string(),
        table: table.to_string(),
        column: col.name.clone(),
        data_type: col.data_type.clone(),
        comment: crate::ingest::sanitize_comment(&col.comment),
        row_estimate,
        sampled: cells.len(),
        nulls,
        counts,
        top_values: top,
    }
}

/// 采样一张目录表并画像其全部可采样列。`None` = 不采样（原因已 warn）：
/// 无可采样列 / 标识符非法 / 闸门拒绝 / 超时或执行失败 / 回包列对不上（防止把值记错列）。
async fn profile_table(
    mysql: &ReadOnlyMySql,
    gate: &MapGate<'_>,
    asset: &Asset,
    row_estimate: i64,
    cols: &[&ColumnInfo],
) -> Option<Vec<ColumnProfile>> {
    let (db, table) = (asset.database, asset.table);
    let cols: Vec<&ColumnInfo> = cols.iter().copied().filter(|c| samplable(c)).collect();
    let Some(sql) = sample_sql(db, table, &cols) else {
        tracing::warn!("datamap 跳过 {db}.{table}：无可采样列或标识符非法");
        return None;
    };
    let scoped = match map_scoped(&sql, gate) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("datamap 采样被闸门拒绝 {db}.{table}: {e}");
            return None;
        }
    };
    let rs = match mysql.fetch(&scoped, SAMPLE_ROWS, PROFILE_TIMEOUT).await {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!("datamap 采样失败 {db}.{table}: {e}");
            return None;
        }
    };
    // 回包列名必须与投影逐位对上（大小写不敏感）：对不上就整表放弃，
    // 宁可缺画像也不把 A 列的值记到 B 列头上（错画像 = 垃圾边）。
    if rs.columns.len() != cols.len()
        || !rs.columns.iter().zip(&cols).all(|(got, want)| got.eq_ignore_ascii_case(&want.name))
    {
        tracing::warn!("datamap 跳过 {db}.{table}：回包列与投影不符 {:?}", rs.columns);
        return None;
    }
    Some(
        cols.iter()
            .enumerate()
            .map(|(i, col)| {
                let cells: Vec<Option<String>> =
                    rs.rows.iter().map(|r| r.get(i).and_then(cell_value)).collect();
                profile_column(db, table, col, row_estimate, &cells)
            })
            .collect(),
    )
}

// ── ② 三类推断（全部纯函数，样本即证据）─────────────────────────────────

/// 列名规范化：小写、去 `_`/`-`/空格。`store_code`/`StoreCode`/`storecode` 收敛同名；
/// `storecode` 与 `customer_code` 不收敛（它们的桥靠值重叠，不靠名字）。
fn normalize_name(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 名称相似度（逐字移植 DataLink `_name_similarity`）：规范化同名 1.0 → 包含关系按长度比
/// 0.5–1.0 → 否则取字符 Jaccard 与前缀比的较大者。
fn name_similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (normalize_name(a), normalize_name(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    if a.contains(&b) || b.contains(&a) {
        let (short, long) = (a.len().min(b.len()), a.len().max(b.len()));
        return 0.5 + 0.5 * short as f64 / long as f64;
    }
    let (sa, sb): (HashSet<char>, HashSet<char>) = (a.chars().collect(), b.chars().collect());
    let jaccard = sa.intersection(&sb).count() as f64 / sa.union(&sb).count() as f64;
    let prefix = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .count() as f64
        / a.len().max(b.len()) as f64;
    jaccard.max(prefix)
}

/// 粗粒度类型桶（joinable/distribution 的兼容性判据；来源是 information_schema 的
/// `data_type`，不是 DataLink 的 pandas dtype，故按 MySQL/Doris 类型名归桶）。
/// `None` = 认不出的类型，不参与带类型闸的推断。
fn dtype_bucket(data_type: &str) -> Option<&'static str> {
    let t = data_type.to_ascii_lowercase();
    if t.contains("int") || t.contains("decimal") || t.contains("double") || t.contains("float") || t.contains("numeric") {
        Some("numeric")
    } else if t.starts_with("date") || t.starts_with("time") || t == "year" {
        Some("temporal")
    } else if t.contains("char") || t.contains("string") || t.contains("text") || t.contains("enum") || t == "boolean" {
        Some("string")
    } else {
        None
    }
}

/// joinable 的类型兼容（DataLink `_compatible_dtypes`）：同桶可判；编码列数值/字符串混存
/// 是常态（storecode 一侧 VARCHAR 一侧 BIGINT），numeric↔string 放行；temporal 只跟 temporal。
fn joinable_dtypes(a: &str, b: &str) -> bool {
    match (dtype_bucket(a), dtype_bucket(b)) {
        (Some(x), Some(y)) if x == y => true,
        (Some(x), Some(y)) => {
            matches!((x, y), ("numeric", "string") | ("string", "numeric"))
        }
        _ => false,
    }
}

/// 琐碎值域（DataLink 的 boolean-skip 推广到本仓）：样本取值全落在 {0,1,true,false} 的列
/// 与任何同类列必然高重叠，joinable 与 distribution 都跳过（DataLink 只给 joinable 跳过，
/// 但 {0,1}↔{0,1} 的 distribution 边同样是评审噪音 —— 差异点记录在此）。
fn trivial_domain(p: &ColumnProfile) -> bool {
    const TRIVIAL: &[&str] = &["0", "1", "0.0", "1.0", "true", "false"];
    !p.counts.is_empty() && p.counts.keys().all(|v| TRIVIAL.contains(&v.to_ascii_lowercase().as_str()))
}

/// 值重叠率（DataLink `_compute_overlap`）：|A∩B| / min(|A|,|B|)，样本取值集即证据。
fn value_overlap(a: &ColumnProfile, b: &ColumnProfile) -> f64 {
    if a.counts.is_empty() || b.counts.is_empty() {
        return 0.0;
    }
    let (small, big) = if a.counts.len() <= b.counts.len() { (a, b) } else { (b, a) };
    let inter = small.counts.keys().filter(|v| big.counts.contains_key(*v)).count();
    inter as f64 / small.counts.len() as f64
}

/// 重叠率分档（置信度基数）：<0.1 不发边；[0.1,0.3) 弱 0.35；[0.3,0.6) 中 0.55；
/// [0.6,0.9) 强 0.75；≥0.9 极强 0.90。抽样口径下不给 1.0 —— 样本不是全量。
fn overlap_band(rate: f64) -> Option<f64> {
    if rate < OVERLAP_FLOOR {
        None
    } else if rate < 0.3 {
        Some(0.35)
    } else if rate < 0.6 {
        Some(0.55)
    } else if rate < 0.9 {
        Some(0.75)
    } else {
        Some(0.90)
    }
}

/// joinable 置信度 = 重叠率分档 + 列名规范匹配加成（+0.05，封顶 0.95）。
/// `storecode ↔ customer_code` 这种名不同、值域同的桥全靠重叠率撑分。
fn joinable_confidence(rate: f64, name_match: bool) -> Option<f64> {
    let band = overlap_band(rate)?;
    Some((band + if name_match { 0.05 } else { 0.0 }).min(0.95))
}

/// synonym 判分（DataLink `_compute_confidence` 的本仓形态）：DataLink 用「同 semantic_type」
/// 当强信号，我们没有 pandas 画像的类型分类，用**注释一致**（同一业务命名的物理证据）顶替。
/// 与 DataLink 的另一处差异：规范化同名（`store_code`≡`storecode`）**不发** synonym 边 ——
/// 那个信号已经由 joinable 的 name_match 加成表达，同义边只留给「名字不同、语义相同」，
/// 否则评审队列会被 `order_date`↔`order_date` 这类自明边淹没。
fn synonym_confidence(a: &ColumnProfile, b: &ColumnProfile) -> Option<f64> {
    if normalize_name(&a.column) == normalize_name(&b.column) {
        return None;
    }
    let comment_match = !a.comment.is_empty() && a.comment == b.comment;
    let name_sim = name_similarity(&a.column, &b.column);
    if comment_match && name_sim > 0.5 {
        Some(0.95)
    } else if comment_match {
        Some(0.85)
    } else if name_sim > 0.7 {
        Some(0.6)
    } else if name_sim > 0.5 {
        Some(0.4)
    } else {
        None
    }
}

/// distribution_similar 相似度 = 基数比 × Top 值加权重合度（任务口径：「基数与 Top 值重合度」；
/// Top 值加权重合逐字移植 DataLink `_categorical_similarity`）。乘法组合是有意的：
/// 任一项为 0 则整体为 0，不会出现「基数碰巧相同、Top 值零交集」的假相似边。
fn distribution_similarity(a: &ColumnProfile, b: &ColumnProfile) -> Option<f64> {
    let bucket = dtype_bucket(&a.data_type)?;
    if dtype_bucket(&b.data_type)? != bucket || trivial_domain(a) || trivial_domain(b) {
        return None;
    }
    let (ca, cb) = (a.cardinality(), b.cardinality());
    if ca == 0 || cb == 0 {
        return None;
    }
    let card_ratio = ca.min(cb) as f64 / ca.max(cb) as f64;
    let (na, nb) = (a.non_null() as f64, b.non_null() as f64);
    if na <= 0.0 || nb <= 0.0 {
        return None;
    }
    let top_b: HashMap<&str, f64> =
        b.top_values.iter().map(|(v, n)| (v.as_str(), *n as f64 / nb)).collect();
    let mut weighted = 0.0f64;
    let mut total_a = 0.0f64;
    for (v, n) in &a.top_values {
        let fa = *n as f64 / na;
        total_a += fa;
        if let Some(fb) = top_b.get(v.as_str()) {
            weighted += fa.min(*fb);
        }
    }
    let total_b: f64 = top_b.values().sum();
    let denom = (total_a + total_b) / 2.0;
    if denom <= 0.0 {
        return None;
    }
    let similarity = card_ratio * (weighted / denom);
    (similarity >= DISTRIBUTION_FLOOR).then_some(similarity)
}

fn round4(x: f64) -> f64 {
    (x * 10000.0).round() / 10000.0
}

/// 三类跨表推断的统一入口（纯函数）：跨表列对全组合，同表不比（DataLink 同款）。
/// 第四类 correlated 是同表两列**联合采样**口径，样本不来自画像，不走这里（见 ②b）。
/// 同一（kind, 无序列对）只留一条边：src/dst 按字典序规范化，重跑/反向发现都收敛同一行。
pub fn infer_edges(profiles: &[ColumnProfile]) -> Vec<DataEdge> {
    let mut sorted: Vec<&ColumnProfile> = profiles.iter().collect();
    sorted.sort_by(|a, b| a.id().cmp(&b.id()));
    // 同（库,表,列）去重：探针快照在「当前库 + 跨库目录合并」边界上可能重复登记。
    let mut seen = HashSet::new();
    sorted.retain(|p| seen.insert(p.id()));
    let mut dedup: BTreeMap<(EdgeKind, String, String), DataEdge> = BTreeMap::new();
    let mut push = |edge: DataEdge| {
        dedup
            .entry((edge.kind, edge.src.clone(), edge.dst.clone()))
            .and_modify(|old| {
                if edge.confidence > old.confidence {
                    *old = edge.clone();
                }
            })
            .or_insert(edge);
    };
    for i in 0..sorted.len() {
        for &b in sorted.iter().skip(i + 1) {
            let a = sorted[i];
            if a.database.eq_ignore_ascii_case(&b.database) && a.table.eq_ignore_ascii_case(&b.table) {
                continue;
            }
            if let Some(conf) = synonym_confidence(a, b) {
                push(DataEdge::new(
                    EdgeKind::Synonym,
                    a,
                    b,
                    conf,
                    serde_json::json!({
                        "name_similarity": round4(name_similarity(&a.column, &b.column)),
                        "comment_match": !a.comment.is_empty() && a.comment == b.comment,
                        "comment_a": a.comment, "comment_b": b.comment,
                    }),
                ));
            }
            if joinable_dtypes(&a.data_type, &b.data_type) && !trivial_domain(a) && !trivial_domain(b) {
                let rate = value_overlap(a, b);
                let name_match = normalize_name(&a.column) == normalize_name(&b.column);
                if let Some(conf) = joinable_confidence(rate, name_match) {
                    push(DataEdge::new(
                        EdgeKind::Joinable,
                        a,
                        b,
                        conf,
                        serde_json::json!({
                            "overlap_rate": round4(rate),
                            "overlap_band": overlap_band(rate),
                            "name_match": name_match,
                            "cardinality_a": a.cardinality(), "cardinality_b": b.cardinality(),
                            "sampled_a": a.sampled, "sampled_b": b.sampled,
                            "row_estimate_a": a.row_estimate, "row_estimate_b": b.row_estimate,
                        }),
                    ));
                }
            }
            if let Some(sim) = distribution_similarity(a, b) {
                push(DataEdge::new(
                    EdgeKind::DistributionSimilar,
                    a,
                    b,
                    round4(sim),
                    serde_json::json!({
                        "similarity": round4(sim),
                        "cardinality_a": a.cardinality(), "cardinality_b": b.cardinality(),
                        "top_a": a.top_values, "top_b": b.top_values,
                    }),
                ));
            }
        }
    }
    dedup.into_values().collect()
}

// ── ②b correlated：同表数值列对联合采样相关（DataLink 第四类推断）────────────────
//
// DataLink 的 correlated 在 pandas 画像上跑 `df.corr()`（默认 Pearson）—— 前提是**同一个
// DataFrame 的成对行**。本仓的单列画像是各自独立 LIMIT 采样，两次采样的第 i 行不是同一行，
// 拼在一起算相关没有语义 —— 故按 DataLink 精神改为「同表两列联合采样」：一条 SQL 选两列
// （复用 `sample_sql` 的 ident 白名单/LIMIT），走同一条 MapGate 全管道与 10s 超时，在
// 「同行同非空」的成对样本上算 Pearson（判据）+ Spearman（证据）。跨表不做：跨表行对齐
// 需要 JOIN 键，那已经超出统计推断、进入业务口径判定。

/// 成对数值提取（纯函数）：任一侧空/非数值/非有限值都丢行（`parse::<f64>` 收 "NaN"/"inf"
/// 这类串，必须挡掉 —— 混进一个 NaN 会让系数变 NaN，分档比较全 false 落进最高档）。
fn paired_numbers(rows: &[(Option<String>, Option<String>)]) -> Vec<(f64, f64)> {
    rows.iter()
        .filter_map(|(a, b)| {
            let x = a.as_deref()?.trim().parse::<f64>().ok()?;
            let y = b.as_deref()?.trim().parse::<f64>().ok()?;
            (x.is_finite() && y.is_finite()).then_some((x, y))
        })
        .collect()
}

/// Pearson 积差相关（DataLink `df.corr()` 默认形态，分档判据锚它）。纯函数。
/// None = 成对样本不足 `CORRELATION_MIN_PAIRS` / 任一侧零方差（常数列与谁都不相关，不是边）。
fn pearson(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < CORRELATION_MIN_PAIRS {
        return None;
    }
    let n = pairs.len() as f64;
    let (mx, my) = (
        pairs.iter().map(|p| p.0).sum::<f64>() / n,
        pairs.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sxy, mut sxx, mut syy) = (0.0f64, 0.0f64, 0.0f64);
    for &(x, y) in pairs {
        let (dx, dy) = (x - mx, y - my);
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    if sxx <= 0.0 || syy <= 0.0 {
        return None;
    }
    let r = (sxy / (sxx * syy).sqrt()).clamp(-1.0, 1.0);
    r.is_finite().then_some(r)
}

/// 换秩（同值取平均秩，1 起）。纯函数。
fn ranks(vals: &[f64]) -> Vec<f64> {
    let mut indexed: Vec<(usize, f64)> = vals.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = vec![0.0; indexed.len()];
    let mut i = 0;
    while i < indexed.len() {
        let mut j = i;
        while j + 1 < indexed.len() && indexed[j + 1].1 == indexed[i].1 {
            j += 1;
        }
        let rank = (i + j) as f64 / 2.0 + 1.0;
        for k in i..=j {
            out[indexed[k].0] = rank;
        }
        i = j + 1;
    }
    out
}

/// Spearman 秩相关（证据辅助，不做判据）：两侧各自换秩后算 Pearson。线性之外的单调
/// 关系（x 与 x²）Pearson 会低估，秩相关把这个视角留给复核人 —— 判据只能锚一个，
/// 锚 Pearson 是 DataLink 默认形态。
fn spearman(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < CORRELATION_MIN_PAIRS {
        return None;
    }
    let rx = ranks(&pairs.iter().map(|p| p.0).collect::<Vec<_>>());
    let ry = ranks(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
    let ranked: Vec<(f64, f64)> = rx.into_iter().zip(ry).collect();
    pearson(&ranked)
}

/// 相关系数分档（置信度）：<`CORRELATION_FLOOR` 不发边；[0.5,0.7) 弱 0.40；
/// [0.7,0.9) 中 0.60；≥0.9 强 0.80。判据用 |Pearson| —— 负相关同样是信号（方向记
/// evidence）；抽样口径下不给 ≥0.95（样本不是全量，与 `overlap_band` 同一纪律）。
/// 非有限值（NaN/±inf）一律 None：比较运算对 NaN 全 false，不挡会落进最高档。
fn correlation_band(abs_r: f64) -> Option<f64> {
    if !abs_r.is_finite() || abs_r < CORRELATION_FLOOR {
        None
    } else if abs_r < 0.7 {
        Some(0.40)
    } else if abs_r < 0.9 {
        Some(0.60)
    } else {
        Some(0.80)
    }
}

/// 参与相关性配对的数值列（纯函数，护栏可单测）：可采样 + numeric 桶，按 ordinal 取
/// 前 `CORRELATION_MAX_NUMERIC_COLS` 个（「前 N」必须确定 —— ordinal 是探针登记的物理序）。
fn correlation_cols<'a>(cols: &[&'a ColumnInfo]) -> Vec<&'a ColumnInfo> {
    let mut numeric: Vec<&ColumnInfo> = cols
        .iter()
        .copied()
        .filter(|c| samplable(c) && dtype_bucket(&c.data_type) == Some("numeric"))
        .collect();
    numeric.sort_by_key(|c| c.ordinal);
    numeric.truncate(CORRELATION_MAX_NUMERIC_COLS);
    numeric
}

/// 一对列的相关性判定（纯函数，样本即证据）：Pearson 过档才发边，证据记两个系数、
/// 方向、成对样本量与采样口径（`sampled` = SQL 回包行数，`pairs` = 同行同非空行数）。
fn correlated_edge(
    db: &str,
    table: &str,
    a: &ColumnInfo,
    b: &ColumnInfo,
    sampled: usize,
    row_estimate: i64,
    pairs: Vec<(f64, f64)>,
) -> Option<DataEdge> {
    let r = pearson(&pairs)?;
    let conf = correlation_band(r.abs())?;
    let col_id = |c: &ColumnInfo| {
        format!(
            "{}.{}.{}",
            db.to_ascii_lowercase(),
            table.to_ascii_lowercase(),
            c.name.to_ascii_lowercase()
        )
    };
    Some(DataEdge::with_refs(
        EdgeKind::Correlated,
        col_id(a),
        col_id(b),
        conf,
        serde_json::json!({
            "pearson": round4(r),
            "spearman": spearman(&pairs).map(round4),
            "direction": if r < 0.0 { "negative" } else { "positive" },
            "pairs": pairs.len(),
            "sampled": sampled,
            "row_estimate": row_estimate,
        }),
    ))
}

/// 一对数值列的联合采样：一条 SQL 选两列（复用 `sample_sql` 的 ident 白名单与 LIMIT），
/// 同一条 MapGate 全管道 + `PROFILE_TIMEOUT`。None = 标识符非法/闸门拒绝/执行失败/回包列
/// 对不上（与 `profile_table` 同纪律：宁可缺这对证据，不把值记错列）。
async fn sample_pair(
    mysql: &ReadOnlyMySql,
    gate: &MapGate<'_>,
    db: &str,
    table: &str,
    a: &ColumnInfo,
    b: &ColumnInfo,
) -> Option<(usize, Vec<(f64, f64)>)> {
    let sql = sample_sql(db, table, &[a, b])?;
    let scoped = match map_scoped(&sql, gate) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("datamap 相关采样被闸门拒绝 {db}.{table}({}, {}): {e}", a.name, b.name);
            return None;
        }
    };
    let rs = match mysql.fetch(&scoped, SAMPLE_ROWS, PROFILE_TIMEOUT).await {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!("datamap 相关采样失败 {db}.{table}({}, {}): {e}", a.name, b.name);
            return None;
        }
    };
    if rs.columns.len() != 2
        || !rs.columns[0].eq_ignore_ascii_case(&a.name)
        || !rs.columns[1].eq_ignore_ascii_case(&b.name)
    {
        tracing::warn!(
            "datamap 相关采样跳过 {db}.{table}({}, {})：回包列与投影不符 {:?}",
            a.name, b.name, rs.columns
        );
        return None;
    }
    let rows: Vec<(Option<String>, Option<String>)> = rs
        .rows
        .iter()
        .map(|r| (r.first().and_then(cell_value), r.get(1).and_then(cell_value)))
        .collect();
    let sampled = rows.len();
    Some((sampled, paired_numbers(&rows)))
}

/// 一张表的相关性推断：护栏内数值列两两联合采样，过档发 `correlated` 边。
/// 逐对失败 warn 跳过 —— 单对失败不拖全表（与单表失败不拖全局同纪律）。
async fn correlate_table(
    mysql: &ReadOnlyMySql,
    gate: &MapGate<'_>,
    asset: &Asset,
    row_estimate: i64,
    cols: &[&ColumnInfo],
) -> Vec<DataEdge> {
    let numeric = correlation_cols(cols);
    let mut edges = Vec::new();
    for i in 0..numeric.len() {
        for &b in numeric.iter().skip(i + 1) {
            let a = numeric[i];
            let Some((sampled, pairs)) =
                sample_pair(mysql, gate, asset.database, asset.table, a, b).await
            else {
                continue;
            };
            if let Some(e) =
                correlated_edge(asset.database, asset.table, a, b, sampled, row_estimate, pairs)
            {
                edges.push(e);
            }
        }
    }
    edges
}

// ── ③ 落库 meta.datamap_edge ───────────────────────────────────────────

/// 幂等建表（同 `warehouse_catalog::ensure_snapshot_table` 的先例：写口自确保，不依赖
/// `ddl::migrate` 的执行顺序）。🔴 本表与 server `datamap_api::DDL`（正本）/ `datamap_usage::DDL`
/// **三处逐字一致**（CREATE IF NOT EXISTS 先跑者赢，不同构就是 race）：行形是复核域的
/// left/right_table+col，本模块的 `db.table.col` 全限定名在 `split_ref` 处拆成裸表名+列名
/// 落库（目录测试保证基础表名跨库唯一，db 维度留在 evidence）。
/// status/kind 的取值集合由 CHECK 钉死，与 `EdgeKind::as_str` / 「一律 pending」互为对账。
const DATAMAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta.datamap_edge(
  id bigserial PRIMARY KEY,
  ds_id text NOT NULL DEFAULT 'dms',
  kind text NOT NULL DEFAULT 'join' CHECK (kind IN ('join','lineage','joinable','synonym','distribution_similar','co_occurs','correlated')),
  left_table text NOT NULL,
  left_col text NOT NULL DEFAULT '',
  right_table text NOT NULL,
  right_col text NOT NULL DEFAULT '',
  confidence real NOT NULL DEFAULT 0,
  evidence text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','accepted','rejected')),
  reviewed_by text NOT NULL DEFAULT '',
  reviewed_at timestamptz,
  seen_count bigint NOT NULL DEFAULT 0,
  first_seen timestamptz NOT NULL DEFAULT now(),
  last_seen timestamptz NOT NULL DEFAULT now(),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_datamap_edge_ds ON meta.datamap_edge(ds_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_datamap_edge_uniq ON meta.datamap_edge(ds_id, kind, left_table, left_col, right_table, right_col);
"#;

async fn ensure_datamap_table(pg: &PgPool) -> anyhow::Result<()> {
    for stmt in DATAMAP_DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

/// 按边唯一键 upsert。🔴 两个刻意：
/// ① 新行 status 恒 'pending'（推断边绝不直接进装配/召回，验收是人工动作）；
/// ② `DO UPDATE` 只刷 confidence/evidence/updated_at —— **status 不在 SET 列表里**，
///    人工 accepted/rejected 的结论不会被下一轮推断重跑冲掉。
const UPSERT_SQL: &str = "INSERT INTO meta.datamap_edge(ds_id, kind, left_table, left_col, right_table, right_col, confidence, evidence, status)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending')
ON CONFLICT (ds_id, kind, left_table, left_col, right_table, right_col) DO UPDATE SET
  confidence = EXCLUDED.confidence, evidence = EXCLUDED.evidence, updated_at = now()";

/// `db.table.col` 全限定名 → (裸表名, 列名)。目录测试保证基础表名跨库唯一，
/// 落库用裸表名（与 `meta.join_edge` / datamap_api 同一命名空间）；db 维度留在 evidence。
fn split_ref(s: &str) -> (String, String) {
    let mut it = s.rsplitn(2, '.');
    let col = it.next().unwrap_or_default();
    let table = it
        .next()
        .and_then(|rest| rest.rsplit('.').next())
        .unwrap_or_default();
    (table.to_string(), col.to_string())
}

/// 落库一批推断边，返回 upsert 行数。逐行执行（幂等，半截失败重跑即收敛），
/// 与 crate 内各 seed/同步写口的形态一致。
pub async fn save_edges(pg: &PgPool, ds_id: &str, edges: &[DataEdge]) -> anyhow::Result<usize> {
    ensure_datamap_table(pg).await?;
    let ds_id = ds_id.trim().to_ascii_lowercase();
    for edge in edges {
        let (lt, lc) = split_ref(&edge.src);
        let (rt, rc) = split_ref(&edge.dst);
        sqlx::query(UPSERT_SQL)
            .bind(&ds_id)
            .bind(edge.kind.as_str())
            .bind(&lt)
            .bind(&lc)
            .bind(&rt)
            .bind(&rc)
            .bind(edge.confidence)
            .bind(edge.evidence.to_string())
            .execute(pg)
            .await?;
    }
    Ok(edges.len())
}

// ── 入口 ───────────────────────────────────────────────────────────────

/// 建一遍数据地图：目录内逐表小样画像 + 同表数值列对联合采样 → 四类推断 → 全部按 pending upsert。
///
/// `snapshot` 由调用方探针（`probe_schema_with_warehouse_catalog` 的结果，本模块不二次探针 ——
/// 公网链路一次探针 ~27s，不能建图再付一遍）。目录外表、探针缺失表自动跳过。
/// 单表采样失败不拖全局（逐表 warn + 记入 `tables_skipped`）；PG 写失败是元数据层故障，Err。
pub async fn build(
    pg: &PgPool,
    mysql: &ReadOnlyMySql,
    gate: &MapGate<'_>,
    ds_id: &str,
    snapshot: &SchemaSnapshot,
) -> anyhow::Result<DataMapReport> {
    // 只读数仓红线：非数仓能力连接一律拒（fetch 在生产能力上另有 2s/50 行红线，
    // 但那道是运行时闸；画像是管理任务，连「够得着生产库」这件事本身就不许发生）。
    anyhow::ensure!(
        mysql.is_warehouse(),
        "数据地图画像只许打数仓（Doris）目标：当前连接不是数仓能力，拒绝对生产 MySQL 采样"
    );
    ensure_datamap_table(pg).await?;

    // 已验证目录是唯一画像范围：表名（小写）→ 资产（目录测试保证基础表名跨库唯一）。
    let catalog: HashMap<String, &Asset> = warehouse_catalog::ASSETS
        .iter()
        .map(|a| (a.table.to_ascii_lowercase(), a))
        .collect();
    let mut cols_by_table: HashMap<String, Vec<&ColumnInfo>> = HashMap::new();
    for (table, col) in &snapshot.columns {
        cols_by_table.entry(table.to_ascii_lowercase()).or_default().push(col);
    }

    let mut profiles: Vec<ColumnProfile> = Vec::new();
    let mut correlated_edges: Vec<DataEdge> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut tables_profiled = 0usize;
    let mut seen_tables = HashSet::new();
    for table in &snapshot.tables {
        let key = table.name.to_ascii_lowercase();
        let Some(asset) = catalog.get(&key).copied() else { continue };
        if !seen_tables.insert((asset.database, key.clone())) {
            continue;
        }
        let cols = cols_by_table.get(&key).cloned().unwrap_or_default();
        match profile_table(mysql, gate, asset, table.row_estimate, &cols).await {
            Some(cols) if !cols.is_empty() => {
                tables_profiled += 1;
                profiles.extend(cols);
            }
            _ => skipped.push((format!("{}.{}", asset.database, asset.table), "采样被拒/失败/无可采样列".to_string())),
        }
        // correlated（②b）：单列画像成败不影响两列联合采样 —— 两类证据口径独立，互不阻塞
        correlated_edges.extend(correlate_table(mysql, gate, asset, table.row_estimate, &cols).await);
    }

    let mut edges = infer_edges(&profiles);
    edges.extend(correlated_edges);
    let (mut joinable, mut synonym, mut distribution_similar, mut correlated) =
        (0usize, 0usize, 0usize, 0usize);
    for e in &edges {
        match e.kind {
            EdgeKind::Joinable => joinable += 1,
            EdgeKind::Synonym => synonym += 1,
            EdgeKind::DistributionSimilar => distribution_similar += 1,
            EdgeKind::Correlated => correlated += 1,
        }
    }
    let saved = save_edges(pg, ds_id, &edges).await?;
    Ok(DataMapReport {
        ds_id: ds_id.trim().to_ascii_lowercase(),
        tables_profiled,
        columns_profiled: profiles.len(),
        tables_skipped: skipped,
        edges: saved,
        joinable,
        synonym,
        distribution_similar,
        correlated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(db: &str, table: &str, name: &str, ty: &str, comment: &str, values: &[&str]) -> ColumnProfile {
        let cells: Vec<Option<String>> = values.iter().map(|v| Some(v.to_string())).collect();
        profile_column(
            db,
            table,
            &ColumnInfo { name: name.into(), data_type: ty.into(), comment: comment.into(), ordinal: 1 },
            1000,
            &cells,
        )
    }

    /// 单元格规范化：NULL/空串/全空白都算空，字符串去空白，数值/布尔走 JSON 形态。
    #[test]
    fn cell_value_normalizes() {
        assert_eq!(cell_value(&serde_json::Value::Null), None);
        assert_eq!(cell_value(&serde_json::json!(" 华东 ")), Some("华东".to_string()));
        assert_eq!(cell_value(&serde_json::json!("")), None, "空串算空（Doris '' 代 NULL）");
        assert_eq!(cell_value(&serde_json::json!("   ")), None);
        assert_eq!(cell_value(&serde_json::json!(42)), Some("42".to_string()));
        assert_eq!(cell_value(&serde_json::json!(true)), Some("true".to_string()));
    }

    /// 画像计数（入参是已规范化的单元格）：空值率、基数、Top 排序一次钉住。
    #[test]
    fn column_profile_counts_nulls_cardinality_and_top() {
        let p = profile_column(
            "sales_dw",
            "t_a",
            &ColumnInfo { name: "region".into(), data_type: "varchar".into(), comment: String::new(), ordinal: 1 },
            999,
            &[
                Some("华东".into()),
                Some("华东".into()),
                Some("华北".into()),
                None,
                None,
                None,
            ],
        );
        assert_eq!(p.sampled, 6);
        assert_eq!(p.nulls, 3);
        assert!((p.null_rate() - 0.5).abs() < 1e-9);
        assert_eq!(p.cardinality(), 2);
        assert_eq!(p.top_values, vec![("华东".to_string(), 2), ("华北".to_string(), 1)]);
        assert_eq!(p.id(), "sales_dw.t_a.region");
    }

    /// ① 列名规范匹配：下划线/连字符/大小写全部收敛；storecode↔customer_code 不收敛。
    #[test]
    fn normalized_name_matching() {
        for name in ["store_code", "StoreCode", "store-code", "STORECODE", "store code"] {
            assert_eq!(normalize_name(name), "storecode", "{name}");
        }
        assert_ne!(normalize_name("customer_code"), normalize_name("storecode"));
        // 规范同名不发 synonym（那个信号由 joinable 的 name_match 加成表达）
        let a = col("d", "t_a", "store_code", "varchar", "客户编码", &["C1"]);
        let b = col("d", "t_b", "storecode", "varchar", "门店编码", &["C1"]);
        assert!(synonym_confidence(&a, &b).is_none());
        // 但 joinable 的 name_match 加成必须在：同重叠率下规范同名者分更高
        let with_name = joinable_confidence(0.7, true).unwrap();
        let without_name = joinable_confidence(0.7, false).unwrap();
        assert!((with_name - without_name - 0.05).abs() < 1e-9, "{with_name} vs {without_name}");
    }

    /// ② 重叠率分档：边界逐档钉死，<0.1 不发边，名匹配加成封顶 0.95。
    #[test]
    fn overlap_rate_banding() {
        assert_eq!(overlap_band(0.09), None);
        assert_eq!(overlap_band(OVERLAP_FLOOR), Some(0.35));
        assert_eq!(overlap_band(0.29), Some(0.35));
        assert_eq!(overlap_band(0.3), Some(0.55));
        assert_eq!(overlap_band(0.59), Some(0.55));
        assert_eq!(overlap_band(0.6), Some(0.75));
        assert_eq!(overlap_band(0.89), Some(0.75));
        assert_eq!(overlap_band(0.9), Some(0.90));
        assert_eq!(overlap_band(1.0), Some(0.90));
        assert_eq!(joinable_confidence(0.05, true), None, "低于地板时名匹配救不回来");
        assert_eq!(joinable_confidence(1.0, true), Some(0.95), "0.90 + 0.05 = 0.95 不溢出");
    }

    /// ③ synonym 判分：注释一致顶替 DataLink 的 semantic_type 信号；四档逐条钉。
    #[test]
    fn synonym_scoring_bands() {
        // 注释一致 + 名字也近 → 0.95
        let a = col("d", "t_a", "cust_code", "varchar", "客户编码", &["x"]);
        let b = col("d", "t_b", "customer_code", "varchar", "客户编码", &["x"]);
        assert_eq!(synonym_confidence(&a, &b), Some(0.95));
        // 注释一致但名字不像 → 0.85（storecode↔customer_code 字符集重合度高，特意换远名）
        let far = col("d", "t_b", "kehu_id", "varchar", "客户编码", &["x"]);
        let store = col("d", "t_a", "storecode", "varchar", "客户编码", &["x"]);
        assert_eq!(synonym_confidence(&store, &far), Some(0.85));
        // 注释为空、名字高相似 → 0.6
        let c = col("d", "t_a", "customer_code", "varchar", "", &["x"]);
        let d = col("d", "t_b", "customer_id", "varchar", "", &["x"]);
        assert_eq!(synonym_confidence(&c, &d), Some(0.6));
        // 注释不同、名字中相似 → 0.4
        let e = col("d", "t_a", "shop_code", "varchar", "门店", &["x"]);
        let f = col("d", "t_b", "storecode", "varchar", "客户", &["x"]);
        assert_eq!(synonym_confidence(&e, &f), Some(0.4));
        // 名字不像、注释也不同 → 不发边
        let g = col("d", "t_a", "order_date", "date", "下单日期", &["x"]);
        let h = col("d", "t_b", "gross_profit", "decimal", "毛利", &["x"]);
        assert_eq!(synonym_confidence(&g, &h), None);
        // 注释只在一侧为空不算一致
        let i = col("d", "t_a", "amt", "decimal", "金额", &["x"]);
        let j = col("d", "t_b", "amount", "decimal", "", &["x"]);
        assert!(synonym_confidence(&i, &j).map_or(true, |c| c <= 0.6));
    }

    /// 名称相似度形态：同名 1.0、包含按长度比、否则 Jaccard/前缀（DataLink 逐字形态）。
    #[test]
    fn name_similarity_shape() {
        assert_eq!(name_similarity("store_code", "storecode"), 1.0);
        let sub = name_similarity("code", "store_code");
        assert!(sub > 0.5 && sub < 1.0, "{sub}");
        assert_eq!(name_similarity("", "x"), 0.0);
        assert!(name_similarity("order_date", "gross_profit") < 0.5);
    }

    /// 琐碎值域与类型兼容：{0,1} 列不进 joinable/distribution；编码列允许 int↔string。
    #[test]
    fn trivial_domains_and_dtype_gates() {
        let flag = col("d", "t_a", "deleted_flag", "int", "", &["0", "1", "1", "0"]);
        assert!(trivial_domain(&flag));
        let code = col("d", "t_a", "storecode", "varchar", "", &["C1", "C2"]);
        assert!(!trivial_domain(&code));
        assert!(joinable_dtypes("bigint", "varchar"), "编码列数值/字符串混存必须放行");
        assert!(joinable_dtypes("decimal(20,2)", "double"));
        assert!(!joinable_dtypes("date", "varchar"), "temporal 只跟 temporal");
        assert!(!joinable_dtypes("hll", "varchar"), "未知桶不参与");
        // {0,1} 对上全同值域也不发 distribution 边
        let flag_b = col("d", "t_b", "is_active", "int", "", &["1", "0", "1"]);
        assert_eq!(distribution_similarity(&flag, &flag_b), None);
    }

    /// ④ upsert 幂等：唯一键、(src,dst) 规范化、dedup 取最大置信度、
    /// status 恒 pending 且**不在** DO UPDATE 的 SET 列表（人工结论不被重跑冲掉）。
    #[test]
    fn upsert_is_idempotent_and_preserves_review_status() {
        // 唯一键 = (ds_id, kind, left_table, left_col, right_table, right_col)；新行 status 恒 pending
        assert!(
            DATAMAP_DDL.contains("idx_datamap_edge_uniq ON meta.datamap_edge(ds_id, kind, left_table, left_col, right_table, right_col)"),
            "{DATAMAP_DDL}"
        );
        assert!(
            UPSERT_SQL.contains("ON CONFLICT (ds_id, kind, left_table, left_col, right_table, right_col) DO UPDATE"),
            "{UPSERT_SQL}"
        );
        assert!(UPSERT_SQL.contains("'pending'"), "{UPSERT_SQL}");
        let set_clause = UPSERT_SQL.split("DO UPDATE SET").nth(1).unwrap();
        for col in ["confidence", "evidence", "updated_at"] {
            assert!(set_clause.contains(col), "重跑必须刷新 {col}：{set_clause}");
        }
        assert!(!set_clause.contains("status"), "status 绝不许被 upsert 覆盖：{set_clause}");
        // DDL 的取值集合与代码事实源对账（七值：join/lineage 是合同与血缘来源，四类推断边在这里）
        assert!(
            DATAMAP_DDL.contains("CHECK (kind IN ('join','lineage','joinable','synonym','distribution_similar','co_occurs','correlated'))"),
            "{DATAMAP_DDL}"
        );
        for kind in [EdgeKind::Joinable, EdgeKind::Synonym, EdgeKind::DistributionSimilar, EdgeKind::Correlated] {
            assert!(DATAMAP_DDL.contains(&format!("'{}'", kind.as_str())), "{DATAMAP_DDL}");
        }
        for status in ["pending", "accepted", "rejected"] {
            assert!(DATAMAP_DDL.contains(&format!("'{status}'")), "{DATAMAP_DDL}");
        }
        // 规范化 + dedup：同一对列双向发现收敛同一行，置信度取大者
        let a = col("d", "t_a", "storecode", "varchar", "", &["C1", "C2", "C3"]);
        let b = col("d", "t_b", "customer_code", "varchar", "", &["C1", "C2", "C3"]);
        let fwd = DataEdge::new(EdgeKind::Joinable, &a, &b, 0.75, serde_json::json!({}));
        let rev = DataEdge::new(EdgeKind::Joinable, &b, &a, 0.55, serde_json::json!({}));
        assert_eq!((fwd.src.as_str(), fwd.dst.as_str()), (rev.src.as_str(), rev.dst.as_str()));
        let edges = infer_edges(&[a.clone(), b.clone()]);
        let joinable: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Joinable).collect();
        assert_eq!(joinable.len(), 1, "同一无序列对只能有一行：{edges:?}");
        assert!((joinable[0].confidence - 0.90).abs() < 1e-9, "完全重叠、名不匹配应为 0.90 档：{joinable:?}");
    }

    /// `db.table.col` → (裸表名, 列名)：目录保证基础表名跨库唯一，db 维度不进唯一键
    #[test]
    fn split_ref_strips_database_prefix() {
        assert_eq!(
            split_ref("sales_dw.dws_sale.storecode"),
            ("dws_sale".to_string(), "storecode".to_string())
        );
        assert_eq!(split_ref("t_a.c1"), ("t_a".to_string(), "c1".to_string()));
        assert_eq!(split_ref("c1"), (String::new(), "c1".to_string()));
    }

    /// 端到端：storecode↔customer_code 靠值重叠+注释一致同时出 joinable 与 synonym 边；
    /// 同表列不发边；证据里带样本口径。
    #[test]
    fn infer_edges_end_to_end() {
        let store = col("sales_dw", "dws_sale", "storecode", "varchar", "客户编码", &["C1", "C2", "C3", "C4"]);
        let customer = col("dms_ods", "t_customer", "customer_code", "varchar", "客户编码", &["C1", "C2", "C3", "X5"]);
        let same_table = col("sales_dw", "dws_sale", "customer_code", "varchar", "客户编码", &["C1", "C2", "C3", "C4"]);
        let edges = infer_edges(&[store, customer, same_table]);
        // store↔customer：重叠 |{C1,C2,C3}|/min(4,4)=0.75 → 0.75 档、名不匹配无加成
        let join = edges
            .iter()
            .find(|e| e.kind == EdgeKind::Joinable && e.dst.ends_with("storecode"))
            .expect("应有 joinable 边");
        assert_eq!(join.src, "dms_ods.t_customer.customer_code");
        assert_eq!(join.dst, "sales_dw.dws_sale.storecode");
        assert!((join.confidence - 0.75).abs() < 1e-9, "0.75 重叠档、名不匹配：{join:?}");
        assert_eq!(join.evidence["overlap_rate"], serde_json::json!(0.75));
        assert_eq!(join.evidence["name_match"], serde_json::json!(false));
        // 同表同名对（customer↔same_table）规范化同名 → synonym 抑制，但 joinable 名匹配加成要在
        let named = edges
            .iter()
            .find(|e| e.kind == EdgeKind::Joinable && e.dst.ends_with("dws_sale.customer_code"))
            .expect("同名跨表对应有 joinable 边");
        assert!((named.confidence - 0.80).abs() < 1e-9, "0.75 档 + 名匹配 0.05：{named:?}");
        let syn = edges.iter().find(|e| e.kind == EdgeKind::Synonym).expect("应有 synonym 边");
        assert!((syn.confidence - 0.95).abs() < 1e-9, "注释一致+名字近：{syn:?}");
        assert!(edges.iter().all(|e| !(e.src.contains("dws_sale") && e.dst.contains("dws_sale"))),
            "同表列对不发边：{edges:?}");
    }

    /// distribution_similar：完全同分布 = 1.0；Top 零交集被乘法组合清零；基数比拉低分数。
    #[test]
    fn distribution_similarity_scoring() {
        let a = col("d", "t_a", "region", "varchar", "", &["华东", "华东", "华北", "华南"]);
        let same = col("d", "t_x", "area", "varchar", "", &["华东", "华东", "华北", "华南"]);
        let sim = distribution_similarity(&a, &same).expect("同值域同分布必须相似");
        assert!((sim - 1.0).abs() < 1e-9, "{sim}");
        // 同值域但集中度不同：加权重合 0.75，基数比 1.0 → 0.75
        let b = col("d", "t_b", "area", "varchar", "", &["华东", "华北", "华北", "华南"]);
        let sim = distribution_similarity(&a, &b).expect("值域相同仍应过地板");
        assert!((sim - 0.75).abs() < 1e-9, "{sim}");
        let disjoint = col("d", "t_c", "city", "varchar", "", &["上海", "北京", "广州", "深圳"]);
        assert_eq!(distribution_similarity(&a, &disjoint), None, "Top 零交集必须清零");
        // 不同桶不发边
        let num = col("d", "t_d", "qty", "bigint", "", &["3", "4", "5", "6"]);
        assert_eq!(distribution_similarity(&a, &num), None);
    }

    /// 采样 SQL：形态钉死 + 反引号越狱封死（与 probe.rs 同一判据）。
    #[test]
    fn sample_sql_is_capped_and_injection_closed() {
        let cols = vec![
            ColumnInfo { name: "storecode".into(), data_type: "varchar".into(), comment: String::new(), ordinal: 1 },
            ColumnInfo { name: "amount".into(), data_type: "decimal".into(), comment: String::new(), ordinal: 2 },
        ];
        let refs: Vec<&ColumnInfo> = cols.iter().collect();
        assert_eq!(
            sample_sql("sales_dw", "dws_off_offline_sale_dfn", &refs).unwrap(),
            "SELECT `storecode`, `amount` FROM `sales_dw`.`dws_off_offline_sale_dfn` LIMIT 500"
        );
        let evil =
            ColumnInfo { name: "a` FROM t_user WHERE 1=1 -- ".into(), data_type: "varchar".into(), comment: String::new(), ordinal: 1 };
        assert!(sample_sql("sales_dw", "t", &[&evil]).is_none());
        assert!(sample_sql("sales_dw", "t`x", &refs).is_none());
        assert!(sample_sql("sales`dw", "t", &refs).is_none());
        assert!(sample_sql("sales_dw", "t", &[]).is_none());
    }

    /// 敏感列与 Doris 聚合态列不进投影（敏感词表与 `is_safe_select` 同一份事实源）。
    #[test]
    fn unsamplable_columns_are_excluded() {
        for (name, ty) in [
            ("login_pwd", "varchar"),
            ("user_token_id", "varchar"), // contains 语义与守卫同款
            ("hll_uv", "hll"),
            ("bitmap_set", "bitmap"),
        ] {
            let c = ColumnInfo { name: name.into(), data_type: ty.into(), comment: String::new(), ordinal: 1 };
            assert!(!samplable(&c), "{name}/{ty} 不许进采样投影");
        }
        let ok = ColumnInfo { name: "storecode".into(), data_type: "varchar".into(), comment: String::new(), ordinal: 1 };
        assert!(samplable(&ok));
    }

    // ── ②b correlated ──

    fn col_info(name: &str, ty: &str, ordinal: i64) -> ColumnInfo {
        ColumnInfo { name: name.into(), data_type: ty.into(), comment: String::new(), ordinal }
    }

    /// 线性数据点：(x, 2x + 1) 的 Pearson 必为 1.0；(x, -2x) 必为 -1.0
    fn linear_pairs(n: usize, slope: f64) -> Vec<(f64, f64)> {
        (0..n).map(|i| { let x = i as f64; (x, slope * x + 1.0) }).collect()
    }

    /// Pearson 数值与护栏：完全正/负线性、样本不足、零方差、非有限值全钉死
    #[test]
    fn pearson_values_and_guards() {
        let pos = pearson(&linear_pairs(CORRELATION_MIN_PAIRS, 2.0)).unwrap();
        assert!((pos - 1.0).abs() < 1e-9, "{pos}");
        let neg = pearson(&linear_pairs(CORRELATION_MIN_PAIRS, -2.0)).unwrap();
        assert!((neg + 1.0).abs() < 1e-9, "{neg}");
        // 无关噪声：x 与 x%2 必不相关（r≈0）
        let noise: Vec<(f64, f64)> = (0..40).map(|i| (i as f64, (i % 2) as f64)).collect();
        let r = pearson(&noise).unwrap();
        assert!(r.abs() < 0.1, "{r}");
        // 样本不足：29 对完全线性也不发
        assert_eq!(pearson(&linear_pairs(CORRELATION_MIN_PAIRS - 1, 2.0)), None);
        // 零方差（常数列）：分母为 0 → None，不是「完全相关」
        let const_col: Vec<(f64, f64)> = (0..40).map(|i| (i as f64, 7.0)).collect();
        assert_eq!(pearson(&const_col), None);
        // 已知中间值：y = x 加一个离群点，r 显著小于 1 但为正
        let mut outlier = linear_pairs(40, 1.0);
        outlier[39] = (39.0, 0.0);
        let r = pearson(&outlier).unwrap();
        assert!(r > 0.5 && r < 1.0, "{r}");
    }

    /// Spearman：单调非线性（x 与 x²）Pearson 低估、秩相关仍 1.0；同值取平均秩
    #[test]
    fn spearman_catches_monotonic_nonlinear_and_ties() {
        let curved: Vec<(f64, f64)> = (0..40).map(|i| { let x = i as f64; (x, x * x) }).collect();
        let p = pearson(&curved).unwrap();
        let s = spearman(&curved).unwrap();
        assert!((s - 1.0).abs() < 1e-9, "严格单调秩相关必须 1.0：{s}");
        assert!(p < s, "Pearson 对曲线单调关系低估：{p} vs {s}");
        // 平均秩钉死：同值取平均秩（1 起）
        assert_eq!(ranks(&[1.0, 1.0, 2.0]), vec![1.5, 1.5, 3.0]);
        assert_eq!(ranks(&[3.0, 1.0, 2.0]), vec![3.0, 1.0, 2.0]);
        // 两段阶梯（一侧大量同值）：平均秩拉到 1.0 以下，但仍是强正相关（实测 ≈0.866）
        let tied: Vec<(f64, f64)> = (0..40).map(|i| (i as f64, if i < 20 { 1.0 } else { 2.0 })).collect();
        let s = spearman(&tied).unwrap();
        assert!(s > 0.8 && s < 1.0, "阶梯单调、同值平均秩：{s}");
        assert_eq!(spearman(&linear_pairs(CORRELATION_MIN_PAIRS - 1, 2.0)), None, "样本不足同护栏");
    }

    /// 相关分档：边界逐档钉死，低于地板不发边，天花板 0.80（样本不是全量）
    #[test]
    fn correlation_band_tiers() {
        assert_eq!(correlation_band(0.49), None);
        assert_eq!(correlation_band(CORRELATION_FLOOR), Some(0.40));
        assert_eq!(correlation_band(0.69), Some(0.40));
        assert_eq!(correlation_band(0.7), Some(0.60));
        assert_eq!(correlation_band(0.89), Some(0.60));
        assert_eq!(correlation_band(0.9), Some(0.80));
        assert_eq!(correlation_band(1.0), Some(0.80), "完全线性也不破 0.80 档");
        assert_eq!(correlation_band(f64::NAN), None, "NaN 不许落进任何档");
    }

    /// 成对提取：任一侧空/非数值/非有限都丢行；科学计数法与负数正常解析
    #[test]
    fn paired_numbers_drops_incomplete_rows() {
        let rows = vec![
            (Some("1.5".into()), Some("2.5".into())),
            (None, Some("2.0".into())),
            (Some("1.0".into()), None),
            (Some("abc".into()), Some("2.0".into())),
            (Some("NaN".into()), Some("2.0".into())),
            (Some("1.0".into()), Some("inf".into())),
            (Some("1.2e3".into()), Some("-4".into())),
        ];
        let pairs = paired_numbers(&rows);
        assert_eq!(pairs, vec![(1.5, 2.5), (1200.0, -4.0)], "{pairs:?}");
    }

    /// 数值列护栏：非数值桶/敏感列/聚合态不进；按 ordinal 取前 8 个（「前 N」是物理序）
    #[test]
    fn correlation_cols_guardrail() {
        let mut cols: Vec<ColumnInfo> = (0..10)
            .map(|i| col_info(&format!("m{i}"), "decimal", (9 - i) as i64))
            .collect();
        cols.push(col_info("region", "varchar", 100));
        cols.push(col_info("login_pwd", "decimal", 101)); // 敏感列：类型是数值也不进
        cols.push(col_info("dt", "datetime", 102));       // temporal 桶不进
        let refs: Vec<&ColumnInfo> = cols.iter().collect();
        let picked = correlation_cols(&refs);
        assert_eq!(picked.len(), CORRELATION_MAX_NUMERIC_COLS);
        assert_eq!(CORRELATION_MAX_NUMERIC_COLS, 8, "8 列 = 28 对封顶的护栏值变了要重写本测试");
        // ordinal 最小的 m9（ordinal=0）必须第一个被选中：「前 N」按物理序不是名字序
        assert_eq!(picked[0].name, "m9");
        assert!(picked.iter().all(|c| c.name.starts_with('m')), "{picked:?}");
    }

    /// correlated 边端到端：强正相关发边（证据全字段）、负相关方向记 negative、
    /// 弱相关不发边、src/dst 规范化与 upsert 键形态
    #[test]
    fn correlated_edge_end_to_end() {
        let a = col_info("gmv", "decimal", 1);
        let b = col_info("pay_amt", "decimal", 2);
        let edge = correlated_edge("SALES_DW", "DWS_SALE", &b, &a, 500, 1_000_000, linear_pairs(200, 3.0))
            .expect("完全线性必须发边");
        assert_eq!(edge.kind.as_str(), "correlated");
        // src/dst 字典序规范化（传入顺序 b,a 也收敛同一行），id 全小写
        assert_eq!(edge.src, "sales_dw.dws_sale.gmv");
        assert_eq!(edge.dst, "sales_dw.dws_sale.pay_amt");
        assert!((edge.confidence - 0.80).abs() < 1e-9, "r=1.0 落强档：{edge:?}");
        assert_eq!(edge.evidence["pearson"], serde_json::json!(1.0));
        assert_eq!(edge.evidence["spearman"], serde_json::json!(1.0));
        assert_eq!(edge.evidence["direction"], serde_json::json!("positive"));
        assert_eq!(edge.evidence["pairs"], serde_json::json!(200));
        assert_eq!(edge.evidence["sampled"], serde_json::json!(500));
        assert_eq!(edge.evidence["row_estimate"], serde_json::json!(1_000_000));
        // 负相关：同样发边，方向记 negative
        let neg = correlated_edge("d", "t", &a, &b, 100, 1000, linear_pairs(100, -1.0)).unwrap();
        assert_eq!(neg.evidence["direction"], serde_json::json!("negative"));
        assert_eq!(neg.evidence["pearson"], serde_json::json!(-1.0));
        // 弱相关（r≈0 噪声对）不发边
        let noise: Vec<(f64, f64)> = (0..100).map(|i| (i as f64, (i % 2) as f64)).collect();
        assert!(correlated_edge("d", "t", &a, &b, 100, 1000, noise).is_none());
        // 样本不足不发边
        assert!(correlated_edge("d", "t", &a, &b, 20, 1000, linear_pairs(20, 1.0)).is_none());
    }
}
