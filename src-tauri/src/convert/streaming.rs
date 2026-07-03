use bytes::Bytes;
use futures::stream::Stream;
use futures::StreamExt;
use serde_json::{json, Value};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

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

// ---------------------------------------------------------------------------
// OpenAI SSE -> Anthropic SSE: Streaming conversion
// ---------------------------------------------------------------------------

/// State for the streaming SSE converter state machine.
struct StreamState {
    started: bool,
    finished: bool,
    block_index: u32,
    thinking_block_open: bool,
    text_block_open: bool,
    tool_blocks_open: Vec<bool>,
    msg_id: String,
    model: String,
    usage_input_tokens: u64,
    usage_output_tokens: u64,
}

/// Convert an OpenAI SSE byte stream into Anthropic SSE events.
pub fn convert_openai_stream_to_anthropic(
    upstream: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    request_model: String,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let state = Arc::new(Mutex::new(StreamState {
        started: false,
        finished: false,
        block_index: 0,
        thinking_block_open: false,
        text_block_open: false,
        tool_blocks_open: Vec::new(),
        msg_id: format!("msg_{:x}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()),
        model: request_model,
        usage_input_tokens: 0,
        usage_output_tokens: 0,
    }));

    let buffer = Arc::new(Mutex::new(String::new()));

    let stream = upstream.flat_map(move |chunk_result| {
        let state = state.clone();
        let buffer = buffer.clone();

        async_stream::stream! {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            let text = String::from_utf8_lossy(&chunk);
            let mut local_buf;
            {
                let mut guard = buffer.lock().await;
                guard.push_str(&text);
                local_buf = std::mem::take(&mut *guard);
            }

            // Process complete lines
            while let Some(newline_pos) = local_buf.find('\n') {
                let line = local_buf[..newline_pos].trim().to_string();
                local_buf = local_buf[newline_pos + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                if line == "data: [DONE]" {
                    let mut st = state.lock().await;
                    if st.finished {
                        continue;
                    }
                    // Close any open blocks and finish
                    log::info!("[Stream] [DONE] received, closing blocks (thinking={}, text={}, blk_idx={})",
                        st.thinking_block_open, st.text_block_open, st.block_index);
                    if st.thinking_block_open {
                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                        st.thinking_block_open = false;
                        st.block_index += 1;
                    }
                    if st.text_block_open {
                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                        st.text_block_open = false;
                        st.block_index += 1;
                    }
                    let tool_count = st.tool_blocks_open.len();
                    for i in 0..tool_count {
                        if st.tool_blocks_open[i] {
                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                            st.block_index += 1;
                        }
                    }

                    // message_delta + message_stop
                    st.finished = true;
                    let msg_delta = json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": "end_turn", "stop_sequence": Value::Null},
                        "usage": {"output_tokens": st.usage_output_tokens}
                    });
                    yield Ok(Bytes::from(format!("event: message_delta\ndata: {}\n\n", msg_delta)));
                    yield Ok(Bytes::from("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()));
                    continue;
                }

                let data = if let Some(stripped) = line.strip_prefix("data: ") {
                    stripped.to_string()
                } else if let Some(stripped) = line.strip_prefix("data:") {
                    stripped.trim().to_string()
                } else {
                    continue;
                };

                let parsed: Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let mut st = state.lock().await;

                // Emit message_start on first chunk
                if !st.started {
                    st.started = true;
                    log::info!("[Stream] first chunk received, emitting message_start");
                    // Try to extract usage from first chunk
                    if let Some(u) = parsed.get("usage") {
                        st.usage_input_tokens = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        if let Some(ct) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
                            st.usage_output_tokens = ct;
                        }
                    }
                    let msg_start = json!({
                        "type": "message_start",
                        "message": {
                            "id": st.msg_id,
                            "type": "message",
                            "role": "assistant",
                            "content": [],
                            "model": st.model,
                            "stop_reason": Value::Null,
                            "stop_sequence": Value::Null,
                            "usage": {
                                "input_tokens": st.usage_input_tokens,
                                "output_tokens": 1,
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": 0
                            }
                        }
                    });
                    yield Ok(Bytes::from(format!("event: message_start\ndata: {}\n\n", msg_start)));
                }

                // Extract delta from choices
                let choices = parsed.get("choices").and_then(|c| c.as_array());
                if let Some(choices) = choices {
                    if let Some(choice) = choices.first() {
                        let delta = choice.get("delta");
                        let finish_reason = choice.get("finish_reason").and_then(|f| f.as_str());

                        // Debug: log every delta for troubleshooting
                        let has_reasoning = delta.and_then(|d| d.get("reasoning_content")).is_some();
                        let has_content = delta.and_then(|d| d.get("content")).map(|c| !c.is_null()).unwrap_or(false);
                        let has_tool_calls = delta.and_then(|d| d.get("tool_calls")).is_some();
                        log::debug!("[Stream] delta: reasoning={} content={} tools={} finish={:?} blk_idx={} thinking_open={} text_open={}",
                                has_reasoning, has_content, has_tool_calls, finish_reason,
                                st.block_index, st.thinking_block_open, st.text_block_open);

                        // Handle reasoning_content -> thinking block
                        if let Some(reasoning) = delta.and_then(|d| d.get("reasoning_content")).and_then(|r| r.as_str()) {
                            if !reasoning.is_empty() {
                                if !st.thinking_block_open {
                                    let start = json!({
                                        "type": "content_block_start",
                                        "index": st.block_index,
                                        "content_block": {"type": "thinking", "thinking": ""}
                                    });
                                    yield Ok(Bytes::from(format!("event: content_block_start\ndata: {}\n\n", start)));
                                    st.thinking_block_open = true;
                                }

                                let delta_event = json!({
                                    "type": "content_block_delta",
                                    "index": st.block_index,
                                    "delta": {"type": "thinking_delta", "thinking": reasoning}
                                });
                                yield Ok(Bytes::from(format!("event: content_block_delta\ndata: {}\n\n", delta_event)));
                            }
                        } else if st.thinking_block_open {
                            // reasoning_content went away -> close thinking block
                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                            st.thinking_block_open = false;
                            st.block_index += 1;
                        }

                        // Handle text content
                        if let Some(content) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
                            if !content.is_empty() {
                                if !st.text_block_open {
                                    // Close any open tool blocks before opening text block
                                    for i in 0..st.tool_blocks_open.len() {
                                        if st.tool_blocks_open[i] {
                                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                            st.tool_blocks_open[i] = false;
                                            st.block_index += 1;
                                        }
                                    }
                                    // Open text block
                                    let start = json!({
                                        "type": "content_block_start",
                                        "index": st.block_index,
                                        "content_block": {"type": "text", "text": ""}
                                    });
                                    yield Ok(Bytes::from(format!("event: content_block_start\ndata: {}\n\n", start)));
                                    st.text_block_open = true;
                                }

                                let delta_event = json!({
                                    "type": "content_block_delta",
                                    "index": st.block_index,
                                    "delta": {"type": "text_delta", "text": content}
                                });
                                yield Ok(Bytes::from(format!("event: content_block_delta\ndata: {}\n\n", delta_event)));
                            }
                        }

                        // Handle tool calls
                        if let Some(tool_calls) = delta.and_then(|d| d.get("tool_calls")).and_then(|t| t.as_array()) {
                            for tc in tool_calls {
                                let tc_index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;

                                // Ensure we have enough slots
                                while st.tool_blocks_open.len() <= tc_index {
                                    // Close thinking block if open
                                    if st.thinking_block_open {
                                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                        st.thinking_block_open = false;
                                        st.block_index += 1;
                                    }
                                    // Close text block if open
                                    if st.text_block_open {
                                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                        st.text_block_open = false;
                                        st.block_index += 1;
                                    }
                                    st.tool_blocks_open.push(false);
                                }

                                // Open tool block if not yet open
                                if !st.tool_blocks_open[tc_index] {
                                    // Close thinking block first if open
                                    if st.thinking_block_open {
                                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                        st.thinking_block_open = false;
                                        st.block_index += 1;
                                    }
                                    // Close text block first if open
                                    if st.text_block_open {
                                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                        st.text_block_open = false;
                                        st.block_index += 1;
                                    }
                                    let tool_name = tc.get("function")
                                        .and_then(|f| f.get("name"))
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("");
                                    let tool_id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                    let tool_start = json!({
                                        "type": "content_block_start",
                                        "index": st.block_index,
                                        "content_block": {
                                            "type": "tool_use",
                                            "id": tool_id,
                                            "name": tool_name,
                                            "input": {}
                                        }
                                    });
                                    yield Ok(Bytes::from(format!("event: content_block_start\ndata: {}\n\n", tool_start)));
                                    st.tool_blocks_open[tc_index] = true;
                                }

                                // Tool arguments delta
                                if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()) {
                                    if !args.is_empty() {
                                        let args_delta = json!({
                                            "type": "content_block_delta",
                                            "index": st.block_index,
                                            "delta": {
                                                "type": "input_json_delta",
                                                "partial_json": args
                                            }
                                        });
                                        yield Ok(Bytes::from(format!("event: content_block_delta\ndata: {}\n\n", args_delta)));
                                    }
                                }
                            }
                        }

                        // Handle finish_reason
                        if let Some(reason) = finish_reason {
                            if !reason.is_empty() && reason != "null" && !st.finished {
                                log::info!("[Stream] finish_reason={}, closing blocks (thinking={}, text={}, blk_idx={})",
                                    reason, st.thinking_block_open, st.text_block_open, st.block_index);
                                let stop_reason = match reason {
                                    "stop" => "end_turn",
                                    "tool_calls" => "tool_use",
                                    "length" => "max_tokens",
                                    _ => "end_turn",
                                };

                                // Close any open content blocks
                                if st.thinking_block_open {
                                    yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                    st.thinking_block_open = false;
                                    st.block_index += 1;
                                }
                                if st.text_block_open {
                                    yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                    st.text_block_open = false;
                                    st.block_index += 1;
                                }
                                let tool_count = st.tool_blocks_open.len();
                                let mut has_open_tool = false;
                                for i in 0..tool_count {
                                    if st.tool_blocks_open[i] {
                                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                        st.tool_blocks_open[i] = false;
                                        has_open_tool = true;
                                        st.block_index += 1;
                                    }
                                }

                                // If finish_reason is "tool_calls" but no tool blocks were actually
                                // created, fall back to "end_turn" to avoid confusing the client
                                let stop_reason = if stop_reason == "tool_use" && !has_open_tool {
                                    log::info!("[Stream] finish_reason was tool_calls but no tool blocks found, using end_turn");
                                    "end_turn"
                                } else {
                                    stop_reason
                                };

                                // Extract usage from final chunk if available
                                if let Some(u) = parsed.get("usage") {
                                    if let Some(ct) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
                                        st.usage_output_tokens = ct;
                                    }
                                }

                                let msg_delta = json!({
                                    "type": "message_delta",
                                    "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
                                    "usage": {"output_tokens": st.usage_output_tokens}
                                });
                                yield Ok(Bytes::from(format!("event: message_delta\ndata: {}\n\n", msg_delta)));
                                yield Ok(Bytes::from("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()));
                                st.finished = true;
                            }
                        }
                    }
                }
            }

            // Save remaining buffer
            *buffer.lock().await = local_buf;
        }
    });

    Box::pin(stream)
}

// ---------------------------------------------------------------------------
// Anthropic SSE -> OpenAI Chat SSE: Streaming conversion
// ---------------------------------------------------------------------------

/// Convert an Anthropic Messages SSE stream into an OpenAI Chat Completions SSE stream.
pub fn convert_anthropic_stream_to_openai(
    upstream: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    request_model: String,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    Box::pin(async_stream::stream! {
        let mut buffer = String::new();
        let mut event_type = String::new();
        let mut response_id = String::new();
        let mut model = request_model.clone();
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut stop_reason = "stop".to_string();
        let mut chunk_id = 0usize;
        let mut open_blocks: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut block_index_to_type: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        // Track tool calls per tool block
        let mut tool_blocks: std::collections::HashMap<u32, serde_json::Map<String, Value>> = std::collections::HashMap::new();
        // Anthropic content-block index → OpenAI 0-based tool_calls index.
        // Anthropic block index counts ALL blocks (text/thinking/tool_use), so a
        // tool at block index 2 must still be tool_calls[0]. Map them explicitly.
        let mut tool_index_map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut next_tool_index: u32 = 0;
        let mut usage_input_tokens: u64 = 0;
        let mut usage_output_tokens: u64 = 0;

        tokio::pin!(upstream);

        while let Some(chunk) = upstream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            let text = String::from_utf8_lossy(&bytes);
            buffer.push_str(&text);

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                buffer = buffer[newline_pos + 1..].to_string();
                let trimmed = line.trim().to_string();

                if trimmed.is_empty() { continue; }

                // Track event: type from line
                if let Some(ev) = trimmed.strip_prefix("event: ") {
                    event_type = ev.trim().to_string();
                    continue;
                }

                let data_str = if let Some(s) = trimmed.strip_prefix("data: ") {
                    s.trim().to_string()
                } else if let Some(s) = trimmed.strip_prefix("data:") {
                    s.trim().to_string()
                } else {
                    continue;
                };

                if data_str == "[DONE]" { continue; }

                let data: Value = match serde_json::from_str(&data_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Determine event type from event: line or data["type"]
                let ev = if !event_type.is_empty() {
                    event_type.clone()
                } else {
                    data.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string()
                };
                event_type.clear();

                match ev.as_str() {
                    "message_start" => {
                        if let Some(msg) = data.get("message") {
                            response_id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("msg_unknown").to_string();
                            model = request_model.clone();
                            if let Some(u) = msg.get("usage") {
                                usage_input_tokens = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            }
                        }
                    }
                    "content_block_start" => {
                        let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let block = data.get("content_block").unwrap_or(&Value::Null);
                        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        block_index_to_type.insert(idx, block_type.clone());

                        match block_type.as_str() {
                            "text" => {
                                let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                open_blocks.insert(idx, text.to_string());
                            }
                            "thinking" => {
                                let text = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                                open_blocks.insert(idx, text.to_string());
                            }
                            "tool_use" => {
                                let tc_id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let mut tc = serde_json::Map::new();
                                tc.insert("id".to_string(), Value::String(tc_id.clone()));
                                tc.insert("type".to_string(), Value::String("function".to_string()));
                                let mut func = serde_json::Map::new();
                                func.insert("name".to_string(), Value::String(name.clone()));
                                func.insert("arguments".to_string(), Value::String(String::new()));
                                tc.insert("function".to_string(), Value::Object(func));
                                tool_blocks.insert(idx, tc);
                                open_blocks.insert(idx, String::new());

                                // Assign a 0-based tool index and emit the standard
                                // OpenAI first tool_call delta (id + name + empty args)
                                // so clients can identify the call before args stream in.
                                let t_idx = next_tool_index;
                                next_tool_index += 1;
                                tool_index_map.insert(idx, t_idx);
                                let first_delta = json!([{
                                    "index": t_idx,
                                    "id": tc_id,
                                    "type": "function",
                                    "function": {"name": name, "arguments": ""}
                                }]);
                                let chat_chunk = chat_delta_chunk(&response_id, &model, created, chunk_id, false, "", Some(first_delta));
                                chunk_id += 1;
                                yield Ok(chat_chunk);
                            }
                            _ => {}
                        }
                    }
                    "content_block_delta" => {
                        let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let delta = data.get("delta").unwrap_or(&Value::Null);
                        let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");

                        match delta_type {
                            "text_delta" => {
                                if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                    let chat_chunk = chat_delta_chunk(&response_id, &model, created, chunk_id, false, text, None);
                                    chunk_id += 1;
                                    yield Ok(chat_chunk);
                                }
                            }
                            "thinking_delta" => {
                                if let Some(text) = delta.get("thinking").and_then(|v| v.as_str()) {
                                    if !text.is_empty() {
                                        let chat_chunk = chat_delta_chunk(&response_id, &model, created, chunk_id, true, text, None);
                                        chunk_id += 1;
                                        yield Ok(chat_chunk);
                                    }
                                }
                            }
                            "input_json_delta" => {
                                if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                                    // Accumulate internally (kept for completeness).
                                    if let Some(tc) = tool_blocks.get_mut(&idx) {
                                        if let Some(func) = tc.get_mut("function").and_then(|f| f.as_object_mut()) {
                                            let args = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                                            let new_args = args.to_string() + partial;
                                            func.insert("arguments".to_string(), Value::String(new_args));
                                        }
                                    }
                                    // Emit the incremental arguments delta only (standard OpenAI
                                    // streaming). Use the mapped 0-based tool index.
                                    if let Some(&t_idx) = tool_index_map.get(&idx) {
                                        let tc_delta = json!([{
                                            "index": t_idx,
                                            "function": {"arguments": partial}
                                        }]);
                                        let chat_chunk = chat_delta_chunk(&response_id, &model, created, chunk_id, false, "", Some(tc_delta));
                                        chunk_id += 1;
                                        yield Ok(chat_chunk);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let block_type = block_index_to_type.get(&idx).map(|s| s.as_str()).unwrap_or("");

                        if block_type == "tool_use" {
                            // Args already streamed incrementally via input_json_delta;
                            // do NOT re-emit a full-args chunk (it doubles the args for
                            // clients that accumulate, producing invalid JSON).
                        }

                        open_blocks.remove(&idx);
                        block_index_to_type.remove(&idx);
                        tool_blocks.remove(&idx);
                    }
                    "message_delta" => {
                        if let Some(delta) = data.get("delta") {
                            if let Some(reason) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                                stop_reason = reason.to_string();
                            }
                        }
                        if let Some(u) = data.get("usage") {
                            usage_output_tokens = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(usage_output_tokens);
                        }
                    }
                    "message_stop" => {
                        // Map Anthropic stop_reason to OpenAI finish_reason
                        let finish_reason = match stop_reason.as_str() {
                            "end_turn" | "stop_sequence" => "stop",
                            "tool_use" => "tool_calls",
                            "max_tokens" => "length",
                            _ => "stop",
                        };
                        let final_chunk = json!({
                            "id": response_id, "object": "chat.completion.chunk",
                            "created": created, "model": model,
                            "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}],
                            "usage": {"prompt_tokens": usage_input_tokens, "completion_tokens": usage_output_tokens, "total_tokens": usage_input_tokens + usage_output_tokens}
                        });
                        yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&final_chunk).unwrap_or_default())));
                        yield Ok(Bytes::from("data: [DONE]\n\n"));
                    }
                    _ => {}
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// OpenAI Responses SSE -> Anthropic SSE: Streaming conversion
// ---------------------------------------------------------------------------

/// State for the Responses SSE -> Anthropic SSE converter.
struct ResponsesStreamState {
    started: bool,
    finished: bool,
    block_index: u32,
    thinking_block_open: bool,
    text_block_open: bool,
    tool_block_open: bool,  // one tool at a time in Responses
    msg_id: String,
    model: String,
    usage_input_tokens: u64,
    usage_output_tokens: u64,
}

/// Convert an OpenAI Responses SSE byte stream into Anthropic SSE events.
pub fn convert_responses_stream_to_anthropic(
    upstream: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    request_model: String,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let state = Arc::new(Mutex::new(ResponsesStreamState {
        started: false,
        finished: false,
        block_index: 0,
        thinking_block_open: false,
        text_block_open: false,
        tool_block_open: false,
        msg_id: format!("msg_{:x}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()),
        model: request_model,
        usage_input_tokens: 0,
        usage_output_tokens: 0,
    }));

    let buffer = Arc::new(Mutex::new(String::new()));

    let stream = upstream.flat_map(move |chunk_result| {
        let state = state.clone();
        let buffer = buffer.clone();

        async_stream::stream! {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            let text = String::from_utf8_lossy(&chunk);
            let mut local_buf;
            {
                let mut guard = buffer.lock().await;
                guard.push_str(&text);
                local_buf = std::mem::take(&mut *guard);
            }

            while let Some(newline_pos) = local_buf.find('\n') {
                let line = local_buf[..newline_pos].trim().to_string();
                local_buf = local_buf[newline_pos + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                // Responses API uses SSE events like:
                //   event: response.output_text.delta
                //   data: {"type":"response.output_text.delta", ...}
                // We only parse the data lines; event lines are informational.
                let data = if let Some(stripped) = line.strip_prefix("data: ") {
                    stripped.to_string()
                } else if let Some(stripped) = line.strip_prefix("data:") {
                    stripped.trim().to_string()
                } else {
                    continue; // skip event: lines and others
                };

                if data == "[DONE]" {
                    let mut st = state.lock().await;
                    if st.finished {
                        continue;
                    }
                    // Close any open blocks
                    if st.thinking_block_open {
                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                        st.thinking_block_open = false;
                        st.block_index += 1;
                    }
                    if st.text_block_open {
                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                        st.text_block_open = false;
                        st.block_index += 1;
                    }
                    if st.tool_block_open {
                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                        st.tool_block_open = false;
                        st.block_index += 1;
                    }
                    st.finished = true;
                    let msg_delta = json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": "end_turn", "stop_sequence": Value::Null},
                        "usage": {"output_tokens": st.usage_output_tokens}
                    });
                    yield Ok(Bytes::from(format!("event: message_delta\ndata: {}\n\n", msg_delta)));
                    yield Ok(Bytes::from("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()));
                    continue;
                }

                let parsed: Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let event_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let mut st = state.lock().await;

                match event_type {
                    "response.created" | "response.in_progress" => {
                        if !st.started {
                            st.started = true;
                            // Extract response id and usage if available
                            if let Some(resp) = parsed.get("response") {
                                if let Some(id) = resp.get("id").and_then(|v| v.as_str()) {
                                    st.msg_id = id.to_string();
                                }
                                if let Some(u) = resp.get("usage") {
                                    st.usage_input_tokens = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                }
                            }
                            let msg_start = json!({
                                "type": "message_start",
                                "message": {
                                    "id": st.msg_id,
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [],
                                    "model": st.model,
                                    "stop_reason": Value::Null,
                                    "stop_sequence": Value::Null,
                                    "usage": {
                                        "input_tokens": st.usage_input_tokens,
                                        "output_tokens": 1,
                                        "cache_creation_input_tokens": 0,
                                        "cache_read_input_tokens": 0
                                    }
                                }
                            });
                            yield Ok(Bytes::from(format!("event: message_start\ndata: {}\n\n", msg_start)));
                        }
                    }
                    "response.output_text.delta" => {
                        if let Some(delta_text) = parsed.get("delta").and_then(|d| d.as_str()) {
                            if !delta_text.is_empty() {
                                if !st.text_block_open {
                                    // Close thinking block first if open
                                    if st.thinking_block_open {
                                        yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                        st.thinking_block_open = false;
                                        st.block_index += 1;
                                    }
                                    let start = json!({
                                        "type": "content_block_start",
                                        "index": st.block_index,
                                        "content_block": {"type": "text", "text": ""}
                                    });
                                    yield Ok(Bytes::from(format!("event: content_block_start\ndata: {}\n\n", start)));
                                    st.text_block_open = true;
                                }

                                let delta_event = json!({
                                    "type": "content_block_delta",
                                    "index": st.block_index,
                                    "delta": {"type": "text_delta", "text": delta_text}
                                });
                                yield Ok(Bytes::from(format!("event: content_block_delta\ndata: {}\n\n", delta_event)));
                            }
                        }
                    }
                    "response.output_text.done" => {
                        if st.text_block_open {
                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                            st.text_block_open = false;
                            st.block_index += 1;
                        }
                    }
                    "response.output_item.added" => {
                        let item_type = parsed.get("item")
                            .and_then(|i| i.get("type"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("");

                        if item_type == "function_call" {
                            // Close thinking block if open
                            if st.thinking_block_open {
                                yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                st.thinking_block_open = false;
                                st.block_index += 1;
                            }
                            // Close text block if open
                            if st.text_block_open {
                                yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                st.text_block_open = false;
                                st.block_index += 1;
                            }

                            let call_id = parsed.get("item")
                                .and_then(|i| i.get("call_id"))
                                .and_then(|c| c.as_str())
                                .unwrap_or("");
                            let name = parsed.get("item")
                                .and_then(|i| i.get("name"))
                                .and_then(|n| n.as_str())
                                .unwrap_or("");

                            let tool_start = json!({
                                "type": "content_block_start",
                                "index": st.block_index,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": call_id,
                                    "name": name,
                                    "input": {}
                                }
                            });
                            yield Ok(Bytes::from(format!("event: content_block_start\ndata: {}\n\n", tool_start)));
                            st.tool_block_open = true;
                        }
                        // For "message" type items, we don't need to do anything special
                        // text deltas will handle the content
                    }
                    "response.function_call_arguments.delta" => {
                        if let Some(args_delta) = parsed.get("delta").and_then(|d| d.as_str()) {
                            if !args_delta.is_empty() {
                                let args_event = json!({
                                    "type": "content_block_delta",
                                    "index": st.block_index,
                                    "delta": {
                                        "type": "input_json_delta",
                                        "partial_json": args_delta
                                    }
                                });
                                yield Ok(Bytes::from(format!("event: content_block_delta\ndata: {}\n\n", args_event)));
                            }
                        }
                    }
                    "response.function_call_arguments.done" | "response.output_item.done" => {
                        // Close tool block if this is a function_call item done
                        if st.tool_block_open {
                            // Only close on output_item.done for function_call
                            if event_type == "response.output_item.done" {
                                let item_type = parsed.get("item")
                                    .and_then(|i| i.get("type"))
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("");
                                if item_type == "function_call" {
                                    yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                                    st.tool_block_open = false;
                                    st.block_index += 1;
                                }
                            }
                        }
                    }
                    "response.completed" => {
                        if st.finished {
                            continue;
                        }
                        // Extract usage if available
                        if let Some(u) = parsed.get("response").and_then(|r| r.get("usage")) {
                            st.usage_output_tokens = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(st.usage_output_tokens);
                        }

                        // Close any remaining open blocks
                        if st.thinking_block_open {
                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                            st.thinking_block_open = false;
                            st.block_index += 1;
                        }
                        if st.text_block_open {
                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                            st.text_block_open = false;
                            st.block_index += 1;
                        }
                        if st.tool_block_open {
                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                            st.tool_block_open = false;
                            st.block_index += 1;
                        }

                        // Determine stop_reason from the response
                        let stop_reason = parsed.get("response")
                            .and_then(|r| r.get("status"))
                            .and_then(|s| s.as_str())
                            .map(|s| match s {
                                "completed" => "end_turn",
                                "incomplete" => "max_tokens",
                                _ => "end_turn",
                            })
                            .unwrap_or("end_turn");

                        // Check for function_calls in output
                        let has_tool_calls = parsed.get("response")
                            .and_then(|r| r.get("output"))
                            .and_then(|o| o.as_array())
                            .map(|arr| arr.iter().any(|item| item.get("type").and_then(|t| t.as_str()) == Some("function_call")))
                            .unwrap_or(false);
                        let stop_reason = if has_tool_calls { "tool_use" } else { stop_reason };

                        st.finished = true;
                        let msg_delta = json!({
                            "type": "message_delta",
                            "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
                            "usage": {"output_tokens": st.usage_output_tokens}
                        });
                        yield Ok(Bytes::from(format!("event: message_delta\ndata: {}\n\n", msg_delta)));
                        yield Ok(Bytes::from("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()));
                    }
                    "response.reasoning_summary_text.delta" => {
                        if let Some(delta_text) = parsed.get("delta").and_then(|d| d.as_str()) {
                            if !delta_text.is_empty() {
                                if !st.thinking_block_open {
                                    let start = json!({
                                        "type": "content_block_start",
                                        "index": st.block_index,
                                        "content_block": {"type": "thinking", "thinking": ""}
                                    });
                                    yield Ok(Bytes::from(format!("event: content_block_start\ndata: {}\n\n", start)));
                                    st.thinking_block_open = true;
                                }
                                let delta_event = json!({
                                    "type": "content_block_delta",
                                    "index": st.block_index,
                                    "delta": {"type": "thinking_delta", "thinking": delta_text}
                                });
                                yield Ok(Bytes::from(format!("event: content_block_delta\ndata: {}\n\n", delta_event)));
                            }
                        }
                    }
                    "response.reasoning_summary_text.done" => {
                        if st.thinking_block_open {
                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", st.block_index)));
                            st.thinking_block_open = false;
                            st.block_index += 1;
                        }
                    }
                    _ => {
                        // Ignore other event types (response.content_part.added, etc.)
                    }
                }
            }

            *buffer.lock().await = local_buf;
        }
    });

    Box::pin(stream)
}

// ---------------------------------------------------------------------------
// Codex: Chat Completions SSE -> Responses API SSE (streaming)
// ---------------------------------------------------------------------------

/// Convert a Chat Completions SSE stream into a Responses API SSE stream.
pub fn convert_chat_stream_to_responses(
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    request_model: &str,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let request_model = request_model.to_string();
    let stream = async_stream::stream! {
        let mut buffer = String::new();
        let mut state = CodexChatToResponsesState::default();
        // Always report the request model to the client, never the upstream model
        state.model = request_model.clone();
        let mut stream_failed = false;
        let mut event_count: u64 = 0;

        tokio::pin!(stream);

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    buffer.push_str(&text);

                    while let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                        buffer = buffer[newline_pos + 1..].to_string();

                        let data_line = if let Some(stripped) = line.strip_prefix("data: ") {
                            stripped.trim().to_string()
                        } else if let Some(stripped) = line.strip_prefix("data:") {
                            stripped.trim().to_string()
                        } else {
                            continue;
                        };
                        if data_line == "[DONE]" {
                            for event in state.finalize() {
                                event_count += 1;
                                let event_str = String::from_utf8_lossy(&event);
                                log::info!("[Chat->Responses] SSE event #{} (DONE finalize): {}", event_count, event_str.lines().next().unwrap_or(""));
                                yield Ok(event);
                            }
                            continue;
                        }
                        let chunk: Value = match serde_json::from_str(&data_line) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        if chunk.get("error").is_some() {
                            log::warn!("[Chat->Responses] upstream error in chunk");
                            yield Ok(state.failed_event("Upstream error".to_string()));
                            stream_failed = true;
                            break;
                        }
                        for event in state.handle_chat_chunk(&chunk) {
                            event_count += 1;
                            let event_str = String::from_utf8_lossy(&event);
                            log::info!("[Chat->Responses] SSE event #{}: {}", event_count, event_str.lines().next().unwrap_or(""));
                            yield Ok(event);
                        }
                    }

                    if stream_failed { break; }
                }
                Err(e) => {
                    log::warn!("[Chat->Responses] stream error: {e}");
                    yield Ok(state.failed_event(format!("Stream error: {e}")));
                    stream_failed = true;
                    break;
                }
            }
        }

        if !stream_failed {
            for event in state.finalize() {
                event_count += 1;
                let event_str = String::from_utf8_lossy(&event);
                log::info!("[Chat->Responses] SSE event #{} (finalize): {}", event_count, event_str.lines().next().unwrap_or(""));
                yield Ok(event);
            }
        }
    };

    Box::pin(stream)
}

// ---------------------------------------------------------------------------
// Responses API SSE -> OpenAI Chat Completions SSE (streaming)
// ---------------------------------------------------------------------------

/// Convert an OpenAI Responses API SSE stream into an OpenAI Chat Completions SSE stream.
pub fn convert_responses_stream_to_chat(
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    request_model: &str,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let request_model = request_model.to_string();
    Box::pin(async_stream::stream! {
        let mut buffer = String::new();
        let mut response_id = String::new();
        let mut created = 0u64;
        let mut model = request_model.clone();
        let mut _first_chunk_sent = false;
        let mut chunk_id = 0usize;
        let mut event_type = String::new();

        tokio::pin!(stream);

        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            let text = String::from_utf8_lossy(&bytes);
            buffer.push_str(&text);

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                buffer = buffer[newline_pos + 1..].to_string();
                let trimmed = line.trim().to_string();

                if trimmed.is_empty() {
                    continue;
                }

                // Track event type from event: line
                if let Some(ev) = trimmed.strip_prefix("event: ") {
                    event_type = ev.trim().to_string();
                    continue;
                }

                // Extract data: content
                let data_str = if let Some(s) = trimmed.strip_prefix("data: ") {
                    s.trim().to_string()
                } else if let Some(s) = trimmed.strip_prefix("data:") {
                    s.trim().to_string()
                } else {
                    continue;
                };

                if data_str == "[DONE]" {
                    continue;
                }

                let data: Value = match serde_json::from_str(&data_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Determine the event name: either from event: line or from data["type"]
                let ev = if !event_type.is_empty() {
                    event_type.clone()
                } else {
                    data.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string()
                };
                event_type.clear();

                match ev.as_str() {
                    "response.created" | "response.in_progress" => {
                        if let Some(resp) = data.get("response") {
                            if response_id.is_empty() {
                                response_id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            }
                            if created == 0 {
                                created = resp.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0);
                            }
                            model = resp.get("model").and_then(|v| v.as_str()).unwrap_or(&request_model).to_string();
                        }
                    }
                    "response.reasoning_summary_text.delta" => {
                        // Map summary text delta -> reasoning_content in Chat
                        if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                            if !delta.is_empty() {
                                let chat_chunk = chat_delta_chunk(&response_id, &model, created, chunk_id, true, delta, None);
                                chunk_id += 1;
                                yield Ok(chat_chunk);
                            }
                        }
                    }
                    "response.output_text.delta" => {
                        if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                            if !delta.is_empty() {
                                let chat_chunk = chat_delta_chunk(&response_id, &model, created, chunk_id, false, delta, None);
                                chunk_id += 1;
                                yield Ok(chat_chunk);
                            }
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                            if !delta.is_empty() {
                                // Tool call index comes from output_index
                                let tool_idx = data.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0);
                                let tc = json!([{
                                    "index": tool_idx,
                                    "function": {"arguments": delta, "name": null}
                                }]);
                                let chat_chunk = chat_delta_chunk(&response_id, &model, created, chunk_id, false, "", Some(tc));
                                chunk_id += 1;
                                yield Ok(chat_chunk);
                            }
                        }
                    }
                    "response.output_item.added" => {
                        let item_type = data.get("item").and_then(|v| v.get("type")).and_then(|v| v.as_str()).unwrap_or("");
                        if item_type == "function_call" {
                            let idx = data.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0);
                            let name = data.get("item").and_then(|v| v.get("name")).and_then(|v| v.as_str()).unwrap_or("");
                            let tc = json!([{
                                "index": idx,
                                "id": data.get("item").and_then(|v| v.get("id")).and_then(|v| v.as_str()).unwrap_or(""),
                                "type": "function",
                                "function": {"name": name, "arguments": ""}
                            }]);
                            let chat_chunk = chat_delta_chunk(&response_id, &model, created, chunk_id, false, "", Some(tc));
                            chunk_id += 1;
                            yield Ok(chat_chunk);
                        }
                    }
                    "response.completed" => {
                        // Determine finish_reason from status
                        let status = data.get("response").and_then(|v| v.get("status")).and_then(|v| v.as_str()).unwrap_or("completed");
                        let finish_reason = match status {
                            "incomplete" => Some("length"),
                            "failed" => Some("stop"),
                            _ => {
                                // Check for function_call items -> tool_calls
                                let has_tools = data.get("response")
                                    .and_then(|v| v.get("output"))
                                    .and_then(|v| v.as_array())
                                    .map(|arr| arr.iter().any(|item| item.get("type").and_then(|t| t.as_str()) == Some("function_call")))
                                    .unwrap_or(false);
                                if has_tools { Some("tool_calls") } else { Some("stop") }
                            }
                        };
                        // Final chunk with empty delta and finish_reason
                        let final_chunk = json!({
                            "id": response_id, "object": "chat.completion.chunk",
                            "created": created, "model": model,
                            "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}]
                        });
                        yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&final_chunk).unwrap_or_default())));
                        yield Ok(Bytes::from("data: [DONE]\n\n"));
                    }
                    "response.failed" => {
                        let err_msg = data.get("response")
                            .and_then(|v| v.get("error"))
                            .and_then(|v| v.get("message"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("stream failed");
                        // Emit error as a Chat completion chunk with finish_reason
                        let err_chunk = json!({
                            "id": response_id, "object": "chat.completion.chunk",
                            "created": created, "model": model,
                            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
                        });
                        yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&err_chunk).unwrap_or_default())));
                        yield Ok(Bytes::from("data: [DONE]\n\n"));
                        log::warn!("[Responses->Chat] stream failed: {}", err_msg);
                    }
                    _ => {}
                }
            }
        }
    })
}

/// Build a Chat Completions SSE delta chunk.
fn chat_delta_chunk(id: &str, model: &str, created: u64, chunk_id: usize, is_reasoning: bool, content: &str, tool_calls: Option<Value>) -> Bytes {
    let mut delta = serde_json::Map::new();
    if chunk_id == 0 || content.is_empty() {
        // First chunk includes role
        delta.insert("role".to_string(), Value::String("assistant".to_string()));
    }
    if is_reasoning {
        delta.insert("reasoning_content".to_string(), Value::String(content.to_string()));
    } else if !content.is_empty() {
        delta.insert("content".to_string(), Value::String(content.to_string()));
    }
    if let Some(tc) = tool_calls {
        delta.insert("tool_calls".to_string(), tc);
    }

    let chunk = json!({
        "id": id, "object": "chat.completion.chunk",
        "created": created, "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": Value::Null}]
    });
    Bytes::from(format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap_or_default()))
}

/// State machine for Chat SSE -> Responses SSE conversion.
#[derive(Default)]
struct CodexChatToResponsesState {
    response_started: bool,
    completed: bool,
    response_id: String,
    model: String,
    created_at: u64,
    next_output_index: u32,
    sequence_number: u64,
    text_started: bool,
    text_output_index: u32,
    text_item_id: String,
    text_content: String,
    reasoning_started: bool,
    reasoning_output_index: u32,
    reasoning_item_id: String,
    reasoning_content: String,
    tools: std::collections::BTreeMap<usize, CodexToolCallState>,
    output_items: Vec<Value>,
    latest_usage: Option<Value>,
    finish_reason: Option<String>,
}

#[derive(Default)]
struct CodexToolCallState {
    output_index: Option<u32>,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    added: bool,
}

impl CodexChatToResponsesState {
    fn next_output_index(&mut self) -> u32 {
        let idx = self.next_output_index;
        self.next_output_index += 1;
        idx
    }

    fn sse_event(&mut self, event: &str, data: Value) -> Bytes {
        self.sequence_number += 1;
        // Responses API SSE events require "type" and "sequence_number".
        // For response lifecycle events, wrap the response object in "response" field.
        let payload = if matches!(event, "response.created" | "response.in_progress" | "response.completed" | "response.failed") {
            serde_json::json!({
                "type": event,
                "sequence_number": self.sequence_number,
                "response": data
            })
        } else {
            match data {
                Value::Object(mut map) => {
                    map.insert("type".to_string(), Value::String(event.to_string()));
                    map.insert("sequence_number".to_string(), Value::Number(self.sequence_number.into()));
                    Value::Object(map)
                }
                other => {
                    serde_json::json!({
                        "type": event,
                        "sequence_number": self.sequence_number,
                        "data": other
                    })
                }
            }
        };
        Bytes::from(format!("event: {event}\ndata: {}\n\n", serde_json::to_string(&payload).unwrap_or_default()))
    }

    fn ensure_response_started(&mut self) -> Vec<Bytes> {
        if self.response_started { return Vec::new(); }
        self.response_started = true;
        let response = json!({
            "id": self.response_id, "object": "response", "created_at": self.created_at,
            "status": "in_progress", "model": self.model, "output": []
        });
        vec![
            self.sse_event("response.created", response.clone()),
            self.sse_event("response.in_progress", response),
        ]
    }

    fn handle_chat_chunk(&mut self, chunk: &Value) -> Vec<Bytes> {
        let mut events = Vec::new();

        if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
            self.response_id = codex_response_id_from_chat_id(Some(id));
        }
        // Do NOT override model -- keep the request model so Codex sees
        // the model name it sent, not the upstream provider's model name.
        if let Some(created) = chunk.get("created").and_then(|v| v.as_u64()) {
            self.created_at = created;
        }

        events.extend(self.ensure_response_started());

        if let Some(usage) = chunk.get("usage").filter(|v| !v.is_null()) {
            self.latest_usage = Some(codex_chat_usage_to_responses_usage(Some(usage)));
        }

        let Some(choice) = chunk.get("choices").and_then(|v| v.as_array()).and_then(|c| c.first()) else {
            return events;
        };

        if let Some(delta) = choice.get("delta") {
            // Reasoning content
            for key in &["reasoning_content", "reasoning"] {
                if let Some(text) = delta.get(*key).and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        events.extend(self.push_reasoning_delta(text));
                    }
                }
            }

            // Text content
            if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                if !content.is_empty() {
                    events.extend(self.push_text_delta(content));
                }
            }

            // Tool calls
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                events.extend(self.finalize_reasoning());
                for tc in tool_calls {
                    events.extend(self.push_tool_call_delta(tc));
                }
            }
        }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            self.finish_reason = Some(finish_reason.to_string());
        }

        events
    }

    fn push_reasoning_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        if !self.reasoning_started {
            let idx = self.next_output_index();
            let item_id = format!("rs_{}", self.response_id);
            self.reasoning_started = true;
            self.reasoning_output_index = idx;
            self.reasoning_item_id = item_id.clone();
            events.push(self.sse_event("response.output_item.added", json!({
                "output_index": idx, "item": {"id": item_id, "type": "reasoning", "status": "in_progress", "summary": []}
            })));
            events.push(self.sse_event("response.reasoning_summary_part.added", json!({
                "item_id": self.reasoning_item_id, "output_index": idx, "summary_index": 0,
                "part": {"type": "summary_text", "text": ""}
            })));
        }
        self.reasoning_content.push_str(delta);
        events.push(self.sse_event("response.reasoning_summary_text.delta", json!({
            "item_id": self.reasoning_item_id, "output_index": self.reasoning_output_index,
            "summary_index": 0, "delta": delta
        })));
        events
    }

    fn finalize_reasoning(&mut self) -> Vec<Bytes> {
        if !self.reasoning_started { return Vec::new(); }
        let idx = self.reasoning_output_index;
        let text = std::mem::take(&mut self.reasoning_content);
        let item = json!({"id": self.reasoning_item_id, "type": "reasoning",
            "summary": [{"type": "summary_text", "text": text}]});
        self.output_items.push(item.clone());
        self.reasoning_started = false;
        vec![
            self.sse_event("response.reasoning_summary_text.done", json!({
                "item_id": self.reasoning_item_id, "output_index": idx, "summary_index": 0, "text": text
            })),
            self.sse_event("response.reasoning_summary_part.done", json!({
                "item_id": self.reasoning_item_id, "output_index": idx, "summary_index": 0,
                "part": {"type": "summary_text", "text": text}
            })),
            self.sse_event("response.output_item.done", json!({"output_index": idx, "item": item})),
        ]
    }

    fn push_text_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        if !self.text_started {
            events.extend(self.finalize_reasoning());
            let idx = self.next_output_index();
            let item_id = format!("{}_msg", self.response_id);
            self.text_started = true;
            self.text_output_index = idx;
            self.text_item_id = item_id.clone();
            events.push(self.sse_event("response.output_item.added", json!({
                "output_index": idx, "item": {"id": item_id, "type": "message", "status": "in_progress", "role": "assistant", "content": []}
            })));
            events.push(self.sse_event("response.content_part.added", json!({
                "item_id": self.text_item_id, "output_index": idx, "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []}
            })));
        }
        self.text_content.push_str(delta);
        events.push(self.sse_event("response.output_text.delta", json!({
            "item_id": self.text_item_id, "output_index": self.text_output_index,
            "content_index": 0, "delta": delta
        })));
        events
    }

    fn finalize_text(&mut self) -> Vec<Bytes> {
        if !self.text_started { return Vec::new(); }
        let idx = self.text_output_index;
        let text = std::mem::take(&mut self.text_content);
        let part = json!({"type": "output_text", "text": text, "annotations": []});
        let item = json!({"id": self.text_item_id, "type": "message", "status": "completed", "role": "assistant", "content": [part]});
        self.output_items.push(item.clone());
        self.text_started = false;
        vec![
            self.sse_event("response.output_text.done", json!({
                "item_id": self.text_item_id, "output_index": idx, "content_index": 0, "text": text
            })),
            self.sse_event("response.content_part.done", json!({
                "item_id": self.text_item_id, "output_index": idx, "content_index": 0, "part": part
            })),
            self.sse_event("response.output_item.done", json!({"output_index": idx, "item": item})),
        ]
    }

    fn push_tool_call_delta(&mut self, tool_call: &Value) -> Vec<Bytes> {
        let chat_index = tool_call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let id_delta = tool_call.get("id").and_then(|v| v.as_str()).map(str::to_string);
        let function = tool_call.get("function").unwrap_or(&Value::Null);
        let name_delta = function.get("name").and_then(|v| v.as_str()).map(str::to_string);
        let args_delta = function.get("arguments").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // Update state first, then extract data for events
        let old_len;
        {
            let state = self.tools.entry(chat_index).or_default();
            if let Some(id) = id_delta { state.call_id = id; }
            if let Some(name) = name_delta { state.name = name; }
            old_len = state.arguments.len();
            if !args_delta.is_empty() {
                // Some providers (DeepSeek, GLM) send the complete arguments
                // JSON in every tool_call delta instead of streaming it
                // incrementally.  When the delta string starts with what we
                // already have, treat it as a replacement rather than
                // appending — otherwise downstream clients (Codex) receive
                // duplicated fragments that concatenate into invalid JSON.
                if state.arguments.is_empty() || args_delta.starts_with(&state.arguments) {
                    state.arguments = args_delta;
                } else {
                    state.arguments.push_str(&args_delta);
                }
            }
        }

        let mut events = Vec::new();
        let state = self.tools.get(&chat_index).unwrap();

        if !state.added && (!state.call_id.is_empty() || !state.name.is_empty()) {
            // Need to add this tool -- extract data then mutate
            let mut call_id = state.call_id.clone();
            let mut name = state.name.clone();
            let arguments = state.arguments.clone();
            let _ = state;

            let assigned = self.next_output_index();
            if call_id.is_empty() { call_id = format!("call_{chat_index}"); }
            if name.is_empty() { name = "unknown_tool".to_string(); }
            let item_id = format!("fc_{}", call_id);

            let state = self.tools.get_mut(&chat_index).unwrap();
            state.added = true;
            state.call_id = call_id.clone();
            state.name = name.clone();
            state.output_index = Some(assigned);
            state.item_id = item_id.clone();

            events.push(self.sse_event("response.output_item.added", json!({
                "output_index": assigned, "item": {
                    "id": item_id, "type": "function_call", "status": "in_progress",
                    "call_id": call_id, "name": name, "arguments": ""
                }
            })));

            if !arguments.is_empty() {
                events.push(self.sse_event("response.function_call_arguments.delta", json!({
                    "item_id": item_id, "output_index": assigned, "delta": arguments
                })));
            }
        } else {
            let added = state.added;
            let item_id = state.item_id.clone();
            let output_index = state.output_index;
            // Emit only the NEW bytes beyond what was already accumulated
            // before this chunk, so downstream clients receive valid
            // incremental JSON regardless of whether the provider streams
            // incrementally or sends complete args each time.
            let new_bytes = &state.arguments[old_len..];
            let _ = state;

            if added && !new_bytes.is_empty() {
                if let Some(output_index) = output_index {
                    events.push(self.sse_event("response.function_call_arguments.delta", json!({
                        "item_id": item_id, "output_index": output_index, "delta": new_bytes
                    })));
                }
            }
        }

        events
    }

    fn finalize_tools(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        let keys: Vec<usize> = self.tools.keys().copied().collect();
        for key in keys {
            // Clone data first to avoid borrow conflicts
            let tc = self.tools.get(&key).unwrap();
            let tc_call_id = tc.call_id.clone();
            let tc_name = tc.name.clone();
            let tc_arguments = tc.arguments.clone();
            let tc_output_index = tc.output_index;
            let tc_item_id = tc.item_id.clone();
            let _ = tc;

            let item = json!({
                "id": tc_item_id, "type": "function_call", "status": "completed",
                "call_id": tc_call_id, "name": tc_name, "arguments": tc_arguments
            });
            self.output_items.push(item.clone());
            events.push(self.sse_event("response.function_call_arguments.done", json!({
                "item_id": tc_item_id, "output_index": tc_output_index, "arguments": tc_arguments
            })));
            events.push(self.sse_event("response.output_item.done", json!({"output_index": tc_output_index, "item": item})));
        }
        events
    }

    fn finalize(&mut self) -> Vec<Bytes> {
        if self.completed { return Vec::new(); }
        let mut events = self.ensure_response_started();
        events.extend(self.finalize_reasoning());
        events.extend(self.finalize_text());
        events.extend(self.finalize_tools());

        let status = match self.finish_reason.as_deref() {
            Some("length") => "incomplete",
            _ => "completed",
        };
        let usage = self.latest_usage.clone().unwrap_or_else(|| json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}));
        let mut response = json!({
            "id": self.response_id, "object": "response", "created_at": self.created_at,
            "status": status, "model": self.model, "output": self.output_items, "usage": usage
        });
        if status == "incomplete" {
            response["incomplete_details"] = json!({"reason": "max_output_tokens"});
        }
        events.push(self.sse_event("response.completed", response));
        self.completed = true;
        events
    }

    fn failed_event(&mut self, message: String) -> Bytes {
        self.completed = true;
        self.sse_event("response.failed", json!({
            "id": self.response_id, "object": "response", "status": "failed",
            "error": {"message": message}
        }))
    }
}

// ---------------------------------------------------------------------------
// Codex: Anthropic Messages SSE -> Responses API SSE (streaming)
// ---------------------------------------------------------------------------

/// Convert an Anthropic Messages SSE stream into a Responses API SSE stream.
pub fn convert_anthropic_stream_to_responses(
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    request_model: &str,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let request_model = request_model.to_string();
    let stream = async_stream::stream! {
        let mut buffer = String::new();
        let mut state = CodexAnthropicToResponsesState::default();
        // Always report the request model to the client, never the upstream model
        state.model = request_model.clone();
        let mut stream_failed = false;
        let mut event_count: u64 = 0;

        tokio::pin!(stream);

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    buffer.push_str(&text);

                    while let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                        buffer = buffer[newline_pos + 1..].to_string();

                        let data_line = if let Some(stripped) = line.strip_prefix("data: ") {
                            stripped.trim().to_string()
                        } else if let Some(stripped) = line.strip_prefix("data:") {
                            stripped.trim().to_string()
                        } else {
                            continue;
                        };
                        if data_line == "[DONE]" {
                            for event in state.finalize() {
                                event_count += 1;
                                let event_str = String::from_utf8_lossy(&event);
                                log::info!("[Anthropic->Responses] SSE event #{}: {}", event_count, event_str.lines().next().unwrap_or(""));
                                yield Ok(event);
                            }
                            continue;
                        }
                        let chunk: Value = match serde_json::from_str(&data_line) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        if chunk.get("error").is_some() {
                            log::warn!("[Anthropic->Responses] upstream error in chunk");
                            yield Ok(state.failed_event("Upstream error".to_string()));
                            stream_failed = true;
                            break;
                        }
                        // Detect event type from the data's "type" field
                        let event_type = chunk.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        for event in state.handle_anthropic_event(event_type, &chunk) {
                            event_count += 1;
                            let event_str = String::from_utf8_lossy(&event);
                            log::info!("[Anthropic->Responses] SSE event #{}: {}", event_count, event_str.lines().next().unwrap_or(""));
                            yield Ok(event);
                        }
                    }

                    if stream_failed { break; }
                }
                Err(e) => {
                    log::warn!("[Anthropic->Responses] stream error: {e}");
                    yield Ok(state.failed_event(format!("Stream error: {e}")));
                    stream_failed = true;
                    break;
                }
            }
        }

        if !stream_failed {
            for event in state.finalize() {
                event_count += 1;
                let event_str = String::from_utf8_lossy(&event);
                log::info!("[Anthropic->Responses] SSE event #{} (finalize): {}", event_count, event_str.lines().next().unwrap_or(""));
                yield Ok(event);
            }
        }
    };

    Box::pin(stream)
}

/// State machine for Anthropic SSE -> Responses SSE conversion.
#[derive(Default)]
struct CodexAnthropicToResponsesState {
    response_id: String,
    model: String,
    created_at: u64,
    output_index: u32,
    output_items: Vec<Value>,
    input_tokens: u64,
    output_tokens: u64,
    response_started: bool,
    stream_terminated: bool,
    sequence_number: u64,
    stop_reason: Option<String>,
    phase: AnthropicPhase,
    current_reasoning: Option<AnthropicItemState>,
    current_message: Option<AnthropicItemState>,
    current_tool: Option<AnthropicToolCallState>,
}

#[derive(Default, PartialEq)]
enum AnthropicPhase {
    #[default]
    Idle,
    Reasoning,
    Message,
    ToolCall,
}

struct AnthropicItemState {
    id: String,
    output_index: u32,
    text: String,
}

struct AnthropicToolCallState {
    id: String,
    name: String,
    args: String,
    output_index: u32,
}

impl CodexAnthropicToResponsesState {
    fn next_output_index(&mut self) -> u32 {
        let idx = self.output_index;
        self.output_index += 1;
        idx
    }

    fn sse_event(&mut self, event: &str, data: Value) -> Bytes {
        self.sequence_number += 1;
        // Responses API SSE events require "type" and "sequence_number".
        // For response lifecycle events (created/in_progress/completed/failed),
        // the response object must be wrapped in a "response" field.
        // For other events (delta/done/added), the fields are at the top level.
        let payload = if matches!(event, "response.created" | "response.in_progress" | "response.completed" | "response.failed") {
            serde_json::json!({
                "type": event,
                "sequence_number": self.sequence_number,
                "response": data
            })
        } else {
            match data {
                Value::Object(mut map) => {
                    map.insert("type".to_string(), Value::String(event.to_string()));
                    map.insert("sequence_number".to_string(), Value::Number(self.sequence_number.into()));
                    Value::Object(map)
                }
                other => {
                    serde_json::json!({
                        "type": event,
                        "sequence_number": self.sequence_number,
                        "data": other
                    })
                }
            }
        };
        Bytes::from(format!("event: {event}\ndata: {}\n\n", serde_json::to_string(&payload).unwrap_or_default()))
    }

    fn ensure_response_started(&mut self) -> Vec<Bytes> {
        if self.response_started { return Vec::new(); }
        self.response_started = true;
        if self.response_id.is_empty() {
            self.response_id = format!("resp_{}", chrono_like_id());
        }
        if self.created_at == 0 {
            self.created_at = chrono_like_id() / 1000;
        }
        let response = json!({
            "id": self.response_id, "object": "response", "created_at": self.created_at,
            "status": "in_progress", "model": self.model, "output": []
        });
        vec![
            self.sse_event("response.created", response.clone()),
            self.sse_event("response.in_progress", response),
        ]
    }

    fn start_reasoning_item(&mut self) -> Vec<Bytes> {
        let idx = self.next_output_index();
        let item_id = format!("rs_{}_{}", chrono_like_id(), idx);
        self.current_reasoning = Some(AnthropicItemState { id: item_id.clone(), output_index: idx, text: String::new() });
        self.phase = AnthropicPhase::Reasoning;
        vec![
            self.sse_event("response.output_item.added", json!({
                "output_index": idx, "item": {"id": item_id, "type": "reasoning", "status": "in_progress", "summary": []}
            })),
            self.sse_event("response.reasoning_summary_part.added", json!({
                "item_id": item_id, "output_index": idx, "summary_index": 0,
                "part": {"type": "summary_text", "text": ""}
            })),
        ]
    }

    fn append_reasoning_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let (output_index, item_id) = if let Some(ref item) = self.current_reasoning {
            (item.output_index, item.id.clone())
        } else {
            return Vec::new();
        };
        if let Some(ref mut item) = self.current_reasoning {
            item.text.push_str(delta);
        }
        vec![self.sse_event("response.reasoning_summary_text.delta", json!({
            "item_id": item_id, "output_index": output_index, "summary_index": 0, "delta": delta
        }))]
    }

    fn close_reasoning_item(&mut self) -> Vec<Bytes> {
        if let Some(item) = self.current_reasoning.take() {
            let completed = json!({
                "id": item.id, "type": "reasoning", "status": "completed",
                "summary": [{"type": "summary_text", "text": item.text}]
            });
            self.output_items.push(completed.clone());
            vec![
                self.sse_event("response.reasoning_summary_text.done", json!({
                    "item_id": item.id, "output_index": item.output_index, "summary_index": 0, "text": item.text
                })),
                self.sse_event("response.reasoning_summary_part.done", json!({
                    "item_id": item.id, "output_index": item.output_index, "summary_index": 0,
                    "part": {"type": "summary_text", "text": item.text}
                })),
                self.sse_event("response.output_item.done", json!({"output_index": item.output_index, "item": completed})),
            ]
        } else { Vec::new() }
    }

    fn start_message_item(&mut self) -> Vec<Bytes> {
        let idx = self.next_output_index();
        let item_id = format!("msg_{}_{}", chrono_like_id(), idx);
        self.current_message = Some(AnthropicItemState { id: item_id.clone(), output_index: idx, text: String::new() });
        self.phase = AnthropicPhase::Message;
        vec![
            self.sse_event("response.output_item.added", json!({
                "output_index": idx, "item": {"id": item_id, "type": "message", "status": "in_progress", "role": "assistant", "content": []}
            })),
            self.sse_event("response.content_part.added", json!({
                "output_index": idx, "item_id": item_id, "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []}
            })),
        ]
    }

    fn append_message_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let (output_index, item_id) = if let Some(ref item) = self.current_message {
            (item.output_index, item.id.clone())
        } else {
            return Vec::new();
        };
        if let Some(ref mut item) = self.current_message {
            item.text.push_str(delta);
        }
        vec![self.sse_event("response.output_text.delta", json!({
            "output_index": output_index, "item_id": item_id, "content_index": 0, "delta": delta
        }))]
    }

    fn close_message_item(&mut self) -> Vec<Bytes> {
        if let Some(item) = self.current_message.take() {
            let part = json!({"type": "output_text", "text": item.text, "annotations": []});
            let completed = json!({"id": item.id, "type": "message", "status": "completed", "role": "assistant", "content": [part]});
            self.output_items.push(completed.clone());
            vec![
                self.sse_event("response.output_text.done", json!({
                    "output_index": item.output_index, "item_id": item.id, "content_index": 0, "text": item.text
                })),
                self.sse_event("response.content_part.done", json!({
                    "output_index": item.output_index, "item_id": item.id, "content_index": 0, "part": part
                })),
                self.sse_event("response.output_item.done", json!({"output_index": item.output_index, "item": completed})),
            ]
        } else { Vec::new() }
    }

    fn open_tool_call_item(&mut self, tool_id: &str, name: &str) -> Vec<Bytes> {
        let idx = self.next_output_index();
        self.current_tool = Some(AnthropicToolCallState {
            id: tool_id.to_string(), name: name.to_string(), args: String::new(), output_index: idx,
        });
        self.phase = AnthropicPhase::ToolCall;
        vec![self.sse_event("response.output_item.added", json!({
            "output_index": idx, "item": {"id": tool_id, "type": "function_call", "call_id": tool_id, "name": name, "arguments": "", "status": "in_progress"}
        }))]
    }

    fn append_tool_call_args(&mut self, delta: &str) -> Vec<Bytes> {
        let (output_index, item_id) = if let Some(ref tc) = self.current_tool {
            (tc.output_index, tc.id.clone())
        } else {
            return Vec::new();
        };
        if let Some(ref mut tc) = self.current_tool {
            tc.args.push_str(delta);
        }
        vec![self.sse_event("response.function_call_arguments.delta", json!({
            "output_index": output_index, "item_id": item_id, "delta": delta
        }))]
    }

    fn close_tool_call_item(&mut self) -> Vec<Bytes> {
        if let Some(tc) = self.current_tool.take() {
            let completed = json!({
                "id": tc.id, "type": "function_call", "call_id": tc.id,
                "name": tc.name, "arguments": tc.args, "status": "completed"
            });
            self.output_items.push(completed.clone());
            vec![
                self.sse_event("response.function_call_arguments.done", json!({
                    "output_index": tc.output_index, "item_id": tc.id, "arguments": tc.args
                })),
                self.sse_event("response.output_item.done", json!({"output_index": tc.output_index, "item": completed})),
            ]
        } else { Vec::new() }
    }

    fn close_current(&mut self) -> Vec<Bytes> {
        match self.phase {
            AnthropicPhase::Reasoning => self.close_reasoning_item(),
            AnthropicPhase::Message => self.close_message_item(),
            _ => Vec::new(),
        }
    }

    fn handle_anthropic_event(&mut self, event_type: &str, data: &Value) -> Vec<Bytes> {
        let mut events = self.ensure_response_started();

        match event_type {
            "message_start" => {
                if let Some(msg) = data.get("message") {
                    // Extract upstream message ID for response_id
                    if let Some(msg_id) = msg.get("id").and_then(|v| v.as_str()) {
                        if !msg_id.is_empty() {
                            self.response_id = msg_id.to_string();
                        }
                    }
                    // Do NOT override model -- keep the request model so Codex sees
                    // the model name it sent, not the upstream provider's model name.
                    if let Some(usage) = msg.get("usage") {
                        self.input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        self.output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    }
                }
            }
            "content_block_start" => {
                let block = data.get("content_block").unwrap_or(&Value::Null);
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "thinking" => {
                        events.extend(self.close_current());
                        events.extend(self.start_reasoning_item());
                    }
                    "text" => {
                        events.extend(self.close_current());
                        events.extend(self.close_tool_call_item());
                        events.extend(self.start_message_item());
                    }
                    "tool_use" => {
                        events.extend(self.close_current());
                        let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        events.extend(self.open_tool_call_item(id, name));
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = data.get("delta").unwrap_or(&Value::Null);
                let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match delta_type {
                    "thinking_delta" => {
                        if self.phase != AnthropicPhase::Reasoning {
                            events.extend(self.close_current());
                            events.extend(self.start_reasoning_item());
                        }
                        if let Some(text) = delta.get("thinking").and_then(|v| v.as_str()) {
                            events.extend(self.append_reasoning_delta(text));
                        }
                    }
                    "text_delta" => {
                        if self.phase != AnthropicPhase::Message {
                            events.extend(self.close_current());
                            events.extend(self.close_tool_call_item());
                            events.extend(self.start_message_item());
                        }
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                            events.extend(self.append_message_delta(text));
                        }
                    }
                    "input_json_delta" => {
                        if self.phase != AnthropicPhase::ToolCall {
                            events.extend(self.close_current());
                        }
                        if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                            events.extend(self.append_tool_call_args(partial));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                events.extend(self.close_current());
                events.extend(self.close_tool_call_item());
                self.phase = AnthropicPhase::Idle;
            }
            "message_delta" => {
                if let Some(delta) = data.get("delta") {
                    if let Some(reason) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                        self.stop_reason = Some(reason.to_string());
                    }
                }
                if let Some(usage) = data.get("usage") {
                    self.output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(self.output_tokens);
                }
            }
            "message_stop" => {
                events.extend(self.close_current());
                events.extend(self.close_tool_call_item());
            }
            _ => {}
        }

        events
    }

    fn finalize(&mut self) -> Vec<Bytes> {
        if self.stream_terminated { return Vec::new(); }
        self.stream_terminated = true;
        let mut events = self.close_current();
        events.extend(self.close_tool_call_item());

        let usage = json!({"input_tokens": self.input_tokens, "output_tokens": self.output_tokens, "total_tokens": self.input_tokens + self.output_tokens});
        let msg_outputs: Vec<&Value> = self.output_items.iter()
            .filter(|it| it.get("type").and_then(|v| v.as_str()) == Some("message"))
            .collect();
        let empty_content = Vec::new();
        let output_text: String = msg_outputs.iter()
            .flat_map(|it| it.get("content").and_then(|v| v.as_array()).unwrap_or(&empty_content).iter())
            .filter_map(|c| if c.get("type").and_then(|v| v.as_str()) == Some("output_text") { c.get("text").and_then(|v| v.as_str()) } else { None })
            .collect();

        let status = match self.stop_reason.as_deref() {
            Some("max_tokens") => "incomplete",
            _ => "completed",
        };
        let mut response = json!({
            "id": self.response_id, "object": "response", "created_at": self.created_at,
            "status": status, "model": self.model, "output": self.output_items,
            "output_text": output_text, "usage": usage
        });
        if status == "incomplete" {
            response["incomplete_details"] = json!({"reason": "max_output_tokens"});
        }
        events.push(self.sse_event("response.completed", response));
        events
    }

    fn failed_event(&mut self, message: String) -> Bytes {
        self.stream_terminated = true;
        self.sse_event("response.failed", json!({
            "id": self.response_id, "object": "response", "status": "failed",
            "error": {"message": message}
        }))
    }
}
