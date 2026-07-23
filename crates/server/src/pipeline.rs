//! NL2SQL 流水线：检索 → LLM 生成 → 确定性校验(Corrector) → 权限注入 → 只读执行 → few-shot 回写。
//! 设计对齐 SuperSonic 控幻觉骨架，但 LLM 直接产 MySQL SELECT（单库单方言，省掉 S2SQL 中间层；
//! 确定性保证全部由 AST 校验 + 权限注入 + 数据库层 READ ONLY 承担——旧项目 90+ 轮实证路线）。

use serde::Serialize;
use sqlparser::ast::Statement;
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use sqlx::{Column, MySqlPool, PgPool, Row, TypeInfo};

use crate::llm::{extract_sql, LlmClient};
use crate::principal::Principal;
use crate::scope::ScopeSets;
use crate::{inject, meta, scope};

#[derive(Serialize)]
pub struct AskResult {
    pub sql: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub truncated: bool,
    pub elapsed_ms: u128,
    pub route: String,
}

const MAX_ROWS: usize = 200;
const EXEC_TIMEOUT_SECS: u64 = 30;

/// 安全校验：单条 SELECT、无敏感列、无占位符幻觉。
pub fn is_safe_select(sql: &str) -> anyhow::Result<()> {
    let stmts = Parser::parse_sql(&MySqlDialect {}, sql)?;
    if stmts.len() != 1 {
        anyhow::bail!("只允许单条语句");
    }
    if !matches!(stmts[0], Statement::Query(_)) {
        anyhow::bail!("只允许 SELECT");
    }
    let lower = sql.to_lowercase();
    for kw in ["login_pwd", "password", "into outfile", "into dumpfile"] {
        if lower.contains(kw) {
            anyhow::bail!("SQL 含禁用项: {kw}");
        }
    }
    // 占位符幻觉防线（旧项目实证：LLM 会编 '__ORDER_CODE__'/'xxx_PLACEHOLDER' 恒空自信答 0）
    if lower.contains("__") && lower.contains("'") {
        for frag in sql.split('\'') {
            if frag.starts_with("__") && frag.ends_with("__") {
                anyhow::bail!("SQL 含未填充占位符: {frag}");
            }
        }
    }
    if lower.contains("_placeholder") {
        anyhow::bail!("SQL 含占位符幻觉");
    }
    Ok(())
}

/// LIMIT 护栏：非纯聚合且无 LIMIT → 追加 LIMIT 200
fn ensure_limit(sql: &str) -> String {
    let upper = sql.to_uppercase();
    if upper.contains("LIMIT") {
        return sql.to_string();
    }
    format!("{} LIMIT {}", sql.trim().trim_end_matches(';'), MAX_ROWS)
}

fn build_system_prompt(p: &Principal, today: &str) -> String {
    format!(
        r#"你是皇家小虎 DMS 数据助手的 SQL 生成器。只输出一条 MySQL SELECT 语句（```sql 围栏包裹），不解释。
【今天】{today}
【当前用户】{}（登录名 {}，工号 {}）。数据权限已由系统底层自动过滤，绝不要自行添加人员/权限过滤条件；用户问"我的"时也不要臆造 owner 条件。
硬规则：
1. 只写一条 SELECT；结果列用中文别名（反引号包裹）。
2. 客户/商品/人名等名称过滤一律用 LIKE '%词%'，绝不用 =（全称常带前缀，等值必 0 行）。
3. 表头注释里的【⚠️...】警告必须逐条遵守——那些是连库验证过的真坑。
4. 有 deleted_flag 列的表加 deleted_flag=0；不确定列是否存在就别加。
5. 时间相对词（本月/上月/今年）基于【今天】用日期函数写；绝不硬编码年份数字。
6. 明细类查询给 8 列以上业务字段并 ORDER BY 时间 DESC；聚合类不受此限。
7. 绝不发明占位符（如 '__XX__'、'xxx_PLACEHOLDER'）。"#,
        p.actual_name, p.login_name, p.employee_id
    )
}

async fn fewshot_block(pg: &PgPool, question: &str) -> String {
    // few-shot：trgm 相似历史问答（向量召回 M4 接 embed 后升级）
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT question, sql FROM meta.sql_exemplar
         WHERE question != $1
         ORDER BY word_similarity($1, question) DESC LIMIT 2",
    )
    .bind(question)
    .fetch_all(pg)
    .await
    .unwrap_or_default();
    if rows.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n## 相似问题的正确写法（参考口径）\n");
    for (q, sql) in rows {
        s.push_str(&format!("问：{q}\n```sql\n{sql}\n```\n"));
    }
    s
}

pub async fn generate_sql(
    llm: &LlmClient,
    pg: &PgPool,
    p: &Principal,
    question: &str,
) -> anyhow::Result<String> {
    let ctxs = meta::retrieve(pg, question, 6).await?;
    let table_names: Vec<String> = ctxs.iter().map(|c| c.table_name.clone()).collect();
    let pitfalls = meta::recall_pitfalls(pg, question, &table_names, 6).await?;
    let fewshot = fewshot_block(pg, question).await;

    let today = chrono_today();
    let system = build_system_prompt(p, &today);
    let mut user = String::from("## 可用表结构\n");
    for c in &ctxs {
        user.push_str(&c.schema_text);
    }
    if !pitfalls.is_empty() {
        user.push_str("\n## 口径教训（连库验证过，必须遵守）\n");
        for l in &pitfalls {
            user.push_str(&format!("- {l}\n"));
        }
    }
    user.push_str(&fewshot);
    user.push_str(&format!("\n## 问题\n{question}\n"));

    let resp = llm.chat(&llm.model_precise, &system, &user).await?;
    extract_sql(&resp).ok_or_else(|| anyhow::anyhow!("LLM 未产出 SQL: {}", resp.chars().take(200).collect::<String>()))
}

fn chrono_today() -> String {
    // MySQL 侧 CURDATE() 才是真相；这里只给 LLM 参照（周几帮助解析"上周"类问法）
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let days = now / 86400;
    let dow = ["周四", "周五", "周六", "周日", "周一", "周二", "周三"][(days % 7) as usize];
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}（{dow}）")
}

/// days since epoch → (y,m,d)（Howard Hinnant civil_from_days，无依赖）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 执行只读 SQL → JSON 表格
pub async fn execute(mysql: &MySqlPool, sql: &str) -> anyhow::Result<(Vec<String>, Vec<Vec<serde_json::Value>>)> {
    let rows = tokio::time::timeout(
        std::time::Duration::from_secs(EXEC_TIMEOUT_SECS),
        sqlx::query(sql).fetch_all(mysql),
    )
    .await
    .map_err(|_| anyhow::anyhow!("查询超时（>{EXEC_TIMEOUT_SECS}s）"))??;

    let mut columns: Vec<String> = vec![];
    let mut data: Vec<Vec<serde_json::Value>> = vec![];
    for (i, row) in rows.iter().enumerate() {
        if i == 0 {
            columns = row.columns().iter().map(|c| c.name().to_string()).collect();
        }
        let mut out_row = vec![];
        for (ci, col) in row.columns().iter().enumerate() {
            out_row.push(cell_to_json(row, ci, col.type_info().name()));
        }
        data.push(out_row);
        if data.len() >= MAX_ROWS {
            break;
        }
    }
    Ok((columns, data))
}

fn cell_to_json(row: &sqlx::mysql::MySqlRow, i: usize, ty: &str) -> serde_json::Value {
    use serde_json::Value;
    match ty {
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "TINYINT UNSIGNED"
        | "SMALLINT UNSIGNED" | "INT UNSIGNED" | "BIGINT UNSIGNED" | "YEAR" => row
            .try_get::<Option<i64>, _>(i)
            .ok()
            .flatten()
            .map(Value::from)
            .unwrap_or(Value::Null),
        "FLOAT" | "DOUBLE" => row
            .try_get::<Option<f64>, _>(i)
            .ok()
            .flatten()
            .map(Value::from)
            .unwrap_or(Value::Null),
        "DECIMAL" => row
            .try_get::<Option<rust_decimal::Decimal>, _>(i)
            .ok()
            .flatten()
            .map(|d| Value::from(d.to_string()))
            .unwrap_or(Value::Null),
        "DATE" | "DATETIME" | "TIMESTAMP" | "TIME" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|d| Value::from(d.format("%Y-%m-%d %H:%M:%S").to_string()))
            .or_else(|| {
                row.try_get::<Option<chrono::NaiveDate>, _>(i)
                    .ok()
                    .flatten()
                    .map(|d| Value::from(d.format("%Y-%m-%d").to_string()))
            })
            .unwrap_or(Value::Null),
        _ => row
            .try_get::<Option<String>, _>(i)
            .ok()
            .flatten()
            .map(Value::from)
            .unwrap_or(Value::Null),
    }
}

/// 完整问答链：生成 → 校验 → 注入 → 执行；SQL 报错时携错误自修一次（旧项目实证通道）。
pub async fn ask(
    llm: &LlmClient,
    mysql: &MySqlPool,
    pg: &PgPool,
    p: &Principal,
    question: &str,
) -> anyhow::Result<AskResult> {
    let t0 = std::time::Instant::now();
    let sets = scope::compute_scope(mysql, p).await?;

    let mut sql = generate_sql(llm, pg, p, question).await?;
    let mut route = "llm".to_string();

    for attempt in 0..2 {
        let candidate = ensure_limit(&sql);
        if let Err(e) = is_safe_select(&candidate) {
            if attempt == 0 {
                sql = repair(llm, pg, p, question, &candidate, &e.to_string()).await?;
                route = "llm+repair".into();
                continue;
            }
            anyhow::bail!("SQL 安全校验未通过: {e}");
        }
        let injected = inject::inject(&candidate, &sets)?;
        match execute(mysql, &injected).await {
            Ok((columns, rows)) => {
                // few-shot 回写：跑通且有结果的问答沉淀为语料（自进化闭环）
                if !rows.is_empty() {
                    let _ = sqlx::query(
                        "INSERT INTO meta.sql_exemplar(question, sql) SELECT $1, $2
                         WHERE NOT EXISTS (SELECT 1 FROM meta.sql_exemplar WHERE question = $1)",
                    )
                    .bind(question)
                    .bind(&candidate)
                    .execute(pg)
                    .await;
                }
                let row_count = rows.len();
                return Ok(AskResult {
                    sql: injected,
                    columns,
                    truncated: row_count >= MAX_ROWS,
                    row_count,
                    rows,
                    elapsed_ms: t0.elapsed().as_millis(),
                    route,
                });
            }
            Err(e) if attempt == 0 => {
                sql = repair(llm, pg, p, question, &candidate, &e.to_string()).await?;
                route = "llm+repair".into();
            }
            Err(e) => return Err(e),
        }
    }
    anyhow::bail!("生成失败（自修后仍不可用）")
}

async fn repair(
    llm: &LlmClient,
    pg: &PgPool,
    p: &Principal,
    question: &str,
    bad_sql: &str,
    error: &str,
) -> anyhow::Result<String> {
    let ctxs = meta::retrieve(pg, question, 6).await?;
    let mut user = String::from("## 可用表结构\n");
    for c in &ctxs {
        user.push_str(&c.schema_text);
    }
    user.push_str(&format!(
        "\n## 问题\n{question}\n\n## 上一版 SQL（执行失败）\n```sql\n{bad_sql}\n```\n## 错误\n{error}\n\n请修正后重新输出一条正确的 MySQL SELECT。"
    ));
    let system = build_system_prompt(p, &chrono_today());
    let resp = llm.chat(&llm.model_precise, &system, &user).await?;
    extract_sql(&resp).ok_or_else(|| anyhow::anyhow!("自修未产出 SQL"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_select_passes() {
        assert!(is_safe_select("SELECT a FROM b WHERE c = 1").is_ok());
    }

    #[test]
    fn rejects_multi_statement() {
        assert!(is_safe_select("SELECT 1; DROP TABLE x").is_err());
    }

    #[test]
    fn rejects_non_select() {
        assert!(is_safe_select("UPDATE t SET a = 1").is_err());
    }

    #[test]
    fn rejects_sensitive() {
        assert!(is_safe_select("SELECT login_pwd FROM t_employee").is_err());
    }

    #[test]
    fn rejects_placeholder() {
        assert!(is_safe_select("SELECT * FROM t WHERE code = '__ORDER_CODE__'").is_err());
        assert!(is_safe_select("SELECT * FROM t WHERE code = 'X_PLACEHOLDER'").is_err());
    }

    #[test]
    fn limit_appended() {
        assert!(ensure_limit("SELECT * FROM t").ends_with("LIMIT 200"));
        assert_eq!(ensure_limit("SELECT * FROM t LIMIT 5"), "SELECT * FROM t LIMIT 5");
    }

    #[test]
    fn civil_date_sane() {
        // 2026-07-23 = epoch day 20657
        let (y, m, d) = civil_from_days(20657);
        assert_eq!((y, m, d), (2026, 7, 23));
    }
}
