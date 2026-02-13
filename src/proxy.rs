//! HTTP handler and streaming: accept Anthropic requests, call upstream, return Anthropic responses.

use crate::config::Config;
use crate::error::{ProxyError, ProxyResult};
use crate::models::{anthropic, openai};
use crate::transform;
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    Extension, Json,
};
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use reqwest::Client;
use serde_json::json;
use std::sync::OnceLock;
use std::sync::Arc;
use std::time::Duration;

const UPSTREAM_TIMEOUT_SECS: u64 = 300;

/// SSE headers built once for streaming responses.
static SSE_HEADERS: OnceLock<HeaderMap> = OnceLock::new();

fn sse_header_map() -> &'static HeaderMap {
    SSE_HEADERS.get_or_init(|| {
        let mut h = HeaderMap::new();
        h.insert("Content-Type", HeaderValue::from_static("text/event-stream"));
        h.insert("Cache-Control", HeaderValue::from_static("no-cache"));
        h.insert("Connection", HeaderValue::from_static("keep-alive"));
        h
    })
}

/// Entrypoint: parse Anthropic request, transform to OpenAI, call upstream, transform response.
pub async fn proxy_handler(
    Extension(config): Extension<Arc<Config>>,
    Extension(client): Extension<Client>,
    Json(req): Json<anthropic::AnthropicRequest>,
) -> ProxyResult<Response> {
    let is_streaming = req.stream.unwrap_or(false);
    tracing::debug!("Received request model={} streaming={}", req.model, is_streaming);

    if config.verbose {
        let _ = tracing::trace!(
            "Incoming Anthropic request: {}",
            serde_json::to_string_pretty(&req).unwrap_or_default()
        );
    }

    let openai_req = transform::anthropic_to_openai(req, &config)?;

    if config.verbose {
        let _ = tracing::trace!(
            "Transformed OpenAI request: {}",
            serde_json::to_string_pretty(&openai_req).unwrap_or_default()
        );
    }

    if is_streaming {
        handle_streaming(config, client, openai_req).await
    } else {
        handle_non_streaming(config, client, openai_req).await
    }
}

/// Build POST request to upstream chat completions with optional auth and timeout.
fn build_upstream_request(
    client: &Client,
    url: &str,
    api_key: Option<&String>,
    body: &openai::OpenAIRequest,
) -> reqwest::RequestBuilder {
    let mut builder = client
        .post(url)
        .json(body)
        .timeout(Duration::from_secs(UPSTREAM_TIMEOUT_SECS));
    if let Some(key) = api_key {
        builder = builder.header("Authorization", format!("Bearer {key}"));
    }
    builder
}

/// Ensure response is success; otherwise read body and return `ProxyError::Upstream`.
async fn require_success(mut response: reqwest::Response) -> ProxyResult<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "Unknown error".to_string());
    tracing::error!("Upstream error ({}): {}", status, body);
    Err(ProxyError::Upstream(format!("Upstream returned {status}: {body}")))
}

async fn handle_non_streaming(
    config: Arc<Config>,
    client: Client,
    openai_req: openai::OpenAIRequest,
) -> ProxyResult<Response> {
    let url = config.chat_completions_url();
    tracing::debug!("Non-streaming request to {} model={}", url, openai_req.model);

    let response = build_upstream_request(
        &client,
        &url,
        config.api_key.as_ref(),
        &openai_req,
    )
    .send()
    .await?;

    let response = require_success(response).await?;
    let openai_resp: openai::OpenAIResponse = response.json().await?;

    if config.verbose {
        let _ = tracing::trace!(
            "OpenAI response: {}",
            serde_json::to_string_pretty(&openai_resp).unwrap_or_default()
        );
    }

    let anthropic_resp = transform::openai_to_anthropic(openai_resp)?;

    if config.verbose {
        let _ = tracing::trace!(
            "Anthropic response: {}",
            serde_json::to_string_pretty(&anthropic_resp).unwrap_or_default()
        );
    }

    Ok(Json(anthropic_resp).into_response())
}

async fn handle_streaming(
    config: Arc<Config>,
    client: Client,
    openai_req: openai::OpenAIRequest,
) -> ProxyResult<Response> {
    let url = config.chat_completions_url();
    tracing::debug!("Streaming request to {} model={}", url, openai_req.model);

    let response = build_upstream_request(
        &client,
        &url,
        config.api_key.as_ref(),
        &openai_req,
    )
    .send()
    .await?;

    let response = require_success(response).await?;
    let stream = response.bytes_stream();
    let sse_stream = create_sse_stream(stream);

    Ok((sse_header_map().clone(), Body::from_stream(sse_stream)).into_response())
}

#[inline]
fn sse_event(event: &str, data: &str) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

fn create_sse_stream(
    stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut message_id = None;
        let mut current_model = None;
        let mut content_index = 0;
        let mut tool_call_id = None;
        let mut has_sent_message_start = false;
        let mut current_block_type: Option<String> = None;

        tokio::pin!(stream);

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));

                    while let Some(pos) = buffer.find("\n\n") {
                        let line = buffer[..pos].to_string();
                        buffer.drain(..=pos + 1);

                        if line.trim().is_empty() {
                            continue;
                        }

                        for l in line.lines() {
                            let Some(data) = l.strip_prefix("data: ") else { continue };
                            if data.trim() == "[DONE]" {
                                let data = serde_json::to_string(&json!({"type": "message_stop"})).unwrap_or_default();
                                yield Ok(sse_event("message_stop", &data));
                                continue;
                            }

                            let Ok(chunk) = serde_json::from_str::<openai::StreamChunk>(data) else { continue };
                            if message_id.is_none() {
                                message_id = Some(chunk.id.clone());
                            }
                            if current_model.is_none() {
                                current_model = Some(chunk.model.clone());
                            }

                            let Some(choice) = chunk.choices.first() else { continue };

                            if !has_sent_message_start {
                                let msg = anthropic::StreamEvent::MessageStart {
                                    message: anthropic::MessageStartData {
                                        id: message_id.clone().unwrap_or_default(),
                                        message_type: "message".to_string(),
                                        role: "assistant".to_string(),
                                        model: current_model.clone().unwrap_or_default(),
                                        usage: anthropic::Usage {
                                            input_tokens: 0,
                                            output_tokens: 0,
                                        },
                                    },
                                };
                                let data = serde_json::to_string(&msg).unwrap_or_default();
                                yield Ok(sse_event("message_start", &data));
                                has_sent_message_start = true;
                            }

                            if let Some(reasoning) = &choice.delta.reasoning {
                                if current_block_type.is_none() {
                                    let event = json!({
                                        "type": "content_block_start",
                                        "index": content_index,
                                        "content_block": { "type": "thinking", "thinking": "" }
                                    });
                                    let data = serde_json::to_string(&event).unwrap_or_default();
                                    yield Ok(sse_event("content_block_start", &data));
                                    current_block_type = Some("thinking".to_string());
                                }
                                let event = json!({
                                    "type": "content_block_delta",
                                    "index": content_index,
                                    "delta": { "type": "thinking_delta", "thinking": reasoning }
                                });
                                let data = serde_json::to_string(&event).unwrap_or_default();
                                yield Ok(sse_event("content_block_delta", &data));
                            }

                            if let Some(content) = &choice.delta.content {
                                if !content.is_empty() {
                                    if current_block_type.as_deref() != Some("text") {
                                        if current_block_type.is_some() {
                                            let event = json!({"type": "content_block_stop", "index": content_index});
                                            let data = serde_json::to_string(&event).unwrap_or_default();
                                            yield Ok(sse_event("content_block_stop", &data));
                                            content_index += 1;
                                        }
                                        let event = json!({
                                            "type": "content_block_start",
                                            "index": content_index,
                                            "content_block": { "type": "text", "text": "" }
                                        });
                                        let data = serde_json::to_string(&event).unwrap_or_default();
                                        yield Ok(sse_event("content_block_start", &data));
                                        current_block_type = Some("text".to_string());
                                    }
                                    let event = json!({
                                        "type": "content_block_delta",
                                        "index": content_index,
                                        "delta": { "type": "text_delta", "text": content }
                                    });
                                    let data = serde_json::to_string(&event).unwrap_or_default();
                                    yield Ok(sse_event("content_block_delta", &data));
                                }
                            }

                            if let Some(tool_calls) = &choice.delta.tool_calls {
                                for tool_call in tool_calls {
                                    if let Some(id) = &tool_call.id {
                                        if current_block_type.is_some() {
                                            let event = json!({"type": "content_block_stop", "index": content_index});
                                            let data = serde_json::to_string(&event).unwrap_or_default();
                                            yield Ok(sse_event("content_block_stop", &data));
                                            content_index += 1;
                                        }
                                        tool_call_id = Some(id.clone());
                                    }
                                    if let Some(function) = &tool_call.function {
                                        if let Some(name) = &function.name {
                                            let event = json!({
                                                "type": "content_block_start",
                                                "index": content_index,
                                                "content_block": {
                                                    "type": "tool_use",
                                                    "id": tool_call_id.clone().unwrap_or_default(),
                                                    "name": name
                                                }
                                            });
                                            let data = serde_json::to_string(&event).unwrap_or_default();
                                            yield Ok(sse_event("content_block_start", &data));
                                            current_block_type = Some("tool_use".to_string());
                                        }
                                        if let Some(args) = &function.arguments {
                                            let event = json!({
                                                "type": "content_block_delta",
                                                "index": content_index,
                                                "delta": { "type": "input_json_delta", "partial_json": args }
                                            });
                                            let data = serde_json::to_string(&event).unwrap_or_default();
                                            yield Ok(sse_event("content_block_delta", &data));
                                        }
                                    }
                                }
                            }

                            if let Some(finish_reason) = &choice.finish_reason {
                                if current_block_type.is_some() {
                                    let event = json!({"type": "content_block_stop", "index": content_index});
                                    let data = serde_json::to_string(&event).unwrap_or_default();
                                    yield Ok(sse_event("content_block_stop", &data));
                                }
                                let stop_reason = transform::map_stop_reason(Some(finish_reason));
                                let event = json!({
                                    "type": "message_delta",
                                    "delta": { "stop_reason": stop_reason, "stop_sequence": serde_json::Value::Null },
                                    "usage": chunk.usage.as_ref().map(|u| json!({ "output_tokens": u.completion_tokens }))
                                });
                                let data = serde_json::to_string(&event).unwrap_or_default();
                                yield Ok(sse_event("message_delta", &data));
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Stream error: {}", e);
                    let error_event = json!({
                        "type": "error",
                        "error": { "type": "stream_error", "message": format!("Stream error: {e}") }
                    });
                    let data = serde_json::to_string(&error_event).unwrap_or_default();
                    yield Ok(sse_event("error", &data));
                    break;
                }
            }
        }
    }
}
