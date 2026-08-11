//! 企业微信 OAuth（端#3）：code → userid → 通讯录身份 → 映射 DMS 员工 → 会话 token。
//! access_token 进程内缓存（企微限频，2h 有效）。

use dms_connector::mysql::ReadOnlyMySql;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const API: &str = "https://qyapi.weixin.qq.com/cgi-bin";
const OAUTH_AUTHORIZE: &str = "https://open.weixin.qq.com/connect/oauth2/authorize";
const OAUTH_STATE_TTL_SECS: u64 = 300;
/// token 提前刷新的余量（秒）：到期前 5 分钟就视为该换
const REFRESH_AHEAD_SECS: u64 = 300;
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
    /// 缓存按 corpid 区分：多企业配置时互不串 token（当前单配置未触发，但形态先钉对）
    corpid: String,
    token: String,
    expiry: u64,
}
/// tokio Mutex 跨 await 持锁 = single-flight：缓存 miss 时并发登录只有第一个真打
/// gettoken（企微有频次限制），其余在锁上等到新 token。无 std Mutex 的锁中毒问题。
static TOKEN: tokio::sync::Mutex<Option<TokenCache>> = tokio::sync::Mutex::const_new(None);
static OAUTH_STATES: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        // 时钟异常归 0 = 全量判过期（刻意 fail-closed）：宁可多打一次 gettoken，不用过期票据
        .unwrap_or(0)
}

/// 缓存命中判据（纯函数）：同 corpid，且距过期还有 `REFRESH_AHEAD_SECS` 余量。
fn cache_fresh(c: &TokenCache, corpid: &str, now_secs: u64) -> bool {
    c.corpid == corpid && c.expiry > now_secs + REFRESH_AHEAD_SECS
}

/// `expires_in` 缺省 7200（企微标称值）；0 / 异常大值 clamp 进 [60, 7200]。
fn ttl_of(v: &serde_json::Value) -> u64 {
    v["expires_in"].as_u64().unwrap_or(7200).clamp(60, 7200)
}

/// 进程内共享一个 Client（连接复用）：`login_by_code` 一条链原来每次调用新建 3 个。
fn http() -> anyhow::Result<reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c.clone());
    }
    let c = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| anyhow::anyhow!("企微身份服务不可用"))?;
    Ok(CLIENT.get_or_init(|| c).clone())
}

/// 手写百分号编码（query 语境，`application/x-www-form-urlencoded` 字符集）。
/// 刻意零新增依赖：`url::form_urlencoded` 只存在于 reqwest 的传递依赖里，
/// 不为这一个函数把它抬成直接依赖。
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

/// redirect_url 白名单（纯函数）：https 直接过；http 只许主机恰为 localhost。
/// 前缀比对会放过 `http://localhost.evil.com` / `http://localhost@evil.com` ——
/// OAuth code 会被引到第三方，故解析 authority（剥 userinfo 与端口）后再比。
fn redirect_url_ok(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    host == "localhost"
}

/// OAuth 必须从本端发起：随机 state 既绑定浏览器 Cookie，也登记为一次性服务端票据。
pub fn oauth_start(cfg: &WeworkCfg) -> anyhow::Result<(String, String)> {
    anyhow::ensure!(
        !cfg.corpid.trim().is_empty() && !cfg.redirect_url.trim().is_empty(),
        "企微登录未配置"
    );
    anyhow::ensure!(
        redirect_url_ok(cfg.redirect_url.trim()),
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

/// state Cookie 拼装只有这一处（Secure 后缀逻辑两处双写必漂移）。
fn state_cookie(value: &str, max_age: u64, secure: bool) -> String {
    format!(
        "{OAUTH_STATE_COOKIE}={value}; Path=/api/wework; Max-Age={max_age}; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

pub fn oauth_cookie(state: &str, secure: bool) -> String {
    state_cookie(state, OAUTH_STATE_TTL_SECS, secure)
}

pub fn clear_oauth_cookie(secure: bool) -> String {
    state_cookie("", 0, secure)
}

async fn get_json(
    req: reqwest::RequestBuilder,
    operation: &str,
) -> anyhow::Result<serde_json::Value> {
    let resp = req
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("企微 {operation} 服务不可用"))?;
    // 非 2xx 带状态码：只回「服务不可用」排障无据
    let status = resp.status();
    anyhow::ensure!(status.is_success(), "企微 {operation} 服务不可用（HTTP {status}）");
    resp.json()
        .await
        .map_err(|_| anyhow::anyhow!("企微 {operation} 响应无效"))
}

/// access_token（进程内缓存，到期前 `REFRESH_AHEAD_SECS` 秒即刷新）
pub async fn access_token(cfg: &WeworkCfg) -> anyhow::Result<String> {
    anyhow::ensure!(
        !cfg.corpid.trim().is_empty() && !cfg.secret.trim().is_empty(),
        "企微登录未配置"
    );
    // 持锁跨 await（single-flight）：miss 后并发调用在锁上等，只有第一个真打 gettoken
    let mut guard = TOKEN.lock().await;
    if let Some(c) = guard.as_ref() {
        if cache_fresh(c, cfg.corpid.trim(), now()) {
            return Ok(c.token.clone());
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
        // errcode/errmsg 是企微排障关键（40014=corpid 错、42001=token 过期等）
        anyhow::bail!(
            "企微 gettoken 失败（errcode={} errmsg={}）",
            v["errcode"],
            v["errmsg"].as_str().unwrap_or("")
        );
    }
    let token = v["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("企微 gettoken 响应无 token"))?
        .to_string();
    *guard = Some(TokenCache {
        corpid: cfg.corpid.trim().to_string(),
        token: token.clone(),
        expiry: now() + ttl_of(&v),
    });
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
        anyhow::bail!(
            "企微 getuserinfo 失败（errcode={} errmsg={}）",
            v["errcode"],
            v["errmsg"].as_str().unwrap_or("")
        );
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
        anyhow::bail!(
            "企微 user/get 失败（errcode={} errmsg={}）",
            v["errcode"],
            v["errmsg"].as_str().unwrap_or("")
        );
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
            _ => {
                // 打码留痕（只记尾号）：脏数据事后可定位，完整手机号不落日志
                let tail = mobile.get(mobile.len().saturating_sub(4)..).unwrap_or(&mobile);
                tracing::warn!(mobile_tail = %tail, "企微手机号命中多名 DMS 员工");
                anyhow::bail!("企微手机号在 DMS 中不唯一");
            }
        }
    }
    // 走到这里 = 手机号未匹配（mobile 明明可能返回了）且无姓名可兜底
    let name = user
        .name
        .ok_or_else(|| anyhow::anyhow!("企微手机号未匹配到 DMS 员工，且通讯录无姓名可兜底"))?;
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

    /// redirect_url 白名单：前缀比对放过的「localhost 开头」第三方域名必须拒
    #[test]
    fn redirect_url_localhost_prefix_cannot_be_abused() {
        assert!(redirect_url_ok("https://agent.example.com/api/wework/login"));
        assert!(redirect_url_ok("http://localhost/api/wework/login"));
        assert!(redirect_url_ok("http://localhost:8080/cb"));
        assert!(!redirect_url_ok("http://localhost.evil.com/cb"), "形似 localhost 的第三方域");
        assert!(!redirect_url_ok("http://localhost@evil.com/cb"), "userinfo 把戏");
        assert!(!redirect_url_ok("http://127.0.0.1/cb"), "只认 localhost 字面量");
        assert!(!redirect_url_ok("ftp://localhost/cb"));
    }

    /// token 缓存按 corpid 区分 + 提前 REFRESH_AHEAD_SECS 刷新
    #[test]
    fn cache_fresh_requires_same_corpid_and_margin() {
        let c = TokenCache { corpid: "ww-a".into(), token: "t".into(), expiry: 1_000 };
        let usable_at = 1_000 - REFRESH_AHEAD_SECS - 1;
        assert!(cache_fresh(&c, "ww-a", usable_at));
        assert!(!cache_fresh(&c, "ww-a", usable_at + 2), "余量不足要提前刷新");
        assert!(!cache_fresh(&c, "ww-b", usable_at), "别的企业不许串 token");
    }

    /// expires_in：缺省 7200；0 与异常大值都 clamp 进 [60, 7200]
    #[test]
    fn expires_in_is_clamped_to_sane_range() {
        assert_eq!(ttl_of(&serde_json::json!({})), 7200);
        assert_eq!(ttl_of(&serde_json::json!({"expires_in": 3600})), 3600);
        assert_eq!(ttl_of(&serde_json::json!({"expires_in": 0})), 60);
        assert_eq!(ttl_of(&serde_json::json!({"expires_in": 999_999})), 7200);
    }

    /// 源码锚点：token 缓存必须跨 await 持锁（tokio Mutex）—— 换回 std Mutex + 提前放锁
    /// 就是丢掉 single-flight，并发登录会同时打 gettoken 撞企微限频。
    #[test]
    fn token_cache_lock_is_held_across_await() {
        let src = include_str!("wework.rs");
        assert!(
            src.contains("static TOKEN: tokio::sync::Mutex<Option<TokenCache>>"),
            "TOKEN 必须是 tokio Mutex（single-flight 的载体）"
        );
        let body = src.split("pub async fn access_token").nth(1).expect("access_token 不见了");
        let lock = body.find("TOKEN.lock().await").expect("access_token 必须持锁");
        // 锚 URL 字面量而不是「gettoken」字样：注释里也会出现这个词
        let fetch = body.find("{API}/gettoken").expect("access_token 必须打 gettoken");
        assert!(lock < fetch, "必须先拿锁再取 token（锁外取数 = 并发各打一次）");
    }
}
