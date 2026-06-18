-- Store the caller key token in plaintext so admins can retrieve and
-- redistribute keys to users from the dashboard. The key_hash column is kept
-- for authentication lookups; existing rows have NULL tokens (irrecoverable,
-- created before this migration).
ALTER TABLE caller_keys ADD COLUMN token TEXT;
