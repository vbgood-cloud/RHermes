# 文档-代码一致性审计报告

> **审计对象**：RHermes `v0.7.27`（HEAD `521ba66`）
> **范围**：`README.md` · `FEATURES.md` · `docs/**` 全部描述性文档 ↔ `src/**` 实际实现
> **方法**：逐模块通读源码 + 交叉核对文档陈述；对可疑数据（cron 段数、工具数、硬编码处数）做**脚本重算**或**实证检索**
> **产出**：① 修正/补写文档（见 §一）② 发现但**未改动代码**的缺陷清单（见 §二，供决策）

---

## 一、本轮修正 / 补写的文档

### 1.1 修正（与代码不符处）

| 文档 | 主要修正 |
|------|---------|
| `README.md` | 版本徽章 0.7.20→**0.7.27**；工具数 26→**33**；「加权轮询」→**主备自动切换**；新增「渠道成熟度」表；代码规模 41,900→**47,677**；测试 377→**414+6=420**；黑名单 70+→**75 条** |
| `FEATURES.md` | 方法名 `search_memories()`→`search()`；`SkillManager`→`SkillEngine`；删「内置 4 种技能分类」；「LLM Review Pass」标**未实现**；SubAgent 描述纠正；新增 **§1.11 Judge 判断层**；工具数 25→**33**；§3.2 dispatcher 改为真实策略；§5.1 删「加权轮询」；§6.7 Web 标 ❌；统计表重写 |
| `docs/architecture.md` | **完全重写**（原停在 v0.7.0、缺 9 个模块）；新增 §十五「已知未完成项与死代码（诚实清单）」；模块表补 `docs/modules/` 链接 |
| `docs/guardrails.md` | Flatten 标「未接入生产路径」；Scavenge「用正则」→「字符串定位 + `serde_json::from_str`」；Storm 补注两份默认值不同；「校验全拦截→continue」改为「不 continue，直接落入最终回复」 |
| `docs/plans.md` | 加时效提示；进度总览 17→**33 个工具**；Repair 标 ⚠️ 三工序生效；成本控制标 ⚠️ 未集成；ADR-006/007 修正；新增 **ADR-008/009/010** |
| `docs/CHANGELOG.md` | **补写 27 个版本**（v0.7.1~v0.7.27，日期取自 `git log`）+ v0.7.0 前过渡期小节 |
| `docs/code-review-report.md` | 加历史快照横幅（v0.4.4/54 文件/2026-06-09），指向 `architecture.md` + `codegraph.md` |
| `docs/learn-export-import.md` | 标题 `（v1）`→`（v3）`；JSON 示例补 `stability`/`next_review_at`/`easiness`；新增版本演进表；实现位置修正（`store.rs` 非 `mod.rs`） |
| `docs/learn-spaced-review.md` | ③ 下界 `60`→`review_floor`（可配置）；SVG 三档→**四档**；`kb_stats` 段落重写；Bento 网格改**3 列×2 行**；测试数改 **414+6** |
| `docs/learn-user-guide.md` | 版本号 v0.7.22→**v0.7.27**（2 处）；③ 下界注明 `review_floor`；Bento 网格更正；**D2 条目改写**（中文库名自 v0.7.22 已原样保留，旧 FAQ「变一串下划线」已过时） |
| `docs/learn-largefile.md` | 加复核说明（内容与实现完全一致，无需改动） |
| `docs/wechat-setup.md` | 二维码**实为 BMP 内容**（扩展名仍 `.png`）；「有效期约 2 分钟」无代码依据→改为「服务端决定，`Expired` 自动重取」；token 失效路径改为「自动清 token 并**重新扫码**」；补 Gateway 模式文件名差异；补非 Windows 不自动打开查看器 |
| `docs/proxy.md` | 功能开关表补 `jev`（判断层）；澄清「默认值」两层次（生成模板 vs `ProxyConfig::default()` 全 false）；说明 `[proxy]` 缺省时**一律不走代理** |
| `docs/OMNIROUTE_INTEGRATION.md` | 加「实施进度复核」横幅；§一 状态表按代码标注 ✅/🟡/❌；**P1 改动 1、2 已落地**（`reasoning_effort` / `reasoning_content` / `cached_tokens` 字段均在）；**改动 3 仍是空块**（思考流无生产者）；P2 标注「effort 推断已做、model 路由未做」；§五 改「含当前进度 + 下一步最小动作」 |
| `docs/edu-p2p-design.md` | `teacher_id` 硬编码 5 处→**12 处**（2 处）；状态「设计待确认」→「主体已实现」 |
| `docs/edu-p2p-e2e-flow.md` | 适用版本 v0.7.15→**v0.7.27**；`teacher_id` 处数更正；「教师端只发不收」→**v0.7.16 起 `spawn_answer_loop` 已收件 + 自动答疑**（仅交互台不上屏） |
| `docs/edu-p2p-testing.md` | 适用版本 → **v0.7.27** |
| `docs/edu-guide.md` | **完全重写**：新增「成熟度总览（照实说）」表；补 `serve`/`addr`/`live`/`sync`/`revoke`/`announce`；修正通道成熟度（QQ 未收尾 / Web 不可用）；「教师远程切换学生模式」标**未接通**；补「已知未完成项」 |
| `docs/edu-connection-plan.md` | 加**历史规划文档（已被取代）**横幅；指出 axum HTTP 降级未实现 |
| `docs/edu-plan.md` | 加**历史规划文档（部分已落地）**横幅 |
| `docs/edu-teaching-plan.md` | 加**设计已落地**横幅 + 表名/接口对照 + 入口说明（斜杠命令非 CLI） |
| `docs/codegraph_gen.py` | **代码修复**：`ROOT` 由 `~/lab/RHermes` 改为基于脚本自身位置（Windows 下原路径展开为 `C:\Users\admin\lab\RHermes` 直接崩）；输出头部去掉写死的版本/计数 |
| `docs/codegraph.md` | 重新生成（110 文件 / 47,787 行 / 725 `pub fn` / 414 测试；含 `knowledge/` `judge/`） |
| `docs/RHermes帮助文档.md`（855 行） | 加**历史快照（v0.3.0 / 2026-06-09）**横幅 + 现行文档导引表；修正「加权轮询」→ 主备降级（2 处）；「内置工具 17 个」→ 标注现为 **33 个**；文末补历史标注 |
| `docs/decisions/D17-proxy.md` | 状态「设计完成」→ **已实施**；指向 `docs/proxy.md`；注明后续新增 `jev` 键不在原始清单 |
| `docs/decisions/D18-jev-judge.md` | 状态「设计完成，待 P0 验证后实施」→ **已实施**（P1–P3 五处集成点全落地） |
| `docs/D35-liteparse-integration.md` | 状态「设计完成」→ **已实施**，列出 8 个已注册文档类工具 |
| `docs/D35-implementation-prompt.md` | 标注为**历史工作指令（已执行）** |
| `README.md` | 新增「文档导航」表（指向 `architecture.md` / `modules/` / 审计报告等） |

### 1.2 补写（原无叙事文档的模块）

新建 **`docs/modules/`** 目录，7 篇 + 索引：

| 文档 | 覆盖 |
|------|------|
| `modules/README.md` | 索引 + 与其他文档的分工说明 |
| `modules/scheduler.md` | cron 调度器：配置、启动对齐、Semaphore、`CaptureSink`、**cron 段数陷阱** |
| `modules/plugin.md` | `Plugin` trait / `PluginRouter` / `registry.toml` / 四种来源 / **两条 Wasm 加载路径并存** |
| `modules/mcp.md` | 三传输（stdio/SSE/Direct）、`McpAdapter`、`mcp__{server}__{tool}`、健康检查 300s、**无热刷新** |
| `modules/provider.md` | `Transport` trait、`ProviderPool` + 熔断、**主备降级（非加权轮询）**、工厂优先级、默认 base_url 表 |
| `modules/judge.md` | 三原语、门面 API、门控语义、**五处集成点**、铁律（失败即回退） |
| `modules/gateway.md` | 启动 10 步、生命周期管理、**硬编码工具数等缺陷** |
| `modules/channel.md` | `Channel` trait / `ChannelManager`、**六渠道成熟度实测表**、配置片段 |

---

## 二、发现的代码缺陷（**未改动代码**，供决策）

### A. 死代码 / 未接线

| # | 位置 | 事实 | 影响 | 建议 |
|---|------|------|------|------|
| **A1** | `provider/transport.rs:222-226` | `choice.delta.reasoning_content` 的 `if` 块**是空的**，从不 `tx.send(ApiEvent::Thinking(..))` | 思考流**永远到不了界面**。`ApiEvent::Thinking` 与 `ApiEvent::ProviderMeta` 均为「**有消费者、无生产者**」：`agent/session.rs:487` 会累积 `thinking_buf`、`tui/mod.rs:840/843` 有渲染分支、`/thinking` 开关也已就绪 —— 唯独上游断流 | 补 3 行 `tx.send(ApiEvent::Thinking(..))`；并在 transport 解析 `x-omniroute-*` 尾注以产出 `ProviderMeta` |
| **A2** | `provider/pool.rs:127` | `ProviderEntry.weight` **只写不读**；`next_healthy()` 实为「主 provider 优先 + 熔断降级轮询」 | 无加权轮询；`factory.rs` 传的 `weight=10/1` 无任何效果 | 要么删除字段（并改正模块注释），要么实现加权选择 |
| **A3** | `src/cost.rs`（**560 行**） | `CostController` / `ModelTier` / `CostPreset` 等**全项目零引用** | 成本控制**未集成**，功能缺失 | 接线到 `ProviderPool`/`AgentSession`，或明确标注为预留 |
| **A4** | `api/mod.rs:494-800` | `DeepSeekClient`（含 `impl Transport`）**零引用**，与 `provider::DeepSeekTransport` 功能重复 | 两套客户端易混淆，改动易漏改 | 删除（或改为 `#[deprecated]` + 迁移） |
| **A5** | `channel/types.rs:38` | `OutboundMessage` 定义后零引用 | 死类型 | 删除 |
| **A6** | `agent/repair.rs` | **Flatten 工序在生产路径从不调用**：`repair_with_api()` 只走 Scavenge + Truncation；`FlattenRepair::flatten` 仅出现在 `#[cfg(test)]` | `RepairAction::Flattened` / `RestoredNested` **永不产生**；文档声称的「三工序生效」需修正为「两工序」 | 接线 Flatten，或在文档中明确其为未启用工序（`guardrails.md` 已标注） |
| **A7** | `agent/curator.rs:47` | `UmbrellaAction` 仅枚举定义，无构造/使用 | 死枚举 | 删除或实现 |
| **A8** | `channel/web/` | `WebChannel` **全项目无任何构造点**；`send_message()` 是空实现；`/api/reply` 无写入方 | Web 对话通道**不可用**（`edu-guide.md` 旧版误称可用，已修正） | 补完并注册，或删除以缩小体积 |
| **A9** | `src/edu/p2p.rs`（**761 行**） | 仅 `edu/mod.rs:21` 的 `pub mod p2p;` 声明，无其他引用（含 `ClassroomMessage::ModeSwitch`） | 遗留模块，与新 `SectionMsg` 协议重复 | 删除 |

### B. 与用户可见行为不符（会误导用户 —— 优先修）

| # | 位置 | 事实 | 影响 |
|---|------|------|------|
| **B1** | `core/config.rs:1332` | `[scheduler.tasks]` 模板示例是 `cron = "0 9 * * 1-5"`（**5 段**），但 `cron = "0.13"` 要求 **6–7 段**（`秒 分 时 日 月 周 [年]`） | **照抄必失败**：任务被静默跳过，只有一条 warn。同一字段的文档注释也写成 5 段 |
| **B2** | `lib.rs:977` · `gateway/mod.rs:286` · `tui/mod.rs:510` | system prompt **硬编码** `## 可用工具（共 17 个）` 并列举 17 个工具名（`lib.rs` 版还含 `read_pdf` 却不含 `kb_*`） | 实际注册 **33 个**。模型被喂了错误的工具清单，可能不敢用新工具或幻觉不存在的工具。**三处均需改为动态生成**（`all_tool_defs()` 已存在） |
| **B3** | `channel/qq/mod.rs:297` | `state_connected()` **恒返回 `false`** | `status()` 永远报未连接，Gateway 启动状态表与 `channel_status.json` 对 QQ 失真 |
| **B4** | `gateway/mod.rs::channel_list` · `generate_annotated_template()` · `mcp/setup.rs` 同类向导 | **均不含 QQ**，但 `channel enable qq` 与 `gateway status` 都认 QQ | 用户按 `channel list` 指引无法发现 QQ 通道存在 |
| **B5** | `core/config.rs::generate_annotated_template()` | 不产出 `[channels.qq]` / `[edu]` / `[knowledge]` 三段（虽为合法段） | 新用户不知这些段存在 |

### C. 一致性 / 健壮性

| # | 位置 | 事实 |
|---|------|------|
| **C1** | `provider/mod.rs:1-4` · `pool.rs:1-4` | 模块注释仍写「**加权轮询**」，与实现（主备降级）不符。**文档已修正，代码注释未改** |
| **C2** | `agent/repair.rs` | 模块头把 Flatten 列为「工序 1」，而 `repair_with_api()` 内的注释是「工序 1: 收集 tool_calls / 工序 3: Truncation」，编号跳号且与模块头不对应 |
| **C3** | `agent/router.rs::reply_to_channel` | `channel_mgr.get(name)` 返回 `None` 时**静默丢弃**，无日志无回落 |
| **C4** | `edu/*` | `teacher_id = 1` 硬编码 **12 处**（`edu/mod.rs` 5 · `router.rs` 3 · `edu/teacher.rs` 3 · `dashboard.rs` 1；另有 8 处测试夹具）。多老师共库会串数据（设计 S2.4 待办） |
| **C5** | `edu/mod.rs` 透传层 | `rhermes edu join/status/courses/profile/report/mode` 均为**占位提示**（`（Phase 4+ 实现）` / 引导去用 `rhermes-stu`），而 `edu` 帮助文本把它们列为可用命令 |
| **C6** | `core/config.rs` | `PathManager::detect()` 以**可执行文件目录**为锚（ADR-008）——可移动模式成立，但换目录后 `home/` 随 exe 走；文档已在 `architecture.md` 说明，**代码注释未点明这是可移动性来源** |

---

## 三、建议的后续动作（按性价比）

1. **B2 / B1**：修 3 处硬编码工具清单（改用 `all_tool_defs()` 动态生成）+ 修 cron 模板示例为 6 段。**用户可见、改动小、收益大。**
2. **A1**：补 `transport.rs` 的 `reasoning_content` 分发（3 行）→ 立刻点亮已就绪的 `/thinking` 思考流展示。
3. **A2 / C1**：删除 `weight` 死字段并改正 2 处模块注释（或实现加权）。
4. **A3 / A4 / A5 / A7 / A8 / A9**：一次性清理死代码（约 **1,900 行**：`cost.rs` 560 + `edu/p2p.rs` 761 + `api/mod.rs` 旧客户端约 300 + 其余零头）。
5. **A6**：接线 Flatten 工序，或将其正式标记为「未启用」。
6. **B3 / B4 / B5 / C3**：补齐 QQ 的状态与配置可见性；`reply_to_channel` 加 warn 日志。
7. **C4 / C5**：纳入设计文档中的 S2.4（双入口收敛 + 清 12 处硬编码）。

> ⚠️ 本报告只记录**事实与建议**，**未改动任何生产代码**。除 §1.1 中明确标注的两处（`codegraph_gen.py` 的路径与头部、`docs/` 全部文档）外，`src/` 未被修改。
