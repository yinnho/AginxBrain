# brain v0.4.2 发版计划 + musl 版征询

> 写于 2026-09-26，brain 侧（aginxbrain 仓）出。给 aginxos-next 侧过目。
> **文末有问题清单**——musl 版按你们的答案打包。

## 一、v0.4.2 内容（服务器侧已上线，Release 待打包）

brain.aginx.net 服务端已在跑新代码（提交 242b084）。v0.4.2 打包内容：

1. **三面 thinking 开关**（对应 BRAIN-CODEX.md 行动项 1-3 的回执）：
   - chat 面：`reasoning_effort: "none"|"minimal"` 关；`"low"|"medium"|"high"`
     开（budget 4000/10000/24000）；另认 `thinking:{"type":...}` 和 `enable_thinking`
   - Anthropic 面：`thinking: {"type":"disabled"}` / `{"type":"enabled","budget_tokens":N}`
   - Responses 面：`reasoning.effort` 同 chat 面映射
   - 显式意图压过路由 reasoning-tag 注入；不传参数行为不变
   - 实测：deepseek-flash reasoning 107→0 token；caizhipu glm-5.3
     142→0、1.68s→0.64s；母体辅助调用加 `"reasoning_effort":"none"` 即可
2. **max_tokens 语义**（写入文档，行动项 2）：Anthropic 格式 provider 上
   只约束答案段，思考走独立 budget_tokens；tag 注入曾把客户端较小
   max_tokens 抬到 budget+6000（母体 150→16000 的原因），显式关思考后不再抬。
3. 文档修正：Anthropic 端点实为 `/anthropic/v1/messages`（同 `/v1/messages`）。

## 二、musl 版（新增交付物）

**动机**：设备侧（ORIN-NX 等）是 Linux，glibc 版本/交叉环境是坑；musl
静态二进制零依赖，任意 Linux 直接跑，适合进包体系。

**技术可行性（已核）**：
- 全链 rustls（reqwest/tungstenite 都 rustls-tls），无 OpenSSL 依赖
- 构建方式：macOS 上 `cargo zigbuild --release --no-default-features
  --features server --target <musl目标>`，产物单文件静态二进制
- 候选目标：`aarch64-unknown-linux-musl`（ARM 设备）、
  `x86_64-unknown-linux-musl`（x86 服务器/容器）
- 交付位置：GitHub Release 附加压缩包（与 dmg 并列）

## 三、问 aginxos-next 侧（请回复）

1. **架构**：aarch64-musl / x86_64-musl / 都要？
2. **形态**：裸二进制 tar.gz 够吗？还是按你们 pkgs/ 树包格式出？
3. **命名**：`aginxbrain-v0.4.2-aarch64-unknown-linux-musl.tar.gz` 可以吗？
4. **配置路径**：服务器版固定 `~/.aginxbrain/`（config.yaml + db）——设备上
   沿用还是需要可指定（环境变量/参数）？
5. **要不要 per-API-key 默认思考档**：母体若希望某把 key 永远关思考
   （不用每个请求带参数），brain 可以加——需要吗？

——以上 1-4 决定打包动作，5 决定是否排期下一版。回复可直接写在
BRAIN-CODEX.md 或口头传达，brain 侧照办。
