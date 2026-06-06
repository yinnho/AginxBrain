use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn chrono_like_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn codex_response_id_from_chat_id(id: Option<&str>) -> String {
    let id = id.unwrap_or("modelrouter");
    if id.starts_with("resp_") { id.to_string() } else { format!("resp_{id}") }
}

fn codex_chat_usage_to_responses_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage.filter(|v| v.is_object() && !v.is_null()) else {
        return json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0});
    };
    let input = usage.get("prompt_tokens").or_else(|| usage.get("input_tokens"))
        .and_then(|v| v.as_u64()).unwrap_or(0);
    let output = usage.get("completion_tokens").or_else(|| usage.get("output_tokens"))
        .and_then(|v| v.as_u64()).unwrap_or(0);
    let total = usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(input + output);
    json!({"input_tokens": input, "output_tokens": output, "total_tokens": total})
}

fn codex_chat_reasoning_text(message: &Value) -> Option<String> {
    for key in &["reasoning_content", "reasoning"] {
        if let Some(text) = message.get(*key).and_then(|v| v.as_str()) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    // Check for inline <think...</think...> blocks
    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
        if let Some((reasoning, _)) = codex_split_leading_think_block(content) {
            if !reasoning.is_empty() {
                return Some(reasoning);
            }
        }
    }
    None
}

fn codex_split_leading_think_block(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("<think") {
        return None;
    }
    let open_idx = text.find("<think")?;
    let after_open = &text[open_idx + 6..]; // skip "<think"
    // Handle both <think > and <think\n
    let after_tag = if after_open.starts_with('>') {
        &after_open[1..]
    } else {
        after_open.trim_start_matches(|c: char| !c.is_whitespace())
            .trim_start()
    };
    let close_idx = after_tag.find("</think")?;
    let reasoning = after_tag[..close_idx].trim().to_string();
    let after_close = &after_tag[close_idx + 7..]; // skip "</think"
    // Skip optional whitespace and the closing '>'
    let rest = after_close.trim_start();
    let rest = if rest.starts_with('>') {
        &rest[1..]
    } else {
        rest
    };
    let answer = rest.trim_start_matches(['\r', '\n', '\t', ' ']).to_string();
    Some((reasoning, answer))
}

// ---------------------------------------------------------------------------
// OpenAI Chat → Anthropic: Non-streaming response conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI Chat Completions response to Anthropic Messages response format.
pub fn openai_to_anthropic_response(body: &Value, request_model: &str) -> Value {
    let mut content: Vec<Value> = Vec::new();

    if let Some(choices) = body.get("choices").and_then(|c| c.as_array()) {
        if let Some(choice) = choices.first() {
            let message = choice.get("message");

            // Reasoning content → thinking block (must come first in content)
            if let Some(reasoning) = message.and_then(|m| m.get("reasoning_content")).and_then(|r| r.as_str()) {
                if !reasoning.is_empty() {
                    content.push(json!({
                        "type": "thinking",
                        "thinking": reasoning
                    }));
                }
            }

            // Text content
            if let Some(text) = message.and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    content.push(json!({
                        "type": "text",
                        "text": text
                    }));
                }
            }

            // Tool calls → tool_use blocks
            if let Some(tool_calls) = message.and_then(|m| m.get("tool_calls")).and_then(|t| t.as_array()) {
                for (idx, tc) in tool_calls.iter().enumerate() {
                    let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = tc.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    let args_str = tc.get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}");
                    let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

                    content.push(json!({
                        "type": "tool_use",
                        "id": if id.is_empty() { format!("toolu_{}", idx) } else { id.to_string() },
                        "name": name,
                        "input": input
                    }));
                }
            }
        }
    }

    // If no content was extracted, add an empty text block
    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }

    // Map finish_reason
    let stop_reason = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("finish_reason"))
        .and_then(|f| f.as_str())
        .map(|r| match r {
            "stop" => "end_turn",
            "tool_calls" => "tool_use",
            "length" => "max_tokens",
            _ => "end_turn",
        })
        .unwrap_or("end_turn");

    // Map usage
    let usage = body.get("usage").map(|u| {
        json!({
            "input_tokens": u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "output_tokens": u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0
        })
    }).unwrap_or(json!({
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0
    }));

    json!({
        "id": body.get("id").and_then(|v| v.as_str()).unwrap_or("msg_unknown"),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": request_model,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": usage
    })
}

// ---------------------------------------------------------------------------
// Anthropic Messages → OpenAI Chat: Non-streaming response conversion
// ---------------------------------------------------------------------------

/// Convert an Anthropic Messages response to OpenAI Chat Completions response format.
pub fn anthropic_to_openai_response(body: &Value, request_model: &str) -> Value {
    let mut content = String::new();
    let mut reasoning: Option<String> = None;
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(blocks) = body.get("content").and_then(|c| c.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("thinking") => {
                    if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                        reasoning = Some(reasoning.unwrap_or_default() + text);
                    }
                }
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        content.push_str(text);
                    }
                }
                Some("tool_use") => {
                    let tc_id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let input = block.get("input").unwrap_or(&Value::Null);
                    let args_str = if input.is_object() {
                        serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
                    } else if let Some(s) = input.as_str() {
                        s.to_string()
                    } else {
                        "{}".to_string()
                    };
                    tool_calls.push(json!({
                        "id": tc_id,
                        "type": "function",
                        "function": {"name": name, "arguments": args_str}
                    }));
                }
                _ => {}
            }
        }
    }

    // Build message
    let mut message = serde_json::Map::new();
    message.insert("role".into(), Value::String("assistant".into()));
    message.insert("content".into(), Value::String(content));
    if let Some(r) = reasoning {
        message.insert("reasoning_content".into(), Value::String(r));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    // Map stop_reason
    let finish_reason = match body.get("stop_reason").and_then(|v| v.as_str()) {
        Some("end_turn") | Some("stop_sequence") => "stop",
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        _ => "stop",
    };

    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("msg_unknown");

    // Usage mapping
    let usage = body.get("usage").map(|u| {
        let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        json!({
            "prompt_tokens": input,
            "completion_tokens": output,
            "total_tokens": input + output
        })
    }).unwrap_or_else(|| json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}));

    json!({
        "id": id,
        "object": "chat.completion",
        "created": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u64,
        "model": request_model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }],
        "usage": usage
    })
}

// ---------------------------------------------------------------------------
// OpenAI Responses → Anthropic: Non-streaming response conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI Responses API response to Anthropic Messages response format.
pub fn responses_to_anthropic_response(body: &Value, request_model: &str) -> Value {
    let mut content: Vec<Value> = Vec::new();

    if let Some(output) = body.get("output").and_then(|o| o.as_array()) {
        for item in output {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                        for part in parts {
                            match part.get("type").and_then(|t| t.as_str()) {
                                Some("output_text") => {
                                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                        if !text.is_empty() {
                                            content.push(json!({
                                                "type": "text",
                                                "text": text
                                            }));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args_str = item.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                    let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

                    content.push(json!({
                        "type": "tool_use",
                        "id": if call_id.is_empty() { "call_0".to_string() } else { call_id.to_string() },
                        "name": name,
                        "input": input
                    }));
                }
                _ => {}
            }
        }
    }

    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }

    // Map status → stop_reason
    let stop_reason = body
        .get("status")
        .and_then(|s| s.as_str())
        .map(|s| match s {
            "completed" => "end_turn",
            "incomplete" => "max_tokens",
            "failed" => "end_turn",
            _ => "end_turn",
        })
        .unwrap_or("end_turn");

    // Check if there are function_call outputs → tool_use stop reason
    let has_tool_calls = body
        .get("output")
        .and_then(|o| o.as_array())
        .map(|arr| arr.iter().any(|item| item.get("type").and_then(|t| t.as_str()) == Some("function_call")))
        .unwrap_or(false);
    let stop_reason = if has_tool_calls { "tool_use" } else { stop_reason };

    // Map usage
    let usage = body.get("usage").map(|u| {
        json!({
            "input_tokens": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "output_tokens": u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0
        })
    }).unwrap_or(json!({
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0
    }));

    json!({
        "id": body.get("id").and_then(|v| v.as_str()).unwrap_or("msg_unknown"),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": request_model,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": usage
    })
}

// ---------------------------------------------------------------------------
// OpenAI Responses → OpenAI Chat: Response conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI Responses API response to OpenAI Chat Completions response format.
pub fn responses_to_openai_response(body: &Value, request_model: &str) -> Value {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(output) = body.get("output").and_then(|o| o.as_array()) {
        for item in output {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                        for part in parts {
                            if part.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    text_parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args = item.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");

                    tool_calls.push(json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": args
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let content_text = text_parts.join("");
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else {
        body.get("status").and_then(|s| s.as_str()).map(|s| match s {
            "completed" => "stop",
            "incomplete" => "length",
            _ => "stop",
        }).unwrap_or("stop")
    };

    let mut message = json!({
        "role": "assistant",
        "content": if content_text.is_empty() { Value::Null } else { Value::String(content_text) }
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    json!({
        "id": body.get("id").and_then(|v| v.as_str()).unwrap_or("chatcmpl_unknown"),
        "object": "chat.completion",
        "model": request_model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }],
        "usage": {
            "prompt_tokens": body.get("usage").and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64()).unwrap_or(0),
            "completion_tokens": body.get("usage").and_then(|u| u.get("output_tokens")).and_then(|v| v.as_u64()).unwrap_or(0),
            "total_tokens": body.get("usage").and_then(|u| u.get("total_tokens")).and_then(|v| v.as_u64()).unwrap_or(0)
        }
    })
}

// ---------------------------------------------------------------------------
// Codex: Chat Completions → Responses API response conversion (non-streaming)
// ---------------------------------------------------------------------------

/// Convert a non-streaming Chat Completions response to Responses API format.
pub fn chat_to_responses_response(body: &Value, request_model: &str) -> Value {
    let choices = body.get("choices").and_then(|v| v.as_array());
    let choice = choices.and_then(|c| c.first());
    let message = choice.and_then(|c| c.get("message"));

    let response_id = codex_response_id_from_chat_id(body.get("id").and_then(|v| v.as_str()));
    let model = request_model;
    let created_at = body.get("created").and_then(|v| v.as_u64()).unwrap_or(0);
    let finish_reason = choice.and_then(|c| c.get("finish_reason").and_then(|v| v.as_str()));

    let mut output = Vec::new();

    if let Some(msg) = message {
        // Reasoning item
        if let Some(reasoning) = codex_chat_reasoning_text(msg) {
            if !reasoning.is_empty() {
                output.push(json!({
                    "id": format!("rs_{}", response_id),
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": reasoning}]
                }));
            }
        }

        // Message item
        if let Some(text) = msg.get("content").and_then(|v| v.as_str()) {
            let text = codex_split_leading_think_block(text)
                .map(|(_, answer)| answer)
                .unwrap_or_else(|| text.to_string());
            if !text.is_empty() {
                output.push(json!({
                    "id": format!("{}_msg", response_id),
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text, "annotations": []}]
                }));
            }
        }

        // Function call items
        if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for (index, tc) in tool_calls.iter().enumerate() {
                let call_id = tc.get("id").and_then(|v| v.as_str()).filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "");
                let function = tc.get("function").unwrap_or(&Value::Null);
                let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = match function.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => serde_json::to_string(v).unwrap_or_default(),
                    None => "{}".to_string(),
                };
                let id = if call_id.is_empty() { format!("call_{index}") } else { call_id.to_string() };
                output.push(json!({
                    "id": format!("fc_{}", id),
                    "type": "function_call",
                    "status": "completed",
                    "call_id": id,
                    "name": name,
                    "arguments": arguments
                }));
            }
        }
    }

    let status = match finish_reason {
        Some("length") => "incomplete",
        _ => "completed",
    };

    let usage = codex_chat_usage_to_responses_usage(body.get("usage"));

    let mut response = json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": status,
        "model": model,
        "output": output,
        "usage": usage
    });

    if finish_reason == Some("length") {
        response["incomplete_details"] = json!({"reason": "max_output_tokens"});
    }

    response
}

// ---------------------------------------------------------------------------
// Codex: Anthropic Messages → Responses API response conversion (non-streaming)
// ---------------------------------------------------------------------------

/// Convert a non-streaming Anthropic Messages response to Responses API format.
pub fn anthropic_to_responses_response(body: &Value, request_model: &str) -> Value {
    let model = request_model;
    let response_id = format!("resp_{}", chrono_like_id());

    let mut output_items = Vec::new();
    let mut output_text = String::new();

    if let Some(content) = body.get("content").and_then(|v| v.as_array()) {
        for block in content {
            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match block_type {
                "thinking" => {
                    let thinking = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                    let item_id = format!("rs_{}", chrono_like_id());
                    output_items.push(json!({
                        "id": item_id, "type": "reasoning", "status": "completed",
                        "summary": [], "content": [{"type": "reasoning_text", "text": thinking}]
                    }));
                }
                "text" => {
                    let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    output_text.push_str(text);
                    let item_id = format!("msg_{}", chrono_like_id());
                    output_items.push(json!({
                        "id": item_id, "type": "message", "status": "completed",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text, "annotations": []}]
                    }));
                }
                "tool_use" => {
                    let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or(json!({}));
                    let args = serde_json::to_string(&input).unwrap_or_default();
                    output_items.push(json!({
                        "id": id, "type": "function_call", "call_id": id,
                        "name": name, "arguments": args, "status": "completed"
                    }));
                }
                _ => {}
            }
        }
    }

    let usage = body.get("usage").map(|u| {
        let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        json!({"input_tokens": input, "output_tokens": output, "total_tokens": input + output})
    }).unwrap_or_else(|| json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}));

    json!({
        "id": response_id,
        "object": "response",
        "created_at": chrono_like_id(),
        "status": "completed",
        "model": model,
        "output": output_items,
        "output_text": output_text,
        "usage": usage
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_openai_to_anthropic_simple_response() {
        let body = json!({
            "id": "chatcmpl-123",
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let result = openai_to_anthropic_response(&body, "claude-sonnet-4-6");

        assert_eq!(result["type"], "message");
        assert_eq!(result["role"], "assistant");
        assert_eq!(result["stop_reason"], "end_turn");
        assert_eq!(result["model"], "claude-sonnet-4-6");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "Hello!");
        assert_eq!(result["usage"]["input_tokens"], 10);
        assert_eq!(result["usage"]["output_tokens"], 5);
    }

    #[test]
    fn test_openai_to_anthropic_tool_calls_response() {
        let body = json!({
            "id": "chatcmpl-456",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 10}
        });
        let result = openai_to_anthropic_response(&body, "claude-sonnet-4-6");

        assert_eq!(result["stop_reason"], "tool_use");
        let content = result["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "call_1");
        assert_eq!(content[0]["name"], "bash");
        assert_eq!(content[0]["input"]["cmd"], "ls");
    }

    #[test]
    fn test_responses_to_anthropic_text_response() {
        let body = json!({
            "id": "resp_123",
            "object": "response",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "Hello!"}
                    ]
                }
            ],
            "status": "completed",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let result = responses_to_anthropic_response(&body, "claude-sonnet-4-6");

        assert_eq!(result["type"], "message");
        assert_eq!(result["role"], "assistant");
        assert_eq!(result["stop_reason"], "end_turn");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "Hello!");
        assert_eq!(result["usage"]["input_tokens"], 10);
        assert_eq!(result["usage"]["output_tokens"], 5);
    }

    #[test]
    fn test_responses_to_anthropic_function_call_response() {
        let body = json!({
            "id": "resp_456",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "Let me check."}
                    ]
                },
                {
                    "type": "function_call",
                    "call_id": "call_abc",
                    "name": "bash",
                    "arguments": "{\"cmd\":\"ls\"}"
                }
            ],
            "status": "completed",
            "usage": {"input_tokens": 20, "output_tokens": 10}
        });
        let result = responses_to_anthropic_response(&body, "claude-sonnet-4-6");

        assert_eq!(result["stop_reason"], "tool_use");
        let content = result["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Let me check.");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "call_abc");
        assert_eq!(content[1]["name"], "bash");
        assert_eq!(content[1]["input"]["cmd"], "ls");
    }

    #[test]
    fn test_responses_to_openai_text_response() {
        let body = json!({
            "id": "resp_123",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "Hello!"}
                    ]
                }
            ],
            "status": "completed",
            "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
        });
        let result = responses_to_openai_response(&body, "gpt-4o");

        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["model"], "gpt-4o");
        assert_eq!(result["choices"][0]["message"]["role"], "assistant");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
        assert_eq!(result["usage"]["prompt_tokens"], 10);
        assert_eq!(result["usage"]["completion_tokens"], 5);
    }

    #[test]
    fn test_responses_to_openai_function_call_response() {
        let body = json!({
            "id": "resp_456",
            "output": [
                {
                    "type": "function_call",
                    "call_id": "call_abc",
                    "name": "bash",
                    "arguments": "{\"cmd\":\"ls\"}"
                }
            ],
            "status": "completed",
            "usage": {"input_tokens": 20, "output_tokens": 10, "total_tokens": 30}
        });
        let result = responses_to_openai_response(&body, "gpt-4o");

        assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
        let tool_calls = result["choices"][0]["message"]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["id"], "call_abc");
        assert_eq!(tool_calls[0]["function"]["name"], "bash");
        assert_eq!(tool_calls[0]["function"]["arguments"], "{\"cmd\":\"ls\"}");
    }

    #[test]
    fn test_codex_split_leading_think_block() {
        // Standard <think > block
        let text = "<think\nI am reasoning\n</think >The answer is 42";
        let result = codex_split_leading_think_block(text);
        assert!(result.is_some());
        let (reasoning, answer) = result.unwrap();
        assert_eq!(reasoning, "I am reasoning");
        assert_eq!(answer, "The answer is 42");

        // No think block
        let text = "Just a normal response";
        assert!(codex_split_leading_think_block(text).is_none());
    }
}
