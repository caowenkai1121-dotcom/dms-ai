//! 教训召回。变更原因＝教训的命中判据。
//!
//! 搬运源 `server/src/meta.rs:1295-1327`（`recall_pitfalls_ds`，SQL 与命中判据逐字保留）。
//!
//! 触发词形态是「表名.列名」，按**召回到的表集合**匹配（`cx.tables`）——旧库设计是把 trigger
//! 锚到会被检索到的表名上，所以「问句没提这张表但表被召回了」也算命中。这个语义别改：
//! 改成只匹配问句会让绝大多数已坐实的教训静默失效（它们的 trigger 是表名，不是人话）。
//!
//! 写侧不在这里：候选沉淀 `save_lesson_candidate` / 复核 `candidate_lessons`
//! `set_lesson_status` / 失败日志 `log_failure` / 纠错日志 `log_correction` 全在
//! `registry::exemplar`（`meta.pitfall` 的唯一写口），抽表名在 `registry::extract_tables`。

use crate::recall::RecallCtx;
use crate::registry::ds_pred;
use sqlx::PgPool;

/// 命中的口径教训。触发词形态=「表名.列名」或关键词（旧库设计：trigger 锚到会被检索到的表名上）——
/// 表名部分命中召回表集合，或触发词直接出现在问题里，均算命中。
pub async fn recall_pitfalls(pg: &PgPool, cx: &RecallCtx<'_>) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(&format!(
        "SELECT trigger_words, lesson FROM meta.pitfall
         WHERE status = 'active' AND kind IN ('pitfall','routing','column_fix'){ds_pred}",
        ds_pred = ds_pred(1)
    ))
    .bind(cx.ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(trig, _)| {
            trig.split([',', '，', '|']).any(|w| {
                let w = w.trim();
                if w.is_empty() {
                    return false;
                }
                let table_part = w.split('.').next().unwrap_or(w);
                cx.question.contains(w) || cx.tables.iter().any(|t| t == table_part)
            })
        })
        .map(|(_, lesson)| lesson)
        .take(cx.limit)
        .collect())
}
