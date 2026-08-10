//! 【KB-EVAL】Yuxi 式 RAG 评估 run 管理。变更原因＝评估协议。
//!
//! 调研：`docs/research/yuxi.json` B8「RAG 评估闭环」+ `target/tmp/research/Yuxi` 的
//! `knowledge_eval_router.py` / `eval/benchmark_generation.py` / `eval/evaluator.py`。
//! 与 Yuxi 的两处刻意差异：不建独立数据集表（题库一次性：抽样→出题即评）；gold 块就是
//! 抽样锚块本身——不让 LLM 回指 chunk_id（它指回来的东西没法证真），评审用金标准原文而非
//! gold_answer（少一次 LLM 调用，也少一层「答案像不像参考答案」的主观度）。
//!
//! ## 端点契约
//!
//! - `POST /api/kb/eval/runs`  body `{space_id?, sample_size?=20, login_name?, role_code?}`
//!   → `{id, space_id, status:"running", total, done:0}`。
//!   `space_id` 缺省＝登录名（个人空间，与 `kb_api::space_of` 同约定）；`sample_size` 夹到
//!   [1, 100]。后台任务：均匀抽样 → fast 出题 → 逐题 `retrieve::search_report` 真实检索 +
//!   `answer::answer` 生成答案 + fast judge → 落库。
//!   401 未认证 / 403 空间不可读 / 429 并发满（`EVAL_PERMITS` 个）。
//! - `GET /api/kb/eval/runs?space_id=` → `{space_id, runs:[{id, space_id, status, total, done,
//!   gen_failed, judge_failed, recall1, recall3, recall5, recall10, answer_acc, elapsed_ms,
//!   error, created_at}]}`（id 倒序，≤50 条；space_id 同样缺省个人空间）。
//! - `GET /api/kb/eval/runs/{id}` → run 全字段 + `items:[{ord, question, gold_chunk_id,
//!   generated_answer, r1, r3, r5, r10, judge, judge_reason, error}]`。
//!   不存在与不可见同返 403 —— id 是可枚举序列，404/403 分开就是他人 run 的存在性探针。
//!
//! ## 指标口径（纯函数实现 + 单测钉死，是本文件的唯一事实源）
//!
//! - `r1/r3/r5/r10`：金标准块是否进最终命中前 k。相邻合并的命中锚在**首块**
//!   （`retrieve::merge_adjacent`），金块被并进去时 chunk_id 对不上但内容确实进了 prompt ——
//!   按「同文档 + ord ∈ [hit.ord, hit.ord+merged)」也算命中。检索端 `TOP_K=6`，故 recall@10
//!   ≈「进入最终命中列表」，这是产品口径不是评测退化。
//! - run 级 `recall@k`：检索成功题里 r_k 为真的占比（检索失败题 r 旗 NULL，不进分母）。
//! - `answer_acc`：correct=1 / partial=0.5 / wrong=0 的均值（分母＝评审成功题数）。
//! - `total`＝目标题数（sample_size 入参）；语料不足时 `done + gen_failed < total`。
//!
//! ## 失败纪律
//!
//! 出题/评审失败**计数继续**（`gen_failed` / `judge_failed`）；检索/答案失败记 `item.error`
//! 照跑；只有明细落库失败才把整跑标 `failed` —— 结果存不下来，跑完也是假绿（本仓反
//! 「判据恒真」家族）。进度回写失败只 warn：终态 UPDATE 还会覆盖写一次。
//!
//! ## 权限
//!
//! 建/看都要求空间可读，闸是 `dms_knowledge::acl::space_readable`（viewer 谓词内联在
//! knowledge 的 SQL 里；server 不复述 ACL 片段，同 `kb_api` 的约定）。后台任务持**创建者**
//! Viewer 跑检索/问答——逐题仍过 `search_report` 的完整 ACL；run 产物对空间可见者公开
//! （内容是该空间语料的派生物）。
//!
//! ## 装配层注册（父任务落 main.rs；本文件不许自己接线）
//!
//! ```rust,ignore
//! mod kb_eval_api;
//! // bootstrap_meta：`kb_eval_api::migrate(owned).await?;` 排在 `query_log::migrate`
//! // 之后（meta schema 由 query_log 建，与 quality_api 同约定；入参是 &OwnedStore）。
//! .route("/api/kb/eval/runs", get(kb_eval_api::list_runs).post(kb_eval_api::create_run))
//! .route("/api/kb/eval/runs/{id}", get(kb_eval_api::get_run))
//! ```

use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use dms_connector::owned::OwnedStore;
use dms_kernel::{ChatModel, ChatRequest, ModelTier};
use dms_knowledge::{acl, answer, retrieve, Viewer};
use std::sync::Arc;

type ApiErr = (StatusCode, Json<serde_json::Value>);
type ApiOk = Json<serde_json::Value>;

/// 响应体沿用现有 `{"error": msg}` 形状（前端只认这一种，与 kb_api 一致）
fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// 幂等 DDL（每次启动都跑，`IF NOT EXISTS` 兜底重入）。逐条独立字面量是为了能走
/// `OwnedStore::fixed()` 通道（只收 `&'static str`），不像 quality_api 那样落 sqlx::query。
const DDL: [&str; 4] = [
    "CREATE TABLE IF NOT EXISTS meta.kb_eval_runs(\
       id bigserial PRIMARY KEY,\
       space_id text NOT NULL,\
       created_by text NOT NULL,\
       status text NOT NULL DEFAULT 'running' CHECK (status IN ('running','done','failed')),\
       total int NOT NULL,\
       done int NOT NULL DEFAULT 0,\
       gen_failed int NOT NULL DEFAULT 0,\
       judge_failed int NOT NULL DEFAULT 0,\
       recall1 float8, recall3 float8, recall5 float8, recall10 float8,\
       answer_acc float8,\
       error text NOT NULL DEFAULT '',\
       elapsed_ms bigint NOT NULL DEFAULT 0,\
       created_at timestamptz NOT NULL DEFAULT now(),\
       finished_at timestamptz)",
    "CREATE INDEX IF NOT EXISTS idx_kb_eval_runs_space ON meta.kb_eval_runs(space_id, id DESC)",
    "CREATE TABLE IF NOT EXISTS meta.kb_eval_items(\
       id bigserial PRIMARY KEY,\
       run_id bigint NOT NULL REFERENCES meta.kb_eval_runs(id) ON DELETE CASCADE,\
       ord int NOT NULL,\
       question text NOT NULL,\
       gold_chunk_id bigint NOT NULL,\
       generated_answer text NOT NULL DEFAULT '',\
       r1 boolean, r3 boolean, r5 boolean, r10 boolean,\
       judge text NOT NULL DEFAULT '' CHECK (judge IN ('','correct','partial','wrong')),\
       judge_reason text NOT NULL DEFAULT '',\
       error text NOT NULL DEFAULT '',\
       created_at timestamptz NOT NULL DEFAULT now())",
    "CREATE INDEX IF NOT EXISTS idx_kb_eval_items_run ON meta.kb_eval_items(run_id, ord)",
];

/// 幂等建表。meta schema 由 `query_log::migrate` 建，本函数必须排在它之后。
pub async fn migrate(store: &OwnedStore) -> anyhow::Result<()> {
    for sql in DDL {
        store.fixed(sql).execute().await?;
    }
    Ok(())
}

/// 重启收割：上次进程退出时仍 `running` 的 run 永远等不到终态回写（后台任务随进程死了），
/// 启动时统一标 `failed`（'interrupted' 写不进去——CHECK 只收 running/done/failed，
/// 中断语义由 error 文案承担）。只动 `status='running'` 的行；本表无 `updated_at`，
/// 终态时刻落在 `finished_at`（与 `run_eval` 的两条收尾 UPDATE 同形状）。
const REAP_SQL: &str =
    "UPDATE meta.kb_eval_runs SET status='failed', error='服务重启中断', finished_at=now() \
     WHERE status='running'";

/// 服务启动收割被重启中断的评估 run。幂等：无 running 行时影响 0 行。返回收割行数。
pub async fn reap_interrupted(store: &OwnedStore) -> anyhow::Result<u64> {
    let n = store.fixed(REAP_SQL).execute().await?;
    if n > 0 {
        tracing::info!(reaped = n, "重启收割：被中断的评估 run 已标 failed");
    }
    Ok(n)
}

/// 出题语料抽样（`ORDER BY random()` ＝ 无放回均匀抽样；单空间块量有界，全量排序可接受）。
/// 文档资格 conjunct 与 `dms_knowledge::retrieve::visible_sql` **逐字同构**
/// （enabled/status/生效期）——检索侧认的文档出题侧才许抽：那边改口径这里不跟，就会出
/// 「抽得到检不到」（恒红）或「检得到抽不到」（恒绿）的题。`length >= 40` 滤掉标题残块。
/// ACL 不在此复述：空间可读闸已在 `create_run` 跑过，逐题检索仍按创建者身份过完整 ACL。
const SAMPLE_SQL: &str =
    "SELECT c.chunk_id, c.doc_id, c.ord, d.name AS doc_name, c.heading_path, c.text \
     FROM kb.chunk c JOIN kb.doc d ON d.doc_id = c.doc_id \
     WHERE d.space_id=$1 AND d.enabled=true AND d.status IN ('chunked','embedded') \
       AND (d.effective_from IS NULL OR d.effective_from <= CURRENT_DATE) \
       AND (d.effective_to IS NULL OR d.effective_to >= CURRENT_DATE) \
       AND length(c.text) >= 40 \
     ORDER BY random() LIMIT $2";

/// run 行的取列口径（与 `RunRow` 逐字同序，两处不许分叉）。
macro_rules! run_cols {
    () => {
        "id,space_id,status,total,done,gen_failed,judge_failed,\
         recall1,recall3,recall5,recall10,answer_acc,elapsed_ms,error,created_at"
    };
}

/// 列序 = `run_cols!()`。15 元组在 sqlx 手写 FromRow 的上限（16）内。
type RunRow = (
    i64, String, String, i32, i32, i32, i32,
    Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>,
    i64, String, chrono::DateTime<chrono::Utc>,
);

/// 列序：ord,question,gold_chunk_id,generated_answer,r1,r3,r5,r10,judge,judge_reason,error。
type ItemRow = (
    i32, String, i64, String,
    Option<bool>, Option<bool>, Option<bool>, Option<bool>,
    String, String, String,
);

const DEFAULT_SAMPLE: i32 = 20;
const MAX_SAMPLE: i32 = 100;
/// 评估并发闸：一跑 = sample_size × (出题+答案+评审) 次 LLM 调用。拿不到许可**直接 429
/// 而不排队**（同 `kb_api::UPLOAD_GATE` 的理由：排队只是把压力推迟到队列长度上）。
const EVAL_PERMITS: usize = 2;
static EVAL_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(EVAL_PERMITS);

const MIN_QUESTION_CHARS: usize = 4;
const MAX_QUESTION_CHARS: usize = 200;
const MAX_REASON_CHARS: usize = 200;
/// 出题给模型看的片段上限（块可能上千字，全塞既贵又稀释锚点）
const GEN_CHUNK_CHARS: usize = 1200;
const JUDGE_GOLD_CHARS: usize = 1500;
const JUDGE_ANSWER_CHARS: usize = 2000;
/// 答案落库上限（评估留痕够看即可，原文可经 `/api/kb/chunk/{id}` 回查）
const ANSWER_STORE_CHARS: usize = 4000;

const GEN_SYSTEM: &str = "你是知识库评测的出题器。根据给定片段出一个**仅凭该片段就能准确回答**的事实性问题。\n\
要求：\n\
1. 问题是完整自洽的中文疑问句；可带文档名/栏目名使其自包含，但不许出现「本文」「该片段」「上述」这类指代；\n\
2. 答案必须确实在片段里，不许出需要外部知识、计算或多片段综合的题；\n\
3. 只输出一个 JSON 对象：{\"question\":\"...\"}，不要输出任何其他文字。";

const JUDGE_SYSTEM: &str = "你是 RAG 答案评审。给定问题、金标准原文片段和系统生成的答案，判定答案质量。\n\
口径：\n\
- correct：答案与金标准原文事实一致，完整回答了问题；\n\
- partial：部分正确或不够完整；\n\
- wrong：事实错误、答非所问、凭空捏造，或声称无法回答但金标准原文其实能答。\n\
只输出一个 JSON 对象：{\"verdict\":\"correct|partial|wrong\",\"reason\":\"一句话理由\"}，不要输出任何其他文字。";

#[derive(serde::Deserialize, Default)]
pub struct CreateRunReq {
    space_id: Option<String>,
    sample_size: Option<i32>,
    login_name: Option<String>,
    role_code: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct EvalListQuery {
    space_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `GET /api/kb/eval/runs/{id}` 的 query。**不用 `#[serde(flatten)]`**：Query 走
/// serde_urlencoded，flatten 在那边直接报 unsupported（同 kb_api 的教训）。
#[derive(serde::Deserialize, Default)]
pub struct EvalIdQuery {
    login_name: Option<String>,
    role_code: Option<String>,
}

/// 评审结论。落库/上线的字符串是英文三值；解析侧容忍中文与同义词（见 `parse_verdict`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Correct,
    Partial,
    Wrong,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Correct => "correct",
            Verdict::Partial => "partial",
            Verdict::Wrong => "wrong",
        }
    }

    /// 答案准确率权重：partial 半对。改这里就是改口径——`answer_accuracy_weights_partial_half` 钉着。
    fn score(self) -> f64 {
        match self {
            Verdict::Correct => 1.0,
            Verdict::Partial => 0.5,
            Verdict::Wrong => 0.0,
        }
    }
}

/// 身份：Bearer 会话 token 优先，回退 login_name（与 `/api/kb/*` 同一个 `resolve_identity`——
/// 认证回退是配置开关，必须经它收口，不能在这里自报家门）。
async fn eval_viewer(
    st: &AppState,
    headers: &HeaderMap,
    login_name: &Option<String>,
    role_code: &Option<String>,
) -> Result<Viewer, ApiErr> {
    let (login, role) = crate::resolve_identity(st, headers, login_name, role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let p = crate::auth::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|_| err(StatusCode::FORBIDDEN, "当前 DMS 身份或角色不可用"))?;
    Ok(Viewer::new(p.login_name, vec![p.role_code]))
}

/// 空间可读闸的 Err 只可能是 Db 变体（count 查询）——映 500 固定文案，不带连接信息。
async fn require_readable(st: &AppState, v: &Viewer, space_id: &str) -> Result<(), ApiErr> {
    let readable = acl::space_readable(&st.owned, v, space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
    if !readable {
        return Err(err(StatusCode::FORBIDDEN, format!("无权访问知识空间 {space_id}")));
    }
    Ok(())
}

/// `space_id` 缺省＝个人空间；只挡超长（存在性由可读闸判，bind 传参无注入面）。
fn normalize_space(v: &Viewer, space_id: Option<&str>) -> Result<String, ApiErr> {
    let s = space_id.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&v.login);
    if s.chars().count() > 64 {
        return Err(err(StatusCode::BAD_REQUEST, "space_id 不能超过 64 字符"));
    }
    Ok(s.to_string())
}

pub async fn create_run(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateRunReq>,
) -> Result<ApiOk, ApiErr> {
    let v = eval_viewer(&st, &headers, &req.login_name, &req.role_code).await?;
    let space_id = normalize_space(&v, req.space_id.as_deref())?;
    let sample_size = req.sample_size.unwrap_or(DEFAULT_SAMPLE).clamp(1, MAX_SAMPLE);
    require_readable(&st, &v, &space_id).await?;
    // 许可在落库**之前**：被拒的并发不许留一行永远 running 的孤儿。
    let permit = EVAL_GATE.try_acquire().map_err(|_| {
        err(
            StatusCode::TOO_MANY_REQUESTS,
            format!("评估并发已满（同时最多 {EVAL_PERMITS} 个），请稍后重试"),
        )
    })?;
    let (id,) = st
        .owned
        .fixed(
            "INSERT INTO meta.kb_eval_runs(space_id,created_by,total) VALUES($1,$2,$3) RETURNING id",
        )
        .bind(&space_id)
        .bind(&v.login)
        .bind(sample_size)
        .fetch_optional::<(i64,)>()
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "评估任务创建失败"))?;
    let body = serde_json::json!({
        "id": id,
        "space_id": &space_id,
        "status": "running",
        "total": sample_size,
        "done": 0,
    });
    // 后台跑全量评估；permit 随任务持有，跑完才放。
    tokio::spawn(run_eval(st.clone(), id, v, space_id, sample_size, permit));
    Ok(Json(body))
}

pub async fn list_runs(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<EvalListQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = eval_viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let space_id = normalize_space(&v, q.space_id.as_deref())?;
    require_readable(&st, &v, &space_id).await?;
    let rows = st
        .owned
        .fixed(concat!(
            "SELECT ", run_cols!(),
            " FROM meta.kb_eval_runs WHERE space_id=$1 ORDER BY id DESC LIMIT 50",
        ))
        .bind(&space_id)
        .fetch_all::<RunRow>()
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
    Ok(Json(serde_json::json!({
        "space_id": space_id,
        "runs": rows.iter().map(run_json).collect::<Vec<_>>(),
    })))
}

pub async fn get_run(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<EvalIdQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = eval_viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let run = st
        .owned
        .fixed(concat!("SELECT ", run_cols!(), " FROM meta.kb_eval_runs WHERE id=$1"))
        .bind(id)
        .fetch_optional::<RunRow>()
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
    // 不存在与不可见同一文案：run id 是可枚举的序列，两种回包分开就是存在性探针
    // （同 `acl::doc_for_viewer` 的论证）。
    let Some(run) = run else {
        return Err(err(StatusCode::FORBIDDEN, format!("评估任务 {id} 不可见")));
    };
    let readable = acl::space_readable(&st.owned, &v, &run.1)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
    if !readable {
        return Err(err(StatusCode::FORBIDDEN, format!("评估任务 {id} 不可见")));
    }
    let items = st
        .owned
        .fixed(
            "SELECT ord,question,gold_chunk_id,generated_answer,r1,r3,r5,r10,judge,judge_reason,error \
             FROM meta.kb_eval_items WHERE run_id=$1 ORDER BY ord",
        )
        .bind(id)
        .fetch_all::<ItemRow>()
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
    let mut body = run_json(&run);
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "items".into(),
            items.iter().map(item_json).collect::<Vec<_>>().into(),
        );
    }
    Ok(Json(body))
}

fn run_json(r: &RunRow) -> serde_json::Value {
    let (id, space_id, status, total, done, gen_failed, judge_failed,
         r1, r3, r5, r10, acc, elapsed_ms, error, created_at) = r;
    serde_json::json!({
        "id": id,
        "space_id": space_id,
        "status": status,
        "total": total,
        "done": done,
        "gen_failed": gen_failed,
        "judge_failed": judge_failed,
        "recall1": r1,
        "recall3": r3,
        "recall5": r5,
        "recall10": r10,
        "answer_acc": acc,
        "elapsed_ms": elapsed_ms,
        "error": error,
        "created_at": created_at.to_rfc3339(),
    })
}

fn item_json(r: &ItemRow) -> serde_json::Value {
    let (ord, question, gold_chunk_id, generated, r1, r3, r5, r10, judge, reason, error) = r;
    serde_json::json!({
        "ord": ord,
        "question": question,
        "gold_chunk_id": gold_chunk_id,
        "generated_answer": generated,
        "r1": r1,
        "r3": r3,
        "r5": r5,
        "r10": r10,
        // 未评审（评审失败/答案失败）是 NULL，不是空串——空串是「评了但没写结论」
        "judge": if judge.is_empty() { None } else { Some(judge.as_str()) },
        "judge_reason": reason,
        "error": error,
    })
}

// ============================ 后台评估管道 ============================

/// 一题的过程态：检索/答案/评审各自独立记，哪步失败都不拖垮整跑。
struct EvalItem {
    question: String,
    answer: String,
    recall: Option<[bool; 4]>,
    verdict: Option<Verdict>,
    reason: String,
    error: String,
}

struct RunMetrics {
    done: i32,
    gen_failed: i32,
    judge_failed: i32,
    recall: [Option<f64>; 4],
    answer_acc: Option<f64>,
}

async fn run_eval(
    st: Arc<AppState>,
    run_id: i64,
    v: Viewer,
    space_id: String,
    sample_size: i32,
    _permit: tokio::sync::SemaphorePermit<'static>,
) {
    let t0 = std::time::Instant::now();
    let out = run_eval_inner(&st, run_id, &v, &space_id, sample_size).await;
    let elapsed_ms = t0.elapsed().as_millis() as i64;
    let written = match &out {
        Ok(m) => {
            st.owned
                .fixed(
                    "UPDATE meta.kb_eval_runs SET status='done', done=$2, gen_failed=$3, judge_failed=$4, \
                     recall1=$5, recall3=$6, recall5=$7, recall10=$8, answer_acc=$9, elapsed_ms=$10, \
                     finished_at=now() WHERE id=$1",
                )
                .bind(run_id)
                .bind(m.done)
                .bind(m.gen_failed)
                .bind(m.judge_failed)
                .bind(m.recall[0])
                .bind(m.recall[1])
                .bind(m.recall[2])
                .bind(m.recall[3])
                .bind(m.answer_acc)
                .bind(elapsed_ms)
                .execute()
                .await
                .is_ok()
        }
        Err(msg) => {
            st.owned
                .fixed(
                    "UPDATE meta.kb_eval_runs SET status='failed', error=$2, elapsed_ms=$3, \
                     finished_at=now() WHERE id=$1",
                )
                .bind(run_id)
                .bind(clip(msg, 500))
                .bind(elapsed_ms)
                .execute()
                .await
                .is_ok()
        }
    };
    if !written {
        tracing::warn!(run_id, "评估终态回写失败（run 行停在 running，明细已落库的不受影响）");
    }
    match &out {
        Ok(m) => tracing::info!(
            run_id,
            done = m.done,
            gen_failed = m.gen_failed,
            judge_failed = m.judge_failed,
            elapsed_ms,
            "知识库评估完成"
        ),
        Err(msg) => tracing::warn!(run_id, reason = %msg, "知识库评估失败"),
    }
}

async fn run_eval_inner(
    st: &AppState,
    run_id: i64,
    v: &Viewer,
    space_id: &str,
    sample_size: i32,
) -> Result<RunMetrics, String> {
    let chunks = sample_chunks(st, space_id, sample_size).await?;
    if chunks.is_empty() {
        // 0 题不许给绿（本仓反空转闸）：语料不足是**可处置的失败**，不是「全对」。
        return Err(format!(
            "空间 {space_id} 没有可用于出题的文本块（需文档已分块、启用且在生效期内）"
        ));
    }
    let mut flags: Vec<[bool; 4]> = Vec::new();
    let mut verdicts: Vec<Verdict> = Vec::new();
    let mut done = 0i32;
    let mut gen_failed = 0i32;
    let mut judge_failed = 0i32;
    for (idx, (gold_chunk, gold_doc, gold_ord, doc_name, heading, text)) in chunks.iter().enumerate() {
        let ord = idx as i32 + 1;
        // ① 出题（fast）。失败：计数继续，不整跑中断。
        let Some(question) = gen_question(&st.llm, doc_name, heading, text).await else {
            gen_failed += 1;
            progress(st, run_id, done, gen_failed, judge_failed).await;
            continue;
        };
        let mut item = EvalItem {
            question,
            answer: String::new(),
            recall: None,
            verdict: None,
            reason: String::new(),
            error: String::new(),
        };
        // ② 真实检索（创建者身份 + 空间限定，ACL 在 search_report 里内联）
        match retrieve::search_report(&st.owned, &st.embed, v, Some(space_id), &item.question, &st.cfg().kb_rrf_weights).await {
            Ok(report) => {
                let shaped: Vec<(i64, &str, i32, u32)> = report
                    .hits
                    .iter()
                    .map(|h| (h.chunk_id, h.doc_id.as_str(), h.ord, h.merged))
                    .collect();
                let f = recall_at(gold_rank(&shaped, *gold_chunk, gold_doc, *gold_ord));
                flags.push(f);
                item.recall = Some(f);
            }
            Err(e) => item.error = format!("检索失败：{e}"),
        }
        // ③ 答案生成：检索已失败就别再烧一次（answer 内部会重检一遍，大概率同样失败）
        if item.error.is_empty() {
            match answer::answer(&st.owned, &st.embed, &st.llm, v, Some(space_id), &item.question, &st.cfg().kb_rrf_weights).await {
                Ok(a) => match a.body {
                    dms_kernel::AnswerBody::Text { markdown, .. } => {
                        item.answer = sanitize(&markdown, ANSWER_STORE_CHARS);
                    }
                    // 知识库问答恒 Text；出现别的形态只可能是 answer 内部改版——记错不猜
                    _ => item.error = "答案不是文本形态".into(),
                },
                Err(e) => item.error = format!("答案生成失败：{e}"),
            }
        }
        // ④ 评审（fast）：判失败计数继续；item 里 judge 留空（上线是 NULL）
        if item.error.is_empty() {
            match judge_answer(&st.llm, &item.question, text, &item.answer).await {
                Some((verdict, reason)) => {
                    verdicts.push(verdict);
                    item.verdict = Some(verdict);
                    item.reason = reason;
                }
                None => judge_failed += 1,
            }
        }
        // 明细落库失败＝结果存不下，继续跑只是烧 LLM 攒假绿——整跑标 failed。
        insert_item(st, run_id, ord, *gold_chunk, &item).await?;
        done += 1;
        progress(st, run_id, done, gen_failed, judge_failed).await;
    }
    Ok(RunMetrics {
        done,
        gen_failed,
        judge_failed,
        recall: [0usize, 1, 2, 3].map(|k| mean_recall(&flags, k)),
        answer_acc: answer_accuracy(&verdicts),
    })
}

/// (chunk_id, doc_id, ord, doc_name, heading_path, text)
type SampledChunk = (i64, String, i32, String, String, String);

async fn sample_chunks(
    st: &AppState,
    space_id: &str,
    n: i32,
) -> Result<Vec<SampledChunk>, String> {
    st.owned
        .fixed(SAMPLE_SQL)
        .bind(space_id)
        .bind(n)
        .fetch_all::<SampledChunk>()
        .await
        .map_err(|e| format!("语料抽样失败：{e}"))
}

/// 进度回写失败只 warn：终态 UPDATE 还会覆盖写一次，进度丢了不是数据丢了。
async fn progress(st: &AppState, run_id: i64, done: i32, gen_failed: i32, judge_failed: i32) {
    if st
        .owned
        .fixed("UPDATE meta.kb_eval_runs SET done=$2, gen_failed=$3, judge_failed=$4 WHERE id=$1")
        .bind(run_id)
        .bind(done)
        .bind(gen_failed)
        .bind(judge_failed)
        .execute()
        .await
        .is_err()
    {
        tracing::warn!(run_id, "评估进度回写失败（继续跑，终态会覆盖）");
    }
}

async fn insert_item(
    st: &AppState,
    run_id: i64,
    ord: i32,
    gold_chunk: i64,
    item: &EvalItem,
) -> Result<(), String> {
    let r = item.recall;
    st.owned
        .fixed(
            "INSERT INTO meta.kb_eval_items(run_id,ord,question,gold_chunk_id,generated_answer,\
             r1,r3,r5,r10,judge,judge_reason,error) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(run_id)
        .bind(ord)
        .bind(&item.question)
        .bind(gold_chunk)
        .bind(&item.answer)
        .bind(r.map(|f| f[0]))
        .bind(r.map(|f| f[1]))
        .bind(r.map(|f| f[2]))
        .bind(r.map(|f| f[3]))
        .bind(item.verdict.map_or("", Verdict::as_str))
        .bind(&item.reason)
        .bind(&item.error)
        .execute()
        .await
        .map_err(|e| format!("评估明细落库失败：{e}"))?;
    Ok(())
}

async fn gen_question(
    llm: &crate::llm::LlmClient,
    doc_name: &str,
    heading: &str,
    text: &str,
) -> Option<String> {
    let loc = if heading.is_empty() {
        format!("文档《{doc_name}》")
    } else {
        format!("文档《{doc_name}》「{heading}」")
    };
    let user = format!("{loc}的片段：\n{}", clip(text, GEN_CHUNK_CHARS));
    let raw = llm_text(llm, GEN_SYSTEM, &user, 0.4).await?;
    parse_question(&raw)
}

async fn judge_answer(
    llm: &crate::llm::LlmClient,
    question: &str,
    gold: &str,
    generated: &str,
) -> Option<(Verdict, String)> {
    let user = format!(
        "问题：{question}\n\n金标准原文：\n{}\n\n系统答案：\n{}",
        clip(gold, JUDGE_GOLD_CHARS),
        clip(generated, JUDGE_ANSWER_CHARS),
    );
    let raw = llm_text(llm, JUDGE_SYSTEM, &user, 0.0).await?;
    parse_judge(&raw)
}

/// fast 档一次对话；任何失败（传输/无内容/空串）统一 None —— 调用方负责计数继续。
async fn llm_text(
    llm: &crate::llm::LlmClient,
    system: &str,
    user: &str,
    temperature: f32,
) -> Option<String> {
    let req = ChatRequest::text(ModelTier::Fast, system, user, Some(temperature));
    let reply = llm.chat(req).await.ok()?;
    let text = reply.content?.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

// ============================ 纯函数（口径唯一事实源） ============================

/// 金标准块在最终命中列表里的名次（1 起，None = 没进列表）。
/// 元组 = (chunk_id, doc_id, ord, merged)。相邻合并的命中锚在首块：金块被并进去时
/// chunk_id 对不上，但内容确实进了 prompt —— 同文档且 ord 落在 [ord, ord+merged) 也算命中。
fn gold_rank(hits: &[(i64, &str, i32, u32)], gold_chunk: i64, gold_doc: &str, gold_ord: i32) -> Option<usize> {
    hits.iter()
        .position(|&(chunk_id, doc_id, ord, merged)| {
            chunk_id == gold_chunk
                || (doc_id == gold_doc && gold_ord >= ord && gold_ord < ord + merged as i32)
        })
        .map(|i| i + 1)
}

/// 名次 → recall@1/3/5/10 四面旗。None（没命中）四面皆 false。
fn recall_at(rank: Option<usize>) -> [bool; 4] {
    let r = rank.unwrap_or(usize::MAX);
    [r <= 1, r <= 3, r <= 5, r <= 10]
}

/// run 级 recall@k：只在**检索成功**的题上取均值（r 旗 NULL 的题不进分母——
/// 「没测成」不是「没命中」，混进分母会把检索故障粉饰成召回差）。
fn mean_recall(flags: &[[bool; 4]], k: usize) -> Option<f64> {
    if flags.is_empty() {
        return None;
    }
    Some(flags.iter().filter(|f| f[k]).count() as f64 / flags.len() as f64)
}

/// 答案准确率：correct=1 / partial=0.5 / wrong=0 的均值；分母＝被成功评审的题数。
fn answer_accuracy(verdicts: &[Verdict]) -> Option<f64> {
    if verdicts.is_empty() {
        return None;
    }
    Some(verdicts.iter().map(|v| v.score()).sum::<f64>() / verdicts.len() as f64)
}

/// 从 LLM 输出里容错抽一个 JSON 对象：原样 → 剥 ``` 围栏 → 第一个 `{` 到最后一个 `}`。
/// 只认对象（数组/裸串不是结构化产出，不许猜）。
fn extract_json(raw: &str) -> Option<serde_json::Value> {
    let parse = |s: &str| {
        serde_json::from_str::<serde_json::Value>(s).ok().filter(|v| v.is_object())
    };
    let t = raw.trim();
    if let Some(v) = parse(t) {
        return Some(v);
    }
    let unfenced = t
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if unfenced.len() != t.len() {
        if let Some(v) = parse(unfenced) {
            return Some(v);
        }
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    parse(&raw[start..=end])
}

/// 判定词归一：英文三值为准，中文/同义词容忍。不在表里的**一律 None**（评审失败计数），
/// 不许把不认识的说法猜成最近的一档——猜错了指标就在说谎。
fn parse_verdict(s: &str) -> Option<Verdict> {
    let t = s.trim().trim_end_matches(['。', '.', '；', ';']).to_lowercase();
    Some(match t.as_str() {
        "correct" | "pass" | "yes" | "true" | "正确" | "对" => Verdict::Correct,
        "partial" | "partially_correct" | "partially correct" | "部分正确" | "部分" => Verdict::Partial,
        "wrong" | "incorrect" | "fail" | "no" | "false" | "错误" | "不对" => Verdict::Wrong,
        _ => return None,
    })
}

/// 评审输出解析：JSON 容错抽取 + 判定词归一 + 理由截断。取不到合法 verdict ＝ None。
fn parse_judge(raw: &str) -> Option<(Verdict, String)> {
    let v = extract_json(raw)?;
    let verdict = ["verdict", "judge", "label", "result"]
        .iter()
        .find_map(|k| v.get(k).and_then(|x| x.as_str()))
        .and_then(parse_verdict)?;
    let reason = ["reason", "reasoning", "explanation", "理由"]
        .iter()
        .find_map(|k| v.get(k).and_then(|x| x.as_str()))
        .map(|s| sanitize(s, MAX_REASON_CHARS))
        .unwrap_or_default();
    Some((verdict, reason))
}

/// 出题输出解析：JSON 取 question/query/q；模型没按 JSON 答时整段短文本兜底当问题
/// （长度窗外的一律不信——太长是絮叨，太短不是题）。
fn parse_question(raw: &str) -> Option<String> {
    if let Some(v) = extract_json(raw) {
        for key in ["question", "query", "q"] {
            if let Some(q) = v.get(key).and_then(|x| x.as_str()) {
                return clean_question(q);
            }
        }
    }
    clean_question(raw)
}

/// 先验长度再收：超长一律不信（截断收下等于把絮叨改成题），窗内原样保留。
fn clean_question(s: &str) -> Option<String> {
    let q = s.trim().trim_matches(['"', '\'']).trim().replace('\0', "");
    let q = q.trim();
    let n = q.chars().count();
    if (MIN_QUESTION_CHARS..=MAX_QUESTION_CHARS).contains(&n) {
        Some(q.to_string())
    } else {
        None
    }
}

/// LLM 产物落库/上线前：剥 NUL（PG text 不收 `\0`）+ 去首尾空白 + 按字符截断（Unicode 安全）。
fn sanitize(s: &str, max: usize) -> String {
    s.replace('\0', "").trim().chars().take(max).collect()
}

/// 按字符截断（`str::truncate` 按字节会切烂多字节字符）。
fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 金块定位：精确 chunk_id / 被并入相邻命中（同文档 ord 跨度）都算；跨文档、跨度外不算
    #[test]
    fn gold_rank_matches_exact_chunk_or_merged_span() {
        let hits: Vec<(i64, &str, i32, u32)> = vec![
            (11, "a", 0, 3), // 合并命中：覆盖 doc a 的 ord 0..=2
            (21, "b", 0, 1),
        ];
        assert_eq!(gold_rank(&hits, 13, "a", 2), Some(1), "金块被并进首块，按跨度算 rank 1");
        assert_eq!(gold_rank(&hits, 21, "b", 0), Some(2), "精确 chunk_id 命中");
        assert_eq!(gold_rank(&hits, 99, "c", 1), None, "同 ord 但不同文档不算");
        assert_eq!(gold_rank(&hits, 14, "a", 3), None, "ord 3 在 merged=3 的 0..=2 跨度之外");
        assert_eq!(gold_rank(&[], 1, "a", 0), None);
    }

    #[test]
    fn recall_flags_are_monotone_by_rank() {
        assert_eq!(recall_at(Some(1)), [true, true, true, true]);
        assert_eq!(recall_at(Some(3)), [false, true, true, true]);
        assert_eq!(recall_at(Some(4)), [false, false, true, true]);
        assert_eq!(recall_at(Some(6)), [false, false, false, true]);
        assert_eq!(recall_at(Some(11)), [false, false, false, false]);
        assert_eq!(recall_at(None), [false, false, false, false]);
    }

    #[test]
    fn recall_mean_excludes_unretrieved_items() {
        let flags = vec![
            [true, true, true, true],
            [false, false, true, true],
            [false, false, false, false],
        ];
        assert_eq!(mean_recall(&flags, 0), Some(1.0 / 3.0));
        assert_eq!(mean_recall(&flags, 2), Some(2.0 / 3.0));
        assert_eq!(mean_recall(&flags, 3), Some(2.0 / 3.0));
        // 0 是「全没命中」，NULL 是「没测」——混了就是把检索故障粉饰成召回差
        assert_eq!(mean_recall(&[], 0), None);
    }

    #[test]
    fn answer_accuracy_weights_partial_half() {
        assert_eq!(
            answer_accuracy(&[Verdict::Correct, Verdict::Partial, Verdict::Wrong]),
            Some(0.5)
        );
        assert_eq!(answer_accuracy(&[Verdict::Correct, Verdict::Correct]), Some(1.0));
        assert_eq!(answer_accuracy(&[Verdict::Wrong]), Some(0.0));
        assert_eq!(answer_accuracy(&[]), None, "没有评审成功的题 → NULL 而不是 0");
    }

    #[test]
    fn judge_parses_clean_fenced_and_chatty_output() {
        let (v, r) = parse_judge(r#"{"verdict":"correct","reason":"完全一致"}"#).unwrap();
        assert_eq!(v, Verdict::Correct);
        assert_eq!(r, "完全一致");
        let (v, _) = parse_judge("```json\n{\"verdict\":\"partial\",\"reason\":\"漏了一半\"}\n```").unwrap();
        assert_eq!(v, Verdict::Partial);
        let (v, r) = parse_judge("评审结果：{\"judge\":\"wrong\",\"reasoning\":\"捏造数字\"}，以上。").unwrap();
        assert_eq!(v, Verdict::Wrong);
        assert_eq!(r, "捏造数字");
        // 中文判定词 + 中文理由键也认
        let (v, _) = parse_judge("{\"verdict\":\"部分正确\",\"理由\":\"缺条件\"}").unwrap();
        assert_eq!(v, Verdict::Partial);
        // 判定词带句读
        let (v, _) = parse_judge("{\"verdict\":\"correct。\"}").unwrap();
        assert_eq!(v, Verdict::Correct);
    }

    #[test]
    fn judge_rejects_garbage_instead_of_guessing() {
        assert!(parse_judge("correct").is_none(), "裸词不是结构化判定");
        assert!(parse_judge("{\"reason\":\"没给结论\"}").is_none(), "缺 verdict 不许猜");
        assert!(parse_judge("{\"verdict\":\"mostly_ok\"}").is_none(), "不认识的档位不许猜");
        assert!(parse_judge("").is_none());
        assert!(parse_judge("[1,2,3]").is_none(), "数组不是结构化判定");
        assert!(parse_judge("} {").is_none());
    }

    #[test]
    fn question_comes_from_json_or_plain_text() {
        assert_eq!(
            parse_question(r#"{"question":"差旅报销上限是多少？"}"#).as_deref(),
            Some("差旅报销上限是多少？")
        );
        assert_eq!(parse_question(r#"{"query":"年假有几天？"}"#).as_deref(), Some("年假有几天？"));
        // 模型没按 JSON 答 → 短文本兜底
        assert_eq!(parse_question("报销流程需要哪些材料？").as_deref(), Some("报销流程需要哪些材料？"));
        // 带引号的 JSON 字符串值
        assert_eq!(parse_question(r#"{"question":"\"病假怎么请？\""}"#).as_deref(), Some("病假怎么请？"));
        // 长度窗外不信
        assert!(parse_question("啥？").is_none());
        assert!(parse_question(&"长".repeat(300)).is_none());
        assert!(parse_question("").is_none());
        assert!(parse_question("{}").is_none(), "空对象没有 question，兜底文本也太短");
    }

    #[test]
    fn llm_text_is_sanitized_for_pg() {
        // PG text 不收 NUL：LLM 产物落库前必须剥掉
        assert_eq!(clean_question("带\0 NUL 的问题？").as_deref(), Some("带 NUL 的问题？"));
        assert_eq!(sanitize("  abc\0def  ", 100), "abcdef");
        assert_eq!(sanitize("知".repeat(300).as_str(), 200).chars().count(), 200);
    }

    /// 三值与 DB CHECK 约束、权重表互锁
    #[test]
    fn verdict_str_score_and_ddl_check_agree() {
        assert_eq!(Verdict::Correct.as_str(), "correct");
        assert_eq!(Verdict::Partial.as_str(), "partial");
        assert_eq!(Verdict::Wrong.as_str(), "wrong");
        assert_eq!(Verdict::Correct.score(), 1.0);
        assert_eq!(Verdict::Partial.score(), 0.5);
        assert_eq!(Verdict::Wrong.score(), 0.0);
        for (s, v) in [("correct", Verdict::Correct), ("partial", Verdict::Partial), ("wrong", Verdict::Wrong)] {
            assert_eq!(parse_verdict(s), Some(v));
        }
        assert!(
            DDL.iter().any(|s| s.contains("'','correct','partial','wrong'")),
            "CHECK 约束与 Verdict 三值分叉了"
        );
    }

    /// 抽样口径必须与 `retrieve::visible_sql` 的文档资格同构（那边改这里必须跟）
    #[test]
    fn sampling_sql_mirrors_retrieval_eligibility() {
        for needle in [
            "d.enabled=true",
            "d.status IN ('chunked','embedded')",
            "d.effective_from IS NULL OR d.effective_from <= CURRENT_DATE",
            "d.effective_to IS NULL OR d.effective_to >= CURRENT_DATE",
            "d.space_id=$1",
            "ORDER BY random()",
        ] {
            assert!(SAMPLE_SQL.contains(needle), "SAMPLE_SQL 缺 {needle}（对照 retrieve::visible_sql）");
        }
    }

    /// 建跑顺序：认证 → 空间可读闸 → 并发许可 → 落库 → spawn。
    /// 许可在落库前：被拒的并发不许留一行永远 running 的孤儿。
    #[test]
    fn create_run_gates_before_persisting() {
        let src = include_str!("kb_eval_api.rs");
        let body = src.split("pub async fn create_run").nth(1).unwrap();
        let body = body.split("tokio::spawn").next().unwrap();
        let auth = body.find("eval_viewer").unwrap();
        let acl = body.find("require_readable").unwrap();
        let gate = body.find("EVAL_GATE").unwrap();
        let insert = body.find("INSERT INTO meta.kb_eval_runs").unwrap();
        assert!(
            auth < acl && acl < gate && gate < insert,
            "建跑必须先认证→可读闸→许可→落库: {body}"
        );
    }

    /// DDL 幂等：migrate 每次启动都跑，任何一条缺 IF NOT EXISTS 第二次就炸
    #[test]
    fn ddl_is_idempotent() {
        for stmt in DDL {
            assert!(stmt.contains("IF NOT EXISTS"), "{stmt}");
        }
        assert!(DDL.iter().any(|s| s.contains("CREATE TABLE IF NOT EXISTS meta.kb_eval_runs")));
        assert!(DDL.iter().any(|s| s.contains("CREATE TABLE IF NOT EXISTS meta.kb_eval_items")));
        assert!(DDL.iter().any(|s| s.contains("REFERENCES meta.kb_eval_runs(id) ON DELETE CASCADE")));
        // items 依赖 runs，必须先建——顺序错了首轮 migrate 就炸
        let runs = DDL.iter().position(|s| s.contains("kb_eval_runs(")).unwrap();
        let items = DDL.iter().position(|s| s.contains("kb_eval_items(")).unwrap();
        assert!(runs < items);
    }

    /// run 取列与 RunRow 同序（两处手对齐，这个测试至少把列数钉死）
    #[test]
    fn run_cols_count_matches_row_type() {
        let cols = run_cols!().split(',').count();
        assert_eq!(cols, 15, "run_cols 列数变了，RunRow 元组也要跟着改");
    }

    /// 收割 SQL 形状锚点：只动 running 行；终态形状与 `run_eval` 的收尾 UPDATE 一致
    #[test]
    fn reap_sql_only_marks_running_runs_failed() {
        assert!(REAP_SQL.contains("UPDATE meta.kb_eval_runs SET status='failed'"));
        assert!(REAP_SQL.contains("WHERE status='running'"),
                "收割只许动 running 行：done/failed 不许回碰");
        assert!(REAP_SQL.contains("error='服务重启中断'"), "中断原因文案变了，前端/运维按它辨认重启遗留");
        assert!(REAP_SQL.contains("finished_at=now()"), "本表无 updated_at，终态时刻落在 finished_at");
        // 收割目标值必须在 CHECK 约束内，否则这条 UPDATE 写不进去
        assert!(DDL[0].contains("'running','done','failed'"), "CHECK 约束变了，收割目标值要重新对");
    }
}
