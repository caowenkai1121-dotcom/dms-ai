//! 【Y2】KB 问答落账：一次知识问答一行 `meta.query_log`（`route='knowledge'`）。
//! 变更原因＝可观测口径与反馈闭环：不落账，KB 问答既绑不上反馈（`meta.query_feedback`
//! 按 `trace_id` + 本人绑 `query_log`）也不进 usage/质量统计 —— 知识运营的飞轮断在第一环。
//!
//! 三条纪律与 server 的问数落账（`query_log.rs`）同款：
//! 1. **观测绝不进主链路**：`finish` 返回 spawn 句柄而自己不 await，失败只 warn。
//! 2. 截断/脱敏/超时判据只有一份实现：`dms_kernel::qalog`（本文件不复制第二份）。
//! 3. INSERT 列清单只有一份：`qalog::INSERT_SQL`（与问数侧同一条语句）。
//!
//! ⚠️ crate 纪律「不碰 `meta.*`」的**唯一例外**：这张表是 server 的观测表（只写不读），
//! 不是 semantic 的注册表域。例外只许这一条 INSERT，别顺着它往里加第二张表。

use dms_connector::owned::OwnedStore;
use dms_kernel::qalog;
use dms_kernel::{Answer, AnswerBody};

use crate::KbError;

/// 一次 KB 问答的观测产出（`respond` 顺手带出来，不许事后二次推导）。
/// 无命中路径不调 LLM（全 0 —— `llm_calls=0` 正是「这发没烧钱」的指纹）。
#[derive(Default)]
pub struct Obs {
    pub usage: dms_kernel::llm::Usage,
    pub llm_calls: u32,
}

impl Obs {
    /// 打过一发 LLM（成败都算 —— 失败那发的钱也已经花了，用量拿不到记 0）
    pub fn called() -> Self {
        Self { usage: Default::default(), llm_calls: 1 }
    }
}

/// 待写入的一行（KB 语义）。字段绑定顺序 = `qalog::INSERT_SQL` 的列顺序。
struct Entry {
    login_name: String,
    question: String,
    /// `sql` 列写检索摘要（KB 没有 SQL）：引用的文档清单；失败行空串（与问数侧同口径）
    sql: String,
    /// `row_count` 列写引用条数（答案靠几篇资料，KB 侧最接近「行数」的语义）
    row_count: i32,
    elapsed_ms: i64,
    prompt_tokens: i32,
    completion_tokens: i32,
    error: String,
    trace_id: String,
    /// 本轮打了几发 LLM（0 = 无命中没调用，1 = 调了一发 —— 成败都算）
    llm_calls: i32,
    status: &'static str,
}

/// 答案落定后的唯一写口（调用点＝`answer` 的出口，成功与失败都写）。
/// 返回写入句柄而**自己不 await**：server 长驻进程把句柄丢弃即可
/// （fire-and-forget，主链一个 `.await` 都不多 —— 纪律 1）。
pub fn finish(
    store: &OwnedStore,
    login: &str,
    question: &str,
    out: &Result<Answer, KbError>,
    obs: &Obs,
    elapsed_ms: u128,
    trace_id: &str,
) -> tokio::task::JoinHandle<()> {
    let e = entry(login, question, out, obs, elapsed_ms, trace_id);
    let store = store.clone();
    tokio::spawn(async move {
        if let Err(err) = insert(&store, &e).await {
            tracing::warn!("KB 问答落账失败（观测降级，不影响回答）: {err}");
        }
    })
}

/// 结果 → 日志行（**纯函数**，单测钉死列语义）。失败也写一行，只是 sql 空、error 有值。
fn entry(
    login: &str,
    question: &str,
    out: &Result<Answer, KbError>,
    obs: &Obs,
    elapsed_ms: u128,
    trace_id: &str,
) -> Entry {
    let (sql, rows, error) = match out {
        Ok(a) => (retrieval_summary(a), citations_of(a) as i32, String::new()),
        Err(e) => (String::new(), 0, qalog::clip(&qalog::sanitize(&e.to_string()))),
    };
    Entry {
        login_name: login.to_string(),
        question: qalog::clip(question),
        sql,
        row_count: rows,
        elapsed_ms: elapsed_ms.min(i64::MAX as u128) as i64,
        prompt_tokens: obs.usage.prompt_tokens.min(i32::MAX as u32) as i32,
        completion_tokens: obs.usage.completion_tokens.min(i32::MAX as u32) as i32,
        error,
        trace_id: trace_id.to_string(),
        llm_calls: obs.llm_calls.min(i32::MAX as u32) as i32,
        status: status_of(out),
    }
}

/// 结局分类：取值仅 `qalog::STATUS_*` 四个（audit 端点白名单就按这四个字面值放行）。
/// 文案超时判据与问数侧同一个函数（`qalog::timeout_marked`），不养第二份词表。
fn status_of(out: &Result<Answer, KbError>) -> &'static str {
    match out {
        Ok(_) => qalog::STATUS_SUCCEEDED,
        // 文档 ACL 拒绝（「不可见」也走这条，fail-closed）—— 与问数侧的 ds ACL 拒绝同归 blocked
        Err(KbError::Forbidden(_)) => qalog::STATUS_BLOCKED,
        Err(e) if qalog::timeout_marked(&e.to_string()) => qalog::STATUS_TIMEOUT,
        Err(_) => qalog::STATUS_FAILED,
    }
}

/// `sql` 列的 KB 语义：引用了哪些文档（运营看「答案靠哪几篇」比看整段 markdown 有用）。
/// 无引用（未命中 / 模型没给出带角标的结论）写「无引用」，与失败行的空串区分开。
fn retrieval_summary(a: &Answer) -> String {
    let AnswerBody::Text { citations, .. } = &a.body else { return String::new() };
    if citations.is_empty() {
        return "KB检索：无引用".into();
    }
    // 文档去重保序，单名截 60 字，最多列 8 篇；总量再由 clip 兜底
    let mut names: Vec<String> = Vec::new();
    for c in citations {
        let n: String = c.doc_name.chars().take(60).collect();
        if !n.is_empty() && !names.iter().any(|x| *x == n) {
            names.push(n);
        }
    }
    let shown = names.len().min(8);
    let mut s = format!("KB检索：引用{}篇（{}）", citations.len(), names[..shown].join("、"));
    if names.len() > shown {
        s.push_str(&format!(" 等{}篇", names.len()));
    }
    qalog::clip(&s)
}

fn citations_of(a: &Answer) -> usize {
    match &a.body {
        AnswerBody::Text { citations, .. } => citations.len(),
        _ => 0,
    }
}

async fn insert(store: &OwnedStore, e: &Entry) -> Result<u64, dms_connector::ConnectorError> {
    store
        .fixed(qalog::INSERT_SQL)
        .bind(&e.login_name)
        // ds_id 空串：KB 文档问答无数据源（`upload_%` 是上传表问数那条通道的指纹，不混）
        .bind("")
        .bind(qalog::ROUTE_KNOWLEDGE)
        .bind(&e.question)
        .bind(&e.sql)
        .bind(e.row_count)
        .bind(e.elapsed_ms)
        // cache_hit 恒 false：语义缓存只存在于问数链（同 `query_log::is_cache_hit` 口径）
        .bind(false)
        .bind(e.prompt_tokens)
        .bind(e.completion_tokens)
        .bind(&e.error)
        // 与 server 同约定：空串才落 NULL。trace_id 这里恒有值；conv_id 恒 NULL
        // （KB 问答无会话概念，与问数侧的 CLI/MCP 同形态）
        .bind(Some(&e.trace_id))
        .bind(None::<&str>)
        .bind(e.llm_calls)
        .bind(e.status)
        .execute()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use dms_kernel::Citation;

    fn citation(doc_name: &str) -> Citation {
        Citation {
            doc_id: "d1".into(),
            doc_name: doc_name.into(),
            chunk_id: 1,
            page: None,
            heading_path: String::new(),
            score: 0.5,
            folder_path: String::new(),
            relations: vec![],
            tags: vec![],
            business_domain: None,
            effective_from: None,
            effective_to: None,
            source_uri: None,
            document_family: None,
            document_revision: None,
            source_hash: String::new(),
            doc_updated_at: String::new(),
            channels: vec![],
            span: None,
        }
    }

    fn obs_used() -> Obs {
        Obs { usage: dms_kernel::llm::Usage { prompt_tokens: 900, completion_tokens: 120 }, llm_calls: 1 }
    }

    /// route 取值是反馈闭环的前提：wire 上的 `Answer.route` 与落账的 route 必须同一常量
    #[test]
    fn route_value_is_the_feedback_loop_premise() {
        assert_eq!(qalog::ROUTE_KNOWLEDGE, "knowledge");
        assert_eq!(Answer::text("x".into(), vec![], 0).route, qalog::ROUTE_KNOWLEDGE);
        let src = include_str!("qa_log.rs");
        assert!(src.contains(".bind(qalog::ROUTE_KNOWLEDGE)"), "落账 route 必须吃共享常量: {src}");
        assert!(src.contains(".fixed(qalog::INSERT_SQL)"), "INSERT 必须吃共享常量: {src}");
    }

    /// 成功行：引用条数进 row_count、文档名去重进 sql 摘要、token/llm_calls 照记
    #[test]
    fn succeeded_entry_maps_answer_fields() {
        let a = Answer::text(
            "正文[^1][^2]".into(),
            vec![citation("报销制度.pdf"), citation("考勤规则.docx"), citation("报销制度.pdf")],
            0,
        );
        let out: Result<Answer, KbError> = Ok(a);
        let e = entry("zhangsan", "报销上限", &out, &obs_used(), 1234, "tid-1");
        assert_eq!(e.status, qalog::STATUS_SUCCEEDED);
        assert_eq!(e.row_count, 3, "row_count = 引用条数");
        assert!(e.sql.contains("KB检索：引用3篇"), "{}", e.sql);
        assert!(e.sql.contains("报销制度.pdf") && e.sql.contains("考勤规则.docx"), "{}", e.sql);
        assert!(!e.sql.contains("报销制度.pdf）"), "{}", e.sql);
        assert_eq!(e.sql.matches("报销制度").count(), 1, "同名文档去重: {}", e.sql);
        assert_eq!((e.prompt_tokens, e.completion_tokens, e.llm_calls), (900, 120, 1));
        assert_eq!((e.error.as_str(), e.elapsed_ms), ("", 1234));
        assert_eq!(e.trace_id, "tid-1");
    }

    /// 无命中/无引用也是 succeeded（问答本身成功），摘要写「无引用」与失败行区分；
    /// 无命中路径不调 LLM：Obs 全 0
    #[test]
    fn no_hit_entry_is_succeeded_with_zero_obs() {
        let out: Result<Answer, KbError> = Ok(Answer::text("知识库里没有相关内容。".into(), vec![], 0));
        let e = entry("u", "月球基地报销", &out, &Obs::default(), 50, "t");
        assert_eq!(e.status, qalog::STATUS_SUCCEEDED);
        assert_eq!(e.sql, "KB检索：无引用");
        assert_eq!((e.row_count, e.prompt_tokens, e.llm_calls), (0, 0, 0));
    }

    /// 失败问答也落一行：status 分类（blocked/timeout/failed）+ error 脱敏，sql 空串
    #[test]
    fn failed_entry_classifies_status_and_sanitizes() {
        let case = |e: KbError| {
            let out: Result<Answer, KbError> = Err(e);
            entry("u", "q", &out, &Obs::default(), 1, "t")
        };
        let b = case(KbError::Forbidden("文档 d1 不可见".into()));
        assert_eq!(b.status, qalog::STATUS_BLOCKED);
        let t = case(KbError::Upstream("大模型：LLM 请求失败: operation timed out".into()));
        assert_eq!(t.status, qalog::STATUS_TIMEOUT, "上游超时的文案形态");
        let t2 = case(KbError::Db("超时 [kb] 等待 30.0s 未返回".into()));
        assert_eq!(t2.status, qalog::STATUS_TIMEOUT);
        let f = case(KbError::Upstream("大模型：连接失败 password=hunter2".into()));
        assert_eq!(f.status, qalog::STATUS_FAILED);
        assert!(f.error.contains("password=***") && !f.error.contains("hunter2"), "凭据不许落库: {}", f.error);
        assert_eq!(f.sql, "", "失败行 sql 空串（与问数侧同口径）");
        assert_eq!(f.row_count, 0);
    }

    /// 落账时机（源锚点）：`answer` 必须先等问答链落定（`run`）再 `qa_log::finish`，
    /// trace_id 在落账之后钉进 Answer —— 顺序倒一个，反馈就绑不上当次问答
    #[test]
    fn logging_happens_after_answer_settles_then_trace_on_wire() {
        let src = include_str!("answer.rs");
        let body = src.split("pub async fn answer(").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let run_at = body.find("= run(").expect("问答链调用点没了");
        let log_at = body.find("qa_log::finish").expect("落账调用点没了（答案落定后写账是 Y2 前提）");
        let wire_at = body.find("a.trace_id = Some(trace_id)").expect("trace_id 没钉进 Answer");
        assert!(run_at < log_at && log_at < wire_at, "顺序必须是 问答落定 → 落账 → trace_id 上 wire: {body}");
        // 空问题在生成 trace_id 之前就 400 —— 入参错误不是一次问答结局，不落账
        let empty_at = body.find("q.is_empty()").expect("空问题闸没了");
        let tid_at = body.find("Uuid::new_v4").expect("trace_id 生成点没了");
        assert!(empty_at < tid_at, "空问题必须先于落账链被拦下: {body}");
    }
}
