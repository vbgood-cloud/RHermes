# Gateway 守护进程（`src/gateway/`）

> 2 个文件 · 816 行。**无 TUI 的后台运行模式**：把 Channel 系统（微信/企微/TG/QQ）与 Agent Loop 接起来，长期挂机。
> 代码：`gateway/mod.rs`（`handle_command` + start/stop/status/channel）· `gateway/setup.rs`（交互式配置）
> 命令：`rhermes gateway <setup|start|stop|status|channel <list|enable|disable>>`

## 与 TUI 模式的差异

| 能力 | TUI 模式 | Gateway 模式 |
|------|---------|-------------|
| 界面 | ratatui 终端 | 无（日志 + 启动横幅） |
| 消息入口 | 键盘 → `TuiSink` | Channel → `inbound_rx` → `SessionRouter` |
| 定时任务 | ❌ 不启动 | ✅ `Scheduler` |
| 通道 | 注册 `tui` / `wecom` / `wechat`（**无 telegram/qq**） | `[gateway].channels` 按名注册（wechat/wecom/telegram/qq） |
| 判断层 / 记忆 / 技能 / Transport | ✅ | ✅（同一套初始化） |

## `gateway start` 流程

1. **防重复启动**：读 `[gateway].pid_file`；Windows 用 `tasklist /FI "PID eq <pid>"` 判断进程是否存活，存活则报错退出。
2. **写 PID 文件**（`create_dir_all` 父目录后写入）。
3. **初始化公共组件**（与 TUI 一致）：
   - 记忆系统（`data_root/memories/memories.db`）
   - 技能引擎（`skills/`）
   - `set_global_config` / `set_display_config` / `set_workspace`（打印工作目录边界）/ `set_global_skill_engine`
   - **判断层** `judge::set_global_judge(Judge::from_config)`（D18）
   - 调试系统（`[debug].enabled` → `SessionDebug`）
   - **Transport**：`create_main_transport(config, cb_threshold, cb_cooldown)` → `set_global_transport`
4. **注册通道**：遍历 `[gateway].channels`，按名匹配**且**该通道 `enabled`：
   `wechat` / `wecom` / `telegram` / `qq`；未注册的通道只 warn。
5. **`channel_mgr.start_all()`**：每通道一个 task；
   随后**轮询等待**（最多 30 秒，每秒一次）直到「每个通道已连接 / 有 last_error / 有 detail」。
6. **打印通道启动状态表**（控制台 + 日志）。
7. **定时任务调度器**（在 `SessionRouter` **之前**初始化，避免所有权问题）：`[scheduler]` 启用且有任务时 `Scheduler::with_shared(..).start()`。
8. **构建 `SessionRouter`**：`full_registry(&config.mcp)`（含 MCP 远程工具）→ `ToolDispatcher` → `SessionRouter`（带 `config_path`、`system_prompt`、`session_config`、`session_debug`）；`router.set_edu_role(&config.edu.role)`。
9. **后台状态写盘**：每 **30s** 把通道状态写到 `home/channel_status.json`。
10. **主循环**：`while let Some(inbound) = inbound_rx.recv().await { router.dispatch(inbound) }`。

## 生命周期管理

| 子命令 | 行为 |
|--------|------|
| `start` | 检查 PID → 写 PID → 初始化 → 阻塞跑 `inbound_rx` 循环 |
| `stop` | 读 PID → `taskkill /F /PID <pid>`（Windows）→ 成功则删 PID 文件；失败也删（视为 PID 过期） |
| `status` | 检查进程存活；存活则读 `home/channel_status.json`（含 `updated_at` / 每通道 `connected`/`detail`/`msg_count`/`last_error`）；未运行则只按配置列出通道 |
| `channel list` | 打印 wechat / wecom / telegram 的启用状态与关键字段（⚠️ **不含 qq**） |
| `channel enable\|disable <名>` | 改 `[channels.<名>].enabled` 并 `config.save()`，**下次 `gateway start` 生效** |

## 已知边界与缺陷

- 🔴 **system prompt 硬编码且过期**：`gateway/mod.rs` 里写死 `## 可用工具（共 17 个）` 并列举 17 个工具名——实际注册 **33 个**工具。
  同一份过期文本还硬编码在 `lib.rs:977`（TUI）与 `tui/mod.rs:510`。三处均需改为动态生成（见审计报告缺陷 B2）。
- **`channel list` 不列 QQ**：`channel_enable_disable` 与 `gateway status` 都认 qq，唯独 `channel_list` 只打印 3 个通道，信息不一致。
- **PID 判断依赖 `tasklist` 文本匹配**：`out.contains(pid)` 可能被其他数字误命中（弱校验）。
- **`stop` 用 `taskkill /F`**：强杀，不给通道优雅关闭机会（`shutdown_mcp()` / 子进程可能不被回收）。
- **无 Windows 服务/守护化**：进程退出即停；无 systemd 单元、无开机自启、无崩溃重启。
- **通道状态文件是唯一外部可观测面**，只在进程存活时更新（stop 后残留旧值）。
