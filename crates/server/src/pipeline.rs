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
    pub view: crate::viewspec::ViewSpec,
    /// 复合问题的子结果（deepagents 拆解-合并）；单结果时为空
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subs: Vec<SubResult>,
}

/// 复合子问题结果（deepagents SubAgent 收敛：每子问题一句题目 + 完整结果）
#[derive(Serialize)]
pub struct SubResult {
    pub question: String,
    pub result: AskResult,
}

impl AskResult {
    /// 复合容器：主体空，subs 装各子结果，前端分面板渲染
    fn compound(subs: Vec<SubResult>, elapsed_ms: u128) -> Self {
        AskResult {
            sql: "[复合问题拆解]".into(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            truncated: false,
            elapsed_ms,
            route: "compound".into(),
            view: crate::viewspec::build(&[], &[]),
            subs,
        }
    }
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
    // 🔴 只读红线（移植 deepagents text-to-sql-agent 硬拦范式）：AST 之外的显式第二道防线。
    // DMS 生产库只读是铁律——DML/DDL 一律拦（尾空格避免误伤 deleted_flag/created_time/updated_time）。
    const FORBIDDEN: &[&str] = &[
        "insert ", "update ", "delete ", "drop ", "alter ", "truncate ",
        "create ", "replace ", "merge ", "grant ", "revoke ", "into outfile", "into dumpfile",
    ];
    for kw in FORBIDDEN {
        if lower.contains(kw) {
            anyhow::bail!("只读红线：禁止写操作 [{}]", kw.trim());
        }
    }
    if lower.contains("login_pwd") || lower.contains("password") {
        anyhow::bail!("SQL 含敏感列");
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
/// JSON 单元格 → f64（DECIMAL 存字符串，数字直取）
fn cell_num(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

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
7. 绝不发明占位符（如 '__XX__'、'xxx_PLACEHOLDER'）。
8. 时间过滤用比较运算符（列 >= '起' AND 列 < '止'），绝不用 YEAR()/DATE_FORMAT() 包裹列做过滤（包裹后走不了索引，大表全表扫）。
9. 问题没明确提时间范围时，聚合类不要自行加时间过滤（查全部），除非是"最近/趋势"类语义。"#,
        p.actual_name, p.login_name, p.employee_id
    )
}

async fn fewshot_block(pg: &PgPool, question: &str) -> String {
    // few-shot：trgm 相似历史问答；复核判错的(disabled)剔除，只用高质量语料
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT question, sql FROM meta.sql_exemplar
         WHERE question != $1 AND status != 'disabled'
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
    let metrics = meta::recall_metrics(pg, question).await.unwrap_or_default();
    let dims = meta::recall_dimensions(pg, question).await.unwrap_or_default();
    let terms = meta::recall_terms(pg, question).await.unwrap_or_default();
    let pitfalls = meta::recall_pitfalls(pg, question, &table_names, 6).await?;
    let fewshot = fewshot_block(pg, question).await;

    let today = chrono_today();
    let system = build_system_prompt(p, &today);
    let mut user = String::new();
    // 指标口径卡（移植 SuperSonic 语义层）——最高优先级，命中即必须严格遵守，杜绝自选表/算法
    if !metrics.is_empty() {
        user.push_str("## 指标口径（问题命中以下指标，必须严格按此口径，禁止自己选表或改算法）\n");
        for m in &metrics {
            user.push_str(&format!("- {m}\n"));
        }
        user.push('\n');
    }
    // 维度口径卡（移植 SuperSonic DimensionResp）——按此维度分组时必须用此取值表达式/连接键
    if !dims.is_empty() {
        user.push_str("## 维度口径（问题命中以下维度，分组取数必须按此口径，禁止自己臆造连接键）\n");
        for d in &dims {
            user.push_str(&format!("- {d}\n"));
        }
        user.push('\n');
    }
    // 业务术语（移植 SuperSonic DomainTerms）——帮 LLM 理解黑话
    if !terms.is_empty() {
        user.push_str("## 业务术语（问题命中，按此理解）\n");
        for t in &terms {
            user.push_str(&format!("- {t}\n"));
        }
        user.push('\n');
    }
    user.push_str("## 可用表结构\n");
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
/// 追问识别：短问句且含追问/指代词，需结合上一轮上下文改写
fn is_followup(q: &str) -> bool {
    let n = q.chars().count();
    if n > 14 {
        return false;
    }
    const MARK: &[&str] = &[
        "那", "再", "呢", "按", "换", "上个", "下个", "它", "这个", "这张", "该", "此",
        "前", "后", "同比", "环比", "拆", "分开", "对比", "上月", "下月", "去年",
    ];
    MARK.iter().any(|m| q.contains(m))
}

/// 多轮追问改写（移植 SuperSonic rewriteMultiTurn）：短追问结合上一轮问题改写成完整独立问题。
async fn rewrite_followup(llm: &LlmClient, question: &str, prev: Option<&str>) -> String {
    let Some(prev_q) = prev else {
        return question.to_string();
    };
    if !is_followup(question) {
        return question.to_string();
    }
    let system = "你把用户的追问结合上一轮问题改写成一个完整、独立、可单独理解的问题。只输出改写后的问题本身，不要解释、不要引号。若追问已经完整则原样输出。";
    let user = format!("上一轮问题：{prev_q}\n本轮追问：{question}\n改写为完整问题：");
    match llm.chat(&llm.model_fast, system, &user).await {
        Ok(r) => {
            let rewritten = r.trim().trim_matches('"').trim_matches('。').to_string();
            if rewritten.is_empty() { question.to_string() } else { rewritten }
        }
        Err(_) => question.to_string(),
    }
}

/// 复合问题识别（deepagents planning 门控）：明确「分别/对比」+ 需多维度/口径拆解
fn is_compound(q: &str) -> bool {
    q.contains("分别") || (q.contains("对比") && q.matches('和').count() + q.matches('与').count() >= 1)
}

/// 拆解复合问题为独立子问题（fast 模型，deepagents write_todos 思想）
async fn split_questions(llm: &LlmClient, question: &str) -> Vec<String> {
    let system = "把用户的复合问题拆成 2-3 个可独立查询的子问题，每个子问题自包含（含时间/维度）。只输出 JSON 字符串数组，如 [\"各省销售额\",\"各商品分类销量\"]，不要解释。";
    match llm.chat(&llm.model_fast, system, question).await {
        Ok(r) => {
            // 抽 JSON 数组
            let start = r.find('[');
            let end = r.rfind(']');
            if let (Some(s), Some(e)) = (start, end) {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(&r[s..=e]) {
                    return v.into_iter().filter(|s| !s.trim().is_empty()).take(3).collect();
                }
            }
            vec![]
        }
        Err(_) => vec![],
    }
}

pub async fn ask(
    llm: &LlmClient,
    mysql: &MySqlPool,
    pg: &PgPool,
    p: &Principal,
    question: &str,
    prev_question: Option<&str>,
) -> anyhow::Result<AskResult> {
    let t0 = std::time::Instant::now();
    // 多轮追问改写：把"那上个月呢"结合上一轮改写成"上月销售额"再走管线
    let rewritten = rewrite_followup(llm, question, prev_question).await;

    // 复合问题拆解（deepagents P0：规划→多步查询→合并）：拆子问题并行执行，各自独立
    if is_compound(&rewritten) {
        let subs_q = split_questions(llm, &rewritten).await;
        if subs_q.len() >= 2 {
            let futs = subs_q.iter().map(|q| ask_single(llm, mysql, pg, p, q));
            let results = futures::future::join_all(futs).await;
            let subs: Vec<SubResult> = subs_q
                .into_iter()
                .zip(results)
                .filter_map(|(q, r)| r.ok().map(|res| SubResult { question: q, result: res }))
                .collect();
            if !subs.is_empty() {
                return Ok(AskResult::compound(subs, t0.elapsed().as_millis()));
            }
        }
    }

    ask_single(llm, mysql, pg, p, &rewritten).await
}

async fn ask_single(
    llm: &LlmClient,
    mysql: &MySqlPool,
    pg: &PgPool,
    p: &Principal,
    question: &str,
) -> anyhow::Result<AskResult> {
    let t0 = std::time::Instant::now();
    let sets = scope::compute_scope_cached(mysql, p).await?;

    // 图关系快路径（AGE，0-LLM）：仅全权限用户（图无行级权限，限权回落 LLM 走注入）
    if sets.is_unrestricted() {
        if let Some(rel) = crate::direct::detect_relation(question) {
            if let Some(r) = try_graph(pg, &rel, t0).await {
                return Ok(r);
            }
        }
    }

    // 确定性快路径（单号直查/高频聚合）：命中即 0-LLM 秒出，仍走安全校验+权限注入+只读执行
    if let Some(hit) = crate::direct::try_direct(question) {
        if is_safe_select(&hit.sql).is_ok() {
            let injected = inject::inject(&hit.sql, &sets)?;
            if let Ok((columns, rows)) = execute(mysql, &injected).await {
                let row_count = rows.len();
                let mut view = crate::viewspec::build(&columns, &rows);
                // KPI 环比：单指标聚合时查上期算 Δ%
                if let Some((prev_sql, label)) = &hit.prev {
                    if let (Some(cur), Ok(prev_inj)) = (
                        rows.first().and_then(|r| r.first()).and_then(cell_num),
                        inject::inject(prev_sql, &sets),
                    ) {
                        if let Ok((_, prow)) = execute(mysql, &prev_inj).await {
                            if let Some(prev) = prow.first().and_then(|r| r.first()).and_then(cell_num) {
                                crate::viewspec::patch_kpi_delta(&mut view, cur, prev, label.clone());
                            }
                        }
                    }
                }
                return Ok(AskResult {
                    sql: injected,
                    columns,
                    truncated: row_count >= MAX_ROWS,
                    row_count,
                    rows,
                    elapsed_ms: t0.elapsed().as_millis(),
                    route: hit.route,
                    view,
                    subs: vec![],
                });
            }
            // 确定性 SQL 执行失败（列漂移等）→ 静默回落 LLM
        }
    }

    // 语义缓存（移植 SuperSonic 向量召回近义问答 + 旧项目护栏）：近义历史问答命中即 0-LLM 秒出
    if !is_followup(question) {
        if let Some(r) = try_semantic_cache(pg, mysql, question, &sets, t0).await {
            return Ok(r);
        }
    }

    let mut sql = generate_sql(llm, pg, p, question).await?;
    let mut route = "llm".to_string();

    // SchemaCorrector（移植 SuperSonic）：执行前字段白名单校验，幻觉列携真实列清单自修一次
    if let Ok(Some(hint)) = crate::corrector::schema_check(pg, &ensure_limit(&sql)).await {
        if let Ok(fixed) = repair(llm, pg, p, question, &sql, &hint).await {
            sql = fixed;
            route = "llm+schema-fix".into();
        }
    }
    // GroupByCorrector（移植 SuperSonic）：漏 GROUP BY 确定性补全（不调 LLM）
    if let Some(fixed) = crate::corrector::fix_group_by(&sql) {
        sql = fixed;
    }
    // AggCorrector（移植 SuperSonic correctAggFunction）：命中指标的聚合列归一到注册表默认聚合
    if let Ok(Some(fixed)) = crate::corrector::correct_agg(pg, question, &sql).await {
        sql = fixed;
    }
    // ValueLinker（移植 SuperSonic 值链接）：编码列中文名直写确定性换码（写中文名必返 0 行的真坑）
    if let Ok(Some(fixed)) = crate::corrector::correct_value(pg, &sql).await {
        sql = fixed;
    }

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
                // few-shot 回写：跑通且有结果的问答沉淀为语料（status=pending 待复核）
                if !rows.is_empty() {
                    let inserted = sqlx::query(
                        "INSERT INTO meta.sql_exemplar(question, sql) SELECT $1, $2
                         WHERE NOT EXISTS (SELECT 1 FROM meta.sql_exemplar WHERE question = $1)",
                    )
                    .bind(question)
                    .bind(&candidate)
                    .execute(pg)
                    .await
                    .map(|r| r.rows_affected() > 0)
                    .unwrap_or(false);
                    // 异步：记忆复核（质量把关）+ 存问句向量（供语义缓存召回）
                    if inserted {
                        let (llm2, pg2) = (llm.clone(), pg.clone());
                        let (q2, sql2) = (question.to_string(), candidate.clone());
                        tokio::spawn(async move {
                            review_exemplar(&llm2, &pg2, &q2, &sql2).await;
                            if let Some(v) = crate::embed::embed_query(&q2).await {
                                let vlit = crate::embed::to_pgvector(&v);
                                let _ = sqlx::query(
                                    "UPDATE meta.sql_exemplar SET embedding = $1::vector WHERE question = $2",
                                )
                                .bind(&vlit)
                                .bind(&q2)
                                .execute(&pg2)
                                .await;
                            }
                        });
                    }
                }
                let row_count = rows.len();
                let view = crate::viewspec::build(&columns, &rows);
                return Ok(AskResult {
                    sql: injected,
                    columns,
                    truncated: row_count >= MAX_ROWS,
                    row_count,
                    rows,
                    elapsed_ms: t0.elapsed().as_millis(),
                    route,
                    view,
                    subs: vec![],
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

/// 记忆复核（移植 SuperSonic MemoryReviewTask）：fast LLM 判 SQL 是否正确回答问题。
/// POSITIVE→enabled（进 few-shot），NEGATIVE→disabled（剔除，不当范例传播）。
async fn review_exemplar(llm: &LlmClient, pg: &PgPool, question: &str, sql: &str) {
    let system = "你是资深数据工程师，审核一条 SQL 是否正确回答了给定问题（口径合理、表/字段对、无明显错误）。\
                  日期过滤是否精确不必挑剔。只输出一行：opinion=POSITIVE 或 opinion=NEGATIVE。";
    let user = format!("问题：{question}\nSQL：\n{sql}\n审核结论：");
    let status = match llm.chat(&llm.model_fast, system, &user).await {
        Ok(r) => {
            if r.to_uppercase().contains("NEGATIVE") {
                "disabled"
            } else {
                "enabled"
            }
        }
        Err(_) => return, // 复核失败保持 pending，下次再议
    };
    let _ = sqlx::query("UPDATE meta.sql_exemplar SET status = $1 WHERE question = $2")
        .bind(status)
        .bind(question)
        .execute(pg)
        .await;
}

/// 时间词集合（护栏：命中缓存的问题时间词必须与本问全等，"上月"≠"本月"）
fn time_tokens(q: &str) -> std::collections::BTreeSet<&'static str> {
    ["今天", "昨天", "前天", "本月", "上月", "上个月", "这个月", "本周", "上周", "今年", "去年", "本季度"]
        .into_iter()
        .filter(|t| q.contains(t))
        .collect()
}

/// 数字词集合（护栏："前5"≠"前10"）
fn number_tokens(q: &str) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    let mut cur = String::new();
    for c in q.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            set.insert(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        set.insert(cur);
    }
    set
}

/// 语义缓存：向量找近义已复核语料，时间/数字词护栏全等则复用其 SQL（0-LLM）。
async fn try_semantic_cache(
    pg: &PgPool,
    mysql: &MySqlPool,
    question: &str,
    sets: &ScopeSets,
    t0: std::time::Instant,
) -> Option<AskResult> {
    let vec = crate::embed::embed_query(question).await?;
    let vlit = crate::embed::to_pgvector(&vec);
    // 最近义的一条 enabled 语料 + 余弦距离
    let row = sqlx::query_as::<_, (String, String, f64)>(
        "SELECT question, sql, (embedding <=> $1::vector) AS dist FROM meta.sql_exemplar
         WHERE status = 'enabled' AND embedding IS NOT NULL AND question != $2
         ORDER BY embedding <=> $1::vector LIMIT 1",
    )
    .bind(&vlit)
    .bind(question)
    .fetch_optional(pg)
    .await
    .ok()
    .flatten()?;
    let (hit_q, hit_sql, dist) = row;
    if dist > 0.12 {
        return None; // 不够近
    }
    // 护栏：时间词、数字词集合必须全等（否则语义近似会把上月命中本月）
    if time_tokens(question) != time_tokens(&hit_q) || number_tokens(question) != number_tokens(&hit_q) {
        return None;
    }
    // 命中：复用 SQL（数据实时查、权限按当轮用户注入）
    let candidate = ensure_limit(&hit_sql);
    is_safe_select(&candidate).ok()?;
    let injected = inject::inject(&candidate, sets).ok()?;
    let (columns, rows) = execute(mysql, &injected).await.ok()?;
    let row_count = rows.len();
    let view = crate::viewspec::build(&columns, &rows);
    Some(AskResult {
        sql: injected,
        columns,
        truncated: row_count >= MAX_ROWS,
        row_count,
        rows,
        elapsed_ms: t0.elapsed().as_millis(),
        route: "semantic-cache".into(),
        view,
        subs: vec![],
    })
}

/// 批量复核 pending 语料（移植 SuperSonic MemoryReviewTask 定时扫 pending）。返回处理条数。
pub async fn review_all_pending(llm: &LlmClient, pg: &PgPool, limit: i64) -> anyhow::Result<usize> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT question, sql FROM meta.sql_exemplar WHERE status = 'pending' LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pg)
    .await?;
    let n = rows.len();
    for (q, sql) in rows {
        review_exemplar(llm, pg, &q, &sql).await;
    }
    Ok(n)
}

/// 图关系查询 → AskResult（表格形态）。查询失败/空结果返回 None（回落 LLM）。
async fn try_graph(
    pg: &PgPool,
    rel: &crate::direct::Relation,
    t0: std::time::Instant,
) -> Option<AskResult> {
    use crate::direct::Relation;
    let (entity_label, rows_data) = match rel {
        Relation::BuyersOfGoods(name) => ("客户", crate::graph::buyers_of_goods(pg, name, 50).await.ok()?),
        Relation::GoodsOfCustomer(name) => ("商品", crate::graph::goods_of_customer(pg, name, 50).await.ok()?),
        Relation::Copurchase(name) => ("商品", crate::graph::copurchase(pg, name, 50).await.ok()?),
    };
    if rows_data.is_empty() {
        return None;
    }
    let columns = vec![
        format!("{entity_label}编码"),
        format!("{entity_label}名称"),
        "购买额".to_string(),
    ];
    let rows: Vec<Vec<serde_json::Value>> = rows_data
        .iter()
        .map(|g| {
            vec![
                serde_json::Value::from(g.code.clone()),
                serde_json::Value::from(g.name.clone()),
                serde_json::Value::from(format!("{:.2}", g.amount)),
            ]
        })
        .collect();
    let row_count = rows.len();
    let view = crate::viewspec::build(&columns, &rows);
    Some(AskResult {
        sql: format!("[AGE 图查询] {rel:?}"),
        columns,
        truncated: false,
        row_count,
        rows,
        elapsed_ms: t0.elapsed().as_millis(),
        route: "graph".into(),
        view,
        subs: vec![],
    })
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
    fn readonly_redline() {
        // 只读红线：DML/DDL 硬拦
        assert!(is_safe_select("DELETE FROM t_sales_order").is_err());
        assert!(is_safe_select("DROP TABLE t").is_err());
        assert!(is_safe_select("UPDATE t SET a=1").is_err());
        // 但 deleted_flag/created_time/updated_time 列名不误伤
        assert!(is_safe_select("SELECT deleted_flag, created_time, updated_time FROM t_sales_order WHERE deleted_flag = 0").is_ok());
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

    #[test]
    fn cache_time_guard() {
        // 本月 ≠ 上月：护栏必须拦
        assert_ne!(time_tokens("本月销售额"), time_tokens("上月销售额"));
        // 同时间词：可命中
        assert_eq!(time_tokens("本月销售额是多少"), time_tokens("查本月销售额"));
    }

    #[test]
    fn cache_number_guard() {
        assert_ne!(number_tokens("前5的省份"), number_tokens("前10的省份"));
        assert_eq!(number_tokens("销售额"), number_tokens("营业额")); // 都无数字
    }
}
