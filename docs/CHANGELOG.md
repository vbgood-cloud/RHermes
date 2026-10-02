# CHANGELOG

## v0.7.28 — 2026-10-02

### 文档
- 新增 D19 设计：企微机器人切换智能机器人长连接（stream 模式，免回调）—— 补齐 `docs/decisions/D19-wecom-stream.md`

## v0.7.27 — 2026-10-01

### 文档
- learn 用户指南修正：初始化命令纠错（`setup` → `init`）、新增「三个初始化别搞混」对照表、补微信端导入说明

## v0.7.26 — 2026-10-01

### 文档
- learn 用户指南新增「导入知识库快速开始」章节

## v0.7.25 — 2026-10-01

### 修复
- **学习模式答题后 Agent 无响应**：模型返回空文本时静默结束 → 增加空响应兜底（最多补问 2 次）

## v0.7.24 — 2026-10-01

### 修复
- `/summary` 与 `/stop` 时同步更新知识图谱与 Bento 战绩

## v0.7.23 — 2026-10-01

### 修复
- `/learn` 建库时库名被 LLM 改写为英文 / 下划线

## v0.7.22 — 2026-09-30

### 新增（D18 P2 / P3）
- **P2 技能生命周期巡检**：`Curator::run_with_judge()` —— 判断层结论置信达标时覆盖时间规则，未达标 / 失败 / 未启用一律回退
- **P3a 修复可信度门控**：仅当修复产出 tool_calls 时用 noul 判定（阈值 0.75），只有明确不可信（p ≤ 0.25）才注入纠正重试
- **P3b 学习反思三档位**：0.25 / 0.6 / 0.95（禁插值；失败保持启发式分数）
- **P3c 搜索结果预筛**：一次 fan-out 判定全部结果，p < 0.5 滤除；过滤后为空则回退全量
- `Judge::min_confidence()` 暴露门控阈值

### 修复
- 知识图谱中文文件名被改写为下划线

### 文档
- 新增 learn 用户指南（详细版，含 FAQ 与实操剧本）

## v0.7.21 — 2026-09-28

- D18 P1 端到端测试：winnow@11435 生产链路验证通过

## v0.7.20 — 2026-09-28

### 新增
- **D18 P1**：记忆保存判断接入判断层（生产端点）—— 判定为临时信息则跳过落库（fail-open）

## v0.7.19 — 2026-09-28

- D18 P0 门槛测试支持端点 / 模型参数化，winnow:e4b 评估通过

## v0.7.18 — 2026-09-27

### 新增
- **D18 判断层基础设施**：新增 `src/judge/` 模块（`Judge` / `Question` 三变体 / `JudgeClient` / `TypedAnswer`）+ P0 门槛测试

## v0.7.17 — 2026-09-27

### 变更
- 全仓库行尾统一为 LF
- 文档同步代码现状；新增 D18 Jev 判断层决策方案

## v0.7.16 — 2026-09

### 新增
- **教师端 AI 答疑循环**（`spawn_answer_loop`）：收到学生提问自动调 LLM（60s 超时）并以 `Answer` 广播回班

### 修复
- 教师 AI 答疑四项修复

## v0.7.15 — 2026-09-16（S2.2）

### 变更
- **REPL 迁移为第一个 `SectionHost` 驱动**：`run_live` 改为 `SectionHost::start(session)` → `repl(&host)`；
  本地 `render_event` 删除，统一走 `host::render_event`；`ask` / `chat` 走 `host.ask` / `host.chat`

### 验证
- CLI 黑盒冒烟输出与 v0.7.13 逐字一致；全量 404 测试通过

## v0.7.14 — 2026-09-16（S2.1）

### 新增
- **`src/edu/host.rs` — SectionHost**：驱动无关的教学班会话宿主。`HostCommand` 进 / `HostEvent` 出
  （broadcast 多订阅者），驱动不再直接接触 `StudentRuntime`
- 纯函数 `build_labels` / `translate` / `render_event`（输出与 v0.7.13 逐字一致）
- 15 条单测 + 1 个含负控的 e2e

## v0.7.13 — 2026-09-16

### 修复（真机 CLI 冒烟暴露的 3 个缺陷）
- `EduStore::open` 不创建父目录 → 便携目录首次运行失败
- `init-teacher` 被通用 `init` 顶掉（clap 变体重名）→ 显式 `#[command(name = "init-teacher")]`
- `student add` 按位置读 argv（4 / 5 参数混淆）→ 抽出纯函数 `parse_student_add`
- `identity::sanitize` 纯中文名退化问题

## v0.7.12 — 2026-09-16

### 新增
- **edu 身份持久化**：`edu_identities/<owner>.key`，一把钥匙进多个班
- **配置驱动**：`[edu.teacher]` 持久化凭据 + `[[edu.student.teachers]]` 多老师清单 + `[[edu.teacher.serve]]` 托管清单

### 修复
- 撤销 / 待认证混淆；`TopicRotate` 只报纪元不带新 topic；iroh `after_handshake` 双向调用 → 白名单只在 Server 侧强制；学生先拉白名单再订阅 Topic

## v0.7.11 — 2026-09-16

### 新增
- **edu 多老师拓扑**：`SectionKey { teacher: EndpointId, section_id }` 全局班键 —— 一位学生可同时接入多位老师、互不串台

### 修复
- 轮换丢发送槽、撤销误伤他班、`WhoAmI` 串班

## v0.7.10 — 2026-09-15

- edu P2P 端到端联调：补齐客户端 + 修复 4 个真实缺陷

## v0.7.9 — 2026-09-11

### 新增
- **edu 去中心化 P2P 教学班通信**：iroh 双 ALPN（`/class-auth/1.0` 开放 + `/class-app/1.0` 受控）、
  Topic 隔离（`BLAKE3("rhermes-section-topic" ‖ seed ‖ epoch)`）、iroh-blobs 作业文件分发、
  R3 组合撤销（落库 + 轮换 epoch + 旧 Topic 广播 + 新表签名广播）

## v0.7.8 — 2026-09-11

### 新增
- `/learn` SM-2 难度系数 EF（调和式）：`EF' = EF + 0.1 − (5−q)(0.08 + 0.02(5−q))`，钳 [1.3, 2.8]
- Bento 待复习面板

## v0.7.7 — 2026-09-10

### 新增
- `/learn` 遗忘曲线（间隔复习）：`retention = 2^(-Δt/S)`，S 分档增长因子 ×2.5 / ×2.0 / ×1.5 / ×0.7，钳 [0.5, 180]
- 可配置复习红线 `[knowledge].review_floor`（默认 60）
- 图谱按有效掌握度自动变浅

## v0.7.6 — 2026-09-04

- 图谱文字对比度修复 + `/learn` 持续教学 + 学习进度复位

## v0.7.5 — 2026-09-03

- `/learn` 建库进度心跳：每分钟推送实时进度（TUI 因有进度条而跳过）

## v0.7.4 — 2026-09-03

- `/learn` 建库闭环校验：节点数不足自动重试（最多 3 次，最少 5 节点）

## v0.7.3 — 2026-09-03

- `/learn` 知识库导出与导入（`.kb.json`）—— 备份 / 换机 / 分享

## v0.7.2 — 2026-09-01

### 修复
- `/learn` 大文件内容丢失 → `kb_append` 分段读取（`CHUNK_CHARS = 20000`，>24000 字符落盘后逐段读）

## v0.7.1 — 2026-08-30

### 新增
- **OmniRoute 适配**：DSML tool_calls 解析 + Provider 权重 + think 过滤
- `/learn` 支持目录建库；session 懒创建；调试日志

### 修复
- 学习模式 kickoff 统一加「不展示思考过程、回复全部中文」指令
- 工作目录绝对路径解析；ProviderPool 改为**主 provider 优先**路由
- 配置模板补 `providers.omniroute` 与默认 URL

## v0.7.0 之前（未打 tag 的过渡期）

- `/learn` 知识库学习系统落地：图谱点亮 + 引导式学习 + Bento 战绩
- `/learn /stop` 全通道支持；`/summary` 阶段总结
- **TUI 作为 Gateway 的一个 channel**（D13 统一会话架构）
- TUI 统一 channel 后，`/learn` 在 Telegram / 企微 / 微信 / QQ 均可使用
- edu 分支合并入 master

## v0.7.0 — 2026-08-18

### 新增
- **Extism Host Functions 安全网关**（P28 核心）：`src/tools/wasm_host_functions.rs`
  - 6 个宿主函数：`host_log`（永远注册）/ `host_http_get` / `host_http_post` / `host_read_file` / `host_write_file` / `host_exec`（默认禁用）
  - 每插件权限声明 `<name>.host.toml`（allowed_hosts / allowed_paths / allow_exec），无声明 = 最小权限纯计算
  - 域名白名单支持 `*` / `*.example.com`；路径白名单 canonicalize 防 `../` 逃逸
  - Manifest 强制内存上限 + 超时；Wasm execute 改走 `spawn_blocking`
- **统一 Plugin 系统**（P28，commit cc5c39e）：`src/plugin/` 四文件
  - `Plugin` trait：`descriptor()` / `execute()` / `health()` / `reload()`
  - `PluginRouter`：RwLock 注册表 + 全局 OnceLock 单例
  - `ExtismPlugin`（复用 WasmPluginTool + host functions）与 `SkillMdPlugin`（SKILL.md frontmatter）两种适配器
  - `plugins/registry.toml` 显式声明优先（per-plugin 权限），无配置时目录扫描（向后兼容）
  - `run_plugin` 内置工具：`__list__` 列出插件，Agent 可直接调用
- **MCP P2 协议扩展**
  - `notifications/tools/list_changed` 自动热刷新（tools 改 `Arc<RwLock>`，后台 task 重拉 tools/list）
  - `resources/list` + `resources/read` 支持（`McpResourceInfo` / `list_resources()` / `read_resource()`）
- Wasm 插件 `load_with_access()`：权限显式注入（registry.toml 场景）

### 变更
- 版本 0.6.9 → 0.7.0
- extism 依赖确认实际解析 1.30.0（Cargo.toml 声明 "1.5"）
- `McpAdapter::tools()` 返回 `Vec<McpToolInfo>` 快照（原 `&[McpToolInfo]`），支持热更新后读取

### 测试
- 246 → 267（+21：host functions 权限 7 + Plugin 系统 11 + 其他 3），全部通过

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
