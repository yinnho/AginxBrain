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

/// Result of checking whether a takeover is active.
#[derive(Debug)]
pub struct TakeoverStatus {
    pub active: bool,
    pub proxy_url: Option<String>,
}

/// Write ~/.claude/settings.json so Claude Code routes through the local proxy
/// at `127.0.0.1:{port}/anthropic`. Returns the proxy URL on success.
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

/// Restore ~/.claude/settings.json from the backup created during takeover.
pub fn restore_claude() -> Result<()> {
    let path = claude_settings_path()?;
    let backup = claude_settings_backup_path()?;

    if backup.exists() {
        std::fs::copy(&backup, &path)?;
        std::fs::remove_file(&backup)?;
    }

    Ok(())
}

/// Check whether Claude Code is currently pointing at the local proxy on `port`.
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

/// Write ~/.codex/config.toml + auth.json so Codex routes through the local
/// proxy at `http://127.0.0.1:{port}` using the given model name.
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

/// Restore Codex config from backup (deletes files created from scratch).
pub fn restore_codex() -> Result<()> {
    let config_path = codex_config_path()?;
    let config_backup = codex_config_backup_path()?;
    let auth_path = codex_auth_path()?;
    let auth_backup = codex_auth_backup_path()?;

    if config_backup.exists() {
        std::fs::copy(&config_backup, &config_path)?;
        std::fs::remove_file(&config_backup)?;
    } else if config_path.exists() {
        std::fs::remove_file(&config_path)?;
    }
    if auth_backup.exists() {
        std::fs::copy(&auth_backup, &auth_path)?;
        std::fs::remove_file(&auth_backup)?;
    } else if auth_path.exists() {
        std::fs::remove_file(&auth_path)?;
    }

    log::info!("[Takeover] Codex config restored");
    Ok(())
}

/// Check whether Codex is currently pointing at the local proxy on `port`.
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

// ─── Remote takeover (desktop client → hosted brain.aginx.net) ──────────────
//
// Unlike the local-proxy takeover above, these write config files that point
// Claude Code / Codex directly at a remote AginxBrain server (e.g.
// https://brain.aginx.net), embedding the caller API key. Used by the desktop
// client's toggle switches. Feature-gated to desktop only.

/// Marker stored alongside the takeover to detect whether the current config
/// was written by a remote takeover (so status checks don't depend on a port).
#[cfg(feature = "desktop")]
const REMOTE_MARKER_PREFIX: &str = "# aginxbrain-remote=";

#[cfg(feature = "desktop")]
fn remote_marker(server: &str) -> String {
    format!("{}{}", REMOTE_MARKER_PREFIX, server)
}

/// Write ~/.claude/settings.json so Claude Code routes through a remote
/// AginxBrain server (e.g. https://brain.aginx.net) with the given API key.
#[cfg(feature = "desktop")]
pub fn take_over_claude_remote(server: &str, api_key: &str) -> Result<bool> {
    let path = claude_settings_path()?;
    let backup = claude_settings_backup_path()?;

    let mut settings: Value = if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    if path.exists() && !backup.exists() {
        std::fs::copy(&path, &backup).context("backing up settings.json")?;
    }

    let base_url = format!("{}/anthropic", server.trim_end_matches('/'));

    if let Some(obj) = settings.as_object_mut() {
        obj.remove("apiBaseUrl");
        if obj.get("env").is_none() {
            obj.insert("env".to_string(), serde_json::json!({}));
        }
        if let Some(env) = obj.get_mut("env").and_then(|e| e.as_object_mut()) {
            for key in ROUTER_OVERRIDE_ENV_KEYS {
                env.remove(*key);
            }
            env.insert("ANTHROPIC_BASE_URL".to_string(), Value::String(base_url));
            env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), Value::String("opus".to_string()));
            env.insert("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), Value::String("sonnet".to_string()));
            env.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), Value::String("haiku".to_string()));
            env.insert("ANTHROPIC_MODEL".to_string(), Value::String("auto".to_string()));
            // The real caller key — Claude Code sends it as x-api-key / bearer,
            // the remote server authenticates the caller with it.
            env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), Value::String(api_key.to_string()));
        }
        // Stash a marker so we can detect remote takeover without a port.
        obj.insert("__aginxbrain_remote".to_string(), Value::String(server.to_string()));
    }

    let content = serde_json::to_string_pretty(&settings)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &content).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("renaming {}", path.display()))?;

    Ok(true)
}

/// Check whether ~/.claude/settings.json is currently pointing at a remote
/// AginxBrain server (any server, not just localhost).
#[cfg(feature = "desktop")]
pub fn check_claude_remote_active() -> bool {
    let path = match claude_settings_path() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // Active if our marker is present, OR if env.ANTHROPIC_BASE_URL points at
    // a brain server (not the localhost port form).
    if content.contains("\"__aginxbrain_remote\"") {
        return true;
    }
    let settings: Value = match serde_json::from_str(&content) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let base = settings
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    base.starts_with("https://") && base.contains("aginx")
}

/// Write ~/.codex/config.toml + auth.json so Codex routes through a remote
/// AginxBrain server with the given API key.
#[cfg(feature = "desktop")]
pub fn take_over_codex_remote(server: &str, api_key: &str) -> Result<bool> {
    let config_path = codex_config_path()?;
    let config_backup = codex_config_backup_path()?;
    let auth_path = codex_auth_path()?;
    let auth_backup = codex_auth_backup_path()?;
    let codex_dir = codex_dir()?;

    std::fs::create_dir_all(&codex_dir)
        .with_context(|| format!("creating {}", codex_dir.display()))?;

    if config_path.exists() && !config_backup.exists() {
        std::fs::copy(&config_path, &config_backup).context("backing up codex config.toml")?;
    }
    if auth_path.exists() && !auth_backup.exists() {
        std::fs::copy(&auth_path, &auth_backup).context("backing up codex auth.json")?;
    }

    let base_url = server.trim_end_matches('/');

    let config_toml = format!(
        r#"{marker}
model = "gpt-5.5"
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
        marker = remote_marker(base_url),
        url = base_url
    );
    let config_tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&config_tmp, &config_toml).with_context(|| format!("writing {}", config_tmp.display()))?;
    std::fs::rename(&config_tmp, &config_path).with_context(|| format!("renaming {}", config_path.display()))?;

    // auth.json: real caller key under OPENAI_API_KEY (Codex sends it as
    // Authorization: Bearer <key>).
    let auth_json = serde_json::json!({ "OPENAI_API_KEY": api_key });
    let auth_content = serde_json::to_string_pretty(&auth_json)?;
    let auth_tmp = auth_path.with_extension("json.tmp");
    std::fs::write(&auth_tmp, &auth_content).with_context(|| format!("writing {}", auth_tmp.display()))?;
    std::fs::rename(&auth_tmp, &auth_path).with_context(|| format!("renaming {}", auth_path.display()))?;

    Ok(true)
}

/// Check whether ~/.codex/config.toml currently routes through a remote
/// AginxBrain server.
#[cfg(feature = "desktop")]
pub fn check_codex_remote_active() -> bool {
    let config_path = match codex_config_path() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    content.starts_with(REMOTE_MARKER_PREFIX)
}

// ─── Claude Desktop takeover (the desktop app's Code tab) ───────────────────
//
// The Claude desktop app does NOT read ~/.claude/settings.json env vars —
// that mechanism is CLI/VS Code only. Its Code tab routes through the app's
// "third-party inference" (3p) configuration instead, which normally lives
// behind Developer → Configure Third-Party Inference (itself gated on
// developer mode). All of it is plain JSON on disk, so we write the same
// files directly:
//
//   <userData>/Claude/developer_settings.json     {"allowDevTools": true}
//   <userData>/Claude-3p/configLibrary/<id>.json  the gateway config
//   <userData>/Claude-3p/configLibrary/_meta.json appliedId → the entry
//
// (<userData> is Electron's app.getPath("userData"): ~/Library/Application
// Support on macOS, %APPDATA% on Windows, ~/.config on Linux — dirs::data_dir
// matches all three.) The app reads the config at launch, so it must be
// restarted (or launched, if quit) after takeover. When 3p mode activates the
// app splits into a separate "Claude-3p" profile (own data dir and logs at
// ~/Library/Logs/Claude-3p/main.log).

/// Stable id for our configLibrary entry, so re-running takeover edits our
/// own entry instead of piling up new ones.
#[cfg(feature = "desktop")]
const CLAUDE_DESKTOP_ENTRY_ID: &str = "6b9f2a4e-8c1d-4a7b-9e3f-2c5d8a1b4e6f";

#[cfg(feature = "desktop")]
fn claude_desktop_profile_dir(profile: &str) -> Result<PathBuf> {
    let data = match dirs::data_dir() {
        Some(d) => d,
        None => return Err(anyhow::anyhow!("no data directory")),
    };
    Ok(data.join(profile))
}

/// Write the developer-mode flag + 3p gateway config so the Claude desktop
/// app's Code tab routes through a remote AginxBrain server. The app picks
/// the config up at next launch.
#[cfg(feature = "desktop")]
pub fn take_over_claude_desktop_remote(server: &str, api_key: &str) -> Result<bool> {
    // 1. developer_settings.json (main profile) — gates the Developer menu.
    //    Merged, not replaced: only force allowDevTools on, keep other keys.
    let dev_path = claude_desktop_profile_dir("Claude")?.join("developer_settings.json");
    let dev_backup = dev_path.with_extension("json.aginxbrain-backup");
    if let Some(parent) = dev_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut dev_settings: Value = if dev_path.exists() {
        if !dev_backup.exists() {
            std::fs::copy(&dev_path, &dev_backup)
                .context("backing up developer_settings.json")?;
        }
        serde_json::from_str(&std::fs::read_to_string(&dev_path)?)?
    } else {
        serde_json::json!({})
    };
    if let Some(obj) = dev_settings.as_object_mut() {
        obj.insert("allowDevTools".to_string(), Value::Bool(true));
    }
    atomic_write_json(&dev_path, &dev_settings)?;

    // 2. configLibrary entry (3p profile) — the actual gateway config.
    let lib_dir = claude_desktop_profile_dir("Claude-3p")?.join("configLibrary");
    std::fs::create_dir_all(&lib_dir)
        .with_context(|| format!("creating {}", lib_dir.display()))?;
    let meta_path = lib_dir.join("_meta.json");
    // One-time backup of whatever the user configured through the app's form.
    if meta_path.exists() {
        let meta_backup = lib_dir.join("_meta.json.aginxbrain-backup");
        if !meta_backup.exists() {
            std::fs::copy(&meta_path, &meta_backup)
                .context("backing up configLibrary _meta.json")?;
        }
    }

    let base_url = format!("{}/anthropic", server.trim_end_matches('/'));
    let entry = serde_json::json!({
        // Base URL, NOT the full endpoint: the app appends /v1/messages and
        // /v1/models itself (entering an endpoint URL silently breaks model
        // discovery).
        "inferenceGatewayBaseUrl": base_url,
        "inferenceGatewayApiKey": api_key,
        "inferenceGatewayAuthScheme": "x-api-key",
        "inferenceProvider": "gateway",
        "inferenceCredentialKind": "static",
    });
    let entry_path = lib_dir.join(format!("{CLAUDE_DESKTOP_ENTRY_ID}.json"));
    atomic_write_json(&entry_path, &entry)?;

    // 3. _meta.json — keep existing entries, add ours, point appliedId at it.
    let mut meta: Value = if meta_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&meta_path)?).unwrap_or_else(|_| {
            serde_json::json!({ "appliedId": null, "entries": [] })
        })
    } else {
        serde_json::json!({ "appliedId": null, "entries": [] })
    };
    if let Some(obj) = meta.as_object_mut() {
        obj.insert(
            "appliedId".to_string(),
            Value::String(CLAUDE_DESKTOP_ENTRY_ID.to_string()),
        );
        let our_entry = serde_json::json!({
            "id": CLAUDE_DESKTOP_ENTRY_ID,
            "name": "AginxBrain",
        });
        if let Some(entries) = obj
            .get_mut("entries")
            .and_then(|e| e.as_array_mut())
        {
            entries.retain(|e| e.get("id").and_then(|i| i.as_str()) != Some(CLAUDE_DESKTOP_ENTRY_ID));
            entries.push(our_entry);
        } else {
            obj.insert("entries".to_string(), serde_json::json!([our_entry]));
        }
    }
    atomic_write_json(&meta_path, &meta)?;

    log::info!(
        "[Takeover] Claude Desktop 3p config written: base_url={}",
        base_url
    );
    Ok(true)
}

/// Check whether the Claude desktop app's 3p config points at an AginxBrain
/// server (the applied configLibrary entry's gateway URL).
#[cfg(feature = "desktop")]
pub fn check_claude_desktop_remote_active() -> bool {
    let lib_dir = match claude_desktop_profile_dir("Claude-3p") {
        Ok(d) => d.join("configLibrary"),
        Err(_) => return false,
    };
    let meta: Value = match std::fs::read_to_string(lib_dir.join("_meta.json"))
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
    {
        Some(m) => m,
        None => return false,
    };
    let applied = meta
        .get("appliedId")
        .and_then(|i| i.as_str())
        .unwrap_or_default();
    let entry: Value = match std::fs::read_to_string(lib_dir.join(format!("{applied}.json")))
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
    {
        Some(e) => e,
        None => return false,
    };
    let base = entry
        .get("inferenceGatewayBaseUrl")
        .and_then(|u| u.as_str())
        .unwrap_or_default();
    entry.get("inferenceProvider").and_then(|p| p.as_str()) == Some("gateway")
        && base.starts_with("http")
        && base.contains("aginx")
}

/// Restore the Claude desktop app's 3p config from the backups made during
/// takeover (or drop our entry when there was nothing to back up).
#[cfg(feature = "desktop")]
pub fn restore_claude_desktop_remote() -> Result<()> {
    // developer_settings.json: restore the pre-takeover copy if we made one.
    let dev_path = claude_desktop_profile_dir("Claude")?.join("developer_settings.json");
    let dev_backup = dev_path.with_extension("json.aginxbrain-backup");
    if dev_backup.exists() {
        std::fs::copy(&dev_backup, &dev_path)?;
        std::fs::remove_file(&dev_backup)?;
    }

    let lib_dir = claude_desktop_profile_dir("Claude-3p")?.join("configLibrary");
    let meta_path = lib_dir.join("_meta.json");
    let meta_backup = lib_dir.join("_meta.json.aginxbrain-backup");
    if meta_backup.exists() {
        std::fs::copy(&meta_backup, &meta_path)?;
        std::fs::remove_file(&meta_backup)?;
    } else {
        // Nothing to restore: drop our entry and rewrite _meta without it.
        let entry_path = lib_dir.join(format!("{CLAUDE_DESKTOP_ENTRY_ID}.json"));
        let _ = std::fs::remove_file(&entry_path);
        if let Ok(content) = std::fs::read_to_string(&meta_path) {
            if let Ok(mut meta) = serde_json::from_str::<Value>(&content) {
                if let Some(obj) = meta.as_object_mut() {
                    if obj
                        .get("appliedId")
                        .and_then(|i| i.as_str())
                        == Some(CLAUDE_DESKTOP_ENTRY_ID)
                    {
                        obj.insert("appliedId".to_string(), Value::Null);
                    }
                    if let Some(entries) = obj.get_mut("entries").and_then(|e| e.as_array_mut()) {
                        entries.retain(|e| {
                            e.get("id").and_then(|i| i.as_str()) != Some(CLAUDE_DESKTOP_ENTRY_ID)
                        });
                    }
                    atomic_write_json(&meta_path, &meta)?;
                }
            }
        }
    }

    log::info!("[Takeover] Claude Desktop 3p config restored");
    Ok(())
}

/// Serialize pretty JSON and write atomically (tmp file + rename).
#[cfg(feature = "desktop")]
fn atomic_write_json(path: &std::path::Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &content)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("renaming tmp to {}", path.display()))?;
    Ok(())
}
