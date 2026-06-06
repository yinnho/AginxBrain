# AginxBrain 多模态 Brain 架构设计

> 目标：把 AginxBrain 从“LLM 路由器”升级为“Agent 的统一 AI 能力网关”。
>
> 这里的图片、视频、TTS、LLM、Brain 相关设计可以直接复用 OpenCarrier 中已有代码。两边都是自有代码，因此后续实现时可以直接复制核心类型和 driver 逻辑，再按 AginxBrain 当前的 Tauri/Axum/proxy 架构做适配。

## 1. 产品定位

AginxBrain 不应该只做：

```text
Claude/Codex → model-router → 上游 LLM
```

它应该做：

```text
Agent / IDE / App / Server
  → AginxBrain
  → chat / reasoning / code / vision / image / video / audio / tts
  → provider endpoints with fallback / health / quota / auth
```

也就是说，AginxBrain 是 Agent 的 AI 大脑：

- 聊天与推理能力
- 代码能力
- 图片理解能力
- 图片生成能力
- 视频生成能力
- 语音/TTS/ASR 能力
- 多 provider、多 endpoint、多 fallback
- 统一的配置、鉴权、日志、计费、限额和健康状态

## 2. 当前 AginxBrain 与目标架构的关系

当前 AginxBrain 已经有三层雏形：

```text
Provider
Route
Tag
```

当前配置大致是：

```rust
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub auth_type: AuthType,
}

pub struct Route {
    pub endpoint: String,
    pub model: String,
    pub provider: String,
    pub tags: Vec<String>,
    pub format: ProviderFormat,
    pub enabled: bool,
}

pub struct Tag {
    pub name: String,
    pub color: String,
    pub is_auto: bool,
}
```

这套适合文本 LLM：

```text
client model name → tag(opus/sonnet/haiku/auto) → route
```

但它还不够表达图片、视频、TTS 这类能力。后续应增加 `modality/capability` 概念：

```text
client model/capability name
  → modality
  → tag/tier
  → endpoint fallback chain
```

例如：

```text
gpt-5.5
  → chat
  → opus
  → [glm-5.1, deepseek-v4-pro, qwen-max]

aginx-image
  → image_generation
  → default
  → [dashscope_wanx, openai_image, minimax_image]

aginx-video
  → video_generation
  → default
  → [kling_video, dashscope_video]
```

## 3. 推荐的三层 Brain 架构

参考 OpenCarrier，AginxBrain 后续可调整为：

```text
Provider  = 凭证身份
Endpoint  = 完整可调用单元
Modality  = 能力路由 + fallback chain
```

### 3.1 Provider

Provider 只表示“凭证和认证方式”，不绑定具体 model/url/format。

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Environment variable name holding the API key.
    /// If empty, provider may not require authentication, e.g. Ollama.
    #[serde(default)]
    pub api_key_env: String,

    /// Authentication type: apikey, jwt, etc.
    #[serde(default = "default_auth_type")]
    pub auth_type: String,

    /// Additional credential parameters.
    /// Example: Kling uses access_key_env + secret_key_env.
    #[serde(default)]
    pub params: HashMap<String, String>,
}

fn default_auth_type() -> String {
    "apikey".to_string()
}
```

AginxBrain 当前 provider 里包含 `base_url`。短期可以保留；长期建议把 `base_url` 下沉到 endpoint。

### 3.2 Endpoint

Endpoint 是一个完整可调用单元：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointConfig {
    /// Provider name — used to look up credentials.
    pub provider: String,

    /// Model identifier.
    pub model: String,

    /// Complete API endpoint URL.
    pub base_url: String,

    /// Protocol or media driver format.
    #[serde(default = "default_format")]
    pub format: ApiFormat,

    /// OpenAI/Azure compatible auth header style.
    #[serde(default)]
    pub auth_header: AuthHeaderType,

    /// Whether this endpoint can be used.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_format() -> ApiFormat {
    ApiFormat::OpenAI
}

fn default_enabled() -> bool {
    true
}
```

Endpoint 比当前 `Route` 语义更清楚。当前 Route 可以逐步迁移成 Endpoint。

### 3.3 Modality

Modality 表达“能力”：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModalityConfig {
    /// Primary endpoint name.
    pub primary: String,

    /// Fallback endpoints, tried in order.
    #[serde(default)]
    pub fallbacks: Vec<String>,

    /// Human-readable description.
    #[serde(default)]
    pub description: String,
}
```

常见 modality：

```text
chat
reasoning
code
vision
image_generation
video_generation
tts
asr
embedding
rerank
```

## 4. API Format / Driver Format

当前 AginxBrain 有：

```rust
pub enum ProviderFormat {
    Anthropic,
    Openai,
    OpenaiResponses,
}
```

后续建议扩展为：

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiFormat {
    #[default]
    OpenAI,
    Anthropic,
    Gemini,

    #[serde(rename = "openai_responses")]
    OpenAIResponses,

    #[serde(rename = "openai_images")]
    OpenAIImages,

    #[serde(rename = "dashscope_image")]
    DashScopeImage,

    #[serde(rename = "dashscope_video")]
    DashScopeVideo,

    #[serde(rename = "dashscope_tts")]
    DashScopeTts,

    Kling,

    #[serde(rename = "minimax_image")]
    MiniMaxImage,
}
```

UI 可展示支持格式：

```rust
pub const SUPPORTED_FORMATS: &[&str] = &[
    "openai",
    "anthropic",
    "openai_responses",
    "gemini",
    "openai_images",
    "dashscope_image",
    "dashscope_video",
    "dashscope_tts",
    "kling",
    "minimax_image",
];
```

## 5. 统一消息中间层：ContentBlock

AginxBrain 现在主要做协议互转。后续支持多模态时，不应该每种协议直接互转，而应该统一成内部消息结构：

```text
Anthropic / OpenAI / Responses / Gemini request
  → BrainMessage / ContentBlock
  → target provider request
```

推荐结构：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<serde_json::Value>,
    },

    #[serde(rename = "image")]
    Image {
        media_type: String,
        data: String,
    },

    #[serde(rename = "audio")]
    Audio {
        media_type: String,
        data: String,
    },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<serde_json::Value>,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        tool_name: String,
        content: String,
        is_error: bool,
    },

    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
    },

    #[serde(other)]
    Unknown,
}
```

### 5.1 OpenAI 图片输入转换

```rust
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum OaiContentPart {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "image_url")]
    ImageUrl { image_url: OaiImageUrl },

    #[serde(rename = "input_audio")]
    InputAudio { input_audio: OaiInputAudio },
}

#[derive(Debug, Serialize)]
struct OaiImageUrl {
    url: String,
}

#[derive(Debug, Serialize)]
struct OaiInputAudio {
    data: String,
    format: String,
}
```

图片 block 转 OpenAI-compatible：

```rust
ContentBlock::Image { data, media_type, .. } => {
    parts.push(OaiContentPart::ImageUrl {
        image_url: OaiImageUrl {
            url: format!("data:{media_type};base64,{data}"),
        },
    });
}
```

### 5.2 Anthropic 图片输入转换

```rust
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ApiContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "image")]
    Image { source: ApiImageSource },

    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: serde_json::Value },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Debug, Serialize)]
struct ApiImageSource {
    #[serde(rename = "type")]
    source_type: String,
    media_type: String,
    data: String,
}
```

图片 block 转 Anthropic：

```rust
ContentBlock::Image { data, media_type, .. } => {
    Some(ApiContentBlock::Image {
        source: ApiImageSource {
            source_type: "base64".to_string(),
            media_type: media_type.clone(),
            data: data.clone(),
        },
    })
}
```

### 5.3 Gemini 图片输入转换

```rust
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum GeminiPart {
    Text { text: String },
    InlineData { inline_data: GeminiInlineData },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiInlineData {
    mime_type: String,
    data: String,
}
```

图片 block 转 Gemini：

```rust
ContentBlock::Image { data, media_type, .. } => {
    Some(GeminiPart::InlineData {
        inline_data: GeminiInlineData {
            mime_type: media_type.clone(),
            data: data.clone(),
        },
    })
}
```

## 6. 统一 CompletionRequest / CompletionResponse

为了让文字、图片、视频、TTS 都能通过同一个 brain 调用，建议引入内部请求/响应：

```rust
#[derive(Debug, Clone, Default)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub system: Option<String>,
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct CompletionResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub media: Option<MediaOutput>,
}
```

普通 LLM：

```rust
CompletionResponse {
    content: vec![ContentBlock::Text { ... }],
    media: None,
    ..
}
```

图片生成：

```rust
CompletionResponse {
    content: vec![],
    media: Some(MediaOutput::Images { items }),
    ..
}
```

视频生成：

```rust
CompletionResponse {
    content: vec![],
    media: Some(MediaOutput::Video { url, cover_url }),
    ..
}
```

## 7. MediaOutput

推荐类型：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    pub data_base64: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaOutput {
    Audio {
        data: Vec<u8>,
        format: String,
        duration_ms: u64,
    },

    Image {
        data: Vec<u8>,
        format: String,
    },

    Images {
        items: Vec<GeneratedImage>,
    },

    AsyncTask {
        task_id: String,
        endpoint_id: String,
    },

    Video {
        url: String,
        cover_url: Option<String>,
    },
}
```

## 8. 图片生成 Driver

### 8.1 OpenAI Images

```rust
async fn complete_openai_images(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
    let prompt = Self::extract_prompt(&request);
    if prompt.is_empty() {
        return Err(LlmError::Api {
            status: 400,
            message: "Image generation requires a prompt".to_string(),
        });
    }

    let n = request.extra.get("n").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let mut body = serde_json::json!({
        "model": request.model,
        "prompt": prompt,
        "n": n,
    });

    if let Some(size) = request.extra.get("size").and_then(|v| v.as_str()) {
        body["size"] = serde_json::Value::String(size.to_string());
    }

    let resp = self.send_request(&self.base_url, &body, &[]).await?;
    let result: serde_json::Value = resp.json().await.map_err(|e| LlmError::Parse(e.to_string()))?;

    let mut images = Vec::new();
    if let Some(data) = result.get("data").and_then(|d| d.as_array()) {
        for item in data {
            let url = item.get("url").and_then(|u| u.as_str()).map(String::from);
            let b64 = item.get("b64_json").and_then(|b| b.as_str()).unwrap_or("").to_string();
            if url.is_none() && b64.is_empty() {
                continue;
            }
            images.push(GeneratedImage { data_base64: b64, url });
        }
    }

    if images.is_empty() {
        return Err(LlmError::Parse("No images in response".to_string()));
    }

    Ok(CompletionResponse {
        media: Some(MediaOutput::Images { items: images }),
        ..Default::default()
    })
}
```

### 8.2 DashScope Image

```rust
async fn complete_dashscope_image(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
    let prompt = Self::extract_prompt(&request);
    if prompt.is_empty() {
        return Err(LlmError::Api {
            status: 400,
            message: "Image generation requires a prompt".to_string(),
        });
    }

    let size_raw = request.extra.get("size").and_then(|v| v.as_str()).unwrap_or("1280*1280");
    let size = size_raw.replace('x', "*");
    let n = request.extra.get("n").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

    let body = serde_json::json!({
        "model": request.model,
        "input": {
            "messages": [{
                "role": "user",
                "content": [{ "text": prompt }]
            }]
        },
        "parameters": {
            "prompt_extend": true,
            "watermark": false,
            "n": n,
            "size": size
        }
    });

    let resp = self.send_request(&self.base_url, &body, &[]).await?;
    let result: serde_json::Value = resp.json().await.map_err(|e| LlmError::Parse(e.to_string()))?;

    if let Some(code) = result.get("code").and_then(|c| c.as_str()) {
        if code != "Success" && code != "200" {
            let msg = result.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error");
            return Err(LlmError::Api {
                status: 400,
                message: format!("DashScope image error ({code}): {msg}"),
            });
        }
    }

    let mut images = Vec::new();

    if let Some(choices) = result.pointer("/output/choices").and_then(|r| r.as_array()) {
        for choice in choices {
            if let Some(content) = choice.pointer("/message/content").and_then(|c| c.as_array()) {
                for block in content {
                    if let Some(url) = block.get("image").and_then(|i| i.as_str()) {
                        images.push(GeneratedImage {
                            data_base64: String::new(),
                            url: Some(url.to_string()),
                        });
                    }
                }
            }
        }
    }

    if images.is_empty() {
        if let Some(results) = result.pointer("/output/results").and_then(|r| r.as_array()) {
            for item in results {
                let url = item.get("url").and_then(|u| u.as_str()).map(|s| s.to_string());
                let b64 = item.get("b64_image").and_then(|b| b.as_str()).unwrap_or("").to_string();
                images.push(GeneratedImage { data_base64: b64, url });
            }
        }
    }

    if images.is_empty() {
        if let Some(data) = result.pointer("/output/data").and_then(|r| r.as_array()) {
            for item in data {
                let url = item.get("url").and_then(|u| u.as_str()).map(|s| s.to_string());
                let b64 = item.get("b64_json").and_then(|b| b.as_str()).unwrap_or("").to_string();
                images.push(GeneratedImage { data_base64: b64, url });
            }
        }
    }

    if images.is_empty() {
        return Err(LlmError::Parse("No images in DashScope response".to_string()));
    }

    Ok(CompletionResponse {
        media: Some(MediaOutput::Images { items: images }),
        ..Default::default()
    })
}
```

### 8.3 MiniMax Image

```rust
async fn complete_minimax_image(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
    let prompt = Self::extract_prompt(&request);
    if prompt.is_empty() {
        return Err(LlmError::Api {
            status: 400,
            message: "Image generation requires a prompt".to_string(),
        });
    }

    let n = request.extra.get("n").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let mut body = serde_json::json!({
        "model": request.model,
        "prompt": prompt,
        "n": n,
        "response_format": "url"
    });

    if let Some(ar) = request.extra.get("aspect_ratio").and_then(|v| v.as_str()) {
        body["aspect_ratio"] = serde_json::Value::String(ar.to_string());
    }
    if let Some(po) = request.extra.get("prompt_optimizer").and_then(|v| v.as_bool()) {
        body["prompt_optimizer"] = serde_json::Value::Bool(po);
    }
    if let Some(seed) = request.extra.get("seed").and_then(|v| v.as_i64()) {
        body["seed"] = serde_json::Value::Number(serde_json::Number::from(seed));
    }

    let resp = self.send_request(&self.base_url, &body, &[]).await?;
    let result: serde_json::Value = resp.json().await.map_err(|e| LlmError::Parse(e.to_string()))?;

    let mut images = Vec::new();
    if let Some(data) = result.get("data") {
        if let Some(urls) = data.get("image_urls").and_then(|u| u.as_array()) {
            for url_val in urls {
                if let Some(url) = url_val.as_str() {
                    images.push(GeneratedImage {
                        data_base64: String::new(),
                        url: Some(url.to_string()),
                    });
                }
            }
        }
        if let Some(b64s) = data.get("image_base64").and_then(|b| b.as_array()) {
            for b64_val in b64s {
                if let Some(b64) = b64_val.as_str() {
                    images.push(GeneratedImage {
                        data_base64: b64.to_string(),
                        url: None,
                    });
                }
            }
        }
    }

    if images.is_empty() {
        if let Some(data) = result.get("data").and_then(|d| d.as_array()) {
            for item in data {
                let url = item.get("url").and_then(|u| u.as_str()).map(String::from);
                let b64 = item.get("b64_json").and_then(|b| b.as_str()).unwrap_or("").to_string();
                if url.is_none() && b64.is_empty() {
                    continue;
                }
                images.push(GeneratedImage { data_base64: b64, url });
            }
        }
    }

    if images.is_empty() {
        return Err(LlmError::Parse("No images in MiniMax response".to_string()));
    }

    Ok(CompletionResponse {
        media: Some(MediaOutput::Images { items: images }),
        ..Default::default()
    })
}
```

## 9. 视频生成 Driver

视频生成一般是异步任务。AginxBrain 应该支持两种模式：

1. 同步等待：submit → poll → 返回最终视频
2. 异步任务：submit → 返回 task_id → 前端/API 轮询

OpenCarrier 当前实现是同步等待。AginxBrain 后续更适合两者都支持。

### 9.1 通用轮询函数

```rust
enum PollStatus {
    Completed(serde_json::Value),
    Failed(String),
    Pending,
}

async fn poll_until_complete(
    &self,
    poll_url: &str,
    check_status: impl Fn(&serde_json::Value) -> PollStatus,
) -> Result<serde_json::Value, LlmError> {
    let max_duration = std::time::Duration::from_secs(300);
    let interval = std::time::Duration::from_secs(5);
    let start = std::time::Instant::now();

    loop {
        tokio::time::sleep(interval).await;
        if start.elapsed() > max_duration {
            return Err(LlmError::Api {
                status: 0,
                message: "Task polling timed out".to_string(),
            });
        }

        let resp = self.send_get(poll_url).await?;
        let result: serde_json::Value = resp.json().await.map_err(|e| LlmError::Parse(e.to_string()))?;

        match check_status(&result) {
            PollStatus::Completed(data) => return Ok(data),
            PollStatus::Failed(msg) => {
                return Err(LlmError::Api { status: 0, message: msg });
            }
            PollStatus::Pending => continue,
        }
    }
}
```

### 9.2 DashScope Video

```rust
async fn complete_dashscope_video(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
    let prompt = Self::extract_prompt(&request);
    if prompt.is_empty() {
        return Err(LlmError::Api {
            status: 400,
            message: "Video generation requires a prompt".to_string(),
        });
    }

    let extra = &request.extra;
    let extra_input = extra.get("input").and_then(|v| v.as_object());
    let extra_params = extra.get("parameters").and_then(|v| v.as_object());
    let resolution = extra_params
        .and_then(|p| p.get("resolution"))
        .and_then(|v| v.as_str())
        .unwrap_or("720P");
    let duration = extra_params
        .and_then(|p| p.get("duration"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5);

    let mut input = serde_json::json!({ "prompt": prompt });
    if let Some(img_url) = extra_input.and_then(|i| i.get("img_url")).and_then(|v| v.as_str()) {
        input["img_url"] = serde_json::Value::String(img_url.to_string());
    }

    let body = serde_json::json!({
        "model": request.model,
        "input": input,
        "parameters": {
            "resolution": resolution,
            "duration": duration
        }
    });

    let resp = self
        .send_request(&self.base_url, &body, &[("X-DashScope-Async", "enable")])
        .await?;

    let submit_result: serde_json::Value = resp.json().await.map_err(|e| LlmError::Parse(e.to_string()))?;

    let task_id = submit_result
        .pointer("/output/task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::Parse("No task_id in DashScope video response".to_string()))?;

    let poll_url = format!("https://dashscope.aliyuncs.com/api/v1/tasks/{task_id}");

    let result = self
        .poll_until_complete(&poll_url, |v| {
            let status = v.pointer("/output/task_status").and_then(|v| v.as_str()).unwrap_or("");
            match status {
                "SUCCEEDED" => PollStatus::Completed(v.clone()),
                "FAILED" => {
                    let msg = v.pointer("/output/message").and_then(|v| v.as_str()).unwrap_or("Unknown error");
                    PollStatus::Failed(msg.to_string())
                }
                _ => PollStatus::Pending,
            }
        })
        .await?;

    let video_url = result
        .pointer("/output/video_url")
        .or_else(|| result.pointer("/output/results/0/url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::Parse("No video URL in completed task".to_string()))?
        .to_string();

    let cover_url = result.pointer("/output/cover_url").and_then(|v| v.as_str()).map(String::from);

    Ok(CompletionResponse {
        media: Some(MediaOutput::Video { url: video_url, cover_url }),
        ..Default::default()
    })
}
```

### 9.3 Kling Video / Image

Kling 用 JWT 认证，任务结果可能是视频，也可能是图片。

```rust
async fn complete_kling(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
    let prompt = Self::extract_prompt(&request);
    if prompt.is_empty() {
        return Err(LlmError::Api {
            status: 400,
            message: "Kling requires a prompt".to_string(),
        });
    }

    let mut body = serde_json::json!({
        "model": request.model,
        "prompt": prompt,
    });

    if let Some(obj) = request.extra.as_object() {
        for (k, v) in obj {
            body[k] = v.clone();
        }
    }

    let resp = self.send_request(&self.base_url, &body, &[]).await?;
    let submit_result: serde_json::Value = resp.json().await.map_err(|e| LlmError::Parse(e.to_string()))?;

    if let Some(code) = submit_result.get("code").and_then(|c| c.as_i64()) {
        if code != 0 {
            let msg = submit_result.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error");
            return Err(LlmError::Api { status: 400, message: msg.to_string() });
        }
    }

    let task_id = submit_result
        .pointer("/data/task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::Parse("No task_id in Kling response".to_string()))?;

    let poll_url = format!("{}/{task_id}", self.base_url);

    let result = self
        .poll_until_complete(&poll_url, |v| {
            let status = v.pointer("/data/task_status").and_then(|v| v.as_str()).unwrap_or("");
            match status {
                "succeed" => PollStatus::Completed(v.clone()),
                "failed" => {
                    let msg = v.pointer("/data/task_status_msg").and_then(|v| v.as_str()).unwrap_or("Unknown error");
                    PollStatus::Failed(msg.to_string())
                }
                _ => PollStatus::Pending,
            }
        })
        .await?;

    let task_result = result.pointer("/data/task_result").and_then(|v| v.as_array());
    if let Some(items) = task_result {
        if let Some(url) = items.first().and_then(|i| i.get("url")).and_then(|u| u.as_str()) {
            let cover_url = items
                .first()
                .and_then(|i| i.get("cover_url"))
                .and_then(|u| u.as_str())
                .map(String::from);

            return Ok(CompletionResponse {
                media: Some(MediaOutput::Video { url: url.to_string(), cover_url }),
                ..Default::default()
            });
        }

        if let Some(images_arr) = items.first().and_then(|i| i.get("images")).and_then(|v| v.as_array()) {
            let mut images = Vec::new();
            for img in images_arr {
                let url = img.get("url").and_then(|u| u.as_str()).map(String::from);
                let b64 = img.get("b64_json").and_then(|b| b.as_str()).unwrap_or("").to_string();
                images.push(GeneratedImage { data_base64: b64, url });
            }

            return Ok(CompletionResponse {
                media: Some(MediaOutput::Images { items: images }),
                ..Default::default()
            });
        }
    }

    Err(LlmError::Parse("No video/images in Kling task result".to_string()))
}
```

## 10. TTS Driver

TTS 也可以走同一个 `CompletionRequest` / `CompletionResponse`。

```rust
async fn complete_dashscope_tts(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
    let text = Self::extract_query(&request);
    if text.is_empty() {
        return Err(LlmError::Api {
            status: 400,
            message: "TTS requires text input".to_string(),
        });
    }

    let voice = request
        .extra
        .get("voice")
        .and_then(|v| v.as_str())
        .unwrap_or("Cherry")
        .to_string();

    let body = serde_json::json!({
        "model": request.model,
        "input": {
            "text": text,
            "voice": voice
        }
    });

    let resp = self.send_request(&self.base_url, &body, &[]).await?;
    let result: serde_json::Value = resp.json().await.map_err(|e| LlmError::Parse(e.to_string()))?;

    if let Some(code) = result.get("code").and_then(|c| c.as_str()) {
        if code != "Success" && code != "200" {
            let msg = result.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error");
            return Err(LlmError::Api {
                status: 400,
                message: format!("DashScope TTS error ({code}): {msg}"),
            });
        }
    }

    let audio_url = result
        .pointer("/output/audio")
        .or_else(|| result.pointer("/output/results/0/url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::Parse("No audio URL in DashScope TTS response".to_string()))?
        .to_string();

    let audio_resp = self
        .client
        .get(&audio_url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| LlmError::Http(format!("Audio download failed: {e}")))?;

    let data = audio_resp
        .bytes()
        .await
        .map_err(|e| LlmError::Http(format!("Audio download read failed: {e}")))?;

    let duration_ms = {
        let word_count = text.split_whitespace().count() as u64;
        (word_count * 400).max(500)
    };

    Ok(CompletionResponse {
        media: Some(MediaOutput::Audio {
            data: data.to_vec(),
            format: "mp3".to_string(),
            duration_ms,
        }),
        ..Default::default()
    })
}
```

## 11. Brain 运行时：Driver 缓存 + 健康状态 + 熔断

OpenCarrier 的 Brain 运行时非常适合 AginxBrain 生产化。

推荐结构：

```rust
pub struct Brain {
    config: BrainConfig,
    drivers: DashMap<String, Arc<dyn LlmDriver>>,
    health: DashMap<String, EndpointTracker>,
    failed_endpoints: DashSet<String>,
}
```

### 11.1 Endpoint health tracker

```rust
const CIRCUIT_BREAKER_THRESHOLD: u32 = 3;
const CIRCUIT_BREAKER_COOLDOWN_MS: u64 = 60_000;

struct EndpointTracker {
    success_count: AtomicU64,
    failure_count: AtomicU64,
    total_latency_ms: AtomicU64,
    latency_count: AtomicU64,
    consecutive_failures: AtomicU32,
    last_failure_at: AtomicU64,
}

impl EndpointTracker {
    fn new() -> Self {
        Self {
            success_count: AtomicU64::new(0),
            failure_count: AtomicU64::new(0),
            total_latency_ms: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            last_failure_at: AtomicU64::new(0),
        }
    }

    fn record_success(&self, latency_ms: u64) {
        self.success_count.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        if latency_ms > 0 {
            self.total_latency_ms.fetch_add(latency_ms, Ordering::Relaxed);
            self.latency_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_failure(&self, latency_ms: u64) {
        self.failure_count.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        if latency_ms > 0 {
            self.total_latency_ms.fetch_add(latency_ms, Ordering::Relaxed);
            self.latency_count.fetch_add(1, Ordering::Relaxed);
        }
        self.last_failure_at.store(now_ms(), Ordering::Relaxed);
    }

    fn is_available(&self) -> bool {
        let consec = self.consecutive_failures.load(Ordering::Relaxed);
        if consec < CIRCUIT_BREAKER_THRESHOLD {
            return true;
        }

        let last = self.last_failure_at.load(Ordering::Relaxed);
        let elapsed = now_ms().saturating_sub(last);
        elapsed >= CIRCUIT_BREAKER_COOLDOWN_MS
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

### 11.2 Modality → endpoint chain

```rust
pub fn endpoints_for(&self, modality: &str) -> Vec<ResolvedEndpoint> {
    let mod_config = self
        .config
        .modalities
        .get(modality)
        .or_else(|| self.config.modalities.get(&self.config.default_modality));

    let Some(mod_config) = mod_config else {
        return vec![];
    };

    let mut chain = vec![mod_config.primary.clone()];
    chain.extend(mod_config.fallbacks.iter().cloned());

    chain
        .into_iter()
        .filter_map(|name| {
            let endpoint = self.config.endpoints.get(&name)?;

            if !endpoint.enabled {
                return None;
            }

            if self.get_or_create_driver(&name).is_none() {
                return None;
            }

            if let Some(tracker) = self.health.get(&name) {
                if !tracker.is_available() {
                    return None;
                }
            }

            Some(ResolvedEndpoint {
                id: name,
                model: endpoint.model.clone(),
                provider: endpoint.provider.clone(),
            })
        })
        .collect()
}
```

### 11.3 Brain complete with fallback

```rust
async fn complete(
    &self,
    modality: &str,
    mut request: CompletionRequest,
) -> Result<CompletionResponse, BrainError> {
    let endpoints = self.endpoints_for(modality);
    if endpoints.is_empty() {
        return Err(BrainError::NoEndpointAvailable(modality.to_string()));
    }

    let mut last_error: Option<String> = None;

    for ep in endpoints {
        let Some(driver) = self.driver_for_endpoint(&ep.id) else {
            continue;
        };

        request.model = ep.model.clone();
        let start = std::time::Instant::now();

        match driver.complete(request.clone()).await {
            Ok(response) => {
                self.report(EndpointReport {
                    endpoint_id: ep.id,
                    success: true,
                    latency_ms: start.elapsed().as_millis() as u64,
                    error: None,
                });
                return Ok(response);
            }
            Err(e) => {
                let err = e.to_string();
                self.report(EndpointReport {
                    endpoint_id: ep.id,
                    success: false,
                    latency_ms: start.elapsed().as_millis() as u64,
                    error: Some(err.clone()),
                });
                last_error = Some(err);
            }
        }
    }

    Err(BrainError::AllEndpointsFailed(last_error.unwrap_or_default()))
}
```

## 12. 配置示例

长期配置可以是 JSON/YAML。YAML 更适合用户编辑：

```yaml
port: 8083
host: 127.0.0.1
management_key: aginxbrain-local

providers:
  deepseek:
    name: DeepSeek
    api_key: ${DEEPSEEK_API_KEY}
    auth_type: bearer

  dashscope:
    name: DashScope
    api_key: ${DASHSCOPE_API_KEY}
    auth_type: bearer

  kling:
    name: Kling
    auth_type: jwt
    params:
      access_key_env: KLING_ACCESS_KEY
      secret_key_env: KLING_SECRET_KEY

endpoints:
  deepseek_chat:
    provider: deepseek
    model: deepseek-v4-pro
    base_url: https://api.deepseek.com/v1/chat/completions
    format: openai
    enabled: true

  qwen_chat:
    provider: dashscope
    model: qwen-max
    base_url: https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions
    format: openai
    enabled: true

  dashscope_image:
    provider: dashscope
    model: wanx2.1-t2i-plus
    base_url: https://dashscope.aliyuncs.com/api/v1/services/aigc/text2image/image-synthesis
    format: dashscope_image
    enabled: true

  dashscope_video:
    provider: dashscope
    model: wanx2.1-i2v-turbo
    base_url: https://dashscope.aliyuncs.com/api/v1/services/aigc/video-generation/video-synthesis
    format: dashscope_video
    enabled: true

  kling_video:
    provider: kling
    model: kling-v1-6
    base_url: https://api.klingai.com/v1/videos/text2video
    format: kling
    enabled: true

modalities:
  chat:
    description: General LLM chat
    primary: deepseek_chat
    fallbacks:
      - qwen_chat

  image_generation:
    description: Text to image generation
    primary: dashscope_image
    fallbacks: []

  video_generation:
    description: Text/image to video generation
    primary: kling_video
    fallbacks:
      - dashscope_video

tags:
  opus:
    modality: chat
    primary: deepseek_chat
    fallbacks:
      - qwen_chat

  sonnet:
    modality: chat
    primary: qwen_chat
    fallbacks:
      - deepseek_chat

  image:
    modality: image_generation
    primary: dashscope_image

  video:
    modality: video_generation
    primary: kling_video
```

为了兼容当前 AginxBrain，可以先保留 `routes`，逐步迁移到 `endpoints + modalities`。

## 13. HTTP API 设计建议

AginxBrain 对外仍然应保留现有兼容 API：

```text
/v1/messages
/v1/chat/completions
/v1/responses
/anthropic/v1/messages
/openai/v1/chat/completions
/openai/v1/responses
```

新增 Brain 能力 API：

```text
GET  /api/brain/status
GET  /api/brain/config
PUT  /api/brain/config
GET  /api/brain/providers
PUT  /api/brain/providers/{name}
GET  /api/brain/endpoints
PUT  /api/brain/endpoints/{name}
GET  /api/brain/modalities
PUT  /api/brain/modalities/{name}
POST /api/brain/complete
POST /api/brain/generate/image
POST /api/brain/generate/video
GET  /api/brain/tasks/{task_id}
```

其中通用 complete：

```json
POST /api/brain/complete
{
  "modality": "image_generation",
  "messages": [
    {
      "role": "user",
      "content": "A cyberpunk city at night"
    }
  ],
  "extra": {
    "size": "1024x1024",
    "n": 1
  }
}
```

返回：

```json
{
  "media": {
    "type": "images",
    "items": [
      {
        "url": "https://...",
        "data_base64": ""
      }
    ]
  }
}
```

## 14. UI 设计建议

AginxBrain Web UI 后续应分成：

1. Dashboard
   - 当前 tag
   - 请求量
   - 成功率
   - 平均延迟
   - 熔断 endpoint

2. Providers
   - provider 名称
   - auth 类型
   - key 是否已配置
   - 环境变量绑定

3. Endpoints
   - provider
   - model
   - base_url
   - format
   - enabled
   - test endpoint

4. Modalities
   - chat / image / video / tts
   - primary endpoint
   - fallback endpoints

5. Tags / Tiers
   - opus / sonnet / haiku / auto
   - 或 image / video / fast / code

6. Logs
   - request model
   - resolved modality
   - resolved endpoint
   - provider/model
   - latency
   - error

7. Media Tasks
   - video task_id
   - status
   - result URL
   - retry/cancel

## 15. 分阶段实现计划

### Phase 1：保守增强当前路由

- 保留现有 `Provider/Route/Tag`
- 给 `Route` 增加 `modality` 字段
- 增加 `ApiFormat` 枚举值：`dashscope_image`、`dashscope_video`、`kling`、`openai_images`
- 添加 media response 类型
- UI 暂时只展示，不大改结构

### Phase 2：引入 Endpoint/Modality

- 新增 `EndpointConfig`
- 新增 `ModalityConfig`
- 当前 `routes` 自动迁移到 `endpoints`
- 当前 `tags` 映射到 chat modality 的 tier
- 增加 endpoint health tracker
- 增加 fallback + circuit breaker

### Phase 3：多模态 Driver

- 复制 OpenCarrier 的 `CompletionRequest/CompletionResponse/ContentBlock/MediaOutput`
- 复制 OpenAI/Anthropic/Gemini 图片输入转换逻辑
- 复制 DashScope Image
- 复制 MiniMax Image
- 复制 DashScope Video
- 复制 Kling JWT + video/image 任务逻辑
- 复制 DashScope TTS

### Phase 4：Browser/Server 产品化

- `/api/brain/*` 管理接口
- Web UI 管理 providers/endpoints/modalities
- 视频异步任务列表
- endpoint health dashboard
- user api key / quota / billing

## 16. 核心判断

AginxBrain 后续不要只围绕 “model-router” 演进，而应该围绕：

```text
AI capability gateway for agents
```

也就是：

```text
Agent 需要什么 AI 能力 → AginxBrain 选择最佳 endpoint → 执行 → 返回统一结果
```

这个方向下，OpenCarrier 中的 Brain/LLM/media 代码可以作为第一版实现的直接来源。