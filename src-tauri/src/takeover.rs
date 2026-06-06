use anyhow::{Context, Result};
use serde_json::Value;
use std::path::PathBuf;

fn claude_settings_path() -> Result<PathBuf> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Err(anyhow::anyhow!("no home directory")),
    };
    Ok(home.join(".claude").join("settings.json"))
}

fn claude_settings_backup_path() -> Result<PathBuf> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Err(anyhow::anyhow!("no home directory")),
    };
    Ok(home.join(".claude").join("settings.json.aginxbrain-backup"))
}

/// Environment variable keys that would override aginxbrain's routing.
const ROUTER_OVERRIDE_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
];

pub struct TakeoverStatus {
    pub active: bool,
    pub proxy_url: Option<String>,
}

pub fn take_over_claude(port: u16) -> Result<String> {
    let path = claude_settings_path()?;
    let backup = claude_settings_backup_path()?;

    let mut settings: Value = if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    // Backup original if not already backed up
    if path.exists() && !backup.exists() {
        std::fs::copy(&path, &backup).context("backing up settings.json")?;
    }

    let proxy_url = format!("http://127.0.0.1:{}/anthropic", port);

    // Remove old apiBaseUrl if present (we now use env vars)
    if let Some(obj) = settings.as_object_mut() {
        obj.remove("apiBaseUrl");

        // Ensure env object exists
        if obj.get("env").is_none() {
            obj.insert("env".to_string(), serde_json::json!({}));
        }

        if let Some(env) = obj.get_mut("env").and_then(|e| e.as_object_mut()) {
            // Remove old env vars that would conflict
            for key in ROUTER_OVERRIDE_ENV_KEYS {
                env.remove(*key);
            }

            // Set new env vars for aginxbrain
            env.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                Value::String(proxy_url.clone()),
            );
            env.insert(
                "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
                Value::String("opus".to_string()),
            );
            env.insert(
                "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                Value::String("sonnet".to_string()),
            );
            env.insert(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                Value::String("haiku".to_string()),
            );
            // Set default model to "auto" so Claude Code routes through auto tag
            env.insert(
                "ANTHROPIC_MODEL".to_string(),
                Value::String("auto".to_string()),
            );
            // Add a placeholder API key so Claude Code doesn't show "Not logged in"
            // Actual requests will use provider keys from proxy config
            env.insert(
                "ANTHROPIC_API_KEY".to_string(),
                Value::String("sk-ant-placeholder-aginxbrain".to_string()),
            );
        }
    }

    let content = serde_json::to_string_pretty(&settings)?;
    // Atomic write: temp file then rename, to prevent corruption on crash
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &content)
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("renaming tmp to {}", path.display()))?;

    Ok(proxy_url)
}

pub fn restore_claude() -> Result<()> {
    let path = claude_settings_path()?;
    let backup = claude_settings_backup_path()?;

    if backup.exists() {
        std::fs::copy(&backup, &path)?;
        std::fs::remove_file(&backup)?;
    }

    Ok(())
}

pub fn check_takeover_status(port: u16) -> TakeoverStatus {
    let path = match claude_settings_path() {
        Ok(p) => p,
        Err(_) => {
            return TakeoverStatus { active: false, proxy_url: None };
        }
    };

    if !path.exists() {
        return TakeoverStatus {
            active: false,
            proxy_url: None,
        };
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[Takeover] failed to read Claude settings file: {}", e);
            return TakeoverStatus {
                active: false,
                proxy_url: None,
            }
        }
    };

    let settings: Value = match serde_json::from_str(&content) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("[Takeover] failed to parse Claude settings JSON: {}", e);
            return TakeoverStatus {
                active: false,
                proxy_url: None,
            }
        }
    };

    let expected_url = format!("http://127.0.0.1:{}/anthropic", port);

    // Check env.ANTHROPIC_BASE_URL
    let env_base_url = settings
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Also check legacy apiBaseUrl for backward compatibility
    let legacy_url = settings
        .get("apiBaseUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let active = env_base_url == expected_url || legacy_url == expected_url.trim_end_matches("/anthropic");
    let proxy_url = if active {
        Some(expected_url.clone())
    } else {
        None
    };

    TakeoverStatus { active, proxy_url }
}

// ─── Codex Takeover ───────────────────────────────────────────────

fn codex_dir() -> Result<PathBuf> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Err(anyhow::anyhow!("no home directory")),
    };
    Ok(home.join(".codex"))
}

fn codex_config_path() -> Result<PathBuf> {
    Ok(codex_dir()?.join("config.toml"))
}

fn codex_config_backup_path() -> Result<PathBuf> {
    Ok(codex_dir()?.join("config.toml.aginxbrain-backup"))
}

fn codex_auth_path() -> Result<PathBuf> {
    Ok(codex_dir()?.join("auth.json"))
}

fn codex_auth_backup_path() -> Result<PathBuf> {
    Ok(codex_dir()?.join("auth.json.aginxbrain-backup"))
}

pub fn take_over_codex(port: u16, default_model: &str) -> Result<String> {
    let config_path = codex_config_path()?;
    let config_backup = codex_config_backup_path()?;
    let auth_path = codex_auth_path()?;
    let auth_backup = codex_auth_backup_path()?;
    let codex_dir = codex_dir()?;

    // Ensure ~/.codex directory exists
    std::fs::create_dir_all(&codex_dir)
        .with_context(|| format!("creating {}", codex_dir.display()))?;

    // Backup originals
    if config_path.exists() && !config_backup.exists() {
        std::fs::copy(&config_path, &config_backup)
            .context("backing up codex config.toml")?;
    }
    if auth_path.exists() && !auth_backup.exists() {
        std::fs::copy(&auth_path, &auth_backup)
            .context("backing up codex auth.json")?;
    }

    // No /v1 suffix — Codex appends /v1/responses itself when wire_api = "responses".
    // Matching the working v1 format where base_url = "https://api.deepseek.com" (no /v1).
    let proxy_url = format!("http://127.0.0.1:{}", port);

    // Write config.toml using a bundled Codex model name (e.g. "gpt-5.5") so
    // Codex uses full model metadata (tool parallelization, reasoning summaries,
    // proper truncation, etc.) instead of degraded fallback metadata.  The proxy
    // ignores the model name and routes by current_tag regardless.
    //
    // requires_openai_auth = true + OPENAI_API_KEY in auth.json matches the
    // working v1 format.  Codex sends Authorization: Bearer PROXY_MANAGED with
    // every request; the proxy ignores auth and routes by tag regardless.
    let config_toml = format!(
        r#"model = "{model}"
model_provider = "aginxbrain"
preferred_auth_method = "apikey"
disable_response_storage = true

[model_providers.aginxbrain]
name = "AginxBrain"
base_url = "{url}"
wire_api = "responses"
requires_openai_auth = true
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0
stream_idle_timeout_ms = 600000
"#,
        model = default_model,
        url = proxy_url
    );
    // Atomic write for config.toml
    let config_tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&config_tmp, &config_toml)
        .with_context(|| format!("writing {}", config_tmp.display()))?;
    std::fs::rename(&config_tmp, &config_path)
        .with_context(|| format!("renaming tmp to {}", config_path.display()))?;

    // Write auth.json — OPENAI_API_KEY format (required when requires_openai_auth = true)
    let auth_json = serde_json::json!({
        "OPENAI_API_KEY": "PROXY_MANAGED"
    });
    let auth_content = serde_json::to_string_pretty(&auth_json)?;
    // Atomic write for auth.json
    let auth_tmp = auth_path.with_extension("json.tmp");
    std::fs::write(&auth_tmp, &auth_content)
        .with_context(|| format!("writing {}", auth_tmp.display()))?;
    std::fs::rename(&auth_tmp, &auth_path)
        .with_context(|| format!("renaming tmp to {}", auth_path.display()))?;

    log::info!("[Takeover] Codex config written: base_url={}", proxy_url);
    Ok(proxy_url)
}

pub fn restore_codex() -> Result<()> {
    let config_path = codex_config_path()?;
    let config_backup = codex_config_backup_path()?;
    let auth_path = codex_auth_path()?;
    let auth_backup = codex_auth_backup_path()?;

    if config_backup.exists() {
        std::fs::copy(&config_backup, &config_path)?;
        std::fs::remove_file(&config_backup)?;
    } else if config_path.exists() {
        // File was created from scratch (no pre-existing backup), delete it
        std::fs::remove_file(&config_path)?;
    }
    if auth_backup.exists() {
        std::fs::copy(&auth_backup, &auth_path)?;
        std::fs::remove_file(&auth_backup)?;
    } else if auth_path.exists() {
        // File was created from scratch (no pre-existing backup), delete it
        std::fs::remove_file(&auth_path)?;
    }

    log::info!("[Takeover] Codex config restored");
    Ok(())
}

pub fn check_codex_takeover_status(port: u16) -> TakeoverStatus {
    let config_path = match codex_config_path() {
        Ok(p) => p,
        Err(_) => {
            return TakeoverStatus { active: false, proxy_url: None };
        }
    };

    if !config_path.exists() {
        return TakeoverStatus {
            active: false,
            proxy_url: None,
        };
    }

    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => {
            return TakeoverStatus {
                active: false,
                proxy_url: None,
            }
        }
    };

    let expected_url = format!("http://127.0.0.1:{}", port);

    // Simple string matching — we wrote this file ourselves
    let active = content.contains(&expected_url);
    let proxy_url = if active {
        Some(expected_url)
    } else {
        None
    };

    TakeoverStatus { active, proxy_url }
}
