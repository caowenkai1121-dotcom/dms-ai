//! 三端统一认证：会话 token 体系 + DMS token 验真（SSO）。
//! 端#2 DMS 嵌入：父页 postMessage 传 DMS x-access-token → 验真(getLoginInfo) → 颁自有会话 token。
//! 权限计算不依赖 DMS 接口：principal 从 MySQL 只读现算（scope.rs）。
//! 【D10】REST API key 双通道：`X-API-Key` / `Bearer <key>` 命中 mcp_keys → 同一 `load_principal` 链
//! （见 `resolve_identity_dual` 的接线契约；key 常量时间比较、错 key fail-closed 不降级）。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};

use dms_connector::mysql::ReadOnlyMySql;

// 12h 闲置过期（对齐旧项目）。⚠️ 纯滑动续期、**无绝对上限**：定时探活可让一张 token
// 永久不过期 —— 这是有意对齐旧项目的取舍（会话可被 revoke，敏感面另有判据兜底）；
// 要加绝对上限需给 Session 增 issued_at 并迁移存量会话，留给会话体系改造一并做。
const TTL_SECS: u64 = 12 * 3600;
const MAX_LOGIN_LEN: usize = 30;
const MAX_ROLE_LEN: usize = 64;
const MAX_PASSWORD_LEN: usize = 256;
/// 上游 token 长度闸（ SSO 与 xcx 同一条，xcx 侧作缓存 key 前也用它挡异常体量）
pub const MAX_UPSTREAM_TOKEN_LEN: usize = 4096;
/// 上游身份头（getLoginInfo 的 token 载体）：SSO 验真与 xcx 校验共用同一字面量，
/// 改协议只许改这一处（两处各写一份必漂）
pub const UPSTREAM_TOKEN_HEADER: &str = "x-access-token";
/// 无验证码登录的失败窗口：窗口内最大失败次数与窗口长度（秒）
const LOGIN_FAIL_MAX: u8 = 5;
const LOGIN_FAIL_WINDOW_SECS: u64 = 300;
/// 失败计数表容量帽与 key 截断：喷洒唯一/超长账号名不许让这张表无界增长
///（与 IP_RATE_CAP 同一取舍：满员先清扫过期窗口，仍满则放行不记账 —— 防暴力闸自身不能成为 DoS 面）
const LOGIN_FAIL_CAP: usize = 4096;
const MAX_FAIL_KEY_LEN: usize = 64;
/// 会话表容量帽：满员先清扫过期项，仍满淘汰最早过期者（见 `make_room`）
const SESSION_CAP: usize = 1000;

#[derive(Clone)]
pub struct Session {
    pub login_name: String,
    pub role_code: Option<String>,
    pub source: SessionSource,
    expiry: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionSource {
    Password,
    DmsSso,
    Wework,
}

#[derive(Clone)]
pub struct ResolvedSession {
    pub login_name: String,
    pub role_code: Option<String>,
    pub source: SessionSource,
}

type Sessions = HashMap<String, Session>;
static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();
static LOGIN_FAILS: OnceLock<Mutex<LoginFailLimiter>> = OnceLock::new();

const DMS_PASSWORD_SQL: &str = "SELECT login_name, administrator_flag FROM t_employee \
    WHERE login_name = ? AND login_pwd = MD5(CONCAT('smart_', ?, '_admin_$^&*')) \
      AND deleted_flag = 0 AND disabled_flag = 0 \
      AND (passwd_expire_time IS NULL OR passwd_expire_time >= CURRENT_TIMESTAMP) LIMIT 1";
const DMS_ACTIVE_IDENTITY_SQL: &str = "SELECT administrator_flag FROM t_employee \
    WHERE login_name = ? AND deleted_flag = 0 AND disabled_flag = 0 LIMIT 1";
const FEDERATED_ROLE_PREFIX: &str = "__dms_federated_role__:";

/// 独立 UI 没有承接 DMS 的强制修改密码页，因此密码过期在此入口继续 fail-closed。
/// 该产品边界不得复用于已由 DMS 登录态认证的 SSO/企微换签。
/// 密码只作为本次只读查询的绑定参数，不保存、不打印。
pub async fn verify_password(
    mysql: &ReadOnlyMySql,
    login_name: &str,
    password: &str,
) -> anyhow::Result<Option<(String, bool)>> {
    let Some(login_name) = normalized_login(login_name) else {
        return Ok(None);
    };
    if password.is_empty() || password.len() > MAX_PASSWORD_LEN {
        return Ok(None);
    }
    let row: Option<(String, Option<i8>)> = mysql
        .fixed(DMS_PASSWORD_SQL)
        .bind(login_name)
        .bind(password)
        .fetch_optional()
        .await?;
    Ok(row.map(|(login, admin)| (login, admin.unwrap_or(0) == 1)))
}

/// SSO/企微换签前的员工有效性闸：只校验未删除、未禁用。
/// DMS `/login/getLoginInfo` 已确认登录态且允许过期密码进入强制修改流程，
/// 因此这里不得再次按 `passwd_expire_time` 拒绝已登录用户。
pub async fn active_identity(
    mysql: &ReadOnlyMySql,
    login_name: &str,
) -> anyhow::Result<Option<bool>> {
    let Some(login_name) = normalized_login(login_name) else {
        return Ok(None);
    };
    let row: Option<(Option<i8>,)> = mysql
        .fixed(DMS_ACTIVE_IDENTITY_SQL)
        .bind(login_name)
        .fetch_optional()
        .await?;
    Ok(row.map(|(admin,)| admin.unwrap_or(0) == 1))
}

/// 无验证码登录的最小防暴力措施：同一账号 5 分钟内最多失败 5 次；成功即清零。
/// 结构体独立出来是为了单测能建私有实例（与 IpRateLimiter 同模子：进程级单例共享会让用例互相污染）。
#[derive(Default)]
struct LoginFailLimiter {
    /// login（小写、截断）→ (窗口内失败次数, 窗口截止 epoch 秒)
    map: HashMap<String, (u8, u64)>,
}

impl LoginFailLimiter {
    /// map key：小写 + 截断 —— 喷洒超长/唯一账号名不许产出无界 key
    fn key(login_name: &str) -> String {
        login_name.trim().to_ascii_lowercase().chars().take(MAX_FAIL_KEY_LEN).collect()
    }

    fn allowed(&mut self, login_name: &str, now: u64) -> bool {
        let key = Self::key(login_name);
        match self.map.get(&key) {
            Some((n, until)) if *until > now => *n < LOGIN_FAIL_MAX,
            Some(_) => {
                self.map.remove(&key);
                true
            }
            None => true,
        }
    }

    fn record(&mut self, login_name: &str, ok: bool, now: u64) {
        let key = Self::key(login_name);
        if ok {
            self.map.remove(&key);
            return;
        }
        match self.map.get_mut(&key) {
            // 窗口内：累加
            Some(e) if e.1 > now => e.0 = e.0.saturating_add(1),
            // 窗口已过期（或无记录）：整体重开一扇 —— 不许带着过期窗口的旧计数累加，
            // 否则窗口语义靠「下次 allowed 顺手删除」才自愈，判定先漂移一轮
            _ => {
                if self.map.len() >= LOGIN_FAIL_CAP {
                    self.map.retain(|_, (_, until)| *until > now);
                    if self.map.len() >= LOGIN_FAIL_CAP {
                        return; // 容量帽打满：放行不记账（同 IP_RATE_CAP 注释的取舍）
                    }
                }
                self.map.insert(key, (1, now + LOGIN_FAIL_WINDOW_SECS));
            }
        }
    }
}

pub fn login_allowed(login_name: &str) -> bool {
    LOGIN_FAILS
        .get_or_init(|| Mutex::new(LoginFailLimiter::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner) // 锁中毒自愈：一次 panic 不许永久打挂登录
        .allowed(login_name, now())
}

pub fn record_login(login_name: &str, ok: bool) {
    LOGIN_FAILS
        .get_or_init(|| Mutex::new(LoginFailLimiter::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .record(login_name, ok, now());
}

// ─────────────────────── 公开端点 per-IP 限流 ───────────────────────
// login / sso / wework / xcx 这些公开端点原来只有账号级失败计数（login_allowed），
// 对「换账号不换 IP」的密码喷洒、以及「坏 token 逐次穿透上游校验」没有闸。这里加
// 进程内 per-IP 固定窗口限流：同一 IP 每分钟最多 20 次，超出由调用方回 429。

/// 限流窗口（秒）与窗口内配额。这是运维口径：改数值前想清楚「正常用户一分钟点几次」。
const IP_RATE_WINDOW_SECS: u64 = 60;
const IP_RATE_MAX_PER_WINDOW: u32 = 20;
/// 容量帽：满员先清扫过期窗口，仍满则放行但不记账 —— 限流器自身绝不能成为 DoS 面
///（宁可短时放宽，也不能因为一张被打满的表把正常登录一起锁在门外）。
const IP_RATE_CAP: usize = 4096;

/// per-IP 固定窗口计数器（与 login_allowed 同模子：进程内 Mutex<HashMap>，窗口过期重开）。
/// 结构体独立出来是为了单测能建私有实例 —— 进程级单例共享会让用例互相污染。
struct IpRateLimiter {
    /// ip → (本窗口已计次数, 窗口起点 epoch 秒)
    map: HashMap<String, (u32, u64)>,
}

impl IpRateLimiter {
    /// true = 放行（并已计数）；false = 超限（调用方回 429）。`now` 注入是为了窗口判定可测。
    fn allow(&mut self, ip: &str, now: u64) -> bool {
        match self.map.get_mut(ip) {
            // 窗口内：超配额拒，否则计数 +1
            Some((n, started)) if now.saturating_sub(*started) < IP_RATE_WINDOW_SECS => {
                if *n >= IP_RATE_MAX_PER_WINDOW {
                    return false;
                }
                *n += 1;
                true
            }
            // 窗口已过期：重开一扇
            Some(entry) => {
                *entry = (1, now);
                true
            }
            None => {
                if self.map.len() >= IP_RATE_CAP {
                    let cutoff = now.saturating_sub(IP_RATE_WINDOW_SECS);
                    self.map.retain(|_, (_, started)| *started > cutoff);
                    if self.map.len() >= IP_RATE_CAP {
                        return true; // 容量帽打满：放行不记账（见 IP_RATE_CAP 注释）
                    }
                }
                self.map.insert(ip.to_string(), (1, now));
                true
            }
        }
    }
}

static IP_RATE: OnceLock<Mutex<IpRateLimiter>> = OnceLock::new();

/// 公开端点 per-IP 限流：同一 IP 每分钟 20 次，超出 false（调用方回 429）。
///
/// ## 接线契约（编排方在 main.rs 首段接线；本包不改 main.rs）
///
/// 已接线：`api_login`、`api_sso` 首段 + xcx 侧 `require_identity`（ask / me 共用）。
/// ⚠️ `api_wework_start` / `api_wework_login` 两个企微公开端点**至今未接**（企微链路
/// 有上游 code 换签兜底，喷洒面小于密码端点；本注释早前写「已接」是文档超前于实现）。
/// 要接就是下面两行：
///
/// ```ignore
/// let ip = auth::client_ip(&headers); // 各 handler 需补 `headers: HeaderMap` 提取器
/// if !auth::ip_rate_allow(&ip) {
///     return Err(err(StatusCode::TOO_MANY_REQUESTS, "请求过于频繁，请稍后重试"));
/// }
/// ```
pub fn ip_rate_allow(ip: &str) -> bool {
    IP_RATE
        .get_or_init(|| Mutex::new(IpRateLimiter { map: HashMap::new() }))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .allow(ip, now())
}

/// 调用方 IP：反代链路（docker/web/nginx.conf 已注入）取 X-Forwarded-For 首跳 = 客户端
/// 原始地址，其次 X-Real-IP，都没有记 "unknown"（无反代形态下同源请求聚成一个桶，
/// 仍防得住单点喷洒）。⚠️ XFF 可被调用方伪造：伪造换来的只是「换串绕限流」，
/// 换不来「伪造别人身份」—— 限流是削弱攻击的闸，不是身份依据。
pub fn client_ip(headers: &axum::http::HeaderMap) -> String {
    let raw = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
    // 截断防异常体量当 map key（IPv6 文本也就 45 字符，64 足够）；
    // 无头请求的 "unknown" 直接给静态串，不白付一次 collect 分配
    match raw {
        Some(ip) => ip.chars().take(64).collect(),
        None => "unknown".to_string(),
    }
}

fn now() -> u64 {
    // unwrap_or(0)：时钟回拨到 1970 之前时，限流窗口/过期判定会按 epoch 0 失真 —— 接受
    // 这个取舍（真发生时机器时间已坏到 TLS 都验不过，限流失真不是最紧要的问题）
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    debug_assert!(secs > 0, "系统时钟在 1970 之前？");
    secs
}

/// 单遍归一校验：一趟同时计长与查控制字符（原先 count + any 要扫两遍）
fn normalized_field(value: &str, max: usize) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut n = 0usize;
    for c in value.chars() {
        if c.is_control() {
            return None;
        }
        n += 1;
        if n > max {
            return None;
        }
    }
    Some(value)
}

pub fn normalized_login(value: &str) -> Option<&str> {
    normalized_field(value, MAX_LOGIN_LEN)
}

pub fn normalized_role(value: &str) -> Option<&str> {
    normalized_field(value, MAX_ROLE_LEN)
}

/// 颁发会话 token。锁异常时不得返回未落入会话表的“假成功” token。
pub fn issue(login_name: String, role_code: Option<String>) -> anyhow::Result<String> {
    issue_from(login_name, role_code, SessionSource::Password)
}

pub fn issue_from(
    login_name: String,
    role_code: Option<String>,
    source: SessionSource,
) -> anyhow::Result<String> {
    let login_name = normalized_login(&login_name)
        .ok_or_else(|| anyhow::anyhow!("无效登录身份"))?
        .to_string();
    let role_code = match role_code {
        Some(role) => Some(
            normalized_role(&role)
                .ok_or_else(|| anyhow::anyhow!("无效角色"))?
                .to_string(),
        ),
        None => None,
    };
    let token = uuid::Uuid::new_v4().to_string();
    let sessions = SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = sessions.lock().map_err(|_| anyhow::anyhow!("会话服务暂不可用"))?;
    let t = now();
    make_room(&mut map, t);
    map.insert(
        token.clone(),
        Session { login_name, role_code, source, expiry: t + TTL_SECS },
    );
    Ok(token)
}

/// 会话表容量帽：满员先清扫过期项；仍满（全是活会话）淘汰最早过期者 ——
/// 表无界涨比挤掉一个会话更糟（对照 IP_RATE_CAP 的取舍：登录可用性优先，
/// 被淘汰者无非是提前重新登录一次）。
fn make_room(map: &mut Sessions, t: u64) {
    if map.len() < SESSION_CAP {
        return;
    }
    map.retain(|_, s| s.expiry > t);
    while map.len() >= SESSION_CAP {
        let Some(oldest) = map.iter().min_by_key(|(_, s)| s.expiry).map(|(k, _)| k.clone()) else {
            break;
        };
        map.remove(&oldest);
    }
}

/// 角色换签后撤销旧 token，避免旧角色会话继续并行生效。
pub fn revoke(token: &str) {
    // 用 get 不用 get_or_init：从未颁发过会话的进程里调 revoke 不该白初始化一张空表
    let Some(sessions) = SESSIONS.get() else {
        return;
    };
    if let Ok(mut map) = sessions.lock() {
        map.remove(token);
    }
}

/// 解析会话 token → (login_name, role_code)，过期返回 None 并滑动续期
pub fn resolve_session(token: &str) -> Option<ResolvedSession> {
    let sessions = SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = sessions.lock().ok()?;
    let t = now();
    let s = map.get_mut(token)?;
    if s.expiry <= t {
        map.remove(token);
        return None;
    }
    s.expiry = t + TTL_SECS; // 活跃滑动续期
    Some(ResolvedSession {
        login_name: s.login_name.clone(),
        role_code: s.role_code.clone(),
        source: s.source,
    })
}

pub fn resolve(token: &str) -> Option<(String, Option<String>)> {
    let session = resolve_session(token)?;
    Some((session.login_name, session.role_code))
}

// ─────────────────────── 【D10】REST API key 双通道身份 ───────────────────────

/// API key 的长度护栏（配置约定 32 位随机串；超长输入直接拒，不给比较逻辑喂异常体量）
const MAX_API_KEY_LEN: usize = 256;

/// 从 Authorization 头值剥 `Bearer ` 前缀（与 main.rs `bearer()` 同一口径；
/// 解析只能有一个事实源，编排方接线 main.rs 时换用本函数）。
pub fn bearer_value(authorization: Option<&str>) -> Option<&str> {
    authorization?.strip_prefix("Bearer ")
}

/// 常量时间字符串比较：逐字节 XOR 累加，不因首个不同字节提前退出；
/// 长度不同也扫完全长（长度差一并计入结果），防时序侧信道逐位探测 key 前缀。
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let mut diff = x.len() ^ y.len();
    for i in 0..x.len().max(y.len()) {
        diff |= usize::from(x.get(i).copied().unwrap_or(0) ^ y.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

/// 在 mcp_keys（key → login_name）里查 presented key。**不走 `HashMap::get`**：
/// 逐条常量时间比较，比较次数只取决于配置的 key 数（运维面，非秘密），
/// 「第几条命中 / 前缀对了几位」都不产生时序差。mcp_keys 语义不动（与 MCP 共用同一份配置）。
pub fn api_key_login<'a>(keys: &'a HashMap<String, String>, presented: &str) -> Option<&'a str> {
    if presented.is_empty() || presented.len() > MAX_API_KEY_LEN {
        return None;
    }
    let mut hit = None;
    for (k, login) in keys {
        if constant_time_eq(k, presented) {
            hit = Some(login.as_str());
        }
    }
    hit
}

/// 双通道身份判定的四态（`resolve_identity_dual` 的返回）。
pub enum IdentityChannel {
    /// API key 命中 mcp_keys：映射的 login_name。随后**必须**走
    /// `load_principal(mysql, &login, None)`（与 MCP 同链：员工/角色逐次现算，
    /// 多角色账号 fail-closed），不得凭 key 直接放行。
    ApiKey(String),
    /// Bearer 会话 token 命中（`resolve_session` 已做滑动续期）。
    Session(ResolvedSession),
    /// 显式递了 API key（X-API-Key）但未命中：fail-closed —— 401，
    /// 且**不得**降级到 `insecure_login_fallback` 的 login_name 自报
    /// （递错了 key ≠ 没递身份，降级等于给爆破 key 的人一条旁路）。
    BadKey,
    /// 两通道都没递 / 都没命中 → 调用方按现有文案 401（或走被显式开启的回退）。
    Absent,
}

/// 「API key 或会话」双通道身份加载。
///
/// - `X-API-Key` 是「这就是 key」的显式声明：命中 → `ApiKey`；未命中 → 给 Bearer
///   会话最后一次机会，仍无 → `BadKey`（fail-closed，不降级）；
/// - 只有 `Authorization: Bearer …` 时它是双义头：先按既有语义解会话（token 是本服务
///   颁发的 UUID），解不开再按 API key 查（脚本形态 `Authorization: Bearer <key>`）；
/// - key 错绝不降级成匿名或 login_name 自报；两通道都没有 → `Absent`，401 文案沿用现有。
///
/// ## 接线契约（编排方在 `main.rs::resolve_identity` 首段接线；本包不改 main.rs）
///
/// ```ignore
/// let key_hdr = headers.get("X-API-Key").and_then(|v| v.to_str().ok());
/// let bearer_tok = auth::bearer_value(
///     headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()));
/// match auth::resolve_identity_dual(&st.mcp_keys, key_hdr, bearer_tok) {
///     // 角色码恒 None：由 load_principal 现算（多角色账号 fail-closed，与 MCP 同语义）
///     auth::IdentityChannel::ApiKey(login) => Some((login, None)),
///     auth::IdentityChannel::Session(s) if s.role_code.is_some() =>
///         Some((s.login_name.clone(), s.principal_role())),
///     // Session 无角色 = 还没选角色；BadKey = 递错 key。两者都 401（同现有文案），
///     // 且 BadKey 不许落进 insecure_login_fallback 臂。
///     auth::IdentityChannel::Session(_) | auth::IdentityChannel::BadKey => None,
///     auth::IdentityChannel::Absent => { /* 原有 insecure_login_fallback 臂，保持不变 */ }
/// }
/// ```
/// 落地样例：`chat.rs::api_conv_steer`（steer 端点）用的就是这条链。
pub fn resolve_identity_dual(
    keys: &HashMap<String, String>,
    x_api_key: Option<&str>,
    bearer_token: Option<&str>,
) -> IdentityChannel {
    let x_api_key = x_api_key.map(str::trim).filter(|k| !k.is_empty());
    if let Some(k) = x_api_key {
        if let Some(login) = api_key_login(keys, k) {
            return IdentityChannel::ApiKey(login.to_string());
        }
        tracing::warn!(key_len = k.len(), "REST API key 未命中（X-API-Key；日志不回显 key 本体）");
        if let Some(session) = bearer_token.and_then(resolve_session) {
            return IdentityChannel::Session(session);
        }
        return IdentityChannel::BadKey;
    }
    if let Some(t) = bearer_token {
        if let Some(session) = resolve_session(t) {
            return IdentityChannel::Session(session);
        }
        if let Some(login) = api_key_login(keys, t) {
            return IdentityChannel::ApiKey(login.to_string());
        }
    }
    IdentityChannel::Absent
}

impl ResolvedSession {
    pub fn principal_role(&self) -> Option<String> {
        principal_role(self.source, self.role_code.as_deref())
    }

    pub fn principal_role_for(&self, role_code: &str) -> String {
        principal_role(self.source, Some(role_code)).expect("explicit role")
    }
}

fn principal_role(source: SessionSource, role_code: Option<&str>) -> Option<String> {
    match source {
        SessionSource::Password => role_code.map(str::to_string),
        SessionSource::DmsSso | SessionSource::Wework => Some(format!(
            "{FEDERATED_ROLE_PREFIX}{}",
            role_code.unwrap_or_default()
        )),
    }
}

/// 服务端统一身份加载。独立账号密码会话沿用 DMS 密码过期规则；已由 DMS/企微认证的
/// 会话只跳过密码过期复查，禁用、删除、角色归属和管理员标记仍逐次实时读取 DMS。
pub async fn load_principal(
    mysql: &ReadOnlyMySql,
    login_name: &str,
    role_code: Option<&str>,
) -> anyhow::Result<crate::dms_policy_core::Principal> {
    let (skip_password_expiry, role_code) = match role_code {
        Some(role) if role.starts_with(FEDERATED_ROLE_PREFIX) => {
            let role = role.strip_prefix(FEDERATED_ROLE_PREFIX).expect("starts_with 已判");
            (true, (!role.is_empty()).then_some(role))
        }
        role => (false, role),
    };
    let employee_sql = if skip_password_expiry {
        "SELECT employee_id, login_name, actual_name, administrator_flag, department_id \
         FROM t_employee WHERE login_name = ? AND deleted_flag = 0 AND disabled_flag = 0"
    } else {
        "SELECT employee_id, login_name, actual_name, administrator_flag, department_id \
         FROM t_employee WHERE login_name = ? AND deleted_flag = 0 AND disabled_flag = 0 \
           AND (passwd_expire_time IS NULL OR passwd_expire_time >= CURRENT_TIMESTAMP)"
    };
    let emp: Option<(i64, String, String, Option<i8>, Option<i64>)> = mysql
        .fixed(employee_sql)
        .bind(login_name)
        .fetch_optional()
        .await?;
    let (employee_id, login_name, actual_name, administrator_flag, department_id) =
        emp.ok_or_else(|| anyhow::anyhow!("DMS 账号不可用"))?;
    let roles: Vec<(i64, String)> = mysql
        .fixed(
            "SELECT r.role_id, r.role_code FROM t_role_employee re \
             JOIN t_role r ON r.role_id = re.role_id \
             WHERE re.employee_id = ? ORDER BY r.role_id",
        )
        .bind(employee_id)
        .fetch_all()
        .await?;
    let administrator_flag = administrator_flag.unwrap_or(0) == 1;
    // 不再按「无标记即剥 admin 角色」过滤：DMS 原生语义是 `administrator_flag || role_code
    // == 'admin'` 双入口同权（scope.rs::admin_shortcut 对齐 Java L93-98/L236-243），角色管理
    // 页里「管理员」角色的数据范围本来就是「全部」。之前这道过滤比被对齐的源系统更严，
    // 把合法 admin 角色持有者打成「无可用角色」（云帆案例，2026-08-10）。
    let (role_id, role_code) = match role_code {
        Some("admin") if administrator_flag && roles.is_empty() => (0, "admin".into()),
        Some(role) => roles
            .iter()
            .find(|(_, code)| code.trim() == role)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("所选角色不可用"))?,
        None => match roles.len() {
            1 => roles[0].clone(),
            0 if administrator_flag => (0, "admin".into()),
            0 => anyhow::bail!("该账号无可用角色"),
            _ => anyhow::bail!("该账号有多个角色，请先选择角色"),
        },
    };
    Ok(crate::dms_policy_core::Principal {
        employee_id,
        login_name,
        actual_name,
        administrator_flag,
        department_id,
        role_id,
        role_code: role_code.trim().to_string(),
    })
}

/// SSO 验真专用静态客户端：复用连接池与 TLS 上下文（xcx_api 的 HTTP 单例同款范式；
/// 每次 SSO 登录新建客户端 = 每次重建 TLS 上下文）
static DMS_HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("DMS HTTP 客户端（仅配超时，构建不可失败）")
});

/// 验真 DMS token：调 getLoginInfo，返回 login_name（证明该 token 属于谁）
pub async fn verify_dms_token(dms_base: &str, dms_token: &str) -> anyhow::Result<String> {
    let dms_token = dms_token.trim();
    anyhow::ensure!(
        !dms_token.is_empty() && dms_token.len() <= MAX_UPSTREAM_TOKEN_LEN,
        "DMS token 无效"
    );
    let url = format!("{}/login/getLoginInfo", dms_base.trim_end_matches('/'));
    // map_err 收敛文案前一律 warn 留真因（对齐 xcx 侧 fetch_identity 每个失败分支都留痕）
    let resp = DMS_HTTP
        .get(&url)
        .header(UPSTREAM_TOKEN_HEADER, dms_token)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(reason = %e, "DMS getLoginInfo 请求失败（网络/超时）");
            anyhow::anyhow!("DMS 身份服务不可用")
        })?;
    let status = resp.status();
    // 先判状态再解析 body：上游 401 回 HTML/空体时，「验真失败: HTTP 401」比「响应无效」好查
    if !status.is_success() {
        anyhow::bail!("DMS token 验真失败: HTTP {status}");
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| {
            tracing::warn!(reason = %e, "DMS getLoginInfo 响应不是 JSON");
            anyhow::anyhow!("DMS 身份服务响应无效")
        })?;
    parse_dms_login_info(&v)
}

fn parse_dms_login_info(v: &serde_json::Value) -> anyhow::Result<String> {
    // DMS ResponseDTO 的真实成功契约：code=0、ok=true（不是旧适配器误写的 code=1）。
    if v.get("code").and_then(|c| c.as_i64()) != Some(0)
        || v.get("ok").and_then(|ok| ok.as_bool()) != Some(true)
    {
        // 上游 msg 可能含内部诊断或用户信息；认证边界只保留固定分类。
        anyhow::bail!("DMS token 验真失败");
    }
    let login = v["data"]["loginName"]
        .as_str()
        .and_then(normalized_login)
        .ok_or_else(|| anyhow::anyhow!("DMS token 验真响应无有效身份"))?;
    Ok(login.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 身份加载收口守卫：server 任何文件不许直接调 policy 的 load_principal ——
    /// SSO/企微会话的角色带 `__dms_federated_role__:` 前缀，只有本文件的 `load_principal`
    /// 会剥（深度报告/知识库曾因直调 policy 版而全线 403）。
    #[test]
    fn principal_loading_only_through_auth_module() {
        let mut offenders = Vec::new();
        // 递归 walk：src/db/ 等子目录同样受守（非递归 read_dir 会漏掉它们）
        let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src 子目录") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                if name == "auth.rs" {
                    continue; // 本文件是收口本体
                }
                let body = std::fs::read_to_string(&path).expect("读源码");
                // 测试模块里的判据文本/注释里的契约说明不算调用
                let code = body.split("#[cfg(test)]").next().expect("源文件必有前置段");
                for bad in [
                    "use dms_policy::principal;",
                    "use dms_policy_core::principal",
                    "dms_policy_core::principal::load_principal(",
                ] {
                    if code.contains(bad) {
                        offenders.push(format!("{}: {}", name, bad));
                    }
                }
                // 直调形态要区分 shim：`crate::dms_policy::…` 是 main.rs 的转发（= 本文件版），
                // 只有不带 crate:: 前缀的 `dms_policy::…`（extern prelude 原名）才是真绕过
                for (i, _) in code.match_indices("dms_policy::principal::load_principal(") {
                    if !code[..i].ends_with("crate::") {
                        offenders.push(format!("{}: dms_policy::principal::load_principal(", name));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "身份加载绕过 auth 收口: {offenders:?}");
    }

    #[test]
    fn issue_resolve_roundtrip() {
        let tok = issue("admin".into(), Some("city_manager".into())).unwrap();
        let (ln, rc) = resolve(&tok).unwrap();
        assert_eq!(ln, "admin");
        assert_eq!(rc, Some("city_manager".into()));
        assert!(resolve("nonexistent-token").is_none());
        revoke(&tok);
        assert!(resolve(&tok).is_none());
    }

    #[test]
    fn federated_session_marks_only_the_principal_lookup() {
        let tok = issue_from("admin".into(), None, SessionSource::DmsSso).unwrap();
        let session = resolve_session(&tok).unwrap();
        assert_eq!(session.login_name, "admin");
        assert_eq!(session.role_code, None);
        assert_eq!(session.principal_role().as_deref(), Some(FEDERATED_ROLE_PREFIX));
        revoke(&tok);
    }

    #[test]
    fn password_check_matches_dms_hash_contract() {
        assert!(DMS_PASSWORD_SQL.contains("MD5(CONCAT('smart_', ?, '_admin_$^&*'))"));
        assert!(DMS_PASSWORD_SQL.contains("deleted_flag = 0"));
        assert!(DMS_PASSWORD_SQL.contains("disabled_flag = 0"));
        assert!(DMS_PASSWORD_SQL.contains("passwd_expire_time"));
        assert!(DMS_ACTIVE_IDENTITY_SQL.contains("administrator_flag"));
        assert!(DMS_ACTIVE_IDENTITY_SQL.contains("deleted_flag = 0"));
        assert!(DMS_ACTIVE_IDENTITY_SQL.contains("disabled_flag = 0"));
        assert!(!DMS_ACTIVE_IDENTITY_SQL.contains("passwd_expire_time"));
    }

    #[test]
    fn dms_login_info_uses_response_dto_success_contract() {
        let ok = serde_json::json!({
            "code": 0,
            "ok": true,
            "msg": "操作成功",
            "data": { "loginName": "admin" }
        });
        assert_eq!(parse_dms_login_info(&ok).unwrap(), "admin");

        let old_wrong_contract = serde_json::json!({
            "code": 1,
            "ok": false,
            "msg": "登录失效",
            "data": null
        });
        assert!(parse_dms_login_info(&old_wrong_contract).is_err());
    }


    #[test]
    fn login_failures_are_limited_and_success_clears_them() {
        let login = format!("limit-{}", uuid::Uuid::new_v4());
        for _ in 0..LOGIN_FAIL_MAX { record_login(&login, false); }
        assert!(!login_allowed(&login));
        record_login(&login, true);
        assert!(login_allowed(&login));
    }

    /// 失败窗口语义：窗口过期后再失败整体重开（不许带旧计数累加）；常量钉值
    ///（改数值的人必须读常量上的注释再想一遍）
    #[test]
    fn login_fail_window_resets_after_expiry() {
        assert_eq!(LOGIN_FAIL_MAX, 5);
        assert_eq!(LOGIN_FAIL_WINDOW_SECS, 300);
        let mut lim = LoginFailLimiter::default();
        let t0 = 1_000_000;
        for _ in 0..LOGIN_FAIL_MAX - 1 { lim.record("alice", false, t0); }
        assert!(lim.allowed("alice", t0));
        // 窗口过期后再失败：按新窗口第一次计，而不是在旧尸骸上累加封禁
        let t1 = t0 + LOGIN_FAIL_WINDOW_SECS + 1;
        lim.record("alice", false, t1);
        assert!(lim.allowed("alice", t1), "过期窗口必须整体重置");
        // 新窗口内打满才拒
        for _ in 0..LOGIN_FAIL_MAX - 1 { lim.record("alice", false, t1); }
        assert!(!lim.allowed("alice", t1));
    }

    /// key 截断 + 容量帽 fail-open：喷洒超长/唯一账号名不许让表无界涨
    #[test]
    fn login_fail_limiter_truncates_keys_and_fails_open_at_cap() {
        let t0 = 1_000_000;
        let mut lim = LoginFailLimiter::default();
        // 超过 MAX_FAIL_KEY_LEN 截断：两个仅尾部不同的超长名落同一个桶
        let long_a = format!("{}-a", "x".repeat(100));
        let long_b = format!("{}-b", "x".repeat(100));
        for _ in 0..LOGIN_FAIL_MAX { lim.record(&long_a, false, t0); }
        assert!(!lim.allowed(&long_b, t0), "截断后同桶（防无界 key）");
        // 容量帽：灌满全活窗口后新账号不记账（fail-open，同 IP_RATE_CAP 的取舍）
        let mut lim2 = LoginFailLimiter::default();
        for i in 0..LOGIN_FAIL_CAP { lim2.record(&format!("u{i}"), false, t0); }
        assert_eq!(lim2.map.len(), LOGIN_FAIL_CAP);
        lim2.record("brand-new", false, t0);
        assert!(!lim2.map.contains_key("brand-new"), "帽满不记账");
        assert!(lim2.allowed("brand-new", t0), "帽满放行 —— 限流器不许成为 DoS 面");
    }

    /// 会话容量帽：先清扫过期项；仍满（全活）淘汰最早过期者
    #[test]
    fn session_cap_sweeps_expired_then_evicts_earliest() {
        let t0 = 1_000_000;
        let mk = |expiry: u64| Session {
            login_name: "u".into(),
            role_code: None,
            source: SessionSource::Password,
            expiry,
        };
        // 全过期：清扫后全部腾出
        let mut map: Sessions = (0..SESSION_CAP).map(|i| (format!("old-{i}"), mk(t0 - 1))).collect();
        make_room(&mut map, t0);
        assert!(map.is_empty(), "过期项应被清扫");
        // 全活且恰好满员：淘汰最早过期者腾出一个空位，其余保住
        let mut map: Sessions = (0..SESSION_CAP - 1)
            .map(|i| (format!("hot-{i}"), mk(t0 + 100 + i as u64)))
            .collect();
        map.insert("victim".into(), mk(t0 + 10)); // 最早过期
        make_room(&mut map, t0 + 5);
        assert_eq!(map.len(), SESSION_CAP - 1, "淘汰一个最早过期者后回到帽内");
        assert!(!map.contains_key("victim"), "最早过期者被淘汰");
        assert!(map.contains_key("hot-0"), "次早的保住");
    }

    // ─────────────── 【D10】REST API key 双通道 ───────────────

    fn test_keys() -> HashMap<String, String> {
        HashMap::from([
            ("rk-aaaa1111bbbb2222".to_string(), "alice".to_string()),
            ("rk-cccc3333dddd4444".to_string(), "bob".to_string()),
        ])
    }

    #[test]
    fn constant_time_eq_compares_full_bytes() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(constant_time_eq("键值甲", "键值甲"));
        assert!(constant_time_eq("", ""));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"), "前缀相同长度不同也必须不等");
        assert!(!constant_time_eq("", "a"));
    }

    #[test]
    fn bearer_value_strips_only_the_prefix() {
        assert_eq!(bearer_value(Some("Bearer tok-1")), Some("tok-1"));
        assert_eq!(bearer_value(Some("bearer tok-1")), None, "前缀大小写敏感（与 main.rs bearer 同口径）");
        assert_eq!(bearer_value(Some("Basic x")), None);
        assert_eq!(bearer_value(None), None);
    }

    /// 🔴 key 比较必须逐字节常量时间：走 `HashMap::get` 会让哈希早退泄露时序，
    /// 攻击者可逐位探测 key 前缀。函数行为 + 源码形态双钉。
    #[test]
    fn api_key_lookup_is_constant_time_and_fail_closed() {
        let keys = test_keys();
        assert_eq!(api_key_login(&keys, "rk-cccc3333dddd4444"), Some("bob"));
        assert_eq!(api_key_login(&keys, "rk-aaaa1111bbbb2222"), Some("alice"));
        assert_eq!(api_key_login(&keys, "rk-cccc3333dddd4445"), None, "差一位都不许命中");
        assert_eq!(api_key_login(&keys, "rk-cccc3333dddd444"), None, "前缀相同也不许命中");
        assert_eq!(api_key_login(&keys, ""), None);
        assert_eq!(api_key_login(&keys, &"x".repeat(300)), None, "超长 key 直接拒");
        assert_eq!(api_key_login(&HashMap::new(), "rk-aaaa1111bbbb2222"), None, "空配置恒不命中");
        // 源码判据：比较路径上不许出现 HashMap 的哈希查找
        let src = include_str!("auth.rs");
        let body = src
            .split("pub fn api_key_login")
            .nth(1)
            .expect("api_key_login 没了")
            .split("\n}\n").next().unwrap();
        assert!(body.contains("constant_time_eq"), "key 比较必须走常量时间：{body}");
        assert!(!body.contains(".get("), "API key 查找不许用 HashMap::get（哈希早退泄露时序）：{body}");
    }

    /// 🔴 双通道四态：命中走 key、会话兜底、错 key fail-closed（绝不降级 login_name 自报）。
    #[test]
    fn dual_channel_identity_prefers_key_then_session_then_fail_closed() {
        let keys = test_keys();
        // ① X-API-Key 命中 → ApiKey（随后必须走 load_principal，见接线契约）
        match resolve_identity_dual(&keys, Some("rk-aaaa1111bbbb2222"), None) {
            IdentityChannel::ApiKey(l) => assert_eq!(l, "alice"),
            _ => panic!("X-API-Key 命中必须走 ApiKey 通道"),
        }
        // ② X-API-Key 未命中 + 无会话 → BadKey（fail-closed：不许降级成 login_name 自报）
        assert!(matches!(
            resolve_identity_dual(&keys, Some("rk-wrong"), None),
            IdentityChannel::BadKey
        ));
        // ③ X-API-Key 未命中但 Bearer 是有效会话 → 会话仍可信（调用方两个头都带是合法形态）
        let tok = issue("dave".into(), Some("r".into())).unwrap();
        assert!(matches!(
            resolve_identity_dual(&keys, Some("rk-wrong"), Some(&tok)),
            IdentityChannel::Session(_)
        ));
        // ④ Bearer 双义头：先按既有语义解会话
        assert!(matches!(
            resolve_identity_dual(&keys, None, Some(&tok)),
            IdentityChannel::Session(_)
        ));
        // ⑤ Bearer 携带 key（脚本形态 `Authorization: Bearer <key>`）→ ApiKey
        match resolve_identity_dual(&keys, None, Some("rk-cccc3333dddd4444")) {
            IdentityChannel::ApiKey(l) => assert_eq!(l, "bob"),
            _ => panic!("Bearer <key> 必须命中 API key 通道"),
        }
        // ⑥ Bearer 既不是会话也不是 key → Absent（不是 BadKey：Bearer 是双义头，
        //    旧行为里无效 Bearer 本来就走 401/回退判定）
        assert!(matches!(
            resolve_identity_dual(&keys, None, Some("not-a-token")),
            IdentityChannel::Absent
        ));
        // ⑦ 两通道都没递 → Absent；X-API-Key 空白串按未递处理
        assert!(matches!(resolve_identity_dual(&keys, None, None), IdentityChannel::Absent));
        assert!(matches!(resolve_identity_dual(&keys, Some("  "), None), IdentityChannel::Absent));
        revoke(&tok);
    }

    // ─────────────── 公开端点 per-IP 限流 ───────────────

    /// 窗口内 20 放 21 拒；过期重开；窗口起点在未来（时钟回拨）不 panic 且按窗内计
    #[test]
    fn ip_rate_limiter_counts_window_and_reopens() {
        let mut limiter = IpRateLimiter { map: HashMap::new() };
        let t0 = 1_000_000;
        for i in 1..=IP_RATE_MAX_PER_WINDOW {
            assert!(limiter.allow("1.2.3.4", t0), "第 {i} 次必须放行");
        }
        assert!(!limiter.allow("1.2.3.4", t0), "第 21 次必须拒");
        assert!(!limiter.allow("1.2.3.4", t0 + 59), "窗口内持续拒");
        assert!(limiter.allow("1.2.3.4", t0 + IP_RATE_WINDOW_SECS), "窗口过期重开");
        assert!(limiter.allow("5.6.7.8", t0), "别的 IP 不受影响");
        // 时钟回拨（NTP 校时）：saturating_sub 不许下溢 panic
        assert!(limiter.allow("9.9.9.9", t0));
        let _ = limiter.allow("9.9.9.9", t0 - 500);
    }

    /// 容量帽：满员先清扫过期窗口；仍满（全是活窗口）则放行不记账 —— 限流器不许成为 DoS 面
    #[test]
    fn ip_rate_limiter_cap_sweeps_expired_then_fails_open() {
        let mut limiter = IpRateLimiter { map: HashMap::new() };
        let t0 = 1_000_000;
        // 灌满 CAP 条**已过期**窗口
        for i in 0..IP_RATE_CAP {
            limiter.map.insert(format!("old-{i}"), (IP_RATE_MAX_PER_WINDOW, t0 - IP_RATE_WINDOW_SECS - 1));
        }
        // 新 IP：清扫过期条目后正常入账
        assert!(limiter.allow("new-1", t0));
        assert_eq!(limiter.map.len(), 1, "过期窗口应被清扫，只留新条目");
        // 灌满 CAP 条**活**窗口：新 IP 放行但不记账（fail-open，见 IP_RATE_CAP 注释）
        for i in 0..IP_RATE_CAP {
            limiter.map.insert(format!("hot-{i}"), (1, t0));
        }
        assert!(limiter.allow("new-2", t0), "容量帽打满时放行");
        assert!(!limiter.map.contains_key("new-2"), "但不记账（不为它挤掉别人）");
    }

    /// 进程级入口可用（用 uuid IP 隔离，不污染其它用例共享的单例）
    #[test]
    fn ip_rate_allow_process_entrypoint() {
        let ip = format!("rl-{}", uuid::Uuid::new_v4());
        assert!(ip_rate_allow(&ip));
    }

    /// XFF 首跳优先、X-Real-IP 兜底、都没有记 unknown；超长截断；空白不算
    #[test]
    fn client_ip_prefers_forwarded_for_first_hop() {
        let mut h = axum::http::HeaderMap::new();
        assert_eq!(client_ip(&h), "unknown");
        h.insert("x-real-ip", "10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&h), "10.0.0.1");
        h.insert("x-forwarded-for", " 203.0.113.9 , 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&h), "203.0.113.9", "XFF 首跳 = 客户端原始地址");
        h.insert("x-forwarded-for", "   ".parse().unwrap());
        assert_eq!(client_ip(&h), "10.0.0.1", "XFF 全空白 → 回落 X-Real-IP");
        h.insert("x-forwarded-for", format!("{}.", "9".repeat(100)).parse().unwrap());
        assert_eq!(client_ip(&h).chars().count(), 64, "超长截断防异常体量 key");
    }
}
