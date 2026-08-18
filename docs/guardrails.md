# 护栏系统 (Guardrails System)

> 在每次 API 响应后对模型生成的 tool_calls 进行修复、校验与重试纠错，防止坏调用进入执行阶段。

---

## 一、总体架构

护栏系统由两层组成，在 `AgentSession` 的 Agent Loop 中串行执行：

```
API 响应 → RepairPipeline (修复) → ResponseValidator (校验) → 执行 / 重试
```

| 层级 | 模块 | 职责 |
|------|------|------|
| 修复层 | `agent/repair.rs` | 四道工序修正模型常见输出缺陷 |
| 校验层 | `agent/guardrails.rs` | 校验工具名和必填参数，失败时构建纠正消息注入 Context |
| 集成点 | `agent/session.rs` | 在 Agent Loop 第 4.5 步调用，控制重试计数和 continue 循环 |
| 配置 | `core/config.rs` | `[agent]` 段下的 `guardrails_*` 四项配置 |

---

## 二、RepairPipeline — 四道修复工序

源码：`src/agent/repair.rs`

### 工序 1：Flatten（参数压平）

**问题**：DeepSeek 在处理深层嵌套参数时容易丢字段。

**方案**：发送给模型前将嵌套参数压平为 dot-notation，收到响应后还原。

```
发送前:  {"file": {"path": "a.txt", "range": "10-20"}}
         ↓ flatten
压平:    {"file.path": "a.txt", "file.range": "10-20"}
         ↓ 模型返回
还原:    {"file": {"path": "a.txt", "range": "10-20"}}
```

- `FlattenRepair::flatten(args)` — 递归压平
- `FlattenRepair::unflatten(flat)` — 递归还原

### 工序 2：Scavenge（工具调用回收）

**问题**：DeepSeek 有时在 `reasoning_content`、`think` 块或代码块中生成了 tool-call JSON，但未在 `tool_calls` 字段中正式发出。

**方案**：用正则从文本中匹配并提取遗失的 tool-call。支持四种模式：

| 模式 | 匹配格式 | 模型来源 |
|------|---------|---------|
| 标签模式 | `<tool_call>{"name":"...","arguments":{...}}</tool_call>` | DeepSeek |
| 代码块模式 | ` ```json\n{"name":"...","arguments":{...}}\n``` ` | 通用 |
| Think 块模式 | `<think>...{JSON}...</think>` | DeepSeek R1 |
| Mistral 模式 | `[TOOL_CALLS] func_name {"key": "value"}, func2 {"k": "v"}` | Mistral |

### 工序 3：Truncation（JSON 截断补全）

**问题**：模型达到 `max_tokens` 时 JSON 在中间截断。

**方案**：检测 `{}`、`[]` 和引号是否闭合，自动补齐。

```
截断:  {"name": "read_file", "arguments": {"path": "test.txt"
补全:  {"name": "read_file", "arguments": {"path": "test.txt"}}
```

- `is_truncated(text)` — 检测 `{!=}` 或 `[!=]`
- `close_braces()` / `close_brackets()` / `close_quotes()` — 逐层补全

### 工序 4：Storm（重复调用抑制）

**问题**：模型陷入循环，对同一工具用相同参数反复调用。

**方案**：滑动窗口追踪 `(tool_name, args_signature, Instant)`，在窗口内重复超过阈值则抑制。

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `window_secs` | 60 | 滑动窗口大小 |
| `max_repeats` | 3 | 窗口内允许的最大重复次数 |

`StormSuppression` 跨轮次持久（存储在 `RepairPipeline` 中），可累计统计 `suppressed_count()`。

---

## 三、ResponseValidator — 校验器

源码：`src/agent/guardrails.rs`

对 RepairPipeline 产出的 `RepairedToolCall` 列表执行两项校验：

### 校验规则

| 检查项 | 错误类型 | 说明 |
|--------|---------|------|
| 工具名校验 | `UnknownTool(name)` | 调用名不在 `ToolRegistry` 中 |
| 必填参数校验 | `MissingRequiredParam { tool, param }` | 缺少 `required: true` 的参数 |

### 校验结果

```rust
ValidationResult {
    valid_calls: Vec<RepairedToolCall>,  // 通过校验的调用
    errors: Vec<ValidationError>,        // 失败的调用及错误
}
```

- `is_ok()` — `errors` 为空即为通过

### 依赖

`ResponseValidator` 构造函数接收 `Arc<ToolRegistry>`，因此**必须有 Dispatcher 才能执行校验**（Dispatcher 持有 Registry）。

---

## 四、NudgeBuilder — 纠正消息

源码：`src/agent/guardrails.rs`

校验失败时，`NudgeBuilder::build(errors, registry)` 构建一条 System 消息注入 Context：

```
⚠️ 工具调用校验失败，请修正后重试：
  ❌ 工具 'bad_tool' 不存在。可用工具: read_file, write_file, search_content, glob...
  ❌ 工具 'read_file' 缺少必填参数 'path'
```

- 对 `UnknownTool`：列出前 20 个可用工具名
- 对 `MissingRequiredParam`：明确标出工具名和缺失参数名

---

## 五、Session 集成流程

源码：`src/agent/session.rs`

### 初始化

```rust
// 根据 guardrails_enabled 决定是否创建 RepairPipeline
let repair_pipeline = if config.guardrails_enabled {
    Some(RepairPipeline::new(
        config.guardrails_storm_window_secs,
        config.guardrails_storm_max_repeats,
    ))
} else {
    None
};
```

### Agent Loop 中的调用（第 4.5 步）

```text
API 响应返回 tool_calls
    ↓
tool_calls 非空 && repair_pipeline.is_some()?
    │
    ├─ 否 → 跳过护栏，直接执行
    │
    └─ 是 → pipeline.repair_with_api(content, api_calls)
              ↓
         记录修复动作（tracing::debug!("护栏修复: {:?}", action)）
              ↓
         Dispatcher 存在?
           │
           ├─ 否 → 直接用修复后的 calls 执行（跳过校验）
           │
           └─ 是 → validator.validate(&repaired.tool_calls)
                     ↓
                 校验通过 → 用 valid_calls 执行
                     ↓
                 校验失败 && guardrail_retry_count < max_retries?
                   │
                   ├─ 是 → NudgeBuilder::build() → push_to_log(System)
                   │      guardrail_retry_count += 1
                   │      continue（重新调用 API）
                   │
                   └─ 否 → 用 valid_calls 执行（重试耗尽，放弃纠错）
```

---

## 六、配置项

源码：`src/core/config.rs` → `AgentConfig`

```toml
[agent]
# 护栏总开关（默认 true）
guardrails_enabled = true

# 校验失败最大重试次数（默认 3）
guardrails_max_retries = 3

# 重复调用抑制滑动窗口（秒，默认 60）
guardrails_storm_window_secs = 60

# 窗口内允许的最大重复次数（默认 3）
guardrails_storm_max_repeats = 3
```

---

## 七、检测护栏工作情况

### 方式一：Tracing 日志（推荐）

| 级别 | 日志内容 | 含义 |
|------|---------|------|
| `DEBUG` | `护栏修复: Flattened` | 参数被压平/还原 |
| `DEBUG` | `护栏修复: Scavenged("tool_name")` | 从文本中回收了遗失调用 |
| `DEBUG` | `护栏修复: TruncationFixed` | JSON 截断被修复 |
| `DEBUG` | `护栏修复: StormSuppressed("id")` | 重复调用被抑制 |
| `WARN` | `护栏校验失败，注入纠正消息:\n...` | 校验未通过，正在重试 |
| `WARN` | `护栏：所有工具调用被校验拦截` | 所有调用均未通过校验 |

### 方式二：计数器

| 计数器 | 位置 | 说明 |
|--------|------|------|
| `guardrail_retry_count` | `AgentSession` | 当前会话累计重试次数 |
| `suppressed_count()` | `StormSuppression` | 累计被风暴抑制的次数 |

### 方式三：配置文件

确认 `config.toml` 中 `guardrails_enabled = true`。

---

## 八、护栏的局限性

护栏**并非所有情况下都可用**：

| 场景 | 护栏行为 |
|------|---------|
| `guardrails_enabled = false` | 整条管线不初始化，完全不执行 |
| 纯文本回复（无 tool_calls） | 不需要护栏，跳过 |
| Dispatcher 为 None（简化模式） | RepairPipeline 执行但**校验跳过** |
| 重试次数耗尽（> `max_retries`） | 不再注入纠正消息，剩余 valid_calls 直接放行 |
| 校验全拦截（所有 calls 非法） | 记录 WARN 日志，`continue` 等下一轮模型响应 |

**护栏生效的完整前提**：`guardrails_enabled = true` + 有 `tool_calls` + 有 `Dispatcher` + 未超重试上限。

---

## 九、相关文件

| 文件 | 内容 |
|------|------|
| `src/agent/repair.rs` | RepairPipeline 四道工序实现 + 测试 |
| `src/agent/guardrails.rs` | ResponseValidator / NudgeBuilder + 测试 |
| `src/agent/session.rs` | AgentSession 护栏集成逻辑 |
| `src/core/config.rs` | `[agent]` 段护栏配置定义 |
