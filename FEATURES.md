# RHermes 功能清单

> RHermes = Reasonix x Hermes：Rust 版自进化终端 AI 编程 Agent
> 融合极致 Token 缓存优化与自进化学习闭环

---

## 1. 智能体核心 (agent/)

### 1.1 AgentSession — 主 Agent 循环
- **描述**：提取自理型 Agent Loop，管理三段式 Context、工具调度、LLM 通信的完整会话生命周期
- **源文件**：`src/agent/session.rs`
- **关键功能**：
  - `handle_message()` 单入口处理外部消息（TUI/微信/企微等共享同一逻辑）
  - 最大轮次控制（防死循环）
  - 自适应压缩（达到 80% token 阈值自动压缩历史）
  - 工具调用护栏重试（最大 3 次）
  - 教育模式：工具白名单 + 学习模式切换（explore/scaffold/locked）
  - 教育反思闭环：自动生成反思提示、评分并落库

### 1.2 MemorySystem — 三层长期记忆系统
- **描述**：基于 SQLite + FTS5 全文索引的三层记忆架构
- **源文件**：`src/agent/memory.rs`
- **功能点**：
  - **Session Memory** — 当前会话上下文（自动管理）
  - **Working Memory** — 跨会话活跃知识（当前项目相关）
  - **Long-term Memory** — 持久化知识库（永久保存）
  - FTS5 全文索引搜索（`search_memories()`）
  - 记忆条目 CRUD（创建/读取/更新/删除）
  - Nudge 机制（定期提示用户查看和管理记忆）
  - 最近记忆统计（`recent_memories()`）
  - 支持 tag 标签分类和按项目过滤

### 1.3 MemoryManager — 记忆编排层
- **描述**：MemoryProvider trait + 内置+外部 Provider 路由
- **源文件**：`src/agent/memory_manager.rs`
- **功能点**：
  - `MemoryProvider` trait 定义 5 个生命周期方法：`prefetch`/`inject`/`intercept`/`sync`/`flush`
  - `BuiltinProvider` — 内置 SQLite+FTS5 + MEMORY.md + USER.md
  - 外部 Provider 接口（支持 honcho/mem0 等第三方记忆系统）
  - 记忆工具 schema 合并
  - Provider 生命周期钩子广播

### 1.4 SkillEngine — 技能引擎
- **描述**：基于 Markdown playbook 的可复用技能引擎，兼容 agentskills.io 格式
- **源文件**：`src/agent/skill.rs`
- **功能点**：
  - Markdown 格式技能定义（含 YAML frontmatter）
  - inline / subagent 两种运行模式
  - 使用次数、成功率、平均耗时统计
  - 进化建议：基于使用数据自动优化
  - UsageTelemetry sidecar（`.usage.json` 与 `.md` 同目录）
  - 技能创建、更新、搜索、列举
  - 技能补丁/维护功能
  - SkillManager：CRUD + 运行时管理
  - 内置 4 种技能分类：explore / research / review / security-review

### 1.5 Curator — 自治技能生命周期管理
- **描述**：自动管理技能状态生命周期
- **源文件**：`src/agent/curator.rs`
- **功能点**：
  - 三态状态机：`active → stale(30天) → archived(90天)`
  - 启动时自动检查和执行
  - LLM Review Pass：扫描同前缀/同域技能群，合并到 umbrella skill
  - CuratorMarker 防止重复执行
  - 归档目录 (`_archived/`)
  - Pinned 技能保护（防止 curator 误触）

### 1.6 Repair Pipeline — Tool-Call 修复流水线
- **描述**：四道工序修复 DeepSeek 模型在 tool-call 上的常见问题
- **源文件**：`src/agent/repair.rs`
- **功能点**：
  - **Flatten** — 发送前参数压平为 dot-notation，收到后自动还原
  - **Scavenge** — 从 `reasoning_content` 捞取模型忘记发出的 tool-call
  - **Truncation** — 检测并补全截断的 JSON
  - **Storm** — 抑制相同 (tool, args) 的重复调用（时间窗口+重复计数）

### 1.7 Guardrails — 护栏系统
- **描述**：响应验证器 + 纠正消息构建器
- **源文件**：`src/agent/guardrails.rs`
- **功能点**：
  - `ResponseValidator` — 校验工具名是否存在
  - 必填参数完整性检查
  - 校验失败时构建纠正消息注入 Context
  - 支持多轮自纠正（重试机制）

### 1.8 SessionRouter — 多会话路由
- **描述**：按 `channel:chat_id` 分配和管理多个 AgentSession
- **源文件**：`src/agent/router.rs`
- **功能点**：
  - 多用户会话隔离（每个 `channel:chat_id` 独立会话）
  - 教育课程上下文追踪（`course_contexts` / `course_profiles` / `current_course`）
  - 学生认证身份管理
  - 向导状态管理
  - 教育模式角色："teacher" / "student" / ""

### 1.9 SubAgent — 子 Agent 系统
- **描述**：主 Agent 将子任务委托给独立的子 Agent 执行
- **源文件**：`src/agent/task.rs`
- **功能点**：
  - 隔离的 tokio task 中运行
  - 并行调研 / 深度分析 / 嵌套任务
  - 共享 ProviderPool
  - 递归深度限制控制

### 1.10 EventSink — 事件抽象层
- **描述**：Agent Loop 事件输出与消费端解耦
- **源文件**：`src/agent/event_sink.rs`
- **功能点**：
  - `TuiSink` — 通过 mpsc channel 推送给 TUI 渲染
  - `ChannelSink` — 通过 ChannelManager 发送到微信/企微等外部通道
  - 事件类型：chunk / tool_calls / tool_result / done / error / usage / balance / typing

---

## 2. 上下文管理系统 (core/)

### 2.1 三段式 Context 架构
- **描述**：省 Token 核心设计，围绕 DeepSeek prefix cache 的 byte 级稳定性需求构建
- **源文件**：`src/core/context.rs`
- **功能点**：
  - **Immutable Prefix** — session 内固定不变（system + tool_specs + few_shots），命中前缀缓存
  - **Append-Only Log** — 对话历史单调递增，不重写已有条目
  - **Volatile Scratch** — 每轮重置（思考/计划/临时状态，不发送到上游）
  - 三条不变式：Prefix 一次计算 / Log 只追加 / Scratch 蒸馏后才能进入 Log
  - 压缩机制：达到 80% token 阈值时自动将中间历史压缩为结构化摘要（6 段式：Goal/Decisions/Files/Commands/Errors/Pending）
  - 热更新系统提示（edu 学习模式切换时重建 prefix）
  - Context 窗口统计（prefix_len / log_len / scratch_count）
  - `build_request_body()` 构建 API 请求 JSON

### 2.2 PrefixCacheManager — 三层前缀缓存
- **描述**：构建三层 system prompt 前缀，用于 DeepSeek prefix cache 命中优化
- **源文件**：`src/core/prefix_cache.rs`
- **功能点**：
  - Layer 1 (Stable)：身份/规则/记忆指引 — session 内固定
  - Layer 2 (Context)：USER.md + MEMORY.md 内容 — 可跨 session 更新
  - Layer 3 (Volatile)：时间/画像摘要/AGENTS.md — session 内不变
  - 冻结为 `Arc<[u8]>` 确保 byte 级稳定性
  - `full_system_prompt()` 获取三层合并文本

### 2.3 Config — 配置分离加载
- **描述**：TOML + .env 双层配置系统
- **源文件**：`src/core/config.rs`
- **功能点**：
  - `config.toml` — 非敏感配置（模型/地址/超时/通道/MCP/搜索/代理等）
  - `.env` — 敏感配置（API Key / Bot Token / Secret）
  - 所有配置结构：API、Provider、请求超时、记忆、调试、显示、Agent行为、熔断器、通道(企业微信/微信/Telegram/QQ)、代理、Gateway、定时任务、WASM插件、MCP客户端、搜索引擎、教育模式、LiteParse
  - 生成带注释的配置模板
  - API Key 写入 `.env`，永不落入 `config.toml`
  - 自动检测配置文件不存在时返回默认值

### 2.4 PathManager — 可移动模式路径管理
- **描述**：所有文件系统操作的唯一路径来源
- **源文件**：`src/core/path.rs`
- **功能点**：
  - 自动检测可执行文件路径，取 `exe_dir/home/` 作为数据根目录
  - 子路径：config / memory.db / skills / sessions / logs / cache / workspace
  - `ensure_dirs()` 惰性创建标准目录结构
  - 支持 U盘/云同步/Docker volume/CI-CD 等可移动部署场景

### 2.5 HttpClientFactory — 代理感知 HTTP 客户端
- **描述**：根据代理配置统一创建 reqwest::Client
- **源文件**：`src/core/http_client.rs`
- **功能点**：
  - 三模式代理（all/off/auto）
  - `no_proxy` 域名排除列表
  - 按功能名（llm/web_search/web_fetch/wechat/wecom/telegram/command）控制是否走代理
  - 请求超时配置

### 2.6 压缩归档
- **描述**：Context 压缩前的完整消息序列保存到 .jsonl 文件
- **源文件**：`src/core/archive.rs`
- **功能点**：
  - 按日期分片归档到 `compressions/YYYY-MM-DD.jsonl`
  - 记录：时间戳、session ID、round、消息数、token 估算、摘要
  - 摘要截断 500 字符

---

## 3. 工具系统 (tools/)

### 3.1 ToolRegistry — 工具注册表
- **描述**：统一的工具注册、查找和参数定义机制
- **源文件**：`src/tools/registry.rs`
- **功能点**：
  - `Tool` trait 定义（`execute` / `parallel_safe` / `name` / `description` / `parameters` / `required_params`）
  - 按名称注册/查找工具
  - 生成 OpenAI 兼容格式的工具定义
  - MCP 远程工具无缝融入 (`mcp__` 前缀)
  - 参数定义模型（`ParamDef` / `ParamType` 枚举：String/Integer/Float/Boolean/Array/Object）

### 3.2 ToolDispatcher — 并行调度器
- **描述**：并行/串行工具调度引擎
- **源文件**：`src/tools/dispatcher.rs`
- **功能点**：
  - `JoinSet` 并发调度标记为 `parallel_safe` 的工具
  - 串行调度非 parallel_safe 的工具
  - 粘性参数注入
  - 全局状态初始化（workspace / config / skill_engine / 等）
  - 调用超时控制（全局 120s 超时）
  - 全局工具定义获取（`all_tool_defs()`）

### 3.3 内置工具 — 25 个工具（+MCP 动态 +Wasm 插件）

| 工具名 | 描述 | 源文件 |
|--------|------|--------|
| `read_file` | 读取文件（支持 head/tail/range） | `src/tools/builtin.rs` |
| `write_file` | 写入文件（创建或覆盖，自动创建父目录） | `src/tools/builtin.rs` |
| `search_content` | 搜索文件内容（正则表达式，支持目录递归） | `src/tools/builtin.rs` |
| `glob` | 使用 glob 模式查找文件 | `src/tools/builtin.rs` |
| `run_command` | 执行 shell 命令（受沙箱限制，危险命令检测黑名单 70+ 模式） | `src/tools/builtin.rs` |
| `get_current_time` | 获取当前时间（可指定时区） | `src/tools/builtin.rs` |
| `web_search` | 搜索引擎搜索（多引擎：Bing/DuckDuckGo/SearXNG/Serper/百度） | `src/tools/builtin.rs` |
| `web_fetch` | 获取网页 HTML 内容并提取文本 | `src/tools/builtin.rs` |
| `delegate_task` | 子任务委托（调用子 Agent 系统） | `src/tools/builtin.rs` |
| `run_skill` | 执行已注册的技能（inline/subagent 模式） | `src/tools/builtin.rs` |
| `skill_list` | 列出所有可用技能（支持 tag 过滤） | `src/tools/builtin.rs` |
| `skill_search` | 搜索技能（按名称/描述） | `src/tools/builtin.rs` |
| `skill_create` | 创建新技能（从对话提炼或手动编写） | `src/tools/builtin.rs` |
| `skill_patch` | 修改/优化已有技能 | `src/tools/builtin.rs` |
| `skill_manage` | 管理技能（归档/还原/删除/移动/启用/禁用） | `src/tools/builtin.rs` |
| `memory` | 长期记忆系统（搜索/注入/写入/提取/备忘/列表） | `src/tools/builtin.rs` |
| `read_pdf` | PDF 文件读取 | `src/tools/builtin.rs` |
| `read_excel` | 读取 Excel (.xlsx) 文件（calamine） | `src/tools/office/excel.rs` |
| `write_excel` | 写入 Excel (.xlsx) 文件（rust_xlsxwriter） | `src/tools/office/excel.rs` |
| `read_docx` | 读取 Word (.docx) 文件（docx-rs） | `src/tools/office/word.rs` |
| `write_docx` | 写入 Word (.docx) 文件（docx-rs） | `src/tools/office/word.rs` |
| `read_pptx` | 读取 PowerPoint (.pptx) 文件（zip+quick-xml） | `src/tools/office/pptx.rs` |
| `parse_document` | LiteParse 文档解析（PDF/DOCX/图片 → 文本/Markdown） | `src/tools/liteparse.rs` |
| `screenshot_document` | 文档页面截图 | `src/tools/liteparse.rs` |
| `check_document_complexity` | 判断文档是否需要 OCR | `src/tools/liteparse.rs` |
| `run_plugin` | 调用已加载插件（`__list__` 列出；Wasm 沙盒/SKILL.md 技能） | `src/tools/builtin.rs` |

### 3.4 文档解析（LiteParse）
- **描述**：基于 liteparse Rust crate 的文档解析工具
- **源文件**：`src/tools/liteparse.rs`
- **功能点**：
  - `parse_document` — 解析文档提取文本/Markdown/JSON
  - `screenshot_document` — 生成文档页面截图
  - `check_document_complexity` — 判断文档是否需要 OCR
  - 支持格式：PDF / DOCX / XLSX / PPTX / 图片（PNG/JPEG）
  - 配置：OCR 语言、输出格式、最大页数、渲染 DPI
  - 支持本地 Tesseract 或远程 OCR 服务

### 3.5 搜索引擎
- **描述**：统一的搜索抽象层 + 多引擎降级
- **源文件**：`src/tools/search/mod.rs`
- **功能点**：
  - `SearchEngine` trait — 统一搜索接口
  - `SearchCache` — 搜索缓存（TTL 过期）
  - `MultiEngineSearcher` — 多引擎优先级降级
  - 5 个搜索引擎实现：
    - Bing (`src/tools/search/bing.rs`)
    - DuckDuckGo (`src/tools/search/duckduckgo.rs`)
    - SearXNG (`src/tools/search/searxng.rs`)
    - Serper (`src/tools/search/serper.rs`)
    - 百度 (`src/tools/search/baidu.rs`)

### 3.6 WASM 插件系统 + 统一 Plugin 系统（P28，v0.7.0）
- **描述**：Extism Wasm 沙盒插件 + Host Functions 安全网关 + 统一 Plugin trait
- **源文件**：`src/tools/wasm_plugin.rs`、`src/tools/wasm_host_functions.rs`、`src/plugin/`
- **功能点**：
  - `WasmPluginTool` — 加载 WASM 插件作为工具（`info_name` / `info_description` / `info_parameters` / `execute` 约定）
  - **Host Functions 安全网关**（v0.7.0）：`host_log` / `host_http_get` / `host_http_post` / `host_read_file` / `host_write_file` / `host_exec`
  - **每插件权限声明**：`<name>.host.toml`（allowed_hosts / allowed_paths / allow_exec），无声明 = 最小权限纯计算
  - 域名白名单支持 `*` / `*.example.com`；路径白名单 canonicalize 防 `../` 逃逸
  - Manifest 强制内存上限 + 超时；execute 走 `spawn_blocking`
  - **Plugin trait**（P28）：`descriptor()` / `execute()` / `health()` / `reload()`
  - **PluginRouter**：注册/发现/路由，全局 OnceLock 单例
  - `ExtismPlugin` / `SkillMdPlugin` 两种适配器；`plugins/registry.toml` 显式声明（per-plugin 权限）优先，无配置时目录扫描
  - `run_plugin` 内置工具：Agent 直接调用（`__list__` 列出全部）

---

## 4. API 客户端 (api/)

### 4.1 DeepSeek API 客户端
- **描述**：DeepSeek Chat Completion API 客户端
- **源文件**：`src/api/mod.rs`
- **功能点**：
  - `ChatRequest` — 请求体构建（model/messages/stream/max_tokens/temperature/tools）
  - 同步请求 & SSE 流式响应
  - 自动重试（rate limit / 超时 / 网络错误，最多 3 次）
  - Token 用量追踪（`Usage` 结构：prompt/completion/cache 各维度）
  - `ApiEvent` 事件枚举（StreamChunk / ToolCalls / Done / Error / Usage / Balance）
  - Support for cached tokens tracking (cache_hit/miss tokens)

---

## 5. Provider 层 (provider/)

### 5.1 Provider Pool — 多 Provider 熔断与负载均衡
- **描述**：多 Provider 实例的加权轮询和熔断器健康检测
- **源文件**：`src/provider/pool.rs`
- **功能点**：
  - `ProviderPool` — 多 Provider 管理器
  - **加权轮询**调度算法
  - **熔断器**：连续失败阈值熔断，冷却后自动恢复
  - 健康检测：定期 ping 摘除不健康节点
  - Provider 指标统计（请求数/成功率/平均延迟）
  - 支持多 Provider 配置：不同 model/base_url/api_key 组合

### 5.2 Transport trait — 协议抽象
- **描述**：将 API 调用抽象为 Transport trait
- **源文件**：`src/provider/transport.rs`
- **功能点**：
  - `Transport` trait：`send_request`/`send_request_streaming`
  - `DeepSeekTransport` 实现
  - 支持流式和同步请求

### 5.3 Transport 工厂
- **描述**：根据配置创建 Transport 实例
- **源文件**：`src/provider/factory.rs`
- **功能点**：
  - `create_main_transport` — 创建主 Transport
  - 自动配置 Provider 池

---

## 6. 多通道通信 (channel/)

### 6.1 Channel 抽象层
- **描述**：统一的 Channel trait 和消息收发接口
- **源文件**：`src/channel/mod.rs`
- **功能点**：
  - `Channel` trait (connect / disconnect / send / recv / status / name)
  - `ChannelManager` — 多通道管理器（启动/停止/路由）
  - `InboundMessage` — 统一入站消息格式
  - `ChannelStatus` — 通道运行时状态快照（connected/msg_count/error）

### 6.2 TUI 通道
- **描述**：ratatui 终端交互界面
- **源文件**：`src/tui/mod.rs`、`src/tui/channel.rs`
- **功能点**：
  - 双面板布局（输入区 + 输出区）
  - Markdown 渲染（`src/tui/markdown.rs`）
  - 键盘输入：Normal/Insert/Select/Command 模式
  - 命令模式 (`/model`, `/clear`, `/export` 等斜杠命令)
  - 滚屏与分页
  - 流式输出实时渲染
  - QR 码生成（`src/tui/qrcode.rs`）

### 6.3 微信个号通道
- **描述**：通过 iLink Bot API 接入微信个人号
- **源文件**：`src/channel/wechat/mod.rs`
- **功能点**：
  - 扫码登录
  - 消息接收轮询
  - 文本消息发送
  - Token 持久化

### 6.4 企业微信通道
- **描述**：企业微信 Bot 接入
- **源文件**：`src/channel/wecom/mod.rs`
- **功能点**：
  - Webhook 发送消息
  - 接收消息轮询（企业微信回调）
  - 发送者白名单过滤

### 6.5 Telegram Bot 通道
- **描述**：Telegram Bot API 接入
- **源文件**：`src/channel/telegram/mod.rs`、`src/channel/telegram/api.rs`、`src/channel/telegram/sink.rs`
- **功能点**：
  - Long Polling 消息接收
  - 文本/HTML Markdown 消息发送
  - 消息编辑（实时更新流式输出）
  - 打字状态指示（Typing indicator）
  - Chat ID 白名单过滤
  - ChannelEventSink 实现

### 6.6 QQ Bot 通道
- **描述**：QQ 官方 Bot API 接入（沙箱环境支持）
- **源文件**：`src/channel/qq/mod.rs`、`src/channel/qq/api.rs`
- **功能点**：
  - 群消息（GROUP_AT_MESSAGE_CREATE）
  - 私聊消息（C2C_MESSAGE_CREATE）
  - 文本/图片 Markdown 消息发送
  - 沙箱环境支持

### 6.7 Web 通道
- **描述**：Webhook/HTTP 接口通道
- **源文件**：`src/channel/web/mod.rs`

---

## 7. MCP 客户端 (mcp/)

### 7.1 McpAdapter — MCP 协议适配器
- **描述**：MCP (Model Context Protocol) 客户端，将远程 MCP Server 工具包装为本地工具
- **源文件**：`src/mcp/adapter.rs`
- **功能点**：
  - 连接管理：connect / reconnect / shutdown
  - 工具列举与调用
  - `McpAdapterManager` — 多 Server 连接管理器
  - 健康检查
  - 请求取消
  - 日志级别设置

### 7.2 MCP 传输层
- **描述**：SSE 和 stdio 两种传输模式
- **源文件**：`src/mcp/sse_transport.rs`
- **功能点**：
  - `McpSseTransport` — SSE (HTTP 流式) 传输
  - `McpDirectTransport` — Direct HTTP 传输
  - JSON-RPC 请求/响应/通知收发
  - 通知接收通道
  - 请求取消

### 7.3 McpToolWrapper — MCP 工具包装器
- **描述**：将 MCP 远程工具的调用包装为 Tool trait 实现
- **源文件**：`src/mcp/tool_wrapper.rs`
- **功能点**：
  - `mcp__` 前缀命名空间（自动避免工具名冲突）
  - 无缝融入 ToolRegistry + ToolDispatcher

### 7.4 MCP 配置与导入
- **描述**：MCP Server 配置管理
- **源文件**：`src/mcp/config.rs`、`src/mcp/import.rs`、`src/mcp/setup.rs`
- **功能点**：
  - 从 .mcp.json 文件导入 MCP Server 配置
  - 交互式添加 MCP Server 向导
  - 配置持久化

---

## 8. Gateway 守护进程 (gateway/)

- **描述**：无 TUI 的后台运行模式，通过 Channel 系统连接外部通道
- **源文件**：`src/gateway/mod.rs`、`src/gateway/setup.rs`
- **功能点**：
  - `rhermes gateway start` — 启动守护进程
  - `rhermes gateway stop` — 停止守护进程
  - `rhermes gateway status` — 查看运行状态
  - `rhermes gateway channel list` — 列出通道状态
  - `rhermes gateway channel enable <name>` — 启用通道
  - `rhermes gateway channel disable <name>` — 禁用通道
  - `rhermes gateway setup` — 交互式配置
  - PID 文件 + 信号优雅退出

---

## 9. 定时任务调度器 (scheduler/)

- **描述**：Gateway 模式下基于 cron 表达式定时执行 Agent 任务
- **源文件**：`src/scheduler/mod.rs`
- **功能点**：
  - cron 表达式解析（`cron::Schedule`）
  - 定期执行 Agent 任务并收集结果
  - 结果推送到指定 Channel（`channel:chat_id`）
  - 并发限制（Semaphore 控制同时执行数）
  - 任务的启用/禁用配置

---

## 10. 成本控制 (cost/)

- **描述**：五个互补机制控制 Token 花费
- **源文件**：`src/cost.rs`
- **功能点**：
  - **Flash-First 分级** — auto / flash / pro 三级 preset
  - **NEEDS_PRO 自动升级** — 模型自报告，flash 自动切 pro
  - **辅助调用强制 Flash** — 摘要/压缩等辅助操作强制走 flash 模型
  - **轮次自动压缩** — 工具结果 >3000 token 自动摘要
  - **成本仪表盘** — 每轮/累计成本实时显示
  - 模型分级：`Flash` / `Pro`

---

## 11. 调试与日志 (debug/)

- **描述**：调试环缓冲区 + 会话追踪 + 报告导出
- **源文件**：`src/debug.rs`
- **功能点**：
  - `DebugEntry` — 事件循环记录（type/timestamp/round/content）
  - 环形缓冲区（最多 500 条）
  - `SessionDebug` — 会话级调试信息
  - 调试导出：`rhermes debug export <session-id>` 生成 JSON 报告
  - 多路日志输出（同时写控制台 + 文件）

---

## 12. CLI 入口 (main.rs)

- **描述**：clap derive 宏定义的 CLI 入口
- **源文件**：`src/main.rs`
- **子命令**：
  - `rhermes` — TUI 交互模式（`--resume` / `-r` 恢复上次会话）
  - `rhermes init` — 交互式初始化向导
  - `rhermes debug export <session-id>` — 导出调试报告
  - `rhermes gateway <start|stop|status|channel|setup>` — 守护进程管理
  - `rhermes mcp <add|remove|list|import>` — MCP 客户端管理
  - `rhermes config init|check` — 配置管理
  - `rhermes edu student|teacher` — 教育模式

---

## 13. 教育模式 (edu/)

### 13.1 课程管理系统
- **描述**：教育模式中的课程配置与 Profile 管理
- **源文件**：`src/edu/course.rs`
- **功能点**：
  - 三种学习模式：`explore` / `scaffold` / `locked`
  - `CourseProfile` — 课程参数配置（工具白名单/学习模式/系统提示/课程编号）
  - `CourseContext` — 课程上下文（助教步骤/难度级别/课后反思/定时提醒）
  - `SwCommand` — 斜杠命令解析（切换课程/查看进度/反思/总结等）
  - `format_course_list()` — 课程列表展示

### 13.2 学生认证系统
- **描述**：教育模式下的学生身份认证
- **源文件**：`src/edu/auth.rs`
- **功能点**：
  - `authenticate()` — 学号+密码认证
  - `validate_token()` — Token 验证
  - `interactive_auth()` — 交互式认证流程
  - 支持登录/注册/退出

### 13.3 教师仪表盘
- **描述**：教师端 Web 管理界面
- **源文件**：`src/edu/dashboard.rs`
- **功能点**：
  - 基于端口的 HTTP Dashboard
  - 学生管理界面
  - 成绩/进度展示
  - 课程设置管理

### 13.4 学习反思闭环
- **描述**：自动化的学生反思评分系统
- **源文件**：`src/edu/reflection.rs`
- **功能点**：
  - `ReflectionScore` — 多维评分结构（depth/accuracy/relevance/reflection/overall）
  - `generate_reflection_prompt()` — 根据对话历史自动生成反思提示
  - `pick_template_reflection()` — 根据使用工具选择反思模板
  - `evaluate_reflection()` — 对反思文本评分（0.0-1.0）
  - `evaluate_question_quality()` — 评估提问质量
  - `generate_growth_report()` — 生成学习成长报告

### 13.5 P2P 课堂网络
- **描述**：基于 iroh 的去中心化课堂网络
- **源文件**：`src/edu/p2p.rs`
- **功能点**：
  - `ClassroomMessage` 枚举（Heartbeat/Announce/Broadcast/Assignment/Submit/Grade）
  - `CourseBrief` / `LessonBrief` / `AssignmentBrief` 数据结构
  - `encode_course_code()` / `validate_course_code()` — 课程码编码与验证
  - 消息序列化

### 13.6 数据存储
- **描述**：教育模式 SQLite 数据存储层
- **源文件**：`src/edu/store.rs`
- **功能点**：
  - `EduStore` — 完整 CRUD 操作：
  - 学生管理：`add_student()` / `get_student()` / `update_student()` / `delete_student()`
  - 课程管理：`add_course()` / `get_courses()` / `update_course()`
  - 成绩管理：`add_grade()` / `get_grades()` / `get_student_grades()`
  - 出勤记录：`record_attendance()` / `get_attendance()`
  - 反思记录：`save_reflection_journal()` / `get_reflection_journals()`
  - 认证 Token 管理：`save_token()` / `validate_token()` / `revoke_token()`

### 13.7 安装向导
- **描述**：教育模式初始化流程
- **源文件**：`src/edu/setup.rs`
- **功能点**：
  - `SetupState` 状态机（欢迎/角色选择/教师配置/学生配置/完成）
  - 交互式安装引导

### 13.8 E2E 测试
- **描述**：教育模式端到端测试
- **源文件**：`src/edu/e2e_tests.rs`

### 13.9 初始化向导 (init.rs)
- **描述**：首次使用时的交互式配置引导
- **源文件**：`src/init.rs`
- **功能点**：
  - API Key 输入
  - 模型选择
  - 通道配置引导
  - 生成 config.toml + .env

---

## 14. CLI 命令总览

```bash
rhermes                         # TUI 交互模式
rhermes --resume                # 恢复上次会话
rhermes init                    # 初始化向导
rhermes debug export <id>       # 导出调试报告
rhermes gateway start           # 启动守护进程
rhermes gateway stop            # 停止守护进程
rhermes gateway status          # 查看状态
rhermes gateway channel list    # 列出通道
rhermes gateway channel enable  # 启用通道
rhermes gateway channel disable # 禁用通道
rhermes mcp add                 # 添加 MCP Server
rhermes mcp remove              # 移除 MCP Server
rhermes mcp list                # 列出 MCP Server
rhermes mcp import              # 从 .mcp.json 导入
rhermes config init             # 生成配置模板
rhermes config check            # 检查配置完整性
rhermes edu student             # 启动学生模式
rhermes edu teacher             # 启动教师模式
```

---

## 统计概览

| 维度 | 数量 |
|------|------|
| 顶层模块 | 15 个（含 plugin/） |
| 内置工具 | 25 个 + MCP 动态 + Wasm 插件 |
| 搜索引擎 | 5 种（Bing/DuckDuckGo/SearXNG/Serper/百度） |
| 通信通道 | 6 种（TUI/微信/企微/Telegram/QQ/Web） |
| MCP 传输 | 2 种（SSE/Direct HTTP）+ resources/list/read + 工具热刷新 |
| 教育子模块 | 10 个 |
| 单元测试 | 267 个 |
| 危险命令黑名单 | 70+ 模式 |
| 学习模式 | 3 种（explore/scaffold/locked） |
| 代理模式 | 3 种（all/off/auto） |
| 成本等级 | 3 级（auto/flash/pro） |
