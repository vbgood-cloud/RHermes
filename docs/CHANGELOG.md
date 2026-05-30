# CHANGELOG

## v0.3.0 — 2026-05-30

### 🚀 里程碑 1.3：并行工具调度系统

**新增模块：**
- `src/tool.rs` — 工具注册表 + `Tool` trait + `ToolCall`/`ToolResult`/`ToolError` 类型系统
- `src/tools.rs` — 5 个内置工具实现（read_file, search_content, glob, write_file, run_command）
- `src/dispatcher.rs` — 并行调度器（tokio::JoinSet + Semaphore 限流）

**核心设计：**
- 每个工具声明 `parallel_safe` 标志 —— 读操作并行，写操作串行
- 调度器保持原始调用顺序：连续的 parallel_safe 工具组成并行批次（JoinSet）
- 串行工具遇到时先排空当前并行批次再执行，保证数据依赖正确
- `ToolRegistry` 启动时注册，运行期只读，`Arc<HashMap>` 零开销共享
- 信号量（Semaphore）控制最大并行度，防止资源耗尽

**内置工具清单：**

| 工具 | parallel_safe | 功能 |
|------|:---:|------|
| `read_file` | ✅ | 读取文件，支持 head/tail/range |
| `search_content` | ✅ | 文本搜索（rg 后端） |
| `glob` | ✅ | Glob 文件匹配（fd 后端） |
| `write_file` | ❌ | 写入文件（自动创建父目录） |
| `run_command` | ❌ | Shell 命令执行（带超时） |

**验证：**
- `cargo check` — ✅ 零错误
- `cargo test` — ✅ **51/51 通过**

---

## v0.2.0 — 2026-05-30

### 🚀 里程碑 1.2：DeepSeek API 集成 + 三段式 Context

**新增模块：**
- `src/config.rs` — 配置加载模块（TOML），支持 API Key / 模型 / base_url / 超时 / 重试
- `src/context.rs` — 三段式 Context 架构（Immutable Prefix + Append-Only Log + Volatile Scratch）
- `src/api.rs` — DeepSeek API 客户端，支持 SSE 流式响应 + 错误处理 + HTTP 重试策略

**核心能力：**
- 三段式 Context 确保 byte 级 prefix 稳定，最大化 DeepSeek prefix cache 命中率
- SSE 流式解析，逐 chunk 显示在 TUI 中
- 非流式 `chat()` + 流式 `chat_stream()` + 自动重试 `chat_with_retry()`
- API 成本自动换算（按 DeepSeek v4-flash/v4-pro 费率）
- 配置保存/加载到 `config.toml`，自动创建默认配置
- TUI 后台任务通过 tokio channel 与 API 异步通信

**TUI 升级：**
- 输入回车后自动调用真实 API（如有配置 API Key）
- 流式响应实时渲染在对话主面板
- 底部统计栏显示 `$成本/轮` | `$累计` | `缓存命中率%`
- 未配置 API Key 时自动降级为模拟模式

**验证：**
- `cargo check` — ✅ 零错误
- `cargo test` — ✅ **34/34 通过**

---

## v0.1.0 — 2026-05-30

### 🎉 初始里程碑：项目骨架 + 双模部署

**新增：**
- `Cargo.toml` — Rust 2024 edition 项目配置
- `src/main.rs` — 入口：部署模式检测 → 目录创建 → 启动信息
- `src/path.rs` — `PathManager`：可移动模式 / 传统模式自动检测
- `README.md` — 6 大核心优势 + 部署策略 + 架构图 + 技术选型 + 开发路线
- `docs/plans.md` — 完整开发计划存档 + 架构决策记录
- `docs/CHANGELOG.md` — 变更日志

**核心能力：**
- 可移动部署：`<exe_dir>/home/` 目录自动识别，全部数据随身带走
- 传统部署：无 `home/` 时自动降级到系统标准路径
- 路径管理 API：`config_path()` / `memory_db_path()` / `skills_dir()` / `sessions_dir()` / `logs_dir()` / `cache_dir()`
- 惰性目录创建：`ensure_dirs()` 首次使用时自动建好目录结构

**验证：**
- `cargo check` — ✅ 零警告
- `cargo test` — ✅ 4/4 通过
  - `test_portable_mode_detection`
  - `test_traditional_mode_fallback`
  - `test_sub_paths`
  - `test_ensure_dirs`
- 运行输出 — ✅ 正确显示部署模式和 data_root
