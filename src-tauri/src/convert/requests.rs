use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Anthropic → OpenAI: Request conversion
// ---------------------------------------------------------------------------

/// Convert an Anthropic Messages API request body to OpenAI Chat Completions format.
pub fn anthropic_to_openai_request(body: &Value, target_model: &str) -> Value {
    // Make a defensive normalized copy of the incoming Anthropic body so the
    // converter can assume messages have valid roles and non-null content.
    let mut norm = body.clone();
    if let Some(msgs) = norm.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in msgs.iter_mut() {
            if let Some(role) = msg.get("role").and_then(|r| r.as_str()) {
                if role == "tool" || role == "system" {
                    msg["role"] = Value::String("assistant".to_string());
                }
            }

            match msg.get("content") {
                Some(c) if c.is_null() => {
                    msg["content"] = Value::String(String::new());
                }
                Some(c) if !(c.is_string() || c.is_array()) => {
                    let s = if c.is_object() || c.is_number() || c.is_boolean() {
                        serde_json::to_string(c).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    msg["content"] = Value::Array(vec![json!({"type":"text","text":s})]);
                }
                None => {
                    msg["content"] = Value::String(String::new());
                }
                _ => {}
            }
        }
    }

    let mut out = serde_json::Map::new();
    out.insert("model".into(), Value::String(target_model.to_string()));

    // system → prepend as system message
    let mut messages = Vec::new();
    if let Some(system) = body.get("system") {
        let system_text = extract_text_from_content(system);
        if !system_text.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": system_text
            }));
        }
    }

    // Convert messages
    if let Some(msgs) = norm.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = msg.get("content");

            match role {
                "assistant" => {
                    let mut openai_msg = json!({"role": "assistant"});
                    let mut text_parts = Vec::new();
                    let mut tool_calls = Vec::new();
                    let mut reasoning_parts = Vec::new();
                    let mut tool_results = Vec::new();

                    if let Some(c) = content {
                        if let Some(arr) = c.as_array() {
                            for block in arr {
                                match block.get("type").and_then(|t| t.as_str()) {
                                    Some("text") => {
                                        if let Some(t) = block.get("text").and_then(|t| t.as_str())
                                        {
                                            text_parts.push(t.to_string());
                                        }
                                    }
                                    Some("thinking") => {
                                        if let Some(t) = block.get("thinking").and_then(|t| t.as_str()) {
                                            reasoning_parts.push(t.to_string());
                                        }
                                    }
                                    Some("tool_use") => {
                                        let id = block
                                            .get("id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let name = block
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let input = block.get("input").cloned().unwrap_or(json!({}));
                                        tool_calls.push(json!({
                                            "id": id,
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": serde_json::to_string(&input).unwrap_or_default()
                                            }
                                        }));
                                    }
                                    Some("tool_result") => {
                                        let tool_call_id = block
                                            .get("tool_use_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let result_content =
                                            extract_text_from_content(block.get("content").unwrap_or(&Value::Null));
                                        tool_results.push(json!({
                                            "role": "tool",
                                            "tool_call_id": tool_call_id,
                                            "content": result_content
                                        }));
                                    }
                                    _ => {}
                                }
                            }
                        } else if let Some(s) = c.as_str() {
                            text_parts.push(s.to_string());
                        }
                    }

                    let text = text_parts.join("");
                    // Always send a string for content (empty string if no text)
                    openai_msg["content"] = Value::String(text);

                    // Convert thinking blocks → reasoning_content
                    if !reasoning_parts.is_empty() {
                        openai_msg["reasoning_content"] = Value::String(reasoning_parts.join(""));
                    } else if !tool_calls.is_empty() {
                        let thinking_type = body.get("thinking").and_then(|t| t.get("type")).and_then(|v| v.as_str()).unwrap_or("");
                        // When thinking is enabled/adaptive, some providers (e.g. GLM) require reasoning_content
                        // on all assistant messages including tool-call-only ones.
                        if thinking_type == "enabled" || thinking_type == "adaptive" {
                            openai_msg["reasoning_content"] = Value::String(" ".to_string());
                        }
                    }

                    if !tool_calls.is_empty() {
                        openai_msg["tool_calls"] = Value::Array(tool_calls);
                    }
                    messages.push(openai_msg);
                    // Tool results extracted from content (e.g. role="tool" →
                    // "assistant" normalization) go out as separate messages.
                    for tr in tool_results {
                        messages.push(tr);
                    }
                }
                "user" => {
                    // Check for tool_result blocks
                    if let Some(c) = content {
                        if let Some(arr) = c.as_array() {
                            let mut has_tool_result = false;
                            let mut text_parts = Vec::new();
                            let mut tool_results = Vec::new();

                            for block in arr {
                                match block.get("type").and_then(|t| t.as_str()) {
                                    Some("tool_result") => {
                                        has_tool_result = true;
                                        let tool_use_id = block
                                            .get("tool_use_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let result_content =
                                            extract_text_from_content(block.get("content").unwrap_or(&Value::Null));
                                        tool_results.push(json!({
                                            "role": "tool",
                                            "tool_call_id": tool_use_id,
                                            "content": result_content
                                        }));
                                    }
                                    Some("text") => {
                                        if let Some(t) = block.get("text").and_then(|t| t.as_str())
                                        {
                                            text_parts.push(t.to_string());
                                        }
                                    }
                                    _ => {
                                        // Pass through other content blocks as text
                                        text_parts.push(block.to_string());
                                    }
                                }
                            }

                            // If there are tool results, split into separate messages
                            if has_tool_result {
                                // Tool results MUST come first (right after assistant's tool_calls)
                                for tr in tool_results {
                                    messages.push(tr);
                                }
                                // Any remaining text goes after tool results
                                let text = text_parts.join("");
                                if !text.is_empty() {
                                    messages.push(json!({
                                        "role": "user",
                                        "content": text
                                    }));
                                }
                            } else {
                                let text = text_parts.join("");
                                messages.push(json!({
                                    "role": "user",
                                    "content": text
                                }));
                            }
                        } else {
                            // Simple string content
                            messages.push(json!({
                                "role": "user",
                                "content": c.clone()
                            }));
                        }
                    } else {
                        messages.push(json!({"role": "user", "content": ""}));
                    }
                }
                _ => {
                    // Other roles → user. Extract text from content blocks to
                    // avoid forwarding an array to an OpenAI-format provider
                    // (which requires content to be a string).
                    if let Some(c) = content {
                        if c.is_array() {
                            let text = extract_text_from_content(c);
                            messages.push(json!({"role": "user", "content": text}));
                        } else {
                            messages.push(json!({"role": "user", "content": c.clone()}));
                        }
                    }
                }
            }
        }
    }

    out.insert("messages".into(), Value::Array(messages));

    // max_tokens
    if let Some(mt) = body.get("max_tokens") {
        out.insert("max_tokens".into(), mt.clone());
    }

    // temperature
    if let Some(t) = body.get("temperature") {
        out.insert("temperature".into(), t.clone());
    }

    // stream
    if let Some(s) = body.get("stream") {
        out.insert("stream".into(), s.clone());
    }

    // tools: Anthropic → OpenAI
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let openai_tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                if tool.get("type").and_then(|t| t.as_str()) == Some("function") {
                    // Already OpenAI format
                    tool.clone()
                } else {
                    // Anthropic format: {name, description, input_schema}
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                            "description": tool.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                            "parameters": tool.get("input_schema").cloned().unwrap_or(json!({"type": "object", "properties": {}}))
                        }
                    })
                }
            })
            .collect();
        out.insert("tools".into(), Value::Array(openai_tools));
    }

    // tool_choice: Anthropic → OpenAI
    if let Some(tc) = body.get("tool_choice") {
        let openai_tc = match tc {
            Value::String(s) => match s.as_str() {
                "auto" => json!("auto"),
                "any" => json!("required"),
                "none" => json!("none"),
                _ => json!("auto"),
            },
            Value::Object(obj) => {
                if obj.get("type").and_then(|t| t.as_str()) == Some("tool") {
                    json!({
                        "type": "function",
                        "function": {"name": obj.get("name").and_then(|n| n.as_str()).unwrap_or("")}
                    })
                } else {
                    json!("auto")
                }
            }
            _ => json!("auto"),
        };
        out.insert("tool_choice".into(), openai_tc);
    }

    Value::Object(out)
}

// ---------------------------------------------------------------------------
// OpenAI Chat → Anthropic Messages: Request conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI Chat Completions request to Anthropic Messages format.
pub fn openai_to_anthropic_request(body: &Value, target_model: &str) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("model".into(), Value::String(target_model.to_string()));

    // Extract system message from messages list (Anthropic uses top-level "system")
    let mut system_parts: Vec<String> = Vec::new();
    let mut anthropic_messages: Vec<Value> = Vec::new();

    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            match role {
                "system" => {
                    // System content can be string or array of content parts
                    let text = extract_text_from_content(msg.get("content").unwrap_or(&Value::Null));
                    if !text.is_empty() {
                        system_parts.push(text);
                    }
                    // Don't add system messages to the messages array
                }
                "user" => {
                    // User content can be string or array (multimodal)
                    let content = msg.get("content").unwrap_or(&Value::Null);
                    if let Some(s) = content.as_str() {
                        anthropic_messages.push(serde_json::json!({
                            "role": "user",
                            "content": s
                        }));
                    } else if let Some(arr) = content.as_array() {
                        // Convert array content: extract text parts, skip non-text (images etc.)
                        let text = arr.iter()
                            .filter_map(|part| {
                                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                    Some(t.to_string())
                                } else if let Some(s) = part.as_str() {
                                    Some(s.to_string())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        anthropic_messages.push(serde_json::json!({
                            "role": "user",
                            "content": text
                        }));
                    } else {
                        anthropic_messages.push(serde_json::json!({
                            "role": "user",
                            "content": ""
                        }));
                    }
                }
                "assistant" => {
                    let mut blocks: Vec<Value> = Vec::new();
                    // reasoning_content → thinking block (must come first)
                    if let Some(rc) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
                        if !rc.is_empty() {
                            blocks.push(serde_json::json!({"type": "thinking", "thinking": rc}));
                        }
                    }
                    // Text content
                    if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            blocks.push(serde_json::json!({"type": "text", "text": content}));
                        }
                    }
                    // Tool calls
                    if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tool_calls {
                            let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let func = tc.get("function").unwrap_or(&Value::Null);
                            let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let args_str = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                            let input: Value = serde_json::from_str(args_str).unwrap_or(Value::Object(serde_json::Map::new()));
                            blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc_id,
                                "name": name,
                                "input": input
                            }));
                        }
                    }
                    if !blocks.is_empty() {
                        anthropic_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": blocks
                        }));
                    } else {
                        // Plain text assistant message
                        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        anthropic_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": content
                        }));
                    }
                }
                "tool" => {
                    let tool_call_id = msg.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("");
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": [{"type": "tool_result", "tool_use_id": tool_call_id, "content": content}]
                    }));
                }
                _ => {
                    // Unknown role → user
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": content
                    }));
                }
            }
        }
    }

    // Set system field if we collected system messages
    if !system_parts.is_empty() {
        out.insert("system".into(), Value::String(system_parts.join("\n")));
    }

    out.insert("messages".into(), Value::Array(anthropic_messages));

    // Convert tools format
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let mut anthropic_tools: Vec<Value> = Vec::new();
        for tool in tools {
            let func = tool.get("function").unwrap_or(tool);
            let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let desc = func.get("description").and_then(|v| v.as_str()).unwrap_or("");
            let params = func.get("parameters").or_else(|| func.get("input_schema"));
            let mut at = serde_json::Map::new();
            at.insert("name".into(), Value::String(name.to_string()));
            at.insert("description".into(), Value::String(desc.to_string()));
            at.insert("input_schema".into(), params.cloned().unwrap_or(Value::Object(serde_json::Map::new())));
            anthropic_tools.push(Value::Object(at));
        }
        out.insert("tools".into(), Value::Array(anthropic_tools));
    }

    // Copy common fields with OpenAI→Anthropic name mapping
    // Handle stop specially: OpenAI accepts string or array, Anthropic requires array
    if let Some(stop_val) = body.get("stop") {
        let stop_seq = match stop_val {
            Value::String(s) => Value::Array(vec![Value::String(s.clone())]),
            arr @ Value::Array(_) => arr.clone(),
            other => Value::Array(vec![other.clone()]),
        };
        out.insert("stop_sequences".to_string(), stop_seq);
    }
    for (key, dest) in &[
        ("max_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("stream", "stream"),
        ("metadata", "metadata"),
    ] {
        if let Some(val) = body.get(*key) {
            out.insert(dest.to_string(), val.clone());
        }
    }

    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract plain text from Anthropic content (string or array of content blocks).
fn extract_text_from_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut texts = Vec::new();
            for block in blocks {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    texts.push(t.to_string());
                } else if let Some(s) = block.as_str() {
                    texts.push(s.to_string());
                }
            }
            texts.join("")
        }
        _ => String::new(),
    }
}

// ===========================================================================
// OpenAI Responses API conversions
// ===========================================================================
//
// The Responses API (used by Codex CLI) differs from Chat Completions:
//   - Input  uses `input` (array of mixed items), not `messages`
//   - Output uses `output` (array of items), not `choices`
//   - Tool calls use `call_id`, not `id`
//   - System prompt goes in `instructions`, not a system message
//   - Streaming events: response.output_text.delta, etc.
//   - `max_output_tokens` instead of `max_tokens`
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Anthropic → OpenAI Responses: Request conversion
// ---------------------------------------------------------------------------

/// Convert an Anthropic Messages API request body to OpenAI Responses API format.
pub fn anthropic_to_responses_request(body: &Value, target_model: &str) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("model".into(), Value::String(target_model.to_string()));

    // system → instructions
    if let Some(system) = body.get("system") {
        let system_text = extract_text_from_content(system);
        if !system_text.is_empty() {
            out.insert("instructions".into(), Value::String(system_text));
        }
    }

    // Convert messages → input
    let mut input: Vec<Value> = Vec::new();

    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = msg.get("content");

            match role {
                "assistant" => {
                    // Collect text parts and tool_use blocks separately
                    let mut text_parts: Vec<Value> = Vec::new();
                    let mut tool_uses: Vec<Value> = Vec::new();

                    if let Some(c) = content {
                        if let Some(arr) = c.as_array() {
                            for block in arr {
                                match block.get("type").and_then(|t| t.as_str()) {
                                    Some("text") => {
                                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                            text_parts.push(json!({"type": "output_text", "text": t}));
                                        }
                                    }
                                    Some("tool_use") => {
                                        let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                        let input_val = block.get("input").cloned().unwrap_or(json!({}));
                                        let args_str = serde_json::to_string(&input_val).unwrap_or_default();
                                        tool_uses.push(json!({
                                            "type": "function_call",
                                            "call_id": id,
                                            "name": name,
                                            "arguments": args_str,
                                        }));
                                    }
                                    Some("thinking") => {
                                        // Skip thinking blocks
                                    }
                                    _ => {
                                        // Pass through as text
                                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                            text_parts.push(json!({"type": "output_text", "text": t}));
                                        }
                                    }
                                }
                            }
                        } else if let Some(s) = c.as_str() {
                            if !s.is_empty() {
                                text_parts.push(json!({"type": "output_text", "text": s}));
                            }
                        }
                    }

                    // Emit assistant message (only if there's text content)
                    if !text_parts.is_empty() {
                        input.push(json!({
                            "role": "assistant",
                            "content": text_parts
                        }));
                    }

                    // Emit function_call items
                    for tu in tool_uses {
                        input.push(tu);
                    }
                }
                "user" => {
                    // Check for tool_result blocks
                    if let Some(c) = content {
                        if let Some(arr) = c.as_array() {
                            let mut has_tool_result = false;
                            let mut text_parts: Vec<Value> = Vec::new();
                            let mut tool_results: Vec<Value> = Vec::new();

                            for block in arr {
                                match block.get("type").and_then(|t| t.as_str()) {
                                    Some("tool_result") => {
                                        has_tool_result = true;
                                        let call_id = block
                                            .get("tool_use_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let result_text =
                                            extract_text_from_content(block.get("content").unwrap_or(&Value::Null));
                                        tool_results.push(json!({
                                            "type": "function_call_output",
                                            "call_id": call_id,
                                            "output": result_text
                                        }));
                                    }
                                    Some("text") => {
                                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                            text_parts.push(Value::String(t.to_string()));
                                        }
                                    }
                                    _ => {
                                        text_parts.push(Value::String(block.to_string()));
                                    }
                                }
                            }

                            if has_tool_result {
                                // function_call_output items first (must follow function_call)
                                for tr in tool_results {
                                    input.push(tr);
                                }
                                // Any remaining text goes after
                                let text = text_parts.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join("");
                                if !text.is_empty() {
                                    input.push(json!({
                                        "role": "user",
                                        "content": text
                                    }));
                                }
                            } else {
                                // Regular user message
                                let text = text_parts.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join("");
                                if !text.is_empty() {
                                    input.push(json!({"role": "user", "content": text}));
                                }
                            }
                        } else {
                            input.push(json!({"role": "user", "content": c.clone()}));
                        }
                    } else {
                        input.push(json!({"role": "user", "content": ""}));
                    }
                }
                _ => {
                    // Other roles → user
                    if let Some(c) = content {
                        input.push(json!({"role": "user", "content": c.clone()}));
                    }
                }
            }
        }
    }

    out.insert("input".into(), Value::Array(input));

    // max_tokens → max_output_tokens
    if let Some(mt) = body.get("max_tokens") {
        out.insert("max_output_tokens".into(), mt.clone());
    }

    // temperature
    if let Some(t) = body.get("temperature") {
        out.insert("temperature".into(), t.clone());
    }

    // stream
    if let Some(s) = body.get("stream") {
        out.insert("stream".into(), s.clone());
    }

    // tools: Anthropic → Responses format
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let resp_tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                if tool.get("type").and_then(|t| t.as_str()) == Some("function") {
                    // Already function format
                    tool.clone()
                } else {
                    // Anthropic format: {name, description, input_schema}
                    json!({
                        "type": "function",
                        "name": tool.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "description": tool.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                        "parameters": tool.get("input_schema").cloned().unwrap_or(json!({"type": "object", "properties": {}}))
                    })
                }
            })
            .collect();
        out.insert("tools".into(), Value::Array(resp_tools));
    }

    // tool_choice
    if let Some(tc) = body.get("tool_choice") {
        let resp_tc = match tc {
            Value::String(s) => match s.as_str() {
                "auto" => json!("auto"),
                "any" => json!("required"),
                "none" => json!("none"),
                _ => json!("auto"),
            },
            Value::Object(obj) => {
                if obj.get("type").and_then(|t| t.as_str()) == Some("tool") {
                    json!({
                        "type": "function",
                        "name": obj.get("name").and_then(|n| n.as_str()).unwrap_or("")
                    })
                } else {
                    json!("auto")
                }
            }
            _ => json!("auto"),
        };
        out.insert("tool_choice".into(), resp_tc);
    }

    Value::Object(out)
}

// ---------------------------------------------------------------------------
// OpenAI Chat → OpenAI Responses: Request conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI Chat Completions request body to OpenAI Responses API format.
pub fn openai_to_responses_request(body: &Value, target_model: &str) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("model".into(), Value::String(target_model.to_string()));

    let mut input: Vec<Value> = Vec::new();
    let mut instructions: Option<String> = None;

    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = msg.get("content");

            match role {
                "system" => {
                    // First system message → instructions
                    if instructions.is_none() {
                        let text = match content {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        if !text.is_empty() {
                            instructions = Some(text);
                        }
                    } else {
                        // Additional system messages → user
                        let text = match content {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        input.push(json!({"role": "user", "content": text}));
                    }
                }
                "assistant" => {
                    // reasoning_content → reasoning item (must precede the message)
                    if let Some(rc) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
                        if !rc.is_empty() {
                            input.push(json!({
                                "type": "reasoning",
                                "summary": [{"type": "summary_text", "text": rc}]
                            }));
                        }
                    }

                    let mut text_parts: Vec<Value> = Vec::new();
                    let mut tool_calls: Vec<Value> = Vec::new();

                    if let Some(c) = content {
                        if !c.is_null() {
                            if let Some(s) = c.as_str() {
                                if !s.is_empty() {
                                    text_parts.push(json!({"type": "output_text", "text": s}));
                                }
                            }
                        }
                    }

                    if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tcs {
                            let call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let name = tc.get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                                .unwrap_or("");
                            let args = tc.get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                                .unwrap_or("{}");
                            tool_calls.push(json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": name,
                                "arguments": args
                            }));
                        }
                    }

                    if !text_parts.is_empty() {
                        input.push(json!({
                            "role": "assistant",
                            "content": text_parts
                        }));
                    }
                    for tc in tool_calls {
                        input.push(tc);
                    }
                }
                "tool" => {
                    // tool message → function_call_output
                    let call_id = msg.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("");
                    let output_text = match content {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => serde_json::to_string(other).unwrap_or_default(),
                        None => String::new(),
                    };
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output_text
                    }));
                }
                _ => {
                    // user (or other) → user message. OpenAI Chat content blocks
                    // must be translated to Responses content blocks, or the
                    // Responses API rejects them ("Invalid content type: text"):
                    //   text      → input_text
                    //   image_url → input_image  (image_url object flattened to a string)
                    let converted = match content {
                        Some(Value::String(s)) => {
                            Value::Array(vec![json!({ "type": "input_text", "text": s })])
                        }
                        Some(Value::Array(blocks)) => {
                            let mut parts: Vec<Value> = Vec::new();
                            for block in blocks {
                                // Detect by key presence, not just `type`: some
                                // clients send {"image_url": "..."} or {"text": "..."}
                                // with no type field, which would otherwise pass
                                // through and be rejected by the Responses API.
                                if block.get("image_url").is_some() {
                                    // Chat: {"image_url": {"url": "..."}}  →  Responses: {"image_url": "..."}
                                    let url = block.get("image_url").and_then(|v| {
                                        if let Some(s) = v.as_str() {
                                            Some(s.to_string())
                                        } else {
                                            v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string())
                                        }
                                    });
                                    if let Some(url) = url {
                                        parts.push(json!({ "type": "input_image", "image_url": url }));
                                    }
                                } else if let Some(txt) = block.get("text").and_then(|v| v.as_str()) {
                                    parts.push(json!({ "type": "input_text", "text": txt }));
                                } else {
                                    // Unknown block — pass through best-effort.
                                    parts.push(block.clone());
                                }
                            }
                            Value::Array(parts)
                        }
                        Some(other) => other.clone(),
                        None => Value::String(String::new()),
                    };
                    input.push(json!({ "role": role, "content": converted }));
                }
            }
        }
    }

    if let Some(instr) = instructions {
        out.insert("instructions".into(), Value::String(instr));
    }
    out.insert("input".into(), Value::Array(input));

    // max_tokens → max_output_tokens
    if let Some(mt) = body.get("max_tokens") {
        out.insert("max_output_tokens".into(), mt.clone());
    }
    if let Some(mt) = body.get("max_completion_tokens") {
        out.insert("max_output_tokens".into(), mt.clone());
    }

    if let Some(t) = body.get("temperature") {
        out.insert("temperature".into(), t.clone());
    }
    if let Some(s) = body.get("stream") {
        out.insert("stream".into(), s.clone());
    }

    // tools: Chat wraps each function as {"type":"function","function":{name,...}},
    // but the Responses API expects the flat form {"type":"function","name":...}.
    // Passing Chat-format tools through unchanged makes the Responses API reject
    // the parameter schema (DashScope: "parameters must conform to valid openai-
    // compatible JSON schema"). Flatten function tools.
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let converted: Vec<Value> = tools
            .iter()
            .map(|tool| {
                if tool.get("type").and_then(|t| t.as_str()) == Some("function") {
                    if let Some(func) = tool.get("function") {
                        let mut flat = serde_json::Map::new();
                        flat.insert("type".into(), Value::String("function".into()));
                        for k in ["name", "description", "parameters", "strict"] {
                            if let Some(v) = func.get(k) {
                                flat.insert(k.into(), v.clone());
                            }
                        }
                        return Value::Object(flat);
                    }
                }
                tool.clone()
            })
            .collect();
        out.insert("tools".into(), Value::Array(converted));
    }
    if let Some(tc) = body.get("tool_choice") {
        out.insert("tool_choice".into(), tc.clone());
    }

    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Codex: Responses API → Chat Completions request conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI Responses API request body to OpenAI Chat Completions format.
/// Used when the Codex client sends Responses requests but the upstream provider
/// only supports Chat Completions (e.g., DeepSeek, Moonshot).
pub fn responses_to_chat_request(body: &Value, target_model: &str) -> Value {
    let mut result = serde_json::Map::new();
    result.insert("model".into(), Value::String(target_model.to_string()));

    let mut messages = Vec::new();

    // instructions → system message
    if let Some(instructions) = body.get("instructions") {
        let text = codex_instruction_text(instructions);
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }

    // input → messages array
    if let Some(input) = body.get("input") {
        codex_append_responses_input_as_chat_messages(input, &mut messages);
    }

    // Fix ordering for strict providers (DeepSeek)
    codex_fix_chat_message_ordering(&mut messages);
    result.insert("messages".into(), json!(messages));

    // Token limits
    if let Some(max_tokens) = body.get("max_output_tokens") {
        result.insert("max_tokens".into(), max_tokens.clone());
    }
    if let Some(max_tokens) = body.get("max_tokens") {
        result.insert("max_tokens".into(), max_tokens.clone());
    }
    if let Some(max_tokens) = body.get("max_completion_tokens") {
        result.insert("max_completion_tokens".into(), max_tokens.clone());
    }

    // Passthrough fields
    for key in &["temperature", "top_p", "stream"] {
        if let Some(value) = body.get(*key) {
            result.insert((*key).to_string(), value.clone());
        }
    }

    // Tools conversion: Responses {type:"function", name, parameters} → Chat {type:"function", function:{name, parameters}}
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let chat_tools: Vec<Value> = tools.iter().filter_map(codex_responses_tool_to_chat_tool).collect();
        if !chat_tools.is_empty() {
            result.insert("tools".into(), json!(chat_tools));
        }
    }

    if let Some(tool_choice) = body.get("tool_choice") {
        result.insert("tool_choice".into(), tool_choice.clone());
    }

    // Extra passthrough fields
    for key in &[
        "frequency_penalty", "logit_bias", "logprobs", "metadata", "n",
        "parallel_tool_calls", "presence_penalty", "response_format", "seed",
        "service_tier", "stop", "stream_options", "top_logprobs", "user",
    ] {
        if let Some(value) = body.get(*key) {
            result.insert((*key).to_string(), value.clone());
        }
    }

    Value::Object(result)
}

fn codex_instruction_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|v| v.as_str()).or_else(|| p.as_str()))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => other.as_str().unwrap_or_default().to_string(),
    }
}

/// State for tracking pending tool_calls and reasoning during conversion.
struct CodexChatConversionState {
    pending_reasoning: String,
    last_reasoning: String,
    pending_tool_calls: Vec<Value>,
    last_flushed_tool_call_ids: Vec<String>,
}

impl CodexChatConversionState {
    fn new() -> Self {
        Self {
            pending_reasoning: String::new(),
            last_reasoning: String::new(),
            pending_tool_calls: Vec::new(),
            last_flushed_tool_call_ids: Vec::new(),
        }
    }

    fn consume_reasoning(&mut self) -> String {
        if !self.pending_reasoning.is_empty() {
            self.last_reasoning = self.pending_reasoning.clone();
            return std::mem::take(&mut self.pending_reasoning);
        }
        self.last_reasoning.clone()
    }

    fn flush_tool_calls(&mut self, messages: &mut Vec<Value>) {
        if self.pending_tool_calls.is_empty() {
            return;
        }
        let reasoning = self.consume_reasoning();
        let mut msg = json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": std::mem::take(&mut self.pending_tool_calls)
        });
        if !reasoning.is_empty() {
            msg["reasoning_content"] = json!(reasoning);
        }
        self.last_flushed_tool_call_ids = msg["tool_calls"]
            .as_array()
            .map(|tcs| {
                tcs.iter()
                    .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        messages.push(msg);
    }

    fn fill_missing_tool_results(&mut self, messages: &mut Vec<Value>) {
        if self.last_flushed_tool_call_ids.is_empty() {
            return;
        }
        let answered: std::collections::HashSet<String> = messages
            .iter()
            .rev()
            .take_while(|m| m.get("role").and_then(|v| v.as_str()) == Some("tool"))
            .filter_map(|m| m.get("tool_call_id").and_then(|v| v.as_str()).map(String::from))
            .collect();
        let missing: Vec<String> = self.last_flushed_tool_call_ids.iter()
            .filter(|id| !answered.contains(id.as_str()))
            .cloned()
            .collect();
        for id in missing {
            messages.push(json!({"role": "tool", "tool_call_id": id, "content": ""}));
        }
        self.last_flushed_tool_call_ids.clear();
    }
}

fn codex_append_responses_input_as_chat_messages(input: &Value, messages: &mut Vec<Value>) {
    let mut state = CodexChatConversionState::new();

    match input {
        Value::String(text) => {
            messages.push(json!({"role": "user", "content": text}));
        }
        Value::Array(items) => {
            for item in items {
                codex_append_responses_item_as_chat_message(item, messages, &mut state);
            }
        }
        Value::Object(_) => {
            codex_append_responses_item_as_chat_message(input, messages, &mut state);
        }
        _ => {}
    }

    state.flush_tool_calls(messages);
}

fn codex_extract_reasoning_text(item: &Value) -> String {
    let from_content = item.get("content")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|c| {
                    c.get("type").and_then(|v| v.as_str()) == Some("reasoning_text")
                        || c.get("type").and_then(|v| v.as_str()) == Some("reasoning")
                })
                .filter_map(|c| c.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    let from_summary = item.get("summary")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|c| c.get("type").and_then(|v| v.as_str()) == Some("summary_text"))
                .filter_map(|c| c.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    if !from_content.is_empty() { from_content } else { from_summary }
}

fn codex_append_responses_item_as_chat_message(
    item: &Value,
    messages: &mut Vec<Value>,
    state: &mut CodexChatConversionState,
) {
    let item_type = item.get("type").and_then(|v| v.as_str());

    match item_type {
        Some("reasoning") => {
            let text = codex_extract_reasoning_text(item);
            if !text.is_empty() {
                state.pending_reasoning.push_str(&text);
            }
        }
        Some("function_call") => {
            state.pending_tool_calls.push(codex_responses_function_call_to_chat_tool_call(item));
        }
        Some("function_call_output") => {
            state.flush_tool_calls(messages);
            let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
            let output = match item.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => serde_json::to_string(v).unwrap_or_default(),
                None => String::new(),
            };
            messages.push(json!({"role": "tool", "tool_call_id": call_id, "content": output}));
        }
        Some("message") | None => {
            state.flush_tool_calls(messages);
            state.fill_missing_tool_results(messages);

            if item.get("role").is_some() || item.get("content").is_some() {
                let raw_role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                let role = match raw_role {
                    "developer" | "system" => "user",
                    other => other,
                };

                if role == "user" {
                    state.pending_reasoning.clear();
                    state.last_reasoning.clear();
                }

                let content = item.get("content")
                    .map(|v| codex_responses_content_to_chat_content(role, v))
                    .unwrap_or(Value::Null);

                let mut msg = json!({"role": role, "content": content});

                if role == "assistant" {
                    let reasoning = state.consume_reasoning();
                    if !reasoning.is_empty() {
                        msg["reasoning_content"] = json!(reasoning);
                    }
                }

                // Skip empty assistant messages
                if role == "assistant"
                    && (content.is_null() || content.as_str().map_or(false, |s| s.is_empty()))
                    && !msg.get("reasoning_content").is_some()
                {
                    return;
                }

                messages.push(msg);
            }
        }
        _ => {
            state.flush_tool_calls(messages);
            state.fill_missing_tool_results(messages);
        }
    }
}

fn codex_responses_content_to_chat_content(_role: &str, content: &Value) -> Value {
    if content.is_null() || content.is_string() {
        return content.clone();
    }
    let Some(parts) = content.as_array() else {
        return content.clone();
    };

    let mut chat_parts: Vec<Value> = Vec::new();
    let mut has_non_text = false;

    for part in parts {
        let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match part_type {
            "input_text" | "output_text" | "text" => {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        chat_parts.push(json!({"type": "text", "text": text}));
                    }
                }
            }
            "refusal" => {
                if let Some(text) = part.get("refusal").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        chat_parts.push(json!({"type": "text", "text": text}));
                    }
                }
            }
            "input_image" => {
                if let Some(image_url) = part.get("image_url") {
                    let image_url = if image_url.is_object() {
                        image_url.clone()
                    } else {
                        json!({"url": image_url.as_str().unwrap_or_default()})
                    };
                    chat_parts.push(json!({"type": "image_url", "image_url": image_url}));
                    has_non_text = true;
                }
            }
            _ => {}
        }
    }

    if !has_non_text {
        Value::String(
            chat_parts.iter()
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        Value::Array(chat_parts)
    }
}

fn codex_responses_function_call_to_chat_tool_call(item: &Value) -> Value {
    let call_id = item.get("call_id").or_else(|| item.get("id"))
        .and_then(|v| v.as_str()).unwrap_or("");
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = match item.get("arguments") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => serde_json::to_string(v).unwrap_or_default(),
        None => "{}".to_string(),
    };
    json!({
        "id": call_id,
        "type": "function",
        "function": {"name": name, "arguments": arguments}
    })
}

fn codex_responses_tool_to_chat_tool(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(|v| v.as_str()) != Some("function") {
        return None;
    }
    // If already has nested "function" key, use as-is
    if tool.get("function").is_some() {
        return Some(tool.clone());
    }
    // Otherwise wrap name/description/parameters into function object
    let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = tool.get("description").cloned().unwrap_or(Value::Null);
    let parameters = tool.get("parameters").cloned().unwrap_or_else(|| json!({}));
    let mut function = json!({"name": name, "description": description, "parameters": parameters});
    if let Some(strict) = tool.get("strict") {
        function["strict"] = strict.clone();
    }
    Some(json!({"type": "function", "function": function}))
}

/// Fix message ordering for strict providers (e.g., DeepSeek).
fn codex_fix_chat_message_ordering(messages: &mut Vec<Value>) {
    use std::collections::HashSet;

    let mut fixed: Vec<Value> = Vec::new();
    let mut placed_tool_calls: HashSet<String> = HashSet::new();

    // Helper: does a message have tool_calls containing a given call_id?
    let has_matching_tool_call = |msg: &Value, cid: &str| -> bool {
        msg.get("tool_calls").and_then(|v| v.as_array())
            .map(|tcs| tcs.iter().any(|tc| tc.get("id").and_then(|v| v.as_str()) == Some(cid)))
            .unwrap_or(false)
    };

    for msg in messages.iter() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        // --- Handle tool messages: validate position, deduplicate, relocate ---
        if role == "tool" {
            if let Some(call_id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                // 1. Handle duplicates: replace empty fill with real data, or skip true dup
                if placed_tool_calls.contains(call_id) {
                    // Search for the existing entry — if it's an empty fill, replace with real data
                    if let Some(existing) = fixed.iter_mut().find(|m| {
                        m.get("role").and_then(|v| v.as_str()) == Some("tool")
                            && m.get("tool_call_id").and_then(|v| v.as_str()) == Some(call_id)
                    }) {
                        let is_empty = existing.get("content")
                            .map_or(true, |c| c.is_null() || c.as_str().map_or(true, |s| s.is_empty()));
                        if is_empty {
                            if let Some(content) = msg.get("content").cloned() {
                                existing["content"] = content;
                            }
                        }
                    }
                    continue;
                }

                // 2. If immediately preceded by assistant with matching tool_calls → add normally
                if let Some(prev) = fixed.last() {
                    let prev_role = prev.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    if prev_role == "assistant" && has_matching_tool_call(prev, call_id) {
                        placed_tool_calls.insert(call_id.to_string());
                        fixed.push(msg.clone());
                        continue;
                    }

                    // Also OK: preceded by another tool message whose assistant has this call_id
                    if prev_role == "tool" {
                        let mut found = false;
                        for m in fixed.iter().rev() {
                            let m_role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
                            if m_role == "assistant" {
                                found = has_matching_tool_call(m, call_id);
                                break;
                            }
                            // Break at any boundary that's not part of the tool block
                            if m_role != "tool" { break; }
                        }
                        if found {
                            placed_tool_calls.insert(call_id.to_string());
                            fixed.push(msg.clone());
                            continue;
                        }
                    }
                }

                // 3. Tool is out of position — find matching assistant and relocate
                let mut relocated = false;
                for i in (0..fixed.len()).rev() {
                    let m_role = fixed[i].get("role").and_then(|v| v.as_str()).unwrap_or("");
                    if m_role == "assistant" {
                        if has_matching_tool_call(&fixed[i], call_id) {
                            // Insert right after this assistant (and any existing tool messages following it)
                            let mut insert_pos = i + 1;
                            while insert_pos < fixed.len() {
                                let next_role = fixed[insert_pos].get("role").and_then(|v| v.as_str()).unwrap_or("");
                                if next_role == "tool" { insert_pos += 1; } else { break; }
                            }
                            fixed.insert(insert_pos, msg.clone());
                            placed_tool_calls.insert(call_id.to_string());
                            relocated = true;
                            break;
                        }
                    }
                    // Break at any boundary that's not part of the tool chain
                    if m_role != "assistant" && m_role != "tool" { break; }
                }
                if !relocated {
                    continue;
                }
                continue;
            }
            // tool message without tool_call_id — keep as-is
            fixed.push(msg.clone());
            continue;
        }

        // --- If previous assistant has tool_calls and current is NOT tool, fill missing results ---
        if let Some(prev_msg) = fixed.last() {
            let prev_role = prev_msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if prev_role == "assistant" {
                if let Some(tcs) = prev_msg.get("tool_calls").and_then(|v| v.as_array()) {
                    if !tcs.is_empty() {
                        let missing: Vec<String> = tcs.iter()
                            .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(String::from))
                            .filter(|id| !placed_tool_calls.contains(id.as_str()))
                            .collect();
                        for id in &missing {
                            fixed.push(json!({"role": "tool", "tool_call_id": id, "content": ""}));
                            placed_tool_calls.insert(id.clone());
                        }
                    }
                }
            }
        }

        // --- Skip consecutive empty assistant messages ---
        if role == "assistant" {
            if let Some(prev_msg) = fixed.last() {
                let prev_role = prev_msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if prev_role == "assistant"
                    && msg.get("content").map_or(true, |c| c.is_null() || c.as_str().map_or(true, |s| s.is_empty()))
                    && msg.get("tool_calls").is_none()
                    && msg.get("reasoning_content").is_none()
                {
                    continue;
                }
            }
        }

        fixed.push(msg.clone());
    }

    // Remove trailing assistant messages without tool_calls
    while fixed.last().map_or(false, |m| {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        role == "assistant" && m.get("tool_calls").and_then(|v| v.as_array()).map_or(true, |tcs| tcs.is_empty())
    }) {
        fixed.pop();
    }

    // If trailing assistant has tool_calls, add empty tool results
    if fixed.last().map_or(false, |m| {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        role == "assistant" && m.get("tool_calls").and_then(|v| v.as_array()).map_or(false, |tcs| !tcs.is_empty())
    }) {
        let tool_call_ids: Vec<String> = fixed.last()
            .and_then(|m| m.get("tool_calls").and_then(|v| v.as_array()))
            .map(|tcs| tcs.iter().filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(String::from)).collect())
            .unwrap_or_default();
        for id in tool_call_ids {
            if !placed_tool_calls.contains(&id) {
                fixed.push(json!({"role": "tool", "tool_call_id": id, "content": ""}));
            }
        }
    }

    // Ensure at least one user or tool message
    if !fixed.iter().any(|m| {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        role == "user" || role == "tool"
    }) {
        fixed.insert(0, json!({"role": "user", "content": "Continue."}));
    }

    *messages = fixed;
}

// ---------------------------------------------------------------------------
// Codex: Responses API → Anthropic Messages request conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI Responses API request body to Anthropic Messages format.
/// Used when the Codex client sends Responses requests but the upstream provider
/// only supports Anthropic Messages (e.g., Qwen, Zhipu GLM).
pub fn responses_to_anthropic_request(body: &Value, target_model: &str) -> Value {
    let mut result = serde_json::Map::new();
    result.insert("model".into(), Value::String(target_model.to_string()));

    // instructions → system
    if let Some(instructions) = body.get("instructions") {
        let text = codex_instruction_text(instructions);
        if !text.is_empty() {
            result.insert("system".into(), json!(text));
        }
    }

    // input → messages
    let mut messages = Vec::new();
    if let Some(input) = body.get("input") {
        codex_append_responses_input_as_anthropic_messages(input, &mut messages);
    }
    codex_fix_anthropic_message_ordering(&mut messages);
    result.insert("messages".into(), json!(messages));

    // Max tokens
    let max_tokens = body.get("max_output_tokens")
        .or_else(|| body.get("max_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(16384);
    result.insert("max_tokens".into(), json!(max_tokens));

    // Stream
    if let Some(stream) = body.get("stream") {
        result.insert("stream".into(), stream.clone());
    }

    // Thinking config
    let thinking_enabled = body.get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|v| v.as_str())
        != Some("disabled");
    if thinking_enabled {
        let budget_tokens = body.get("thinking")
            .and_then(|t| t.get("budget_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(10000);
        result.insert("thinking".into(), json!({"type": "enabled", "budget_tokens": budget_tokens}));
        let min_max_tokens = (budget_tokens + 6000) as u64;
        if max_tokens < min_max_tokens {
            result.insert("max_tokens".into(), json!(min_max_tokens));
        }
    }

    // Tools: Responses {type:"function", name, parameters} → Anthropic {name, description, input_schema}
    if let Some(tools) = body.get("tools") {
        let anthropic_tools = codex_responses_tools_to_anthropic_tools(tools);
        if !anthropic_tools.is_empty() {
            result.insert("tools".into(), json!(anthropic_tools));
        }
    }

    Value::Object(result)
}

fn codex_append_responses_input_as_anthropic_messages(input: &Value, messages: &mut Vec<Value>) {
    let mut pending_tool_calls: Vec<Value> = Vec::new();
    let mut pending_reasoning = String::new();

    match input {
        Value::String(text) => {
            messages.push(json!({"role": "user", "content": text}));
        }
        Value::Array(items) => {
            for item in items {
                let item_type = item.get("type").and_then(|v| v.as_str());
                match item_type {
                    Some("reasoning") => {
                        let text = codex_extract_reasoning_text(item);
                        if !text.is_empty() {
                            pending_reasoning.push_str(&text);
                        }
                    }
                    Some("function_call") => {
                        let call_id = item.get("call_id").or_else(|| item.get("id"))
                            .and_then(|v| v.as_str()).unwrap_or("");
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = item.get("arguments").cloned().unwrap_or(json!({}));
                        let input_val = match args {
                            Value::String(s) => serde_json::from_str(&s).unwrap_or(json!({})),
                            v => v,
                        };
                        pending_tool_calls.push(json!({
                            "type": "tool_use", "id": call_id, "name": name, "input": input_val
                        }));
                    }
                    Some("function_call_output") => {
                        codex_flush_anthropic_tool_calls(messages, &mut pending_tool_calls, &mut pending_reasoning);
                        let call_id = item.get("call_id").or_else(|| item.get("tool_call_id"))
                            .and_then(|v| v.as_str()).unwrap_or("");
                        let output = match item.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => serde_json::to_string(v).unwrap_or_default(),
                            None => String::new(),
                        };
                        messages.push(json!({
                            "role": "user",
                            "content": [{"type": "tool_result", "tool_use_id": call_id, "content": output}]
                        }));
                    }
                    Some("message") | None => {
                        codex_flush_anthropic_tool_calls(messages, &mut pending_tool_calls, &mut pending_reasoning);
                        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                        let text = codex_anthropic_content_to_text(item.get("content"));

                        match role {
                            "developer" => {
                                if !text.is_empty() {
                                    messages.push(json!({"role": "user", "content": text}));
                                }
                            }
                            "user" => {
                                pending_reasoning.clear();
                                if !text.is_empty() {
                                    messages.push(json!({"role": "user", "content": text}));
                                }
                            }
                            "assistant" => {
                                let mut content: Vec<Value> = Vec::new();
                                if !pending_reasoning.is_empty() {
                                    content.push(json!({"type": "thinking", "thinking": std::mem::take(&mut pending_reasoning)}));
                                }
                                if !text.is_empty() {
                                    content.push(json!({"type": "text", "text": text}));
                                }
                                if !content.is_empty() {
                                    messages.push(json!({"role": "assistant", "content": content}));
                                }
                            }
                            _ => {
                                if !text.is_empty() {
                                    messages.push(json!({"role": "user", "content": text}));
                                }
                            }
                        }
                    }
                    _ => {
                        codex_flush_anthropic_tool_calls(messages, &mut pending_tool_calls, &mut pending_reasoning);
                    }
                }
            }
        }
        _ => {}
    }

    codex_flush_anthropic_tool_calls(messages, &mut pending_tool_calls, &mut pending_reasoning);
}

fn codex_flush_anthropic_tool_calls(
    messages: &mut Vec<Value>,
    pending_tool_calls: &mut Vec<Value>,
    pending_reasoning: &mut String,
) {
    if pending_tool_calls.is_empty() {
        return;
    }
    let mut content: Vec<Value> = Vec::new();
    if !pending_reasoning.is_empty() {
        content.push(json!({"type": "thinking", "thinking": std::mem::take(pending_reasoning)}));
    }
    content.extend(std::mem::take(pending_tool_calls));
    messages.push(json!({"role": "assistant", "content": content}));
}

fn codex_anthropic_content_to_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts.iter()
            .filter_map(|p| p.get("text").and_then(|v| v.as_str()).or_else(|| p.as_str()))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn codex_responses_tools_to_anthropic_tools(tools: &Value) -> Vec<Value> {
    let Some(arr) = tools.as_array() else { return Vec::new() };
    arr.iter()
        .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("function"))
        .filter(|t| t.get("name").is_some())
        .map(|t| {
            let mut schema = json!({"type": "object", "properties": {}});
            if let Some(params) = t.get("parameters") {
                if let Some(obj) = params.as_object() {
                    let mut clean = serde_json::Map::new();
                    for (k, v) in obj {
                        if k != "additionalProperties" && k != "strict" {
                            clean.insert(k.clone(), v.clone());
                        }
                    }
                    schema = Value::Object(clean);
                }
            }
            json!({
                "name": t["name"],
                "description": t.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                "input_schema": schema
            })
        })
        .collect()
}

fn codex_fix_anthropic_message_ordering(messages: &mut Vec<Value>) {
    let mut fixed: Vec<Value> = Vec::with_capacity(messages.len());

    for msg in messages.drain(..) {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        // If previous assistant has tool_use, current must be user with tool_result
        if let Some(prev) = fixed.last() {
            if prev.get("role").and_then(|v| v.as_str()) == Some("assistant") {
                let has_tool_use = prev.get("content")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().any(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use")))
                    .unwrap_or(false);
                if has_tool_use && role != "user" {
                    let tool_use_ids: Vec<String> = prev.get("content")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter()
                            .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                            .filter_map(|b| b.get("id").and_then(|v| v.as_str()).map(String::from))
                            .collect())
                        .unwrap_or_default();
                    if !tool_use_ids.is_empty() {
                        let results: Vec<Value> = tool_use_ids.iter()
                            .map(|id| json!({"type": "tool_result", "tool_use_id": id, "content": ""}))
                            .collect();
                        fixed.push(json!({"role": "user", "content": results}));
                    }
                }
            }
        }

        // Merge consecutive user messages
        if role == "user" {
            if let Some(prev) = fixed.last_mut() {
                if prev.get("role").and_then(|v| v.as_str()) == Some("user") {
                    if let Some(Value::Array(arr)) = prev.get_mut("content") {
                        if let Some(Value::Array(new_arr)) = msg.get("content") {
                            arr.extend(new_arr.clone());
                            continue;
                        } else if let Some(text) = msg.get("content").and_then(|v| v.as_str()) {
                            arr.push(json!({"type": "text", "text": text}));
                            continue;
                        }
                    }
                }
            }
        }

        fixed.push(msg);
    }

    // Ensure not ending with assistant
    while fixed.last().map_or(false, |m| m.get("role").and_then(|v| v.as_str()) == Some("assistant")) {
        let last = fixed.last().unwrap();
        let has_tool_use = last.get("content")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().any(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use")))
            .unwrap_or(false);
        if has_tool_use {
            let tool_use_ids: Vec<String> = last.get("content")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter()
                    .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                    .filter_map(|b| b.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect())
                .unwrap_or_default();
            let results: Vec<Value> = tool_use_ids.iter()
                .map(|id| json!({"type": "tool_result", "tool_use_id": id, "content": ""}))
                .collect();
            fixed.push(json!({"role": "user", "content": results}));
        } else {
            fixed.push(json!({"role": "user", "content": " "}));
        }
    }

    // Ensure at least one user message
    if !fixed.iter().any(|m| m.get("role").and_then(|v| v.as_str()) == Some("user")) {
        fixed.push(json!({"role": "user", "content": "Continue."}));
    }

    *messages = fixed;
}
