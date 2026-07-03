use serde::{Deserialize, Serialize};

// ─── Admin auth ──────────────────────────────────────────────────────────────

/// Request to create the initial admin account. Only succeeds when no admin exists.
#[derive(Debug, Clone, Deserialize)]
pub struct AdminSetupRequest {
    pub username: String,
    pub password: String,
}

/// Admin login request.
#[derive(Debug, Clone, Deserialize)]
pub struct AdminLoginRequest {
    pub username: String,
    pub password: String,
}

/// Response from the me endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct AdminMeResponse {
    pub username: String,
}

// ─── Caller keys ─────────────────────────────────────────────────────────────

/// A caller API key as stored in the database. The raw token is hashed;
/// plaintext is only available if created after the 003 migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerKey {
    pub id: i64,
    pub name: String,
    pub note: String,
    pub enabled: bool,
    pub created_at: String,
    /// Plaintext token (nullable for legacy keys before 003 migration).
    pub token: Option<String>,
}

/// Request to create a new caller API key.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCallerKeyRequest {
    pub name: String,
    #[serde(default)]
    pub note: String,
}

/// Response after creating a key; the raw token is shown exactly once.
#[derive(Debug, Clone, Serialize)]
pub struct CreateCallerKeyResponse {
    pub id: i64,
    pub name: String,
    pub note: String,
    pub enabled: bool,
    pub created_at: String,
    /// The raw API key token (shown once at creation).
    pub token: String,
}

/// Request to update a caller key's name, note, or enabled status.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCallerKeyRequest {
    pub name: String,
    #[serde(default)]
    pub note: String,
    pub enabled: bool,
}

// ─── Usage ───────────────────────────────────────────────────────────────────

/// Aggregated usage for one day, grouped by caller key (if any).
#[derive(Debug, Clone, Serialize)]
pub struct DailyUsage {
    pub day: String,
    pub caller_key_id: Option<i64>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub estimated_cost: f64,
}

/// Aggregated usage for one calendar month.
#[derive(Debug, Clone, Serialize)]
pub struct MonthlyUsage {
    pub month: String,
    pub caller_key_id: Option<i64>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub estimated_cost: f64,
}

/// All-time usage summary grouped by caller key.
#[derive(Debug, Clone, Serialize)]
pub struct UsageSummary {
    pub caller_key_id: Option<i64>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub estimated_cost: f64,
}

// ─── Cost rates ──────────────────────────────────────────────────────────────

/// A per-provider-per-model cost rate for estimating usage cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRate {
    pub id: i64,
    pub provider: String,
    pub model: String,
    pub input_price_per_1k: f64,
    pub output_price_per_1k: f64,
}

/// Request to create or update a cost rate.
#[derive(Debug, Clone, Deserialize)]
pub struct SetCostRateRequest {
    pub provider: String,
    pub model: String,
    pub input_price_per_1k: f64,
    pub output_price_per_1k: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealth {
    pub provider: String,
    pub total_requests: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEntry {
    pub timestamp: String,
    pub error_message: String,
    pub model: String,
}
