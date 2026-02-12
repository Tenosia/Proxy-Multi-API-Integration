//! Request/response translation between Anthropic Messages API and OpenAI chat completions.

use crate::config::Config;
use crate::error::{ProxyError, ProxyResult};
use crate::models::{anthropic, openai};
use serde_json::{json, Value};

/// Picks the model name: reasoning vs completion from config or request.
fn select_model(config: &Config, req: &anthropic::AnthropicRequest, has_thinking: bool) -> String {
    let fallback = || req.model.clone();
    if has_thinking {
        config
            .reasoning_model
            .clone()
            .unwrap_or_else(fallback)
    } else {
        config
            .completion_model
            .clone()
            .unwrap_or_else(fallback)
    }
}

/// Returns true if the request has extended thinking enabled (e.g. thinking.type == "enabled").
fn has_thinking_enabled(extra: &Value) -> bool {
    extra
        .get("thinking")
        .and_then(|v| v.as_object())
        .map(|o| o.get("type").and_then(|t| t.as_str()) == Some("enabled"))
        .unwrap_or(false)
}

/// Converts an Anthropic request into an OpenAI chat completions request.
pub fn anthropic_to_openai(
    req: anthropic::AnthropicRequest,
    config: &Config,
) -> ProxyResult<openai::OpenAIRequest> {
    let has_thinking = has_thinking_enabled(&req.extra);
    let model = select_model(config, &req, has_thinking);

    let mut openai_messages = Vec::new();

    if let Some(system) = req.system {
        match system {
            anthropic::SystemPrompt::Single(text) => {
                openai_messages.push(openai_message("system", Some(openai::MessageContent::Text(text)), None, None));
            }
            anthropic::SystemPrompt::Multiple(messages) => {
                for msg in messages {
                    openai_messages.push(openai_message(
                        "system",
                        Some(openai::MessageContent::Text(msg.text)),
                        None,
                        None,
                    ));
                }
            }
        }
    }

    for msg in req.messages {
        openai_messages.extend(convert_message(msg)?);
    }

    let tools = req.tools.and_then(|tools| {
        let filtered: Vec<_> = tools
            .into_iter()
            .filter(|t| t.tool_type.as_deref() != Some("BatchTool"))
            .collect();
        if filtered.is_empty() {
            None
        } else {
            Some(
                filtered
                    .into_iter()
                    .map(|t| openai::Tool {
                        tool_type: "function".to_string(),
                        function: openai::Function {
                            name: t.name,
                            description: t.description,
                            parameters: clean_schema(t.input_schema),
                        },
                    })
                    .collect(),
            )
        }
    });

    Ok(openai::OpenAIRequest {
        model,
        messages: openai_messages,
        max_tokens: Some(req.max_tokens),
        temperature: req.temperature,
        top_p: req.top_p,
        stop: req.stop_sequences,
        stream: req.stream,
        tools,
        tool_choice: None,
    })
}

fn openai_message(
    role: &str,
    content: Option<openai::MessageContent>,
    tool_calls: Option<Vec<openai::ToolCall>>,
    tool_call_id: Option<String>,
) -> openai::Message {
    openai::Message {
        role: role.to_string(),
        content,
        tool_calls,
        tool_call_id,
        name: None,
    }
}

/// Converts one Anthropic message into one or more OpenAI messages.
fn convert_message(msg: anthropic::Message) -> ProxyResult<Vec<openai::Message>> {
    let mut result = Vec::new();

    match msg.content {
        anthropic::MessageContent::Text(text) => {
            result.push(openai_message(
                &msg.role,
                Some(openai::MessageContent::Text(text)),
                None,
                None,
            ));
        }
        anthropic::MessageContent::Blocks(blocks) => {
            let mut current_content_parts = Vec::new();
            let mut tool_calls = Vec::new();

            for block in blocks {
                match block {
                    anthropic::ContentBlock::Text { text, .. } => {
                        current_content_parts.push(openai::ContentPart::Text { text });
                    }
                    anthropic::ContentBlock::Image { source } => {
                        let data_url = format!("data:{};base64,{}", source.media_type, source.data);
                        current_content_parts.push(openai::ContentPart::ImageUrl {
                            image_url: openai::ImageUrl { url: data_url },
                        });
                    }
                    anthropic::ContentBlock::ToolUse { id, name, input } => {
                        let args = serde_json::to_string(&input).map_err(ProxyError::from)?;
                        tool_calls.push(openai::ToolCall {
                            id,
                            call_type: "function".to_string(),
                            function: openai::FunctionCall { name, arguments: args },
                        });
                    }
                    anthropic::ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        result.push(openai_message(
                            "tool",
                            Some(openai::MessageContent::Text(content)),
                            None,
                            Some(tool_use_id),
                        ));
                    }
                    anthropic::ContentBlock::Thinking { .. } => {}
                }
            }

            if !current_content_parts.is_empty() || !tool_calls.is_empty() {
                let content = if current_content_parts.is_empty() {
                    None
                } else if current_content_parts.len() == 1 {
                    match &current_content_parts[0] {
                        openai::ContentPart::Text { text } => {
                            Some(openai::MessageContent::Text(text.clone()))
                        }
                        _ => Some(openai::MessageContent::Parts(current_content_parts)),
                    }
                } else {
                    Some(openai::MessageContent::Parts(current_content_parts))
                };

                result.push(openai_message(&msg.role, content, Some(tool_calls).filter(|t| !t.is_empty()), None));
            }
        }
    }

    Ok(result)
}

/// Removes JSON schema fields that some OpenAI-compatible backends reject (e.g. "format": "uri").
fn clean_schema(mut schema: Value) -> Value {
    if let Some(obj) = schema.as_object_mut() {
        if obj.get("format").and_then(|v| v.as_str()) == Some("uri") {
            obj.remove("format");
        }
        if let Some(properties) = obj.get_mut("properties").and_then(|v| v.as_object_mut()) {
            for (_, value) in properties.iter_mut() {
                *value = clean_schema(value.clone());
            }
        }
        if let Some(items) = obj.get_mut("items") {
            *items = clean_schema(items.clone());
        }
    }
    schema
}

/// Converts an OpenAI chat completions response into Anthropic message format.
pub fn openai_to_anthropic(
    resp: openai::OpenAIResponse,
) -> ProxyResult<anthropic::AnthropicResponse> {
    let choice = resp
        .choices
        .first()
        .ok_or_else(|| ProxyError::Transform("No choices in response".to_string()))?;

    let mut content = Vec::new();

    if let Some(text) = &choice.message.content {
        if !text.is_empty() {
            content.push(anthropic::ResponseContent::Text {
                content_type: "text".to_string(),
                text: text.clone(),
            });
        }
    }

    if let Some(tool_calls) = &choice.message.tool_calls {
        for tool_call in tool_calls {
            let input: Value = serde_json::from_str(&tool_call.function.arguments)
                .unwrap_or_else(|_| json!({}));
            content.push(anthropic::ResponseContent::ToolUse {
                content_type: "tool_use".to_string(),
                id: tool_call.id.clone(),
                name: tool_call.function.name.clone(),
                input,
            });
        }
    }

    let stop_reason = choice
        .finish_reason
        .as_ref()
        .and_then(|r| map_stop_reason(Some(r)));

    Ok(anthropic::AnthropicResponse {
        id: resp.id,
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content,
        model: resp.model,
        stop_reason,
        stop_sequence: None,
        usage: anthropic::Usage {
            input_tokens: resp.usage.prompt_tokens,
            output_tokens: resp.usage.completion_tokens,
        },
    })
}

/// Maps OpenAI finish_reason to Anthropic stop_reason.
pub fn map_stop_reason(finish_reason: Option<&str>) -> Option<String> {
    finish_reason.map(|r| match r {
        "tool_calls" => "tool_use",
        "stop" => "end_turn",
        "length" => "max_tokens",
        _ => "end_turn",
    }.to_string())
}
