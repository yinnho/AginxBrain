# OpenCarrier 流式接入规范 (Streaming Migration Spec)

> 目标读者：OpenCarrier LLM driver 开发者
> 背景：通过 AginxBrain 网关访问 LLM 的所有 agent，应将非流式请求改为流式（SSE），以消除重负载下的 60s header 超时和模型卡顿不可恢复问题。
> 状态：AginxBrain 网关侧已完全就绪，**零改动**；所有变更在 OpenCarrier 的 LLM driver。

---

## 1. 问题陈述

### 1.1 现状（非流式）的失败模式

OpenCarrier 当前对 AginxBrain 发送非流式请求（`stream=false`）。在重 context（如 ai-writer 的 72 条消息 reasoning）下，观察到：

```
agent → AginxBrain (非流式) → 上游模型 (glm-5.2)
                                   ↑
                        接受连接后长时间不出字（卡顿 / 慢首 token）
                                   ↓
客户端 60s 收不到任何 HTTP 响应 → "Response header timeout" 硬超时 → 任务失败
```

**根本矛盾**：非流式下，网关必须等上游**完整响应**才能回客户端。客户端 60s 超时 < 网关 120s failover 超时，导致 failover **来不及触发**，请求直接失败。此问题在网关侧无法解决。

### 1.2 流式如何解决

流式（SSE）下，响应 header 与第一个 token **立即返回**：

- **永不撞 header 超时** —— 客户端立刻拿到 HTTP 200 + 流头。
- **卡顿立刻暴露** —— 加一个"首 token 超时"（见 §6），收不到第一个 chunk 就判定卡顿并重试，把"60s 卡死无救"变成"20s 无首字 → 自动重试"。
- **正常慢推理持续吐字** —— reasoning 模型边想边吐，不会因"思考太久"触发超时。

---

## 2. AginxBrain 网关侧已就绪的能力（无需改动）

OpenCarrier 只需消费 SSE，网关已提供：

| 能力 | 说明 |
|------|------|
| 9 种协议流式互转 | OpenAI Chat / Anthropic Messages / OpenAI Responses 任意 client × provider 组合，流式全覆盖 |
| thinking 块 | 推理内容正确转换（`reasoning_content` ↔ `thinking` ↔ `reasoning_summary`） |
| tool_use 块 | 工具调用流式正确转换 |
| 模型名替换 | 流式也替换上游 `model` 字段为客户端请求的别名，切断模型反馈循环 |
| fast/haiku 剥离推理 | 流式剥离 `reasoning_content`，分类器/轻量 chat 不被推理链污染 |
| usage 估算兜底 | 上游 usage 上报不全时，从 SSE 内容估算 |

**结论：OpenCarrier 全部改 `stream: true` + 加 SSE 解析器即可，网关不动。**

---

## 3. OpenCarrier 改造清单

| # | 改动 | 说明 |
|---|------|------|
| 1 | 所有请求加 `stream: true` | 见 §4.1 |
| 2 | 实现 SSE 流式解析器 | 替换"读完整 JSON body"，见 §5 |
| 3 | 累积 tool_call delta | 流式下工具调用分片到达，需按 index 拼接，见 §5.4 |
| 4 | 从终止事件读 usage | 流式无顶层 usage，见 §5.5 |
| 5 | 加首-token 超时（卡顿检测） | 见 §6，**最大收益点** |
| 6 | 错误事件处理 | 流中途的错误事件，见 §7 |

---

## 4. 请求格式

### 4.1 启用流式

所有请求 body 加 `"stream": true`：

```json
{
  "model": "reasoning",
  "messages": [...],
  "stream": true,
  "stream_options": { "include_usage": true },
  ...
}
```

> **OpenAI Chat 协议**：建议同时传 `stream_options: {"include_usage": true}`，以便在流的末尾收到 usage（见 §5.5）。Anthropic 协议不需要此参数。

### 4.2 端点与协议

OpenCarrier 按 agent 现有端点发送即可（协议不变，只加 stream）：

| 协议 | 端点 |
|------|------|
| OpenAI Chat | `POST /v1/chat/completions` |
| Anthropic Messages | `POST /api/anthropic/v1/messages`（或 `/v1/messages`） |
| OpenAI Responses | `POST /responses`（或 `/v1/responses`） |

响应均为 `Content-Type: text/event-stream`（SSE）。

---

## 5. SSE 解析规范

SSE 格式：每条事件由若干行组成，`data:` 行承载 JSON，事件之间用空行分隔。部分协议（Anthropic / Responses）还带 `event:` 行标注类型。

### 5.1 通用解析步骤

```
按 "\n\n" 切分事件块
对每个块：
    从 "data: " 行提�� JSON（一行或多行拼接）
    解析 JSON，按协议分发处理（见 5.2 / 5.3 / 5.4）
    "data: [DONE]" 表示流结束（仅 OpenAI Chat）
```

### 5.2 OpenAI Chat 流式事件

```text
data: {"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}

data: {"choices":[{"index":0,"delta":{"content":"Hello"}}]}

data: {"choices":[{"index":0,"delta":{"content":" world"}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\""}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\":\"北京\"}"}}]}}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":50,"completion_tokens":12,"total_tokens":62}}

data: [DONE]
```

关键字段：
- `choices[0].delta.content` → 文本增量，累加
- `choices[0].delta.tool_calls` → 工具调用增量，**按 `index` 累积**（见 §5.4）
- `choices[0].finish_reason` → `"stop"` / `"tool_calls"` / `"length"`
- `usage`（仅当 `stream_options.include_usage=true`）→ 在 `finish_reason` 那帧或其后一帧出现
- `data: [DONE]` → 流结束

### 5.3 Anthropic Messages 流式事件

```text
event: message_start
data: {"type":"message_start","message":{"id":"msg_1","role":"assistant","model":"...","usage":{"input_tokens":50,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}

event: message_stop
data: {"type":"message_stop"}
```

content block 类型：
- `text` → delta 类型 `text_delta`，累加 `delta.text`
- `thinking` → delta 类型 `thinking_delta`，累加 `delta.thinking`（推理内容）
- `tool_use` → `content_block_start` 带 `id` / `name`，delta 类型 `input_json_delta`，累加 `delta.partial_json`（见 §5.4）

关键终止事件：
- `message_delta` → 带 `stop_reason` 和 `output_tokens`
- `message_stop` → 流结束

### 5.4 ⭐ 工具调用累积（最重要的实现点）

流式下工具调用**分片到达**，必须累积。非流式是一次性给完整对象，流式要自己拼。

#### OpenAI Chat 累积（伪代码）

```python
text = ""
tool_calls = {}   # index -> {"id":..., "name":..., "arguments":""}
finish_reason = None
usage = None

for chunk in sse_stream:
    delta = chunk["choices"][0]["delta"]
    if "content" in delta and delta["content"]:
        text += delta["content"]
    if "tool_calls" in delta:
        for tc in delta["tool_calls"]:
            idx = tc["index"]
            slot = tool_calls.setdefault(idx, {"id": "", "name": "", "arguments": ""})
            if tc.get("id"):          slot["id"] = tc["id"]              # 首帧带 id
            fn = tc.get("function", {})
            if fn.get("name"):        slot["name"] = fn["name"]          # 首帧带 name
            if fn.get("arguments"):   slot["arguments"] += fn["arguments"]  # 后续帧拼参数
    if chunk["choices"][0].get("finish_reason"):
        finish_reason = chunk["choices"][0]["finish_reason"]
    if chunk.get("usage"):
        usage = chunk["usage"]

# 流结束后：把每个 tool_call 的 arguments 字符串解析为 JSON
for tc in tool_calls.values():
    tc["input"] = json.loads(tc["arguments"])   # "{}" -> {}
```

#### Anthropic tool_use 累积（伪代码）

```python
blocks = {}  # index -> {"type":..., "text":"", "thinking":"", "id":"", "name":"", "partial_json":""}
stop_reason = None
output_tokens = 0

for ev_type, data in sse_stream:
    if ev_type == "content_block_start":
        i = data["index"]; cb = data["content_block"]
        blocks[i] = {"type": cb["type"], "text": "", "thinking": "",
                     "id": cb.get("id",""), "name": cb.get("name",""), "partial_json": ""}
    elif ev_type == "content_block_delta":
        i = data["index"]; d = data["delta"]; b = blocks[i]
        if d["type"] == "text_delta":        b["text"] += d["text"]
        elif d["type"] == "thinking_delta":  b["thinking"] += d["thinking"]
        elif d["type"] == "input_json_delta": b["partial_json"] += d["partial_json"]
    elif ev_type == "message_delta":
        stop_reason = data["delta"].get("stop_reason")
        output_tokens = data["usage"].get("output_tokens", output_tokens)

# 流结束后：tool_use 块的 partial_json 解析为 input
for b in blocks.values():
    if b["type"] == "tool_use":
        b["input"] = json.loads(b["partial_json"])
```

> **坑提示**：`arguments` / `partial_json` 在第一帧可能是空字符串，后续帧增量拼接，**不要在中间帧尝试 JSON 解析**，必须等流结束后整体解析。

### 5.5 usage 提取

流式没有顶层 `usage` 字段，按协议从终止事件取：

| 协议 | input_tokens | output_tokens |
|------|--------------|---------------|
| OpenAI Chat | `usage.prompt_tokens`（带 `include_usage` 时） | `usage.completion_tokens` |
| Anthropic | `message_start.message.usage.input_tokens` | `message_delta.usage.output_tokens` |
| Responses | `response.completed` 事件的 `usage` 对象 | 同上 |

> AginxBrain 在上游 usage 缺失时会从 SSE 内容估算 output tokens 兜底，但**客户端仍应以终止事件的 usage 为准**。

---

## 6. ⭐ 超时与卡顿检测策略

这是流式改造**最大的可靠性收益**。建议 driver 实现以下分层超时：

| 超时层 | 建议值 | 触发动作 |
|--------|--------|---------|
| 连接超时 | 10s | 连不上网关 → 重试 / 报错 |
| **首 token 超时** | **20s** | **收不到第一个 chunk → 判定卡顿，重试（同路由或换路由）** |
| 流间隔超时 | 30s | 流中途某 chunk 间超过 30s → 判定卡顿，中断重试 |
| 总时长 | 不设硬上限（或 300s+） | 长输出不应被掐断 |

**核心**：用"首 token 超时 20s"替代原来的"60s header 超时"。正常慢推理会持续吐字（不触发首 token 超时，因为它早就有首字了），只有真正卡顿（接了连接不出字）才会在 20s 触发重试。

重试策略建议：首-token 超时后，重试 1 次；仍无首字则报错（AginxBrain 侧也会 failover）。

---

## 7. 错误处理

- **流开始前的 HTTP 错误**（4xx/5xx）：网关在建立流之前返回普通 JSON 错误（非 SSE），driver �� HTTP 状态码处理（与现状一致）。
- **流中途的错误事件**：网关在 SSE 流里发一条错误事件后关闭。driver 应捕获并终止当前流。
- **客户端主动断开**：driver 取消时，网关会后台排空上游以保留 usage 统计，无需特殊处理。

---

## 8. 迁移计划

1. **第一阶段（必做）**：`reasoning` 和 `chat` 两个重负载档改流式（这俩才会卡）。`fast` / `haiku` 档可选。
2. **driver 抽象**：把"读完整 body 一次"重构为"SSE 解析器"——优先用成熟库（各语言均有 OpenAI / Anthropic SSE 客户端），**不要手写解析**。
3. **过渡期 fallback**：流式解析报错时降级为非流式重试一次，避免解析 bug 导致全量失败。
4. **灰度**：先在 ai-writer（最易触发重 context 卡顿）验证，再推全量。

---

## 9. 验收清单

- [ ] 所有请求带 `stream: true`
- [ ] 三种协议（OpenAI Chat / Anthropic / Responses，按 agent 实际使用的）SSE 解析正确
- [ ] tool_call delta 累积正确（多帧 arguments 拼接 + 最终 JSON 解析）
- [ ] usage 从终止事件正确提取
- [ ] 首-token 超时（20s）实现，触发后自动重试
- [ ] 流中途错误事件能捕获并终止
- [ ] ai-writer 72 条消息 reasoning 场景：20s 内拿到首字，不再 60s 超时

---

## 附录 A：协议选型建议

OpenCarrier 各 agent 用的协议不必统一——AginxBrain 会做转换。但为降低 driver 维护成本，**建议 OpenCarrier 内部标准化为一种流式协议**（推荐 OpenAI Chat，生态最成熟），由 AginxBrain 负责对接不同 provider 格式。

## 附录 B：为什么网关不能自己改成流式

OpenCarrier 发的是非流式请求，网关只能按非流式处理（等完整响应再返回）。**流式必须由客户端（OpenCarrier）发起 `stream: true`**，网关无法替客户端决定。因此改造在 OpenCarrier 侧，这是协议决定的，不是分工偏好。
