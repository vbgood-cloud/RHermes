# Provider 层（`src/provider/`）

> 4 个文件 · 938 行。把「调 AI」抽象为 `Transport` trait，再叠加**熔断 + 降级**的 `ProviderPool`。
> 代码：`provider/mod.rs` · `provider/transport.rs`（`Transport` + `DeepSeekTransport`）·
> `provider/pool.rs`（`ProviderPool` + `CircuitBreaker`）· `provider/factory.rs`（`create_transport` / `create_main_transport`）
> 相关：`api/mod.rs`（请求/响应/事件类型）· 代理见 `docs/proxy.md`

## `Transport` trait

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError>;
    async fn chat_stream(&self, request: ChatRequest, tx: UnboundedSender<ApiEvent>) -> Result<(), ApiError>;
    async fn get_balance(&self) -> Result<f64, ApiError>;
    fn model_name(&self) -> String;
    fn set_model(&self, _model: &str) {}   // 默认空实现（向后兼容）
}
```

**唯一实现是 `DeepSeekTransport`**——名字带 DeepSeek，实际是**通用 OpenAI 兼容 HTTP 客户端**（`[provider].api_type = "openai"`），
所以 OpenAI / GLM / SiliconFlow / Moonshot / Ollama / OMNIRoute 全走它，靠 `base_url` 区分。

`DeepSeekTransport` 内含 `model_override: RwLock<Option<String>>` —— `set_model()` 热切换，`None` 时回落到 `config.api.model`。
HTTP 客户端由 `create_proxied_client(&config.proxy, "llm", timeout)` 创建（即 `[proxy].rules.llm` 控制）。

## `ProviderPool`（`Transport` 的另一个实现）

持有 `Vec<ProviderEntry>`，每个条目 = `transport + CircuitBreaker + weight`。

### 选择策略：**主 provider 优先 + 熔断降级轮询**

```
next_healthy():
  ① providers[0].is_healthy()  → 用它        ← 主 provider 优先
  ② 否则 index.fetch_add(1) 起点，环形扫描 idx != 0 的健康者   ← 降级轮询
  ③ 全不可用 → None
```

> 🔴 **不是加权轮询**。`ProviderEntry.weight` 字段**只被写入、从不被读取**（死字段）。
> `provider/mod.rs` 与 `pool.rs` 顶部的文档注释仍写「加权轮询 / weight」，**与实际实现不符**——见审计报告缺陷 A2。
> 实际 `factory.rs` 给主 provider 传 `weight=10`、fallback 传 `weight=1`，但该值未被使用。

### 熔断器（`CircuitBreaker`）

| 状态 | 语义 |
|------|------|
| `Closed` | 正常，请求直达 |
| `Open` | 快速失败；`now - last_failure ≥ cooldown_secs` 时**原子转** `HalfOpen` |
| `HalfOpen` | 允许试探（`allow_request()` 返回 true） |

- `record_failure()`：`failure_count += 1`；`≥ threshold` → `Open` 并返回 `true`
- `record_success()`：立刻回 `Closed` 且 `failure_count = 0`
- 阈值/冷却来自 `[provider_pool]`：`circuit_breaker_threshold`（默认 3）/ `circuit_breaker_cooldown_secs`（默认 30）

### 重试与退避（仅 `chat()` 非流式路径）

- `max_retries()` 当前**硬编码返回 3**（注释标注「可配置化」）
- 退避：第 `attempt` 次前 sleep `500ms × 2^(attempt-1)`（即 500ms / 1s / 2s）
- **非可重试错误（`!e.is_retryable()`，如 400/401/403/429）立即返回，不消耗重试**
- 每次失败都会 `entry.breaker.record_failure()` —— 重试会**换 provider**（因为 `next_healthy()` 每轮重算）

> ⚠️ `chat_stream()` **没有重试循环**：选一次 provider，成功记 success、失败记 failure 后直接返回 `Err`。
> 且流式路径会用选中 provider 的 `model_name()` **覆盖 `request.model`**（跨 provider 模型名不同）。

## 工厂（`create_transport`）

Provider 名解析优先级：

1. **显式 `explicit_provider`**（来自 `agent.default_provider`）
2. **model 前缀推断**（`infer_provider_from_model`：`gpt-*`→openai、`deepseek*`→deepseek、`glm*`→zhipu、`qwen*`→qwen、`kimi/moonshot`→kimi、`gemini-*`→gemini、`claude-*`→anthropic、`llama/mistral/qwen2/qwen3`→ollama）
3. **第一个有 API Key 的 provider**（`fallback_provider_name`，优先级 deepseek → openai → siliconflow → zhipu → moonshot → kimi → newapi → ollama → lmstudio；本地服务免 key）
4. 兜底 `"deepseek"`

**多 Provider 组装（P2a）**：主 provider + 遍历 `config.providers` 中**有显式 `base_url`** 的其他 provider
（跳过没写 `base_url` 的，视为 Default 占位）；只有 1 个 → `ProviderPool::single`，否则 `with_providers`。

`create_main_transport(config, ..)` = 用 `config.api.model` + `agent.default_provider` 调 `create_transport`。

### 默认 base_url 映射

| provider | base_url |
|----------|----------|
| omniroute | `http://10.126.126.220:20128/v1` |
| openai / openrouter | `https://api.openai.com/v1` |
| deepseek | `https://api.deepseek.com` |
| siliconflow | `https://api.siliconflow.cn/v1` |
| zhipu | `https://open.bigmodel.cn/api/paas/v4` |
| moonshot / kimi | `https://api.moonshot.cn/v1` |
| ollama | `http://localhost:11434/v1` |
| groq | `https://api.groq.com/openai/v1` |
| newapi | `http://localhost:3000/v1` |
| lmstudio | `http://localhost:1234/v1` |
| 其他 | `https://api.openai.com/v1` |

## 事件模型（与 `api/mod.rs` 的契约）

`chat_stream` 通过 `UnboundedSender<ApiEvent>` 发：`Usage` / `StreamChunk` / `ToolCalls`；
DSML 检测：正文里一旦出现 `DSML` 标记，**停止发文本**并累积到 `dsml_buffer`，流末用 `parse_dsml_tool_calls` 解析成 `tool_calls`。

**已知死链路**（详见审计报告 A1）：`ApiEvent::Thinking` 与 `ApiEvent::ProviderMeta` **有消费者、无生产者**——
`transport.rs` 里 `choice.delta.reasoning_content` 的分发分支是**空块**，且**没有**解析 `x-omniroute-*` 响应尾注的代码。
`Usage.cached_tokens` / `reasoning_tokens` 字段已解析但无消费方。

## 已知边界

- **`max_retries()` 硬编码 3**，不受配置影响。
- **流式无重试**（见上）。
- **`weight` 死字段**（见上）。
- **`ProviderMeta` 无生产者**：`ApiEvent::ProviderMeta` 结构已定义、TUI 有渲染分支，但 transport 从不发送。
- **`api/mod.rs::DeepSeekClient` 是另一套旧客户端**（未被 `provider/` 使用），属历史遗留。
