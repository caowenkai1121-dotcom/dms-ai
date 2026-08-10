//! Authenticated multimodal chat endpoint.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use crate::AppState;

type ApiErr = (StatusCode, Json<serde_json::Value>);

#[derive(serde::Deserialize)]
pub struct VisionReq {
    prompt: String,
    image_url: String,
}

#[derive(serde::Serialize)]
pub struct VisionResp {
    text: String,
    provider: String,
    model: String,
    fallback: bool,
    usage: VisionUsage,
}

#[derive(serde::Serialize)]
pub struct VisionUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

/// POST `/api/vision/chat`.
///
/// Protocol and image-size validation deliberately stay in `LlmClient::vision_chat`,
/// the single multimodal transport boundary shared by HTTP and future clients.
pub async fn chat(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<VisionReq>,
) -> Result<Json<VisionResp>, ApiErr> {
    // 图片入口不继承兼容性的自报 login_name 回退：只接受服务端签发且未过期的会话。
    let (login, role) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(crate::auth::resolve)
        .ok_or_else(|| api_err(StatusCode::UNAUTHORIZED, "未认证：请先登录"))?;
    dms_policy::principal::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|_| api_err(StatusCode::FORBIDDEN, "当前 DMS 身份或角色不可用"))?;
    if req.prompt.len() > 20_000 {
        return Err(api_err(StatusCode::BAD_REQUEST, "图片问题不能超过 20000 字节"));
    }

    let (text, usage, route) = st
        .llm
        .vision_chat(&req.prompt, &req.image_url)
        .await
        .map_err(public_vision_error)?;

    Ok(Json(VisionResp {
        text,
        provider: route.provider,
        model: route.model,
        fallback: route.fallback,
        usage: VisionUsage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.prompt_tokens.saturating_add(usage.completion_tokens),
        },
    }))
}

fn api_err(code: StatusCode, message: &'static str) -> ApiErr {
    (code, Json(serde_json::json!({ "error": message })))
}

fn public_vision_error(error: crate::llm::VisionError) -> ApiErr {
    use crate::llm::VisionError;
    match error {
        VisionError::InvalidImage => api_err(
            StatusCode::BAD_REQUEST,
            "图片仅支持 HTTPS 地址或受支持的 data:image Base64 数据",
        ),
        VisionError::ImageTooLarge => {
            api_err(StatusCode::PAYLOAD_TOO_LARGE, "图片大小不能超过 16MB")
        }
        VisionError::Unavailable => api_err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "当前未配置可用的多模态模型，请联系管理员在系统设置中配置",
        ),
        VisionError::Upstream => {
            api_err(StatusCode::BAD_GATEWAY, "图片解析服务暂时不可用，请稍后重试")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_errors_keep_useful_public_messages() {
        let invalid = public_vision_error(crate::llm::VisionError::InvalidImage);
        assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
        assert_eq!(invalid.1.0["error"], "图片仅支持 HTTPS 地址或受支持的 data:image Base64 数据");
        let too_large = public_vision_error(crate::llm::VisionError::ImageTooLarge);
        assert_eq!(too_large.0, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(too_large.1.0["error"], "图片大小不能超过 16MB");
    }

    #[test]
    fn provider_failures_are_fully_redacted() {
        let error = public_vision_error(crate::llm::VisionError::Upstream);
        assert_eq!(error.0, StatusCode::BAD_GATEWAY);
        assert_eq!(error.1.0["error"], "图片解析服务暂时不可用，请稍后重试");
    }

    #[test]
    fn missing_vision_route_is_actionable_without_config_details() {
        let error = public_vision_error(crate::llm::VisionError::Unavailable);
        assert_eq!(error.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            error.1.0["error"],
            "当前未配置可用的多模态模型，请联系管理员在系统设置中配置"
        );
    }

    /// HTTP 层只做认证和协议映射；主模型优先/备用千问降级必须始终由同一个
    /// `LlmClient::vision_chat` 运行时快照裁决，避免这里按供应商名写第二套路由。
    #[test]
    fn endpoint_delegates_all_provider_routing_to_llm_client() {
        let src = include_str!("vision_api.rs");
        let body = src
            .split("pub async fn chat(")
            .nth(1)
            .expect("vision chat 端点不见了")
            .split("\nfn api_err")
            .next()
            .unwrap();
        assert!(body.contains(".vision_chat("), "端点没有走统一多模态出口：{body}");
        assert!(!body.to_ascii_lowercase().contains("qwen"), "HTTP 层不许硬编码备用供应商：{body}");
        assert!(!body.to_ascii_lowercase().contains("deepseek"), "HTTP 层不许猜主供应商能力：{body}");
    }

    #[test]
    fn endpoint_requires_a_server_session_and_never_uses_reported_identity() {
        let src = include_str!("vision_api.rs");
        let body = src
            .split("pub async fn chat(")
            .nth(1)
            .expect("vision chat 端点不见了")
            .split("\nfn api_err")
            .next()
            .unwrap();
        assert!(body.contains("crate::auth::resolve"));
        assert!(!body.contains("resolve_identity"), "图片入口不许继承 login_name 兼容回退");
        assert!(!src.contains(concat!("login_name", ": Option")));
        assert!(!src.contains(concat!("role_code", ": Option")));
    }
}
