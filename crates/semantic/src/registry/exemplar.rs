//! **语料与教训表的唯一读写口**（`meta.sql_exemplar` / `meta.pitfall` / 两张日志表）。
//!
//! 变更原因＝语料的读写口径。为什么必须收在这里：这些 SQL 今天散在 `pipeline.rs` 与 `meta.rs`
//! 两处，而 `ds_id`（与 F6 之后的 `visibility`）两道总闸靠 `tests/drift.rs` 扫源码守 ——
//! agent 自己写 `meta.*` 的 SQL，守卫就扫不到它。
//!
//! 搬运源 `server/src/pipeline.rs:178-191/740-769/841-859/881-888/934-946/978-983`
//! 与 `server/src/meta.rs:610-629/679-699`（SQL 文本、绑定顺序、`.ok()` 吞错位置逐行保留）。

use crate::registry::datasource::DMS_DS_ID;
use crate::registry::{ds_pred, warehouse_asset};
use sqlx::PgPool;
use std::collections::HashSet;

/// few-shot 语料：trgm 相似历史问答（返回 `(question, sql)`，最多 2 条，最相似在前）。
/// 只用人工确认且在当前只读业务源真实执行通过的高质量语料。
/// ds 谓词不可省：别的源的 SQL 当范例 = 把不存在的表名教给 LLM。
pub async fn fewshot(pg: &PgPool, ds: &str, question: &str) -> Vec<(String, String)> {
    let rows: Vec<(String, String)> = sqlx::query_as(&format!(
        "SELECT question, sql FROM meta.sql_exemplar
         WHERE question != $1 AND status = 'enabled' AND validation_status = 'valid'{ds_pred}
         ORDER BY word_similarity($1, question) DESC LIMIT 8",
        ds_pred = ds_pred(2)
    ))
    .bind(question)
    .bind(ds)
    .fetch_all(pg)
    .await
    .unwrap_or_default();
    let live = live_warehouse_tables(pg, ds).await;
    rows.into_iter()
        .filter(|(sample_question, sql)| {
            exemplar_assets_allowed(ds, sample_question, sql, &live)
        })
        .take(2)
        .collect()
}

async fn live_warehouse_tables(pg: &PgPool, ds: &str) -> HashSet<String> {
    if ds != DMS_DS_ID {
        return HashSet::new();
    }
    sqlx::query_as::<_, (String,)>(&format!(
        "SELECT table_name FROM meta.table_doc WHERE enabled{ds_pred}",
        ds_pred = ds_pred(1)
    ))
    .bind(ds)
    .fetch_all(pg)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(table,)| table.to_ascii_lowercase())
    .collect()
}

fn asked_default_sales_metrics(question: &str) -> Vec<crate::sales_fact::Metric> {
    crate::sales_fact::METRICS
        .iter()
        .copied()
        .filter(|metric| {
            question.contains(metric.name())
                || metric.aliases().iter().any(|alias| question.contains(alias))
        })
        .collect()
}

fn default_sales_sql_allowed(sql: &str, metrics: &[crate::sales_fact::Metric]) -> bool {
    let compact = sql
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '`')
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace("sf.", "");
    let contains_forbidden_column = sql
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty())
        .any(crate::registry::forbidden_default_sales_column);
    !compact.contains("count(")
        && !contains_forbidden_column
        && if metrics.is_empty() {
            !compact.contains("sum(")
        } else {
            metrics.iter().all(|metric| {
                let expression = metric
                    .expression()
                    .chars()
                    .filter(|c| !c.is_ascii_whitespace() && *c != '`')
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                compact.contains(&expression)
            })
        }
}

fn exemplar_assets_allowed(
    ds: &str,
    question: &str,
    sql: &str,
    live: &HashSet<String>,
) -> bool {
    if ds != DMS_DS_ID {
        return true;
    }
    let Ok(refs) = dms_kernel::sql::ast::table_refs_of(sql, &dms_kernel::MysqlDialect) else {
        return false;
    };
    if refs.is_empty() {
        return false;
    }
    let tables: Vec<&str> = refs
        .iter()
        .filter_map(|parts| parts.last().map(String::as_str))
        .collect();
    let valid = refs.iter().all(|parts| {
        let Some(table) = parts.last() else {
            return false;
        };
        let Some(asset) = warehouse_asset(table) else {
            return false;
        };
        parts.len() >= 2
            && parts[parts.len() - 2]
                .eq_ignore_ascii_case(crate::warehouse_catalog::database_of(asset))
            && live.contains(&table.to_ascii_lowercase())
    });
    if !valid {
        return false;
    }
    let uses_default_sales = tables
        .iter()
        .any(|table| table.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME));
    let metrics = asked_default_sales_metrics(question);
    if !metrics.is_empty()
        && (tables.len() != 1
            || !tables[0].eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME))
    {
        return false;
    }
    !uses_default_sales || default_sales_sql_allowed(sql, &metrics)
}

/// 【A15】冷启动推荐问句：**只从人工复核通过（enabled）的语料里取** ——
/// 它们既是真实问法又是验证过的正确 SQL 的问句形态，比 LLM 现编推荐
/// 多一层「问过、对过」的背书。过长的问句不上 chip（塞不下也读不动）。
/// 顺序用最新优先（业务热点会漂移，老问法未必还贴切）；取满 limit 条。
pub async fn suggest_questions(pg: &PgPool, ds: &str, limit: i64) -> Vec<String> {
    let rows: Vec<(String, String)> = sqlx::query_as(&format!(
        "SELECT question, sql FROM meta.sql_exemplar
         WHERE status = 'enabled' AND validation_status = 'valid' AND length(question) <= 40{ds_pred}
         ORDER BY id DESC LIMIT $2",
        ds_pred = ds_pred(1)
    ))
    .bind(ds)
    .bind(limit.max(0).saturating_mul(4))
    .fetch_all(pg)
    .await
    .unwrap_or_default();
    let live = live_warehouse_tables(pg, ds).await;
    rows.into_iter()
        .filter(|(question, sql)| exemplar_assets_allowed(ds, question, sql, &live))
        .map(|(question, _)| question)
        .take(limit.max(0) as usize)
        .collect()
}

/// 语料沉淀（`status=pending` 待复核）。返回是否**新插入**——调用方靠它决定要不要起复核/存向量。
pub async fn save(pg: &PgPool, ds: &str, question: &str, sql: &str) -> bool {
    save_with_context(pg, ds, question, sql, "", "").await
}

/// 【A10】同构沉淀：连当轮的 schema 段（`schema_snapshot`）与口径卡（`side_info`）一起存。
/// 存的是**渲染好的文本**（不存结构）—— few-shot 将来渲染时不需要再召回一次。
/// 空串与旧行为逐字等价（`save` 就是两个空串调过来的）。
pub async fn save_with_context(
    pg: &PgPool,
    ds: &str,
    question: &str,
    sql: &str,
    schema_snapshot: &str,
    side_info: &str,
) -> bool {
    sqlx::query(
        "INSERT INTO meta.sql_exemplar(question, sql, ds_id, schema_snapshot, side_info)
         SELECT $1, $2, $3, $4, $5
         WHERE NOT EXISTS (SELECT 1 FROM meta.sql_exemplar
                           WHERE question = $1 AND ds_id = $3)",
    )
    .bind(question)
    .bind(sql)
    .bind(ds)
    .bind(schema_snapshot)
    .bind(side_info)
    .execute(pg)
    .await
    .map(|r| r.rows_affected() > 0)
    .unwrap_or(false)
}

/// 存问句向量（供语义缓存召回）。`qvec` 是 pgvector 字面量。
pub async fn set_embedding(pg: &PgPool, ds: &str, question: &str, qvec: &str) {
    let _ = sqlx::query(
        "UPDATE meta.sql_exemplar SET embedding = $1::vector
         WHERE question = $2 AND ds_id = $3",
    )
    .bind(qvec)
    .bind(question)
    .bind(ds)
    .execute(pg)
    .await;
}

/// 复核结论落库（enabled 进 few-shot / disabled 剔除，不当范例传播）。
///
/// 🔴 本文件里**只有这一条**从 `let _ =`（吞错）改成 `Result` 传播（形状对齐
/// `set_lesson_status`）。同文件的 `set_embedding` / `log_correction` / `log_failure`
/// **刻意保留吞错**，别顺手一起改：那三条是纯观测写入（向量、纠错日志、失败日志），
/// 写失败只是少一条记录，答案该对还是对；传播会把「记日志失败」升级成「整轮问答失败」。
///
/// 这一条不是观测而是**决策**：写不进去 = 被 LLM 判 NEGATIVE 的语料没被 disable，
/// 继续当 few-shot 范例喂给下一个问句 —— 它是 二·Q「few-shot 语料在投毒」的唯一对策。
/// 吞错的实际后果（二·AS2）：PG 抖一下，`review_all_pending` 一条都没更新，
/// 却照样返回「处理了 N 条」，运维看不出复核根本没生效。
pub async fn set_status(
    pg: &PgPool,
    ds: &str,
    question: &str,
    status: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE meta.sql_exemplar SET status = $1 WHERE question = $2 AND ds_id = $3")
        .bind(status)
        .bind(question)
        .bind(ds)
        .execute(pg)
        .await?;
    Ok(())
}

/// AI 初筛只记录意见：positive 保持 pending 等人工+执行验证，negative 才禁用。
/// AI 不是业务口径授权人，不能直接把样例送进 few-shot。
pub async fn set_ai_review(
    pg: &PgPool,
    ds: &str,
    question: &str,
    opinion: &str,
) -> anyhow::Result<()> {
    let negative = opinion == "negative";
    sqlx::query(
        "UPDATE meta.sql_exemplar
         SET ai_review = $1,
             status = CASE WHEN $4 THEN 'disabled' ELSE 'pending' END,
             validation_status = CASE WHEN $4 THEN 'invalid' ELSE 'unverified' END,
             invalid_reason = CASE WHEN $4 THEN 'AI 初筛判定 SQL 不适合作为样例' ELSE '' END
         WHERE question = $2 AND ds_id = $3",
    )
    .bind(opinion)
    .bind(question)
    .bind(ds)
    .bind(negative)
    .execute(pg)
    .await?;
    Ok(())
}

/// 待复核语料 `(ds_id, question, sql)`。
/// 跨源批处理：每行带着自己的 ds_id 回来（复核是逐条的，不需要 ds 谓词）
pub async fn pending(pg: &PgPool, limit: i64) -> anyhow::Result<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT ds_id, question, sql FROM meta.sql_exemplar WHERE status = 'pending' LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pg)
    .await?)
}

/// 语义缓存的向量最近邻：最近义的一条 enabled 语料 + 余弦距离 `(question, sql, dist)`。
/// ds 谓词不可省：复用别的源的 SQL 必答错表。距离阈值与时间/数字词护栏在调用方（缓存策略）。
pub async fn nearest(
    pg: &PgPool,
    ds: &str,
    qvec: &str,
    question: &str,
) -> Option<(String, String, f64)> {
    let rows = sqlx::query_as::<_, (String, String, f64)>(&format!(
        "SELECT question, sql, (embedding <=> $1::vector) AS dist FROM meta.sql_exemplar
         WHERE status = 'enabled' AND validation_status = 'valid'
           AND embedding IS NOT NULL AND question != $2{ds_pred}
         ORDER BY embedding <=> $1::vector LIMIT 8",
        ds_pred = ds_pred(3)
    ))
    .bind(qvec)
    .bind(question)
    .bind(ds)
    .fetch_all(pg)
    .await
    .unwrap_or_default();
    let live = live_warehouse_tables(pg, ds).await;
    rows.into_iter().find(|(sample_question, sql, _)| {
        exemplar_assets_allowed(ds, sample_question, sql, &live)
    })
}

/// 存候选教训（复盘产物）：status='candidate' 不参与召回，复核启用后生效；同 trigger+lesson 去重
pub async fn save_lesson_candidate(
    pg: &PgPool,
    ds: &str,
    trigger_tables: &str,
    lesson: &str,
) -> bool {
    sqlx::query(
        "INSERT INTO meta.pitfall(kind, trigger_words, lesson, status, ds_id)
         SELECT 'pitfall', $1, $2, 'candidate', $3
         WHERE NOT EXISTS (SELECT 1 FROM meta.pitfall
                           WHERE trigger_words = $1 AND lesson = $2 AND ds_id = $3)",
    )
    .bind(trigger_tables)
    .bind(lesson)
    .bind(ds)
    .execute(pg)
    .await
    .map(|r| r.rows_affected() > 0)
    .unwrap_or(false)
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
    .bind(limit)
    .fetch_all(pg)
    .await?)
}

/// 候选教训的复核结论：`active`（进召回）/ `disabled`
pub async fn set_lesson_status(pg: &PgPool, id: i64, status: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE meta.pitfall SET status = $1 WHERE id = $2")
        .bind(status)
        .bind(id)
        .execute(pg)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_execution_validated_exemplars_are_recalled() {
        let src = include_str!("exemplar.rs");
        for fn_name in ["pub async fn fewshot", "pub async fn suggest_questions", "pub async fn nearest"] {
            let body = src.split(fn_name).nth(1).expect("召回函数被改名").split("\n///").next().unwrap();
            assert!(body.contains("status = 'enabled'") && body.contains("validation_status = 'valid'"),
                "{fn_name} 绕过 VQR：{body}");
        }
    }

    #[test]
    fn ai_review_cannot_enable_an_exemplar() {
        let src = include_str!("exemplar.rs");
        let body = src.split("pub async fn set_ai_review").nth(1).unwrap().split("\n///").next().unwrap();
        assert!(!body.contains("'enabled'"), "AI 初筛又获得直接启用权：{body}");
        assert!(body.contains("'pending'") && body.contains("'disabled'"));
    }

    #[test]
    fn dms_fewshot_cannot_reintroduce_unknown_or_old_sales_assets() {
        let live = HashSet::from([
            "dws_off_offline_sale_dfn".to_string(),
            "dws_off_third_party_sales_dnf".to_string(),
        ]);
        assert!(exemplar_assets_allowed(
            DMS_DS_ID,
            "本月销售额",
            "SELECT SUM(amount) FROM sales_dw.dws_off_offline_sale_dfn",
            &live,
        ));
        assert!(!exemplar_assets_allowed(
            DMS_DS_ID,
            "本月销售额",
            "SELECT SUM(amount) FROM dws_mkt_app_distribution_inventory_dfn",
            &live,
        ));
        assert!(!exemplar_assets_allowed(
            DMS_DS_ID,
            "本月销售额",
            "SELECT SUM(amount) FROM sales_dw.dws_off_third_party_sales_dnf",
            &live
        ));
        assert!(!exemplar_assets_allowed(DMS_DS_ID, "查询", "SELECT 1", &live));
        assert!(exemplar_assets_allowed(
            "upload_1",
            "查询",
            "SELECT * FROM up_1.t0",
            &HashSet::new()
        ));
    }
}

/// 纠错反哺日志（引擎 B+）：校正器出手即记录，供同错累计升格 pitfall（自进化，不静默修）
///
/// `trace_id` 是本轮新增（AX29）：`correction_log` / `failure_log` / `query_log` 三张表
/// 原来各记一段、拼不回同一次问答 —— 「数字错了是模型写错还是校正器改坏」查不出来。
/// 写入失败不许让问答失败（`query_log.rs` 的纪律 1）。
pub async fn log_correction(pg: &PgPool, kind: &str, question: &str, detail: &str) {
    log_correction_traced(pg, kind, question, detail, "").await;
}

/// `log_correction` 的带 `trace_id` 版。空 `trace_id` 时与上面逐字等价（老调用点不受影响）。
pub async fn log_correction_traced(pg: &PgPool, kind: &str, question: &str, detail: &str, trace_id: &str) {
    let _ = sqlx::query(
        "INSERT INTO meta.correction_log(kind, question, detail, trace_id) VALUES ($1,$2,$3,$4)",
    )
    .bind(kind)
    .bind(question.chars().take(200).collect::<String>())
    .bind(detail.chars().take(500).collect::<String>())
    .bind(if trace_id.is_empty() { None } else { Some(trace_id) })
    .execute(pg)
    .await;
}

/// 失败记录（引擎 C）：执行报错/0 行落日志，报错类供 LLM 复盘产出候选教训
pub async fn log_failure(pg: &PgPool, kind: &str, question: &str, sql: &str, error: &str) {
    log_failure_traced(pg, kind, question, sql, error, "").await;
}

/// `log_failure` 的带 `trace_id` 版。空 `trace_id` 时与上面逐字等价。
pub async fn log_failure_traced(pg: &PgPool, kind: &str, question: &str, sql: &str, error: &str, trace_id: &str) {
    let _ = sqlx::query(
        "INSERT INTO meta.failure_log(kind, question, sql, error, trace_id) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(kind)
    .bind(question.chars().take(200).collect::<String>())
    .bind(sql.chars().take(2000).collect::<String>())
    .bind(error.chars().take(500).collect::<String>())
    .bind(if trace_id.is_empty() { None } else { Some(trace_id) })
    .execute(pg)
    .await;
}
