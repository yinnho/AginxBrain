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
}
