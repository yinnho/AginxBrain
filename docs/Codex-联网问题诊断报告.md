# Codex「无法联网 / 未连上 aginxbrain」诊断报告

排查时间：2026-10-09 17:01
机器：macOS（本地用户 chennan）
Codex CLI 版本：0.143.0（`/opt/homebrew/bin/codex`）

---

## 一句话结论

**网络是通的，问题不在网络，而在鉴权模式。**

Codex 有两条独立的对外通道：

1. **模型推理通道** → `brain.aginx.net`（aginxbrain）—— **完全正常**
2. **ChatGPT 云端通道** → `chatgpt.com` / `api.openai.com`—— **全部被拒**

你现在的 `auth.json` 里只有 API Key（`ab_…`），没有 ChatGPT 登录态。而 Codex 桌面版的云端功能（远程控制、云任务、插件目录同步、使用量、推送）**硬性要求 ChatGPT 账号登录，明确不支持 API Key**。aginxbrain 只能替换第 1 条通道，管不到第 2 条。

---

## 一、网络层实测：正常

| 检查项 | 结果 |
| --- | --- |
| `brain.aginx.net` DNS 解析 | → `106.75.32.216` ✅ |
| HTTPS 直连（绕过代理） | `HTTP 200`，0.13s ✅ |
| HTTPS 走本地代理 `127.0.0.1:57871` | `HTTP 200`，0.16s ✅ |
| `GET /v1/models`（带 key） | `HTTP 200`，返回模型列表 ✅ |
| `POST /v1/responses`（`gpt-5.5`） | `HTTP 200`，正常返回 ✅ |
| `POST /responses`（无 `/v1`） | `HTTP 200` ✅ |
| `api.openai.com`（无代理） | 超时 ⏱️（国内正常现象，与本次故障无关） |

**实测运行 Codex CLI：**

```
codex exec --skip-git-repo-check "reply with exactly: OK"
→ model: gpt-5.5 / provider: aginxbrain / 输出 "OK" / tokens used 1,661
```

→ CLI 走 aginxbrain 这条路是**通**的，密钥有效、模型可用。

---

## 二、当前配置状态

`~/.codex/config.toml`（当前生效，15 行）：

```toml
model = "gpt-5.5"
model_provider = "aginxbrain"
preferred_auth_method = "apikey"
disable_response_storage = true

[model_providers.aginxbrain]
name = "AginxBrain"
base_url = "https://brain.aginx.net"
wire_api = "responses"
requires_openai_auth = true
supports_websockets = false
```

`~/.codex/auth.json`（当前生效）：

```json
{ "OPENAI_API_KEY": "ab_08ee…（aginxbrain 的 key）" }
```

对比备份 `~/.codex/auth.json.aginxbrain-backup`：里面是**原 ChatGPT 登录态**（`auth_mode: "chatgpt"`，账号 `cn***@gmail.com`，已脱敏）。解码其 `id_token` 可知订阅计划为 `prolite`，有效期 **截至 2026-10-05 —— 已经过期**。之后你切换成了 aginxbrain 的 API Key 模式。

**这就是分界线**：从 ChatGPT 登录 → API Key 的那一刻起，桌面版的云端能力就全部失效了。

---

## 三、桌面版故障日志（`~/Library/Logs/com.openai.codex/2026/10/09`）

三条最直接的证据：

```
error  remote control requires ChatGPT authentication; API key auth is not supported
warn   chatgpt authentication required for remote plugin catalog; api key auth is not supported
error  failed to connect to websocket: HTTP error: 401 Unauthorized,
       url: wss://api.openai.com/v1/responses
```

云端接口调用统计（今日）：

| 状态码 | 接口 | 次数 |
| --- | --- | --- |
| 432 | `/wham/usage` | 606 |
| 432 | `/wham/tasks/list` | 338 |
| 432 | `/settings/user` | 176 |
| 432 | `/pins` | 3 |
| 432 | `/gizmos/snorlax/sidebar` | 2 |
| 432 | `/conversations` | 1 |

伴随报错：

```
warning  sa_server_request_failed ... errorMessage="Workspace routing is unavailable"   ×1125
info     remote_connections.connection_state_changed ... state=disconnected
```

`Workspace routing is unavailable` 就是 `432` 的应用层说法 —— 服务端认出了请求带了鉴权（`attachAuth=true`），但那是 API Key，不是 ChatGPT 会话，所以拒绝路由。

> 注：桌面版自身的**本地 agent** 是好的（当天上午还在正常跑任务、有推理摘要和工具调用）。坏的只有「云」那部分。

---

## 四、所以「连不上 aginxbrain」是怎么回事

aginxbrain 是通过 `model_provider` 注入的 **模型层** 代理，只覆盖「我该把 prompt 发给谁」。

而桌面版里所有标着「云 / 远程 / 跨设备」的功能，是 Codex 客户端**内置写死**走 OpenAI 官方后端的（`chatgpt.com`、`api.openai.com`），既不读 `model_provider`，也不接受任何第三方中转。这跟网络代理、跟 aginxbrain 的配置都没有关系 —— 换任何中转站结果都一样。

---

## 五、可选处置方案

### 方案 A：保持 API Key 模式（省钱，推荐当前状态）
- CLI 用 `codex` 命令 → 走 aginxbrain，**完全可用**，这是你目前最顺的路径。
- 桌面版里**避开云端相关按钮**（远程控制、云任务列表、跨设备），只当本地 agent 用。

### 方案 B：恢复桌面版云端能力
- 必须重新用 **ChatGPT 账号登录**（桌面版 → 设置 → 登录）。
- 需要有效的 ChatGPT 订阅（原 `prolite` 已于 2026-10-05 到期）。
- 注意：登录 ChatGPT 后，`auth.json` 会回到 `auth_mode: "chatgpt"`，此时 `preferred_auth_method = "apikey"` 与 aginxbrain provider 的共存关系需要重新验证。

### 方案 C：顺手修掉两个附带问题
1. **配置被覆盖**：现在的 `config.toml` 是精简版，你原有的 `[marketplaces.*]`、`[plugins.*]`、`[mcp_servers.*]`、`[projects.*]`、`notify`、`[desktop]` 全部只剩在 `config.toml.aginxbrain-backup` 里，没合并回来 → 桌面版的插件启用状态、项目信任标记会丢。建议把这些段合并回 `config.toml`，同时保留 aginxbrain 的 `model` / `model_provider` / `[model_providers.aginxbrain]`。
2. **模型缓存告警**：`ERROR codex_models_manager::cache: failed to load models cache: missing field 'base_instructions'` → `~/.codex/models_cache.json` 结构过旧，删除后会自动重建。

---

## 附：涉及的文件清单

| 路径 | 说明 |
| --- | --- |
| `~/.codex/config.toml` | 当前生效配置（aginxbrain 精简版） |
| `~/.codex/config.toml.aginxbrain-backup` | 被覆盖前的完整配置（含 plugins / mcp / projects） |
| `~/.codex/auth.json` | 当前生效鉴权（仅 API Key） |
| `~/.codex/auth.json.aginxbrain-backup` | 原 ChatGPT 登录态（订阅已于 2026-10-05 到期） |
| `~/.codex/models_cache.json` | 模型缓存（结构过旧，建议删除重建） |
| `~/Library/Logs/com.openai.codex/2026/10/09/` | 桌面版日志（本次诊断主要证据来源） |
