use crate::config::{save_config, AppConfig, AppState, RequestLog};
use crate::db;
use crate::models::{
    AdminLoginRequest, AdminMeResponse, AdminSetupRequest, CallerKey, CostRate,
    CreateCallerKeyRequest, CreateCallerKeyResponse, DailyUsage, MonthlyUsage,
    SetCostRateRequest, UpdateCallerKeyRequest, UsageSummary,
};
use crate::proxy::{self, TestResult};
use crate::takeover::{
    check_codex_takeover_status, check_takeover_status, restore_claude, restore_codex,
    take_over_claude, take_over_codex,
};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

pub const ADMIN_SESSION_KEY: &str = "admin_id";

// ─── Management API authentication middleware ───────────────────────────

/// Ensure the request has a valid admin session. All management endpoints use this.
pub async fn require_admin_session(
    session: Session,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, ApiError> {
    match session.get::<i64>(ADMIN_SESSION_KEY).await {
        Ok(Some(_)) => Ok(next.run(request).await),
        _ => Err(ApiError::Unauthorized),
    }
}

/// Accept either an admin session or a valid caller-key Bearer token.
/// Used for client-facing endpoints (logs, status, takeover) that the
/// desktop app accesses with just an API key.
pub async fn require_admin_or_caller_key(
    State(state): State<AppState>,
    session: Session,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, ApiError> {
    // 1. Check admin session first
    match session.get::<i64>(ADMIN_SESSION_KEY).await {
        Ok(Some(_)) => return Ok(next.run(request).await),
        _ => {}
    }
    // 2. Fall back to Bearer caller key
    let token = crate::axum_server::extract_caller_token(request.headers());
    if let Some(token) = token {
        let result = db::find_caller_key_by_token(&state.db, &token)
            .await
            .map_err(|_| ApiError::Internal("db error".to_string()))?;
        if let Some((_id, enabled)) = result {
            if enabled {
                return Ok(next.run(request).await);
            }
        }
    }
    Err(ApiError::Unauthorized)
}

// ─── Admin auth endpoints ───────────────────────────────────────────────

// POST /api/admin/setup
pub async fn admin_setup(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<AdminSetupRequest>,
) -> Result<StatusCode, ApiError> {
    let count = db::admin_count(&state.db).await.map_err(ApiError::from)?;
    if count > 0 {
        return Err(ApiError::Validation("admin already exists".to_string()));
    }
    let trimmed = req.username.trim();
    if trimmed.is_empty() || req.password.len() < 6 {
        return Err(ApiError::Validation(
            "username required and password must be at least 6 characters".to_string(),
        ));
    }
    let hash = hash_password(&req.password).map_err(|e| ApiError::Internal(e.to_string()))?;
    let id = db::create_admin(&state.db, trimmed, &hash)
        .await
        .map_err(ApiError::from)?;
    // Log the new admin in immediately — setup establishes the session so the
    // UI lands in the dashboard without a separate login step.
    session
        .insert(ADMIN_SESSION_KEY, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(StatusCode::OK)
}

// POST /api/admin/login
pub async fn admin_login(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<AdminLoginRequest>,
) -> Result<StatusCode, ApiError> {
    let row = db::find_admin_by_username(&state.db, req.username.trim())
        .await
        .map_err(ApiError::from)?;
    let Some((id, password_hash)) = row else {
        return Err(ApiError::Unauthorized);
    };
    if !verify_password(&req.password, &password_hash).map_err(|e| ApiError::Internal(e.to_string()))? {
        return Err(ApiError::Unauthorized);
    }
    session
        .insert(ADMIN_SESSION_KEY, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(StatusCode::OK)
}

// POST /api/admin/logout
pub async fn admin_logout(session: Session) -> Result<StatusCode, ApiError> {
    let _ = session.remove::<i64>(ADMIN_SESSION_KEY).await;
    Ok(StatusCode::OK)
}

// GET /api/admin/me
pub async fn admin_me(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<AdminMeResponse>, ApiError> {
    let admin_id = session
        .get::<i64>(ADMIN_SESSION_KEY)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::Unauthorized)?;
    // Find admin username by id. Reuse find_admin_by_username is not possible, so query directly.
    let username: String = sqlx::query_scalar("SELECT username FROM admins WHERE id = ?1")
        .bind(admin_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(AdminMeResponse { username }))
}

fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("failed to hash password: {}", e))?;
    Ok(hash.to_string())
}

fn verify_password(password: &str, hash: &str) -> anyhow::Result<bool> {
    let parsed = PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("invalid hash: {}", e))?;
    let argon2 = Argon2::default();
    Ok(argon2
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

// ─── Caller key management ──────────────────────────────────────────────

// GET /api/keys
pub async fn list_keys(State(state): State<AppState>) -> Result<Json<Vec<CallerKey>>, ApiError> {
    let keys = db::list_caller_keys(&state.db).await.map_err(ApiError::from)?;
    Ok(Json(keys))
}

// POST /api/keys
pub async fn create_key(
    State(state): State<AppState>,
    Json(req): Json<CreateCallerKeyRequest>,
) -> Result<Json<CreateCallerKeyResponse>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::Validation("name cannot be empty".to_string()));
    }
    let resp = db::create_caller_key(&state.db, name, req.note.trim())
        .await
        .map_err(ApiError::from)?;
    Ok(Json(resp))
}

// PUT /api/keys/:id
pub async fn update_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateCallerKeyRequest>,
) -> Result<StatusCode, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::Validation("name cannot be empty".to_string()));
    }
    let ok = db::update_caller_key(&state.db, id, name, req.note.trim(), req.enabled)
        .await
        .map_err(ApiError::from)?;
    if !ok {
        return Err(ApiError::Validation("key not found".to_string()));
    }
    Ok(StatusCode::OK)
}

// DELETE /api/keys/:id
pub async fn delete_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let ok = db::delete_caller_key(&state.db, id).await.map_err(ApiError::from)?;
    if !ok {
        return Err(ApiError::Validation("key not found".to_string()));
    }
    Ok(StatusCode::OK)
}

// ─── Cost rates ─────────────────────────────────────────────────────────

// GET /api/cost-rates
pub async fn list_cost_rates(
    State(state): State<AppState>,
) -> Result<Json<Vec<CostRate>>, ApiError> {
    let rates = db::list_cost_rates(&state.db).await.map_err(ApiError::from)?;
    Ok(Json(rates))
}

// POST /api/cost-rates
pub async fn set_cost_rate(
    State(state): State<AppState>,
    Json(req): Json<SetCostRateRequest>,
) -> Result<Json<CostRate>, ApiError> {
    let provider = req.provider.trim();
    let model = req.model.trim();
    if provider.is_empty() || model.is_empty() {
        return Err(ApiError::Validation("provider and model required".to_string()));
    }
    let rate = db::set_cost_rate(&state.db, provider, model, req.input_price_per_1k, req.output_price_per_1k)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(rate))
}

// DELETE /api/cost-rates/:id
pub async fn delete_cost_rate(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let ok = db::delete_cost_rate(&state.db, id).await.map_err(ApiError::from)?;
    if !ok {
        return Err(ApiError::Validation("rate not found".to_string()));
    }
    Ok(StatusCode::OK)
}

// ─── Usage ──────────────────────────────────────────────────────────────

/// Query parameters for daily usage aggregation.
#[derive(Deserialize)]
pub struct DailyUsageQuery {
    pub key_id: Option<i64>,
    pub from: String,
    pub to: String,
}

// GET /api/usage/daily
pub async fn daily_usage(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<DailyUsageQuery>,
) -> Result<Json<Vec<DailyUsage>>, ApiError> {
    let rows = db::daily_usage(&state.db, q.key_id, &q.from, &q.to)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(rows))
}

/// Query parameters for monthly usage aggregation.
#[derive(Deserialize)]
pub struct MonthlyUsageQuery {
    pub key_id: Option<i64>,
    pub year: i32,
    pub month: i32,
}

// GET /api/usage/monthly
pub async fn monthly_usage(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<MonthlyUsageQuery>,
) -> Result<Json<Vec<MonthlyUsage>>, ApiError> {
    let rows = db::monthly_usage(&state.db, q.key_id, q.year, q.month)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(rows))
}

// GET /api/usage/summary
pub async fn usage_summary(
    State(state): State<AppState>,
) -> Result<Json<Vec<UsageSummary>>, ApiError> {
    let rows = db::usage_summary(&state.db).await.map_err(ApiError::from)?;
    Ok(Json(rows))
}

// ─── Config/status/logs ─────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StatusResponse {
    pub current_tag: String,
    pub takeover: TakeoverInfo,
    pub codex_takeover: TakeoverInfo,
    pub setup_required: bool,
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
pub async fn get_status(State(state): State<AppState>) -> Result<Json<StatusResponse>, ApiError> {
    let config = state.config.read().await;
    let port = config.port;
    let claude_status = check_takeover_status(port);
    let codex_status = check_codex_takeover_status(port);
    let setup_required = db::admin_count(&state.db).await.unwrap_or(1) == 0;
    Ok(Json(StatusResponse {
        current_tag: config.current_tag.clone(),
        takeover: TakeoverInfo {
            active: claude_status.active,
            proxy_url: claude_status.proxy_url,
        },
        codex_takeover: TakeoverInfo {
            active: codex_status.active,
            proxy_url: codex_status.proxy_url,
        },
        setup_required,
    }))
}

// GET /api/logs
pub async fn get_logs(State(state): State<AppState>) -> Result<Json<Vec<RequestLog>>, ApiError> {
    let rows: Vec<(String, String, String, String, String, String, Option<String>, Option<i64>, Option<i64>, i64, f64, i64)> = sqlx::query_as(
        "SELECT u.request_model, u.tag, u.provider, u.model as target_model, u.modality, u.timestamp, k.name as caller_key_name,
                u.input_tokens, u.output_tokens, u.latency_ms,
                COALESCE((u.input_tokens / 1000.0) * COALESCE(r.input_price_per_1k, 0.0) + (u.output_tokens / 1000.0) * COALESCE(r.output_price_per_1k, 0.0), 0.0) as cost,
                CAST((strftime('%s', u.timestamp) * 1000) AS INTEGER) as timestamp_ms
         FROM usage_logs u
         LEFT JOIN caller_keys k ON u.caller_key_id = k.id
         LEFT JOIN cost_rates r ON u.provider = r.provider AND u.model = r.model
         ORDER BY u.timestamp DESC
         LIMIT 200",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let logs = rows
        .into_iter()
        .map(|(request_model, tag, provider, target_model, modality, timestamp, caller_key_name, input_tokens, output_tokens, latency_ms, cost, timestamp_ms)| RequestLog {
            request_model,
            tag,
            provider,
            target_model,
            modality,
            timestamp,
            caller_key_name,
            input_tokens,
            output_tokens,
            latency_ms,
            cost,
            timestamp_ms,
        })
        .collect();
    Ok(Json(logs))
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

// POST /api/brain/generate/image
pub async fn generate_image_handler(
    State(state): State<AppState>,
    Json(req): Json<proxy::GenerateImageRequest>,
) -> Json<proxy::GenerateImageResponse> {
    Json(proxy::generate_image(&state, req).await)
}

// GET /v1/models — OpenAI-compatible model discovery for Codex and other clients.
// Returns all configured tags as valid model IDs. Since tags are now purely
// user-defined, administrators can add a new tag (e.g. "gpt-5.5" or "codex")
// at runtime without code changes, and clients using that model name will
// immediately be routed through the matching tag.
pub async fn get_models(State(state): State<AppState>) -> Json<serde_json::Value> {
    let config = state.config.read().await;

    // Collect unique tag names from all routes plus configured tags
    let mut tag_set = std::collections::HashSet::new();
    for tag in &config.tags {
        tag_set.insert(tag.name.clone());
    }
    for route in &config.routes {
        for tag in &route.tags {
            tag_set.insert(tag.clone());
        }
    }
    // Always include "auto" as it's the default fallback
    tag_set.insert("auto".to_string());

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
    validate_config(&import_config).map_err(ApiError::Validation)?;

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
