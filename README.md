# RHermes

> **RHermes** = **R**easonix + **Hermes**，也是 Rust 版 Hermes。

> **Rust 写的 AI Agent，越用越聪明。** 🦀

[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.7.40-brightgreen.svg)](https://github.com/vbgood-cloud/RHermes)

不满足于"又一个 AI 助手"。DeepSeek 前缀缓存压到极限、工具并行调度榨干 IO、自进化技能让 Agent 越长越强、Jev 判断层给高频决策降本增效——用 Rust 写的，就该零妥协。

---

## 能干什么

| 能力 | 怎么做到的 |
|------|-----------|
| 🧠 **越用越聪明** | 自动从对话中提炼技能（Markdown Playbook），带使用统计和成功率，Curator 三态状态机自动淘汰/归档过期技能（30 天 → stale、90 天 → archived） |
| ⚡ **Token 省到极致** | 三段式 Context（Immutable Prefix + Append Log + Scratch），专为 DeepSeek 前缀缓存设计；压缩阈值 0.8、工具结果按工具分级截断 |
| ⚖️ **Jev 判断层（D18）** | 高频结构化判断（记忆保存 / 技能巡检 / 修复可信度 / 学习反思 / 搜索预筛）接入 System One 决策模型——单次前向出校准概率，300ms 级；纯增益非硬依赖，失败自动回退启发式 |
| 📚 **知识库学习模式** | `/learn` 一键进出：文档→知识图谱→引导式学习，带遗忘曲线复习（SM-2）、Bento 战绩面板、知识库导入导出，全通道可用 |
| 🎓 **去中心化教学班** | edu 模式：教师建课/学生认证/多老师多班/iroh gossip P2P 课堂通信/签名白名单与撤销轮换/教师仪表盘，三独立可执行程序 |
| 🔧 **33 个内置工具** | 文件读写 / ripgrep 搜索 / PDF 解析 / Office 文档(Excel/Word/PPT) / 命令执行 / 子 Agent 委派 / 知识库 kb_* / Wasm 插件 / MCP 远程工具 |
| 🔌 **统一 Plugin 系统** | Plugin trait + PluginRouter：Extism Wasm 沙盒插件（host functions 权限网关）与 SKILL.md 技能插件同一入口 |
| 🌐 **多渠道接入** | TUI 终端 / 微信个号 / 企业微信 / Telegram 稳定可用；QQ 已实现未收尾、Web 为半成品（详见「渠道成熟度」） |
| 🦾 **Provider 高可用** | 10 类 Provider + 熔断器（连续 3 次失败 / 30s 冷却）+ 主备自动切换，全走 OpenAI 兼容协议 |
| 🔒 **安全不是后话** | 命令黑名单 75 条（48 危险模式 + 27 链式绕过）/ 白名单 / 工作目录边界 / 配置写保护 / 内网 SSRF 防护 / Wasm 插件权限声明 |

---

## 3 秒上手

```bash
# 安装构建
git clone https://github.com/vbgood-cloud/RHermes
cd RHermes
cargo build --release

# 初始化 — 只需要配个 API Key
./rhermes init

# 开打
./rhermes
```

---

## 怎么用

```bash
# TUI 终端模式
rhermes                         # 进入交互式编程
rhermes --resume                # 恢复上次会话

# Gateway 后台模式 — 挂微信/Telegram 上
rhermes gateway start           # 启动守护进程
rhermes gateway setup           # 配置频道向导
rhermes gateway status          # 看状态
rhermes gateway stop            # 停了

# 知识库学习模式
rhermes                         # TUI 里 /learn <文档或目录> 建库学习
                                # /learn /stop 退出；/summary 阶段总结
                                # 复习面板自动按遗忘曲线出题

# 教育模式三件套（超集架构）
rhermes-teacher init-teacher 张老师          # 教师初始化（必须先做）
rhermes-teacher course create CS101 数据结构  # 教师建课
rhermes-teacher serve --all                  # 托管教学班
rhermes-stu login 2026001 pass               # 学生认证
rhermes-stu live                             # 进入课堂（多老师多班）
rhermes edu ...                              # 通用入口也能跑教育命令

# MCP 远程工具
rhermes mcp setup               # 添加 MCP Server（向导）
rhermes mcp list                # 看看连了哪些
rhermes mcp import servers.json # 批量导入

# 配置
rhermes config init             # 生成带注释的配置模板
rhermes config check            # 检查配置有没有写对
```

---

## 架构（说人话版）

```
你发的消息
    ↓
[Channel] ← TUI / 微信 / 企业微信 / Telegram / QQ（channel/ 统一 trait）
    ↓
[SessionRouter] ← 按 channel:chat_id 分会话，互不串台；再按课程上下文分流
    ↓
[AgentSession] ← 核心大脑：Context管理 → 记忆召回 → Jev判断 → 调 AI → 跑工具 → 学技能
    ↓
[ProviderPool] ← DeepSeek→OpenAI→Ollama… 主 provider 优先，熔断了在备选间轮转
    ↓  ↓  ↓
[ToolDispatcher]      [MemorySystem]       [SkillEngine]
  33 个内置工具            SQLite+FTS5          Markdown Playbook
  · 文件 read/write/glob   三层记忆              自动进化
  · search_content         跨会话检索            成功率统计
  · run_command            用户画像              Curator 淘汰归档
  · web_search ──→ 多引擎降级（Bing/DDG/SearXNG/Serper/百度）
  · web_fetch  ──→ SSRF 防护 + <untrusted> 包裹
  · delegate_task ──→ 子 Agent 独立跑
  · office: read/write_excel·docx、read_pptx
  · liteparse: parse_document / screenshot_document / check_document_complexity
  · kb_*: 知识库 8 件套（建库/追加/图谱/学习/测验/统计/列表/复位）
  · run_plugin ──→ Wasm 插件 / SKILL.md 技能插件（Extism 沙盒 + 权限网关）
  · mcp__*     ──→ 远程 MCP Server 工具（stdio/SSE/Direct 三传输）
    ↓
[Judge] (D18) ← Jev 判断层：高频决策走结构化判断端点，非硬依赖、失败即回退
```

---

## 渠道成熟度（如实标注）

| 渠道 | 状态 | 说明 |
|------|------|------|
| **TUI** | ✅ 稳定 | 主入口；含 Markdown 渲染、流式输出、斜杠命令 |
| **微信个号** | ✅ 稳定 | iLink Bot 扫码登录，收发/typing 全套 |
| **企业微信** | ✅ 文本可用 | 智能机器人 **WebSocket 长连接**（API 模式，免回调）；⚠️ 打字机流式未接线，当前为一问一答 |
| **Telegram** | ✅ 稳定 | Long Polling + MarkdownV2 + typing；**仅 gateway 模式注册** |
| **QQ** | ⚠️ 未收尾 | WSS+REST 收发已实现，但状态上报恒为"未连接"、不在 setup 向导与配置模板中，仅 gateway 模式可用 |
| **Web** | ❌ 半成品 | `send_message` 为空实现、回复缓存无写入方、无任何调用点；**暂不可用** |

> TUI 模式与 gateway 模式注册的通道集合不同：TUI 只挂 TUI + 微信 + 企微；Telegram / QQ 需走 gateway。

---

## 技术栈（激进版）

| 组件 | 选择 | 为什么 |
|------|------|--------|
| 语言 | Rust 2024 | 零成本抽象，不写 unsafe |
| 异步 | tokio | 工具并发调度，JoinSet 一把梭 |
| TUI | ratatui + crossterm | 终端下的 UI，不是凑合 |
| 搜索 | grep-regex + grep-searcher | Andrew Gallant 的 ripgrep 库，不多解释 |
| 搜索引擎 | scraper | 手撕 Bing/DDG HTML，不用 API Key |
| MCP 传输 | JSON-RPC stdio / SSE / Direct | 三种传输自动降级 |
| 数据库 | rusqlite + FTS5 | 全文检索记忆，嵌在进程里 |
| HTTP | reqwest | socks 代理、SSE 流、连接复用 |
| 插件沙盒 | extism | Wasm 插件 + host functions 权限网关 |
| P2P 课堂 | iroh + iroh-gossip | 去中心化教学班通信，每班独立 Topic |
| 二维码 | qrcodegen | 微信扫码登录，BMP 手撸 |
| 判断层 | System One 兼容 API | 结构化判断（noul/choice/score），自托管兼容端点亦可 |

---

## 配置（极简版 config.toml）

```toml
[providers.deepseek]
base_url = "https://api.deepseek.com"
models = ["deepseek-v4-flash"]

[agent]
workspace = "/your/projects"     # 文件操作只允许在这个目录下
command_allowed_prefixes = ["git", "ls", "cat", "cargo", "python"]
# 不配白名单 = 所有命令都能跑，但不建议

[jev]                            # D18 判断层（可选，纯增益）
enabled = true
base_url = "https://api.typesafe.ai"   # 或自托管兼容端点
model = "jev-latest"                   # 默认值；可覆盖为具体模型名
min_confidence = 0.6                   # 低于此置信度走回退
```

API Key 放 `.env`：
```
DEEPSEEK_API_KEY=sk-your-key
TYPESAFE_API_KEY=your-key        # 启用判断层才需要
```

`config.toml` 实际有 20 个顶层配置段；完整带注释模板：`rhermes config init` 一把生成。

---

## 项目状态

| 指标 | 值 |
|------|:---|
| 版本 | v0.7.40（⚠️ 本地未发布；**最新已发布版为 v0.7.27**，见 [CHANGELOG](docs/CHANGELOG.md) 顶部发布状态） |
| 代码规模 | 110 个 `.rs` 文件（47,878 行，纯 Rust） |
| 内置工具 | 33 个（+ 条件注册 `run_plugin`、`wasm_*`、`mcp__*`） |
| 测试 | 静态计数 399 个单测（`src/`）+ 6 个集成测试；v0.7.27 实测为 414+6=420，**删除 `cost.rs` 后尚未重测** |
| 顶层模块 | 15 个目录 + `debug.rs` / `init.rs` / `lib.rs` / `main.rs` |
| 支持渠道 | TUI / 微信 / 企业微信 / Telegram（QQ 未收尾、Web 半成品） |
| 可执行程序 | rhermes / rhermes-stu / rhermes-teacher 三件套 |
| AI Provider | omniRoute / OpenAI / DeepSeek / SiliconFlow / Zhipu / Moonshot / Ollama / Groq / New API / LM Studio |
| 搜索引擎 | 5 种（Bing / DuckDuckGo / SearXNG / Serper / 百度） |

### 近期亮点（v0.7.9 → v0.7.40）

- **v0.7.28–40** 企业微信**智能机器人长连接**（D19 · API 模式）：主动 wss 连出、免公网回调、免 AES 加解密；`aibot_subscribe` 认证 + ping 30s 心跳 + 指数退避重连；旧 webhook/轮询链路整体删除；⚠️ 确诊网关按 **TLS 指纹**区别对待 → 必须 `native-tls`
- **v0.7.36** 删除死代码 `cost.rs`（560 行，全项目零引用）
- **v0.7.23–27** learn 体验打磨：库名/图谱中文名不再被改写，答题后无响应修复，用户指南完善
- **v0.7.28** 文档-代码一致性全量审计：修正 24 篇 + 新增 `docs/modules/` 8 篇
- **v0.7.20–22** D18 判断层 P1–P3：记忆保存、技能巡检、修复可信度、学习反思、搜索预筛五处接入
- **v0.7.14–15** SectionHost：驱动无关会话宿主，REPL 第一个迁移
- **v0.7.16** 教师端 AI 答疑循环：自动应答学生提问并广播回班
- **v0.7.11–13** edu 多老师拓扑 + 身份持久化 + 师端多班托管 + CLI 冒烟缺陷修复
- **v0.7.9–10** 去中心化 P2P 教学班（iroh）：Topic 隔离 / 签名白名单 / 撤销轮换
- **v0.7.1–8** `/learn` 知识库学习系统全链路：建库闭环 / 遗忘曲线 SM-2 / 图谱点亮 / 导入导出

详细功能清单见 [FEATURES.md](FEATURES.md)。

### 文档导航

| 想了解 | 看这个 |
|--------|--------|
| 全局架构、分层、数据流、命令树 | [docs/architecture.md](docs/architecture.md) |
| 单模块纵深（调度器/插件/MCP/Provider/判断层/Gateway/通道） | [docs/modules/](docs/modules/README.md) |
| 功能清单 | [FEATURES.md](FEATURES.md) |
| 护栏与修复管线 | [docs/guardrails.md](docs/guardrails.md) |
| `/learn` 知识库学习 | [docs/learn-user-guide.md](docs/learn-user-guide.md) |
| 教育版（P2P 教学班） | [docs/edu-guide.md](docs/edu-guide.md) |
| 微信 / 企微接入 | [docs/wechat-setup.md](docs/wechat-setup.md) |
| 网络代理 | [docs/proxy.md](docs/proxy.md) |
| 版本历史 | [docs/CHANGELOG.md](docs/CHANGELOG.md) |
| 代码图谱（自动生成） | [docs/codegraph.md](docs/codegraph.md) |
| **文档-代码一致性审计** | [docs/doc-code-audit-report.md](docs/doc-code-audit-report.md) |

---

## License

MIT — 随便用，改了记得说一声。
