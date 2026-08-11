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
/// 代价是丢失窗口：进程 shutdown 时，在飞任务被 runtime 丢弃连 warn 都没有
/// （只有 insert 明确失败才 warn）——观测行允许丢，业务行不允许，
/// 这正是它只能写观测表的原因。
pub fn finish(
    store: &OwnedStore,
    login: &str,
    question: &str,
    out: &Result<Answer, KbError>,
    obs: &Obs,
    elapsed_ms: u64,
    trace_id: &str,
) -> tokio::task::JoinHandle<()> {
    let e = entry(login, question, out, obs, elapsed_ms, trace_id);
    let store = store.clone();
    tokio::spawn(async move {
        match insert(&store, &e).await {
            // 写 0 行：异常但非错误（INSERT 本身没报错），不留痕就永远没人察觉
            Ok(0) => tracing::warn!("KB 问答落账写入 0 行（异常但非错误）"),
            Ok(_) => {}
            // 错误原文可能带 SQL 片段/绑定值（含用户问题原文）——日志面与落库面同一脱敏口径
            Err(err) => tracing::warn!(
                "KB 问答落账失败（观测降级，不影响回答）: {}",
                qalog::sanitize(&err.to_string())
            ),
        }
    })
}

/// 结果 → 日志行（**纯函数**，单测钉死列语义）。失败也写一行，只是 sql 空、error 有值。
fn entry(
    login: &str,
    question: &str,
    out: &Result<Answer, KbError>,
    obs: &Obs,
    elapsed_ms: u64,
    trace_id: &str,
) -> Entry {
    // 错误文案只物化一次：error 列（sanitize+clip）与 status 分类（timeout_marked）共用
    let err_msg = out.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
    let (sql, rows, error) = match out {
        Ok(a) => (
            retrieval_summary(a),
            citations_of(a).min(i32::MAX as usize) as i32,
            String::new(),
        ),
        Err(_) => (String::new(), 0, qalog::clip(&qalog::sanitize(&err_msg))),
    };
    Entry {
        login_name: login.to_string(),
        question: qalog::clip(question),
        sql,
        row_count: rows,
        elapsed_ms: elapsed_ms.min(i64::MAX as u64) as i64,
        prompt_tokens: clamp_i32(obs.usage.prompt_tokens),
        completion_tokens: clamp_i32(obs.usage.completion_tokens),
        error,
        trace_id: trace_id.to_string(),
        llm_calls: clamp_i32(obs.llm_calls),
        status: status_of(out, &err_msg),
    }
}

/// u32 → i32 饱和写入（同函数只此一份 clamp 口径）
fn clamp_i32(v: u32) -> i32 {
    v.min(i32::MAX as u32) as i32
}

/// 结局分类：取值仅 `qalog::STATUS_*` 四个（audit 端点白名单就按这四个字面值放行）。
/// 文案超时判据与问数侧同一个函数（`qalog::timeout_marked`），不养第二份词表。
/// `err_msg` 收 `entry` 已物化的错误文案（Ok 路径传空串，不会被读到）。
fn status_of(out: &Result<Answer, KbError>, err_msg: &str) -> &'static str {
    match out {
        Ok(_) => qalog::STATUS_SUCCEEDED,
        // 文档 ACL 拒绝（「不可见」也走这条，fail-closed）—— 与问数侧的 ds ACL 拒绝同归 blocked
        Err(KbError::Forbidden(_)) => qalog::STATUS_BLOCKED,
        Err(_) if qalog::timeout_marked(err_msg) => qalog::STATUS_TIMEOUT,
        Err(_) => qalog::STATUS_FAILED,
    }
}

/// `sql` 列的 KB 语义：引用了哪些文档（运营看「答案靠哪几篇」比看整段 markdown 有用）。
/// 无引用（未命中 / 模型没给出带角标的结论）写「无引用」，与失败行的空串区分开。
fn retrieval_summary(a: &Answer) -> String {
    let AnswerBody::Text { citations, .. } = &a.body else {
        // KB 永远只产 Text，此分支当前不可达；若未来新增 body 变体，静默吞摘要会难以察觉
        tracing::warn!("retrieval_summary 遇到非 Text body，sql 摘要落空");
        return String::new();
    };
    if citations.is_empty() {
        return "KB检索：无引用".into();
    }
    // 先按**全名**去重保序（截断后再去重会把「前 60 字相同的两篇」并成一篇、摘要少报），
    // 展示时才截单名 60 字，最多列 8 篇。「引用N篇」的 N 与括号名单同一口径（唯一文档数），
    // 「 等N篇」同——一句话里不混用「条数」与「篇数」两种计数。
    let mut names: Vec<&str> = Vec::new();
    for c in citations {
        let n = c.doc_name.as_str();
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
    let shown = names.len().min(8);
    let list = names[..shown]
        .iter()
        .map(|n| n.chars().take(60).collect::<String>())
        .collect::<Vec<_>>()
        .join("、");
    let mut s = format!("KB检索：引用{}篇（{}）", names.len(), list);
    if names.len() > shown {
        s.push_str(&format!(" 等{}篇", names.len()));
    }
    // clip 是纯防御：8 篇 × 60 字 + 头尾 ≈ 500 字，恒在 CLIP_CHARS 之内（有测试钉着），
    // 真触发也只是摘要被截、「等N篇」后缀可能丢——可接受，不为它换复杂度
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
        // 与 server 同约定：空串落 NULL。trace_id 这里恒有值（filter 只是防外部调用的
        // 防御，本函数是 pub）；conv_id 恒 NULL（KB 问答无会话概念，与问数侧的 CLI/MCP 同形态）
        .bind(Some(e.trace_id.as_str()).filter(|s| !s.is_empty()))
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

    // Citation 夹具与 answer.rs 测试里的 `hit()` 是同一份样板的两处维护——
    // Citation 加字段时两边都要跟，改一边时记得对齐另一边
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
        assert!(src.contains(".bind(qalog::ROUTE_KNOWLEDGE)"), "落账 route 必须吃共享常量（若是改名请同步改本锚点）: {src}");
        assert!(src.contains(".fixed(qalog::INSERT_SQL)"), "INSERT 必须吃共享常量（若是改名请同步改本锚点）: {src}");
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
        assert!(e.sql.contains("KB检索：引用2篇"), "篇数 = 唯一文档数，与名单同口径: {}", e.sql);
        assert!(e.sql.contains("报销制度.pdf") && e.sql.contains("考勤规则.docx"), "{}", e.sql);
        assert!(!e.sql.contains("报销制度.pdf）"), "{}", e.sql);
        assert_eq!(e.sql.matches("报销制度").count(), 1, "同名文档去重: {}", e.sql);
        assert_eq!((e.prompt_tokens, e.completion_tokens, e.llm_calls), (900, 120, 1));
        assert_eq!((e.error.as_str(), e.elapsed_ms), ("", 1234));
        assert_eq!(e.trace_id, "tid-1");
    }

    /// 去重键是文档**全名**：前 60 字相同的两篇是不同的篇，不许被并掉
    #[test]
    fn dedup_uses_full_name_not_truncated_prefix() {
        let p = "共".repeat(61);
        let a1 = format!("{p}上册.pdf");
        let a2 = format!("{p}下册.pdf");
        assert!(a1.chars().count() > 60 && a2.chars().count() > 60);
        assert_eq!(a1.chars().take(60).collect::<String>(), a2.chars().take(60).collect::<String>());
        let out: Result<Answer, KbError> =
            Ok(Answer::text("x[^1][^2]".into(), vec![citation(&a1), citation(&a2)], 0));
        let e = entry("u", "q", &out, &Obs::default(), 1, "t");
        assert!(e.sql.contains("引用2篇"), "前 60 字相同也是两篇: {}", e.sql);
    }

    /// 摘要总长恒在 clip 上限内（8 篇 × 60 字 + 头尾 ≈ 500）：clip 是纯防御，永不真截
    #[test]
    fn summary_never_reaches_clip_limit() {
        let name = "某".repeat(60);
        let cites: Vec<Citation> = (0..20).map(|_| citation(&name)).collect();
        // 同名去重后只剩 1 篇；再造 20 个不同的满长名触发「等N篇」
        let mut cites2: Vec<Citation> = cites;
        for i in 0..20 {
            cites2.push(citation(&format!("{}{i}", "档".repeat(59))));
        }
        let out: Result<Answer, KbError> = Ok(Answer::text("x".into(), cites2, 0));
        let e = entry("u", "q", &out, &Obs::default(), 1, "t");
        assert!(
            e.sql.chars().count() < qalog::CLIP_CHARS,
            "摘要应在 clip 上限内: {}",
            e.sql.chars().count()
        );
        assert!(e.sql.contains("等21篇"), "等N篇后缀应完整保留: {}", e.sql);
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
        let run_at = body.find("= run(").expect("问答链调用点没了（若是改名请同步改本锚点）");
        let log_at = body.find("qa_log::finish").expect("落账调用点没了（答案落定后写账是 Y2 前提；若是改名请同步改本锚点）");
        let wire_at = body.find("a.trace_id = Some(trace_id)").expect("trace_id 没钉进 Answer（若是改名请同步改本锚点）");
        assert!(run_at < log_at && log_at < wire_at, "顺序必须是 问答落定 → 落账 → trace_id 上 wire: {body}");
        // 空问题在生成 trace_id 之前就 400 —— 入参错误不是一次问答结局，不落账
        let empty_at = body.find("q.is_empty()").expect("空问题闸没了（若是改名请同步改本锚点）");
        let tid_at = body.find("Uuid::new_v4").expect("trace_id 生成点没了（若是改名请同步改本锚点）");
        assert!(empty_at < tid_at, "空问题必须先于落账链被拦下: {body}");
    }
}
