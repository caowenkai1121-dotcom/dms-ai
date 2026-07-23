//! LLM 客户端：OpenAI 兼容 HTTP（DeepSeek），无框架依赖。

use serde::Serialize;

#[derive(Clone)]
pub struct LlmClient {
    pub base_url: String,
    pub api_key: String,
    pub model_fast: String,
    pub model_precise: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct Msg<'a> {
    role: &'a str,
    content: &'a str,
}

impl LlmClient {
    pub fn new(base_url: &str, api_key: &str, fast: &str, precise: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model_fast: fast.to_string(),
            model_precise: precise.to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(90))
                .build()
                .expect("http client"),
        }
    }

    pub async fn chat(&self, model: &str, system: &str, user: &str) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "model": model,
            "messages": [
                Msg { role: "system", content: system },
                Msg { role: "user", content: user },
            ],
            "temperature": 0.1,
        });
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("LLM {status}: {}", v.to_string().chars().take(300).collect::<String>());
        }
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("LLM 响应缺 content"))
    }
}

/// 从 LLM 回复中抽出 SQL（```sql 围栏优先，其次裸文本首个 SELECT 起始段）
pub fn extract_sql(text: &str) -> Option<String> {
    let t = text.trim();
    if let Some(start) = t.find("```") {
        let after = &t[start..];
        let inner_start = after.find('\n')?;
        let inner = &after[inner_start + 1..];
        let end = inner.find("```")?;
        let sql = inner[..end].trim();
        if !sql.is_empty() {
            return Some(sql.to_string());
        }
    }
    let upper = t.to_uppercase();
    let pos = upper.find("SELECT")?;
    Some(t[pos..].trim().trim_end_matches(';').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_sql() {
        let s = "好的：\n```sql\nSELECT 1 FROM t\n```\n说明";
        assert_eq!(extract_sql(s).unwrap(), "SELECT 1 FROM t");
    }

    #[test]
    fn extracts_bare_select() {
        assert_eq!(extract_sql("SELECT a FROM b;").unwrap(), "SELECT a FROM b");
    }

    #[test]
    fn none_when_no_sql() {
        assert!(extract_sql("我不知道").is_none());
    }
}
