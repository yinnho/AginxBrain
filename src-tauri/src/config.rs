use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Recent request log entry.
#[derive(Debug, Clone, Serialize)]
pub struct RequestLog {
    pub request_model: String,
    pub tag: String,
    pub provider: String,
    pub target_model: String,
    pub modality: String,
    pub timestamp: String,
    pub caller_key_name: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub latency_ms: i64,
    pub cost: f64,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_providers")]
    pub providers: HashMap<String, Provider>,
    #[serde(default = "default_routes")]
    pub routes: Vec<Route>,
    #[serde(default = "default_tags")]
    pub tags: Vec<Tag>,
    #[serde(default = "default_tag")]
    pub current_tag: String,
    /// Legacy field: kept for config import/export compatibility.
    /// Admin auth is now handled exclusively via session-based login.
    #[serde(default = "default_management_key")]
    pub management_key: String,
    /// Smart auto-routing configuration.
    #[serde(default)]
    pub smart_routing: crate::smart_routing::SmartRoutingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    pub api_key: String,
    #[serde(default = "default_auth_type")]
    pub auth_type: AuthType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    Bearer,
    XApiKey,
    XGoogApiKey,
}

impl AuthType {
    pub fn header_name(&self) -> &str {
        match self {
            AuthType::Bearer => "authorization",
            AuthType::XApiKey => "x-api-key",
            AuthType::XGoogApiKey => "x-goog-api-key",
        }
    }

    pub fn header_value(&self, api_key: &str) -> String {
        match self {
            AuthType::Bearer => format!("Bearer {}", api_key),
            _ => api_key.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFormat {
    Anthropic,
    Openai,
    #[serde(rename = "openai_responses")]
    OpenaiResponses,
    #[serde(rename = "openai_images")]
    OpenaiImages,
    #[serde(rename = "dashscope_image")]
    DashscopeImage,
    #[serde(rename = "dashscope_video")]
    DashscopeVideo,
    #[serde(rename = "dashscope_tts")]
    DashscopeTts,
    #[serde(rename = "dashscope_asr")]
    DashscopeAsr,
    #[serde(rename = "dashscope_chat_image")]
    DashscopeChatImage,
    Kling,
    #[serde(rename = "minimax_image")]
    MinimaxImage,
}

fn default_format() -> ProviderFormat {
    ProviderFormat::Openai
}

fn default_enabled() -> bool {
    true
}

impl ProviderFormat {
    /// Return the standard API path for this format (e.g. "/v1/chat/completions" for Openai).
    pub fn path(&self) -> &'static str {
        match self {
            ProviderFormat::Openai | ProviderFormat::DashscopeAsr => "/v1/chat/completions",
            ProviderFormat::Anthropic => "/v1/messages",
            ProviderFormat::OpenaiResponses => "/v1/responses",
            ProviderFormat::OpenaiImages => "/v1/images/generations",
            ProviderFormat::DashscopeImage => "/api/v1/services/aigc/multimodal-generation/generation",
            ProviderFormat::DashscopeChatImage => "/chat/completions",
            ProviderFormat::DashscopeVideo => "/api/v1/services/aigc/video-generation/video-synthesis",
            ProviderFormat::DashscopeTts => "/api/v1/services/aigc/text-to-speech/stream",
            ProviderFormat::Kling => "/v1/videos/text2video",
            ProviderFormat::MinimaxImage => "/v1/image_generation",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    /// Stable unique ID (auto-generated). Survives reordering/insertion/deletion.
    #[serde(default = "generate_route_id")]
    pub id: String,
    /// Upstream server base URL (e.g. "https://api.deepseek.com"). The request
    /// path is derived from `format` via `ProviderFormat::path()`.
    #[serde(default)]
    pub base_url: String,
    /// Optional WebSocket URL. Used by dashscope_tts/dashscope_asr formats that
    /// require WebSocket instead of HTTP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_url: Option<String>,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_format")]
    pub format: ProviderFormat,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn generate_route_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Simple unique ID: timestamp + random suffix
    format!("r_{:x}_{:04}", ts, rand_digit())
}

fn rand_digit() -> u16 {
    // Use a simple hash of the timestamp for pseudo-randomness (no external rand dep)
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (ts % 10000) as u16
}

/// Generate a stable route ID (public for use in api.rs).
pub fn new_route_id() -> String {
    generate_route_id()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub is_auto: bool,
    /// Route priority map: key = route ID, value = priority
    /// (lower = tried first). Routes not listed come last in config order.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub route_priority: HashMap<String, u32>,
}

impl Tag {
    pub fn new(name: &str, color: &str, is_auto: bool) -> Self {
        Self { name: name.to_string(), color: color.to_string(), is_auto, route_priority: HashMap::new() }
    }
}

fn default_port() -> u16 {
    8083
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_tag() -> String {
    "auto".to_string()
}

fn default_management_key() -> String {
    "aginxbrain-local".to_string()
}

fn default_auth_type() -> AuthType {
    AuthType::Bearer
}

fn default_providers() -> HashMap<String, Provider> {
    let mut m = HashMap::new();
    let key = "your-key-here".to_string();
    m.insert("deepseek".into(), Provider { name: "DeepSeek".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("zhipu".into(), Provider { name: "Zhipu GLM".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("baidu".into(), Provider { name: "Baidu ERNIE".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("kimi".into(), Provider { name: "Kimi".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("dashscope".into(), Provider { name: "Qwen (DashScope)".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("dashscope_media".into(), Provider { name: "DashScope Media".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("minimax".into(), Provider { name: "MiniMax".into(), api_key: key, auth_type: AuthType::Bearer });
    m
}

fn default_routes() -> Vec<Route> {
    vec![
        Route { id: "r_default_1".into(), base_url: "https://api.deepseek.com".into(), ws_url: None, model: "deepseek-v4-pro".into(), provider: "deepseek".into(), tags: vec!["sonnet".into(), "auto".into()], format: ProviderFormat::Openai, enabled: true },
        Route { id: "r_default_2".into(), base_url: "https://api.deepseek.com".into(), ws_url: None, model: "deepseek-v4-flash".into(), provider: "deepseek".into(), tags: vec!["haiku".into()], format: ProviderFormat::Openai, enabled: true },
        Route { id: "r_default_3".into(), base_url: "https://open.bigmodel.cn/api/anthropic".into(), ws_url: None, model: "glm-5.1".into(), provider: "zhipu".into(), tags: vec!["opus".into()], format: ProviderFormat::Anthropic, enabled: true },
        Route { id: "r_default_4".into(), base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(), ws_url: None, model: "qwen3.7-max".into(), provider: "dashscope".into(), tags: vec!["sonnet".into()], format: ProviderFormat::Openai, enabled: true },
        Route { id: "r_default_5".into(), base_url: "https://api.kimi.com/coding".into(), ws_url: None, model: "K2.6".into(), provider: "kimi".into(), tags: vec!["sonnet".into()], format: ProviderFormat::Anthropic, enabled: true },
        Route { id: "r_default_6".into(), base_url: "https://api.minimaxi.com/anthropic".into(), ws_url: None, model: "MiniMax-M3".into(), provider: "minimax".into(), tags: vec!["haiku".into()], format: ProviderFormat::Anthropic, enabled: true },
        Route { id: "r_default_7".into(), base_url: "https://api.deepseek.com".into(), ws_url: None, model: "deepseek-v4-pro".into(), provider: "deepseek".into(), tags: vec!["gpt-5.5".into(), "codex".into()], format: ProviderFormat::Openai, enabled: true },
    ]
}

fn default_tags() -> Vec<Tag> {
    vec![
        Tag { name: "opus".into(), color: "#A855F7".into(), is_auto: false, route_priority: HashMap::new() },
        Tag { name: "sonnet".into(), color: "#3B82F6".into(), is_auto: false, route_priority: HashMap::new() },
        Tag { name: "haiku".into(), color: "#22C55E".into(), is_auto: false, route_priority: HashMap::new() },
        Tag { name: "auto".into(), color: "#F59E0B".into(), is_auto: true, route_priority: HashMap::new() },
        // Popular client model names can be added as tags without code changes.
        // When a request arrives with model="gpt-5.5", it resolves directly to
        // the "gpt-5.5" tag, and the route below routes it to DeepSeek.
        Tag { name: "gpt-5.5".into(), color: "#10B981".into(), is_auto: false, route_priority: HashMap::new() },
        Tag { name: "codex".into(), color: "#6366F1".into(), is_auto: false, route_priority: HashMap::new() },
        // Multimodal tags: OpenCarrier sends model="<tag>" for non-chat
        // capabilities (POST /v1/chat/completions), routed by format.
        Tag { name: "image".into(), color: "#10B981".into(), is_auto: false, route_priority: HashMap::new() },
        Tag { name: "tts".into(), color: "#F59E0B".into(), is_auto: false, route_priority: HashMap::new() },
        Tag { name: "vision".into(), color: "#EC4899".into(), is_auto: false, route_priority: HashMap::new() },
        Tag { name: "audio".into(), color: "#06B6D4".into(), is_auto: false, route_priority: HashMap::new() },
    ]
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            providers: default_providers(),
            routes: default_routes(),
            tags: default_tags(),
            current_tag: default_tag(),
            management_key: default_management_key(),
            smart_routing: Default::default(),
        }
    }
}

pub fn config_path() -> Result<PathBuf> {
    // Allow override via AGINXBRAIN_CONFIG environment variable
    if let Ok(path) = std::env::var("AGINXBRAIN_CONFIG") {
        if !path.is_empty() {
            log::info!("[Config] using config path from AGINXBRAIN_CONFIG: {}", path);
            return Ok(PathBuf::from(path));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".aginxbrain").join("config.yaml"))
}

/// Migrate old config format where providers carried `base_url`/`ws_url` and
/// routes had `endpoint`. New format puts `base_url` (and optional `ws_url`)
/// on each route and removes them from providers. Returns Some(yaml_string) if
/// migration was performed, None if the config is already up-to-date.
fn migrate_v0_config(raw_yaml: &str) -> Option<String> {
    use serde_json::Value;
    let mut doc: Value = serde_json::from_str(raw_yaml).ok()?;

    // Check if any route still uses `endpoint` (old format marker)
    let needs_migration = doc.get("routes")?.as_array()?
        .iter().any(|r| r.get("endpoint").is_some());
    if !needs_migration {
        return None;
    }

    // Extract provider base_urls before mutation (avoids borrow conflict)
    let provider_urls: std::collections::HashMap<String, (String, Option<String>)> = doc
        .get("providers")
        .and_then(|p| p.as_object())
        .map(|provs| {
            provs.iter().map(|(k, v)| {
                let base = v.get("base_url").and_then(|b| b.as_str()).unwrap_or("").to_string();
                let ws = v.get("ws_url").and_then(|b| b.as_str()).map(String::from);
                (k.clone(), (base, ws))
            }).collect()
        })
        .unwrap_or_default();

    // Migrate each route: base_url = provider.base_url, ws_url = provider.ws_url
    if let Some(routes_arr) = doc.get_mut("routes").and_then(|r| r.as_array_mut()) {
        for route in routes_arr.iter_mut() {
            let provider_id = route.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let (prov_base, prov_ws) = provider_urls.get(provider_id)
                .cloned()
                .unwrap_or_default();

            // Set base_url from provider's base_url
            if let Some(obj) = route.as_object_mut() {
                obj.insert("base_url".to_string(), Value::String(prov_base));
                obj.remove("endpoint");

                // Migrate ws_url from provider (only set if not already present on route)
                if !obj.contains_key("ws_url") {
                    if let Some(ws) = &prov_ws {
                        obj.insert("ws_url".to_string(), Value::String(ws.clone()));
                    }
                }
            }
        }
    }

    // Remove base_url and ws_url from providers
    if let Some(provs) = doc.get_mut("providers").and_then(|p| p.as_object_mut()) {
        for (_, prov) in provs.iter_mut() {
            if let Some(obj) = prov.as_object_mut() {
                obj.remove("base_url");
                obj.remove("ws_url");
            }
        }
    }

    serde_yaml::to_string(&doc).ok()
}

pub fn load_config() -> Result<AppConfig> {
    let path = config_path()?;
    log::info!("[Config] loading config from {}", path.display());
    if !path.exists() {
        log::info!("[Config] config file not found, creating with defaults");
        let config = AppConfig::default();
        // Persist defaults so the user can see and edit them
        if let Err(e) = save_config(&config) {
            log::warn!("[Config] failed to save default config: {}", e);
        }
        return Ok(config);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;

    // Migrate: old config format (provider.base_url + route.endpoint → route.base_url)
    let mut config: AppConfig = match migrate_v0_config(&content) {
        Some(migrated_yaml) => {
            log::info!("[Config] migrated from v0 format (provider.base_url + route.endpoint → route.base_url)");
            let c: AppConfig = serde_yaml::from_str(&migrated_yaml)
                .with_context(|| format!("parsing migrated config from {}", path.display()))?;
            if let Err(e) = save_config(&c) {
                log::warn!("[Config] failed to save migrated config: {}", e);
            }
            c
        }
        None => serde_yaml::from_str(&content)
            .with_context(|| format!("parsing {}", path.display()))?,
    };

    // Backfill defaults for empty fields (e.g. user upgraded from an older
    // version that had explicit empty arrays).  Serde defaults only apply
    // when a field is *missing*, not when it's present-but-empty.
    let mut dirty = false;
    if config.providers.is_empty() {
        config.providers = default_providers();
        dirty = true;
    }
    if config.routes.is_empty() {
        config.routes = default_routes();
        dirty = true;
    }
    if config.tags.is_empty() {
        config.tags = default_tags();
        dirty = true;
    }
    if config.management_key.is_empty() {
        config.management_key = default_management_key();
        dirty = true;
    }

    // Migrate: assign stable IDs to routes that lack them (upgraded from older version)
    for route in &mut config.routes {
        if route.id.trim().is_empty() {
            route.id = generate_route_id();
            dirty = true;
        }
    }
    // Migrate: convert index-based route_priority keys to route ID keys
    for tag in &mut config.tags {
        let mut new_priority = HashMap::new();
        let mut changed = false;
        for (key, value) in &tag.route_priority {
            // If key is a numeric index, resolve to the route's ID
            if let Ok(idx) = key.parse::<usize>() {
                if let Some(route) = config.routes.get(idx) {
                    new_priority.insert(route.id.clone(), *value);
                    changed = true;
                }
            } else {
                // Already a route ID, keep as-is
                new_priority.insert(key.clone(), *value);
            }
        }
        if changed {
            tag.route_priority = new_priority;
            dirty = true;
        }
    }
    if dirty {
        log::info!("[Config] backfilling empty fields with defaults");
        if let Err(e) = save_config(&config) {
            log::warn!("[Config] failed to save backfilled config: {}", e);
        }
    }

    log::info!("[Config] loaded current_tag={}", config.current_tag);
    Ok(config)
}

pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_yaml::to_string(config)?;

    // Atomic write: write to temp file then rename.
    // Use PID + timestamp to avoid collisions if multiple processes write concurrently.
    let tmp_path = path.with_extension(format!("yaml.tmp.{}.{}", std::process::id(), std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()));
    std::fs::write(&tmp_path, &content)
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()))?;

    Ok(())
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub http_client: reqwest::Client,
    pub db: sqlx::SqlitePool,
    pub smart_routing_cache: Arc<RwLock<crate::smart_routing::SessionCache>>,
}

impl AppState {
    pub async fn new(config: AppConfig) -> Result<Self> {
        let cache_max = config.smart_routing.cache_max_sessions;
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3600))
            .build()
            .map_err(|e| anyhow::anyhow!("failed to create HTTP client: {}", e))?;
        let db = crate::db::init_db().await?;
        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            http_client,
            db,
            smart_routing_cache: Arc::new(RwLock::new(
                crate::smart_routing::SessionCache::new(cache_max),
            )),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_has_sensible_values() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.port, 8083);
        assert_eq!(cfg.current_tag, "auto");
        assert_eq!(cfg.management_key, "aginxbrain-local");
        assert!(!cfg.providers.is_empty());
        assert!(!cfg.routes.is_empty());
        assert!(!cfg.tags.is_empty());
        assert_eq!(cfg.tags.len(), 10); // opus, sonnet, haiku, auto, gpt-5.5, codex, image, tts, vision, audio
    }

    #[test]
    fn test_auth_type_header_name() {
        assert_eq!(AuthType::Bearer.header_name(), "authorization");
        assert_eq!(AuthType::XApiKey.header_name(), "x-api-key");
        assert_eq!(AuthType::XGoogApiKey.header_name(), "x-goog-api-key");
    }

    #[test]
    fn test_auth_type_header_value() {
        assert_eq!(AuthType::Bearer.header_value("key123"), "Bearer key123");
        assert_eq!(AuthType::XApiKey.header_value("key123"), "key123");
        assert_eq!(AuthType::XGoogApiKey.header_value("key123"), "key123");
    }

    #[test]
    fn test_provider_format_serde() {
        let yaml = "openai";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::Openai);

        let yaml = "openai_responses";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::OpenaiResponses);

        let yaml = "dashscope_image";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::DashscopeImage);

        let yaml = "dashscope_chat_image";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::DashscopeChatImage);

        let yaml = "dashscope_asr";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::DashscopeAsr);

        let yaml = "dashscope_video";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::DashscopeVideo);

        let yaml = "kling";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::Kling);

        let yaml = "minimax_image";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::MinimaxImage);

        let yaml = "anthropic";
        let fmt: ProviderFormat = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fmt, ProviderFormat::Anthropic);
    }

    #[test]
    fn test_default_format_is_openai() {
        assert_eq!(default_format(), ProviderFormat::Openai);
    }

    #[test]
    fn test_management_key_default() {
        assert_eq!(default_management_key(), "aginxbrain-local");
    }
}
