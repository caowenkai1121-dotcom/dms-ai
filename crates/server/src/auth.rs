//! 三端统一认证：会话 token 体系 + DMS token 验真（SSO）。
//! 端#2 DMS 嵌入：父页 postMessage 传 DMS x-access-token → 验真(getLoginInfo) → 颁自有会话 token。
//! 权限计算不依赖 DMS 接口：principal 从 MySQL 只读现算（scope.rs）。
//! 【D10】REST API key 双通道：`X-API-Key` / `Bearer <key>` 命中 mcp_keys → 同一 `load_principal` 链
//! （见 `resolve_identity_dual` 的接线契约；key 常量时间比较、错 key fail-closed 不降级）。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use dms_connector::mysql::ReadOnlyMySql;

const TTL_SECS: u64 = 12 * 3600; // 12h 闲置过期（对齐旧项目）
const MAX_LOGIN_LEN: usize = 30;
const MAX_ROLE_LEN: usize = 64;
const MAX_PASSWORD_LEN: usize = 256;
const MAX_UPSTREAM_TOKEN_LEN: usize = 4096;

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
static LOGIN_FAILS: OnceLock<Mutex<HashMap<String, (u8, u64)>>> = OnceLock::new();

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
pub fn login_allowed(login_name: &str) -> bool {
    let key = login_name.trim().to_ascii_lowercase();
    let now = now();
    let mut fails = LOGIN_FAILS.get_or_init(|| Mutex::new(HashMap::new())).lock().expect("login fail lock");
    match fails.get(&key) {
        Some((n, until)) if *until > now => *n < 5,
        Some(_) => { fails.remove(&key); true }
        None => true,
    }
}

pub fn record_login(login_name: &str, ok: bool) {
    let key = login_name.trim().to_ascii_lowercase();
    let mut fails = LOGIN_FAILS.get_or_init(|| Mutex::new(HashMap::new())).lock().expect("login fail lock");
    if ok {
        fails.remove(&key);
    } else {
        let e = fails.entry(key).or_insert((0, now() + 300));
        e.0 = e.0.saturating_add(1);
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn normalized_login(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= MAX_LOGIN_LEN
        && !value.chars().any(char::is_control))
    .then_some(value)
}

pub fn normalized_role(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= MAX_ROLE_LEN
        && !value.chars().any(char::is_control))
    .then_some(value)
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
    if map.len() > 1000 {
        map.retain(|_, s| s.expiry > t);
    }
    map.insert(
        token.clone(),
        Session { login_name, role_code, source, expiry: t + TTL_SECS },
    );
    Ok(token)
}

/// 角色换签后撤销旧 token，避免旧角色会话继续并行生效。
pub fn revoke(token: &str) {
    if let Ok(mut map) = SESSIONS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
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
            let role = role.strip_prefix(FEDERATED_ROLE_PREFIX).unwrap_or_default();
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
    let roles: Vec<(i64, String)> = roles
        .into_iter()
        .filter(|(_, code)| administrator_flag || code.trim() != "admin")
        .collect();
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

/// 验真 DMS token：调 getLoginInfo，返回 login_name（证明该 token 属于谁）
pub async fn verify_dms_token(dms_base: &str, dms_token: &str) -> anyhow::Result<String> {
    let dms_token = dms_token.trim();
    anyhow::ensure!(
        !dms_token.is_empty() && dms_token.len() <= MAX_UPSTREAM_TOKEN_LEN,
        "DMS token 无效"
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| anyhow::anyhow!("DMS 身份服务不可用"))?;
    let url = format!("{}/login/getLoginInfo", dms_base.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .header("x-access-token", dms_token)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("DMS 身份服务不可用"))?;
    let status = resp.status();
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("DMS 身份服务响应无效"))?;
    if !status.is_success() {
        anyhow::bail!("DMS token 验真失败: HTTP {status}");
    }
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
        for _ in 0..5 { record_login(&login, false); }
        assert!(!login_allowed(&login));
        record_login(&login, true);
        assert!(login_allowed(&login));
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
}
