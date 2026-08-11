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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
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

    /// 流式变体：支持 SSE 的供应商边收边把**原始增量**推进 `on_delta`（增量未过调用方
    /// 任何后处理，只许当预览；最终内容以返回的 `ChatReply` 为准）。
    /// 默认实现回退非流式：拿到全文一次性推一条增量 —— 只实现 `chat` 的存量实现
    /// （含全部测试桩）因此零改动获得流式入口，只是没有边收边推的效果。
    /// 回调是同步 `FnMut`（在读流循环里同步调用）：批量刷/节流由消费方做，kernel 不引 tokio。
    fn chat_stream<'a>(
        &'a self,
        req: ChatRequest,
        mut on_delta: Box<dyn FnMut(&str) + Send + 'a>,
    ) -> BoxFut<'a, Result<ChatReply, LlmError>> {
        Box::pin(async move {
            let reply = self.chat(req).await?;
            if let Some(text) = reply.content.as_deref() {
                on_delta(text);
            }
            Ok(reply)
        })
    }
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

    /// 默认 `chat_stream` 回退非流式：只实现 `chat` 的桩也能走流式入口 ——
    /// 增量恰一条（全文）、返回值原样（usage 不丢）。这是全部存量测试桩的兼容契约。
    #[test]
    fn default_chat_stream_falls_back_to_chat() {
        struct Stub;
        impl ChatModel for Stub {
            fn chat<'a>(&'a self, _req: ChatRequest) -> BoxFut<'a, Result<ChatReply, LlmError>> {
                Box::pin(async {
                    Ok(ChatReply {
                        content: Some("完整回答".into()),
                        usage: Usage { prompt_tokens: 7, completion_tokens: 3 },
                    })
                })
            }
        }
        // 手写一个一次性 block_on：kernel 不引 tokio（硬规则），本测试只需跑完一个 ready future
        let deltas: std::sync::Mutex<Vec<String>> = Vec::new().into();
        let fut = Stub.chat_stream(
            ChatRequest::text(ModelTier::Fast, "s", "u", None),
            Box::new(|piece: &str| deltas.lock().unwrap().push(piece.to_string())),
        );
        let reply = futures_lite_block_on(fut).unwrap();
        assert_eq!(reply.content.as_deref(), Some("完整回答"));
        assert_eq!(reply.usage.prompt_tokens, 7);
        assert_eq!(deltas.into_inner().unwrap(), vec!["完整回答".to_string()]);
    }

    /// 极简 block_on：标准库组件拼一个空 waker 轮询（本 crate 的 future 都不真挂起）。
    fn futures_lite_block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn no_op(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
        let waker: Waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                // 本 crate 的 future 要么立即 Ready，要么真 IO（测试里不会出现）；空转即 panic 防死等
                Poll::Pending => panic!("测试 future 不该挂起"),
            }
        }
    }
}
