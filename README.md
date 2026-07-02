<div align="center">

# AginxBrain

**Local-first AI gateway · Bidirectional protocol conversion · Tag-based routing**

本地优先的 AI 能力网关 — 让 Claude Code / Codex CLI 无缝对接任意模型提供商

[![Version](https://img.shields.io/badge/version-0.3.0-blue)](https://github.com/yinnho/AginxBrain/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](./LICENSE)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux%20%7C%20Server-lightgrey)](https://github.com/yinnho/AginxBrain/releases)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange)](https://www.rust-lang.org/)

[Features](#features) · [Why AginxBrain](#why-aginxbrain-vs-other-gateways) · [Quick Start](#quick-start) · [Comparison](#comparison)

</div>

---

AginxBrain sits between your AI client (Claude Code, Codex CLI, any OpenAI/Anthropic/Responses SDK) and your model providers, and does three things transparently:

1. **Protocol conversion** — speak Anthropic Messages, OpenAI Chat Completions, *or* OpenAI Responses on the client side; speak any of them on the provider side. All **9 combinations**, both directions, streaming included.
2. **Tag-based routing** — route by quality tier (`opus` / `sonnet` / `haiku` / `auto`) instead of hard-coding model names, with automatic failover chains.
3. **Key custody** — providers' real API keys live in one place; your apps and teammates never see them.

It runs as a **desktop app (Tauri)** on your own machine — keys never leave your device — or as a **headless server binary**.

> AginxBrain 是 [Aginx](../) Agent 互联网生态中的「大脑」层：Agent 只表达需求（对话、生图、语音……），AginxBrain 负责选 provider、转格式、failover、审计。

---

## Features

### 🔄 Bidirectional protocol conversion (the 9-way matrix)

| Client ↓ / Provider → | OpenAI Chat | OpenAI Responses | Anthropic |
|---|:---:|:---:|:---:|
| **Anthropic Messages** (Claude Code) | ✅ | ✅ | ✅ passthrough |
| **OpenAI Chat** | ✅ passthrough | ✅ | ✅ |
| **OpenAI Responses** (Codex CLI) | ✅ | ✅ passthrough | ✅ |

Streaming SSE is fully converted — `thinking`, `text`, and `tool_use` blocks all translate correctly across formats.

### 🏷️ Tag routing & failover

Route by quality tier, not vendor lock-in:

```
opus   → Zhipu GLM-5.1        (strongest)
sonnet → DeepSeek v4-pro       (balanced)  ──┐ failover chain
haiku  → DeepSeek v4-flash     (fast)       │ on 429 / 400 / timeout
auto   → smart-routing picks   (adaptive)  ──┘
```

Unrecognized model names fall back to `auto` — no request is ever dropped.

### 🔌 One-click client takeover

The management UI rewrites your real client config for you:
- **Claude Code** — flips `settings.json` to point at AginxBrain, restore with one click.
- **Codex CLI** — sets `model_provider = aginxbrain` in `config.toml`.

No env-var juggling, no editing config files by hand.

### 🎨 Multimodal

Image generation (DashScope `wan2.7`, OpenAI images, MiniMax), TTS & ASR (DashScope WebSocket) — all behind the same tag-routing surface.

### 🛡️ Production-grade reliability

- Context-limit / rate-limit / timeout errors are **retryable** → automatic failover to the next route.
- Fast non-streaming failover (45s) + 10s connect timeout — no more silent stalls.
- `reasoning_content` stripped from fast/haiku tiers so classifiers and simple chat stay clean.
- Per-caller API keys, usage logging, cost tracking.

### 🖥️ Runs anywhere

- **Desktop**: native app, system-tray resident, built-in web UI at `http://127.0.0.1:8083`.
- **Server**: `cargo build --release --features server` → single static binary, systemd-ready.

---

## Comparison

Most LLM gateways (One API, OpenRouter, Helicone) only expose an **OpenAI-compatible input** — so Claude Code (Anthropic Messages) and Codex CLI (Responses) **cannot connect unchanged**. AginxBrain is **local-first + protocol-native**:

| | AginxBrain | Portkey | LiteLLM | New API | One API | OpenRouter |
|---|---|---|---|---|---|---|
| Anthropic Messages **input** | ✅ | ✅ | ✅ (beta) | ✅ | ❌ | ✅ |
| OpenAI Responses **input** | ✅ | ✅ | partial | ✅ | ❌ | ❌ |
| Local-first / desktop | ✅ Tauri | ❌ server | ❌ server | ❌ server | ❌ server | ❌ SaaS |
| One-click Claude Code / Codex takeover | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Tag-based quality routing | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| China providers first-class | ✅ | partial | partial | ✅ | ✅ | ✅ |
| Keys stay on your device | ✅ | depends | depends | ❌ | ❌ | ❌ |

Full breakdown: **[COMPARISON.md](./COMPARISON.md)** · 中文省钱攻略: **[ARTICLE.md](./ARTICLE.md)**

---

## Quick Start

### Option A — Download

Grab the installer for your platform from [**Releases**](https://github.com/yinnho/AginxBrain/releases), install, and open the web UI.

### Option B — Build from source

```bash
git clone https://github.com/yinnho/AginxBrain.git
cd AginxBrain

# Desktop dev
npm --prefix web install
npm --prefix web run tauri dev

# Server binary (no Tauri/desktop deps)
cd src-tauri && cargo build --release --no-default-features --features server
```

### Configure

Edit `~/.aginxbrain/config.yaml`. Providers hold only auth; **routes own their full `base_url`**:

```yaml
port: 8083
host: 127.0.0.1
current_tag: auto

providers:
  deepseek:
    name: DeepSeek
    api_key: sk-your-key
    auth_type: bearer
  zhipu:
    name: Zhipu GLM
    api_key: sk-your-key
    auth_type: bearer

routes:
  - base_url: https://api.deepseek.com
    model: deepseek-v4-pro
    provider: deepseek
    tags: [sonnet, auto]
    format: openai              # OpenAI Chat Completions

  - base_url: https://open.bigmodel.cn/api/anthropic
    model: glm-5.1
    provider: zhipu
    tags: [opus]
    format: anthropic           # Anthropic Messages (passthrough from Claude Code)

tags:
  - { name: opus,   color: "#A855F7" }
  - { name: sonnet, color: "#3B82F6" }
  - { name: haiku,  color: "#22C55E" }
  - { name: auto,   color: "#F59E0B", is_auto: true }

management_key: aginxbrain-local
```

### Route formats

| `format` | Wire format | Path derived |
|---|---|---|
| `openai` | OpenAI Chat Completions | `/v1/chat/completions` |
| `openai_responses` | OpenAI Responses API | `/v1/responses` |
| `anthropic` | Anthropic Messages | `/v1/messages` |
| `dashscope_image` / `dashscope_tts` / `dashscope_asr` / `openai_images` / `minimax_image` | multimodal | per-format |

### Use

1. Launch AginxBrain (system tray).
2. Open `http://127.0.0.1:8083`.
3. Toggle **Claude Code** or **Codex** takeover.
4. Use your CLI normally — all traffic flows through AginxBrain.

---

## Screenshots

| Logs (live request stream) | Routes |
|---|---|
| ![Logs](./web/src/assets/screenshots/logs.png) | ![Routes](./web/src/assets/screenshots/routes.png) |

| Takeover | Tags |
|---|---|
| ![Takeover](./web/src/assets/screenshots/takeover.png) | ![Tags](./web/src/assets/screenshots/tags.png) |

---

## Architecture

```
  Claude Code / Codex CLI / any SDK
              │  HTTP (Anthropic | OpenAI | Responses)
              ▼
      AginxBrain  (Rust · axum · Tauri)
        ├──  protocol conversion  (9-way, streaming)
        ├──  tag routing + failover
        └──  model-name sanitization
              │
   ┌──────────┼──────────┬──────────┐
   ▼          ▼          ▼          ▼
 OpenAI    Responses  Anthropic   Image/TTS/ASR
(DeepSeek) (DashScope) (Zhipu…)   (wan2.7…)
```

## Endpoints

| Endpoint | Protocol |
|---|---|
| `POST /v1/chat/completions`, `/openai/v1/chat/completions` | OpenAI Chat |
| `POST /api/anthropic/v1/messages`, `/v1/messages` | Anthropic Messages |
| `POST /responses`, `/v1/responses` | OpenAI Responses (Codex) |
| `GET /v1/models` | model list (Codex-compatible) |
| `GET /api/logs` · `/api/providers` · `/api/routes` · `/api/tags` | admin |

---

## Project context

AginxBrain is the **AI-capability gateway** of the [Aginx](../) ecosystem — Agent infrastructure modeled on the internet stack:

| Component | Role | Analogy |
|---|---|---|
| aginx | Agent interconnect (ACP routing) | nginx |
| **aginxbrain** | unified AI-capability entry | the brain |
| aginx-api | registry / discovery / auth | DNS |
| aginx-relay | NAT traversal / forwarding | CDN |
| aginxium | unified client engine | Chromium |

## License

[MIT](./LICENSE) · © 2026 yinnho
