# CHANGELOG

## v0.7.0 — 2026-08-18

### 新增
- **Extism Host Functions 安全网关**（P28 核心）：`src/tools/wasm_host_functions.rs`
  - 6 个宿主函数：`host_log`（永远注册）/ `host_http_get` / `host_http_post` / `host_read_file` / `host_write_file` / `host_exec`（默认禁用）
  - 每插件权限声明 `<name>.host.toml`（allowed_hosts / allowed_paths / allow_exec），无声明 = 最小权限纯计算
  - 域名白名单支持 `*` / `*.example.com`；路径白名单 canonicalize 防 `../` 逃逸
  - Manifest 强制内存上限 + 超时；Wasm execute 改走 `spawn_blocking`
- **统一 Plugin 系统**（P28，commit cc5c39e）：`src/plugin/` 四文件
  - `Plugin` trait：`descriptor()` / `execute()` / `health()` / `reload()`
  - `PluginRouter`：RwLock 注册表 + 全局 OnceLock 单例
  - `ExtismPlugin`（复用 WasmPluginTool + host functions）与 `SkillMdPlugin`（SKILL.md frontmatter）两种适配器
  - `plugins/registry.toml` 显式声明优先（per-plugin 权限），无配置时目录扫描（向后兼容）
  - `run_plugin` 内置工具：`__list__` 列出插件，Agent 可直接调用
- **MCP P2 协议扩展**
  - `notifications/tools/list_changed` 自动热刷新（tools 改 `Arc<RwLock>`，后台 task 重拉 tools/list）
  - `resources/list` + `resources/read` 支持（`McpResourceInfo` / `list_resources()` / `read_resource()`）
- Wasm 插件 `load_with_access()`：权限显式注入（registry.toml 场景）

### 变更
- 版本 0.6.9 → 0.7.0
- extism 依赖确认实际解析 1.30.0（Cargo.toml 声明 "1.5"）
- `McpAdapter::tools()` 返回 `Vec<McpToolInfo>` 快照（原 `&[McpToolInfo]`），支持热更新后读取

### 测试
- 246 → 267（+21：host functions 权限 7 + Plugin 系统 11 + 其他 3），全部通过

## v0.2.0 — 2026-06-02

### 新增
- **15 个内置工具**：read_file / write_file / search_content / run_command / glob / get_current_time / web_search / web_fetch / delegate_task / run_skill / skill_list / skill_search / skill_create / skill_patch / read_pdf
- **三段式 Context 架构**：stable + volatile + history，最大化 prefix cache 命中率
- **Context 自动压缩**：80% 阈值自动触发 6 段结构化摘要
- **记忆系统**：SQLite + FTS5 全文搜索，自动召回/写入
- **技能引擎**：Markdown playbook，CRUD，使用统计，进化建议
- **子 Agent 系统**：delegate_task 委托独立 Agent 执行
- **会话持久化**：Ctrl+Q 保存，-r 恢复
- **调试系统**：SessionDebug + rhermes debug export
- **输入排队**：响应期间可输入，自动排队等待
- **search_content 改用 ripgrep 库**：自动跳过二进制/.gitignore
- **配置化**：DisplayConfig / DebugConfig / AgentConfig
- **系统提示词**：14 个工具列表 + 自进化规则

### 修复
- 子进程 stdin 抢占 TUI 键盘输入
- UTF-8 字符边界 panic（truncate 函数）
- 工具结果截断过小（2000→15000 字符）
- 模型重复调用同一工具

### 变更
- 版本 0.1.0 → 0.2.0
- 仅支持便携式模式
- 添加 /version 命令

### 测试
- 119 个单元测试，全部通过

---

## v0.1.0 — 2026-05-30

初始版本：项目骨架 + PathManager + 基础 TUI
