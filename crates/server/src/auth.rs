//! 三端统一认证：会话 token 体系 + DMS token 验真（SSO）。
//! 端#2 DMS 嵌入：iframe 带 DMS 的 x-access-token → 验真(getLoginInfo)拿 login_name → 颁自有会话 token。
//! 权限计算不依赖 DMS 接口：principal 从 MySQL 只读现算（scope.rs）。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const TTL_SECS: u64 = 12 * 3600; // 12h 闲置过期（对齐旧项目）

#[derive(Clone)]
pub struct Session {
    pub login_name: String,
    pub role_code: Option<String>,
    expiry: u64,
}

type Sessions = HashMap<String, Session>;
static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 颁发会话 token
pub fn issue(login_name: String, role_code: Option<String>) -> String {
    let token = uuid::Uuid::new_v4().to_string();
    let sessions = SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = sessions.lock() {
        // 顺手清理过期项（>1000 时）
        let t = now();
        if map.len() > 1000 {
            map.retain(|_, s| s.expiry > t);
        }
        map.insert(
            token.clone(),
            Session { login_name, role_code, expiry: t + TTL_SECS },
        );
    }
    token
}

/// 解析会话 token → (login_name, role_code)，过期返回 None 并滑动续期
pub fn resolve(token: &str) -> Option<(String, Option<String>)> {
    let sessions = SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = sessions.lock().ok()?;
    let t = now();
    let s = map.get_mut(token)?;
    if s.expiry <= t {
        map.remove(token);
        return None;
    }
    s.expiry = t + TTL_SECS; // 活跃滑动续期
    Some((s.login_name.clone(), s.role_code.clone()))
}

/// 验真 DMS token：调 getLoginInfo，返回 login_name（证明该 token 属于谁）
pub async fn verify_dms_token(dms_base: &str, dms_token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let url = format!("{}/login/getLoginInfo", dms_base.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .header("x-access-token", dms_token)
        .send()
        .await?;
    let v: serde_json::Value = resp.json().await?;
    // DMS 响应 {code, msg, data}，code==1 成功
    if v.get("code").and_then(|c| c.as_i64()) != Some(1) {
        anyhow::bail!("DMS token 验真失败: {}", v.get("msg").and_then(|m| m.as_str()).unwrap_or("未知"));
    }
    v["data"]["loginName"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("getLoginInfo 返回缺 loginName"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_resolve_roundtrip() {
        let tok = issue("admin".into(), Some("city_manager".into()));
        let (ln, rc) = resolve(&tok).unwrap();
        assert_eq!(ln, "admin");
        assert_eq!(rc, Some("city_manager".into()));
        assert!(resolve("nonexistent-token").is_none());
    }
}
