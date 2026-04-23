use std::sync::OnceLock;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use regex::Regex;

/// Central error type for the bridge runtime.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// The requested agent was not found
    #[error("agent not found: {0}")]
    AgentNotFound(String),

    /// The requested conversation was not found
    #[error("conversation not found: {0}")]
    ConversationNotFound(String),

    /// The conversation has already ended
    #[error("conversation ended: {0}")]
    ConversationEnded(String),

    /// The request was invalid
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// An error occurred in the LLM provider
    #[error("provider error: {0}")]
    ProviderError(String),

    /// An error occurred in an MCP connection
    #[error("mcp error: {0}")]
    McpError(String),

    /// An error occurred during tool execution
    #[error("tool error: {0}")]
    ToolError(String),

    /// A configuration error
    #[error("config error: {0}")]
    ConfigError(String),

    /// A webhook delivery error
    #[error("webhook error: {0}")]
    WebhookError(String),

    /// An internal error
    #[error("internal error: {0}")]
    Internal(String),

    /// Rate limit exceeded
    #[error("rate limited")]
    RateLimited,

    /// Unauthorized access
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// Conflict with current state
    #[error("conflict: {0}")]
    Conflict(String),

    /// Capacity exhausted (max conversations, task budget, etc.)
    #[error("capacity exhausted: {0}")]
    CapacityExhausted(String),
}

/// Convenience Result type alias for bridge operations.
pub type Result<T> = std::result::Result<T, BridgeError>;

/// Cap on how many bytes of a provider/MCP/tool error we echo to clients.
const SANITIZE_MAX_BYTES: usize = 512;

fn sk_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"sk-[A-Za-z0-9_\-]+").unwrap())
}

fn bearer_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Bearer\s+\S+").unwrap())
}

fn api_key_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)api[_-]?key["']?\s*[:=]\s*["']?\S+"#).unwrap())
}

/// Redact common secret patterns and truncate to `SANITIZE_MAX_BYTES`
/// (UTF-8-safe) before sending error text to API clients.
fn sanitize_for_response(raw: &str) -> String {
    let step1 = sk_regex().replace_all(raw, "sk-***");
    let step2 = bearer_regex().replace_all(&step1, "Bearer ***");
    let step3 = api_key_regex().replace_all(&step2, "api_key=***");
    let s: &str = &step3;
    if s.len() <= SANITIZE_MAX_BYTES {
        return s.to_string();
    }
    let mut end = 0;
    for (idx, _) in s.char_indices() {
        if idx > SANITIZE_MAX_BYTES {
            break;
        }
        end = idx;
    }
    s[..end].to_string()
}

impl IntoResponse for BridgeError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            BridgeError::AgentNotFound(_) => (StatusCode::NOT_FOUND, "agent_not_found"),
            BridgeError::ConversationNotFound(_) => {
                (StatusCode::NOT_FOUND, "conversation_not_found")
            }
            BridgeError::ConversationEnded(_) => (StatusCode::BAD_REQUEST, "conversation_ended"),
            BridgeError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            BridgeError::ProviderError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "provider_error"),
            BridgeError::McpError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "mcp_error"),
            BridgeError::ToolError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "tool_error"),
            BridgeError::ConfigError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "config_error"),
            BridgeError::WebhookError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "webhook_error"),
            BridgeError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
            BridgeError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            BridgeError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            BridgeError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            BridgeError::CapacityExhausted(_) => {
                (StatusCode::TOO_MANY_REQUESTS, "capacity_exhausted")
            }
        };

        let message = match &self {
            BridgeError::ProviderError(msg) => {
                tracing::error!(error = %msg, "provider_error_response");
                format!("provider error: {}", sanitize_for_response(msg))
            }
            BridgeError::McpError(msg) => {
                tracing::error!(error = %msg, "mcp_error_response");
                format!("mcp error: {}", sanitize_for_response(msg))
            }
            BridgeError::ToolError(msg) => {
                tracing::error!(error = %msg, "tool_error_response");
                format!("tool error: {}", sanitize_for_response(msg))
            }
            _ => self.to_string(),
        };

        let body = serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        });

        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_for_each_variant() {
        assert_eq!(
            BridgeError::AgentNotFound("a1".into()).to_string(),
            "agent not found: a1"
        );
        assert_eq!(
            BridgeError::ConversationNotFound("c1".into()).to_string(),
            "conversation not found: c1"
        );
        assert_eq!(
            BridgeError::ConversationEnded("c1".into()).to_string(),
            "conversation ended: c1"
        );
        assert_eq!(
            BridgeError::InvalidRequest("bad".into()).to_string(),
            "invalid request: bad"
        );
        assert_eq!(
            BridgeError::ProviderError("fail".into()).to_string(),
            "provider error: fail"
        );
        assert_eq!(
            BridgeError::McpError("fail".into()).to_string(),
            "mcp error: fail"
        );
        assert_eq!(
            BridgeError::ToolError("fail".into()).to_string(),
            "tool error: fail"
        );
        assert_eq!(
            BridgeError::ConfigError("fail".into()).to_string(),
            "config error: fail"
        );
        assert_eq!(
            BridgeError::WebhookError("fail".into()).to_string(),
            "webhook error: fail"
        );
        assert_eq!(
            BridgeError::Internal("fail".into()).to_string(),
            "internal error: fail"
        );
        assert_eq!(BridgeError::RateLimited.to_string(), "rate limited");
        assert_eq!(
            BridgeError::Unauthorized("bad token".into()).to_string(),
            "unauthorized: bad token"
        );
        assert_eq!(
            BridgeError::Conflict("active conversations".into()).to_string(),
            "conflict: active conversations"
        );
        assert_eq!(
            BridgeError::CapacityExhausted("max conversations reached".into()).to_string(),
            "capacity exhausted: max conversations reached"
        );
    }

    #[test]
    fn test_capacity_exhausted_returns_429() {
        let err = BridgeError::CapacityExhausted("max conversations".into());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_sanitize_redacts_sk_tokens() {
        let out = sanitize_for_response("leaked sk-1234ABCD_efgh-zzz in the log");
        assert!(!out.contains("sk-1234"));
        assert!(out.contains("sk-***"));
    }

    #[test]
    fn test_sanitize_redacts_bearer_tokens() {
        let out = sanitize_for_response("Authorization: Bearer abcdef.ghijkl");
        assert!(!out.contains("abcdef"));
        assert!(out.contains("Bearer ***"));
    }

    #[test]
    fn test_sanitize_redacts_api_key_assignments() {
        let out = sanitize_for_response("config api_key=\"supersecret\" done");
        assert!(!out.contains("supersecret"));
        assert!(out.contains("api_key=***"));

        let out2 = sanitize_for_response("config Api-Key: superSecret done");
        assert!(!out2.contains("superSecret"));
        assert!(out2.contains("api_key=***"));
    }

    #[test]
    fn test_sanitize_truncates_to_512_bytes_utf8_safe() {
        let giant = "é".repeat(1000);
        let out = sanitize_for_response(&giant);
        assert!(out.len() <= SANITIZE_MAX_BYTES);
        // Ensures no half-char slicing
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn test_sanitize_short_string_passes_through() {
        let out = sanitize_for_response("simple error");
        assert_eq!(out, "simple error");
    }
}
