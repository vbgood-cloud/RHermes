# RHermes — 终端 AI 编程 Agent（Rust）

DeepSeek API 驱动的终端 AI Agent，三段式 Context 缓存优化、并行工具调度、长期记忆与自主技能进化；附带教育版（P2P 去中心化教学班）。

- **入口**: `src/lib.rs` 提供三个应用入口 `run_default_app()`（通用）/ `run_student_app()`（学生端）/ `run_teacher_app()`（教师端），对应 `src/main.rs` 与 `src/bin/rhermes_{stu,teacher}.rs`
- **部署**: 可移动模式 (`home/` 目录旁)，绿色部署
- **配置**: `config.toml`(非敏感) + `.env`(API Key)

## Commands

| 命令 | 说明 |
|------|------|
| `cargo build` | 编译 debug |
| `cargo build --release` | 编译 release |
| `cargo test` | 运行全部 410+ 个单元测试 |

### 应用命令（三个 bin 共享通用子命令，各有特有命令）

| 命令 | 说明 |
|------|------|
| `rhermes` / `rhermes-stu` / `rhermes-teacher` | TUI 交互模式（`--resume` 恢复上次会话） |
| `rhermes init` | 初始化向导（API Key + 模型） |
| `rhermes config init/check/save` | 生成模板 / 检查完整性 / 保存带注释配置 |
| `rhermes gateway setup/start/stop/status` | 守护进程模式（`channel list/enable/disable` 管通道启停） |
| `rhermes mcp setup/list/remove/import` | MCP Server 管理 |
| `rhermes debug export [session-id]` | 导出调试报告 |
| `rhermes edu student/teacher ...` | 教育模式（透传给 `edu::handle_edu`） |
| `rhermes-teacher course/class/lesson/student/roster` | 课程 / 班级 / 课次 / 学生 / 花名册管理 |
| `rhermes-teacher dashboard/serve/addr/init-teacher` | 仪表板 / 多班托管 / 打印老师地址 / 初始化身份 |
| `rhermes-stu login/join/status/courses/profile/report/mode/live` | 学生学习流程 |

## Architecture

```
src/
├── lib.rs            库入口：CLI 定义 (clap) + 三个应用入口
├── main.rs           通用 bin（转发 run_default_app）
├── bin/              rhermes_stu.rs / rhermes_teacher.rs
├── core/             基础设施
│   ├── config.rs     TOML + .env 配置加载
│   ├── context.rs    三段式 Context (prefix/log/scratch)
│   ├── path.rs       PathManager (可移动模式)
│   ├── prefix_cache.rs  前缀缓存管理器 (DeepSeek prefix cache)
│   └── http_client.rs   代理感知 HTTP 客户端工厂
├── agent/            智能体逻辑
│   ├── session.rs    AgentSession (Agent Loop, TUI/Gateway 共用)
│   ├── router.rs     SessionRouter (按用户分流 + edu 切课)
│   ├── memory.rs     长期记忆 (SQLite+FTS5)
│   ├── memory_manager.rs   MemoryProvider trait + 多 provider 路由
│   ├── skill.rs      技能引擎 (Markdown 文件)
│   ├── curator.rs    技能生命周期管理 (active→stale→archived)
│   ├── repair.rs     Tool-Call 修复流水线 (flatten/scavenge/truncation/storm)
│   ├── guardrails.rs 护栏（风暴抑制）
│   └── task.rs       子 Agent 系统
├── api/              DeepSeek API 客户端 (同步/流式, 自动重试)
├── provider/         Provider Pool + 熔断器 + 加权轮询
├── tools/            工具系统（共 33 个内置工具）
│   ├── registry.rs   注册表 + Tool trait + 参数定义
│   ├── builtin.rs    内置工具实现 + kb_* 知识库工具
│   ├── dispatcher.rs 并行调度器 (parallel-safe vs serial, 按 call_id 对齐)
│   ├── office/       Excel(calamine+rust_xlsxwriter) / Word(docx-rs) / PPTX(zip+quick-xml)
│   ├── search/       多引擎搜索 (duckduckgo/searxng/baidu/bing/serper)
│   └── liteparse.rs  文档解析 / 截图 / 复杂度检查
├── knowledge/        知识库学习系统 (SQLite store / 图谱 svg / bento 面板 / SM-2 间隔复习)
├── edu/              教育版：P2P 教学班 (iroh+gossip+blobs)、师生认证、授权白名单、反思闭环、教师仪表板
├── channel/          多通道: TUI / 微信 / 企微 / Telegram / QQ / Web
├── mcp/              MCP 客户端 (JSON-RPC stdio/SSE)
├── plugin/           WASM 插件宿主 (extism) + skill.md 适配器；插件文件放仓库根 plugins/
├── scheduler/        Cron 定时任务调度器
├── gateway/          守护进程模式
├── tui/mod.rs        ratatui 终端 UI (~2200 行)
├── cost.rs           成本控制 (model tiers / 压缩)
├── debug.rs          调试日志缓冲区
└── init.rs           交互式初始化向导
```

## Conventions

- **注释**: 中文 `//!` 模块注释 + `//` 代码注释；段落分隔使用 `// ----...`
- **错误处理**: 自定义错误枚举 + `String`，使用 `map_err`/`map_err(|e| ...)` 转换
- **并行安全**: 工具通过 `parallel_safe()` 标记，dispatcher 据此分组调度
- **异步**: 全部 `tokio`；标准模式：`#[tokio::main]` + `async fn`
- **测试**: `#[cfg(test)] mod tests { ... }` 内联在文件底部；使用 `tempfile` 做 IO 测试
- **模块导出**: `mod.rs` 统一 `pub use` 重导出，模块内 `mod` 声明
- **数据目录**: 可移动模式 `home/`；所有路径通过 `PathManager` 获取
- **全局状态**: `tools::set_global_*()` 函数设置单例（config/skill_engine/display_config）
- **性能**: 大文件只读头部/尾部；`read_file` 受 `max_chars` 限制；Context 自动压缩

## Notes

<!-- 临时记录、待办、快速笔记放在这里 -->
- D17: 分层网络代理方案 — 详见 [`docs/decisions/D17-proxy.md`](docs/decisions/D17-proxy.md)
- D18: Jev 决策模型集成方案（判断层） — 详见 [`docs/decisions/D18-jev-judge.md`](docs/decisions/D18-jev-judge.md)

### 构建产物部署（每次编译后必须执行）

编译成功后（`cargo build --release`），把生成的三个二进制分别拷贝到对应目录：

| 产物 | 部署目录 | 入口 |
|------|----------|------|
| `target/release/rhermes.exe` | `e:\ai\new\` | 通用模式 |
| `target/release/rhermes-stu.exe` | `e:\ai\stu\` | 学生端（`run_student_app`） |
| `target/release/rhermes-teacher.exe` | `e:\ai\teacher\` | 教师端（`run_teacher_app`） |

```bash
cd /e/lab/RHermes && cargo build --release
cp target/release/rhermes.exe /e/ai/new/
mkdir -p /e/ai/stu /e/ai/teacher
cp target/release/rhermes-stu.exe /e/ai/stu/
cp target/release/rhermes-teacher.exe /e/ai/teacher/
```

- 文件被占用时先 `taskkill //F //IM <进程名>.exe`（Windows 下 taskkill 用双斜杠语法）再拷贝
- 三个目录各自是独立的可移动模式部署（各自有 `config.toml` + `.env` + `home/`）
- 学生端/教师端的 `[channels.telegram]`、`[channels.wechat]` 保持 `enabled = false`，避免与 `e:\ai\new` 的 gateway 抢同一个 bot 的 getUpdates
