# AginxBrain 开发文档

## 项目定位

AginxBrain 是 Aginx 生态的 **AI 能力网关**。Agent 只需表达需求（对话、生成图片、嵌入……），AginxBrain 负责选 provider、转格式、failover、审计。Agent 开发者不接触真实 API Key，不处理格式差异。

Aginx 生态中的定位：

| 组件 | 职责 | 类比 |
|------|------|------|
| aginx | Agent 互联互通（ACP 路由） | nginx |
| **aginxbrain** | Agent 访问 AI 能力的统一入口 | 大脑 |
| aginx-api | 注册、发现、认证 | DNS |
| aginx-relay | NAT 穿透、连接转发 | CDN |
| aginxium | 统一客户端引擎 | Chromium |

## 技术栈

- **后端**: Rust (Axum HTTP server，内嵌于 Tauri 或独立运行)
- **前端**: React + TypeScript + Vite + TailwindCSS
- **桌面**: Tauri v2 (macOS/Windows/Linux)
- **配置**: YAML

## 项目结构

```
aginxbrain/
├── src-tauri/               # Rust 后端
│   └── src/
│       ├── main.rs              # 入口：默认桌面模式，--server 走 run_server
│       ├── lib.rs                # run_desktop（内嵌网关启动+tray+takeover 命令）与 run_server
│       ├── config.rs             # 配置加载（providers、routes、tags）
│       ├── proxy.rs              # 核心代理逻辑（协议检测、路由、failover）
│       ├── api.rs                # 管理 REST API（provider/route/tag/config CRUD）
│       ├── axum_server.rs        # Axum HTTP 服务器启动
│       ├── takeover.rs           # Codex / Claude Code / Claude 桌面版接管（config.toml、settings.json、3p configLibrary）
│       ├── tray.rs               # 系统托盘
│       └── convert/              # 协议格式转换
│           ├── mod.rs                # 模块导出
│           ├── requests.rs           # 请求转换（Anthropic↔OpenAI↔Responses）
│           ├── responses.rs          # 非流式响应转换
│           └── streaming.rs          # 流式 SSE 转换
├── web/                      # 前端管理界面
│   └── src/
│       ├── App.tsx               # 主应用（tab 导航）
│       ├── lib/api.ts            # 后端 API 调用
│       ├── lib/updater.ts        # 自动更新
│       └── pages/                # Providers / Routes / Tags / Logs 页面
├── config.example.yaml       # 示例配置
└── README.md
```

## 核心架构

### 三层抽象：模型 → 标签 → 路由

```
Agent 请求（model: "opus" / "sonnet" / "gpt-5.5"）
    ↓ resolve_tag_from_model()
标签（opus / sonnet / haiku / auto）
    ↓ find_candidate_routes()
路由列表（按优先级排序，支持 failover）
    ↓ 协议转换
Provider（DeepSeek / 智谱 / Kimi / ...）
```

- **模型名**：客户端发送的名称（`claude-sonnet-4-6`、`gpt-5.5`、直接用标签名）
- **标签**：质量等级抽象（opus=最强，sonnet=均衡，haiku=快速，auto=自动）
- **路由**：标签到具体 provider + 模型的映射，支持多个候选（failover 链）

### 协议转换矩阵

客户端协议 × Provider 格式，共 9 种组合：

| | Provider: OpenAI | Provider: Anthropic | Provider: Responses |
|---|---|---|---|
| **Client: Anthropic** | ✅ Anthropic→OpenAI | ✅ 直通 | ✅ Anthropic→Responses |
| **Client: OpenAI** | ✅ 直通 | ✅ OpenAI→Anthropic | ✅ OpenAI→Responses |
| **Client: Responses** | ✅ Responses→OpenAI | ✅ Responses→Anthropic | ✅ 直通 |

### Codex 接管（Takeover）

当 Codex CLI 配置指向 AginxBrain 时：
- 修改 Codex 的 `config.toml`，设置 `model_provider = "aginxbrain"`
- 修改 VS Code `settings.json`，将 API base URL 指向 AginxBrain
- 使用 `gpt-5.5` 作为虚拟模型名（Codex 目录中存在该模型，确保完整元数据支持）

### 桌面双模式（2026-11-03 起）

桌面 App 不再是纯瘦客户端，启动时**内嵌完整网关**（`run_desktop` 里
`tauri::async_runtime::spawn(bootstrap_gateway)`：load_config → AppState →
axum_server::start，默认 `127.0.0.1:8083`，config `host: 0.0.0.0` 开局域网
共享）。管理台 = 浏览器打开 `http://127.0.0.1:8083/`（release 构建嵌入
web/dist，托盘「打开管理界面」）；窗口（web-client）保留 remote 模式并新增
「使用本地网关」。本地接管 = remote takeover 函数 + `127.0.0.1:port` +
真实 ab_ key（legacy 本地变体写 `PROXY_MANAGED` 已 401，勿用）。host/port
改动重启 App 生效。

## 配置

### `~/.aginxbrain/config.yaml`

```yaml
port: 8083                    # 监听端口
host: 127.0.0.1               # 0.0.0.0 = 局域网共享（桌面模式重启生效）

providers:                    # AI 提供商（真实 Key 在这里；只放凭据）
  deepseek:
    name: DeepSeek
    api_key: sk-real-key
    auth_type: bearer
    # proxy: http://127.0.0.1:8118  # 可选：仅该 provider 的上游走此代理
    #                                # （http/https；如 OpenAI 走美国出口）

routes:                       # 路由规则（base_url 在 route 上）
  - base_url: https://api.deepseek.com
    model: deepseek-v4-pro
    provider: deepseek
    tags: [sonnet, auto]
    format: openai

tags:                         # 质量等级
  - name: opus
    color: "#A855F7"

current_tag: auto             # 默认标签
management_key: aginxbrain-local  # 管理 API 密钥
```

### API 端点

| 端点 | 用途 |
|------|------|
| `POST /v1/chat/completions` | OpenAI Chat 格式代理 |
| `POST /anthropic/v1/messages` (同 `/v1/messages`) | Anthropic Messages 格式代理 |
| `POST /responses` | OpenAI Responses 格式代理（Codex） |
| `GET /v1/models` | 模型列表（Codex 兼容） |
| `GET /api/providers` | 管理：列出 providers |
| `GET /api/routes` | 管理：列出 routes |
| `GET /api/tags` | 管理：列出 tags |
| `GET /api/logs` | 管理：请求日志 |
| `POST /api/test` | 管理：测试指定标签 |

## 构建

```bash
# 管理台前端（release 构建会嵌入 src-tauri/../web/dist，必须先构建）
cd web && pnpm install && pnpm build

# 桌面窗口前端
cd ../web-client && pnpm install && pnpm build

# 开发模式
cd ../src-tauri && cargo tauri dev

# 生产构建（桌面 dmg/app/msi；此时 web/dist 已就绪，rust_embed 才能编译）
cd src-tauri && cargo tauri build
```

注意：`beforeBuildCommand` 为空——两个前端都要手动先 build。release 桌面构建
依赖 `web/dist` 存在（rust_embed 编译期嵌入），缺失会编译失败；debug 构建不嵌
（`/` 返回 JSON 404，用 `cd web && pnpm dev` 起管理台）。

## 部署

- 桌面版：Tauri 打包 dmg/app/msi
- 服务端模式：`cargo build --release` 后直接运行二进制（不需要 Tauri），监听 HTTP 端口
