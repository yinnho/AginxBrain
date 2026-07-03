use crate::config::{AppState, AppConfig, CircuitState, CircuitStatus, Provider, ProviderFormat, Route};
use crate::convert;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use futures::StreamExt;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

const STREAM_TIMEOUT: u64 = 3600;
const NON_STREAM_TIMEOUT: u64 = 45;
// Reasoning models (qwen3.7-max, deepseek-v4-pro thinking, etc.) legitimately
// spend 60-120s "thinking" on heavy payloads before emitting output. The 45s
// chat timeout cuts them off mid-reasoning and triggers a wasteful failover.
// Give the `reasoning` tag a longer ceiling; connect_timeout (10s) still bounds
// genuine connection-level stalls.
const NON_STREAM_TIMEOUT_REASONING: u64 = 120;
pub const CONNECT_TIMEOUT: u64 = 10;
const HEALTH_CHECK_TIMEOUT: u64 = 30;
// Same-route retry for transient upstream errors (truncated response body,
// connection reset mid-read). These are usually instantaneous hiccups, so an
// immediate retry on the SAME route usually succeeds — keeping the same model
// instead of jumping to a different provider via cross-route failover.
const SAME_ROUTE_RETRIES: u32 = 2;
const SAME_ROUTE_DELAY: std::time::Duration = std::time::Duration::from_millis(300);
// Circuit breaker: after consecutive failures on a route, stop trying it for
// a cooldown period, then probe once (half-open). Prevents hammering a dead
// provider on every new request.
const CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
const CIRCUIT_COOLDOWN_SECS: u64 = 60;

/// Truncate a string to at most `max_chars` characters, safe for multi-byte UTF-8.
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Extract tag from Claude model name by keyword matching.
/// e.g. "claude-opus-4-8" → "opus", "claude-sonnet-4-6" → "sonnet", "claude-haiku-4-5" → "haiku"
/// Also handles plain values like "opus", "sonnet", "haiku" (set via ANTHROPIC_DEFAULT_*_MODEL).

/// Forward client headers to the upstream request, filtering out
/// hop-by-hop headers, auth headers we already set, and
/// anthropic-beta thinking flags that most providers don't support.
fn forward_client_headers(
    headers: &HeaderMap,
    req_builder: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let mut builder = req_builder;
    for (name, value) in headers.iter() {
        let lower = name.as_str().to_lowercase();
        if matches!(
            lower.as_str(),
            "host" | "content-length" | "transfer-encoding" | "connection"
                | "accept-encoding" | "authorization" | "x-api-key" | "x-goog-api-key"
        ) {
            continue;
        }
        if lower == "anthropic-beta" {
            if let Ok(v) = value.to_str() {
                let filtered: Vec<&str> = v
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.contains("thinking"))
                    .collect();
                if !filtered.is_empty() {
                    builder = builder.header("anthropic-beta", filtered.join(", "));
                }
                continue;
            }
        }
        if let Ok(v) = value.to_str() {
            builder = builder.header(name.as_str(), v);
        }
    }
    builder
}

/// Resolve a tag purely from user-defined tag names.
///
/// The model name is split into components; if a configured tag's components
/// appear as a contiguous subsequence inside the model name, that tag wins.
/// Longer tags are preferred over shorter ones so a tag like "gpt-5.5" beats
/// a generic "gpt" tag. This keeps AginxBrain model-agnostic: new provider
/// model names (gpt-6, claude-xyz, etc.) require no code changes — just add
/// a tag and attach it to a route.
fn resolve_tag_from_model(model: &str, tags: &[crate::config::Tag]) -> Option<String> {
    let model_lower = model.to_lowercase();
    let model_parts: Vec<&str> = model_lower.split(&['-', '_', '.', ' ', '/'][..]).collect();

    // Prefer longer tag names first to avoid partial matches shadowing specific ones
    // (e.g. a tag "gpt-5.5" should win over a tag "gpt" if both exist).
    let mut sorted_tags: Vec<&crate::config::Tag> = tags.iter().collect();
    sorted_tags.sort_by(|a, b| b.name.len().cmp(&a.name.len()));

    for tag in sorted_tags {
        let tag_lower = tag.name.to_lowercase();
        let tag_parts: Vec<&str> = tag_lower.split(&['-', '_', '.', ' ', '/'][..]).collect();
        if tag_parts.is_empty() {
            continue;
        }

        // Look for the tag's parts as a contiguous subsequence in the model parts.
        if model_parts.windows(tag_parts.len()).any(|w| w == tag_parts.as_slice()) {
            return Some(tag.name.clone());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Anthropic client handlers
// ---------------------------------------------------------------------------

pub async fn handle_anthropic_messages(
    state: State<AppState>,
    request: Request,
) -> Result<Response, ProxyError> {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let caller_key_id = parts.extensions.get::<i64>().copied();
    let body = parse_body(body).await?;
    handle_proxy("anthropic", state, headers, caller_key_id, axum::Json(body)).await
}

pub async fn handle_anthropic_count_tokens(
    state: State<AppState>,
    request: Request,
) -> Result<Response, ProxyError> {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let caller_key_id = parts.extensions.get::<i64>().copied();
    let body = parse_body(body).await?;
    handle_count_tokens("anthropic", state, headers, caller_key_id, axum::Json(body)).await
}

// ---------------------------------------------------------------------------
// OpenAI client handlers
// ---------------------------------------------------------------------------

pub async fn handle_openai_chat(
    state: State<AppState>,
    request: Request,
) -> Result<Response, ProxyError> {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let caller_key_id = parts.extensions.get::<i64>().copied();
    let body = parse_body(body).await?;
    handle_proxy("openai", state, headers, caller_key_id, axum::Json(body)).await
}

pub async fn handle_openai_responses(
    state: State<AppState>,
    request: Request,
) -> Result<Response, ProxyError> {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let caller_key_id = parts.extensions.get::<i64>().copied();
    let body = parse_body(body).await?;
    handle_proxy("openai_responses", state, headers, caller_key_id, axum::Json(body)).await
}

/// Parse request body as JSON, accepting any content-type.
/// More forgiving than `axum::Json` which requires `Content-Type: application/json`.
async fn parse_body(body: Body) -> Result<Value, ProxyError> {
    let bytes = body
        .collect()
        .await
        .map_err(|e| ProxyError::Upstream(format!("failed to read request body: {}", e)))?
        .to_bytes();
    if bytes.is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| ProxyError::Upstream(format!("invalid JSON body: {}", e)))
}

// ---------------------------------------------------------------------------
// Route selection helpers
// ---------------------------------------------------------------------------

/// Filters out disabled routes. Three-tier fallback:
/// 1. Exact tag match → all matching enabled routes
/// 2. Routes tagged "auto"
/// 3. All enabled routes as last resort
/// Modality isolation is handled by tags themselves (e.g. the "image" tag
/// only has image routes), so no category filtering is needed here.
fn find_candidate_routes<'a>(
    routes: &'a [Route],
    tag: &str,
    tags: &[crate::config::Tag],
) -> Vec<(usize, &'a Route)> {
    let exact: Vec<(usize, &_)> = routes
        .iter()
        .enumerate()
        .filter(|(_, r)| r.enabled && r.tags.iter().any(|t| t == tag))
        .collect();
    let candidates = if !exact.is_empty() {
        exact
    } else {
        let auto: Vec<(usize, &_)> = routes
            .iter()
            .enumerate()
            .filter(|(_, r)| r.enabled && r.tags.iter().any(|t| t == "auto"))
            .collect();
        if !auto.is_empty() {
            auto
        } else {
            routes
                .iter()
                .enumerate()
                .filter(|(_, r)| r.enabled)
                .collect()
        }
    };

    // Sort by tag's route_priority if configured
    let mut sorted = candidates;
    if let Some(tag_config) = tags.iter().find(|t| t.name == tag) {
        if !tag_config.route_priority.is_empty() {
            sorted.sort_by_key(|(_, route)| {
                tag_config.route_priority.get(&route.id).copied().unwrap_or(u32::MAX)
            });
        }
    }
    sorted
}

/// Whether a proxy error is retryable (connection-level failure or upstream 5xx).
/// Non-retryable errors (NoRoute, NoProvider, 4xx responses) are returned immediately.
fn is_retryable(err: &ProxyError) -> bool {
    // Any upstream error is retryable — try the next route in the failover chain.
    // Only config-level errors (no route, unknown provider) are non-retryable.
    matches!(err, ProxyError::Upstream(_))
}

async fn check_and_transition_circuit(
    route_id: &str,
    circuits: &Arc<RwLock<std::collections::HashMap<String, CircuitState>>>,
) -> Result<(), String> {
    let mut guard = circuits.write().await;
    let state = guard.entry(route_id.to_string()).or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    match state.status {
        CircuitStatus::Closed => Ok(()),
        CircuitStatus::HalfOpen => Ok(()),
        CircuitStatus::Open => {
            let elapsed = now.saturating_sub(state.opened_at_secs);
            if elapsed >= CIRCUIT_COOLDOWN_SECS {
                // Cooldown expired — transition to half-open for one probe
                state.status = CircuitStatus::HalfOpen;
                state.cooldown_remaining_secs = 0;
                log::info!("[CircuitBreaker] {} → half-open (probing)", route_id);
                Ok(())
            } else {
                let remaining = CIRCUIT_COOLDOWN_SECS - elapsed;
                state.cooldown_remaining_secs = remaining;
                Err(format!("circuit open ({}s cooldown remaining)", remaining))
            }
        }
    }
}

/// Check whether a route's circuit breaker allows a request (read-only).
/// For the full check that also transitions to half-open, see above.

async fn record_circuit_failure(
    route_id: &str,
    err: &ProxyError,
    circuits: &Arc<RwLock<std::collections::HashMap<String, crate::config::CircuitState>>>,
) {
    if !is_retryable(err) {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut guard = circuits.write().await;
    let state = guard.entry(route_id.to_string()).or_default();
    state.consecutive_failures += 1;
    state.last_error = Some(err.to_string());
    state.cooldown_remaining_secs = CIRCUIT_COOLDOWN_SECS;
    if state.consecutive_failures >= CIRCUIT_FAILURE_THRESHOLD
        && matches!(
            state.status,
            crate::config::CircuitStatus::Closed | crate::config::CircuitStatus::HalfOpen
        )
    {
        state.status = crate::config::CircuitStatus::Open;
        state.opened_at_secs = now;
        log::warn!(
            "[CircuitBreaker] {} → open ({} failures, cooldown {}s)",
            route_id,
            state.consecutive_failures,
            CIRCUIT_COOLDOWN_SECS
        );
    }
}

async fn close_circuit(
    route_id: &str,
    circuits: &Arc<RwLock<std::collections::HashMap<String, crate::config::CircuitState>>>,
) {
    let mut guard = circuits.write().await;
    if let Some(state) = guard.get_mut(route_id) {
        if !matches!(state.status, crate::config::CircuitStatus::Closed) {
            log::info!("[CircuitBreaker] {} → closed (success)", route_id);
        }
        state.status = crate::config::CircuitStatus::Closed;
        state.consecutive_failures = 0;
        state.cooldown_remaining_secs = 0;
        state.last_error = None;
    }
}

/// Replace the `tools` array in an Anthropic-format body with XML tool
/// definitions injected into the system prompt. Used when a route has
/// `tool_mode: react_xml` for providers that lack native function calling.
fn inject_react_xml_tools(body: &mut Value) {
    let tools = match body.get("tools").and_then(|t| t.as_array()) {
        Some(t) if !t.is_empty() => t.clone(),
        _ => return,
    };
    let xml_defs: Vec<String> = tools.iter().map(|tool| {
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let desc = tool.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let params = tool.get("input_schema")
            .map(|p| serde_json::to_string(p).unwrap_or_default())
            .unwrap_or_default();
        format!("<tool_definition>\n<tool_name>{name}</tool_name>\n<description>{desc}</description>\n<parameters>{params}</parameters>\n</tool_definition>")
    }).collect();
    let xml_section = format!(
        "\n\nYou have access to the following tools. To use a tool, output a <tool_use> block.\n\n{0}\n\nWhen you need to use a tool, respond with:\n<tool_use>\n<tool_name>tool_name_here</tool_name>\n<parameters>{{\"param\":\"value\"}}</parameters>\n</tool_use>",
        xml_defs.join("\n")
    );
    let current = body.get("system").and_then(|s| s.as_str()).unwrap_or("").to_string();
    body["system"] = Value::String(format!("{current}{xml_section}"));
    if let Some(obj) = body.as_object_mut() {
        obj.remove("tools");
        obj.remove("tool_choice");
    }
}

/// Parse `<tool_use>` XML blocks from a text string and return the remaining
/// plain text together with structured Anthropic `tool_use` content blocks.
/// Used in the response path when `tool_mode == ReactXml`.
fn scan_xml_tool_uses(text: &str) -> (String, Vec<Value>) {
    let mut remaining = String::new();
    let mut tool_uses: Vec<Value> = Vec::new();
    let mut pos = 0;
    while let Some(start) = text[pos..].find("<tool_use>") {
        let abs_start = pos + start;
        remaining.push_str(&text[pos..abs_start]);
        if let Some(end) = text[abs_start..].find("</tool_use>") {
            let abs_end = abs_start + end + "</tool_use>".len();
            let block = &text[abs_start + "<tool_use>".len()..abs_start + end];
            let name = extract_xml_tag(block, "tool_name");
            let params_str = extract_xml_tag(block, "parameters");
            let input: Value =
                serde_json::from_str(params_str.trim()).unwrap_or(json!({}));
            tool_uses.push(json!({
                "type": "tool_use",
                "id": format!("toolu_{:x}", std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()),
                "name": name.trim(),
                "input": input
            }));
            pos = abs_end;
        } else {
            remaining.push_str(&text[abs_start..]);
            break;
        }
    }
    remaining.push_str(&text[pos..]);
    (remaining, tool_uses)
}

fn extract_xml_tag(xml: &str, tag: &str) -> String {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    if let Some(start) = xml.find(&open) {
        let after = start + open.len();
        if let Some(end) = xml[after..].find(&close) {
            return xml[after..after + end].to_string();
        }
    }
    String::new()
}

/// Walk the content blocks of an Anthropic response and replace text blocks
/// that contain `<tool_use>` XML with the parsed structured tool_use blocks.
fn extract_xml_tool_uses(resp: &mut Value) {
    let content = match resp.get_mut("content").and_then(|c| c.as_array_mut()) {
        Some(c) => c,
        None => return,
    };
    let mut new_content: Vec<Value> = Vec::new();
    for block in content.iter() {
        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if block_type == "text" {
            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                let (remaining, tool_uses) = scan_xml_tool_uses(text);
                if !remaining.is_empty() {
                    new_content.push(json!({"type": "text", "text": remaining}));
                }
                for tu in tool_uses {
                    new_content.push(tu);
                }
            } else {
                new_content.push(block.clone());
            }
        } else {
            new_content.push(block.clone());
        }
    }
    *content = new_content;
}

/// Detect upstream error bodies that signal the request exceeded the provider's
/// context window / token limit. These are per-route limits (not malformed
/// requests), so failover to another route with a larger window may succeed.
fn is_context_limit_error(err_body: &str) -> bool {
    let lower = err_body.to_lowercase();
    const MARKERS: &[&str] = &[
        "context length",
        "context window",
        "maximum context",
        "context_length_exceeded",
        "token limit",
        "tokens limit",
        "maximum number of tokens",
        "too many tokens",
        "prompt is too long",
        "request too large",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

fn is_chat_format(format: &ProviderFormat) -> bool {
    matches!(
        format,
        ProviderFormat::Anthropic | ProviderFormat::Openai | ProviderFormat::OpenaiResponses
    )
}

fn is_image_format(format: &ProviderFormat) -> bool {
    matches!(
        format,
        ProviderFormat::OpenaiImages
            | ProviderFormat::DashscopeImage
            | ProviderFormat::DashscopeChatImage
            | ProviderFormat::MinimaxImage
    )
}

// ─── Multimodal extraction helpers ─────────────────────────────────────────
//
// OpenCarrier sends non-chat capabilities (image/tts/audio) as OpenAI chat
// requests: the payload is in `messages`. These helpers pull the relevant
// content out of that chat-shaped body.

/// Extract text from the last user message. Handles both plain-string content
/// and the content-block array form (`[{"type":"text","text":"..."}, ...]`).
/// Used for image prompt and TTS text.
fn last_user_text(body: &Value) -> Option<String> {
    let messages = body.get("messages").and_then(|m| m.as_array())?;
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let content = msg.get("content")?;
        if let Some(s) = content.as_str() {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
        if let Some(arr) = content.as_array() {
            let mut parts = Vec::new();
            for block in arr {
                // content blocks: {"type":"text","text":"..."} or {"text":"..."}
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }
            if !parts.is_empty() {
                return Some(parts.join(""));
            }
        }
    }
    None
}

/// Find an audio content block anywhere in messages and return its decoded
/// (base64, format). Handles OpenAI chat audio format
/// `{"type":"input_audio","input_audio":{"data":"<b64>","format":"mp3"}}`
/// and strips a `data:audio/...;base64,` prefix if present.
fn find_input_audio(body: &Value) -> Option<(String, String)> {
    let messages = body.get("messages").and_then(|m| m.as_array())?;
    for msg in messages {
        let content = msg.get("content")?;
        if let Some(arr) = content.as_array() {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) != Some("input_audio") {
                    continue;
                }
                let audio = block.get("input_audio")?;
                let raw = audio.get("data").and_then(|d| d.as_str())?;
                let (data, fmt_from_data) = if let Some((mime, b64)) = raw.split_once(";base64,") {
                    // data URL form: data:audio/mp3;base64,...
                    let fmt = mime
                        .strip_prefix("data:audio/")
                        .unwrap_or("mp3")
                        .to_string();
                    (b64.to_string(), Some(fmt))
                } else {
                    (raw.to_string(), None)
                };
                let fmt = audio
                    .get("format")
                    .and_then(|f| f.as_str())
                    .map(|s| s.to_string())
                    .or(fmt_from_data)
                    .unwrap_or_else(|| "mp3".to_string());
                return Some((data, fmt));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Core proxy logic (protocol-aware)
// ---------------------------------------------------------------------------

async fn handle_proxy(
    client_protocol: &str,
    State(state): State<AppState>,
    headers: HeaderMap,
    caller_key_id: Option<i64>,
    body: axum::Json<Value>,
) -> Result<Response, ProxyError> {

    let body = body.0;
    let request_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let msg_count = body
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let is_streaming = body
        .as_object()
        .and_then(|o| o.get("stream"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    log::info!(
        "[Proxy] incoming request: protocol={}, model={}, {} messages, stream={}",
        client_protocol,
        request_model,
        msg_count,
        is_streaming
    );

    let start = std::time::Instant::now();

    // Read config and immediately drop the lock to avoid blocking admin config writes.
    // Clone the entire config — it's small (5.8KB) and avoids holding RwLockReadGuard
    // for the duration of streaming responses (which can be 30+ seconds).
    let config = state.config.read().await.clone();

    // 1. Resolve tag from model, purely from configured tag names
    let tag = resolve_tag_from_model(&request_model, &config.tags)
        .unwrap_or_else(|| config.current_tag.clone());

    // 1b. Smart auto routing: if the tag is an "auto" tag, inspect the
    //     request body to pick a more specific tier.
    let tag = if config.smart_routing.enabled
        && config.tags.iter().any(|t| t.name == tag && t.is_auto)
    {
        match crate::smart_routing::route(
            &body, client_protocol, caller_key_id,
            &config.smart_routing, &state.smart_routing_cache,
        ).await {
            Some(smart_tag) => {
                log::info!("[SmartRouting] auto → {} (caller={:?})", smart_tag, caller_key_id);
                smart_tag
            }
            None => tag,
        }
    } else {
        tag
    };

    // 2. Find candidate routes for this tag (sorted by tag's route_priority)
    let candidates = find_candidate_routes(&config.routes, &tag, &config.tags);

    // Fast/non-reasoning tiers must never return reasoning chains to the client.
    // Upstream models (glm-5.1, deepseek-v4-flash) reason by default and leak
    // reasoning_content / thinking blocks into the response, which confuses
    // apps that use these tiers for classification or simple chat.
    let strip_reasoning = matches!(tag.as_str(), "fast" | "haiku");
    if candidates.is_empty() {
        let _ = crate::db::insert_usage_log(
            &state.db,
            crate::db::UsageInsert {
                caller_key_id,
                tag: tag.clone(),
                provider: "".into(),
                model: "".into(),
                request_model: request_model.clone(),
                modality: String::new(),
                input_tokens: None,
                output_tokens: None,
                latency_ms: start.elapsed().as_millis() as i64,
                status: "error".into(),
                error_message: Some("no route".into()),
            },
        )
        .await;
        return Err(ProxyError::NoRoute(tag.clone()));
    }

    let mut last_error: Option<ProxyError> = None;
    let mut last_modality: String = String::new();
    // Track how many candidates failed due to rate-limiting (429).
    let mut rate_limited_count: u32 = 0;

    for (attempt, (_route_idx, route)) in candidates.iter().enumerate() {
        if attempt > 0 {
            log::warn!("[Proxy] {} failover: trying route #{} (provider={}, model={})",
                tag, attempt + 1, route.provider, route.model);
        }

        // Check circuit breaker before hitting this route
        {
            if let Err(reason) = check_and_transition_circuit(&route.id, &state.circuit_breaker).await {
                log::warn!("[Proxy] skipping route {} ({}): {}", route.id, route.provider, reason);
                last_error = Some(ProxyError::Upstream(format!("route {} circuit open: {}", route.id, reason)));
                continue;
            }
        }

        let provider_format = &route.format;
        last_modality = format!("{:?}", route.format);
        let provider = match config.providers.get(&route.provider) {
            Some(p) => p,
            None => {
                log::warn!("[Proxy] route references unknown provider '{}', skipping", route.provider);
                last_error = Some(ProxyError::NoProvider(route.provider.clone()));
                continue;
            }
        };

        // 3. Validate provider API key
        if provider.api_key.is_empty() || provider.api_key.starts_with("sk-your-") {
            log::warn!("[Proxy] provider '{}' has no valid API key, skipping", route.provider);
            last_error = Some(ProxyError::NoProvider(format!(
                "provider '{}' has no valid API key configured",
                route.provider
            )));
            continue;
        }

        // 4. Non-chat capabilities (image/tts/audio) are dispatched by format
        //    and early-return. They MUST NOT fall through to the chat fwd_body /
        //    streaming conversion below — their bodies are not chat-shaped.
        if !is_chat_format(provider_format) {
            match provider_format {
                ProviderFormat::OpenaiImages
                | ProviderFormat::DashscopeImage
                | ProviderFormat::DashscopeChatImage
                | ProviderFormat::MinimaxImage => {
                    match handle_image_request(state.clone(), caller_key_id, route, provider, &body, start).await {
                        Ok(resp) => return Ok(resp),
                        Err(e) => {
                            log::warn!("[Proxy] image route failed: {}", e);
                            if is_retryable(&e) { last_error = Some(e); continue; }
                            return Err(e);
                        }
                    }
                }
                ProviderFormat::DashscopeTts => {
                    match handle_tts_request(state.clone(), caller_key_id, route, provider, &body, start).await {
                        Ok(resp) => return Ok(resp),
                        Err(e) => {
                            log::warn!("[Proxy] tts route failed: {}", e);
                            if is_retryable(&e) { last_error = Some(e); continue; }
                            return Err(e);
                        }
                    }
                }
                ProviderFormat::DashscopeAsr => {
                    match handle_asr_request(state.clone(), caller_key_id, route, provider, &body, start).await {
                        Ok(resp) => return Ok(resp),
                        Err(e) => {
                            log::warn!("[Proxy] audio route failed: {}", e);
                            if is_retryable(&e) { last_error = Some(e); continue; }
                            return Err(e);
                        }
                    }
                }
                other => {
                    last_error = Some(ProxyError::Upstream(format!(
                        "format {:?} is not supported for non-chat dispatch", other
                    )));
                    continue;
                }
            }
        }

    log::info!(
        "[Proxy] {} → {} (model={}, format={:?})",
        tag,
        provider.name,
        route.model,
        provider_format
    );

    // 4. Build forwarded request body based on protocol conversion
    let mut fwd_body = match (client_protocol, provider_format) {
        ("anthropic", ProviderFormat::Anthropic) => {
            // Anthropic → Anthropic: passthrough with fixes
            // Note: don't strip_thinking here — Anthropic-compatible providers
            // like Zhipu support thinking blocks
            let mut b = body.clone();
            b["model"] = Value::String(route.model.clone());
            normalize_roles(&mut b);
            strip_anthropic_specific_fields(&mut b);
            inject_reasoning_content(&mut b);
            if route.tool_mode == crate::config::ToolMode::ReactXml {
                inject_react_xml_tools(&mut b);
            }
            b
        }
        ("anthropic", ProviderFormat::Openai) => {
            // Anthropic → OpenAI Chat Completions.
            // Don't strip_thinking here — anthropic_to_openai_request converts
            // thinking blocks to reasoning_content which providers like DeepSeek
            // require to be echoed back when thinking mode is active.
            let mut b = body.clone();
            normalize_roles(&mut b);
            strip_anthropic_specific_fields(&mut b);
            // Remove the top-level thinking config — OpenAI providers don't
            // understand it, but keep the thinking blocks in messages for
            // conversion to reasoning_content.
            if let Some(obj) = b.as_object_mut() { obj.remove("thinking"); }
            convert::anthropic_to_openai_request(&b, &route.model)
        }
        ("anthropic", ProviderFormat::OpenaiResponses) => {
            // Anthropic → OpenAI Responses API
            let mut b = body.clone();
            normalize_roles(&mut b);
            strip_anthropic_specific_fields(&mut b);
            strip_thinking(&mut b);
            if let Some(obj) = b.as_object_mut() { obj.remove("thinking"); }
            convert::anthropic_to_responses_request(&b, &route.model)
        }
        ("openai", ProviderFormat::Openai) => {
            // OpenAI Chat → OpenAI Chat: passthrough
            let mut b = body.clone();
            b["model"] = Value::String(route.model.clone());
            b
        }
        ("openai", ProviderFormat::OpenaiResponses) => {
            // OpenAI Chat → OpenAI Responses
            convert::openai_to_responses_request(&body, &route.model)
        }
        ("openai", ProviderFormat::Anthropic) => {
            // OpenAI Chat → Anthropic Messages
            convert::openai_to_anthropic_request(&body, &route.model)
        }
        ("openai_responses", ProviderFormat::OpenaiResponses) => {
            // Responses → Responses: passthrough
            let mut b = body.clone();
            b["model"] = Value::String(route.model.clone());
            b
        }
        ("openai_responses", ProviderFormat::Openai) => {
            // Codex Responses → OpenAI Chat Completions provider
            convert::responses_to_chat_request(&body, &route.model)
        }
        ("openai_responses", ProviderFormat::Anthropic) => {
            // Codex Responses → Anthropic Messages provider
            convert::responses_to_anthropic_request(&body, &route.model)
        }
        _ => {
            return Err(ProxyError::Upstream(format!(
                "unsupported protocol conversion: {} → {:?}",
                client_protocol, provider_format
            )));
        }
    };

    // 4b. For OpenAI Chat format, ensure reasoning_content is present on all
    //     assistant messages when thinking mode is active. Clients like
    //     OpenCarrier don't echo reasoning_content back, but providers like
    //     DeepSeek require it and return 400 if it's missing.
    if matches!(provider_format, ProviderFormat::Openai) {
        inject_openai_reasoning_content(&mut fwd_body);
    }

    // 4c. For Anthropic format, if the route's tag suggests reasoning/thinking
    //     is needed (e.g. "reasoning" tag) and the request doesn't already have
    //     a thinking config, inject one. This ensures providers like Zhipu GLM
    //     activate thinking mode even when the client uses OpenAI Chat format
    //     (which has no thinking field).
    if matches!(provider_format, ProviderFormat::Anthropic) {
        let needs_thinking = route.tags.iter().any(|t| t == "reasoning")
            || tag == "reasoning";
        if needs_thinking && fwd_body.get("thinking").is_none() {
            if let Some(obj) = fwd_body.as_object_mut() {
                obj.insert("thinking".to_string(), json!({"type": "enabled", "budget_tokens": 10000}));
            }
        }
    }

    // 5. Build URL
    let url = format!(
        "{}{}",
        route.base_url.trim_end_matches('/'),
        route.format.path()
    );
    log::info!("[Proxy] forwarding to {} (model={})", url, route.model);

    // 6. Build headers
    let mut req_builder = state.http_client.post(&url);

    // DEBUG: log the forwarded body (size + truncated preview) to help diagnose
    // validation and token-count issues.
    if let Ok(s) = serde_json::to_string(&fwd_body) {
        let msg_count = fwd_body.get("messages").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0);
        let preview: String = s.chars().take(160).collect();
        log::info!("[Proxy] forwarding body ({} bytes, {} msgs): {}...", s.len(), msg_count, preview);
    }

    // Auth
    req_builder = req_builder.header(
        provider.auth_type.header_name(),
        provider.auth_type.header_value(&provider.api_key),
    );

    // Forward client headers (hop-by-hop and auth headers are filtered internally)
    req_builder = forward_client_headers(&headers, req_builder);

    let req_timeout = std::time::Duration::from_secs(
        if is_streaming {
            STREAM_TIMEOUT
        } else if tag == "reasoning" {
            NON_STREAM_TIMEOUT_REASONING
        } else {
            NON_STREAM_TIMEOUT
        }
    );

    // 7. Send. Keep a clone AFTER setting the body so same-route retry
    //    re-sends the full request (not an empty body).
    let json_builder = req_builder.json(&fwd_body);
    let req_clone = json_builder.try_clone();
    let resp = match json_builder
        .timeout(req_timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let err = ProxyError::Upstream(e.to_string());
            if is_retryable(&err) {
                log::warn!("[Proxy] {} timeout/connection error (retryable): {}", if is_streaming { "streaming" } else { "non-streaming" }, err);
                record_circuit_failure(&route.id, &err, &state.circuit_breaker).await;
                last_error = Some(err);
                continue;
            }
            return Err(err);
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let status_code = status.as_u16();
        let err_body = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                log::warn!("[Proxy] failed to read upstream error body: {}", e);
                String::new()
            }
        };
        log::warn!(
            "[Proxy] <<< {} {}: {}",
            status_code,
            status.canonical_reason().unwrap_or("?"),
            truncate_chars(&err_body, 300)
        );
        // 5xx server errors and 429 (rate limit) → retryable, try next candidate
        if status_code >= 500 || status_code == 429 {
            if status_code == 429 { rate_limited_count += 1; }
            let err = ProxyError::Upstream(format!("HTTP {}: {}",
                status_code, truncate_chars(&err_body, 200)));
            log::warn!("[Proxy] upstream {} (retryable): {}", status_code, err);
            record_circuit_failure(&route.id, &err, &state.circuit_breaker).await;
            last_error = Some(err);
            continue;
        }
        // 400 "context length / token limit exceeded" is a per-route limit, not a
        // malformed request — a different route may have a higher context window.
        // Retry on the next candidate so an oversized request isn't hard-failed
        // just because the first route's limit is smaller.
        if status_code == 400 && is_context_limit_error(&err_body) {
            let err = ProxyError::Upstream(format!(
                "HTTP 400 context limit: {}",
                truncate_chars(&err_body, 200)
            ));
            log::warn!("[Proxy] {} context-limit 400 (retryable): trying next route", tag);
            last_error = Some(err);
            continue;
        }
        // 4xx client errors → non-retryable, return immediately. Still log usage
        // so the dashboard surfaces auth failures, rate limits, and bad requests.
        let axum_status =
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY);
        let _ = crate::db::insert_usage_log(
            &state.db,
            crate::db::UsageInsert {
                caller_key_id,
                tag: tag.clone(),
                provider: provider.name.clone(),
                model: route.model.clone(),
                request_model: request_model.clone(),
                modality: format!("{:?}", route.format),
                input_tokens: None,
                output_tokens: None,
                latency_ms: start.elapsed().as_millis() as i64,
                status: "error".to_string(),
                error_message: Some(format!(
                    "HTTP {}: {}",
                    status_code,
                    truncate_chars(&err_body, 200)
                )),
            },
        )
        .await;
        return Ok((
            axum_status,
            [("content-type", "application/json")],
            err_body,
        )
            .into_response());
    }

    // Record successful request log (placeholder; tokens updated for non-streaming)
    let usage_log_id = crate::db::insert_usage_log(
        &state.db,
        crate::db::UsageInsert {
            caller_key_id,
            tag: tag.clone(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            request_model: request_model.clone(),
            modality: format!("{:?}", route.format),
            input_tokens: None,
            output_tokens: None,
            latency_ms: start.elapsed().as_millis() as i64,
            status: "success".to_string(),
            error_message: None,
        },
    )
    .await
    .ok();

    // Close the circuit on success — the route is healthy.
    close_circuit(&route.id, &state.circuit_breaker).await;

    // 8. Convert response if needed
    if is_streaming {
        // ── Drain upstream in background to capture full usage even if client disconnects ──
        // The upstream response must be fully consumed so the usage tee sees the
        // message_delta event (which contains the final token counts). Without this,
        // a client disconnect causes Axum to drop the response body, which stops the
        // stream converter from polling upstream, and the tee misses the tail of the SSE.
        let (drain_tx, drain_rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();
        let db = state.db.clone();
        let log_id = usage_log_id;
        let usage_format = provider_format.clone();
        {
            let db_c = db.clone();
            let log_id_c = log_id;
            let usage_format_c = usage_format.clone();
            let upstream = resp.bytes_stream();
            tokio::spawn(async move {
                const MAX_USAGE_BUF: usize = 1024 * 1024; // 1 MB cap for usage extraction
                let mut buf = Vec::new();
                let mut capped = false;
                let mut upstream = Box::pin(upstream);
                while let Some(result) = upstream.next().await {
                    if let Ok(ref bytes) = result {
                        if !capped && buf.len() + bytes.len() <= MAX_USAGE_BUF {
                            buf.extend_from_slice(bytes);
                        } else if !capped {
                            buf.extend_from_slice(&bytes[..MAX_USAGE_BUF.saturating_sub(buf.len())]);
                            capped = true;
                            log::warn!("[Proxy] usage extraction buffer capped at {} bytes", MAX_USAGE_BUF);
                        }
                    }
                    let _ = drain_tx.send(result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
                }
                let (input, output) = extract_usage_from_sse_buffer(&buf, &usage_format_c);
                if let (Some(i), Some(o)) = (input, output) {
                    if let Some(id) = log_id_c {
                        let _ = crate::db::update_usage_tokens(&db_c, id, i, o).await;
                    }
                } else if log_id_c.is_some() {
                    log::warn!("[Proxy] usage extraction incomplete: input={:?}, output={:?} for log_id={:?}, buf_len={}", input, output, log_id_c, buf.len());
                }
            });
        }

        let mut drain_rx = drain_rx;
        let raw_stream = async_stream::stream! {
            while let Some(item) = drain_rx.recv().await {
                yield item;
            }
        };

        match (client_protocol, provider_format) {
            ("anthropic", ProviderFormat::Openai) => {
                // Convert OpenAI Chat SSE → Anthropic SSE
                let converted = convert::convert_openai_stream_to_anthropic(
                    Box::pin(raw_stream),
                    request_model,
                );
                let body = Body::from_stream(converted);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
            ("anthropic", ProviderFormat::OpenaiResponses) => {
                // Convert OpenAI Responses SSE → Anthropic SSE
                let converted = convert::convert_responses_stream_to_anthropic(
                    Box::pin(raw_stream),
                    request_model,
                );
                let body = Body::from_stream(converted);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
            ("openai_responses", ProviderFormat::Openai) => {
                // Convert OpenAI Chat SSE → Responses SSE (for Codex)
                let converted = convert::convert_chat_stream_to_responses(
                    Box::pin(raw_stream),
                    &request_model,
                );
                let body = Body::from_stream(converted);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
            ("openai_responses", ProviderFormat::Anthropic) => {
                // Convert Anthropic SSE → Responses SSE (for Codex)
                let converted = convert::convert_anthropic_stream_to_responses(
                    Box::pin(raw_stream),
                    &request_model,
                );
                let body = Body::from_stream(converted);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
            ("openai", ProviderFormat::OpenaiResponses) => {
                // Convert OpenAI Responses SSE → OpenAI Chat SSE
                let converted = convert::convert_responses_stream_to_chat(
                    Box::pin(raw_stream),
                    &request_model,
                );
                let body = Body::from_stream(converted);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
            ("openai", ProviderFormat::Anthropic) => {
                // Convert Anthropic SSE → OpenAI Chat SSE
                let converted = convert::convert_anthropic_stream_to_openai(
                    Box::pin(raw_stream),
                    request_model.clone(),
                );
                let boxed: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, std::io::Error>> + Send>> =
                    if strip_reasoning {
                        strip_reasoning_from_openai_stream(Box::pin(converted))
                    } else {
                        Box::pin(converted)
                    };
                let body = Body::from_stream(boxed);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
            ("anthropic", ProviderFormat::Anthropic) => {
                // Anthropic passthrough: replace model to match client's requested model name
                let converted = replace_model_in_anthropic_stream(Box::pin(raw_stream), request_model);
                let body = Body::from_stream(converted);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
            ("openai", ProviderFormat::Openai)
            | ("openai_responses", ProviderFormat::OpenaiResponses) => {
                // Passthrough: forward raw bytes without JSON parsing
                let stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, std::io::Error>> + Send>> =
                    if strip_reasoning && client_protocol == "openai" {
                        strip_reasoning_from_openai_stream(Box::pin(raw_stream))
                    } else {
                        Box::pin(raw_stream)
                    };
                let body = Body::from_stream(stream);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
            _ => {
                // Passthrough streaming with model preservation
                let converted = replace_model_in_anthropic_stream(Box::pin(raw_stream), request_model);
                let body = Body::from_stream(converted);

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(body)
                    .map_err(|e| ProxyError::Upstream(format!("failed to build streaming response: {}", e)))?)
            }
        }
    } else {
        // Non-streaming
        let status_code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK);
        log::info!("[Proxy] non-streaming response received: status={} latency={}ms", status.as_u16(), start.elapsed().as_millis());
        let resp_body = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                // Transient truncation (upstream closed mid-body). Retry the SAME
                // route up to SAME_ROUTE_RETRIES times before cross-route failover
                // — keeps the same model and usually succeeds on immediate retry.
                let mut last_err = e.to_string();
                let mut recovered: Option<bytes::Bytes> = None;
                // Only retry if the original response was fast. A slow response
                // that then truncated signals upstream OVERLOAD (e.g. deepseek
                // 43s + bad body) — retrying just wastes the remaining time budget
                // and risks blowing the client's deadline. Fail over instead.
                let elapsed = start.elapsed();
                let retry_budget_ok = elapsed < req_timeout / 2;
                if let Some(base) = req_clone.as_ref() {
                    for attempt in 1..=SAME_ROUTE_RETRIES {
                        if !retry_budget_ok {
                            log::warn!(
                                "[Proxy] non-streaming body read error after slow response ({}ms), skipping same-route retry — failing over",
                                elapsed.as_millis()
                            );
                            break;
                        }
                        log::warn!(
                            "[Proxy] non-streaming body read error, same-route retry #{}: {}",
                            attempt, last_err
                        );
                        tokio::time::sleep(SAME_ROUTE_DELAY).await;
                        let retry = match base.try_clone() {
                            Some(c) => c.timeout(req_timeout).send().await,
                            None => break,
                        };
                        match retry {
                            Ok(r) if r.status().is_success() => match r.bytes().await {
                                Ok(b) => {
                                    log::info!(
                                        "[Proxy] same-route retry #{} recovered response ({} bytes)",
                                        attempt, b.len()
                                    );
                                    recovered = Some(b);
                                    break;
                                }
                                Err(e2) => last_err = e2.to_string(),
                            },
                            Ok(r) => {
                                last_err = format!("retry returned status {}", r.status());
                                break;
                            }
                            Err(e2) => last_err = e2.to_string(),
                        }
                    }
                }
                match recovered {
                    Some(b) => b,
                    None => {
                        let err = ProxyError::Upstream(last_err);
                        log::warn!(
                            "[Proxy] non-streaming body read failed after same-route retries, failing over: {}",
                            err
                        );
                        last_error = Some(err);
                        continue;
                    }
                }
            }
        };

        // Try to extract usage tokens from upstream response and update the log.
        if let Some(log_id) = usage_log_id {
            if let Ok(value) = serde_json::from_slice::<Value>(&resp_body) {
                let (input, output) = extract_usage_tokens(&value, provider_format);
                if input.is_some() || output.is_some() {
                    let _ = crate::db::update_usage_tokens_opt(
                        &state.db,
                        log_id,
                        input,
                        output,
                    )
                    .await;
                }
            }
        }

        log::info!("[Proxy] non-streaming complete: {} bytes, total latency={}ms", resp_body.len(), start.elapsed().as_millis());

        match (client_protocol, provider_format) {
            ("anthropic", ProviderFormat::Openai) => {
                // Convert OpenAI Chat response → Anthropic response
                let openai_resp: Value = parse_upstream_json(&resp_body)?;
                let anthropic_resp = convert::openai_to_anthropic_response(&openai_resp, &request_model);
                let anthropic_bytes = serde_json::to_vec(&anthropic_resp).unwrap_or(resp_body.to_vec());
                return Ok((
                    status_code,
                    [("content-type", "application/json")],
                    anthropic_bytes,
                )
                    .into_response())
            }
            ("anthropic", ProviderFormat::OpenaiResponses) => {
                // Convert OpenAI Responses response → Anthropic response
                let responses_resp: Value = parse_upstream_json(&resp_body)?;
                let anthropic_resp = convert::responses_to_anthropic_response(&responses_resp, &request_model);
                let anthropic_bytes = serde_json::to_vec(&anthropic_resp).unwrap_or(resp_body.to_vec());
                return Ok((
                    status_code,
                    [("content-type", "application/json")],
                    anthropic_bytes,
                )
                    .into_response())
            }
            ("openai", ProviderFormat::OpenaiResponses) => {
                // Convert OpenAI Responses response → OpenAI Chat response
                let responses_resp: Value = parse_upstream_json(&resp_body)?;
                let openai_resp = convert::responses_to_openai_response(&responses_resp, &request_model);
                let openai_bytes = serde_json::to_vec(&openai_resp).unwrap_or(resp_body.to_vec());
                return Ok((
                    status_code,
                    [("content-type", "application/json")],
                    openai_bytes,
                )
                    .into_response())
            }
            ("openai", ProviderFormat::Anthropic) => {
                // Convert Anthropic response → OpenAI Chat response
                let anthropic_resp: Value = parse_upstream_json(&resp_body)?;
                let mut openai_resp = convert::anthropic_to_openai_response(&anthropic_resp, &request_model);
                if strip_reasoning {
                    strip_reasoning_from_openai_response(&mut openai_resp);
                }
                let openai_bytes = serde_json::to_vec(&openai_resp).unwrap_or(resp_body.to_vec());
                return Ok((
                    status_code,
                    [("content-type", "application/json")],
                    openai_bytes,
                )
                    .into_response())
            }
            ("openai_responses", ProviderFormat::Openai) => {
                // Convert OpenAI Chat response → Responses API response (for Codex)
                let chat_resp: Value = parse_upstream_json(&resp_body)?;
                let responses_resp = convert::chat_to_responses_response(&chat_resp, &request_model);
                let bytes = serde_json::to_vec(&responses_resp).unwrap_or(resp_body.to_vec());
                return Ok((
                    status_code,
                    [("content-type", "application/json")],
                    bytes,
                )
                    .into_response())
            }
            ("openai_responses", ProviderFormat::Anthropic) => {
                // Convert Anthropic response → Responses API response (for Codex)
                let anthropic_resp: Value = parse_upstream_json(&resp_body)?;
                let responses_resp = convert::anthropic_to_responses_response(&anthropic_resp, &request_model);
                let bytes = serde_json::to_vec(&responses_resp).unwrap_or(resp_body.to_vec());
                return Ok((
                    status_code,
                    [("content-type", "application/json")],
                    bytes,
                )
                    .into_response())
            }
            _ => {
                // Passthrough response with model preservation
                // Parse and replace model field to prevent client feedback loop
                let mut resp_value: Value = parse_upstream_json(&resp_body)?;

                // Override the model field to prevent client from updating its model
                // to provider's model name (e.g., prevent "glm-5.1" from replacing "claude-opus-4-8")
                if let Some(obj) = resp_value.as_object_mut() {
                    obj.insert("model".to_string(), Value::String(request_model.clone()));
                }

                // For fast/non-reasoning tiers, drop reasoning_content that the
                // upstream model emits even though thinking was never requested.
                if strip_reasoning && client_protocol == "openai" {
                    strip_reasoning_from_openai_response(&mut resp_value);
                }

                // ReAct XML: scan text content blocks for <tool_use> and
                // convert them to structured tool_use content blocks.
                if route.tool_mode == crate::config::ToolMode::ReactXml
                    && client_protocol == "anthropic"
                {
                    extract_xml_tool_uses(&mut resp_value);
                }

                let resp_bytes = serde_json::to_vec(&resp_value).unwrap_or(resp_body.to_vec());

                return Ok((
                    status_code,
                    [("content-type", "application/json")],
                    resp_bytes,
                )
                    .into_response())
            }
        }
    }
    } // end of for loop over candidates

    // All candidates failed — return a clean, user-friendly error instead of
    // leaking the raw upstream provider error to the user.
    let _ = crate::db::insert_usage_log(
        &state.db,
        crate::db::UsageInsert {
            caller_key_id,
            tag: tag.clone(),
            provider: "".into(),
            model: "".into(),
            request_model: request_model.clone(),
            modality: last_modality.clone(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: start.elapsed().as_millis() as i64,
            status: "error".into(),
            error_message: last_error.as_ref().map(|e| e.to_string()),
        },
    )
    .await;

    // All candidates failed — if they were all rate-limited, return 429 so the
    // client knows to retry with backoff instead of crashing on an unexpected 502.
    if rate_limited_count > 0 && rate_limited_count as usize == candidates.len() {
        Err(ProxyError::RateLimited("all providers rate limited, please retry later".into()))
    } else {
        Err(ProxyError::Upstream("all providers unavailable, please try again later".into()))
    }
}

// ---------------------------------------------------------------------------
// Count tokens handler
// ---------------------------------------------------------------------------

async fn handle_count_tokens(
    client_protocol: &str,
    State(state): State<AppState>,
    headers: HeaderMap,
    caller_key_id: Option<i64>,
    body: axum::Json<Value>,
) -> Result<Response, ProxyError> {
    let body = body.0;
    let request_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    log::info!("[Proxy] count_tokens request: protocol={}, model={}", client_protocol, request_model);

    let config = state.config.read().await.clone();

    let tag = resolve_tag_from_model(&request_model, &config.tags)
        .unwrap_or_else(|| config.current_tag.clone());

    // Smart auto routing: if the tag is an "auto" tag, inspect the
    // request body to pick a more specific tier.
    let tag = if config.smart_routing.enabled
        && config.tags.iter().any(|t| t.name == tag && t.is_auto)
    {
        match crate::smart_routing::route(
            &body, client_protocol, caller_key_id,
            &config.smart_routing, &state.smart_routing_cache,
        ).await {
            Some(smart_tag) => {
                log::info!("[SmartRouting] count_tokens auto → {} (caller={:?})", smart_tag, caller_key_id);
                smart_tag
            }
            None => tag,
        }
    } else {
        tag
    };

    let candidates = find_candidate_routes(&config.routes, &tag, &config.tags);
    if candidates.is_empty() {
        let _ = crate::db::insert_usage_log(
            &state.db,
            crate::db::UsageInsert {
                caller_key_id,
                tag: tag.clone(),
                provider: "".into(),
                model: "".into(),
                request_model: request_model.clone(),
                modality: String::new(),
                input_tokens: None,
                output_tokens: None,
                latency_ms: 0,
                status: "error".into(),
                error_message: Some("no route".into()),
            },
        )
        .await;
        return Err(ProxyError::NoRoute(tag.clone()));
    }

    for (attempt, (_route_idx, route)) in candidates.iter().enumerate() {
        if attempt > 0 {
            log::warn!("[Proxy] count_tokens {} failover: trying route #{} (provider={}, model={})",
                tag, attempt + 1, route.provider, route.model);
        }

        if !is_chat_format(&route.format) {
            let err = ProxyError::Upstream(format!(
                "count_tokens is not supported for route format {:?}",
                route.format
            ));
            log::warn!("[Proxy] count_tokens skipping route: {}", err);
            continue;
        }

        let provider = match config.providers.get(&route.provider) {
            Some(p) => p,
            None => {
                log::warn!("[Proxy] count_tokens route references unknown provider '{}', skipping", route.provider);
                continue;
            }
        };

    if provider.api_key.is_empty() || provider.api_key.starts_with("sk-your-") {
        log::warn!("[Proxy] count_tokens provider '{}' has no valid API key, skipping", route.provider);
        continue;
    }

    log::info!(
        "[Proxy] count_tokens: {} → {} (model={}, format={:?})",
        tag,
        provider.name,
        route.model,
        route.format
    );

    // Apply protocol conversion based on (client_protocol, provider_format)
    let fwd_body = match (client_protocol, &route.format) {
        ("anthropic", ProviderFormat::Anthropic) => {
            let mut b = body.clone();
            b["model"] = Value::String(route.model.clone());
            normalize_roles(&mut b);
            b
        }
        _ => {
            return Err(ProxyError::Upstream(format!(
                "count_tokens only supports Anthropic-format routes, got {:?}",
                route.format
            )));
        }
    };

    let url = format!(
        "{}{}/count_tokens",
        route.base_url.trim_end_matches('/'),
        route.format.path()
    );
    log::info!("[Proxy] count_tokens forwarding to {}", url);

    let mut req_builder = state.http_client.post(&url);
    req_builder = req_builder.header(
        provider.auth_type.header_name(),
        provider.auth_type.header_value(&provider.api_key),
    );

    req_builder = forward_client_headers(&headers, req_builder);

    let resp = match req_builder
        .json(&fwd_body)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let err = ProxyError::Upstream(e.to_string());
            if is_retryable(&err) {
                log::warn!("[Proxy] count_tokens connection error (retryable): {}", err);
                continue;
            }
            return Err(err);
        }
    };

    let status = resp.status();
    let status_code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK);
    let resp_body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            let err = ProxyError::Upstream(e.to_string());
            if is_retryable(&err) {
                log::warn!("[Proxy] count_tokens body read error (retryable): {}", err);
                continue;
            }
            return Err(err);
        }
    };

    if !status.is_success() {
        let body_str = String::from_utf8_lossy(&resp_body);
        // Fallback statuses mean the upstream simply doesn't expose /count_tokens
        // (common for OpenAI-format providers). This is expected and handled by a
        // local estimate — log at debug, not warn, to avoid log spam on routine
        // advisory calls. Genuine errors (400/401/403/429/…) still WARN below.
        if matches!(status.as_u16(), 404 | 405 | 500 | 501 | 502 | 503) {
            log::debug!(
                "[Proxy] count_tokens <<< {} {} (unsupported, using local estimate)",
                status.as_u16(),
                status.canonical_reason().unwrap_or("?")
            );
            let estimated = estimate_tokens(&body);
            let estimate_json = serde_json::json!({"input_tokens": estimated});
            return Ok((
                StatusCode::OK,
                [("content-type", "application/json")],
                serde_json::to_vec(&estimate_json).unwrap_or_default(),
            )
                .into_response());
        }
        log::warn!(
            "[Proxy] count_tokens <<< {} {}: {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("?"),
            truncate_chars(&body_str, 300)
        );
        continue;
    }

    return Ok((
        status_code,
        [("content-type", "application/json")],
        resp_body.to_vec(),
    )
        .into_response())
    } // end of for loop over candidates

    // All candidates failed — fall back to a local estimate so the client
    // never sees a 502 for count_tokens (which is purely advisory).
    log::info!("[Proxy] count_tokens: all routes failed, falling back to local estimate");
    let estimated = estimate_tokens(&body);
    let estimate_json = serde_json::json!({"input_tokens": estimated});
    Ok((
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::to_vec(&estimate_json).unwrap_or_default(),
    )
        .into_response())
}

/// Rough token estimate: ~4 chars per token for English text. Used as a fallback
/// when the upstream provider doesn't support /count_tokens. This is advisory only
/// and used by clients for context-window management.
fn estimate_tokens(body: &Value) -> u64 {
    let mut total_chars: usize = 0;
    // Count system prompt
    if let Some(s) = body.get("system") {
        total_chars += match s {
            Value::String(t) => t.chars().count(),
            other => other.to_string().chars().count(),
        };
    }
    // Count all message content
    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            if let Some(content) = msg.get("content") {
                total_chars += match content {
                    Value::String(t) => t.chars().count(),
                    Value::Array(blocks) => blocks.iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .map(|t| t.chars().count())
                        .sum(),
                    other => other.to_string().chars().count(),
                };
            }
        }
    }
    // ~4 chars per token, minimum 1
    ((total_chars as f64) / 4.0).ceil() as u64
}

// ---------------------------------------------------------------------------
// Streaming helpers
// ---------------------------------------------------------------------------

use bytes::Bytes;
use futures::stream::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Extract input/output tokens from an SSE stream buffer by scanning all data lines.
/// Works for both Anthropic-format SSE (usage in message_start/message_delta) and
/// OpenAI-format SSE (usage in the final chunk).
///
/// For Anthropic-format streams, some providers (Zhipu GLM, Baidu) report
/// `output_tokens` that excludes thinking tokens. We estimate output tokens
/// from the actual content (text_delta + thinking_delta) and use the higher value.
fn extract_usage_from_sse_buffer(buf: &[u8], format: &ProviderFormat) -> (Option<i64>, Option<i64>) {
    let text = String::from_utf8_lossy(buf);
    let mut input = None;
    let mut output = None;
    let mut estimated_output_chars: i64 = 0;
    let mut delta_events = 0u32;

    for line in text.lines() {
        let line = line.trim();
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if data == "[DONE]" {
                continue;
            }
            if let Ok(json) = serde_json::from_str::<Value>(data) {
                // Estimate output from content_block_delta events (covers thinking + text + tool use)
                if let Some(delta) = json.get("delta") {
                    delta_events += 1;
                    if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                        estimated_output_chars += t.len() as i64;
                    }
                    if let Some(t) = delta.get("thinking").and_then(|v| v.as_str()) {
                        estimated_output_chars += t.len() as i64;
                    }
                    if let Some(t) = delta.get("partial_json").and_then(|v| v.as_str()) {
                        estimated_output_chars += t.len() as i64;
                    }
                }
                if let Some(usage) = json.get("usage") {
                    match format {
                        ProviderFormat::Anthropic | ProviderFormat::OpenaiResponses => {
                            if let Some(total) = anthropic_total_input(usage) {
                                input = Some(total);
                            }
                            if let Some(v) = usage.get("output_tokens").and_then(|v| v.as_i64()) {
                                output = Some(v);
                            }
                        }
                        ProviderFormat::Openai | ProviderFormat::OpenaiImages => {
                            if let Some(v) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
                                input = Some(v);
                            }
                            if let Some(v) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
                                output = Some(v);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Rough token estimate: ~3 chars per token for mixed CJK/English content (conservative)
    let estimated_output_tokens = if estimated_output_chars > 0 {
        estimated_output_chars / 3
    } else {
        0
    };
    if estimated_output_tokens > 0 || delta_events > 0 {
        let reported = output.unwrap_or(0);
        // Use the higher of: provider report, char-based estimate, or delta-count estimate (~1.5 tokens/delta)
        let delta_estimate = (delta_events as i64 * 3) / 2;
        let best_estimate = estimated_output_tokens.max(delta_estimate);
        if best_estimate > reported {
            log::info!("[Usage] output estimate ({}, from {} chars + {} deltas) > provider report ({}), using estimate",
                best_estimate, estimated_output_chars, delta_events, reported);
            output = Some(best_estimate);
        }
    }

    (input, output)
}

/// Sum the total input tokens from an Anthropic-style usage object, including
/// prompt-cache hits. Providers report only the *uncached* portion in
/// `input_tokens` when caching is used; the cached portion is in
/// `cache_creation_input_tokens` / `cache_read_input_tokens`. Without this
/// sum, cached requests log tiny input token counts (e.g. 2).
fn anthropic_total_input(usage: &Value) -> Option<i64> {
    let base = usage.get("input_tokens").and_then(|v| v.as_i64());
    if base.is_none() {
        return None;
    }
    let cache_create = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Some(base.unwrap() + cache_create + cache_read)
}

/// Extract input/output tokens from an upstream response body.
fn extract_usage_tokens(value: &Value, format: &ProviderFormat) -> (Option<i64>, Option<i64>) {
    match format {
        ProviderFormat::Anthropic | ProviderFormat::OpenaiResponses => {
            let input = value
                .pointer("/usage")
                .and_then(|u| anthropic_total_input(u));
            let output = value
                .pointer("/usage/output_tokens")
                .and_then(|v| v.as_i64());
            (input, output)
        }
        ProviderFormat::Openai | ProviderFormat::OpenaiImages => {
            let input = value
                .pointer("/usage/prompt_tokens")
                .and_then(|v| v.as_i64());
            let output = value
                .pointer("/usage/completion_tokens")
                .and_then(|v| v.as_i64());
            (input, output)
        }
        _ => (None, None),
    }
}

/// Stream converter for Anthropic passthrough that:
/// 1. Replaces the model field with the client's requested model name
/// 2. Tracks open content blocks and ensures they are closed before message_stop
///    (some providers like Baidu/GLM skip content_block_stop when max_tokens is hit)
/// 3. Buffers `event:` lines so that injected content_block_stop events appear
///    in the correct position (before the event: + data: pair, not between them)
fn replace_model_in_anthropic_stream(
    upstream: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    request_model: String,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let buffer = Arc::new(Mutex::new(String::new()));
    // Track which content block indices are currently open
    let open_blocks: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));

    Box::pin(upstream.flat_map(move |chunk_result| {
        let buffer = buffer.clone();
        let open_blocks = open_blocks.clone();
        let request_model = request_model.clone();

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

            // Buffer for pending `event:` lines. We don't emit them immediately
            // because we may need to inject content_block_stop events before the
            // event: + data: pair. Without buffering, the sequence would be:
            //   event: message_delta      (already emitted)
            //   event: content_block_stop  (injected — wrong position!)
            //   data: {...}                 (orphaned from its event: line)
            let mut pending_event: Option<String> = None;

            // Process each line
            while let Some(newline_pos) = local_buf.find('\n') {
                let line = local_buf[..newline_pos].to_string();
                local_buf = local_buf[newline_pos + 1..].to_string();

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    yield Ok(Bytes::from("\n"));
                    continue;
                }

                // Buffer `event:` lines instead of emitting immediately
                if let Some(ev) = trimmed.strip_prefix("event: ") {
                    pending_event = Some(ev.trim().to_string());
                    continue;
                } else if let Some(ev) = trimmed.strip_prefix("event:") {
                    pending_event = Some(ev.trim().to_string());
                    continue;
                }

                // Try to parse as SSE data
                let data = if let Some(stripped) = trimmed.strip_prefix("data: ") {
                    stripped.to_string()
                } else if let Some(stripped) = trimmed.strip_prefix("data:") {
                    stripped.trim().to_string()
                } else {
                    // Not a data line, pass through
                    yield Ok(Bytes::from(format!("{}\n", line)));
                    continue;
                };

                if data == "[DONE]" {
                    yield Ok(Bytes::from("data: [DONE]\n\n"));
                    continue;
                }

                let mut parsed: Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(_) => {
                        // Not valid JSON, emit buffered event + raw data
                        if let Some(ev) = pending_event.take() {
                            yield Ok(Bytes::from(format!("event: {}\n", ev)));
                        }
                        yield Ok(Bytes::from(format!("data: {}\n\n", data)));
                        continue;
                    }
                };

                // Extract event_type as owned String to avoid borrow conflict with parsed
                let event_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();

                // Track content_block_start
                if event_type == "content_block_start" {
                    if let Some(idx) = parsed.get("index").and_then(|v| v.as_u64()) {
                        let mut blocks = open_blocks.lock().await;
                        blocks.push(idx as u32);
                    }
                }

                // Track content_block_stop
                if event_type == "content_block_stop" {
                    if let Some(idx) = parsed.get("index").and_then(|v| v.as_u64()) {
                        let mut blocks = open_blocks.lock().await;
                        blocks.retain(|&b| b != idx as u32);
                    }
                }

                // Replace model in message_start events and message.message
                if event_type == "message_start" {
                    if let Some(msg) = parsed.get_mut("message").and_then(|m| m.as_object_mut()) {
                        msg.insert("model".to_string(), Value::String(request_model.clone()));
                    }
                }
                // Also handle direct message objects in the stream
                if parsed.get("message").is_some() {
                    if let Some(msg) = parsed.get_mut("message").and_then(|m| m.as_object_mut()) {
                        if msg.get("model").is_some() {
                            msg.insert("model".to_string(), Value::String(request_model.clone()));
                        }
                    }
                }

                // Some providers (glm-5.2) omit usage.output_tokens from
                // message_delta, which causes clients to crash on NoneType
                // arithmetic (int - None). Default to 0 if missing.
                if event_type == "message_delta" {
                    if let Some(obj) = parsed.as_object_mut() {
                        let usage = obj
                            .entry("usage".to_string())
                            .or_insert_with(|| json!({}));
                        if let Some(u) = usage.as_object_mut() {
                            u.entry("output_tokens".to_string())
                                .or_insert_with(|| Value::Number(0.into()));
                        }
                    }
                }

                // Before message_delta or message_stop, close any open content blocks.
                // IMPORTANT: We emit the injected content_block_stop events BEFORE
                // the buffered event: line, so the SSE sequence is correct:
                //   (injected) event: content_block_stop + data: {...}
                //   (original)  event: message_delta     + data: {...}
                if event_type == "message_delta" || event_type == "message_stop" {
                    let mut blocks = open_blocks.lock().await;
                    if !blocks.is_empty() {
                        log::warn!("[Stream] closing {} unclosed content block(s) before {}", blocks.len(), event_type);
                        for &idx in blocks.iter() {
                            let stop_event = json!({
                                "type": "content_block_stop",
                                "index": idx
                            });
                            yield Ok(Bytes::from(format!("event: content_block_stop\ndata: {}\n\n", stop_event)));
                        }
                        blocks.clear();
                    }
                }

                // Now emit the buffered event: line (if any) + the data: line
                if let Some(ev) = pending_event.take() {
                    yield Ok(Bytes::from(format!("event: {}\n", ev)));
                }
                let serialized = serde_json::to_string(&parsed).unwrap_or_else(|_| data.clone());
                yield Ok(Bytes::from(format!("data: {}\n\n", serialized)));
            }

            // Emit any trailing buffered event line
            if let Some(ev) = pending_event.take() {
                yield Ok(Bytes::from(format!("event: {}\n", ev)));
            }

            // Save remaining buffer
            *buffer.lock().await = local_buf;
        }
    }))
}

// ---------------------------------------------------------------------------
// Body transformation helpers (for Anthropic passthrough)
// ---------------------------------------------------------------------------
fn normalize_roles(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(messages) = map.get_mut("messages").and_then(|m| m.as_array_mut()) {
                for msg in messages.iter_mut() {
                    if let Some(role) = msg.get("role").and_then(|r| r.as_str()) {
                        let normalized = match role {
                            "assistant" | "user" => role.to_string(),
                            "tool" => "assistant".to_string(),
                            _ => "user".to_string(),
                        };
                        if normalized != role {
                            msg["role"] = Value::String(normalized);
                        }
                    }

                    // Ensure content is not null: Anthropic requires either a string
                    // or an array of content blocks. Normalize `null` -> empty string,
                    // and convert non-string/non-array content into a text block.
                    match msg.get("content") {
                        Some(c) if c.is_null() => {
                            msg["content"] = Value::String(String::new());
                        }
                        Some(c) if !(c.is_string() || c.is_array()) => {
                            let s = if c.is_object() || c.is_number() || c.is_boolean() {
                                serde_json::to_string(c).unwrap_or_else(|_| c.to_string())
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

                    normalize_roles(msg);
                }
                return;
            }
            for v in map.values_mut() {
                normalize_roles(v);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                normalize_roles(item);
            }
        }
        _ => {}
    }
}

/// Inject placeholder `thinking` blocks into assistant messages when thinking is enabled.
/// KIMI's API requires every assistant message to have a thinking block in the content array
/// when thinking is enabled — even tool-call-only messages that came from earlier turns.
/// Remove `reasoning_content` from an OpenAI Chat completion response's message(s).
/// Used for fast/non-reasoning tiers where the client must not receive reasoning
/// chains (e.g. skill classifiers that read the first field they find).
fn strip_reasoning_from_openai_response(value: &mut Value) {
    if let Some(choices) = value.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for choice in choices.iter_mut() {
            if let Some(msg) = choice.get_mut("message").and_then(|m| m.as_object_mut()) {
                msg.remove("reasoning_content");
            }
        }
    }
}

/// Remove `reasoning_content` from an OpenAI Chat SSE delta/message object in place.
/// Returns true if anything was removed (so callers know to re-serialize).
fn strip_reasoning_from_openai_delta(value: &mut Value) -> bool {
    let mut removed = false;
    if let Some(choices) = value.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for choice in choices.iter_mut() {
            for key in &["delta", "message"] {
                if let Some(obj) = choice.get_mut(*key).and_then(|v| v.as_object_mut()) {
                    if obj.remove("reasoning_content").is_some() {
                        removed = true;
                    }
                }
            }
        }
    }
    removed
}

/// Stream wrapper that strips `reasoning_content` from OpenAI Chat SSE data lines.
/// Used for fast/non-reasoning tiers so reasoning chains never leak to streaming
/// clients.
fn strip_reasoning_from_openai_stream(
    upstream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let buffer = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
    Box::pin(async_stream::stream! {
        let mut upstream = upstream;
        while let Some(chunk_result) = upstream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => { yield Err(e); return; }
            };
            let text = String::from_utf8_lossy(&chunk);
            let mut local_buf;
            {
                let mut guard = buffer.lock().await;
                guard.push_str(&text);
                local_buf = std::mem::take(&mut *guard);
            }
            while let Some(newline_pos) = local_buf.find('\n') {
                let line = local_buf[..newline_pos].to_string();
                local_buf = local_buf[newline_pos + 1..].to_string();
                let trimmed = line.trim();
                let data = if let Some(s) = trimmed.strip_prefix("data: ") {
                    s.to_string()
                } else if let Some(s) = trimmed.strip_prefix("data:") {
                    s.trim().to_string()
                } else {
                    yield Ok(Bytes::from(format!("{}\n", line)));
                    continue;
                };
                if data == "[DONE]" {
                    yield Ok(Bytes::from(format!("{}\n", line)));
                    continue;
                }
                if let Ok(mut value) = serde_json::from_str::<Value>(&data) {
                    if strip_reasoning_from_openai_delta(&mut value) {
                        yield Ok(Bytes::from(format!("data: {}\n", value)));
                    } else {
                        yield Ok(Bytes::from(format!("{}\n", line)));
                    }
                } else {
                    yield Ok(Bytes::from(format!("{}\n", line)));
                }
            }
            if !local_buf.is_empty() {
                let mut guard = buffer.lock().await;
                guard.push_str(&local_buf);
            }
        }
    })
}

fn inject_reasoning_content(value: &mut Value) {
    // Claude Code sends thinking.type as "enabled" or "adaptive"
    // Both require thinking blocks in all assistant messages for KIMI/文心
    let thinking_type = value
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let thinking_active = thinking_type == "enabled" || thinking_type == "adaptive";

    if !thinking_active {
        return;
    }

    if let Some(messages) = value.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                continue;
            }

            // Ensure content is an array — convert string/null to array first
            let content_is_array = msg.get("content").map(|c| c.is_array()).unwrap_or(false);
            if !content_is_array {
                // Convert content to array: string → [{"type":"text","text":"..."}], null/missing → []
                let text_content = msg
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                msg["content"] = if text_content.is_empty() {
                    json!([])
                } else {
                    json!([{"type": "text", "text": text_content}])
                };
            }

            // Check if content already has a thinking block
            let has_thinking = msg
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| arr.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("thinking")))
                .unwrap_or(false);

            if has_thinking {
                continue;
            }

            // Inject a placeholder thinking block at the start of the content array
            if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                content.insert(0, json!({
                    "type": "thinking",
                    "thinking": " "
                }));
            }
        }
    }
}

/// Inject placeholder `reasoning_content` into OpenAI Chat assistant messages
/// when thinking mode is active. Providers like DeepSeek require
/// `reasoning_content` on all assistant messages when thinking mode is enabled
/// — if the client doesn't echo it back (because its runtime treats it as a
/// transparent extension), the provider returns 400.
///
/// Detection heuristic:
/// - Any assistant message already has `reasoning_content` → thinking was
///   active in a prior turn, so it's still active now.
/// - The request has `enable_thinking: true` (DeepSeek's thinking switch).
fn inject_openai_reasoning_content(value: &mut Value) {
    // Detect thinking mode in two steps to satisfy the borrow checker:
    // 1. Check enable_thinking flag (immutable borrow)
    // 2. Check messages for existing reasoning_content (immutable borrow)
    // 3. Mutate messages (mutable borrow)
    let enable_thinking = value.get("enable_thinking").and_then(|v| v.as_bool()) == Some(true);
    let has_reasoning_in_messages = value
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter().any(|msg| {
                msg.get("role").and_then(|r| r.as_str()) == Some("assistant")
                    && msg.get("reasoning_content").is_some()
            })
        })
        .unwrap_or(false);

    if !enable_thinking && !has_reasoning_in_messages {
        return;
    }

    let Some(messages) = value.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        if msg.get("reasoning_content").is_some() {
            continue;
        }
        // Inject placeholder reasoning_content so the provider doesn't reject
        // the request for missing it on a prior-turn assistant message.
        msg["reasoning_content"] = Value::String(" ".to_string());
    }
}

/// Strip Anthropic-specific fields that domestic providers do not understand.
/// These fields are meaningful only to the real Anthropic API; leaving them in
/// the forwarded body can cause providers to return errors or unexpectedly
/// large responses, which makes Claude Code retry with an ever-growing context.
///
/// Note: `thinking` is NOT stripped here — Anthropic-compatible providers like
/// Zhipu GLM and Kimi support it and require it for thinking mode. It is
/// removed in the conversion functions for non-Anthropic formats (OpenAI Chat,
/// Responses) where providers don't understand it.
fn strip_anthropic_specific_fields(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("context_management");
        obj.remove("metadata");
        obj.remove("tool_choice");
        // beta/extended fields are provider-specific
        obj.remove("anthropic_beta");
        obj.remove("anthropic_version");
    }
}

/// Strip "thinking" blocks from assistant content arrays
fn strip_thinking(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            if let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
                for msg in messages.iter_mut() {
                    if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                        if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                            content.retain(|block| {
                                block.get("type").and_then(|t| t.as_str()) != Some("thinking")
                            });
                        }
                    }
                }
            }
            for v in obj.values_mut() {
                strip_thinking(v);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                strip_thinking(item);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Image generation endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateImageRequest {
    #[serde(default)]
    pub tag: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default = "default_image_count")]
    pub n: u32,
    #[serde(default)]
    pub extra: std::collections::HashMap<String, Value>,
}

fn default_image_count() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize)]
pub struct GeneratedImage {
    pub url: Option<String>,
    #[serde(default)]
    pub base64: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateImageResponse {
    pub success: bool,
    pub tag: String,
    pub provider: String,
    pub model: String,
    pub format: String,
    pub images: Vec<GeneratedImage>,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn generate_image(state: &AppState, req: GenerateImageRequest) -> GenerateImageResponse {
    let prompt = req.prompt.trim().to_string();
    if prompt.is_empty() {
        return image_error(String::new(), "image prompt cannot be empty".to_string());
    }

    let (tag, candidates, providers) = {
        let config = state.config.read().await;
        let tag = req.tag.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| config.current_tag.clone());
        let candidates: Vec<Route> = find_candidate_routes(&config.routes, &tag, &config.tags)
            .into_iter()
            .filter(|(_, r)| is_image_format(&r.format))
            .map(|(_, r)| r.clone())
            .collect();
        (tag, candidates, config.providers.clone())
    };

    if candidates.is_empty() {
        return image_error(tag.clone(), format!("no enabled image_generation route found for tag '{}'", tag));
    }

    let mut last_error: Option<String> = None;
    for (attempt, route) in candidates.iter().enumerate() {
        if attempt > 0 {
            log::warn!("[Image] {} failover: trying route #{} (provider={}, model={})", tag, attempt + 1, route.provider, route.model);
        }

        let provider = match providers.get(&route.provider) {
            Some(p) => p,
            None => {
                last_error = Some(format!("provider '{}' not found", route.provider));
                continue;
            }
        };
        if provider.api_key.is_empty() || provider.api_key.starts_with("sk-your-") || provider.api_key == "your-key-here" {
            last_error = Some(format!("provider '{}' has no valid API key configured", route.provider));
            continue;
        }

        let url = format!("{}{}", route.base_url.trim_end_matches('/'), route.format.path());
        let start = std::time::Instant::now();
        let result = match route.format {
            ProviderFormat::OpenaiImages => generate_openai_images(state, provider, route, &url, &prompt, &req).await,
            ProviderFormat::DashscopeImage => generate_dashscope_image(state, provider, route, &url, &prompt, &req).await,
            ProviderFormat::DashscopeChatImage => generate_dashscope_chat_image(state, provider, route, &url, &prompt, &req).await,
            ProviderFormat::MinimaxImage => generate_minimax_image(state, provider, route, &url, &prompt, &req).await,
            _ => Err(ProxyError::Upstream(format!("format {:?} is not an image generation format", route.format))),
        };
        let latency_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(images) if !images.is_empty() => {
                let _ = crate::db::insert_usage_log(
                    &state.db,
                    crate::db::UsageInsert {
                        caller_key_id: None, // image generation does not yet authenticate caller keys
                        tag: tag.clone(),
                        provider: provider.name.clone(),
                        model: route.model.clone(),
                        request_model: route.model.clone(),
                        modality: "image_generation".into(),
                        input_tokens: None,
                        output_tokens: None,
                        latency_ms: latency_ms as i64,
                        status: "success".into(),
                        error_message: None,
                    },
                ).await;
                return GenerateImageResponse {
                    success: true,
                    tag,
                    provider: provider.name.clone(),
                    model: route.model.clone(),
                    format: format_string(&route.format),
                    images,
                    latency_ms,
                    error: None,
                };
            }
            Ok(_) => {
                last_error = Some("image provider returned no images".to_string());
                continue;
            }
            Err(err) => {
                let err_text = err.to_string();
                log::warn!("[Image] route failed: {}", err_text);
                if is_retryable(&err) {
                    last_error = Some(err_text);
                    continue;
                }
                return GenerateImageResponse {
                    success: false,
                    tag,
                    provider: provider.name.clone(),
                    model: route.model.clone(),
                    format: format_string(&route.format),
                    images: Vec::new(),
                    latency_ms,
                    error: Some(err_text),
                };
            }
        }
    }

    image_error(tag, last_error.unwrap_or_else(|| "all image generation routes failed".to_string()))
}

fn image_error(tag: String, error: String) -> GenerateImageResponse {
    GenerateImageResponse {
        success: false,
        tag,
        provider: String::new(),
        model: String::new(),
        format: String::new(),
        images: Vec::new(),
        latency_ms: 0,
        error: Some(error),
    }
}

fn format_string(format: &ProviderFormat) -> String {
    match format {
        ProviderFormat::Anthropic => "anthropic",
        ProviderFormat::Openai => "openai",
        ProviderFormat::OpenaiResponses => "openai_responses",
        ProviderFormat::OpenaiImages => "openai_images",
        ProviderFormat::DashscopeImage => "dashscope_image",
        ProviderFormat::DashscopeChatImage => "dashscope_chat_image",
        ProviderFormat::DashscopeVideo => "dashscope_video",
        ProviderFormat::DashscopeTts => "dashscope_tts",
        ProviderFormat::DashscopeAsr => "dashscope_asr",
        ProviderFormat::Kling => "kling",
        ProviderFormat::MinimaxImage => "minimax_image",
    }.to_string()
}

// ─── Multimodal handlers (dispatched from handle_proxy) ────────────────────
//
// OpenCarrier sends image/tts/audio as OpenAI chat requests. These handlers
// extract the payload, call the provider, and return the OpenCarrier-expected
// response shape (see AGINXBRAIN_MULTIMODAL_SPEC.md).

/// Image generation: reuse existing per-format drivers, wrap result in
/// OpenCarrier format A. url-or-data-url per GeneratedImage.
async fn handle_image_request(
    state: AppState,
    caller_key_id: Option<i64>,
    route: &Route,
    provider: &Provider,
    body: &Value,
    start: std::time::Instant,
) -> Result<Response, ProxyError> {
    let prompt = last_user_text(body)
        .ok_or_else(|| ProxyError::Upstream("image: no text prompt found in messages".into()))?;
    let size = body.get("size").and_then(|v| v.as_str()).map(|s| s.to_string());
    let n = body.get("n").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

    let url = format!("{}{}", route.base_url.trim_end_matches('/'), route.format.path());
    log::info!(
        "[Proxy] image → {} (model={}, format={:?})",
        provider.name, route.model, route.format
    );
    log::info!(
        "[Proxy] forwarding image request to {}",
        url
    );

    let req = GenerateImageRequest {
        tag: None,
        prompt: prompt.clone(),
        size,
        n,
        extra: std::collections::HashMap::new(),
    };

    let images: Vec<GeneratedImage> = match &route.format {
        ProviderFormat::OpenaiImages => generate_openai_images(&state, provider, route, &url, &prompt, &req).await?,
        ProviderFormat::DashscopeImage => generate_dashscope_image(&state, provider, route, &url, &prompt, &req).await?,
        ProviderFormat::DashscopeChatImage => generate_dashscope_chat_image(&state, provider, route, &url, &prompt, &req).await?,
        ProviderFormat::MinimaxImage => generate_minimax_image(&state, provider, route, &url, &prompt, &req).await?,
        other => return Err(ProxyError::Upstream(format!("format {:?} is not an image format", other))),
    };

    if images.is_empty() {
        return Err(ProxyError::Upstream("image provider returned no images".into()));
    }

    log::info!(
        "[Proxy] image response: {} image(s), latency={}ms",
        images.len(),
        start.elapsed().as_millis()
    );

    // Build OpenCarrier format A: output.choices[].message.content[].image
    let content: Vec<Value> = images.iter().map(|img| {
        let src = if let Some(url) = &img.url {
            url.clone()
        } else if !img.base64.is_empty() {
            format!("data:image/png;base64,{}", img.base64)
        } else {
            String::new()
        };
        json!({ "image": src })
    }).collect();

    let _ = crate::db::insert_usage_log(
        &state.db,
        crate::db::UsageInsert {
            caller_key_id,
            tag: route.tags.first().cloned().unwrap_or_default(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            request_model: route.model.clone(),
            modality: "image_generation".into(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: start.elapsed().as_millis() as i64,
            status: "success".into(),
            error_message: None,
        },
    ).await;

    let resp = json!({
        "output": { "choices": [{ "message": { "content": content } }] },
        "code": "Success"
    });
    Ok(Json(resp).into_response())
}

/// TTS: call DashScope text-to-speech, save audio bytes to disk, return a
/// relative URL that OpenCarrier downloads via the public /audio/ route.
async fn handle_tts_request(
    state: AppState,
    caller_key_id: Option<i64>,
    route: &Route,
    provider: &Provider,
    body: &Value,
    start: std::time::Instant,
) -> Result<Response, ProxyError> {
    let text = last_user_text(body)
        .ok_or_else(|| ProxyError::Upstream("tts: no text found in messages".into()))?;
    let voice = body.get("voice").and_then(|v| v.as_str()).unwrap_or("longxiaochun_v2").to_string();
    let format = body.get("audio_format").and_then(|v| v.as_str()).unwrap_or("mp3").to_string();
    let sample_rate = body.get("sample_rate").and_then(|v| v.as_u64()).unwrap_or(22050) as u32;

    let ws_url = route.ws_url.as_deref().unwrap_or("");

    let audio_bytes = crate::dashscope_ws::tts_via_websocket(
        ws_url,
        &provider.api_key,
        &crate::dashscope_ws::TtsParams {
            text,
            model: route.model.clone(),
            voice,
            format,
            sample_rate,
        },
    )
    .await
    .map_err(|e| ProxyError::Upstream(format!("tts websocket failed: {}", e)))?;

    if audio_bytes.is_empty() {
        return Err(ProxyError::Upstream("tts: no audio data received".into()));
    }

    // Save to ~/.aginxbrain/audio/{id}.mp3
    let audio_dir = match dirs::home_dir() {
        Some(h) => h.join(".aginxbrain").join("audio"),
        None => return Err(ProxyError::Upstream("no home directory for audio storage".into())),
    };
    let _ = tokio::fs::create_dir_all(&audio_dir).await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let rand_suffix: u64 = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        caller_key_id.hash(&mut h);
        now_ms.hash(&mut h);
        h.finish()
    };
    let filename = format!("{}_{}.mp3", now_ms, rand_suffix);
    let file_path = audio_dir.join(&filename);
    tokio::fs::write(&file_path, &audio_bytes).await
        .map_err(|e| ProxyError::Upstream(format!("tts: failed to save audio: {}", e)))?;

    let _ = crate::db::insert_usage_log(
        &state.db,
        crate::db::UsageInsert {
            caller_key_id,
            tag: route.tags.first().cloned().unwrap_or_default(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            request_model: route.model.clone(),
            modality: "tts".into(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: start.elapsed().as_millis() as i64,
            status: "success".into(),
            error_message: None,
        },
    ).await;

    let resp_json = json!({
        "output": { "audio": format!("/audio/{}", filename) },
        "code": "Success"
    });
    Ok(Json(resp_json).into_response())
}

/// ASR (audio → text): decode base64 audio from the chat body, send it via
/// DashScope WebSocket (Paraformer/Fun-ASR), wrap the transcription in an
/// response.
async fn handle_asr_request(
    state: AppState,
    caller_key_id: Option<i64>,
    route: &Route,
    provider: &Provider,
    body: &Value,
    start: std::time::Instant,
) -> Result<Response, ProxyError> {
    let (b64, format) = find_input_audio(body)
        .ok_or_else(|| ProxyError::Upstream("audio: no input_audio block found in messages".into()))?;
    let audio_bytes = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes())
        .map_err(|e| ProxyError::Upstream(format!("audio: invalid base64: {}", e)))?;

    let ws_url = route.ws_url.as_deref().unwrap_or("");
    let sample_rate = body.get("sample_rate").and_then(|v| v.as_u64()).unwrap_or(22050) as u32;

    let transcription = crate::dashscope_ws::asr_via_websocket(
        ws_url,
        &provider.api_key,
        &crate::dashscope_ws::AsrParams {
            audio_bytes,
            model: route.model.clone(),
            format,
            sample_rate,
        },
    )
    .await
    .map_err(|e| ProxyError::Upstream(format!("asr websocket failed: {}", e)))?;

    let _ = crate::db::insert_usage_log(
        &state.db,
        crate::db::UsageInsert {
            caller_key_id,
            tag: route.tags.first().cloned().unwrap_or_default(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            request_model: route.model.clone(),
            modality: "audio".into(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: start.elapsed().as_millis() as i64,
            status: "success".into(),
            error_message: None,
        },
    ).await;

    // Wrap as a standard OpenAI chat response.
    let chat_resp = json!({
        "choices": [{
            "message": { "role": "assistant", "content": transcription },
            "finish_reason": "stop"
        }]
    });
    Ok(Json(chat_resp).into_response())
}

async fn send_image_post(
    state: &AppState,
    provider: &Provider,
    url: &str,
    body: &Value,
    extra_headers: &[(&str, &str)],
) -> Result<Value, ProxyError> {
    let mut builder = state.http_client.post(url)
        .header("content-type", "application/json")
        .header(provider.auth_type.header_name(), provider.auth_type.header_value(&provider.api_key));
    for (k, v) in extra_headers {
        builder = builder.header(*k, *v);
    }
    let resp = builder
        .json(body)
        .timeout(std::time::Duration::from_secs(45))
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(e.to_string()))?;
    parse_image_response(resp).await
}

async fn send_image_get(state: &AppState, provider: &Provider, url: &str) -> Result<Value, ProxyError> {
    let resp = state.http_client.get(url)
        .header(provider.auth_type.header_name(), provider.auth_type.header_value(&provider.api_key))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(e.to_string()))?;
    parse_image_response(resp).await
}

async fn parse_image_response(resp: reqwest::Response) -> Result<Value, ProxyError> {
    let status = resp.status();
    let status_code = status.as_u16();
    let text = resp.text().await.map_err(|e| ProxyError::Upstream(format!("failed to read image response body: {}", e)))?;
    if !status.is_success() {
        return Err(ProxyError::Upstream(format!("HTTP {}: {}", status_code, truncate_chars(&text, 500))));
    }
    serde_json::from_str(&text).map_err(|e| ProxyError::Upstream(format!("failed to parse image response: {}", e)))
}

fn merge_extra(body: &mut Value, extra: &std::collections::HashMap<String, Value>, protected: &[&str]) {
    if let Some(obj) = body.as_object_mut() {
        for (k, v) in extra {
            if !protected.contains(&k.as_str()) && k != "parameters" && k != "input" {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
}

async fn generate_openai_images(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    url: &str,
    prompt: &str,
    req: &GenerateImageRequest,
) -> Result<Vec<GeneratedImage>, ProxyError> {
    let mut body = json!({
        "model": route.model,
        "prompt": prompt,
        "n": req.n.max(1),
        "response_format": "url",
    });
    if let Some(size) = &req.size {
        body["size"] = Value::String(size.clone());
    }
    merge_extra(&mut body, &req.extra, &["model", "prompt"]);
    let result = send_image_post(state, provider, url, &body, &[]).await?;
    parse_openai_images(&result).ok_or_else(|| ProxyError::Upstream("No images in OpenAI Images response".to_string()))
}

async fn generate_dashscope_image(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    url: &str,
    prompt: &str,
    req: &GenerateImageRequest,
) -> Result<Vec<GeneratedImage>, ProxyError> {
    let size = req.size.clone().unwrap_or_else(|| "1024*1024".to_string()).replace('x', "*");
    let mut parameters = json!({ "size": size, "n": req.n.max(1), "prompt_extend": true, "watermark": false });
    if let Some(extra_params) = req.extra.get("parameters").and_then(|v| v.as_object()) {
        if let Some(obj) = parameters.as_object_mut() {
            for (k, v) in extra_params {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    for (k, v) in &req.extra {
        if k != "parameters" && k != "input" {
            if let Some(obj) = parameters.as_object_mut() {
                obj.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }

    // DashScope multimodal generation API now uses input.messages format:
    // {"model": "...", "input": {"messages": [{"role": "user", "content": [{"text": prompt}]}]}}
    let mut input = json!({ "messages": [{"role": "user", "content": [{"text": prompt}]}] });
    if let Some(extra_input) = req.extra.get("input").and_then(|v| v.as_object()) {
        if let Some(obj) = input.as_object_mut() {
            for (k, v) in extra_input {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let body = json!({ "model": route.model, "input": input, "parameters": parameters });
    let result = send_image_post(state, provider, url, &body, &[]).await?;
    if let Some(images) = parse_dashscope_images(&result) {
        if !images.is_empty() {
            return Ok(images);
        }
    }

    if let Some(task_id) = result.pointer("/output/task_id").and_then(|v| v.as_str()) {
        return poll_dashscope_image_task(state, provider, task_id, url).await;
    }

    Err(ProxyError::Upstream("No images or task_id in DashScope image response".to_string()))
}

async fn poll_dashscope_image_task(
    state: &AppState,
    provider: &Provider,
    task_id: &str,
    base_url: &str,
) -> Result<Vec<GeneratedImage>, ProxyError> {
    let poll_url = format!("{}/api/v1/tasks/{}", base_url.trim_end_matches('/'), task_id);
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > std::time::Duration::from_secs(120) {
            return Err(ProxyError::Upstream("DashScope image task polling timed out".to_string()));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let result = send_image_get(state, provider, &poll_url).await?;
        let status = result.pointer("/output/task_status").and_then(|v| v.as_str()).unwrap_or("");
        match status {
            "SUCCEEDED" | "Success" => {
                return parse_dashscope_images(&result)
                    .filter(|images| !images.is_empty())
                    .ok_or_else(|| ProxyError::Upstream("DashScope image task completed without images".to_string()));
            }
            "FAILED" | "Failed" => {
                let msg = result.pointer("/output/message").and_then(|v| v.as_str()).unwrap_or("Unknown DashScope task error");
                return Err(ProxyError::Upstream(msg.to_string()));
            }
            _ => continue,
        }
    }
}

/// Generate image via DashScope's OpenAI-compatible chat completions endpoint
/// (e.g. token-plan.cn-beijing.maas.aliyuncs.com). These providers serve
/// image models like wan2.7-image-pro via POST /v1/chat/completions, where
/// the response contains `output.choices[].message.content[].image` URLs
/// instead of the standard OpenAI images format.
async fn generate_dashscope_chat_image(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    url: &str,
    prompt: &str,
    req: &GenerateImageRequest,
) -> Result<Vec<GeneratedImage>, ProxyError> {
    let mut body = json!({
        "model": route.model,
        "messages": [{"role": "user", "content": [{"type": "text", "text": prompt}]}],
    });
    if let Some(size) = &req.size {
        body["size"] = Value::String(size.clone());
    }
    if req.n > 1 {
        body["n"] = json!(req.n);
    }
    let result = send_image_post(state, provider, url, &body, &[]).await?;

    // Parse response: output.choices[].message.content[].image
    let mut images = Vec::new();
    if let Some(choices) = result.pointer("/output/choices").and_then(|c| c.as_array()) {
        for choice in choices {
            if let Some(content) = choice.pointer("/message/content").and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("image") {
                        if let Some(url) = block.get("image").and_then(|u| u.as_str()) {
                            images.push(GeneratedImage { url: Some(url.to_string()), base64: String::new() });
                        }
                    }
                }
            }
        }
    }

    // Fallback: try standard OpenAI choices[] format (some endpoints may wrap differently)
    if images.is_empty() {
        if let Some(choices) = result.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(content) = choice.pointer("/message/content").and_then(|c| c.as_array()) {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("image") {
                            if let Some(url) = block.get("image").and_then(|u| u.as_str()) {
                                images.push(GeneratedImage { url: Some(url.to_string()), base64: String::new() });
                            }
                        }
                    }
                }
            }
        }
    }

    if images.is_empty() {
        return Err(ProxyError::Upstream("No images in DashScope chat-image response".to_string()));
    }
    Ok(images)
}

async fn generate_minimax_image(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    url: &str,
    prompt: &str,
    req: &GenerateImageRequest,
) -> Result<Vec<GeneratedImage>, ProxyError> {
    let mut body = json!({ "model": route.model, "prompt": prompt, "n": req.n.max(1), "response_format": "url" });
    merge_extra(&mut body, &req.extra, &["model", "prompt"]);
    let result = send_image_post(state, provider, url, &body, &[]).await?;
    parse_minimax_images(&result).ok_or_else(|| ProxyError::Upstream("No images in MiniMax response".to_string()))
}

fn parse_openai_images(result: &Value) -> Option<Vec<GeneratedImage>> {
    let mut images = Vec::new();
    for item in result.get("data")?.as_array()? {
        let url = item.get("url").and_then(|v| v.as_str()).map(String::from);
        let base64 = item.get("b64_json").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if url.is_some() || !base64.is_empty() {
            images.push(GeneratedImage { url, base64 });
        }
    }
    Some(images)
}

fn parse_dashscope_images(result: &Value) -> Option<Vec<GeneratedImage>> {
    let mut images = Vec::new();
    if let Some(choices) = result.pointer("/output/choices").and_then(|v| v.as_array()) {
        for choice in choices {
            if let Some(content) = choice.pointer("/message/content").and_then(|v| v.as_array()) {
                for block in content {
                    if let Some(url) = block.get("image").and_then(|v| v.as_str()) {
                        images.push(GeneratedImage { url: Some(url.to_string()), base64: String::new() });
                    }
                }
            }
        }
    }
    if let Some(results) = result.pointer("/output/results").and_then(|v| v.as_array()) {
        for item in results {
            let url = item.get("url").and_then(|v| v.as_str()).map(String::from);
            let base64 = item.get("b64_image").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if url.is_some() || !base64.is_empty() {
                images.push(GeneratedImage { url, base64 });
            }
        }
    }
    if let Some(data) = result.pointer("/output/data").and_then(|v| v.as_array()) {
        for item in data {
            let url = item.get("url").and_then(|v| v.as_str()).map(String::from);
            let base64 = item.get("b64_json").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if url.is_some() || !base64.is_empty() {
                images.push(GeneratedImage { url, base64 });
            }
        }
    }
    Some(images)
}

fn parse_minimax_images(result: &Value) -> Option<Vec<GeneratedImage>> {
    let mut images = Vec::new();
    if let Some(data) = result.get("data") {
        if let Some(urls) = data.get("image_urls").and_then(|v| v.as_array()) {
            for url_val in urls {
                if let Some(url) = url_val.as_str() {
                    images.push(GeneratedImage { url: Some(url.to_string()), base64: String::new() });
                }
            }
        }
        if let Some(b64s) = data.get("image_base64").and_then(|v| v.as_array()) {
            for b64_val in b64s {
                if let Some(base64) = b64_val.as_str() {
                    images.push(GeneratedImage { url: None, base64: base64.to_string() });
                }
            }
        }
    }
    if images.is_empty() {
        if let Some(openai_style) = parse_openai_images(result) {
            images.extend(openai_style);
        }
    }
    Some(images)
}

// ---------------------------------------------------------------------------
// Test endpoint — send a test prompt through the full pipeline
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub success: bool,
    pub tag: String,
    pub provider: String,
    pub model: String,
    pub format: String,
    pub latency_ms: u64,
    pub error: Option<String>,
    pub response: Option<Value>,
}

/// Send a test request to a single specific route (no failover).
/// Returns the TestResult directly — either success or the specific error.
async fn send_test_to_route(
    state: &AppState,
    config: &AppConfig,
    route: &Route,
    tag: &str,
    prompt: &str,
) -> TestResult {
    if !is_chat_format(&route.format) {
        return TestResult {
            success: false,
            tag: tag.to_string(),
            provider: String::new(),
            model: route.model.clone(),
            format: format!("{:?}", route.format),
            latency_ms: 0,
            error: Some(format!(
                "route format {:?} is not yet supported by the chat test endpoint",
                route.format
            )),
            response: None,
        };
    }

    let provider = match config.providers.get(&route.provider) {
        Some(p) => p,
        None => {
            return TestResult {
                success: false,
                tag: tag.to_string(),
                provider: String::new(),
                model: route.model.clone(),
                format: format!("{:?}", route.format),
                latency_ms: 0,
                error: Some(format!("provider '{}' not found", route.provider)),
                response: None,
            };
        }
    };

    // Validate API key
    if provider.api_key.is_empty() || provider.api_key.starts_with("sk-your-") {
        return TestResult {
            success: false,
            tag: tag.to_string(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            format: format!("{:?}", route.format),
            latency_ms: 0,
            error: Some(format!(
                "provider '{}' has no valid API key",
                route.provider,
            )),
            response: None,
        };
    }

    // Construct a minimal Anthropic Messages request
    let anthropic_body = json!({
        "model": tag,
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": prompt}
        ]
    });

    // Convert based on provider format
    let fwd_body = match route.format {
        ProviderFormat::Anthropic => {
            let mut b = anthropic_body.clone();
            b["model"] = Value::String(route.model.clone());
            normalize_roles(&mut b);
            b
        }
        ProviderFormat::Openai => {
            let mut b = anthropic_body.clone();
            normalize_roles(&mut b);
            convert::anthropic_to_openai_request(&b, &route.model)
        }
        ProviderFormat::OpenaiResponses => {
            let mut b = anthropic_body.clone();
            normalize_roles(&mut b);
            strip_thinking(&mut b);
            convert::anthropic_to_responses_request(&b, &route.model)
        }
        _ => {
            return TestResult {
                success: false,
                tag: tag.to_string(),
                provider: String::new(),
                model: route.model.clone(),
                format: format!("{:?}", route.format),
                latency_ms: 0,
                error: Some(format!(
                    "route format {:?} is not yet supported by the chat test endpoint",
                    route.format
                )),
                response: None,
            };
        }
    };

    // Build URL
    let url = format!(
        "{}{}",
        route.base_url.trim_end_matches('/'),
        route.format.path()
    );
    log::info!("[Test] testing route tag={} → {} {} (format={:?})", tag, url, route.model, route.format);

    if let Ok(s) = serde_json::to_string(&fwd_body) {
        let truncated = if s.chars().count() > 500 { format!("{}...(truncated)", truncate_chars(&s, 500)) } else { s };
        log::info!("[Test] forwarding body: {}", truncated);
    }

    // Send request
    let start = std::time::Instant::now();
    let resp = match state
        .http_client
        .post(&url)
        .header(
            provider.auth_type.header_name(),
            provider.auth_type.header_value(&provider.api_key),
        )
        .header("content-type", "application/json")
        .json(&fwd_body)
        .timeout(std::time::Duration::from_secs(HEALTH_CHECK_TIMEOUT))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return TestResult {
                success: false,
                tag: tag.to_string(),
                provider: provider.name.clone(),
                model: route.model.clone(),
                format: format!("{:?}", route.format),
                latency_ms: start.elapsed().as_millis() as u64,
                error: Some(format!("connection error: {}", e)),
                response: None,
            };
        }
    };

    let latency_ms = start.elapsed().as_millis() as u64;
    let status = resp.status();
    let resp_text = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            return TestResult {
                success: false,
                tag: tag.to_string(),
                provider: provider.name.clone(),
                model: route.model.clone(),
                format: format!("{:?}", route.format),
                latency_ms,
                error: Some(format!("read response error: {}", e)),
                response: None,
            };
        }
    };

    if !status.is_success() {
        let err_preview = truncate_chars(&resp_text, 500);
        log::warn!("[Test] <<< HTTP {}: {}", status, err_preview);
        return TestResult {
            success: false,
            tag: tag.to_string(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            format: format!("{:?}", route.format),
            latency_ms,
            error: Some(format!("HTTP {}: {}", status, err_preview)),
            response: None,
        };
    }

    // Convert response back to Anthropic format
    let response = match route.format {
        ProviderFormat::Openai => {
            let openai_resp: Value =
                serde_json::from_str(&resp_text).map_err(|e| { log::warn!("[Test] failed to parse response as JSON: {}", e); Value::Null }).unwrap_or(Value::Null);
            convert::openai_to_anthropic_response(&openai_resp, tag)
        }
        ProviderFormat::OpenaiResponses => {
            let responses_resp: Value =
                serde_json::from_str(&resp_text).map_err(|e| { log::warn!("[Test] failed to parse response as JSON: {}", e); Value::Null }).unwrap_or(Value::Null);
            convert::responses_to_anthropic_response(&responses_resp, tag)
        }
        ProviderFormat::Anthropic => {
            serde_json::from_str(&resp_text).map_err(|e| { log::warn!("[Test] failed to parse response as JSON: {}", e); Value::Null }).unwrap_or(Value::Null)
        }
        _ => unreachable!("non-chat formats are filtered before test response conversion"),
    };

    log::info!("[Test] ✓ success in {}ms", latency_ms);

    TestResult {
        success: true,
        tag: tag.to_string(),
        provider: provider.name.clone(),
        model: route.model.clone(),
        format: format!("{:?}", route.format),
        latency_ms,
        error: None,
        response: Some(response),
    }
}

pub async fn test_route(
    state: &AppState,
    tag: &str,
    prompt: &str,
) -> TestResult {
    // Clone to release read lock immediately — test sends an HTTP request
    // that can take seconds, and holding the lock blocks admin config writes.
    let config = state.config.read().await.clone();

    // Find candidate routes (enabled, sorted by route_priority)
    let candidates = find_candidate_routes(&config.routes, tag, &config.tags);
    if candidates.is_empty() {
        return TestResult {
            success: false,
            tag: tag.to_string(),
            provider: String::new(),
            model: String::new(),
            format: String::new(),
            latency_ms: 0,
            error: Some(format!("no enabled route found for tag '{}'", tag)),
            response: None,
        };
    }

    let mut last_result: Option<TestResult> = None;

    for (attempt, (_route_idx, route)) in candidates.iter().enumerate() {
        if attempt > 0 {
            log::warn!("[Test] {} failover: trying route #{} (provider={}, model={})",
                tag, attempt + 1, route.provider, route.model);
        }

        let result = send_test_to_route(state, &config, route, tag, prompt).await;

        if result.success {
            return result;
        }

        // For tag-based test with failover: retry on 5xx/429/connection errors
        let is_retryable = result.error.as_ref().map_or(false, |e| {
            e.starts_with("HTTP 5") || e.starts_with("HTTP 429") || e.starts_with("connection error")
        });

        last_result = Some(result);

        if !is_retryable {
            // 4xx or other non-retryable errors: return immediately
            return last_result.unwrap();
        }
    }

    // All candidates failed — return last error
    last_result.unwrap_or_else(|| TestResult {
        success: false,
        tag: tag.to_string(),
        provider: String::new(),
        model: String::new(),
        format: String::new(),
        latency_ms: 0,
        error: Some(format!("all routes failed for tag '{}'", tag)),
        response: None,
    })
}

/// Test a single specific route by its index in the config (no failover).
/// Used by the Routes page "Test" button to test the exact route the user clicked.
pub async fn test_route_by_index(
    state: &AppState,
    route_index: usize,
    prompt: &str,
) -> TestResult {
    // Clone to release read lock immediately — test sends an HTTP request
    // that can take seconds, and holding the lock blocks admin config writes.
    let config = state.config.read().await.clone();

    let route = match config.routes.get(route_index) {
        Some(r) => r,
        None => {
            return TestResult {
                success: false,
                tag: String::new(),
                provider: String::new(),
                model: String::new(),
                format: String::new(),
                latency_ms: 0,
                error: Some(format!("route index {} out of bounds ({} routes configured)", route_index, config.routes.len())),
                response: None,
            };
        }
    };

    // Use first tag as display label, or route model name
    let tag = route.tags.first().map(|s| s.as_str()).unwrap_or(&route.model);
    log::info!("[Test-by-index] testing route #{}: {} via {} (format={:?})", route_index, route.model, route.provider, route.format);

    send_test_to_route(state, &config, route, tag, prompt).await
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("no route for tag '{0}'")]
    NoRoute(String),
    #[error("provider '{0}' not found")]
    NoProvider(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
}


/// Parse upstream response body as JSON, returning a proper error on failure
/// instead of silently degrading to Value::Null.
fn parse_upstream_json(resp_body: &[u8]) -> Result<Value, ProxyError> {
    serde_json::from_slice(resp_body)
        .map_err(|e| ProxyError::Upstream(format!("failed to parse upstream response: {}", e)))
}


impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            ProxyError::NoRoute(_) | ProxyError::NoProvider(_) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            ProxyError::RateLimited(_) => (StatusCode::TOO_MANY_REQUESTS, self.to_string()),
            ProxyError::Upstream(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
        };
        // Return error in Anthropic format so Claude Code / Anthropic SDK clients
        // can parse it correctly. OpenAI clients also tolerate this format.
        let body = serde_json::json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "message": msg
            }
        });
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_is_context_limit_error_detects_variants() {
        assert!(is_context_limit_error(
            "Error code: 400 - {'error': {'message': 'input token limit is 202752'}}"
        ));
        assert!(is_context_limit_error(
            "This model's maximum context length is 8192 tokens."
        ));
        assert!(is_context_limit_error("context_length_exceeded"));
        assert!(is_context_limit_error("Your prompt is too long for this model."));
        assert!(!is_context_limit_error("Invalid API key"));
        assert!(!is_context_limit_error("Bad request: missing field model"));
    }

    #[test]
    fn test_last_user_text_string_content() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": "describe a cat"}
            ]
        });
        assert_eq!(last_user_text(&body).as_deref(), Some("describe a cat"));
    }

    #[test]
    fn test_last_user_text_block_array_content() {
        // OpenCarrier image/vision form: content is an array of blocks.
        let body = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "a cat on the moon"}]}
            ]
        });
        assert_eq!(last_user_text(&body).as_deref(), Some("a cat on the moon"));
    }

    #[test]
    fn test_last_user_text_joins_multiple_blocks() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [{"text": "hello "}, {"text": "world"}]}
            ]
        });
        assert_eq!(last_user_text(&body).as_deref(), Some("hello world"));
    }

    #[test]
    fn test_last_user_text_skips_assistant() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "question"},
                {"role": "assistant", "content": "answer"}
            ]
        });
        // last user message is "question"
        assert_eq!(last_user_text(&body).as_deref(), Some("question"));
    }

    #[test]
    fn test_find_input_audio_plain_b64() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "input_audio": {"data": "AAAA", "format": "mp3"}
                }]
            }]
        });
        let (data, fmt) = find_input_audio(&body).expect("audio not found");
        assert_eq!(data, "AAAA");
        assert_eq!(fmt, "mp3");
    }

    #[test]
    fn test_find_input_audio_data_url_prefix() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "input_audio": {"data": "data:audio/wav;base64,UklGRg=="}
                }]
            }]
        });
        let (data, fmt) = find_input_audio(&body).expect("audio not found");
        assert_eq!(data, "UklGRg==");
        assert_eq!(fmt, "wav");
    }

    #[test]
    fn test_normalize_roles_converts_system_to_user() {
        let mut body = json!({
            "model": "test",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi"}
            ]
        });
        normalize_roles(&mut body);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
    }

    #[test]
    fn test_normalize_roles_preserves_user_and_assistant() {
        let mut body = json!({
            "model": "test",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi"}
            ]
        });
        normalize_roles(&mut body);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
    }

    #[test]
    fn test_normalize_roles_with_tool_messages() {
        let mut body = json!({
            "model": "test",
            "messages": [
                {"role": "system", "content": "system prompt"},
                {"role": "user", "content": "user msg"},
                {"role": "assistant", "content": [{"type": "tool_use", "name": "bash"}]},
                {"role": "user", "content": [{"type": "tool_result"}]}
            ]
        });
        normalize_roles(&mut body);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "user", "system should become user");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[3]["role"], "user");
    }

    #[test]
    fn test_strip_thinking_array_content() {
        let mut body = json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "inner monologue"},
                    {"type": "text", "text": "Hello"}
                ]}
            ]
        });
        strip_thinking(&mut body);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
    }

    #[test]
    fn test_strip_thinking_string_content_noop() {
        let mut body = json!({
            "messages": [
                {"role": "assistant", "content": "plain text response"}
            ]
        });
        strip_thinking(&mut body);
        assert_eq!(body["messages"][0]["content"], "plain text response");
    }

    fn test_tags() -> Vec<crate::config::Tag> {
        vec![
            crate::config::Tag::new("opus".into(), "#A855F7".into(), false),
            crate::config::Tag::new("sonnet".into(), "#3B82F6".into(), false),
            crate::config::Tag::new("haiku".into(), "#22C55E".into(), false),
            crate::config::Tag::new("auto".into(), "#F59E0B".into(), true),
        ]
    }

    #[test]
    fn test_resolve_tag_direct_matches() {
        let tags = test_tags();
        assert_eq!(resolve_tag_from_model("opus", &tags), Some("opus".to_string()));
        assert_eq!(resolve_tag_from_model("sonnet", &tags), Some("sonnet".to_string()));
        assert_eq!(resolve_tag_from_model("haiku", &tags), Some("haiku".to_string()));
        // "auto" is a configured tag, so a model literally named "auto" resolves to it.
        assert_eq!(resolve_tag_from_model("auto", &tags), Some("auto".to_string()));
    }

    #[test]
    fn test_resolve_tag_from_model_name_components() {
        let tags = test_tags();
        assert_eq!(resolve_tag_from_model("claude-opus-4-8", &tags), Some("opus".to_string()));
        assert_eq!(resolve_tag_from_model("claude-sonnet-4-6", &tags), Some("sonnet".to_string()));
        assert_eq!(resolve_tag_from_model("claude-haiku-4-5", &tags), Some("haiku".to_string()));
    }

    #[test]
    fn test_resolve_tag_unknown() {
        let tags = test_tags();
        // Unknown model names should return None (falls back to current_tag)
        assert_eq!(resolve_tag_from_model("some-random-model", &tags), None);
        assert_eq!(resolve_tag_from_model("", &tags), None);
    }

    #[test]
    fn test_resolve_tag_prefers_longer_match() {
        let tags = vec![
            crate::config::Tag::new("gpt".into(), "#000".into(), false),
            crate::config::Tag::new("gpt-5.5".into(), "#fff".into(), false),
        ];
        assert_eq!(resolve_tag_from_model("gpt-5.5", &tags), Some("gpt-5.5".to_string()));
        assert_eq!(resolve_tag_from_model("gpt-4o", &tags), Some("gpt".to_string()));
    }

    #[test]
    fn test_inject_reasoning_content_inserts_thinking_block() {
        let mut body = json!({
            "thinking": {"type": "adaptive"},
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "bash", "input": {}}]}
            ]
        });
        inject_reasoning_content(&mut body);

        let messages = body["messages"].as_array().unwrap();
        // User message unchanged
        assert_eq!(messages[0]["content"], "Hello");
        // String content becomes array with injected thinking
        let content1 = messages[1]["content"].as_array().unwrap();
        assert_eq!(content1[0]["type"], "thinking");
        assert_eq!(content1[0]["thinking"], " ");
        assert_eq!(content1[1]["type"], "text");
        assert_eq!(content1[1]["text"], "Hi");
        // Existing array content gets injected thinking at front
        let content2 = messages[2]["content"].as_array().unwrap();
        assert_eq!(content2[0]["type"], "thinking");
        assert_eq!(content2[1]["type"], "tool_use");
    }

    #[test]
    fn test_inject_reasoning_content_noop_when_thinking_disabled() {
        let mut body = json!({
            "messages": [
                {"role": "assistant", "content": "Hi"}
            ]
        });
        inject_reasoning_content(&mut body);
        assert_eq!(body["messages"][0]["content"], "Hi");
    }

    #[test]
    fn test_inject_reasoning_content_preserves_existing_thinking() {
        let mut body = json!({
            "thinking": {"type": "enabled"},
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "real reasoning"},
                    {"type": "text", "text": "Hi"}
                ]}
            ]
        });
        inject_reasoning_content(&mut body);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["thinking"], "real reasoning");
    }

    #[test]
    fn test_inject_openai_reasoning_content_from_enable_thinking() {
        let mut body = json!({
            "enable_thinking": true,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"}
            ]
        });
        inject_openai_reasoning_content(&mut body);
        assert_eq!(body["messages"][1]["reasoning_content"], " ");
        assert!(body["messages"][0].get("reasoning_content").is_none());
    }

    #[test]
    fn test_inject_openai_reasoning_content_from_prior_reasoning() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "thinking...", "reasoning_content": "I thought about it"},
                {"role": "user", "content": "follow up"},
                {"role": "assistant", "content": "response"}
            ]
        });
        inject_openai_reasoning_content(&mut body);
        // First assistant already has reasoning_content — untouched
        assert_eq!(body["messages"][1]["reasoning_content"], "I thought about it");
        // Second assistant gets placeholder injected
        assert_eq!(body["messages"][3]["reasoning_content"], " ");
    }

    #[test]
    fn test_inject_openai_reasoning_content_noop_when_not_thinking() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"}
            ]
        });
        inject_openai_reasoning_content(&mut body);
        assert!(body["messages"][1].get("reasoning_content").is_none());
    }
}
