# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

RHermes = Reasonix + Hermes，Rust 版自进化终端 AI 编程 Agent。核心卖点：DeepSeek 前缀缓存优化、自进化技能引擎、多通道接入（TUI/微信/企微/Telegram）、多 Provider 熔断。

## 构建与开发命令

```bash
cargo build --release        # release 构建
cargo test                   # 运行全部测试（~173 个）
cargo test tool_name         # 运行单个测试（按名称过滤）
cargo clippy                 # lint 检查
cargo fmt                    # 格式化代码
```

应用命令：
```bash
rhermes init                 # 交互式初始化向导
rhermes                      # TUI 交互模式
rhermes --resume             # 恢复上次会话
rhermes gateway start        # 守护进程模式
rhermes config init          # 生成配置模板
rhermes config check         # 检查配置完整性
```

## 项目结构

单体 Rust 2024 项目（非 workspace），`src/` 下按功能分模块：

| 模块 | 职责 |
|------|------|
| `core/` | 基础设施：Config 解析、三段式 Context、PathManager、PrefixCacheManager、HttpClientFactory（代理感知） |
| `agent/` | 核心大脑：AgentSession（Agent Loop）、MemorySystem、SkillEngine、SessionRouter、Curator |
| `api/` | LLM API 客户端（OpenAI 兼容格式） |
| `provider/` | Provider Pool + 熔断器 + 加权轮询 |
| `tools/` | 22 个内置工具 + ToolDispatcher 并行调度 |
| `tools/office/` | Office 文档处理：Excel(calamine+rust_xlsxwriter) / Word(docx-rs) / PPTX(zip+quick-xml) |
| `tui/` | ratatui 终端 UI |
| `channel/` | 多通道：TUI / 微信(wechat/) / 企微(wecom/) / Telegram(telegram/) |
| `mcp/` | MCP 客户端（JSON-RPC stdio/SSE） |
| `gateway/` | 守护进程模式 |
| `scheduler/` | Cron 定时任务调度器 |

## 核心架构

**请求流转**: Channel → SessionRouter（按用户分流）→ AgentSession → ProviderPool → LLM API

**三段式 Context**（`core/context.rs`）:
- `Immutable Prefix` — 系统提示词，不变，命中前缀缓存
- `Append Log` — 对话历史，追加写入
- `Scratch` — 临时内容

达 80% 阈值自动压缩历史。

**工具系统**（`tools/`）:
- 每个工具实现 `Tool` trait（`execute` + `parallel_safe` 标记）
- `ToolDispatcher` 用 `JoinSet` 并发调度标记为 `parallel_safe` 的工具
- MCP 远程工具通过 `mcp__` 前缀无缝融入 ToolRegistry

**Provider Pool**（`provider/`）:
- 多 Provider 加权轮询，连续失败触发熔断，自动摘除不健康节点

**技能引擎**（`agent/skill.rs`）:
- Markdown Playbook 格式，自动从对话中提炼技能
- Curator 管理生命周期：active → stale → archived

**WASM 插件**（`plugins/`）:
- Extism 框架，导出函数：`info_name`、`info_description`、`info_parameters`、`execute`

## 配置与路径

**配置分离**:
- `config.toml` — 非敏感配置（模型/地址/超时），与可执行文件同目录
- `.env` — 敏感配置（`DEEPSEEK_API_KEY=sk-xxx`），与可执行文件同目录

**部署模式**（`core/path.rs` 中 PathManager）:
- **可移动模式**: 所有数据保存在可执行文件旁的 `home/` 目录（skills/memories/sessions/logs/cache/workspace）

## 代码风格

- 中文注释（`//!` 模块文档 + `//` 行注释）
- 全部 `tokio` 异步
- 错误处理：自定义错误枚举 + `String`，各模块自定义
- 工具并行安全通过 `Tool::parallel_safe()` 标记
- 测试用 `#[cfg(test)] mod tests` 内联
- 模块导出通过 `mod.rs` 统一 `pub use` 重导出
- `clap` derive 宏定义 CLI 子命令

## 开发规范
1.当生成一个二进制文件后，把这个二进制文件拷贝到E:\ai\new\目录下。如果拷贝的的二进制程序正在执行，那么在进程中关掉它再进行拷贝。