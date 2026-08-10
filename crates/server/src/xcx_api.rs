//! 【小程序接入】商城小程序（uni-app）的问答与登录态探活端点。
//!
//! ## 接线契约（编排方在 main.rs 统一接线，本文件不自己挂路由）
//!
//! main.rs 里只需两行路由（模块声明已带 `#[allow(dead_code)]`，接线时一并删掉它）：
//!
//! ```ignore
//! .route("/api/xcx/ask", post(xcx_api::ask))
//! .route("/api/xcx/me", get(xcx_api::me))
//! ```
//!
//! 与 `/api/mcp` 同理：**刻意不挂会话鉴权中间件** —— 本模块自带 `x-access-token` 校验，
//! 套上 Web 会话中间件只会让小程序请求全 401。
//!
//! ## 身份模型
//!
//! 小程序用户就是 DMS 员工：小程序登录后持有商城/DMS 后端签发的 `x-access-token`，
//! 本模块**不自己验签**，而是 server-to-server 回调签发方：
//!
//! ```text
//! GET {xcx_auth_base}/login/getLoginInfo
//! x-access-token: <token>
//! → {"code":0, "data":{...}, "msg":""}     （code=30007/30012 = token 失效）
//! ```
//!
//! data 的字段名未钉死，一律防御性解析（`parse_identity`）。拿到 login 后**必须**过
//! `principal::load_principal`：员工禁用 / 多角色未选都在这里被拒 —— 数据权限与该员工
//! Web 登录完全同源，没有「小程序就是超管」的旁路（与 `mcp_api` 纪律 2 同款）。
//!
//! `xcx_auth_base` 未配置（None 或全空白）= 功能整体关闭，`/api/xcx/*` 恒 404（fail-closed，
//! 见 `db.rs` 该字段的文档）。生产值：`https://dms.huangjiaxiaohu.com/dms-api`。
//!
//! ## 响应协议（小程序拦截器契约）
//!
//! - 成功：`{"code":0, "data":..., "msg":""}`。`/ask` 的 data = `AskResult` 全字段
//!   （前端自己拆），外加一个 `conv_id` —— 客户端拿着它追问，服务端才串得起多轮。
//! - token 失效：HTTP 401 + `{"code":30007, "data":null, "msg":"token 失效"}` ——
//!   拦截器按 30007 弹登录框。token 空白、上游 401/403、上游 code 非 0 全归一到这一种
//!   （对小程序来说「该重新登录了」只有这一种安全姿态）。
//! - 校验服务本身不可用（超时/网络/上游 5xx）：HTTP 502 + `{"code":500,...}` ——
//!   重试可能好，**不该**骗用户重新登录。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};

use crate::dms_policy::principal;
use crate::{chat, AppState};
use dms_agent::triage;
use dms_semantic::registry::datasource as ds_reg;

/// 与 mcp_api 同款：(HTTP 状态码, 协议体)。HTTP 码给网关/抓包看，body.code 给小程序拦截器看。
type ApiErr = (StatusCode, Json<Value>);

/// 鉴权头（小程序与商城后端约定的登录态载体）
const TOKEN_HEADER: &str = "x-access-token";
/// 校验通道路径：拼在 `xcx_auth_base`（先剥尾斜杠）之后
const LOGIN_INFO_PATH: &str = "/login/getLoginInfo";
/// 上游校验超时：5s。登录态校验卡在问答主链路上 —— 外部抖动不能拖垮整链，
/// 超时按「暂不可用」（502）拒，让用户重试，而不是无限等。
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
/// 进程内身份缓存 TTL：60s。这是「每问一句都打一次外部校验」与「权限实时性」的取舍：
/// token 失效/切角色最多滞后 60s 生效，60s 内同一个 token 重复问不重复打外部。
const CACHE_TTL: Duration = Duration::from_secs(60);
/// 缓存容量上限：1000 条（一条不过几百字节）。满了淘汰过期点最早的那条。
/// 不设上限 = 常驻内存随登录态数量单调涨。
const CACHE_CAP: usize = 1000;

/// 小程序协议码：成功
const CODE_OK: i64 = 0;
/// 小程序协议码：token 失效（拦截器按它弹登录框）。上游 30007/30012 两个码
/// 都归一到它 —— 对小程序来说「该重新登录了」是同一件事，没必要让它认两个码。
const CODE_TOKEN_INVALID: i64 = 30007;

// ---------------------------------------------------------------- 身份解析（纯函数）

/// 校验通过后的身份。role/name 都可能缺（上游 data 字段名未钉死，取不到不硬失败）——
/// 角色缺了由 `load_principal` 的「单角色直通 / 多角色拒绝」判据兜底，fail-closed 在它那边。
#[derive(Clone, Debug, PartialEq)]
pub struct XcxIdentity {
    pub login_name: String,
    pub role_code: Option<String>,
    pub name: Option<String>,
}

/// data 里按优先级取第一个**非空白**字符串字段（`"  "` 不算填了 —— 表单常回这种）。
fn first_non_empty<'a>(data: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|k| data.get(k).and_then(Value::as_str))
        .map(str::trim)
        .find(|s| !s.is_empty())
}

/// 防御性解析 getLoginInfo 的 data：
/// - 登录名：`loginName/userName/username/employeeCode` 取第一个非空；顶层取不到再看
///   `user/employee/sysUser/userInfo` 嵌套对象的同一组键（上游各端返回结构不一）。
/// - 角色：`activeRoleCode/currentRoleCode/roleCode` 顶层键优先 → `activeRole.roleCode` →
///   `roleVOList/roleList/roles` 任一数组首元素的 `roleCode`（`roleList` 不是猜的：小程序
///   登录页 `pages/login/login.vue` 的令牌登录分支实证读 `data.roleList`）。
///   数组缺失/为空/元素缺字段都不硬失败，角色就是 None —— 单角色直通、多角色由
///   `load_principal` fail-closed（「请选择登录角色」），我们绝不替用户选。
/// - 姓名：`actualName/name/nickName/employeeName` 取第一个非空，只用于 `/me` 展示。
///
/// 返回 None = 拿不到登录名 —— 身份立不起来，调用方按 fail-closed 拒。
fn parse_identity(data: &Value) -> Option<XcxIdentity> {
    const LOGIN_KEYS: &[&str] = &["loginName", "userName", "username", "employeeCode"];
    let login = first_non_empty(data, LOGIN_KEYS).or_else(|| {
        ["user", "employee", "sysUser", "userInfo"]
            .iter()
            .filter_map(|k| data.get(k))
            .find_map(|nested| first_non_empty(nested, LOGIN_KEYS))
    })?;
    let role = first_non_empty(data, &["activeRoleCode", "currentRoleCode", "roleCode"])
        .map(str::to_string)
        .or_else(|| {
            first_non_empty(data.get("activeRole")?, &["roleCode"]).map(str::to_string)
        })
        .or_else(|| {
            ["roleVOList", "roleList", "roles"].iter().find_map(|k| {
                let first = data.get(k)?.as_array()?.first()?;
                first_non_empty(first, &["roleCode"]).map(str::to_string)
            })
        });
    let name =
        first_non_empty(data, &["actualName", "name", "nickName", "employeeName"]).map(str::to_string);
    Some(XcxIdentity {
        login_name: login.to_string(),
        role_code: role,
        name,
    })
}

/// 上游业务码 → 本端判定（纯函数，映射表有单测钉着）。
/// **白名单只认 0**：30007/30012 是上游明示的失效码，其余非 0（含上游日后加的新码）
/// 同样按失效拒 —— 「不认识的码默认不放行」是 fail-closed 的，上游改协议不会意外开口子。
fn map_upstream_code(code: i64) -> Result<(), AuthFail> {
    if code == 0 {
        Ok(())
    } else {
        Err(AuthFail::TokenInvalid)
    }
}

/// 校验失败的两种出路，响应协议在这里分叉：
/// - `TokenInvalid` → 401 + code 30007（小程序弹登录框）；
/// - `Unavailable` → 502 + code 500（上游抖动/超时/5xx，重试可能好，不该骗用户重新登录）。
#[derive(Debug, PartialEq)]
enum AuthFail {
    TokenInvalid,
    Unavailable,
}

// ---------------------------------------------------------------- 进程内缓存

struct CacheEntry {
    identity: XcxIdentity,
    expires_at: Instant,
}

/// token → (身份, 过期点)。`Mutex<HashMap>` 就够：临界区只做 map 操作，**锁不跨 await**
///（`validate_xcx_token` 里拿锁/放锁都在同步段 —— 跨了就是全进程的问答在等一把身份锁）。
/// 过期条目不主动清扫：靠「读时判过期 + 满员插入时淘汰最旧」两件事兜住增长。
struct TokenCache {
    map: HashMap<String, CacheEntry>,
}

impl TokenCache {
    /// 命中且未过期才给身份；过期按 miss（留着不删，下次 put 同 key 自然覆盖）。
    fn get(&self, token: &str, now: Instant) -> Option<XcxIdentity> {
        let e = self.map.get(token)?;
        (e.expires_at > now).then(|| e.identity.clone())
    }

    fn put(&mut self, token: String, identity: XcxIdentity, now: Instant) {
        // 满员且是新 key：淘汰过期点最早（最旧）的一条。O(n) 只在满员插入时付一次，
        // 1000 条量级无感 —— 为一个 1000 条的缓存引一套堆结构是过度设计。
        if self.map.len() >= CACHE_CAP && !self.map.contains_key(&token) {
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.expires_at)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(
            token,
            CacheEntry {
                identity,
                expires_at: now + CACHE_TTL,
            },
        );
    }
}

/// 进程级单例。token 本身当 key —— 缓存命中即视为登录态有效，这是 TTL 内的事；
/// 注意缓存**不随 `xcx_auth_base` 运行时切换失效**：base 换地址后旧身份最多再活 60s，
/// 对「换校验后端」这种运维动作这是可接受的滞后（已在文件头协议里声明 TTL 语义）。
static TOKEN_CACHE: LazyLock<Mutex<TokenCache>> =
    LazyLock::new(|| Mutex::new(TokenCache { map: HashMap::new() }));

/// 校验专用 HTTP 客户端：进程级复用（连接池），超时收在客户端上 ——
/// 每请求单建客户端会把「复用」与「超时口径」都弄丢（llm.rs 同款 builder 模式）。
static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(UPSTREAM_TIMEOUT)
        .build()
        .expect("xcx http client")
});

/// 配置快照里的校验基地址：None 或全空白 = 功能关。
/// 空白必须按没配处理 —— 否则 `" "` 能拼出 `" /login/getLoginInfo"` 这种必 404 的 URL，
/// 报错指向全错（运维会以为端点挂着、上游坏了）。
fn normalize_base(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- token 校验

/// token → 身份：缓存命中（未过期）直接返，不打外部；miss/过期才调 getLoginInfo 并回填缓存。
/// 失败出路就 `AuthFail` 两种，HTTP/协议码分叉收在 `require_identity`。
async fn validate_xcx_token(base: &str, token: &str) -> Result<XcxIdentity, AuthFail> {
    let now = Instant::now();
    if let Some(hit) = TOKEN_CACHE.lock().expect("xcx 缓存锁中毒").get(token, now) {
        return Ok(hit);
    }
    // 锁已在上一句结尾放掉 —— await 期间不持锁（TokenCache 注释那条红线）
    let id = fetch_identity(base, token).await?;
    TOKEN_CACHE
        .lock()
        .expect("xcx 缓存锁中毒")
        .put(token.to_string(), id.clone(), Instant::now());
    Ok(id)
}

/// 实际打外部那一次：GET {base}/login/getLoginInfo。
/// 任何分支都不把 token 与上游响应体写进日志（token 与 DSN 同级敏感；body 含员工信息）。
async fn fetch_identity(base: &str, token: &str) -> Result<XcxIdentity, AuthFail> {
    let url = format!("{}{}", base.trim_end_matches('/'), LOGIN_INFO_PATH);
    let resp = HTTP
        .get(&url)
        .header(TOKEN_HEADER, token)
        .send()
        .await
        .map_err(|e| {
            // reqwest 的错误文本含 URL 但不含我们设的请求头 —— 可记
            tracing::warn!(reason = %e, "小程序登录态校验请求失败（网络/超时）");
            AuthFail::Unavailable
        })?;
    let status = resp.status();
    // 上游 401/403 = 网关/后端先拒了这个 token；5xx 及其余 = 校验服务自己病了。两者出路不同。
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(AuthFail::TokenInvalid);
    }
    if !status.is_success() {
        tracing::warn!(http_status = %status.as_u16(), "小程序登录态校验上游非 2xx");
        return Err(AuthFail::Unavailable);
    }
    let body: Value = resp.json().await.map_err(|e| {
        tracing::warn!(reason = %e, "小程序登录态校验响应不是 JSON");
        AuthFail::Unavailable
    })?;
    // code 缺失/非数字 = 协议不认识：不能默认成 0（那是放行），按失效拒（fail-closed）。
    match body.get("code").and_then(Value::as_i64) {
        Some(code) => map_upstream_code(code).map_err(|f| {
            tracing::warn!(upstream_code = code, "小程序 token 被上游判定失效/拒绝");
            f
        })?,
        None => {
            tracing::warn!("小程序登录态校验响应缺 code 字段");
            return Err(AuthFail::TokenInvalid);
        }
    }
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    parse_identity(&data).ok_or_else(|| {
        // code=0 却取不到登录名：上游 shape 变了。重新登录修不好它 —— 按不可用报，
        // 让运维从日志看到，而不是让小程序用户陷入「弹登录 → 登录 → 再弹」的死循环。
        tracing::warn!("小程序登录态校验 code=0 但 data 取不到登录名（上游字段 shape 变了？）");
        AuthFail::Unavailable
    })
}

// ---------------------------------------------------------------- 协议出口

fn ok(data: Value) -> Json<Value> {
    Json(json!({ "code": CODE_OK, "data": data, "msg": "" }))
}

/// 业务失败统一形：`data` 恒 null，客户端只看 code/msg。
fn fail(http: StatusCode, code: i64, msg: &str) -> ApiErr {
    (http, Json(json!({ "code": code, "data": Value::Null, "msg": msg })))
}

/// token 失效的统一应答（拦截器认 30007 弹登录框）。文案与码是外部契约，有单测钉着。
fn invalid_token_err() -> ApiErr {
    fail(StatusCode::UNAUTHORIZED, CODE_TOKEN_INVALID, "token 失效")
}

/// 两个端点共用的前段：功能开关 → 取 token → 校验（含缓存）。
/// 404（未配置）/ 401（token 空白或失效）/ 502（校验服务不可用）全部收在这里。
async fn require_identity(st: &AppState, headers: &HeaderMap) -> Result<XcxIdentity, ApiErr> {
    let base = normalize_base(st.cfg().xcx_auth_base.clone())
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, 404, "小程序接入未启用"))?;
    let token = headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or_default();
    if token.is_empty() {
        return Err(invalid_token_err());
    }
    validate_xcx_token(&base, token).await.map_err(|f| match f {
        AuthFail::TokenInvalid => invalid_token_err(),
        AuthFail::Unavailable => {
            fail(StatusCode::BAD_GATEWAY, 500, "登录态校验暂不可用，请稍后重试")
        }
    })
}

// ---------------------------------------------------------------- 端点

#[derive(serde::Deserialize)]
pub struct XcxAskReq {
    question: String,
    /// 归属会话；缺省 = 服务端新开会话（响应 `data.conv_id` 回传，客户端带着它追问）
    conv_id: Option<i64>,
    /// 客户端自管的上一轮（不依赖服务端会话历史时显式带上）；给了就**优先**于会话历史。
    /// 两个字段名与 CLI/判官题集的 `prev`/`prev_sql` 同语义：问句 + 那一轮执行的 SQL。
    prev_question: Option<String>,
    prev_sql: Option<String>,
}

/// `POST /api/xcx/ask`：与 `/api/ask` 完全同一条问答管道（分诊 → `crate::ask` /
/// `kb_answer`），只是身份来源从 Bearer 会话换成 `x-access-token`，响应套小程序协议。
pub async fn ask(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<XcxAskReq>,
) -> Result<Json<Value>, ApiErr> {
    let id = require_identity(&st, &headers).await?;
    let question = req.question.trim();
    if question.is_empty() {
        return Err(fail(StatusCode::BAD_REQUEST, 400, "question 不能为空"));
    }
    // 身份 → Principal：员工禁用 / 多角色未选在这里被拒（与 Web 登录同一判据，零旁路）
    let p = principal::load_principal(&st.auth_mysql, &id.login_name, id.role_code.as_deref())
        .await
        .map_err(|e| {
            // 客户端文案保持笼统（不泄权限结构），但服务端必须留可排查的真因 ——
            // 「admin 角色无 administrator_flag 被拦」与「员工不存在」运维上是两条完全不同的路。
            tracing::warn!(login = %id.login_name, role = ?id.role_code, reason = %e,
                "小程序身份核验被 load_principal 拒");
            fail(StatusCode::FORBIDDEN, 403, "当前账号或角色不可用")
        })?;
    // 会话：带了就校验属主（同 api_ask 语义：非属主 403，不泄存在性之外的越权面）；
    // 没带就开新会话 —— 小程序客户端不必先调「新建会话」接口，一问即开。
    let conv_id = match req.conv_id {
        Some(cid) => {
            match chat::conv_owner(st.owned.pool(), cid).await {
                Ok(Some(owner)) if owner == id.login_name => {}
                Ok(_) => return Err(fail(StatusCode::FORBIDDEN, 403, "无权访问该会话")),
                Err(_) => {
                    return Err(fail(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        500,
                        "会话状态读取失败，请稍后重试",
                    ))
                }
            }
            cid
        }
        None => chat::new_conv(st.owned.pool(), &id.login_name)
            .await
            .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, 500, "新建会话失败，请稍后重试"))?,
    };
    // 上一轮上下文：显式 prev_question 优先（客户端自管形态）；否则续会话时取服务端历史
    //（同 api_ask：取不到按首问处理，不失败，但 warn 留痕 —— 静默丢上下文查不出来）。
    let prev_q = req.prev_question.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let prev: Option<(String, Option<String>)> = match prev_q {
        Some(q) => Some((q.to_string(), req.prev_sql.clone())),
        None if req.conv_id.is_some() => chat::last_turn(st.owned.pool(), conv_id)
            .await
            .inspect_err(|_| {
                tracing::warn!(conv_id, reason = "chat_context_load_failed", "取上一轮失败，本轮按首问处理")
            })
            .ok()
            .flatten(),
        None => None, // 刚开的空会话没有上一轮，不空查一库
    };
    let prev_turn = prev.as_ref().map(|(q, s)| (q.as_str(), s.as_deref(), &[][..]));
    // 分诊（同 api_ask：无强制 chip，主源元数据判 Data/Knowledge；分诊内部不失败）
    let intent = triage::triage(&st.llm, st.owned.pool(), ds_reg::DMS_DS_ID, question, None).await;
    let conv_id_str = conv_id.to_string();
    let payload = match intent {
        triage::Intent::Data => {
            let (r, _log) = crate::ask(
                &st.llm,
                &st.auth_mysql,
                &st.mysql,
                &st.sources,
                st.owned.pool(),
                &st.embed,
                &p,
                question,
                prev_turn,
                None, // ds：小程序侧不提供选源，后端选源（可见源只有一个时直通主源）
                Some(&conv_id_str),
                st.sc_samples,
            )
            .await;
            // 观测写入句柄丢弃（fire-and-forget，同 api_ask / mcp_api）
            let r = r.map_err(|e| {
                // 「无权访问数据源」是权限拒绝 → 403，同 api_ask 的语义（见那里的注释）
                if e.to_string().contains("无权访问数据源") {
                    fail(StatusCode::FORBIDDEN, 403, "当前账号无权访问该数据源")
                } else {
                    fail(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        422,
                        "暂时无法完成本次问数，请调整问题后重试",
                    )
                }
            })?;
            serde_json::to_value(&r).unwrap_or_else(|_| json!({}))
        }
        triage::Intent::Knowledge => {
            let a = crate::kb_answer(&st, &p, None, question).await.map_err(|_| {
                fail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    422,
                    "暂时无法完成知识检索，请稍后重试",
                )
            })?;
            serde_json::to_value(&a).unwrap_or_else(|_| json!({}))
        }
    };
    // 存会话（用户问 + AI 结果），同 api_ask：写库失败只丢历史，不拦响应
    let _ = chat::save_msg(st.owned.pool(), conv_id, "user", question, None).await;
    let _ = chat::save_msg(st.owned.pool(), conv_id, "ai", "", Some(&payload)).await;
    // data = AskResult/Answer 全字段（前端自己拆）+ conv_id：客户端拿着它追问才能串起多轮。
    // AskResult 自身没有 conv_id 字段，注入不撞键（有撞键那天这个 insert 会静默盖掉它 ——
    // 所以这里断言式说明：注入键是协议增量，不是覆盖）。
    let mut data = payload;
    if let Some(m) = data.as_object_mut() {
        m.insert("conv_id".to_string(), json!(conv_id));
    }
    Ok(ok(data))
}

/// `GET /api/xcx/me`：进 AI 页时的登录态探活。只验 token 不落 Principal ——
/// 探活回答的是「登录态还活着吗」，不是「权限算得出来吗」；后者是 `/ask` 自己的事，
/// 在这里多查一次身份库只会拖慢每个进页请求。
pub async fn me(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiErr> {
    let id = require_identity(&st, &headers).await?;
    Ok(ok(json!({
        "login_name": id.login_name,
        "role_code": id.role_code,
        "name": id.name,
    })))
}

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn id(login: &str) -> XcxIdentity {
        XcxIdentity {
            login_name: login.to_string(),
            role_code: Some("admin".to_string()),
            name: None,
        }
    }

    // ---------- parse_identity：上游 data 字段名未钉死，各形态都要认 ----------

    #[test]
    fn parse_login_name_priority_and_fallbacks() {
        // 四个候选键各自单独出现都要能解出
        for (k, v) in [
            ("loginName", "zhangsan"),
            ("userName", "lisi"),
            ("username", "wangwu"),
            ("employeeCode", "E0042"),
        ] {
            let d = json!({ k: v });
            assert_eq!(parse_identity(&d).unwrap().login_name, v, "键 {k} 解不出");
        }
        // 同时出现时按优先级取第一个**非空**（loginName 空白要落给 userName）
        let d = json!({ "loginName": "  ", "userName": "lisi", "username": "wangwu" });
        assert_eq!(parse_identity(&d).unwrap().login_name, "lisi");
        // 一个能登录的键都没有 → None（fail-closed，调用方按拒处理）
        assert_eq!(parse_identity(&json!({ "activeRoleCode": "admin" })), None);
        assert_eq!(parse_identity(&Value::Null), None, "data=null 不许 panic、不许放行");
        assert_eq!(parse_identity(&json!({ "loginName": "   " })), None, "空白不算登录名");
    }

    #[test]
    fn parse_role_active_first_then_role_vo_list() {
        // activeRoleCode 优先
        let d = json!({ "loginName": "a", "activeRoleCode": "city_manager",
                        "roleVOList": [{ "roleCode": "admin" }] });
        assert_eq!(parse_identity(&d).unwrap().role_code.as_deref(), Some("city_manager"));
        // 缺省回退 roleVOList[0].roleCode
        let d = json!({ "loginName": "a", "roleVOList": [{ "roleCode": "staff" }, { "roleCode": "admin" }] });
        assert_eq!(parse_identity(&d).unwrap().role_code.as_deref(), Some("staff"));
        // 数组空 / 缺 / 元素缺字段 / 值空白 → None（不硬失败）
        for d in [
            json!({ "loginName": "a", "roleVOList": [] }),
            json!({ "loginName": "a" }),
            json!({ "loginName": "a", "roleVOList": [{}] }),
            json!({ "loginName": "a", "activeRoleCode": " ", "roleVOList": [{ "roleCode": "" }] }),
        ] {
            assert_eq!(parse_identity(&d).unwrap().role_code, None, "{d}");
        }
        // 姓名键各形态（只喂 /me 展示，取不到是 None 不失败）
        let d = json!({ "loginName": "a", "actualName": "张三" });
        assert_eq!(parse_identity(&d).unwrap().name.as_deref(), Some("张三"));
    }

    /// 上游实证的另外两个形态：roleList 数组名（小程序登录页就读它）、user 嵌套对象
    #[test]
    fn parse_role_list_key_and_nested_user_object() {
        let d = json!({ "loginName": "a", "roleList": [{ "roleCode": "staff" }] });
        assert_eq!(parse_identity(&d).unwrap().role_code.as_deref(), Some("staff"));
        let d = json!({ "loginName": "a", "roles": [{ "roleCode": "city_manager" }] });
        assert_eq!(parse_identity(&d).unwrap().role_code.as_deref(), Some("city_manager"));
        let d = json!({ "loginName": "a", "activeRole": { "roleCode": "admin" } });
        assert_eq!(parse_identity(&d).unwrap().role_code.as_deref(), Some("admin"));
        // 嵌套 user：顶层没有登录名时往里看
        let d = json!({ "user": { "loginName": "yunfan", "actualName": "云帆" }, "roleList": [{ "roleCode": "admin" }] });
        let id = parse_identity(&d).unwrap();
        assert_eq!((id.login_name.as_str(), id.role_code.as_deref()), ("yunfan", Some("admin")));
        // 顶层优先于嵌套
        let d = json!({ "loginName": "top", "user": { "loginName": "nested" } });
        assert_eq!(parse_identity(&d).unwrap().login_name, "top");
        assert_eq!(parse_identity(&json!({ "loginName": "a" })).unwrap().name, None);
    }

    // ---------- 上游 code 映射：白名单只认 0 ----------

    #[test]
    fn upstream_code_only_zero_passes() {
        assert_eq!(map_upstream_code(0), Ok(()));
        // 30007/30012 是上游明示的失效码；其余非 0（含负数、500、不认识的新码）同样拒
        for c in [30007, 30012, 1, -1, 500, 401] {
            assert_eq!(map_upstream_code(c), Err(AuthFail::TokenInvalid), "code {c} 必须拒");
        }
    }

    // ---------- 缓存：命中 / 过期 / 满员淘汰最旧 ----------

    #[test]
    fn cache_hit_miss_and_expiry() {
        let mut c = TokenCache { map: HashMap::new() };
        let t0 = Instant::now();
        c.put("tok1".to_string(), id("zhangsan"), t0);
        // TTL 内命中
        assert_eq!(c.get("tok1", t0 + Duration::from_secs(59)), Some(id("zhangsan")));
        // 恰好到点即过期（边界用 > 判定，等于 expires_at 的那一拍按 miss）
        assert_eq!(c.get("tok1", t0 + CACHE_TTL), None);
        assert_eq!(c.get("tok1", t0 + Duration::from_secs(61)), None);
        // 没见过的 key
        assert_eq!(c.get("nope", t0), None);
        // 过期后重放同 key：新身份覆盖旧条目
        c.put("tok1".to_string(), id("lisi"), t0 + Duration::from_secs(120));
        assert_eq!(c.get("tok1", t0 + Duration::from_secs(121)), Some(id("lisi")));
    }

    #[test]
    fn cache_evicts_oldest_when_full() {
        let mut c = TokenCache { map: HashMap::new() };
        let t0 = Instant::now();
        // 逐秒插入 CAP 条：tok0 最旧（过期点最早）
        for i in 0..CACHE_CAP {
            c.put(format!("tok{i}"), id("u"), t0 + Duration::from_secs(i as u64));
        }
        assert_eq!(c.map.len(), CACHE_CAP);
        // 满员插新 key → 最旧的 tok0 被淘汰，其余还在
        c.put("tok-new".to_string(), id("v"), t0 + Duration::from_secs(CACHE_CAP as u64));
        assert_eq!(c.map.len(), CACHE_CAP, "淘汰后容量不许涨");
        assert_eq!(c.get("tok0", t0), None, "最旧的没被淘汰");
        assert!(c.get("tok1", t0 + Duration::from_secs(2)).is_some(), "次旧的不该被误伤");
        assert!(c.get("tok-new", t0 + Duration::from_secs(CACHE_CAP as u64)).is_some());
        // 满员时**更新已存在的 key** 不触发淘汰（容量不变、内容刷新）
        c.put("tok1".to_string(), id("u2"), t0 + Duration::from_secs(CACHE_CAP as u64));
        assert_eq!(c.map.len(), CACHE_CAP);
        assert_eq!(
            c.get("tok1", t0 + Duration::from_secs(CACHE_CAP as u64)),
            Some(id("u2"))
        );
    }

    // ---------- 协议出口形状：外部契约钉死 ----------

    #[test]
    fn protocol_shapes() {
        let Json(o) = ok(json!({ "x": 1 }));
        assert_eq!(o, json!({ "code": 0, "data": { "x": 1 }, "msg": "" }));
        let (st, Json(e)) = invalid_token_err();
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(e, json!({ "code": 30007, "data": null, "msg": "token 失效" }));
    }

    #[test]
    fn normalize_base_blank_means_disabled() {
        assert_eq!(normalize_base(None), None);
        assert_eq!(normalize_base(Some("   ".to_string())), None, "全空白 = 没配");
        assert_eq!(
            normalize_base(Some(" https://a.b/c/ ".to_string())),
            Some("https://a.b/c/".to_string())
        );
    }

    /// 常量即运维口径：有人改数值时这条测试会红，逼他读常量上的注释再想一遍
    #[test]
    fn tuning_constants_are_pinned() {
        assert_eq!(UPSTREAM_TIMEOUT, Duration::from_secs(5));
        assert_eq!(CACHE_TTL, Duration::from_secs(60));
        assert_eq!(CACHE_CAP, 1000);
    }

    // ---------- fetch_identity：本地 HTTP 桩，真打一遍 reqwest ----------

    /// 一次性/计数 HTTP 桩：固定回 (status, body)，把收到的请求行+头经 channel 吐出，
    /// 命中次数经 Arc 计数。用 std::net 手写而不引 mock 库：本仓 dev-deps 为零，不为一个桩破例。
    fn stub_server(
        status: &'static str,
        body: &'static str,
    ) -> (String, Arc<AtomicUsize>, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = listener.accept() {
                hits2.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (addr, hits, rx)
    }

    #[tokio::test]
    async fn fetch_sends_token_header_and_parses_identity() {
        let (base, hits, rx) = stub_server(
            "200 OK",
            r#"{"code":0,"data":{"userName":"zhangsan","roleVOList":[{"roleCode":"staff"}],"actualName":"张三"},"msg":""}"#,
        );
        let got = fetch_identity(&base, "tok-abc").await.unwrap();
        assert_eq!(got.login_name, "zhangsan");
        assert_eq!(got.role_code.as_deref(), Some("staff"));
        assert_eq!(got.name.as_deref(), Some("张三"));
        // 请求真的带了 x-access-token 头，且打的是 /login/getLoginInfo
        let req = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(req.starts_with("GET /login/getLoginInfo "), "{req}");
        assert!(req.to_ascii_lowercase().contains("x-access-token: tok-abc"), "{req}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fetch_maps_upstream_rejections() {
        // 上游业务码 30007/30012/其它非 0 → TokenInvalid
        for body in [
            r#"{"code":30007,"data":null,"msg":"token失效"}"#,
            r#"{"code":30012,"data":null,"msg":"登录过期"}"#,
            r#"{"code":1,"data":null,"msg":"系统错误"}"#,
            r#"{"data":{"loginName":"a"}}"#, // 缺 code：协议不认识，fail-closed
        ] {
            let (base, _, _) = stub_server("200 OK", body);
            assert_eq!(
                fetch_identity(&base, "tok-x").await,
                Err(AuthFail::TokenInvalid),
                "{body}"
            );
        }
        // HTTP 401/403 → TokenInvalid；500 → Unavailable
        let (base, _, _) = stub_server("401 Unauthorized", r#"{"code":401,"msg":"unauthorized"}"#);
        assert_eq!(fetch_identity(&base, "tok-x").await, Err(AuthFail::TokenInvalid));
        let (base, _, _) = stub_server("500 Internal Server Error", r#"{"code":500}"#);
        assert_eq!(fetch_identity(&base, "tok-x").await, Err(AuthFail::Unavailable));
        // code=0 但取不到登录名 → Unavailable（上游 shape 变了，重登录修不好）
        let (base, _, _) = stub_server("200 OK", r#"{"code":0,"data":{"foo":"bar"},"msg":""}"#);
        assert_eq!(fetch_identity(&base, "tok-x").await, Err(AuthFail::Unavailable));
        // 200 但 body 不是 JSON → Unavailable
        let (base, _, _) = stub_server("200 OK", "not-json");
        assert_eq!(fetch_identity(&base, "tok-x").await, Err(AuthFail::Unavailable));
    }

    // ---------- 端到端（进程内）：缓存命中不重复打外部 ----------

    /// 全局 TOKEN_CACHE 是进程级单例：测试间用 uuid token 隔离，互不污染（也不依赖执行序）。
    #[tokio::test]
    async fn validate_caches_within_ttl() {
        let (base, hits, _rx) = stub_server(
            "200 OK",
            r#"{"code":0,"data":{"loginName":"cacheuser","activeRoleCode":"admin"},"msg":""}"#,
        );
        let tok = format!("tok-cache-{}", uuid::Uuid::new_v4());
        let a = validate_xcx_token(&base, &tok).await.unwrap();
        let b = validate_xcx_token(&base, &tok).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a.login_name, "cacheuser");
        // 第二次是缓存命中：外部只被打了一次 —— 这是缓存存在的全部理由，必须钉住
        assert_eq!(hits.load(Ordering::SeqCst), 1, "TTL 内重复校验不许重复打外部");
    }

    #[tokio::test]
    async fn validate_propagates_token_invalid() {
        let (base, _, _) = stub_server("200 OK", r#"{"code":30007,"data":null,"msg":"x"}"#);
        let tok = format!("tok-bad-{}", uuid::Uuid::new_v4());
        assert_eq!(validate_xcx_token(&base, &tok).await, Err(AuthFail::TokenInvalid));
    }
}
