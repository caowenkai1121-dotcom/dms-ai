//! **教训表 `meta.pitfall` 的唯一读写口**（候选落库 / 待复核清单 / 复核结论）。
//!
//! 从 `exemplar.rs` 拆出来的理由是 D2（>500 行必拆）与 D3（一个文件一个变更原因）：
//! 语料（`sql_exemplar`）与教训（`pitfall`）是两条独立的学习链 —— 语料喂 few-shot、
//! 教训喂 prompt 的「坑」段，复核口径也各不相同。拆之前两者挤在一个 600 行的文件里，
//! 改任何一条都要通读另一条。
//!
//! 落账纪律与语料侧完全一致（见 `learn.rs` 文件头）：每个写口都带 `who = (批次号, 操作者)`，
//! 状态变更**先读前值再改**，判据由 `learn_writes_are_all_ledgered` 统一扫。

use crate::registry::ds_pred;
use sqlx::PgPool;

/// 存候选教训（复盘产物）：status='candidate' 不参与召回，复核启用后生效；同 trigger+lesson 去重
pub async fn save_lesson_candidate(
    pg: &PgPool,
    // `who` = (批次号, 操作者)，见 `save_with_context`
    who: (&str, &str),
    ds: &str,
    trigger_tables: &str,
    lesson: &str,
) -> bool {
    if super::judge_mode() {
        return false;
    }
    // `RETURNING id`：账本要主键才撤得回来（同 `save_with_context`）
    let inserted: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO meta.pitfall(kind, trigger_words, lesson, status, ds_id)
         SELECT 'pitfall', $1, $2, 'candidate', $3
         WHERE NOT EXISTS (SELECT 1 FROM meta.pitfall
                           WHERE trigger_words = $1 AND lesson = $2 AND ds_id = $3)
         RETURNING id",
    )
    .bind(trigger_tables)
    .bind(lesson)
    .bind(ds)
    .fetch_optional(pg)
    .await
    .unwrap_or_else(|e| {
        // 错误谎报「已存在」会丢教训还零留痕（与 save_with_context 同一修法）
        tracing::warn!(err = %e, "候选教训落库失败（按未插入处理）");
        None
    });
    if let Some((id,)) = inserted {
        super::learn::log_event(
            pg, who.0, who.1, "meta.pitfall", &id.to_string(), "insert", None,
            Some(serde_json::json!({ "trigger": trigger_tables })),
        )
        .await;
    }
    inserted.is_some()
}

/// 待复核的候选教训 `(id, trigger_words, lesson)`。
pub async fn candidate_lessons(
    pg: &PgPool,
    limit: i64,
) -> anyhow::Result<Vec<(i64, String, String)>> {
    // 标记必须紧贴 SQL：漂移守卫的窗口只往上看 2 行（`query_log` 那次就是标记离太远假红）。
    // ds:any —— 跨源管理批处理（复核所有源的候选教训），按 id 逐条更新，不需要 ds 谓词
    Ok(sqlx::query_as(
        "SELECT id, trigger_words, lesson FROM meta.pitfall WHERE status = 'candidate' ORDER BY id LIMIT $1",
    )
    .bind(limit.max(0)) // 负 limit PG 直接报错，夹紧
    .fetch_all(pg)
    .await?)
}

/// 候选教训的复核结论：`active`（进召回）/ `disabled`
pub async fn set_lesson_status(
    pg: &PgPool,
    // `who` = (批次号, 操作者)，见 `save_with_context`
    who: (&str, &str),
    id: i64,
    status: &str,
) -> anyhow::Result<()> {
    // 账本要**前值**才撤得回来：先读旧状态，再改（两条往返换一次可回滚，值得）
    // ds:any —— 按**主键**读回自己刚要改的那一行（id 来自复核通道的可信入参，不是外部输入），
    // 与 `candidate_lessons` 的跨源批处理同一条豁免。
    let before: Option<(String,)> = sqlx::query_as("SELECT status FROM meta.pitfall WHERE id = $1")
        .bind(id)
        .fetch_optional(pg)
        .await
        .unwrap_or(None);
    if let Some((old,)) = &before {
        super::learn::log_event(
            pg, who.0, who.1, "meta.pitfall", &id.to_string(), "update",
            Some(serde_json::json!({ "status": old })),
            Some(serde_json::json!({ "status": status })),
        )
        .await;
    }
    let affected = sqlx::query("UPDATE meta.pitfall SET status = $1 WHERE id = $2")
        .bind(status)
        .bind(id)
        .execute(pg)
        .await?
        .rows_affected();
    if affected == 0 {
        tracing::warn!("候选教训复核落库 0 行（id={id} 未命中）");
    }
    Ok(())
}
