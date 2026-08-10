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
//! axum 默认 2MB body limit 就是它的上限）。换来的是零存储、零形状改动，以及
//! **零「读他人已存结果」的越权面** —— 响应里的一切都是调用方自己刚发上来的东西，
//! 服务端不从库里取任何别人的数据。
//!
//! ⚠️ 因此回传的 `sql` / `rows` 都是**不可信输入**：口径说明由 `sql` 派生，故那段文本连同
//! 结果表一律进 `wrap_untrusted`（见 `dms_agent::insight` 的第二条硬线），绝不进 prompt 可信段。

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

/// 请求体 = 前端手上那次 `/api/ask` 结果的四个字段 + 身份。
/// 全部 `#[serde(default)]`（除 `question`/`sql`）：老前端补字段是渐进的，缺 `rows` 也该能出口径说明。
#[derive(serde::Deserialize)]
pub struct AnalysisReq {
    question: String,
    /// 那次**已执行的** SQL（`AskResult.sql`）。口径说明的唯一素材源。
    sql: String,
    #[serde(default)]
    columns: Vec<String>,
    /// 只回传前几行也行（解读只看前 5 行）—— 但 `row_count` 要给真数，否则会把前 5 行当全部
    #[serde(default)]
    rows: Vec<Vec<serde_json::Value>>,
    #[serde(default)]
    row_count: Option<usize>,
    /// `AskResult.caliber_note`（口径复核未通过的标注）。回传它才能让告警印在解读最前面
    #[serde(default)]
    caliber_note: Option<String>,
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
    let (login, role) = crate::resolve_identity(&st, &headers, &req.login_name, &req.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    // 本端点不读库、不取数（素材全在请求体里），核身份只为一件事：
    // **别让「烧 LLM 额度」比问数更便宜** —— 不核的话任意 login_name 都能白刷 fast 调用。
    // 权限集合与脱敏不在这里判：能进这个 body 的行，是调用方上一次问数时已经过闸门给他的。
    principal::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|e| identity_err(&login, e))?;
    let r = dms_agent::Reading {
        question: &req.question,
        sql: &req.sql,
        columns: &req.columns,
        rows: &req.rows,
        row_count: req.row_count.unwrap_or(req.rows.len()),
        caliber_note: req.caliber_note.as_deref(),
    };
    let caliber = r.caliber();
    // 开关只挡 LLM 那一半：关了照样返口径说明（那部分零成本），前端不用改。
    // 【深度模式】`deep=true` 走 Precise 档四段式；精简维持 fast 档 2-4 句。
    let insight = if st.insight_enabled {
        if req.deep { r.insight_deep(&st.llm).await } else { r.insight(&st.llm).await }
    } else {
        None
    };
    Ok(Json(body(caliber, insight)))
}

/// 响应体构造（**纯函数**）：`{ "caliber": …, "insight": … | null }`。
///
/// 🔴 抽出来是因为原来那条断言是**恒真的**：它在测试体里自己 `json!({...})` 造一个字面量、
/// 再断言这个字面量有 `insight` 键 —— 测的是 `serde_json` 的行为，不是 handler 的响应构造。
/// 实测两次打坏都不红：把 handler 改成只返 `caliber`、或改成「`None` 时不插这个键」，
/// 全量测试照旧 139 passed。而这正是前端赖以区分「没解读」与「字段没实现」的那条契约。
/// 现在断言打在这个函数上，删键/skip-null 当场红。
fn body(caliber: String, insight: Option<String>) -> serde_json::Value {
    serde_json::json!({ "caliber": caliber, "insight": insight })
}

// =========================== 【S2】分析报表 artifact 化 ===========================
// datanote 的 CreateArtifactTool 对应物：把一次已生成的解读固化成 `meta.artifact`
// （kind=report），会话气泡出一张「📄 报表」卡，点击右侧沙箱面板预览（S1 地基）。
// **零 LLM**：caliber 服务端从 SQL 重算（不信回传文本），insight 是前端那份的回声
// （用户自己的数据、自己的产物，且 md_to_html escape-first —— 成不了注入）。

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
    /// 前端 `/api/analysis` 那份 `insight` 的原样回声（可缺 = 没有模型解读）
    #[serde(default)]
    insight: Option<String>,
    /// 【图表】`view.blocks` 里 Chart 块的回声（kind/x/y/series/top）。数据不再回传 —
    /// 服务端用已有的 columns/rows 按规格自己取数（回声只是**下标与图型**，信任级同 columns）。
    #[serde(default)]
    charts: Vec<crate::chart_svg::ChartSpec>,
    /// 必填：产物按会话归属（view/download/list 的归属校验全指着它）
    conv_id: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

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
    let total = req.row_count.unwrap_or(req.rows.len());
    if req.rows.len() < total {
        s.push_str(&format!("共 {total} 行（下表为前 {} 行）\n\n", req.rows.len()));
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
        for r in req.rows.iter().take(50) {
            s.push('|');
            for v in r {
                let txt = match v {
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::String(x) => x.clone(),
                    other => other.to_string(),
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
    s.push_str("## SQL\n\n```sql\n");
    s.push_str(req.sql.trim());
    s.push_str("\n```\n");
    s
}

/// `POST /api/analysis/report` → `{ id, title, preview_url, download_url }`
pub async fn report(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ReportReq>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let (login, _role) = crate::resolve_identity(&st, &headers, &req.login_name, &req.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    // 写前校归属：别把报表写进别人的会话（view 那一层也有校验，但脏数据从源头就不该落）
    let cid: i64 = req
        .conv_id
        .parse()
        .map_err(|_| err(StatusCode::BAD_REQUEST, "conv_id 必须是会话主键数字"))?;
    let owner = crate::chat::conv_owner(st.owned.pool(), cid)
        .await
        .map_err(|e| internal_err("insight 服务端读写失败", e))?;
    if owner.as_deref() != Some(login.as_str()) {
        return Err(err(StatusCode::FORBIDDEN, "无权在该会话下生成报表"));
    }
    // caliber 服务端重算（回传的任何文本都不信——口径说明的素材只能是从 SQL 现读）
    let caliber = dms_agent::Reading {
        question: &req.question,
        sql: &req.sql,
        columns: &req.columns,
        rows: &req.rows,
        row_count: req.row_count.unwrap_or(req.rows.len()),
        caliber_note: req.caliber_note.as_deref(),
    }
    .caliber();
    let title: String = req.question.chars().take(40).collect();
    let md = report_md(&req, &caliber);
    // 图表回声 → SVG（退化规格 = 空串 = 占位符原样留，报表不塌）；先渲染再替换，
    // 顺序反了 SVG 会被 md_to_html 当文本转义掉。
    let svgs: Vec<String> =
        req.charts.iter().map(|c| crate::chart_svg::chart_svg(c, &req.columns, &req.rows)).collect();
    let html_body = crate::chart_svg::fill_charts(&crate::artifact_api::md_to_html(&md), &svgs);
    let html = crate::artifact_api::page_shell(&title, &html_body);
    let id = crate::artifact_api::save_artifact(&st, &req.conv_id, "report", &title, &html, &login)
        .await
        .map_err(|e| internal_err("insight 服务端读写失败", e))?;
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
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        for bad in [
            "err(StatusCode::INTERNAL_SERVER_ERROR, e)",
            "err(StatusCode::FORBIDDEN, e)",
        ] {
            assert!(!code.contains(bad), "错误原文泄露回来了：{bad}");
        }
    }

    /// 🔴 响应两个键的形状是与前端的契约：`caliber` 恒有、`insight` 可为 `null`。
    /// 断言打在 **handler 用的那个 `body()`** 上，不是在测试体里自己造一个字面量 ——
    /// 后者测的是 `serde_json`，删键也不会红（实测：改成只返 caliber 仍 139 passed）。
    #[test]
    fn response_keeps_insight_key_even_when_null() {
        let v = super::body("口径：…".into(), None);
        assert!(v.get("insight").is_some(), "键必须在（前端靠它区分「没解读」与「字段没实现」）：{v}");
        assert!(v["insight"].is_null());
        assert_eq!(v["caliber"], "口径：…");
        // 有解读时原样带出（不许被包一层或改名）
        let v2 = super::body("口径：…".into(), Some("这个月比上月涨了 17%".into()));
        assert_eq!(v2["insight"], "这个月比上月涨了 17%");
        assert_eq!(v2["caliber"], "口径：…");
        // 只有这两个键（多一个恒在的键就是一次形状破坏）
        assert_eq!(v2.as_object().unwrap().len(), 2, "{v2}");
    }

    /// 🔴 止血阀（`insight_enabled=false`）：**口径说明一字不少，只是没有模型那段话**。
    /// 原来这条路径判据为零 —— 关掉开关时若连 caliber 一起没了，前端就只剩一个空面板，
    /// 而「解读关着」与「解读失败」在界面上就分不开了。
    ///
    /// 这里钉的是 handler 里那个三目的两侧结果如何进 `body()`（handler 本体要连 LLM 与 MySQL，
    /// 不适合单测；把可判定的那部分——「开关只挡 insight、不挡 caliber」——用 body() 表达）。
    #[test]
    fn disabled_valve_keeps_caliber_and_nulls_insight() {
        for enabled in [true, false] {
            let insight = if enabled { Some("模型那段话".to_string()) } else { None };
            let v = super::body("口径：来源表 t_sales_order；过滤 …".into(), insight);
            assert_eq!(v["caliber"], "口径：来源表 t_sales_order；过滤 …", "开关不许影响口径说明");
            assert_eq!(v["insight"].is_null(), !enabled);
        }
    }

    /// 请求体：只有 `question`/`sql` 是必填，其余缺席即默认（老前端只发这两个也能拿口径说明）
    #[test]
    fn request_needs_only_question_and_sql() {
        let r: super::AnalysisReq =
            serde_json::from_value(serde_json::json!({ "question": "本月销售额", "sql": "SELECT 1" }))
                .expect("最小请求体必须能解析");
        assert!(r.rows.is_empty() && r.columns.is_empty());
        assert_eq!(r.row_count, None);
        assert!(r.caliber_note.is_none());
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
        // SQL 围栏
        assert!(md.contains("```sql\nSELECT 1\nFROM t\n```"), "{md}");
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
}
