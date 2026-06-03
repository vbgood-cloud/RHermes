# RHermes

> **终端 AI 编程 Agent · Rust · 15 个内置工具 · 自进化技能系统**

基于 DeepSeek 的终端 AI 编程 Agent，融合三段式 Context 缓存优化、并行工具调度、长期记忆与自主技能进化。

---

## 快速开始

```bash
# 下载后直接运行
./rhermes init          # 初始化向导（API Key + 模型）
./rhermes code          # 启动编程 Agent
./rhermes --version     # 查看版本
```

> 仅支持**便携式模式**：所有数据保存在 `./home/` 目录，随二进制一起带走。

---

## 内置工具（15 个）

| 类别 | 工具 | 说明 | 并行安全 |
|------|------|------|:--------:|
| **文件** | `read_file` | 读取文件（head/tail/range） | ✅ |
| | `write_file` | 写入文件 | ❌ |
| | `glob` | 按模式列出文件（fd 后端） | ✅ |
| | `search_content` | 搜索文本（ripgrep 库） | ✅ |
| | `read_pdf` | 读取 PDF 纯文本 | ✅ |
| **系统** | `run_command` | 执行 shell 命令 | ❌ |
| | `get_current_time` | 获取当前时间 | ✅ |
| **网络** | `web_search` | 搜索网络（DuckDuckGo） | ❌ |
| | `web_fetch` | 获取网页内容 | ❌ |
| **Agent** | `delegate_task` | 子 Agent 独立执行 | ❌ |
| | `run_skill` | 执行已安装技能 | ❌ |
| **技能** | `skill_list` | 列出所有技能 | ✅ |
| | `skill_search` | 搜索技能 | ✅ |
| | `skill_create` | 创建新技能 | ❌ |
| | `skill_patch` | 更新已有技能 | ❌ |

---

## 命令

| 命令 | 说明 |
|------|------|
| `/help` | 显示帮助 |
| `/version` | 显示版本信息 |
| `/init` | 初始化向导 |
| `/config` | 查看配置 |
| `/compress` | 手动触发上下文压缩 |
| `/note <内容>` | 记录笔记到 MEMORY.md |
| `/回忆 <关键词>` | 跨会话检索记忆 |
| `/归档` | 归档当前会话 |
| `/clear` | 清空对话 |
| `/quit` / `/exit` | 退出 |
| `/skill create/search/delete/edit/optimize` | 技能管理 |

---

## 架构

```
┌─────────────────────────────────────────────────┐
│                 TUI (ratatui)                     │
├─────────────────────────────────────────────────┤
│              Agent Loop（tokio 异步）              │
│  三段式 Context → API 调用 → 工具调度 → 结果写回   │
├─────────────────────────────────────────────────┤
│  记忆系统 (SQLite+FTS5)    │   技能引擎 (Markdown)  │
├─────────────────────────────────────────────────┤
│    15 个内置工具 · 并行调度 · 子 Agent 系统        │
├─────────────────────────────────────────────────┤
│              DeepSeek API 客户端                   │
└─────────────────────────────────────────────────┘
```

---

## 配置（config.toml）

```toml
[api]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"

[request]
timeout_secs = 60
max_retries = 3

[memory]
max_memory_md_chars = 5000

[display]
tool_result_max_chars = 15000
read_pdf_max_chars = 30000

[debug]
enabled = false
buffer_size = 500

[agent]
max_rounds = 50
compression_ratio = 0.8
```

---

## 技术栈

| 模块 | 选型 |
|------|------|
| 语言 | Rust 2024 edition |
| 异步 | tokio |
| HTTP | reqwest |
| TUI | ratatui + crossterm |
| 数据库 | rusqlite + FTS5 |
| 搜索 | grep-regex + grep-searcher（ripgrep 库） |
| PDF | pdf-extract |
| 配置 | TOML + serde |
| 日志 | tracing + tracing-subscriber |
| 序列化 | serde + serde_json |

---

## 项目状态

| 指标 | 值 |
|------|:---:|
| 版本 | v0.2.0 |
| 内置工具 | 15 个 |
| 单元测试 | 119 个（全部通过） |
| 源文件 | 18 个 |
| 部署 | 单文件 < 10MB |

---

## License

MIT
