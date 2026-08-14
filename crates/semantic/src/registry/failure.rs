//! 失败经验的**读回**半：同一个坑连续踩过几次？
//!
//! `meta.failure_log` 此前全仓零 `SELECT` —— 写了没人读。后果有两层：
//! ① 用户感知到的是「同一个问法反复失败，系统学不会」；
//! ② 每一次失败都照样起一次 fast LLM 复盘（一天自动日报的 7 次重复失败 = 7 次全量复盘），
//!    而复盘产出的是同一条候选教训，去重在 `save_lesson_candidate` 里，白烧的是模型调用。
//!
//! 判据形态刻意简单：**连续次数**（同 kind + 同错误类）。第 1 次只记日志不惊动模型，
//! 第 2 次起才值得复盘 —— 一次偶发（网络抖动、超时）本来就不该沉淀成教训。
//!
//! 为什么单独一个文件：`exemplar.rs` 非测试段已逼近 D2 的 450 行，且本模块的变更原因是
//! 「失败的重复度怎么判」，与「语料怎么召回」是两件事（D3）。

use sqlx::PgPool;

/// 错误文本取前这么多字符做「同一类错误」的判据。
///
/// 取前缀而不是全等：执行错误的尾部常带行号、耗时、连接 id 这类每次都不同的噪声
/// （`Duplicate entry '12345'` / `timeout after 30.02s`），全等比对会让每次都算「新错」，
/// 判据恒返 1，等于没有这个功能。
const ERR_CLASS_CHARS: usize = 60;

/// 值得惊动模型复盘的最低连续次数。1 次是偶发（抖动/超时），不该沉淀成教训。
pub const REVIEW_STREAK: i64 = 2;

/// 同一类失败在最近 `days` 天里出现过几次（含本次之前的记录）。
///
/// 读失败返回 0：这是**增强**判据，取不到就退回「照旧复盘」的老行为，不拖垮主路。
pub async fn failure_streak(pg: &PgPool, ds: &str, kind: &str, error: &str, days: i32) -> i64 {
    let class: String = error.chars().take(ERR_CLASS_CHARS).collect();
    // ds:any 不适用 —— 失败经验按源隔离（DMS 的超时不该算进 CRM 的连续次数）
    let sql = format!(
        "SELECT count(*)::bigint FROM meta.failure_log \
         WHERE kind = $2 AND left(error, {ERR_CLASS_CHARS}) = $3 \
           AND created_at > now() - make_interval(days => $4){ds_pred}",
        ds_pred = super::ds_pred(1)
    );
    sqlx::query_scalar(&sql)
        .bind(ds)
        .bind(kind)
        .bind(&class)
        .bind(days)
        .fetch_one(pg)
        .await
        .unwrap_or_else(|e| {
            tracing::debug!(err = %e, "失败连续次数读取失败 → 按 0 处理（退回照旧复盘）");
            0
        })
}

#[cfg(test)]
mod tests {
    /// 🔴 判据必须带 ds 谓词（失败经验按源隔离）+ 用错误前缀而不是全等。
    ///
    /// 全等比对会被错误文本尾部的行号/耗时/连接 id 打散，让每次都算「新错」——
    /// 那样这个函数恒返 1，功能等于不存在。
    #[test]
    fn streak_sql_is_ds_scoped_and_classifies_by_prefix() {
        let src = include_str!("failure.rs");
        let body = src.split("pub async fn failure_streak").nth(1).expect("函数改名了");
        let sql = body.split("\n}").next().unwrap();
        assert!(sql.contains("ds_pred = super::ds_pred(1)"), "缺 ds 谓词：{sql}");
        assert!(sql.contains("left(error, {ERR_CLASS_CHARS})"), "不是按前缀分类：{sql}");
        assert!(!sql.contains("error = $"), "全等比对会被尾部噪声打散：{sql}");
    }
}
