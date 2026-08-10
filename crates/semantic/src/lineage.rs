//! 数据血缘推断：从 PG 侧元数据反推「DWS/ADS 表 ← ODS 表」的表级血缘边，upsert 进
//! `meta.datamap_edge`（`kind='lineage'`、一律 `status='pending'`、`left_col`/`right_col`
//! 留空 = 表级血缘；落库 left = 高层表、right = 源表 —— 血缘是自高层向源的推导）。
//!
//! 背景：Doris 数仓的表注释只有业务描述，ETL SQL 拿不到（SHOW CREATE VIEW 权限被拒），
//! 血缘只能靠元数据信号推断。输入全部是 PG 既有元数据（`meta.column_doc` / `meta.datamap_edge`
//! 既有统计边）+ 编译期目录（`warehouse_catalog::ASSETS`），**纯 PG 查询，不打 Doris**。
//!
//! ## 信号（按强度排序）与刻意取舍
//! 1. **目录直证 `catalog_mention`（base 0.85，单独成边）**：高层资产的 grain/metrics/
//!    forbidden 等目录文本里逐字出现源表名（词边界判定，`t_goods` 不撞 `t_goods_category`）。
//!    目录是人工维护的合同，点名来源就是最强信号。
//! 2. **列 schema 重叠（base 0.70/0.55/0.40 三档，≥2 共名列才成边）**：候选表对（跨层）
//!    在 `meta.column_doc` 里的列名重合度 = 共有列数 / min(两表有效列数)；共有列注释一致
//!    按比例加权（+0.10×比例）。技术列（id/created_time/deleted_flag…）先剔除 —— 它们在
//!    任意两表间都同名，留着只注水口径。类型桶一致率只记 evidence 不加分：ETL 常 CAST，
//!    类型漂移不是反证。单列撞名（region/order_date 这类通用维度列）不构成血缘。
//! 3. **joinable 佐证（+0.10，不单独成边）**：同表对已有高置信统计边（`kind='joinable'`
//!    且 `confidence>=0.9` 且非 rejected）—— 值级证据补强结构推断。
//! 4. **命名启发（+0.05，不单独成边）**：剥层前缀（dws_/ads_/t_…）、域前缀（off_/fin_/mkt_…）、
//!    后缀（_dfn/_dnf/_min…）并对 token 去单复数后，核心集合相同或包含
//!    （`dws_fin_customer_balance_dnf` ↔ `t_customer_balance`）。ETL 命名惯性是真信号，
//!    但撞名太易，只加权。
//! - domain 同域（+0.05，不单独成边）：同域是主题分区，不是血缘。
//! - 刻意不做：ADS←DWS / DWS←DWD 的中间层血缘本轮不落（口径只落「DWS/ADS←ODS」）；
//!   grain 维度词（客户×商品…）的 token 重合不进公式 —— 维度词太通用，噪大于信。
//!
//! ## 纪律（与 datamap.rs 同色）
//! - **不建表**：`meta.datamap_edge` 的 DDL 正本在 `server::datamap_api`（与 datamap.rs /
//!   datamap_usage.rs 三处逐字一致）。本模块只做存在性检查，缺表或缺
//!   `idx_datamap_edge_uniq` 即 Err 并提示先跑迁移。
//! - upsert 幂等（ON CONFLICT 仲裁六元组 = `idx_datamap_edge_uniq`）；**status 不进
//!   SET** —— 人工 accepted/rejected 结论不被重跑冲掉，重跑只刷
//!   confidence/evidence/updated_at。
//! - 落库用裸表名（目录测试保证基础表名跨库唯一，与 `meta.join_edge` / datamap 同一
//!   命名空间），db 维度留 evidence。
//! - 纯函数（命名规整/重叠/分档/合成/规划）全部可单测，每个阈值都有测试钉点。
//!
//! ## 接线说明（本模块不改 main.rs）
//! 编排方（CLI 管理任务或定时作业）统一接线：
//! ```ignore
//! let report = dms_semantic::lineage::build(pg, ds_id).await?;
//! let card = dms_semantic::lineage::table_relations(pg, ds_id).await?; // API 层一站式关系卡
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{json, Value};
use sqlx::PgPool;

use crate::warehouse_catalog::{self, Asset};

// ── 阈值与权重（每个都有测试钉点，改动即评审）────────────────────────────────
/// 列重叠分档：重合度 = 共有列数 / min(两表有效列数)。
const OVERLAP_STRONG: f64 = 0.5;
const OVERLAP_MID: f64 = 0.3;
const OVERLAP_WEAK: f64 = 0.15;
/// 共名列下限：单列撞名（通用维度列）不构成血缘。
const MIN_SHARED_COLS: usize = 2;
/// 目录直证：高层资产文本逐字点名源表（最强信号，单独成边）。
const MENTION_BASE: f64 = 0.85;
/// 共有列注释一致的加权上限（× 一致比例）。
const COMMENT_BONUS: f64 = 0.10;
const DOMAIN_BONUS: f64 = 0.05;
const NAMING_BONUS: f64 = 0.05;
const JOINABLE_BONUS: f64 = 0.10;
/// 置信度封顶：推断边永远给人工复核留余量，不给满分。
const CONFIDENCE_CAP: f64 = 0.95;
/// joinable 佐证门槛（既有统计边的高置信线）。
const JOINABLE_MIN: f32 = 0.9;
/// 关系卡每表每桶（stat / co_occurs）的 topN。
const RELATIONS_TOP_N: i64 = 10;

/// 技术列停用表：ETL 审计/软删列在任意两表间都同名，进重叠只注水口径。
const STOP_COLS: &[&str] = &[
    "id", "created_time", "created_by", "create_time", "creator",
    "updated_time", "updated_by", "update_time", "updater",
    "deleted_flag", "is_deleted", "tenant_id", "remark", "version", "sync_time",
];

/// `meta.column_doc` 的一行（有效注释 = custom_comment 优先、否则 col_comment，SQL 侧折叠）。
#[derive(Debug, Clone, PartialEq)]
pub struct ColMeta {
    pub name: String,
    pub data_type: String,
    pub comment: String,
}

/// 一对表的列重叠结果。`ratio` 是分档唯一依据；注释比例加权、类型比例只记证据。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overlap {
    /// 共有列名（升序，确定性产出）
    pub shared: Vec<String>,
    /// 共有列数 / min(两表有效列数)
    pub ratio: f64,
    /// 共有列中有效注释一致（双侧非空且相等）的占比
    pub comment_ratio: f64,
    /// 共有列中类型桶一致的占比（只记证据不加分）
    pub type_ratio: f64,
}

/// 一对（高层, 源）的全部输入信号（纯数据，`score_pair` 的入参）。
#[derive(Debug, Clone)]
pub struct PairSignals {
    /// 目录直证：高层资产文本逐字点名源表
    pub mention: bool,
    /// 目录 domain 完全同域
    pub domain_match: bool,
    /// 命名规整后核心 token 集合相同/包含
    pub name_match: bool,
    pub overlap: Overlap,
    /// 同表对既有高置信 joinable 边的最高置信度（无 = None）
    pub joinable_confidence: Option<f64>,
}

/// 合成结果：`base` 是出边主依据（report 按它分桶计数），`signals` 是全部命中信号（进 evidence）。
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub confidence: f64,
    pub base: &'static str,
    pub signals: Vec<&'static str>,
}

/// 一次血缘推断的产出汇总（编排方打日志/落审计用）。
#[derive(Debug)]
pub struct LineageReport {
    pub ds_id: String,
    /// 参与评估的 DWS/ADS 表数 / ODS 表数（编译期目录）
    pub high_tables: usize,
    pub ods_tables: usize,
    /// 实际跑过分档的表对数（双侧有列 schema，或目录直证免 schema）
    pub pairs_evaluated: usize,
    /// 因任一侧在 column_doc 无列而跳过的表对数
    pub pairs_skipped_no_schema: usize,
    /// 评估了但够不到出边档位的表对数
    pub skipped_below_threshold: usize,
    /// upsert 进 `meta.datamap_edge` 的边数（全部 status=pending）
    pub edges: usize,
    /// 各主依据的出边数（互斥，合计 = edges）
    pub by_catalog_mention: usize,
    pub by_overlap_strong: usize,
    pub by_overlap_mid: usize,
    pub by_overlap_weak: usize,
    /// 带高置信 joinable 佐证的边数（与上面四类重叠统计）
    pub corroborated_joinable: usize,
    /// 目录表在 column_doc 里一列都没有的清单（db.table，跳过根因留痕）
    pub tables_without_columns: Vec<String>,
}

// ── ① 纯函数：命名规整 / 目录直证 ────────────────────────────────────────────

/// 层前缀 / 域前缀 / 后缀 token（命名规整的剥除清单，只服务弱信号，清单变化需评审）。
const LAYER_TOKENS: &[&str] = &["dws", "ads", "dwd", "ods", "dim", "t"];
const DOMAIN_TOKENS: &[&str] = &["off", "mkt", "fin", "hr", "msy", "app", "winc"];
const SUFFIX_TOKENS: &[&str] = &["dfn", "dnf", "fin", "min", "scd", "d", "m"];

/// 英文 token 去单复数尾（sales→sale）；`ss`/`us` 结尾不剥（status/address 不是复数）。
fn deplural(token: &str) -> &str {
    if token.len() > 3 && token.ends_with('s') && !token.ends_with("ss") && !token.ends_with("us") {
        &token[..token.len() - 1]
    } else {
        token
    }
}

/// 表名核心 token：小写拆词 → 剥层/域前缀与后缀 → 逐 token 去单复数。
/// `dws_fin_customer_balance_dnf` 与 `t_customer_balance` 收敛同核。
fn table_core(name: &str) -> Vec<String> {
    let lowered = name.to_ascii_lowercase();
    let mut tokens: Vec<&str> = lowered.split('_').collect();
    while tokens
        .first()
        .is_some_and(|t| LAYER_TOKENS.contains(t) || DOMAIN_TOKENS.contains(t))
    {
        tokens.remove(0);
    }
    while tokens.last().is_some_and(|t| SUFFIX_TOKENS.contains(t)) {
        tokens.pop();
    }
    tokens
        .into_iter()
        .map(deplural)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// 命名启发（弱信号）：两侧核心 token 集合相同或一方包含另一方。
fn name_match(high: &str, source: &str) -> bool {
    let (a, b) = (table_core(high), table_core(source));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (sa, sb): (HashSet<&String>, HashSet<&String>) = (a.iter().collect(), b.iter().collect());
    sa.is_subset(&sb) || sb.is_subset(&sa)
}

/// 词边界包含：`t_goods` 不许撞上 `t_goods_category`（前后字符非 [a-z0-9_] 才算命中）。
fn contains_table_name(haystack: &str, table: &str) -> bool {
    let boundary = |c: char| !(c.is_ascii_alphanumeric() || c == '_');
    haystack.match_indices(table).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + table.len()..].chars().next();
        before.map_or(true, boundary) && after.map_or(true, boundary)
    })
}

/// 目录直证：高层资产的合同文本（粒度/时间/指标/禁用/比较五段）里逐字点名源表。
fn catalog_mention(high: &Asset, source_table: &str) -> bool {
    [high.grain, high.time_rule, high.metrics, high.forbidden, high.comparison]
        .iter()
        .any(|field| contains_table_name(field, source_table))
}

// ── ② 纯函数：列重叠与分档合成 ──────────────────────────────────────────────

/// 粗粒度类型桶（与 `datamap.rs::dtype_bucket` 同一规则 —— 那一个是私有的，判定逻辑保持
/// 逐字一致，散开两处是文件边界所迫，改规则要两边一起改）。`None` = 认不出的类型。
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

/// 有效列索引（去技术列，列名 → 列）。自由函数形态：闭包写不出这个生命周期签名。
fn indexed(cols: &[ColMeta]) -> HashMap<&str, &ColMeta> {
    cols.iter()
        .filter(|c| !STOP_COLS.contains(&c.name.as_str()))
        .map(|c| (c.name.as_str(), c))
        .collect()
}

/// 一对表的列重叠（纯函数）：技术列先剔除，重合度 = 共有列数 / min(两表有效列数)。
pub fn column_overlap(high: &[ColMeta], source: &[ColMeta]) -> Overlap {
    let (a, b) = (indexed(high), indexed(source));
    let mut shared: Vec<&str> = a.keys().filter(|k| b.contains_key(*k)).copied().collect();
    shared.sort_unstable();
    let min_cols = a.len().min(b.len());
    let ratio = if min_cols == 0 { 0.0 } else { shared.len() as f64 / min_cols as f64 };
    let comment_hits = shared
        .iter()
        .filter(|k| {
            let (ca, cb) = (a[*k].comment.trim(), b[*k].comment.trim());
            !ca.is_empty() && ca == cb
        })
        .count();
    let type_hits = shared
        .iter()
        .filter(|k| {
            matches!(
                (dtype_bucket(&a[*k].data_type), dtype_bucket(&b[*k].data_type)),
                (Some(x), Some(y)) if x == y
            )
        })
        .count();
    let n = shared.len() as f64;
    Overlap {
        shared: shared.into_iter().map(str::to_string).collect(),
        ratio,
        comment_ratio: if n == 0.0 { 0.0 } else { comment_hits as f64 / n },
        type_ratio: if n == 0.0 { 0.0 } else { type_hits as f64 / n },
    }
}

/// 重叠分档：≥0.5 强 0.70 / ≥0.3 中 0.55 / ≥0.15 弱 0.40 / 以下不出边（逐档测试钉死）。
fn overlap_band(ratio: f64) -> Option<(&'static str, f64)> {
    if ratio >= OVERLAP_STRONG {
        Some(("column_overlap_strong", 0.70))
    } else if ratio >= OVERLAP_MID {
        Some(("column_overlap_mid", 0.55))
    } else if ratio >= OVERLAP_WEAK {
        Some(("column_overlap_weak", 0.40))
    } else {
        None
    }
}

fn round4(x: f64) -> f64 {
    (x * 10000.0).round() / 10000.0
}

/// 合成一对候选的置信度（纯函数）。主依据只认目录直证与列重叠两档强信号；
/// 同域/命名/joinable 佐证只加权、绝不单独成边（`None` = 不出边）。
pub fn score_pair(s: &PairSignals) -> Option<Scored> {
    let band = if s.overlap.shared.len() >= MIN_SHARED_COLS {
        overlap_band(s.overlap.ratio)
    } else {
        None
    };
    let mut signals: Vec<&'static str> = Vec::new();
    if s.mention {
        signals.push("catalog_mention");
    }
    if let Some((band_name, _)) = band {
        signals.push(band_name);
    }
    let (base, mut confidence) = if s.mention {
        // 目录直证强于任何重叠档（0.85 > 0.70），直证在就是主依据
        ("catalog_mention", MENTION_BASE)
    } else if let Some((band_name, band_base)) = band {
        (band_name, band_base)
    } else {
        return None;
    };
    if !s.overlap.shared.is_empty() && s.overlap.comment_ratio > 0.0 {
        confidence += COMMENT_BONUS * s.overlap.comment_ratio;
        signals.push("comment_consistent");
    }
    if s.domain_match {
        confidence += DOMAIN_BONUS;
        signals.push("domain_match");
    }
    if s.name_match {
        confidence += NAMING_BONUS;
        signals.push("name_match");
    }
    if s.joinable_confidence.is_some() {
        confidence += JOINABLE_BONUS;
        signals.push("joinable_corroborated");
    }
    Some(Scored { confidence: round4(confidence.min(CONFIDENCE_CAP)), base, signals })
}

// ── ③ 纯函数：全候选对规划 ──────────────────────────────────────────────────

/// 一条待落库的血缘边（表级：列恒空；left = 高层表，right = 源表）。
#[derive(Debug)]
struct PlannedEdge {
    left_table: String,
    right_table: String,
    confidence: f64,
    base: &'static str,
    corroborated: bool,
    evidence: String,
}

/// 一轮规划的纯产出（PG I/O 全在 `build` 里，这里可单测）。
#[derive(Debug, Default)]
struct Plan {
    edges: Vec<PlannedEdge>,
    pairs_evaluated: usize,
    pairs_skipped_no_schema: usize,
    skipped_below_threshold: usize,
}

/// evidence JSON：主依据 + 全部命中信号 + 重叠细节 + 佐证置信度，复核者一眼能追因。
fn evidence_of(high: &Asset, source: &Asset, s: &PairSignals, scored: &Scored) -> String {
    json!({
        "source": "warehouse_catalog+column_doc",
        "base": scored.base,
        "signals": scored.signals,
        "left": { "db": high.database, "table": high.table, "layer": high.layer, "domain": high.domain },
        "right": { "db": source.database, "table": source.table, "layer": source.layer, "domain": source.domain },
        "overlap": round4(s.overlap.ratio),
        "shared_count": s.overlap.shared.len(),
        "shared_cols": s.overlap.shared.iter().take(20).collect::<Vec<_>>(),
        "comment_ratio": round4(s.overlap.comment_ratio),
        "type_ratio": round4(s.overlap.type_ratio),
        "joinable_confidence": s.joinable_confidence,
        "name_core_left": table_core(high.table),
        "name_core_right": table_core(source.table),
    })
    .to_string()
}

/// 全候选对规划（纯函数）：high × ods 逐对合成；任一侧在 column_doc 无列的表对跳过留痕
/// （目录直证例外 —— 点名来源本身就是合同，不依赖列 schema）。
fn plan_edges(
    high: &[&Asset],
    ods: &[&Asset],
    cols: &HashMap<String, Vec<ColMeta>>,
    joinable: &HashMap<(String, String), f64>,
) -> Plan {
    let mut plan = Plan::default();
    let empty: &[ColMeta] = &[];
    for h in high {
        for o in ods {
            let mention = catalog_mention(h, o.table);
            let (hc, oc) = match (cols.get(h.table), cols.get(o.table)) {
                (Some(a), Some(b)) => (a.as_slice(), b.as_slice()),
                _ if mention => (empty, empty),
                _ => {
                    plan.pairs_skipped_no_schema += 1;
                    continue;
                }
            };
            plan.pairs_evaluated += 1;
            // datamap 统计边按字典序规范化落库，这里两个方向都查（无序对）
            let joinable_confidence = joinable
                .get(&(h.table.to_string(), o.table.to_string()))
                .or_else(|| joinable.get(&(o.table.to_string(), h.table.to_string())))
                .copied();
            let signals = PairSignals {
                mention,
                domain_match: h.domain == o.domain,
                name_match: name_match(h.table, o.table),
                overlap: column_overlap(hc, oc),
                joinable_confidence,
            };
            let Some(scored) = score_pair(&signals) else {
                plan.skipped_below_threshold += 1;
                continue;
            };
            plan.edges.push(PlannedEdge {
                left_table: h.table.to_string(),
                right_table: o.table.to_string(),
                confidence: scored.confidence,
                base: scored.base,
                corroborated: signals.joinable_confidence.is_some(),
                evidence: evidence_of(h, o, &signals, &scored),
            });
        }
    }
    plan
}

// ── ④ PG 装载与落库 ─────────────────────────────────────────────────────────

/// 存在性检查（🔴 不建表：DDL 正本在 `server::datamap_api`，三处逐字一致；缺表或缺
/// 仲裁唯一索引 = 迁移没跑，直接 Err 指路，绝不自己 CREATE）。
async fn ensure_edge_table_ready(pg: &PgPool) -> anyhow::Result<()> {
    let (table,): (Option<String>,) =
        sqlx::query_as("SELECT to_regclass('meta.datamap_edge')::text").fetch_one(pg).await?;
    anyhow::ensure!(
        table.is_some(),
        "meta.datamap_edge 不存在：先跑迁移（DDL 正本 server::datamap_api::migrate），血缘边不落库"
    );
    let (index,): (Option<String>,) =
        sqlx::query_as("SELECT to_regclass('meta.idx_datamap_edge_uniq')::text").fetch_one(pg).await?;
    anyhow::ensure!(
        index.is_some(),
        "meta.idx_datamap_edge_uniq 不存在：先跑迁移（它是 upsert ON CONFLICT 的仲裁唯一索引）"
    );
    Ok(())
}

/// 目录表的列 schema（有效注释 = custom_comment 优先，SQL 侧折叠）。`ds_id DESC` = 撞名时
/// 本源行优先于 '*' 全局行（先到先得，顺序确定）。
const COLUMNS_SQL: &str = "SELECT table_name, column_name, data_type, \
     CASE WHEN custom_comment <> '' THEN custom_comment ELSE col_comment END \
     FROM meta.column_doc WHERE ds_id IN ($1, '*') AND table_name = ANY($2::text[]) \
     ORDER BY table_name, ordinal, ds_id DESC";

/// 高置信 joinable 佐证：同表对取最高置信度；rejected 是人工已否，不作证。
const JOINABLE_SQL: &str = "SELECT left_table, right_table, MAX(confidence) \
     FROM meta.datamap_edge \
     WHERE ds_id IN ($1, '*') AND kind = 'joinable' AND confidence >= $2 AND status <> 'rejected' \
     GROUP BY left_table, right_table";

/// 幂等 upsert（与 datamap.rs 同纪律）：新行 status 恒 'pending'；DO UPDATE 只刷
/// confidence/evidence/updated_at —— **status 不在 SET**，人工复核结论不被重跑冲掉。
const UPSERT_SQL: &str = "INSERT INTO meta.datamap_edge(ds_id, kind, left_table, left_col, right_table, right_col, confidence, evidence, status)
VALUES ($1, 'lineage', $2, '', $3, '', $4, $5, 'pending')
ON CONFLICT (ds_id, kind, left_table, left_col, right_table, right_col) DO UPDATE SET
  confidence = EXCLUDED.confidence, evidence = EXCLUDED.evidence, updated_at = now()";

/// 跑一遍血缘推断：目录分层 × PG 列 schema × 既有统计边佐证 → 全部按 pending upsert。
///
/// 表/索引缺失即 Err（先跑迁移）；单对评估不产生任何 PG 往返，全部输入三次查询取齐。
/// 重跑收敛：同一对表收敛同一行（六元组唯一键），只刷置信度与证据。
pub async fn build(pg: &PgPool, ds_id: &str) -> anyhow::Result<LineageReport> {
    ensure_edge_table_ready(pg).await?;
    let ds_id = ds_id.trim().to_ascii_lowercase();

    let high: Vec<&Asset> = warehouse_catalog::ASSETS
        .iter()
        .filter(|a| matches!(a.layer, "DWS" | "ADS"))
        .collect();
    let ods: Vec<&Asset> = warehouse_catalog::ASSETS
        .iter()
        .filter(|a| a.layer == "ODS")
        .collect();

    let names: Vec<String> = warehouse_catalog::ASSETS.iter().map(|a| a.table.to_string()).collect();
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(COLUMNS_SQL)
        .bind(&ds_id)
        .bind(&names)
        .fetch_all(pg)
        .await?;
    let mut cols: HashMap<String, Vec<ColMeta>> = HashMap::new();
    for (table, name, data_type, comment) in rows {
        let entry = cols.entry(table.to_ascii_lowercase()).or_default();
        if !entry.iter().any(|c: &ColMeta| c.name == name) {
            entry.push(ColMeta { name, data_type, comment });
        }
    }

    let jrows: Vec<(String, String, f32)> = sqlx::query_as(JOINABLE_SQL)
        .bind(&ds_id)
        .bind(JOINABLE_MIN)
        .fetch_all(pg)
        .await?;
    let joinable: HashMap<(String, String), f64> =
        jrows.into_iter().map(|(l, r, c)| ((l, r), f64::from(c))).collect();

    let mut tables_without_columns: Vec<String> = high
        .iter()
        .chain(ods.iter())
        .filter(|a| !cols.contains_key(a.table))
        .map(|a| format!("{}.{}", a.database, a.table))
        .collect();
    tables_without_columns.sort();

    let plan = plan_edges(&high, &ods, &cols, &joinable);
    for edge in &plan.edges {
        sqlx::query(UPSERT_SQL)
            .bind(&ds_id)
            .bind(&edge.left_table)
            .bind(&edge.right_table)
            .bind(edge.confidence)
            .bind(&edge.evidence)
            .execute(pg)
            .await?;
    }

    let mut report = LineageReport {
        ds_id,
        high_tables: high.len(),
        ods_tables: ods.len(),
        pairs_evaluated: plan.pairs_evaluated,
        pairs_skipped_no_schema: plan.pairs_skipped_no_schema,
        skipped_below_threshold: plan.skipped_below_threshold,
        edges: plan.edges.len(),
        by_catalog_mention: 0,
        by_overlap_strong: 0,
        by_overlap_mid: 0,
        by_overlap_weak: 0,
        corroborated_joinable: 0,
        tables_without_columns,
    };
    for edge in &plan.edges {
        match edge.base {
            "catalog_mention" => report.by_catalog_mention += 1,
            "column_overlap_strong" => report.by_overlap_strong += 1,
            "column_overlap_mid" => report.by_overlap_mid += 1,
            _ => report.by_overlap_weak += 1,
        }
        if edge.corroborated {
            report.corroborated_joinable += 1;
        }
    }
    tracing::info!(
        ds = %report.ds_id,
        edges = report.edges,
        pairs = report.pairs_evaluated,
        "血缘推断完成（全部 pending，待人工复核）"
    );
    Ok(report)
}

// ── ⑤ table_relations：按表聚合的一站式关系卡（纯 SELECT，供 API 层直接用）──────────

/// 合同边（已确认 join 合同，与 datamap_api 同一读取口径）。
const CONTRACTS_SQL: &str = "SELECT left_table, left_col, right_table, right_col, card, note \
     FROM meta.join_edge WHERE ds_id IN ($1, '*') AND status = 'active' ORDER BY left_table, right_table";

/// 血缘边（量小，全量取回按方向在 Rust 侧分桶）。
const LINEAGE_SQL: &str = "SELECT left_table, right_table, confidence, status \
     FROM meta.datamap_edge \
     WHERE ds_id IN ($1, '*') AND kind = 'lineage' AND status <> 'rejected' \
     ORDER BY confidence DESC, left_table, right_table";

/// 统计边 + co_occurs：左右两侧 UNION ALL 成「以每张表为视点」的行，再按视点分桶
/// （stat = joinable/synonym/distribution_similar 三合桶）窗口取 topN。rejected 不进卡。
const STAT_SQL: &str = "WITH edges AS ( \
     SELECT left_table AS pivot_table, right_table AS other_table, left_col AS pivot_col, right_col AS other_col, \
            kind, confidence, status, seen_count, \
            CASE WHEN kind = 'co_occurs' THEN 'co_occurs' ELSE 'stat' END AS bucket \
     FROM meta.datamap_edge \
     WHERE ds_id IN ($1, '*') AND status <> 'rejected' AND kind IN ('joinable','synonym','distribution_similar','co_occurs') \
     UNION ALL \
     SELECT right_table, left_table, right_col, left_col, \
            kind, confidence, status, seen_count, \
            CASE WHEN kind = 'co_occurs' THEN 'co_occurs' ELSE 'stat' END \
     FROM meta.datamap_edge \
     WHERE ds_id IN ($1, '*') AND status <> 'rejected' AND kind IN ('joinable','synonym','distribution_similar','co_occurs') \
   ) \
   SELECT pivot_table, other_table, pivot_col, other_col, kind, confidence, status, seen_count \
   FROM (SELECT edges.*, row_number() OVER ( \
             PARTITION BY pivot_table, bucket \
             ORDER BY confidence DESC, seen_count DESC, other_table, other_col) AS rn \
         FROM edges) ranked \
   WHERE rn <= $2 \
   ORDER BY pivot_table, bucket, rn";

#[derive(Default)]
struct Card {
    contracts: Vec<Value>,
    lineage_sources: Vec<Value>,
    lineage_consumers: Vec<Value>,
    stat_edges: Vec<Value>,
    co_occurs: Vec<Value>,
}

/// 按表聚合一站式关系卡（纯 SELECT，API 层直接透出）：
/// - `contracts`：合同边（`meta.join_edge` active，`side` 记本表在合同里的方向）；
/// - `lineage_sources` / `lineage_consumers`：血缘两个方向 —— 本表作为高层消费谁 /
///   作为源被谁消费（lineage 非 rejected，量小不分 topN）；
/// - `stat_edges`：joinable/synonym/distribution_similar 按 confidence 取 topN；
/// - `co_occurs`：使用轨迹边按 confidence/seen_count 取 topN。
/// 键 = 裸表名（BTreeMap，输出序确定）；空段保留空数组，前端不用判空。
pub async fn table_relations(pg: &PgPool, ds_id: &str) -> anyhow::Result<Value> {
    let ds = ds_id.trim().to_ascii_lowercase();
    let mut cards: BTreeMap<String, Card> = BTreeMap::new();

    let contracts: Vec<(String, String, String, String, String, String)> =
        sqlx::query_as(CONTRACTS_SQL).bind(&ds).fetch_all(pg).await?;
    for (lt, lc, rt, rc, card, note) in contracts {
        cards.entry(lt.clone()).or_default().contracts.push(json!({
            "side": "left", "other_table": rt.clone(), "pivot_col": lc.clone(),
            "other_col": rc.clone(), "card": card.clone(), "note": note.clone(),
        }));
        cards.entry(rt).or_default().contracts.push(json!({
            "side": "right", "other_table": lt, "pivot_col": rc,
            "other_col": lc, "card": card, "note": note,
        }));
    }

    let lineage: Vec<(String, String, f32, String)> =
        sqlx::query_as(LINEAGE_SQL).bind(&ds).fetch_all(pg).await?;
    for (lt, rt, confidence, status) in lineage {
        cards.entry(lt.clone()).or_default().lineage_sources.push(json!({
            "table": rt.clone(), "confidence": confidence, "status": status.clone(),
        }));
        cards.entry(rt).or_default().lineage_consumers.push(json!({
            "table": lt, "confidence": confidence, "status": status,
        }));
    }

    let stats: Vec<(String, String, String, String, String, f32, String, i64)> =
        sqlx::query_as(STAT_SQL).bind(&ds).bind(RELATIONS_TOP_N).fetch_all(pg).await?;
    for (pivot, other, pivot_col, other_col, kind, confidence, status, seen_count) in stats {
        let is_co_occurs = kind == "co_occurs";
        let row = json!({
            "kind": kind, "other_table": other, "pivot_col": pivot_col, "other_col": other_col,
            "confidence": confidence, "status": status, "seen_count": seen_count,
        });
        let card = cards.entry(pivot).or_default();
        if is_co_occurs {
            card.co_occurs.push(row);
        } else {
            card.stat_edges.push(row);
        }
    }

    let mut tables = serde_json::Map::new();
    for (table, card) in cards {
        tables.insert(table, json!({
            "contracts": card.contracts,
            "lineage_sources": card.lineage_sources,
            "lineage_consumers": card.lineage_consumers,
            "stat_edges": card.stat_edges,
            "co_occurs": card.co_occurs,
        }));
    }
    Ok(json!({
        "ds_id": ds,
        "top_n": RELATIONS_TOP_N,
        "table_count": tables.len(),
        "tables": Value::Object(tables),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, comment: &str) -> ColMeta {
        ColMeta { name: name.to_string(), data_type: ty.to_string(), comment: comment.to_string() }
    }

    fn asset(layer: &'static str, domain: &'static str, table: &'static str) -> Asset {
        Asset {
            database: "x_db", table, layer, domain,
            grain: "", time_rule: "", metrics: "", forbidden: "", comparison: "",
        }
    }

    fn ov(shared: &[&str], ratio: f64, comment_ratio: f64) -> Overlap {
        Overlap {
            shared: shared.iter().map(|s| s.to_string()).collect(),
            ratio, comment_ratio, type_ratio: 0.0,
        }
    }

    fn sig(mention: bool, domain: bool, naming: bool, overlap: Overlap, joinable: Option<f64>) -> PairSignals {
        PairSignals {
            mention, domain_match: domain, name_match: naming,
            overlap, joinable_confidence: joinable,
        }
    }

    /// 阈值钉点：分档线、共名列下限、各信号权重、封顶、佐证门槛、关系卡 topN。
    #[test]
    fn thresholds_are_pinned() {
        assert_eq!((OVERLAP_STRONG, OVERLAP_MID, OVERLAP_WEAK), (0.5, 0.3, 0.15));
        assert_eq!(MIN_SHARED_COLS, 2);
        assert_eq!((MENTION_BASE, COMMENT_BONUS), (0.85, 0.10));
        assert_eq!((DOMAIN_BONUS, NAMING_BONUS, JOINABLE_BONUS), (0.05, 0.05, 0.10));
        assert_eq!(CONFIDENCE_CAP, 0.95);
        assert!((JOINABLE_MIN - 0.9).abs() < 1e-6, "佐证门槛 = 0.9");
        assert_eq!(RELATIONS_TOP_N, 10);
    }

    /// 重叠分档：边界逐档钉死，0.15 以下不出边。
    #[test]
    fn overlap_band_tiers() {
        assert_eq!(overlap_band(0.5), Some(("column_overlap_strong", 0.70)));
        assert_eq!(overlap_band(1.0), Some(("column_overlap_strong", 0.70)));
        assert_eq!(overlap_band(0.49), Some(("column_overlap_mid", 0.55)));
        assert_eq!(overlap_band(0.3), Some(("column_overlap_mid", 0.55)));
        assert_eq!(overlap_band(0.299), Some(("column_overlap_weak", 0.40)));
        assert_eq!(overlap_band(0.15), Some(("column_overlap_weak", 0.40)));
        assert_eq!(overlap_band(0.149), None);
        assert_eq!(overlap_band(0.0), None);
    }

    /// 命名规整：层前缀/域前缀/后缀逐类剥除，单复数收敛；ss/us 结尾不剥。
    #[test]
    fn naming_core_normalization() {
        assert_eq!(table_core("dws_fin_customer_balance_dnf"), vec!["customer", "balance"]);
        assert_eq!(table_core("t_customer_balance"), vec!["customer", "balance"]);
        assert_eq!(table_core("t_sales_order"), vec!["sale", "order"]);
        assert_eq!(table_core("dws_off_offline_sale_dfn"), vec!["offline", "sale"]);
        assert_eq!(table_core("ads_off_region_sales_plan_min"), vec!["region", "sale", "plan"]);
        assert_eq!(table_core("t_winc_stock_report"), vec!["stock", "report"]);
        assert_eq!(deplural("status"), "status", "us 结尾不是复数");
        assert_eq!(deplural("address"), "address", "ss 结尾不是复数");
        assert_eq!(deplural("goods"), "good");
    }

    /// 命名匹配只服务弱信号：同核/包含命中，撞不上就是撞不上。
    #[test]
    fn name_match_is_subset_based() {
        assert!(name_match("dws_fin_customer_balance_dnf", "t_customer_balance"));
        assert!(name_match("dws_off_activity_promoter_fin", "t_activity_promoter_fee"),
            "[activity,promoter] ⊆ [activity,promoter,fee]");
        assert!(!name_match("dws_off_offline_sale_dfn", "t_sales_order"));
        assert!(!name_match("dws_off_activity_promoter_fin", "t_activity_main"));
        assert!(!name_match("t_winc_stock_report", "t_winc_sale_report"),
            "共享 report 一个 token 不构成包含");
    }

    /// 目录直证的词边界：t_goods 不撞 t_goods_category；目录五段文本任一段点名即命中。
    #[test]
    fn mention_uses_word_boundaries() {
        assert!(contains_table_name("数据来自 t_sales_order 汇总", "t_sales_order"));
        assert!(contains_table_name("t_goods", "t_goods"));
        assert!(contains_table_name("（t_goods）", "t_goods"));
        assert!(!contains_table_name("见 t_goods_category 表", "t_goods"));
        assert!(!contains_table_name("at_sales_orderx", "t_sales_order"));
        let mut a = asset("DWS", "商品", "dws_x_goods_dnf");
        a.metrics = "商品结构，来源 t_goods 每日同步";
        assert!(catalog_mention(&a, "t_goods"));
        assert!(!catalog_mention(&a, "t_sales_order"));
        let plain = asset("DWS", "线下销售", "dws_off_offline_sale_dfn");
        assert!(!catalog_mention(&plain, "t_sales_order"), "空文本绝不直证");
    }

    /// 列重叠：技术列先剔除（两侧 id/created_time/remark 不计），重合度 = 共有/min(有效列数)。
    #[test]
    fn overlap_excludes_stop_columns() {
        let high = vec![
            col("id", "bigint", ""), col("created_time", "datetime", ""),
            col("order_date", "date", "日期"), col("storecode", "varchar", "客户编码"),
            col("amount", "decimal", "金额"),
        ];
        let source = vec![
            col("id", "bigint", ""), col("created_time", "datetime", ""), col("remark", "varchar", ""),
            col("order_date", "date", "下单日期"), col("storecode", "varchar", "客户编码"),
            col("amount", "decimal", "金额"), col("qty", "decimal", "数量"),
        ];
        let o = column_overlap(&high, &source);
        assert_eq!(o.shared, vec!["amount", "order_date", "storecode"], "技术列不进共有");
        assert!((o.ratio - 1.0).abs() < 1e-9, "3 / min(3, 4) = 1.0：{}", o.ratio);
        // order_date 两侧注释不同（下单日期≠日期）→ 2/3 一致
        assert!((o.comment_ratio - 2.0 / 3.0).abs() < 1e-9, "{}", o.comment_ratio);
        assert!((o.type_ratio - 1.0).abs() < 1e-9, "三列类型桶全同");
        let empty = column_overlap(&[], &source);
        assert_eq!((empty.shared.len(), empty.ratio), (0, 0.0), "空表不除零");
    }

    /// 注释一致加权：一侧空注释不算一致；类型不一致只影响证据不进置信度。
    #[test]
    fn comment_weights_and_type_is_evidence_only() {
        let high = vec![
            col("storecode", "varchar", "客户编码"), col("amount", "decimal", "金额"),
        ];
        let source = vec![
            col("storecode", "varchar", "客户编码"), col("amount", "decimal", ""),
        ];
        let o = column_overlap(&high, &source);
        assert!((o.comment_ratio - 0.5).abs() < 1e-9, "一侧空注释不算一致");
        // 类型漂移（varchar→bigint）拉低 type_ratio，但 score_pair 根本不读它
        let drifted = vec![col("storecode", "bigint", "客户编码"), col("amount", "decimal", "")];
        let o2 = column_overlap(&high, &drifted);
        assert!((o2.type_ratio - 0.5).abs() < 1e-9);
        let s1 = score_pair(&sig(false, false, false, o.clone(), None)).unwrap();
        let s2 = score_pair(&sig(false, false, false, o2, None)).unwrap();
        assert_eq!(s1.confidence, s2.confidence, "类型比例不进置信度公式");
    }

    /// 合成门禁：目录直证/列重叠单独成边；同域、命名、joinable 佐证**单独都不成边**。
    #[test]
    fn score_gating() {
        // 目录直证单独成边 0.85
        let s = score_pair(&sig(true, false, false, ov(&[], 0.0, 0.0), None)).unwrap();
        assert_eq!((s.confidence, s.base), (0.85, "catalog_mention"));
        assert_eq!(s.signals, vec!["catalog_mention"]);
        // 命名/同域/佐证/低于地板的重叠，单独全部不出边
        assert!(score_pair(&sig(false, false, true, ov(&[], 0.0, 0.0), None)).is_none(), "命名单独不成边");
        assert!(score_pair(&sig(false, true, false, ov(&[], 0.0, 0.0), None)).is_none(), "同域单独不成边");
        assert!(score_pair(&sig(false, false, false, ov(&[], 0.0, 0.0), Some(0.95))).is_none(), "佐证单独不成边");
        assert!(score_pair(&sig(false, true, true, ov(&["a", "b"], 0.149, 0.0), Some(0.95))).is_none(),
            "三弱叠加也不许够到出边线");
        // 共名列 < 2 即使全撞也不成边
        assert!(score_pair(&sig(false, false, false, ov(&["region"], 1.0, 1.0), None)).is_none(),
            "单列撞名不构成血缘");
    }

    /// 置信度堆叠与封顶：强重叠 + 注释满分 + 同域 + 命名 + 佐证 = 1.00 → 封顶 0.95。
    #[test]
    fn score_stacking_and_cap() {
        let full = score_pair(&sig(false, true, true, ov(&["a", "b", "c"], 0.6, 1.0), Some(0.92))).unwrap();
        assert_eq!(full.base, "column_overlap_strong");
        assert!((full.confidence - 0.95).abs() < 1e-9, "0.70+0.10+0.05+0.05+0.10=1.00 → 封顶：{}", full.confidence);
        for s in ["column_overlap_strong", "comment_consistent", "domain_match", "name_match", "joinable_corroborated"] {
            assert!(full.signals.contains(&s), "缺信号 {s}");
        }
        // 中档裸边 0.55；弱档 + 佐证 0.50
        let mid = score_pair(&sig(false, false, false, ov(&["a", "b"], 0.4, 0.0), None)).unwrap();
        assert!((mid.confidence - 0.55).abs() < 1e-9);
        let weak = score_pair(&sig(false, false, false, ov(&["a", "b"], 0.2, 0.0), Some(0.9))).unwrap();
        assert!((weak.confidence - 0.50).abs() < 1e-9, "0.40+0.10：{}", weak.confidence);
        // 直证 + 注释一致（共名列不足 2 也加权，因为注释一致性本身是证据）
        let m = score_pair(&sig(true, false, false, ov(&["a"], 0.1, 1.0), None)).unwrap();
        assert!((m.confidence - 0.95).abs() < 1e-9, "0.85+0.10=0.95：{}", m.confidence);
    }

    /// upsert 纪律（镜像 datamap.rs）：六元组仲裁、kind 钉 'lineage'、新行恒 pending、
    /// status 绝不在 SET 列表（人工结论不被重跑冲掉）。
    #[test]
    fn upsert_is_idempotent_and_preserves_review_status() {
        assert!(
            UPSERT_SQL.contains("ON CONFLICT (ds_id, kind, left_table, left_col, right_table, right_col) DO UPDATE"),
            "{UPSERT_SQL}"
        );
        assert!(UPSERT_SQL.contains("'lineage'") && UPSERT_SQL.contains("'pending'"), "{UPSERT_SQL}");
        let set_clause = UPSERT_SQL.split("DO UPDATE SET").nth(1).unwrap();
        for col in ["confidence", "evidence", "updated_at"] {
            assert!(set_clause.contains(col), "重跑必须刷新 {col}：{set_clause}");
        }
        assert!(!set_clause.contains("status"), "status 绝不许被 upsert 覆盖：{set_clause}");
        // 表级血缘：列恒空
        assert!(UPSERT_SQL.contains("$2, '', $3, ''"), "{UPSERT_SQL}");
    }

    /// 端到端规划：方向（left=高层）、分档计数、schema 缺失跳过分三类留痕。
    #[test]
    fn plan_edges_end_to_end() {
        let h_balance = asset("DWS", "客户余额", "dws_fin_customer_balance_dnf");
        let h_report = asset("DWS", "报表", "dws_x_report_dnf");
        let mut h_goods = asset("DWS", "商品分析", "dws_y_sale_analyze_dnf");
        h_goods.metrics = "商品销售结构，来源 t_goods 每日同步";
        let high = [&h_balance, &h_report, &h_goods];
        let o_balance = asset("ODS", "客户余额", "t_customer_balance");
        let o_goods = asset("ODS", "商品主数据", "t_goods");
        let ods = [&o_balance, &o_goods];

        let mut cols: HashMap<String, Vec<ColMeta>> = HashMap::new();
        cols.insert("dws_fin_customer_balance_dnf".to_string(), vec![
            col("data_date", "varchar", "日期"), col("customer_code", "varchar", "客户编码"),
            col("balance_type", "varchar", "余额类型"), col("balance_amount", "decimal", "余额"),
        ]);
        cols.insert("t_customer_balance".to_string(), vec![
            col("customer_code", "varchar", "客户编码"), col("balance_type", "varchar", "余额类型"),
            col("balance_amount", "decimal", "余额"),
            col("id", "bigint", ""), col("created_time", "datetime", ""),
        ]);
        cols.insert("dws_x_report_dnf".to_string(), vec![
            col("report_date", "date", "日期"), col("region", "varchar", "省区"),
        ]);
        // t_goods 与 dws_y_sale_analyze_dnf 刻意不进 column_doc：验证 schema 缺失路径

        let mut joinable: HashMap<(String, String), f64> = HashMap::new();
        // datamap 落库按字典序（dws_... < t_...），规划两个方向都要查得到
        joinable.insert(("dws_fin_customer_balance_dnf".to_string(), "t_customer_balance".to_string()), 0.92);

        let plan = plan_edges(&high, &ods, &cols, &joinable);
        // 6 对：balance×balance（出边）+ report×balance（无重叠）+ goods×goods（直证免 schema 出边）
        //       + balance×goods / report×goods / goods×balance（任一侧无列且无直证 → no_schema）
        assert_eq!(plan.pairs_evaluated, 3, "{plan:?}");
        assert_eq!(plan.pairs_skipped_no_schema, 3, "任一侧无列且无直证的对跳过：{plan:?}");
        assert_eq!(plan.edges.len(), 2, "{plan:?}");

        let e = plan.edges.iter().find(|e| e.right_table == "t_customer_balance").unwrap();
        assert_eq!(e.left_table, "dws_fin_customer_balance_dnf", "left = 高层表");
        assert_eq!(e.base, "column_overlap_strong", "3/min(4,3)=1.0：{e:?}");
        assert!(e.corroborated);
        // 0.70 + 注释满分 0.10 + 同域 0.05 + 命名 0.05 + 佐证 0.10 = 1.00 → 封顶 0.95
        assert!((e.confidence - 0.95).abs() < 1e-9, "{e:?}");

        let m = plan.edges.iter().find(|e| e.right_table == "t_goods").unwrap();
        assert_eq!((m.left_table.as_str(), m.base), ("dws_y_sale_analyze_dnf", "catalog_mention"));
        assert!((m.confidence - 0.85).abs() < 1e-9, "目录直证免列 schema 单独成边：{m:?}");
        assert!(!m.corroborated);

        assert_eq!(plan.skipped_below_threshold, 1, "report×balance 无重叠：{plan:?}");
    }

    /// evidence 形状：主依据/信号/重叠细节/佐证齐全，shared_cols 截断 20 条。
    #[test]
    fn evidence_records_all_signals() {
        let h = asset("DWS", "客户余额", "dws_fin_customer_balance_dnf");
        let o = asset("ODS", "客户余额", "t_customer_balance");
        let shared: Vec<String> = (0..25).map(|i| format!("c{i}")).collect();
        let signals = PairSignals {
            mention: false, domain_match: true, name_match: true,
            overlap: Overlap { shared, ratio: 0.6, comment_ratio: 0.5, type_ratio: 0.8 },
            joinable_confidence: Some(0.92),
        };
        let scored = score_pair(&signals).unwrap();
        let ev: Value = serde_json::from_str(&evidence_of(&h, &o, &signals, &scored)).unwrap();
        assert_eq!(ev["base"], json!("column_overlap_strong"));
        assert_eq!(ev["shared_count"], json!(25));
        assert_eq!(ev["shared_cols"].as_array().unwrap().len(), 20, "证据截断 20 条");
        assert_eq!(ev["overlap"], json!(0.6));
        assert_eq!(ev["joinable_confidence"], json!(0.92));
        assert_eq!(ev["left"]["layer"], json!("DWS"));
        assert_eq!(ev["right"]["layer"], json!("ODS"));
        assert!(ev["signals"].as_array().unwrap().contains(&json!("joinable_corroborated")));
        // 无佐证时证据里显式为 null（复核者分得清「没查」与「没有」）
        let no = score_pair(&sig(false, false, false, ov(&["a", "b"], 0.4, 0.0), None)).unwrap();
        let ev2: Value = serde_json::from_str(&evidence_of(&h, &o, &sig(false, false, false, ov(&["a", "b"], 0.4, 0.0), None), &no)).unwrap();
        assert_eq!(ev2["joinable_confidence"], Value::Null);
    }
}
