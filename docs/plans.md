# RHermes 开发计划存档

> 本文件记录 RHermes 项目的完整开发规划、里程碑状态与架构决策。
> 每次里程碑完成后更新。

---

## 📊 总体进度

| 阶段 | 里程碑 | 状态 | 完成日期 |
|------|--------|------|----------|
| **Phase 1** | 1.1 — 项目骨架 + 路径管理器 + 双模部署 | ✅ **已完成** | 2026-05-30 |
| | 1.2 — DeepSeek API 集成 + 缓存利用 | ✅ **已完成** | 2026-05-30 |
| | 1.3 — 并行工具调度 | ✅ **已完成** | 2026-05-30 |
| | 1.3 — 并行工具调度 | ⏳ 待开始 | — |
| | 1.4 — Tool-Call Repair Pipeline | ⏳ 待开始 | — |
| | 1.5 — 成本控制体系 | ⏳ 待开始 | — |
| **Phase 2** | 2.1 — 长期记忆系统 | ⏳ 待开始 | — |
| | 2.2 — 自主 Skill 生成与进化 | ⏳ 待开始 | — |
| | 2.3 — 跨会话检索 | ⏳ 待开始 | — |
| | 2.4 — 子 Agent 系统 | ⏳ 待开始 | — |
| | 2.5 — 消息网关 | ⏳ 待开始 | — |

---

## 一、Phase 1 — Reasonix 省 Token 核心引擎

### 里程碑 1.1 ✅ 已完成

**目标：** Rust 项目脚手架 + 三区 Context 架构 + 双模部署路径管理器

**完成内容：**

| 模块 | 文件 | 说明 |
|------|------|------|
| 项目骨架 | `Cargo.toml` | clap / tokio / reqwest / serde / ratatui / rusqlite 依赖声明 |
| 入口 | `src/main.rs` | 启动流程：检测模式 → 创建目录 → 打印启动信息 |
| 路径管理器 | `src/path.rs` | `PathManager` 双模检测 + 6 个子路径 + 目录惰性创建 |
| 项目文档 | `README.md` | 6 大优势 + 部署策略 + 架构图 + 技术选型 + 开发路线 |

**关键技术决策：**
- `PathManager::detect()` — 启动时 `std::env::current_exe()` 检测 `home/` 目录
- 可移动模式：`exe_dir/home/` 下保存全部数据
- 传统模式：平台标准路径（XDG / macOS Support / Windows APPDATA）
- 两种模式对上层透明，上层只调用 `path_mgr.xxx_path()`

**验证：** `cargo check` 零警告，`cargo test` 4/4 通过

---

### 里程碑 1.2 ✅ 已完成

**目标：** DeepSeek API 集成 + 三段式 Context 架构 + SSE 流式

**完成内容：**

| 模块 | 文件 | 说明 |
|------|------|------|
| 配置加载 | `src/config.rs` | TOML 配置，API Key / 模型 / base_url / 超时 / 重试 |
| 三段式 Context | `src/context.rs` | Immutable Prefix(Arc\<[u8]\>) + Append-Only Log + Volatile Scratch |
| API 客户端 | `src/api.rs` | reqwest SSE 流式 + 错误处理 + HTTP 重试策略 |
| TUI 集成 | `src/main.rs` + `src/tui.rs` | tokio channel 异步通信，流式实时渲染 |

**核心数据结构（已实现）：**

```rust
pub struct Context {
    immutable_prefix: Arc<[u8]>,   // session 内固定，被缓存
    append_only_log: Vec<u8>,      // 只追加，不重写
    scratch: Vec<Message>,         // 每轮重置，不发送到 API
}
```

**验收结果：**
- ✅ SSE 流式解析 —— StreamChunk 结构体 + data: 事件循环
- ✅ byte 级 prefix 稳定性 —— Arc<[u8]> 零拷贝共享
- ✅ 连续多轮 prefix 不变 —— 测试 `test_context_multiple_rounds_prefix_stable`
- ✅ HTTP 重试策略 —— 指数退避，5xx 可重试，4xx 不可重试
- ✅ 配置自动创建 —— 文件不存在时返回默认值

---

### 里程碑 1.3 ✅ 已完成

**目标：** 工具注册表 + parallel_safe 标志 + tokio::JoinSet 并行调度

**完成内容：**

| 模块 | 文件 | 说明 |
|------|------|------|
| 工具类型系统 | `src/tool.rs` | Tool trait + Registry + Call/Result/Error 类型 |
| 内置工具 | `src/tools.rs` | 5 个工具（read_file, search, glob, write_file, run_command） |
| 并行调度器 | `src/dispatcher.rs` | JoinSet 批次 + 顺序保持 + Semaphore 限流 |

**核心设计：**
- `parallel_safe: bool` —— 读操作并行（true），写操作串行（false）
- 调度器保持原始调用顺序：连续的 parallel_safe 组成并行批次
- 串行工具遇到时先排空当前并行批次，保证 write → read 的数据依赖
- `Semaphore` 限制最大并行数（默认 10），防止 FD 耗尽
- `JoinSet` 提供结构化并发，自动处理 task panic

**验收结果：**
- ✅ 两个 parallel_safe 工具并行运行 —— `test_dispatch_parallel_read_files`
- ✅ 串行工具在并行批次之前/之后正确执行 —— `test_dispatch_serial_write_then_read`
- ✅ 混合场景：parallel → serial → parallel 交错执行 —— `test_dispatch_mixed_parallel_and_serial`
- ✅ 并行限流：5 个文件同时读但 Semaphore 限制为 2 —— `test_dispatch_concurrency_limit`
- ✅ 未知工具优雅降级 + 空调用列表正确处理

---

### 里程碑 1.4 — Tool-Call Repair Pipeline（待开始）

**功能需求：**
- [ ] Flatten：参数嵌套过深时转 dot-notation
- [ ] Scavenge：从 reasoning_content 捞取丢失的 tool-call
- [ ] Truncation：检测不完整 JSON 并补全
- [ ] Storm Suppression：滑动窗口去重

**验收标准：**
- 模型忘记发出的 tool-call 能从 reasoning 中捞回
- 同一 tool 相同参数 3 秒内重复调用被抑制
- 截断的 JSON 能被正确补全

---

### 里程碑 1.5 — 成本控制体系（待开始）

**功能需求：**
- [ ] Flash-First 三级预设（auto / flash / pro）
- [ ] NEEDS_PRO 自动升级检测
- [ ] 辅助调用强制 Flash
- [ ] 轮次自动压缩（>3000 token 自动缩）
- [ ] 成本实时仪表盘

**验收标准：**
- 普通任务跑在 flash 上
- 遇到难题自动切 pro
- 界面显示每轮 $ 和累计 $

---

## 二、Phase 2 — Hermes 自进化层

### 里程碑 2.1 — 长期记忆系统（待开始）

**功能需求：**
- [ ] Session Memory / Working Memory / Long-term Memory 三层
- [ ] SQLite 持久化（rusqlite）
- [ ] Agent 驱动的记忆 Nudge 机制
- [ ] FTS5 全文搜索

---

### 里程碑 2.2 — 自主 Skill 生成与进化（待开始）

**功能需求：**
- [ ] Markdown 格式 Skill（agentskills.io 兼容）
- [ ] 任务完成后自动提取模式生成 Skill
- [ ] Skill 使用反馈闭环 → 自动更新
- [ ] `/skills` 浏览/搜索/启用/禁用

---

### 里程碑 2.3 — 跨会话检索（待开始）

**功能需求：**
- [ ] 会话归档 + LLM 摘要
- [ ] FTS5 全文索引
- [ ] 跨会话自然语言查询
- [ ] Honcho 式用户画像

---

### 里程碑 2.4 — 子 Agent 系统（待开始）

**功能需求：**
- [ ] 隔离 tokio task 运行时
- [ ] 子 Agent 协议：任务描述 → 最终结论
- [ ] 并行子 Agent、嵌套子 Agent

---

### 里程碑 2.5 — 消息网关（可选）

**功能需求：**
- [ ] Telegram / Discord adapter
- [ ] tokio channel 消息分发
- [ ] 远程命令审批

---

## 三、架构决策记录（ADR）

### ADR-001：双模部署策略

| 字段 | 值 |
|------|-----|
| 日期 | 2026-05-30 |
| 状态 | ✅ 已实现 |
| 决策 | 启动时检测 `<exe_dir>/home/` 是否存在，决定可移动模式或传统模式 |
| 理由 | 单文件部署 + 便携性需求，零配置切换 |
| 影响 | PathManager 对上层透明，所有模块通过它获取路径 |

### ADR-002：Rust Edition 2024

| 字段 | 值 |
|------|-----|
| 日期 | 2026-05-30 |
| 状态 | ✅ 已采纳 |
| 决策 | 使用 Rust 2024 edition |
| 理由 | 2026 年 5 月，Rust 1.94 稳定，2024 edition 已广泛支持 |

### ADR-003：三段式 Context 架构

| 字段 | 值 |
|------|-----|
| 日期 | 2026-05-30 |
| 状态 | ✅ 已实现 |
| 决策 | Immutable Prefix + Append-Only Log + Volatile Scratch |
| 理由 | 最大化 DeepSeek prefix cache 命中率 |
| 实现 | `src/context.rs` — `Arc<[u8]>` 零拷贝前缀，`Vec<u8>` 只追加日志 |

### ADR-004：tokio channel 异步 TUI ↔ API 通信

| 字段 | 值 |
|------|-----|
| 日期 | 2026-05-30 |
| 状态 | ✅ 已实现 |
| 决策 | `mpsc::UnboundedChannel` 双向通信（TUI→API 命令 / API→TUI 事件） |
| 理由 | ratatui 同步渲染 + tokio 异步 API 需要通过 channel 桥接 |
| 实现 | `src/tui.rs` — `AppCommand` + `ApiEvent` 双通道 |

### ADR-005：配置使用 TOML + serde

| 字段 | 值 |
|------|-----|
| 日期 | 2026-05-30 |
| 状态 | ✅ 已实现 |
| 决策 | 使用 `config.toml` 持久化配置，serde 自动序列化/反序列化 |
| 理由 | TOML 是人类可读性最好的配置格式之一，serde 生态成熟 |
| 实现 | `src/config.rs` — `#[derive(Serialize, Deserialize)]` |

### ADR-006：工具系统 —— async_trait + parallel_safe 标志

| 字段 | 值 |
|------|-----|
| 日期 | 2026-05-30 |
| 状态 | ✅ 已实现 |
| 决策 | `Tool` trait 使用 `#[async_trait]`，每个工具声明 `parallel_safe: bool` |
| 理由 | 读操作（文件/搜索）无副作用可以并行；写操作（文件/命令）必须串行 |
| 实现 | `src/tool.rs` + `src/tools.rs` — 5 个内置工具 |

### ADR-007：调度器保持调用顺序

| 字段 | 值 |
|------|-----|
| 日期 | 2026-05-30 |
| 状态 | ✅ 已实现 |
| 决策 | 调度器保持原始调用顺序：连续的 parallel_safe 组成并行批次（JoinSet），遇到串行工具时先排空当前批次 |
| 理由 | 保证 `[write(a), read(a)]` 的数据依赖 —— 如果先把所有 parallel 执行完再执行 serial，read(a) 会在 write(a) 之前执行 |
| 实现 | `src/dispatcher.rs` — `dispatch()` 方法 |

---

## 四、技术栈

| 层 | 选型 | 版本 | 用途 |
|----|------|------|------|
| 语言 | Rust | 2024 edition | 主语言 |
| 异步 | tokio | 1.x | 运行时 / 并行 / 流式 |
| HTTP | reqwest | 0.12 | DeepSeek API + SSE |
| 序列化 | serde + serde_json | 1.x | JSON 序列化 |
| 字节 | bytes | 1.x | 零拷贝前缀共享 |
| TUI | ratatui | 0.29 | 终端交互界面 |
| 数据库 | rusqlite | 0.32 | 记忆/会话存储 |
| 配置 | toml + serde | 0.8 / 1.x | 配置文件 |
| 日志 | tracing + tracing-subscriber | 0.1 / 0.3 | 结构化日志 |
| 路径 | dirs | 6.0 | 跨平台系统路径 |
