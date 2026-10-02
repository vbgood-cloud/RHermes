# RHermes × OMNIRoute 性能适配方案

> **📌 实施进度复核（对照 v0.7.27 代码）**
>
> 本文档写于「改造前」。复核结果：**P1 的两项数据结构改动（改动 1、2）与 P2 的「思考强度推断」已落地**，
> 但**最关键的「思考流分发」（P1 改动 3）仍是空实现** —— `ApiEvent::Thinking` 至今**没有生产者**，思考流到不了界面。
> 下方各节已按代码现状标注：✅ 已实现 / 🟡 部分实现 / ❌ 未实现。

## 一、当前状态（已对照代码核实）

| 维度 | RHermes 现状（v0.7.27） | OMNIRoute 实测能力 |
|------|-------------|-------------------|
| 协议 | OpenAI 兼容（`DeepSeekTransport` 统一处理） | ✅ OpenAI 兼容 `/v1/chat/completions` |
| 流式 | ✅ SSE；`StreamDelta` **已含** `reasoning_content` 字段，但 transport **未分发**（空块，见 P1-3） | ✅ 返回 `reasoning_content` + `cached_tokens` + 路由元数据 |
| 模型 | 通过 `config.api.model` 固定；`transport.set_model()` 可热切换 | 130+ 模型，含 `auto/*` 自动路由族 |
| Thinking 控制 | 🟡 **`ChatRequest.reasoning_effort` 已有**；`session.rs` 按消息特征自动推断（非按任务类型选模型） | ✅ 支持 `reasoning_effort` (low/medium/high/xhigh) |
| 前缀缓存 | ✅ 三段式 Context 设计 | 网关层无缓存（两次相同 prompt 均未命中）→ **缓存红利完全在 RHermes 自己的前缀缓存上** |
| 缓存可见性 | 🟡 `Usage` **已含** `cached_tokens` / `reasoning_tokens` / `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`，但**无消费方展示** | ✅ 返回 `usage.cached_tokens` 字段 |
| 模型路由元数据 | 🟡 `ApiEvent::ProviderMeta` **枚举已定义、TUI 有渲染分支**，但 transport 未解析响应尾注 → **无生产者** | ✅ 响应尾注 `x-omniroute-model/provider/latency-ms/cost` |

## 二、性能瓶颈定位（原始分析，仍然成立）

**RHermes 当前走 OMNIRoute 的三大浪费：**

### 浪费 1：固定模型，无法按任务难度路由
- 现状：`model` 写死，简单任务用重模型、复杂任务又不够强
- 实测延迟差异：`glm-cn/glm-5-turbo` 1.4s vs `auto/coding:pro` 1.4s vs `auto/coding:fast` 6.9s
- **机会**：按任务类型选模型，简单 bash 调用用 fast，复杂代码生成用 pro
- 🟡 **现状**：`reasoning_effort` 已按消息推断，但**模型本身仍未动态选择**（`infer_reasoning_effort` 只调 effort，不换 model）

### 浪费 2：thinking 强度无法控制
- ~~现状：ChatRequest 无 `reasoning_effort` 字段~~ → ✅ **已修复**：`api/mod.rs` 已加字段，`session.rs:455` 用 `infer_reasoning_effort(user_msg)` 注入
- 实测：`reasoning_effort=low` 59 thinking tokens vs `high` 67 thinking tokens（简单任务差距更明显）
- 🟡 **剩余机会**：当前推断只按「关键词 + 消息长度」，未结合工具调用阶段/上下文预算

### 浪费 3：丢失了思考过程和缓存命中信息
- 🟡 **现状**：
  - `StreamDelta.reasoning_content` **已加字段**，但 transport 里的分发分支是**空块**（`if !reasoning.is_empty() { }`）→ 思考流被丢弃，`ApiEvent::Thinking` 无人生产
  - `Usage` 的 `cached_tokens` 等字段**已加**，但没有任何地方读取展示 → 缓存命中率仍不可观测
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

### 🟡 P1 — 小改代码（3 处中 2 处已落地）

#### ✅ 改动 1：`src/api/mod.rs` — ChatRequest 加 `reasoning_effort`（**已实现**）
```rust
// 现状（api/mod.rs:28-42）
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ApiMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    /// OMNIRoute / OpenAI 兼容的思考强度控制：low|medium|high|xhigh
    /// None 时不序列化，向后兼容现有 DeepSeek provider
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,   // ✅ 字段已存在
}
```

**注入点**（`agent/session.rs:455`）：
```rust
reasoning_effort: infer_reasoning_effort(user_msg).map(|s| s.to_string()),
```
`infer_reasoning_effort()`（`session.rs:873`）：按「复杂关键词/长度>500→high；简单工具词→low；≤2字符或寒暄→None；其余→medium」推断。
其余 4 处构造 `ChatRequest` 仍传 `None`（`session.rs:395` 摘要、`task.rs:61/132/252` 子任务、测试），即**只有主对话路径启用了 effort**。

**收益**：简单工具调用/buffer 操作时 effort=low 省 30-50% tokens。✅ 已生效。

#### ✅ 改动 2：`src/api/mod.rs` — StreamDelta / Usage 加字段（**已实现**）
```rust
// 现状（api/mod.rs:815-823）
pub(crate) struct StreamDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
    /// OMNIRoute / GLM / DeepSeek-R1 等模型的思考流（与 content 并行，单独字段）
    #[serde(default)]
    pub reasoning_content: Option<String>,   // ✅ 字段已存在
}

// 现状（api/mod.rs:385-399）——比原方案多两个 DeepSeek 专用字段
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)] pub prompt_cache_hit_tokens: u32,   // ➕ 超出原方案
    #[serde(default)] pub prompt_cache_miss_tokens: u32,  // ➕ 超出原方案
    #[serde(default)] pub cached_tokens: Option<u32>,     // ✅
    #[serde(default)] pub reasoning_tokens: Option<u32>,  // ✅
}
```

**收益**：
- 思考流可在 TUI 单独展示 —— ⚠️ **尚未兑现**（字段有了，但没人读，见改动 3）
- 缓存命中率可观测 —— ⚠️ **尚未兑现**（字段有了，但无任何消费方）

#### ❌ 改动 3：`src/provider/transport.rs` — 分发 reasoning_content（**未实现 → 当前最大缺口**）
```rust
// 现状（transport.rs:222-226）——空块，什么都不做！
if let Some(ref reasoning) = choice.delta.reasoning_content {
    if !reasoning.is_empty() {
        // ← 这里没有任何代码。从未 tx.send(ApiEvent::Thinking(..))
    }
}
```
**应改为**：
```rust
if let Some(ref reasoning) = choice.delta.reasoning_content {
    if !reasoning.is_empty() {
        let _ = tx.send(ApiEvent::Thinking(reasoning.to_string()));
    }
}
```

**为什么这是最大缺口**：下游**早已就绪**——
- 枚举已定义：`ApiEvent::Thinking(String)`（`api/mod.rs:473`）、`ApiEvent::ProviderMeta(ProviderMeta)`（`:475`）
- 消费端已就绪：`agent/session.rs:487` 会把 `Thinking` 累积进 `thinking_buf`；`tui/mod.rs:840` 有渲染分支；
  `tui/mod.rs:843` 已会读 `ProviderMeta` 的 `routed_model`/`latency_ms`，并受 `/thinking` 命令开关控制（`tui/mod.rs:295`）
- 唯独 **transport 不发** → 整条链路上游断流，`Thinking` / `ProviderMeta` 属「有消费者、无生产者」的死链路。

**另需修复（精确清单）**：
- `agent/session.rs:510`：`Ok(Some(ApiEvent::ProviderMeta(_))) => {}` —— **直接丢弃**。且 `EventSink` trait（`event_sink.rs:16-26`）目前**没有** `on_provider_meta` 方法，所以要点亮状态栏需**新增 trait 方法 + 两个实现**（TuiSink / ChannelSink）。
- `tui/mod.rs:840`：`ApiEvent::Thinking(text) => { /* 注释说累积，实际空实现 */ }` —— 若走 TUI 直连通道，需补 `self.thinking_buffer.push_str(&text)`。
  （注：**router 路径下** `session.rs:487` 已把思考流累积进 `thinking_buf` 并在首次正文/结束时经 `self.sink.on_chunk(&format_thinking_block(..))` 转发，此路径无需改。）
- **`ProviderMeta` 的解析**：transport 里**完全没有**解析响应尾注 `x-omniroute-model/provider/latency-ms/cost` 的代码 —— 这是 `ProviderMeta` 无生产者的根因。

**收益**：思考流可独立呈现，不混入正文；路由元数据可在状态栏展示。

### 🟠 P2 — 中等改造（半天，长期红利）— 🟡 部分实现
**「思考强度动态化」已做、「模型动态路由」未做：**

- ✅ **已实现**：`infer_reasoning_effort(user_msg)`（`session.rs:873`）按消息关键词/长度推断 `low|medium|high`，注入主对话请求
- ❌ **未实现**：按任务特征**切换 model**。设计目标：
  - 纯工具调用 / 短回复 → `glm-cn/glm-5-turbo` 或 `auto/fast`
  - 代码生成 → `auto/coding:pro`（实测 1.4s 反而比 fast 快）
  - 复杂推理 → `glm-cn/glm-5.2` + `reasoning_effort=high`
- ✅ 基础设施就绪：`transport.set_model()` 热切换可用（`Transport` trait 已支持，`/model set` 命令已接）
- ⚠️ 注意：`infer_reasoning_effort` 当前**只调 effort、不换 model**，且复杂/简单判定为「先到先得」的顺序匹配（复杂关键词优先），short 消息可能被复杂词误判

**收益**：综合延迟降 30-50%，成本降 20-40%（模型路由部分尚未兑现）。

### 🔴 P3 — 高级（1-2 天，锦上添花）— ❌ 未实现
**解析 OMNIRoute 路由元数据，做自优化：**

- ❌ 解析响应尾注 `x-omniroute-model/latency-ms/cost`（`ProviderMeta` 结构已定义，**无解析代码**）
- ❌ 持久化到 `~/.rhermes/metrics.jsonl`
- ❌ 启动时统计各模型平均延迟/成本，自动推荐最优模型
- ❌ 配合 P2 的动态路由形成闭环

> 📎 相关：`ProviderMeta` 结构可在 `api/mod.rs` 查（含 `routed_model` / `latency_ms` 等字段），TUI 渲染分支已存在（`tui/mod.rs:843`），只差生产端。

## 四、风险提示

1. **`auto/*` 不稳定**：`auto/coding:fast` 实测 6.9s（比 pro 还慢），说明 OMNIRoute 路由策略与名称不严格对应。**优先用具体模型 ID（如 `glm-cn/glm-5.2`）而不是 `auto/*`**。
2. **流式响应里 reasoning_content 可能与 content 交错**：GLM 系列先输出 reasoning_content 再输出 content；如果模型把 tool-call 写在 reasoning_content 里，现有 ScavengeRepair 需要适配。
3. **熔断器阈值**：OMNIRoute 作为本地网关（0.8ms 网络延迟），后端 LLM 抖动会触发熔断；现有 threshold=3 / cooldown=30s 可能过于敏感，建议改为 5/60s。
4. **cached_tokens 口径**：OMNIRoute 的 `cached_tokens` 是后端 LLM 自身缓存（不是 OMNIRoute 网关缓存，网关层实测不缓存）。RHermes 自己的前缀缓存机制（三段式 Context）才是真正可控的缓存层。

## 五、推荐实施路径（含当前进度）

```
第 1 步：P0 零代码方案                        → ✅ 已跑通
第 2 步：P1 改 3 处代码                       → ✅ 改动 1、2 已落地；❌ 改动 3（思考流分发）仍缺
第 3 步：观察 cached_tokens 数据，反推前缀优化  → ⏳ 卡在「无消费方」——字段有了但没人读
第 4 步：P2 动态模型路由（按需）               → 🟡 effort 推断已做，model 路由未做
第 5 步：P3 自优化闭环（远期）                 → ❌ 未开始
```

**下一步最小动作（按性价比排序）**：
1. **补 `transport.rs` 的 `reasoning_content` 分发**（3 行代码）→ 立刻点亮 `/thinking` 思考流展示
2. **修 `tui/mod.rs:840` 空实现 + `session.rs:510` 丢弃 ProviderMeta** → 打通已有的消费端
3. 解析 `x-omniroute-*` 尾注 → 产出 `ProviderMeta` → 状态栏显示路由/延迟
4. 在 `Usage` 消费处（如 `tui/stats`）展示 `cached_tokens` → 让缓存命中率可见
5. 扩展 `infer_reasoning_effort` → 同时决定 model（P2）

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
