# AginxBrain 多模态使用指南（TTS / ASR / Video / PPT）

本文档说明如何通过 AginxBrain 调用**语音合成（TTS）**、**语音识别（ASR）**、**视频生成（Video）**、**PPT 生成**四项能力。

> 文档中所有示例的当前后端：
> - TTS：阿里 DashScope `cosyvoice-v2`（WebSocket）
> - ASR：阿里 DashScope `paraformer-realtime-v2`（WebSocket）
> - Video：阿里 DashScope `wanx2.1-t2v-turbo`（异步任务）
> - PPT：阿里 DashScope `qwen-doc-turbo`（流式，模板模式）

---

## 0. 通用约定

| 项 | 值 |
|----|----|
| 服务地址 | `https://brain.aginx.net`（本地为 `http://127.0.0.1:8083`） |
| 鉴权 | `Authorization: Bearer <caller_key>`（在管理后台创建的调用密钥） |
| 统一入口 | `POST /v1/chat/completions`（OpenAI Chat 格式） |
| 模型名 | 用**标签名**当 model：`tts` / `audio` / `video` / `ppt` / `seedance` / `seedream` / `seedream-lite`（AginxBrain 内部解析到具体后端模型） |

四项能力**都复用 OpenAI Chat 接口**——把要处理的"内容"放在 `messages` 里，靠 `model` 字段区分走哪条管道：

```bash
curl https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{ "model": "<tts|audio|video|ppt|seedance|seedream|seedream-lite>", "messages": [...] }'
```

`$KEY` 是你的 caller key。下文每节给出完整示例。

---

## 1. TTS —— 语音合成（文字 → 语音）

把要合成的文字放在最后一条 `user` 消息里。

**请求**
```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "tts",
    "messages": [
      { "role": "user", "content": "你好，这是一段语音合成测试。" }
    ],
    "voice": "longxiaochun_v2",
    "audio_format": "mp3",
    "sample_rate": 22050
  }'
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `voice` | 音色（见下表） | `longxiaochun_v2` |
| `audio_format` | 输出格式：`mp3` / `wav` / `pcm` | `mp3` |
| `sample_rate` | 采样率 | `22050` |

`voice` 用 CosyVoice 的音色 ID，命名大致是 `long<名>_v2`。默认 `longxiaochun_v2`（女声·温柔）已内置可用。常用还包括 `longcheng_v2`（男声）、`longhua_v2`（男声）、`longwan_v2`（女声）等；**完整音色列表以 DashScope CosyVoice 文档为准**（含男/女声、方言、情感音色）。

> 也支持声音克隆音色（在 DashScope 控制台训练后用克隆音色 ID）。传一个不存在的音色 ID 会在合成阶段报错。

**响应**
```json
{
  "code": "Success",
  "output": { "audio": "/audio/1784281502796_xxx.mp3" }
}
```

**获取音频文件**：返回的是相对路径，直接 GET 同一个域名（**此接口免鉴权**）：
```bash
curl -o result.mp3 https://brain.aginx.net/audio/1784281502796_xxx.mp3
```

---

## 2. ASR —— 语音识别（语音 → 文字）

把音频以 base64 放在 `messages` 的 `input_audio` 内容块里（OpenAI 的 audio block 格式）。

**请求**
```bash
# 把音频转成 data URL 再发
AUDIO=$(base64 -i voice.mp3 | tr -d '\n')

curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "audio",
    "messages": [
      {
        "role": "user",
        "content": [
          { "type": "text", "text": "请转写这���音频" },
          {
            "type": "input_audio",
            "input_audio": {
              "data": "data:audio/mp3;base64,'"${AUDIO}"'",
              "format": "mp3"
            }
          }
        ]
      }
    ],
    "sample_rate": 22050
  }'
```

| 字段 | 说明 |
|------|------|
| `input_audio.data` | base64 音频。推荐用 **data URL**：`data:audio/<fmt>;base64,<...>`；也可只放裸 base64 |
| `input_audio.format` | `mp3` / `wav` / `pcm` 等（用 data URL 时可省略，自动从 mime 推断） |
| `sample_rate` | 采样率，默认 `22050` |

**响应**（标准 OpenAI Chat 格式，转写文字在 `message.content`）
```json
{
  "choices": [
    {
      "finish_reason": "stop",
      "message": {
        "role": "assistant",
        "content": "你好，这是一段语音合成测试。"
      }
    }
  ]
}
```

> 支持 TTS→ASR 互测：把 TTS 产出的 mp3 再喂给 ASR，应能还原文字。

---

## 3. Video —— 视频生成（文字 → 视频，异步）

视频生成耗时较长（turbo ~30–60s），因此是**真异步**：提交立刻返回 `task_id`，**客户端轮询**直到完成。

### 3.1 提交任务

把视频描述（prompt）放在最后一条 `user` 消息里。

```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "video",
    "messages": [
      { "role": "user", "content": "一只金毛在阳光下的公园草地上奔跑，慢动作" }
    ],
    "size": "1280*720",
    "duration": 5
  }'
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `size` | 分辨率，如 `1280*720`、`960*960`、`1080*1920` | `1280*720` |
| `duration` | 时长（秒） | `5` |

**响应**（**立刻返回**，通常 < 1s）
```json
{
  "code": "Success",
  "task_id": "c3f9065c-091b-4d34-9b97-dde5faa10bcb",
  "task_status": "PENDING",
  "tag": "video"
}
```

### 3.2 轮询结果

用返回的 `task_id` 轮询（建议每 5s 一次）。路径里的 `video` 就是上面返回的 `tag`。

```bash
curl https://brain.aginx.net/v1/tasks/video/$TASK_ID \
  -H "Authorization: Bearer $KEY"
```

**响应**（DashScope 任务格式）
```json
{
  "output": {
    "task_id": "c3f9065c-...",
    "task_status": "RUNNING",
    "video_url": null
  },
  "request_id": "..."
}
```

`task_status` 取值：

| 状态 | 含义 | 处理 |
|------|------|------|
| `PENDING` / `RUNNING` | 生成中 | 继续轮询 |
| `SUCCEEDED` | 完成 | 取 `output.video_url` |
| `FAILED` | 失败 | 看 `output.message` |

**完成时的响应**
```json
{
  "output": {
    "task_id": "c3f9065c-...",
    "task_status": "SUCCEEDED",
    "video_url": "https://dashscope-result-xxx.aliyuncs.com/.../c3f9065c-....mp4?Expires=...&Signature=...",
    "actual_prompt": "...(模型重写后的 prompt)..."
  }
}
```

> `video_url` 是阿里云 OSS 临时链接，**有过期时间**（`Expires`），拿到后尽快下载。

### 3.3 轮询示例脚本

```bash
TASK="<提交得到的 task_id>"
while true; do
  R=$(curl -s https://brain.aginx.net/v1/tasks/video/$TASK -H "Authorization: Bearer $KEY")
  S=$(echo "$R" | python3 -c "import sys,json;print(json.load(sys.stdin)['output']['task_status'])")
  echo "status=$S"
  [ "$S" = "SUCCEEDED" ] && { echo "$R" | python3 -c "import sys,json;print(json.load(sys.stdin)['output']['video_url'])"; break; }
  [ "$S" = "FAILED" ] && { echo "$R"; break; }
  sleep 5
done
```

### 3.4 Seedance 2.0（火山方舟，另一套视频后端）

除了阿里 wanx，还接入了字节火山方舟的 **Seedance 2.0**（文生视频 / 首帧图生视频 / 多模态参考），用**独立的 `seedance` 标签**。调用方式和 wanx 一样是"提交 + 轮询"两步，但**参数和轮询响应格式不同**。

> 各异步视频后端用各自独立的 tag（`video`=wanx，`seedance`=Seedance），因为轮询按 tag 的第一个候选路由解析 provider，混用 tag 会把任务轮询到错的 provider。

**提交**（model=`seedance`，返回立刻拿到 task_id）：

```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "seedance",
    "messages": [
      { "role": "user", "content": "一只金毛犬在阳光明媚的草地上奔跑，慢动作" }
    ],
    "resolution": "720p",
    "ratio": "16:9",
    "duration": 5,
    "generate_audio": true
  }'
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `resolution` | 分辨率：`480p` / `720p` / `1080p` / `4K` | `1080p` |
| `ratio` | 画幅：`16:9` / `9:16` / `1:1` 等 | `16:9` |
| `duration` | 时长（秒）`4`–`15`，或 `-1` 智能 | `5` |
| `generate_audio` | 是否生成配音 | `true` |

**图生视频**：在 `messages` 的 user 内容里加一个 `image_url` 块作为首帧：

```json
{ "role": "user", "content": [
    { "type": "text", "text": "让画面动起来，镜头缓慢推进" },
    { "type": "image_url", "image_url": { "url": "https://.../first_frame.png" } }
] }
```

**提交响应**（和 wanx 同样的 shape）：
```json
{ "code": "Success", "task_id": "cgt-20260719210128-66hb5", "task_status": "queued", "tag": "seedance" }
```

**轮询**：`GET /v1/tasks/seedance/{task_id}`。注意返回的是**火山原生格式**（和 wanx 的 `output.task_status`/`output.video_url` 不同）：

```json
{
  "id": "cgt-...",
  "status": "running",
  "model": "doubao-seedance-2-0-260128"
}
```

完成时（`status: "succeeded"`）视频链接在 `content.video_url`：
```json
{ "id": "cgt-...", "status": "succeeded", "content": { "video_url": "https://...mp4?X-Tos-Expires=86400&..." }, "usage": { "completion_tokens": 108900 } }
```

`status` 取值：`queued` / `running` / `succeeded` / `failed` / `cancelled`。

> - Seedance 带音频生成较慢，常要 **2–5 分钟**，轮询间隔建议 10–15s。
> - `video_url` 是火山 TOS 签名链接，24h 有效，拿到后用 GET 下载。
> - 换 Fast 版（更快更便宜）把 route 的 model 改成 `doubao-seedance-2-0-fast-260128` 即可。

### 3.5 Seedream 5.0 文生图（火山方舟，配 Seedance 做图生视频）

Seedance 会**拒识「真人」首帧**（photograph of a real human）。绕过办法：用 **Seedream 5.0**（字节文生图）生成首帧--AI 生成的图被当作非真人，Seedance 放行。两个 tag：

| tag | 模型 | 说明 |
|-----|------|------|
| `seedream` | `doubao-seedream-5-0-pro-260628` | 旗舰，质量好，任意尺寸 |
| `seedream-lite` | `doubao-seedream-5-0-260128` | **有免费额度**，但要求 `size` ≥ 3,686,400 像素（如 `2560x1440` / `2048x2048`） |

**请求**（和其它图片生成一样，prompt 放 user 消息）：
```bash
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{
    "model": "seedream-lite",
    "messages": [{"role":"user","content":"一只金毛幼犬坐在草地上，电影感光照，无真人"}],
    "size": "2560x1440"
  }'
```
**响应**（图片 URL 在 `output.choices[].message.content[].image`，TOS 签名 24h）：
```json
{"code":"Success","output":{"choices":[{"message":{"content":[{"image":"https://...jpeg?X-Tos-Expires=86400&..."}]}}]}}
```

> Seedream 出图较慢（pro ~60–70s），AginxBrain 图片生成超时已放到 180s。

**图生图 / 编辑**（image-to-image）：在 `messages` 的 user 内容里加一个 `image_url` 块作为参考图，AginxBrain 会把它作为 `image` 参数透传给 Seedream。可配合 `size`（支持 `2K` 等别名）、`output_format`、`watermark` 等参数：
```json
{ "role": "user", "content": [
    { "type": "text", "text": "在左下角加一摞杂志，移除草图线条，保持构图" },
    { "type": "image_url", "image_url": { "url": "https://.../sketch.png" } }
] }
```

### 3.6 短剧完整链路（short-drama -> seedream -> seedance）

把上面三块串起来就是短剧生成流水线：

1. **`short-drama`**（doubao 2.1 推理）：写剧本/分镜/首帧画面描述
2. **`seedream` / `seedream-lite`**：按首帧描述生非真人图，拿 `image` URL
3. **`seedance`**（图生视频）：把上一步的 URL 当首帧 `image_url`，加运镜指令，出片

```bash
# 3. seedance 图生视频（用 2. 拿到的 image URL 当首帧）
curl -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{
    "model": "seedance",
    "messages": [{"role":"user","content":[
        {"type":"text","text":"幼犬开始朝镜头奔跑，慢动作"},
        {"type":"image_url","image_url":{"url":"<seedream 返回的 image URL>"}}
    ]}],
    "resolution":"720p","ratio":"16:9","duration":5,"generate_audio":false
  }'
# -> task_id -> 轮询 GET /v1/tasks/seedance/{task_id} -> video_url
```

> 首帧用 Seedream 生成（而非真实照片），Seedance 不触发真人拒识。`generate_audio:false` 出片更快（~2 分钟）。

---

## 4. PPT -- 幻灯片生成（文档 -> PPT，流式）

基于一份文档内容生成可下载的 `.pptx`。后端是 qwen-doc-turbo 的 **模板模式**（`mode: general`）：模型先出大纲、再逐页生成 HTML，最后给出 `.pptx` 下载链接。

### 4.1 关键约定

- **必须流式**：`stream: true`。非流式调用 qwen-doc-turbo 会直接报错（`skill is only supported in stream mode`）。AginxBrain 会对 `ppt` 标签强制 `stream: true`，客户端按 SSE 读取即可。
- **文档内容必须放在 `system` 消息里**：模板模式只认 system message 里的文档内容。结构固定为三条消息：
  1. `system`：角色设定（如 `you are a helpful assistant.`）
  2. `system`：**文档正文**（PPT 的素材来源）
  3. `user`：生成指令（如 `生成一个5页的ppt`）
- **`skill` 参数由 AginxBrain 自动注入**，客户端不用传。可选传 `template_id` 覆盖默认模板。

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `template_id` | 模板：`news_01` / `summary_01` / `internet_01` / `thesis_01` | `news_01` |

### 4.2 请求

```bash
curl -N -X POST https://brain.aginx.net/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "ppt",
    "stream": true,
    "template_id": "news_01",
    "messages": [
      {"role": "system", "content": "you are a helpful assistant."},
      {"role": "system", "content": "人工智能（AI）是计算机科学的一个分支……（这里放完整文档内容）"},
      {"role": "user", "content": "生成一个5页的ppt"}
    ]
  }'
```

> `-N` 关闭 curl 缓冲，实时看到流式输出。文档内容单条消息限制约 9000 Token；更长请先用文件上传接口拿 `file-id`，再以 `system` 消息传 `fileid://{FILE_ID}`（创意模式才支持 file_id，模板模式用纯文本 system 消息）。

### 4.3 流式响应（三阶段）

每个 SSE chunk 是标准 OpenAI Chat 流式格式，关键看两个字段：

| 阶段 | 所在字段 | 内容 |
|------|----------|------|
| 1. 大纲 | `delta.reasoning_content` | PPT 大纲（页数、每页要点、版式规划） |
| 2. 页面 HTML | `delta.reasoning_content` | 逐页的完整 `<html>` 文档（960×540 slide，含内联 CSS） |
| 3. 最终 PPT 文件 | `delta.content` | `.pptx` 下载链接（最后几个 chunk） |

示例 chunk（页面 HTML 阶段）：
```
data: {"choices":[{"delta":{"reasoning_content":"<!DOCTYPE html>...一整页 slide 的 HTML..."},"finish_reason":null}]}
```

最后给出下载链接：
```
data: {"choices":[{"delta":{"content":"http://zhiwen-tob-prod.oss-cn-hangzhou.aliyuncs.com/ppt/.../result.pptx?Expires=...&Signature=..."},"finish_reason":"stop"}]}
data: [DONE]
```

### 4.4 下载 .pptx

`content` 里的链接是阿里云 OSS 临时链接，**有过期时间**（`Expires`），拿到后用 **GET** 下载（HEAD 会被 OSS 签名拒绝返回 403）：

```bash
curl -o result.pptx "http://zhiwen-tob-prod.oss-cn-hangzhou.aliyuncs.com/ppt/.../result.pptx?Expires=...&Signature=..."
```

> 链接 http/https 均可。生成的是真正的 `.pptx`（PowerPoint 可直接打开）。

---

## 5. 速查表

| 能力 | model | 入口 | 输入 | 返回 |
|------|-------|------|------|------|
| TTS | `tts` | POST `/v1/chat/completions` | messages 里的文字 | `/audio/{file}` 路径（再 GET 取音频） |
| ASR | `audio` | POST `/v1/chat/completions` | messages 里 base64 音频 | OpenAI 格式，转写文字在 `message.content` |
| Video | `video` | POST `/v1/chat/completions` | messages 里的 prompt | `task_id`（再轮询） |
| Video 轮询 | — | GET `/v1/tasks/video/{task_id}` | — | `task_status` + `video_url` |
| PPT | `ppt` | POST `/v1/chat/completions` | system 消息放文档，user 消息放指令 | 流式：`reasoning_content`=大纲+各页 HTML，`content`=`.pptx` 下载链接 |
| Seedream | `seedream` | POST `/v1/chat/completions` | messages 里 prompt | `output.choices[].message.content[].image`（TOS URL） |
| Seedream Lite | `seedream-lite` | POST `/v1/chat/completions` | messages 里 prompt（size≥3,686,400px） | 同上（有免费额度） |
| Seedance | `seedance` | POST `/v1/chat/completions` | messages 里 prompt（+可选首帧 image_url） | `task_id`（再轮询） |
| Seedance 轮询 | - | GET `/v1/tasks/seedance/{task_id}` | - | 火山原生：`status` + `content.video_url` |

## 6. 注意事项

- **鉴权**：除 `GET /audio/{file}`（TTS 音频下载）外，所有接口都需要 caller key。
- **视频是异步**：务必用 提交 + 轮询 的两步流程，不要在一个请求里阻塞等待——视频生成耗时不可控。
- **PPT 必须流式**：`ppt` 标签强制 `stream: true`；文档内容放 `system` 消息，生成指令放 `user` 消息，`.pptx` 链接在流末尾的 `content` 字段。
- **OSS 链接过期**：TTS/Video/PPT 返回的下载链接都有 `Expires`，拿到后请及时下载。
- **模型/音色配置**：以上后端模型、音色都在服务器 `~/.aginxbrain/config.yaml` 的 routes/providers 里配置；换模型改配置即可，不用改代码。
