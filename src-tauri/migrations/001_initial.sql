CREATE TABLE IF NOT EXISTS admins (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS caller_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS usage_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    caller_key_id INTEGER REFERENCES caller_keys(id),
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    tag TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    modality TEXT NOT NULL DEFAULT 'chat',
    input_tokens INTEGER,
    output_tokens INTEGER,
    latency_ms INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'success',
    error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_usage_logs_caller_key_id ON usage_logs(caller_key_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_timestamp ON usage_logs(timestamp);

CREATE TABLE IF NOT EXISTS cost_rates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input_price_per_1k REAL NOT NULL DEFAULT 0,
    output_price_per_1k REAL NOT NULL DEFAULT 0,
    UNIQUE(provider, model)
);
