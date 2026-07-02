# RHermes 架构总览

> 基于 DeepSeek 的终端 AI 编程 Agent · Rust 2024 edition · 17 个内置工具 · 自进化技能系统

---

## 一、整体架构

```
┌──────────────────────────────────────────────────────────┐
│                     main.rs (CLI 入口)                    │
│          clap 解析 → 初始化 → 启动三种模式之一             │
├──────────────────────────────────────────────────────────┤
│                     tui/mod.rs (前端)                     │
│          ratatui 终端 UI · 消息列表 · 输入框 · 状态栏      │
├──────────────────────────────────────────────────────────┤
│                     api/mod.rs (API 客户端)               │
│           DeepSeek Chat Completion · SSE 流式 · 重试      │
├──────────────────────────────────────────────────────────┤
│        agent/ (智能体核心)      │   tools/ (工具系统)     │
│  ┌────────────────────────┐    │  ┌────────────────────┐ │
│  │ memory.rs   SQLite+FTS5│    │  │ registry.rs  注册表│ │
│  │ skill.rs    Markdown   │    │  │ dispatcher.rs 调度器│ │
│  │ curator.rs  生命周期    │    │  │ builtin.rs   17工具│ │
│  │ repair.rs   修复管线    │    │  └────────────────────┘ │
│  │ task.rs     子Agent    │    │                          │
│  └────────────────────────┘    └──────────────────────────┘
├──────────────────────────────────────────────────────────┤
│          core/ (核心基础设施)                              │
│   config.rs(TOML配置) · context.rs(三段式Context)          │
│   path.rs(可移动部署模式)                                   │
├──────────────────────────────────────────────────────────┤
│     cost.rs (成本控制)     │   debug.rs (调试系统)        │
│     Flash/Pro 分级 · 自动升级 │   会话录制 · 调试报告导出   │
└──────────────────────────────────────────────────────────┘
```

---

## 二、模块详解

### 1. `core/` — 核心基础设施

| 文件 | 职责 |
|------|------|
| **config.rs** | TOML 配置 · 含 API、Request、Memory、Display、Debug、Agent 六段 |
| **context.rs** | **三段式 Context 架构** — 省 Token 的核心设计 |
| **path.rs** | 路径管理器 · 可移动模式(`home/`) |
| **prefix_cache.rs** | 前缀缓存管理器 — DeepSeek prefix cache 命中率监控与优化 |
| **http_client.rs** | 代理感知 HTTP 客户端工厂 — 分层代理（all/off/auto）+ no_proxy 排除 |

#### 三段式 Context 设计（`context.rs`）

这是 RHermes 省 Token 的精髓 —— 围绕 DeepSeek prefix cache 的 byte 级稳定性需求构建。

```
┌────────────────────────────────────────────┐
│  IMMUTABLE PREFIX (Arc<[u8]>)              │ ← session 内固定不变
│  system + tool_specs + few_shots           │ ← 缓存命中候选
├────────────────────────────────────────────┤
│  APPEND-ONLY LOG                           │ ← 单调递增
│  [assistant][tool][assistant]...           │ ← 保留之前轮次的前缀
├────────────────────────────────────────────┤
│  VOLATILE SCRATCH                          │ ← 每轮重置
│  思考/计划/临时状态                         │ ← 不发送到上游
└────────────────────────────────────────────┘
```

**三条不变式**：
1. **Prefix 一次计算** — session 建立时计算、哈希、锁定，不再改动
2. **Log 只追加** — 按顺序序列化，不重写任何已有条目
3. **Scratch 蒸馏后才能进入 Log** — 摘要压缩后追加

当 Context 窗口达到 80% 时触发自动压缩：将中间历史消息替换为 **6 段式结构化摘要**（Goal / Decisions / Files / Commands / Errors / Pending），保留最近的尾部消息。

---

### 2. `tui/mod.rs` — 终端界面

基于 **ratatui** + **crossterm** 的终端交互界面。

**核心结构体 `App`**：

- **对话消息列表** — 支持滚动，按角色着色显示
- **输入框** — 支持光标移动、中英文输入、Tab 补全
- **底部状态栏** — 实时显示 Token 用量、成本（¥）、缓存命中率、模型名、部署模式
- **后台通信** — 通过 `mpsc` channel 与 API 客户端异步通信，不阻塞 UI

**Slash 命令**：

| 命令 | 说明 |
|------|------|
| `/help` / `/?` | 显示帮助 |
| `/init` | 运行初始化向导 |
| `/config` | 查看和修改配置 |
| `/clear` | 清空对话 |
| `/note <内容>` | 记录关键笔记到记忆系统 |
| `/回忆 <关键词>` | 跨会话检索记忆 |
| `/归档` | 归档当前会话 |
| `/quit` / `/exit` | 退出程序 |

---

### 3. `api/mod.rs` — DeepSeek API 客户端

封装 DeepSeek Chat Completion API（兼容 OpenAI 格式）。

**核心能力**：

- **同步请求** — `chat()` 一次性获取完整响应
- **SSE 流式响应** — `chat_stream()` 逐 chunk 解析，实时转发文本和工具调用
- **自动重试** — 指数退避，5xx 可重试，4xx/解析错误不重试
- **Token 用量追踪** — 解析 `usage` 字段，含 prompt_cache_hit/miss 统计
- **余额查询** — `get_balance()` 获取账户人民币余额

**事件模型** — 流式解析产生 `ApiEvent` 枚举：

| 事件 | 说明 |
|------|------|
| `StreamChunk(String)` | 普通文本块 |
| `ToolCalls(Vec<ToolCallData>)` | 工具调用（含 id / name / arguments） |
| `Usage(Usage)` | Token 用量信息 |
| `Done` | 流结束 |
| `Error(String)` | 错误 |

---

### 4. `tools/` — 工具系统

| 文件 | 职责 |
|------|------|
| **registry.rs** | `ToolRegistry` — 全局只读注册表 + `Tool` trait 定义 + 参数模型 |
| **builtin.rs** | 17 个内置工具的具体实现 |
| **dispatcher.rs** | 并行调度器 — 按 `parallel_safe` 标志分组执行 |

#### 调度策略

```
[read(a), read(b), write(c), search(d), write(e)]

  并行组1 (JoinSet)         串行队列
  ┌────────────────┐       ┌─────────┐
  │ read(a)        │       │ write(c)│  ← 等并行组完成
  │ read(b)        │       └─────────┘
  │ search(d)      │            ↓
  └───────┬────────┘       ┌─────────┐
          ↓                │ write(e)│
      (全部完成)           └─────────┘
          ↓                     ↓
      合并结果 → 返回 Vec<ToolResult>
```

#### 17 个内置工具

| 类别 | 工具 | 说明 | 并行安全 |
|------|------|------|:--------:|
| **文件** | `read_file` | 读取文件（head/tail/range） | ✅ |
| | `write_file` | 写入文件 | ❌ |
| | `glob` | 按模式列出文件 | ✅ |
| | `search_content` | 文本搜索（ripgrep 后端） | ✅ |
| | `read_pdf` | 读取 PDF 纯文本 | ✅ |
| **系统** | `run_command` | 执行 shell 命令 | ❌ |
| | `get_current_time` | 获取当前时间 | ✅ |
| **网络** | `web_search` | 搜索网络（DuckDuckGo） | ❌ |
| | `web_fetch` | 获取网页内容 | ❌ |
| **Agent** | `delegate_task` | 子 Agent 独立执行 | ❌ |
| | `run_skill` | 执行已安装技能 | ❌ |
| **技能** | `skill_list` | 列出所有技能 | ✅ |
| | `skill_search` | 搜索技能 | ✅ |
| | `skill_create` | 创建新技能 | ❌ |
| | `skill_patch` / `skill_manage` | 更新/管理技能 | ❌ |
| **记忆** | `memory` | 读写管理 USER.md / MEMORY.md | ❌ |

---

### 5. `agent/` — 智能体核心

| 文件 | 职责 |
|------|------|
| **memory.rs** | 三层记忆系统：Session / Working / Long-term |
| **memory_manager.rs** | 记忆管理器，提供 `remember` / `recall` / `forget` 接口 |
| **skill.rs** | 技能引擎，Markdown playbook 格式 |
| **curator.rs** | 技能生命周期管理，自动归档 |
| **repair.rs** | Tool-Call Repair Pipeline — 四道工序 |
| **task.rs** | 子 Agent 系统，独立任务执行 |

#### 记忆系统（`memory.rs`）

**三层记忆架构**：

| 层级 | 范围 | 存储 |
|------|------|------|
| Session Memory | 当前会话（自动管理） | SQLite |
| Working Memory | 跨会话活跃知识（当前项目相关） | SQLite |
| Long-term Memory | 持久化知识库（永久保存） | SQLite + FTS5 全文索引 |

#### 技能引擎（`skill.rs`）

- Markdown 格式技能定义（含 YAML frontmatter）
- **inline** / **subagent** 两种运行模式
- 自动统计：使用次数、成功率、平均耗时
- 与 agentskills.io 格式兼容

#### Curator 技能生命周期管理（`curator.rs`）

```
active ──30天未用──→ stale ──90天未用──→ archived
   ↑                                        │
   └────────── 重新使用 ──────────────────────┘
```

启动时自动检查上次 curator 运行 > 7 天则触发检查流程。

#### Repair Pipeline（`repair.rs`）

四道工序修复 DeepSeek 模型在 tool-call 上的常见问题：

1. **Flatten** — 参数嵌套过深时转 dot-notation，dispatch 时还原
2. **Scavenge** — 从 `reasoning_content` 捞取模型忘记发出的 tool-call
3. **Truncation** — 检测并补全截断的 JSON
4. **Storm** — 抑制相同 (tool, args) 的重复调用

---

### 6. `cost.rs` — 成本控制

五个互补机制控制 Token 花费：

#### ① Flash-First 分级

| 预设 | 主模型 | 自动升级 | 适合场景 |
|------|--------|:--------:|----------|
| **Flash** | deepseek-v4-flash | ❌ | 日常开发，最省钱 |
| **Auto** | flash → pro (自动) | ✅ | 平衡成本与能力 |
| **Pro** | deepseek-v4-pro | ❌ | 复杂任务，最强推理 |

#### ② NEEDS_PRO 自动升级

模型在 Flash 级别遇到超出能力范围的任务时，在响应第一行输出 `<<<NEEDS_PRO>>>` 请求升级到 Pro。Auto 预设下自动切换，一轮结束后降级回 Flash。

#### ③ 辅助调用强制 Flash

摘要、压缩等辅助性 API 调用始终走便宜的 Flash 模型。

#### ④ 轮次自动压缩

工具结果超过 3000 token 时自动截断：保留开头 1000 字符和结尾 500 字符，中间用 `[省略 N 字符]` 替代。

#### ⑤ 缓存计费优化

DeepSeep prefix cache 命中的 input token 只按 10% 计费，因此三段式 Context 设计能显著降低成本。

---

## 三、数据流（一次完整对话）

```
用户输入
    ↓
TUI App.handle_key() 捕获键盘事件
    ↓
Enter → 构建 ChatRequest (messages + 17 个 tool definitions)
    ↓
DeepSeekClient.chat_stream() 发起 SSE 请求
    ↓
SSE 流式解析 — 逐 chunk 处理
    ├── StreamChunk → 逐字显示在 TUI 消息区
    └── ToolCalls → 进入工具执行阶段
         ↓
  ToolDispatcher.dispatch(tool_calls)
    ├── parallel_safe 工具 → JoinSet 并行执行
    └── 非 parallel_safe 工具 → 串行执行
         ↓
  工具结果写回 messages
    ↓
  再次调用 API（含工具结果）→ 模型生成新回复
    ↓
  循环直到模型不再发出 tool_call
    ↓
  API 返回 → Usage 更新成本统计
    ↓
  TUI 状态栏更新：成本/Token/缓存命中率
    ↓
  会话归档 / 记忆系统更新
```

---

## 四、技术栈

| 模块 | 选型 |
|------|------|
| 语言 | Rust 2024 edition |
| 异步 | tokio |
| HTTP | reqwest |
| TUI | ratatui + crossterm |
| 数据库 | rusqlite + FTS5 |
| 搜索 | grep-regex + grep-searcher（ripgrep 库） |
| PDF | pdf-extract |
| 配置 | TOML + serde |
| 序列化 | serde + serde_json |
| 日志 | tracing + tracing-subscriber |
| WASM | extism（WASM 插件运行时） |
| CLI | clap |
| 测试 | 119 个单元测试（全部通过） |
| 部署 | 单文件 < 10MB，可移动模式 |

---

## 五、配置（config.toml）

```toml
[api]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"

[request]
timeout_secs = 60
max_retries = 3

[memory]
max_memory_md_chars = 5000

[display]
tool_result_max_chars = 15000
read_pdf_max_chars = 30000

[debug]
enabled = false
buffer_size = 500

[agent]
max_rounds = 50
compression_ratio = 0.8
```

---

## 六、部署模式

### 可移动模式（Portable Mode）
- 所有配置/记忆/技能/会话保存在可执行文件旁的 `home/` 目录
- 适用于：U盘、云同步文件夹、Docker volume、CI/CD 挂载点

---

## 七、一句话总结

**RHermes 是一个以"省 Token"为第一性原理的终端 AI Agent**，核心创新在三段式 Context（最大化 DeepSeek prefix cache 命中）、并行工具调度、自动 Tool-Call Repair Pipeline，以及自进化的技能 + 记忆系统。
