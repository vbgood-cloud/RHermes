# RHermes Codegraph

> 自动生成：`python3 docs/codegraph_gen.py`（或让 Agent 重新生成）—— **请勿手工编辑本文件**

## 总览

| 指标 | 值 |
|------|-----|
| 源文件 | 110 |
| 总行数 | 47,787 |
| pub fn | 725 |
| 单元测试 | 414 |
| 核心 trait | Channel · EventSink · MemoryProvider · Plugin · RegistrarClient · SearchEngine · Tool · Transport |

## 模块依赖图

```mermaid
graph TD
    agent["agent<br/>11f/6k行"]
    api["api<br/>1f/0k行"]
    bin["bin<br/>2f/0k行"]
    channel["channel<br/>11f/3k行"]
    core["core<br/>7f/3k行"]
    cost["cost<br/>1f/0k行"]
    debug["debug<br/>1f/0k行"]
    edu["edu<br/>25f/13k行"]
    gateway["gateway<br/>2f/0k行"]
    init["init<br/>1f/0k行"]
    judge["judge<br/>3f/0k行"]
    knowledge["knowledge<br/>5f/2k行"]
    lib["lib<br/>1f/1k行"]
    main["main<br/>1f/0k行"]
    mcp["mcp<br/>8f/1k行"]
    plugin["plugin<br/>4f/0k行"]
    provider["provider<br/>4f/0k行"]
    scheduler["scheduler<br/>1f/0k行"]
    tools["tools<br/>17f/6k行"]
    tui["tui<br/>4f/2k行"]

    agent --> api
    agent --> channel
    agent --> core
    agent --> edu
    agent --> knowledge
    agent --> provider
    agent --> tools
    api --> core
    api --> provider
    channel --> agent
    channel --> api
    channel --> core
    core --> tui
    edu --> core
    gateway --> channel
    gateway --> core
    init --> core
    judge --> core
    lib --> channel
    lib --> core
    lib --> tui
    mcp --> core
    mcp --> tools
    plugin --> tools
    provider --> api
    provider --> core
    scheduler --> agent
    scheduler --> channel
    scheduler --> core
    scheduler --> provider
    scheduler --> tools
    tools --> api
    tools --> core
    tools --> knowledge
    tools --> mcp
    tui --> agent
    tui --> api
    tui --> channel
    tui --> core
    tui --> knowledge
    tui --> tools
```

## 模块清单

| 模块 | 文件 | 行数 | pub fn | 测试 | traits | 依赖 |
|------|-----:|-----:|-------:|-----:|--------|------|
| `edu/` | 25 | 13,318 | 256 | 145 | RegistrarClient | core |
| `tools/` | 17 | 6,878 | 60 | 37 | Tool, SearchEngine | api, core, knowledge, mcp |
| `agent/` | 11 | 6,779 | 107 | 65 | EventSink, MemoryProvider | api, channel, core, edu, knowledge, provider, tools |
| `channel/` | 11 | 3,410 | 41 | 11 | Channel | agent, api, core |
| `core/` | 7 | 3,254 | 50 | 36 | — | tui |
| `tui/` | 4 | 2,858 | 18 | 19 | — | agent, api, channel, core, knowledge, tools |
| `knowledge/` | 5 | 2,205 | 42 | 37 | — | — |
| `mcp/` | 8 | 1,832 | 51 | 11 | — | core, tools |
| `lib/` | 1 | 1,060 | 4 | 0 | — | channel, core, tui |
| `api/` | 1 | 986 | 11 | 6 | — | core, provider |
| `provider/` | 4 | 942 | 12 | 0 | Transport | api, core |
| `init/` | 1 | 877 | 1 | 0 | — | core |
| `gateway/` | 2 | 818 | 2 | 0 | — | channel, core |
| `plugin/` | 4 | 733 | 16 | 11 | Plugin | tools |
| `judge/` | 3 | 705 | 17 | 10 | — | core |
| `cost/` | 1 | 561 | 27 | 20 | — | — |
| `debug/` | 1 | 337 | 8 | 6 | — | — |
| `scheduler/` | 1 | 219 | 2 | 0 | — | agent, channel, core, provider, tools |
| `bin/` | 2 | 10 | 0 | 0 | — | — |
| `main/` | 1 | 5 | 0 | 0 | — | — |

## 核心 trait 索引

- **`Channel`** — `channel/mod.rs`
- **`EventSink`** — `agent/event_sink.rs`
- **`MemoryProvider`** — `agent/memory_manager.rs`
- **`Plugin`** — `plugin/mod.rs`
- **`RegistrarClient`** — `edu/registrar.rs`
- **`SearchEngine`** — `tools/search/mod.rs`
- **`Tool`** — `tools/registry.rs`
- **`Transport`** — `provider/transport.rs`

## 关键类型（跨模块 pub struct/enum，按模块）

- **edu/**: `AllowlistEntry`, `SignedAllowlist`, `AllowlistState`, `RevokeOutcome`, `AuthResult`, `MemberBinding`, `AuthRegistry`, `WhitelistHook`, `LearnMode`, `CourseProfile` (+64)
- **tools/**: `ReadFile`, `SearchContent`, `ReadPdf`, `Glob`, `WriteFile`, `RunCommand`, `GetCurrentTime`, `WebSearch`, `WebFetch`, `DelegateTask` (+43)
- **agent/**: `UmbrellaAction`, `SkillStatus`, `Curator`, `CuratorReport`, `TuiSink`, `ChannelSink`, `ValidationError`, `ValidationResult`, `ResponseValidator`, `NudgeBuilder` (+27)
- **channel/**: `ChannelManager`, `ChannelStatus`, `ChannelState`, `QqApi`, `QqError`, `QqEvent`, `GroupMessage`, `GroupAuthor`, `C2cMessage`, `C2cAuthor` (+15)
- **core/**: `Config`, `ProxyMode`, `ProxyConfig`, `JevConfig`, `ChannelsConfig`, `WeComConfig`, `WeChatConfig`, `TelegramConfig`, `QqConfig`, `GatewayConfig` (+27)
- **tui/**: `TuiChannel`, `AppCommand`, `Role`, `Message`, `Stats`, `App`
- **knowledge/**: `NodeIn`, `EdgeIn`, `NodeRow`, `EdgeRow`, `GraphSnapshot`, `NextNode`, `KbStats`, `KbExport`, `TopicOut`, `GraphOut` (+7)
- **mcp/**: `McpLogLevel`, `McpToolInfo`, `McpResourceInfo`, `McpAdapter`, `McpAdapterManager`, `McpSseTransport`, `McpDirectTransport`, `McpTransportWrapper`, `McpRemoteTool`, `McpError` (+1)
- **lib/**: `Cli`, `CommonCommands`, `TeacherCli`, `TeacherCommands`, `StudentCli`, `StudentCommands`
- **api/**: `ChatRequest`, `ToolDef`, `ToolFunction`, `ChatResponse`, `Choice`, `ResponseMessage`, `ResponseToolCall`, `ResponseToolFunction`, `Usage`, `StreamEvent` (+7)
- **provider/**: `ProviderPool`, `DeepSeekTransport`
- **plugin/**: `ExtismPlugin`, `PluginRouter`, `SkillMdPlugin`, `PluginDescriptor`, `PluginSource`, `PluginOutput`, `PluginError`, `PluginEntry`, `RegistryToml`, `WasmSandboxConfig`
- **judge/**: `JudgeError`, `JudgeClient`, `Judge`, `Question`, `JudgeRequest`, `TypedAnswer`, `JudgeUsage`, `JudgeResponse`
- **cost/**: `ModelTier`, `CostPreset`, `CostBreakdown`, `NeedsProDetector`, `ResultCompressor`, `CostController`
- **debug/**: `DebugEntry`, `SessionDebug`, `DebugStats`, `DebugError`
- **scheduler/**: `SchedulerShared`, `Scheduler`

## 大文件 Top 15

| 文件 | 行数 | pub fn | 测试 |
|------|-----:|-------:|-----:|
| `tools/builtin.rs` | 2,787 | 20 | 10 |
| `tui/mod.rs` | 2,397 | 14 | 9 |
| `edu/store.rs` | 2,225 | 57 | 21 |
| `core/config.rs` | 2,208 | 10 | 18 |
| `knowledge/store.rs` | 1,454 | 28 | 19 |
| `agent/router.rs` | 1,433 | 7 | 0 |
| `agent/session.rs` | 1,073 | 12 | 3 |
| `lib.rs` | 1,060 | 4 | 0 |
| `edu/runtime.rs` | 1,042 | 28 | 3 |
| `channel/wechat/mod.rs` | 1,016 | 3 | 6 |
| `edu/mod.rs` | 990 | 2 | 0 |
| `api/mod.rs` | 986 | 11 | 6 |
| `agent/skill.rs` | 956 | 29 | 11 |
| `init.rs` | 877 | 1 | 0 |
| `agent/memory.rs` | 825 | 23 | 10 |
