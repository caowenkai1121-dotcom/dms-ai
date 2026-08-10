//! 六种召回的统一入参 `RecallCtx` + 召回族 re-export。
//!
//! 变更原因＝召回的入参形状。六种召回今天在 `server/src/meta.rs` 各带 3~5 个 `&str`/`usize`
//! 形参（`(pg, ds, question)` / `(pg, ds, question, limit)` / `(pg, ds, question, tables, limit)`），
//! 调用点全在 `pipeline::generate_sql` 一处按同一批值逐个重排 —— 收成一个 ctx（D4）。
//!
//! `Copy`：调用点的 `limit` 今天逐条不同（表召回 6 / 元素 8 / 教训 6，`main.rs` 的冒烟是 6 / 5），
//! 用 `RecallCtx { limit: 8, ..cx }` 覆盖那一个字段即可，不必重建。
//!
//! `embed` 是**问句向量的 pgvector 字面量**而不是 embed 客户端：与上游
//! `registry::exemplar::nearest` / `datasource::nearest_datasources` 同一约定（embed 调用留在
//! 调用侧，semantic 不持 HTTP 客户端）。`None` = embed 服务缺席 → 两条向量路整段跳过，
//! 与今天 `embed_query()` 返 `None` 时的降级等价。

pub mod cards;
pub mod metric;
pub mod ods;
pub mod pitfall;
pub mod schema;

pub use cards::{
    recall_dimensions, recall_elements, recall_term_mapped, recall_terms, recall_value_domains,
    recall_value_hints, value_domain_card,
};
pub use metric::{
    alt_questions, metric_card, metric_card_for, recall_metric_hits, recall_metrics, MetricHit,
};
pub use ods::{join_evidence_edges, ods_candidate_tables, JoinEvidenceRow};
pub use pitfall::recall_pitfalls;
pub use schema::{retrieve, schema_card, schema_card_with_columns, SchemaCard, TableCtx};

/// 一次召回的全部入参。`tables` 只有教训召回读（触发词按**已召回到的表集合**匹配），
/// `limit` 的语义随召回族不同（表召回=k，元素/教训=条数上限）。
#[derive(Clone, Copy)]
pub struct RecallCtx<'a> {
    /// 用户问句原文（substring 命中判据吃它，别做归一化——顺序即行为）
    pub question: &'a str,
    /// 已召回到的物理表名（教训触发词「表名.列名」的表名部分与它比）
    pub tables: &'a [String],
    /// 条数上限 / 表召回的 k
    pub limit: usize,
    /// 数据源 id（多源总闸：每条召回 SQL 都按 `registry::ds_pred` 限定到它）
    pub ds: &'a str,
    /// 问句向量的 pgvector 字面量；`None` = embed 缺席，向量路降级跳过
    pub embed: Option<&'a str>,
    /// 【A8】问句切片向量（含整句那条在首位的 pgvector 字面量）。元素召回按
    /// 「任一片最近」取 MIN 距离 —— 整句向量被长问句稀释时，专名片段照样打得中。
    /// 空 = 只有整句向量（走单向量老路）。只有 `recall_elements` 读它。
    pub embed_slices: &'a [String],
}
