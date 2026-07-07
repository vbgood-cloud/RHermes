# RHermes

> **RHermes** = **R**easonix + **Hermes**，也是 Rust 版 Hermes。

> **Rust 写的 AI Agent，越用越聪明。** 🦀

[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.6.5-brightgreen.svg)](https://github.com/vbgood-cloud/RHermes)

不满足于"又一个 AI 助手"。DeepSeek 前缀缓存压到极限、工具并行调度榨干 IO、自进化技能让 Agent 越长越强——用 Rust 写的，就该零妥协。

---

## 能干什么

| 能力 | 怎么做到的 |
|------|-----------|
| 🧠 **越用越聪明** | 自动从对话中提炼技能（Markdown Playbook），带使用统计和成功率，会自动淘汰过期技能 |
| ⚡ **Token 省到极致** | 三段式 Context（Immutable Prefix + Append Log + Scratch），专为 DeepSeek 前缀缓存设计 |
| 🔧 **22+ 工具随便使** | 文件读写 / ripgrep 搜索 / PDF 解析 / Office 文档(Excel/Word/PPT) / 命令执行 / 子 Agent 委派 / MCP 远程工具 |
| 🌐 **多渠道接入** | TUI 终端 / 微信个号 / 企业微信 / Telegram，出门也能使唤 |
| 🦾 **Provider 高可用** | 多 AI Provider 池 + 熔断器 + 加权轮询，挂一个自动切下一个 |
| 🔒 **安全不是后话** | 命令黑名单 / 白名单 / 工作目录边界 / 配置写保护 / 内网 SSRF 防护 |

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
[Channel] ← TUI / 微信 / 企业微信 / Telegram（随便哪个渠道都能进）
    ↓
[SessionRouter] ← 按人分开对话，互不串台
    ↓
[AgentSession] ← 核心大脑：Context管理 → 记忆召回 → 调 AI → 跑工具 → 学技能
    ↓
[ProviderPool] ← DeepSeek挂了换 OpenAI，OpenAI 挂了换 Ollama，都挂了骂街
    ↓  ↓  ↓
[ToolDispatcher]      [MemorySystem]       [SkillEngine]
  read_file               SQLite+FTS5          Markdown Playbook
  write_file              三层记忆              自动进化
  search_content          跨会话检索            成功率统计
  run_command             用户画像              过期淘汰
  web_search ──→ 多引擎降级（DDG→Serper→Bing）
  delegate_task ──→ 子 Agent 独立跑
  read_excel/write_excel ──→ Office 文档处理
  read_docx/write_docx   ──→ Word 读写
  read_pptx              ──→ PPTX 读取
  mcp__*     ──→ 远程 MCP Server 工具
```

---

## 技术栈（激进版）

| 组件 | 选择 | 为什么 |
|------|------|--------|
| 语言 | Rust 2024 | 零成本抽象，不写 unsafe |
| 异步 | tokio | 工具并发调度，JoinSet 一把梭 |
| TUI | ratatui + crossterm | 终端下的 UI，不是凑合 |
| 搜索 | grep-regex + grep-searcher | Andrew Gallant 的 ripgrep 库，不多解释 |
| 搜索引擎 | scraper | 手撕 DDG/Bing HTML，不用 API Key |
| MCP 传输 | JSON-RPC stdio/SSE | 协议完整实现，三种传输模式全支持 |
| 数据库 | rusqlite + FTS5 | 全文检索记忆，嵌在进程里 |
| HTTP | reqwest | socks 代理、SSE 流、连接复用 |
| 二维码 | qrcodegen | 微信扫码登录，BMP 手撸 |

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
```

API Key 放 `.env`：
```
DEEPSEEK_API_KEY=sk-your-key
```

完整配置模板：`rhermes config init` 一把生成。

---

## 项目状态

| 指标 | 值 |
|------|:---|
| 版本 | v0.6.5 |
| 文件数 | 61 .rs |
| 内置工具 | 22 + MCP 动态扩展 |
| 支持渠道 | TUI / 微信 / 企业微信 / Telegram |
| AI Provider | DeepSeek / OpenAI / Zhipu / SiliconFlow / Ollama / LM Studio / New API / ... |

---

## License

MIT — 随便用，改了记得说一声。
