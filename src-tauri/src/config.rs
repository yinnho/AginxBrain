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
    #[serde(default = "default_modality")]
    pub modality: String,
    pub timestamp: String,
    #[serde(default)]
    pub caller_key_name: Option<String>,
    #[serde(default)]
    pub input_tokens: Option<i64>,
    #[serde(default)]
    pub output_tokens: Option<i64>,
    #[serde(default)]
    pub latency_ms: i64,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
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
    #[serde(default = "default_management_key")]
    pub management_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub endpoint: String,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_format")]
    pub format: ProviderFormat,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_modality")]
    pub modality: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub is_auto: bool,
}

fn default_port() -> u16 {
    8083
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_modality() -> String {
    "chat".to_string()
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
    m.insert("deepseek".into(), Provider { name: "DeepSeek".into(), base_url: "https://api.deepseek.com".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("deepseek_anthropic".into(), Provider { name: "DeepSeek (Anthropic)".into(), base_url: "https://api.deepseek.com/anthropic".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("zhipu".into(), Provider { name: "Zhipu GLM".into(), base_url: "https://open.bigmodel.cn/api/anthropic".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("baidu".into(), Provider { name: "Baidu ERNIE".into(), base_url: "https://qianfan.baidubce.com/anthropic/coding".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("kimi".into(), Provider { name: "Kimi".into(), base_url: "https://api.kimi.com/coding".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("dashscope".into(), Provider { name: "Qwen (DashScope)".into(), base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(), api_key: key.clone(), auth_type: AuthType::Bearer });
    m.insert("minimax".into(), Provider { name: "MiniMax".into(), base_url: "https://api.minimaxi.com/anthropic".into(), api_key: key, auth_type: AuthType::Bearer });
    m
}

fn default_routes() -> Vec<Route> {
    vec![
        Route { endpoint: "/v1/chat/completions".into(), model: "deepseek-v4-pro".into(), provider: "deepseek".into(), tags: vec!["sonnet".into(), "auto".into()], format: ProviderFormat::Openai, enabled: true, modality: default_modality() },
        Route { endpoint: "/v1/chat/completions".into(), model: "deepseek-v4-flash".into(), provider: "deepseek".into(), tags: vec!["haiku".into()], format: ProviderFormat::Openai, enabled: true, modality: default_modality() },
        Route { endpoint: "/v1/messages".into(), model: "glm-5.1".into(), provider: "zhipu".into(), tags: vec!["opus".into()], format: ProviderFormat::Anthropic, enabled: true, modality: default_modality() },
        Route { endpoint: "/v1/chat/completions".into(), model: "qwen3.7-max".into(), provider: "dashscope".into(), tags: vec!["sonnet".into()], format: ProviderFormat::Openai, enabled: true, modality: default_modality() },
        Route { endpoint: "/v1/messages".into(), model: "K2.6".into(), provider: "kimi".into(), tags: vec!["sonnet".into()], format: ProviderFormat::Anthropic, enabled: true, modality: default_modality() },
        Route { endpoint: "/v1/messages".into(), model: "MiniMax-M3".into(), provider: "minimax".into(), tags: vec!["haiku".into()], format: ProviderFormat::Anthropic, enabled: true, modality: default_modality() },
        // Example: route popular Codex model names directly. Add more tags (gpt-5.4, etc.) as needed.
        Route { endpoint: "/v1/chat/completions".into(), model: "deepseek-v4-pro".into(), provider: "deepseek".into(), tags: vec!["gpt-5.5".into(), "codex".into()], format: ProviderFormat::Openai, enabled: true, modality: default_modality() },
    ]
}

fn default_tags() -> Vec<Tag> {
    vec![
        Tag { name: "opus".into(), color: "#A855F7".into(), is_auto: false },
        Tag { name: "sonnet".into(), color: "#3B82F6".into(), is_auto: false },
        Tag { name: "haiku".into(), color: "#22C55E".into(), is_auto: false },
        Tag { name: "auto".into(), color: "#F59E0B".into(), is_auto: true },
        // Popular client model names can be added as tags without code changes.
        // When a request arrives with model="gpt-5.5", it resolves directly to
        // the "gpt-5.5" tag, and the route below routes it to DeepSeek.
        Tag { name: "gpt-5.5".into(), color: "#10B981".into(), is_auto: false },
        Tag { name: "codex".into(), color: "#6366F1".into(), is_auto: false },
        // Multimodal tags: OpenCarrier sends model="<tag>" for non-chat
        // capabilities (POST /v1/chat/completions), routed by modality.
        Tag { name: "image".into(), color: "#10B981".into(), is_auto: false },
        Tag { name: "tts".into(), color: "#F59E0B".into(), is_auto: false },
        Tag { name: "vision".into(), color: "#EC4899".into(), is_auto: false },
        Tag { name: "audio".into(), color: "#06B6D4".into(), is_auto: false },
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
    let mut config: AppConfig = serde_yaml::from_str(&content)
        .with_context(|| format!("parsing {}", path.display()))?;

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

    // Atomic write: write to temp file then rename
    let tmp_path = path.with_extension("yaml.tmp");
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
}

impl AppState {
    pub async fn new(config: AppConfig) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3600))
            .build()
            .map_err(|e| anyhow::anyhow!("failed to create HTTP client: {}", e))?;
        let db = crate::db::init_db().await?;
        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            http_client,
            db,
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
    fn test_route_default_modality_is_chat() {
        let yaml = r#"
endpoint: /v1/chat/completions
model: test-model
provider: test-provider
tags: [sonnet]
format: openai
enabled: true
"#;
        let route: Route = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(route.modality, "chat");
    }

    #[test]
    fn test_default_routes_are_chat_modality() {
        for route in default_routes() {
            assert_eq!(route.modality, "chat");
        }
    }

    #[test]
    fn test_management_key_default() {
        assert_eq!(default_management_key(), "aginxbrain-local");
    }
}
