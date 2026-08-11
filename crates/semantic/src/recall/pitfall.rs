//! 教训召回。变更原因＝教训的命中判据。
//!
//! 搬运源 `server/src/meta.rs:1295-1327`（`recall_pitfalls_ds`）。本轮评审优化收紧了命中判据
//! （≥2 字门槛、分隔符补分号/顿号、表名大小写无关、「库.表.列」取中段），SQL 形状未变。
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

/// 参与召回的教训 kind。🔴 新增 kind 不进召回：加 kind 必须同步这里与种子/写口两侧。
const RECALLED_KINDS: &[&str] = &["pitfall", "routing", "column_fix"];

/// 召回 SQL（对固定参数是确定串）：进程内拼一次，不每问句重建。
fn pitfalls_sql() -> &'static str {
    static SQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SQL.get_or_init(|| {
        let kinds = RECALLED_KINDS
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "SELECT trigger_words, lesson FROM meta.pitfall
             WHERE status = 'active' AND kind IN ({kinds}){ds_pred}",
            ds_pred = ds_pred(1)
        )
    })
}

/// 触发词的表名部分命中召回表集合（裸名/限定名两形态都认，大小写不敏感）：
/// 「表名.列名」取首段；「库.表.列名」的表名是倒数第二段（首段是库名，原来误取首段永不命中）。
fn trigger_table_hit(w: &str, tables: &[String]) -> bool {
    let segs: Vec<&str> = w.split('.').collect();
    if segs.len() < 2 {
        // 裸关键词：整词与表名比（旧行为）
        return tables.iter().any(|t| t.eq_ignore_ascii_case(w));
    }
    let bare = segs[segs.len() - 2];
    let qualified = segs[..segs.len() - 1].join(".");
    tables
        .iter()
        .any(|t| t.eq_ignore_ascii_case(bare) || t.eq_ignore_ascii_case(&qualified))
}

/// 单条 trigger_words 的命中判据（纯函数，无库可单测）：任一词命中即中。
/// 命中 = 触发词（≥2 字）直接出现在问句里，或触发词的表名部分命中召回表集合。
fn trigger_matches(question: &str, tables: &[String], trig: &str) -> bool {
    trig.split([',', '，', '|', ';', '；', '、']).any(|w| {
        let w = w.trim();
        // ≥2 字门槛：单字 trigger（如「退」）contains 必中一切相关问句，
        // 与 map_filter R1「中文单字无区分度」同一纪律
        if w.chars().count() < 2 {
            return false;
        }
        // 表名全等比较（便宜）排在子串扫（贵）前面；两判据无副作用，交换零行为差
        trigger_table_hit(w, tables) || question.contains(w)
    })
}

/// 命中的口径教训（trigger 语义见模块头注；「表名.列名」锚召回表集合的语义别改）。
pub async fn recall_pitfalls(pg: &PgPool, cx: &RecallCtx<'_>) -> anyhow::Result<Vec<String>> {
    // (trigger_words, lesson)
    let rows: Vec<(String, String)> = sqlx::query_as(pitfalls_sql())
        .bind(cx.ds)
        .fetch_all(pg)
        .await?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut lessons: Vec<String> = rows
        .into_iter()
        .filter(|(trig, _)| trigger_matches(cx.question, cx.tables, trig))
        .map(|(_, lesson)| lesson)
        // 空串 lesson（`lesson text NOT NULL` 不拒 ''）不进 prompt 凑空行
        .filter(|lesson| !lesson.trim().is_empty())
        // 多条 trigger 行命中同一 lesson 文本时按文本去重（同一句话不重复进 prompt）
        .filter(|lesson| seen.insert(lesson.clone()))
        .collect();
    let hits = lessons.len();
    lessons.truncate(cx.limit);
    if hits > cx.limit {
        // 截断留痕（命中 12 取 6 这类）：调参依据，对比 cards.rs 的放宽留痕
        tracing::debug!(hits, taken = cx.limit, "教训召回按 limit 截断");
    }
    Ok(lessons)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 命中判据（纯函数）：问句直命中 / 表名锚定（裸名、限定名、大小写）/ 分隔符全集 /
    /// ≥2 字门槛 / 「库.表.列」取中段 —— 全部无库钉死。
    #[test]
    fn trigger_matches_pinned() {
        let tables = tabs(&["t_sales_order", "dws_off_offline_sale_dfn"]);
        // 问句直命中
        assert!(trigger_matches("本月销售额有多少", &[], "销售额"));
        // 表名锚定：「表名.列名」首段；大小写不敏感
        assert!(trigger_matches("随便问句", &tables, "t_sales_order.order_status"));
        assert!(trigger_matches("随便问句", &tabs(&["T_Sales_Order"]), "t_sales_order.order_status"));
        // 「库.表.列名」：表名是倒数第二段（首段库名不许误取）
        assert!(trigger_matches("随便问句", &tables, "dms_ods.t_sales_order.order_status"));
        assert!(!trigger_matches("随便问句", &tables, "dms_ods.t_other.x"));
        // 限定名形态同样认
        assert!(trigger_matches("随便问句", &tabs(&["dms_ods.t_sales_order"]), "dms_ods.t_sales_order.order_status"));
        // 分隔符全集：逗号/中文逗号/竖线/分号/中文分号/顿号
        for sep in [",", "，", "|", ";", "；", "、"] {
            let trig = format!("无关词{sep}销售额");
            assert!(trigger_matches("本月销售额", &[], &trig), "分隔符 {sep}");
        }
        // ≥2 字门槛：单字 trigger 永不命中
        assert!(!trigger_matches("本月退货率", &[], "退"));
        // 全空白词不算命中
        assert!(!trigger_matches("销售额", &[], "， "));
        // 都不沾边不中
        assert!(!trigger_matches("本月销售额", &tables, "t_goods.goods_name"));
    }
}
