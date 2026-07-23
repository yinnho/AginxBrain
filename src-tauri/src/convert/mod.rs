pub mod requests;
pub mod responses;
pub mod streaming;

// Re-export all public functions so external code can use crate::convert::*
pub use requests::{
    anthropic_to_openai_request,
    openai_to_anthropic_request,
    anthropic_to_responses_request,
    openai_to_responses_request,
    responses_to_chat_request,
    responses_to_anthropic_request,
};

pub use responses::{
    openai_to_anthropic_response,
    anthropic_to_openai_response,
    responses_to_anthropic_response,
    responses_to_openai_response,
    chat_to_responses_response,
    anthropic_to_responses_response,
};

pub use streaming::{
    convert_openai_stream_to_anthropic,
    convert_anthropic_stream_to_openai,
    convert_responses_stream_to_anthropic,
    convert_chat_stream_to_responses,
    convert_responses_stream_to_chat,
    convert_anthropic_stream_to_responses,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_anthropic_to_openai_simple_messages() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there"},
            ],
            "max_tokens": 1024
        });
        let result = anthropic_to_openai_request(&body, "deepseek-v4-pro");

        assert_eq!(result["model"], "deepseek-v4-pro");
        assert_eq!(result["max_tokens"], 1024);

        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "Hi there");
    }

    #[test]
    fn test_anthropic_to_openai_with_system() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "system": "You are helpful",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        let result = anthropic_to_openai_request(&body, "deepseek-v4-pro");

        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
        assert!(result.get("system").is_none());
    }

    #[test]
    fn test_anthropic_to_openai_with_tool_use() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "user", "content": "List files"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "I'll list files."},
                    {"type": "tool_use", "id": "toolu_1", "name": "bash", "input": {"cmd": "ls"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "file1.txt\nfile2.txt"}
                ]}
            ],
            "tools": [
                {"name": "bash", "description": "Run bash", "input_schema": {"type": "object", "properties": {"cmd": {"type": "string"}}}}
            ]
        });
        let result = anthropic_to_openai_request(&body, "deepseek-v4-pro");

        let messages = result["messages"].as_array().unwrap();
        // user, assistant (text + tool_calls), tool result
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "I'll list files.");
        assert_eq!(messages[1]["tool_calls"][0]["type"], "function");
        assert_eq!(messages[1]["tool_calls"][0]["function"]["name"], "bash");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "toolu_1");
        assert_eq!(messages[2]["content"], "file1.txt\nfile2.txt");

        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "bash");
    }

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
    fn test_anthropic_to_openai_strips_thinking_fields() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1024,
            "anthropic_version": "2023-06-01",
            "stream": true
        });
        let result = anthropic_to_openai_request(&body, "deepseek-v4-pro");

        assert!(result.get("anthropic_version").is_none());
        assert_eq!(result["stream"], true);
    }

    #[test]
    fn test_anthropic_to_openai_injects_reasoning_content_for_tool_calls() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "thinking": {"type": "adaptive"},
            "messages": [
                {"role": "user", "content": "List files"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "bash", "input": {"cmd": "ls"}}
                ]}
            ]
        });
        let result = anthropic_to_openai_request(&body, "glm-5.1");

        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["reasoning_content"], " ");
        assert_eq!(messages[1]["tool_calls"][0]["function"]["name"], "bash");
    }

    #[test]
    fn test_anthropic_to_openai_no_reasoning_when_thinking_disabled() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "user", "content": "List files"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "bash", "input": {"cmd": "ls"}}
                ]}
            ]
        });
        let result = anthropic_to_openai_request(&body, "glm-5.1");

        let messages = result["messages"].as_array().unwrap();
        assert!(messages[1].get("reasoning_content").is_none());
    }

    #[test]
    fn test_anthropic_to_openai_backfills_orphan_tool_use() {
        // Orphaned tool_use: assistant emitted a tool_call with no matching
        // tool_result (e.g. history truncated mid tool-turn by the client, as
        // happens with Claude Code /compact). The converter must synthesize an
        // empty tool response so the OpenAI provider doesn't 400 on an
        // unanswered tool_call_id.
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "user", "content": "List files"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_orphan", "name": "bash", "input": {"cmd": "ls"}}
                ]}
            ],
            "tools": [
                {"name": "bash", "description": "Run bash", "input_schema": {"type": "object"}}
            ]
        });
        let result = anthropic_to_openai_request(&body, "deepseek-v4-pro");
        let messages = result["messages"].as_array().unwrap();
        // user, assistant(tool_calls), synthesized empty tool reply
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["tool_calls"][0]["id"], "toolu_orphan");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "toolu_orphan");
        assert_eq!(messages[2]["content"], "");
    }

    // =======================================================================
    // OpenAI Responses conversion tests
    // =======================================================================

    #[test]
    fn test_anthropic_to_responses_simple() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "system": "You are helpful",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there"},
            ],
            "max_tokens": 1024
        });
        let result = anthropic_to_responses_request(&body, "gpt-4o");

        assert_eq!(result["model"], "gpt-4o");
        assert_eq!(result["instructions"], "You are helpful");
        assert_eq!(result["max_output_tokens"], 1024);

        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "Hello");
        assert_eq!(input[1]["role"], "assistant");
    }

    #[test]
    fn test_anthropic_to_responses_with_tool_use() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "user", "content": "List files"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "I'll list files."},
                    {"type": "tool_use", "id": "toolu_1", "name": "bash", "input": {"cmd": "ls"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "file1.txt\nfile2.txt"}
                ]}
            ],
            "tools": [
                {"name": "bash", "description": "Run bash", "input_schema": {"type": "object", "properties": {"cmd": {"type": "string"}}}}
            ]
        });
        let result = anthropic_to_responses_request(&body, "gpt-4o");

        let input = result["input"].as_array().unwrap();
        // user, assistant message, function_call, function_call_output
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "toolu_1");
        assert_eq!(input[2]["name"], "bash");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "toolu_1");

        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "bash");
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
    fn test_openai_to_responses_simple() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there"}
            ],
            "max_tokens": 1024
        });
        let result = openai_to_responses_request(&body, "gpt-4o");

        assert_eq!(result["model"], "gpt-4o");
        assert_eq!(result["instructions"], "You are helpful");
        assert_eq!(result["max_output_tokens"], 1024);

        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 2); // system extracted as instructions
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
    }

    #[test]
    fn test_openai_to_responses_with_tool_calls() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "List files"},
                {"role": "assistant", "content": "I'll check.", "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "file1.txt"}
            ]
        });
        let result = openai_to_responses_request(&body, "gpt-4o");

        let input = result["input"].as_array().unwrap();
        // user, assistant, function_call, function_call_output
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call_1");
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
    fn test_openai_to_responses_user_image() {
        // Chat-format user content (text + image_url) must become Responses
        // input_text + input_image, or the Responses API rejects the type names.
        let body = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what color?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
                ]
            }]
        });
        let result = openai_to_responses_request(&body, "qwen3.7-plus");

        let input = result["input"].as_array().unwrap();
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "what color?");
        assert_eq!(content[1]["type"], "input_image");
        // image_url object flattened to a bare string
        assert_eq!(content[1]["image_url"], "data:image/png;base64,AAA");
    }

    #[test]
    fn test_openai_to_responses_user_image_no_type() {
        // Some clients send {"image_url": "..."} with NO type field. The
        // converter must still emit input_image (detected by key presence),
        // or the Responses API rejects the block.
        let body = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"text": "describe this"},
                    {"image_url": "data:image/jpeg;base64,BBB"}
                ]
            }]
        });
        let result = openai_to_responses_request(&body, "qwen3.7-plus");

        let content = result["input"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "describe this");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/jpeg;base64,BBB");
    }

    #[test]
    fn test_openai_to_responses_flattens_tools() {
        // Chat wraps functions as {"type":"function","function":{...}}; the
        // Responses API needs the flat form or it rejects the parameter schema.
        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "get weather",
                    "parameters": {"type": "object", "properties": {}}
                }
            }]
        });
        let result = openai_to_responses_request(&body, "qwen3.7-plus");
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        // flattened: name/parameters at top level, no nested "function"
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["description"], "get weather");
        assert!(tools[0]["parameters"].is_object());
        assert!(tools[0].get("function").is_none());
    }

    #[test]
    fn test_anthropic_to_openai_converts_image_block() {
        // Claude Code sends Anthropic image blocks; the OpenAI Chat provider
        // needs image_url blocks, not a stringified JSON blob.
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What is in this image?"},
                    {"type": "image", "source": {"type": "url", "url": "https://example.com/cat.png"}}
                ]
            }],
            "max_tokens": 100
        });
        let result = anthropic_to_openai_request(&body, "gpt-4o");
        let messages = result["messages"].as_array().unwrap();
        // system message is not present (no system field), so messages[0] is the user
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "What is in this image?");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "https://example.com/cat.png");
    }

    #[test]
    fn test_anthropic_to_openai_image_base64_to_data_url() {
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "QUJD"}}
                ]
            }],
            "max_tokens": 100
        });
        let result = anthropic_to_openai_request(&body, "gpt-4o");
        let content = result["messages"].as_array().unwrap()[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "image_url");
        assert_eq!(content[0]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
    }

    #[test]
    fn test_anthropic_to_openai_text_only_stays_string() {
        // Text-only user messages must collapse back to a plain string
        // (prior behaviour), not an array.
        let body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
            "max_tokens": 100
        });
        let result = anthropic_to_openai_request(&body, "gpt-4o");
        assert_eq!(result["messages"][0]["content"], "hi");
    }

    #[test]
    fn test_anthropic_to_responses_converts_image_block() {
        // Claude Code -> qwen3.7-plus (Responses). Image must become input_image,
        // not a stringified JSON blob in the prompt.
        let body = json!({
            "model": "vision",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "Describe this"},
                    {"type": "image", "source": {"type": "url", "url": "https://example.com/x.png"}}
                ]
            }],
            "max_tokens": 100
        });
        let result = anthropic_to_responses_request(&body, "qwen3.7-plus");
        let input = result["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "https://example.com/x.png");
    }

    #[test]
    fn test_anthropic_to_responses_image_only_message_survives() {
        // An image-only user message (no text) used to be dropped by the
        // empty-text guard; it must now be emitted with the image.
        let body = json!({
            "model": "vision",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image", "source": {"type": "url", "url": "https://example.com/y.png"}}
                ]
            }],
            "max_tokens": 100
        });
        let result = anthropic_to_responses_request(&body, "qwen3.7-plus");
        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 1, "image-only message must not be dropped");
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "input_image");
    }

    #[test]
    fn test_anthropic_to_responses_text_only_stays_string() {
        let body = json!({
            "model": "vision",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}],
            "max_tokens": 100
        });
        let result = anthropic_to_responses_request(&body, "qwen3.7-plus");
        assert_eq!(result["input"][0]["content"], "hello");
    }
}
