# RHermes

> **Reasonix 的省 Token 肌肉 + Hermes 的自进化大脑 + Rust 的零开销骨架**

RHermes 是一个基于 Rust 的终端 AI 编程 Agent。它融合了 [Reasonix](https://github.com/esengine/DeepSeek-Reasonix) 的极致 Token 缓存优化与 [Hermes Agent](https://github.com/NousResearch/hermes-agent) 的自进化学习闭环——以 DeepSeek 为原生后端，让你**越用越省钱，越省越聪明**。

---

## 优势一：省钱的飞轮效应 —— 最便宜的自我进化

这是 RHermes 最核心的、两个项目都无法单独实现的结构性优势。

```
Reasonix 省 Token   →   省下的钱   →   跑 Hermes 学习循环   →   越用越聪明
         ↕                               ↕
  99.82% 缓存命中                 技能生成 / 记忆摘要 / 跨会话检索
```

| 单独看 | 结论 |
|--------|------|
| **Reasonix 单独存在** | 省钱了，但不会变聪明，每 session 从零开始 |
| **Hermes 单独存在** | 会变聪明，但学习循环本身消耗大量 Token，用不起 |
| **RHermes 合体** | 用 Reasonix 省下的 5× Token 预算，免费跑 Hermes 的自进化闭环 |

> 飞轮效应 —— 越用越省钱，越省钱越能学。

---

## 优势二：Rust 的内存安全 → 超越 99.82% 的缓存命中率

Reasonix 的 TypeScript（Node.js）实现有一个被忽略的隐患：**V8 GC 会移动内存**。

| 环节 | TypeScript (Reasonix) | Rust (RHermes) |
|------|----------------------|----------------|
| 内存管理 | V8 GC 不定时触发，对象移动 | 所有权系统，零 GC 暂停 |
| 前缀缓冲区 | Buffer/string 可能被 GC 移动 | `Arc<[u8]>` —— 指针固定，字节级稳定 |
| 并发读写 | 单线程 Event Loop | 无数据竞争的并行访问 |
| 可预测性 | GC pause 不可控 | 完全确定性的延迟 |

**DeepSeek prefix cache 的要求是 exact byte match** —— Rust 的 `Arc<[u8]>` 共享不可变前缀，没有 V8 GC 移动内存的干扰，理论上能实现比 TypeScript 更高且更稳定的缓存命中率。

> 这不是微优化 —— 99.82% 到 99.9%，差 0.08% 就可能差几百万 Token。

---

## 优势三：单文件静态二进制 —— 零运行时依赖

| 对比 | Reasonix | Hermes Agent | RHermes |
|------|----------|-------------|---------|
| 运行时 | Node.js ≥22 | Python 3.11 + uv | **裸机运行** |
| 安装 | `npm i -g reasonix` | curl 脚本 + uv + pip | **curl → chmod → 跑** |
| 体积 | 项目 + node_modules ≈ 100MB | venv + deps ≈ 200MB+ | **单文件 < 30MB** |
| 升级 | npm update | hermes update (pip) | **下载替换即可** |
| 跨平台 | 需要 Node.js 版本一致 | 需要 Python 版本一致 | **编译即分发** |

对 Docker 部署、CI/CD 管道、边缘节点、嵌入式设备来说是本质差异。

---

## 优势三补充：双模部署 —— 既便携又标准

**可移动模式（Portable Mode）：** 在可执行文件旁边放一个 `home/` 目录，所有配置 / 记忆 / 技能 / 会话都装在这个目录里。把 `rhermes + home/` 一起放到 U 盘、云同步文件夹（Dropbox / iCloud / 坚果云）、Docker volume 里——插到哪台机器都一样。

**传统模式（Traditional Mode）：** 没有 `home/` 目录？自动走系统标准路径（Linux `~/.config/rhermes`，macOS `~/Library/Application Support`，Windows `%APPDATA%/rhermes`）。

两种模式用同一份代码、同一个二进制，**启动时自动检测，零配置切换**。

---

## 优势四：Tokio 并行引擎 vs 单线程 Event Loop

Reasonix 已经实现了 `parallelSafe` 并行工具调度，但 Node.js 的 Event Loop 在遇到 CPU 密集型操作时（JSON 解析、LLM 摘要、FTS5 检索）会阻塞。

**RHermes 用 tokio 能做到真正的并行：**

```
传统 Agent (Node/Python):
  ┌─[Event Loop / GIL]────────────────────┐
  │  读文件       JSON 解析               │
  │  ██░░░░  →    等待        →    ████   │  ← 阻塞
  └───────────────────────────────────────┘

RHermes (Rust + tokio):
  ┌─[Work-Stealing Thread Pool]────────────┐
  │  读文件 ──→  ████████                 │
  │  搜索  ──→  ████████                 │  ← 真正的并行
  │  解析  ──→  ████████                 │
  │  FTS5   ──→  ████████                 │
  └───────────────────────────────────────┘
```

特别是 Hermes 层加入之后：技能生成需要 **LLM 调用 + 文件分析 + FTS5 检索** 三者并行，在 Rust 里用 `tokio::JoinSet` 一行搞定。

---

## 优势五：编译期类型安全 —— Agent 循环的可靠性

Agent 编程的核心痛点是 **JSON 形状不对、Tool-call 参数拼错、状态机状态炸飞**。

| 问题 | Reasonix (TS) | Hermes (Python) | RHermes (Rust) |
|------|--------------|----------------|----------------|
| 工具调用参数 | 运行时校验 | 运行时校验 | **编译期 + serde 反序列化** |
| 状态机迁移 | 运行时炸 | 运行时炸 | **枚举 + match，编译器全覆盖** |
| null/undefined | 到处都是 | None 检查 | **`Option<T>` 必须处理** |
| 重构安全 | 改字段可能漏 | 改字段可能漏 | **改一个字段，编译器告诉你怎么改** |

Tool-Call Repair Pipeline 在 Rust 里天然更强 —— `serde_json` 的 `Value` 可类型安全地处理深层嵌套参数，flatten / scavenge / truncation 每一步都有明确的类型输入输出。

---

## 优势六：首次将"自进化"与"极致成本"绑定为同一个架构决策

这是两个项目都无法单独实现的架构层面差异化：

| 维度 | Reasonix | Hermes | RHermes |
|------|---------|--------|---------|
| 成本策略 | Flash-First | 无优化 | **Flash-First + 自动升级** |
| 学习循环 | 无 | 内置 | **内置，且跑在成本最低的引擎上** |
| 上下文组织 | 三段式缓存 | 标准 | **三段式 + 学习注入 = 缓存不被打碎** |
| 技能进化 | 手动写 Skill | 自动生成 | **自动生成 + 5× 更便宜的迭代成本** |
| 模型选择 | DeepSeek Only | 任意 | **DeepSeek 原生 + 学习层兼容任意模型** |
| 分发 | npm install | Python venv | **单文件 curl** |

> 学习循环本身需要消耗 Token 去生成摘要、分析模式、改进技能。RHermes 的学习循环跑在 **99.82% 缓存命中率** 的引擎上——别人的学习是成本，RHermes 的学习几乎是免费的。

---

## 部署策略

### 检测逻辑（启动时执行一次）

```
启动
  │
  ├─ 获取可执行文件路径: std::env::current_exe()
  │
  ├─ 检查 <exe_dir>/home/ 是否存在且是目录?
  │     │
  │     ├─ 是 → 可移动模式
  │     │    └─ data_root = <exe_dir>/home/
  │     │
  │     └─ 否 → 传统模式
  │          └─ data_root = 系统标准配置目录
  │               Linux:   ~/.config/rhermes/
  │               macOS:   ~/Library/Application Support/rhermes/
  │               Windows: %APPDATA%/rhermes/
  │
  └─ 初始化 PathManager，全局传递 data_root
```

### 可移动模式目录结构

```
my-project/
├── rhermes              # 单文件可执行程序（静态编译）
└── home/                # 所有数据都在这里
    ├── config.toml      # 主配置文件（API Key、模型选择、权限）
    ├── memory.db        # 长期记忆库（SQLite + FTS5）
    ├── skills/          # 用户自定义技能（Markdown playbook）
    │   ├── my-skill.md
    │   └── ...
    ├── sessions/        # 会话历史归档
    │   └── 2026-05-30T10-30-00.jsonl
    ├── logs/            # 运行日志
    └── cache/           # 临时缓存（可安全删除）
```

### 传统模式目录结构（`~/.rhermes/`）

```
~/.rhermes/
├── config.toml
├── memory.db
├── skills/
├── sessions/
├── logs/
└── cache/
```

两种模式内部结构完全一致，`PathManager` 对上层代码透明——Agent Loop 和各模块只问 `PathManager` 要路径，不关心是哪种模式。

---

## 架构概览

```
┌─────────────────────────────────────────────────┐
│                    CLI TUI                        │
│           (ratatui · 成本仪表盘)                   │
├─────────────────────────────────────────────────┤
│                   Agent Loop                      │
│  ┌─────────────┐  ┌────────────┐  ┌──────────┐ │
│  │ Context Mgr │  │Tool Dispat │  │Cost Ctrl │ │
│  │ (3-zone)    │  │cher (并行)│  │(flash    │ │
│  │             │  │            │  │/pro)     │ │
│  └──────┬──────┘  └─────┬──────┘  └────┬─────┘ │
│         │               │              │        │
│  ┌──────┴──────┐  ┌─────┴──────┐       │        │
│  │ Repair Pipe │  │  MCP Host  │       │        │
│  │(flatten /   │  │ (stdio/SSE)│       │        │
│  │ scavenge /  │  └────────────┘       │        │
│  │ truncation  │                       │        │
│  │ / storm)    │                       │        │
│  └─────────────┘                       │        │
├────────────────────────────────────────┴────────┤
│              Self-Evolving Layer                  │
│  ┌──────────┐ ┌──────────┐ ┌─────────┐ ┌──────┐│
│  │ Memory   │ │ Skills   │ │Cross-   │ │Sub-  ││
│  │ System   │ │ Engine   │ │Session  │ │Agent ││
│  │(SQLite)  │ │(Markdown)│ │Search   │ │Pool  ││
│  └──────────┘ └──────────┘ └─────────┘ └──────┘│
├─────────────────────────────────────────────────┤
│                 Infrastructure                    │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐│
│  │ DeepSeek │ │ File Ops │ │ Shell + Git Ops  ││
│  │ API      │ │          │ │                   ││
│  └──────────┘ └──────────┘ └──────────────────┘│
│  ┌──────────────────────────────────────────────┐│
│  │            Path Manager                      ││
│  │ 可移动模式检测 · data_root 分发 · 目录创建    ││
│  └──────────────────────────────────────────────┘│
└─────────────────────────────────────────────────┘
```

### 四大模块

| 模块 | 职责 | 关键技术 |
|------|------|---------|
| **Cache-First Loop** | 三段式 Context 管理，最大化 prefix cache 命中率 | `Arc<[u8]>`，Append-Only Log，字节级稳定性 |
| **Tool-Call Repair** | flatten / scavenge / truncation / storm 四道工序 | serde_json，滑动窗口检测 |
| **Cost Control** | Flash-First 分级 + NEEDS_PRO 自动升级 + 自动压缩 | Tokio 中断重试 |
| **Self-Evolving** | 记忆持久化 / 技能自动生成 / 跨会话检索 / 用户画像 | SQLite FTS5 |

---

## 技术选型

| 模块 | 选型 | 理由 |
|------|------|------|
| 异步运行时 | `tokio` | 并行调度、SubAgent、流式响应 |
| HTTP 客户端 | `reqwest` | SSE 流式、TLS、重试 |
| 序列化 | `serde` + `serde_json` | 稳定 byte 输出 |
| 字节管理 | `bytes` | 零拷贝前缀共享 |
| 终端 UI | `ratatui` | Rust 最成熟的 TUI 框架 |
| 数据库 | `rusqlite` + FTS5 | 记忆/会话索引 |
| 配置文件 | `serde_yaml` / `toml` | 灵活配置 |

---

## 开发路线

```
第一阶段（Reasonix 核心省 Token 引擎）
├── ✅ 里程碑 1.1 — 项目骨架 + 路径管理器 + 双模部署
├── ⏳ 里程碑 1.2 — DeepSeek API 集成 + 缓存利用
├── ⏳ 里程碑 1.3 — 并行工具调度
├── ⏳ 里程碑 1.4 — Tool-Call Repair Pipeline
└── ⏳ 里程碑 1.5 — 成本控制体系

第二阶段（Hermes 自进化层）
├── ⏳ 里程碑 2.1 — 长期记忆系统
├── ⏳ 里程碑 2.2 — 自主 Skill 生成与进化
├── ⏳ 里程碑 2.3 — 跨会话检索
├── ⏳ 里程碑 2.4 — 子 Agent 系统
└── ⏳ 里程碑 2.5 — 消息网关（可选）
```

---

## 相关文档

| 文档 | 说明 |
|------|------|
| [`docs/plans.md`](docs/plans.md) | 完整开发计划存档 + 架构决策记录（ADR） |
| [`docs/CHANGELOG.md`](docs/CHANGELOG.md) | 版本变更日志 |

---

## License

MIT
