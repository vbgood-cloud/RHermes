# RHermes 帮助文档

> **版本**: v0.2.0 | **语言**: Rust 2024 Edition | **协议**: MIT

---

## 目录

1. [项目概述](#1-项目概述)
2. [快速开始](#2-快速开始)
3. [架构总览](#3-架构总览)
4. [核心模块](#4-核心模块)
5. [API 客户端](#5-api-客户端)
6. [Agent 智能体系统](#6-agent-智能体系统)
7. [工具系统](#7-工具系统)
8. [TUI 终端界面](#8-tui-终端界面)
9. [配置说明](#9-配置说明)
10. [内置工具参考](#10-内置工具参考)
11. [技能系统](#11-技能系统)
12. [记忆系统](#12-记忆系统)
13. [Tool-Call 修复管线](#13-tool-call-修复管线)
14. [Curator 技能生命周期管理](#14-curator-技能生命周期管理)
15. [开发指南](#15-开发指南)

---

## 1. 项目概述

RHermes 是一个**终端 AI 编码 Agent**，基于 Rust 构建，使用 DeepSeek API 作为后端大语言模型。它通过 TUI（Terminal User Interface）提供交互式编程体验，支持文件读写、代码搜索、PDF 解析、网络搜索、子任务委派等 17 个内置工具，以及自进化的技能系统和三层记忆架构。

### 核心特性

| 特性 | 说明 |
|------|------|
| 三层上下文架构 | Immutable Prefix + Append-Only Log + Volatile Scratch，优化 DeepSeek 前缀缓存 |
| 并行工具调度 | 基于 `JoinSet` 的并行执行，按 `parallel_safe` 标志分组 |
| Tool-Call 修复 | 四道工序自动修复模型输出的工具调用问题 |
| 技能进化引擎 | Markdown 格式的 Playbook，带遥测和使用统计 |
| 三层记忆系统 | Session / Working / Long-term，SQLite + FTS5 全文检索 |
| 自治 Curator | 技能生命周期管理（active → stale → archived） |
| SSE 流式输出 | 实时 Token 级流式响应展示 |

### 技术栈

| 类别 | 技术 |
|------|------|
| 语言 | Rust 2024 Edition |
| 异步运行时 | tokio (full) |
| TUI 框架 | ratatui 0.29 + crossterm |
| HTTP 客户端 | reqwest (json + stream) |
| 数据库 | rusqlite (bundled + FTS5) |
| 文本搜索 | grep-regex + ripgrep-searcher |
| PDF 解析 | pdf-extract |
| CLI 解析 | clap derive |
| 交互提示 | dialoguer (ColorfulTheme) |
| 序列化 | serde / toml / json |

---

## 2. 快速开始

### 安装构建

```bash
# 克隆项目
git clone <repo-url>
cd RHermes

# 构建 release 版本
cargo build --release

# 运行
./target/release/rhermes code
```

### 首次初始化

```bash
# 启动交互式初始化向导（4 步）
./target/release/rhermes init
```

初始化流程：
1. **部署模式选择** — Portable（便携）或 Traditional（传统）
2. **API Key 输入** — 验证 `sk-` 前缀
3. **模型选择** — flash / pro / custom
4. **Base URL 配置** — 可选自定义 API 地址

### 日常使用

```bash
# 启动编码会话
rhermes code

# 从上次中断处恢复
rhermes code --resume

# 导出调试信息
rhermes debug export
```

---

## 3. 架构总览

```
┌─────────────────────────────────────────────────────────────┐
│                        main.rs (CLI 入口)                     │
│  clap 解析 → 初始化链 → TUI App 循环                         │
└──────────────────────┬──────────────────────────────────────┘
                       │
          ┌────────────┼────────────┐
          ▼            ▼            ▼
   ┌──────────┐  ┌──────────┐  ┌──────────┐
   │  Core    │  │   API    │  │   TUI    │
   │ Config   │  │DeepSeek  │  │ ratatui  │
   │ Context  │  │SSE Stream│  │ crossterm│
   │ PathMgr  │  │ Retry    │  │ Channel  │
   └────┬─────┘  └────┬─────┘  └────┬─────┘
        │             │              │
        ▼             ▼              ▼
   ┌──────────┐  ┌──────────┐  ┌──────────┐
   │  Agent   │  │  Tools   │  │ Dispatch │
   │ Memory   │  │ Registry │  │ Parallel │
   │ Skill    │  │ Builtin  │  │ JoinSet  │
   │ Repair   │  │ 17 tools │  │Semaphore │
   │ Curator  │  │          │  │          │
   └──────────┘  └──────────┘  └──────────┘
```

### 启动初始化链

```
PathManager::detect()           ← 检测路径模式
    ↓
ToolRegistry::builtin_registry() ← 注册 17 个内置工具
    ↓
Config::load()                  ← 加载 config.toml + .env
    ↓
MemorySystem::open()            ← 打开/创建 SQLite 数据库
    ↓
SkillEngine::load()             ← 扫描 skills/ 目录加载 .md 技能
    ↓
DebugSystem::init()             ← 初始化调试子系统
    ↓
Curator::new().run()            ← 运行 curator 检查（7 天间隔）
    ↓
App::new() → run()              ← 进入 TUI 主循环
```

---

## 4. 核心模块

### 4.1 配置管理 (`core/config.rs`)

配置分离为两部分：
- **`config.toml`** — 非敏感配置（模型、超时、记忆参数等）
- **`.env`** — API Key（永不写入 TOML）

#### Config 结构

```rust
pub struct Config {
    pub api_key: String,           // 来自 .env
    pub api: ApiConfig,            // model, base_url, timeout, retries
    pub request: RequestConfig,    // max_rounds, compression_ratio
    pub memory: MemoryConfig,      // nudge intervals, max chars
    pub debug: DebugConfig,        // 调试开关
    pub display: DisplayConfig,    // read_pdf_max_chars 等
    pub agent: AgentConfig,        // agent 特定参数
}
```

#### 默认值速查

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `model` | `deepseek-v4-flash` | 使用的模型 |
| `base_url` | `https://api.deepseek.com` | API 端点 |
| `timeout` | 60s | 请求超时 |
| `retries` | 3 | 重试次数 |
| `max_rounds` | 50 | 最大对话轮次 |
| `compression_ratio` | 0.8 | 触发压缩的上下文占比 |
| `tool_result_max_chars` | 15000 | 工具返回最大字符数 |
| `read_pdf_max_chars` | 30000 | PDF 预览最大字符数 |
| `max_memory_md_chars` | 2200 | USER.md 最大字符数 |

### 4.2 路径管理 (`core/path.rs`)

双模式支持：

| 模式 | 条件 | 配置目录 | 数据目录 |
|------|------|----------|----------|
| Portable | 二进制旁有 `home/` 目录 | `<bin_dir>/home/.config/rhermes` | `<bin_dir>/home/` |
| Traditional | 默认 | `$XDG_CONFIG_HOME/rhermes` 或平台等效 | 平台数据目录 |

#### 子路径说明

| 方法 | 用途 |
|------|------|
| `config_path()` | config.toml 位置 |
| `memory_db_path()` | SQLite 数据库位置 |
| `skills_dir()` | 技能目录 |
| `sessions_dir()` | 会话存档 |
| `logs_dir()` | 日志目录 |
| `cache_dir()` | 缓存目录 |
| `workspace_dir()` | 工作区目录 |

### 4.3 三层上下文 (`core/context.rs`)

这是 RHermes 的核心数据结构，专为 DeepSeek 前缀缓存优化：

```
┌─────────────────────────────────────────┐
│           Context (完整状态)              │
│                                         │
│  ┌──────────────┐  immutable_prefix     │
│  │ System Prompt │  Arc<[u8]> 字节锁定   │
│  │ + 用户指令    │  不可变，命中前缀缓存  │
│  └──────────────┘                      │
│  ┌──────────────┐  append_only_log      │
│  │ 历史消息      │  Vec<u8> 只追加       │
│  │ (tool result) │  压缩时才清理         │
│  └──────────────┘                      │
│  ┌──────────────┐  scratch              │
│  │ 当前轮次消息   │  Vec<Message> 易失    │
│  │ (临时性)      │  每轮结束可蒸馏       │
│  └──────────────┘                      │
│                                         │
│  approx_tokens: usize                   │
└─────────────────────────────────────────┘
```

#### 关键方法

| 方法 | 功能 |
|------|------|
| `new(system_prompt)` | 创建新上下文，设置不可变前缀 |
| `extend_prefix(content)` | 追加到不可变前缀（启动时一次性操作） |
| `push_to_log(message)` | 追加到日志（序列化为 JSON） |
| `push_to_scratch(message)` | 推入临时区 |
| `clear_scratch()` | 清空临时区（新轮次开始时） |
| `distill_scratch_to_log()` | 将 Scratch 中关键信息蒸馏到 Log |
| `build_request_body()` | 构建发送给 API 的完整请求体 |
| `needs_compress()` | 检查是否需要压缩（>80% 窗口） |
| `compress()` | 六段式结构化摘要压缩 |

#### 压缩算法

触发条件：已用 token 超过上下文窗口的 `compression_ratio`（默认 80%）

压缩策略：
- 尾部预算：约 16K tokens
- 最少保留：最近 2 条消息
- 六段摘要结构：**Goal / Decisions / Files / Commands / Errors / Pending**

#### 序列化格式

消息以 JSON 数组元素形式存储，每个元素前缀逗号分隔：
```text
,{"role":"user","content":"..."}
,{"role":"assistant","content":"...", "tool_calls":[...]}
,{"role":"tool","content":"..."}
```

---

## 5. API 客户端 (`api/mod.rs`)

### DeepSeekClient

封装与 DeepSeek API 的所有通信。

#### 核心能力

| 方法 | 类型 | 说明 |
|------|------|------|
| `chat()` | 同步 | 非流式请求（用于子 Agent） |
| `chat_stream()` | 异步流 | SSE 流式请求，通过 mpsc channel 返回事件 |
| `chat_with_retry()` | 自动重试 | 指数退避重试（500ms * 2^attempt） |
| `get_balance()` | 同步 | 查询账户余额（CNY） |

#### SSE 事件类型 (`ApiEvent`)

```rust
enum ApiEvent {
    StreamChunk(String),       // 文本内容片段
    Done,                      // 流结束
    Usage(Usage),              // 使用量统计（含 prompt_cache_hit_tokens）
    ToolCalls(Vec<ToolCallData>), // 工具调用
    Balance(f64),              // 余额查询结果
    Error(String),             // 错误
}
```

#### 重试策略

- **可重试错误**：网络错误、HTTP 5xx
- **不可重试错误**：HTTP 4xx、解析错误
- **退避公式**：`500ms × 2^attempt`
- **最大重试次数**：由 `config.api.retries` 控制（默认 3 次）

#### 内置工具定义 (`default_tools()`)

返回 17 个 OpenAI function-calling 格式的 `ToolDef`，注册到 API 请求中让模型感知可用工具。

---

## 6. Agent 智能体系统

Agent 模块是 RHermes 的"大脑"，包含以下子模块：

### 6.1 记忆系统 (`agent/memory.rs`)

#### 三层架构

| 层级 | 存储位置 | 生命周期 | 用途 |
|------|----------|----------|------|
| Session | SQLite `memories` 表 | 当前会话 | 对话中的即时信息 |
| Working | SQLite + MEMORY.md | 跨数天 | 项目级工作记忆 |
| LongTerm | SQLite + USER.md | 永久 | 用户画像和长期偏好 |

#### 数据表 Schema

```sql
-- memories 主表
CREATE TABLE memories (
    id INTEGER PRIMARY KEY,
    type TEXT NOT NULL,           -- session/working/longterm
    content TEXT NOT NULL,
    tags TEXT,                    -- JSON 数组
    project TEXT,
    created_at TEXT,
    updated_at TEXT,
    last_accessed TEXT,
    access_count INTEGER DEFAULT 0
);

-- FTS5 全文检索虚拟表
CREATE VIRTUAL TABLE memories_fts USING fts5(
    content, tags, project,
    content=memories, content_rowid=rowid
);

-- 用户画像表
CREATE TABLE user_profile (
    id INTEGER PRIMARY KEY,
    preferred_languages TEXT,    -- JSON 数组
    common_tasks TEXT,           -- JSON 数组
    expertise_level TEXT,
    interaction_style TEXT,
    skill_preferences TEXT,      -- JSON
    session_count INTEGER DEFAULT 0,
    total_messages INTEGER DEFAULT 0
);
```

#### 核心方法

| 方法 | 说明 |
|------|------|
| `store(type, content, tags, project)` | 存入记忆条目 |
| `remember(content, tags, project)` | 简化存储接口 |
| `get(id)` | 按 ID 获取（自动递增 access_count） |
| `search(query, limit)` | FTS5 全文搜索 |
| `list(type, limit)` | 按类型列出 |
| `delete(id)` | 删除单条 |
| `should_nudge(last_nudge)` | 是否应该提醒（5 分钟间隔） |
| `export_memory_md(path, project, max)` | 导出为 Markdown 格式 |
| `aggregate_profile()` | 自动提取用户画像 |

#### UserProfile 字段

系统会从记忆条目中自动聚合用户画像：
- `preferred_languages` — 通过代码注释语言检测
- `common_tasks` — 通过关键词匹配识别高频任务
- `expertise_level` — 通过任务复杂度推断
- `interaction_style` — 通过对话模式学习
- `skill_preferences` — 统计技能使用偏好

### 6.2 记忆管理器 (`agent/memory_manager.rs`)

编排层，统一管理内置 Provider 和外部 Provider（预留）。

#### 架构

```
Agent Loop
    ↓
MemoryManager（编排层）
    ├── BuiltinProvider（始终存在）
    │   ├── SQLite + FTS5
    │   └── MEMORY.md / USER.md
    └── ExternalProvider（可选，trait）
        └── honcho / mem0 / memgpt / ...
```

#### Provider Trait 接口

```rust
trait MemoryProvider: Send + Sync {
    fn prefetch(&self, query: &str, limit: usize) -> Vec<MemoryEntry>;
    fn inject(&self, entries: &[MemoryEntry]) -> String;
    fn intercept(&self, tool_name: &str, tool_args: &str) -> Option<String>;
    fn sync(&self, content: &str, tags: &[&str], project: &str);
    fn flush(&self) -> Result<(), String>;
}
```

#### 生命周期钩子流程

```
prefetch(query) → 搜索相关记忆
      ↓
inject(entries) → 合并到上下文字符串
      ↓
intercept(tool) → 拦截工具调用补充记忆（可选）
      ↓
sync(content) → 写入新记忆
      ↓
flush() → 持久化到磁盘
```

#### 外部 Provider 类型（预留）

| 类型 | name |
|------|------|
| Honcho | `honcho` |
| Mem0 | `mem0` |
| MemGpt | `memgpt` |
| Memary | `memary` |
| Letta | `letta` |
| Zep | `zep` |
| Graphiti | `graphiti` |
| Custom | `custom` |

### 6.3 子 Agent 系统 (`agent/task.rs`)

#### `run_sub_agent(task, context, config)`

创建独立的 API 调用：
- 任务聚焦的 system prompt
- `max_tokens=2048`
- 返回 `SubAgentResult { success, output, duration_ms }`

#### `auto_refine_skill(conversation)`

审查对话发现可学习的模式：
- 创建或更新技能
- 最多 8 次迭代
- 通过 `skill_manage` 工具执行

#### `auto_refine_memory(conversation)`

审查对话中值得跨会话记住的用户事实：
- 使用 memory 工具存储
- 自动打标签

#### `run_parallel(tasks)并发执行多个子 Agent`：
- 使用 `tokio::spawn` 
- 收集所有结果

### 6.4 技能引擎 (`agent/skill.rs`)

详见 [第 11 节：技能系统](#11-技能系统)。

---

## 7. 工具系统

### 7.1 工具特征 (`tools/registry.rs`)

所有工具必须实现 `Tool` trait：

```rust
#[async_trait]
trait Tool: Send + Sync {
    fn name(&self) -> &'static str;           // 工具名（API 匹配用）
    fn description(&self) -> &'static str;    // 描述（模型理解何时调用）
    fn parallel_safe(&self) -> bool;          // 是否可并行
    fn parameters(&self) -> Vec<ParamDef>;    // 参数定义列表
    async fn execute(&self, args: Value) -> Result<String, ToolError>;
}
```

### 7.2 参数系统

```rust
struct ParamDef {
    name: &'static str,
    param_type: ParamType,    // String / Integer / Float / Boolean / Array / Object
    description: &'static str,
    required: bool,
}
```

构造辅助：
- `ParamDef::required(name, type, desc)` — 必填参数
- `ParamDef::optional(name, type, desc)` — 可选参数

### 7.3 工具注册表

```rust
struct ToolRegistry {
    tools: Arc<HashMap<&'static str, Arc<dyn Tool>>>,  // 运行期只读
}
```

- `register(tool)` — 注册单个工具（Builder 模式）
- `register_all(tools)` — 批量注册
- `get(name)` — 按名获取
- `parallel_safe_names()` — 所有可并行的工具名
- `all_names()` — 全部工具名

### 7.4 并行调度器 (`tools/dispatcher.rs`)

#### 调度策略

```
输入: [read(a), read(b), write(c), search(d), write(e)]

时间线:
t=0 ─→ [并行组1: JoinSet] ─→ t=N ─→ write(c) ─→ t=N+1 ─→ [并行组2: search(d)] ─→ t=M ─→ write(e)
        │ read(a)                          │
        │ read(b)                          │
        └──────────────────────────────────┘
```

**规则**：
1. 连续的 `parallel_safe=true` 工具归为一组，用 `JoinSet` 并发执行
2. `parallel_safe=false` 的工具单独执行，且需等待当前并行组完成
3. 最大并行度由 `Semaphore` 控制（默认 10）
4. 保持原始调用顺序返回结果

#### ToolDispatcher 配置

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `max_concurrency` | 10 | 最大并行任务数 |

---

## 8. TUI 终端界面 (`tui/mod.rs`)

基于 **ratatui 0.29** + **crossterm**，约 1500 行。

### 通信机制

```
┌──────────┐  mpsc::channel  ┌──────────────────┐
│   TUI    │ ◄────────────── │  API 后台任务    │
│  (主线程) │   AppCommand    │  (tokio::spawn)  │
│  渲染循环 │                 │  SSE / Retry     │
└──────────┘                 └──────────────────┘
```

### 主要组件

| 组件 | 说明 |
|------|------|
| `App` | 主应用状态机，持有 Context、API Client、Dispatcher 等 |
| `Message` | 角色 + 内容（User / Assistant / System） |
| `Stats` | 实时统计（本轮成本、累计成本、余额、token 用量） |
| `AppCommand` | TUI → 后台的通道命令（目前仅 SendMessage） |

### 键盘绑定

| 按键 | 功能 |
|------|------|
| `Enter` | 发送消息 |
| `Ctrl+C` / `Esc` | 退出 |
| `Ctrl+U` | 清空输入 |
| 方向键 / Page Up/Down | 滚动历史 |

### 日志

同时输出到：
- **stderr** — 终端实时查看
- **`rhermes.log`** — 文件持久化（tracing-subscriber）

---

## 9. 配置说明

### config.toml 示例

```toml
[api]
model = "deepseek-v4-flash"       # 或 deepseek-v4-pro / 自定义模型
base_url = "https://api.deepseek.com"
timeout_sec = 60
retries = 3

[request]
max_rounds = 50
compression_ratio = 0.8

[memory]
creation_nudge_interval_min = 15
memory_nudge_interval_min = 10
max_memory_md_chars = 2200

[display]
read_pdf_max_chars = 30000

[agent]
tool_result_max_chars = 15000
```

### .env 文件

```
DEEPSEEK_API_KEY=sk-your-key-here
```

> ⚠️ API Key 仅存储在 `.env` 中，绝不序列化到 config.toml。

### 配置加载优先级

1. `config.toml` 中的显式值
2. 内置默认值
3. `.env` 中的环境变量覆盖（仅 API Key）

---

## 10. 内置工具参考

RHermes 内置 **17 个工具**，分为可并行和串行两类：

### 可并行工具 (parallel_safe = true)

| 工具名 | 功能 | 关键参数 |
|--------|------|----------|
| `read_file` | 读取文件内容 | `path`(必), `head`, `tail`, `range` |
| `search_content` | 正则搜索文件内容（ripgrep） | `pattern`(必), `path`, `glob` |
| `glob` | 按 glob 模式列文件（fd） | `pattern`(必), `path` |
| `get_current_time` | 获取当前时间 | 无参数 |
| `skill_list` | 列出所有已安装技能 | 无参数 |
| `skill_search` | 按关键词搜索技能 | `query`(必) |
| `read_pdf` | 读取 PDF 提取文本 | `path`(必) |

### 串行工具 (parallel_safe = false)

| 工具名 | 功能 | 关键参数 |
|--------|------|----------|
| `write_file` | 写入/创建文件 | `path`(必), `content`(必) |
| `run_command` | 执行 Shell 命令 | `command`(必), `timeout`, `cwd` |
| `web_search` | DuckDuckGo 搜索 | `query`(必) |
| `web_fetch` | 获取网页内容 | `url`(必), `max_chars` |
| `delegate_task` | 委派给子 Agent | `task`(必), `context` |
| `run_skill` | 执行已安装技能 | `name`(必), `arguments`(必) |
| `skill_create` | 创建新技能 | `name`(必), `description`(必), `body`(必), `category`, `allowed_tools` |
| `skill_patch` | 更新已有技能 | `name`(必), `description`, `body`, `allowed_tools` |
| `skill_manage` | 创建或更新技能（智能合并） | `name`(必), `description`(必), `body`(必), `category` |
| `memory` | 写入长期记忆 | `action`(必), `target`(必), `content`(必) |

### 工具详情

#### read_file
- 支持 `head`（前 N 行）、`tail`（后 N 行）、`range`（行范围如 `"50-100"`）三种截取模式
- 输出格式：`[path] (字节数)\n内容`

#### search_content
- 底层使用 **ripgrep** (`grep-regex` + `grep-searcher`)
- 支持正则表达式、`.gitignore` 过滤、glob 文件过滤
- 最大返回 200 条结果
- 目录遍历使用 `ignore::WalkBuilder`（自动尊重 gitignore）

#### run_command
- Windows 使用 `cmd /c`，Unix 使用 `sh -c`
- **重要**：断开 stdin（`Stdio::null()`），防止抢走 TUI 键盘输入
- 默认超时 60 秒，可通过参数调整
- 输出包含 stdout、stderr 和退出码

#### web_search
- 使用 **DuckDuckGo Instant Answer API**（无需 API Key）
- 返回摘要、答案和相关主题链接

#### web_fetch
- 下载指定 URL 内容
- 自动检测 HTML 并剥离标签（保留 script/style 处理）
- 可限制最大字符数

#### delegate_task
- 创建独立子 Agent 执行深度分析任务
- 子 Agent 有自己的 system prompt 和 token 预算
- 返回结果 + 耗时

#### skill_* 系列
- `skill_create`：全新创建
- `skill_patch`：部分更新（不传的字段保持不变）
- `skill_manage`：智能合并（存在则 patch，不存在则 create）
- 所有技能文件为 Markdown 格式，含 YAML frontmatter

#### memory
- 目前支持 `action=add` + `target=user`
- 写入 `USER.md`（原子写入：先写 .tmp 再 rename）
- 用于记录用户偏好、纠正、个人信息

---

## 11. 技能系统 (`agent/skill.rs`)

### 技能文件格式

技能是 **Markdown + YAML Frontmatter** 格式的 Playbook：

```markdown
---
name: my-skill
description: 一句话描述用途
run_as: subagent          # inline(追加到父log) | subagent(隔离调用)
model: deepseek-v4-flash   # 可选，覆盖默认模型
allowed_tools:            # 可选
  - read_file
  - search_content
---

# 技能标题

技能正文，描述执行步骤、注意事项、最佳实践...
```

### RunAs 模式

| 模式 | 行为 |
|------|------|
| `Inline` | 技能内容追加到父 Agent 的 append-only log |
| `Subagent` | 创建独立 API 调用，隔离执行（默认） |

### Skill 结构

```rust
struct Skill {
    name: String,
    category: Option<String>,
    description: String,
    body: String,               // Markdown 正文
    run_as: RunAs,
    allowed_tools: Option<Vec<String>>,
    model_override: Option<String>,
    telemetry: UsageTelemetry,  // 使用统计
}
```

### UsageTelemetry（.usage.json sidecar）

每个技能同目录下维护一个 `.usage.json` 文件：

```json
{
  "use_count": 42,
  "view_count": 10,
  "patch_count": 5,
  "last_used_at": "2026-06-04T10:00:00Z",
  "created_at": "2026-01-01T00:00:00Z",
  "archived_at": null,
  "pinned": false
}
```

### SkillEngine 核心 API

| 方法 | 功能 |
|------|------|
| `load(skills_dir)` | 递归扫描 .md 文件，解析 frontmatter + telemetry |
| `create(name, description, body, run_as)` | 创建新技能（名称校验：字母数字+_-） |
| `create_with_category(..., category)` | 创建到指定子目录 |
| `update_skill(name, desc?, body?, tools?, model?)` | 部分更新（patch） |
| `delete(name)` | 删除（pinned 保护） |
| `record_usage(name, success, duration_ms)` | 记录使用情况 |
| `search(query)` | 名称/描述模糊搜索 |
| `suggest_optimizations()` | 健康建议 |

### 健康指标

| 指标 | 公式 | 阈值 |
|------|------|------|
| 成功率 | `success / use_count` | >50% healthy, <30% needs_attention |
| 使用频率 | `days_since_last_used` | 30d→stale, 90d→archived |

### 名称校验规则

只允许：小写英文字母、数字、连字符 `-`、下划线 `_`

---

## 12. 记忆系统

### 存储层次

```
┌─────────────────────────────────────┐
│         Long-term (永久)             │
│  用户偏好 · 个人信息 · 纠正记录       │
│  存储: SQLite + USER.md 导出          │
├─────────────────────────────────────┤
│         Working (跨天数)             │
│  项目上下文 · 当前进度 · 工作结论     │
│  存储: SQLite + MEMORY.md 导出        │
├─────────────────────────────────────┤
│         Session (当前会话)           │
│  对话中的临时信息 · 即时上下文        │
│  存储: SQLite                        │
└─────────────────────────────────────┘
```

### Nudge 提醒机制

| 类型 | 间隔 | 触发行为 |
|------|------|----------|
| Creation | 每 15 分钟 | 提醒 Agent 将有价值的信息存为记忆 |
| Memory | 每 10 分钟 | 提醒 Agent 检索相关记忆注入上下文 |

### 自动画像聚合

`aggregate_profile()` 方法从记忆条目中自动提取：

- **语言偏好**：检测代码注释中的语言关键词
- **常见任务**：匹配预定义的任务类别关键词
- **专业水平**：推断任务复杂度级别
- **交互风格**：学习对话模式
- **技能偏好**：统计各技能的使用分布

---

## 13. Tool-Call 修复管线 (`agent/repair.rs`)

针对 DeepSeek 模型在 tool-call 输出上的常见问题，设计了四道修复工序：

### 管线概览

```
模型原始响应
    ↓
[工序1] Flatten — 参数嵌套还原
    ↓
[工序2] Scavenge — 从 reasoning_content 捞取丢失的 tool-call
    ↓
[工序3] Truncation — 补全被截断的 JSON
    ↓
[工序4] Storm — 抑制重复调用
    ↓
修复后的 RepairedResponse { content, tool_calls, injected_reflection, actions }
```

### 工序 1: Flatten（参数压平/还原）

**问题**：深层嵌套 JSON 参数容易导致模型丢失字段。

**方案**：
- 发送给模型前：`{"a":{"b":"value"}}` → `{"a.b":"value"}`
- 收到响应后：还原为标准嵌套格式
- 递归处理任意层级

### 工序 2: Scavenge（回收丢失的工具调用）

**问题**：模型有时在 `reasoning_content` / `Think` 块中生成了 tool-call JSON，但忘记在正式的 `content` 或 `tool_calls` 字段中发出。

**扫描模式**（按优先级）：

| 模式 | 目标区域 | 示例 |
|------|----------|------|
| 模式 1 | `...` 标签内 | `{"name":"read_file",...}` |
| 模式 2 | ```json ... ``` 代码块 | 标准 JSON 代码块 |
| 模式 3 | `...` 思考块 | Think 标签内的 JSON 对象 |

### 工序 3: Truncation（截断补全）

**问题**：达到 `max_tokens` 时 JSON 在中间被截断。

**修复逻辑**：
1. 检测 `{`/`}` `[`/`]` 不配对
2. 补全缺失的 `}` `]` `"`
3. 处理转义字符状态跟踪

### 工序 4: Storm Suppression（风暴抑制）

**问题**：模型可能重复发出相同的工具调用。

**机制**：
- 滑动窗口（默认 5 秒）
- 按 `(tool_name, args_signature)` 去重
- 超过阈值时抑制并注入反思指令
- 可配置 `window_secs` 和 `max_repeats`

### RepairPipeline 使用

```rust
let mut pipeline = RepairPipeline::new(window_secs: 5, max_repeats: 1);
let repaired = pipeline.repair(&model_response_text);

// repaired.content      — 修复后的文本
// repaired.tool_calls   — 提取出的工具调用
// repaired.injected_reflection — 是否触发了反思
// repaired.actions      — 各阶段执行的修复动作
```

---

## 14. Curator 技能生命周期管理 (`agent/curator.rb`)

### 状态机

```
              30 天未用          90 天未用
  active ──────────────→ stale ──────────────→ archived
    ↑                                           │
    └────────────────── 重新使用 ────────────────┘
```

### 触发条件

- **首次运行**：无 `.curator_last_run` 标记文件
- **间隔运行**：距上次运行超过 **7 天**
- 启动时自动检查

### Curator 操作流程

```
should_run()? ──No──→ 跳过
     │ Yes
     ▼
snapshot() ──→ 创建 _snapshots/timestamp/ 快照目录
     ▼
scan_skill_states() ──→ 遍历所有 .md + .usage.json
     │
     ├─ Active (<30天) → 跳过
     ├─ Stale (30-90天) → mark_stale() 写入 status: stale
     └─ Archived (>90天) → archive_skill() 移至 _archived/
     ▼
write_marker() ──→ 更新 .curator_last_run 时间戳
```

### 归档细节

- 归档目标：`skills/_archived/{name}.md`
- 原位置留下 `{name}.md.archived` 标记文件（含恢复指引）
- **Pinned 技能跳过检查**
- 归档前创建快照备份到 `_snapshots/`

### CuratorReport

```rust
struct CuratorReport {
    message: String,           // 摘要
    archived: Vec<String>,     // 已归档技能名
    stale: Vec<String>,        // 已标记过期技能名
    errors: Vec<String>,       // 错误信息
    snapshot_path: Option<String>, // 快照路径
    duration_ms: u64,          // 执行耗时
}
```

---

## 15. 开发指南

### 项目结构

```
RHermes/
├── Cargo.toml                 # 项目清单
├── README.md                  # 项目简介
├── src/
│   ├── main.rs                # CLI 入口 (291 行)
│   ├── init.rs                # 初始化向导 (185 行)
│   ├── core/
│   │   ├── mod.rs             # Core 模块重导出
│   │   ├── config.rs          # 配置管理 (487 行, 9 测试)
│   │   ├── context.rs         # 三层上下文 (522 行, 11 测试)
│   │   └── path.rs            # 路径管理 (301 行, 4 测试)
│   ├── api/
│   │   └── mod.rs             # DeepSeek API 客户端 (805 行, 6 测试)
│   ├── agent/
│   │   ├── mod.rs             # Agent 模块重导出
│   │   ├── memory.rs          # 三层记忆 (740 行, 13 测试)
│   │   ├── memory_manager.rs  # 记忆编排层 (353 行, 5 测试)
│   │   ├── skill.rs           # 技能引擎 (945 行, 14 测试)
│   │   ├── task.rs            # 子 Agent 系统 (311 行)
│   │   ├── repair.rs          # Tool-Call 修复 (679 行, ~15 测试)
│   │   └── curator.rs         # 技能生命周期 (503 行, 8 测试)
│   ├── tools/
│   │   ├── mod.rs             # 工具模块重导出
│   │   ├── registry.rs        # 工具注册表 + Trait (352 行, 5 测试)
│   │   ├── builtin.rs         # 17 个内置工具实现 (1182 行, 8 测试)
│   │   └── dispatcher.rs      # 并行调度器 (311 行, 5 测试)
│   └── tui/
│       └── mod.rs             # TUI 界面 (~1500 行)
├── docs/
│   ├── CHANGELOG.md
│   └── plans.md
└── rhermes.log                # 运行日志
```

### 代码统计

| 模块 | 源码行数（约） | 测试数量 |
|------|----------------|----------|
| core/ | ~1310 | 24 |
| api/ | ~805 | 6 |
| agent/ | ~3530 | ~55 |
| tools/ | ~1845 | ~18 |
| tui/ | ~1500 | — |
| main.rs + init.rs | ~476 | — |
| **合计** | **~9466** | **~103** |

### 添加新工具

1. 在 `tools/builtin.rs` 中实现 `Tool` trait
2. 决定 `parallel_safe()` 返回值
3. 在 `builtin_registry()` 中注册
4. （可选）在 `api/default_tools()` 中添加 ToolDef

### 添加新技能

技能只需在 `skills/` 目录放置 Markdown 文件即可：

```bash
# 创建技能文件
cat > skills/my-skill.md << 'EOF'
---
name: my-skill
description: 我的自定义技能
run_as: subagent
allowed_tools:
  - read_file
  - search_content
---

# My Skill

步骤 1: ...
步骤 2: ...
注意: ...
EOF
```

下次启动时 SkillEngine 会自动加载。

### 运行测试

```bash
# 全部测试
cargo test

# 单个模块
cargo test --lib core::config
cargo test --lib agent::repair
cargo test --lib tools::dispatcher

# 显示输出
cargo test -- --nocapture
```

### 设计决策记录

| 决策 | 原因 |
|------|------|
| 三层上下文 | 优化 DeepSeek 前缀缓存命中率，Immutable Prefix 利用字节锁零拷贝共享 |
| parallel_safe 分组 | 读操作安全并行，写操作必须串行，避免竞态 |
| Markdown 技能格式 | 人类可读可编辑，版本控制友好，LLM 原生理解 |
| SQLite + FTS5 | 轻量级无需额外进程，全文检索内置支持 |
| OnceLock 全局状态 | 延迟初始化，避免 main() 中传递大量引用 |
| Semaphore 并行控制 | 防止资源耗尽（文件句柄、网络连接） |
| .usage.json sidecar | 技能元数据与内容解耦，避免频繁修改 .md 文件 |

---

*文档生成时间: 2026-06-04 | 基于 RHermes v0.2.0 源码*
