use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Admin auth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AdminSetupRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminMeResponse {
    pub username: String,
}

// ---------------------------------------------------------------------------
// Caller keys
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerKey {
    pub id: i64,
    pub name: String,
    pub note: String,
    pub enabled: bool,
    pub created_at: String,
    /// Plaintext token, present only for keys created after the
    /// 003_add_caller_token_plain migration. NULL for legacy keys.
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCallerKeyRequest {
    pub name: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateCallerKeyResponse {
    pub id: i64,
    pub name: String,
    pub note: String,
    pub enabled: bool,
    pub created_at: String,
    /// The raw token is shown exactly once on creation.
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCallerKeyRequest {
    pub name: String,
    #[serde(default)]
    pub note: String,
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct UsageLog {
    pub id: i64,
    pub caller_key_id: Option<i64>,
    pub timestamp: String,
    pub tag: String,
    pub provider: String,
    pub model: String,
    pub modality: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub latency_ms: i64,
    pub status: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyUsage {
    pub day: String,
    pub caller_key_id: Option<i64>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub estimated_cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthlyUsage {
    pub month: String,
    pub caller_key_id: Option<i64>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub estimated_cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSummary {
    pub caller_key_id: Option<i64>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub estimated_cost: f64,
}

// ---------------------------------------------------------------------------
// Cost rates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRate {
    pub id: i64,
    pub provider: String,
    pub model: String,
    pub input_price_per_1k: f64,
    pub output_price_per_1k: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetCostRateRequest {
    pub provider: String,
    pub model: String,
    pub input_price_per_1k: f64,
    pub output_price_per_1k: f64,
}
