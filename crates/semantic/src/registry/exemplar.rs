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
/// few-shot 的相似度下限。
///
/// 没有它时 `ORDER BY … DESC LIMIT 8` 会把**完全不相干**的语料当 top2 塞进 prompt
/// （问句只要非空就一定有人中选）——「报销政策是什么」也会被配上两条销售额 SQL 示例，
/// 模型被示例带着往取数上写。示例的作用是「同类问法长这样」，不同类就不该出现。
///
/// 0.15 是保守值：中文 trigram 的 `word_similarity` 普遍偏低（同族问法常在 0.3-0.6），
/// 这条闸只砍「一个词都不沾」的那批。调它要连带看 `tools/regression.py` 的 LLM 路题。
const FEWSHOT_MIN_SIMILARITY: f32 = 0.15;

pub async fn fewshot(pg: &PgPool, ds: &str, question: &str) -> Vec<(String, String)> {
    let rows: Vec<(String, String)> = sqlx::query_as(&format!(
        "SELECT question, sql FROM meta.sql_exemplar
         WHERE question != $1 AND status = 'enabled' AND validation_status = 'valid'
           AND word_similarity($1, question) >= {FEWSHOT_MIN_SIMILARITY}{ds_pred}
         ORDER BY word_similarity($1, question) DESC LIMIT 8",
        ds_pred = ds_pred(2)
    ))
    .bind(question)
    .bind(ds)
    .fetch_all(pg)
    .await
    .map_err(|e| tracing::warn!(err = %e, "few-shot 语料读取失败 → 本轮无 few-shot"))
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
    // 读失败 → 空集 → DMS 语料全被过滤光（静默 fail-closed）：必须留痕
    .map_err(|e| tracing::warn!(err = %e, "活性表清单读取失败 → DMS 语料过滤按空集（全过滤光）"))
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
    // compact 规范化与 `registry::compact_contract_expr` 同一份（不开第二份拷贝）；
    // `sf.` 别名只剥前缀位置（`asf.` 子串不误伤）
    let compact = crate::registry::strip_sf_alias(&crate::registry::compact_contract_expr(sql));
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
                compact_metric_expressions(*metric)
                    .iter()
                    .any(|form| compact.contains(form))
            })
        }
}

/// 各默认销售指标表达式的 compact 形态：METRICS 是静态表，进程内只算一次
/// （原来每次调用对每个指标重算一遍 compact）。
/// 各默认销售指标表达式的 compact 形态：METRICS 是静态表，进程内只算一次
/// （原来每次调用对每个指标重算一遍 compact）。新旧两形都产：合同表达式
/// 2026-08-11 起包 COALESCE(…,0)，人工/LLM 写的裸 SUM 样例是同一口径，不许因此拒收。
fn compact_metric_expressions(metric: crate::sales_fact::Metric) -> &'static [String] {
    static EXPRS: std::sync::LazyLock<Vec<Vec<String>>> = std::sync::LazyLock::new(|| {
        crate::sales_fact::METRICS
            .iter()
            .map(|m| {
                let compact = crate::registry::compact_contract_expr(m.expression());
                let legacy = crate::sales_fact::legacy_contract_form(&compact);
                if legacy != compact {
                    vec![compact, legacy]
                } else {
                    vec![compact]
                }
            })
            .collect()
    });
    let i = crate::sales_fact::METRICS
        .iter()
        .position(|m| *m == metric)
        .expect("metric 出自 METRICS");
    &EXPRS[i]
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
    // 单趟完成「收集末段表名 + 逐条校验」（原来 tables 先收集一遍、valid 又逐条重算 parts.last()）
    let mut tables: Vec<&str> = Vec::with_capacity(refs.len());
    let valid = refs.iter().all(|parts| {
        let Some(table) = parts.last().map(String::as_str) else {
            return false;
        };
        let Some(asset) = warehouse_asset(table) else {
            return false;
        };
        let ok = parts.len() >= 2
            && parts[parts.len() - 2]
                .eq_ignore_ascii_case(crate::warehouse_catalog::database_of(asset))
            && live.contains(&table.to_ascii_lowercase());
        if ok {
            tables.push(table);
        }
        ok
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
    .map_err(|e| tracing::warn!(err = %e, "推荐问句语料读取失败 → 本轮无推荐"))
    .unwrap_or_default();
    let live = live_warehouse_tables(pg, ds).await;
    rows.into_iter()
        .filter(|(question, sql)| exemplar_assets_allowed(ds, question, sql, &live))
        .map(|(question, _)| question)
        .take(limit.max(0) as usize)
        .collect()
}

/// 语料沉淀（`status=pending` 待复核）。返回是否**新插入**——调用方靠它决定要不要起复核/存向量。
pub async fn save(pg: &PgPool, who: (&str, &str), ds: &str, question: &str, sql: &str) -> bool {
    save_with_context(pg, who, ds, question, sql, "", "").await
}

/// 【A10】同构沉淀：连当轮的 schema 段（`schema_snapshot`）与口径卡（`side_info`）一起存。
/// 存的是**渲染好的文本**（不存结构）—— few-shot 将来渲染时不需要再召回一次。
/// 空串与旧行为逐字等价（`save` 就是两个空串调过来的）。
pub async fn save_with_context(
    pg: &PgPool,
    // `who` = (批次号, 操作者)：批次粒度钉死为「一轮问答 / 一次复核批」，不是会话。
    // 🔴 空批次号写进账本 = **这条学习永远撤不回来**：`recent_batches` 的谓词是
    // `batch_id <> ''`（管理员那张台账此前恒空），而 rollback 传空串又能撤光全部历史。
    who: (&str, &str),
    ds: &str,
    question: &str,
    sql: &str,
    schema_snapshot: &str,
    side_info: &str,
) -> bool {
    // 判官模式：观察系统不许改变系统（见 `registry::judge_mode`）
    if super::judge_mode() {
        return false;
    }
    // `RETURNING id`：账本要记下这一条的主键才撤得回来（`rows_affected` 撤不了具体哪一行）
    let inserted: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO meta.sql_exemplar(question, sql, ds_id, schema_snapshot, side_info)
         SELECT $1, $2, $3, $4, $5
         WHERE NOT EXISTS (SELECT 1 FROM meta.sql_exemplar
                           WHERE question = $1 AND ds_id = $3)
         RETURNING id",
    )
    .bind(question)
    .bind(sql)
    .bind(ds)
    .bind(schema_snapshot)
    .bind(side_info)
    .fetch_optional(pg)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(err = %e, "语料沉淀失败（按未插入处理）");
        None
    });
    if let Some((id,)) = inserted {
        super::learn::log_event(
            pg, who.0, who.1, "meta.sql_exemplar", &id.to_string(), "insert", None,
            Some(serde_json::json!({ "question": question })),
        )
        .await;
    }
    inserted.is_some()
}

/// 存问句向量（供语义缓存召回）。`qvec` 是 pgvector 字面量。
pub async fn set_embedding(pg: &PgPool, ds: &str, question: &str, qvec: &str) {
    // 刻意不传播（纯观测写入，见 set_status 的 doc 对比），但失败要留痕：
    // 非法 qvec（`$1::vector` 解析错）/连接抖动，debug 一次
    if let Err(e) = sqlx::query(
        "UPDATE meta.sql_exemplar SET embedding = $1::vector
         WHERE question = $2 AND ds_id = $3",
    )
    .bind(qvec)
    .bind(question)
    .bind(ds)
    .execute(pg)
    .await
    {
        tracing::debug!(err = %e, "语料向量写回失败（观测写入，不传播）");
    }
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
    // `who` = (批次号, 操作者)，见 `save_with_context`
    who: (&str, &str),
    ds: &str,
    question: &str,
    status: &str,
) -> anyhow::Result<()> {
    ledger_status_change(pg, who, ds, question, status).await;
    let affected = sqlx::query("UPDATE meta.sql_exemplar SET status = $1 WHERE question = $2 AND ds_id = $3")
        .bind(status)
        .bind(question)
        .bind(ds)
        .execute(pg)
        .await?
        .rows_affected();
    if affected == 0 {
        // question 打错/已被删时静默 no-op 仍 Ok：留痕（复核回路空转的排查依据）
        tracing::warn!("语料复核落库 0 行（question 未命中）：{question:?}");
    }
    Ok(())
}

/// 语料状态变更的落账（两个写口共用）。**读前值 → 记一条 → 调用方再改**。
///
/// 读不到（问句打错/已删）就不记：账本里一条没有前值的 update 撤不回来，
/// 记了反而给回滚一条假线索。落账失败只 warn —— 账本是观测面，学习是主路（模块头纪律）。
async fn ledger_status_change(pg: &PgPool, who: (&str, &str), ds: &str, question: &str, after: &str) {
    // ds:any —— 按 (question, ds_id) 读回自己下一句就要改的那一行
    let before: Option<(String,)> =
        sqlx::query_as("SELECT status FROM meta.sql_exemplar WHERE question = $1 AND ds_id = $2")
            .bind(question)
            .bind(ds)
            .fetch_optional(pg)
            .await
            .unwrap_or(None);
    let Some((old, )) = before else { return };
    super::learn::log_event(
        pg, who.0, who.1, "meta.sql_exemplar", question, "update",
        Some(serde_json::json!({ "status": old })),
        Some(serde_json::json!({ "status": after })),
    )
    .await;
}

/// AI 初筛只记录意见：positive 保持 pending 等人工+执行验证，negative 才禁用。
/// AI 不是业务口径授权人，不能直接把样例送进 few-shot。
pub async fn set_ai_review(
    pg: &PgPool,
    // `who` = (批次号, 操作者)，见 `save_with_context`
    who: (&str, &str),
    ds: &str,
    question: &str,
    opinion: &str,
) -> anyhow::Result<()> {
    // 入参白名单：`"negativ"` 这类 typo 不许静默归 positive 侧（按 pending 放行）
    anyhow::ensure!(matches!(opinion, "positive" | "negative"), "未知 AI 复核意见 {opinion:?}");
    let negative = opinion == "negative";
    // 账本要**前值**才撤得回来（同 `set_lesson_status`：两条往返换一次可回滚，值得）。
    // AI 初筛会把语料打成 disabled —— 判错了得能一键撤回，否则只能人工去库里翻。
    ledger_status_change(pg, who, ds, question, if negative { "disabled" } else { "pending" }).await;
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
    .bind(limit.max(0)) // 负 limit PG 直接报错，夹紧
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
    .map_err(|e| tracing::warn!(err = %e, "语义缓存最近邻读取失败 → 本轮缓存 miss"))
    .unwrap_or_default();
    let live = live_warehouse_tables(pg, ds).await;
    rows.into_iter().find(|(sample_question, sql, _)| {
        exemplar_assets_allowed(ds, sample_question, sql, &live)
    })
}

/// 纠错反哺日志（引擎 B+）：校正器出手即记录，供同错累计升格 pitfall（自进化，不静默修）
///
/// `trace_id` 是本轮新增（AX29）：`correction_log` / `failure_log` / `query_log` 三张表
/// 原来各记一段、拼不回同一次问答 —— 「数字错了是模型写错还是校正器改坏」查不出来。
/// 写入失败不许让问答失败（`query_log.rs` 的纪律 1）。
/// （原不带 trace_id 的 `log_correction` 包装已删：全仓调用点全走 `_traced` 版。）
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

/// 失败记录（引擎 C）：执行报错/0 行落日志，报错类供 LLM 复盘产出候选教训。
/// （原不带 trace_id 的 `log_failure` 包装已删：全仓调用点全走 `_traced` 版。）
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_execution_validated_exemplars_are_recalled() {
        let src = include_str!("exemplar.rs");
        // 🔴 few-shot 必须有相似度下限：没有它，问句只要非空就一定有两条语料被塞进 prompt，
        // 「报销政策是什么」也会被配上销售额 SQL 示例，模型被示例带着往取数上写（2026-08-13 审计）。
        let fewshot_body = src.split("pub async fn fewshot").nth(1).unwrap();
        assert!(
            fewshot_body.contains("word_similarity($1, question) >= "),
            "few-shot 没有相似度下限：不相干语料会进 prompt：{fewshot_body}"
        );
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
