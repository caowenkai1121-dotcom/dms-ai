# Task 4：connector LLM 重写 ChatModel + embed 批量/双模式 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 server 的 `llm.rs` 重写为「kernel 契约 + connector OpenAI 兼容实现」的 ChatModel 形状（7 个调用点一次改到位，将来 ReAct 不必再动）；`extract_sql` 迁出 connector 归位业务侧（agent::prompt）；`embed.rs` 重做为批量 + query/passage 双模式 + 共享 reqwest::Client + 地址入配置。

**Architecture:** 契约（ChatModel trait / ChatRequest / ChatReply / ModelTier / LlmError / BoxFut）放 `dms-kernel`（纯类型零 IO）；HTTP 实现（OpenAiChatModel / EmbedClient）放 `dms-connector`；`extract_sql`（业务语义）放 `dms-agent::prompt`；server 只做装配与调用点改写，删除 `crates/server/src/llm.rs`，`embed.rs` 退化为进程级单例薄壳。

**Tech Stack:** Rust workspace、reqwest、serde/serde_json、tokio（测试）。零新增第三方依赖。

## Global Constraints

- **行为不变（逐条对齐，不许顺手改）**：
  - 7 个 chat 调用点 temperature 全部**显式 `Some(0.1)`**（历史 llm.rs:41 硬编码 0.1 的请求级平移，不得改成 None/默认值）。
  - tier 映射不许错：generate_sql:318 / repair:939 → `ModelTier::Precise`；rewrite_followup:462 / split_questions:479 / review_failure:726 / review_lessons:752 / review_exemplar:770 → `ModelTier::Fast`。
  - embed 今天的单条查询路径必须走 `EmbedMode::Query`（BGE query 指令前缀，掉成 passage 召回质量会掉）。
  - 熔断语义不变：send 失败 → 冷却 300s 内直接 None；响应解析失败 → None **不触发**熔断；timeout 3s（embed）/ 90s（LLM）不变。
  - 错误文案对齐：非 2xx → `LLM {status}: {body前300字符}`；generate_sql/repair 缺 content → `anyhow!("LLM 响应缺 content")`。
- **形状摆正但 v1 不实现 tool 循环**：`ChatRequest.tools` 全仓只填 `Vec::new()`；ToolDef/ToolCall 类型与线格式解析就位，无任何 tool 调用逻辑。
- **依赖红线**：零新增第三方依赖。异步 trait 手写 `Pin<Box<dyn Future+Send+'a>>`（BoxFut 别名），不引 async-trait。kernel 禁 reqwest/sqlx/axum 不变。
- **TDD**：每个实现子任务先写失败测试（编译失败也算红）再写实现。
- **既有测试不动**：全仓现有单测（server 157 + 新增）一个不改地通过；`extract_sql` 的 3 个原测试**一字不改**随迁。
- Windows 构建须前缀 MinGW bin 路径（见文末备注），cargo 命令走 PowerShell 不走 Bash。
- **前置假设**：Task 1（6-crate 骨架 + path 依赖）、Task 2/3（kernel 已有内容）已完成。kernel/agent/connector 的 `Cargo.toml` 本任务一律不动——所需依赖 Task 1 已就位（kernel: serde/serde_json；connector: + reqwest/tokio/futures；agent 纯 str 处理无需新依赖）。

## 调用点盘点（实测，与任务书差异已标 ⚠️）

| 类别 | 位置 | 说明 |
|---|---|---|
| chat 调用点 ×7 | pipeline.rs:318（Precise)、:462、:479、:726、:752、:770（均 Fast）、**:939 repair（Precise)⚠️ 任务书漏列** | 全部改写 |
| `llm: &LlmClient` 签名 ×10 | pipeline.rs:228/453/477/496/527/720/739/766/863/923 | 统一改 `&dyn ChatModel` |
| embed_query 调用点 ×4 | pipeline.rs:668、:820；meta.rs:601（recall_elements）、:1143（retrieve） | **零改动**（薄壳保签名） |
| extract_sql 调用点 ×2 | pipeline.rs:319、:940 | 改 import 来源 |
| main.rs 装配点 | :11 `mod llm`、:32 AppState.llm、:51 llm_client()、:94/:103/:152/:220 构造、:363 调用 | 换类型，位置不动 |

---

### Task 4.1: kernel 定义 ChatModel 契约

**Files:**
- Modify: `crates/kernel/src/lib.rs`（追加一行 `pub mod llm;`）
- Create: `crates/kernel/src/llm.rs`

**Interfaces:**
- Consumes: 无（serde/serde_json 已在 kernel 依赖）
- Produces: `BoxFut / ModelTier / Role / Message / ToolDef / ToolCall / Usage / ChatRequest / ChatReply / LlmError / ChatModel`——connector 实现它、server/agent 只依赖这层契约

- [ ] **Step 1: 写契约测试（红）**

`crates/kernel/src/llm.rs` 先只建文件，底部 tests mod 写全（此时类型不存在，编译失败=红）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
    }

    #[test]
    fn message_wire_shape_matches_legacy() {
        // 历史请求体 messages 元素就是 {"role":"system","content":"..."}
        let v = serde_json::to_value(Message::system("s")).unwrap();
        assert_eq!(v, serde_json::json!({"role": "system", "content": "s"}));
    }

    #[test]
    fn text_constructor_shape() {
        let r = ChatRequest::text(ModelTier::Fast, "sys", "usr", Some(0.1));
        assert_eq!(r.tier, ModelTier::Fast);
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages[0].role, Role::System);
        assert_eq!(r.messages[1].role, Role::User);
        assert!(r.tools.is_empty());
        assert_eq!(r.temperature, Some(0.1));
        assert_eq!(r.max_tokens, None);
    }

    #[test]
    fn error_display_matches_legacy() {
        // 历史 anyhow 文案：LLM {status}: {body}
        let e = LlmError::Api { status: 400, body: "bad".into() };
        assert_eq!(e.to_string(), "LLM 400: bad");
    }

    #[test]
    fn transient_classification() {
        assert!(LlmError::Transport("x".into()).is_transient());
        assert!(LlmError::Api { status: 429, body: "".into() }.is_transient());
        assert!(LlmError::Api { status: 500, body: "".into() }.is_transient());
        assert!(!LlmError::Api { status: 400, body: "".into() }.is_transient());
    }
}
```

- [ ] **Step 2: 写契约本体（绿）**

`crates/kernel/src/llm.rs` 完整内容（tests mod 见 Step 1，保留在文件底部）：

```rust
//! ChatModel 契约：形状对齐 ReAct（messages/tools/tier），v1 不实现 tool 循环。
//! 纯契约零 IO；OpenAI 兼容 HTTP 实现在 dms-connector。

use serde::{Deserialize, Serialize};

/// boxed future 别名：异步 trait 手写，不引 async-trait（依赖红线）
pub type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelTier {
    Fast,
    Precise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(s: &str) -> Self {
        Self { role: Role::System, content: s.to_string() }
    }
    pub fn user(s: &str) -> Self {
        Self { role: Role::User, content: s.to_string() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// OpenAI 线格式 arguments 是 JSON 字符串，保持原样不预解析
    pub arguments: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub tier: ModelTier,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl ChatRequest {
    /// v1 纯文本（system+user）便利构造：tools 空、max_tokens 不限。
    /// temperature 无默认——调用点必须显式给（历史硬编码 0.1 的行为对齐靠它）。
    pub fn text(tier: ModelTier, system: &str, user: &str, temperature: Option<f32>) -> Self {
        Self {
            tier,
            messages: vec![Message::system(system), Message::user(user)],
            tools: Vec::new(),
            temperature,
            max_tokens: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatReply {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

#[derive(Debug)]
pub enum LlmError {
    /// 网络/超时/响应 JSON 解析失败（可重试抖动）
    Transport(String),
    /// 非 2xx：status + 截断 300 字符的响应体（对齐历史文案）
    Api { status: u16, body: String },
}

impl LlmError {
    /// 抖动才重试（spec 4.2）：Transport 与 429/5xx 可重试，其余 4xx 确定性失败。
    /// v1 无调用方，Task 9 AskRun Repair 用；此处只定形状不重试。
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Api { status, .. } => *status == 429 || *status >= 500,
        }
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "LLM 传输错误: {e}"),
            Self::Api { status, body } => write!(f, "LLM {status}: {body}"),
        }
    }
}

impl std::error::Error for LlmError {}

pub trait ChatModel: Send + Sync {
    fn chat<'a>(&'a self, req: ChatRequest) -> BoxFut<'a, Result<ChatReply, LlmError>>;
}
```

并在 `crates/kernel/src/lib.rs` 追加（Task 2 已有内容则就近追加，不动既有行）：

```rust
pub mod llm;
```

> 说明：`LlmError` 按 spec 4.2 表格名义属 connector 层，但 `ChatModel::chat` 签名引用它，Rust 依赖方向（connector→kernel，不可反向）决定它只能放 kernel。这是编译约束，不是自由选择。

- [ ] **Step 3: 验证**

Run（PowerShell，前缀 MinGW）:
```
cargo test -p dms-kernel 2>&1 | Select-Object -Last 5
```
Expected: `test result: ok. 5 passed`，无 error。

- [ ] **Step 4: 提交**

```bash
git add crates/kernel/src/lib.rs crates/kernel/src/llm.rs
git commit -m "契约: kernel 定义 ChatModel/ChatRequest/ChatReply/ModelTier/LlmError（BoxFut 手写，零新依赖）"
```

---

### Task 4.2: connector 实现 OpenAiChatModel + 共享 HTTP Client

**Files:**
- Create: `crates/connector/src/http.rs`
- Create: `crates/connector/src/llm.rs`
- Modify: `crates/connector/src/lib.rs`（追加模块声明）

**Interfaces:**
- Consumes: `dms_kernel::llm::*`（Task 4.1）、reqwest（已就位）
- Produces: `OpenAiChatModel: ChatModel`；`http::shared()` 供本 crate 内 llm/embed 共用

- [ ] **Step 1: 写实现测试（红）**

`crates/connector/src/llm.rs` 底部 tests mod（此时类型不存在，编译失败=红）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use dms_kernel::llm::{ChatRequest, ModelTier};

    fn model() -> OpenAiChatModel {
        OpenAiChatModel::new("http://x/", "k", "fast-m", "precise-m")
    }

    #[test]
    fn tier_resolves_to_configured_model() {
        let m = model();
        assert_eq!(m.resolve(ModelTier::Fast), "fast-m");
        assert_eq!(m.resolve(ModelTier::Precise), "precise-m");
    }

    #[test]
    fn wire_body_matches_legacy_shape() {
        // 历史体：{"model":..,"messages":[{"role","content"}..],"temperature":0.1}，无 tools/max_tokens 键
        let req = ChatRequest::text(ModelTier::Precise, "sys", "usr", Some(0.1));
        let v = serde_json::to_value(build_body(model().resolve(req.tier), &req)).unwrap();
        assert_eq!(v["model"], "precise-m");
        assert_eq!(v["messages"][0], serde_json::json!({"role":"system","content":"sys"}));
        assert_eq!(v["messages"][1], serde_json::json!({"role":"user","content":"usr"}));
        assert_eq!(v["temperature"], 0.1);
        assert!(v.get("tools").is_none());
        assert!(v.get("max_tokens").is_none());
    }

    #[test]
    fn parse_reply_content_and_usage() {
        let body = r#"{"choices":[{"message":{"content":"SELECT 1"}}],
                      "usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;
        let r = parse_reply(body).unwrap();
        assert_eq!(r.content.as_deref(), Some("SELECT 1"));
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.usage.total_tokens, 15);
    }

    #[test]
    fn parse_reply_tool_calls_shape() {
        let body = r#"{"choices":[{"message":{"content":null,
                      "tool_calls":[{"id":"c1","function":{"name":"run_sql","arguments":"{\"sql\":\"SELECT 1\"}"}}]}}]}"#;
        let r = parse_reply(body).unwrap();
        assert_eq!(r.content, None);
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].name, "run_sql");
        assert_eq!(r.tool_calls[0].arguments, "{\"sql\":\"SELECT 1\"}");
    }

    #[test]
    fn parse_reply_empty_choices_is_transport_err() {
        assert!(matches!(parse_reply(r#"{"choices":[]}"#), Err(LlmError::Transport(_))));
    }

    #[tokio::test]
    async fn conn_refused_is_transient_transport() {
        let m = OpenAiChatModel::new("http://127.0.0.1:1", "k", "f", "p");
        let err = m.chat(ChatRequest::text(ModelTier::Fast, "s", "u", Some(0.1))).await.unwrap_err();
        assert!(matches!(err, LlmError::Transport(_)));
        assert!(err.is_transient());
    }
}
```

- [ ] **Step 2: 写实现（绿）**

`crates/connector/src/http.rs` 完整内容：

```rust
//! 全 connector 共享的 reqwest::Client（连接池复用，替代历史每次新建）。
//! 超时在各请求上单独设置（LLM 90s / embed 3s），故共享 Client 不设全局超时。

pub(crate) fn shared() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().build().expect("http client"))
}
```

`crates/connector/src/llm.rs` 完整内容（tests mod 见 Step 1，保留在底部）：

```rust
//! OpenAI 兼容 ChatModel 实现（DeepSeek）。纯 HTTP+协议，零业务语义。

use dms_kernel::llm::{
    BoxFut, ChatModel, ChatReply, ChatRequest, LlmError, Message, ModelTier, ToolCall, Usage,
};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct OpenAiChatModel {
    base_url: String,
    api_key: String,
    model_fast: String,
    model_precise: String,
}

impl OpenAiChatModel {
    pub fn new(base_url: &str, api_key: &str, fast: &str, precise: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model_fast: fast.to_string(),
            model_precise: precise.to_string(),
        }
    }

    fn resolve(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Fast => &self.model_fast,
            ModelTier::Precise => &self.model_precise,
        }
    }
}

// ---- 私有线格式：与历史请求/响应逐字段对齐 ----
// tools 为空 / Option 为 None 时整键省略，v1 请求体与历史不多一个字段。

#[derive(Serialize)]
struct WireReq<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct WireTool<'a> {
    r#type: &'static str, // 恒 "function"
    function: WireFn<'a>,
}

#[derive(Serialize)]
struct WireFn<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct WireResp {
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Usage,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMsg,
}

#[derive(Deserialize)]
struct WireMsg {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: String,
    function: WireFnOut,
}

#[derive(Deserialize)]
struct WireFnOut {
    name: String,
    arguments: String,
}

fn build_body<'a>(model: &'a str, req: &'a ChatRequest) -> WireReq<'a> {
    WireReq {
        model,
        messages: &req.messages,
        tools: req
            .tools
            .iter()
            .map(|t| WireTool {
                r#type: "function",
                function: WireFn { name: &t.name, description: &t.description, parameters: &t.parameters },
            })
            .collect(),
        temperature: req.temperature,
        max_tokens: req.max_tokens,
    }
}

fn parse_reply(text: &str) -> Result<ChatReply, LlmError> {
    let r: WireResp =
        serde_json::from_str(text).map_err(|e| LlmError::Transport(format!("响应解析失败: {e}")))?;
    let Some(first) = r.choices.into_iter().next() else {
        return Err(LlmError::Transport("响应缺 choices".into()));
    };
    Ok(ChatReply {
        content: first.message.content,
        tool_calls: first
            .message
            .tool_calls
            .into_iter()
            .map(|t| ToolCall { id: t.id, name: t.function.name, arguments: t.function.arguments })
            .collect(),
        usage: r.usage,
    })
}

impl ChatModel for OpenAiChatModel {
    fn chat<'a>(&'a self, req: ChatRequest) -> BoxFut<'a, Result<ChatReply, LlmError>> {
        Box::pin(async move {
            let body = build_body(self.resolve(req.tier), &req);
            let resp = crate::http::shared()
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.api_key)
                .timeout(std::time::Duration::from_secs(90)) // 对齐历史 client 级 90s
                .json(&body)
                .send()
                .await
                .map_err(|e| LlmError::Transport(e.to_string()))?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| LlmError::Transport(e.to_string()))?;
            if !status.is_success() {
                return Err(LlmError::Api {
                    status: status.as_u16(),
                    body: text.chars().take(300).collect(), // 对齐历史截断 300
                });
            }
            parse_reply(&text)
        })
    }
}
```

`crates/connector/src/lib.rs` 追加：

```rust
mod http;
pub mod llm;
```

> 与历史的两处刻意差异（仅错误路径信息形态，不算行为变化）：① 非 2xx 时历史先按 JSON 解析再 `to_string` 截 300，现直接取原始 body 截 300——历史遇非 JSON 错误体会先报解析错，现总能拿到原文；② 成功但缺 content 历史在客户端内报错，现按契约返回 `content: None` 由调用点决定（Task 4.5 在 generate_sql/repair 用 `ok_or_else` 复刻原文案，回落型调用点对 None 与 Err 同等回落）。

- [ ] **Step 3: 验证**

Run:
```
cargo test -p dms-connector 2>&1 | Select-Object -Last 8
```
Expected: `test result: ok. 6 passed`，无 error。

- [ ] **Step 4: 提交**

```bash
git add crates/connector/src/http.rs crates/connector/src/llm.rs crates/connector/src/lib.rs
git commit -m "connector: OpenAiChatModel 实现 ChatModel，共享 reqwest Client，线格式对齐历史"
```

---

### Task 4.3: connector EmbedClient（批量/双模式/实例熔断）+ to_pgvector 迁入

**Files:**
- Create: `crates/connector/src/embed.rs`
- Modify: `crates/connector/src/lib.rs`（追加 `pub mod embed;`）

**Interfaces:**
- Consumes: `crate::http::shared()`（Task 4.2）
- Produces: `EmbedClient{embed, embed_query, embed_passages}` / `EmbedMode{Query,Passage}` / `to_pgvector`——server 薄壳（Task 4.6）与未来 ingest 用

- [ ] **Step 1: 写实现测试（红）**

`crates/connector/src/embed.rs` 底部 tests mod：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_query_flag_by_mode() {
        let texts = vec!["a".to_string()];
        assert_eq!(build_body(&texts, EmbedMode::Query)["query"], true);
        assert_eq!(build_body(&texts, EmbedMode::Passage)["query"], false);
        assert_eq!(build_body(&texts, EmbedMode::Query)["texts"][0], "a");
    }

    #[test]
    fn parse_matrix() {
        let v = serde_json::json!({"embeddings": [[1.0, 0.5], [0.25]]});
        let m = parse_embeddings(&v).unwrap();
        assert_eq!(m, vec![vec![1.0f32, 0.5], vec![0.25]]);
        assert!(parse_embeddings(&serde_json::json!({})).is_none());
        assert!(parse_embeddings(&serde_json::json!({"embeddings": [1]})).is_none());
    }

    #[test]
    fn pgvector_literal_format() {
        assert_eq!(to_pgvector(&[1.0, 0.5]), "[1.000000,0.500000]");
    }

    #[tokio::test]
    async fn conn_refused_degrades_and_breaker_shared_across_clone() {
        let c = EmbedClient::new("http://127.0.0.1:1");
        assert!(c.embed_query("q").await.is_none()); // send 失败 → 熔断 300s
        let c2 = c.clone(); // Clone 共享熔断（对齐历史全局 static 语义）
        let t0 = std::time::Instant::now();
        assert!(c2.embed_query("q").await.is_none()); // 冷却期内直接 None，不发请求
        assert!(t0.elapsed().as_millis() < 500);
    }

    #[tokio::test]
    async fn empty_texts_short_circuits() {
        let c = EmbedClient::new("http://127.0.0.1:1");
        assert!(c.embed(&[], EmbedMode::Query).await.is_none());
    }
}
```

- [ ] **Step 2: 写实现（绿）**

`crates/connector/src/embed.rs` 完整内容（tests mod 见 Step 1，保留在底部）：

```rust
//! bge 向量服务客户端：批量 + query/passage 双模式 + 共享 HTTP + 实例级熔断。
//! 服务挂时静默降级返回 None，调用方回落词典召回——语义与历史一致。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedMode {
    Query,   // 查询侧（服务端 query=true，BGE query 指令前缀）
    Passage, // 语料侧（入库向量）
}

#[derive(Clone)]
pub struct EmbedClient {
    url: String,
    cooldown_until: Arc<AtomicU64>, // Clone 共享熔断状态（对齐历史全局 static）
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl EmbedClient {
    pub fn new(url: &str) -> Self {
        Self { url: url.to_string(), cooldown_until: Arc::new(AtomicU64::new(0)) }
    }

    /// 批量取向量。空输入 / 服务不可用 / 熔断中返回 None。
    pub async fn embed(&self, texts: &[String], mode: EmbedMode) -> Option<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return None;
        }
        if now() < self.cooldown_until.load(Ordering::Relaxed) {
            return None;
        }
        let resp = match crate::http::shared()
            .post(&self.url)
            .timeout(std::time::Duration::from_secs(3)) // 对齐历史 3s
            .json(&build_body(texts, mode))
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => {
                // 熔断 300s（仅 send 失败触发；解析失败不触发——对齐历史）
                self.cooldown_until.store(now() + 300, Ordering::Relaxed);
                return None;
            }
        };
        let v: serde_json::Value = resp.json().await.ok()?;
        parse_embeddings(&v)
    }

    /// 单条查询向量（历史 embed_query 语义：Query 模式取首行）
    pub async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        self.embed(&[text.to_string()], EmbedMode::Query).await?.into_iter().next()
    }

    /// 语料批量（Passage 模式），供未来 ingest 入库用，v1 无调用方
    pub async fn embed_passages(&self, texts: &[String]) -> Option<Vec<Vec<f32>>> {
        self.embed(texts, EmbedMode::Passage).await
    }
}

fn build_body(texts: &[String], mode: EmbedMode) -> serde_json::Value {
    serde_json::json!({ "texts": texts, "query": mode == EmbedMode::Query })
}

/// 响应 {"embeddings": [[...], ...]} → 矩阵；形状不符返回 None（不触发熔断，对齐历史）
fn parse_embeddings(v: &serde_json::Value) -> Option<Vec<Vec<f32>>> {
    let arr = v["embeddings"].as_array()?;
    arr.iter()
        .map(|row| {
            let r = row.as_array()?;
            Some(r.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
        })
        .collect()
}

/// f32 向量 → pgvector 字面量 '[...]'（原样迁自 server/embed.rs）
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
```

`crates/connector/src/lib.rs` 追加：

```rust
pub mod embed;
```

- [ ] **Step 3: 验证**

Run:
```
cargo test -p dms-connector 2>&1 | Select-Object -Last 8
```
Expected: `test result: ok. 11 passed`（4.2 的 6 + 本步 5），无 error。

- [ ] **Step 4: 提交**

```bash
git add crates/connector/src/embed.rs crates/connector/src/lib.rs
git commit -m "connector: EmbedClient 批量/query-passage双模式/实例熔断，to_pgvector 迁入"
```

---

### Task 4.4: extract_sql 迁 agent::prompt（业务语义归位）

**Files:**
- Create: `crates/agent/src/prompt.rs`
- Modify: `crates/agent/src/lib.rs`（追加 `pub mod prompt;`）

**Interfaces:**
- Consumes: 无（纯 str 处理）
- Produces: `dms_agent::prompt::extract_sql`——server pipeline（Task 4.5）import；spec 3.3 规定 prompt.rs 同时是 Task 9「8 段上下文组装」的落点，本步只放 extract_sql

- [ ] **Step 1: 原样搬迁（代码+3 个测试一字不改）**

`crates/agent/src/prompt.rs` 完整内容（函数体与测试从 `crates/server/src/llm.rs:62-99` 原样复制，仅加模块 doc）：

```rust
//! prompt 组装（8 段上下文组装属 Task 9）与 LLM 回复抽取。
//! extract_sql 是业务语义不是协议，故从 connector 迁出（spec 3.3）。

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
```

`crates/agent/src/lib.rs` 追加：

```rust
pub mod prompt;
```

- [ ] **Step 2: 验证**

Run:
```
cargo test -p dms-agent 2>&1 | Select-Object -Last 5
```
Expected: `test result: ok. 3 passed`（原 3 测试一字不改地过=硬验收）。

- [ ] **Step 3: 提交**

```bash
git add crates/agent/src/prompt.rs crates/agent/src/lib.rs
git commit -m "agent: extract_sql 迁 prompt.rs（业务语义归位，connector 只留协议）"
```

---

### Task 4.5: server LLM 侧切换（7 调用点 + 10 签名 + 装配 + 删旧 llm.rs）

**Files:**
- Modify: `crates/server/src/pipeline.rs`（import、10 处签名、7 处调用点、tests mod 加 mock 测试）
- Modify: `crates/server/src/main.rs`（删 `mod llm;`、AppState 字段类型、llm_client() 返回类型）
- Delete: `crates/server/src/llm.rs`

**Interfaces:**
- Consumes: `dms_kernel::llm::{ChatModel, ChatRequest, ModelTier}`、`dms_agent::prompt::extract_sql`、`dms_connector::llm::OpenAiChatModel`（path 依赖 Task 1 已就位）
- Produces: pipeline 全部 llm 入口只认 `&dyn ChatModel`——评测可塞录制回放实现，server 不再感知 reqwest

- [ ] **Step 1: 写 mock 解耦测试（红）**

`crates/server/src/pipeline.rs` 底部 `mod tests` 内追加（此时 `&dyn ChatModel` 签名未改，编译失败=红）：

```rust
    // ---- ChatModel 解耦证明：调用点只依赖 kernel trait，可塞录制回放实现 ----
    use dms_kernel::llm::{BoxFut, ChatReply, LlmError};

    struct MockChatModel {
        canned: ChatReply,
        seen: std::sync::Mutex<Vec<ChatRequest>>,
    }

    impl MockChatModel {
        fn replies(content: Option<&str>) -> Self {
            Self {
                canned: ChatReply { content: content.map(|s| s.to_string()), ..Default::default() },
                seen: std::sync::Mutex::new(vec![]),
            }
        }
    }

    impl ChatModel for MockChatModel {
        fn chat<'a>(&'a self, req: ChatRequest) -> BoxFut<'a, Result<ChatReply, LlmError>> {
            self.seen.lock().unwrap().push(req);
            let r = self.canned.clone();
            Box::pin(async move { Ok(r) })
        }
    }

    #[tokio::test]
    async fn split_questions_via_chat_model_trait() {
        let mock = MockChatModel::replies(Some("[\"各省销售额\",\"各商品分类销量\"]"));
        let subs = split_questions(&mock, "分别查各省销售额和各商品分类销量").await;
        assert_eq!(subs, ["各省销售额", "各商品分类销量"]);
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].tier, ModelTier::Fast);
        assert_eq!(seen[0].temperature, Some(0.1)); // 行为对齐：历史硬编码 0.1
        assert_eq!(seen[0].messages.len(), 2);
        assert!(seen[0].tools.is_empty());
    }

    #[tokio::test]
    async fn rewrite_followup_via_chat_model_trait() {
        let mock = MockChatModel::replies(Some("上月销售额"));
        let out = rewrite_followup(&mock, "那上月呢", Some("今年销售额")).await;
        assert_eq!(out, "上月销售额");
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen[0].tier, ModelTier::Fast);
        assert_eq!(seen[0].temperature, Some(0.1));
    }

    #[tokio::test]
    async fn rewrite_followup_falls_back_when_content_missing() {
        // 对齐历史「缺 content = Err → 回落原问题」语义
        let mock = MockChatModel::replies(None);
        let out = rewrite_followup(&mock, "那上月呢", Some("今年销售额")).await;
        assert_eq!(out, "那上月呢");
    }
```

- [ ] **Step 2: 改 import 与 10 处签名**

`pipeline.rs:11` 把：
```rust
use crate::llm::{extract_sql, LlmClient};
```
改为：
```rust
use dms_agent::prompt::extract_sql;
use dms_kernel::llm::{ChatModel, ChatRequest, ModelTier};
```

10 处形参 `llm: &LlmClient` → `llm: &dyn ChatModel`（行号：228/453/477/496/527/720/739/766/863/923）。函数体内实参传递（`rewrite_followup(llm, ...)` 等）不动——引用原样透传。

- [ ] **Step 3: 改 7 处调用点（每处显式 Some(0.1)，tier 按下表）**

| # | 位置 | tier | 模式 |
|---|---|---|---|
| 1 | :318 generate_sql | Precise | 传播 `?` + 缺 content 复刻原文案 |
| 2 | :462 rewrite_followup | Fast | 回落原问题 |
| 3 | :479 split_questions | Fast | 回落 vec![] |
| 4 | :726 review_failure | Fast | let-else return |
| 5 | :752 review_lessons | Fast | let-else continue |
| 6 | :770 review_exemplar | Fast | match return |
| 7 | :939 repair | Precise | 传播 `?` + 缺 content 复刻原文案 |

#1（:318-319）把：
```rust
    let resp = llm.chat(&llm.model_precise, &system, &user).await?;
    extract_sql(&resp).ok_or_else(|| anyhow::anyhow!("LLM 未产出 SQL: {}", resp.chars().take(200).collect::<String>()))
```
改为：
```rust
    let reply = llm.chat(ChatRequest::text(ModelTier::Precise, &system, &user, Some(0.1))).await?;
    let resp = reply.content.ok_or_else(|| anyhow::anyhow!("LLM 响应缺 content"))?;
    extract_sql(&resp).ok_or_else(|| anyhow::anyhow!("LLM 未产出 SQL: {}", resp.chars().take(200).collect::<String>()))
```

#2（:462）把 `match llm.chat(&llm.model_fast, system, &user).await {` 改为：
```rust
    match llm.chat(ChatRequest::text(ModelTier::Fast, system, &user, Some(0.1))).await {
        Ok(reply) => {
            let Some(r) = reply.content else { return question.to_string() };
            let rewritten = r.trim().trim_matches('"').trim_matches('。').to_string();
            if rewritten.is_empty() { question.to_string() } else { rewritten }
        }
        Err(_) => question.to_string(),
    }
```
（即原 `Ok(r)` 臂内插一行 None 解构，其余原样）

#3（:479）同构：`match llm.chat(ChatRequest::text(ModelTier::Fast, system, question, Some(0.1))).await {`，`Ok(reply)` 臂首行插 `let Some(r) = reply.content else { return vec![] };`，原 JSON 抽取逻辑不动。

#4（:726）把：
```rust
    let Ok(resp) = llm.chat(&llm.model_fast, system, &user).await else { return };
```
改为：
```rust
    let Ok(reply) = llm.chat(ChatRequest::text(ModelTier::Fast, system, &user, Some(0.1))).await else { return };
    let Some(resp) = reply.content else { return };
```

#5（:752）同 #4，`else { continue }` 两连。

#6（:770）把 `let status = match llm.chat(&llm.model_fast, system, &user).await {` 改为：
```rust
    let status = match llm.chat(ChatRequest::text(ModelTier::Fast, system, &user, Some(0.1))).await {
        Ok(reply) => {
            let Some(r) = reply.content else { return };
            if r.to_uppercase().contains("NEGATIVE") { "disabled" } else { "enabled" }
        }
        Err(_) => return, // 复核失败保持 pending，下次再议
    };
```

#7（:939-940）把：
```rust
    let resp = llm.chat(&llm.model_precise, &system, &user).await?;
    extract_sql(&resp).ok_or_else(|| anyhow::anyhow!("自修未产出 SQL"))
```
改为：
```rust
    let reply = llm.chat(ChatRequest::text(ModelTier::Precise, &system, &user, Some(0.1))).await?;
    let resp = reply.content.ok_or_else(|| anyhow::anyhow!("LLM 响应缺 content"))?;
    extract_sql(&resp).ok_or_else(|| anyhow::anyhow!("自修未产出 SQL"))
```

> `?` 可直接吞 LlmError：kernel 侧已实现 `std::error::Error + Send + Sync`，anyhow 自动 From——pipeline 的 anyhow 风格不动。

- [ ] **Step 4: main.rs 装配 + 删旧文件**

- 删 `main.rs:11` 的 `mod llm;` 一行；删文件 `crates/server/src/llm.rs`。
- `main.rs:32` AppState 字段 `llm: llm::LlmClient,` → `llm: dms_connector::llm::OpenAiChatModel,`
- `main.rs:51-53` 改为：
```rust
fn llm_client(cfg: &db::Settings) -> dms_connector::llm::OpenAiChatModel {
    dms_connector::llm::OpenAiChatModel::new(&cfg.llm_base_url, &cfg.llm_api_key, &cfg.llm_model_fast, &cfg.llm_model_precise)
}
```
- 4 处构造点（:94/:103/:152/:220）与 3 处传参（:104/:154/:363）不动：`&OpenAiChatModel` 在实参位自动 unsized 强转为 `&dyn ChatModel`。

- [ ] **Step 5: 验证（硬门禁）**

Run:
```
cargo build 2>&1 | Select-Object -Last 3
cargo test -p dms-ai-server 2>&1 | Select-Object -Last 8
```
Expected: 编译零 error；既有测试全过 + 新增 3 个 mock 测试过。

Run（显式 0.1 计数门禁——spec 步 4 验收）:
```
(Select-String -Path crates\server\src\pipeline.rs -Pattern "Some\(0\.1\)").Count
```
Expected: **7**（任务书写 6，实测含 repair:939 共 7，见盘点表）。

Run（确认旧符号绝迹）:
```
Select-String -Path crates\server\src -Pattern "LlmClient|model_fast, &system|model_precise, &system"
```
Expected: 空。

- [ ] **Step 6: 提交**

```bash
git add crates/server/src/pipeline.rs crates/server/src/main.rs
git rm crates/server/src/llm.rs
git commit -m "server: pipeline 7 调用点切 ChatModel 契约，temperature 显式 0.1，删旧 llm.rs"
```

---

### Task 4.6: server embed 侧切换（薄壳 + 单例 + 地址入配置）

**Files:**
- Modify: `crates/server/src/embed.rs`（重写为薄壳）
- Modify: `crates/server/src/db.rs`（Settings 加 `embed_url`）
- Modify: `crates/server/src/main.rs`（`load_settings` 后加一行 init）
- Modify: `settings.example.json`（加示例键）

**Interfaces:**
- Consumes: `dms_connector::embed::{EmbedClient, to_pgvector}`
- Produces: `crate::embed::{init, embed_query, to_pgvector}`——签名与历史一致，pipeline.rs:668/820 与 meta.rs:601/1143 四个调用点**零改动**

> 设计说明：不改成 EmbedClient 参数穿透——那要动 `meta::retrieve`/`recall_elements` 签名，越界进 Task 7（meta 解体）地盘。进程级单例语义与历史完全一致（历史熔断本来就是全局 static）。

- [ ] **Step 1: 重写 embed.rs 为薄壳**

`crates/server/src/embed.rs` 完整替换为：

```rust
//! embed 薄壳：实现已迁 dms-connector；本层只做进程级单例，对齐历史全局语义。
//! 调用点（pipeline/meta 共 4 处）零改动；init 缺省时静默降级（=服务缺席语义）。

use dms_connector::embed::EmbedClient;
use std::sync::OnceLock;

static CLIENT: OnceLock<EmbedClient> = OnceLock::new();

/// main() 加载配置后调用一次；重复调用忽略
pub fn init(url: &str) {
    let _ = CLIENT.set(EmbedClient::new(url));
}

/// 查询向量（512维）。未 init/服务不可用/熔断中返回 None，调用方降级到词典召回。
pub async fn embed_query(text: &str) -> Option<Vec<f32>> {
    CLIENT.get()?.embed_query(text).await
}

pub use dms_connector::embed::to_pgvector;
```

- [ ] **Step 2: Settings 加 embed_url（serde default 保旧 settings.json 兼容）**

`crates/server/src/db.rs` 在 `wework_agentid` 字段后追加：

```rust
    #[serde(default = "default_embed_url")]
    pub embed_url: String,
```

并加默认函数（与 `default_listen` 并列）：

```rust
fn default_embed_url() -> String {
    "http://127.0.0.1:8077/embed".into()
}
```

> 旧 settings.json 无此键 → serde default 填入与历史硬编码一致的地址，用户配置文件零改动。

- [ ] **Step 3: main.rs 挂 init**

`main.rs:66` 的 `let cfg = db::load_settings()?;` 之后紧接一行（覆盖其后全部 CLI/服务分支）：

```rust
    embed::init(&cfg.embed_url);
```

- [ ] **Step 4: settings.example.json 加示例键**

在 `"llm_model_precise"` 行后加：

```json
  "embed_url": "http://127.0.0.1:8077/embed",
```

- [ ] **Step 5: 验证（硬门禁）**

Run:
```
cargo build 2>&1 | Select-Object -Last 3
cargo test --workspace 2>&1 | Select-String "test result"
```
Expected: 编译零 error；各 crate `test result: ok`（server 既有测试全过）。

Run（默认走 query 模式门禁 + 硬编码清除门禁）:
```
Select-String -Path crates\server\src\embed.rs -Pattern "127\.0\.0\.1:8077"
Select-String -Path crates\server\src\embed.rs -Pattern "reqwest"
```
Expected: 均空（地址进 db.rs default；HTTP 全在 connector）。

Run（4 个调用点确认零改动）:
```
Select-String -Path crates\server\src\pipeline.rs,crates\server\src\meta.rs -Pattern "embed_query|to_pgvector"
```
Expected: 仍为 pipeline.rs:668/669、:820/821，meta.rs:601/604、:1143/1144 原样调用。

- [ ] **Step 6: 提交**

```bash
git add crates/server/src/embed.rs crates/server/src/db.rs crates/server/src/main.rs settings.example.json
git commit -m "server: embed 薄壳接 connector EmbedClient，服务地址入配置 embed_url"
```

---

## 自检（已执行）

- **spec 覆盖**：对应迁移步 4「connector：llm 重写 ChatModel + embed 批量/双模式」，验收门禁（调用点显式 temperature=0.1、embed 默认 query 模式）已落成 Step 5 的可执行命令。✓
- **依赖红线**：全计划零新增第三方依赖；BoxFut 手写不引 async-trait；kernel 仍零 IO；`cargo tree` 方向不变（server→agent/connector/kernel 均为 Task 1 已建边）。✓
- **占位符扫描**：无 TBD/TODO；4.1/4.2/4.3/4.4/4.6 全部给出完整文件内容，4.5 给出每处 before/after。✓
- **行为对拍点**：7×`Some(0.1)`、tier 映射表、熔断三语义（send 失败才熔断/300s/解析失败不熔断）、90s/3s 超时、错误文案 `LLM {status}: {body}` 与 `LLM 响应缺 content`、缺 content=回落（回落型）/=Err 文案复刻（传播型）、embed 4 调用点零改动、旧 settings.json 免改。✓
- **任务书差异**：chat 调用点实测 **7** 个（任务书列 6，漏 pipeline.rs:939 repair）——不修它编译就过不了，已纳入并按 Precise+0.1 处理。✓
- **mock 解耦证明**：4.5 Step 1 三个测试用 MockChatModel 驱动 split_questions/rewrite_followup，断言捕获请求的 tier/temperature/messages/tools——证明调用点只依赖 kernel trait。✓
- **后续任务钩子**：Task 9 的 prompt.rs 落点（4.4）、AskRun 的 `is_transient`（4.1）、ingest 的 `embed_passages`（4.3）已各就各位，均不提前实现。✓

## 备注（Windows 构建）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）

## 备注（可选人工冒烟，非自动化门禁）
4.5/4.6 完成后如需真链路确认，由用户自行决定执行（需真实 settings.json、LLM 与库可达）：
`cargo run -- ask <login_name> "上月销售额"`——预期与迁移前同问同结果集。
