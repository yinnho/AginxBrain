//! Smart auto-routing: inspect request body for signals (agentic, reasoning,
//! coding, etc.) and dynamically select a tier tag (opus/sonnet/haiku) instead
//! of the static "auto" tag.
//!
//! Design principles:
//! - Zero external dependencies (no ML, no embeddings)
//! - Pure Rust string/JSON matching, < 1ms overhead
//! - Upgrade-only session cache (once a conversation needs a powerful model,
//!   it never downgrades)
//! - Fully configurable via config.yaml

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Tier
// ---------------------------------------------------------------------------

/// Routing tier, ordered by capability: Haiku < Sonnet < Opus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    Haiku,
    Sonnet,
    Opus,
}

impl Tier {
    pub fn as_tag(&self) -> &'static str {
        match self {
            Tier::Haiku => "haiku",
            Tier::Sonnet => "sonnet",
            Tier::Opus => "opus",
        }
    }

    fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "haiku" => Some(Tier::Haiku),
            "sonnet" => Some(Tier::Sonnet),
            "opus" => Some(Tier::Opus),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// The signals detected from a request body.
#[derive(Debug, Clone, Default)]
pub struct Signals {
    pub agentic: bool,
    pub reasoning: bool,
    pub complex_coding: bool,
    pub subagent: bool,
    pub code_pattern: bool,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}
fn default_cache_ttl() -> u64 {
    1800
}
fn default_cache_max() -> usize {
    1024
}
fn default_signal_tiers() -> HashMap<String, Tier> {
    let mut m = HashMap::new();
    m.insert("agentic".into(), Tier::Sonnet);
    m.insert("reasoning".into(), Tier::Opus);
    m.insert("complex_coding".into(), Tier::Opus);
    m.insert("subagent".into(), Tier::Haiku);
    m.insert("code_pattern".into(), Tier::Sonnet);
    m
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartRoutingConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_secs: u64,
    #[serde(default = "default_cache_max")]
    pub cache_max_sessions: usize,
    #[serde(default = "default_signal_tiers")]
    pub signal_tiers: HashMap<String, Tier>,
}

impl Default for SmartRoutingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_ttl_secs: default_cache_ttl(),
            cache_max_sessions: default_cache_max(),
            signal_tiers: default_signal_tiers(),
        }
    }
}

// ---------------------------------------------------------------------------
// Session cache
// ---------------------------------------------------------------------------

struct SessionEntry {
    tier: Tier,
    expires_at: Instant,
}

pub struct SessionCache {
    sessions: HashMap<String, SessionEntry>,
    max_sessions: usize,
}

impl SessionCache {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sessions: HashMap::new(),
            max_sessions,
        }
    }

    pub fn get(&mut self, key: &str) -> Option<Tier> {
        if let Some(entry) = self.sessions.get(key) {
            if entry.expires_at > Instant::now() {
                return Some(entry.tier);
            }
            // Expired — remove
            self.sessions.remove(key);
        }
        None
    }

    /// Insert or upgrade the cached tier. Only upgrades (never downgrades).
    pub fn upsert(&mut self, key: String, tier: Tier, ttl_secs: u64) {
        let final_tier = if let Some(existing) = self.sessions.get(&key) {
            if existing.expires_at <= Instant::now() {
                // Expired — fresh value wins
                tier
            } else {
                // Upgrade-only: take the higher tier
                existing.tier.max(tier)
            }
        } else {
            tier
        };

        self.sessions.insert(
            key,
            SessionEntry {
                tier: final_tier,
                expires_at: Instant::now() + std::time::Duration::from_secs(ttl_secs),
            },
        );

        // Evict if over capacity
        if self.sessions.len() > self.max_sessions {
            self.evict();
        }
    }

    fn evict(&mut self) {
        // First pass: remove expired entries
        let now = Instant::now();
        self.sessions.retain(|_, e| e.expires_at > now);

        // Second pass: if still over capacity, remove oldest
        while self.sessions.len() > self.max_sessions {
            if let Some(oldest_key) = self
                .sessions
                .iter()
                .min_by_key(|(_, e)| e.expires_at)
                .map(|(k, _)| k.clone())
            {
                self.sessions.remove(&oldest_key);
            } else {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Protocol-aware text extraction
// ---------------------------------------------------------------------------

/// Extract the system prompt text from any request format.
fn extract_system_prompt(body: &Value, protocol: &str) -> String {
    match protocol {
        "anthropic" => {
            // body["system"] can be a string or an array of content blocks
            match body.get("system") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                            b.get("text").and_then(|v| v.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            }
        }
        "openai" => {
            // System prompt is a message with role="system" in messages array
            if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
                for msg in messages {
                    if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
                        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                            return content.to_string();
                        }
                    }
                }
            }
            String::new()
        }
        "openai_responses" => {
            // body["instructions"]
            body.get("instructions")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        }
        _ => String::new(),
    }
}

/// Extract the text of the last user message from any request format.
fn extract_last_user_text(body: &Value, protocol: &str) -> String {
    match protocol {
        "anthropic" | "openai" => {
            if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
                for msg in messages.iter().rev() {
                    if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                        return extract_message_text(msg);
                    }
                }
            }
            String::new()
        }
        "openai_responses" => {
            // Responses format uses "input" array with items of type "message"
            if let Some(items) = body.get("input").and_then(|i| i.as_array()) {
                for item in items.iter().rev() {
                    if item.get("type").and_then(|t| t.as_str()) == Some("message")
                        && item.get("role").and_then(|r| r.as_str()) == Some("user")
                    {
                        return extract_responses_item_text(item);
                    }
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

/// Extract text content from an Anthropic/OpenAI message object.
fn extract_message_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| {
                let typ = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match typ {
                    "text" => b.get("text").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    "tool_result" => {
                        // tool_result content can be string or array
                        match b.get("content") {
                            Some(Value::String(s)) => Some(s.clone()),
                            Some(Value::Array(arr)) => Some(
                                arr.iter()
                                    .filter_map(|c| {
                                        if c.get("type").and_then(|v| v.as_str()) == Some("text") {
                                            c.get("text").and_then(|v| v.as_str())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" "),
                            ),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Extract text from a Responses API input item.
fn extract_responses_item_text(item: &Value) -> String {
    if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
        parts
            .iter()
            .filter_map(|p| {
                let typ = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match typ {
                    "input_text" | "output_text" => p.get("text").and_then(|v| v.as_str()),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        String::new()
    }
}

/// Extract tool names from recent messages.
fn extract_recent_tool_names(body: &Value, protocol: &str, max_messages: usize) -> Vec<String> {
    let mut names = Vec::new();
    match protocol {
        "anthropic" => {
            if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
                let mut seen = 0;
                for msg in messages.iter().rev() {
                    if seen >= max_messages {
                        break;
                    }
                    if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                        if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
                            for b in blocks {
                                if b.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                    if let Some(name) = b.get("name").and_then(|n| n.as_str()) {
                                        names.push(name.to_string());
                                    }
                                }
                            }
                        }
                        seen += 1;
                    }
                }
            }
        }
        "openai" => {
            if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
                let mut seen = 0;
                for msg in messages.iter().rev() {
                    if seen >= max_messages {
                        break;
                    }
                    if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                        if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
                            for call in calls {
                                if let Some(name) = call
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())
                                {
                                    names.push(name.to_string());
                                }
                            }
                        }
                        seen += 1;
                    }
                }
            }
        }
        "openai_responses" => {
            if let Some(items) = body.get("input").and_then(|i| i.as_array()) {
                let mut seen = 0;
                for item in items.iter().rev() {
                    if seen >= max_messages {
                        break;
                    }
                    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                        if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                            names.push(name.to_string());
                        }
                        seen += 1;
                    }
                }
            }
        }
        _ => {}
    }
    names
}

// ---------------------------------------------------------------------------
// Signal detectors
// ---------------------------------------------------------------------------

/// Detect if the request is part of an agentic session.
fn detect_agentic(body: &Value, protocol: &str) -> bool {
    // Check for tools definition (top-level, all formats)
    if body.get("tools").and_then(|t| t.as_array()).map_or(false, |a| !a.is_empty()) {
        return true;
    }

    match protocol {
        "anthropic" => {
            // Check messages for tool_use / tool_result content blocks
            if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
                for msg in messages {
                    if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
                        for b in blocks {
                            let typ = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if typ == "tool_use" || typ == "tool_result" {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        "openai" => {
            // Check messages for tool_calls
            if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
                for msg in messages {
                    if msg.get("tool_calls").and_then(|c| c.as_array()).map_or(false, |a| !a.is_empty())
                    {
                        return true;
                    }
                    if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
                        return true;
                    }
                }
            }
        }
        "openai_responses" => {
            // Check input for function_call / function_call_output
            if let Some(items) = body.get("input").and_then(|i| i.as_array()) {
                for item in items {
                    let typ = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if typ == "function_call" || typ == "function_call_output" {
                        return true;
                    }
                }
            }
        }
        _ => {}
    }
    false
}

/// Reasoning markers — English (lowercase for matching)
const REASONING_MARKERS_EN: &[&str] = &[
    "step by step",
    "think carefully",
    "think through",
    "prove that",
    "chain of thought",
    "analyze in detail",
    "reason about",
    "compare and contrast",
    "diagnose the root cause",
    "architectural decision",
    "design a system",
    "let's think",
    "work through",
    "break this down",
    "critically analyze",
    "weigh the",
    "formal proof",
    "derive the",
    "what are the pros and cons",
];

/// Reasoning markers — Chinese
const REASONING_MARKERS_ZH: &[&str] = &[
    "逐步分析",
    "仔细思考",
    "证明",
    "深入分析",
    "推理",
    "一步一步",
    "详细分析",
    "优缺点",
    "对比分析",
    "设计方案",
];

/// Detect if the last user message requires reasoning capabilities.
fn detect_reasoning(body: &Value, protocol: &str) -> bool {
    let text = extract_last_user_text(body, protocol);
    let lower = text.to_lowercase();

    for marker in REASONING_MARKERS_EN {
        if lower.contains(marker) {
            return true;
        }
    }
    for marker in REASONING_MARKERS_ZH {
        if text.contains(marker) {
            return true;
        }
    }
    false
}

/// Coding keywords for complex coding detection
const CODING_KEYWORDS: &[&str] = &[
    "implement",
    "refactor",
    "debug",
    "optimize",
    "fix bug",
    "multiple files",
    "create feature",
    "build a",
];

/// Detect complex coding tasks from tool usage patterns and keywords.
fn detect_complex_coding(body: &Value, protocol: &str) -> bool {
    // Signal 1: Heavy editing — 3+ Edit/Write/Bash tool calls in recent messages
    let tool_names = extract_recent_tool_names(body, protocol, 5);
    let edit_count = tool_names
        .iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            lower.contains("edit")
                || lower.contains("write")
                || lower.contains("bash")
                || lower.contains("replace")
                || lower.contains("notebook")
        })
        .count();
    if edit_count >= 3 {
        return true;
    }

    // Signal 2: Coding keywords in last user message
    let text = extract_last_user_text(body, protocol);
    let lower = text.to_lowercase();
    let mut keyword_hits = 0;
    for kw in CODING_KEYWORDS {
        if lower.contains(kw) {
            keyword_hits += 1;
        }
    }
    if keyword_hits >= 2 {
        return true;
    }

    false
}

/// Subagent markers
const SUBAGENT_MARKERS: &[&str] = &["subagent", "delegate", "task:", "sub-task", "子任务"];

/// Detect if this is a subagent / delegated task.
fn detect_subagent(body: &Value, protocol: &str) -> bool {
    let sys = extract_system_prompt(body, protocol);
    // Subagents typically have very short system prompts
    if sys.len() >= 200 {
        return false;
    }
    let text = extract_last_user_text(body, protocol);
    let lower = text.to_lowercase();
    for marker in SUBAGENT_MARKERS {
        if lower.contains(marker) {
            return true;
        }
    }
    false
}

/// Code fence languages to detect
const CODE_FENCE_LANGS: &[&str] = &[
    "python",
    "javascript",
    "typescript",
    "rust",
    "java",
    "go",
    "cpp",
    "c",
    "sh",
    "bash",
];

/// Detect code patterns (fenced code blocks) in the last user message.
fn detect_code_pattern(body: &Value, protocol: &str) -> bool {
    let text = extract_last_user_text(body, protocol);
    let lower = text.to_lowercase();
    for lang in CODE_FENCE_LANGS {
        // Look for ```<lang> pattern
        if lower.contains(&format!("```{}", lang)) {
            return true;
        }
    }
    // Also detect generic code fence with no language
    if lower.contains("```") && lower.matches("```").count() >= 2 {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Signal collection + routing decision
// ---------------------------------------------------------------------------

fn detect_signals(body: &Value, protocol: &str) -> Signals {
    Signals {
        agentic: detect_agentic(body, protocol),
        reasoning: detect_reasoning(body, protocol),
        complex_coding: detect_complex_coding(body, protocol),
        subagent: detect_subagent(body, protocol),
        code_pattern: detect_code_pattern(body, protocol),
    }
}

/// Compute the smart routing tier based on detected signals and config.
pub fn compute_smart_tier(body: &Value, protocol: &str, config: &SmartRoutingConfig) -> Tier {
    let signals = detect_signals(body, protocol);
    let mut tier = Tier::Haiku; // default for "auto" is cheapest

    let signal_map: [(&str, bool); 5] = [
        ("agentic", signals.agentic),
        ("reasoning", signals.reasoning),
        ("complex_coding", signals.complex_coding),
        ("subagent", signals.subagent),
        ("code_pattern", signals.code_pattern),
    ];

    let mut detected = Vec::new();
    for (name, active) in &signal_map {
        if *active {
            if let Some(min_tier) = config.signal_tiers.get(*name) {
                tier = tier.max(*min_tier);
            }
            detected.push(*name);
        }
    }

    if !detected.is_empty() {
        log::debug!(
            "[SmartRouting] signals: {} → tier={:?}",
            detected.join(","),
            tier
        );
    }

    tier
}

// ---------------------------------------------------------------------------
// Session key
// ---------------------------------------------------------------------------

fn compute_session_key(caller_key_id: Option<i64>, system_prompt: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    // Hash first 2048 chars of system prompt to bound work
    system_prompt.chars().take(2048).for_each(|c| c.hash(&mut hasher));
    let hash = hasher.finish();
    format!(
        "{}:{:016x}",
        caller_key_id.unwrap_or(0),
        hash
    )
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Main smart routing entry point. Returns the resolved tag name,
/// or None if no override is needed (fall through to the original tag).
pub async fn route(
    body: &Value,
    protocol: &str,
    caller_key_id: Option<i64>,
    config: &SmartRoutingConfig,
    cache: &Arc<RwLock<SessionCache>>,
) -> Option<String> {
    let system_prompt = extract_system_prompt(body, protocol);
    let session_key = compute_session_key(caller_key_id, &system_prompt);

    // Check cache
    {
        let mut cache = cache.write().await;
        if let Some(cached_tier) = cache.get(&session_key) {
            log::debug!(
                "[SmartRouting] cache hit: key={} tier={:?}",
                &session_key[..16.min(session_key.len())],
                cached_tier
            );
            return Some(cached_tier.as_tag().to_string());
        }
    }

    // No cache hit — compute from signals
    let tier = compute_smart_tier(body, protocol, config);

    // Write to cache (upgrade-only inside upsert)
    {
        let mut cache = cache.write().await;
        cache.upsert(session_key, tier, config.cache_ttl_secs);
    }

    Some(tier.as_tag().to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tier_ordering() {
        assert!(Tier::Haiku < Tier::Sonnet);
        assert!(Tier::Sonnet < Tier::Opus);
        assert_eq!(Tier::Sonnet.max(Tier::Haiku), Tier::Sonnet);
        assert_eq!(Tier::Haiku.max(Tier::Opus), Tier::Opus);
    }

    #[test]
    fn test_tier_as_tag() {
        assert_eq!(Tier::Haiku.as_tag(), "haiku");
        assert_eq!(Tier::Sonnet.as_tag(), "sonnet");
        assert_eq!(Tier::Opus.as_tag(), "opus");
    }

    #[test]
    fn test_detect_agentic_anthropic_with_tools() {
        let body = json!({
            "model": "auto",
            "tools": [{"name": "bash", "description": "Run bash commands"}],
            "messages": [{"role": "user", "content": "list files"}]
        });
        assert!(detect_agentic(&body, "anthropic"));
    }

    #[test]
    fn test_detect_agentic_anthropic_with_tool_use() {
        let body = json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "read file"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "1", "name": "Read", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "1", "content": "file contents"}]}
            ]
        });
        assert!(detect_agentic(&body, "anthropic"));
    }

    #[test]
    fn test_detect_agentic_openai_with_tool_calls() {
        let body = json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "read file"},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "1", "type": "function", "function": {"name": "Read", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "1", "content": "file contents"}
            ]
        });
        assert!(detect_agentic(&body, "openai"));
    }

    #[test]
    fn test_detect_agentic_no_tools() {
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}]
        });
        assert!(!detect_agentic(&body, "anthropic"));
    }

    #[test]
    fn test_detect_reasoning_english() {
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Please think step by step about this problem"}]
        });
        assert!(detect_reasoning(&body, "anthropic"));
    }

    #[test]
    fn test_detect_reasoning_chinese() {
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "请逐步分析这个架构的优缺点"}]
        });
        assert!(detect_reasoning(&body, "anthropic"));
    }

    #[test]
    fn test_detect_reasoning_not_in_casual_chat() {
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "你好，今天天气怎么样"}]
        });
        assert!(!detect_reasoning(&body, "anthropic"));
    }

    #[test]
    fn test_detect_complex_coding_heavy_editing() {
        let body = json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "fix the bug"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "1", "name": "Read", "input": {}}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "2", "name": "Edit", "input": {}}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "3", "name": "Edit", "input": {}}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "4", "name": "Bash", "input": {}}]},
            ]
        });
        assert!(detect_complex_coding(&body, "anthropic"));
    }

    #[test]
    fn test_detect_complex_coding_keywords() {
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "implement the new feature and debug the issue"}]
        });
        assert!(detect_complex_coding(&body, "anthropic"));
    }

    #[test]
    fn test_detect_subagent() {
        let body = json!({
            "model": "auto",
            "system": "You are a helper.",
            "messages": [{"role": "user", "content": "Complete this subagent task: summarize"}]
        });
        assert!(detect_subagent(&body, "anthropic"));
    }

    #[test]
    fn test_detect_subagent_long_system_prompt() {
        let body = json!({
            "model": "auto",
            "system": "x".repeat(300),
            "messages": [{"role": "user", "content": "Complete this subagent task"}]
        });
        assert!(!detect_subagent(&body, "anthropic"));
    }

    #[test]
    fn test_detect_code_pattern_python() {
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Fix this code:\n```python\ndef foo():\n    pass\n```"}]
        });
        assert!(detect_code_pattern(&body, "anthropic"));
    }

    #[test]
    fn test_detect_code_pattern_no_fence() {
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "write a function that sorts a list"}]
        });
        assert!(!detect_code_pattern(&body, "anthropic"));
    }

    #[test]
    fn test_compute_smart_tier_default() {
        let config = SmartRoutingConfig::default();
        // Simple chat, no signals → haiku
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "你好"}]
        });
        assert_eq!(compute_smart_tier(&body, "anthropic", &config), Tier::Haiku);
    }

    #[test]
    fn test_compute_smart_tier_agentic() {
        let config = SmartRoutingConfig::default();
        let body = json!({
            "model": "auto",
            "tools": [{"name": "bash"}],
            "messages": [{"role": "user", "content": "list files"}]
        });
        assert_eq!(compute_smart_tier(&body, "anthropic", &config), Tier::Sonnet);
    }

    #[test]
    fn test_compute_smart_tier_reasoning() {
        let config = SmartRoutingConfig::default();
        let body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Prove that the algorithm is correct step by step"}]
        });
        assert_eq!(compute_smart_tier(&body, "anthropic", &config), Tier::Opus);
    }

    #[test]
    fn test_compute_smart_tier_agentic_plus_reasoning() {
        let config = SmartRoutingConfig::default();
        let body = json!({
            "model": "auto",
            "tools": [{"name": "bash"}],
            "messages": [{"role": "user", "content": "Prove that the algorithm is correct step by step"}]
        });
        // agentic → sonnet, reasoning → opus → max = opus
        assert_eq!(compute_smart_tier(&body, "anthropic", &config), Tier::Opus);
    }

    #[test]
    fn test_extract_system_prompt_anthropic_string() {
        let body = json!({"system": "You are a helpful assistant.", "messages": []});
        assert_eq!(
            extract_system_prompt(&body, "anthropic"),
            "You are a helpful assistant."
        );
    }

    #[test]
    fn test_extract_system_prompt_anthropic_array() {
        let body = json!({
            "system": [{"type": "text", "text": "You are "}, {"type": "text", "text": "helpful."}],
            "messages": []
        });
        assert_eq!(extract_system_prompt(&body, "anthropic"), "You are  helpful.");
    }

    #[test]
    fn test_extract_system_prompt_openai() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hi"}
            ]
        });
        assert_eq!(extract_system_prompt(&body, "openai"), "You are helpful.");
    }

    #[test]
    fn test_extract_system_prompt_responses() {
        let body = json!({"instructions": "You are helpful.", "input": []});
        assert_eq!(
            extract_system_prompt(&body, "openai_responses"),
            "You are helpful."
        );
    }

    #[test]
    fn test_extract_last_user_text_anthropic() {
        let body = json!({
            "messages": [
                {"role": "assistant", "content": "hi"},
                {"role": "user", "content": "hello world"}
            ]
        });
        assert_eq!(extract_last_user_text(&body, "anthropic"), "hello world");
    }

    #[test]
    fn test_extract_last_user_text_anthropic_blocks() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "look at "}, {"type": "image", "source": {}}]},
                {"role": "user", "content": [{"type": "text", "text": "hello"}]}
            ]
        });
        assert_eq!(extract_last_user_text(&body, "anthropic"), "hello");
    }

    #[test]
    fn test_session_cache_upgrade_only() {
        let mut cache = SessionCache::new(100);
        cache.upsert("key1".into(), Tier::Sonnet, 60);
        assert_eq!(cache.get("key1"), Some(Tier::Sonnet));

        // Try to downgrade — should stay at Sonnet
        cache.upsert("key1".into(), Tier::Haiku, 60);
        assert_eq!(cache.get("key1"), Some(Tier::Sonnet));

        // Upgrade to Opus
        cache.upsert("key1".into(), Tier::Opus, 60);
        assert_eq!(cache.get("key1"), Some(Tier::Opus));
    }

    #[test]
    fn test_session_cache_expiry() {
        let mut cache = SessionCache::new(100);
        cache.upsert("key1".into(), Tier::Sonnet, 0); // TTL = 0, expires immediately
        assert_eq!(cache.get("key1"), None);
    }

    #[test]
    fn test_detect_agentic_responses_format() {
        let body = json!({
            "model": "auto",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "function_call", "call_id": "1", "name": "Read", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "1", "output": "contents"}
            ]
        });
        assert!(detect_agentic(&body, "openai_responses"));
    }

    #[test]
    fn test_detect_reasoning_responses_format() {
        let body = json!({
            "model": "auto",
            "instructions": "You are an assistant.",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Prove that P=NP step by step"}]}
            ]
        });
        assert!(detect_reasoning(&body, "openai_responses"));
    }
}
