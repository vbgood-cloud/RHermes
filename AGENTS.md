# RHermes — 终端 AI 编程 Agent（Rust）

DeepSeek API 驱动的终端 AI Agent，三段式 Context 缓存优化、并行工具调度、长期记忆与自主技能进化。

- **入口**: `src/main.rs` → `run_code()` 引导 → `tui::App::run()` 主循环
- **部署**: 便携模式 (`home/` 目录旁) / 传统模式 (`%APPDATA%/rhermes`)，自动检测，绿色部署
- **配置**: `config.toml`(非敏感) + `.env`(API Key)

## Commands

| 命令 | 说明 |
|------|------|
| `cargo build` | 编译 debug |
| `cargo build --release` | 编译 release |
| `cargo run` | 直接运行（自动检测配置，无配置则进初始化向导） |
| `cargo run -- init` | 初始化向导（API Key + 模型） |
| `cargo run -- debug export [session-id]` | 导出调试报告 |
| `cargo test` | 运行全部 119+ 个单元测试 |

## Architecture

```
src/
├── main.rs           CLI 入口 (clap), 引导初始化/运行
├── core/             基础设施
│   ├── config.rs     TOML + .env 配置加载
│   ├── context.rs    三段式 Context (prefix/log/scratch)
│   └── path.rs       PathManager (portable/traditional 绿色模式)
├── agent/            智能体逻辑
│   ├── memory.rs     长期记忆 (SQLite+FTS5)
│   ├── memory_manager.rs   MemoryProvider trait + 多 provider 路由
│   ├── skill.rs      技能引擎 (Markdown 文件)
│   ├── curator.rs    技能生命周期管理 (active→stale→archived)
│   ├── repair.rs     Tool-Call 修复流水线 (flatten/scavenge/truncation/storm)
│   └── task.rs       子 Agent 系统
├── api/              DeepSeek API 客户端 (同步/流式, 自动重试)
├── tools/            工具系统
│   ├── registry.rs   注册表 + Tool trait + 参数定义
│   ├── builtin.rs    17 个内置工具实现 (~3500 行)
│   └── dispatcher.rs 并行调度器 (parallel-safe vs serial)
├── tui/mod.rs        ratatui 终端 UI (~2000 行)
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
- **数据目录**: 便携模式 `home/`，传统模式 OS 标准目录；所有路径通过 `PathManager` 获取
- **全局状态**: `tools::set_global_*()` 函数设置单例（config/skill_engine/display_config）
- **性能**: 大文件只读头部/尾部；`read_file` 受 `max_chars` 限制；Context 自动压缩

## Notes

<!-- 临时记录、待办、快速笔记放在这里 -->
