use crate::config::{AppState, AppConfig, CircuitState, CircuitStatus, Provider, ProviderFormat, Route};
use crate::convert;
use crate::failure_class::{self, FailoverAction, FailureClass};
use axum::body::Body;
use axum::extract::{Path, Request, State};
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
// First-chunk (TTFT) deadline for streaming responses on NON-LAST candidates
// only: a provider that accepts the request then goes silent hands off to the
// next candidate instead of hanging until STREAM_TIMEOUT. The last candidate
// always runs to an answer. All major protocols emit a session/meta event
// immediately (message_start / response.created / role delta), so a generous
// 45s bound never cuts a legitimately-thinking model.
const FIRST_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
// Streaming idle watchdog: reset on every upstream chunk. True silence (no
// bytes at all, not even thinking deltas — providers stream their reasoning)
// for this long means a dead connection: terminate the stream instead of
// hanging the client until STREAM_TIMEOUT.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
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

/// Build the upstream poll URL for an async generation task. Seedance (Volcano
/// Ark) polls the task resource under its own v3 path; everything else
/// (DashScope) uses the shared /api/v1/tasks/{id} endpoint.
fn task_poll_url(route: &Route, task_id: &str) -> String {
    let base = route.base_url.trim_end_matches('/');
    match route.format {
        ProviderFormat::Seedance => {
            format!("{}/api/v3/contents/generations/tasks/{}", base, task_id)
        }
        _ => format!("{}/api/v1/tasks/{}", base, task_id),
    }
}

/// Build the upstream request body for a client-protocol × provider-format
/// conversion. THE single source of truth: the live proxy path (handle_proxy)
/// and the admin Test button (test_route) both call this, so what Test
/// exercises is the same conversion real traffic gets. (Lesson from
/// 2026-11-09: a duplicated copy in test_route drifted — the live path
/// renamed max_tokens for gpt-6 models, Test kept sending it and 400ed.)
fn build_fwd_body(
    client_protocol: &str,
    provider_format: &ProviderFormat,
    body: &Value,
    route: &Route,
) -> Result<Value, ProxyError> {
    Ok(match (client_protocol, provider_format) {
        ("anthropic", ProviderFormat::Anthropic) => {
            // Anthropic → Anthropic: passthrough with fixes
            // Note: don't strip_thinking here — Anthropic-compatible providers
            // like Zhipu support thinking blocks
            let mut b = body.clone();
            b["model"] = Value::String(route.model.clone());
            normalize_roles(&mut b);
            // Z.ai server-tool blocks (opencarrier analyze_image/webReader) are
            // malformed for Anthropic-compatible providers like Kimi K3; strip
            // them (their call+result are duplicated as text). Then repair any
            // orphan/misplaced regular tool_use pairs from context truncation.
            strip_server_tool_use(&mut b);
            repair_anthropic_tool_pairs(&mut b);
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
            let mut out = convert::anthropic_to_openai_request(&b, &route.model);
            // Anthropic requires max_tokens, so every request carries it —
            // rename for OpenAI's newer models or they 400.
            openai_rename_max_tokens(&mut out, route);
            out
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
            openai_rename_max_tokens(&mut b, route);
            b
        }
        ("openai", ProviderFormat::OpenaiResponses) => {
            // OpenAI Chat → OpenAI Responses
            convert::openai_to_responses_request(body, &route.model)
        }
        ("openai", ProviderFormat::Anthropic) => {
            // OpenAI Chat → Anthropic Messages
            convert::openai_to_anthropic_request(body, &route.model)
        }
        ("openai_responses", ProviderFormat::OpenaiResponses) => {
            // Responses → Responses: passthrough
            let mut b = body.clone();
            b["model"] = Value::String(route.model.clone());
            b
        }
        ("openai_responses", ProviderFormat::Openai) => {
            // Codex Responses → OpenAI Chat Completions provider
            let mut b = convert::responses_to_chat_request(body, &route.model);
            openai_rename_max_tokens(&mut b, route);
            b
        }
        ("openai_responses", ProviderFormat::Anthropic) => {
            // Codex Responses → Anthropic Messages provider
            convert::responses_to_anthropic_request(body, &route.model)
        }
        _ => {
            return Err(ProxyError::Upstream(format!(
                "unsupported protocol conversion: {} → {:?}",
                client_protocol, provider_format
            )));
        }
    })
}

/// api.openai.com's newer models (gpt-5+/gpt-6/o-series) reject `max_tokens`
/// on /v1/chat/completions: "Use 'max_completion_tokens' instead". Rename it
/// when — and only when — the upstream host is OpenAI itself: third-party
/// OpenAI-compatible providers (DeepSeek/zhipu/kimi) still speak max_tokens
/// and may not know the new name. If the caller sent both, the explicit
/// max_completion_tokens wins and max_tokens is dropped (sending both 400s).
fn openai_rename_max_tokens(body: &mut Value, route: &Route) {
    let base = route
        .base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    if !base.starts_with("api.openai.com") {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        if let Some(mt) = obj.remove("max_tokens") {
            obj.entry("max_completion_tokens").or_insert(mt);
        }
    }
}

/// GET /v1/help - return the multimodal usage guide (embedded markdown) so an
/// agent with a caller key can discover how to use each tag at runtime: which
/// tags exist, their endpoints, params, and special contracts (ppt needs a
/// document in a system message; video/seedance are async submit+poll;
/// seedream-lite needs a large size; etc.). Caller-key protected (lives in the
/// proxy_routes group), so it's reachable by authenticated agents but not open
/// to the public.
pub async fn handle_help() -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/markdown; charset=utf-8")],
        include_str!("../../MULTIMODAL_USAGE.md"),
    )
        .into_response()
}

/// Poll a DashScope async task by tag and task ID. After POSTing a video (or
/// future async image) generation, the client polls GET /v1/tasks/{tag}/{task_id}
/// for completion. AginxBrain proxies the GET to the upstream provider's
/// /api/v1/tasks/{task_id} endpoint.
pub async fn handle_task_poll(
    State(state): State<AppState>,
    Path((tag, task_id)): Path<(String, String)>,
    _request: Request,
) -> Result<Response, ProxyError> {
    let (task_url, provider) = {
        let config = state.config.read().await;
        let candidates = find_candidate_routes(&config.routes, &tag, &config.tags);
        let (_, route) = candidates
            .first()
            .ok_or_else(|| ProxyError::Upstream(format!("no route found for tag '{}'", tag)))?;
        let provider = config
            .providers
            .get(&route.provider)
            .cloned()
            .ok_or_else(|| ProxyError::Upstream(format!("provider '{}' not found", route.provider)))?;
        if provider.api_key.is_empty() {
            return Err(ProxyError::Upstream(format!(
                "provider '{}' has no API key configured",
                route.provider
            )));
        }

        // Pixmax polls via an authenticated POST, not a plain GET passthrough.
        if matches!(route.format, ProviderFormat::PixmaxVideo) {
            let item = pixmax_poll_task(&state, &provider, route, &task_id).await?;
            return Ok(Json(item).into_response());
        }

        let task_url = task_poll_url(route, &task_id);
        (task_url, provider)
    };

    log::info!(
        "[Proxy] task poll: tag={} task_id={} url={}",
        tag,
        task_id,
        task_url
    );
    let result = send_image_get(&state, &provider, &task_url).await?;

    // Capture authoritative token usage for async tasks that report it in the
    // completion response (Seedance bills by completion_tokens). Updates the
    // submit-time usage_log row matched by task_id; no-op for tasks/providers
    // whose poll response carries no usage (e.g. DashScope wanx video).
    capture_async_task_usage(&state.db, &task_id, &result).await;

    Ok(Json(result).into_response())
}

/// Update the submit-time usage_log row for an async generation task with the
/// authoritative token usage + terminal status from its poll response. Only
/// acts when the task reports usage (Seedance) or reaches a terminal state.
async fn capture_async_task_usage(db: &sqlx::SqlitePool, task_id: &str, result: &Value) {
    let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let usage = result.get("usage");
    let completion = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64());
    let total = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_i64());
    let prompt = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64());
    // Video models bill completion_tokens (output) with 0 input; derive input
    // as prompt_tokens, else total - completion, else 0.
    let input = Some(
        prompt
            .or_else(|| total.zip(completion).map(|(t, c)| t - c))
            .unwrap_or(0),
    );

    let is_terminal = matches!(status, "succeeded" | "failed" | "cancelled" | "expired");
    if completion.is_none() && !is_terminal {
        return;
    }
    let new_status = if status.is_empty() { None } else { Some(status) };
    let _ = crate::db::update_video_usage_by_task_id(db, task_id, new_status, input, completion)
        .await;
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

/// Backoff before same-route retry #`attempt` (1-based): 300ms, 600ms, capped
/// at 2s — transient hiccups clear instantly, so the delays stay short.
fn same_route_backoff(attempt: u32) -> std::time::Duration {
    let shift = attempt.saturating_sub(1).min(3);
    std::cmp::min(
        SAME_ROUTE_DELAY * (1u32 << shift),
        std::time::Duration::from_millis(2000),
    )
}

/// Send the upstream request, retrying the SAME route on transient failures
/// (network errors and 5xx) with short backoff — these are usually instant
/// hiccups, and keeping the same model beats jumping providers. Rate-limit /
/// credential / quota failures are NOT retried here: another attempt on the
/// same key can't clear them, so the caller's class-aware failover handles
/// those (docs/BIFROST-STUDY.md §2).
async fn send_with_same_route_retries(
    builder: reqwest::RequestBuilder,
    clone: &Option<reqwest::RequestBuilder>,
    timeout: std::time::Duration,
) -> Result<reqwest::Response, ProxyError> {
    let mut resp = match builder.timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) => {
            // Network-level failure (connect error, TLS, timeout): same-route
            // retries before giving the route up.
            let mut last_err = e.to_string();
            for attempt in 1..=SAME_ROUTE_RETRIES {
                let Some(base) = clone.as_ref() else { break };
                log::warn!(
                    "[Proxy] send failed, same-route retry #{}: {}",
                    attempt,
                    last_err
                );
                tokio::time::sleep(same_route_backoff(attempt)).await;
                match base.try_clone() {
                    Some(c) => match c.timeout(timeout).send().await {
                        Ok(r) => return Ok(r),
                        Err(e2) => last_err = e2.to_string(),
                    },
                    None => break,
                }
            }
            return Err(ProxyError::Upstream(last_err));
        }
    };

    // Upstream 5xx — retry the same route a couple of times before handing
    // the (still-failing) response back for cross-route classification.
    let mut attempt = 0;
    while resp.status().as_u16() >= 500 && attempt < SAME_ROUTE_RETRIES {
        let Some(base) = clone.as_ref() else { break };
        attempt += 1;
        log::warn!(
            "[Proxy] upstream {}, same-route retry #{}",
            resp.status(),
            attempt
        );
        tokio::time::sleep(same_route_backoff(attempt)).await;
        match base.try_clone() {
            Some(c) => match c.timeout(timeout).send().await {
                Ok(r) => resp = r,
                Err(e) => {
                    // Network error mid-retry: keep the 5xx response in hand —
                    // the caller classifies it Transient and fails over.
                    log::warn!("[Proxy] same-route retry #{} send failed: {}", attempt, e);
                    break;
                }
            },
            None => break,
        }
    }
    Ok(resp)
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
            | ProviderFormat::OpenaiImageEdit
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

/// Detect whether a (already protocol-converted) request body carries image
/// content - i.e. a vision request. Reasoning-over-images models such as
/// qwen3.7-plus legitimately spend well past the 45s non-streaming ceiling
/// before emitting the first byte, so callers use this to opt into the longer
/// reasoning timeout. Scans both `messages` (Chat / Anthropic) and `input`
/// (Responses) arrays and their content-block arrays for image block types
/// (`image_url`, `input_image`, `image`).
fn body_has_image_content(body: &Value) -> bool {
    for key in ["messages", "input"] {
        let Some(arr) = body.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for msg in arr {
            let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) else {
                continue;
            };
            for block in blocks {
                if let Some(t) = block.get("type").and_then(|v| v.as_str()) {
                    if matches!(t, "image_url" | "input_image" | "image") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Best-effort image MIME from a URL's extension; used when the fetch response
/// has no usable `Content-Type: image/*`. The query/fragment is stripped first
/// so `image.jpeg?sender_id=...%40im.wechat` doesn't poison the lookup.
fn guess_image_mime(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/jpeg",
    }
}

/// Cap on a single inlined image (raw bytes). Base64 inflates ~1.37x, so 10MB
/// raw becomes ~13.6MB in the provider request - around typical vision limits.
/// Larger images are left as URLs (the provider may still reject them, but we
/// avoid ballooning the request / OOMing on a hostile huge file).
const INLINE_IMAGE_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Fetch an image URL as `(media_type, base64)`. opencarrier serves vision
/// images at publicly fetchable (anonymous) URLs, so no auth is needed.
/// 30s timeout, 10MB cap. Returns None on any failure so the caller can leave
/// the URL in place.
async fn fetch_image_to_base64(
    client: &reqwest::Client,
    url: &str,
) -> Option<(String, String)> {
    // Bounded fetch: 30s per image. The shared client's global timeout is 1h -
    // a hanging image host must not stall the whole request.
    let resp = match client
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            log::warn!(
                "[Proxy] inline_image: {} -> HTTP {} (leaving url as-is)",
                url,
                r.status()
            );
            return None;
        }
        Err(e) => {
            log::warn!(
                "[Proxy] inline_image: {} fetch failed: {} (leaving url as-is)",
                url,
                e
            );
            return None;
        }
    };
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next())
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("image/"))
        .unwrap_or_else(|| guess_image_mime(url).to_string());
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            log::warn!(
                "[Proxy] inline_image: {} read body failed: {} (leaving url as-is)",
                url,
                e
            );
            return None;
        }
    };
    if bytes.is_empty() {
        log::warn!("[Proxy] inline_image: {} empty body (leaving url as-is)", url);
        return None;
    }
    if bytes.len() > INLINE_IMAGE_MAX_BYTES {
        log::warn!(
            "[Proxy] inline_image: {} too large ({} bytes > {}, leaving url as-is)",
            url,
            bytes.len(),
            INLINE_IMAGE_MAX_BYTES
        );
        return None;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    log::info!(
        "[Proxy] inline_image: inlined {} ({} bytes -> {} b64 chars)",
        url,
        bytes.len(),
        b64.len()
    );
    Some((mime, b64))
}

/// Walk an Anthropic-format request body and inline-fetch every `image` block
/// whose source is a URL, rewriting it to a base64 source. Anthropic-compatible
/// providers (Kimi K3, GLM, 豆包) reject `{"type":"url"}` image sources
/// ("unsupported image url") and only accept base64. Runs AFTER protocol
/// conversion, gated on the target provider being Anthropic-format, so it
/// covers every client path (openai/anthropic/responses -> anthropic) - the
/// decision is provider-driven, not client-protocol-driven. Best-effort: on
/// fetch failure the URL source is left in place (a provider that supports URLs
/// still gets it; a strict one 400s, same as today).
async fn inline_anthropic_image_urls(
    client: &reqwest::Client,
    body: &mut Value,
) {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    for msg in messages.iter_mut() {
        let Some(blocks) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        for block in blocks.iter_mut() {
            if block.get("type").and_then(|v| v.as_str()) != Some("image") {
                continue;
            }
            let is_url_source = block
                .get("source")
                .and_then(|s| s.get("type"))
                .and_then(|v| v.as_str())
                == Some("url");
            if !is_url_source {
                continue;
            }
            let Some(url) = block
                .get("source")
                .and_then(|s| s.get("url"))
                .and_then(|u| u.as_str())
                .map(|s| s.to_string())
            else {
                continue;
            };
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                continue;
            }
            if let Some((media_type, data)) = fetch_image_to_base64(client, &url).await {
                block["source"] = json!({"type":"base64","media_type":media_type,"data":data});
            }
        }
    }
}

/// Inject the qwen-doc-turbo PPT `skill` parameter and force `stream: true` on
/// an already protocol-converted request body. qwen-doc-turbo only emits slide
/// HTML (in `reasoning_content`) when `skill` is present at the top level and
/// the request streams. The client may override `template_id` by sending it as
/// a top-level field; the override field is stripped so it isn't forwarded to
/// the provider as a (invalid) Chat param.
fn inject_ppt_skill(body: &mut Value) {
    let template_id = body
        .get("template_id")
        .and_then(|v| v.as_str())
        .unwrap_or("news_01")
        .to_string();
    if let Some(obj) = body.as_object_mut() {
        obj.remove("template_id");
        obj.remove("mode");
        obj.insert(
            "skill".to_string(),
            json!([{ "type": "ppt", "mode": "general", "template_id": template_id }]),
        );
        obj.insert("stream".to_string(), Value::Bool(true));
    }
}

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
    let mut is_streaming = body
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

    // Per-API-key default thinking tier (BRAIN-CODEX.md §4.5). Precedence:
    // request param > per-key default > global default (never force) — the
    // per-key intent is only consulted when the request expresses nothing.
    let per_key_thinking = match caller_key_id {
        Some(kid) => crate::db::get_caller_default_thinking(&state.db, kid)
            .await
            .ok()
            .flatten()
            .and_then(|t| effort_to_intent(&t)),
        None => None,
    };
    if per_key_thinking.is_some() {
        log::info!("[Proxy] thinking: per-key default applies (caller={:?})", caller_key_id);
    }

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

    'candidates: for (attempt, (_route_idx, route)) in candidates.iter().enumerate() {
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
                | ProviderFormat::OpenaiImageEdit
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
                ProviderFormat::DashscopeVideo => {
                    match handle_video_request(state.clone(), caller_key_id, route, provider, &body, start).await {
                        Ok(resp) => return Ok(resp),
                        Err(e) => {
                            log::warn!("[Proxy] video route failed: {}", e);
                            if is_retryable(&e) { last_error = Some(e); continue; }
                            return Err(e);
                        }
                    }
                }
                ProviderFormat::Seedance => {
                    match handle_seedance_video_request(state.clone(), caller_key_id, route, provider, &body, start).await {
                        Ok(resp) => return Ok(resp),
                        Err(e) => {
                            log::warn!("[Proxy] seedance route failed: {}", e);
                            if is_retryable(&e) { last_error = Some(e); continue; }
                            return Err(e);
                        }
                    }
                }
                ProviderFormat::PixmaxVideo => {
                    match handle_pixmax_video_request(state.clone(), caller_key_id, route, provider, &body, start).await {
                        Ok(resp) => return Ok(resp),
                        Err(e) => {
                            log::warn!("[Proxy] pixmax route failed: {}", e);
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
    let mut fwd_body = build_fwd_body(client_protocol, provider_format, &body, route)?;

    // 4b. For OpenAI Chat format, ensure reasoning_content is present on all
    //     assistant messages when thinking mode is active. Clients like
    //     OpenCarrier don't echo reasoning_content back, but providers like
    //     DeepSeek require it and return 400 if it's missing.
    if matches!(provider_format, ProviderFormat::Openai) {
        inject_openai_reasoning_content(&mut fwd_body);
    }

    // 4c. Thinking control. An explicit client-side switch overrides the
    //     route-tag default:
    //     - Anthropic face: thinking.type "disabled" / "enabled" (budget_tokens)
    //     - OpenAI chat face: reasoning_effort none/minimal → off,
    //       low/medium/high → on (budget 4000/10000/24000); zhipu-style
    //       thinking{type} and enable_thinking also honored
    //     - Responses face: reasoning.effort same mapping
    //     Without any intent the legacy default applies: a "reasoning"
    //     tag on the route injects thinking with a 10000 budget — even when
    //     the client asked for a small max_tokens (aux callers like the
    //     aginxos mother's summary/compact calls hit this).
    let client_thinking = parse_thinking_intent(client_protocol, &body).or(per_key_thinking);
    if matches!(provider_format, ProviderFormat::Anthropic) {
        match client_thinking {
            Some(ThinkingIntent::Disabled) => {
                if let Some(obj) = fwd_body.as_object_mut() {
                    obj.insert("thinking".to_string(), json!({"type": "disabled"}));
                }
            }
            Some(ThinkingIntent::Enabled { budget_tokens }) => {
                enable_anthropic_thinking(&mut fwd_body, budget_tokens);
            }
            None => {
                let needs_thinking = route.tags.iter().any(|t| t == "reasoning")
                    || tag == "reasoning";
                if needs_thinking && fwd_body.get("thinking").is_none() {
                    enable_anthropic_thinking(&mut fwd_body, 10000);
                }
            }
        }
    } else if matches!(provider_format, ProviderFormat::Openai)
        && client_thinking == Some(ThinkingIntent::Disabled)
    {
        // Translate an explicit OFF into the zhipu/ark/moonshot-style chat
        // switch; deepseek-style providers ignore the field. ON flows
        // natively via reasoning_effort passthrough.
        if let Some(obj) = fwd_body.as_object_mut() {
            obj.insert("thinking".to_string(), json!({"type": "disabled"}));
        }
    } else if matches!(provider_format, ProviderFormat::OpenaiResponses)
        && client_thinking == Some(ThinkingIntent::Disabled)
    {
        if let Some(obj) = fwd_body.as_object_mut() {
            obj.remove("reasoning");
        }
    }

    // 4d. PPT tag: qwen-doc-turbo emits slide HTML as streaming chunks in
    //     `reasoning_content`, but only when the top-level `skill` parameter is
    //     present and `stream: true`. Inject a default skill (the client may
    //     override template_id by sending it in the body) and force streaming so
    //     slides come back regardless of what the client requested.
    if tag == "ppt" {
        inject_ppt_skill(&mut fwd_body);
        is_streaming = true;
    }

    // 4d. For Anthropic format, sanitise content block types the provider
    //     doesn't recognise (e.g. server_tool_use from OpenAI Agents SDK).
    //     Anthropic only accepts: text, thinking, image, document, tool_use,
    //     tool_result. Unknown types are remapped where possible, dropped if not.
    if matches!(provider_format, ProviderFormat::Anthropic) {
        sanitize_anthropic_blocks(&mut fwd_body);
        // Inline-fetch URL image sources -> base64. Anthropic-compatible
        // providers (Kimi K3, GLM, 豆包) reject {"type":"url"} image sources
        // ("unsupported image url"); clients send bare URLs, AginxBrain fetches
        // + inlines per provider. Runs post-conversion so it covers every
        // client path targeting an Anthropic-format provider.
        inline_anthropic_image_urls(&state.http_client, &mut fwd_body).await;
    }

    // 5. Build URL
    let url = format!(
        "{}{}",
        route.base_url.trim_end_matches('/'),
        route.effective_path()
    );
    log::info!("[Proxy] forwarding to {} (model={})", url, route.model);

    // 6. Build headers. Provider-level outbound proxy: resolve the reqwest
    //    client before dialing (invalid/down proxy URL fails this route and
    //    falls through to the next candidate).
    let http = match crate::config::client_for_provider(&state, provider).await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[Proxy] provider '{}' unusable, skipping: {}", route.provider, e);
            last_error = Some(ProxyError::Upstream(e));
            continue;
        }
    };
    let mut req_builder = http.post(&url);

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
        } else if matches!(tag.as_str(), "reasoning" | "vision" | "short-drama")
            || route.tags.iter().any(|t| matches!(t.as_str(), "reasoning" | "vision" | "short-drama"))
            || body_has_image_content(&fwd_body)
        {
            // Reasoning / deep-thinking models (qwen3.7-plus vision,
            // deepseek-v4-pro thinking, doubao-seed-2.1-pro, Kimi K3) legitimately
            // spend 60-120s "thinking" (+ web search) before emitting output. The
            // 45s chat ceiling cuts off the slow tail and triggers a wasteful
            // failover - and for K3 it cancels mid built-in tool call (Kimi logs
            // `context canceled` / 499). Key off BOTH the request tag AND the
            // route's tags: a reasoning model reached through a non-reasoning tag
            // (e.g. K3 via `opus`, which is tagged [reasoning, opus]) still needs
            // the long ceiling. body_has_image_content covers image requests
            // routed through any other tag.
            NON_STREAM_TIMEOUT_REASONING
        } else {
            NON_STREAM_TIMEOUT
        }
    );

    // 7. Send. Keep a clone AFTER setting the body so same-route retry
    //    re-sends the full request (not an empty body). Transient failures
    //    (5xx / network) are retried on the SAME route with backoff inside
    //    send_with_same_route_retries; everything else is classified below.
    let json_builder = req_builder.json(&fwd_body);
    let req_clone = json_builder.try_clone();
    let resp = match send_with_same_route_retries(json_builder, &req_clone, req_timeout).await {
        Ok(r) => r,
        Err(err) => {
            // Network-level failure survived same-route retries — transient by
            // definition. Try the next candidate.
            log::warn!("[Proxy] {} timeout/connection error after same-route retries: {}",
                if is_streaming { "streaming" } else { "non-streaming" }, err);
            record_circuit_failure(&route.id, &err, &state.circuit_breaker).await;
            last_error = Some(err);
            continue;
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
        // Classify first (structured code/type → message phrases → bare
        // status), then act on the class — see docs/BIFROST-STUDY.md §1-2.
        let class = failure_class::classify_upstream_failure(Some(status_code), &err_body);
        match class.failover_action() {
            FailoverAction::NextRoute { backoff } => {
                if matches!(class, FailureClass::RateLimit | FailureClass::Quota) {
                    rate_limited_count += 1;
                }
                let err = ProxyError::Upstream(format!("HTTP {}: {}",
                    status_code, truncate_chars(&err_body, 200)));
                log::warn!("[Proxy] upstream {} classified {:?} (retryable): trying next route",
                    status_code, class);
                // Request-shaped failures (context limit, modality gap) say
                // nothing about route health and must not trip the circuit.
                if class.counts_against_route_health() {
                    record_circuit_failure(&route.id, &err, &state.circuit_breaker).await;
                }
                if let Some(delay) = backoff {
                    tokio::time::sleep(delay).await;
                }
                last_error = Some(err);
                continue;
            }
            FailoverAction::ReturnToClient => {
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
        }
    }

    // 8. Convert response if needed
    if is_streaming {
        // ── Commit gate: validate the first SSE event before forwarding ──
        // Once the streaming response is returned the request is committed —
        // no failover is possible. Inspect the head of the stream first:
        //   * TTFT deadline (non-last candidates only): a provider that
        //     accepts the request then goes silent hands off to the next
        //     candidate instead of hanging until STREAM_TIMEOUT. The last
        //     candidate always runs to an answer.
        //   * In-band error scan: some providers (zhipu, kimi) return HTTP
        //     200 and then emit an error event as the first SSE frame. A
        //     detected error still failovers — a 200 + error event is never
        //     the client's fault.
        let is_last_candidate = attempt + 1 == candidates.len();
        let mut upstream = Box::pin(resp.bytes_stream());
        let mut prefix: Vec<Result<bytes::Bytes, reqwest::Error>> = Vec::new();
        let mut event_buf: Vec<u8> = Vec::new();
        const MAX_FIRST_EVENT_SCAN: usize = 16 * 1024;

        let mut validated = false;
        while !validated {
            let next = if is_last_candidate {
                upstream.next().await
            } else {
                match tokio::time::timeout(FIRST_CHUNK_TIMEOUT, upstream.next()).await {
                    Ok(item) => item,
                    Err(_) => {
                        let err = ProxyError::Upstream(format!(
                            "first chunk timeout after {}s (TTFT deadline)",
                            FIRST_CHUNK_TIMEOUT.as_secs()
                        ));
                        log::warn!("[Proxy] {} — failing over to next candidate", err);
                        record_circuit_failure(&route.id, &err, &state.circuit_breaker).await;
                        last_error = Some(err);
                        continue 'candidates;
                    }
                }
            };
            let item = match next {
                Some(i) => i,
                None => {
                    // Upstream closed before a complete first event — broken
                    // response, not an empty answer.
                    let err = ProxyError::Upstream(
                        "upstream closed stream before first event".to_string(),
                    );
                    log::warn!("[Proxy] {} — failing over to next candidate", err);
                    record_circuit_failure(&route.id, &err, &state.circuit_breaker).await;
                    last_error = Some(err);
                    continue 'candidates;
                }
            };
            let bytes = match item {
                Ok(b) => b,
                Err(e) => {
                    let err =
                        ProxyError::Upstream(format!("stream error before first event: {}", e));
                    log::warn!("[Proxy] {} — failing over to next candidate", err);
                    record_circuit_failure(&route.id, &err, &state.circuit_breaker).await;
                    last_error = Some(err);
                    continue 'candidates;
                }
            };
            event_buf.extend_from_slice(&bytes);
            let complete = event_buf.windows(2).any(|w| w == b"\n\n")
                || event_buf.windows(4).any(|w| w == b"\r\n\r\n")
                || event_buf.len() >= MAX_FIRST_EVENT_SCAN;
            prefix.push(Ok(bytes));
            if complete {
                if let Some(err_detail) = failure_class::scan_sse_event_error(&event_buf) {
                    // HTTP 200 + error event: provider-level failure. Always
                    // fail over — even for classes that would normally return
                    // to the client, the request was accepted.
                    let err = ProxyError::Upstream(format!(
                        "in-stream error: {}",
                        truncate_chars(&err_detail, 200)
                    ));
                    log::warn!("[Proxy] {} — failing over to next candidate", err);
                    record_circuit_failure(&route.id, &err, &state.circuit_breaker).await;
                    last_error = Some(err);
                    continue 'candidates;
                }
                validated = true;
            }
        }

        // First event is clean — NOW commit: record usage, close the circuit.
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

        close_circuit(&route.id, &state.circuit_breaker).await;

        // ── Drain upstream in background to capture full usage even if client disconnects ──
        // The upstream response must be fully consumed so the usage tee sees the
        // message_delta event (which contains the final token counts). Without this,
        // a client disconnect causes Axum to drop the response body, which stops the
        // stream converter from polling upstream, and the tee misses the tail of the SSE.
        // The validated prefix is chained in front so the converters and the usage
        // tee see the exact byte stream the provider sent.
        let chained: std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>,
        > = if prefix.is_empty() {
            upstream
        } else {
            Box::pin(futures::stream::iter(prefix).chain(upstream))
        };
        let (drain_tx, drain_rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();
        let db = state.db.clone();
        let log_id = usage_log_id;
        let usage_format = provider_format.clone();
        {
            let db_c = db.clone();
            let log_id_c = log_id;
            let usage_format_c = usage_format.clone();
            let mut upstream = chained;
            tokio::spawn(async move {
                // Usage events sit at the two ends of the stream (Anthropic
                // message_start at the head, final message_delta at the tail),
                // so keep the first and last 512 KB - the middle is dead
                // weight for extraction. Head-only retention lost the tail on
                // big streams, where zhipu/kimi put ALL real usage numbers.
                const MAX_USAGE_HEAD: usize = 512 * 1024;
                const MAX_USAGE_TAIL: usize = 512 * 1024;
                let mut head: Vec<u8> = Vec::new();
                let mut tail: Vec<u8> = Vec::new();
                let mut capped = false;
                loop {
                    // Idle watchdog (docs/BIFROST-STUDY.md §4): reset on every
                    // chunk. Thinking models stream their reasoning, so true
                    // byte-silence means a dead connection — terminate the
                    // stream instead of hanging until STREAM_TIMEOUT.
                    let item = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, upstream.next()).await {
                        Ok(item) => item,
                        Err(_) => {
                            log::warn!(
                                "[Proxy] upstream stream idle for {}s — terminating stream (log_id={:?})",
                                STREAM_IDLE_TIMEOUT.as_secs(), log_id_c
                            );
                            let _ = drain_tx.send(Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "upstream stream idle timeout",
                            )));
                            break;
                        }
                    };
                    let Some(result) = item else { break };
                    if let Ok(ref bytes) = result {
                        if !capped {
                            if head.len() + bytes.len() <= MAX_USAGE_HEAD {
                                head.extend_from_slice(bytes);
                            } else {
                                let take = MAX_USAGE_HEAD - head.len();
                                head.extend_from_slice(&bytes[..take]);
                                capped = true;
                                log::info!(
                                    "[Proxy] usage extraction: large stream, retaining head+tail ({}+{} bytes cap each)",
                                    MAX_USAGE_HEAD, MAX_USAGE_TAIL
                                );
                                let rest = &bytes[take..];
                                if rest.len() >= MAX_USAGE_TAIL {
                                    tail.extend_from_slice(&rest[rest.len() - MAX_USAGE_TAIL..]);
                                } else {
                                    tail.extend_from_slice(rest);
                                }
                            }
                        } else if tail.len() + bytes.len() <= MAX_USAGE_TAIL {
                            tail.extend_from_slice(bytes);
                        } else {
                            let overflow = tail.len() + bytes.len() - MAX_USAGE_TAIL;
                            if overflow >= tail.len() {
                                tail.clear();
                                tail.extend_from_slice(&bytes[bytes.len() - MAX_USAGE_TAIL..]);
                            } else {
                                tail.drain(..overflow);
                                tail.extend_from_slice(bytes);
                            }
                        }
                    }
                    let _ = drain_tx.send(result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
                }
                let (input, output) = if capped {
                    extract_usage_from_sse_buffer(&head, Some(&tail), &usage_format_c)
                } else {
                    extract_usage_from_sse_buffer(&head, None, &usage_format_c)
                };
                if let (Some(i), Some(o)) = (input, output) {
                    if let Some(id) = log_id_c {
                        let _ = crate::db::update_usage_tokens(&db_c, id, i, o).await;
                    }
                } else if log_id_c.is_some() {
                    log::warn!("[Proxy] usage extraction incomplete: input={:?}, output={:?} for log_id={:?}, head_len={}, tail_len={}", input, output, log_id_c, head.len(), tail.len());
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
        // Non-streaming: success is already committed (the full body arrived
        // with a 2xx), so record usage and close the circuit up front.
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

    // All candidates failed — if they were all rate-limited (or quota-blocked
    // with 401/403), return 429 so the client knows to retry with backoff
    // instead of crashing on an unexpected 502.
    if rate_limited_count > 0 && rate_limited_count as usize == candidates.len() {
        Err(ProxyError::RateLimited("all providers rate limited, please retry later".into()))
    } else {
        let detail = last_error.as_ref().map(|e| e.to_string()).unwrap_or_default();
        Err(ProxyError::Upstream(format!(
            "all providers unavailable, please try again later: {}",
            detail
        )))
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
        route.effective_path()
    );
    log::info!("[Proxy] count_tokens forwarding to {}", url);

    let http = match crate::config::client_for_provider(&state, provider).await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[Proxy] count_tokens provider '{}' unusable, skipping: {}", route.provider, e);
            continue;
        }
    };
    let mut req_builder = http.post(&url);
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
struct SseUsageScan {
    input: Option<i64>,
    output: Option<i64>,
    estimated_output_chars: i64,
    delta_events: u32,
}

fn scan_sse_for_usage(buf: &[u8], format: &ProviderFormat) -> SseUsageScan {
    let text = String::from_utf8_lossy(buf);
    let mut scan = SseUsageScan {
        input: None,
        output: None,
        estimated_output_chars: 0,
        delta_events: 0,
    };

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
                    scan.delta_events += 1;
                    if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                        scan.estimated_output_chars += t.len() as i64;
                    }
                    if let Some(t) = delta.get("thinking").and_then(|v| v.as_str()) {
                        scan.estimated_output_chars += t.len() as i64;
                    }
                    if let Some(t) = delta.get("partial_json").and_then(|v| v.as_str()) {
                        scan.estimated_output_chars += t.len() as i64;
                    }
                }
                // Usage placement varies by provider: top-level `usage`
                // (Anthropic message_delta, OpenAI chat final chunk), nested
                // `message.usage` (Anthropic message_start), nested
                // `response.usage` (OpenAI Responses `response.completed`).
                let usage = json
                    .get("usage")
                    .or_else(|| json.pointer("/message/usage"))
                    .or_else(|| json.pointer("/response/usage"));
                if let Some(usage) = usage {
                    match format {
                        ProviderFormat::Anthropic | ProviderFormat::OpenaiResponses => {
                            if let Some(total) = anthropic_total_input(usage) {
                                scan.input = Some(total);
                            }
                            if let Some(v) = usage.get("output_tokens").and_then(|v| v.as_i64()) {
                                scan.output = Some(v);
                            }
                        }
                        ProviderFormat::Openai | ProviderFormat::OpenaiImages => {
                            if let Some(v) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
                                scan.input = Some(v);
                            }
                            if let Some(v) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
                                scan.output = Some(v);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    scan
}

/// Extract token usage from the retained head (and, for streams that exceeded
/// it, tail) of an upstream SSE buffer. Usage events sit at the two ends of a
/// stream - Anthropic `message_start` at the head, `message_delta` with the
/// final counts at the tail - so scanning both keeps provider-reported numbers
/// for arbitrarily large streams. `tail` must be None when the whole stream fit
/// in `buf` (the slices never overlap, so estimates are not double-counted).
fn extract_usage_from_sse_buffer(
    buf: &[u8],
    tail: Option<&[u8]>,
    format: &ProviderFormat,
) -> (Option<i64>, Option<i64>) {
    let head_scan = scan_sse_for_usage(buf, format);
    let mut input = head_scan.input;
    let mut output = head_scan.output;
    let mut estimated_output_chars = head_scan.estimated_output_chars;
    let mut delta_events = head_scan.delta_events;

    if let Some(tail) = tail {
        let tail_scan = scan_sse_for_usage(tail, format);
        // Tail events are the later ones (message_delta); let them win.
        if tail_scan.input.is_some() {
            input = tail_scan.input;
        }
        if tail_scan.output.is_some() {
            output = tail_scan.output;
        }
        estimated_output_chars += tail_scan.estimated_output_chars;
        delta_events += tail_scan.delta_events;
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
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
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

            let mut local_buf;

            {
                let mut guard = buffer.lock().await;
                // 字节缓冲+只解码完整行（防跨读劈开的多字节 UTF-8 变 U+FFFD，
                // 同 convert/streaming.rs::take_complete_lines）
                guard.extend_from_slice(&chunk);
                local_buf = crate::convert::streaming::take_complete_lines(&mut guard);
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

            // 残尾已在共享字节缓冲（take_complete_lines 不取无换行尾巴），无需写回
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
    let buffer = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<u8>::new()));
    Box::pin(async_stream::stream! {
        let mut upstream = upstream;
        while let Some(chunk_result) = upstream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => { yield Err(e); return; }
            };
            let mut local_buf;
            {
                let mut guard = buffer.lock().await;
                // 字节缓冲+只解码完整行（防跨读劈开的多字节 UTF-8 变 U+FFFD）
                guard.extend_from_slice(&chunk);
                local_buf = crate::convert::streaming::take_complete_lines(&mut guard);
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
            // 残尾已在共享字节缓冲（take_complete_lines 不取无换行尾巴），无需写回
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
/// Explicit client-side thinking intent, normalized across the three faces.
/// None means the client expressed nothing — legacy route-tag defaults apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkingIntent {
    Disabled,
    Enabled { budget_tokens: u64 },
}

fn effort_to_intent(effort: &str) -> Option<ThinkingIntent> {
    match effort {
        "none" | "minimal" => Some(ThinkingIntent::Disabled),
        "low" => Some(ThinkingIntent::Enabled { budget_tokens: 4000 }),
        "medium" => Some(ThinkingIntent::Enabled { budget_tokens: 10000 }),
        "high" => Some(ThinkingIntent::Enabled { budget_tokens: 24000 }),
        _ => None,
    }
}

fn parse_anthropic_thinking_param(t: &Value) -> Option<ThinkingIntent> {
    match t.get("type")?.as_str()? {
        "disabled" => Some(ThinkingIntent::Disabled),
        "enabled" => Some(ThinkingIntent::Enabled {
            budget_tokens: t.get("budget_tokens").and_then(|v| v.as_u64()).unwrap_or(10000),
        }),
        _ => None,
    }
}

/// Read thinking intent from the ORIGINAL client body (not the converted
/// fwd_body — conversions drop the thinking param).
fn parse_thinking_intent(client_protocol: &str, body: &Value) -> Option<ThinkingIntent> {
    match client_protocol {
        "anthropic" => body.get("thinking").and_then(parse_anthropic_thinking_param),
        "openai" => body
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .and_then(effort_to_intent)
            .or_else(|| body.get("thinking").and_then(parse_anthropic_thinking_param))
            .or_else(|| {
                match body.get("enable_thinking").and_then(|v| v.as_bool()) {
                    Some(false) => Some(ThinkingIntent::Disabled),
                    Some(true) => Some(ThinkingIntent::Enabled { budget_tokens: 10000 }),
                    None => None,
                }
            }),
        "openai_responses" => body
            .get("reasoning")
            .and_then(|r| r.get("effort"))
            .and_then(|v| v.as_str())
            .and_then(effort_to_intent),
        _ => None,
    }
}

fn enable_anthropic_thinking(body: &mut Value, budget_tokens: u64) {
    let Some(obj) = body.as_object_mut() else { return };
    obj.insert(
        "thinking".to_string(),
        json!({"type": "enabled", "budget_tokens": budget_tokens}),
    );
    // Anthropic requires max_tokens > thinking.budget_tokens.
    let cur = obj.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(4096);
    if cur <= budget_tokens {
        obj.insert("max_tokens".to_string(), json!(budget_tokens + 6000));
    }
}

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

/// Strip Z.ai-style `server_tool_use` blocks and their `tool_result` responses.
/// Clients like opencarrier emit server-side tool calls (analyze_image,
/// webReader) as `server_tool_use` blocks whose `tool_result` lands *inside* the
/// same assistant message — malformed for Anthropic-compatible providers like
/// Kimi K3, which reject the request ("tool_calls must be followed by tool
/// messages"). The call and result are already duplicated as text blocks
/// ("🌐 Z.ai Built-in Tool: ..." / "Output: ..."), so dropping the structured
/// blocks loses no information. Removes every `server_tool_use` block and every
/// `tool_result` whose tool_use_id matches one.
fn strip_server_tool_use(body: &mut Value) {
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    let mut server_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in msgs.iter() {
        if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
            for blk in arr {
                if blk.get("type").and_then(|v| v.as_str()) == Some("server_tool_use") {
                    if let Some(id) = blk.get("id").and_then(|v| v.as_str()) {
                        server_ids.insert(id.to_string());
                    }
                }
            }
        }
    }
    if server_ids.is_empty() {
        return;
    }
    for m in msgs.iter_mut() {
        if let Some(arr) = m.get_mut("content").and_then(|c| c.as_array_mut()) {
            arr.retain(|blk| match blk.get("type").and_then(|v| v.as_str()) {
                Some("server_tool_use") => false,
                Some("tool_result") => blk
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .map(|id| !server_ids.contains(id))
                    .unwrap_or(true),
                _ => true,
            });
        }
    }
}

/// Ensure every `tool_use` block is followed by a matching `tool_result`, as
/// Anthropic-compatible providers require. Inbound history (e.g. Claude Code
/// after context truncation /compact) can orphan or misplace a `tool_result`,
/// and strict Anthropic-format providers like Kimi K3 reject the request with
/// "tool_calls must be followed by tool messages". This collects every
/// `tool_result`, strips it from its origin, then re-derives the user message
/// after each assistant `tool_use` so every `tool_use_id` is answered (existing
/// result repositioned, or a synthesized empty one when missing). It preserves
/// user/assistant alternation, merges results with any text the following user
/// message carries, and drops orphan `tool_result` blocks with no matching
/// `tool_use` (which would themselves trigger a 400).
fn repair_anthropic_tool_pairs(body: &mut Value) {
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    // 1. Index every tool_result by tool_use_id (first wins) and strip it from
    //    its original user message, leaving the non-tool_result content behind.
    let mut results: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for m in msgs.iter_mut() {
        if m.get("role").and_then(|v| v.as_str()) == Some("user") {
            if let Some(arr) = m.get_mut("content").and_then(|c| c.as_array_mut()) {
                let mut kept = Vec::with_capacity(arr.len());
                for block in arr.drain(..) {
                    let is_tr = block.get("type").and_then(|v| v.as_str()) == Some("tool_result");
                    let id = block.get("tool_use_id").and_then(|v| v.as_str()).map(String::from);
                    if is_tr {
                        if let Some(id) = id {
                            results.entry(id).or_insert(block);
                        }
                        // duplicate or id-less tool_result: drop
                    } else {
                        kept.push(block);
                    }
                }
                *arr = kept;
            }
        }
    }

    // 2. Rebuild. `pending` holds tool_use_ids from the last assistant message
    //    awaiting their tool_results; they are answered in the next user
    //    message (merged), or flushed as a synthetic user message otherwise.
    let mut out: Vec<Value> = Vec::with_capacity(msgs.len() + 4);
    let mut pending: Vec<String> = Vec::new();
    for m in msgs.iter() {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        if role == "user" {
            let mut blocks: Vec<Value> = pending
                .drain(..)
                .map(|id| {
                    results
                        .remove(&id)
                        .unwrap_or_else(|| json!({"type":"tool_result","tool_use_id":id,"content":""}))
                })
                .collect();
            match m.get("content") {
                Some(Value::Array(arr)) => blocks.extend(arr.iter().cloned()),
                Some(Value::String(s)) => blocks.push(json!({"type":"text","text":s})),
                _ => {}
            }
            if !blocks.is_empty() {
                out.push(json!({"role":"user","content":blocks}));
            }
            // an emptied user message (only tool_results, now stripped) is dropped
        } else {
            // Flush pending before a non-user message to keep pairing correct.
            if !pending.is_empty() {
                let blocks: Vec<Value> = pending
                    .drain(..)
                    .map(|id| {
                        results
                            .remove(&id)
                            .unwrap_or_else(|| json!({"type":"tool_result","tool_use_id":id,"content":""}))
                    })
                    .collect();
                out.push(json!({"role":"user","content":blocks}));
            }
            out.push(m.clone());
            if role == "assistant" {
                if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
                    pending = arr
                        .iter()
                        .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                        .filter_map(|b| b.get("id").and_then(|v| v.as_str()).map(String::from))
                        .filter(|id| !id.is_empty())
                        .collect();
                }
            }
        }
    }
    // Trailing assistant tool_use with no following user message: answer it.
    if !pending.is_empty() {
        let blocks: Vec<Value> = pending
            .drain(..)
            .map(|id| {
                results
                    .remove(&id)
                    .unwrap_or_else(|| json!({"type":"tool_result","tool_use_id":id,"content":""}))
            })
            .collect();
        out.push(json!({"role":"user","content":blocks}));
    }

    *msgs = out;
}

/// Remove or convert content blocks with types that Anthropic providers don't
/// accept (only `text`, `thinking`, `image`, `document`, `tool_use`, and
/// `tool_result` are valid). Unknown types are either mapped to a supported
/// type or dropped.
///
/// Some clients (e.g. OpenAI Agents SDK) emit `server_tool_use` blocks, which
/// share the same shape as `tool_use` (`id`/`name`/`input`) but use a
/// different type string. We map those to `tool_use`.
fn sanitize_anthropic_blocks(value: &mut Value) {
    const VALID_TYPES: &[&str] = &["text", "thinking", "image", "document", "tool_use", "tool_result"];

    if let Some(messages) = value.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut dropped_empty_text = 0usize;
        let mut backfilled = 0usize;
        for msg in messages.iter_mut() {
            if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                // First remap known non-standard types that have the same shape
                for block in content.iter_mut() {
                    if let Some(obj) = block.as_object_mut() {
                        if obj.get("type").and_then(|t| t.as_str()) == Some("server_tool_use") {
                            obj.insert("type".to_string(), Value::String("tool_use".to_string()));
                        }
                    }
                }
                // Then drop anything still not in the allow-list, AND drop
                // empty text blocks. Some Anthropic-compatible providers (Kimi
                // K3) reject `{"type":"text","text":""}` with "text content is
                // empty" even though the real Anthropic API tolerates them.
                content.retain(|block| {
                    let t = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if !t.is_empty() && !VALID_TYPES.contains(&t) {
                        log::warn!("[Proxy] dropping unrecognised Anthropic content block type: {t}");
                        return false;
                    }
                    if t == "text" {
                        let empty = block
                            .get("text")
                            .and_then(|v| v.as_str())
                            .map(|s| s.is_empty())
                            .unwrap_or(true);
                        if empty {
                            dropped_empty_text += 1;
                            return false;
                        }
                    }
                    true
                });
                // Dropping empty/invalid blocks can leave a message with an
                // empty content array, which providers also reject. Backfill
                // with a minimal placeholder so the message survives (we can't
                // drop the message itself without breaking user/assistant
                // alternation).
                if content.is_empty() {
                    backfilled += 1;
                    content.push(json!({"type":"text","text":" "}));
                }
            }
        }
        // One summary line per request instead of one WARN per block - large
        // Claude Code contexts routinely carry dozens of empty text blocks and
        // would otherwise flood the log.
        if dropped_empty_text > 0 || backfilled > 0 {
            log::info!(
                "[Proxy] sanitize_anthropic: dropped {} empty text block(s), backfilled {} emptied message(s)",
                dropped_empty_text, backfilled
            );
        }
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

        let url = format!("{}{}", route.base_url.trim_end_matches('/'), route.effective_path());
        let start = std::time::Instant::now();
        let result = match route.format {
            ProviderFormat::DashscopeImage => generate_dashscope_image(state, provider, route, &url, &prompt, &req).await,
            ProviderFormat::DashscopeChatImage => generate_dashscope_chat_image(state, provider, route, &url, &prompt, &req).await,
            ProviderFormat::MinimaxImage => generate_minimax_image(state, provider, route, &url, &prompt, &req).await,
            ProviderFormat::OpenaiImages => generate_openai_images(state, provider, route, &url, &prompt, &req).await,
            ProviderFormat::OpenaiImageEdit => match req.extra.get("image").and_then(|v| v.as_str()) {
                Some(image_ref) => generate_openai_image_edit(state, provider, route, &url, &prompt, &req, image_ref).await,
                None => Err(ProxyError::Upstream("image edit: reference image required — set extra.image (url or data url)".into())),
            },
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
        ProviderFormat::OpenaiImageEdit => "openai_image_edit",
        ProviderFormat::DashscopeImage => "dashscope_image",
        ProviderFormat::DashscopeChatImage => "dashscope_chat_image",
        ProviderFormat::DashscopeVideo => "dashscope_video",
        ProviderFormat::DashscopeTts => "dashscope_tts",
        ProviderFormat::DashscopeAsr => "dashscope_asr",
        ProviderFormat::Kling => "kling",
        ProviderFormat::Seedance => "seedance",
        ProviderFormat::MinimaxImage => "minimax_image",
        ProviderFormat::PixmaxVideo => "pixmax_video",
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

    let url = format!("{}{}", route.base_url.trim_end_matches('/'), route.effective_path());
    log::info!(
        "[Proxy] image → {} (model={}, format={:?})",
        provider.name, route.model, route.format
    );
    log::info!(
        "[Proxy] forwarding image request to {}",
        url
    );

    // Reference image (image-to-image / editing): forward the first image_url
    // block in messages as the provider's `image` param (Seedream 5.0 supports
    // it on /images/generations). Also pass through a few Seedream-specific
    // params the client may set. These ride on `extra`, which
    // generate_openai_images merges into the upstream request body.
    let mut extra = std::collections::HashMap::new();
    if let Some(img) = find_first_image_url(body) {
        extra.insert("image".to_string(), Value::String(img));
    }
    for k in ["output_format", "watermark"] {
        if let Some(v) = body.get(k) {
            extra.insert(k.to_string(), v.clone());
        }
    }

    let req = GenerateImageRequest {
        tag: None,
        prompt: prompt.clone(),
        size,
        n,
        extra,
    };

    let images: Vec<GeneratedImage> = match &route.format {
        ProviderFormat::OpenaiImages => generate_openai_images(&state, provider, route, &url, &prompt, &req).await?,
        ProviderFormat::OpenaiImageEdit => {
            // Explicit edit route: /v1/images/edits only makes sense with a
            // reference image; without one the request belongs on openai_images.
            let image_ref = req
                .extra
                .get("image")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ProxyError::Upstream(
                        "image edit: reference image required — add an image_url block to the last user message, or use the openai_images format for text-to-image".into(),
                    )
                })?;
            generate_openai_image_edit(&state, provider, route, &url, &prompt, &req, image_ref).await?
        }
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

/// Video generation (text-to-video): send the prompt to DashScope
/// video-synthesis (async task), poll for completion, return the video URL.
async fn handle_video_request(
    state: AppState,
    caller_key_id: Option<i64>,
    route: &Route,
    provider: &Provider,
    body: &Value,
    start: std::time::Instant,
) -> Result<Response, ProxyError> {
    let prompt = last_user_text(body)
        .ok_or_else(|| ProxyError::Upstream("video: no text prompt found in messages".into()))?;
    let size = body.get("size").and_then(|v| v.as_str()).unwrap_or("1280*720").to_string();
    let duration = body.get("duration").and_then(|v| v.as_u64()).unwrap_or(5);

    let url = format!("{}{}", route.base_url.trim_end_matches('/'), route.effective_path());
    log::info!(
        "[Proxy] video → {} (model={}, format={:?})",
        provider.name, route.model, route.format
    );
    log::info!("[Proxy] submitting video task to {}", url);

    // Submit the async task (sync returns 403). Return the task_id immediately;
    // the client polls GET /v1/tasks/{tag}/{task_id} for completion. This keeps
    // the HTTP connection short and avoids timeouts on slow video generation.
    let parameters = json!({ "size": size, "duration": duration });
    let input = json!({ "prompt": prompt });
    let submit_body = json!({ "model": route.model, "input": input, "parameters": parameters });
    let result =
        send_image_post(&state, provider, &url, &submit_body, &[("X-DashScope-Async", "enable")])
            .await?;
    let task_id = result
        .pointer("/output/task_id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| ProxyError::Upstream("No task_id in DashScope video response".to_string()))?;
    let task_status = result
        .pointer("/output/task_status")
        .and_then(|v| v.as_str())
        .unwrap_or("PENDING");
    let tag = route.tags.first().cloned().unwrap_or_else(|| "video".to_string());

    log::info!(
        "[Proxy] video task submitted: task_id={} status={} latency={}ms",
        task_id,
        task_status,
        start.elapsed().as_millis()
    );

    let usage_id = crate::db::insert_usage_log(
        &state.db,
        crate::db::UsageInsert {
            caller_key_id,
            tag: tag.clone(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            request_model: route.model.clone(),
            modality: "video_generation".into(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: start.elapsed().as_millis() as i64,
            status: "submitted".into(),
            error_message: None,
        },
    ).await;
    if let Ok(id) = usage_id {
        let _ = crate::db::set_usage_task_id(&state.db, id, &task_id).await;
    }

    // Truly async — return the task ID immediately. The client polls
    // poll_url (self-describing: no guessing the task-query path).
    let poll_url = format!("/v1/tasks/{}/{}", tag, task_id);
    let resp = json!({
        "task_id": task_id,
        "task_status": task_status,
        "poll_url": poll_url,
        "tag": tag,
        "code": "Success"
    });
    Ok(Json(resp).into_response())
}

/// Find the first image_url anywhere in messages (for image-to-video first
/// frame). Handles both `{"image_url":"..."}` and `{"image_url":{"url":"..."}}`.
fn find_first_image_url(body: &Value) -> Option<String> {
    let messages = body.get("messages").and_then(|m| m.as_array())?;
    for msg in messages {
        let Some(arr) = msg.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for block in arr {
            let url = block.get("image_url").and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(s.to_string())
                } else {
                    v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string())
                }
            });
            if let Some(u) = url {
                return Some(u);
            }
        }
    }
    None
}

/// Seedance (Volcano Ark) video generation: submit an async task and return the
/// task_id immediately. The client polls GET /v1/tasks/{tag}/{task_id}, which
/// forwards to GET {base}/api/v3/contents/generations/tasks/{id} (see
/// handle_task_poll). The Volcano native poll response is passed through.
///
/// Client body (OpenAI chat):
///   text-to-video:  messages: [{role:user, content:"<prompt>"}]
///   image-to-video: messages: [{role:user, content:[{type:text,text:..},{type:image_url,image_url:{url}}]}]
///   resolution: "480p"|"720p"|"1080p"|"4K"  (default "1080p")
///   ratio:      "16:9"|"9:16"|"1:1"|...      (default "16:9")
///   duration:   4..15 | -1 (smart)           (default 5)
///   generate_audio: bool                      (default true)
async fn handle_seedance_video_request(
    state: AppState,
    caller_key_id: Option<i64>,
    route: &Route,
    provider: &Provider,
    body: &Value,
    start: std::time::Instant,
) -> Result<Response, ProxyError> {
    let prompt = last_user_text(body)
        .ok_or_else(|| ProxyError::Upstream("seedance: no text prompt found in messages".into()))?;
    let resolution = body
        .get("resolution")
        .and_then(|v| v.as_str())
        .unwrap_or("1080p")
        .to_string();
    let ratio = body
        .get("ratio")
        .and_then(|v| v.as_str())
        .unwrap_or("16:9")
        .to_string();
    let duration = body.get("duration").and_then(|v| v.as_i64()).unwrap_or(5);
    let generate_audio = body
        .get("generate_audio")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let url = format!("{}{}", route.base_url.trim_end_matches('/'), route.effective_path());
    log::info!(
        "[Proxy] seedance → {} (model={}, format={:?})",
        provider.name, route.model, route.format
    );
    log::info!("[Proxy] submitting seedance task to {}", url);

    // content: text prompt + optional first-frame image (image-to-video).
    let mut content = vec![json!({ "type": "text", "text": prompt })];
    if let Some(img) = find_first_image_url(body) {
        content.push(json!({ "type": "image_url", "image_url": { "url": img } }));
    }

    let submit_body = json!({
        "model": route.model,
        "content": content,
        "resolution": resolution,
        "ratio": ratio,
        "duration": duration,
        "generate_audio": generate_audio,
    });
    let result = send_image_post(&state, provider, &url, &submit_body, &[]).await?;
    let task_id = result
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| {
            ProxyError::Upstream(format!(
                "No task id in Seedance response: {}",
                truncate_chars(&result.to_string(), 200)
            ))
        })?;
    let tag = route
        .tags
        .first()
        .cloned()
        .unwrap_or_else(|| "seedance".to_string());

    log::info!(
        "[Proxy] seedance task submitted: task_id={} latency={}ms",
        task_id,
        start.elapsed().as_millis()
    );

    let usage_id = crate::db::insert_usage_log(
        &state.db,
        crate::db::UsageInsert {
            caller_key_id,
            tag: tag.clone(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            request_model: route.model.clone(),
            modality: "video_generation".into(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: start.elapsed().as_millis() as i64,
            status: "submitted".into(),
            error_message: None,
        },
    )
    .await;
    if let Ok(id) = usage_id {
        let _ = crate::db::set_usage_task_id(&state.db, id, &task_id).await;
    }

    // Truly async — return the task ID immediately (same shape as the DashScope
    // video response); the client polls poll_url.
    let poll_url = format!("/v1/tasks/{}/{}", tag, task_id);
    let resp = json!({
        "task_id": task_id,
        "task_status": "queued",
        "poll_url": poll_url,
        "tag": tag,
        "code": "Success"
    });
    Ok(Json(resp).into_response())
}

// ─── Pixmax (app.pixmax.cn) video generation ─────────────────────────────────
//
// Pixmax is a video SaaS with no OpenAPI for this account, so we drive the
// browser-facing /user/api with the agent-canvas-v1 protocol (reverse-engineered
// from the official pixmax-cli skill): one shared canvas file holds
// GENERATE_VIDEO nodes; a submit = canvas get (revision) → node/batch create →
// generate/batch; the poll = generate/progress keyed by nodeUuid.
//
// Auth is a 15-day bearer token. provider.api_key carries either the bare
// token, or packed renew credentials "subuser:{account}:{password}:{mainUserUuid}"
// (RSA-encrypted sub-user login renews the token automatically).

/// Pixmax site RSA system public key (X.509 SPKI, base64) — the web app's
/// VITE_PIXMAX_RSA_SYSTEM_PUB, mirrored in pixmax-cli's config.py.
const PIXMAX_RSA_PUB: &str = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDrJMmCeIkfaRK2JCvTVGPxd3kpO1MVanc+89XShiOFAWRcXFIx7X+nC5m5Z0bNM3OnP4Cz/Xl9w//Ib12cE9cjj16Hc3eHsUe8ImX5RQugEhpyb8aHECqqjG83RMEjPiBflr+/RnKiyjX6vTlSRc63moCzMB2zXBntmPdpB2yMhwIDAQAB";

/// Canvas label on every node AginxBrain creates — identifies our nodes for
/// garbage collection on the shared canvas.
const PIXMAX_NODE_LABEL: &str = "AginxBrain";

#[derive(Clone, Debug)]
struct PixmaxSession {
    token: String,
    /// Enterprise sub-account current team — required as a cookie on every
    /// /user/api call (errCode Enterprise.TeamUuid.Required otherwise).
    team_uuid: String,
    /// Canvas file all video nodes live in.
    file_uuid: String,
}

static PIXMAX_SESSIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, PixmaxSession>>> =
    std::sync::OnceLock::new();

fn pixmax_sessions() -> &'static std::sync::Mutex<std::collections::HashMap<String, PixmaxSession>> {
    PIXMAX_SESSIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Resolve the /user/api base from the route's base_url. Accepts both
/// "https://app.pixmax.cn" and "https://app.pixmax.cn/user/api" spellings.
fn pixmax_api_base(route: &Route) -> String {
    let base = route.base_url.trim_end_matches('/');
    let base = base.strip_suffix("/user/api").unwrap_or(base);
    format!("{}/user/api", base)
}

/// provider.api_key is either a bare bearer token or packed renewal
/// credentials. Split into (token, login credentials).
fn pixmax_credentials(provider: &Provider) -> (Option<String>, Option<[String; 3]>) {
    let key = provider.api_key.trim();
    if let Some(rest) = key.strip_prefix("subuser:") {
        let parts: Vec<&str> = rest.splitn(3, ':').collect();
        if parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) {
            return (
                None,
                Some([parts[0].into(), parts[1].into(), parts[2].into()]),
            );
        }
    }
    if key.is_empty() {
        (None, None)
    } else {
        (Some(key.to_string()), None)
    }
}

/// PKCS#1 v1.5-encrypt the password with the site key (JSEncrypt-compatible).
fn pixmax_encrypt_password(password: &str) -> Result<String, ProxyError> {
    use base64::Engine;
    use rsa::pkcs8::DecodePublicKey;
    let der = base64::engine::general_purpose::STANDARD
        .decode(PIXMAX_RSA_PUB)
        .map_err(|e| ProxyError::Upstream(format!("pixmax rsa key decode: {}", e)))?;
    let key = rsa::RsaPublicKey::from_public_key_der(&der)
        .map_err(|e| ProxyError::Upstream(format!("pixmax rsa key parse: {}", e)))?;
    let mut rng = rand::thread_rng();
    let ct = key
        .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, password.as_bytes())
        .map_err(|e| ProxyError::Upstream(format!("pixmax rsa encrypt: {}", e)))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(ct))
}

/// Raw pixmax POST — Bearer token + team cookie. Returns the parsed envelope
/// (business success flag included; callers check it).
async fn pixmax_post_raw(
    state: &AppState,
    provider: &Provider,
    url: &str,
    token: &str,
    team_uuid: &str,
    body: &Value,
) -> Result<Value, ProxyError> {
    let http = crate::config::client_for_provider(state, provider)
        .await
        .map_err(ProxyError::Upstream)?;
    let mut builder = http
        .post(url)
        .header("authorization", format!("Bearer {}", token))
        .header("origin", "https://app.pixmax.cn")
        .header("referer", "https://app.pixmax.cn/");
    if !team_uuid.is_empty() {
        builder = builder.header("cookie", format!("pixmax-current-team-uuid={}", team_uuid));
    }
    let resp = builder
        .json(body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(e.to_string()))?;
    // Pixmax signals business failures (including auth) as JSON envelopes, at
    // any HTTP status — return the envelope so callers can retry on auth races
    // instead of losing the errCode. Only non-JSON bodies stay errors.
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| ProxyError::Upstream(format!("pixmax read body: {}", e)))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.get("success").is_some() || v.get("errCode").is_some() => Ok(v),
        Ok(v) if status.is_success() => Ok(v),
        _ => Err(ProxyError::Upstream(format!(
            "HTTP {}: {}",
            status.as_u16(),
            truncate_chars(&text, 300)
        ))),
    }
}

/// True when the pixmax envelope indicates the bearer token died — re-login
/// and retry once. Token lifetime is ~15 days; errCodes seen/expected carry
/// Login/Auth/Token fragments, and an HTTP 401 body reaches here as text.
fn pixmax_is_auth_error(envelope: &Value) -> bool {
    let code = envelope.get("errCode").and_then(|v| v.as_str()).unwrap_or("");
    let msg = envelope
        .get("errMessage")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let hay = format!("{} {}", code, msg).to_lowercase();
    hay.contains("login") || hay.contains("token") || hay.contains("auth") || hay.contains("401")
}

/// Get the session (token + team + canvas file), logging in and bootstrapping
/// the canvas on first use. Cached per provider name.
async fn pixmax_ensure_session(
    state: &AppState,
    provider: &Provider,
    route: &Route,
) -> Result<PixmaxSession, ProxyError> {
    if let Some(s) = pixmax_sessions().lock().unwrap().get(&provider.name) {
        return Ok(s.clone());
    }
    let base = pixmax_api_base(route);
    let (bare_token, creds) = pixmax_credentials(provider);
    if bare_token.is_none() && creds.is_none() {
        return Err(ProxyError::Upstream(format!(
            "pixmax provider '{}' has no api_key (set a bearer token or subuser:{{account}}:{{password}}:{{mainUserUuid}})",
            provider.name
        )));
    }

    // 1. Token: bare if given, else sub-user RSA login.
    let token = match bare_token {
        Some(t) => t,
        None => {
            let [account, password, main_user_uuid] = creds.unwrap();
            let enc = pixmax_encrypt_password(&password)?;
            let login = pixmax_post_raw(
                state,
                provider,
                &format!("{}/sub-user/login", base),
                "",
                "",
                &json!({
                    "mainUserUuid": main_user_uuid,
                    "subUserAccount": account,
                    "password": enc
                }),
            )
            .await?;
            let token = login
                .get("data")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .ok_or_else(|| {
                    ProxyError::Upstream(format!(
                        "pixmax login failed: {}",
                        truncate_chars(&login.to_string(), 200)
                    ))
                })?;
            // A token minted moments ago can be rejected once before its
            // session propagates — give it a head start (observed seconds).
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            token
        }
    };

    // 2. Team: sub-accounts must pin the current team as a cookie.
    let teams = pixmax_post_raw(
        state,
        provider,
        &format!("{}/enterprise/team/current-sub-user/list", base),
        &token,
        "",
        &json!({}),
    )
    .await?;
    let team_uuid = teams
        .pointer("/data/0/teamUuid")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| ProxyError::Upstream("pixmax: no team found for account".into()))?;

    // 3. Canvas: find or create the dedicated project + file.
    let file_uuid = pixmax_ensure_canvas_file(state, provider, &base, &token, &team_uuid).await?;

    let session = PixmaxSession {
        token,
        team_uuid,
        file_uuid,
    };
    pixmax_sessions()
        .lock()
        .unwrap()
        .insert(provider.name.clone(), session.clone());
    log::info!(
        "[Proxy] pixmax session ready: provider={} file_uuid={}",
        provider.name,
        session.file_uuid
    );
    Ok(session)
}

/// Find or create project "AginxBrain" and its "video" canvas file. Returns
/// the fileUuid.
async fn pixmax_ensure_canvas_file(
    state: &AppState,
    provider: &Provider,
    base: &str,
    token: &str,
    team_uuid: &str,
) -> Result<String, ProxyError> {
    // Project: search by keyword, create when missing.
    let list = pixmax_post_raw(state, provider, &format!("{}/project/list", base), token, team_uuid, &json!({"pageIndex": 1, "pageSize": 20, "keyword": PIXMAX_NODE_LABEL})).await?;
    let project_uuid = (list.pointer("/data/data").and_then(|v| v.as_array()).cloned().unwrap_or_default())
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(PIXMAX_NODE_LABEL))
        .and_then(|p| p.get("uuid").and_then(|u| u.as_str()).map(String::from));
    let project_uuid = match project_uuid {
        Some(u) => u,
        None => {
            let created = pixmax_post_raw(state, provider, &format!("{}/project/createOrUpdate", base), token, team_uuid, &json!({"name": PIXMAX_NODE_LABEL, "projectType": "PERSONAL"})).await?;
            created.get("data").and_then(|v| v.as_str()).map(String::from).ok_or_else(|| {
                ProxyError::Upstream(format!(
                    "pixmax: project create failed: {}",
                    truncate_chars(&created.to_string(), 200)
                ))
            })?
        }
    };

    // File: walk the project tree for a FILE named "video", create when missing.
    let tree = pixmax_post_raw(state, provider, &format!("{}/project/file/tree", base), token, team_uuid, &json!({"projectUuid": project_uuid})).await?;
    fn find_video_file(items: &[Value]) -> Option<String> {
        for item in items {
            if item.get("type").and_then(|t| t.as_str()) == Some("FILE")
                && item.get("name").and_then(|n| n.as_str()) == Some("video")
            {
                if let Some(u) = item.get("uuid").and_then(|u| u.as_str()) {
                    return Some(u.to_string());
                }
            }
            if let Some(children) = item.get("children").and_then(|c| c.as_array()) {
                if let Some(found) = find_video_file(children) {
                    return Some(found);
                }
            }
        }
        None
    }
    let tree_items = tree.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    if let Some(found) = find_video_file(&tree_items) {
        return Ok(found);
    }
    let created = pixmax_post_raw(state, provider, &format!("{}/project/file/create", base), token, team_uuid, &json!({"projectUuid": project_uuid, "name": "video", "type": "FILE"})).await?;
    created.get("data").and_then(|v| v.as_str()).map(String::from).ok_or_else(|| {
        ProxyError::Upstream(format!(
            "pixmax: file create failed: {}",
            truncate_chars(&created.to_string(), 200)
        ))
    })
}

/// Upload a client image to pixmax as a canvas asset (image-to-video first
/// frame). Chain (verified live 2026-11-09): /assets/oss/authorize → signed
/// OSS STS PUT (v1 signature; every x-oss-* header, including the callback,
/// must be in the signed canonical headers or OSS answers
/// SignatureDoesNotMatch) → poll /assets/oss/check until the server-side
/// callback registers the asset. Returns the assetsUuid.
async fn pixmax_upload_image_asset(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    bytes: &[u8],
    mime: &str,
) -> Result<String, ProxyError> {
    let ext = mime.strip_prefix("image/").unwrap_or("png");
    let file_name = format!("aginxbrain-i2v-{}.{}", uuid::Uuid::new_v4(), ext);
    let auth_payload = json!({
        "fileName": file_name,
        "fileSize": bytes.len(),
        "fileType": "IMG",
        "contentType": mime,
    });
    let auth = pixmax_call(state, provider, route, "/assets/oss/authorize", &auth_payload).await?;
    let data = auth.get("data").cloned().unwrap_or(Value::Null);
    let s = |k: &str| data.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (bucket, host, object_key, ak, sk, token) = (
        s("bucketName"),
        s("endpoint"),
        s("objectKey"),
        s("accessKeyId"),
        s("accessKeySecret"),
        s("securityToken"),
    );
    let (cb_url, cb_body, cb_type) = (s("callbackUrl"), s("callbackBody"), s("callbackBodyType"));
    if bucket.is_empty() || object_key.is_empty() || ak.is_empty() || sk.is_empty() || token.is_empty() {
        return Err(ProxyError::Upstream(format!(
            "pixmax: oss authorize incomplete: {}",
            truncate_chars(&data.to_string(), 200)
        )));
    }
    if cb_url.is_empty() || cb_body.is_empty() {
        // Without the callback the PUT succeeds but the asset never registers
        // (check stays AUTHORIZED/callbackReceived:false forever).
        return Err(ProxyError::Upstream("pixmax: oss authorize missing callback fields".into()));
    }

    // OSS signature v1. Canonical headers must include EVERY x-oss-* header
    // being sent, sorted — the callback header included.
    let cb_json = json!({"callbackUrl": cb_url, "callbackBody": cb_body, "callbackBodyType": cb_type});
    let cb_header = base64::engine::general_purpose::STANDARD.encode(cb_json.to_string());
    let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    let canonical = format!("x-oss-callback:{}\nx-oss-security-token:{}\n", cb_header, token);
    // Canonical resource = "/" + bucket + "/" + key. objectKey arrives WITHOUT
    // a leading slash — a missing separator here signs "/bucketkey/…" and OSS
    // answers SignatureDoesNotMatch (verified against its own <StringToSign>).
    let string_to_sign = format!("PUT\n\n{}\n{}\n{}/{}/{}", mime, date, canonical, bucket, object_key);
    use hmac::Mac as _;
    let mut mac = <hmac::Hmac<sha1::Sha1> as hmac::KeyInit>::new_from_slice(sk.as_bytes())
        .map_err(|e| ProxyError::Upstream(format!("pixmax: hmac init: {}", e)))?;
    mac.update(string_to_sign.as_bytes());
    let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

    let upload_url = format!("https://{}.{}{}", bucket, host, {
        // objectKey may need escaping; keys are RES-UUID.ext (safe charset) —
        // escape conservatively except path separators.
        let mut escaped = String::new();
        for ch in object_key.chars() {
            if ch.is_ascii_alphanumeric() || ch == '/' || ch == '.' || ch == '-' || ch == '_' || ch == '~' {
                escaped.push(ch);
            } else {
                escaped.push_str(&format!("%{:02X}", ch as u8));
            }
        }
        if escaped.starts_with('/') { escaped } else { format!("/{}", escaped) }
    });
    let resp = state
        .http_client
        .put(&upload_url)
        .header("Content-Type", mime)
        .header("Date", &date)
        .header("x-oss-security-token", &token)
        .header("x-oss-callback", &cb_header)
        .header("Authorization", format!("OSS {}:{}", ak, signature))
        .timeout(std::time::Duration::from_secs(120))
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(format!("pixmax: oss put: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProxyError::Upstream(format!(
            "pixmax: oss put {}: {}",
            status,
            truncate_chars(&body, 2000)
        )));
    }

    // Poll registration: the OSS callback fires server-side; the asset uuid
    // appears under data.asset.assetsUuid once registered (observed <2s).
    let mut check_payload = auth_payload.clone();
    for k in ["sessionId", "objectKey", "relativePath", "webUrl"] {
        check_payload[k] = data.get(k).cloned().unwrap_or(Value::Null);
    }
    for _ in 0..20 {
        let check = pixmax_call(state, provider, route, "/assets/oss/check", &check_payload).await?;
        let cd = check.get("data").cloned().unwrap_or(Value::Null);
        let asset_uuid = cd
            .pointer("/asset/assetsUuid")
            .or_else(|| cd.pointer("/asset/uuid"))
            .or_else(|| cd.get("assetsUuid"))
            .and_then(|v| v.as_str())
            .map(String::from);
        if let Some(uuid) = asset_uuid {
            log::info!("[Proxy] pixmax asset registered: {} ({} bytes as {})", uuid, bytes.len(), mime);
            return Ok(uuid);
        }
        let status = cd.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status == "FAILED" || status == "ERROR" {
            return Err(ProxyError::Upstream(format!(
                "pixmax: asset registration {}: {}",
                status,
                truncate_chars(&check.to_string(), 200)
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(ProxyError::Upstream("pixmax: asset registration timed out (callback not received)".into()))
}

/// Authenticated pixmax call, resilient to two observed flakiness modes:
/// 1. A token minted seconds ago can be rejected once ("UNAUTHORIZED / No
///    login" or a bare "System Error") before the session propagates — retry
///    the same token after a short backoff.
/// 2. A genuinely expired token (~15 days) — drop the cached session,
///    re-login (when renewal credentials are packed in api_key), retry once.
async fn pixmax_call(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    endpoint: &str,
    body: &Value,
) -> Result<Value, ProxyError> {
    let base = pixmax_api_base(route);
    let session = pixmax_ensure_session(state, provider, route).await?;
    // endpoints arrive with a leading slash; don't double it — pixmax routes
    // "/user/api//x" to an auth catch-all (observed as 401 No login).
    let url = format!("{}/{}", base, endpoint.trim_start_matches('/'));
    let mut result = pixmax_post_raw(state, provider, &url, &session.token, &session.team_uuid, body).await?;

    if !pixmax_succeeded(&result) && pixmax_is_transient(&result) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        result = pixmax_post_raw(state, provider, &url, &session.token, &session.team_uuid, body).await?;
    }
    if !pixmax_succeeded(&result) && pixmax_is_auth_error(&result) {
        pixmax_sessions().lock().unwrap().remove(&provider.name);
        let session = pixmax_ensure_session(state, provider, route).await?;
        result = pixmax_post_raw(state, provider, &url, &session.token, &session.team_uuid, body).await?;
    }

    if !pixmax_succeeded(&result) {
        return Err(ProxyError::Upstream(format!(
            "pixmax {}: {}",
            result.get("errCode").and_then(|v| v.as_str()).unwrap_or("?"),
            result
                .get("errMessage")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
        )));
    }
    Ok(result)
}

fn pixmax_succeeded(envelope: &Value) -> bool {
    envelope.get("success").and_then(|v| v.as_bool()) == Some(true)
}

/// Auth failures plus bare "System Error" 500s — both observed as one-shot
/// rejections right after a fresh login that clear on retry.
fn pixmax_is_transient(envelope: &Value) -> bool {
    if pixmax_is_auth_error(envelope) {
        return true;
    }
    envelope.get("errCode").and_then(|v| v.as_str()) == Some("500")
}

/// Canvas model catalog, cached per provider for 1h. (provider name → fetched-at, data)
static PIXMAX_CATALOGS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Value)>>,
> = std::sync::OnceLock::new();

fn pixmax_catalogs(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Value)>> {
    PIXMAX_CATALOGS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Param specs (name → (default, allowed option values)) for route.model from
/// the pixmax model catalog. Models vary wildly — PIXDANCE_2_5_DRAFT has no
/// 720P, KLING_4 has no adaptive ratio and no includeAudio — so client params
/// are validated against this. Returns an empty map when the catalog can't be
/// fetched (callers then fall back to conservative hardcoded defaults).
async fn pixmax_model_params(
    state: &AppState,
    provider: &Provider,
    route: &Route,
) -> std::collections::HashMap<String, (String, Vec<String>)> {
    let fresh = {
        let cache = pixmax_catalogs().lock().unwrap();
        cache
            .get(&provider.name)
            .map(|(t, _)| t.elapsed().as_secs() < 3600)
            .unwrap_or(false)
    };
    if !fresh {
        match pixmax_call(state, provider, route, "/canvas/modelConfig", &json!({})).await {
            Ok(envelope) => {
                let data = envelope.get("data").cloned().unwrap_or(Value::Null);
                pixmax_catalogs()
                    .lock()
                    .unwrap()
                    .insert(provider.name.clone(), (std::time::Instant::now(), data));
            }
            Err(e) => log::warn!(
                "[Proxy] pixmax modelConfig fetch failed ({}), using fallback defaults",
                e
            ),
        }
    }
    let catalog = {
        let cache = pixmax_catalogs().lock().unwrap();
        cache
            .get(&provider.name)
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Null)
    };

    let mut out = std::collections::HashMap::new();
    let Some(groups) = catalog.as_array() else {
        return out;
    };
    'outer: for group in groups {
        let Some(models) = group.get("nodeConfig").and_then(|v| v.as_array()) else {
            continue;
        };
        for m in models {
            let matches = m.get("modelCode").and_then(|v| v.as_str()) == Some(route.model.as_str())
                || m.get("modelCodeAlias").and_then(|v| v.as_str()) == Some(route.model.as_str());
            if !matches {
                continue;
            }
            if let Some(params) = m.pointer("/modelConfig/params").and_then(|v| v.as_array()) {
                for p in params {
                    let Some(name) = p.get("name").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let default = p
                        .get("default")
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    let options = p
                        .get("options")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|o| o.get("value").and_then(|v| v.as_str()))
                                .map(|s| s.to_string())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    out.insert(name.to_string(), (default, options));
                }
            }
            break 'outer;
        }
    }
    out
}

/// Resolve one client param against the catalog spec: keep the client value
/// when the model allows it (or when there is no spec), else snap to the
/// model default instead of failing the generation with "parameter illegal".
fn pixmax_resolve_param(
    specs: &std::collections::HashMap<String, (String, Vec<String>)>,
    name: &str,
    client: Option<String>,
    fallback: &str,
) -> String {
    let Some((default, options)) = specs.get(name) else {
        return client.unwrap_or_else(|| fallback.to_string());
    };
    if let Some(v) = client {
        if options.is_empty() || options.contains(&v) {
            return v;
        }
        log::warn!(
            "[Proxy] pixmax {} '{}': not supported by this model (options {:?}), using default '{}'",
            name,
            v,
            options,
            default
        );
        return default.clone();
    }
    if default.is_empty() {
        return fallback.to_string();
    }
    default.clone()
}

/// Client body (OpenAI chat, mirroring the seedance contract):
///   messages: [{role:user, content:"<prompt>"}]
///   resolution: "480p"|"720p"|"1080p"   (default "720p")
///   ratio:      "16:9"|"9:16"|...|"auto" (default "auto" → pixmax "adaptive")
///   duration:   seconds                   (default 5)
///   generate_audio: bool                  (default true)
///   n / count:  1|2|4                     (default 1)
/// route.model is the pixmax modelCode (PIXDANCE_2_5, KLING_4, HAILUO_23, ...).
/// Image-to-video is not supported (needs the asset upload + BASE_IMAGE node
/// chain); v1 is text-to-video only.
///
/// Submit = canvas get → node/batch create → generate/batch. task_id returned
/// to the client is the nodeUuid (generate/progress is keyed by node).
async fn handle_pixmax_video_request(
    state: AppState,
    caller_key_id: Option<i64>,
    route: &Route,
    provider: &Provider,
    body: &Value,
    start: std::time::Instant,
) -> Result<Response, ProxyError> {
    let prompt = last_user_text(body)
        .ok_or_else(|| ProxyError::Upstream("pixmax: no text prompt found in messages".into()))?;
    // First image_url in messages = image-to-video first frame (uploaded to
    // pixmax as a canvas asset below); no image = text-to-video.
    let image_ref = find_first_image_url(body);
    let tag = route.tags.first().cloned().unwrap_or_else(|| "pixmax".to_string());

    // pixmax wants every param as a string. Defaults and allowed values come
    // from the per-model catalog (draft models have no 720P; kling has no
    // adaptive ratio and no audio flag), so illegal client values snap to the
    // model default instead of failing the generation with "parameter illegal".
    let specs = pixmax_model_params(&state, provider, route).await;
    let resolution = pixmax_resolve_param(
        &specs,
        "resolution",
        body.get("resolution")
            .and_then(|v| v.as_str())
            .map(|s| s.to_uppercase()),
        "720P",
    );
    // "auto" ratio → pixmax "adaptive" where the model supports it (pixdance);
    // models without adaptive (kling) fall back to their catalog default.
    let aspect_ratio = match body.get("ratio").and_then(|v| v.as_str()) {
        Some("") | None => {
            let has_adaptive = specs
                .get("aspectRatio")
                .map(|(_, options)| options.iter().any(|o| o == "adaptive"))
                .unwrap_or(true);
            if has_adaptive {
                "adaptive".to_string()
            } else {
                pixmax_resolve_param(&specs, "aspectRatio", None, "adaptive")
            }
        }
        Some(other) => pixmax_resolve_param(&specs, "aspectRatio", Some(other.to_string()), "adaptive"),
    };
    let duration = pixmax_resolve_param(
        &specs,
        "duration",
        body.get("duration").and_then(|v| v.as_u64()).map(|d| d.to_string()),
        "5",
    );
    let count = pixmax_resolve_param(
        &specs,
        "count",
        body.get("count")
            .or_else(|| body.get("n"))
            .and_then(|v| v.as_u64())
            .map(|c| c.to_string()),
        "1",
    );
    let include_audio = pixmax_resolve_param(
        &specs,
        "includeAudio",
        body.get("generate_audio").map(|v| v.as_bool().unwrap_or(true).to_string()),
        "true",
    );
    // kling-style models have no includeAudio param at all — omit rather than
    // risk a rejection (specs empty = catalog unavailable, keep sending it).
    let send_include_audio = specs.is_empty() || specs.contains_key("includeAudio");

    log::info!(
        "[Proxy] pixmax → {} (model={}, prompt {} chars)",
        provider.name,
        route.model,
        prompt.len()
    );

    let session = pixmax_ensure_session(&state, provider, route).await?;

    // Image-to-video: upload the reference as a canvas asset, then carry a
    // BASE_IMAGE node upstream of the video node (canvas edge) plus
    // referModel=imageToVideo — the CLI's `video image-to-video` shape.
    let mut image_node: Option<Value> = None;
    if let Some(image_ref) = &image_ref {
        let (bytes, mime) = image_ref_to_bytes(&state, image_ref).await?;
        log::info!(
            "[Proxy] pixmax i2v: uploading first frame ({} bytes, {})",
            bytes.len(),
            mime
        );
        let asset_uuid = pixmax_upload_image_asset(&state, provider, route, &bytes, &mime).await?;
        let img_uuid = uuid::Uuid::new_v4().to_string();
        image_node = Some(json!({
            "uuid": img_uuid,
            "fileUuid": session.file_uuid,
            "type": "BASE_IMAGE",
            "isGenerateNode": false,
            "metaData": json!({
                "data": {"label": PIXMAX_NODE_LABEL, "assetUuid": asset_uuid},
                "position": {"x": -400, "y": 0}
            }).to_string(),
            "defaultAssetUuid": asset_uuid,
            "params": {},
            "prevNodeUuids": []
        }));
    }

    // Canvas read: revision for the optimistic-concurrency batch, plus the
    // node list for occasional garbage collection of our old nodes.
    let canvas = pixmax_call(
        &state,
        provider,
        route,
        "/canvas/get",
        &json!({"fileUuid": session.file_uuid}),
    )
    .await?;
    let canvas_data = canvas.get("data").cloned().unwrap_or(Value::Null);
    let mut revision = canvas_data
        .get("revision")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Non-fatal GC: above 200 of our nodes, delete all but the newest 50 in a
    // separate batch (a failure here must never block a generation).
    let nodes = canvas_data
        .get("nodes")
        .and_then(|n| n.as_array())
        .cloned()
        .unwrap_or_default();
    pixmax_gc_nodes(&state, provider, route, &session, &nodes).await;

    // Create the GENERATE_VIDEO node. Retry on revision conflict /
    // duplicate-submission (5s per-file dedup lock) with fresh revision,
    // mirroring the CLI's ≤3 attempts.
    let node_uuid = uuid::Uuid::new_v4().to_string();
    let img_node_uuid = image_node
        .as_ref()
        .and_then(|n| n.get("uuid"))
        .and_then(|u| u.as_str())
        .map(String::from);
    let meta = match &img_node_uuid {
        Some(img_uuid) => json!({
            "data": {
                "label": PIXMAX_NODE_LABEL,
                "workflow": "image-to-video",
                "referenceNodeUuid": img_uuid
            },
            "position": {"x": 0, "y": 0}
        }),
        None => json!({
            "data": {"label": PIXMAX_NODE_LABEL, "workflow": "text-to-video"},
            "position": {"x": 0, "y": 0}
        }),
    }
    .to_string();
    let mut node = json!({
        "uuid": node_uuid,
        "fileUuid": session.file_uuid,
        "type": "GENERATE_VIDEO",
        "isGenerateNode": true,
        "metaData": meta,
        "defaultAssetUuid": "",
        "params": {
            "model": route.model,
            "prompt": prompt,
            "resolution": resolution,
            "aspectRatio": aspect_ratio,
            "duration": duration,
            "referModel": if img_node_uuid.is_some() { "imageToVideo" } else { "textToVideo" },
            "count": count
        },
        "prevNodeUuids": match &img_node_uuid { Some(u) => json!([u]), None => json!([]) }
    });
    if send_include_audio {
        node["params"]["includeAudio"] = json!(include_audio);
    }
    // i2v: the image node rides in the same create batch, upstream of the
    // video node (canvas edge = prevNodeUuids reference).
    let mut create_list: Vec<Value> = Vec::new();
    if let Some(img) = image_node.clone() {
        create_list.push(img);
    }
    create_list.push(node.clone());
    let mut created = Err(ProxyError::Upstream("pixmax: node create not attempted".into()));
    for attempt in 0..3 {
        let batch = json!({
            "fileUuid": session.file_uuid,
            "baseRevision": revision,
            "create": create_list,
            "update": [],
            "delete": []
        });
        created = pixmax_call(&state, provider, route, "/canvas/node/batch", &batch).await;
        match &created {
            Ok(_) => break,
            Err(e) if attempt < 2 && pixmax_is_batch_retryable(e) => {
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                let canvas = pixmax_call(
                    &state,
                    provider,
                    route,
                    "/canvas/get",
                    &json!({"fileUuid": session.file_uuid}),
                )
                .await?;
                revision = canvas
                    .pointer("/data/revision")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            Err(_) => break,
        }
    }
    created?;

    // Submit the generation task.
    let gen = pixmax_call(
        &state,
        provider,
        route,
        "/generate/batch",
        &json!({"fileUuid": session.file_uuid, "nodeUuids": [node_uuid], "nodes": [node_uuid]}),
    )
    .await?;
    let task_status = gen
        .pointer("/data/0/status")
        .and_then(|v| v.as_str())
        .unwrap_or("QUEUE")
        .to_lowercase();

    log::info!(
        "[Proxy] pixmax task submitted: task_id={} status={} latency={}ms",
        node_uuid,
        task_status,
        start.elapsed().as_millis()
    );

    let usage_id = crate::db::insert_usage_log(
        &state.db,
        crate::db::UsageInsert {
            caller_key_id,
            tag: tag.clone(),
            provider: provider.name.clone(),
            model: route.model.clone(),
            request_model: route.model.clone(),
            modality: "video_generation".into(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: start.elapsed().as_millis() as i64,
            status: "submitted".into(),
            error_message: None,
        },
    )
    .await;
    if let Ok(id) = usage_id {
        let _ = crate::db::set_usage_task_id(&state.db, id, &node_uuid).await;
    }

    let poll_url = format!("/v1/tasks/{}/{}", tag, node_uuid);
    let resp = json!({
        "task_id": node_uuid,
        "task_status": task_status,
        "poll_url": poll_url,
        "tag": tag,
        "code": "Success"
    });
    Ok(Json(resp).into_response())
}

/// Batch-write rejections that are safe to retry with the same payload:
/// revision CAS conflict and the per-file 5s duplicate-submission lock.
fn pixmax_is_batch_retryable(e: &ProxyError) -> bool {
    let msg = e.to_string();
    msg.contains("Canvas.Revision.Conflict") || msg.contains("DUPLICATE_SUBMISSION")
}

/// Delete our oldest canvas nodes once they pile up, keeping the newest 50.
/// Runs only past a 200-node threshold, as its own batch call so a GC failure
/// never blocks the generation path. Errors are logged and swallowed.
async fn pixmax_gc_nodes(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    session: &PixmaxSession,
    nodes: &[Value],
) {
    let mut ours: Vec<&Value> = nodes.iter().filter(|n| pixmax_is_our_node(n)).collect();
    if ours.len() < 200 {
        return;
    }
    // Newest last (updateTime epoch string) — drop all but the newest 50.
    ours.sort_by_key(|n| {
        n.get("updateTime")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
    });
    let doomed: Vec<String> = ours[..ours.len() - 50]
        .iter()
        .filter_map(|n| n.get("uuid").and_then(|u| u.as_str()).map(String::from))
        .collect();

    let canvas = match pixmax_call(
        state,
        provider,
        route,
        "/canvas/get",
        &json!({"fileUuid": session.file_uuid}),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[Proxy] pixmax gc: canvas read failed (skipping): {}", e);
            return;
        }
    };
    let revision = match canvas.pointer("/data/revision").and_then(|v| v.as_str()) {
        Some(r) => r.to_string(),
        None => {
            log::warn!("[Proxy] pixmax gc: no revision, skipping");
            return;
        }
    };
    let batch = json!({
        "fileUuid": session.file_uuid,
        "baseRevision": revision,
        "create": [],
        "update": [],
        "delete": doomed
    });
    match pixmax_call(state, provider, route, "/canvas/node/batch", &batch).await {
        Ok(_) => log::info!("[Proxy] pixmax gc: trimmed old nodes"),
        Err(e) => log::warn!("[Proxy] pixmax gc failed (ignored): {}", e),
    }
}

/// A node is ours when its metaData label is the AginxBrain label. metaData is
/// a JSON string on the wire; tolerate unparsable values (never GC those).
fn pixmax_is_our_node(node: &Value) -> bool {
    let meta = node.get("metaData").and_then(|m| m.as_str()).unwrap_or("");
    serde_json::from_str::<Value>(meta)
        .ok()
        .and_then(|m| m.pointer("/data/label").cloned())
        .and_then(|l| l.as_str().map(String::from))
        .map(|l| l == PIXMAX_NODE_LABEL)
        .unwrap_or(false)
}

/// Absolutize a pixmax asset URL: results carry a relative webUrl plus the
/// OSS domain it lives on ("/static/video/..." + "https://pixmax-prod.oss...").
fn pixmax_absolutize(asset: &mut Value) {
    let domain = asset
        .get("ossDomain")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    if domain.is_empty() {
        return;
    }
    for key in ["webUrl", "previewWebUrl", "thumbnailWebUrl", "thumbnailVideoWebUrl"] {
        let url = asset
            .get(key)
            .and_then(|u| u.as_str())
            .map(String::from)
            .filter(|u| !u.starts_with("http://") && !u.starts_with("https://"));
        if let Some(url) = url {
            if let Some(obj) = asset.as_object_mut() {
                obj.insert(
                    key.to_string(),
                    Value::String(format!("{}/{}", domain, url.trim_start_matches('/'))),
                );
            }
        }
    }
}

/// Poll a pixmax video task: POST /generate/progress {nodeUuids:[task_id]}.
/// Returns the task item with absolutized result asset URLs, flattened to the
/// top level (status / progress / resultAssets / providerErrorMsg).
pub async fn pixmax_poll_task(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    task_id: &str,
) -> Result<Value, ProxyError> {
    let result = pixmax_call(
        state,
        provider,
        route,
        "/generate/progress",
        &json!({"nodeUuids": [task_id]}),
    )
    .await?;
    let mut item = result
        .pointer("/data/0")
        .cloned()
        .ok_or_else(|| ProxyError::Upstream("pixmax: no task in progress response".into()))?;
    if let Some(assets) = item.get_mut("resultAssets").and_then(|a| a.as_array_mut()) {
        for asset in assets.iter_mut() {
            pixmax_absolutize(asset);
        }
    }
    // Map pixmax statuses onto the shared async-task vocabulary so the usage
    // log rows line up with wanx/seedance history.
    let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let mapped = match status {
        "COMPLETE" => "succeeded",
        "FAILED" => "failed",
        "ABORTED" => "cancelled",
        _ => "",
    };
    if !mapped.is_empty() {
        let _ = crate::db::update_video_usage_by_task_id(&state.db, task_id, Some(mapped.into()), None, None).await;
    }
    // Zombie-dispatch detection: pixmax occasionally accepts a task that its
    // scheduler never picks up — the record is BORN at RUNNING/99 and never
    // moves (observed 2026-11-09: two tasks wedged >1h, generate/cancel also
    // refused them). Healthy tasks climb 2%→…→100 and only pass 99 briefly,
    // and even their `updateTime` stays frozen, so age-at-99 is the only
    // reliable signature. After PIXMAX_STUCK_SECS at 99 with zero completions,
    // report failure so callers stop polling and resubmit instead of waiting
    // forever.
    const PIXMAX_STUCK_SECS: i64 = 900;
    let progress = item.get("progress").and_then(|p| p.as_i64()).unwrap_or(0);
    let completed = item.get("completedCount").and_then(|c| c.as_i64()).unwrap_or(0);
    let created = item
        .get("createTime")
        .and_then(|c| c.as_i64())
        .or_else(|| item.get("createTime").and_then(|c| c.as_str()).and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if status == "RUNNING" && progress >= 99 && completed == 0 && created > 0 && now - created > PIXMAX_STUCK_SECS {
        log::warn!(
            "[Proxy] pixmax task {} wedged (RUNNING/99 since creation, {}s old, cancel-refused upstream) — reporting failed",
            task_id,
            now - created
        );
        if let Some(obj) = item.as_object_mut() {
            obj.insert("status".into(), Value::String("FAILED".into()));
            obj.insert(
                "providerErrorMsg".into(),
                Value::String(
                    "pixmax task wedged: accepted but never dispatched (stuck at 99% >15min); \
                     please resubmit"
                        .into(),
                ),
            );
            obj.insert("stuck".into(), Value::Bool(true));
        }
        let _ = crate::db::update_video_usage_by_task_id(&state.db, task_id, Some("failed".into()), None, None).await;
    }
    if let Some(obj) = item.as_object_mut() {
        obj.insert("task_id".into(), Value::String(task_id.to_string()));
        obj.insert("code".into(), Value::String("Success".into()));
    }
    Ok(item)
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

    // Save to {brain_home}/audio/{id}.mp3
    let audio_dir = match crate::config::brain_home() {
        Ok(h) => h.join("audio"),
        Err(e) => return Err(ProxyError::Upstream(format!("no brain home for audio storage: {e}"))),
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
    let http = crate::config::client_for_provider(state, provider)
        .await
        .map_err(ProxyError::Upstream)?;
    let mut builder = http
        .post(url)
        .header(provider.auth_type.header_name(), provider.auth_type.header_value(&provider.api_key));
    for (k, v) in extra_headers {
        builder = builder.header(*k, *v);
    }
    let resp = builder
        .json(body)
        // Image generation can be slow: high-quality models like Seedream 5.0
        // pro spend 60-70s per image. 45s cut it off and 502'd. This is a
        // ceiling (fast providers like wanx still return quickly), also used
        // for async video task submission which returns in <1s.
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(e.to_string()))?;
    parse_image_response(resp).await
}

async fn send_image_get(state: &AppState, provider: &Provider, url: &str) -> Result<Value, ProxyError> {
    let http = crate::config::client_for_provider(state, provider)
        .await
        .map_err(ProxyError::Upstream)?;
    let resp = http.get(url)
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
    // Reference image present → /v1/images/edits. generations has no image
    // parameter and gpt-image-* rejects unknown fields, so sending the
    // reference there 400s ("upstream rejects the reference image").
    if let Some(image_ref) = req.extra.get("image").and_then(|v| v.as_str()) {
        let edits_url = match url.strip_suffix("/generations") {
            Some(base) => format!("{}/edits", base),
            None => format!("{}{}", url.trim_end_matches('/'), "/edits"),
        };
        return generate_openai_image_edit(state, provider, route, &edits_url, prompt, req, image_ref).await;
    }
    let mut body = json!({
        "model": route.model,
        "prompt": prompt,
        "n": req.n.max(1),
        // No "response_format": dall-e-2/3 default to url anyway, while
        // gpt-image-* only return b64_json and reject the parameter outright
        // (HTTP 400 unknown_parameter). parse_openai_images handles both shapes.
    });
    if let Some(size) = &req.size {
        body["size"] = Value::String(size.clone());
    }
    merge_extra(&mut body, &req.extra, &["model", "prompt"]);
    let result = send_image_post(state, provider, url, &body, &[]).await?;
    parse_openai_images(&result).ok_or_else(|| ProxyError::Upstream("No images in OpenAI Images response".to_string()))
}

/// Image editing / image-to-image: POST /v1/images/edits with the reference
/// image as a multipart file part (data URLs decode directly; http(s) URLs are
/// fetched with the shared client — reference URLs are client-side and
/// anonymous, like inline_anthropic_image_urls). gpt-image-* replies with
/// b64_json, same shape as generations. `edits_url` is the final endpoint —
/// openai_images auto-switch derives it from its generations URL, explicit
/// openai_image_edit routes pass route.effective_path() as-is.
/// Resolve a client-supplied image reference (http(s) URL or data:base64) to
/// raw bytes + mime. Data URLs decode locally; remote URLs ride the shared
/// client's fetch path (30s timeout, 10MB cap). Shared by OpenAI image edits
/// and pixmax image-to-video first frames.
async fn image_ref_to_bytes(state: &AppState, image_ref: &str) -> Result<(Vec<u8>, String), ProxyError> {
    if let Some(rest) = image_ref.strip_prefix("data:") {
        let (meta, data) = rest
            .split_once(',')
            .ok_or_else(|| ProxyError::Upstream("image: malformed data URL reference".into()))?;
        let mime = meta.split(';').next().unwrap_or("image/png").to_string();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| ProxyError::Upstream(format!("image: invalid base64 reference: {}", e)))?;
        Ok((bytes, mime))
    } else {
        let (mime, b64) = fetch_image_to_base64(&state.http_client, image_ref)
            .await
            .ok_or_else(|| ProxyError::Upstream(format!("image: failed to fetch reference image {}", image_ref)))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .map_err(|e| ProxyError::Upstream(format!("image: reference decode: {}", e)))?;
        Ok((bytes, mime))
    }
}

async fn generate_openai_image_edit(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    edits_url: &str,
    prompt: &str,
    req: &GenerateImageRequest,
    image_ref: &str,
) -> Result<Vec<GeneratedImage>, ProxyError> {
    let (bytes, mime) = image_ref_to_bytes(state, image_ref).await?;
    let ext = mime.strip_prefix("image/").unwrap_or("png");
    log::info!(
        "[Proxy] image edit → {} (model={}, {} byte reference as {})",
        edits_url,
        route.model,
        bytes.len(),
        mime
    );

    let mut form = reqwest::multipart::Form::new()
        .text("model", route.model.clone())
        .text("prompt", prompt.to_string())
        .text("n", req.n.max(1).to_string());
    if let Some(size) = &req.size {
        form = form.text("size", size.clone());
    }
    // Pass through client extras (quality / background / input_fidelity / …)
    // as form fields; "image" itself is the file part below.
    for (k, v) in &req.extra {
        if k == "image" {
            continue;
        }
        let text = match v {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(num) => num.to_string(),
            _ => continue,
        };
        form = form.text(k.clone(), text);
    }
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(format!("reference.{}", ext))
        .mime_str(&mime)
        .map_err(|e| ProxyError::Upstream(format!("image: bad reference mime {}: {}", mime, e)))?;
    form = form.part("image", part);

    let http = crate::config::client_for_provider(state, provider)
        .await
        .map_err(ProxyError::Upstream)?;
    let resp = http
        .post(edits_url)
        .header(provider.auth_type.header_name(), provider.auth_type.header_value(&provider.api_key))
        .timeout(std::time::Duration::from_secs(180))
        .multipart(form)
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(e.to_string()))?;
    let result = parse_image_response(resp).await?;
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
    // image-generation/generation requires async mode (sync returns 403).
    // Returns task_id → poll_dashscope_image_task resolves it.
    let result = send_image_post(state, provider, url, &body, &[("X-DashScope-Async", "enable")]).await?;
    if let Some(images) = parse_dashscope_images(&result) {
        if !images.is_empty() {
            return Ok(images);
        }
    }

    if let Some(task_id) = result.pointer("/output/task_id").and_then(|v| v.as_str()) {
        // Poll against the provider origin (route.base_url), NOT the full POST
        // url — otherwise the generation path gets prepended to /api/v1/tasks/{id}.
        return poll_dashscope_image_task(state, provider, task_id, &route.base_url).await;
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

/// Image-route variant of the admin Test request: run one real, minimal
/// image generation through the same dispatch the proxy uses (submit + poll
/// for async providers) and report a compact summary instead of raw bytes.
async fn test_image_route(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    tag: &str,
) -> TestResult {
    let url = format!("{}{}", route.base_url.trim_end_matches('/'), route.effective_path());
    log::info!("[Test] testing image route tag={} → {} {} (format={:?})", tag, url, route.model, route.format);

    let prompt = "a small solid red circle on a white background";
    let req = GenerateImageRequest {
        tag: Some(tag.to_string()),
        prompt: prompt.to_string(),
        size: None, // provider default size — keep the test footprint minimal
        n: 1,
        extra: std::collections::HashMap::new(),
    };

    let start = std::time::Instant::now();
    let result = match &route.format {
        ProviderFormat::OpenaiImages => generate_openai_images(state, provider, route, &url, prompt, &req).await,
        ProviderFormat::OpenaiImageEdit => {
            // Edits need a reference image; embed a tiny 8x8 red PNG so the
            // Test button exercises the real multipart /edits pipeline.
            const TINY_REF_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEklEQVR4nGP4z8CAFWEXHbQSACj/P8Fu7N9hAAAAAElFTkSuQmCC";
            let mut extra = std::collections::HashMap::new();
            extra.insert("image".to_string(), Value::String(TINY_REF_PNG.to_string()));
            let edit_req = GenerateImageRequest { extra, ..req.clone() };
            generate_openai_image_edit(state, provider, route, &url, prompt, &edit_req, TINY_REF_PNG).await
        }
        ProviderFormat::DashscopeImage => generate_dashscope_image(state, provider, route, &url, prompt, &req).await,
        ProviderFormat::DashscopeChatImage => generate_dashscope_chat_image(state, provider, route, &url, prompt, &req).await,
        ProviderFormat::MinimaxImage => generate_minimax_image(state, provider, route, &url, prompt, &req).await,
        other => Err(ProxyError::Upstream(format!("format {:?} is not an image format", other))),
    };
    let latency_ms = start.elapsed().as_millis() as u64;

    let base = TestResult {
        success: false,
        tag: tag.to_string(),
        provider: provider.name.clone(),
        model: route.model.clone(),
        format: format!("{:?}", route.format),
        latency_ms,
        error: None,
        response: None,
    };
    match result {
        Ok(images) if !images.is_empty() => TestResult {
            success: true,
            response: Some(json!({
                "images": images.len(),
                "first": if let Some(u) = &images[0].url {
                    json!({"kind": "url", "url": truncate_chars(u, 120)})
                } else {
                    json!({"kind": "b64_png", "bytes": images[0].base64.len()})
                }
            })),
            ..base
        },
        Ok(_) => TestResult {
            error: Some("provider returned no images".to_string()),
            ..base
        },
        Err(e) => TestResult {
            error: Some(format!("{}", e)),
            ..base
        },
    }
}

/// Pixmax-route variant of the admin Test request: bootstrap the full session
/// (login → team → canvas) and read the credit balance. Read-only — no node,
/// no generation, no credits spent.
async fn test_pixmax_route(
    state: &AppState,
    provider: &Provider,
    route: &Route,
    tag: &str,
) -> TestResult {
    let start = std::time::Instant::now();
    let base = TestResult {
        success: false,
        tag: tag.to_string(),
        provider: provider.name.clone(),
        model: route.model.clone(),
        format: "PixmaxVideo".to_string(),
        latency_ms: 0,
        error: None,
        response: None,
    };
    let session = match pixmax_ensure_session(state, provider, route).await {
        Ok(s) => s,
        Err(e) => {
            return TestResult { error: Some(format!("{}", e)), latency_ms: start.elapsed().as_millis() as u64, ..base }
        }
    };
    // GET /credit/balance (this endpoint rejects POST with a System Error).
    let balance = async {
        let http = crate::config::client_for_provider(state, provider)
            .await
            .map_err(ProxyError::Upstream)?;
        let resp = http
            .get(format!("{}/credit/balance", pixmax_api_base(route)))
            .header("authorization", format!("Bearer {}", session.token))
            .header("cookie", format!("pixmax-current-team-uuid={}", session.team_uuid))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ProxyError::Upstream(e.to_string()))?;
        parse_image_response(resp).await
    }
    .await;
    let latency_ms = start.elapsed().as_millis() as u64;
    match balance {
        Ok(b) if b.get("success").and_then(|v| v.as_bool()) == Some(true) => TestResult {
            success: true,
            latency_ms,
            response: Some(json!({
                "session": "ok",
                "file_uuid": truncate_chars(&session.file_uuid, 40),
                "quota_balance": b.pointer("/data/quotaBalance"),
                "recharge_balance": b.pointer("/data/rechargeCreditBalance"),
            })),
            ..base
        },
        Ok(b) => TestResult {
            error: Some(format!(
                "session ok, credit check failed: {}",
                truncate_chars(&b.to_string(), 200)
            )),
            latency_ms,
            ..base
        },
        Err(e) => TestResult {
            error: Some(format!("session ok, credit check failed: {}", e)),
            latency_ms,
            ..base
        },
    }
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

    // Image formats go through the real generation pipeline (one minimal
    // image request, submit + poll for async providers) instead of the chat
    // path below.
    if is_image_format(&route.format) {
        return test_image_route(state, provider, route, tag).await;
    }

    // Pixmax: no cheap chat request exists — verify the session end-to-end
    // (login + team + canvas bootstrap) and report the credit balance.
    if matches!(route.format, ProviderFormat::PixmaxVideo) {
        return test_pixmax_route(state, provider, route, tag).await;
    }

    if !is_chat_format(&route.format) {
        return TestResult {
            success: false,
            tag: tag.to_string(),
            provider: provider.name.clone(),
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

    // Construct a minimal Anthropic Messages request
    let anthropic_body = json!({
        "model": tag,
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": prompt}
        ]
    });

    // Convert using the SAME code as the live proxy path (build_fwd_body is
    // the single source of truth — the template above is an Anthropic
    // Messages request, so client_protocol is "anthropic").
    let fwd_body = match build_fwd_body("anthropic", &route.format, &anthropic_body, route) {
        Ok(b) => b,
        Err(e) => {
            return TestResult {
                success: false,
                tag: tag.to_string(),
                provider: provider.name.clone(),
                model: route.model.clone(),
                format: format!("{:?}", route.format),
                latency_ms: 0,
                error: Some(format!("route format not supported by the chat test endpoint: {}", e)),
                response: None,
            };
        }
    };

    // Build URL
    let url = format!(
        "{}{}",
        route.base_url.trim_end_matches('/'),
        route.effective_path()
    );
    log::info!("[Test] testing route tag={} → {} {} (format={:?})", tag, url, route.model, route.format);

    if let Ok(s) = serde_json::to_string(&fwd_body) {
        let truncated = if s.chars().count() > 500 { format!("{}...(truncated)", truncate_chars(&s, 500)) } else { s };
        log::info!("[Test] forwarding body: {}", truncated);
    }

    // Send request (per-provider outbound proxy: resolve client first so a bad
    // proxy URL shows up in the admin UI Test result instead of a generic error)
    let start = std::time::Instant::now();
    let http = match crate::config::client_for_provider(state, provider).await {
        Ok(c) => c,
        Err(e) => {
            return TestResult {
                success: false,
                tag: tag.to_string(),
                provider: provider.name.clone(),
                model: route.model.clone(),
                format: format!("{:?}", route.format),
                latency_ms: 0,
                error: Some(e),
                response: None,
            };
        }
    };
    let resp = match http
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
    fn test_guess_image_mime_strips_query() {
        // Query string must not poison the extension lookup (file.yinnho.cn
        // vision URLs carry ?sender_id=...%40im.wechat).
        assert_eq!(
            guess_image_mime("https://file.yinnho.cn/api/files/view/ai-writer/input/image_1785.jpeg?sender_id=o9cq%40im.wechat"),
            "image/jpeg"
        );
        assert_eq!(guess_image_mime("https://x/a/b/c.PNG"), "image/png");
        assert_eq!(guess_image_mime("https://x/pic.webp#frag"), "image/webp");
        // No extension -> jpeg default.
        assert_eq!(guess_image_mime("https://x/avatar"), "image/jpeg");
    }

    #[test]
    fn test_extract_usage_anthropic_tail_message_delta() {
        // Zhipu/kimi put ALL real usage in the stream-tail message_delta
        // (message_start carries input_tokens: 0 or nothing useful).
        let head = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hel\"}}\n\n";
        let tail = b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":45230,\"output_tokens\":12135,\"cache_read_input_tokens\":900}}\n\n";
        let (input, output) =
            extract_usage_from_sse_buffer(head, Some(tail), &ProviderFormat::Anthropic);
        assert_eq!(input, Some(45230 + 900));
        assert_eq!(output, Some(12135));
    }

    #[test]
    fn test_extract_usage_uncapped_single_buffer() {
        // Whole stream fits: tail=None, usage from message_delta in the one buffer.
        let buf = b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hello world\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"input_tokens\":18,\"output_tokens\":29}}\n\n";
        let (input, output) = extract_usage_from_sse_buffer(buf, None, &ProviderFormat::Anthropic);
        assert_eq!(input, Some(18));
        assert_eq!(output, Some(29));
    }

    #[test]
    fn test_extract_usage_responses_nested_response_usage() {
        // OpenAI Responses `response.completed` nests usage under response -
        // top-level lookup alone never found it (deepseek /v1/responses).
        let buf = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"usage\":{\"input_tokens\":777,\"output_tokens\":55}}}\n\n";
        let (input, output) =
            extract_usage_from_sse_buffer(buf, None, &ProviderFormat::OpenaiResponses);
        assert_eq!(input, Some(777));
        assert_eq!(output, Some(55));
    }

    #[test]
    fn test_extract_usage_openai_chat_unchanged() {
        let buf = b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":20}}\n\ndata: [DONE]\n\n";
        let (input, output) = extract_usage_from_sse_buffer(buf, None, &ProviderFormat::Openai);
        assert_eq!(input, Some(100));
        assert_eq!(output, Some(20));
    }

    #[test]
    fn test_extract_usage_truncated_boundary_line_is_tolerated() {
        // The head's last line may be cut mid-JSON at the cap boundary; the
        // parse failure must not poison the rest of the scan.
        let head = b"data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"abc\"}}\n\ndata: {\"type\":\"mess";
        let tail = b"age_delta\"},\"usage\":{\"input_tokens\":9,\"output_tokens\":3}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"input_tokens\":91,\"output_tokens\":33}}\n\n";
        let (input, output) =
            extract_usage_from_sse_buffer(head, Some(tail), &ProviderFormat::Anthropic);
        assert_eq!(input, Some(91));
        assert_eq!(output, Some(33));
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
    fn test_repair_anthropic_injects_missing_tool_result() {
        // Orphaned tool_use with no tool_result anywhere (e.g. truncated by /compact).
        let mut body = json!({
            "model": "kimi",
            "messages": [
                {"role": "user", "content": "analyze these"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "analyze_image:48", "name": "analyze_image", "input": {"url": "x"}}
                ]},
                {"role": "user", "content": "never mind, reply hi"}
            ]
        });
        repair_anthropic_tool_pairs(&mut body);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        let content = messages[2]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "analyze_image:48");
        assert_eq!(content[0]["content"], "");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "never mind, reply hi");
    }

    #[test]
    fn test_repair_anthropic_repositions_misplaced_tool_result() {
        // tool_result lands in a user message AFTER a text user message, so it is
        // not adjacent to the assistant tool_use. Must be moved up and merged.
        let mut body = json!({
            "model": "kimi",
            "messages": [
                {"role": "user", "content": "do X"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "analyze_image:50", "name": "analyze_image", "input": {}}
                ]},
                {"role": "user", "content": "an intermediate note"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "analyze_image:50", "content": "img desc"}
                ]},
                {"role": "user", "content": "reply hi"}
            ]
        });
        repair_anthropic_tool_pairs(&mut body);
        let messages = body["messages"].as_array().unwrap();
        let asst_idx = messages
            .iter()
            .position(|m| m.get("role").and_then(|v| v.as_str()) == Some("assistant"))
            .unwrap();
        let next = &messages[asst_idx + 1];
        assert_eq!(next["role"], "user");
        let content = next["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "analyze_image:50");
        assert_eq!(content[0]["content"], "img desc");
        // no empty user messages left behind
        for m in messages.iter() {
            if m.get("role").and_then(|v| v.as_str()) == Some("user") {
                if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
                    assert!(!arr.is_empty(), "no empty user content blocks");
                }
            }
        }
    }

    #[test]
    fn test_repair_anthropic_wellformed_unchanged() {
        let mut body = json!({
            "model": "kimi",
            "messages": [
                {"role": "user", "content": "do X"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "let me look"},
                    {"type": "tool_use", "id": "t1", "name": "bash", "input": {"cmd": "ls"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
                    {"type": "text", "text": "thanks"}
                ]},
                {"role": "user", "content": "next"}
            ]
        });
        repair_anthropic_tool_pairs(&mut body);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        let content = messages[2]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "t1");
        assert_eq!(content[0]["content"], "ok");
        assert_eq!(content[1]["text"], "thanks");
    }

    #[test]
    fn test_strip_server_tool_use_removes_blocks_and_results() {
        // opencarrier/Z.ai server-tool pattern: a server_tool_use block with its
        // tool_result embedded in the same assistant message.
        let mut body = json!({
            "model": "kimi",
            "messages": [
                {"role": "user", "content": "analyze"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "calling tool"},
                    {"type": "server_tool_use", "id": "call_srv1", "name": "analyze_image", "input": {}},
                    {"type": "tool_result", "tool_use_id": "call_srv1", "content": "image desc"},
                    {"type": "tool_use", "id": "call_cli1", "name": "bash", "input": {"cmd": "ls"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_cli1", "content": "file1"}
                ]}
            ]
        });
        strip_server_tool_use(&mut body);
        let messages = body["messages"].as_array().unwrap();
        let asst = messages[1]["content"].as_array().unwrap();
        assert!(!asst.iter().any(|b| b["type"] == "server_tool_use"));
        assert!(!asst.iter().any(|b| b["type"] == "tool_result" && b["tool_use_id"] == "call_srv1"));
        assert!(asst.iter().any(|b| b["type"] == "text"));
        assert!(asst.iter().any(|b| b["type"] == "tool_use" && b["id"] == "call_cli1"));
        let user = messages[2]["content"].as_array().unwrap();
        assert_eq!(user[0]["tool_use_id"], "call_cli1");
    }

    #[test]
    fn test_sanitize_drops_empty_text_blocks_and_backfills() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                // assistant message with an empty text block alongside real content
                {"role": "assistant", "content": [
                    {"type": "text", "text": ""},
                    {"type": "tool_use", "id": "t1", "name": "bash", "input": {}}
                ]},
                // message whose ONLY block is empty text -> becomes placeholder
                {"role": "user", "content": [ {"type": "text", "text": ""} ]},
                {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}
            ]
        });
        sanitize_anthropic_blocks(&mut body);
        let m = body["messages"].as_array().unwrap();
        // empty text dropped, tool_use kept
        let asst = m[1]["content"].as_array().unwrap();
        assert_eq!(asst.len(), 1);
        assert_eq!(asst[0]["type"], "tool_use");
        // emptied message backfilled with a placeholder text block
        let user = m[2]["content"].as_array().unwrap();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0]["type"], "text");
        assert!(!user[0]["text"].as_str().unwrap().is_empty());
        // real text untouched
        assert_eq!(m[3]["content"][0]["text"], "ok");
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
    fn test_inject_ppt_skill_defaults() {
        let mut body = json!({
            "model": "ppt",
            "messages": [{"role": "user", "content": "make a 5-slide deck about cats"}]
        });
        inject_ppt_skill(&mut body);
        assert_eq!(body["stream"], true);
        let skill = body["skill"].as_array().unwrap();
        assert_eq!(skill.len(), 1);
        assert_eq!(skill[0]["type"], "ppt");
        assert_eq!(skill[0]["mode"], "general");
        assert_eq!(skill[0]["template_id"], "news_01");
        // override fields must not leak to the provider
        assert!(body.get("template_id").is_none());
        assert!(body.get("mode").is_none());
    }

    #[test]
    fn test_inject_ppt_skill_template_override() {
        let mut body = json!({
            "model": "ppt",
            "template_id": "science_01",
            "messages": [{"role": "user", "content": "make a deck"}]
        });
        inject_ppt_skill(&mut body);
        assert_eq!(body["skill"][0]["template_id"], "science_01");
        assert_eq!(body["stream"], true);
        assert!(body.get("template_id").is_none());
    }

    fn route_with_format(format: crate::config::ProviderFormat) -> Route {
        Route {
            id: "r1".into(),
            provider: "seedance".into(),
            base_url: "https://ark.cn-beijing.volces.com".into(),
            ws_url: None,
            model: "doubao-seedance-2-0-260128".into(),
            tags: vec!["seedance".into()],
            format,
            path: "".into(),
            enabled: true,
            tool_mode: crate::config::ToolMode::Native,
        }
    }

    #[test]
    fn test_find_first_image_url() {
        let obj = json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"make it move"},
            {"type":"image_url","image_url":{"url":"https://x/first.png"}}
        ]}]});
        assert_eq!(find_first_image_url(&obj).as_deref(), Some("https://x/first.png"));

        let as_str = json!({"messages":[{"role":"user","content":[
            {"type":"image_url","image_url":"https://x/f2.png"}
        ]}]});
        assert_eq!(find_first_image_url(&as_str).as_deref(), Some("https://x/f2.png"));

        let none = json!({"messages":[{"role":"user","content":"text only"}]});
        assert!(find_first_image_url(&none).is_none());
    }

    #[test]
    fn test_task_poll_url_formats() {
        let seedance = route_with_format(crate::config::ProviderFormat::Seedance);
        assert_eq!(
            task_poll_url(&seedance, "cgt-123"),
            "https://ark.cn-beijing.volces.com/api/v3/contents/generations/tasks/cgt-123"
        );

        let mut ds = route_with_format(crate::config::ProviderFormat::DashscopeVideo);
        ds.base_url = "https://dashscope.aliyuncs.com".into();
        assert_eq!(
            task_poll_url(&ds, "task-9"),
            "https://dashscope.aliyuncs.com/api/v1/tasks/task-9"
        );
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

    #[test]
    fn test_parse_thinking_intent_anthropic_face() {
        assert_eq!(
            parse_thinking_intent("anthropic", &json!({"thinking": {"type": "disabled"}})),
            Some(ThinkingIntent::Disabled)
        );
        assert_eq!(
            parse_thinking_intent(
                "anthropic",
                &json!({"thinking": {"type": "enabled", "budget_tokens": 2000}})
            ),
            Some(ThinkingIntent::Enabled { budget_tokens: 2000 })
        );
        // absent / unknown type / not an object → no intent
        assert_eq!(parse_thinking_intent("anthropic", &json!({})), None);
        assert_eq!(
            parse_thinking_intent("anthropic", &json!({"thinking": {"type": "adaptive"}})),
            None
        );
    }

    #[test]
    fn test_parse_thinking_intent_openai_face() {
        assert_eq!(
            parse_thinking_intent("openai", &json!({"reasoning_effort": "none"})),
            Some(ThinkingIntent::Disabled)
        );
        assert_eq!(
            parse_thinking_intent("openai", &json!({"reasoning_effort": "minimal"})),
            Some(ThinkingIntent::Disabled)
        );
        assert_eq!(
            parse_thinking_intent("openai", &json!({"reasoning_effort": "high"})),
            Some(ThinkingIntent::Enabled { budget_tokens: 24000 })
        );
        // unknown effort → no intent (falls through, nothing else set)
        assert_eq!(
            parse_thinking_intent("openai", &json!({"reasoning_effort": "xhigh"})),
            None
        );
        // zhipu-style thinking param fallback
        assert_eq!(
            parse_thinking_intent("openai", &json!({"thinking": {"type": "disabled"}})),
            Some(ThinkingIntent::Disabled)
        );
        // enable_thinking fallback
        assert_eq!(
            parse_thinking_intent("openai", &json!({"enable_thinking": false})),
            Some(ThinkingIntent::Disabled)
        );
        assert_eq!(parse_thinking_intent("openai", &json!({})), None);
    }

    #[test]
    fn test_parse_thinking_intent_responses_face() {
        assert_eq!(
            parse_thinking_intent(
                "openai_responses",
                &json!({"reasoning": {"effort": "none"}})
            ),
            Some(ThinkingIntent::Disabled)
        );
        assert_eq!(
            parse_thinking_intent(
                "openai_responses",
                &json!({"reasoning": {"effort": "low"}})
            ),
            Some(ThinkingIntent::Enabled { budget_tokens: 4000 })
        );
        assert_eq!(parse_thinking_intent("openai_responses", &json!({})), None);
    }

    #[test]
    fn test_enable_anthropic_thinking_bumps_only_when_needed() {
        // max_tokens below budget → bumped above it
        let mut b = json!({"max_tokens": 150});
        enable_anthropic_thinking(&mut b, 10000);
        assert_eq!(b["thinking"]["budget_tokens"], 10000);
        assert_eq!(b["max_tokens"], 16000);

        // max_tokens already above budget → respected
        let mut b = json!({"max_tokens": 12000});
        enable_anthropic_thinking(&mut b, 10000);
        assert_eq!(b["max_tokens"], 12000);

        // missing max_tokens → defaulted to 4096; budget above it forces a bump
        let mut b = json!({});
        enable_anthropic_thinking(&mut b, 5000);
        assert_eq!(b["max_tokens"], 11000);
    }

    #[test]
    fn test_openai_rename_max_tokens() {
        let route = |base: &str| Route {
            id: "r".into(), base_url: base.into(), ws_url: None, model: "gpt-6.1-sol".into(),
            provider: "chatgpt".into(), tags: vec![], format: ProviderFormat::Openai,
            path: String::new(), enabled: true, tool_mode: crate::config::ToolMode::Native,
        };

        // OpenAI host + max_tokens → renamed
        let mut b = json!({"model": "gpt-6.1-sol", "max_tokens": 512});
        openai_rename_max_tokens(&mut b, &route("https://api.openai.com/"));
        assert_eq!(b["max_completion_tokens"], 512);
        assert!(b.get("max_tokens").is_none());

        // trailing-slash-free and http variants still match
        let mut b = json!({"max_tokens": 512});
        openai_rename_max_tokens(&mut b, &route("https://api.openai.com"));
        assert!(b.get("max_completion_tokens").is_some());

        // third-party OpenAI-compatible provider → untouched
        let mut b = json!({"max_tokens": 512});
        openai_rename_max_tokens(&mut b, &route("https://api.deepseek.com"));
        assert_eq!(b["max_tokens"], 512);
        assert!(b.get("max_completion_tokens").is_none());

        // both sent → explicit max_completion_tokens wins, max_tokens dropped
        let mut b = json!({"max_tokens": 100, "max_completion_tokens": 200});
        openai_rename_max_tokens(&mut b, &route("https://api.openai.com/"));
        assert_eq!(b["max_completion_tokens"], 200);
        assert!(b.get("max_tokens").is_none());

        // no token fields → no-op
        let mut b = json!({"model": "gpt-6.1-sol"});
        openai_rename_max_tokens(&mut b, &route("https://api.openai.com/"));
        assert!(b.get("max_tokens").is_none());
        assert!(b.get("max_completion_tokens").is_none());
    }
}
