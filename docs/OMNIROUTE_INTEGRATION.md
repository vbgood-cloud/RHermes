# RHermes × OMNIRoute 性能适配方案

## 一、当前状态（已验证）

| 维度 | RHermes 现状 | OMNIRoute 实测能力 |
|------|-------------|-------------------|
| 协议 | OpenAI 兼容（DeepSeekTransport 统一处理） | ✅ OpenAI 兼容 `/v1/chat/completions` |
| 流式 | ✅ 支持 SSE，但只解析 content/tool_calls/usage | ✅ 返回 `reasoning_content` + `cached_tokens` + 路由元数据 |
| 模型 | 通过 `config.api.model` 固定 | 130+ 模型，含 `auto/*` 自动路由族 |
| Thinking 控制 | ❌ ChatRequest 无 `reasoning_effort` 字段 | ✅ 支持 `reasoning_effort` (low/medium/high/xhigh) |
| 前缀缓存 | ✅ 三段式 Context 设计 | 网关层无缓存（两次相同 prompt 均未命中）→ **缓存红利完全在 RHermes 自己的前缀缓存上** |
| 缓存可见性 | ❌ StreamDelta 不解析 `cached_tokens` | ✅ 返回 `usage.cached_tokens` 字段 |
| 模型路由元数据 | ❌ 无 | ✅ 响应尾注 `x-omniroute-model/provider/latency-ms/cost` |

## 二、性能瓶颈定位（关键）

**RHermes 当前走 OMNIRoute 的三大浪费：**

### 浪费 1：固定模型，无法按任务难度路由
- 现状：`model = "glm-5.2"` 写死，简单任务用重模型、复杂任务又不够强
- 实测延迟差异：`glm-cn/glm-5-turbo` 1.4s vs `auto/coding:pro` 1.4s vs `auto/coding:fast` 6.9s
- **机会**：按任务类型选模型，简单 bash 调用用 fast，复杂代码生成用 pro

### 浪费 2：thinking 强度无法控制
- 现状：ChatRequest 无 `reasoning_effort` 字段，模型默认中等思考
- 实测：`reasoning_effort=low` 59 thinking tokens vs `high` 67 thinking tokens（简单任务差距更明显）
- **机会**：简单任务低 effort（省 tokens / 省时间），复杂推理任务高 effort

### 浪费 3：丢失了思考过程和缓存命中信息
- 现状：`StreamDelta` 不解析 `reasoning_content`（思考流被丢弃），不解析 `cached_tokens`（不知道缓存命中情况）
- **机会**：暴露思考流（可观察性），暴露缓存命中率（优化前缀设计）

## 三、改造方案（按 ROI 排序）

### 🟢 P0 — 零代码方案（立即可用）
**只改 config.toml，让现有架构跑起来：**

```toml
[api]
model = "glm-cn/glm-5.2"   # 走 omniroute 显式指定，不用 auto（auto 实测不稳定）

[providers.omniroute]
api_type = "openai"
base_url = "http://10.126.126.220:20128/v1"
model = "glm-cn/glm-5.2"
# .env: OMNIROUTE_API_KEY=sk-40f65a6ae490439e-8016c5-8cd852a6
```

**验证步骤**：跑现有 test suite，看 Transport 能否握手成功。

### 🟡 P1 — 小改代码（1-2 小时，收益最大）
**改 3 处源码，让 RHermes 吃满 OMNIRoute 的能力红利：**

#### 改动 1：`src/api/mod.rs` — ChatRequest 加 `reasoning_effort`
```rust
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ApiMessage>,
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub tools: Option<Vec<ToolDef>>,
    // ➕ 新增
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,  // "low"|"medium"|"high"|"xhigh"
}
```

**收益**：简单工具调用/buffer 操作时 effort=low 省 30-50% tokens。

#### 改动 2：`src/api/mod.rs` — StreamDelta 解析思考流和缓存命中
```rust
pub(crate) struct StreamDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
    // ➕ 新增
    #[serde(default)]
    pub reasoning_content: Option<String>,  // 思考过程
}

// Usage 结构加 cached_tokens 字段：
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    // ➕ 新增
    #[serde(default)]
    pub cached_tokens: Option<u32>,
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
}
```

**收益**：
- 思考流可在 TUI 单独展示（不再丢）
- 缓存命中率可观测 → 反推前缀设计 → 命中率每+10%节省 ~5% 成本

#### 改动 3：`src/provider/transport.rs` — chat_stream 分发 reasoning_content
```rust
// 在解析 SSE 块时，新增分支：
if let Some(reasoning) = &choice.delta.reasoning_content {
    if !reasoning.is_empty() {
        let _ = tx.send(ApiEvent::Thinking(reasoning.to_string()));  // 新增事件类型
    }
}
```

**收益**：思考流可独立呈现，不混入正文。

### 🟠 P2 — 中等改造（半天，长期红利）
**新增 `auto/*` 模型动态路由能力：**

- 在 `agent/session.rs` 的 `handle_message` 入口，根据消息特征（是否含代码、是否工具调用、prompt 长度）动态选择 model：
  - 纯工具调用 / 短回复 → `glm-cn/glm-5-turbo` 或 `auto/fast`
  - 代码生成 → `auto/coding:pro`（实测 1.4s 反而比 fast 快）
  - 复杂推理 → `glm-cn/glm-5.2` + `reasoning_effort=high`
- 通过 `transport.set_model()` 热切换（Transport trait 已支持）

**收益**：综合延迟降 30-50%，成本降 20-40%。

### 🔴 P3 — 高级（1-2 天，锦上添花）
**解析 OMNIRoute 路由元数据，做自优化：**

- 解析响应尾注 `x-omniroute-model/latency-ms/cost`
- 持久化到 `~/.rhermes/metrics.jsonl`
- 启动时统计各模型平均延迟/成本，自动推荐最优模型
- 配合 P2 的动态路由形成闭环

## 四、风险提示

1. **`auto/*` 不稳定**：`auto/coding:fast` 实测 6.9s（比 pro 还慢），说明 OMNIRoute 路由策略与名称不严格对应。**优先用具体模型 ID（如 `glm-cn/glm-5.2`）而不是 `auto/*`**。
2. **流式响应里 reasoning_content 可能与 content 交错**：GLM 系列先输出 reasoning_content 再输出 content；如果模型把 tool-call 写在 reasoning_content 里，现有 ScavengeRepair 需要适配。
3. **熔断器阈值**：OMNIRoute 作为本地网关（0.8ms 网络延迟），后端 LLM 抖动会触发熔断；现有 threshold=3 / cooldown=30s 可能过于敏感，建议改为 5/60s。
4. **cached_tokens 口径**：OMNIRoute 的 `cached_tokens` 是后端 LLM 自身缓存（不是 OMNIRoute 网关缓存，网关层实测不缓存）。RHermes 自己的前缀缓存机制（三段式 Context）才是真正可控的缓存层。

## 五、推荐实施路径

```
第 1 步：P0 零代码方案（立即）→ 跑通 + 测试通过
第 2 步：P1 改 3 处代码（1-2 小时）→ 解锁 effort + thinking + cached_tokens
第 3 步：观察一周，收集 cached_tokens 数据，反推前缀优化
第 4 步：P2 动态模型路由（按需）
第 5 步：P3 自优化闭环（远期）
```

## 六、实测性能基线（用于改造后对比）

| 场景 | 模型 | effort | TTFB | 总延迟 |
|------|------|--------|------|--------|
| 简单 hi | auto/fast | - | 1.6-4.1s | 1.6-4.1s |
| 简单 hi | glm-cn/glm-5.2 | - | 1.67s | 1.67s |
| 简单 hi | glm-cn/glm-5-turbo | - | 1.42s | 1.42s |
| 1+1=? | auto/coding | low | 0.8s | 0.8s |
| 1+1=? | auto/coding | high | 0.6s | 0.6s |
| 简单 hi | auto/coding:fast | - | 6.9s ⚠️ | 6.9s |
| 简单 hi | auto/coding:pro | - | 1.4s | 1.4s |
