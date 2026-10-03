-- Per-API-key default thinking tier (BRAIN-CODEX.md §4.5).
-- NULL = unset (global default applies). Values: 'none' | 'low' | 'medium' | 'high'.
-- Precedence: request param > per-key default > global default — never force.
ALTER TABLE caller_keys ADD COLUMN default_thinking TEXT;
