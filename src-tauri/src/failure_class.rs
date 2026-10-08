//! Upstream failure classification (bifrost-style, see docs/BIFROST-STUDY.md §1).
//!
//! Providers disagree wildly about status codes: Gemini rejects a bad key with
//! 400, Bedrock never answers 401 (403 + AWS exception names), Anthropic
//! reports an empty credit balance as 400, Kimi reports a usage cap as 403.
//! So classification reads structured error fields (code/type) first, message
//! phrases second, and only falls back to the bare status code.
//!
//! The resulting class answers three orthogonal questions:
//!   * `failover_action`      — same-route retry / next route / return to client
//!   * `is_per_key`           — would rotating the provider key help? (multi-key future)
//!   * `counts_against_route_health` — should this trip the circuit breaker?

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// 5xx / network errors / overloaded (529). Server-side, not the key's
    /// fault — same-route retry with backoff, then next route.
    Transient,
    /// 429 / rate-limit wording at any status. Account quotas are shared
    /// across keys, so a brief backoff before the next route still helps.
    RateLimit,
    /// The key itself is invalid/revoked (401, `invalid_api_key`, ...).
    /// Retrying the same key never clears it — move on immediately.
    Credential,
    /// Billing/usage caps (402, "insufficient balance", Kimi's 403 usage
    /// limit). Key is alive but the wallet is empty — move on immediately.
    Quota,
    /// The route can't serve THIS request: context window too small,
    /// text-only model given an image, model decommissioned, reasoning
    /// passback demands. Request-shaped, not route health — never trips
    /// the circuit.
    ModelAccess,
    /// Malformed request (other 4xx with a real provider body). Return to
    /// the client unchanged; another provider would fail the same way.
    CallerFault,
    /// Unrecognized. Treated like CallerFault (return to client) for
    /// status-bearing errors; in-stream errors override this at the call site.
    Unknown,
}

/// What the failover loop should do after classifying a failure. Same-route
/// retries for `Transient` are consumed inside `send_with_same_route_retries`
/// before classification ever runs, so only cross-route actions remain here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverAction {
    /// Advance to the next candidate route, optionally after a short backoff.
    NextRoute { backoff: Option<std::time::Duration> },
    /// Surface the upstream error to the client unchanged.
    ReturnToClient,
}

impl FailureClass {
    /// Would rotating to a different provider key help this class?
    /// (Reserved for the multi-key-per-provider pool, docs/BIFROST-STUDY.md §8.)
    #[allow(dead_code)]
    pub fn is_per_key(&self) -> bool {
        matches!(self, FailureClass::RateLimit | FailureClass::Credential | FailureClass::Quota)
    }

    /// Key-level failures that never recover within a request's lifetime.
    #[allow(dead_code)]
    pub fn is_permanent_per_key(&self) -> bool {
        matches!(self, FailureClass::Credential | FailureClass::Quota)
    }

    /// Whether repeated failures of this class should count toward opening
    /// the route's circuit breaker. Request-shaped failures (context limit,
    /// modality gap) say nothing about route health: a perfectly healthy
    /// route must not go dark because three oversized requests came through.
    pub fn counts_against_route_health(&self) -> bool {
        matches!(
            self,
            FailureClass::Transient | FailureClass::RateLimit | FailureClass::Credential | FailureClass::Quota
        )
    }

    pub fn failover_action(&self) -> FailoverAction {
        match self {
            // Same-route retries already burned their backoff budget.
            FailureClass::Transient => FailoverAction::NextRoute { backoff: None },
            // Fleet-wide limits get a short breather before the next route.
            FailureClass::RateLimit => FailoverAction::NextRoute {
                backoff: Some(std::time::Duration::from_millis(300)),
            },
            FailureClass::Credential | FailureClass::Quota | FailureClass::ModelAccess => {
                FailoverAction::NextRoute { backoff: None }
            }
            FailureClass::CallerFault | FailureClass::Unknown => FailoverAction::ReturnToClient,
        }
    }
}

/// Extract structured error signatures from a JSON error body:
/// the concatenation of `code`/`type`-ish fields (strong signals) and of
/// `message`-ish fields (weaker signals), both lowercased. Non-JSON bodies
/// yield empty signatures and classification falls through to phrases/status.
fn json_signature(body: &str) -> (String, String) {
    let mut codes = String::new();
    let mut msgs = String::new();
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return (codes, msgs);
    };
    let push = |val: Option<&str>, dst: &mut String| {
        if let Some(s) = val {
            if !s.is_empty() {
                dst.push_str(s);
                dst.push(' ');
            }
        }
    };
    // code/type fields, both top-level and nested under "error"
    // (OpenAI/Zhipu style) — ark nests as {"error":{"errcode":...}} too.
    for key in ["code", "type", "errcode", "err_code", "sub_code"] {
        push(v.get(key).and_then(|x| x.as_str()), &mut codes);
        push(v.get("error").and_then(|e| e.get(key)).and_then(|x| x.as_str()), &mut codes);
    }
    for key in ["message", "msg"] {
        push(v.get(key).and_then(|x| x.as_str()), &mut msgs);
        push(v.get("error").and_then(|e| e.get(key)).and_then(|x| x.as_str()), &mut msgs);
    }
    (codes.to_lowercase(), msgs.to_lowercase())
}

/// Classify an upstream failure. `status` is `None` for in-stream errors
/// (HTTP 200 + error SSE event), where no status code exists.
pub fn classify_upstream_failure(status: Option<u16>, err_body: &str) -> FailureClass {
    let lower = err_body.to_lowercase();
    let (codes, msgs) = json_signature(err_body);
    // Codes are the stronger signal; check both, codes first, per class so a
    // distinctive code beats a vaguer message phrase.
    let hits = |table: &[&str]| table.iter().any(|m| codes.contains(m) || msgs.contains(m) || lower.contains(m));

    // The key itself is invalid — no retry on this key can succeed.
    const CREDENTIAL: &[&str] = &[
        "invalid_api_key",
        "invalid api key",
        "invalid_api_token",
        "authentication_error",
        "authentication failed",
        "invalid token",
        "incorrect api key",
        "unauthorized",
    ];
    if hits(CREDENTIAL) {
        return FailureClass::Credential;
    }

    // Billing / usage caps. Kimi answers 403 "reached your usage limit",
    // DeepSeek 402 "Insufficient Balance", ark "Quota exceeded" — the key is
    // alive, the wallet isn't.
    const QUOTA: &[&str] = &[
        "insufficient_quota",
        "insufficient balance",
        "balance is insufficient",
        "arrearage",
        "usage limit",
        "exceeded your current quota",
        "quota exceeded",
        "subscription quota",
        "spend limit",
        "credit balance",
        "欠费",
        "余额不足",
    ];
    if hits(QUOTA) {
        return FailureClass::Quota;
    }

    const RATE: &[&str] = &[
        "rate limit",
        "rate_limit",
        "ratelimit",
        "too many requests",
        "concurrent",
        "requests per minute",
        "tps limit",
        "并发",
    ];
    if hits(RATE) {
        return FailureClass::RateLimit;
    }

    if is_model_access_error(err_body) {
        return FailureClass::ModelAccess;
    }

    // Bare status as the last resort — providers disagree even here (a bare
    // 403 is credentials; Kimi's quota-403 was already caught by phrases).
    match status {
        Some(429) => FailureClass::RateLimit,
        Some(401) | Some(403) => FailureClass::Credential,
        Some(402) => FailureClass::Quota,
        Some(404) => FailureClass::ModelAccess,
        Some(408) => FailureClass::Transient,
        Some(s) if s >= 500 => FailureClass::Transient,
        Some(_) => FailureClass::CallerFault,
        None => FailureClass::Unknown,
    }
}

/// Request-shaped route-capability failures: another route (bigger context
/// window, multimodal model, thinking-format-compatible provider) may still
/// serve the request, so these fail over without touching route health.
fn is_model_access_error(err_body: &str) -> bool {
    is_context_limit_error(err_body)
        || is_unsupported_modality_error(err_body)
        || is_reasoning_passback_error(err_body)
        || {
            let lower = err_body.to_lowercase();
            const MARKERS: &[&str] = &[
                "model_not_found",
                "model not found",
                "no such model",
                "model_not_exist",
                "does not exist",
                "model_decommissioned",
                "decommissioned",
                "no longer supported",
            ];
            MARKERS.iter().any(|m| lower.contains(m))
        }
}

/// Detect upstream error bodies that signal the request exceeded the provider's
/// context window / token limit. These are per-route limits (not malformed
/// requests), so failover to another route with a larger window may succeed.
pub fn is_context_limit_error(err_body: &str) -> bool {
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
        // Zhipu gateway variant of "too big": on a 9.5 MB request zhipu-v3
        // reported 1261 "prompt is too long" while a sibling zhipu account
        // reported 1213 below for the same body - the gateway failing to take
        // the payload, not a malformed request.
        "未正常接收到prompt参数",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Detect upstream error bodies that signal the request's modality exceeds
/// the provider's capability - e.g. a vision request landing on a text-only
/// model (Ark coding glm-5.3 returns 400 "Model only support text input").
/// The request is well-formed; this route just lacks the capability, so a
/// multimodal-capable route further down the chain may still serve it.
pub fn is_unsupported_modality_error(err_body: &str) -> bool {
    let lower = err_body.to_lowercase();
    const MARKERS: &[&str] = &[
        "only support text",
        "only supports text",
        "not support image",
        "doesn't support image",
        "does not support image",
        "image input",
        "multimodal input",
        "not support audio",
        "not support video",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Detect upstream 400s that demand the assistant's reasoning be passed back
/// (DeepSeek thinking mode: "The `reasoning_text` in the thinking mode must be
/// passed back to the API"). Happens when the client's history carries
/// textless reasoning items (encrypted_content / empty summary) that no
/// conversion can reconstruct for THIS provider's format - another route
/// (e.g. Anthropic-format thinking passthrough) may still serve the request.
pub fn is_reasoning_passback_error(err_body: &str) -> bool {
    let lower = err_body.to_lowercase();
    lower.contains("reasoning_text") && lower.contains("passed back")
        || lower.contains("reasoning must be passed back")
        || lower.contains("reasoning_content") && lower.contains("passed back")
}

/// Scan the first SSE event of a 200-status stream for an in-band error.
/// Some providers (zhipu, kimi) accept the request, then emit
/// `data: {"error": {...}}` as the first frame. Returns the serialized error
/// object (for classification) when found.
///
/// An HTTP 200 + error event is never the client's fault — the request was
/// accepted — so callers treat even CallerFault/Unknown results as failover-
/// able rather than returning them to the client.
pub fn scan_sse_event_error(event_bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event_bytes).ok()?;
    // Join the event's data lines (multi-line JSON payloads are split across
    // `data:` lines per the SSE spec).
    let data: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| l.trim())
        .collect();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let v: Value = serde_json::from_str(&data).ok()?;
    match v.get("error") {
        Some(err) if err.is_object() || err.is_string() => Some(err.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bare_status_classification() {
        assert_eq!(classify_upstream_failure(Some(500), ""), FailureClass::Transient);
        assert_eq!(classify_upstream_failure(Some(503), "oops"), FailureClass::Transient);
        assert_eq!(classify_upstream_failure(Some(529), ""), FailureClass::Transient);
        assert_eq!(classify_upstream_failure(Some(429), ""), FailureClass::RateLimit);
        assert_eq!(classify_upstream_failure(Some(401), ""), FailureClass::Credential);
        assert_eq!(classify_upstream_failure(Some(403), ""), FailureClass::Credential);
        assert_eq!(classify_upstream_failure(Some(402), ""), FailureClass::Quota);
        assert_eq!(classify_upstream_failure(Some(404), ""), FailureClass::ModelAccess);
        assert_eq!(classify_upstream_failure(Some(400), "bad json"), FailureClass::CallerFault);
        assert_eq!(classify_upstream_failure(None, "weird"), FailureClass::Unknown);
    }

    #[test]
    fn test_phrases_beat_status() {
        // Kimi's usage cap arrives as 403 — the phrase must beat bare-403
        // credential so rate_limited_count and the no-backoff path see Quota.
        assert_eq!(
            classify_upstream_failure(Some(403), "You have reached your usage limit"),
            FailureClass::Quota
        );
        // Anthropic's overloaded_error arrives as 529 with a type field.
        assert_eq!(
            classify_upstream_failure(
                Some(529),
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            ),
            FailureClass::Transient // falls to bare 529; no false phrase hits
        );
        // DeepSeek 402 with a distinctive code.
        assert_eq!(
            classify_upstream_failure(
                Some(402),
                r#"{"error":{"code":"insufficient_balance","message":"Insufficient Balance"}}"#
            ),
            FailureClass::Quota
        );
        // A quota error delivered with a 400 status (Anthropic billing).
        assert_eq!(
            classify_upstream_failure(
                Some(400),
                r#"{"error":{"type":"billing_error","message":"Your credit balance is too low"}}"#
            ),
            FailureClass::Quota
        );
    }

    #[test]
    fn test_structured_code_field() {
        // OpenAI-style code field, delivered at an unexpected status.
        assert_eq!(
            classify_upstream_failure(
                Some(400),
                r#"{"error":{"code":"invalid_api_key","message":"Incorrect API key provided"}}"#
            ),
            FailureClass::Credential
        );
        assert_eq!(
            classify_upstream_failure(
                Some(403),
                r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#
            ),
            FailureClass::Credential
        );
    }

    #[test]
    fn test_model_access_variants() {
        // Zhipu gateway 1213 on an oversized 9.5 MB Codex request (same body
        // got 1261 "prompt is too long" from a sibling zhipu account).
        assert_eq!(
            classify_upstream_failure(
                Some(400),
                "{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"code\":\"1213\",\"message\":\"[1213][未正常接收到prompt参数。]\"}}"
            ),
            FailureClass::ModelAccess
        );
        assert_eq!(
            classify_upstream_failure(
                Some(400),
                "Error code: 400 - {'error': {'message': 'input token limit is 202752'}}"
            ),
            FailureClass::ModelAccess
        );
        // Text-only model given an image-bearing request (Ark glm-5.3).
        assert_eq!(
            classify_upstream_failure(Some(400), "Model only support text input"),
            FailureClass::ModelAccess
        );
        // DeepSeek thinking-mode reasoning passback.
        assert_eq!(
            classify_upstream_failure(
                Some(400),
                "The `reasoning_text` in the thinking mode must be passed back to the API"
            ),
            FailureClass::ModelAccess
        );
        // Model decommissioned.
        assert_eq!(
            classify_upstream_failure(Some(404), "model gpt-4-turbo has been decommissioned"),
            FailureClass::ModelAccess
        );
    }

    #[test]
    fn test_route_health_predicate() {
        // Request-shaped failures must not trip the circuit.
        assert!(!FailureClass::ModelAccess.counts_against_route_health());
        assert!(!FailureClass::CallerFault.counts_against_route_health());
        assert!(FailureClass::Transient.counts_against_route_health());
        assert!(FailureClass::RateLimit.counts_against_route_health());
        assert!(FailureClass::Credential.counts_against_route_health());
        assert!(FailureClass::Quota.counts_against_route_health());
    }

    #[test]
    fn test_failover_actions() {
        assert_eq!(
            FailureClass::RateLimit.failover_action(),
            FailoverAction::NextRoute { backoff: Some(std::time::Duration::from_millis(300)) }
        );
        assert_eq!(
            FailureClass::Transient.failover_action(),
            FailoverAction::NextRoute { backoff: None }
        );
        assert_eq!(FailureClass::CallerFault.failover_action(), FailoverAction::ReturnToClient);
    }

    #[test]
    fn test_scan_sse_event_error() {
        // In-band zhipu-style error event.
        assert!(scan_sse_event_error(
            b"event: error\ndata: {\"error\":{\"code\":\"1302\",\"message\":\"content filtered\"}}\n\n"
        )
        .is_some());
        // Normal first events never match.
        assert!(scan_sse_event_error(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\"}}\n\n"
        )
        .is_none());
        assert!(scan_sse_event_error(b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n").is_none());
        assert!(scan_sse_event_error(b"data: [DONE]\n\n").is_none());
        // Multi-line JSON reassembly.
        assert!(scan_sse_event_error(b"data: {\"error\":\ndata: {\"type\":\"overloaded\"}}\n\n").is_some());
    }

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
        assert!(is_context_limit_error(
            "{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"code\":\"1213\",\"message\":\"[1213][未正常接收到prompt参数。]\"}}"
        ));
        assert!(!is_context_limit_error("Invalid API key"));
        assert!(!is_context_limit_error("Bad request: missing field model"));
    }

    #[test]
    fn test_is_unsupported_modality_error_detects_variants() {
        // Ark coding glm-5.3 on an image-bearing request.
        assert!(is_unsupported_modality_error(
            "{\"error\":{\"code\":\"InvalidParameter\",\"message\":\"Model only support text input Request id: 0217...\"}}"
        ));
        assert!(is_unsupported_modality_error("This model does not support image input"));
        assert!(is_unsupported_modality_error("Image input is not enabled for this model"));
        assert!(is_unsupported_modality_error("multimodal input not supported"));
        // Text-only 400s that are request-shape or auth problems stay put.
        assert!(!is_unsupported_modality_error("Invalid API key"));
        assert!(!is_unsupported_modality_error("Bad request: missing field model"));
        assert!(!is_context_limit_error("Model only support text input"));
    }

    #[test]
    fn test_is_reasoning_passback_error_detects_variants() {
        // DeepSeek thinking mode on a Codex history with textless reasoning items.
        assert!(is_reasoning_passback_error(
            "{\"error\":{\"message\":\"The `reasoning_text` in the thinking mode must be passed back to the API.\",\"type\":\"invalid_request_error\",\"param\":null,\"code\":\"invalid_request_error\"}}"
        ));
        assert!(is_reasoning_passback_error(
            "The reasoning_content must be passed back to the API."
        ));
        // Ordinary request-shape errors stay non-retryable.
        assert!(!is_reasoning_passback_error("Invalid API key"));
        assert!(!is_reasoning_passback_error(
            "Model only support text input"
        ));
        assert!(!is_reasoning_passback_error(
            "[1213][未正常接收到prompt参数。]"
        ));
    }
}
