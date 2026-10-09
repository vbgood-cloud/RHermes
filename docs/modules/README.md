# 模块文档索引（`docs/modules/`）

本目录存放**按代码现状**撰写的模块级说明——面向「想改这个模块」的读者，
覆盖：职责 / 配置 / 关键类型 / 运行时行为 / 集成点 / **已知边界与缺陷**。

> 与 `docs/architecture.md` 的分工：`architecture.md` 是**全局视图**（分层、数据流、模块地图）；
> 本目录是**单模块纵深**。两者若冲突，以**代码**为准。

| 文档 | 模块 | 文件 / 行数 | 一句话 |
|------|------|:---:|------|
| [`scheduler.md`](scheduler.md) | `src/scheduler/` | 1 / 219 | cron 定时跑 Agent 任务并推送到通道（仅 Gateway 模式） |
| [`plugin.md`](plugin.md) | `src/plugin/` | 4 / 733 | 统一插件系统：Extism Wasm 沙盒 + SKILL.md，经 `run_plugin` 调用 |
| [`mcp.md`](mcp.md) | `src/mcp/` | 8 / 1,832 | MCP 客户端：stdio / SSE / Direct 三传输，工具名 `mcp__<server>__<tool>` |
| [`provider.md`](provider.md) | `src/provider/` | 4 / 942 | `Transport` trait + ProviderPool（熔断 + 主备降级，**非加权轮询**） |
| [`judge.md`](judge.md) | `src/judge/` | 3 / 705 | Jev 判断层（D18）：fan-out 结构化判断，**失败一律回退** |
| [`gateway.md`](gateway.md) | `src/gateway/` | 2 / 817 | 无 TUI 守护进程：多通道常驻 + 调度器 + 状态文件 |
| [`channel.md`](channel.md) | `src/channel/` | 12 / 3,786 | `Channel` trait + `ChannelManager`；六渠道成熟度实测表 |

> 上表「文件 / 行数」随代码变化，**权威数字见 [`../codegraph.md`](../codegraph.md)**（自动生成）。

## 其他已存在文档

| 主题 | 文档 |
|------|------|
| 全局架构 / 分层 / 数据流 / 命令树 | `../architecture.md` |
| 护栏与修复管线 | `../guardrails.md` |
| 知识库学习（用户指南 / 间隔复习 / 导入导出 / 大文件） | `../learn-user-guide.md` · `../learn-spaced-review.md` · `../learn-export-import.md` · `../learn-largefile.md` |
| 教育版（指南 / 设计 / 测试 / e2e / 课程头） | `../edu-guide.md` · `../edu-p2p-design.md` · `../edu-p2p-testing.md` · `../edu-p2p-e2e-flow.md` · `../edu-class-course-override.md` |
| 微信 / 企业微信接入 | `../wechat-setup.md` |
| 网络代理 | `../proxy.md` |
| OMNIRoute 适配 | `../OMNIROUTE_INTEGRATION.md` |
| 设计决策（ADR） | `../plans.md` |
| 版本历史 | `../CHANGELOG.md` |
| 代码图谱（自动生成） | `../codegraph.md`（由 `../codegraph_gen.py` 生成） |
| **文档-代码一致性审计报告** | `../doc-code-audit-report.md` |
