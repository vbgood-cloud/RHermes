# RHermes 开发计划存档

> 本文件记录项目里程碑状态与架构决策。
>
> ⚠️ 本文成文于 v0.7.0 时代。**当前实现状态请以 [`docs/architecture.md`](architecture.md)
> 与 [`docs/codegraph.md`](codegraph.md) 为准**（2026-10-02 已按代码校正工具数 / 配置段 / 成本控制描述）。

---

## 📊 进度总览

| 阶段 | 模块 | 状态 |
|------|------|------|
| 基础架构 | 项目骨架 + 路径管理器 | ✅ |
| | DeepSeek API 集成 + 三段式 Context | ✅ |
| | 内置工具 + 并行调度 | ✅ 现 **33 个**（含 kb_* 8 / Office 5 / 文档解析 3） |
| | Tool-Call Repair Pipeline | ⚠️ 三工序生效（Scavenge/Truncation/Storm）；**Flatten 未接入生产路径** |
| | 成本控制体系 | ⚠️ `cost.rs` 已实现但**全项目零引用**（未集成） |
| | Agent Loop + TUI 交互优化 | ✅ |
| 自进化层 | 记忆系统 (SQLite+FTS5) | ✅ |
| | 技能引擎 (Markdown) | ✅ |
| | 子 Agent 系统 | ✅ |
| | 调试系统 + debug export | ✅ |
| 优化 | search_content 改用 ripgrep 库 | ✅ |
| | 输入排队 | ✅ |
| | 配置化 (Display/Debug/AgentConfig) | ✅ 现为 **19 个配置段** |
| 平台化 | 多渠道接入（TUI/微信/企微/TG/QQ/Web） | ✅ 4 稳定 + QQ 未收尾 + Web 半成品 |
| | Gateway 守护进程 + cron 定时任务 | ✅ |
| | MCP 客户端（stdio/SSE/Direct 三传输） | ✅ |
| | 统一 Plugin 系统（Extism Wasm + SKILL.md） | ✅ |
| 教学与学习 | `/learn` 知识库学习（图谱/遗忘曲线/Bento/导入导出） | ✅ v0.7.1–v0.7.8 |
| | edu 去中心化教学班（iroh P2P + 多老师） | ✅ v0.7.9–v0.7.16 |
| | Jev 判断层（D18 P0–P3） | ✅ v0.7.18–v0.7.22 |

---

## 核心架构

### 三段式 Context

```
immutable_prefix:  system + tool_specs + few_shots（一次计算、哈希、锁定）
append_only_log:   [assistant][tool][assistant]…（单调递增，不重写）
volatile_scratch:  思考/计划/临时状态（每轮重置，蒸馏后才进 Log）
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

### ADR-006：内置工具
**状态**：✅ 已实现（当前 **33 个**）  
**决策**：文件/搜索/网络/系统/技能/记忆/**知识库 kb_***/**Office**/**文档解析** 工具

### ADR-007：config.toml 配置化
**状态**：✅ 已实现  
**决策**：TOML 格式，嵌套配置节。当前 **19 段**：
`api` / `providers` / `request` / `memory` / `debug` / `display` / `agent` / `provider_pool` /
`channels` / `proxy` / `gateway` / `scheduler` / `wasm` / `mcp` / `search` / `edu` /
`liteparse` / `knowledge` / `jev`

### ADR-008：可移动模式锚点 = exe 目录
**状态**：✅ 已实现  
**决策**：`PathManager::detect()` 取 `current_exe()` 所在目录，数据根 = `<exe_dir>/home`；
因此**换目录 = 换整套数据**（这也是 USB 可运行的原因）。无 `--config` 参数。

### ADR-009：判断层纯增益、失败即回退（D18）
**状态**：✅ 已实现  
**决策**：Jev 判断层只做「加分项」—— 任何失败 / 置信不足一律回退启发式，
绝不因判断层不可用而改变行为（所有错误路径吞成 `None`，无 panic）。

### ADR-010：edu 采用 iroh 双 ALPN + 签名白名单
**状态**：✅ 已实现（v0.7.9+）  
**决策**：`/class-auth/1.0`（开放认证）+ `/class-app/1.0`（受控业务）双 ALPN；
成员表由教师 Ed25519 签名（`ALLOWLIST_VERSION = 1`），纪元单调防回放；
撤销 = 落库 + 轮换 epoch + `TopicRotate` + 新表签名广播。
