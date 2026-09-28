# RHermes

> **RHermes** = **R**easonix + **Hermes**，也是 Rust 版 Hermes。

> **Rust 写的 AI Agent，越用越聪明。** 🦀

[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.7.20-brightgreen.svg)](https://github.com/vbgood-cloud/RHermes)

不满足于"又一个 AI 助手"。DeepSeek 前缀缓存压到极限、工具并行调度榨干 IO、自进化技能让 Agent 越长越强、Jev 判断层给高频决策降本增效——用 Rust 写的，就该零妥协。

---

## 能干什么

| 能力 | 怎么做到的 |
|------|-----------|
| 🧠 **越用越聪明** | 自动从对话中提炼技能（Markdown Playbook），带使用统计和成功率，Curator 三态状态机自动淘汰/归档/合并过期技能 |
| ⚡ **Token 省到极致** | 三段式 Context（Immutable Prefix + Append Log + Scratch），专为 DeepSeek 前缀缓存设计；五机制成本控制（Flash/Pro 分级 + 自动压缩） |
| ⚖️ **Jev 判断层（D18）** | 高频结构化判断（如记忆保存决策）接入 System One 决策模型 winnow:e4b——单次前向出校准概率，300ms 级、成本约为 LLM 的 1/60；纯增益非硬依赖，失败自动回退原方案 |
| 📚 **知识库学习模式** | `/learn` 一键进出：文档→知识图谱→引导式学习，带遗忘曲线复习（SM-2）、Bento 战绩面板、知识库导入导出，全通道可用 |
| 🎓 **去中心化教学班** | edu 模式：教师建课/学生认证/多老师多班/P2P 课堂通信（iroh gossip）/作业分发与提交/反思评分闭环/教师仪表盘，三独立可执行程序 |
| 🔧 **26+ 工具随便使** | 文件读写 / ripgrep 搜索 / PDF 解析 / Office 文档(Excel/Word/PPT) / 命令执行 / 子 Agent 委派 / Wasm 插件 / MCP 远程工具 |
| 🔌 **统一 Plugin 系统** | Plugin trait + PluginRouter：Extism Wasm 沙盒插件（host functions 权限网关）与 SKILL.md 技能插件同一入口 |
| 🌐 **多渠道接入** | TUI 终端 / 微信个号 / 企业微信 / Telegram / QQ / Web，TUI 也是 Gateway 的一个 channel，全渠道共享会话架构 |
| 🦾 **Provider 高可用** | 多 AI Provider 池 + 熔断器 + 加权轮询 + OmniRoute 适配，挂一个自动切下一个 |
| 🔒 **安全不是后话** | 命令黑名单 70+ 模式 / 白名单 / 工作目录边界 / 配置写保护 / 内网 SSRF 防护 / Wasm 插件权限声明 |

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
rhermes-teacher course create CS101 数据结构  # 教师建课
rhermes-stu login 2026001 password             # 学生登录
rhermes edu ...                                # 通用入口也能跑教育命令

# MCP 远程工具
rhermes mcp setup               # 添加 MCP Server
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
[Channel] ← TUI / 微信 / 企业微信 / Telegram / QQ / Web（全是 Gateway 的 channel）
    ↓
[SessionRouter] ← 按人按课分开对话，互不串台（SectionKey 全局班标识）
    ↓
[AgentSession] ← 核心大脑：Context管理 → 记忆召回 → Jev判断 → 调 AI → 跑工具 → 学技能
    ↓
[ProviderPool] ← DeepSeek挂了换 OpenAI，OpenAI 挂了换 Ollama，都挂了骂街
    ↓  ↓  ↓
[ToolDispatcher]      [MemorySystem]       [SkillEngine]
  read_file               SQLite+FTS5          Markdown Playbook
  write_file              三层记忆              自动进化
  search_content          跨会话检索            成功率统计
  run_command             用户画像              Curator 淘汰合并
  web_search ──→ 多引擎降级（Bing/DDG/SearXNG/Serper/百度）
  delegate_task ──→ 子 Agent 独立跑
  read_excel/write_excel ──→ Office 文档处理
  read_docx/write_docx   ──→ Word 读写
  read_pptx              ──→ PPTX 读取
  parse_document 等 3 个 ──→ LiteParse 文档解析（PDF/图片/截图）
  run_plugin ──→ Wasm 插件 / SKILL.md 技能插件（Extism 沙盒 + 权限网关）
  mcp__*     ──→ 远程 MCP Server 工具（resources 支持 + 工具热刷新）
    ↓
[Judge] (D18) ← Jev 判断层：高频决策走 winnow:e4b，非硬依赖、失败即回退
```

---

## 技术栈（激进版）

| 组件 | 选择 | 为什么 |
|------|------|--------|
| 语言 | Rust 2024 | 零成本抽象，不写 unsafe |
| 异步 | tokio | 工具并发调度，JoinSet 一把梭 |
| TUI | ratatui + crossterm | 终端下的 UI，不是凑合 |
| 搜索 | grep-regex + grep-searcher | Andrew Gallant 的 ripgrep 库，不多解释 |
| 搜索引擎 | scraper | 手撕 Bing/DDG HTML，不用 API Key |
| MCP 传输 | JSON-RPC stdio/SSE | 协议完整实现，多种传输模式全支持 |
| 数据库 | rusqlite + FTS5 | 全文检索记忆，嵌在进程里 |
| HTTP | reqwest | socks 代理、SSE 流、连接复用 |
| 插件沙盒 | extism | Wasm 插件 + host functions 权限网关 |
| P2P 课堂 | iroh + iroh-gossip | 去中心化教学班通信，每班独立 Topic |
| 二维码 | qrcodegen | 微信扫码登录，BMP 手撸 |
| 判断层 | Jev / System One API | 结构化判断（winnow:e4b），自托管 Ollaya 兼容端点亦可 |

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
base_url = "https://api.typesafe.ai"   # 或自托管 Ollaya 端点
model = "winnow:e4b"
min_confidence = 0.6             # 低于此置信度走回退
```

API Key 放 `.env`：
```
DEEPSEEK_API_KEY=sk-your-key
TYPESAFE_API_KEY=your-key        # 启用判断层才需要
```

完整配置模板：`rhermes config init` 一把生成。

---

## 项目状态

| 指标 | 值 |
|------|:---|
| 版本 | v0.7.20 |
| 代码规模 | 110 .rs 文件（~41,900 行，纯 Rust） |
| 内置工具 | 26 + MCP 动态扩展 + Wasm/Skill 插件 |
| 单元测试 | 377 个 `#[test]` + edu P2P 端到端 + kb 集成测试 |
| 支持渠道 | TUI / 微信 / 企业微信 / Telegram / QQ / Web |
| 可执行程序 | rhermes / rhermes-stu / rhermes-teacher 三件套 |
| AI Provider | DeepSeek / OpenAI / Zhipu / SiliconFlow / Ollama / LM Studio / OmniRoute / New API / ... |
| 搜索引擎 | 5 种（Bing / DuckDuckGo / SearXNG / Serper / 百度） |

### 近期亮点（v0.7.9 → v0.7.20）

- **v0.7.20** D18 判断层 P1：记忆保存判断接入 winnow:e4b 生产端点
- **v0.7.18-19** D18 判断层基础设施 + 门槛测试通过
- **v0.7.14-15** SectionHost：驱动无关会话宿主，REPL 第一个迁移
- **v0.7.11-12** edu 多老师拓扑 + 身份持久化 + 师端多班托管
- **v0.7.9-10** 去中心化 P2P 教学班（iroh）：Topic 隔离 / 作业分发 / 端到端联调
- **v0.7.1-8** `/learn` 知识库学习系统全链路：建库闭环 / 遗忘曲线 SM-2 / 图谱点亮 / 导入导出

详细功能清单见 [FEATURES.md](FEATURES.md)。

---

## License

MIT — 随便用，改了记得说一声。
