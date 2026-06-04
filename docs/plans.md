# RHermes 开发计划存档

> 本文件记录项目里程碑状态与架构决策。

---

## 📊 进度总览

| 阶段 | 模块 | 状态 |
|------|------|------|
| 基础架构 | 项目骨架 + 路径管理器 | ✅ |
| | DeepSeek API 集成 + 三段式 Context | ✅ |
| | 17 个内置工具 + 并行调度 | ✅ |
| | Tool-Call Repair Pipeline | ✅ |
| | 成本控制体系 | ✅ |
| | Agent Loop + TUI 交互优化 | ✅ |
| 自进化层 | 记忆系统 (SQLite+FTS5) | ✅ |
| | 技能引擎 (Markdown) | ✅ |
| | 子 Agent 系统 | ✅ |
| | 调试系统 + debug export | ✅ |
| 优化 | search_content 改用 ripgrep 库 | ✅ |
| | 输入排队 | ✅ |
| | 配置化 (Display/Debug/AgentConfig) | ✅ |

---

## 核心架构

### 三段式 Context

```
stable layer:     persona + tools + skills（session 内不变）
volatile layer:   时间 + USER.md + AGENTS.md（session 开始时冻结）
history layer:    对话日志 + 工具结果（append-only）
```

### Agent Loop

```
用户输入 → Context 构建 → API 调用 → 解析响应
    ↓                              ↓
  工具结果写回 ← 工具调度 ← 有 tool_calls?
    ↓                              ↓
  无 → 显示回复 → 记忆写入 → 结束
```

---

## 技术栈

| 模块 | 选型 |
|------|------|
| 语言 | Rust 2024 edition |
| 异步 | tokio |
| HTTP | reqwest (json+stream) |
| TUI | ratatui + crossterm |
| 数据库 | rusqlite + FTS5 |
| 搜索 | grep-regex + grep-searcher |
| PDF | pdf-extract |
| 配置 | TOML + serde |
| 日志 | tracing + tracing-subscriber |
| CLI | clap (derive) |
| 序列化 | serde + serde_json |

---

## 架构决策记录

### ADR-001：便携式模式
**状态**：✅ 已实现  
**决策**：仅支持便携式模式，所有数据保存在 `./home/` 目录  
**理由**：单文件部署 + 随带随走，零配置

### ADR-002：Rust 2024 edition
**状态**：✅ 已采纳  
**决策**：使用 Rust 2024 edition（1.94+）

### ADR-003：三段式 Context
**状态**：✅ 已实现  
**决策**：stable + volatile + history，最大化 DeepSeek prefix cache

### ADR-004：tokio channel 异步通信
**状态**：✅ 已实现  
**决策**：mpsc channel 桥接 TUI ↔ API

### ADR-005：工具系统 async_trait + parallel_safe
**状态**：✅ 已实现  
**决策**：每个工具声明 parallel_safe，读并行写串行

### ADR-006：17 个内置工具
**状态**：✅ 已实现  
**决策**：文件/搜索/网络/系统/技能/记忆 工具

### ADR-007：config.toml 配置化
**状态**：✅ 已实现  
**决策**：TOML 格式，嵌套配置节（api/request/memory/display/debug/agent）
