use anyhow::{Context, Result};
use sqlx::{migrate::MigrateDatabase, sqlite::SqlitePoolOptions, Sqlite, SqlitePool};
use std::path::PathBuf;

use crate::models::{
    CallerKey, CostRate, CreateCallerKeyResponse, DailyUsage, MonthlyUsage, UsageSummary,
};

/// Path to the SQLite database file.
pub fn db_path() -> Result<PathBuf> {
    // Allow override via AGINXBRAIN_DB environment variable.
    if let Ok(path) = std::env::var("AGINXBRAIN_DB") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".aginxbrain").join("aginxbrain.db"))
}

/// Initialize the SQLite database: create file if missing, create pool, run migrations.
pub async fn init_db() -> Result<SqlitePool> {
    let path = db_path()?;
    log::info!("[DB] using database {}", path.display());

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let url = format!("sqlite:{}", path.display());
    if !Sqlite::database_exists(&url).await.unwrap_or(false) {
        Sqlite::create_database(&url)
            .await
            .with_context(|| format!("creating database {}", path.display()))?;
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .with_context(|| format!("connecting to database {}", path.display()))?;

    // Run embedded migrations.
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running database migrations")?;

    Ok(pool)
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

pub async fn admin_count(pool: &SqlitePool) -> Result<i64> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admins")
        .fetch_one(pool)
        .await
        .context("counting admins")?;
    Ok(count)
}

pub async fn create_admin(pool: &SqlitePool, username: &str, password_hash: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO admins (username, password_hash) VALUES (?1, ?2)",
    )
    .bind(username)
    .bind(password_hash)
    .execute(pool)
    .await
    .context("creating admin")?;
    Ok(())
}

pub async fn find_admin_by_username(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<(i64, String)>> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, password_hash FROM admins WHERE username = ?1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .context("finding admin by username")?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Caller keys
// ---------------------------------------------------------------------------

fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("ab_{}", hex::encode(bytes))
}

fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

pub async fn list_caller_keys(pool: &SqlitePool) -> Result<Vec<CallerKey>> {
    let rows: Vec<(i64, String, String, i64, String)> = sqlx::query_as(
        "SELECT id, name, note, enabled, created_at FROM caller_keys ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .context("listing caller keys")?;

    Ok(rows
        .into_iter()
        .map(|(id, name, note, enabled, created_at)| CallerKey {
            id,
            name,
            note,
            enabled: enabled != 0,
            created_at,
        })
        .collect())
}

pub async fn create_caller_key(
    pool: &SqlitePool,
    name: &str,
    note: &str,
) -> Result<CreateCallerKeyResponse> {
    let token = generate_token();
    let key_hash = hash_token(&token);

    let id: i64 = sqlx::query_scalar(
        "INSERT INTO caller_keys (key_hash, name, note) VALUES (?1, ?2, ?3) RETURNING id",
    )
    .bind(&key_hash)
    .bind(name)
    .bind(note)
    .fetch_one(pool)
    .await
    .context("creating caller key")?;

    let row: (String, i64, String) = sqlx::query_as(
        "SELECT created_at, enabled, note FROM caller_keys WHERE id = ?1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .context("fetching created caller key")?;

    Ok(CreateCallerKeyResponse {
        id,
        name: name.to_string(),
        note: row.2,
        enabled: row.1 != 0,
        created_at: row.0,
        token,
    })
}

pub async fn update_caller_key(
    pool: &SqlitePool,
    id: i64,
    name: &str,
    note: &str,
    enabled: bool,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE caller_keys SET name = ?1, note = ?2, enabled = ?3 WHERE id = ?4",
    )
    .bind(name)
    .bind(note)
    .bind(if enabled { 1 } else { 0 })
    .bind(id)
    .execute(pool)
    .await
    .context("updating caller key")?;

    Ok(result.rows_affected() > 0)
}

pub async fn delete_caller_key(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM caller_keys WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await
        .context("deleting caller key")?;
    Ok(result.rows_affected() > 0)
}

pub async fn find_caller_key_by_token(
    pool: &SqlitePool,
    token: &str,
) -> Result<Option<(i64, bool)>> {
    let key_hash = hash_token(token);
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT id, enabled FROM caller_keys WHERE key_hash = ?1",
    )
    .bind(&key_hash)
    .fetch_optional(pool)
    .await
    .context("finding caller key by token")?;

    Ok(row.map(|(id, enabled)| (id, enabled != 0)))
}

// ---------------------------------------------------------------------------
// Usage logging
// ---------------------------------------------------------------------------

pub struct UsageInsert {
    pub caller_key_id: Option<i64>,
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

pub async fn insert_usage_log(pool: &SqlitePool, usage: UsageInsert) -> Result<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO usage_logs
         (caller_key_id, tag, provider, model, modality, input_tokens, output_tokens, latency_ms, status, error_message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         RETURNING id",
    )
    .bind(usage.caller_key_id)
    .bind(usage.tag)
    .bind(usage.provider)
    .bind(usage.model)
    .bind(usage.modality)
    .bind(usage.input_tokens)
    .bind(usage.output_tokens)
    .bind(usage.latency_ms)
    .bind(usage.status)
    .bind(usage.error_message)
    .fetch_one(pool)
    .await
    .context("inserting usage log")?;
    Ok(id)
}

pub async fn update_usage_tokens(
    pool: &SqlitePool,
    id: i64,
    input_tokens: i64,
    output_tokens: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE usage_logs SET input_tokens = ?1, output_tokens = ?2 WHERE id = ?3",
    )
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(id)
    .execute(pool)
    .await
    .context("updating usage tokens")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Usage aggregation
// ---------------------------------------------------------------------------

pub async fn daily_usage(
    pool: &SqlitePool,
    caller_key_id: Option<i64>,
    from: &str,
    to: &str,
) -> Result<Vec<DailyUsage>> {
    let rows: Vec<(String, Option<i64>, i64, Option<i64>, Option<i64>)> = if let Some(key_id) = caller_key_id {
        sqlx::query_as(
            "SELECT date(timestamp) as day, caller_key_id,
                    COUNT(*) as request_count,
                    COALESCE(SUM(input_tokens), 0) as input_tokens,
                    COALESCE(SUM(output_tokens), 0) as output_tokens
             FROM usage_logs
             WHERE caller_key_id = ?1 AND date(timestamp) >= ?2 AND date(timestamp) <= ?3
             GROUP BY day
             ORDER BY day DESC",
        )
        .bind(key_id)
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .context("querying daily usage by key")?
    } else {
        sqlx::query_as(
            "SELECT date(timestamp) as day, caller_key_id,
                    COUNT(*) as request_count,
                    COALESCE(SUM(input_tokens), 0) as input_tokens,
                    COALESCE(SUM(output_tokens), 0) as output_tokens
             FROM usage_logs
             WHERE date(timestamp) >= ?1 AND date(timestamp) <= ?2
             GROUP BY day, caller_key_id
             ORDER BY day DESC, caller_key_id",
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .context("querying daily usage")?
    };

    let mut result = Vec::new();
    for (day, key_id, request_count, input_tokens, output_tokens) in rows {
        let cost = estimate_cost(pool, key_id, input_tokens.unwrap_or(0), output_tokens.unwrap_or(0)).await?;
        result.push(DailyUsage {
            day,
            caller_key_id: key_id,
            request_count,
            input_tokens: input_tokens.unwrap_or(0),
            output_tokens: output_tokens.unwrap_or(0),
            estimated_cost: cost,
        });
    }
    Ok(result)
}

pub async fn monthly_usage(
    pool: &SqlitePool,
    caller_key_id: Option<i64>,
    year: i32,
    month: i32,
) -> Result<Vec<MonthlyUsage>> {
    let start = format!("{:04}-{:02}-01", year, month);
    let rows: Vec<(String, Option<i64>, i64, Option<i64>, Option<i64>)> = if let Some(key_id) = caller_key_id {
        sqlx::query_as(
            "SELECT strftime('%Y-%m', timestamp) as month, caller_key_id,
                    COUNT(*) as request_count,
                    COALESCE(SUM(input_tokens), 0) as input_tokens,
                    COALESCE(SUM(output_tokens), 0) as output_tokens
             FROM usage_logs
             WHERE caller_key_id = ?1 AND strftime('%Y-%m', timestamp) = ?2
             GROUP BY month",
        )
        .bind(key_id)
        .bind(format!("{:04}-{:02}", year, month))
        .fetch_all(pool)
        .await
        .context("querying monthly usage by key")?
    } else {
        sqlx::query_as(
            "SELECT strftime('%Y-%m', timestamp) as month, caller_key_id,
                    COUNT(*) as request_count,
                    COALESCE(SUM(input_tokens), 0) as input_tokens,
                    COALESCE(SUM(output_tokens), 0) as output_tokens
             FROM usage_logs
             WHERE strftime('%Y-%m', timestamp) = ?1
             GROUP BY month, caller_key_id
             ORDER BY caller_key_id",
        )
        .bind(format!("{:04}-{:02}", year, month))
        .fetch_all(pool)
        .await
        .context("querying monthly usage")?
    };

    let mut result = Vec::new();
    for (month, key_id, request_count, input_tokens, output_tokens) in rows {
        let cost = estimate_cost(pool, key_id, input_tokens.unwrap_or(0), output_tokens.unwrap_or(0)).await?;
        result.push(MonthlyUsage {
            month,
            caller_key_id: key_id,
            request_count,
            input_tokens: input_tokens.unwrap_or(0),
            output_tokens: output_tokens.unwrap_or(0),
            estimated_cost: cost,
        });
    }
    Ok(result)
}

pub async fn usage_summary(pool: &SqlitePool) -> Result<Vec<UsageSummary>> {
    let rows: Vec<(Option<i64>, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT caller_key_id,
                COUNT(*) as request_count,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens
         FROM usage_logs
         GROUP BY caller_key_id
         ORDER BY caller_key_id",
    )
    .fetch_all(pool)
    .await
    .context("querying usage summary")?;

    let mut result = Vec::new();
    for (key_id, request_count, input_tokens, output_tokens) in rows {
        let cost = estimate_cost(pool, key_id, input_tokens.unwrap_or(0), output_tokens.unwrap_or(0)).await?;
        result.push(UsageSummary {
            caller_key_id: key_id,
            request_count,
            input_tokens: input_tokens.unwrap_or(0),
            output_tokens: output_tokens.unwrap_or(0),
            estimated_cost: cost,
        });
    }
    Ok(result)
}

async fn estimate_cost(
    pool: &SqlitePool,
    caller_key_id: Option<i64>,
    input_tokens: i64,
    output_tokens: i64,
) -> Result<f64> {
    // Cost is computed per-provider/model using cost_rates. Without a specific
    // provider/model per aggregate row we cannot accurately cost it. For now,
    // sum per-log costs by joining with cost_rates.
    let total: Option<f64> = if let Some(key_id) = caller_key_id {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(
                (COALESCE(l.input_tokens, 0) / 1000.0) * COALESCE(r.input_price_per_1k, 0) +
                (COALESCE(l.output_tokens, 0) / 1000.0) * COALESCE(r.output_price_per_1k, 0)
            ), 0)
             FROM usage_logs l
             LEFT JOIN cost_rates r ON l.provider = r.provider AND l.model = r.model
             WHERE l.caller_key_id = ?1",
        )
        .bind(key_id)
        .fetch_one(pool)
        .await
        .context("estimating cost by key")?
    } else {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(
                (COALESCE(l.input_tokens, 0) / 1000.0) * COALESCE(r.input_price_per_1k, 0) +
                (COALESCE(l.output_tokens, 0) / 1000.0) * COALESCE(r.output_price_per_1k, 0)
            ), 0)
             FROM usage_logs l
             LEFT JOIN cost_rates r ON l.provider = r.provider AND l.model = r.model",
        )
        .fetch_one(pool)
        .await
        .context("estimating cost")?
    };
    Ok(total.unwrap_or(0.0))
}

// ---------------------------------------------------------------------------
// Cost rates
// ---------------------------------------------------------------------------

pub async fn list_cost_rates(pool: &SqlitePool) -> Result<Vec<CostRate>> {
    let rows: Vec<(i64, String, String, f64, f64)> = sqlx::query_as(
        "SELECT id, provider, model, input_price_per_1k, output_price_per_1k FROM cost_rates ORDER BY provider, model",
    )
    .fetch_all(pool)
    .await
    .context("listing cost rates")?;

    Ok(rows
        .into_iter()
        .map(|(id, provider, model, input_price_per_1k, output_price_per_1k)| CostRate {
            id,
            provider,
            model,
            input_price_per_1k,
            output_price_per_1k,
        })
        .collect())
}

pub async fn set_cost_rate(
    pool: &SqlitePool,
    provider: &str,
    model: &str,
    input_price: f64,
    output_price: f64,
) -> Result<CostRate> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO cost_rates (provider, model, input_price_per_1k, output_price_per_1k)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(provider, model) DO UPDATE SET
            input_price_per_1k = excluded.input_price_per_1k,
            output_price_per_1k = excluded.output_price_per_1k
         RETURNING id",
    )
    .bind(provider)
    .bind(model)
    .bind(input_price)
    .bind(output_price)
    .fetch_one(pool)
    .await
    .context("setting cost rate")?;

    Ok(CostRate {
        id,
        provider: provider.to_string(),
        model: model.to_string(),
        input_price_per_1k: input_price,
        output_price_per_1k: output_price,
    })
}

pub async fn delete_cost_rate(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM cost_rates WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await
        .context("deleting cost rate")?;
    Ok(result.rows_affected() > 0)
}
