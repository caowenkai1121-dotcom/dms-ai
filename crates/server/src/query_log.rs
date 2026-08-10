//! 【K6-B】查询日志（**只写不读**）：一次问答一行 + 按阶段 token 用量。
//! 变更原因＝可观测口径（成本与延迟）。SQLBot 的 H9：今天完全没有。
//!
//! 两条纪律：
//! 1. **观测绝不进主链路**：写入走 `tokio::spawn` + 失败只 warn（观测掉了不算故障），
//!    调用点（`crate::ask` 的出口）一个 `.await` 都不多。
//! 2. `question`/`sql`/`error` 入库前各截 2000 字 —— 一行日志几百 KB 会把这张表写成事故本身。
//!
//! 共享件在 `dms_kernel::qalog`：INSERT 列清单、`STATUS_*` 取值域、脱敏/截断、超时文案判据 ——
//! KB 落账（knowledge `qa_log`，`route='knowledge'`）与本文件同一写口纪律，两份即漂移。
//!
//! ## 读侧（`GET /api/stats`）已删 —— 二·AS7
//! 零消费者（全仓 grep：`web/src` 无 fetch、`tools/*.py` 无一处调它），且本表没有
//! `conv_id`，统计维度本来就窄。一起带走的是 `stats()`/`clamp_days`/两条统计 SQL/
//! `StatsQuery`/`is_admin`，以及只断言这些已删对象的 4 条单测
//! （被判对象没了还留着断言 = 恒真判据；本 crate 单测 145 → 141）。
//! 要重开：直接连 PG 查 `meta.query_log`（p50/p95 用 `percentile_cont(...) WITHIN GROUP`
//! 在 SQL 里算，**别把全量 elapsed_ms 拉进内存排序** —— 百万行 = OOM + 拖死 PG），
//! 端点必须 admin_only 且判据只认 `administrator_flag`（表里是**别人的问句**）。
//!
//! T10 把 server 拆成 `api/` 目录时，本文件整体随 agent 的 ask 出口走。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use dms_kernel::llm::Usage;
// 列清单、状态取值域、脱敏/截断、超时判据的唯一事实源（KB 落账共用，见文件头）
use dms_kernel::qalog::{
    clip, sanitize, timeout_marked, INSERT_SQL, STATUS_BLOCKED, STATUS_FAILED, STATUS_SUCCEEDED,
    STATUS_TIMEOUT,
};
use sqlx::PgPool;

use dms_agent::AskResult;

const DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS meta;
CREATE TABLE IF NOT EXISTS meta.query_log(
  id bigserial PRIMARY KEY,
  at timestamptz NOT NULL DEFAULT now(),
  login_name text NOT NULL DEFAULT '',
  ds_id text NOT NULL DEFAULT '',
  route text NOT NULL DEFAULT '',
  question text NOT NULL DEFAULT '',
  sql text NOT NULL DEFAULT '',
  row_count int NOT NULL DEFAULT 0,
  elapsed_ms bigint NOT NULL DEFAULT 0,
  cache_hit boolean NOT NULL DEFAULT false,
  prompt_tokens int NOT NULL DEFAULT 0,
  completion_tokens int NOT NULL DEFAULT 0,
  error text NOT NULL DEFAULT '',
  -- 这三列历史上由 semantic/ddl.rs 的 ALTER 补——但那条 migrate 在本表建表之前跑，
  -- 全新空库直接起不来（部署演练抓到）。建表即全量；老库由下方 IF EXISTS 幂等对齐。
  trace_id text,
  conv_id text,
  llm_calls int NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_query_log_at ON meta.query_log(at DESC);
CREATE INDEX IF NOT EXISTS idx_query_log_route_at ON meta.query_log(route, at DESC);
CREATE INDEX IF NOT EXISTS idx_query_trace ON meta.query_log(trace_id);
-- 老库三列兜底（新库建表已含；IF EXISTS 幂等）
ALTER TABLE meta.query_log ADD COLUMN IF NOT EXISTS trace_id text;
ALTER TABLE meta.query_log ADD COLUMN IF NOT EXISTS conv_id text;
ALTER TABLE meta.query_log ADD COLUMN IF NOT EXISTS llm_calls int NOT NULL DEFAULT 0;
-- 【A2】全状态审计：成功不再是唯一被记录的结局。老行（本列上线前）留默认空串，
-- 不回填 —— 回填要对一张只增不减的日志表做全表 UPDATE，每次启动都扫一遍不值。
ALTER TABLE meta.query_log ADD COLUMN IF NOT EXISTS status text NOT NULL DEFAULT '';
-- 【D7】本轮实际进 prompt 的上下文摘要（JSON 文本：结构/尺寸/表名，无用户数据值）。
-- 不进 INSERT_SQL：那条是 KB 落账共用的 15 列契约（kernel qalog 的测试钉着 $15/!$16），
-- 本列只属本写口，INSERT 后按 trace_id UPDATE 贴回（见 `insert`）。老行留默认空串不回填。
ALTER TABLE meta.query_log ADD COLUMN IF NOT EXISTS context_summary text NOT NULL DEFAULT '';
"#;

/// 建表。与 `dms_semantic::ddl::migrate` 同风格（按分号逐句切，故 DDL 里不许出现 `DO $$` 与注释内分号）。
pub async fn migrate(pg: &PgPool) -> anyhow::Result<()> {
    for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

/// 本轮问答的观测累加器。`&Trace` 往下传（`Send + Sync`，复合问题的并行子问句共用一个）。
///
/// 只有**最贵的那两次** precise 调用（`generate_sql` / `repair`）往里加用量：
/// 便宜的 fast 调用（改写/拆解/分诊/复核）占比小，接它们要动 8 处签名，本轮不接。
#[derive(Default)]
pub struct Trace {
    prompt: AtomicU32,
    completion: AtomicU32,
    /// 本轮真的打了几次 precise LLM（每次 `add` = 一次）。开了自一致采样后这个数
    /// 才有意义：`sc_samples=3` 时 1~3，提前收工的效果直接读得出来。
    calls: AtomicU32,
    /// 选源之后才知道；`OnceLock` = 只写一次，选源之前失败就是空串
    ds: OnceLock<String>,
    /// 一次问答的关联键（`correction_log` / `failure_log` / `query_log` 三张表共用它）。
    /// server 侧生成、透传给 agent 的 `AskCtx` —— 三表因此拼得回同一次问答。
    /// `OnceLock` 与 `ds` 同一个理由：复合问题的并行子问句共用一个，只许写一次。
    trace_id: OnceLock<String>,
    conv_id: OnceLock<String>,
}

impl Trace {
    pub fn add(&self, u: &Usage) {
        self.prompt.fetch_add(u.prompt_tokens, Ordering::Relaxed);
        self.completion.fetch_add(u.completion_tokens, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_ds(&self, ds: &str) {
        let _ = self.ds.set(ds.to_string());
    }

    /// 关联键只许写一次（`OnceLock`）。调用方在 `ask_traced` 里给：一次问答一个。
    pub fn set_trace(&self, trace_id: &str, conv_id: &str) {
        let _ = self.trace_id.set(trace_id.to_string());
        let _ = self.conv_id.set(conv_id.to_string());
    }

    fn tokens(&self) -> (i32, i32) {
        let get = |a: &AtomicU32| a.load(Ordering::Relaxed).min(i32::MAX as u32) as i32;
        (get(&self.prompt), get(&self.completion))
    }
}

/// 待写入的一行。字段顺序 = `INSERT` 的列顺序。
pub struct Entry {
    pub login_name: String,
    pub ds_id: String,
    pub route: String,
    pub question: String,
    pub sql: String,
    pub row_count: i32,
    pub elapsed_ms: i64,
    pub cache_hit: bool,
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub error: String,
    /// 一次问答的关联键（`correction_log` / `failure_log` 共用同一个）
    pub trace_id: String,
    pub conv_id: String,
    /// 本轮真的打了几次 precise LLM（开了自一致采样后这个数才有意义：1~`sc_samples`）
    pub llm_calls: i32,
    /// 结局分类（A2）：取值仅 `STATUS_*` 四个常量
    pub status: &'static str,
    /// 【D7】本轮实际进 prompt 的上下文摘要（JSON 文本：结构/尺寸/表名，无用户数据值）。
    /// **不是 `INSERT_SQL` 的列**（那条是 KB 落账共用的 15 列契约，kernel qalog 的测试钉着
    /// `$15`/`!$16`）：`finish` 从 agent 的进程内暂存取来，`insert` 里按 `trace_id` UPDATE 贴回。
    pub context_summary: Option<String>,
}

/// 一次问答的出口写入（唯一调用点＝`crate::ask`）。返回写入句柄而**自己不 await**：
/// 服务侧调用方直接丢弃句柄（fire-and-forget，主链路一个 `.await` 都不多 —— 纪律 1）；
/// CLI 一次性进程**必须 await 它** —— 否则 `main` 返回时 spawn 出的 INSERT 还没跑，
/// 进程退出连任务一起带走，`query_log` 整行丢失（实测：CLI 问完库中无新行）。
pub fn finish(
    pg: &PgPool,
    trace: &Trace,
    login: &str,
    question: &str,
    out: &anyhow::Result<AskResult>,
    elapsed_ms: u128,
) -> tokio::task::JoinHandle<()> {
    let mut e = entry(trace, login, question, out, elapsed_ms);
    // 【D7】gather（agent）装配 prompt 时按 trace_id 进程内暂存的上下文摘要随本轮落库
    // （take = 取走即清，一行只贴一份）。不走 prompt 装配的路由（graph/direct/need-intent…）
    // 没有暂存 → None → 不贴。失败轮也照贴：「这轮 LLM 到底看到了什么」在失败时更要查得到。
    if let Some(tid) = trace.trace_id.get() {
        e.context_summary = dms_agent::ctx::take_context_summary(tid);
    }
    spawn_write(pg, e)
}

/// 结果 → 日志行（**纯函数**）。失败也写一行，只是 route/sql 空、`error` 有值。
fn entry(
    trace: &Trace,
    login: &str,
    question: &str,
    out: &anyhow::Result<AskResult>,
    elapsed_ms: u128,
) -> Entry {
    let (route, sql, rows, error) = match out {
        Ok(r) => (r.route.clone(), clip(&r.sql), r.row_count, String::new()),
        Err(e) => (String::new(), String::new(), 0, clip(&sanitize(&e.to_string()))),
    };
    let (prompt_tokens, completion_tokens) = trace.tokens();
    let trace_id = trace.trace_id.get().cloned().unwrap_or_default();
    let conv_id = trace.conv_id.get().cloned().unwrap_or_default();
    Entry {
        login_name: login.to_string(),
        ds_id: trace.ds.get().cloned().unwrap_or_default(),
        cache_hit: is_cache_hit(&route),
        route,
        question: clip(question),
        sql,
        row_count: rows.min(i32::MAX as usize) as i32,
        elapsed_ms: elapsed_ms.min(i64::MAX as u128) as i64,
        prompt_tokens,
        completion_tokens,
        error,
        trace_id,
        conv_id,
        // `add` 只在 precise 调用（`generate_sql`/`repair`）后触发，所以计数即「本轮打了
        // 几发 precise」。采样提前收工的效果（1 发而非 `sc_samples` 发）直接读得出来。
        llm_calls: trace.calls.load(Ordering::Relaxed).min(i32::MAX as u32) as i32,
        status: status_of(out),
        // D7：纯函数不碰进程内暂存 —— 摘要由 `finish` 补（这样 `entry` 的既有判据一条不动）
        context_summary: None,
    }
}

/// 结局分类（纯函数）。typed 判据优先：错误在 agent 侧多数是 `anyhow::Error::from`
/// 原样上抛（fail-closed 纪律要求 PolicyError 绝不包装降级），downcast 拿得到原类型。
fn status_of(out: &anyhow::Result<AskResult>) -> &'static str {
    let Err(e) = out else { return STATUS_SUCCEEDED };
    // 权限注入失败 / 只读红线：闸门原样上抛时类型还在（agent `gate_on` → `?`）。
    if e.downcast_ref::<dms_kernel::PolicyError>().is_some()
        || e.downcast_ref::<dms_kernel::GuardError>().is_some()
    {
        return STATUS_BLOCKED;
    }
    let msg = e.to_string();
    // 两处类型被折成文案的拒绝，按契约文案认（两处文案都有冻结单测钉着）：
    // llm 路径自修后仍过不了红线（agent run 折成「SQL 安全校验未通过: …」）、
    // 选源层 ds 级 ACL 拒绝（「无权访问数据源 …」，HTTP 侧同文案映 403）。
    if msg.starts_with("SQL 安全校验未通过") || msg.contains("无权访问数据源") {
        return STATUS_BLOCKED;
    }
    // 取数超时：typed 是主判据；文案兜底给丢了类型的上游超时（reqwest 的
    // 「operation timed out」、中文「超时」），判据与 KB 落账共用 `qalog::timeout_marked`。
    if matches!(
        e.downcast_ref::<dms_connector::ConnectorError>(),
        Some(dms_connector::ConnectorError::Timeout(_))
    ) || timeout_marked(&msg)
    {
        return STATUS_TIMEOUT;
    }
    STATUS_FAILED
}

/// cache_hit 判定：只有语义缓存复用了别人的 SQL 才算命中。
/// 确定性快路径（`direct-*`/`graph`）**不算** —— 它们省的是 LLM，不是缓存，
/// 混进去会让「缓存命中率」这个指标失去意义。
fn is_cache_hit(route: &str) -> bool {
    route == "semantic-cache"
}

fn spawn_write(pg: &PgPool, e: Entry) -> tokio::task::JoinHandle<()> {
    let pg = pg.clone();
    tokio::spawn(async move {
        if let Err(err) = insert(&pg, &e).await {
            tracing::warn!("查询日志写入失败（观测降级，不影响取数）: {err}");
        }
    })
}

async fn insert(pg: &PgPool, e: &Entry) -> anyhow::Result<()> {
    // 列清单常量与 KB 落账共用（`qalog::INSERT_SQL`）：改列只许改那一处
    sqlx::query(INSERT_SQL)
    .bind(&e.login_name)
    .bind(&e.ds_id)
    .bind(&e.route)
    .bind(&e.question)
    .bind(&e.sql)
    .bind(e.row_count)
    .bind(e.elapsed_ms)
    .bind(e.cache_hit)
    .bind(e.prompt_tokens)
    .bind(e.completion_tokens)
    .bind(&e.error)
    // 空串落 NULL：与「没设关联键」（老行）同形，与「设了但为空」区分开
    .bind(if e.trace_id.is_empty() { None } else { Some(&e.trace_id) })
    .bind(if e.conv_id.is_empty() { None } else { Some(&e.conv_id) })
    .bind(e.llm_calls)
    .bind(e.status)
    .execute(pg)
    .await?;
    // 【D7】context_summary 不进 INSERT（共享 15 列契约，见 DDL 注释）：主语句一字未动，
    // 摘要按 trace_id 贴回本行。同一 spawn 内顺序执行 —— 主链依旧零额外 await（纪律 1）。
    // trace_id 为空（老行/未设关联键）时没有可贴的行，那一档本来也不会有暂存摘要。
    if let Some(cs) = &e.context_summary {
        if !e.trace_id.is_empty() {
            sqlx::query("UPDATE meta.query_log SET context_summary = $1 WHERE trace_id = $2")
                .bind(cs)
                .bind(&e.trace_id)
                .execute(pg)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dms_kernel::qalog::CLIP_CHARS;

    /// INSERT 列清单只有一份（`qalog::INSERT_SQL`，与 KB 落账共用）：本文件不许再内联第二份
    #[test]
    fn insert_sql_lives_in_kernel_qalog() {
        let src = include_str!("query_log.rs");
        assert!(src.contains("sqlx::query(INSERT_SQL)"), "写口必须吃共享常量");
        // concat! 防自匹配（本测试自己的字符串不该被数进去）
        let needle = concat!("INSERT INTO ", "meta.query_log");
        assert_eq!(src.matches(needle).count(), 0, "列清单在这里重述就是第二份");
    }

    /// 问句/SQL 必须按**字符**截断到 2000（几百 KB 一行会把日志表写成事故）
    #[test]
    fn question_and_sql_are_clipped_by_chars() {
        let long_cn = "销".repeat(3000);
        let out = clip(&long_cn);
        assert_eq!(out.chars().count(), CLIP_CHARS);
        assert_eq!(out.len(), CLIP_CHARS * 3, "按字节截会切出半个中文字");
        assert_eq!(clip("短问句"), "短问句");
    }

    /// cache_hit 只认语义缓存：确定性快路径省的是 LLM，不是缓存
    #[test]
    fn cache_hit_only_for_semantic_cache() {
        assert!(is_cache_hit("semantic-cache"));
        for r in ["llm", "llm+repair", "direct-agg", "graph", "compound", ""] {
            assert!(!is_cache_hit(r), "{r} 不该算缓存命中");
        }
    }


    /// 失败也写一行：`error` 有值、route/sql 空、token 用量照记（那次 LLM 钱已经花了）
    #[test]
    fn failed_ask_still_logs_error_and_tokens() {
        let trace = Trace::default();
        trace.set_ds("crm_pg");
        trace.add(&Usage { prompt_tokens: 1200, completion_tokens: 80 });
        trace.add(&Usage { prompt_tokens: 300, completion_tokens: 20 });
        let out: anyhow::Result<AskResult> = Err(anyhow::anyhow!("SQL 安全校验未通过: {}", "x"));
        let e = entry(&trace, "zhangsan", "本月销售额", &out, 1234);
        assert_eq!(e.error, "SQL 安全校验未通过: x");
        assert_eq!(e.status, STATUS_BLOCKED, "红线拒绝（折成文案的形态）");
        assert_eq!((e.route.as_str(), e.sql.as_str()), ("", ""));
        assert_eq!((e.row_count, e.elapsed_ms), (0, 1234));
        assert_eq!((e.prompt_tokens, e.completion_tokens), (1500, 100), "两次 precise 调用累加");
        assert_eq!(e.llm_calls, 2, "每次 add 就是一发 precise");
        assert_eq!(e.ds_id, "crm_pg");
        assert!(!e.cache_hit);
    }

    /// 一条成功的 AskResult（字段集对齐 `deep_api` 测试里的同款构造）
    fn ok_result() -> AskResult {
        AskResult {
            sql: "SELECT 1".into(),
            columns: vec!["c".into()],
            rows: vec![vec![serde_json::Value::from(1)]],
            row_count: 1,
            truncated: false,
            elapsed_ms: 1,
            route: "llm".into(),
            view: dms_kernel::present::ViewSpec {
                columns: vec![],
                blocks: vec![],
                interact: Default::default(),
                insight: None,
            },
            supplemental: None,
            comparisons: vec![],
            subs: vec![],
            caliber_note: None,
            truncation_note: None,
            redacted: vec![],
            scope_note: None,
            trust: None,
            steps: vec![],
            clarify_options: vec![],
            value_labels: vec![],
            sales_context: None,
        }
    }

    fn status(err: anyhow::Error) -> &'static str {
        let out: anyhow::Result<AskResult> = Err(err);
        entry(&Trace::default(), "u", "q", &out, 1).status
    }

    /// 成功路径：status=succeeded、error 空，route/sql/行数照旧（行为一字不变）
    #[test]
    fn succeeded_status_and_fields_unchanged() {
        let out: anyhow::Result<AskResult> = Ok(ok_result());
        let e = entry(&Trace::default(), "zhangsan", "本月销售额", &out, 42);
        assert_eq!(e.status, STATUS_SUCCEEDED);
        assert_eq!((e.error.as_str(), e.route.as_str(), e.sql.as_str()), ("", "llm", "SELECT 1"));
        assert_eq!(e.row_count, 1);
    }

    /// 权限注入失败（PolicyError）→ blocked：typed 原样上抛，downcast 必须认得出
    #[test]
    fn policy_error_status_is_blocked() {
        let s = status(anyhow::Error::new(dms_kernel::PolicyError::UnregisteredTable("t_x".into())));
        assert_eq!(s, STATUS_BLOCKED);
    }

    /// 红线拒绝（GuardError）→ blocked：typed 与折成文案的两种形态都认
    #[test]
    fn guard_error_status_is_blocked() {
        let s = status(anyhow::Error::new(dms_kernel::GuardError::WriteToken("delete".into())));
        assert_eq!(s, STATUS_BLOCKED, "闸门原样上抛时类型还在");
        let s = status(anyhow::anyhow!("SQL 安全校验未通过: 只读红线：禁止写操作 [delete]"));
        assert_eq!(s, STATUS_BLOCKED, "llm 路径自修后仍是红线不过（run 折成文案）");
    }

    /// ds 级 ACL 拒绝 → blocked（HTTP 侧同文案映 403，审计口径必须一致）
    #[test]
    fn ds_acl_denial_status_is_blocked() {
        assert_eq!(status(anyhow::anyhow!("无权访问数据源 ds-9")), STATUS_BLOCKED);
    }

    /// 执行超时 → timeout：typed（ConnectorError::Timeout）与丢类型的文案形态都认
    #[test]
    fn timeout_status_is_timeout() {
        let s = status(anyhow::Error::new(dms_connector::ConnectorError::timeout(
            "dms",
            std::time::Duration::from_secs(30),
        )));
        assert_eq!(s, STATUS_TIMEOUT, "取数超时是 typed 上抛");
        let s = status(anyhow::anyhow!("LLM 请求失败: error sending request: operation timed out"));
        assert_eq!(s, STATUS_TIMEOUT, "上游超时的丢类型文案");
        let s = status(anyhow::anyhow!("超时 [dms] 等待 30.0s 未返回"));
        assert_eq!(s, STATUS_TIMEOUT, "丢了类型的中文超时文案");
    }

    /// 其余执行错误 → failed；且 blocked/timeout 的判据不许误伤普通 SQL 报错
    #[test]
    fn generic_error_status_is_failed() {
        assert_eq!(status(anyhow::anyhow!("查询失败 [dms] Unknown column 'x' in 'field list'")), STATUS_FAILED);
        assert_eq!(status(anyhow::anyhow!("生成失败（自修后仍不可用）")), STATUS_FAILED);
    }

    /// 脱敏：URL userinfo 与凭据键值对不许落库；正常错误文案剥完逐字不变
    #[test]
    fn error_reason_is_sanitized() {
        let out: anyhow::Result<AskResult> = Err(anyhow::anyhow!(
            "连接失败: dsn=postgres://svc:TopSecret@db.internal/mds password=hunter2 API_KEY=sk-123"
        ));
        let e = entry(&Trace::default(), "u", "q", &out, 1);
        assert!(!e.error.contains("TopSecret") && !e.error.contains("hunter2") && !e.error.contains("sk-123"), "凭据落库: {}", e.error);
        assert!(e.error.contains("postgres://***@db.internal"), "{}", e.error);
        assert!(e.error.contains("password=***") && e.error.contains("API_KEY=***"), "{}", e.error);
        // 无凭据形态时逐字不变（剥过头会把 LLM 自修要看的报错改坏）
        assert_eq!(sanitize("查询失败 [dms] Unknown column 'x'"), "查询失败 [dms] Unknown column 'x'");
    }

    /// migrate 幂等纪律：DDL 每句都必须可重复执行（启动路径每次全跑）
    #[test]
    fn ddl_statements_are_idempotent() {
        for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            assert!(stmt.contains("IF NOT EXISTS"), "非幂等语句: {stmt}");
        }
        assert!(DDL.contains("ADD COLUMN IF NOT EXISTS status"), "status 列的迁移丢了");
        assert!(DDL.contains("ADD COLUMN IF NOT EXISTS context_summary"), "context_summary 列的迁移丢了");
    }

    /// 【D7】`entry` 是纯函数：不碰进程内暂存，`context_summary` 恒 None 起步（由 `finish` 补）——
    /// 这条钉住的是「`entry` 的既有判据一条不动」那半；接线那半在下一条。
    #[test]
    fn entry_leaves_context_summary_to_finish() {
        let out: anyhow::Result<AskResult> = Ok(ok_result());
        let e = entry(&Trace::default(), "u", "q", &out, 1);
        assert_eq!(e.context_summary, None);
    }

    /// 🔴 D7 接线判据（无库可测的部分照本仓既有形态用源码守）：
    /// ① `finish` 必须按 trace_id 从 agent 暂存取摘要 —— 删掉它，`context_summary` 列会永远空着，
    ///    而本文件其他单测照旧全绿（恒真家族）；
    /// ② `insert` 的贴回 UPDATE 必须按 `trace_id` 定位本行、且只在有摘要时开火；
    /// ③ INSERT 主语句一字未动（共享契约）—— UPDATE 是追加，不是改写。
    #[test]
    fn context_summary_is_taken_in_finish_and_posted_by_trace_id() {
        let src = include_str!("query_log.rs");
        let finish_body = src
            .split("pub fn finish(")
            .nth(1)
            .expect("finish 没了 —— 顺手把这条判据一起改")
            .split("\n}")
            .next()
            .unwrap();
        assert!(
            finish_body.contains("take_context_summary"),
            "finish 没从暂存取摘要 —— context_summary 列会永远空着"
        );
        assert!(finish_body.contains("trace_id"), "必须按 trace_id 取（暂存键就是 trace_id）");
        let insert_body = src.split("async fn insert(").nth(1).expect("insert 没了");
        assert!(
            insert_body.contains("UPDATE meta.query_log SET context_summary"),
            "贴回 UPDATE 没了"
        );
        assert!(insert_body.contains("WHERE trace_id = $2"), "必须按 trace_id 贴回本行");
        assert!(src.contains("sqlx::query(INSERT_SQL)"), "INSERT 主语句必须吃共享常量（一字未动）");
    }
}
