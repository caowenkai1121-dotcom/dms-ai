//! 企业微信 OAuth（端#3）：code → userid → 通讯录身份 → 映射 DMS 员工 → 会话 token。
//! access_token 进程内缓存（企微限频，2h 有效）。

use dms_connector::mysql::ReadOnlyMySql;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const API: &str = "https://qyapi.weixin.qq.com/cgi-bin";
const OAUTH_AUTHORIZE: &str = "https://open.weixin.qq.com/connect/oauth2/authorize";
const OAUTH_STATE_TTL_SECS: u64 = 300;
pub const OAUTH_STATE_COOKIE: &str = "dms_ai_wework_state";

#[derive(Clone)]
pub struct WeworkCfg {
    pub corpid: String,
    pub secret: String,
    /// allow：OAuth 免登（本文件唯一在用的能力）不需要它；它是**消息推送**（`message/send`）的必填参数，
    /// 而推送本身被 ARCHITECTURE §8 判为「等第二个消费者出现再落」。配置早就有这一项，先接着不丢。
    #[allow(dead_code)]
    pub agentid: String,
    pub redirect_url: String,
}

struct TokenCache {
    token: String,
    expiry: u64,
}
static TOKEN: OnceLock<Mutex<Option<TokenCache>>> = OnceLock::new();
static OAUTH_STATES: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn http() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| anyhow::anyhow!("企微身份服务不可用"))
}

fn query_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// OAuth 必须从本端发起：随机 state 既绑定浏览器 Cookie，也登记为一次性服务端票据。
pub fn oauth_start(cfg: &WeworkCfg) -> anyhow::Result<(String, String)> {
    anyhow::ensure!(
        !cfg.corpid.trim().is_empty() && !cfg.redirect_url.trim().is_empty(),
        "企微登录未配置"
    );
    anyhow::ensure!(
        cfg.redirect_url.starts_with("https://") || cfg.redirect_url.starts_with("http://localhost"),
        "企微回调地址必须使用 HTTPS"
    );
    let state = uuid::Uuid::new_v4().to_string();
    let expiry = now() + OAUTH_STATE_TTL_SECS;
    let states = OAUTH_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut states = states.lock().map_err(|_| anyhow::anyhow!("企微登录服务暂不可用"))?;
    states.retain(|_, value| *value > now());
    states.insert(state.clone(), expiry);
    drop(states);

    let mut url = format!(
        "{OAUTH_AUTHORIZE}?appid={}&redirect_uri={}&response_type=code&scope=snsapi_base&state={}",
        query_encode(cfg.corpid.trim()),
        query_encode(cfg.redirect_url.trim()),
        query_encode(&state),
    );
    if !cfg.agentid.trim().is_empty() {
        url.push_str("&agentid=");
        url.push_str(&query_encode(cfg.agentid.trim()));
    }
    url.push_str("#wechat_redirect");
    Ok((url, state))
}

/// state 必须同时匹配浏览器 Cookie 和服务端票据；校验即消费，重放必失败。
pub fn consume_oauth_state(query_state: &str, cookie_state: Option<&str>) -> bool {
    let query_state = query_state.trim();
    let Some(cookie_state) = cookie_state.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if query_state.is_empty() || query_state != cookie_state {
        return false;
    }
    let Ok(mut states) = OAUTH_STATES.get_or_init(|| Mutex::new(HashMap::new())).lock() else {
        return false;
    };
    let valid = states.remove(query_state).is_some_and(|expiry| expiry > now());
    states.retain(|_, expiry| *expiry > now());
    valid
}

pub fn oauth_cookie(state: &str, secure: bool) -> String {
    format!(
        "{OAUTH_STATE_COOKIE}={state}; Path=/api/wework; Max-Age={OAUTH_STATE_TTL_SECS}; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_oauth_cookie(secure: bool) -> String {
    format!(
        "{OAUTH_STATE_COOKIE}=; Path=/api/wework; Max-Age=0; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

async fn get_json(
    req: reqwest::RequestBuilder,
    operation: &str,
) -> anyhow::Result<serde_json::Value> {
    let resp = req
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("企微 {operation} 服务不可用"))?;
    anyhow::ensure!(resp.status().is_success(), "企微 {operation} 服务不可用");
    resp.json()
        .await
        .map_err(|_| anyhow::anyhow!("企微 {operation} 响应无效"))
}

/// access_token（缓存，提前 5 分钟刷新）
pub async fn access_token(cfg: &WeworkCfg) -> anyhow::Result<String> {
    anyhow::ensure!(
        !cfg.corpid.trim().is_empty() && !cfg.secret.trim().is_empty(),
        "企微登录未配置"
    );
    let cache = TOKEN.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some(c) = guard.as_ref() {
            if c.expiry > now() + 300 {
                return Ok(c.token.clone());
            }
        }
    }
    let v = get_json(
        http()?.get(format!("{API}/gettoken")).query(&[
            ("corpid", cfg.corpid.as_str()),
            ("corpsecret", cfg.secret.as_str()),
        ]),
        "gettoken",
    )
    .await?;
    if v["errcode"].as_i64() != Some(0) {
        anyhow::bail!("企微 gettoken 失败");
    }
    let token = v["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("企微 gettoken 响应无 token"))?
        .to_string();
    let ttl = v["expires_in"].as_u64().unwrap_or(7200);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(TokenCache { token: token.clone(), expiry: now() + ttl });
    }
    Ok(token)
}

/// OAuth code → 企微 userid
pub async fn code_to_userid(cfg: &WeworkCfg, code: &str) -> anyhow::Result<String> {
    let code = code.trim();
    anyhow::ensure!(!code.is_empty() && code.len() <= 512, "企微授权 code 无效");
    let token = access_token(cfg).await?;
    let v = get_json(
        http()?.get(format!("{API}/auth/getuserinfo")).query(&[
            ("access_token", token.as_str()),
            ("code", code),
        ]),
        "getuserinfo",
    )
    .await?;
    if v["errcode"].as_i64() != Some(0) {
        anyhow::bail!("企微 getuserinfo 失败");
    }
    // 企业成员返回 userid（外部/未关注返回 openid，不支持）
    v["userid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("非企业成员或未授权（无 userid）"))
}

struct WeworkUser {
    mobile: Option<String>,
    name: Option<String>,
}

/// userid → 通讯录身份。手机号优先；旧 agent-harness 使用的花名保留为唯一匹配兜底。
async fn userid_to_user(cfg: &WeworkCfg, userid: &str) -> anyhow::Result<WeworkUser> {
    let token = access_token(cfg).await?;
    let v = get_json(
        http()?.get(format!("{API}/user/get")).query(&[
            ("access_token", token.as_str()),
            ("userid", userid),
        ]),
        "user/get",
    )
    .await?;
    if v["errcode"].as_i64() != Some(0) {
        anyhow::bail!("企微 user/get 失败");
    }
    Ok(WeworkUser {
        mobile: v["mobile"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
        name: v["name"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
    })
}

async fn user_to_login(mysql: &ReadOnlyMySql, user: WeworkUser) -> anyhow::Result<String> {
    if let Some(mobile) = user.mobile {
        let rows: Vec<(String,)> = mysql
            .fixed("SELECT login_name FROM t_employee WHERE phone = ? AND deleted_flag = 0 AND disabled_flag = 0 AND employee_num IS NOT NULL LIMIT 2")
            .bind(&mobile)
            .fetch_all()
            .await?;
        match rows.as_slice() {
            [(login,)] => return Ok(login.clone()),
            [] => {}
            _ => anyhow::bail!("企微手机号在 DMS 中不唯一"),
        }
    }
    let name = user.name.ok_or_else(|| anyhow::anyhow!("企微通讯录未返回手机号或姓名"))?;
    let rows: Vec<(String,)> = mysql
        .fixed("SELECT login_name FROM t_employee WHERE actual_name = ? AND deleted_flag = 0 AND disabled_flag = 0 AND employee_num IS NOT NULL LIMIT 2")
        .bind(&name)
        .fetch_all()
        .await?;
    match rows.as_slice() {
        [(login,)] => Ok(login.clone()),
        [] => anyhow::bail!("企微用户未匹配到 DMS 员工"),
        _ => anyhow::bail!("企微花名在 DMS 中不唯一"),
    }
}

/// 完整链：code → login_name
pub async fn login_by_code(
    cfg: &WeworkCfg,
    mysql: &ReadOnlyMySql,
    code: &str,
) -> anyhow::Result<String> {
    let userid = code_to_userid(cfg, code).await?;
    let user = userid_to_user(cfg, &userid).await?;
    user_to_login(mysql, user).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> WeworkCfg {
        WeworkCfg {
            corpid: "ww-test".into(),
            secret: "not-used".into(),
            agentid: "1000001".into(),
            redirect_url: "https://agent.example.com/api/wework/login".into(),
        }
    }

    #[test]
    fn oauth_state_is_cookie_bound_and_single_use() {
        let (url, state) = oauth_start(&cfg()).unwrap();
        assert!(url.contains("redirect_uri=https%3A%2F%2Fagent.example.com%2Fapi%2Fwework%2Flogin"));
        assert!(url.contains(&format!("state={state}")));
        assert!(!consume_oauth_state(&state, Some("wrong")));
        assert!(consume_oauth_state(&state, Some(&state)));
        assert!(!consume_oauth_state(&state, Some(&state)), "state 重放必须失败");
    }

    #[test]
    fn oauth_cookie_is_http_only_and_secure_on_https() {
        let cookie = oauth_cookie("state", true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("; Secure"));
    }
}
