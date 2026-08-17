//! 【小程序接入】商城小程序（uni-app）的问答与登录态探活端点。
//!
//! ## 接线契约（编排方在 main.rs 统一接线，本文件不自己挂路由）
//!
//! main.rs 里只需两行路由（**已在 main.rs 接线**：`mod xcx_api;` + 下面两条路由）：
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
//!   （对小程序来说「该重新登录了」只有这一种安全姿态）。上游明确判失效的 token 进
//!   60s 负缓存：坏 token 重放打在缓存上，不逐次穿透上游。
//! - 限流：HTTP 429 + `{"code":429,...}` —— 同一 IP 每分钟 20 次（per-IP 固定窗口，
//!   `auth::ip_rate_allow`），ask/me 共用前段一道闸。
//! - 入参限长：question / prev_question ≤ 500 字、prev_sql ≤ 2000 字，超出 400
//!   （与 web 端输入框 maxlength 同口径；超限拒收，不静默截断）。
//! - 校验服务本身不可用（超时/网络/上游 5xx）：HTTP 502 + `{"code":500,...}` ——
//!   重试可能好，**不该**骗用户重新登录。瞬时故障不进缓存（下一次重试必须真能打到上游）。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};

use crate::dms_policy::principal;
use crate::{chat, AppState};
use dms_agent::intent::IntentRoute;

/// 与 mcp_api 同款：(HTTP 状态码, 协议体)。HTTP 码给网关/抓包看，body.code 给小程序拦截器看。
type ApiErr = (StatusCode, Json<Value>);

/// 鉴权头（小程序与商城后端约定的登录态载体）：单一事实源在 `auth::UPSTREAM_TOKEN_HEADER`
///（SSO 验真同一条头，两处各写一份字面量必漂）
use crate::auth::UPSTREAM_TOKEN_HEADER as TOKEN_HEADER;
/// 校验通道路径：拼在 `xcx_auth_base`（先剥尾斜杠）之后
const LOGIN_INFO_PATH: &str = "/login/getLoginInfo";
/// 上游校验超时：5s。登录态校验卡在问答主链路上 —— 外部抖动不能拖垮整链，
/// 超时按「暂不可用」（502）拒，让用户重试，而不是无限等。
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
/// 上游响应体上限：上游被攻破/配错回巨型 body 时，解析阶段不许吃无界内存
const MAX_IDENTITY_BODY_BYTES: usize = 256 * 1024;

/// 进程内身份缓存 TTL：60s。这是「每问一句都打一次外部校验」与「权限实时性」的取舍：
/// token 失效/切角色最多滞后 60s 生效，60s 内同一个 token 重复问不重复打外部。
/// 同 TTL 兼作**负缓存**：上游明确判失效的 token 也记 60s，坏 token 重放打在缓存上，
/// 不再逐次穿透上游（拿我们当对上游的放大器）。
const CACHE_TTL: Duration = Duration::from_secs(60);
/// 缓存容量上限：1000 条（一条不过几百字节）。满了淘汰过期点最早的那条。
/// 不设上限 = 常驻内存随登录态数量单调涨。
const CACHE_CAP: usize = 1000;

/// 入参限长（与 web 端口径对齐：web 输入框 maxlength=500；上一轮 SQL 给到 2000）。
/// 无上限字符串是慢查询 / 会话存储 / LLM prompt 三处共同的成本放大面 —— 超限 400 拒收
/// 而不是截断（静默截断会把「问题没问全」藏成「答非所问」）。
const MAX_QUESTION_CHARS: usize = 500;
const MAX_PREV_SQL_CHARS: usize = 2000;

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
/// - 登录名：`loginName/userName/username/employeeCode` 取第一个非空。
/// - 角色/姓名/登录名同一条「顶层 → 嵌套」回退链：顶层取不到再看
///   `user/employee/sysUser/userInfo` 嵌套对象（上游各端返回结构不一）。只查顶层会在
///   上游把完整身份塞进 `user` 时丢 role_code —— 多角色账号随后被 load_principal 误拒 403。
/// - 角色链：`activeRoleCode/currentRoleCode/roleCode` 键优先 → `activeRole.roleCode` →
///   `roleVOList/roleList/roles` 任一数组首元素的 `roleCode`（`roleList` 不是猜的：小程序
///   登录页 `pages/login/login.vue` 的令牌登录分支实证读 `data.roleList`）。
///   数组缺失/为空/元素缺字段都不硬失败，角色就是 None —— 单角色直通、多角色由
///   `load_principal` fail-closed（「请选择登录角色」），我们绝不替用户选。
/// - 姓名：`actualName/name/nickName/employeeName` 取第一个非空，只用于 `/me` 展示。
///
/// 返回 None = 拿不到登录名 —— 身份立不起来，调用方按 fail-closed 拒。
fn parse_identity(data: &Value) -> Option<XcxIdentity> {
    const LOGIN_KEYS: &[&str] = &["loginName", "userName", "username", "employeeCode"];
    const NESTED_KEYS: &[&str] = &["user", "employee", "sysUser", "userInfo"];
    /// 角色链（在单个对象层内求值）
    fn role_of(data: &Value) -> Option<String> {
        first_non_empty(data, &["activeRoleCode", "currentRoleCode", "roleCode"])
            .map(str::to_string)
            .or_else(|| {
                first_non_empty(data.get("activeRole")?, &["roleCode"]).map(str::to_string)
            })
            .or_else(|| {
                ["roleVOList", "roleList", "roles"].iter().find_map(|k| {
                    let first = data.get(k)?.as_array()?.first()?;
                    first_non_empty(first, &["roleCode"]).map(str::to_string)
                })
            })
    }
    fn name_of(data: &Value) -> Option<String> {
        first_non_empty(data, &["actualName", "name", "nickName", "employeeName"]).map(str::to_string)
    }
    // 顶层优先，逐个嵌套对象回退（同一组键、同一条角色链）
    let scopes: Vec<&Value> = std::iter::once(data)
        .chain(NESTED_KEYS.iter().filter_map(|k| data.get(k)))
        .collect();
    let login = scopes.iter().find_map(|s| first_non_empty(s, LOGIN_KEYS))?;
    let role = scopes.iter().find_map(|s| role_of(s));
    let name = scopes.iter().find_map(|s| name_of(s));
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

/// 缓存判定：有效身份 / 上游明确判失效（负缓存）。
/// 瞬时故障（Unavailable）**绝不进缓存** —— 把「上游抖了一下」存成「这 token 不行」会误伤。
#[derive(Clone, Debug, PartialEq)]
enum CacheVerdict {
    Valid(XcxIdentity),
    /// 上游明确判失效（401/403 或业务码非 0）：60s 负缓存，重放不再穿透上游
    Invalid,
}

struct CacheEntry {
    verdict: CacheVerdict,
    expires_at: Instant,
}

/// token → (判定, 过期点)。`Mutex<HashMap>` 就够：临界区只做 map 操作，**锁不跨 await**
///（`validate_xcx_token` 里拿锁/放锁都在同步段 —— 跨了就是全进程的问答在等一把身份锁）。
/// 过期条目不主动清扫：靠「读时判过期 + 满员插入时淘汰最旧」两件事兜住增长。
struct TokenCache {
    map: HashMap<String, CacheEntry>,
}

impl TokenCache {
    /// 命中且未过期才给判定；过期按 miss（留着不删，下次 put 同 key 自然覆盖）。
    fn get(&self, token: &str, now: Instant) -> Option<CacheVerdict> {
        let e = self.map.get(token)?;
        (e.expires_at > now).then(|| e.verdict.clone())
    }

    fn put(&mut self, token: String, verdict: CacheVerdict, now: Instant) {
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
                verdict,
                expires_at: now + CACHE_TTL,
            },
        );
    }
}

/// 进程级单例。token 本身当 key —— 命中 Valid 即视为登录态有效（TTL 内的事）；
/// 命中 Invalid 即视为已失效（负缓存）。注意缓存**不随 `xcx_auth_base` 运行时切换失效**：
/// base 换地址后旧判定最多再活 60s，对「换校验后端」这种运维动作这是可接受的滞后
///（已在文件头协议里声明 TTL 语义）。
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
/// 上游**明确判失效**的 token 同样回填（负缓存）：同一个坏 token 的重复重放打在缓存上，
/// 不再逐次穿透上游。瞬时故障（Unavailable）不缓存 —— 下一次重试必须真能打到上游。
/// 失败出路就 `AuthFail` 两种，HTTP/协议码分叉收在 `require_identity`。
///
/// ⚠️ 无 single-flight：冷启动/缓存集中过期时，N 个并发同 token 请求会各打一次上游。
/// 这是接受的取舍 —— 单 token 的并发天然受该用户的操作频率限制，上游又是内网服务；
/// 引 in-flight 去重 map 会让「锁不跨 await」那条红线（见 TokenCache 注释）变复杂，收益不成比例。
async fn validate_xcx_token(base: &str, token: &str) -> Result<XcxIdentity, AuthFail> {
    let now = Instant::now();
    // 锁中毒自愈（unwrap_or_else(into_inner)）：一次 put 期间 panic 不许让此后所有 xcx 请求 500
    if let Some(hit) = TOKEN_CACHE.lock().unwrap_or_else(PoisonError::into_inner).get(token, now) {
        return match hit {
            CacheVerdict::Valid(id) => Ok(id),
            // 负缓存命中：与上游亲判同形（调用方无感），只是不再打外部
            CacheVerdict::Invalid => Err(AuthFail::TokenInvalid),
        };
    }
    // 锁已在上一句结尾放掉 —— await 期间不持锁（TokenCache 注释那条红线）
    match fetch_identity(base, token).await {
        Ok(id) => {
            TOKEN_CACHE
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .put(token.to_string(), CacheVerdict::Valid(id.clone()), Instant::now());
            Ok(id)
        }
        Err(AuthFail::TokenInvalid) => {
            TOKEN_CACHE
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .put(token.to_string(), CacheVerdict::Invalid, Instant::now());
            Err(AuthFail::TokenInvalid)
        }
        Err(AuthFail::Unavailable) => Err(AuthFail::Unavailable),
    }
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
    // Content-Length 预检 + 分块限长读取（chunked 无长度声明也兜得住）
    if resp.content_length().is_some_and(|n| n > MAX_IDENTITY_BODY_BYTES as u64) {
        tracing::warn!(len = ?resp.content_length(), "小程序登录态校验响应体超上限");
        return Err(AuthFail::Unavailable);
    }
    let mut resp = resp;
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| {
        tracing::warn!(reason = %e, "小程序登录态校验响应读取失败");
        AuthFail::Unavailable
    })? {
        if buf.len() + chunk.len() > MAX_IDENTITY_BODY_BYTES {
            tracing::warn!(size = buf.len() + chunk.len(), "小程序登录态校验响应体超上限");
            return Err(AuthFail::Unavailable);
        }
        buf.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&buf).map_err(|e| {
        tracing::warn!(reason = %e, "小程序登录态校验响应不是 JSON");
        AuthFail::Unavailable
    })?;
    // code 缺失/非数字 = 协议不认识：不能默认成 0（那是放行），按失效拒（fail-closed）。
    match body.get("code").and_then(Value::as_i64) {
        Some(code) => map_upstream_code(code).map_err(|f| {
            // 30007/30012 是 token 生命周期的日常事件（用户重新登录即好），每 token 每 60s
            // 一条 warn 是噪音；未知码保留 warn —— 那才说明上游协议变了
            match code {
                30007 | 30012 => tracing::debug!(upstream_code = code, "小程序 token 失效（日常码）"),
                _ => tracing::warn!(upstream_code = code, "小程序 token 被上游判定失效/拒绝（非常见码）"),
            }
            f
        })?,
        None => {
            tracing::warn!("小程序登录态校验响应缺 code 字段");
            return Err(AuthFail::TokenInvalid);
        }
    }
    // data 子树不克隆（员工信息 payload 可能不小），parse_identity 只要引用
    parse_identity(body.get("data").unwrap_or(&Value::Null)).ok_or_else(|| {
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

/// ⑤ 入参限长判据（纯函数，handler 只负责把 Err 映射成 400）：
/// question / prev_question ≤ 500 字、prev_sql ≤ 2000 字（按字符计，不按字节切 CJK）。
/// 文案拼常量：改常量不改文案即失真（钉值测试只钉常量，不钉文案里的数字）。
fn lengths_ok(question: &str, prev_question: Option<&str>, prev_sql: Option<&str>) -> Result<(), String> {
    if question.chars().count() > MAX_QUESTION_CHARS {
        return Err(format!("question 超长（最多 {MAX_QUESTION_CHARS} 字）"));
    }
    if prev_question.is_some_and(|q| q.chars().count() > MAX_QUESTION_CHARS) {
        return Err(format!("prev_question 超长（最多 {MAX_QUESTION_CHARS} 字）"));
    }
    if prev_sql.is_some_and(|s| s.chars().count() > MAX_PREV_SQL_CHARS) {
        return Err(format!("prev_sql 超长（最多 {MAX_PREV_SQL_CHARS} 字）"));
    }
    Ok(())
}

/// 两个端点共用的前段：per-IP 限流 → 功能开关 → 取 token → 校验（含缓存）。
/// 429（限流）/ 404（未配置）/ 401（token 空白或失效）/ 502（校验服务不可用）全部收在这里。
async fn require_identity(st: &AppState, headers: &HeaderMap) -> Result<XcxIdentity, ApiErr> {
    // ① per-IP 限流：这两个端点原本零限流 —— 坏 token 每次实打上游（穿透放大），
    // 好 token 也能拿来刷问答预算。同一 IP 20 次/分钟，超出 429（与 login/sso 同模子，
    // 契约见 `auth::ip_rate_allow` 文档）。放在最前：这是最便宜的一道闸。
    if !crate::auth::ip_rate_allow(&crate::auth::client_ip(headers)) {
        return Err(fail(
            StatusCode::TOO_MANY_REQUESTS,
            429,
            "请求过于频繁，请稍后重试",
        ));
    }
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
    // 长度闸（与 SSO 同一条 MAX_UPSTREAM_TOKEN_LEN）：超长按 token 失效拒 ——
    // 它随后是缓存 key：无上限的 key 拖慢哈希，还把 1000 条 CAP 撑成 ~8MB/条 量级
    if token.len() > crate::auth::MAX_UPSTREAM_TOKEN_LEN {
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

/// `POST /api/xcx/ask`：与 `/api/ask` 完全同一条问答管道（统一意图准备 → Data/Knowledge /
/// `kb_answer`），只是身份来源从 Bearer 会话换成 `x-access-token`，响应套小程序协议。
pub async fn ask(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<XcxAskReq>,
) -> Result<Json<Value>, ApiErr> {
    let gate = ask_gate(&st, &headers, &req).await?;
    let prev = gate
        .prev
        .as_ref()
        .map(|(q, s)| (q.as_str(), s.as_deref(), &[][..], &[][..]));
    let prepared = crate::prepare_ask(&st, &gate.question, prev, None).await;
    // 🔴 Data / Knowledge / Unknown **同一个出口**（2026-08-16）。
    // 此前这里是四臂 `match`，`Knowledge` 直连 `kb_answer` —— 与 `/api/ask` 2026-08-14
    // 修掉的那个缺陷一字不差：「线下-浏阳品元商贸有限公司」在 web 上修好了，
    // 在小程序上照旧只答「知识库里没有这家公司的规定」，而这家公司在业务库里有客户卡。
    // 同一个缺陷在不同入口上各活各的，正是本仓反复付账的那个形状。
    // 现在只剩 Hybrid 一档单列 —— 它的 wire 形状（两路并排 + AI 综合）确实不同。
    let payload = match prepared.question.route() {
        IntentRoute::Hybrid => xcx_hybrid_payload(&st, &gate, &prepared).await?,
        _ => ask_data_payload(&st, &gate, &prepared).await?,
    };
    Ok(ask_finish(&st, &gate, payload).await)
}

/// `POST /api/xcx/ask/stream` —— `/api/xcx/ask` 的流式变体（事件协议见 `kb_api` 的
/// 「SSE 流式问答」段头注）。鉴权沿用 `x-access-token` 同一套（`require_identity` 一道闸不少）。
/// 分诊落 **Data**：回普通 JSON（`Content-Type: application/json`，协议与 `/api/xcx/ask`
/// 逐字相同，客户端按 content-type 分派）；落 **Knowledge**：回 `text/event-stream`
/// （meta 带 `conv_id` → delta×N → done/error）。小程序端 `wx.request enableChunked` 消费。
pub async fn ask_stream(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<XcxAskReq>,
) -> Result<axum::response::Response, ApiErr> {
    use axum::response::IntoResponse;
    let gate = ask_gate(&st, &headers, &req).await?;
    let prev = gate
        .prev
        .as_ref()
        .map(|(q, s)| (q.as_str(), s.as_deref(), &[][..], &[][..]));
    let prepared = crate::prepare_ask(&st, &gate.question, prev, None).await;
    // 🔴 `Knowledge` 开流**之前先探一次确定性问数车道**（2026-08-16，与 `/api/ask/stream`
    // 同一个 `deterministic_data_probe`）。此前这里直接开 SSE —— 与 `/api/ask` 2026-08-14
    // 修掉的缺陷一字不差：「线下-浏阳品元商贸有限公司」在 web 上修好了，
    // 在小程序上照旧只答「知识库里没有这家公司的规定」，而这家公司在业务库里有客户卡。
    // 探到实质就走同步双臂答案；探不到才开流 —— 纯资料问句的流式体验一点没变。
    if prepared.question.route() == IntentRoute::Knowledge {
        let probe = crate::deterministic_data_probe(
            &st,
            &gate.p,
            None,
            Some(gate.conv_id.to_string().as_str()),
            st.sc_samples,
            &prepared,
            None,
        )
        .await;
        // 🔴 探针结果要**接住**（2026-08-16 对抗复核逮到）：只判 `is_none()` 会把
        // 问数臂整个跑第二遍 —— 一次提问打两遍库、两份 query_log、两倍 LLM 开销，
        // 两次结果还可能不一致。`/api/ask/stream` 那侧一直是复用 `r` 再补一次 `r.kb`。
        match probe {
            None => return xcx_stream_knowledge(&st, &gate, &prepared),
            Some(mut r) => {
                // 这一档已经不是纯资料问句了：资料半同步取一次挂 `kb` 键，
                // 与混合问句同形（判据与 agent 侧同一条 `kb_has_substance`）。
                r.kb = crate::kb_answer(&st, &gate.p, None, &prepared.question.effective_question)
                    .await
                    .ok()
                    .filter(dms_agent::hybrid::kb_has_substance);
                let payload = serde_json::to_value(&r).unwrap_or_else(|e| {
                    tracing::warn!(conv_id = gate.conv_id, reason = %e, "AskResult 序列化失败，回空对象");
                    json!({})
                });
                return Ok(ask_finish(&st, &gate, payload).await.into_response());
            }
        }
    }
    // 其余全部走与非流式**同一个出口**（Data / Unknown / 探到数的 Knowledge）。
    // 少了这一条，同一个人在小程序里换个开关（流式/非流式）就得到两种答案。
    let payload = match prepared.question.route() {
        IntentRoute::Hybrid => xcx_hybrid_payload(&st, &gate, &prepared).await?,
        _ => ask_data_payload(&st, &gate, &prepared).await?,
    };
    Ok(ask_finish(&st, &gate, payload).await.into_response())
}

/// 纯资料问句的 SSE 出口（从 `ask_stream` 的 `Knowledge` 臂原样搬出，一个字节没改）。
fn xcx_stream_knowledge(
    st: &Arc<AppState>,
    gate: &XcxAskGate,
    prepared: &crate::PreparedAsk,
) -> Result<axum::response::Response, ApiErr> {
    use axum::response::IntoResponse;
    // `Principal` → `Viewer` 与同步分支同一个映射（`kb_answer` 内部也是它）
    let v = dms_agent::answerers::knowledge::viewer(&gate.p);
    let mut extra = serde_json::Map::new();
    extra.insert("conv_id".into(), json!(gate.conv_id));
    extra.insert(
        "intent_summary".into(),
        serde_json::to_value(prepared.question.intent_summary())
            .expect("IntentSummary 是纯数据 struct，派生 Serialize 不会失败"),
    );
    if prepared.question.effective_question != gate.question {
        extra.insert(
            "resolved_question".into(),
            json!(prepared.question.effective_question),
        );
    }
    // 持久化在工人里做（答案落定后存 user/ai 两条）；错误文案与同步分支的 422 同一句
    let rx = crate::kb_api::spawn_kb_worker(
        st,
        v,
        None,
        &prepared.question.effective_question,
        Some(&gate.question),
        Some(extra.clone()),
        Some(gate.conv_id),
        |_| "暂时无法完成知识检索，请稍后重试".to_string(),
    );
    Ok(crate::kb_api::sse_response(rx, extra).into_response())
}

/// `ask` / `ask_stream` 共用的前段：require_identity → 入参校验 → Principal → 会话
/// （属主校验或新开）→ 上一轮上下文。两个端点在这些判定上必须逐字同语义 —— 同一处代码。
struct XcxAskGate {
    p: principal::Principal,
    conv_id: i64,
    /// 已 trim 的问题（限长校验也过了）
    question: String,
    /// 上一轮 (问句, 那一轮执行的 SQL)：显式 prev_question 优先，否则续会话时取服务端历史
    prev: Option<(String, Option<String>)>,
}

async fn ask_gate(
    st: &AppState,
    headers: &HeaderMap,
    req: &XcxAskReq,
) -> Result<XcxAskGate, ApiErr> {
    let id = require_identity(st, headers).await?;
    let question = req.question.trim();
    if question.is_empty() {
        return Err(fail(StatusCode::BAD_REQUEST, 400, "question 不能为空"));
    }
    // prev_sql 与 prev_question 同口径：先 trim、空串按未传（Some("") 不许直入问答管道），再限长
    let prev_sql = req.prev_sql.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // ⑤ 限长（与 web 端口径对齐）：超限 400 拒收，不静默截断
    if let Err(msg) = lengths_ok(
        question,
        req.prev_question.as_deref().map(str::trim),
        prev_sql,
    ) {
        return Err(fail(StatusCode::BAD_REQUEST, 400, &msg));
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
        Some(q) => Some((q.to_string(), prev_sql.map(str::to_string))),
        None if req.conv_id.is_some() => chat::last_turn(st.owned.pool(), conv_id)
            .await
            .inspect_err(|e| {
                tracing::warn!(conv_id, reason = %e, "取上一轮失败，本轮按首问处理")
            })
            .ok()
            .flatten(),
        None => None, // 刚开的空会话没有上一轮，不空查一库
    };
    Ok(XcxAskGate { p, conv_id, question: question.to_string(), prev })
}

/// `ask` / `ask_stream` 共用的问数分支：`crate::ask_prepared` → 错误映射（403/422 逐字同 api_ask
/// 口径）→ payload。观测写入句柄丢弃（fire-and-forget，同 api_ask / mcp_api）。
async fn ask_data_payload(
    st: &AppState,
    gate: &XcxAskGate,
    prepared: &crate::PreparedAsk,
) -> Result<Value, ApiErr> {
    let conv_id_str = gate.conv_id.to_string();
    let (r, _log) = crate::ask_prepared(
        &st.llm,
        &st.auth_mysql,
        &st.mysql,
        &st.sources,
        st.owned.pool(),
        &st.embed,
        &gate.p,
        prepared,
        None, // ds：小程序侧不提供选源，后端选源（可见源只有一个时直通主源）
        Some(&conv_id_str),
        st.sc_samples,
        None, // space_id：小程序无空间选择面（恒不限空间）
        true,
    )
    .await;
    let r = r.map_err(|e| {
        // 「无权访问数据源」是权限拒绝 → 403，同 api_ask 的语义（见那里的注释）。
        // 文案子串匹配是有损分类：上游措辞一改就静默降级成 422 —— 措辞由
        // `ds_acl_denial_wording_is_pinned_upstream` 钉着，改文案的人当场撞红
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
    // 纯资料答案走**与 `/api/ask` 同一份分档**：整份 `Answer`（角标要点得开），
    // 不是 AskResult 壳。抄第二份必漂 —— 那正是这一族缺陷的形状。
    // 小程序目前没有能力 chip；有了之后按 web 同款传进来
    if let Some(payload) = crate::knowledge_arm_payload(&r, prepared, &gate.question, false) {
        return Ok(payload);
    }
    Ok(serde_json::to_value(&r).unwrap_or_else(|e| {
        tracing::warn!(conv_id = gate.conv_id, reason = %e, "AskResult 序列化失败，回空对象（客户端会看到空白答案）");
        json!({})
    }))
}

/// 【混合查询】小程序侧编排：与 web 同一个 `crate::hybrid_payload`（两路并行 + AI 综合 +
/// `kb` 键），入参从 `XcxAskGate` 拼（无选源、无知识空间、无深度模式 —— 与 `ask_data_payload`
/// 同口径）；错误体映回小程序协议：Err 只可能来自「双路全挂」，而问数路只产 403/422 两种
/// （见 `ask_data_payload` 的映射），web 的 `{"error":…}` 壳小程序拦截器不认。
async fn xcx_hybrid_payload(
    st: &AppState,
    gate: &XcxAskGate,
    prepared: &crate::PreparedAsk,
) -> Result<Value, ApiErr> {
    let conv_id = gate.conv_id.to_string();
    let h = crate::HybridAsk {
        question: &gate.question,
        p: &gate.p,
        ds: None, // 小程序侧不提供选源，后端选源（同 `ask_data_payload`）
        conv_id: Some(conv_id.as_str()),
        space_id: None,
        sc_samples: st.sc_samples,
    };
    crate::hybrid_payload(st, &h, prepared)
        .await
        .map_err(|(status, _)| {
            if status == StatusCode::FORBIDDEN {
                fail(status, 403, "当前账号无权访问该数据源")
            } else {
                fail(status, 422, "暂时无法完成本次问数，请调整问题后重试")
            }
        })
}

/// `ask` / `ask_stream` 共用的收尾（问数分支；知识库流式的持久化在工人里做）：
/// 存会话（用户问 + AI 结果，写库失败只丢历史不拦响应）→ data 注入 `conv_id` → 包协议。
async fn ask_finish(st: &AppState, gate: &XcxAskGate, payload: Value) -> Json<Value> {
    // 存会话（用户问 + AI 结果），同 api_ask：写库失败只丢历史，不拦响应 —— 但不再静默吞错
    if let Err(e) = chat::save_msg(st.owned.pool(), gate.conv_id, "user", &gate.question, None).await {
        tracing::debug!(conv_id = gate.conv_id, reason = %e, "会话历史写入失败（user 轮）");
    }
    if let Err(e) = chat::save_msg(st.owned.pool(), gate.conv_id, "ai", "", Some(&payload)).await {
        tracing::debug!(conv_id = gate.conv_id, reason = %e, "会话历史写入失败（ai 轮）");
    }
    // data = AskResult/Answer 全字段（前端自己拆）+ conv_id：客户端拿着它追问才能串起多轮。
    // AskResult 自身没有 conv_id 字段，注入不撞键（有撞键那天这个 insert 会静默盖掉它 ——
    // 所以这里断言式说明：注入键是协议增量，不是覆盖）。
    let mut data = payload;
    if let Some(m) = data.as_object_mut() {
        m.insert("conv_id".to_string(), json!(gate.conv_id));
    } else {
        // payload 恒为 object（AskResult/Answer 序列化）；真到这儿说明序列化形状变了
        tracing::debug!(conv_id = gate.conv_id, "ask 结果非 object，conv_id 未注入（多轮将串不起来）");
    }
    ok(data)
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

    /// 完整身份塞进嵌套对象时，role/name 与登录名同一条回退链（上游各端结构不一）
    #[test]
    fn parse_role_and_name_fall_back_to_nested_objects() {
        let d = json!({ "user": { "loginName": "yunfan", "roleCode": "admin", "actualName": "云帆" } });
        let id = parse_identity(&d).unwrap();
        assert_eq!(id.login_name, "yunfan");
        assert_eq!(id.role_code.as_deref(), Some("admin"), "嵌套 user 里的角色不许丢");
        assert_eq!(id.name.as_deref(), Some("云帆"));
        // 顶层角色优先于嵌套
        let d = json!({ "loginName": "a", "roleCode": "staff",
                        "user": { "roleCode": "admin", "actualName": "云帆" } });
        let id = parse_identity(&d).unwrap();
        assert_eq!(id.role_code.as_deref(), Some("staff"), "顶层优先");
        assert_eq!(id.name.as_deref(), Some("云帆"), "顶层没有则嵌套兜底");
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
        c.put("tok1".to_string(), CacheVerdict::Valid(id("zhangsan")), t0);
        // TTL 内命中
        assert_eq!(
            c.get("tok1", t0 + Duration::from_secs(59)),
            Some(CacheVerdict::Valid(id("zhangsan")))
        );
        // 恰好到点即过期（边界用 > 判定，等于 expires_at 的那一拍按 miss）
        assert_eq!(c.get("tok1", t0 + CACHE_TTL), None);
        assert_eq!(c.get("tok1", t0 + Duration::from_secs(61)), None);
        // 没见过的 key
        assert_eq!(c.get("nope", t0), None);
        // 过期后重放同 key：新身份覆盖旧条目
        c.put("tok1".to_string(), CacheVerdict::Valid(id("lisi")), t0 + Duration::from_secs(120));
        assert_eq!(
            c.get("tok1", t0 + Duration::from_secs(121)),
            Some(CacheVerdict::Valid(id("lisi")))
        );
    }

    /// 负缓存条目同样受 TTL 约束：60s 内命中 Invalid，过期后按 miss（重打上游）
    #[test]
    fn cache_negative_verdict_expires_too() {
        let mut c = TokenCache { map: HashMap::new() };
        let t0 = Instant::now();
        c.put("bad".to_string(), CacheVerdict::Invalid, t0);
        assert_eq!(c.get("bad", t0 + Duration::from_secs(59)), Some(CacheVerdict::Invalid));
        assert_eq!(c.get("bad", t0 + CACHE_TTL), None, "负缓存不是终身黑名单");
    }

    #[test]
    fn cache_evicts_oldest_when_full() {
        let mut c = TokenCache { map: HashMap::new() };
        let t0 = Instant::now();
        // 逐秒插入 CAP 条：tok0 最旧（过期点最早）
        for i in 0..CACHE_CAP {
            c.put(format!("tok{i}"), CacheVerdict::Valid(id("u")), t0 + Duration::from_secs(i as u64));
        }
        assert_eq!(c.map.len(), CACHE_CAP);
        // 满员插新 key → 最旧的 tok0 被淘汰，其余还在
        c.put("tok-new".to_string(), CacheVerdict::Valid(id("v")), t0 + Duration::from_secs(CACHE_CAP as u64));
        assert_eq!(c.map.len(), CACHE_CAP, "淘汰后容量不许涨");
        assert_eq!(c.get("tok0", t0), None, "最旧的没被淘汰");
        assert!(c.get("tok1", t0 + Duration::from_secs(2)).is_some(), "次旧的不该被误伤");
        assert!(c.get("tok-new", t0 + Duration::from_secs(CACHE_CAP as u64)).is_some());
        // 满员时**更新已存在的 key** 不触发淘汰（容量不变、内容刷新）
        c.put("tok1".to_string(), CacheVerdict::Valid(id("u2")), t0 + Duration::from_secs(CACHE_CAP as u64));
        assert_eq!(c.map.len(), CACHE_CAP);
        assert_eq!(
            c.get("tok1", t0 + Duration::from_secs(CACHE_CAP as u64)),
            Some(CacheVerdict::Valid(id("u2")))
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

    /// ① 负缓存：上游明确判失效的 token，60s 内重放不再穿透上游（ hits 恒 1 ）
    #[tokio::test]
    async fn validate_negative_caches_upstream_invalid() {
        let (base, hits, _rx) = stub_server("200 OK", r#"{"code":30007,"data":null,"msg":"x"}"#);
        let tok = format!("tok-neg-{}", uuid::Uuid::new_v4());
        assert_eq!(validate_xcx_token(&base, &tok).await, Err(AuthFail::TokenInvalid));
        assert_eq!(validate_xcx_token(&base, &tok).await, Err(AuthFail::TokenInvalid));
        assert_eq!(hits.load(Ordering::SeqCst), 1, "失效判定 TTL 内重放不许再打上游");
    }

    /// 上游回巨型 body：解析阶段不许吃无界内存（Content-Length 预检当场拒）
    #[tokio::test]
    async fn fetch_rejects_oversized_body() {
        let big: &'static str = Box::leak("x".repeat(MAX_IDENTITY_BODY_BYTES + 1).into_boxed_str());
        let (base, _, _) = stub_server("200 OK", big);
        assert_eq!(fetch_identity(&base, "tok-x").await, Err(AuthFail::Unavailable));
    }

    /// 「无权访问数据源」是 ask 链 403/422 的分类依据（contains 匹配）：上游措辞一改，
    /// 权限拒绝就静默降级成 422 —— 钉住上游文案（agent::source 的 bail 文本），改的人当场撞红
    #[test]
    fn ds_acl_denial_wording_is_pinned_upstream() {
        let src = include_str!("../../agent/src/source.rs");
        assert!(src.contains("无权访问数据源"), "ask 链 ds ACL 拒绝文案变了：xcx 的 403 分类会静默失效");
    }

    /// 瞬时故障（上游 5xx）**不进**负缓存：下一次重试必须真能打到上游
    #[tokio::test]
    async fn validate_never_caches_unavailable() {
        let (base, hits, _rx) = stub_server("500 Internal Server Error", r#"{"code":500}"#);
        let tok = format!("tok-flaky-{}", uuid::Uuid::new_v4());
        assert_eq!(validate_xcx_token(&base, &tok).await, Err(AuthFail::Unavailable));
        assert_eq!(validate_xcx_token(&base, &tok).await, Err(AuthFail::Unavailable));
        assert_eq!(hits.load(Ordering::SeqCst), 2, "瞬时故障不许缓存（误存 = 把抖动判成失效）");
    }

    // ---------- ⑤ 入参限长（与 web 端口径对齐） ----------

    #[test]
    fn lengths_are_capped_by_chars() {
        let q500 = "问".repeat(500);
        let q501 = "问".repeat(501);
        let s2000 = "s".repeat(2000);
        let s2001 = "s".repeat(2001);
        assert!(lengths_ok(&q500, None, None).is_ok(), "500 字放行（边界）");
        assert!(lengths_ok(&q501, None, None).unwrap_err().contains("question 超长"));
        assert!(lengths_ok("短", Some(&q500), None).is_ok());
        assert!(lengths_ok("短", Some(&q501), None).unwrap_err().contains("prev_question 超长"));
        assert!(lengths_ok("短", None, Some(&s2000)).is_ok(), "2000 字 SQL 放行（边界）");
        assert!(lengths_ok("短", None, Some(&s2001)).unwrap_err().contains("prev_sql 超长"));
        assert!(lengths_ok("短", None, None).is_ok(), "可选项缺省不拦");
    }

    /// 限流/限长常量是外部口径：有人改数值这条会红，逼他读常量上的注释再想一遍
    #[test]
    fn security_tuning_constants_are_pinned() {
        assert_eq!(MAX_QUESTION_CHARS, 500, "与 web 输入框 maxlength 同口径");
        assert_eq!(MAX_PREV_SQL_CHARS, 2000);
        assert_eq!(crate::auth::MAX_UPSTREAM_TOKEN_LEN, 4096, "与 SSO 验真同一条长度闸");
        assert_eq!(MAX_IDENTITY_BODY_BYTES, 256 * 1024);
    }
}
