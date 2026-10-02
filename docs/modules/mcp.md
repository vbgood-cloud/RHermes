# MCP 客户端（`src/mcp/`）

> 8 个文件 · 1824 行。把远程 MCP Server 的工具**无缝包装成 `Tool`**，纳入 `ToolRegistry` + `ToolDispatcher`。
> 代码：`transport.rs`（stdio）· `sse_transport.rs`（SSE + Direct + Wrapper）· `adapter.rs`（连接/调用/健康）·
> `tool_wrapper.rs`（`McpRemoteTool`）· `setup.rs`（交互式配置）· `import.rs`（JSON 导入）
> 配置：`[mcp]`（`core/config.rs::McpConfig` / `McpServerConfig`）

## 三种传输（`McpTransportWrapper`）

| 变体 | 实现 | 触发条件 |
|------|------|---------|
| `Stdio` | `McpTransport` | `command` 有值 → 启动**子进程**，JSON-RPC over stdin/stdout |
| `Sse` | `McpSseTransport` | `url` 有值且为 SSE 端点 |
| `Direct` | `McpDirectTransport` | SSE 不可用，改用 `message_url` 发 HTTP 请求 |

`McpTransport` 要点：
- **ID 路由**：`next_id` 自增 + `pending: HashMap<u64, oneshot::Sender>`；`_read_task` 从 stdout 逐行读 JSON-RPC 并按 id 派发
- 支持 Server 在响应之间**主动发通知**（`notification_rx`）与**主动取消**（`cancel_request(id, reason)`）

## 配置

```toml
[mcp]
enabled = false                    # 默认关
sampling_enabled = false           # 默认关（安全敏感）
sampling_max_tokens = 1024
# 注意：servers 是表，用 [mcp.servers.<名>] 声明

[mcp.servers.filesystem]
command = "npx"                                   # 有 command → stdio
args = ["-y", "@modelcontextprotocol/server-filesystem", "/data"]
env = { KEY = "value" }
server_type = ""                 # 留空自动推断：stdio / sse / http
parallel_safe = false            # Server 级并行安全
[mcp.servers.filesystem.tool_parallel_safe]      # 工具级覆盖（按工具名）
read_file = true

[mcp.servers.remote]
url = "http://localhost:3000/sse"                 # 有 url → SSE
message_url = "http://localhost:3000/message"     # SSE 不可用时改走 Direct
headers = { Authorization = "Bearer xxx" }
```

> 模式推断（`setup.rs`）：`command.is_some()` → stdio，否则按 `url` 走 SSE。

## `McpAdapter`

`connect(name, config)` → 建立传输 + `initialize` 握手 + `tools/list`（存 `Vec<McpToolInfo>` 快照）。

| 方法 | 说明 |
|------|------|
| `call_tool(name, args)` | 调 `tools/call`，返回文本结果 |
| `tools()` | 已发现的工具快照（`original_name` / `description` / `input_schema`） |
| `server_name()` / `server_name_owned()` | 名称 |
| `parallel_safe()` / `tool_parallel_safe()` | 并行安全配置（工具级优先） |
| `health_check()` / `reconnect()` | 健康检查 / 重连 |
| `cancel_request(id, reason)` / `set_log_level(level)` | `notifications/cancelled` / `logging/setLevel` |
| `list_resources()` / `read_resource(uri)` | MCP 资源（resources 能力） |
| `shutdown()` | 优雅关闭（含子进程） |

`McpAdapterManager`：`connect_all(&servers)` / `adapters()` / `shutdown_all()`。

## `McpRemoteTool`

- **工具全名 = `mcp__<server>__<tool>`**（避免与内置工具/其他 Server 撞名）
- `parallel_safe` 解析：`tool_parallel_config.get(原工具名).unwrap_or(server_parallel_safe)`

## 生命周期（`tools/builtin.rs::full_registry`）

1. 遍历 `[mcp].servers` 逐个 `McpAdapter::connect`：
   - 成功 → 记录到 `report.connected_servers`（含工具数），**每个工具注册一个 `McpRemoteTool`**
   - 失败 → 进 `report.failed_servers`，只 warn，**不影响其他 Server**
2. 把 manager 存入 `GLOBAL_MCP_MANAGER`（供退出时 `shutdown_mcp()` 优雅关闭）
3. 若 `server_count > 0`，起**后台健康检查任务**：首次等 **30s**，之后每 **300s（5 分钟）** 轮询；
   `health_check()` 失败 → 打日志并 `reconnect()`

CLI：`rhermes mcp setup`（交互式）/ `list` / `remove <名>` / `import [文件] [--force]`。

## 已知边界

- **配置改动需重启生效**：`servers` 只在 `full_registry` 时读取一次，`mcp import` / `mcp setup` 只写 `config.toml`，**没有热刷新**（运行中的进程不会重连）。
- **无工具级超时配置**：单个 `tools/call` 的超时未在配置里暴露。
- **`parallel_safe` 只影响调度器判断**，若 Server 声明有误可能导致并发调用互相干扰。
- **Sampling 默认关闭**：`sampling_enabled = false`（安全敏感，需显式开启，上限 `sampling_max_tokens`）。
- **`mcp/config.rs` 仅 3 行**（模块占位），真实配置类型在 `core/config.rs`。
