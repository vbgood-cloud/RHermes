# CHANGELOG

## v0.2.0 — 2026-06-02

### 新增
- **15 个内置工具**：read_file / write_file / search_content / run_command / glob / get_current_time / web_search / web_fetch / delegate_task / run_skill / skill_list / skill_search / skill_create / skill_patch / read_pdf
- **三段式 Context 架构**：stable + volatile + history，最大化 prefix cache 命中率
- **Context 自动压缩**：80% 阈值自动触发 6 段结构化摘要
- **记忆系统**：SQLite + FTS5 全文搜索，自动召回/写入
- **技能引擎**：Markdown playbook，CRUD，使用统计，进化建议
- **子 Agent 系统**：delegate_task 委托独立 Agent 执行
- **会话持久化**：Ctrl+Q 保存，-r 恢复
- **调试系统**：SessionDebug + rhermes debug export
- **输入排队**：响应期间可输入，自动排队等待
- **search_content 改用 ripgrep 库**：自动跳过二进制/.gitignore
- **配置化**：DisplayConfig / DebugConfig / AgentConfig
- **系统提示词**：14 个工具列表 + 自进化规则

### 修复
- 子进程 stdin 抢占 TUI 键盘输入
- UTF-8 字符边界 panic（truncate 函数）
- 工具结果截断过小（2000→15000 字符）
- 模型重复调用同一工具

### 变更
- 版本 0.1.0 → 0.2.0
- 仅支持便携式模式
- 添加 /version 命令

### 测试
- 119 个单元测试，全部通过

---

## v0.1.0 — 2026-05-30

初始版本：项目骨架 + PathManager + 基础 TUI
