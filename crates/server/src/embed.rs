//! embed 客户端：调本地 bge 向量服务（:8077），带熔断（服务挂时静默降级不阻塞）。

use std::sync::atomic::{AtomicU64, Ordering};

const URL: &str = "http://127.0.0.1:8077/embed";
/// 熔断：连续失败后冷却期内不再尝试（防 embed 服务挂时每问白等）
static COOLDOWN_UNTIL: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 查询向量（512维）。服务不可用/熔断中返回 None，调用方降级到词典召回。
pub async fn embed_query(text: &str) -> Option<Vec<f32>> {
    if now() < COOLDOWN_UNTIL.load(Ordering::Relaxed) {
        return None;
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let body = serde_json::json!({ "texts": [text], "query": true });
    match client.post(URL).json(&body).send().await {
        Ok(resp) => {
            let v: serde_json::Value = resp.json().await.ok()?;
            let arr = v["embeddings"][0].as_array()?;
            Some(arr.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
        }
        Err(_) => {
            // 熔断 300s
            COOLDOWN_UNTIL.store(now() + 300, Ordering::Relaxed);
            None
        }
    }
}

/// f32 向量 → pgvector 字面量 '[...]'
pub fn to_pgvector(v: &[f32]) -> String {
    let mut s = String::from("[");
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{x:.6}"));
    }
    s.push(']');
    s
}
