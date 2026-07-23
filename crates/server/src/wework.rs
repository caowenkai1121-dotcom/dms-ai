//! 企业微信 OAuth（端#3）：code → userid → 手机号 → 映射 t_employee.phone → 会话 token。
//! access_token 进程内缓存（企微限频，2h 有效）。

use sqlx::MySqlPool;
use std::sync::{Mutex, OnceLock};

const API: &str = "https://qyapi.weixin.qq.com/cgi-bin";

#[derive(Clone)]
pub struct WeworkCfg {
    pub corpid: String,
    pub secret: String,
    pub agentid: String,
}

struct TokenCache {
    token: String,
    expiry: u64,
}
static TOKEN: OnceLock<Mutex<Option<TokenCache>>> = OnceLock::new();

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("http")
}

/// access_token（缓存，提前 5 分钟刷新）
pub async fn access_token(cfg: &WeworkCfg) -> anyhow::Result<String> {
    let cache = TOKEN.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some(c) = guard.as_ref() {
            if c.expiry > now() + 300 {
                return Ok(c.token.clone());
            }
        }
    }
    let url = format!("{API}/gettoken?corpid={}&corpsecret={}", cfg.corpid, cfg.secret);
    let v: serde_json::Value = http().get(&url).send().await?.json().await?;
    if v["errcode"].as_i64() != Some(0) {
        anyhow::bail!("企微 gettoken 失败: {}", v["errmsg"].as_str().unwrap_or(""));
    }
    let token = v["access_token"].as_str().unwrap_or("").to_string();
    let ttl = v["expires_in"].as_u64().unwrap_or(7200);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(TokenCache { token: token.clone(), expiry: now() + ttl });
    }
    Ok(token)
}

/// OAuth code → 企微 userid
pub async fn code_to_userid(cfg: &WeworkCfg, code: &str) -> anyhow::Result<String> {
    let token = access_token(cfg).await?;
    let url = format!("{API}/auth/getuserinfo?access_token={token}&code={code}");
    let v: serde_json::Value = http().get(&url).send().await?.json().await?;
    if v["errcode"].as_i64() != Some(0) {
        anyhow::bail!("企微 getuserinfo 失败: {}", v["errmsg"].as_str().unwrap_or(""));
    }
    // 企业成员返回 userid（外部/未关注返回 openid，不支持）
    v["userid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("非企业成员或未授权（无 userid）"))
}

/// userid → 手机号（user/get）
pub async fn userid_to_mobile(cfg: &WeworkCfg, userid: &str) -> anyhow::Result<String> {
    let token = access_token(cfg).await?;
    let url = format!("{API}/user/get?access_token={token}&userid={userid}");
    let v: serde_json::Value = http().get(&url).send().await?.json().await?;
    if v["errcode"].as_i64() != Some(0) {
        anyhow::bail!("企微 user/get 失败: {}", v["errmsg"].as_str().unwrap_or(""));
    }
    v["mobile"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("企微用户无手机号（无法映射员工）"))
}

/// 手机号 → t_employee.login_name（映射，phone 全员覆盖最可靠）
pub async fn mobile_to_login(mysql: &MySqlPool, mobile: &str) -> anyhow::Result<String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT login_name FROM t_employee WHERE phone = ? AND deleted_flag = 0 AND disabled_flag = 0 LIMIT 1",
    )
    .bind(mobile)
    .fetch_optional(mysql)
    .await?;
    row.map(|(ln,)| ln)
        .ok_or_else(|| anyhow::anyhow!("手机号 {mobile} 未匹配到员工（请确认企微手机号与 DMS 一致）"))
}

/// 完整链：code → login_name
pub async fn login_by_code(cfg: &WeworkCfg, mysql: &MySqlPool, code: &str) -> anyhow::Result<String> {
    let userid = code_to_userid(cfg, code).await?;
    let mobile = userid_to_mobile(cfg, &userid).await?;
    mobile_to_login(mysql, &mobile).await
}
