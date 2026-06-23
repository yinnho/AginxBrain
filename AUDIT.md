# AginxBrain 代码审计报告

**日期**: 2026-06-23
**范围**: `proxy.rs`、`api.rs`、`axum_server.rs`、`config.rs`
**版本**: v0.2.6
**修复状态**: C-1~C-6、H-1~H-7 已修复，详见各条目

---

## 审计总结

| 严重度 | 数量 | 关键问题 |
|--------|------|----------|
| 🔴 关键 | 5 | 日志泄密、TOCTOU、Panic、数据完整性 |
| 🟠 高 | 8 | 输入验证缺失、内存泄漏、API Key 泄露 |
| 🟡 中 | 9 | 信息泄露、CORS、Cookie 安全、统计失真 |
| 🔵 低 | 7 | 代码质量、默认值、命名 |

---

## 🔴 关键问题

### ~~C-1: Caller Key 可执行 Takeover~~ — 预期行为，非漏洞

**文件**: `axum_server.rs` 路由注册（约 148-156 行）
**结论**: **设计如此，无需修复。** Caller key 是桌面客户端的认证方式，takeover（修改本机 Codex/VS Code 配置）是客户端的核心功能。用户在自己的机器上通过客户端接管本地 Agent 配置，这是正常使用流程。只有管理员才能创建 caller key，且 caller key 只能操作自己机器上的文件，不存在权限提升风险。

---

### C-2: x-api-key 全文写入日志（密钥泄露） ✅ 已修复

**文件**: `axum_server.rs:271-273`
**影响**: `request_log_middleware` 将 `x-api-key` 请求头的完整值记入日志。如果客户端转发 Anthropic API Key，该 Key 会明文出现在日志文件中。`Authorization` 头只截取前 20 字符，但 `x-api-key` 无任何截断。

**修复方案**: 对两个头统一掩码处理，只保留末 4 位：

```rust
fn mask_secret(s: &str) -> String {
    if s.len() <= 4 { return "***".to_string(); }
    format!("***{}" , &s[s.len()-4..])
}
```

注意：`&s[s.len()-4..]` 对多字节 UTF-8 也可能 panic，应使用 `chars()` 方法：

```rust
fn mask_secret(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 4 { return "***".to_string(); }
    format!("***{}", chars[chars.len()-4..].iter().collect::<String>())
}
```

---

### C-3: import_config TOCTOU 竞态条件 ✅ 已修复

**文件**: `api.rs:879-893`
**影响**: `import_config` 先用读锁获取当前 `management_key`，释放读锁，然后用写锁赋值。在两锁之间的窗口期，另一个请求可能修改了配置，导致导入覆盖了更新的数据。更严重的是，如果 `save_config` 成功但写锁获取失败，磁盘和内存状态不一致。

**当前代码**:
```rust
let current = state.config.read().await;       // 读锁
import_config.management_key = current.management_key.clone();
}                                               // 读锁释放
save_config(&import_config).map_err(ApiError::from)?;  // 存盘
let mut config = state.config.write().await;   // 写锁
*config = import_config;
```

**修复方案**: 先获取写锁，在写锁内完成所有操作：

```rust
let mut config = state.config.write().await;
if import_config.management_key == "YOUR_MANAGEMENT_KEY" {
    import_config.management_key = config.management_key.clone();
}
save_config(&import_config).map_err(ApiError::from)?;
*config = import_config;
```

同样的问题存在于 `update_config`（api.rs:369-378），应一并修复：先写锁 → 再存盘 → 再赋值。

---

### C-4: UTF-8 字节截断可导致 Panic ✅ 已修复

**文件**: `proxy.rs:623, 628, 652, 2405`
**影响**: 使用 `&err_body[..N]` 或 `&s[..500]` 截断字符串，如果截断位置落在多字节 UTF-8 字符中间，程序 panic（`byte index N is not a char boundary`）。

**涉及位置**:
- `proxy.rs:623` — `&err_body[..err_body.len().min(300)]`
- `proxy.rs:628` — `&err_body[..err_body.len().min(200)]`
- `proxy.rs:652` — `&err_body[..err_body.len().min(200)]`
- `proxy.rs:2405` — `&s[..500]`

**修复方案**: 使用字符级截断：

```rust
fn truncate_str(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
```

---

### C-5: 成功日志在响应体读取前写入（数据失真）

**文件**: `proxy.rs:667`
**影响**: 非流式路径下，成功日志在 `resp.bytes().await` 之前插入数据库。如果读取响应体失败（网络中断、超时），日志已记录"成功"，然后 `continue` 触发 failover 重试，重试成功后又插入一条成功日志，导致请求计数虚高。

**修复方案**: 将成功日志的插入移到确认响应体完整读取之后。或在 body 读取失败时，先删除/标记已有的成功日志为失败，再 continue。

---

### C-6: list_keys 返回明文 Token ✅ 已修复

**文件**: `api.rs:176`，`db.rs:111`
**影响**: `GET /api/keys` 返回所有 caller key 的明文 `token` 字段。违反了"创建时只显示一次"的设计意图。管理员通过浏览器 devtools、日志、XSS 等渠道可以获取所有 token。

**修复方案**: `list_caller_keys` 查询时不 SELECT `token` 列，或返回时将 `token` 设为 `None`：

```rust
// db.rs 中 list_caller_keys
fn list_caller_keys(pool: &SqlitePool) -> Result<Vec<CallerKey>> {
    let rows = sqlx::query_as::<_, CallerKey>(
        "SELECT id, name, note, enabled, created_at, NULL as token FROM caller_keys ORDER BY id"
    ).fetch_all(pool).await?;
    Ok(rows)
}
```

---

## 🟠 高优先级问题

### H-1: 删除 Provider 不检查关联路由 ✅ 已修复

**文件**: `api.rs:557-569`
**影响**: 删除 provider 后，引用它的路由在运行时找不到 provider，所有请求失败。

**修复方案**: 在 `delete_provider` 中检查是否有路由引用此 provider：

```rust
if config.routes.iter().any(|r| r.provider == id) {
    return Err(ApiError::Validation(
        format!("Cannot delete provider '{}': still referenced by routes", id)
    ));
}
```

---

### H-2: 删除 Tag 不清理路由中的引用 ✅ 已修复

**文件**: `api.rs:625-639`
**影响**: 删除 tag 后，路由的 `tags` 数组中仍保留已删 tag 名，导致路由匹配混乱。

**修复方案**: 删除 tag 时，同时从所有路由的 `tags` 中移除该 tag：

```rust
for route in &mut config.routes {
    route.tags.retain(|t| t != &name);
}
```

---

### H-3: move_route direction 不校验 ✅ 已修复

**文件**: `api.rs:490-514`
**影响**: `direction` 字段为 `i32`，传入 `5` 或 `-3` 会导致跨多位置交换，而非只移动一位。

**修复方案**:
```rust
if req.direction != -1 && req.direction != 1 {
    return Err(ApiError::Validation("direction must be -1 or 1".into()));
}
```

---

### H-4: set_current_tag 不验证 Tag 是否存在 ✅ 已修复

**文件**: `api.rs:386-394`
**影响**: 设置不存在的 tag 后，所有使用默认 tag 的代理请求无法路由。

**修复方案**:
```rust
if !config.tags.iter().any(|t| t.name == req.tag) {
    return Err(ApiError::Validation(
        format!("Tag '{}' does not exist", req.tag)
    ));
}
```

---

### H-5: 流式使用量提取无界缓冲（内存泄漏） ✅ 已修复

**文件**: `proxy.rs:697`
**影响**: 流式响应的所有 SSE 字节追加到 `Vec<u8>` 缓冲区。长对话（大段代码生成）可消耗数百 MB 内存，仅在 task 完成后释放。

**修复方案**: 增量解析 SSE 事件，提取 `message_delta` / 最终 chunk 中的 usage 数据后丢弃早期数据。或设置缓冲区大小上限，超过后停止收集（仅损失使用量统计，不影响代理功能）。

---

### H-6: 测试端点泄露 API Key 前缀 ✅ 已修复

**文件**: `proxy.rs:2342-2344`
**影响**: 测试失败时错误信息返回 API key 前 8 个字符（如 `sk-ant-a`），暴露 key 类型。

**修复方案**: 移除 key 预览，改为固定掩码如 `"sk-***"` 或完全省略。

---

### H-7: update_config 先存磁盘再更新内存 ✅ 已修复

**文件**: `api.rs:369-378`
**影响**: `save_config` 成功后如果 `write().await` 或 `*config = new_config` 失败，磁盘是新配置但内存是旧配置。重启后以磁盘为准，但运行中的请求使用旧配置。

**修复方案**: 与 C-3 相同，先获取写锁，再存盘，再赋值。

---

### H-8: DashScope 轮询 URL 硬编码 ✅ 已修复

**文件**: `proxy.rs:2101`
**影响**: `poll_dashscope_image_task` 硬编码 `https://dashscope.aliyuncs.com/api/v1/tasks/`，忽略了 provider 的 `base_url`。使用兼容 DashScope 的其他服务时，轮询会打错地址。

**修复方案**: 从 provider 的 `base_url` 构造轮询 URL：

```rust
let poll_url = format!("{}/api/v1/tasks/{}", provider.base_url.trim_end_matches('/'), task_id);
```

---

## 🟡 中优先级问题

### M-1: ApiError::Internal 泄露内部实现细节 ✅ 已修复

**文件**: `api.rs:897-922`
**影响**: 内部错误（SQL 错误、文件路径等）直接返回给客户端。

**修复方案**: 返回通用错误信息，详细错误只记日志。

---

### M-2: GET /api/config 返回所有 Provider 明文 API Key ✅ 已修复

**文件**: `api.rs:363-366`
**影响**: 管理员浏览器 devtools 可见所有上游 API Key。

**修复方案**: GET 响应中掩码 API Key（如 `sk-***abc`），仅在 PUT/POST 时接受完整 Key。

---

### M-3: CorsLayer::permissive() 在 SaaS 部署不安全

**文件**: `axum_server.rs:185`
**影响**: 允许任何源跨域请求。本地开发可接受，但 brain.aginx.net 上可被恶意网站利用。

**修复方案**: 根据部署环境配置 CORS，生产环境限制为已知域名。

---

### M-4: Session Cookie Secure 标记硬编码为 false

**文件**: `axum_server.rs:57`
**影响**: HTTPS 环境下 cookie 缺少 `Secure` 标记，可被 HTTP 拦截。

**修复方案**: 根据协议或配置动态设置 `with_secure()`。

---

### M-5: Token 统计 None 与 0 混淆

**文件**: `proxy.rs:852-864`
**影响**: `input.unwrap_or(0)` 将"未知"记为"零"，污染使用量统计。

**修复方案**: 仅在 `Some` 时更新统计，`None` 保持数据库原值。

---

### M-6: 所有候选失败时 modality 硬编码为 "chat"

**文件**: `proxy.rs:966`
**影响**: TTS/图片请求失败时日志记录错误的 modality。

**修复方案**: 使用最后一个候选路由的实际 modality。

---

### M-7: count_tokens URL 对非 Anthropic 格式拼接错误

**文件**: `proxy.rs:1104-1108, 1152`
**影响**: OpenAI 格式路由的 count_tokens 请求会发到 `/v1/chat/completions/count_tokens`，这不是有效端点。

**修复方案**: 仅对 Anthropic 格式路由支持 count_tokens，其他格式返回明确错误。

---

### M-8: admin_setup 并发创建多个 Admin

**文件**: `api.rs:81-107`
**影响**: 并发 setup 请求可能同时通过 `admin_count == 0` 检查，创建多个 admin。

**修复方案**: 使用 `INSERT ... WHERE NOT EXISTS` 或 `BEGIN IMMEDIATE` 事务。

---

### M-9: route_priority 用路由索引作 Key

**文件**: `config.rs:143`
**影响**: 路由增删改后索引偏移，所有优先级映射失效。

**修复方案**: 为路由添加稳定 ID（UUID 或自增），用 ID 作为 priority key。这是一个较大的重构，可延后处理。

---

## 🔵 低优先级问题

### L-1: SSE 序列化失败时发送空 data 行
**文件**: `proxy.rs:1354`
**修复**: 序列化失败时传回原始 data 行，而非发送空的 `data: \n\n`。

### L-2: 不支持的协议组合静默转发
**文件**: `proxy.rs:528-534`
**修复**: 对不支持的 (client_protocol, provider_format) 组合返回明确错误。

### L-3: management_key 存储但从未校验
**文件**: `config.rs:165-167`
**说明**: 死代码，当前所有管理端点通过 session auth，management_key 未被任何中间件检查。

### L-4: Route.endpoint 字段不控制实际路由
**文件**: `config.rs:123`
**说明**: 该字段仅作文档用途，容易误导。建议加注释或重命名。

### L-5: 默认 api_key "your-key-here" 通过验证
**文件**: `config.rs:175`
**修复**: 在 `validate_config` 中增加占位符检测。

### L-6: save_config 临时文件名不唯一
**文件**: `config.rs:301`
**说明**: 并发调用可能互相覆盖 `.yaml.tmp`，单进程场景无影响。

### L-7: RequestLog 有 #[serde(default)] 但未 derive Deserialize
**文件**: `config.rs:15-29`
**说明**: `#[serde(default)]` 仅在 `Deserialize` 时有效，当前无害但误导。

---

## 修复优先级建议

**立即修复**（影响安全/稳定性）:
1. C-2: 日志泄密 — 掩码函数 + 替换 3 处调用
2. C-4: UTF-8 panic — 截断工具函数 + 替换 4 处调用
3. C-6: list_keys 明文 token — SQL 改一行

**短期修复**（影响数据完整性）:
5. C-3 + H-7: TOCTOU — 统一为"先写锁再操作"
6. H-1 + H-2: 删除时检查依赖 + 清理引用
7. H-3 + H-4: 输入验证

**中期优化**:
8. C-5: 成功日志时序
9. H-5: 流式缓冲上限
10. M-2: API Key 掩码
11. M-1: 内部错误脱敏
