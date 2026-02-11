//! Proxy error types and HTTP response mapping.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

/// Application-specific errors for the Anthropic proxy.
#[derive(Error, Debug)]
pub enum ProxyError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Request transformation error: {0}")]
    Transform(String),

    #[error("Upstream API error: {0}")]
    Upstream(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ProxyError::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            ProxyError::Transform(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            ProxyError::Upstream(msg) => (StatusCode::BAD_GATEWAY, msg.clone()),
            ProxyError::Serialization(e) => (StatusCode::BAD_REQUEST, format!("JSON error: {e}")),
            ProxyError::Http(e) => (StatusCode::BAD_GATEWAY, format!("HTTP error: {e}")),
            ProxyError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
        };

        let body = Json(json!({
            "error": {
                "type": "proxy_error",
                "message": message,
            }
        }));

        (status, body).into_response()
    }
}

/// Result type for proxy operations.
pub type ProxyResult<T> = Result<T, ProxyError>;
