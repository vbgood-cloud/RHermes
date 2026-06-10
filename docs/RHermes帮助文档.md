# RHermes 帮助文档

> **版本**: v0.3.0 | **语言**: Rust 2024 Edition | **协议**: MIT | **源文件**: 54+ (.rs)

---

## 目录

1. [项目概述](#1-项目概述)
2. [快速开始](#2-快速开始)
3. [架构总览](#3-架构总览)
4. [核心模块](#4-核心模块)
5. [Provider 系统（多 Provider + 熔断器）](#5-provider-系统多-provider--熔断器)
6. [MCP 客户端（Model Context Protocol）](#6-mcp-客户端model-context-protocol)
7. [Agent 智能体系统](#7-agent-智能体系统)
8. [工具系统](#8-工具系统)
9. [TUI 终端界面](#9-tui-终端界面)
10. [Channel 多渠道系统](#10-channel-多渠道系统)
11. [Gateway 守护进程](#11-gateway-守护进程)
12. [多搜索引擎系统](#12-多搜索引擎系统)
13. [AgentSession & EventSink](#13-agentsession--eventsink)
14. [配置说明](#14-配置说明)
15. [内置工具参考](#15-内置工具参考)
16. [技能系统](#16-技能系统)
17. [记忆系统](#17-记忆系统)
18. [Tool-Call 修复管线](#18-tool-call-修复管线)
19. [Curator 技能生命周期管理](#19-curator-技能生命周期管理)
20. [开发指南](#20-开发指南)

---

## 1. 项目概述

RHermes 是一个**终端 AI 编码 Agent**，基于 Rust 构建，支持多种 AI Provider（DeepSeek / OpenAI / Ollama）和多种交互渠道（TUI / 微信 / 企业微信 / Telegram）。提供了完整的 MCP 客户端、多搜索引擎降级、自进化技能系统和三层记忆架构。

### 核心特性

| 特性 | 说明 |
|------|------|
| **Provider Pool** | 多 AI Provider 负载均衡 + 熔断器（Closed/HalfOpen/Open）+ 加权轮询 |
| 三层上下文 + Prefix Cache | Immutable Prefix + Append-Only Log + Volatile Scratch，三层 System Prompt 冻结，优化 DeepSeek 前缀缓存命中 |
| 并行工具调度 | `JoinSet` + `Semaphore` 并发执行，按 `parallel_safe` 标志分组 |
| **MCP 客户端** | 完整 Model Context Protocol，支持 stdio / SSE / 直连 POST 三种传输，远程工具自动注册到 ToolRegistry |
| **多渠道系统** | 统一 `Channel` trait：TUI / 微信个号(iLink Bot) / 企业微信 / Telegram |
| **Gateway 守护进程** | 无头后台模式，`SessionRouter` 管理多会话路由 |
| **多搜索引擎降级** | DuckDuckGo → Serper (Google) → Bing，LRU 缓存 + 1s 速率限制 |
| Tool-Call 修复 | 四道工序：Flatten / Scavenge / Truncation / Storm Suppression |
| 技能进化引擎 | Markdown Playbook + YAML frontmatter + `.usage.json` 遥测 |
| 三层记忆系统 | Session / Working / Long-term，SQLite + FTS5 全文检索 |
| 自治 Curator | 技能生命周期管理（active → stale → archived） |
| **AgentSession** | 提取自理型 Agent Loop，TUI 和 Gateway 共用 |
| **EventSink** | Agent 事件输出抽象（TuiSink / ChannelSink） |

### 新增 CLI 子命令（v0.3.0）

| 命令 | 功能 |
|------|------|
| `rhermes gateway start` | 启动 Gateway 守护进程 |
| `rhermes gateway stop` | 停止 Gateway |
| `rhermes gateway status` | 查看 Gateway 状态 |
| `rhermes gateway setup` | Gateway 配置向导 |
| `rhermes mcp setup` | MCP Server 配置向导 |
| `rhermes mcp list` | 列出已配置的 MCP Server |
| `rhermes mcp import <file>` | 从 JSON 文件导入 MCP Server |
| `rhermes mcp remove <name>` | 移除 MCP Server |

### 技术栈

| 类别 | 技术 |
|------|------|
| 语言 | Rust 2024 Edition |
| 异步运行时 | tokio (full) |
| TUI 框架 | ratatui 0.29 + crossterm |
| HTTP 客户端 | reqwest (json + stream + socks + multipart) |
| 数据库 | rusqlite (bundled + FTS5) |
| 文本搜索 | grep-regex + grep-searcher |
| HTML 解析 | scraper |
| PDF 解析 | pdf-extract |
| MCP 传输 | JSON-RPC over stdio/SSE |
| CLI 解析 | clap derive |
| 交互提示 | dialoguer (ColorfulTheme) |
| 二维码 | qrcodegen |
| 缓存 | lru |
| 序列化 | serde / toml / json / base64 |

---

## 2. 快速开始

### 安装构建

```bash
git clone <repo-url>
cd RHermes
cargo build --release
```

### 首次初始化

```bash
# TUI 模式初始化
./target/release/rhermes init

# Gateway 模式配置向导
./target/release/rhermes gateway setup

# MCP Server 配置向导
./target/release/rhermes mcp setup
```

### 日常使用

```bash
# TUI 编码会话
rhermes code
rhermes code --resume

# Gateway 守护进程（后台运行）
rhermes gateway start
rhermes gateway stop
rhermes gateway status
rhermes gateway channel list

# MCP 管理
rhermes mcp list
rhermes mcp import my-servers.json
```

---

## 3. 架构总览

```
┌────────────────────────────────────────────────────────────────┐
│                     main.rs (CLI 入口)                          │
│  clap → code / init / gateway / mcp / config 子命令             │
│  初始化链 → TUI App 或 Gateway Daemon                          │
└───────────────────────┬────────────────────────────────────────┘
                        │
      ┌─────────────────┼───────────────────────┐
      ▼                 ▼                       ▼
┌───────────┐   ┌──────────────┐   ┌──────────────────────┐
│  Channel  │   │   Gateway    │   │       TUI            │
│  Manager  │   │  Daemon      │   │  ratatui + crossterm │
│  ┌──────┐ │   │  start/stop  │   │  Markdown renderer   │
│  │WeChat│ │   │  session rtr │   │  QR code display     │
│  │WeCom │ │   └──────────────┘   │  TuiChannel adapter  │
│  │  TG  │ │                      └──────────┬───────────┘
│  │ TUI  │ │                                 │
│  └──────┘ │                                 ▼
└─────┬─────┘                        ┌──────────────────┐
      │                              │   AgentSession   │
      ▼                              │  (Agent Loop核心) │
┌───────────────┐                    │  Context管理      │
│    EventSink  │◄───────────────────│  Tool调度         │
│  TuiSink      │                    │  压缩/记忆/技能    │
│  ChannelSink  │                    └────────┬─────────┘
└───────────────┘                             │
                                              ▼
┌──────────────────────────────────────────────────────────────┐
│                     Provider Pool                              │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐              │
│  │ Transport  │  │ Transport  │  │ Transport  │              │
│  │ (DeepSeek) │  │  (OpenAI)  │  │ (Ollama)   │              │
│  └─────┬──────┘  └─────┬──────┘  └─────┬──────┘              │
│        │               │               │                      │
│  ┌─────┴─────┐   ┌─────┴─────┐   ┌─────┴─────┐               │
│  │CircuitBrkr│   │CircuitBrkr│   │CircuitBrkr│               │
│  └───────────┘   └───────────┘   └───────────┘               │
└──────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
        ┌──────────┐   ┌──────────┐   ┌──────────┐
        │  MCP     │   │  Core    │   │  Agent   │
        │  Client  │   │ Config   │   │ Memory   │
        │  Remote  │   │ Context  │   │ Skill    │
        │  Tools   │   │ PrefixC  │   │ Repair   │
        │  Adapter │   │ Archive  │   │ Curator  │
        └──────────┘   └──────────┘   │ Session  │
                                      │ Router   │
                                      └──────────┘
```

### 启动初始化链（v0.3.0 更新）

```
PathManager::detect()
    ↓
Config::load()                    ← config.toml + .env
    ↓
ToolRegistry::full_registry()     ← builtin + MCP 远程工具
    ↓
MemorySystem::open()              ← SQLite
    ↓
SkillEngine::load()               ← skills/*.md
    ↓
SessionDebug::new()               ← 调试系统
    ↓
Curator::new().run()              ← 技能生命周期（7 天间隔）
    ↓
ChannelManager 初始化              ← TuiChannel + WeCom + WeChat + Telegram
    ↓
ProviderFactory::create_main_transport()  ← Provider Pool
    ↓
[Gateway 模式]                    [TUI 模式]
SessionRouter 监听 Channel        App::run() TUI 主循环
```

---

## 4. 核心模块

### 4.1 配置管理 (`core/config.rs`)

配置分离为两部分：
- **`config.toml`** — 非敏感配置
- **`.env`** — API Key 和敏感 Secret

#### Config 结构（v0.3.0 扩展）

| 字段 | 说明 |
|------|------|
| `api` | 通用 API 配置（向后兼容，等价于 providers.deepseek） |
| `providers` | 多 Provider 配置表（deepseek/openai/zhipu/ollama 等） |
| `request` | 请求配置（max_rounds, compress_ratio, max_retries） |
| `memory` | 记忆系统配置 |
| `debug` | 调试开关 |
| `display` | 显示与截断配置 |
| `agent` | Agent 行为参数 |
| `provider_pool` | Provider Pool 熔断器参数 |
| `channels` | 多通道配置（wecom/wechat/telegram） |
| `proxy` | 网络代理配置（all/off/auto 模式 + 功能级开关） |
| `gateway` | Gateway 守护进程配置 |
| `mcp` | MCP 客户端配置 |
| `search` | 搜索引擎配置 |

#### Proxy Config（新增）

```toml
[proxy]
mode = "auto"              # all / off / auto
url = "http://127.0.0.1:7890"
no_proxy = ["localhost", "127.0.0.1"]

[proxy.rules]
llm = true                 # AI API 走代理
web_search = true          # 网络搜索走代理
wechat = false             # 微信 Bot 直连
```

### 4.2 Prefix Cache 管理 (`core/prefix_cache.rs`)

专为 DeepSeek prefix caching 设计的三层 System Prompt 构建器：

```
Layer 1 (Stable): 身份 / 规则     ← session 内固定不变
Layer 2 (Context): USER.md + MEMORY.md  ← 可跨 session 更新
Layer 3 (Volatile): 时间 / 画像 / AGENTS.md  ← session 内不变
```

`freeze()` 将三层合并序列化为 JSON，存入 `Arc<[u8]>`，冻结后修改不影响已冻结副本。

### 4.3 上下文归档 (`core/archive.rs`)

压缩前后的消息摘要按日期分片写入 `compressions/YYYY-MM-DD.jsonl`，字段包括 timestamp / session_id / round / msg_count / before_token_est / summary。

### 4.4 上下文压缩 (`core/context.rs`)

- 六段摘要结构：**Goal / Decisions / Files / Commands / Errors / Pending**
- 触发条件：超过 128K 上下文的 80%（v0.3.0 更新为 128K）
- 尾部预算：约 16K tokens

### 4.5 HTTP 客户端工厂 (`core/http_client.rs`)

`create_proxied_client()` 统一管理代理配置，所有 HTTP 调用（LLM / 搜索 / 微信 / Telegram）共用此工厂。

---

## 5. Provider 系统（多 Provider + 熔断器）

### 5.1 Transport Trait (`provider/transport.rs`)

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError>;
    async fn chat_stream(&self, request: ChatRequest, tx: UnboundedSender<ApiEvent>) -> Result<(), ApiError>;
    async fn get_balance(&self) -> Result<f64, ApiError>;
    fn model_name(&self) -> &str;
}
```

`DeepSeekTransport` 是 OpenAI 兼容的 HTTP 实现，所有 OpenAI 兼容 Provider 通用。

### 5.2 Provider Pool (`provider/pool.rs`)

#### CircuitBreaker（熔断器）

三态模型：
```
         连续失败 N 次
Closed ──────────────→ Open
  ↑                      │
  │        冷却后         │
  └────── HalfOpen ──────┘
           试探请求
```

- 所有状态：`AtomicU8/AtomicU32/AtomicU64` 无锁并发
- 半开恢复：冷却时间过后自动转 HalfOpen，允许一次试探
- 非重试错误（401/400）：立即返回，不浪费重试次数

#### 加权轮询

`next_healthy()` 跳过不健康的 Provider，按顺序选择下一个。

#### 退避重试

最多 3 次重试，间隔 500ms * 2^n。

### 5.3 Provider 工厂 (`provider/factory.rs`)

`infer_provider_from_model(model)` 从模型名前缀自动推断 Provider：

| 前缀 | Provider | 默认 base_url |
|------|----------|---------------|
| `deepseek` | deepseek | `https://api.deepseek.com` |
| `gpt-` | openai | `https://api.openai.com` |
| `glm-` | zhipu | `https://open.bigmodel.cn` |
| `qwen` | qwen | `https://dashscope.aliyuncs.com` |
| `moonshot` | moonshot | `https://api.moonshot.cn` |
| ollama | ollama | `http://localhost:11434`（无需 API Key） |

`create_main_transport()` 创建主对话 Transport，自动包装到 `ProviderPool`。

---

## 6. MCP 客户端（Model Context Protocol）

### 6.1 架构

```
ToolRegistry (RHermes)
    │
    ├── builtin: ReadFile, WriteFile, RunCommand, ...
    └── mcp__*: McpRemoteTool (动态注册)
           │
           └── McpAdapterManager
                   ├── McpAdapter (server-1)
                   │     └── McpTransportWrapper
                   │           ├── McpTransport (stdio)
                   │           ├── McpSseTransport (SSE)
                   │           └── McpDirectTransport (POST)
                   └── McpAdapter (server-2)
                         └── ...
```

### 6.2 三种传输模式

| 模式 | 底层 | 适用场景 |
|------|------|----------|
| **Stdio** | JSON-RPC over stdin/stdout | 本地 MCP Server（如 filesystem, github） |
| **SSE** | HTTP GET 长连接 + POST 消息 | 标准远程 MCP Server |
| **直连 POST** | HTTP POST 同步请求 | 非标准兼容 Server |

### 6.3 McpAdapter 核心能力

| 方法 | 说明 |
|------|------|
| `connect()` | 完整握手：initialize → initialized 通知 → tools/list |
| `call_tool()` | 调用远程工具，I/O 错误自动重连一次 |
| `reconnect()` | 清理旧连接 → 重连 → 重新握手 |
| `health_check()` | 10 秒超时调用 tools/list 检测健康 |
| `cancel_request()` | 发送 `notifications/cancelled` |
| `set_log_level()` | 设置 Server 日志级别 |

### 6.4 McpRemoteTool（Tool trait 适配器）

- 命名规则：`mcp__{server_name}__{tool_name}`
- 自动从 JSON Schema 转换为 `Vec<ParamDef>`
- `parallel_safe` 优先取 per-tool 配置，否则用 Server 级别配置

### 6.5 传输层细节

#### Stdio 传输
- `AtomicU64` 唯一请求 ID 生成
- `HashMap<u64, oneshot::Sender>` 异步路由响应
- 子进程 stdout/stderr 独立后台 task 处理
- Windows 支持：`creation_flags(0x08000000)` 隐藏控制台窗口
- Drop 自动 `start_kill()` 子进程

#### SSE 传输
- GET 建立 SSE 长连接，从 endpoint 事件提取 `message_url`
- POST 到 message_url 发送 JSON-RPC 请求
- 收到非 200 自动 fallback 到直连 POST 模式

### 6.6 MCP 配置导入

```bash
# 从 JSON 文件导入
rhermes mcp import servers.json

# 交互式导入（打开编辑器粘贴 JSON）
rhermes mcp import -i
```

---

## 7. Agent 智能体系统

### 7.1 记忆系统 (`agent/memory.rs`)

三层架构保持不变（Session / Working / LongTerm），SQLite + FTS5 后端。详见 [§17 记忆系统](#17-记忆系统)。

### 7.2 技能引擎 (`agent/skill.rs`)

Markdown Playbook + `.usage.json` 遥测。详见 [§16 技能系统](#16-技能系统)。

### 7.3 子 Agent 系统 (`agent/task.rs`)

`run_sub_agent(task)` 创建独立 API 调用，`auto_refine_skill` / `auto_refine_memory` 在 AgentSession 中通过 `tokio::spawn` 后台执行。

### 7.4 AgentSession (`agent/session.rs`)

详见 [§13 AgentSession & EventSink](#13-agentsession--eventsink)。

### 7.5 SessionRouter (`agent/router.rs`)

按 `channel:chat_id` 管理多 AgentSession，Gateway 模式下使用。

### 7.6 EventSink (`agent/event_sink.rs`)

详见 [§13 AgentSession & EventSink](#13-agentsession--eventsink)。

### 7.7 记忆编排层 (`agent/memory_manager.rs`)

Provider trait 接口：prefetch / inject / intercept / sync / flush。预留外部记忆系统支持（honcho / mem0 / memgpt）。

---

## 8. 工具系统

工具系统核心架构不变：`Tool` trait → `ToolRegistry` → `ToolDispatcher`。

### 新增：MCP 远程工具集成

`full_registry()` 在 builtin_registry 基础上注入所有 MCP 远程工具，命名格式 `mcp__{server}__{tool}`。

### 并行调度器

不变：连续的 `parallel_safe=true` 工具用 `JoinSet` 并发，`parallel_safe=false` 的等当前组完成再串行。

---

## 9. TUI 终端界面 (`tui/mod.rs` + 新增文件)

### 新增组件

| 文件 | 功能 |
|------|------|
| `tui/markdown.rs` | 轻量 Markdown → ratatui Line 渲染器（粗体/代码块/标题/引用/列表/分隔线） |
| `tui/qrcode.rs` | ASCII 二维码渲染器（双宽字符保持正方形比例） |
| `tui/channel.rs` | TuiChannel 适配器，将 TUI 包装为 `Channel` trait 实现 |

### TuiChannel

实现 `Channel` trait，`start()` 保持 pending（UI 循环由外部驱动），使得 TUI 可以无缝接入统一的 Channel 路由系统。

---

## 10. Channel 多渠道系统

### 10.1 Channel Trait

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn start(self: Arc<Self>, inbound_tx, shutdown_rx) -> JoinHandle<()>;
    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String>;
    fn name(&self) -> &'static str;
    async fn login_qrcode(&self) -> Option<(String, Vec<u8>)>;
}
```

### 10.2 ChannelManager

统一管理所有 Channel 的生命周期：
- `register()` 注册
- `start_all()` 启动所有 Channel 的 tokio task
- `take_inbound_rx()` 取出汇聚的消息接收端
- `broadcast()` 向所有 Channel 发送
- `get(name)` 按名定向发送
- `shutdown()` 通过 broadcast channel 通知停止

### 10.3 微信个号 Channel (`channel/wechat/`)

最复杂的 Channel 实现（828 行）：

- **扫码登录**：`fetch_qrcode()` → 轮询 `poll_login_status()` → `save_token()`
- **Token 持久化**：文件缓存 bot_token 避免重复扫码
- **消息同步**：`get_updates_buf` 增量同步
- **长消息分片**：UTF-8 字符边界安全切分
- **Token 过期自动重登**：检测 API 错误码 -1005
- **内置 BMP QR 码生成**：`qrcodegen` 编码 + 手动 BMP 文件结构

### 10.4 企业微信 Channel (`channel/wecom/`)

双模式：Webhook 发送 + 应用 API 轮询接收。`access_token` 缓存（7000s 自动刷新）。

### 10.5 Telegram Channel (`channel/telegram/`)

Long Polling 接收，`allowed_chats` 白名单过滤，长消息 4000 字符边界分片。

---

## 11. Gateway 守护进程

### 架构

```
Gateway Daemon
├── PID 文件管理（Windows: tasklist 验证）
├── Channel 系统初始化（wecom/wechat/telegram）
├── SessionRouter（按 channel:chat_id 路由）
└── AgentSession（同 TUI 模式）
```

### 子命令

| 命令 | 功能 |
|------|------|
| `gateway start` | 启动守护进程 |
| `gateway stop` | 停止（taskkill） |
| `gateway status` | 显示进程状态和通道启用情况 |
| `gateway setup` | 3 步交互式配置向导 |
| `gateway channel list` | 列出所有通道 |
| `gateway channel enable <name>` | 启用通道 |
| `gateway channel disable <name>` | 禁用通道 |

### 启动流程

1. PID 检查（避免重复启动）
2. 初始化公共组件（Memory, Skill, Config, Transport...）
3. 按配置注册 Channel
4. 启动 Channel + 扫码登录（显示 ASCII QR）
5. SessionRouter 管理多会话
6. 轮询 inbound_rx → `router.dispatch()`
7. 退出前关闭 MCP、清理 PID

---

## 12. 多搜索引擎系统

### 引擎降级链

```
用户查询
    ↓
LRU 缓存命中？──Yes──→ 返回缓存结果
    ↓ No
DuckDuckGo (免费)
    ↓ 失败
Serper (Google API, 需 Key)
    ↓ 失败
Bing HTML (免费备用)
    ↓
返回结果（包裹 <untrusted> 标记）
```

### SearchResult

统一数据结构：`title, url, snippet, source`

### 速率限制

两次请求间隔至少 1 秒，防止被限流。

### DuckDuckGo 解析器

三级降级解析策略：
1. `parse_class_result()` — `.result` 类（旧版稳定结构）
2. `parse_generic_links()` — 含 `uddg=` 的重定向链接
3. `parse_plain_text_fallback()` — 提取所有 http/https URL

内置反爬检测：`is_captcha_page()` 检查 8 种反爬关键词。

---

## 13. AgentSession & EventSink

### AgentSession (`agent/session.rs`)

从 TUI 的 `init_api` 中提取出来的自理型 Agent Loop（413 行）。

**核心流程** (`handle_message()`):
1. 用户消息写入 Context
2. 循环（最多 max_rounds 轮，**上限 200 次工具调用**防死循环）：
   - 每 5 轮展示技能进化建议
   - **128K 上下文**超 80% 触发 LLM 压缩（6 段结构总结 + 归档）
   - 记忆召回（用户消息关键词搜索 SQLite）
   - 调用 Transport.chat()（非流式，120s 超时）
   - 执行工具调用（ToolDispatcher）
   - 最终文本回复 → 记忆写入 + 自动技能/记忆提炼（后台 tokio::spawn）
3. `delegate_task` 工具特殊处理：直接作为最终输出

### EventSink (`agent/event_sink.rs`)

Agent 事件输出抽象，解耦 Agent Loop 与具体消费端：

```rust
pub trait EventSink: Send + Sync {
    fn on_chunk(&self, chunk: &str);
    fn on_tool_calls(&self, calls: &[ToolCallData]);
    fn on_tool_result(&self, tool_name: &str, result: &str);
    fn on_done(&self);
    fn on_error(&self, error: &str);
    fn on_usage(&self, usage: &Usage);
    fn on_balance(&self, balance: f64);
}
```

**两个实现**:

| Sink | 行为 |
|------|------|
| `TuiSink` | 通过 `mpsc::UnboundedSender<ApiEvent>` 推送给 TUI 实时渲染 |
| `ChannelSink` | `on_chunk()` 追加到 buffer，`on_done()` 一次性 flush 发送，避免刷屏 |

---

## 14. 配置说明

### config.toml 完整示例（v0.3.0）

```toml
[api]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"

[providers.deepseek]
api_key_env = "DEEPSEEK_API_KEY"
base_url = "https://api.deepseek.com"
models = ["deepseek-v4-flash", "deepseek-v4-pro"]

[providers.openai]
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"
models = ["gpt-4o", "gpt-4o-mini"]

[providers.ollama]
base_url = "http://localhost:11434/v1"
models = ["qwen2.5:7b"]

[request]
max_rounds = 50
max_retries = 3
compression_ratio = 0.8

[provider_pool]
failure_threshold = 3
cooldown_secs = 30
max_retries = 3

[agent]
tool_result_max_chars = 15000
read_pdf_max_chars = 30000
max_memory_md_chars = 2200

[proxy]
mode = "auto"
url = "http://127.0.0.1:7890"
no_proxy = ["localhost", "127.0.0.1"]

[proxy.rules]
llm = true
web_search = false
wechat = false

[channels.wecom]
enabled = false
corp_id = ""
agent_id = ""
secret_env = "WECOM_SECRET"

[channels.wechat]
enabled = false
token_path = ""
poll_interval = 3

[channels.telegram]
enabled = false
bot_token_env = "TG_BOT_TOKEN"
allowed_chats = []

[gateway]
pid_file = "rhermes-gateway.pid"
log_file = "rhermes-gateway.log"

[mcp]
enabled = true

[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed"]
auto_start = true

[[mcp.servers]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_xxx" }

[search]
serper_api_key_env = "SERPER_API_KEY"
cache_ttl_secs = 300
rate_limit_secs = 1
```

---

## 15. 内置工具参考

内置工具仍为 17 个（同 v0.2.0），但 v0.3.0 新增了 **MCP 远程工具**动态注入：

| 类别 | 工具 |
|------|------|
| 可并行 | `read_file`, `search_content`, `glob`, `get_current_time`, `skill_list`, `skill_search`, `read_pdf` |
| 串行 | `write_file`, `run_command`, `web_search`, `web_fetch`, `delegate_task`, `run_skill`, `skill_create`, `skill_patch`, `skill_manage`, `memory` |

### web_search 引擎选择

支持通过 `search` 配置项选择搜索引擎。DuckDuckGo 免费，Serper 需要 API Key，Bing 免费备用。所有结果包裹 `<untrusted>...</untrusted>` 标记。

---

## 16. 技能系统

同 v0.2.0，技能 Markdown + YAML frontmatter + `.usage.json` 遥测。

---

## 17. 记忆系统

同 v0.2.0，三层架构 + SQLite + FTS5 + USER.md / MEMORY.md 导出。

---

## 18. Tool-Call 修复管线

四道工序不变：Flatten → Scavenge → Truncation → Storm Suppression。

---

## 19. Curator 技能生命周期管理

状态机不变：active → stale (30天) → archived (90天)。

---

## 20. 开发指南

### 项目结构（v0.3.0 更新）

```
RHermes/
├── Cargo.toml
├── src/
│   ├── main.rs                # CLI 入口 (609 行, 含 gateway/mcp/config 子命令)
│   ├── init.rs                # 初始化向导
│   ├── core/ (8 文件)
│   │   ├── config.rs          # 配置 (含 proxy/channels/gateway/mcp/search)
│   │   ├── context.rs         # 三层上下文
│   │   ├── prefix_cache.rs    # Prefix Cache 管理器 (新)
│   │   ├── archive.rs         # 上下文压缩归档 (新)
│   │   ├── http_client.rs     # HTTP 客户端工厂 (新)
│   │   └── path.rs
│   ├── provider/ (4 文件, 新)
│   │   ├── transport.rs       # Transport trait + DeepSeekTransport
│   │   ├── pool.rs            # ProviderPool + CircuitBreaker
│   │   └── factory.rs         # Provider 工厂
│   ├── mcp/ (8 文件, 新)
│   │   ├── mod.rs, config.rs, tool_wrapper.rs
│   │   ├── adapter.rs         # McpAdapter 连接管理
│   │   ├── transport.rs       # stdio JSON-RPC
│   │   ├── sse_transport.rs   # SSE + 直连 POST
│   │   ├── import.rs          # JSON 导入
│   │   └── setup.rs           # 交互式配置向导
│   ├── channel/ (7 文件, 新)
│   │   ├── mod.rs, types.rs, manager.rs
│   │   ├── wechat/mod.rs      # 微信个号 (828 行)
│   │   ├── wecom/mod.rs       # 企业微信
│   │   └── telegram/ (mod.rs, api.rs)
│   ├── gateway/ (2 文件, 新)
│   │   ├── mod.rs             # Gateway 主逻辑
│   │   └── setup.rs           # Gateway 配置向导
│   ├── agent/ (9 文件)
│   │   ├── session.rs         # AgentSession (新, 提取的 Agent Loop)
│   │   ├── router.rs          # SessionRouter (新)
│   │   ├── event_sink.rs      # EventSink trait (新)
│   │   ├── memory.rs, memory_manager.rs
│   │   ├── skill.rs, task.rs, repair.rs, curator.rs
│   ├── tools/ (8 文件)
│   │   ├── registry.rs, builtin.rs, dispatcher.rs
│   │   └── search/ (4 文件)
│   │       ├── mod.rs         # MultiEngineSearcher
│   │       ├── duckduckgo.rs  # DDG HTML 解析
│   │       ├── serper.rs      # Google Search API
│   │       └── bing.rs        # Bing HTML 解析
│   ├── api/ (1 文件)
│   │   └── mod.rs             # 请求/响应类型定义
│   ├── tui/ (4 文件)
│   │   ├── mod.rs             # TUI 主界面
│   │   ├── markdown.rs        # Markdown 渲染 (新)
│   │   ├── qrcode.rs          # QR 码渲染 (新)
│   │   └── channel.rs         # TuiChannel 适配器 (新)
│   ├── cost.rs                # 成本控制（待集成）
│   └── debug.rs               # 调试系统
└── docs/
    ├── RHermes帮助文档.md
    ├── code-review-report.md
    ├── CHANGELOG.md
    └── plans.md
```

### 新增依赖

| 依赖 | 用途 |
|------|------|
| `scraper` | DuckDuckGo/Bing HTML 解析 |
| `lru` | 搜索缓存 LRU 淘汰 |
| `qrcodegen` | ASCII/BMP 二维码生成 |
| `base64` | 微信 token/图片编码 |
| `rand` | 微信 UIN/ClientID 生成 |
| `futures-util` | SSE 流解析 |

### 与 v0.2.0 的主要变化

| 变化 | 说明 |
|------|------|
| API 层重命名 | `api/mod.rs` 中的函数逻辑迁至 `provider/transport.rs` |
| Agent Loop 提取 | 从 `tui/mod.rs` 内联 ~600 行提取为 `agent/session.rs` |
| 多 Provider 支持 | 新增 `provider/pool.rs` + `provider/factory.rs` |
| MCP 客户端 | 全新 `mcp/` 模块（8 文件） |
| 多渠道系统 | 全新 `channel/` 模块（7 文件） |
| Gateway 模式 | 全新 `gateway/` 模块（2 文件） |
| 搜索引擎重构 | 从单一 DDG 到 `tools/search/` 多引擎降级 |
| Prefix Cache | 全新 `core/prefix_cache.rs` |
| 上下文归档 | 全新 `core/archive.rs` |
| TUI 增强 | Markdown 渲染、QR 码显示 |

---

*文档生成时间: 2026-06-09 | 基于 RHermes v0.3.0 源码*
