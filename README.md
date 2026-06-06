# AginxLLM

> AI 模型代理 · 协议转换 · 智能路由 — 无缝对接 Anthropic Messages、OpenAI Chat Completions 与 OpenAI Responses API

AginxLLM 是一个轻量级桌面应用，置于 AI 客户端（如 Claude Code、Codex CLI）与模型提供商之间，实现透明的协议转换和基于标签的智能路由。

---

## 特性

### 协议转换

支持三种主流 API 格式的任意互转：

| 客户端格式 | Provider 格式 | 典型场景 |
|-----------|-------------|---------|
| Anthropic Messages | OpenAI Chat Completions | DeepSeek、Moonshot (Kimi)、SiliconFlow |
| Anthropic Messages | OpenAI Responses API | 通义千问 DashScope |
| Anthropic Messages | Anthropic（透传） | 百度文心、智谱 GLM |
| OpenAI Chat Completions | OpenAI Responses API | Codex CLI |
| OpenAI Responses API | OpenAI Chat Completions | Codex CLI |
| OpenAI Responses API | Anthropic Messages | Codex CLI |

流式 SSE 转换同样完整支持，thinking 块、text 块、tool_use 块全部正确处理。

### 标签路由系统

通过 `opus` / `sonnet` / `haiku` / `auto` 标签自由路由请求：

```
opus   → 百度文心 (qianfan-code-latest)
sonnet → DeepSeek (deepseek-v4-pro)
haiku  → Moonshot Kimi (K2.6)
auto   → 智谱 GLM (glm-5.1)
```

任何未识别的模型名自动走 `auto` 路由，不会丢失请求。

### Claude Code 一键接管

管理界面点击 "Takeover"，自动配置 Claude Code 的环境变量，将所有流量路由到 AginxLLM。点击 "Restore" 一键恢复。

### Codex CLI 支持

管理界面点击 "Codex" 开关，自动将 Codex CLI（OpenAI Responses API 协议）连接到 AginxLLM。支持的 Codex 模型名：`gpt-5.2`、`gpt-5.3-codex`、`gpt-5.4`、`gpt-5.4-mini`、`gpt-5.5`。

### 模型名保护

provider 返回的 `model` 字段会被替换为原始请求的模型别名，彻底切断客户端模型反馈循环。

### 管理界面

内建 Web UI（`http://127.0.0.1:8083`）：

- 实时请求日志
- Provider / 路由 / 标签管理
- Takeover 开关状态
- 一键测试路由

### Thinking Blocks 自动处理

支持 `type: "thinking"` 内容块的自动转换与透传，兼容 DeepSeek 等 provider 的 `reasoning_content`。

---

## 架构

```
  Claude Code / Codex CLI
         │
         ▼  HTTP
  AginxLLM (Tauri v2 + axum)
         │
         ├─── 协议转换引擎 (Anthropic ↔ OpenAI ↔ Responses)
         ├─── 标签路由 (tag → provider)
         └─── 模型名替换
                │
        ┌───────┴────────┬──────────┐
        ▼                ▼          ▼
   OpenAI Chat     OpenAI Resp.  Anthropic
   (DeepSeek,      (DashScope)   (Baidu, Zhipu)
    Moonshot, ...)
```

---

## 快速开始

### 下载安装

从 [Releases](https://github.com/yinnho/model-router/releases) 下载对应平台的安装包，双击安装即用。

### 源码编译

```bash
git clone https://github.com/yinnho/model-router
cd aginxllm

# 开发模式
npm --prefix web install
npm --prefix web run tauri dev

# 构建安装包
npm --prefix web run tauri build
```

### 配置

编辑 `~/.aginxllm/config.yaml`：

```yaml
port: 8083
current_tag: auto

tags:
  - name: opus
    color: "#A855F7"
  - name: sonnet
    color: "#3B82F6"
  - name: haiku
    color: "#22C55E"
  - name: auto
    color: "#F59E0B"
    is_auto: true

providers:
  deepseek:
    name: DeepSeek
    base_url: https://api.deepseek.com
    api_key: sk-your-key
    auth_type: bearer
  dashscope_responses:
    name: Qwen (DashScope)
    base_url: https://dashscope.aliyuncs.com/compatible-mode
    api_key: sk-your-key
    auth_type: bearer

routes:
  - endpoint: /v1/chat/completions
    model: deepseek-v4-pro
    provider: deepseek
    tags: [sonnet]
    format: openai
  - endpoint: /v1/responses
    model: qwen-plus
    provider: dashscope_responses
    tags: [haiku]
    format: openai_responses
```

### 使用

1. 启动 AginxLLM（系统托盘常驻）
2. 浏览器打开 `http://127.0.0.1:8083`
3. 点击 **Claude Code** 或 **Codex** 开关接管 CLI 配置
4. 正常使用 Claude Code / Codex CLI — 所有流量自动经过 AginxLLM

---

## 配置参考

| 字段 | 类型 | 说明 |
|-----|------|------|
| `port` | number | 监听端口 (默认 8083) |
| `current_tag` | string | 当前激活的标签 |
| `management_key` | string | 管理 API 认证密钥 (默认 `model-router-local`) |
| `providers` | map | Provider 配置 (name / base_url / api_key / auth_type) |
| `routes` | array | 路由规则 (endpoint / model / provider / tags / format) |
| `tags` | array | 标签定义 (name / color / is_auto) |

### Provider format

- `anthropic` — 透传 Anthropic Messages 格式
- `openai` — OpenAI Chat Completions 格式
- `openai_responses` — OpenAI Responses API 格式

---

## License

MIT
