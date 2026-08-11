//! 【使用统计 + 样例问题】两个只读端点。变更原因＝个人使用面与知识库冷启动引导。
//!
//! ## 端点契约（路由在 `main.rs` 注册，本文件只供 handler）
//!
//! ### `GET /api/usage/summary?login_name=&role_code=`
//! 本人口径的使用统计（`meta.query_log` 聚合，经 `OwnedStore::fixed` 读 —— 与
//! `quality_api` 同款先例）。身份：`resolve_identity` → `load_principal`（401/403）。
//! ```json
//! { "login_name": "zhangsan", "today": 2, "total": 57, "avg_elapsed_ms": 1834.2,
//!   "deep_ratio": 3.5, "kb_ratio": 0.0,
//!   "routes": [{"route": "llm", "count": 40}],
//!   "daily":  [{"day": "2026-08-02", "count": 1}],          // 近 7 天逐日（含今天，缺日补 0）
//!   "global": { "today": …, "total": …, "avg_elapsed_ms": …, "deep_ratio": …,
//!               "kb_ratio": …, "routes": […], "daily": […] } }   // ⚠️ 仅管理员出现
//! ```
//! - **本人过滤**是 SQL 内的 `login_name = $1`（fail-closed 内联，不是查完再过滤）。
//! - `global` 块只对 `is_admin(p)` 出现：判据 = `administrator_flag || role_code == "admin"`，
//!   读的是 **DMS 校验后的 `Principal`**（`load_principal` 只在 `administrator_flag` 为真时才
//!   授得出 `admin` 角色），不是请求里可伪造的 `role_code` 串 —— 与 `admin_api` 那条纪律同源。
//! - 全局块与本人块**共用同三条 SQL**（`$1 IS NULL` = 全局）：两套口径写两份必漂。
//!
//! 两个占比的口径声明（query_log 里能 structurally 拿到的最近信号，别当精确语义读）：
//! - `deep_ratio`：`llm_calls >= 2` 的行占比。深度模式 SC≥3 的 LLM 路径恒 ≥2 发 precise
//!   （两发指纹一致才提前收工）；普通路径含自修也会 ≥2。即「多轮采样或多轮自修」的合并指纹。
//! - `kb_ratio`：知识库两路的合并占比 —— 上传表格源的问数（`ds_id LIKE 'upload\_%'`）
//!   ∪ 文档问答（`route = 'knowledge'`）。Y2 起 KB 问答也落 query_log（knowledge 层
//!   `qa_log` 统一埋点，`/api/kb/ask` 与 `/api/ask` 分诊分支同一条），这里按行直读即可。
//!
//! ### `GET /api/kb/sample-questions?space_id=&login_name=&role_code=`
//! 该空间可见文档的 5 条样例问题。**绝不报错**（LLM/缓存/取块失败一律回退）：
//! ```json
//! { "space_id": "kb-hr", "questions": ["…"], "source": "llm|fallback|empty|cache",
//!   "cached": false }
//! ```
//! - `space_id` 必填（400）；空间不可读 403（`acl::space_readable`，fail-closed）。
//! - 缓存：`meta.kv['kb_samples:{space_id}']`，24h 有效（读写走 `admin_api::KV_*_SQL` 同两张
//!   语句）。**按空间共享是 ACL 安全的**：过了 space_readable 闸的读者在空间粒度上看到同一组
//!   文档（doc 级授权只在无空间授权时生效，而那种请求在闸口就 403 了），先后者给后来者
//!   留下的问题集不会透出后者看不见的文档。
//! - 取块两道 ACL：文档清单走 `store::list_docs`（`visible_docs` 内联）；正文块的 SQL 里
//!   再内联一次空间级谓词（撤权发生在两步之间也不会带出正文 —— 同 `retrieve` 两步内联的理由）。
//! - LLM（fast 档，20s 超时）失败/空解析 → 按文档名拼保守问题；无可见文档 → 空数组。
//!   回退产物**也进缓存**（模型欠费时每个请求都重试一次 LLM = 白烧 20s × N）；
//!   `"empty"`（无可见文档 / 文档名全是怪名）**不缓存** —— 空间刚传完文档，
//!   不能被 24h 的空样例挡住。
//! - 已知取舍：缓存 miss 无 singleflight，并发 N 个请求会各打一次 20s LLM
//!   （KV_SET 是 upsert，写侧安全，纯浪费）。单飞要跨请求共享状态（动 `AppState`），
//!   超出本文件自治范围，留此备查。

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use dms_kernel::{ChatModel, ChatRequest, ModelTier};
use dms_knowledge::{acl, store, Viewer};

use crate::admin_api::{err, ApiErr};
use crate::AppState;

// ───────────────────────── ① 使用统计 ─────────────────────────

/// 本人/全局同一条：`$1` 给 login = 本人口径；`$1` 给 NULL = 全局（只许 `is_admin` 那条路给 NULL）。
/// `at >= CURRENT_DATE` ＝今日（会话时区，与 `quality_api` 的 `now()` 口径同库同 zone）。
const SUMMARY_SQL: &str =
    "SELECT count(*)::bigint,
       count(*) FILTER (WHERE at >= CURRENT_DATE)::bigint,
       COALESCE(avg(elapsed_ms),0)::float8,
       COALESCE(100.0*count(*) FILTER (WHERE llm_calls >= 2)/NULLIF(count(*),0),0)::float8,
       COALESCE(100.0*count(*) FILTER (WHERE ds_id LIKE 'upload\\_%' OR route = 'knowledge')/NULLIF(count(*),0),0)::float8
     FROM meta.query_log WHERE ($1::text IS NULL OR login_name = $1)";

/// 空 route ＝失败行（与 `quality_api` 同一显示口径）
const ROUTES_SQL: &str =
    "SELECT COALESCE(NULLIF(route,''),'失败') route, count(*)::bigint
     FROM meta.query_log WHERE ($1::text IS NULL OR login_name = $1)
     GROUP BY 1 ORDER BY count(*) DESC";

/// 近 7 天逐日（含今天），缺日补 0：`generate_series` 出全日期再 LEFT JOIN，
/// 不做「查回应用层补零」—— 补零逻辑写第二遍就是第二个事实源。
/// ⚠️ `generate_series(date,date,interval)` 的产出是 **timestamp**（date 被隐式抬升），
/// 直接 `::text` 会得到 '2026-08-02 00:00:00' —— 必须先 `::date` 再 `::text`。
const DAILY_SQL: &str =
    "SELECT d.day::date::text, count(q.id)::bigint
     FROM generate_series(CURRENT_DATE - 6, CURRENT_DATE, interval '1 day') d(day)
     LEFT JOIN meta.query_log q ON q.at::date = d.day::date
       AND ($1::text IS NULL OR q.login_name = $1)
     GROUP BY d.day ORDER BY d.day";

type SummaryRow = (i64, i64, f64, f64, f64);
type RouteRow = (String, i64);
type DailyRow = (String, i64);

/// 管理员判据：DMS 校验后的 `Principal` 上读（见文件头）。抽成纯函数是为了能钉单测。
/// 口径互指：`admin_api::is_admin` 只认 `administrator_flag`，本处多认 DMS 授出的 `admin`
/// 角色（全局用量块口径）——差异是两边各自语义而非漂移，改任何一边都要评审另一边。
fn is_admin(p: &dms_policy::Principal) -> bool {
    p.administrator_flag || p.role_code == "admin"
}

#[derive(serde::Deserialize, Default)]
pub struct UsageQuery {
    login_name: Option<String>,
    role_code: Option<String>,
}

/// 三条聚合同一口径跑一遍；`login = None` 即全局（调用方必须先过 `is_admin`，fail-closed：
/// 本函数自己不判权限，给不给 NULL 的决策只在 `usage_summary` 里那一处）。
/// 三条 SQL 互不依赖，`try_join!` 并行；任一失败整体 500（同一通用文案，无优先级歧义）。
async fn usage_block(
    st: &AppState,
    login: Option<&str>,
) -> Result<serde_json::Value, ApiErr> {
    // DB 错误不透 sqlx 原文（与 `sample_questions` 的通用文案同一条纪律）：warn 留痕 + 通用 500
    let db_err = |e: dms_connector::ConnectorError| {
        tracing::warn!(err = %e, "usage 聚合查询失败");
        err(StatusCode::INTERNAL_SERVER_ERROR, "统计数据暂时不可用，请稍后重试")
    };
    let (summary, routes, daily) = tokio::try_join!(
        st.owned.fixed(SUMMARY_SQL).bind(login).fetch_optional::<SummaryRow>(),
        st.owned.fixed(ROUTES_SQL).bind(login).fetch_all::<RouteRow>(),
        st.owned.fixed(DAILY_SQL).bind(login).fetch_all::<DailyRow>(),
    )
    .map_err(db_err)?;
    // 无 GROUP BY 的聚合恒返一行，`fetch_optional` 的 None 分支不存在（`PgStmt` 没有
    // fetch_one，用 expect 钉住这个不变量）
    let summary = summary.expect("SUMMARY_SQL 是无 GROUP BY 聚合，恒返一行");
    Ok(serde_json::json!({
        "today": summary.1,
        "total": summary.0,
        "avg_elapsed_ms": summary.2,
        "deep_ratio": summary.3,
        "kb_ratio": summary.4,
        "routes": routes.into_iter().map(|x| serde_json::json!({
            "route": x.0, "count": x.1
        })).collect::<Vec<_>>(),
        "daily": daily.into_iter().map(|x| serde_json::json!({
            "day": x.0, "count": x.1
        })).collect::<Vec<_>>(),
    }))
}

pub async fn usage_summary(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let (login, role) = crate::resolve_identity(&st, &headers, &q.login_name, &q.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let p = crate::auth::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(principal_err)?;
    // 全局块只在这一处挂上：判据在 DMS 校验后的 Principal 上（fail-closed 内联分支）。
    // admin 时本人块与全局块（各三条 SQL）互不依赖，`try_join!` 并行。
    let (mut body, global) = if is_admin(&p) {
        let (b, g) = tokio::try_join!(
            usage_block(&st, Some(&p.login_name)),
            usage_block(&st, None),
        )?;
        (b, Some(g))
    } else {
        (usage_block(&st, Some(&p.login_name)).await?, None)
    };
    let obj = body.as_object_mut().expect("usage_block 恒返对象");
    if let Some(global) = global {
        obj.insert("global".into(), global);
    }
    obj.insert("login_name".into(), serde_json::Value::from(p.login_name));
    Ok(Json(body))
}

/// `load_principal` 的错误分两档：查无此人/角色不可用（anyhow 文案）→ 403；
/// DB 故障（`ConnectorError`：auth MySQL 超时/宕机）→ warn + 500 —— DB 故障报权限错误
/// 会把排障方向带歪。
fn principal_err(e: anyhow::Error) -> ApiErr {
    if e.downcast_ref::<dms_connector::ConnectorError>().is_some() {
        tracing::warn!(err = %e, "DMS 身份查询失败（auth MySQL）");
        err(StatusCode::INTERNAL_SERVER_ERROR, "身份服务暂时不可用，请稍后重试")
    } else {
        err(StatusCode::FORBIDDEN, "当前 DMS 身份或角色不可用")
    }
}

// ───────────────────────── ② 样例问题 ─────────────────────────

/// 缓存键前缀与有效期（24h）。键形：`kb_samples:{space_id}`
const CACHE_PREFIX: &str = "kb_samples:";
const CACHE_TTL_SECS: u64 = 24 * 3600;

/// fast 调用预算（与 `kb_mindmap_api` 的分支标签同一档：这是锦上添花，不许拖住页面）
const LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// 参与生成的文档/块上限：prompt 体量封顶（6 篇 × 2 块 × 300 字 ≈ 4k 字符内）
const MAX_DOCS: usize = 6;
const CHUNKS_PER_DOC: usize = 2;
const CHUNK_CLIP_CHARS: usize = 300;

/// 正文块抽取。两道防线各自的形状：
/// - `c.doc_id = ANY($1)`：候选集来自 `store::list_docs`（`visible_docs` 已内联过滤）；
/// - 正文侧再内联一次**空间级**读谓词（`acl::space_readable` 同一形状）：撤权若发生在
///   清单与取块之间，这条语句一行都返不出（同 `retrieve` 两步内联的理由）。
/// 只内联空间级而不是整份 `visible_docs`：本端点先过了 `space_readable` 闸，doc 级授权
/// （无空间授权的补充通道）在这条路径上不可达，引整份片段只会多一层永不命中的分支。
/// `c.ord < $4`（$4 = `CHUNKS_PER_DOC`）库侧截断：每篇只用前 2 块，不把全部 chunk
/// 拉回应用层再丢（大文档可达数千块）。
const CHUNK_SQL: &str =
    "SELECT c.doc_id, c.text FROM kb.chunk c JOIN kb.doc d ON d.doc_id = c.doc_id
     WHERE c.doc_id = ANY($1::text[])
       AND c.ord < $4
       AND d.enabled = true AND d.status IN ('chunked','embedded')
       AND EXISTS (SELECT 1 FROM kb.space s WHERE s.space_id = d.space_id
         AND (s.owner = $2 OR EXISTS (SELECT 1 FROM kb.acl a
           WHERE a.scope = 'space' AND a.target_id = s.space_id
             AND a.perm IN ('read','write')
             AND ((a.grantee_kind = 'login' AND a.grantee = $2)
               OR (a.grantee_kind = 'role' AND a.grantee = ANY($3::text[]))))))
     ORDER BY c.doc_id, c.ord";

fn cache_key(space_id: &str) -> String {
    format!("{CACHE_PREFIX}{space_id}")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 缓存值：`{"at": <unix>, "questions": [...]}`。任何畸形一律当 miss（回退路径照样能产出），
/// 绝不能因为一行坏缓存把端点打挂。`at` 在未来（时钟回拨/脏数据）同样当 miss ——
/// 否则差值饱和为 0，旧问题集会被钉住直到那个未来时刻到期。
fn cache_parse(v: &str, now: u64) -> Option<Vec<String>> {
    let j: serde_json::Value = serde_json::from_str(v).ok()?;
    let at = j.get("at")?.as_u64()?;
    if at > now || now - at >= CACHE_TTL_SECS {
        return None;
    }
    let qs = j.get("questions")?.as_array()?;
    Some(qs.iter().filter_map(|q| q.as_str().map(str::to_string)).collect())
}

fn cache_render(questions: &[String]) -> String {
    serde_json::json!({ "at": now_secs(), "questions": questions }).to_string()
}

/// LLM 产出 → 问题清单（**容错是本体**：模型回什么形态的都有）。
/// 逐行：剥序号/项目符号前缀 → trim → 去重保序 → 每条封顶 60 字 → 取前 5。
/// 一行都解析不出 = 空 Vec（调用方走回退，不报错）。
fn parse_questions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        // 剥行首的「1.」「1、」「1)」「- 」「• 」「* 」一族：仅当数字串后紧跟
        // 序号分隔符才视作编号剥掉 —— 「2026年预算怎么定？」是合法问题，不能削成
        // 「年预算怎么定？」；项目符号（非数字开头）照旧整段剥
        let line = line.trim();
        let digits = line.bytes().take_while(u8::is_ascii_digit).count();
        let after = &line[digits..];
        let is_numbered = digits > 0
            && after
                .chars()
                .next()
                .is_some_and(|c| matches!(c, '.' | '、' | ')' | '）' | ':' | '：'));
        let q = if is_numbered {
            after.trim_start_matches(|c: char| {
                matches!(c, '.' | '、' | ')' | '）' | '-' | '•' | '*' | ':' | '：' | ' ')
            })
        } else {
            line.trim_start_matches(|c: char| matches!(c, '-' | '•' | '*' | ' '))
        }
        .trim();
        if q.is_empty() {
            continue;
        }
        // 纯符号行（"。''"——"一族）不是问题：至少要有一个文字/数字字符
        if !q.chars().any(char::is_alphanumeric) {
            continue;
        }
        let q: String = q.chars().take(60).collect();
        if !out.iter().any(|x| x == &q) {
            out.push(q);
        }
        if out.len() >= 5 {
            break;
        }
    }
    out
}

/// 回退问题：按文档名（去扩展名）拼保守问法。确定性、零 LLM、永不失败。
fn fallback_questions(doc_names: &[String]) -> Vec<String> {
    const TEMPLATES: &[&str] = &["《{}》的主要内容是什么？", "请总结一下《{}》的要点"];
    let mut out = Vec::new();
    for name in doc_names {
        if out.len() >= 5 {
            break;
        }
        // 文件名去扩展名：「报销制度v2.pdf」→「报销制度v2」。仅当末段像扩展名
        // （≤5 字符且纯 ASCII 字母数字）才剥 —— 「v1.2报销制度」的点在中部不是
        // 扩展名，剥了会截成「v1」。去完是空的（".pdf" 这种怪名）与空名一律跳过
        // —— 产出《.pdf》还不如少一条
        let stem = match name.rsplit_once('.') {
            Some((stem, ext))
                if !ext.is_empty()
                    && ext.len() <= 5
                    && ext.bytes().all(|b| b.is_ascii_alphanumeric()) =>
            {
                stem.trim()
            }
            _ => name.trim(),
        };
        if stem.is_empty() {
            continue;
        }
        // 模板按产出数轮换：被跳过的怪名不占轮换位（产出不随跳过数漂）
        let q: String = TEMPLATES[out.len() % TEMPLATES.len()].replace("{}", stem);
        if !out.contains(&q) {
            out.push(q);
        }
    }
    out
}

#[derive(serde::Deserialize, Default)]
pub struct SampleQuery {
    space_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

pub async fn sample_questions(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<SampleQuery>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let space_id = q
        .space_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= 64)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "space_id 必填（≤64 字符）"))?;
    let (login, role) = crate::resolve_identity(&st, &headers, &q.login_name, &q.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let p = crate::auth::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(principal_err)?;
    let v = Viewer::new(p.login_name, vec![p.role_code]);
    // 空间级闸（fail-closed）：不可读的空间连「有没有文档」都不许探出来
    let readable = acl::space_readable(&st.owned, &v, space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
    if !readable {
        return Err(err(StatusCode::FORBIDDEN, format!("无权访问知识空间 {space_id}")));
    }

    // 缓存命中直接返（坏行当 miss，见 `cache_parse`）。**读失败也降级为 miss**：
    // 缓存是优化不是正确性（写失败同样只 warn，见下）——不许一行坏缓存把端点打挂
    let key = cache_key(space_id);
    let cached: Option<(String,)> = match st
        .owned
        .fixed(crate::admin_api::KV_GET_SQL)
        .bind(&key)
        .fetch_optional()
        .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(space_id, err = %e, "样例问题缓存读取失败（降级为 miss 重算）");
            None
        }
    };
    if let Some(questions) = cached.and_then(|(v,)| cache_parse(&v, now_secs())) {
        return Ok(Json(serde_json::json!({
            "space_id": space_id, "questions": questions, "source": "cache", "cached": true,
        })));
    }

    let (questions, source) = generate_samples(&st, &v, space_id).await;
    // 回退产物也写缓存（见文件头）；`"empty"` 不缓存 —— 空间刚传完文档，
    // 不能被 24h 的空样例挡住。写失败只 warn —— 缓存是优化，不是正确性
    if source != "empty" {
        if let Err(e) = st
            .owned
            .fixed(crate::admin_api::KV_SET_SQL)
            .bind(&key)
            .bind(cache_render(&questions))
            .execute()
            .await
        {
            tracing::warn!(space_id, err = %e, "样例问题缓存写入失败（降级不缓存）");
        }
    }
    Ok(Json(serde_json::json!({
        "space_id": space_id, "questions": questions, "source": source, "cached": false,
    })))
}

/// 产出问题集。`(questions, source)`：`source ∈ {llm, fallback, empty}`。
/// 任何一步失败都向下一档降级，这个函数本身不返回 Err。
async fn generate_samples(st: &AppState, v: &Viewer, space_id: &str) -> (Vec<String>, &'static str) {
    // 可见文档清单（ACL 在 list_docs 的 SQL 里）：既是候选集也是回退的素材。
    // 端点纪律是「绝不报错」，但排障侧该留痕：失败 warn 一条再降级空清单
    let docs = match store::list_docs(&st.owned, v, space_id).await {
        Ok(docs) => docs,
        Err(e) => {
            tracing::warn!(space_id, err = %e, "样例问题取文档清单失败（降级为空清单）");
            Vec::new()
        }
    };
    let docs: Vec<(String, String)> = docs
        .into_iter()
        .filter(|d| d.enabled && matches!(d.status.as_str(), "chunked" | "embedded"))
        .take(MAX_DOCS)
        .map(|d| (d.doc_id, d.name))
        .collect();
    if docs.is_empty() {
        return (Vec::new(), "empty");
    }
    let doc_names: Vec<String> = docs.iter().map(|(_, n)| n.clone()).collect();

    let doc_ids: Vec<String> = docs.iter().map(|(id, _)| id.clone()).collect();
    // 同上的纪律：取块失败 warn 留痕再降级（下游按无摘录走 LLM/回退）
    let chunks: Vec<(String, String)> = match st
        .owned
        .fixed(CHUNK_SQL)
        .bind(&doc_ids)
        .bind(&v.login)
        .bind(&v.roles)
        .bind(CHUNKS_PER_DOC as i64)
        .fetch_all()
        .await
    {
        Ok(chunks) => chunks,
        Err(e) => {
            tracing::warn!(space_id, err = %e, "样例问题取正文块失败（降级为无摘录）");
            Vec::new()
        }
    };
    // 每篇取前 CHUNKS_PER_DOC 块（开头是标题/导语，最具代表性），每块截 300 字。
    // CHUNK_SQL 按 (doc_id, ord) 有序，同篇的块连续到达；**按 doc_id 分组**——文档可重名，
    // 按名分组会把两篇同名文档的摘录并进一条。
    let name_of: std::collections::HashMap<&str, &str> =
        docs.iter().map(|(id, n)| (id.as_str(), n.as_str())).collect();
    let mut by_doc: Vec<(&str, Vec<String>)> = Vec::new(); // (doc_id, clips)
    for (doc_id, text) in &chunks {
        if by_doc.last().map(|(cur, _)| *cur) != Some(doc_id.as_str()) {
            by_doc.push((doc_id, Vec::new()));
        }
        let (_, clips) = by_doc.last_mut().expect("刚压入一组");
        if clips.len() < CHUNKS_PER_DOC {
            clips.push(text.chars().take(CHUNK_CLIP_CHARS).collect());
        }
    }
    let excerpts: Vec<(&str, Vec<String>)> = by_doc
        .into_iter()
        .map(|(id, clips)| (name_of.get(id).copied().unwrap_or(""), clips))
        .collect();

    if let Some(qs) = llm_samples(st, &excerpts).await {
        if !qs.is_empty() {
            return (qs, "llm");
        }
    }
    // 回退也可能一条都产不出（文档名全是怪名）：空集与 `"empty"` 同义，不混报 `"fallback"`
    let qs = fallback_questions(&doc_names);
    if qs.is_empty() { (qs, "empty") } else { (qs, "fallback") }
}

/// fast 档生成 5 问；失败/超时/空解析一律 `None`（调用方回退，绝不把错误透出端点）。
async fn llm_samples(st: &AppState, excerpts: &[(&str, Vec<String>)]) -> Option<Vec<String>> {
    if excerpts.is_empty() {
        return None;
    }
    const SYSTEM: &str = "你是知识库助手。根据用户提供的文档摘录，生成用户最可能向这个知识库提出的 5 个问题。\
        只输出问题本身，每行一个，不要编号、不要解释、不要输出其他内容。问题用中文，每条不超过 40 字。";
    let mut user = String::new();
    for (name, texts) in excerpts {
        user.push_str("文档《");
        user.push_str(name);
        user.push_str("》摘录：\n");
        for t in texts {
            user.push_str(t);
            user.push('\n');
        }
        user.push('\n');
    }
    let mut req = ChatRequest::text(ModelTier::Fast, SYSTEM, &user, Some(0.1));
    req.max_tokens = Some(400);
    let reply = match tokio::time::timeout(LLM_TIMEOUT, st.llm.chat(req)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(err = %e, "样例问题 fast 调用失败 → 回退文档名问题");
            return None;
        }
        Err(_) => {
            tracing::warn!("样例问题 fast 调用超时 → 回退文档名问题");
            return None;
        }
    };
    let parsed = parse_questions(reply.content.as_deref().unwrap_or_default());
    (!parsed.is_empty()).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(administrator_flag: bool, role_code: &str) -> dms_policy::Principal {
        dms_policy::Principal {
            employee_id: 1,
            login_name: "zhangsan".into(),
            actual_name: "张三".into(),
            administrator_flag,
            department_id: None,
            role_id: 0,
            role_code: role_code.into(),
        }
    }

    /// 权限分支：两条路径都认（flag / DMS 校验后的 admin 角色），普通角色一律否
    #[test]
    fn admin_branch_reads_validated_principal() {
        assert!(is_admin(&principal(true, "city_manager")), "administrator_flag 优先");
        assert!(is_admin(&principal(false, "admin")), "DMS 授出的 admin 角色");
        assert!(!is_admin(&principal(false, "city_manager")));
        assert!(!is_admin(&principal(false, "")));
    }

    /// 全局块必须挂在 is_admin 分支之后（fail-closed 内联：源锚点，守「分支被挪走/删掉」）
    #[test]
    fn global_block_is_inside_admin_branch() {
        let src = include_str!("usage_api.rs");
        let body = src.split("pub async fn usage_summary").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let gate = body.find("if is_admin(&p)").expect("admin 分支不在了");
        let global = body.find("usage_block(&st, None)").expect("全局取数不在了");
        assert!(gate < global, "全局块必须先过 is_admin 再取数: {body}");
        // 本人路径永远带 login（None 只许出现在 admin 分支里）
        assert!(body.contains("usage_block(&st, Some(&p.login_name))"), "{body}");
    }

    /// 聚合 SQL 形状锚点：本人过滤内联、今日口径、深度/知识库指纹、补零序列
    #[test]
    fn summary_sql_shape_anchors() {
        for sql in [SUMMARY_SQL, ROUTES_SQL, DAILY_SQL] {
            assert!(sql.contains("meta.query_log"), "{sql}");
            // 内联本人过滤（DAILY 在 JOIN 侧用限定名 q.login_name，故拆两半断言）
            assert!(sql.contains("($1::text IS NULL OR"), "{sql}");
            assert!(sql.contains("login_name = $1)"), "本人过滤必须内联在 SQL 里: {sql}");
        }
        assert!(SUMMARY_SQL.contains("at >= CURRENT_DATE"), "今日口径: {SUMMARY_SQL}");
        assert!(SUMMARY_SQL.contains("llm_calls >= 2"), "深度指纹: {SUMMARY_SQL}");
        assert!(SUMMARY_SQL.contains(r"ds_id LIKE 'upload\_%'"), "知识库上传源指纹: {SUMMARY_SQL}");
        assert!(SUMMARY_SQL.contains("route = 'knowledge'"), "知识库文档问答指纹（Y2 落账行）: {SUMMARY_SQL}");
        assert!(SUMMARY_SQL.contains("avg(elapsed_ms)"), "{SUMMARY_SQL}");
        assert!(ROUTES_SQL.contains("GROUP BY 1"), "{ROUTES_SQL}");
        assert!(ROUTES_SQL.contains("COALESCE(NULLIF(route,''),'失败')"), "与 quality_api 同口径: {ROUTES_SQL}");
        assert!(DAILY_SQL.contains("generate_series(CURRENT_DATE - 6, CURRENT_DATE"), "近 7 天补零: {DAILY_SQL}");
        assert!(DAILY_SQL.contains("LEFT JOIN"), "{DAILY_SQL}");
        // generate_series 给的是 timestamp：不先 ::date 就会把 '2026-08-02 00:00:00' 发给前端
        assert!(DAILY_SQL.contains("d.day::date::text"), "逐日键必须是日期形: {DAILY_SQL}");
    }

    /// 样例问题解析容错：序号/符号/空行/重复/超长/全垃圾 六种形态
    #[test]
    fn parse_questions_tolerates_model_formats() {
        let text = "1. 报销上限是多少？\n2、差旅住宿标准是什么？\n- 请假流程怎么走\n\
                    3) 报销上限是多少？\n\n• 考勤规则有哪些\n* 加班如何申请\n";
        let qs = parse_questions(text);
        assert_eq!(qs.len(), 5, "去重后应剩 5 条: {qs:?}");
        assert_eq!(qs[0], "报销上限是多少？");
        assert!(qs.iter().all(|q| !q.starts_with(|c: char| c.is_ascii_digit())), "序号必须剥掉: {qs:?}");
        // 超长截 60 字
        let long = format!("{}\n", "问".repeat(100));
        assert_eq!(parse_questions(&long)[0].chars().count(), 60);
        // 全垃圾/空 → 空集（调用方回退，不报错）
        assert!(parse_questions("").is_empty());
        assert!(parse_questions(" \n- \n。\n").is_empty());
        // 不足 5 条不硬凑
        assert_eq!(parse_questions("只有一条问题").len(), 1);
        // 数字开头但不是序号（后无分隔符）：合法问题原样保留，不削前缀
        assert_eq!(parse_questions("2026年预算怎么定？"), ["2026年预算怎么定？"]);
        assert_eq!(parse_questions("3 个报销档位分别是？"), ["3 个报销档位分别是？"]);
    }

    /// 回退问题：文档名去扩展名、模板轮换、封顶 5 条、怪名不炸
    #[test]
    fn fallback_questions_are_conservative_and_bounded() {
        let docs = vec![
            "报销制度v2.pdf".to_string(),
            "考勤规则.docx".to_string(),
            "无扩展名".to_string(),
            ".pdf".to_string(), // 空 stem：跳过（产出《.pdf》不如少一条）
            "".to_string(),     // 空名：跳过
        ];
        let qs = fallback_questions(&docs);
        assert_eq!(qs.len(), 3, "{qs:?}");
        assert!(qs[0].contains("报销制度v2"), "{qs:?}");
        assert!(qs[1].contains("考勤规则"), "{qs:?}");
        assert!(qs.iter().all(|q| !q.contains(".pdf") && !q.contains(".docx")), "扩展名必须剥: {qs:?}");
        assert!(fallback_questions(&[]).is_empty());
        let many: Vec<String> = (0..9).map(|i| format!("文档{i}")).collect();
        assert_eq!(fallback_questions(&many).len(), 5);
        // 点在中部不是扩展名（后缀非纯字母数字）：「v1.2报销制度」不许截成「v1」
        let qs = fallback_questions(&["v1.2报销制度".to_string()]);
        assert!(qs[0].contains("v1.2报销制度"), "中部点不许当扩展名剥: {qs:?}");
        // 真扩展名照旧剥：超 5 字符的后缀（".markdown"）不是扩展名，保留全名
        let qs = fallback_questions(&["手册.markdown".to_string()]);
        assert!(qs[0].contains("手册.markdown"), "超 5 字符后缀不剥: {qs:?}");
        // 怪名被跳过后模板轮换按产出数走，不占位
        let qs = fallback_questions(&[".pdf".to_string(), "甲制度".to_string(), "乙办法".to_string()]);
        assert_eq!(qs.len(), 2, "{qs:?}");
        assert!(qs[0].starts_with("《甲制度》的主要") && qs[1].starts_with("请总结"), "轮换不按跳过数漂: {qs:?}");
    }

    /// 缓存键形与 24h 过期；坏行一律 miss
    #[test]
    fn cache_key_shape_and_ttl() {
        assert_eq!(cache_key("kb-hr"), "kb_samples:kb-hr");
        assert_eq!(CACHE_PREFIX, "kb_samples:");
        assert_eq!(CACHE_TTL_SECS, 86_400, "24h 契约");
        let now = 1_800_000_000u64;
        let good = serde_json::json!({ "at": now - 100, "questions": ["q1", "q2"] }).to_string();
        assert_eq!(cache_parse(&good, now).unwrap(), vec!["q1".to_string(), "q2".to_string()]);
        // 边界：差 1 秒到期的仍有效，刚好 24h 的已过期
        let edge = serde_json::json!({ "at": now - CACHE_TTL_SECS + 1, "questions": ["q"] }).to_string();
        assert!(cache_parse(&edge, now).is_some());
        let stale = serde_json::json!({ "at": now - CACHE_TTL_SECS, "questions": ["q"] }).to_string();
        assert!(cache_parse(&stale, now).is_none(), "满 24h 即过期");
        // at 在未来（时钟回拨/脏数据）按 miss：不许饱和差把旧问题集钉到未来时刻
        let future = serde_json::json!({ "at": now + 3600, "questions": ["q"] }).to_string();
        assert!(cache_parse(&future, now).is_none(), "未来 at 必须按 miss");
        // 畸形一律 miss：坏 JSON / 缺 at / 缺 questions / at 非数
        assert!(cache_parse("not json", now).is_none());
        assert!(cache_parse("{}", now).is_none());
        assert!(cache_parse(r#"{"at":1}"#, now).is_none());
        assert!(cache_parse(r#"{"at":"x","questions":[]}"#, now).is_none());
        // 写读回环
        let rt = cache_parse(&cache_render(&["甲".into()]), now_secs()).unwrap();
        assert_eq!(rt, vec!["甲".to_string()]);
    }

    /// 取块 SQL 的 ACL 锚点（双侧）：本文件的常量与 `acl::space_readable` 的谓词必须同形状 ——
    /// 上游改了空间读判据，这里不改就当场红（防两份 ACL 各漂各的）。
    /// acl.rs 侧判据已宏化（`space_acl_sql!`，读/写共用一条形状、perm 是参数）：钉宏体谓词
    /// 碎片 + `space_readable` 传给宏的读权限实参，两侧任一漂移当场红。
    #[test]
    fn chunk_sql_inlines_space_acl_same_shape_as_acl_rs() {
        let acl_src = include_str!("../../knowledge/src/acl.rs");
        let mac = acl_src.split("macro_rules! space_acl_sql").nth(1).unwrap();
        let mac = mac.split("\n}").next().unwrap();
        for frag in ["a.scope='space'", "a.grantee_kind='login'", "a.grantee_kind='role'"] {
            assert!(mac.contains(frag), "acl.rs 的谓词变了: {frag}");
            assert!(
                CHUNK_SQL.replace(' ', "").contains(&frag.replace(' ', "")),
                "取块 SQL 的空间谓词与 acl.rs 不同步: {frag}"
            );
        }
        let readable = acl_src.split("pub async fn space_readable").nth(1).unwrap();
        let readable = readable.split("\n}\n").next().unwrap();
        assert!(readable.contains("a.perm IN ('read','write')"), "读权限谓词: {readable}");
        assert!(CHUNK_SQL.contains("a.perm IN ('read','write')"), "读权限谓词: {CHUNK_SQL}");
        assert!(CHUNK_SQL.contains("s.owner = $2") && CHUNK_SQL.contains("ANY($3::text[])"), "{CHUNK_SQL}");
        assert!(CHUNK_SQL.contains("c.doc_id = ANY($1::text[])"), "候选集只许来自 ACL 过的清单: {CHUNK_SQL}");
        assert!(CHUNK_SQL.contains("d.enabled = true") && CHUNK_SQL.contains("('chunked','embedded')"), "{CHUNK_SQL}");
        // 库侧截断：每篇只用前 CHUNKS_PER_DOC 块，不把全量 chunk 拉回应用层再丢
        assert!(CHUNK_SQL.contains("c.ord < $4"), "每篇块数的库侧截断没了: {CHUNK_SQL}");
    }

    /// 缓存纪律锚点：读失败降级 miss 不 500（warn 留痕）；`"empty"` 不写缓存；
    /// fallback 产出为空时 `source` 归 `"empty"` 不混报
    #[test]
    fn cache_discipline_anchors() {
        let src = include_str!("usage_api.rs");
        let body = src.split("pub async fn sample_questions").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("样例问题缓存读取失败"), "读失败必须 warn 留痕: {body}");
        assert!(body.contains("None"), "读失败降级为 miss: {body}");
        assert!(
            body.contains(r#"if source != "empty""#),
            "empty 结果不许进 24h 缓存: {body}"
        );
        let gen = src.split("async fn generate_samples").nth(1).unwrap();
        let gen = gen.split("\n}\n").next().unwrap();
        assert!(gen.contains(r#"(qs, "empty")"#), "fallback 空集要归 empty: {gen}");
    }

    /// `load_principal` 错误分档锚点：DB 故障（ConnectorError）500 + warn，
    /// 查无此人/角色不可用 403 —— DB 宕机报权限错误会把排障方向带歪
    #[test]
    fn principal_err_splits_db_failure_from_forbidden() {
        let src = include_str!("usage_api.rs");
        let helper = src.split("fn principal_err").nth(1).unwrap();
        let helper = helper.split("\n}\n").next().unwrap();
        assert!(helper.contains("downcast_ref::<dms_connector::ConnectorError>"), "{helper}");
        assert!(helper.contains("INTERNAL_SERVER_ERROR"), "DB 故障 500: {helper}");
        assert!(helper.contains("当前 DMS 身份或角色不可用"), "查无此人 403: {helper}");
        // 两个 handler 都走同一分档（不只 usage_summary 一处）
        let body = src.split("pub async fn sample_questions").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("map_err(principal_err)"), "{body}");
    }

    /// 端点契约锚点：文件头写清两个端点；handler 形状不许漂（路由注册处按这个签名接线）
    #[test]
    fn endpoint_contract_is_written_in_header() {
        let src = include_str!("usage_api.rs");
        let head = src.split("\nuse ").next().unwrap();
        assert!(head.contains("GET /api/usage/summary"), "{head}");
        assert!(head.contains("GET /api/kb/sample-questions"), "{head}");
        assert!(head.contains("kb_samples:{space_id}"), "缓存键契约: {head}");
        for h in ["pub async fn usage_summary", "pub async fn sample_questions"] {
            assert!(src.contains(h), "{h}");
        }
    }
}
