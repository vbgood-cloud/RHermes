# 统一插件系统（`src/plugin/`）— P28

> 4 个文件 · 729 行。`Plugin` 是比 `Tool` 更高级的抽象：带**描述符 / 来源 / 健康检查 / 热重载**。
> 代码：`plugin/mod.rs`（trait + Router + 全局单例 + 加载）· `plugin/types.rs`（类型）·
> `plugin/extism_adapter.rs`（Wasm）· `plugin/skill_md_adapter.rs`（Markdown 技能）
> 配置：`[wasm]`（plugins_dir / max_memory / timeout_ms）

## 四种来源（`PluginSource`）

| 来源 | 说明 | 是否经 PluginRouter |
|------|------|--------------------|
| `Wasm { path }` | Extism 沙盒（`.wasm`），经 `WasmPluginTool` + host functions | ✅ |
| `SkillMd { path }` | `SKILL.md` 上下文注入 | ✅ |
| `Native` | Rust 原生内置工具 | ❌ **直挂 `ToolRegistry`，不重复包装** |
| `Mcp { server }` | MCP 远程工具 | ❌ **经 `McpRemoteTool` 自动注册** |

> 因此 `registry.toml` 里写 `type = "native"` / `"mcp"` 会**报错**（明确提示「无需在 registry.toml 配置」）。

## `Plugin` trait

```rust
#[async_trait]
pub trait Plugin: Send + Sync {
    async fn descriptor(&self) -> PluginDescriptor;
    async fn execute(&self, input: &Value) -> Result<PluginOutput, PluginError>;
    async fn health(&self) -> Result<bool, PluginError>;
    async fn reload(&self) -> Result<(), PluginError>;   // 默认不支持 → Err
}
```

`PluginDescriptor { name, description, source, parallel_safe, version? }`
`PluginOutput { success, output, error? }`（`PluginOutput::ok(..)` / `::err(..)`）
`PluginError { Config | NotFound | ExecutionFailed | Extism }`

## `PluginRouter`

进程内 `Arc<RwLock<HashMap<String, Arc<dyn Plugin>>>>`，经 `OnceLock` 全局共享（`global_router()`）。

| 方法 | 说明 |
|------|------|
| `register(plugin)` | 按 `descriptor().name` 注册，**重名覆盖** |
| `call(name, input)` | 未找到 → `PluginError::NotFound` |
| `list_descriptors()` | 并行取所有描述符，**按 name 排序** |
| `count()` / `health_all()` | 数量 / 逐插件健康检查（失败视作 `false`） |

## 加载（`load_registry`）

优先顺序：**`registry.toml` 显式声明 > 目录自动扫描**。

- **有 `registry.toml`**：只加载 `enabled = true` 的条目；加载失败仅 warn
- **无 `registry.toml`**：扫描 `plugins/` 目录，`*.wasm`（**无权限声明 = 最小权限**）+ `*.md`

`plugins/` 目录不存在 → 静默返回 0（可选配置）。

### `registry.toml` 格式

```toml
[[plugins]]
type  = "wasm"                  # wasm | skill_md | skill
path  = "echo.wasm"             # 相对 plugins/ 或绝对路径
enabled = true                  # 默认 true
allowed_hosts = ["api.example.com"]   # wasm：网络白名单
allowed_paths = ["/data"]             # wasm：文件白名单前缀
allow_exec    = false                 # wasm：子进程执行（默认 false）
name_override = "echo"                # 可选：覆盖从文件提取的名称
tags = ["demo"]
```

Wasm 沙盒默认参数（`WasmSandboxConfig::default()`）：`max_memory = 32 MiB`、`timeout_ms = 30_000`。

## 集成点

`tools/builtin.rs::full_registry()`：

1. **`[wasm].enabled`** → `wasm_plugin::load_plugins()` 把 Wasm **直挂为 Tool**（v0.7.0 旧路径，向后兼容）
2. **始终**调用 `plugin::init_plugin_router(&config.wasm.plugins_dir, &sandbox)`；
   仅当加载到 **> 0 个插件**时才注册 **`run_plugin`** 工具（`registry.register(RunPlugin)`）

`run_plugin` 工具（`builtin.rs:2443`）：
- `name = "__list__"` → 列出全部插件（`- 名称 [来源] v版本: 描述`）
- 否则 → `router.call(name, input)`；`input` 允许传 JSON 字符串（解析失败则包成 `{"raw": ...}`）

## 已知边界与重复点

- ⚠️ **两条 Wasm 加载路径并存**：`[wasm].enabled` 的直挂 `Tool`（`wasm_plugin.rs`）与 `PluginRouter`（`registry.toml`）**互不知晓**；同名插件可能被加载两次（一次作为工具、一次作为插件）。`max_memory`/`timeout_ms` 取自 `[wasm]` 段，两条路径共用。
- **`run_plugin` 是唯一入口**：不加载到插件时该工具**根本不存在**，Agent 无法感知插件系统。
- **`reload()` 基本未用**：默认实现返回 `Err`，两个适配器的热重载能力有限（见对应文件）。
- **无插件级并发限制**：`parallel_safe` 只是描述符字段，是否并行由 `ToolDispatcher` 对 `run_plugin` 这一工具的判断决定。
