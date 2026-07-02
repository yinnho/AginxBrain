# LLM 网关横评：AginxBrain vs LiteLLM / One API / New API / Portkey / OpenRouter

> 2026 年 7 月。当你想让 Claude Code 跑国产模型、或让 Codex CLI 对接 Anthropic provider 时，到底该选哪个网关？本文基于各项目当前实际能力（已核对文档与源码）做横向对比，并诚实地说明 AginxBrain 的真实定位——它不是「唯一」的双向网关，但在一个被忽视的维度上做得最好。

---

## 一句话结论

如果你要的是「**本地运行 + 让 Claude Code / Codex CLI 原生接入任意模型 + 按质量分级路由**」，选 **AginxBrain**。
如果你要「企业级、可观测、100+ 模型、SaaS 托管」，选 **Portkey** 或 **LiteLLM**。
如果你只要「OpenAI 格式的多 key 聚合 + 计费」，选 **One API / New API**。

---

## 关键能力对比表

先回答最关键的一个问题：**你的客户端协议，网关能不能原样吃下？**

| 网关 | 接收 Anthropic Messages (Claude Code) | 接收 OpenAI Responses (Codex CLI) | 本地/桌面优先 | 一键接管 Claude Code/Codex | 标签路由 | 国产模型一等公民 | 密钥留在本机 |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| **AginxBrain** | ✅ | ✅ | ✅ Tauri | ✅ | ✅ | ✅ | ✅ |
| Portkey | ✅ | ✅ | ❌ 服务端 | ❌ | ❌ | 部分 | 取决于部署 |
| New API | ✅ | ✅ | ❌ 服务端 | ❌ | ❌ | ✅ | ❌ |
| LiteLLM | ✅ (beta) | 部分 | ❌ 服务端 | ❌ | ❌ | 部分 | 取决于部署 |
| OpenRouter | ✅ | ❌ | ❌ SaaS | ❌ | ❌ | ✅ | ❌ |
| One API | ❌ | ❌ | ❌ 服务端 | ❌ | ❌ | ✅ | ❌ |

> 表格里「接收 Anthropic Messages」这一列是分水岭：**One API 至今只接受 OpenAI 格式输入**，Claude Code 无法不改代码直接接入（社区 issue #2028 / #2322 悬而未决）。「接收 Responses」更稀缺——只有 AginxBrain / Portkey / New API 三个真正支持。

---

## 逐个点评

### AginxBrain —— 本地优先的协议原生网关

**它真正不同在哪**（诚实版，不吹「唯一」）：

1. **双向协议转换并不是独家**。Portkey 的 Universal API 也能做 Anthropic↔OpenAI↔Responses 三向互转。但 AginxBrain 是**唯一桌面原生**的：一个 Tauri 应用常驻托盘，所有 provider 密钥只存在你本机的 `~/.aginxbrain/config.yaml`，不出设备。
2. **一键接管自动化**是别家没有的。点一下 Takeover，它直接改写 Claude Code 的 `settings.json` 和 Codex 的 `config.toml`；别家要么靠你手设环境变量，要么压根不碰客户端配置。
3. **标签路由（opus/sonnet/haiku/auto）**模仿了 Claude Code 自己的模型选择逻辑——你按「这次要最强的」「这次要快的」来路由，而不是绑死某个模型名。这是 AginxBrain 的核心心智模型，别家没有对等概念。
4. **国产模型一等公民**：DeepSeek、智谱 GLM、百度文心、Kimi、通义 DashScope 默认配好，开箱即用。

**短板**：生态年轻、star 少、企业级可观测/计费不如 LiteLLM/Portkey；以桌面/单机为主，大规模多租户场景不是它的目标。

---

### Portkey —— 最强开源同类，企业级 SaaS

- TypeScript，MIT 协议，~12k+ star。
- **Universal API 是它和 AginxBrain 最像的地方**：OpenAI Chat / Responses / Anthropic Messages 任选输入，任选 provider 输出。Responses 跨 provider 支持在 2026 年 2 月上线。
- 强路由、failover、retry、guardrails、1600+ 模型，有托管 SaaS。

**和 AginxBrain 的差异**：Portkey 是**服务端优先 + SaaS**，没有桌面应用、没有「接管 Claude Code」的自动化、没有标签路由。它面向「团队/企业部署网关」，AginxBrain 面向「个人开发者本机跑」。**如果你要的是 Portkey 的能力但想本地跑、想接 Claude Code，AginxBrain 是更轻的选择。**

---

### LiteLLM —— 体量最大的统一代理

- Python，~52k+ star，但**注意协议不是 MIT**——是带企业限制的自定义协议（SSO>5 用户、部分功能需付费 license），别再写文章说它是 MIT 了。
- 100+ provider，路由/fallback/retry 成熟，可观测强。
- Anthropic `/v1/messages` 输入支持（beta），Responses 输入侧较弱。

**和 AginxBrain 的差异**：LiteLLM 是 **Python 服务、重、面向统一 SDK + 代理**；AginxBrain 是 **Rust 单二进制、轻、面向桌面客户端原生接入**。LiteLLM 没有「接管 Claude Code」、没有标签路由、密钥默认要进它的服务进程。**LiteLLM 适合需要 100+ 模型统一接入的团队，AginxBrain 适合个人开发者本机用 Claude Code/Codex 省钱。**

---

### New API —— 最接近的开源同类，计费优先

- Go，AGPL-3.0，~40k+ star，One API 的 fork。
- 支持 Anthropic / OpenAI / Responses / Gemini / Realtime 多格式输入，多模态。
- 核心是**渠道聚合 + 计费 + 多租户**，是个「API 聚合站」而非透明代理。

**和 AginxBrain 的差异**：New API 是**服务端多租户计费站**，AginxBrain 是**本机透明代理**。New API 没有 Takeover 自动化、没有标签路由，部署形态完全不同（你要起一个 Go 服务 + 数据库）。**要搭对外卖 token 的聚合站选 New API，自己本机用选 AginxBrain。**

---

### One API —— 经典 OpenAI 聚合，但有硬伤

- Go + JS，MIT，~35k+ star。
- **只接受 OpenAI 格式输入**——这是最大的局限。Claude Code（Anthropic 协议）无法不改代码接入，社区多年诉求未合并进主线。

**一句话**：如果你只用 OpenAI 格式的客户端，One API 够用且成熟；**但凡你要接 Claude Code 或 Codex，它就不行**。这正是 AginxBrain 存在的理由之一。

---

### OpenRouter —— 托管聚合，省心但闭源

- 闭源 SaaS，300+ 模型，同时暴露 OpenAI 和 Anthropic 端点（Claude Code 可用），**但没有公开的 Responses 输入**。
- 最省心，但**密钥和流量都过它的服务器**，且不可自托管。

**和 AginxBrain 的差异**：OpenRouter 是**托管 SaaS**，AginxBrain 是**本机自托管**。不想把密钥/数据交给第三方、想要可控 failover，选 AginxBrain。

---

## 什么场景选什么

| 你的场景 | 推荐 |
|---|---|
| 个人开发，想让 Claude Code / Codex 跑国产模型省钱，密钥留本机 | **AginxBrain** |
| 团队/企业要统一网关 + 可观测 + 100+ 模型 | Portkey / LiteLLM |
| 要搭对外卖 token 的聚合计费站 | New API |
| 只用 OpenAI 格式客户端的多 key 聚合 | One API |
| 完全不想运维，按量付费 | OpenRouter |

---

## 为什么我又写了一个网关（AginxBrain 的来历）

把 Claude Code 接到国产模型，最痛的不是「能不能转格式」——Portkey 也能转。而是三件事凑在一起没人做：

1. **客户端协议原生接入**：Claude Code 说 Anthropic Messages，Codex 说 Responses，我不想改它们的任何代码。
2. **本地优先**：密钥不出我自己的机器，不想起一个永远在线的服务去喂 token。
3. **按质量而非模型名路由**：Claude Code 自己就是按 opus/sonnet/haiku 切模型的，我想要一个网关也这么想——`opus` 标签走最强国产模型，`haiku` 走最便宜的，`auto` 自动判断。

这三条加起来，市面上的网关要么不本地、要么不接管客户端、要么不按标签路由。所以有了 AginxBrain。

它不打算替代 LiteLLM 或 Portkey 的企业场景。它的目标是：**个人开发者，本机一个应用，Claude Code / Codex 原生接入，国产模型省钱，按质量路由，failover 自动兜底。**

---

## 附：技术栈与可复现性

- **语言**：Rust（axum HTTP + Tauri v2 桌面），单二进制，~无运行时依赖。
- **协议矩阵**：Anthropic ↔ OpenAI Chat ↔ OpenAI Responses，9 种组合，流式 SSE 全覆盖（含 thinking / tool_use 块）。
- **多模态**：图片生成（DashScope wan2.7 / OpenAI / MiniMax）、TTS、ASR。
- **可靠性**：429 / 上下文超限 / 超时 均可重试并 failover；45s 非流式超时 + 10s 连接超时。
- **协议**：MIT，开源可自审。

相关阅读：
- 省钱实操：[用国产模型跑 Claude Code，每年省下几千块](./ARTICLE.md)
- 项目主页：[github.com/yinnho/AginxBrain](https://github.com/yinnho/AginxBrain)

---

*本文能力数据核对于 2026 年 7 月，各项目能力会演进，使用前请以各项目最新文档为准。*
