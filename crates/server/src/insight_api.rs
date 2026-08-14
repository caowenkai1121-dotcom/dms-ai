//! 【AI 解读】`POST /api/analysis` —— 按需给一次取数结果配「这个结果说明了什么」+ 口径说明。
//! 变更原因＝协议形状与开关；解读逻辑本体在 `dms_agent::insight`（那边零 axum）。
//!
//! ## 为什么是**独立端点**，而不是随 `/api/ask` 一起返回
//! 解读是**额外一次 LLM 调用**（fast 档）。随取数返回意味着每一次问数都付这笔延迟，而机器
//! 调用方根本不看解读：`tools/evaluation.py` / `regression.py` 走 CLI `ask` 子命令，
//! `kb_eval.py` 走 `/api/ask` —— 它们的 p95 是有基线的（本轮实测 28~42s），
//! 而基线一旦被一笔与判分无关的调用抬高，「今天这次是不是变慢了」就再也量不出来。
//! 前端点「AI 解读」再调这里：**不点＝零成本**，于是「对评测关着」这个默认是**结构性**的，
//! 不依赖任何人记得去关某个开关。
//!
//! ## 为什么形状是「把结果回传」，而不是 `/api/record/{id}/analysis`
//! 按 id 拉要先有个 id，今天**没有**：
//! - `/api/ask` 的响应体就是 `AskResult` 的 serde 形状，而前端 + `regression.py` +
//!   `evaluation.py` 三处都在解析它 —— 加一个恒在的 `id` 字段就是一次形状破坏（`ctx.rs` 有断言锁）；
//! - `chat::conv_msgs` 连 `msg.id` 都不返回，前端手上没有任何可引用的记录号；
//! - `conv_id` 是**可选**的（不开会话也能问数），按 id 拉会让「没会话时点解读」这条路直接没有。
//!
//! 回传的代价是结果集在网线上走第二趟（≤200 行，`MAX_ROWS` 管着，量级 10-100KB；
//! axum 默认 2MB body limit 就是它的上限）。换来的是零存储、仅新增可选 receipt 字段，以及
//! **零「读他人已存结果」的越权面** —— 响应里的一切都是调用方自己刚发上来的东西，
//! 服务端不从库里取任何别人的数据。
//!
//! ⚠️ 客户端回传本身不可信：`/api/ask` 对完整事实集签 HMAC，analysis/report 先按当前账号与
//! 角色验 receipt，任一问句、SQL、列、行、补充事实、比较或子结果变化都 422。验过的文本仍进
//! `wrap_untrusted`（防 prompt injection），而不是提升为系统指令。

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use crate::AppState;
use crate::dms_policy::principal;

type ApiErr = (StatusCode, Json<serde_json::Value>);

fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// 内部错误的统一出口（安全审查②）：响应只带固定文案 —— anyhow/sqlx 原文可能含关系名、
/// 约束名与连接细节，回前端等于泄露内部结构。真因一律 `tracing::warn!` 留服务端
///（照 `kb_api::kb_err` 的收敛模子）。响应形状不变：`{"error": 固定文案}` + 原状态码。
fn internal_err(context: &'static str, e: impl std::fmt::Display) -> ApiErr {
    tracing::warn!(error = %e, "{context}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "服务暂时不可用，请稍后重试")
}

/// 身份核验失败（同 `api_ask` 的 403 文案）：load_principal 的 anyhow 可能携带身份库
/// 错误原文（连接细节），不外回；业务分类（多角色未选等）由 warn 留痕。
fn identity_err(login: &str, e: impl std::fmt::Display) -> ApiErr {
    tracing::warn!(login = %login, error = %e, "身份核验被 load_principal 拒");
    err(StatusCode::FORBIDDEN, "当前账号或角色不可用")
}

/// 两个 handler 共用的身份核验前段（401 文案逐字保持：前端按文案认「未认证」）。
fn require_login(
    st: &AppState,
    headers: &HeaderMap,
    login_name: &Option<String>,
    role_code: &Option<String>,
) -> Result<(String, Option<String>), ApiErr> {
    crate::resolve_identity(st, headers, login_name, role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))
}

/// `Reading` 的拼装只许有这一处（`analysis` 与 `report` 共用，逐字段各拼一份必漂移）。
/// `row_count` 不能全信调用方：`row_count < rows.len()` 时下游会说出「共 X 行」却列出
/// 更多行的自相矛盾文案，故与 `rows.len()` 取大兜底。
fn reading_of<'a>(
    question: &'a str,
    sql: &'a str,
    columns: &'a [String],
    rows: &'a [Vec<serde_json::Value>],
    row_count: Option<usize>,
    caliber_note: Option<&'a str>,
    extras: ReadingExtras<'a>,
) -> dms_agent::Reading<'a> {
    dms_agent::Reading {
        question,
        sql,
        columns,
        rows,
        row_count: row_count.unwrap_or(rows.len()).max(rows.len()),
        caliber_note,
        supplemental: extras.supplemental,
        comparisons: extras.comparisons,
        sales_context: extras.sales_context,
    }
}

#[derive(Default)]
struct ReadingExtras<'a> {
    supplemental: Option<dms_agent::insight::ReadingTable<'a>>,
    comparisons: Option<dms_agent::insight::ReadingTable<'a>>,
    sales_context: Option<dms_agent::insight::ReadingTable<'a>>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
struct AnalysisTable {
    #[serde(default)]
    columns: Vec<String>,
    #[serde(default)]
    rows: Vec<Vec<serde_json::Value>>,
    #[serde(default)]
    row_count: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct AnalysisComparison {
    #[serde(default)]
    label: String,
    #[serde(default)]
    current: Option<f64>,
    #[serde(default)]
    baseline: Option<f64>,
    #[serde(default)]
    change: Option<f64>,
    #[serde(default)]
    pct: Option<f64>,
}

/// `/api/ask` 已执行结果中允许交给解读模型/报表的完整事实集。
///
/// receipt 只签这一份最小材料，不签 `view`、耗时、登录参数等展示/传输字段。签发和验签
/// 都先构造本结构，再走同一份 Rust canonicalization；前端只负责原样回传 token。
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct AnalysisMaterial {
    question: String,
    sql: String,
    columns: Vec<String>,
    rows: Vec<Vec<serde_json::Value>>,
    row_count: Option<usize>,
    caliber_note: Option<String>,
    supplemental: Option<AnalysisTable>,
    comparisons: Vec<AnalysisComparison>,
    sales_context: Option<AnalysisTable>,
    subs: Vec<AnalysisMaterial>,
}

const ANALYSIS_RECEIPT_PREFIX: &str = "v1.";
const ANALYSIS_RECEIPT_DOMAIN: &[u8] = b"dms-ai/analysis-material/v1\0";
const REPORT_RECEIPT_PREFIX: &str = "v1.";
const REPORT_RECEIPT_DOMAIN: &[u8] = b"dms-ai/analysis-report/v1\0";

impl AnalysisMaterial {
    fn from_analysis(req: &AnalysisReq) -> Self {
        Self {
            question: req.question.clone(),
            sql: req.sql.clone(),
            columns: req.columns.clone(),
            rows: req.rows.clone(),
            row_count: req.row_count,
            caliber_note: req.caliber_note.clone(),
            supplemental: req.supplemental.clone(),
            comparisons: req.comparisons.clone(),
            sales_context: req.sales_context.clone(),
            subs: req.subs.clone(),
        }
    }

    fn from_report(req: &ReportReq) -> Self {
        Self {
            question: req.question.clone(),
            sql: req.sql.clone(),
            columns: req.columns.clone(),
            rows: req.rows.clone(),
            row_count: req.row_count,
            caliber_note: req.caliber_note.clone(),
            supplemental: req.supplemental.clone(),
            comparisons: req.comparisons.clone(),
            sales_context: req.sales_context.clone(),
            subs: req.subs.clone(),
        }
    }

    /// AskResult 的 wire 比分析材料多 `view`/`dir`/`truncated` 等字段；这里只投影白名单，
    /// 避免未来新增展示字段导致历史 receipt 无意义失效。
    fn from_ask_payload(question: &str, payload: &serde_json::Value) -> Option<Self> {
        let string_vec = |value: Option<&serde_json::Value>| {
            serde_json::from_value::<Vec<String>>(value?.clone()).ok()
        };
        let rows = |value: Option<&serde_json::Value>| {
            serde_json::from_value::<Vec<Vec<serde_json::Value>>>(value?.clone()).ok()
        };
        let table = |value: Option<&serde_json::Value>, infer_count: bool| {
            let value = value?.as_object()?;
            let rows = rows(value.get("rows"))?;
            Some(AnalysisTable {
                columns: string_vec(value.get("columns"))?,
                row_count: value
                    .get("row_count")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .or_else(|| infer_count.then_some(rows.len())),
                rows,
            })
        };
        let comparisons = payload
            .get("comparisons")
            .cloned()
            .map(serde_json::from_value::<Vec<AnalysisComparison>>)
            .transpose()
            .ok()?
            .unwrap_or_default();
        let subs = match payload.get("subs").and_then(serde_json::Value::as_array) {
            Some(subs) => subs
                .iter()
                .map(|sub| {
                    let question = sub.get("question")?.as_str()?;
                    Self::from_ask_payload(question, sub.get("result")?)
                })
                .collect::<Option<Vec<_>>>()?,
            None => vec![],
        };
        Some(Self {
            question: question.to_string(),
            sql: payload.get("sql")?.as_str()?.to_string(),
            columns: string_vec(payload.get("columns"))?,
            rows: rows(payload.get("rows"))?,
            row_count: payload
                .get("row_count")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| usize::try_from(n).ok()),
            caliber_note: payload
                .get("caliber_note")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            supplemental: table(payload.get("supplemental"), false),
            comparisons,
            sales_context: table(payload.get("sales_context"), true),
            subs,
        })
    }
}

/// JSON 文本不是 canonical（`1.0` 经浏览器会变成 `1`，对象键顺序也不稳定）。
/// 这里按值递归规范化：对象键排序、有限数字统一走 f64 的最短十进制表示、-0 归 0。
fn canonical_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_f64() {
                if value == 0.0 {
                    out.push('0');
                } else {
                    out.push_str(&value.to_string());
                }
            }
        }
        serde_json::Value::String(value) => {
            out.push_str(&serde_json::to_string(value).expect("String 序列化不会失败"));
        }
        serde_json::Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(value, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(values) => {
            out.push('{');
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("对象键序列化不会失败"));
                out.push(':');
                canonical_json(&values[key], out);
            }
            out.push('}');
        }
    }
}

fn receipt_identity(principal: &principal::Principal) -> String {
    format!("{}\0{}", principal.login_name, principal.role_code)
}

fn receipt_message(material: &AnalysisMaterial, identity: &str) -> Vec<u8> {
    let value = serde_json::to_value(material).expect("AnalysisMaterial 序列化不会失败");
    let mut canonical = String::new();
    canonical_json(&value, &mut canonical);
    let mut message = Vec::with_capacity(
        ANALYSIS_RECEIPT_DOMAIN.len() + std::mem::size_of::<u64>() + identity.len() + canonical.len(),
    );
    message.extend_from_slice(ANALYSIS_RECEIPT_DOMAIN);
    message.extend_from_slice(&(identity.len() as u64).to_be_bytes());
    message.extend_from_slice(identity.as_bytes());
    message.extend_from_slice(canonical.as_bytes());
    message
}

fn mint_analysis_receipt(material: &AnalysisMaterial, identity: &str) -> String {
    use base64::Engine as _;
    let key = crate::db::crypto::default_key().0;
    let sig = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key),
        &receipt_message(material, identity),
    );
    format!(
        "{ANALYSIS_RECEIPT_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.as_ref())
    )
}

fn verify_analysis_receipt(material: &AnalysisMaterial, identity: &str, receipt: &str) -> bool {
    use base64::Engine as _;
    let Some(encoded) = receipt.strip_prefix(ANALYSIS_RECEIPT_PREFIX) else { return false };
    let Ok(sig) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    let key = crate::db::crypto::default_key().0;
    ring::hmac::verify(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key),
        &receipt_message(material, identity),
        &sig,
    )
    .is_ok()
}

fn require_analysis_receipt(
    material: &AnalysisMaterial,
    principal: &principal::Principal,
    receipt: &str,
) -> Result<(), ApiErr> {
    verify_analysis_receipt(material, &receipt_identity(principal), receipt)
        .then_some(())
        .ok_or_else(|| err(StatusCode::UNPROCESSABLE_ENTITY, "分析素材凭证无效，请重新查询"))
}

fn report_receipt_message(
    material: &AnalysisMaterial,
    insight: Option<&str>,
    identity: &str,
) -> Vec<u8> {
    let mut message = receipt_message(material, identity);
    message.extend_from_slice(REPORT_RECEIPT_DOMAIN);
    match insight {
        Some(insight) => {
            message.push(1);
            message.extend_from_slice(&(insight.len() as u64).to_be_bytes());
            message.extend_from_slice(insight.as_bytes());
        }
        None => message.push(0),
    }
    message
}

fn mint_report_receipt(
    material: &AnalysisMaterial,
    insight: Option<&str>,
    identity: &str,
) -> String {
    use base64::Engine as _;
    let key = crate::db::crypto::default_key().0;
    let sig = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key),
        &report_receipt_message(material, insight, identity),
    );
    format!(
        "{REPORT_RECEIPT_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.as_ref())
    )
}

fn verify_report_receipt(
    material: &AnalysisMaterial,
    insight: Option<&str>,
    identity: &str,
    receipt: &str,
) -> bool {
    use base64::Engine as _;
    let Some(encoded) = receipt.strip_prefix(REPORT_RECEIPT_PREFIX) else { return false };
    let Ok(sig) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    let key = crate::db::crypto::default_key().0;
    ring::hmac::verify(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key),
        &report_receipt_message(material, insight, identity),
        &sig,
    )
    .is_ok()
}

/// 问数返回前注入不可伪造的分析素材凭证；payload 仍保留 AskResult 的兼容 wire，
/// receipt 是唯一新增字段。返回 false 只代表调用方传的不是数据 AskResult。
pub(crate) fn attach_analysis_receipt(
    payload: &mut serde_json::Value,
    question: &str,
    principal: &principal::Principal,
) -> bool {
    let Some(material) = AnalysisMaterial::from_ask_payload(question, payload) else { return false };
    let Some(object) = payload.as_object_mut() else { return false };
    object.insert(
        "analysis_receipt".into(),
        serde_json::Value::String(mint_analysis_receipt(&material, &receipt_identity(principal))),
    );
    true
}

fn reading_table(table: &AnalysisTable) -> dms_agent::insight::ReadingTable<'_> {
    dms_agent::insight::ReadingTable {
        columns: &table.columns,
        rows: &table.rows,
        row_count: table.row_count.unwrap_or(table.rows.len()).max(table.rows.len()),
    }
}

/// 请求体 = 前端手上那次 `/api/ask` 的完整事实集 + 服务端 receipt + 身份。
/// `question`/`sql`/`analysis_receipt` 是信任边界；其余容器字段缺省为空。
#[derive(serde::Deserialize)]
pub struct AnalysisReq {
    question: String,
    /// 那次**已执行的** SQL（`AskResult.sql`）。口径说明的唯一素材源。
    sql: String,
    #[serde(default)]
    columns: Vec<String>,
    /// 必须回传 `/api/ask` 返回的完整行集；解读层自己限展示行数，receipt 拒绝客户端截断/改写。
    #[serde(default)]
    rows: Vec<Vec<serde_json::Value>>,
    #[serde(default)]
    row_count: Option<usize>,
    /// `AskResult.caliber_note`（口径复核未通过的标注）。回传它才能让告警印在解读最前面
    #[serde(default)]
    caliber_note: Option<String>,
    /// 主查询之外的明细/结构事实。缺省为空，兼容旧前端。
    #[serde(default)]
    supplemental: Option<AnalysisTable>,
    /// 已执行的环比/同比原值；服务端转成固定五列表后进入 COMPARE 事实域。
    #[serde(default)]
    comparisons: Vec<AnalysisComparison>,
    /// 与主指标同时间窗的成本、收入、毛利等补充事实。
    #[serde(default)]
    sales_context: Option<AnalysisTable>,
    /// 复合问数的每个子结果；空 = 单结果。
    #[serde(default)]
    subs: Vec<AnalysisMaterial>,
    /// `/api/ask` 对上述事实集签发的 HMAC。前端不得重算，只原样回传。
    #[serde(default)]
    analysis_receipt: String,
    /// 【深度模式】`true` → `Reading::insight_deep`（Precise 档 + 结构化四段 + 15 行素材）。
    /// 缺省 false = 精简解读，老前端与判官脚本的 body 一字不用改。
    #[serde(default)]
    deep: bool,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/analysis` → `{ "caliber": "...", "insight": "..." | null }`
///
/// - `caliber` **恒有**（确定性、零 LLM、不会失败）：来源表 / 过滤条件（含注入的行级权限）/
///   时间窗 / 去重，逐项从回传的 SQL 里读出来。
/// - `insight` 是 fast LLM 那段话，**可能是 `null`**：模型挂了、回了空串、回了网址，
///   或 `insight_enabled=false`。前端遇 `null` 就只显示 `caliber`——
///   解读失败绝不能让一次已经成功的取数看起来失败（`insight.rs` 的第三条硬线）。
pub async fn analysis(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<AnalysisReq>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let (login, role) = require_login(&st, &headers, &req.login_name, &req.role_code)?;
    // 本端点不读库、不取数（素材全在请求体里），核身份只为一件事：
    // **别让「烧 LLM 额度」比问数更便宜** —— 不核的话任意 login_name 都能白刷 fast 调用。
    // 权限集合与脱敏不在这里判：能进这个 body 的行，是调用方上一次问数时已经过闸门给他的。
    let principal = principal::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|e| identity_err(&login, e))?;
    require_analysis_receipt(
        &AnalysisMaterial::from_analysis(&req),
        &principal,
        &req.analysis_receipt,
    )?;
    let comparison_columns = vec![
        "label".to_string(),
        "current".to_string(),
        "baseline".to_string(),
        "change".to_string(),
        "pct".to_string(),
    ];
    let comparison_rows = req
        .comparisons
        .iter()
        .map(|comparison| {
            vec![
                serde_json::Value::String(comparison.label.clone()),
                serde_json::json!(comparison.current),
                serde_json::json!(comparison.baseline),
                serde_json::json!(comparison.change),
                serde_json::json!(comparison.pct),
            ]
        })
        .collect::<Vec<_>>();
    let extras = ReadingExtras {
        supplemental: req.supplemental.as_ref().map(reading_table),
        comparisons: (!comparison_rows.is_empty()).then_some(dms_agent::insight::ReadingTable {
            columns: &comparison_columns,
            rows: &comparison_rows,
            row_count: comparison_rows.len(),
        }),
        sales_context: req.sales_context.as_ref().map(reading_table),
    };
    let material = AnalysisMaterial::from_analysis(&req);
    let r = reading_of(
        &req.question,
        &req.sql,
        &req.columns,
        &req.rows,
        req.row_count,
        req.caliber_note.as_deref(),
        extras,
    );
    let caliber = r.caliber();
    // 开关只挡 LLM 那一半：关了照样返口径说明（那部分零成本），前端不用改。
    // 【深度模式】`deep=true` 走 Precise 档四段式；精简维持 fast 档 2-4 句。
    let insight = if st.insight_enabled {
        if req.deep { r.insight_deep(&st.llm).await } else { r.insight(&st.llm).await }
    } else {
        None
    };
    Ok(Json(body(caliber, insight, &material, &principal)))
}

/// 响应体构造（**纯函数**）：`{ "caliber": …, "insight": … | null }`。
///
/// 🔴 抽出来是因为原来那条断言是**恒真的**：它在测试体里自己 `json!({...})` 造一个字面量、
/// 再断言这个字面量有 `insight` 键 —— 测的是 `serde_json` 的行为，不是 handler 的响应构造。
/// 实测两次打坏都不红：把 handler 改成只返 `caliber`、或改成「`None` 时不插这个键」，
/// 全量测试照旧 139 passed。而这正是前端赖以区分「没解读」与「字段没实现」的那条契约。
/// 现在断言打在这个函数上，删键/skip-null 当场红。
fn body(
    caliber: String,
    insight: Option<String>,
    material: &AnalysisMaterial,
    principal: &principal::Principal,
) -> serde_json::Value {
    let report_receipt = mint_report_receipt(
        material,
        insight.as_deref(),
        &receipt_identity(principal),
    );
    serde_json::json!({
        "caliber": caliber,
        "insight": insight,
        "report_receipt": report_receipt,
    })
}

// =========================== 【S2】分析报表 artifact 化 ===========================
// datanote 的 CreateArtifactTool 对应物：把一次已生成的解读固化成 `meta.artifact`
// （kind=report），会话气泡出一张「📄 报表」卡，点击右侧沙箱面板预览（S1 地基）。
// **零 LLM**：caliber 服务端从已验签 SQL 重算；insight 必须带 analysis 响应签发的
// report_receipt，客户端改一字都拒绝。md_to_html 仍 escape-first，防内容注入。

#[derive(serde::Deserialize)]
pub struct ReportReq {
    question: String,
    sql: String,
    #[serde(default)]
    columns: Vec<String>,
    #[serde(default)]
    rows: Vec<Vec<serde_json::Value>>,
    #[serde(default)]
    row_count: Option<usize>,
    #[serde(default)]
    caliber_note: Option<String>,
    #[serde(default)]
    supplemental: Option<AnalysisTable>,
    #[serde(default)]
    comparisons: Vec<AnalysisComparison>,
    #[serde(default)]
    sales_context: Option<AnalysisTable>,
    #[serde(default)]
    subs: Vec<AnalysisMaterial>,
    /// 与生成解读时相同的服务端事实凭证。报表也不得把客户端改写后的数字固化为“已验证”。
    #[serde(default)]
    analysis_receipt: String,
    /// 前端 `/api/analysis` 那份 `insight` 的原样回声（可缺 = 没有模型解读）
    #[serde(default)]
    insight: Option<String>,
    /// `/api/analysis` 对“事实材料 + insight”整体签发；客户端改 insight 后不能冒充模型产物。
    #[serde(default)]
    report_receipt: String,
    /// 【图表】`view.blocks` 里 Chart 块的回声（kind/x/y/series/top）。数据不再回传 —
    /// 服务端用已有的 columns/rows 按规格自己取数（回声只是**下标与图型**，信任级同 columns）。
    #[serde(default)]
    charts: Vec<crate::chart_svg::ChartSpec>,
    /// 必填：产物按会话归属（view/download/list 的归属校验全指着它）
    conv_id: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// 报表数据表的行数上限（文案与表体共用这一个数，不许双写漂移）。
const REPORT_TABLE_ROWS: usize = 50;

/// 报表 markdown 组装（**纯函数**，判据打在这里）。
/// 形状：标题 → （可选）口径告警 → 口径说明 → AI 解读 → 数据表（≤50 行）→ SQL。
fn report_md(req: &ReportReq, caliber: &str) -> String {
    let mut s = String::new();
    s.push_str("# ");
    s.push_str(req.question.trim());
    s.push_str("\n\n");
    if let Some(n) = req.caliber_note.as_deref().filter(|n| !n.trim().is_empty()) {
        // 口径复核未通过必须印在数字**之前**（同 .caliber-warn 的「先拦住视线」原则）
        s.push_str("**⚠️ 口径复核未通过：");
        s.push_str(n.trim());
        s.push_str("**\n\n");
    }
    s.push_str("## 口径说明\n\n");
    s.push_str(caliber.trim());
    s.push_str("\n\n## AI 解读\n\n");
    s.push_str(req.insight.as_deref().map(str::trim).filter(|i| !i.is_empty()).unwrap_or("（本次没有模型解读）"));
    s.push_str("\n\n## 数据\n\n");
    // 与 reading_of 同一兜底：row_count 不许小于实际回传的行数
    let total = req.row_count.unwrap_or(req.rows.len()).max(req.rows.len());
    let shown = req.rows.len().min(REPORT_TABLE_ROWS);
    if shown < total {
        s.push_str(&format!("共 {total} 行（下表为前 {shown} 行）\n\n"));
    } else {
        s.push_str(&format!("共 {total} 行\n\n"));
    }
    if !req.columns.is_empty() {
        let cell = |v: &str| v.replace('|', "｜").replace(['\n', '\r'], " ");
        s.push('|');
        for c in &req.columns {
            s.push_str(&format!(" {} |", cell(c)));
        }
        s.push('\n');
        s.push('|');
        for _ in &req.columns {
            s.push_str("---|");
        }
        s.push('\n');
        for r in req.rows.iter().take(REPORT_TABLE_ROWS) {
            s.push('|');
            // 行长按表头补齐/截断：单元格数与 columns 不一致会歪掉整张 markdown 表
            for i in 0..req.columns.len() {
                let txt = match r.get(i) {
                    Some(serde_json::Value::Null) | None => String::new(),
                    Some(serde_json::Value::String(x)) => x.clone(),
                    Some(other) => other.to_string(),
                };
                s.push_str(&format!(" {} |", cell(&txt)));
            }
            s.push('\n');
        }
        s.push('\n');
    }
    // 【图表】占位符跟在数据表后：md_to_html 之后由 `fill_charts` 换成 inline SVG
    //（占位符是我们写的，不是外部文本；生僻括号 `⟦⟧` 数据撞不上）。
    for (i, _) in req.charts.iter().enumerate() {
        s.push_str(&format!("{}{i}{}\n\n", crate::chart_svg::CHART_MARK.0, crate::chart_svg::CHART_MARK.1));
    }
    // 围栏升四级：回传的 sql 本身含 ``` 时也不会顶破围栏（sql 是不可信输入）
    s.push_str("## SQL\n\n````sql\n");
    s.push_str(req.sql.trim());
    s.push_str("\n````\n");
    s
}

/// conv_id 解析（纯函数）：合法数字还不够 —— `"-5"`/`"0"` 也合法但不是主键，
/// 放过去会落 403 而非 400，「必须是会话主键数字」的文案名不副实。
fn parse_conv_id(raw: &str) -> Result<i64, ApiErr> {
    raw.parse::<i64>()
        .ok()
        .filter(|cid| *cid > 0)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "conv_id 必须是会话主键数字"))
}

/// `POST /api/analysis/report` → `{ id, title, preview_url, download_url }`
pub async fn report(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ReportReq>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    // 空问题/空 SQL 不该固化出一张空报表 artifact
    if req.question.trim().is_empty() || req.sql.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "question 与 sql 不能为空"));
    }
    let (login, role) = require_login(&st, &headers, &req.login_name, &req.role_code)?;
    let principal = principal::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|e| identity_err(&login, e))?;
    require_analysis_receipt(
        &AnalysisMaterial::from_report(&req),
        &principal,
        &req.analysis_receipt,
    )?;
    if !verify_report_receipt(
        &AnalysisMaterial::from_report(&req),
        req.insight.as_deref(),
        &receipt_identity(&principal),
        &req.report_receipt,
    ) {
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "报表解读凭证无效，请重新生成解读"));
    }
    // 写前校归属：别把报表写进别人的会话（view 那一层也有校验，但脏数据从源头就不该落）
    let cid = parse_conv_id(&req.conv_id)?;
    let owner = crate::chat::conv_owner(st.owned.pool(), cid)
        .await
        .map_err(|e| internal_err("insight 读会话归属失败", e))?;
    if owner.as_deref() != Some(login.as_str()) {
        return Err(err(StatusCode::FORBIDDEN, "无权在该会话下生成报表"));
    }
    // caliber 服务端重算（回传的任何文本都不信——口径说明的素材只能是从 SQL 现读）
    let caliber = reading_of(
        &req.question,
        &req.sql,
        &req.columns,
        &req.rows,
        req.row_count,
        req.caliber_note.as_deref(),
        ReadingExtras::default(),
    )
    .caliber();
    let title: String = req.question.trim().chars().take(40).collect();
    let md = report_md(&req, &caliber);
    // 图表回声 → SVG（退化规格 = 空串 = 占位符原样留，报表不塌）；先渲染再替换，
    // 顺序反了 SVG 会被 md_to_html 当文本转义掉。
    let svgs: Vec<String> =
        // 请求体只带裸列名（`Semantic` 没上过 wire）→ 语义未声明，落回按列名猜
        req.charts
            .iter()
            .map(|c| {
                crate::chart_svg::chart_svg(c, &req.columns, &req.rows, dms_kernel::present::Semantic::None)
            })
            .collect();
    let html_body = crate::chart_svg::fill_charts(&crate::artifact_api::md_to_html(&md), &svgs);
    let html = crate::artifact_api::page_shell(&title, &html_body);
    // conv_id 落库用解析校验过的主键（`"012"` 这类写法不该原样进库）
    let id = crate::artifact_api::save_artifact(&st, &cid.to_string(), "report", &title, &html, &login)
        .await
        .map_err(|e| internal_err("insight 保存报表失败", e))?;
    Ok(Json(serde_json::json!({
        "id": id,
        "title": title,
        "preview_url": format!("/api/artifact/{id}/view"),
        "download_url": format!("/api/artifact/{id}/download"),
    })))
}

#[cfg(test)]
mod tests {
    /// 安全审查②：内部错误只回固定文案（原文含关系名/约束名/连接细节，只进 warn 不进响应体）
    #[test]
    fn internal_err_has_fixed_message_and_keeps_shape() {
        let raw = "duplicate key violates \"msg_pkey\" (host=10.0.0.8:5432)";
        let (code, axum::Json(body)) = super::internal_err("测试上下文", raw);
        assert_eq!(code, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, serde_json::json!({ "error": "服务暂时不可用，请稍后重试" }));
        assert!(!body.to_string().contains("msg_pkey"), "约束名不许外泄");
        let (code, axum::Json(body)) = super::identity_err("zhangsan", raw);
        assert_eq!(code, axum::http::StatusCode::FORBIDDEN);
        assert_eq!(body, serde_json::json!({ "error": "当前账号或角色不可用" }));
    }

    /// 源码闸：`err(状态码, e)` 直回原文的写法不许回来
    #[test]
    fn raw_causes_never_reach_the_client() {
        let src = include_str!("insight_api.rs");
        let code = src.split("#[cfg(test)]").next().expect("测试模块必然存在");
        assert!(!code.is_empty(), "切分出空串 = 下面两条断言恒绿");
        for bad in [
            "err(StatusCode::INTERNAL_SERVER_ERROR, e)",
            "err(StatusCode::FORBIDDEN, e)",
        ] {
            assert!(!code.contains(bad), "错误原文泄露回来了：{bad}");
        }
    }

    /// 🔴 响应三键是与前端的契约：`caliber` 恒有、`insight` 可为 `null`，
    /// `report_receipt` 把事实材料与这份 insight 绑定，报表入口据此拒绝客户端改写。
    /// 断言打在 **handler 用的那个 `body()`** 上，不是在测试体里自己造一个字面量 ——
    /// 后者测的是 `serde_json`，删键也不会红（实测：改成只返 caliber 仍 139 passed）。
    #[test]
    fn response_keeps_insight_key_even_when_null() {
        let material = material();
        let principal = principal();
        let v = super::body("口径：…".into(), None, &material, &principal);
        assert!(v.get("insight").is_some(), "键必须在（前端靠它区分「没解读」与「字段没实现」）：{v}");
        assert!(v["insight"].is_null());
        assert_eq!(v["caliber"], "口径：…");
        assert!(v["report_receipt"].as_str().is_some_and(|receipt| !receipt.is_empty()));
        // 有解读时原样带出（不许被包一层或改名）
        let v2 = super::body(
            "口径：…".into(),
            Some("这个月比上月涨了 17%".into()),
            &material,
            &principal,
        );
        assert_eq!(v2["insight"], "这个月比上月涨了 17%");
        assert_eq!(v2["caliber"], "口径：…");
        assert_eq!(v2.as_object().unwrap().len(), 3, "{v2}");
    }

    /// 🔴 止血阀（`insight_enabled=false`）：**口径说明一字不少，只是没有模型那段话**。
    /// 原来这条路径判据为零 —— 关掉开关时若连 caliber 一起没了，前端就只剩一个空面板，
    /// 而「解读关着」与「解读失败」在界面上就分不开了。
    ///
    /// 这里钉的是 handler 里那个三目的两侧结果如何进 `body()`（handler 本体要连 LLM 与 MySQL，
    /// 不适合单测；把可判定的那部分——「开关只挡 insight、不挡 caliber」——用 body() 表达）。
    #[test]
    fn disabled_valve_keeps_caliber_and_nulls_insight() {
        let material = material();
        let principal = principal();
        for enabled in [true, false] {
            let insight = if enabled { Some("模型那段话".to_string()) } else { None };
            let v = super::body(
                "口径：来源表 t_sales_order；过滤 …".into(),
                insight,
                &material,
                &principal,
            );
            assert_eq!(v["caliber"], "口径：来源表 t_sales_order；过滤 …", "开关不许影响口径说明");
            assert_eq!(v["insight"].is_null(), !enabled);
        }
    }

    /// 请求体：receipt 字段可缺省解析，但 handler 会 fail-closed 拒绝空凭证；
    /// 这样老客户端得到明确 422，而不是 serde 的无说明 400。
    #[test]
    fn request_needs_only_question_and_sql() {
        let r: super::AnalysisReq =
            serde_json::from_value(serde_json::json!({ "question": "本月销售额", "sql": "SELECT 1" }))
                .expect("最小请求体必须能解析");
        assert!(r.rows.is_empty() && r.columns.is_empty());
        assert_eq!(r.row_count, None);
        assert!(r.caliber_note.is_none());
        assert!(r.analysis_receipt.is_empty());
        assert!(r.supplemental.is_none() && r.comparisons.is_empty() && r.sales_context.is_none());
        // 【深度模式】deep 缺省 false（老 body 一字不改就是精简解读）
        assert!(!r.deep);
        let d: super::AnalysisReq = serde_json::from_value(
            serde_json::json!({ "question": "q", "sql": "SELECT 1", "deep": true }),
        )
        .unwrap();
        assert!(d.deep);
        // 缺 sql → 必须报错（没有 SQL 就没有口径，那才是这个端点存在的理由）
        assert!(serde_json::from_value::<super::AnalysisReq>(
            serde_json::json!({ "question": "本月销售额" })
        )
        .is_err());
    }

    /// 【S2】报表 markdown 的形状与转义（纯函数 `report_md` —— handler 要连库，不测）。
    #[test]
    fn report_md_shape_and_escaping() {
        let req: super::ReportReq = serde_json::from_value(serde_json::json!({
            "question": "本月销售额",
            "sql": "SELECT 1\nFROM t",
            "columns": ["省|份", "金额"],
            "rows": [["湖南", 100.5], ["甲\n乙", null]],
            "row_count": 120,
            "caliber_note": "口径存疑",
            "insight": "比上月**涨**了",
            "conv_id": "12",
        }))
        .unwrap();
        let md = super::report_md(&req, "口径：来源表 t");
        // 告警在数字之前
        assert!(md.find("口径复核未通过").unwrap() < md.find("## 数据").unwrap(), "{md}");
        // 解读原样进（** 留给 md_to_html 渲粗体）
        assert!(md.contains("比上月**涨**了"), "{md}");
        // 截断说明 = row_count 真数 + 前 N 行
        assert!(md.contains("共 120 行（下表为前 2 行）"), "{md}");
        // 单元格的 | 与换行不许拆表（全角替换 / 空格替换）
        assert!(md.contains("| 省｜份 | 金额 |"), "{md}");
        assert!(md.contains("| 甲 乙 |  |"), "{md}");
        // SQL 围栏（四级反引号：sql 里含 ``` 也顶不破）
        assert!(md.contains("````sql\nSELECT 1\nFROM t\n````"), "{md}");
        // 无 insight / 无告警的退化形
        let req2: super::ReportReq = serde_json::from_value(serde_json::json!({
            "question": "q", "sql": "SELECT 1", "conv_id": "1",
        }))
        .unwrap();
        let md2 = super::report_md(&req2, "口径");
        assert!(md2.contains("（本次没有模型解读）"), "{md2}");
        assert!(!md2.contains("口径复核未通过"), "{md2}");
        assert!(md2.contains("共 0 行"), "{md2}");
    }

    /// 行数说明必须与表体一致：表体恒截 REPORT_TABLE_ROWS 行，文案报的是实际列出的行数；
    /// rows 多于上限时（即使 row_count == rows.len()）也必须带截断说明。
    #[test]
    fn report_md_row_count_text_matches_truncated_table() {
        let rows: Vec<Vec<serde_json::Value>> =
            (0..61).map(|i| vec![serde_json::json!(i)]).collect();
        let req: super::ReportReq = serde_json::from_value(serde_json::json!({
            "question": "q", "sql": "SELECT 1", "columns": ["n"],
            "rows": rows, "row_count": 61, "conv_id": "1",
        }))
        .unwrap();
        let md = super::report_md(&req, "口径");
        assert!(md.contains("共 61 行（下表为前 50 行）"), "{md}");
        assert!(md.contains("| 49 |") && !md.contains("| 50 |"), "表体只列前 50 行：{md}");
        // row_count 小于实际行数时不许自相矛盾（取大兜底）
        let req2: super::ReportReq = serde_json::from_value(serde_json::json!({
            "question": "q", "sql": "SELECT 1", "columns": ["n"],
            "rows": [[1], [2]], "row_count": 1, "conv_id": "1",
        }))
        .unwrap();
        let md2 = super::report_md(&req2, "口径");
        assert!(md2.contains("共 2 行"), "row_count < rows.len() 时按实际行数说：{md2}");
    }

    /// 行单元格数与 columns 不一致：按表头补齐空单元格/截掉多余单元格，不许出锯齿行
    #[test]
    fn report_md_pads_ragged_rows_to_header() {
        let req: super::ReportReq = serde_json::from_value(serde_json::json!({
            "question": "q", "sql": "SELECT 1", "columns": ["a", "b", "c"],
            "rows": [[1], [1, 2, 3, 4]], "conv_id": "1",
        }))
        .unwrap();
        let md = super::report_md(&req, "口径");
        assert!(md.contains("| 1 |  |  |"), "短行补齐空单元格：{md}");
        assert!(md.contains("| 1 | 2 | 3 |"), "长行截到表头宽：{md}");
        assert!(!md.contains("| 4 |"), "{md}");
    }

    /// sql 里含 ``` 时四级围栏不被顶破
    #[test]
    fn report_md_sql_with_backticks_keeps_fence() {
        let req: super::ReportReq = serde_json::from_value(serde_json::json!({
            "question": "q", "sql": "SELECT 1 ``` 注释", "conv_id": "1",
        }))
        .unwrap();
        let md = super::report_md(&req, "口径");
        assert!(md.contains("````sql\nSELECT 1 ``` 注释\n````"), "{md}");
    }

    /// conv_id 解析：合法数字但非主键（负/零）与解析失败同罪 400
    #[test]
    fn conv_id_must_be_positive_key() {
        assert_eq!(super::parse_conv_id("12").unwrap(), 12);
        for bad in ["-5", "0", "abc", ""] {
            let (code, _) = super::parse_conv_id(bad).unwrap_err();
            assert_eq!(code, axum::http::StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    /// `reading_of` 的 row_count 兜底：与 rows.len() 取大（analysis/report 共用这一处拼装）
    #[test]
    fn reading_of_row_count_never_below_rows_len() {
        let rows = vec![vec![serde_json::json!(1)], vec![serde_json::json!(2)]];
        let r = super::reading_of(
            "q", "SELECT 1", &[], &rows, Some(1), None, super::ReadingExtras::default(),
        );
        assert_eq!(r.row_count, 2);
        let r = super::reading_of(
            "q", "SELECT 1", &[], &rows, None, None, super::ReadingExtras::default(),
        );
        assert_eq!(r.row_count, 2);
        let r = super::reading_of(
            "q", "SELECT 1", &[], &rows, Some(120), None, super::ReadingExtras::default(),
        );
        assert_eq!(r.row_count, 120);
    }

    #[test]
    fn analysis_request_accepts_all_verified_fact_sources() {
        let req: super::AnalysisReq = serde_json::from_value(serde_json::json!({
            "question": "本月销售额及环比",
            "sql": "SELECT 1000000",
            "columns": ["销售额"],
            "rows": [[1000000]],
            "supplemental": {
                "columns": ["客户", "订单金额"], "rows": [["甲", 250000]], "row_count": 1
            },
            "comparisons": [{
                "label": "环比", "current": 1000000, "baseline": 800000,
                "change": 200000, "pct": 0.25
            }],
            "sales_context": {
                "columns": ["毛利额", "毛利率"], "rows": [[300000, 0.3]]
            }
        }))
        .unwrap();
        assert_eq!(req.supplemental.as_ref().unwrap().rows.len(), 1);
        assert_eq!(req.comparisons[0].baseline, Some(800000.0));
        assert_eq!(req.sales_context.as_ref().unwrap().columns[0], "毛利额");
    }

    fn material() -> super::AnalysisMaterial {
        super::AnalysisMaterial {
            question: "山东本月销售额及环比".into(),
            sql: "SELECT province, SUM(amount) FROM sales GROUP BY province".into(),
            columns: vec!["省份".into(), "销售额".into()],
            rows: vec![vec![serde_json::json!("山东"), serde_json::json!(1_000_000.0)]],
            row_count: Some(1),
            caliber_note: None,
            supplemental: Some(super::AnalysisTable {
                columns: vec!["客户".into(), "金额".into()],
                rows: vec![vec![serde_json::json!("甲"), serde_json::json!(250_000)]],
                row_count: Some(1),
            }),
            comparisons: vec![super::AnalysisComparison {
                label: "环比".into(),
                current: Some(1_000_000.0),
                baseline: Some(800_000.0),
                change: Some(200_000.0),
                pct: Some(25.0),
            }],
            sales_context: Some(super::AnalysisTable {
                columns: vec!["毛利额".into()],
                rows: vec![vec![serde_json::json!(300_000)]],
                row_count: Some(1),
            }),
            subs: vec![],
        }
    }

    fn principal() -> dms_policy::principal::Principal {
        dms_policy::principal::Principal {
            employee_id: 7,
            login_name: "alice".into(),
            actual_name: "Alice".into(),
            administrator_flag: false,
            department_id: Some(8),
            role_id: 9,
            role_code: "province_manager".into(),
        }
    }

    #[test]
    fn analysis_receipt_roundtrip_and_all_fact_fields_are_bound() {
        let original = material();
        let identity = "alice\0province_manager";
        let receipt = super::mint_analysis_receipt(&original, identity);
        assert!(super::verify_analysis_receipt(&original, identity, &receipt));
        assert!(!super::verify_analysis_receipt(&original, "alice\0admin", &receipt));

        let mutations: Vec<(&str, Box<dyn Fn(&mut super::AnalysisMaterial)>)> = vec![
            ("question", Box::new(|m| m.question.push_str("（改）"))),
            ("sql", Box::new(|m| m.sql.push_str(" LIMIT 1"))),
            ("columns", Box::new(|m| m.columns[1] = "毛利额".into())),
            ("rows", Box::new(|m| m.rows[0][1] = serde_json::json!(9_999_999))),
            ("supplemental", Box::new(|m| {
                m.supplemental.as_mut().unwrap().rows[0][1] = serde_json::json!(1)
            })),
            ("comparisons", Box::new(|m| m.comparisons[0].pct = Some(999.0))),
            ("sales_context", Box::new(|m| {
                m.sales_context.as_mut().unwrap().rows[0][0] = serde_json::json!(1)
            })),
            ("subs", Box::new(|m| {
                let mut sub = m.clone();
                sub.subs.clear();
                sub.question = "子问题".into();
                m.subs.push(sub);
            })),
        ];
        for (name, mutate) in mutations {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(
                !super::verify_analysis_receipt(&changed, identity, &receipt),
                "篡改 {name} 必须拒绝"
            );
        }
    }

    #[test]
    fn invalid_or_tampered_material_maps_to_422() {
        let original = material();
        let principal = principal();
        let receipt = super::mint_analysis_receipt(
            &original,
            &super::receipt_identity(&principal),
        );
        assert!(super::require_analysis_receipt(&original, &principal, &receipt).is_ok());

        let mut tampered = original;
        tampered.rows[0][1] = serde_json::json!(9_999_999);
        let (code, axum::Json(body)) =
            super::require_analysis_receipt(&tampered, &principal, &receipt).unwrap_err();
        assert_eq!(code, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body, serde_json::json!({ "error": "分析素材凭证无效，请重新查询" }));

        let (code, _) = super::require_analysis_receipt(&tampered, &principal, "").unwrap_err();
        assert_eq!(code, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn report_receipt_binds_material_and_model_insight() {
        let original = material();
        let identity = "alice\0province_manager";
        let insight = Some("销售额环比增长 25%。");
        let receipt = super::mint_report_receipt(&original, insight, identity);
        assert!(super::verify_report_receipt(&original, insight, identity, &receipt));
        assert!(!super::verify_report_receipt(
            &original,
            Some("销售额环比增长 250%。"),
            identity,
            &receipt,
        ));
        let mut changed = original;
        changed.rows[0][1] = serde_json::json!(1);
        assert!(!super::verify_report_receipt(&changed, insight, identity, &receipt));
    }

    #[test]
    fn ask_projection_matches_browser_numeric_roundtrip_and_rejects_bad_comparison() {
        let original = material();
        let mut payload = serde_json::to_value(&original).unwrap();
        // AskResult 的 comparison 多 dir，sales_context 没 row_count；两者都应被白名单投影规整。
        payload["comparisons"][0]["dir"] = serde_json::json!("up");
        payload["sales_context"].as_object_mut().unwrap().remove("row_count");
        let projected =
            super::AnalysisMaterial::from_ask_payload(&original.question, &payload).unwrap();
        assert_eq!(projected, original);
        let identity = "alice\0province_manager";
        let receipt = super::mint_analysis_receipt(&projected, identity);
        // 模拟 JSON.parse/stringify：1000000.0 会成为整数；canonicalization 后仍能验。
        let browser: super::AnalysisMaterial =
            serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert!(super::verify_analysis_receipt(&browser, identity, &receipt));

        let mut bad = original.clone();
        bad.comparisons[0].change = Some(300_000.0);
        assert!(!super::verify_analysis_receipt(&bad, identity, &receipt));
    }

    #[test]
    fn compound_projection_roundtrips_the_same_material() {
        let child = material();
        let child_payload = serde_json::json!({
            "sql": child.sql,
            "columns": child.columns,
            "rows": child.rows,
            "row_count": child.row_count,
            "caliber_note": child.caliber_note,
            "supplemental": child.supplemental,
            "comparisons": child.comparisons,
            "sales_context": child.sales_context,
        });
        let payload = serde_json::json!({
            "sql": "[复合问题拆解]",
            "columns": [],
            "rows": [],
            "row_count": 0,
            "comparisons": [],
            "subs": [{ "question": "山东销售额", "result": child_payload }],
        });
        let projected =
            super::AnalysisMaterial::from_ask_payload("销售额与环比", &payload).unwrap();
        assert_eq!(projected.subs.len(), 1);
        let identity = "alice\0province_manager";
        let receipt = super::mint_analysis_receipt(&projected, identity);
        let wire: super::AnalysisMaterial =
            serde_json::from_str(&serde_json::to_string(&projected).unwrap()).unwrap();
        assert!(super::verify_analysis_receipt(&wire, identity, &receipt));
    }

    #[test]
    fn attach_receipt_roundtrips_for_plain_and_hybrid_data_payloads() {
        let principal = principal();
        let original = material();
        let mut plain = serde_json::json!({
            "sql": original.sql,
            "columns": original.columns,
            "rows": original.rows,
            "row_count": original.row_count,
            "caliber_note": original.caliber_note,
            "supplemental": original.supplemental,
            "comparisons": original.comparisons,
            "sales_context": original.sales_context,
        });
        assert!(super::attach_analysis_receipt(
            &mut plain,
            &original.question,
            &principal,
        ));
        let receipt = plain["analysis_receipt"].as_str().unwrap();
        let projected =
            super::AnalysisMaterial::from_ask_payload(&original.question, &plain).unwrap();
        assert!(super::require_analysis_receipt(&projected, &principal, receipt).is_ok());

        // hybrid 只是在同一数据 payload 上附加 kb/view.insight；白名单投影不应因此失效。
        plain["kb"] = serde_json::json!({ "kind": "text", "markdown": "资料侧回答" });
        plain["view"] = serde_json::json!({ "insight": "综合回答" });
        let existing = plain["analysis_receipt"].as_str().unwrap().to_string();
        let projected =
            super::AnalysisMaterial::from_ask_payload(&original.question, &plain).unwrap();
        assert!(super::require_analysis_receipt(&projected, &principal, &existing).is_ok());
        assert!(super::attach_analysis_receipt(
            &mut plain,
            &original.question,
            &principal,
        ));
        let receipt = plain["analysis_receipt"].as_str().unwrap();
        assert!(super::require_analysis_receipt(&projected, &principal, receipt).is_ok());
    }

    /// 源码锚点：report 入口的空 question/sql 校验必须排在身份核验之前
    /// （空请求不该白烧一次身份核验，更不该固化出空报表）。
    #[test]
    fn report_rejects_blank_question_or_sql_first() {
        let src = include_str!("insight_api.rs");
        let at = src.find("pub async fn report(").expect("report handler 不见了");
        let body = &src[at..];
        let check = body.find("question 与 sql 不能为空").expect("report 缺空 question/sql 的 400 校验");
        let auth = body.find("require_login").expect("report 缺身份核验");
        assert!(check < auth, "空校验必须在身份核验之前");
    }
}
