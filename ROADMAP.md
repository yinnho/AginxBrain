# AginxBrain 产品规划

## 一句话

**AginxBrain 是 Agent 的 AI 大脑——Agent 只说需求，AginxBrain 负责剩下的一切。**

---

## 解决什么问题

### Agent 开发者的痛苦

1. **格式地狱** — Anthropic、OpenAI、Gemini、DeepSeek……每个 provider 的 API 格式都不一样。Agent 要支持多个 provider，就得写大量格式转换代码。
2. **Key 管理混乱** — API Key 散落在每台机器上，无法统一管理、轮换、回收。
3. **Failover 手动** — 一个 provider 挂了，Agent 自己要处理重试、切换。
4. **无法审计** — 谁用了什么模型、花了多少钱、发了什么数据，完全没有追踪。

### 企业的痛苦

1. **Key 泄露风险** — 开发者人手一个 Key，离职后 Key 仍然有效。
2. **成本失控** — 没有额度控制，没有人知道每个月花了多少。
3. **合规风险** — 敏感数据发到了境外的 API，无法管控。

---

## AginxBrain 是什么

一个**部署在任何地方的 AI 能力网关**。Agent 不直接调 LLM provider，而是调 AginxBrain：

```
Agent  ──"我要 opus 级别的对话"──→  AginxBrain  ──→  DeepSeek / 智谱 / ...
Agent  ──"生成一张猫的图片"──────→  AginxBrain  ──→  DALL-E / Midjourney / ...
Agent  ──"给我文本嵌入"────────→  AginxBrain  ──→  OpenAI / Cohere / ...
```

Agent 不需要知道后面是什么 provider、什么格式、什么 Key。它只知道 AginxBrain 的地址和自己的 token。

---

## 三个阶段

### 第一阶段：统一 LLM 网关（当前）

**目标：** Agent 接入一个地址，透明使用所有 LLM provider。

当前 model-router-v2 已具备的能力：
- ✅ 多 provider 配置（DeepSeek、智谱、Kimi、百度、MiniMax……）
- ✅ 标签路由（opus/sonnet/haiku/auto → 不同 provider）
- ✅ 自动 failover（按路由优先级依次尝试）
- ✅ 格式转换（Anthropic ↔ OpenAI ↔ Responses API，9 种组合全覆盖）
- ✅ 流式转换（SSE 逐层转换）
- ✅ Codex / Claude Code 接管
- ✅ 桌面管理界面（provider/route/tag CRUD + 日志）
- ✅ 桌面 app（Tauri，macOS/Windows）

**本阶段要补的：**
- 🔲 用户系统 — 管理员创建用户，每个用户分配 API Token + 额度
- 🔲 Token 认证 — Agent 请求带 `Authorization: Bearer <user-token>`，网关验证身份
- 🔲 额度控制 — 每用户/标签的调用次数或 token 用量限制
- 🔲 独立部署 — 不依赖 Tauri，纯二进制 + Web UI，Docker 镜像

### 第二阶段：企业级能力

**目标：** 团队和企业的 AI 基础设施。

- 🔲 成本追踪 — 按 provider/用户/标签统计费用，可视化报表
- 🔲 审计日志 — 完整记录每次请求（谁、什么时候、哪个模型、输入摘要）
- 🔲 数据合规 — 规则引擎（如：含敏感信息的请求不转发到境外 provider）
- 🔲 Key 轮换 — 自动检测 Key 余额/到期，主动切换
- 🔲 智能路由 — 根据请求内容自动选 provider（代码类 → 代码模型，翻译 → 便宜模型）
- 🔲 多模态 — 图片生成（DALL-E / Stable Diffusion）、语音（TTS/STT）、嵌入（Embedding）

### 第三阶段：与 Aginx 生态融合

**目标：** AginxBrain 成为 Aginx 生态的 AI 能力层。

- 🔲 aginx 原生集成 — aginx 配置中指定 AginxBrain 地址，Agent 自动获得 AI 能力
- 🔲 Agent 编排 — Agent A 调 Agent B 时，中间的 LLM 请求自动走 AginxBrain
- 🔲 能力市场 — 第三方注册新的 AI 能力（图片、语音、搜索……），Agent 发现和使用
- 🔲 aginx-api 联动 — AginxBrain 实例注册到 aginx-api，其他 aginx 可发现

---

## 与 New API 的对比

[New API](https://github.com/QuantumNous/new-api) 是成熟的 LLM 网关，AginxBrain 的第一阶段功能与它高度重合。差异在于：

| 维度 | New API | AginxBrain |
|------|---------|------------|
| 定位 | 通用 LLM 管理平台 | Aginx 生态的 AI 能力层 |
| 生态 | 独立产品 | 与 aginx、aginxium 深度集成 |
| Agent 接入 | 标准化后即止 | 未来支持意图路由（"生成图片"→ 自动选 provider） |
| 部署 | Docker（Go + React） | 单二进制（Rust），零依赖 |
| 协议 | OpenAI/Claude/Gemini | 同，且原生支持 Codex Responses API |
| 长期方向 | API 管理和分发 | Agent AI 能力的统一抽象 |

**第一阶段参考 New API 的用户/额度/审计体系，但不复制其实现。** 用 Rust 重写，保持轻量、高性能、单二进制部署。

---

## 部署形态

```
个人开发者：
  AginxBrain 跑在本机 → Agent 指向 localhost:8083

团队/公司：
  AginxBrain 跑在内网服务器 → 所有开发者指向 brain.company.internal:8083
  管理员在 Web UI 管理 Key、额度、路由

云服务：
  AginxBrain 跑在云上 → 多地 Agent 通过公网/VPN 接入
```

---

## 技术决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 语言 | Rust | 高性能、低资源、单二进制 |
| HTTP 框架 | Axum | 异步、类型安全、生态好 |
| 前端 | React + Vite | 轻量、快速 |
| 桌面 | Tauri v2 | Rust 原生，不嵌入浏览器内核 |
| 配置 | YAML | 人类可读，比 JSON 友好 |
| 存储 | 文件（YAML）→ 后期可选 SQLite | 第一阶段简单优先，不需要数据库 |
| 认证 | Bearer Token | 简单，与 OpenAI/Anthropic 一致 |
