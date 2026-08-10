//! `ChatModel` 契约（**只有形状，没有实现**）：唯一实现在 `connector/llm.rs`（OpenAI 兼容 HTTP）。
//!
//! 手写 `BoxFut` 而非引 `async-trait`（D6 零新增依赖）。
//! `tools` 字段不建 —— v1 不做 ReAct（ARCHITECTURE §7/§8），真做时加它是 5 行；
//! 现在建它等于让每个实现都写一遍恒空的序列化分支。

use std::error::Error;
use std::fmt;

/// 手写异步 trait 的返回类型：`Pin<Box<dyn Future + Send>>`，借用 `&'a self`。
pub type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// 模型档位。**不是模型名** —— 名字是配置（`model_fast`/`model_precise`），档位是语义：
/// 分诊/改写/汇总用 `Fast`，SQL 生成与复核用 `Precise`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier {
    Fast,
    Precise,
}

pub struct Message {
    pub role: String,
    pub content: String,
}

/// 一次对话请求。`temperature`/`max_tokens` 是**请求级**的（升温重试不许写回共享配置）。
pub struct ChatRequest {
    pub tier: ModelTier,
    pub messages: Vec<Message>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl ChatRequest {
    /// 全仓 95% 的用法：一条 system + 一条 user。**顺序即契约**（system 必须在前）。
    pub fn text(tier: ModelTier, system: &str, user: &str, temperature: Option<f32>) -> Self {
        Self {
            tier,
            messages: vec![
                Message { role: "system".into(), content: system.into() },
                Message { role: "user".into(), content: user.into() },
            ],
            temperature,
            max_tokens: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

pub struct ChatReply {
    /// `None` = 供应商返回了 200 但没给 content（该由调用方判定为失败，不是空回答）
    pub content: Option<String>,
    pub usage: Usage,
}

/// Display 文案与迁移前的 anyhow 消息逐字一致（`server/llm.rs:53/58`）：
/// repair 轮与日志吃这些文本。`Transport` 原样透传底层（reqwest）消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmError {
    Transport(String),
    Api { status: u16, body: String },
    MissingContent,
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(m) => write!(f, "{m}"),
            Self::Api { status, body } => write!(f, "LLM {status}: {body}"),
            Self::MissingContent => write!(f, "LLM 响应缺 content"),
        }
    }
}

impl Error for LlmError {}

/// 对话模型。`&'a self` + `BoxFut<'a, _>`：实现侧持 http 客户端，调用侧可放 `Arc<dyn ChatModel>`。
pub trait ChatModel: Send + Sync {
    fn chat<'a>(&'a self, req: ChatRequest) -> BoxFut<'a, Result<ChatReply, LlmError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_request_is_system_then_user() {
        let r = ChatRequest::text(ModelTier::Fast, "你是内核", "问句", Some(0.1));
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages[0].role, "system");
        assert_eq!(r.messages[0].content, "你是内核");
        assert_eq!(r.messages[1].role, "user");
        assert_eq!(r.messages[1].content, "问句");
        assert_eq!(r.temperature, Some(0.1));
        assert!(r.max_tokens.is_none());
        assert_eq!(r.tier, ModelTier::Fast);
    }

    #[test]
    fn llm_error_wording_frozen() {
        assert_eq!(LlmError::Transport("connect timeout".into()).to_string(), "connect timeout");
        assert_eq!(
            LlmError::Api { status: 429, body: "{\"e\":1}".into() }.to_string(),
            "LLM 429: {\"e\":1}"
        );
        assert_eq!(LlmError::MissingContent.to_string(), "LLM 响应缺 content");
    }
}
