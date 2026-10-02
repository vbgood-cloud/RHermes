# 定时任务调度器（`src/scheduler/`）

> 1 个文件 · 218 行 · 仅在 **Gateway 模式**下启用（TUI 模式不启动调度器）。
> 代码：`src/scheduler/mod.rs` · 配置：`[scheduler]`（`core/config.rs::SchedulerConfig`）

## 职责

在无 TUI 的后台进程中，按 **cron 表达式**定时运行一次 Agent 会话，并把结果推送到指定 Channel（微信/企微/TG）。

典型用途：每日早报、定时巡检、周期性提醒。

## 配置

```toml
[scheduler]
enabled = true
max_concurrent_tasks = 2          # 最多同时执行的任务数（Semaphore 许可数）

[[scheduler.tasks]]
name   = "morning_standup"
cron   = "0 0 9 * * Mon-Fri"      # 工作日 09:00:00（⚠️ 6 段式，第 1 段是「秒」）
prompt = "汇总今天的待办并生成早报"
target = "wechat:文件传输助手"     # 可选：<通道名>:<chat_id>，留空则不外推
enabled = true
```

| 字段 | 说明 |
|------|------|
| `enabled` | 全局开关；`false` 或 `tasks` 为空 → `Scheduler::with_shared()` 返回 `None`，**不启动** |
| `max_concurrent_tasks` | `Semaphore` 许可数，实际取 `max(1)`；Rust 层默认 `5` |
| `tasks[].cron` | 由 `cron` crate（**0.13.0**）解析；**解析失败只 warn 并跳过该任务**，不影响其他任务 |
| `tasks[].target` | `"<channel>:<chat_id>"`，用 `split_once(':')` 拆分；通道不存在只 warn |

> 🔴 **cron 是 6 段式**（`秒 分 时 日 月 周`，可选第 7 段 `年`），与常见 5 段式 Linux crontab **不兼容**：
> ```
> 秒  分  时  日  月  周  [年]
> 0   0   9   *   *   Mon-Fri
> ```
> ⚠️ **已知缺陷**：`config.rs` 中该字段的文档注释写的是 5 段 `"分 时 日 月 周"`，且 `[scheduler.tasks]` 的模板示例给的是 `cron = "0 9 * * 1-5"`（5 段）——
> 该示例**无法通过解析**，照抄的用户会看到任务被静默跳过（仅日志一条 warn）。正确写法见上表。详见审计报告缺陷 B1。

## 运行时行为

- **启动对齐**：任务启动时先睡到「下一个整分钟」（`60 - 当前秒`，仅当偏移 >5s 且 <60s），避免启动即触发。
- **触发循环**：对 `schedule.upcoming(chrono::Utc)` 逐次取下次时间 → sleep 到点 → 取 `Semaphore` 许可 → 执行。
- **执行**：每次都**新建** `AgentSession`（session id 形如 `scheduler_<name>`），传 `None` 给 memory / skill_engine / debug（即**不带记忆、不带技能、不记调试**），只带 `dispatcher` / `transport` / `system_prompt` / `session_config`。
- **结果采集**：内部 `CaptureSink` 实现 `EventSink`，`on_chunk` 累积文本；执行完 `take_result()` 取出最终回复。
- **外推**：`target` 非空且能拆出 `ch:chat_id` 时，通过 `ChannelManager::get(ch).send_message()` 发送 `⏰ 定时任务 [<name>]\n\n<回复>`。
- **防重触发**：每次触发后固定 sleep 58s，避免同分钟内重复触发。
- **生命周期**：`start()` 返回 `Vec<JoinHandle>`，由 Gateway 持有（在 `gateway_start` 中存为 `_sched_handles`）。

## 集成点

`src/gateway/mod.rs:300-319`——在构建 `SessionRouter` **之前**初始化（注释说明是为避免所有权问题），把 `dispatcher` / `memory` / `skill_engine` / `transport` / `channel_mgr` / `system_prompt` / `session_config` 打包成 `SchedulerShared` 传入。

## 已知边界

- **仅 Gateway 模式**：TUI 模式不会启动调度器。
- **任务用的是硬编码 system prompt**（与 Gateway 同一份，含过期的「共 17 个工具」，见审计报告缺陷 B2）。
- **无持久化**：进程重启后按 cron 重新计算，不补跑错过的触发。
- **`target` 的 chat_id 需人工填**：没有「按名称查 chat_id」的辅助命令。
- 触发是「到点即跑」——若上一次执行尚未结束且许可已用尽，会**等待许可**（并发受 `max_concurrent_tasks` 限制）。
