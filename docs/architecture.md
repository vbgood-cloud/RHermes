# RHermes 架构总览

> 多 Provider 终端 AI Agent · Rust 2024 edition · **110 个 `.rs` 文件 / 47,878 行** ·
> **33 个内置工具** + MCP 动态 + Wasm 插件 · 自进化技能系统 · Jev 判断层 · 知识库学习 · 去中心化教学班
>
> 本文对应当前代码：**v0.7.40**（`Cargo.toml`；⚠️ **最新已发布版为 v0.7.27** —— v0.7.28–v0.7.40 尚未推送/tag）。文中所有数字均按源码实测，权威计数见 `codegraph.md`。

---

## 一、整体架构（分层）

```
┌─────────────────────────────────────────────────────────────────────┐
│  main.rs / lib.rs              clap CLI（三套：rhermes / -stu / -teacher）│
├─────────────────────────────────────────────────────────────────────┤
│  接入层                                                              │
│   tui/          ratatui 终端 UI（主入口）                             │
│   channel/      Channel trait + 6 渠道（微信/企微/TG/QQ/Web/TUI）      │
│   gateway/      无 TUI 守护进程（多通道常驻 + 状态文件）                 │
│   scheduler/    cron 定时任务（gateway 模式）                          │
├─────────────────────────────────────────────────────────────────────┤
│  路由与编排                                                          │
│   agent/router.rs   SessionRouter — 按 `channel:chat_id` 隔离会话，     │
│                     再按课程上下文分流；拦截 /learn /stop /summary 等   │
├─────────────────────────────────────────────────────────────────────┤
│  智能体核心                                                          │
│   agent/session.rs   AgentSession — 主循环（Context/工具/记忆/进化）    │
│   agent/memory*.rs   三层记忆 + 外部 Provider 编排                     │
│   agent/skill.rs     SkillEngine — Markdown playbook                 │
│   agent/curator.rs   技能生命周期（30/90 天三态）                      │
│   agent/repair.rs    Tool-Call 修复管线（4 工序）                      │
│   agent/guardrails.rs 校验 + 纠正消息                                  │
│   agent/task.rs      子 Agent / 自动提炼                              │
│   judge/             Jev 判断层（fan-out 结构化判断，纯增益）           │
├─────────────────────────────────────────────────────────────────────┤
│  Provider 层         api/ 事件模型 + provider/ 池·熔断·传输             │
├─────────────────────────────────────────────────────────────────────┤
│  工具系统            tools/ 33 内置 + MCP + Wasm/SKILL 插件           │
├─────────────────────────────────────────────────────────────────────┤
│  领域模块             knowledge/（知识库学习） edu/（去中心化教学班）    │
├─────────────────────────────────────────────────────────────────────┤
│  基础设施             core/（config·context·path·http·prefix_cache·archive）│
│                      judge/（判断层）debug.rs（调试） init.rs（向导） │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 二、模块地图

| 模块 | 文件 | 行数 | 职责 | 专项文档 |
|------|:---:|-----:|------|---------|
| `edu/` | 25 | 13,318 | 去中心化教学班（P2P/白名单/托管） | `edu-p2p-design.md` 等 8 篇 |
| `agent/` | 11 | 7,045 | 智能体核心 | `guardrails.md` |
| `tools/` | 17 | 6,878 | 工具注册/调度/内置实现/搜索/Office | 本文 §六 |
| `channel/` | 12 | 3,786 | 六渠道接入 | `modules/channel.md` · `wechat-setup.md` |
| `core/` | 7 | 3,267 | 配置/上下文/路径/HTTP/缓存/归档 | 本文 §十 |
| `tui/` | 4 | 2,858 | 终端界面 | 本文 §三 |
| 根级 | — | 2,277 | `lib.rs`(1068) `init.rs`(867) `debug.rs`(337) `main.rs`(5) | 本文 §十 |
| `knowledge/` | 5 | 2,205 | 知识库存储/图谱/Bento | `learn-*.md` 3 篇 |
| `mcp/` | 8 | 1,832 | MCP 客户端（三传输） | `modules/mcp.md` · 本文 §七 |
| `api/` | 1 | 986 | OpenAI 兼容请求/响应/事件模型 | 本文 §五 |
| `provider/` | 4 | 942 | Provider 池/熔断/传输/工厂 | `modules/provider.md` · 本文 §五 |
| `gateway/` | 2 | 817 | 守护进程 + 配置向导 | `modules/gateway.md` · 本文 §三 |
| `plugin/` | 4 | 733 | 统一插件系统（Extism/SKILL.md） | `modules/plugin.md` · 本文 §七 |
| `judge/` | 3 | 705 | Jev 判断层 | `modules/judge.md` · 本文 §四.7 |
| `scheduler/` | 1 | 219 | cron 定时任务 | `modules/scheduler.md` · 本文 §三 |
| `bin/` | 2 | 10 | `rhermes-stu` / `rhermes-teacher` 入口 | — |
| **合计** | **110** | **47,878** | | |

> 📂 **模块级文档**见 `docs/modules/`：`scheduler.md` · `plugin.md` · `mcp.md` · `provider.md` · `judge.md` · `gateway.md` · `channel.md`。

---

## 三、接入层

### 3.1 `channel/` — 统一通道抽象

`Channel` trait（`channel/mod.rs:113`）：

```rust
async fn start(self: Arc<Self>, inbound_tx, shutdown_rx) -> JoinHandle<()>;
async fn send_message(&self, chat_id: &str, text: &str) -> Result<()>;
fn  name(&self) -> &str;
async fn send_formatted(...)   // 默认回退纯文本
async fn login_qrcode(...)     // 默认 None
fn  status(&self) -> ChannelStatus;   // 默认「未连接」
async fn send_typing(...)      // 默认空实现
```

`ChannelManager`：`register` / `start_all` / `broadcast` / `get` / `shutdown` / `all_status` / `write_status_file`（产出 `home/channel_status.json`）。

**渠道成熟度（实测）**

| 渠道 | 状态 | 关键实现 | 备注 |
|------|:---:|---------|------|
| TUI | ✅ | `tui/channel.rs` + 全局 `TUI_OUTBOUND` 队列 | `start()` 是 `pending()` 占位，真渲染在 `App::run()` |
| 微信个号 | ✅ | iLink Bot：扫码 → `bot_token` → `get_updates` 长轮询（2s）；发送 4000 字节分片 | token 存 `home/wechat_token.txt`；二维码实际写 **BMP** 字节（函数名仍叫 `generate_qr_png`） |
| 企业微信 | ✅ | 智能机器人 **WebSocket 长连接**（API 模式，免回调）：`aibot_subscribe` 认证 → `aibot_msg_callback` 收 → `aibot_send_msg` 发 | ping 30s；断线指数退避重连 1s→30s；凭证缺失走 `Disabled`（不静默降级）；⚠️ TLS 必须 `native-tls`（rustls 指纹被网关拒）；⚠️ 打字机流式通路已备但 **Sink 未接线** |
| Telegram | ✅ | `getUpdates` 长轮询 + MarkdownV2 + `sendChatAction` typing | **仅 gateway 模式注册** |
| QQ | ⚠️ | app_id/secret → WSS Gateway + op10/11/0 协议；群 + C2C | 见下「已知缺口」 |
| Web | ❌ | axum 内嵌页 + `POST /api/chat` | 半成品，见下 |

**QQ 已知缺口**：① `state_connected()` 恒 `false` → `status()` 永远报未连接；② setup 向导 / `channel list` / 配置模板均不含 QQ；③ 每次 Hello 都 spawn 无退出条件的心跳任务；④ 仅 gateway 模式。

**Web 已知缺口**：① `send_message` 空实现；② `replies` 缓存只读不写；③ **无任何调用方**；④ 无 `WebConfig`；⑤ `admin_token` 从未校验。

> **模式差异**：TUI 模式只注册 `tui` + `wechat` + `wecom`（`lib.rs:841-857`）；
> `telegram` / `qq` 只在 gateway 模式可用。

### 3.2 `gateway/` — 守护进程

- `rhermes gateway start` → 注册 wechat/wecom/telegram/qq（`mod.rs:148-186`）→ 扫码 → `start_all` → 等 30s 打印中文状态表 → 起 `SessionRouter` + 调度器 + inbound 消费循环。
- 状态文件 `home/channel_status.json` 每 30s 写一次；PID 文件 `home/gateway.pid`；日志 `home/gateway.log`。
- `gateway stop` 走 `taskkill /F /PID`（Windows）。
- ⚠️ `channel_list` 只列 wechat/wecom/telegram（**不含 qq**），而 `channel_enable_disable` 支持 qq。

### 3.3 `tui/` — 终端界面

`App`（`tui/mod.rs:196`）持有 `tui_router_mode` / `outbound_rx` / `channel_inbound_tx` / `thinking_buffer` / `show_thinking` 等。
- `tui/markdown.rs` — 自研轻量 Markdown → ratatui（粗体/行内代码/代码块/标题/引用/列表/分隔线）
- `tui/qrcode.rs` — `render_ascii_qr` 终端 ASCII 二维码
- ⚠️ `ApiEvent::Thinking`（`mod.rs:840`）处理体为空 —— 因为**没有生产者**（见 §五）

### 3.4 `scheduler/` — cron 定时任务

Gateway 模式下从 `config.scheduler.tasks` 读 cron 表达式，`cron::Schedule` 计算触发点；每个任务一个 `tokio::spawn` 循环；`Semaphore(max_concurrent_tasks)`（默认 5）限并发；触发时新建独立 `AgentSession` 执行 `prompt`，经 `CaptureSink` 收结果，按 `target = "channel:chat_id"` 推送到通道。启动时对齐到下一整分钟，每轮后 sleep 58s 防重复触发。

---

## 四、智能体核心（`agent/` + `judge/`）

### 4.1 `agent/session.rs`（1072 行）— 主循环

`AgentSession::handle_message()` 是唯一入口，TUI 与所有渠道共用。

**关键常量与行为**

| 项 | 值 |
|----|----|
| 上下文窗口 | `CONTEXT_WINDOW = 128000`；压缩阈值比 0.8 |
| 主模型 max_tokens | 4096（压缩摘要 1024） |
| 流式超时 | 120s |
| 轮次上限 | `max_rounds` 默认 50 |
| 全局工具调用上限 | `MAX_TOTAL_TOOL_CALLS = 200` |
| 空响应兜底 | 最多补问 2 次（v0.7.25） |
| 工具输出截断 | run_command 5000 / web_search 4000 / web_fetch 8000 / read_file·read_pdf 10000 / Office 8000 / 默认 5000（字符） |
| 动态工具筛选 | `select_tools()` 按关键词筛，命中 <5 回退全量 |
| 思考强度推断 | `infer_reasoning_effort()`：复杂词或 >500 字 → high；简单词 → low；≤2 字 → None |
| 进化建议 | 每 5 轮输出一次 |
| 记忆召回 | limit 5 |
| 教学规则 | `KB_TUTOR_BLOCK` 内嵌块（`session.rs:1002`） |

### 4.2 `agent/router.rs`（1432 行）— 多会话路由

会话键 `format!("{channel}:{chat_id}{course_suffix}")`（`router.rs:301`）。
拦截并处理 `/learn` `/stop` `/summary` `/sw` `/model` 与教育斜杠命令；承载知识库建库闭环与学习反思落库。

**知识库相关常量**：`CHUNK_CHARS = 20000`、小文件全文注入阈值 **24000** 字符、`KB_BUILD_MAX_RETRIES = 3`、`KB_BUILD_MIN_NODES = 5`、`KB_BUILD_HEARTBEAT_SECS = 60`。

### 4.3 `agent/memory.rs`（824 行）+ `memory_manager.rs`（341 行）

三层记忆（Session / Working / LongTerm）落 SQLite + FTS5（WAL）。API：`store` / `get` / `search` / `list` / `delete` / `should_nudge`（默认 300s）/ `recent` / `export_memory_md` / 画像 `load_profile`·`save_profile`·`aggregate_profile` / 会话消息 `save_session_messages`·`load_session_messages`。

`memory_manager.rs` 定义 `MemoryProvider` trait（`prefetch` / `inject` / `intercept` / `sync` / `flush`）、`BuiltinProvider`，以及 8 种外部类型枚举（Honcho / Mem0 / MemGpt / Memary / Letta / Zep / Graphiti / Custom）。
> ⚠️ `intercept` 当前恒返回 `None`（无实际拦截逻辑）；文件头带 `#![allow(dead_code)]`。

### 4.4 `agent/skill.rs`（955 行）+ `curator.rs`（593 行）

- 技能 = Markdown playbook + YAML frontmatter；`RunAs::{Inline, Subagent}`。
- 使用统计存 sidecar `.usage.json`（含从旧 `.md` 注释迁移的逻辑）。
- 健康度：成功率 **>0.5** 为健康、**<0.3** 需关注；使用 **>10** 次且成功率 **>0.95** 触发"进化建议"。
- `pinned` 技能受保护；技能名只允许字母数字与 `_` `-`。
- **Curator 三态**：`days ≥ 30` → Stale；`days ≥ 90` → Archived；否则 Active。运行触发间隔 **7 天**（标记文件 `.curator_last_run`），归档目录 `_archived/`。
- ⚠️ `UmbrellaAction`（Consolidated / Pruned）**只有枚举、无实现**。

### 4.5 `agent/repair.rs`（806 行）— Tool-Call 修复管线

四道工序：**Flatten**（参数压平/还原）、**Scavenge**（从 `<tool_call>` 标签 / ```json 块 / `<think>` 块 / Mistral `[TOOL_CALLS]` 四种来源捞取）、**Truncation**（补全截断 JSON）、**Storm**（抑制相同 (tool,args) 重复调用）。

> ⚠️ **Flatten 未接入生产路径**：`repair_with_api()` 只执行 Scavenge / Truncation / Storm，
> `Flattened` / `RestoredNested` 两个动作在生产中永不产生（仅单测覆盖）。
> `StormSuppression::default()` 是 `5s / 1 次`，而配置默认是 `60s / 3 次`。

### 4.6 `agent/guardrails.rs`（211 行）+ `event_sink.rs`（197 行）

- **校验仅两条规则**：工具名存在于 `ToolRegistry`、`required: true` 参数齐全。
- 失败时 `NudgeBuilder` 构建纠正消息（未知工具时列出**前 20 个**可用工具）。
- ⚠️ 全部拦截时只记 WARN，**不再 `continue` 重试**，直接落入最终回复。
- `event_sink.rs`：`EventSink` trait（8 个钩子）→ `TuiSink` / `ChannelSink`；`strip_thinking` 用双正则剥离思考块。

### 4.7 `judge/` — Jev 判断层（D18）

- `Judge::ask(state, HashMap<String, Question>)` —— **fan-out 一问多答**。
- 三种问法：`Noul`（是非，无置信度）/ `Choice`（多选一，criteria ≤255）/ `Score`（有序档位 2–10，**禁插值**）。
- 门控：`min_confidence`（默认 0.6）—— 不足返回 `None`，调用方回退启发式。
- `from_config` 需 `enabled=true` 且 api_key 非空；客户端重试 1 次、4xx 不重试、超时默认 5s。
- 全局单例：`set_global_judge` / `global_judge`。**所有失败路径吞成 `None`，无 panic**。
- **五处集成点**：记忆保存门控（0.7）· 技能巡检 `Curator::run_with_judge()` · 修复可信度（0.75）· 学习反思三档（0.25/0.6/0.95）· 搜索预筛（p<0.5 滤除，全滤则回退）。

---

## 五、Provider 层（`provider/` + `api/`）

### 5.1 池与熔断（`provider/pool.rs`）

- **调度策略：主 provider 优先**（`providers[0]` 健康就一直用）；仅当它熔断时才在备选之间轮转。
- ⚠️ `ProviderEntry.weight` 字段存在但**从不被读取** —— 不存在加权轮询，`factory` 设的 10:1 权重无效。
- **熔断器**三态 Closed / HalfOpen / Open：连续失败 **3** 次断开，冷却 **30s** 后半开，成功复位。
- 重试：指数退避 `500 × 2^(n-1)` ms，最多 **3** 次（`max_retries()` 硬编码，未关联 `config.request.max_retries`）；4xx 不重试。

### 5.2 传输（`provider/transport.rs` + `factory.rs`）

**所有 Provider 共用 `DeepSeekTransport`**，统一按 OpenAI 兼容 `/chat/completions` 发送。
支持的 base_url 映射（`factory.rs:11-24`）：`omniroute` / `openai`·`openrouter` / `deepseek` / `siliconflow` / `zhipu` / `moonshot`·`kimi` / `ollama` / `groq` / `newapi` / `lmstudio`，未知回落 OpenAI。

`chat_stream` 已实现：DSML tool_calls 解析、流式 tool_calls 累积、`x-omniroute-*` 注释元数据解析。
> ⚠️ 解析出的 `provider_meta` **从不发送**（`transport.rs:277-283` 是空块），`ApiEvent::ProviderMeta` 全仓无生产者；
> `reasoning_content` 读到后同样进空块 → `ApiEvent::Thinking` 也无生产者。
> 净效果：**消费者（TUI / SessionRouter）已写好，生产者缺失**，思考流与路由元数据实际都未利用。

### 5.3 事件模型（`api/mod.rs`）

`ApiEvent`：`StreamChunk` / `ToolCalls` / `Usage` / `Done` / `Error` / `Thinking` / `ProviderMeta` / `Balance`。
`Usage` 含 `cached_tokens` / `reasoning_tokens`；`ChatRequest` 含 `reasoning_effort`。
> ⚠️ `api/mod.rs` 里的 `DeepSeekClient`（含 `impl Transport`）**全仓无引用**，与 `provider::DeepSeekTransport` 重复。
> `default_tools()` 已标 `#[deprecated]`（仍列 17 个旧工具名，仅作降级兜底）。

---

## 六、工具系统（`tools/`）

### 6.1 注册与调度

- `registry.rs`：`Tool` trait（`execute` / `parallel_safe` / `name` / `description` / `parameters` / `required_params`）+ `ToolRegistry`。
- `dispatcher.rs`：**连续**的 `parallel_safe` 调用聚为一个 `JoinSet` 批次并发；遇非并行工具先等批次完成再串行（屏障）。并发上限 `max_concurrency` 默认 **10**（Semaphore 限流）。
  - ⚠️ **并行结果按完成顺序返回，不保证等于调用顺序** → 必须按 `ToolResult.call_id` 对齐。
  - 子任务 panic 产出 `ToolResult::error("unknown","unknown",…)`。

### 6.2 33 个内置工具

| 类别 | 工具 |
|------|------|
| 文件 | `read_file` · `write_file` · `glob` · `search_content` · `read_pdf` |
| 系统 | `run_command` · `get_current_time` |
| 网络 | `web_search`（5 引擎降级） · `web_fetch`（SSRF 防护 + `<untrusted>` 包裹） |
| Agent | `delegate_task` · `run_skill` |
| 技能 | `skill_list` · `skill_search` · `skill_create` · `skill_patch` · `skill_manage` |
| 记忆 | `memory` |
| 知识库 | `kb_create` · `kb_append` · `kb_graph` · `kb_learn` · `kb_quiz` · `kb_stats` · `kb_list` · `kb_reset` |
| Office | `read_excel` · `write_excel` · `read_docx` · `write_docx` · `read_pptx` |
| 文档解析 | `parse_document` · `screenshot_document` · `check_document_complexity` |

另有条件/动态注册：`run_plugin`（仅在真的加载到插件时）、`wasm_{name}`、`mcp__{server}__{tool}`。

> ⚠️ 3 个 LiteParse 工具**无条件注册**，可用性取决于运行时 `init_liteparse()` 是否成功。
> ⚠️ **3 处**硬编码 system prompt 仍写"可用工具（共 17 个）"——已与注册表（实际 33 个）脱钩：`lib.rs:985` · `gateway/mod.rs:286` · `tui/mod.rs:510`。

### 6.3 安全边界

- **命令黑名单 75 条**：`DANGEROUS_PATTERNS` 48 条 + `DANGEROUS_CHAIN_PATTERNS` 27 条（检测管道/分号/`&&`/`||`/`$()`/反引号/`cmd /c`/base64 管道等绕过）。
- 命令白名单 `agent.command_allowed_prefixes`（按 basename 匹配）+ `command_require_confirm`。
- `read_file` 除工作目录外还放行 `data_root/home/`（知识库 sources / 图谱 / bento）。
- `web_fetch`：scheme 白名单 + 内网/元数据 IP 黑名单 + 扩展名预检 + Content-Type 检查 + 5MB 上限。

### 6.4 搜索（`tools/search/`）

`SearchEngine` trait + `SearchCache`（TTL）+ `MultiEngineSearcher`（优先级降级）；5 个实现：Bing / DuckDuckGo / SearXNG / Serper / 百度。

---

## 七、MCP 与 Plugin

### 7.1 MCP 客户端（`mcp/`）

**三种传输，按配置自动降级**：
1. **stdio**（`transport.rs`）— `command` 存在时优先；子进程 stdin/stdout，Windows 隐藏窗口。
2. **SSE**（`sse_transport.rs`）— GET 探测 + 等 `endpoint` 事件拿 message_url。
3. **Direct POST** — SSE 探测失败时回退，直接 POST JSON-RPC。

握手：`initialize`（protocolVersion `2024-11-05`）→ `notifications/initialized` → `tools/list`。调用 `tools/call`。请求超时 120s。
工具命名 `mcp__{server}__{tool}`；`tool_parallel_safe` 可按工具名覆盖 Server 级并行标志。

> ⚠️ **热刷新局限**：`notifications/tools/list_changed` 只更新 adapter 内部快照，**已注册进 ToolRegistry 的条目不会增删**，需重启生效。
> `resources/list` / `resources/read` 已实现但**未包装为 Tool**；`sampling_enabled` 配置项存在但无实现。

### 7.2 统一 Plugin 系统（`plugin/`）

```rust
trait Plugin {
    async fn descriptor(&self) -> PluginDescriptor;
    async fn execute(&self, input: &Value) -> Result<PluginOutput, PluginError>;
    async fn health(&self) -> Result<bool, PluginError>;
    async fn reload(&self) -> Result<(), PluginError>;  // 默认不支持
}
```

- `PluginRouter`：`register`（重名覆盖）/ `call` / `list_descriptors`（按名排序）/ `count` / `health_all`；全局 `OnceLock` 单例。
- 两个适配器：`ExtismPlugin`（Wasm 沙盒）与 `SkillMdPlugin`（返回技能全文作上下文注入，无副作用）。
- **加载优先级**：`plugins/registry.toml` 存在则只加载其中 `enabled=true` 的条目；不存在则扫描目录 `*.wasm` + `*.md`。`native` / `mcp` 类型被显式拒绝。
- **Wasm 权限网关**（`tools/wasm_host_functions.rs`）：`host_log` 永远注册；`host_http_get/post` 需 `allowed_hosts` 非空；`host_read_file/write_file` 需 `allowed_paths` 非空；`host_exec` 需 `allow_exec=true`（默认 false）。域名支持 `*` 与 `*.example.com`；路径用 `canonicalize` 双向解析防 `../` 逃逸；Manifest 二次加固。
- 权限来源：侧车文件 `plugins/<name>.host.toml` 或 `registry.toml` 条目；缺失即最小权限。
- 沙盒参数：`max_memory` 默认 32MB、`timeout_ms` 默认 30000。

> ⚠️ 存在两套命名路径并存：旧 `wasm_{name}`（Tool 直挂）与新 `run_plugin(name=…)`（PluginRouter）。

---

## 八、知识库学习（`knowledge/`）

| 文件 | 行数 | 职责 |
|------|-----:|------|
| `store.rs` | 1453 | SQLite 数据层 + 掌握度/遗忘曲线/SM-2 纯函数 + `next_node` 调度 + 导入导出 |
| `svg.rs` | 258 | 图谱 SVG 渲染（四档配色，按 `effective_mastery` 着色） |
| `bento.rs` | 233 | 战绩面板（终端 ANSI 版 + HTML 版） |
| `layout.rs` | 187 | Kahn 拓扑分层 + 层内水平居中 + 画布尺寸 |
| `mod.rs` | 69 | 路径工厂（`db_path` / `graphs_dir` / `bento_dir` / `sources_dir` / `exports_dir`） |

**常量**：`EXPORT_FORMAT = "rhermes-kb"`、`EXPORT_VERSION = 3`、`REVIEW_FLOOR_DEFAULT = 60`、`S_INIT = 1.0`、`S_MIN = 0.5`、`S_MAX = 180.0`、`EF_INIT = 2.5`、`EF_MIN = 1.3`、`EF_MAX = 2.8`。

**表（6 张 + 3 索引）**：`topics` / `nodes`（含 `stability` `next_review_at` `easiness`）/ `edges` / `quiz_log` / `sessions` / `question_bank`（预留空）。

**公式**
- 保留率 `retention = 0.5^(Δt/S)`；有效掌握度 `effective = round(mastery × retention)`，clamp [0,100]。
- 分档增长因子：≥90 → ×2.5；≥70 → ×2.0；≥50 → ×1.5；否则 ×0.7。
- SM-2：`q = round(score/20)`，`EF' = clamp(EF + 0.1 − (5−q)(0.08 + 0.02(5−q)), 1.3, 2.8)`；
  `next_stability = clamp(S × factor × clamp(EF/2.5, 0.5, 1.2), 0.5, 180)`（**用测验前旧 EF**）。
- 掌握度 EMA：首次 = 分数；其后 `(旧×40 + 新×60)/100`；同日同节点只取最高分（24h 防刷分）。
- 分档：0 未学 / 1–49 初识 / 50–79 掌握中 / ≥80 精通。

**`next_node` 调度优先级**（`store.rs:347`）
① `effective < review_floor` 最遗忘优先 → ② `mastery == 0`（layer 升序 → id 升序）→ ③ `review_floor ≤ effective < 80` 薄弱巩固 → ④ 全部 ≥80 返回 `None`。

数据库 schema 迁移用 `PRAGMA user_version`（当前到 **2**），与 `EXPORT_VERSION = 3` 是**两套独立编号**。

---

## 九、教育模式（`edu/`）

见 `docs/edu-p2p-design.md`。核心结构：

- **全局班键** `SectionKey { teacher: EndpointId, section_id: i64 }`（section_id 非全局唯一）。
- **Topic 派生** `BLAKE3("rhermes-section-topic" ‖ seed ‖ epoch_le)`，seed 视为密钥。
- **双 ALPN**：`/class-auth/1.0`（开放，认证）+ `/class-app/1.0`（受控，业务）。
- **`SectionMsg` 7 变体**：`AssignmentPosted` / `Question` / `Answer` / `Chat` / `Announce` / `TopicRotate` / `AllowlistUpdate`（postcard 序列化）。
- **`SectionHost`**（`host.rs`，S2.1）—— 驱动无关会话宿主：`HostCommand` 进、`HostEvent` 出（broadcast 多订阅者）；`NOTICE_BUFFER = 256`、`SHUTDOWN_TIMEOUT = 5s`。
- **教师端 AI 答疑循环**（`runtime.rs:324`，v0.7.16）：收到学生 `Question` 自动并发调 LLM（60s 超时）并以 `Answer` 广播回班。
- **签名白名单**（`allowlist.rs`）：Ed25519 签名 64 字节，`ALLOWLIST_VERSION = 1`，纪元单调防回放；撤销 = 落库 + 轮换 epoch + `TopicRotate` + 新表签名广播。

### 三驱动收敛进度（S2.1–S2.5）

| 阶段 | 状态 | 说明 |
|------|:---:|------|
| S2.1 `SectionHost` | ✅ | `src/edu/host.rs`（15 条单测 + 1 e2e） |
| S2.2 REPL 迁移 | ✅ | `client_app.rs` 已改用宿主，`render_event` 统一 |
| S2.3 推送驱动 + 学生 `/class` | ⬜ | `SessionRouter.edu_hosts` 不存在；学生侧 `/class` 未实现 |
| S2.4 双入口收敛 | ⬜ | `edu/mod.rs::handle_slash_command` 与 `agent/router.rs::handle_edu_slash_command` 仍并存 |
| S2.5 渠道真机验证 | ⬜ | 未接线 |

> ⚠️ **硬编码 `teacher_id = 1` 共 14 处**（生产代码）：`edu/mod.rs` 5 处、`edu/teacher.rs` 3 处、
> `edu/setup.rs` 2 处、`edu/dashboard.rs` 1 处、`agent/router.rs` 3 处。
> 一位老师一套库时无碍，多老师共库时需改。

---

## 十、基础设施（`core/` + 根级模块）

### 10.1 `core/context.rs` — 三段式 Context

`immutable_prefix: Arc<[u8]>` / `append_only_log: Vec<u8>` / `scratch: Vec<Message>`。
压缩：`TAIL_BUDGET_TOKENS = 16384`（×2 转 bytes）+ `recent_msgs.len() < 20` 双约束；`MIN_RECENT_KEEP = 2`；
摘要 prompt 为六段式（Goal / Decisions & rationale / Files & code / Commands & outcomes / Errors & fixes / Pending & next step）。
token 估算 `body.len() / 2`。
> ⚠️ `COMPACT_RATIO = 0.8` 定义后**从未使用**（实际比例由调用方从配置传入）。

### 10.2 `core/config.rs`（2207 行）— 19 个配置段

`api` · `providers` · `request` · `memory` · `debug` · `display` · `agent` · `provider_pool` · `channels` · `proxy` · `gateway` · `scheduler` · `wasm` · `mcp` · `search` · `edu` · `liteparse` · `knowledge` · `jev`
（另 `api_key` 走 `.env`，`#[serde(skip)]`）

`.env` 约定：`{PROVIDER}_API_KEY` → `providers.{name}`；`TYPESAFE_API_KEY` → jev；另有 `SERPER_API_KEY` / `SEARCH_PROXY` / `TELEGRAM_BOT_TOKEN` / `QQ_BOT_APP_ID` / `QQ_BOT_APP_SECRET`。

> ⚠️ `generate_annotated_template()` **不产出** `[channels.qq]` / `[edu]` / `[knowledge]` 三段（虽为合法段）。

### 10.3 其他

| 文件 | 职责 |
|------|------|
| `core/path.rs` | `detect()`：`exe_dir` → `data_root = exe_dir/home`；子目录 memory.db / skills / sessions / logs / cache / workspace |
| `core/prefix_cache.rs` | 三层 system prompt（Stable / Context / Volatile）冻结为 `Arc<[u8]>` |
| `core/http_client.rs` | 代理感知客户端工厂：三模式 all/off/auto + `no_proxy` |
| `core/archive.rs` | 压缩归档 `compressions/YYYY-MM-DD.jsonl`，摘要截断 500 字符 |
| `debug.rs` | 会话调试环形缓冲 500 条 + `debug export` JSON 报告 |
| `init.rs` | 交互式初始化向导（5 步 + 工作目录/代理/通道三段） |

---

## 十一、数据流（一次完整对话）

```
用户输入
    ↓
tui App.handle_key() 或 Channel 入站 → mpsc
    ↓
SessionRouter.dispatch(channel, chat_id, text)  → 定位 AgentSession
    ↓
AgentSession.handle_message()
    ├── 记忆召回（limit 5）→ 判断层门控 → 注入
    ├── select_tools() 动态筛工具 + infer_reasoning_effort()
    ├── Context.build_request_body()（三段式）
    ↓
ProviderPool.chat_stream()（主 provider 优先 → 熔断则备选 → 指数退避）
    ↓
SSE 流式解析 → EventSink（TUI / Channel）
    ├── StreamChunk → 逐字渲染
    └── ToolCalls → 修复管线（Scavenge/Truncation/Storm）→ 护栏校验 → 纠正重试
         ↓
     ToolDispatcher.dispatch()
       ├── 连续 parallel_safe → JoinSet 批次（Semaphore 限 10）
       └── 非并行 → 屏障后串行
         ↓
     结果按 call_id 对齐回写 messages
    ↓
循环直到模型不再发 tool_call（或达 max_rounds / MAX_TOTAL_TOOL_CALLS）
    ↓
反思与记忆落库 · 每 5 轮技能进化建议 · 会话归档
```

---

## 十二、技术栈

| 模块 | 选型 |
|------|------|
| 语言 / 异步 | Rust 2024 · tokio |
| HTTP | reqwest（socks 代理、SSE、连接复用） |
| TUI | ratatui + crossterm |
| 数据库 | rusqlite + FTS5（记忆与知识库各自独立库） |
| 搜索 | grep-regex + grep-searcher（ripgrep 库）+ scraper |
| Office | calamine / rust_xlsxwriter / docx-rs / zip + quick-xml |
| PDF | pdf-extract |
| MCP | JSON-RPC：stdio / SSE / Direct |
| WASM | extism（沙盒 + host functions 权限网关） |
| P2P | iroh + iroh-gossip + iroh-blobs + postcard |
| 二维码 | qrcodegen（输出 BMP） |
| CLI | clap（derive） |
| 配置 | TOML + serde + dotenvy |
| 日志 | tracing + tracing-subscriber |
| 测试 | **414 单元 + 6 集成 = 420** |

---

## 十三、目录约定（可移动模式）

所有数据都在**可执行文件旁**的 `home/` 下（`PathManager::detect()` 以 exe 目录为锚）：

```
<exe_dir>/
├── config.toml          # 配置（含注释模板由 config init 生成）
├── .env                 # API Key / Token（永不写入 config.toml）
└── home/
    ├── memories/memories.db    # 记忆库（lib.rs 实际使用此路径）
    ├── skills/                 # 技能（.md + .usage.json）
    ├── sessions/               # 会话归档
    ├── logs/
    ├── knowledge/
    │   ├── kb.db               # 知识库
    │   ├── graphs/<库>.html    # 知识图谱
    │   ├── bento/              # 战绩面板
    │   ├── sources/<库>/part_NNNN.md   # 大文件分段
    │   └── exports/<名>-<时间戳>.kb.json
    └── ...                     # 教育模式另有 edu.db / edu_identities/
```

> 提示：因为锚点是 exe 目录，**换目录 = 换整套数据**，这也是它 USB 可运行的原因。

---

## 十四、命令树（三套 CLI）

```
rhermes                       # TUI
  init | debug export | config init|check|save
  gateway setup|start|stop|status + channel list|enable|disable
  mcp setup|remove|list|import
  edu <透传>

rhermes-teacher               # 教师（含上述全部通用命令）
  init-teacher <名> [密码]     # alias: create-teacher（必须显式写，勿用 init）
  course create|list · class create · lesson create
  student add|import · roster [码]
  serve [<码> <班> ...|--all|--offline] · addr · dashboard

rhermes-stu                   # 学生（含上述全部通用命令）
  login <学号> <密码> · live [--offline]
  status | courses | profile | report | mode [explore|scaffold]
  join <码>                   # ⚠️ 占位未实现
```

> `status` / `courses` / `profile` / `report` / `mode` 目前均为**占位回显**，`join` 打印"Phase 4+ 实现"。
> 学生真正的课堂交互在 `live` 内部的交互台：`list` / `ask` / `chat` / `quit`。

---

## 十五、已知未完成项与死代码（诚实清单）

| 项 | 位置 | 状态 |
|----|------|------|
| ~~`cost.rs` 五机制~~ | ~~`src/cost.rs`~~ | ✅ **已于 v0.7.36（`47ba060`）整体删除**（560 行全项目零引用）；成本统计由 `api::Usage` 承担 |
| `edu/p2p.rs` 大部分 | `src/edu/p2p.rs`（761 行） | ⚠️ **并非零引用**（`net/mod.rs` 用了 `encode_course_code` + `EDU_ALPN`），但 761 行中绝大多数未使用，宜收敛为 2 项导出 |
| Repair 的 Flatten 工序 | `agent/repair.rs` | ⚠️ 生产路径不调用，仅单测覆盖 |
| `ApiEvent::Thinking` / `ProviderMeta` | `provider/transport.rs` | ⚠️ 有消费者无生产者（读到的值进了空块） |
| `DeepSeekClient` | `api/mod.rs:494` | ⚠️ 死代码，与 `provider::DeepSeekTransport` 重复 |
| `OutboundMessage` | `channel/types.rs:38` | ⚠️ 定义后零引用 |
| `ProviderEntry.weight` | `provider/pool.rs` | ⚠️ 从不被读取（无加权轮询） |
| Web 通道 | `channel/web/` | ❌ 半成品，无调用方 |
| QQ 通道状态 | `channel/qq/mod.rs:297` | ⚠️ `state_connected()` 恒 `false` |
| MCP 工具热刷新 | `mcp/adapter.rs` | ⚠️ 只更新内部快照，Registry 条目需重启 |
| MCP resources / sampling | `mcp/` | ⚠️ resources 已实现未包装为 Tool；sampling 无实现 |
| `Curator` umbrella 合并 | `agent/curator.rs` | ⚠️ 仅 `UmbrellaAction` 枚举占位 |
| `MemoryManager::intercept` | `agent/memory_manager.rs` | ⚠️ 恒 `None` |
| `grading`/外部记忆 Provider | `agent/memory_manager.rs` | ⚠️ 仅枚举，无具体实现 |
| 学生 `join` 与 `status` 族 | `edu/mod.rs` | ⚠️ 占位回显 |
| 三驱动 S2.3–S2.5 | `edu/` | ⬜ 见 §九 |
| 硬编码 `teacher_id = 1` | 5 个文件 14 处 | ⚠️ 多老师共库时需改 |

---

## 十六、一句话总结

**RHermes 是一个以"省 Token"为第一性原理的多渠道终端 AI Agent**：三段式 Context 最大化前缀缓存命中，
工具并行调度 + Tool-Call 修复管线保证鲁棒性，自进化技能与三层记忆让它越用越强，
Jev 判断层给高频决策降本，知识库学习与去中心化教学班把能力延伸到教学场景。
