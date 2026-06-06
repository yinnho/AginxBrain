use crate::config::{save_config, AppConfig, AppState, RequestLog};
use crate::proxy::{self, TestResult};
use crate::takeover::{
    check_codex_takeover_status, check_takeover_status, restore_claude, restore_codex,
    take_over_claude, take_over_codex,
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};


// ─── Management API authentication middleware ───────────────────────────

/// Extract and validate the X-Management-Key header for write operations.
/// Returns Ok(()) if the key matches the configured management_key,
/// or an ApiError::Unauthorized response.
pub async fn require_management_key(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, ApiError> {
    let config = state.config.read().await;
    let expected = config.management_key.clone();
    drop(config);

    if let Some(key) = request.headers().get("x-management-key") {
        if let Ok(key_str) = key.to_str() {
            if key_str == expected {
                return Ok(next.run(request).await);
            }
        }
    }
    Err(ApiError::Unauthorized)
}

use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct StatusResponse {
    pub current_tag: String,
    pub takeover: TakeoverInfo,
    pub codex_takeover: TakeoverInfo,
}

#[derive(Serialize, Clone)]
pub struct TakeoverInfo {
    pub active: bool,
    pub proxy_url: Option<String>,
}

fn validate_config(config: &AppConfig) -> Result<(), String> {
    if config.port == 0 {
        return Err("port cannot be 0".to_string());
    }

    for (id, provider) in &config.providers {
        if provider.base_url.trim().is_empty() {
            return Err(format!("provider '{}' has empty base_url", id));
        }
        if provider.api_key.trim().is_empty() {
            return Err(format!("provider '{}' has empty api_key", id));
        }
    }

    for (i, route) in config.routes.iter().enumerate() {
        if route.model.trim().is_empty() {
            return Err(format!("route #{} has empty model", i + 1));
        }
        if route.provider.trim().is_empty() {
            return Err(format!("route #{} has empty provider", i + 1));
        }
        if !config.providers.contains_key(&route.provider) {
            return Err(format!(
                "route #{} references unknown provider '{}'",
                i + 1,
                route.provider
            ));
        }
    }

    Ok(())
}

// GET /api/config
pub async fn get_config(State(state): State<AppState>) -> Json<AppConfig> {
    let config = state.config.read().await;
    Json(config.clone())
}

// PUT /api/config
pub async fn update_config(
    State(state): State<AppState>,
    Json(new_config): Json<AppConfig>,
) -> Result<StatusCode, ApiError> {
    validate_config(&new_config).map_err(ApiError::Validation)?;
    save_config(&new_config).map_err(ApiError::from)?;
    let mut config = state.config.write().await;
    *config = new_config;
    Ok(StatusCode::OK)
}

// PUT /api/current-tag
#[derive(Deserialize)]
pub struct SetTagRequest {
    pub tag: String,
}

pub async fn set_current_tag(
    State(state): State<AppState>,
    Json(req): Json<SetTagRequest>,
) -> Result<StatusCode, ApiError> {
    let mut config = state.config.write().await;
    config.current_tag = req.tag;
    save_config(&config).map_err(ApiError::from)?;
    Ok(StatusCode::OK)
}

// POST /api/takeover/claude
pub async fn takeover_claude_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let port = state.config.read().await.port;
    let proxy_url = take_over_claude(port).map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({ "proxy_url": proxy_url })))
}

// DELETE /api/takeover/claude
pub async fn restore_claude_handler() -> Result<StatusCode, ApiError> {
    restore_claude().map_err(ApiError::from)?;
    Ok(StatusCode::OK)
}

// GET /api/status
pub async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let config = state.config.read().await;
    let port = config.port;
    let claude_status = check_takeover_status(port);
    let codex_status = check_codex_takeover_status(port);
    Json(StatusResponse {
        current_tag: config.current_tag.clone(),
        takeover: TakeoverInfo {
            active: claude_status.active,
            proxy_url: claude_status.proxy_url,
        },
        codex_takeover: TakeoverInfo {
            active: codex_status.active,
            proxy_url: codex_status.proxy_url,
        },
    })
}

// GET /api/logs
pub async fn get_logs(State(state): State<AppState>) -> Json<Vec<RequestLog>> {
    let logs = state.request_log.read().await;
    Json(logs.clone())
}

// POST /api/test
#[derive(Deserialize)]
pub struct TestRequest {
    pub tag: String,
    #[serde(default = "default_test_prompt")]
    pub prompt: String,
}

fn default_test_prompt() -> String {
    "Hi, reply with one word.".to_string()
}

pub async fn test_route_handler(
    State(state): State<AppState>,
    Json(req): Json<TestRequest>,
) -> Json<TestResult> {
    let result = proxy::test_route(&state, &req.tag, &req.prompt).await;
    Json(result)
}

// GET /v1/models — OpenAI-compatible model discovery for Codex
// Returns tag-based virtual model names (opus, sonnet, haiku, auto) PLUS
// Codex catalog model names (gpt-5.5, gpt-5.4, gpt-5.4-mini, gpt-5.3-codex, gpt-5.2)
// so Codex can find its configured model in the list and use full model metadata.
pub async fn get_models(State(state): State<AppState>) -> Json<serde_json::Value> {
    let config = state.config.read().await;

    // Collect unique tag names from all routes
    let mut tag_set = std::collections::HashSet::new();
    for route in &config.routes {
        for tag in &route.tags {
            tag_set.insert(tag.clone());
        }
    }
    // Always include "auto" as it's the default fallback
    tag_set.insert("auto".to_string());

    // Codex bundled catalog model names — these must appear in the list so Codex
    // recognizes its configured model and uses full metadata (tool parallelization,
    // reasoning summaries, proper truncation, etc.) instead of degraded fallback.
    const CODEX_MODELS: &[&str] = &[
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.3-codex",
        "gpt-5.2",
    ];
    for &m in CODEX_MODELS {
        tag_set.insert(m.to_string());
    }

    let mut models: Vec<serde_json::Value> = tag_set.into_iter().map(|tag| {
        serde_json::json!({
            "id": tag,
            "object": "model",
            "created": 0,
            "owned_by": "aginxbrain"
        })
    }).collect();
    // Sort for consistent ordering
    models.sort_by(|a, b| {
        let a_str = a["id"].as_str().unwrap_or("");
        let b_str = b["id"].as_str().unwrap_or("");
        a_str.cmp(b_str)
    });

    Json(serde_json::json!({
        "object": "list",
        "data": models
    }))
}

// POST /api/takeover/codex
pub async fn takeover_codex_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let port = state.config.read().await.port;
    // Use "gpt-5.5" — a model that exists in Codex's bundled catalog so Codex
    // uses full model metadata instead of degraded fallback.  The proxy routes
    // by current_tag regardless of the model name sent by the client.
    let proxy_url = take_over_codex(port, "gpt-5.5").map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({ "proxy_url": proxy_url })))
}

// DELETE /api/takeover/codex
pub async fn restore_codex_handler() -> Result<StatusCode, ApiError> {
    restore_codex().map_err(ApiError::from)?;
    Ok(StatusCode::OK)
}

// POST /api/config/export
pub async fn export_config(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let config = state.config.read().await;
    let mut export_config = config.clone();
    // Replace real management_key with placeholder for security
    export_config.management_key = "YOUR_MANAGEMENT_KEY".to_string();
    let json_value =
        serde_json::to_value(&export_config).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json_value))
}

// POST /api/config/import
pub async fn import_config(
    State(state): State<AppState>,
    Json(mut import_config): Json<AppConfig>,
) -> Result<StatusCode, ApiError> {
    // Validate the imported config
    validate_config(&import_config).map_err(ApiError::Validation)?;

    // If the imported management_key is the placeholder, preserve the existing key
    if import_config.management_key == "YOUR_MANAGEMENT_KEY" {
        let current = state.config.read().await;
        import_config.management_key = current.management_key.clone();
    }

    save_config(&import_config).map_err(ApiError::from)?;
    let mut config = state.config.write().await;
    *config = import_config;
    Ok(StatusCode::OK)
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    Internal(String),
    #[error("{0}")]
    Validation(String),
    #[error("unauthorized")]
    Unauthorized,
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            ApiError::Validation(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
        };
        let body = serde_json::json!({ "error": msg });
        (status, Json(body)).into_response()
    }
}
