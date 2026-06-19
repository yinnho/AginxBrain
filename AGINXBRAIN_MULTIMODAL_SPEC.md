# AginxBrain 多模态能力接入规格

> 目标：让 AginxBrain 接管 OpenCarrier 的**全部** AI 能力 —— 文本对话 + 图片理解 + 图片生成 + 语音合成 + 语音识别 + 视频生成。实现完后 OpenCarrier 不再直连 DashScope/MiniMax/Kling/Groq 等任何 provider，所有能力统一走 `https://brain.aginx.net`。
>
> 本文档把 OpenCarrier 现有 driver 的**确切请求/响应格式**整理出来，aginxbrain 项目组照着实现即可，不必再翻 OpenCarrier 代码。所有格式均来自 `crates/runtime/src/llm_driver_impl.rs`。

---

## 能力清单总表（先看这张）

OpenCarrier 的 `ApiFormat` 枚举（`crates/types/src/brain.rs:84`）当前有 9 种 format，对应这些能力：

| format | 能力 | 当前是否有工具调用 | OpenCarrier 源码 | 真实 provider |
|--------|------|------------------|-----------------|--------------|
| `openai` | 文本对话/vision（带图） | ✅ 主力 | `complete_openai` :1119 | 任意 OpenAI 兼容 |
| `anthropic` | 文本对话 | ✅ 用过（现已弃用，aginxbrain 接管） | `complete_anthropic` :1479 | Anthropic/Zhipu/百度 |
| `gemini` | 文本对话 | ⚠️ 预留 | `complete_gemini` :1701 | Google Gemini |
| `dashscope_image` | 文字→图片 | ✅ `image_generate` 工具 | `complete_dashscope_image` :476 | 阿里 DashScope (wan2.7) |
| `dashscope_tts` | 文字→语音 | ✅ `text_to_speech` 工具 | `complete_dashscope_tts` :554 | 阿里 DashScope TTS |
| `dashscope_video` | 文字/图→视频 | ⚠️ **预留，当前无工具调用** | `complete_dashscope_video` :609 | 阿里 DashScope (wanx) |
| `kling` | 文字→视频/图片 | ⚠️ **预留，当前无工具调用** | `complete_kling` :670 | 快手 Kling（JWT 认证） |
| `minimax_image` | 文字→图片 | ⚠️ 预留（可替代 dashscope_image） | `complete_minimax_image` :418 | MiniMax image-01 |
| `openai_images` | 文字→图片 | ⚠️ 预留（DALL-E） | `complete_openai_images` :385 | OpenAI DALL-E |

**OpenCarrier 当前实际通过 modality 调用的只有 4 个**：`vision`、`image`、`tts`、`audio`（见 `tools/media.rs`）。video 和 kling driver 写好了但**还没有工具触发**（dead code / 预留）。

### 豆包 / Seedance / 即梦 的现状

- **豆包（Doubao）**：OpenCarrier 只有 `VOLCENGINE_BASE_URL = "https://ark.cn-beijing.volces.com/api/v3"` 常量（`model_catalog.rs:49`），用于豆包**文本 LLM**，走 OpenAI 兼容格式，**没有专用 driver**。aginxbrain 要支持豆包文本对话，只需加一条 route 指向 ark 端点即可，无需特殊格式。
- **Seedance（火山引擎视频生成）**：OpenCarrier **完全没有**。如果要做，是 aginxbrain 的**新增能力**，不在本规格里。
- **即梦（字节）**：OpenCarrier **完全没有**。同上。

> 结论：aginxbrain 第一期只需实现 image / tts / vision / audio 四个能力即可让 OpenCarrier 完全切换。video（DashScope + Kling）是第二期，代码已备好可直接参考。豆包/Seedance/即梦是纯新增，不阻塞 OpenCarrier 迁移。

---

## 0. 调用约定（先读这段）

OpenCarrier 调用 AginxBrain 的方式：

```
POST https://brain.aginx.net/v1/chat/completions
Authorization: Bearer <aginxbrain caller key>
Content-Type: application/json

{
  "model": "<modality名>",
  "messages": [ ... ],
  "stream": false,        // 多模态一律非流式
  ...其它字段
}
```

**`model` 字段 = modality 名**，aginxbrain 据此路由到对应能力。需要支持的 modality 名：

| `model` 值 | 能力 | 请求里的关键信号 |
|-----------|------|----------------|
| `chat` / `reasoning` / `code` / `fast` | 文本对话（**已支持**） | `messages` 文本 |
| `vision` | 图片理解 | `messages` 含 image content block |
| `image` | 文字生成图片 | `messages` 里纯文本 prompt，`extra.size` / `extra.n` |
| `tts` | 文字转语音 | `messages` 里纯文本，`extra.voice` |
| `audio` | 语音转文字 | `messages` 含 audio content block |

**OpenCarrier 这边的请求有两套形态**（取决于能力类型）：
- **对话型**（vision / audio）：走标准 OpenAI chat 格式，`messages` 里带 image/audio 的 base64 block。
- **任务型**（image / tts）：prompt 放在 `messages` 的文本里，参数放在请求的扩展字段（OpenCarrier 用 `extra`，序列化时会平铺或放进顶层）。

> ⚠️ 关键点：**OpenCarrier 端的 `format` 字段决定它怎么发请求、怎么解析响应**。当前 OpenCarrier 对每种 media 用了不同的 `format`（`dashscope_image` / `dashscope_tts` / `openai` 等），每个 format 对应一套硬编码的请求构造 + 响应解析。
>
> **aginxbrain 不必复刻这些 provider 私有格式**。更好的做法是：aginxbrain 定义一套**统一的 OpenAI-兼容多模态接口**，内部再转译到 DashScope/MiniMax/Kling。下文每个能力都给出「OpenCarrier 期望的请求/响应」+「aginxbrain 内部要对接的真实 provider 格式」。

---

## 1. Image（文字生成图片）

### 1.1 OpenCarrier 发出的请求

OpenCarrier 的 `image_generate` 工具构造 `CompletionRequest`，model=`image`，然后走它配置的 image endpoint（目前是 `dashscope_image` format）。**真实发到 DashScope 的请求长这样**：

```json
POST https://dashscope.aliyuncs.com/api/v1/services/aigc/...
Authorization: Bearer <QWEN_API_KEY>

{
  "model": "wan2.7-image",
  "input": {
    "messages": [
      { "role": "user", "content": [ { "text": "<prompt>" } ] }
    ]
  },
  "parameters": {
    "prompt_extend": true,
    "watermark": false,
    "n": 1,
    "size": "1024*1024"
  }
}
```

OpenCarrier 传进来的参数：
- prompt：从 `messages` 提取的文本
- `extra.size`：如 `"1024x1024"`，**OpenCarrier 会把 `x` 替换成 `*`** 变成 `"1024*1024"`（DashScope 要求星号）；默认 `"1280*1280"`
- `extra.n`：图片数量，默认 1

### 1.2 OpenCarrier 期望的响应

OpenCarrier 的 `dashscope_image` driver 按**三种格式依次尝试**解析响应，aginxbrain 返回任意一种即可（推荐第一种）：

**格式 A（DashScope 多模态生成格式，主用）：**
```json
{
  "output": {
    "choices": [
      {
        "message": {
          "content": [
            { "image": "https://...图片URL..." }
          ]
        }
      }
    ]
  },
  "code": "Success",
  "request_id": "..."
}
```

**格式 B（legacy results）：**
```json
{
  "output": {
    "results": [
      { "url": "https://...", "b64_image": "" }
    ]
  }
}
```

**格式 C（OpenAI 风格）：**
```json
{
  "output": {
    "data": [
      { "url": "https://...", "b64_json": "" }
    ]
  }
}
```

**错误判断**：如果顶层 `code` 字段是字符串且不等于 `"Success"` 和 `"200"`，OpenCarrier 视为失败，用 `message` 字段报错。

### 1.3 aginxbrain 建议

- route：`model = "image"` → provider `qwen/dashscope`，真实 model `wan2.7-image`
- aginxbrain 接收 OpenCarrier 发来的 OpenAI chat 请求（model=image），内部转成上面的 DashScope `input.messages` + `parameters` 格式，再把响应转成格式 A 返回。
- **关键约束**：OpenCarrier 会自动把 `size` 里的 `x` 换成 `*`，所以 aginxbrain 收到的 size 已经是星号格式，直接透传即可。

---

## 2. TTS（文字转语音）

### 2.1 OpenCarrier 发出的请求

OpenCarrier 的 `text_to_speech` 工具，model=`tts`，走 `dashscope_tts` format：

```json
POST https://dashscope.aliyuncs.com/api/v1/services/audio/...
Authorization: Bearer <key>

{
  "model": "<tts模型>",
  "input": {
    "text": "<要转语音的文本>",
    "voice": "Cherry"
  }
}
```

参数来源：
- text：`extract_query(messages)` —— 从 messages 文本提取（不是 `extract_prompt`，注意区分）
- `extra.voice`：音色名，默认 `"Cherry"`（DashScope 默认音色）

### 2.2 OpenCarrier 期望的响应

```json
{
  "output": {
    "audio": "https://...mp3下载URL..."
  },
  "code": "Success"
}
```

或备用路径 `/output/results/0/url`。

**OpenCarrier 拿到 URL 后会自己去下载音频字节**（用 HTTP GET，30 秒超时），所以响应里**只给 URL，不要给 base64**。

下载后 OpenCarrier 估算时长：`max(词数 × 400ms, 500ms)`，标记为 `mp3` 格式。

**错误判断**：同样看顶层 `code` 字段。

### 2.3 aginxbrain 建议

- route：`model = "tts"` → DashScope TTS
- 注意 text 提取的是 **query 不是 prompt**，aginxbrain 要从 `messages` 里取用户最后一条消息的文本内容。
- 响应务必返回可下载的音频 URL（DashScope 原本就是返回 URL，透传即可）。

---

## 3. Vision（图片理解）

### 3.1 调用方式

Vision **走标准 OpenAI chat 格式**（OpenCarrier 这边 format=`openai`）。OpenCarrier 的 `image_analyze` / `media_describe` 工具构造的请求：

```json
POST https://brain.aginx.net/v1/chat/completions
{
  "model": "vision",
  "messages": [
    {
      "role": "user",
      "content": [
        { "type": "text", "text": "<分析指令，可选>" },
        { "type": "image_url", "image_url": { "url": "data:image/png;base64,<base64>" } }
      ]
    }
  ],
  "max_tokens": 1024,
  "stream": false
}
```

> OpenCarrier 用 `qwen3.6-plus`（走 DashScope OpenAI 兼容端点 `https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions`），所以 vision 本质就是一次带图的 chat。

### 3.2 响应

**标准 OpenAI chat 响应**：

```json
{
  "choices": [
    {
      "message": {
        "role": "assistant",
        "content": "<对图片的文字描述>"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": { "prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150 }
}
```

### 3.3 aginxbrain 建议

- route：`model = "vision"` → qwen vision 模型（`qwen3.6-plus` 或 `GLM-4.6V-Flash`），format=openai
- 这就是一次普通 chat，只是 messages 里带 image。aginxbrain 现有的 OpenAI 透传逻辑应该已经能处理，只要该 route 指向一个支持视觉的模型即可。
- **图片以 base64 data URL 形式传入**，aginxbrain 透传给上游即可。

---

## 4. Audio / ASR（语音转文字）

### 4.1 ⚠️ 重要：OpenCarrier 的 ASR 当前不走 Brain modality

需要先澄清一个现状。OpenCarrier 有**两套**语音转文字路径：

**路径 A：`media_understanding.rs`（音频附件自动转写，当前主力）**
- 收到语音消息时，自动把音频附件转成文字，喂给 LLM。
- 实现：优先 Groq Whisper (`whisper-large-v3-turbo`)，其次 OpenAI Whisper (`whisper-1`)，最后本地 Parakeet MLX。
- **直接调 provider，不经过 Brain**。

**路径 B：`media_transcribe` / `speech_to_text` 工具（显式调用）**
- Agent 主动调工具转写一个音频文件。
- 这条路径**会走 Brain**，modality 名 = `"audio"`：

```json
{
  "model": "audio",
  "messages": [
    {
      "role": "user",
      "content": [
        { "type": "audio", "input_audio": { "data": "<base64>", "format": "mp3" } }
      ]
    }
  ]
}
```

### 4.2 aginxbrain 建议

- route：`model = "audio"` → Groq Whisper 或 OpenAI Whisper（aginxbrain 选一个，推荐 Groq，快且便宜）
- aginxbrain 接收 OpenAI chat 格式的 audio block，内部转成 Whisper 的 `/audio/transcriptions` multipart 请求（Whisper 不接受 chat 格式，需要 aginxbrain 做转译）。
- 响应：标准 OpenAI chat 格式，把转写文字放进 `choices[0].message.content`。

> 注意：如果 aginxbrain 暂时不做 ASR，**路径 A 的自动转写仍会直连 Groq/OpenAI**，不影响主流程；只有 Agent 显式调 `media_transcribe` 工具时才需要 `audio` modality。优先级最低。

---

## 5. Video — DashScope（文字/图片 → 视频）

### 5.1 现状

⚠️ **当前 OpenCarrier 没有任何工具调用 `video` modality**。`complete_dashscope_video` driver 写好了但未接线（预留能力）。aginxbrain 实现它是为未来 `generate_video` 工具做准备。

### 5.2 DashScope 视频请求格式

```json
POST https://dashscope.aliyuncs.com/api/v1/services/aigc/video-generation/video-synthesis
Authorization: Bearer <key>
X-DashScope-Async: enable        ← 关键：异步任务头

{
  "model": "wanx2.1-i2v-turbo",
  "input": {
    "prompt": "<文字描述>",
    "img_url": "<首帧图片URL，可选；文生视频时省略>"
  },
  "parameters": {
    "resolution": "720P",
    "duration": 5
  }
}
```

参数来源：
- prompt：`extract_prompt(messages)`
- `extra.input.img_url`：首帧图 URL（图生视频），可选
- `extra.parameters.resolution`：默认 `"720P"`
- `extra.parameters.duration`：默认 `5`（秒）

### 5.3 异步轮询（视频生成本质是异步任务）

提交后立即返回 task_id：
```json
{ "output": { "task_id": "abc123", "task_status": "PENDING" }, "request_id": "..." }
```

OpenCarrier 用固定的轮询 URL（**硬编码**到 DashScope 域名）：
```
GET https://dashscope.aliyuncs.com/api/v1/tasks/{task_id}
```

轮询条件（`poll_until_complete`，间隔 5 秒，最长 300 秒）：
- `/output/task_status == "SUCCEEDED"` → 完成
- `/output/task_status == "FAILED"` → 失败，错误信息取 `/output/message`
- 其它 → 继续轮询

### 5.4 OpenCarrier 期望的最终响应

```json
{
  "output": {
    "task_status": "SUCCEEDED",
    "video_url": "https://...mp4...",
    "cover_url": "https://...封面图..."
  }
}
```

备用视频 URL 路径：`/output/results/0/url`。封面 `cover_url` 可选。

OpenCarrier 拿到 video_url + cover_url 后直接返回给上层，**自己不下载**（和 TTS 不同，TTS 会下载，视频不下载）。

### 5.5 aginxbrain 建议

- ⚠️ **难点**：OpenCarrier 的轮询 URL 是写死的 DashScope 域名。如果 aginxbrain 要接管，有两种方案：
  - **方案 A**（简单）：aginxbrain 内部调 DashScope，但把 task_id 映射成自己的，OpenCarrier 轮询 aginxbrain，aginxbrain 再转发轮询。需要 aginxbrain 暴露一个 task 轮询端点。
  - **方案 B**（推荐）：video 能力**暂不由 OpenCarrier 直接触发**，等 aginxbrain 提供完整的同步 video API（提交+阻塞等待+返回 URL），OpenCarrier 新写一个 video driver 调 aginxbrain。这样绕开轮询 URL 写死的问题。
- 视频生成耗时长（几十秒到几分钟），aginxbrain 应支持**同步等待**（内部轮询）或**异步 task**（返回 task_id 让客户端轮询）两种模式。OpenCarrier 现有 driver 是同步等待模式。

---

## 6. Video / Image — Kling（快手，JWT 认证）

### 6.1 现状

⚠️ **同样预留，当前无工具调用**。Kling 的特殊性在于 **JWT 认证**（不是 Bearer token）。

### 6.2 Kling 认证（关键差异）

Kling 用 HMAC-SHA256 签名的 JWT，而不是普通 API key：

```
Authorization: Bearer <JWT>
```

JWT 生成（`generate_jwt`，`llm_driver_impl.rs:198-233`）：
- header：`{"alg":"HS256","typ":"JWT"}`
- payload：`{"iss":"<access_key>","exp":<now+1800>,"nbf":<now-5>}`
- 签名：HMAC-SHA256(secret_key, base64url(header) + "." + base64url(payload))
- access_key 放 `api_key` 字段，secret_key 放 `secret_key` 字段（OpenCarrier 的 brain.json 里 provider 配 `api_key_env` + secret 通过 params 传）

> aginxbrain 接管后，**JWT 生成逻辑搬到 aginxbrain**，OpenCarrier 不再需要知道 access_key/secret_key。OpenCarrier 只用一个普通 caller key 调 aginxbrain。

### 6.3 Kling 请求格式

```json
POST https://api.klingai.com/v1/videos/text2video    (或图片生成端点)
Authorization: Bearer <JWT>

{
  "model": "kling-v1-6",
  "prompt": "<描述>",
  ...其它 extra 参数直接平铺进 body
}
```

OpenCarrier 把 `request.extra` 的所有字段直接 merge 进请求 body（`for (k,v) in extra { body[k] = v }`）。

### 6.4 异步轮询

提交返回：
```json
{ "code": 0, "data": { "task_id": "xxx", "task_status": "submitted" } }
```

`code != 0` 视为失败，错误取 `message`。

轮询 URL：`{base_url}/{task_id}`（base_url 就是提交用的那个 endpoint）。

轮询条件：
- `/data/task_status == "succeed"` → 完成
- `/data/task_status == "failed"` → 失败，错误取 `/data/task_status_msg`
- 其它 → 继续

### 6.5 结果解析（视频 or 图片，Kling 两种都可能返回）

```json
{
  "data": {
    "task_status": "succeed",
    "task_result": [
      {
        "url": "https://...视频或图片URL...",
        "cover_url": "https://...封面（视频时）...",
        "images": [ { "url": "...", "b64_json": "..." } ]   ← 图片任务时
      }
    ]
  }
}
```

OpenCarrier 依次判断：
1. `task_result[0].url` 存在 → 视频结果（带 cover_url）
2. `task_result[0].images[]` 存在 → 图片结果集

### 6.6 aginxbrain 建议

- Kling 的 JWT 认证 + 异步轮询都搬到 aginxbrain 内部。
- 对 OpenCarrier 暴露成统一的同步 video/image 接口（model=`video` 或 `kling`）。

---

## 7. MiniMax Image（备选图片生成）

### 7.1 请求格式

```json
POST https://api.minimaxi.com/v1/image_generation
Authorization: Bearer <key>

{
  "model": "image-01",
  "prompt": "<描述>",
  "n": 1,
  "response_format": "url",
  "aspect_ratio": "1:1",          // 可选：1:1, 16:9, 4:3, 3:2, 2:3, 3:4, 9:16, 21:9
  "prompt_optimizer": false,      // 可选
  "seed": 12345                   // 可选
}
```

参数来源：`extra.aspect_ratio` / `extra.prompt_optimizer` / `extra.seed` / `extra.n`。

### 7.2 响应（MiniMax 私有格式）

```json
{
  "data": {
    "image_urls": ["https://...url1...", "https://...url2..."],
    "image_base64": ["<base64>", ...]
  }
}
```

OpenCarrier 依次尝试：
1. `data.image_urls[]` → URL 列表
2. `data.image_base64[]` → base64 列表
3. fallback：`data[].url` / `data[].b64_json`（OpenAI 风格）

### 7.3 aginxbrain 建议

- MiniMax 是 DashScope image 的备选。aginxbrain 的 `image` route 可以指向 MiniMax 或 DashScope 任一，OpenCarrier 不关心（只要响应符合 image 规格第 1.2 节的格式 A/B/C 之一）。
- 如果 aginxbrain 内部用 MiniMax，要把上面的私有响应格式转成统一的 image 响应（格式 A）再返回给 OpenCarrier。

---

## 8. 统一接口建议（给 aginxbrain 的架构建议）

最干净的做法是 aginxbrain 定义一套 **OpenAI-Chat-兼容的多模态协议**，所有能力都走 `/v1/chat/completions`，靠 `model` 字段路由：

```
client (OpenCarrier) ──model="image"──→  aginxbrain ──→ DashScope 图片 API
client (OpenCarrier) ──model="tts"───→  aginxbrain ──→ DashScope TTS API
client (OpenCarrier) ──model="vision"→  aginxbrain ──→ qwen-vision (OpenAI 兼容)
client (OpenCarrier) ──model="audio"─→  aginxbrain ──→ Groq/OpenAI Whisper
```

好处：OpenCarrier 端只需 `format=openai` 一种格式，可以删掉 `dashscope_image`/`dashscope_tts`/`minimax_image`/`kling` 等所有 provider 私有 driver（共约 400 行）。

aginxbrain 内部为每个能力实现一个「OpenAI-chat → provider 私有格式」的转译器，就是本文档第 1~7 节描述的那些请求/响应格式。

---

## 9. 验收测试清单

aginxbrain 实现后，用这些请求测（caller key 用 `ab_45966845a20e7e188562a9455562c5df01bb8597b9c880876940f6992605f2a7`）：

### 9.1 image
```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
  -d '{"model":"image","messages":[{"role":"user","content":[{"text":"一只在月球上的猫"}]}],"size":"1024*1024","n":1}'
```
期望：响应含图片 URL（格式 A/B/C 任一）。

### 9.2 tts
```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
  -d '{"model":"tts","messages":[{"role":"user","content":"你好世界"}],"voice":"Cherry"}'
```
期望：响应含可下载的 mp3 URL。

### 9.3 vision
```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
  -d '{"model":"vision","messages":[{"role":"user","content":[{"type":"text","text":"描述这张图"},{"type":"image_url","image_url":{"url":"data:image/png;base64,<BASE64>"}}]}],"max_tokens":512}'
```
期望：标准 OpenAI chat 响应，`choices[0].message.content` 是图片描述。

### 9.4 audio
```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
  -d '{"model":"audio","messages":[{"role":"user","content":[{"type":"audio","input_audio":{"data":"<BASE64>","format":"mp3"}}]}]}'
```
期望：标准 OpenAI chat 响应，`choices[0].message.content` 是转写文字。

### 9.5 video（第二期）
```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
  -d '{"model":"video","messages":[{"role":"user","content":[{"text":"一只猫在奔跑"}]}],"parameters":{"resolution":"720P","duration":5}}'
```
期望：响应含 video_url + cover_url。

---

## 10. 分期实施建议

**第一期（让 OpenCarrier 完全切换，必做）：**
- ✅ `image` — 文字→图片（DashScope wan2.7）
- ✅ `tts` — 文字→语音（DashScope TTS）
- ✅ `vision` — 图片理解（qwen-vision，基本就是带图 chat）
- ⬇️ `audio` — 可选（自动转写仍直连 Groq，不阻塞）

**第二期（视频，OpenCarrier 目前没工具调用，可延后）：**
- `video` — DashScope wanx
- Kling（JWT 认证）— 需把 JWT 逻辑搬过来

**第三期（纯新增，OpenCarrier 完全没有，可选）：**
- 豆包 Seedance 视频生成
- 即梦
- 其它国产多模态

完成第一期后，OpenCarrier 就能删掉约 500 行 provider 私有 driver 代码（image/tts/minimax/openai_images + Anthropic/Gemini 翻译层），所有 AI 能力统一走 aginxbrain。

---

## 附：源码定位

| 能力 | OpenCarrier 源码 |
|------|-----------------|
| image | `crates/runtime/src/llm_driver_impl.rs:476-551` (`complete_dashscope_image`) |
| tts | `crates/runtime/src/llm_driver_impl.rs:554-606` (`complete_dashscope_tts`) |
| video (dashscope) | `crates/runtime/src/llm_driver_impl.rs:609-667` (`complete_dashscope_video`) |
| video/image (kling) | `crates/runtime/src/llm_driver_impl.rs:670-739` (`complete_kling`) |
| kling JWT | `crates/runtime/src/llm_driver_impl.rs:198-233` (`generate_jwt`) |
| minimax image | `crates/runtime/src/llm_driver_impl.rs:418-473` (`complete_minimax_image`) |
| openai images | `crates/runtime/src/llm_driver_impl.rs:385-415` (`complete_openai_images`) |
| format 分派 | `crates/runtime/src/llm_driver_impl.rs:315-326` |
| 认证头分派 | `crates/runtime/src/llm_driver_impl.rs:162-195` (`apply_auth`) |
| 轮询逻辑 | `crates/runtime/src/llm_driver_impl.rs:236-271` (`poll_until_complete`) |
| vision 调用 | `crates/runtime/src/tools/media.rs:485` (`.complete("vision", ...)`) |
| image 调用 | `crates/runtime/src/tools/media.rs:635` (`.complete("image", ...)`) |
| tts 调用 | `crates/runtime/src/tools/media.rs:840` (`.complete("tts", ...)`) |
| audio 调用 | `crates/runtime/src/tools/media.rs:558,945` (`.complete("audio", ...)`) |
| ASR 自动转写 | `crates/runtime/src/media_understanding.rs` (Groq/OpenAI Whisper，不走 Brain) |
| ApiFormat 枚举 | `crates/types/src/brain.rs:84` |
| provider base URL | `crates/types/src/model_catalog.rs` (DashScope/MiniMax/Volcengine 等) |
