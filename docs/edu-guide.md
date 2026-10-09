# RHermes Edu 教育版使用指南

> 适用版本：**v0.7.27**
> 相关文档：通信原理见 `docs/edu-p2p-design.md`；测试方法见 `docs/edu-p2p-testing.md`；
> 端到端操作手册见 `docs/edu-p2p-e2e-flow.md`。

## 成熟度总览（照实说）

| 能力 | 状态 | 说明 |
|------|------|------|
| 课程/班级/课次/学生 管理（CLI） | ✅ 可用 | `course` / `class` / `lesson` / `student` |
| 学生认证登录（argon2） | ✅ 可用 | `rhermes-stu login` |
| 学习模式 explore/scaffold/locked | ✅ 可用 | 本地切换 |
| **P2P 教学班（iroh）** | ✅ 可用 | `serve`（教师）/ `live`（学生）；双 ALPN + 签名白名单 |
| 学生提问 → 教师自动 AI 答疑 | ✅ 可用（v0.7.16+） | 需配 `[agent].default_provider`；教师端仅在**日志**可见提问 |
| 教师仪表板（Web） | ✅ 可用 | `dashboard`，端口 8080 |
| 教务名单同步 | ✅ 可用 | `rhermes edu sync`（CSV / HTTP 两路） |
| 撤销成员 / 公告广播 | ✅ 可用 | `edu revoke` / `edu announce` |
| 作业文件分发（blobs） | ⚠️ 代码就绪，**无 CLI 入口** | 只能用 Rust API |
| 恢复授权（撤销的逆操作） | ⚠️ **无 CLI 入口** | 仅有 `TeacherRuntime::restore_member` |
| QQ 通道 | ⚠️ 未收尾 | `state_connected()` 恒 `false` |
| Web 网页对话通道 | ❌ 半成品 | `WebChannel` **未被任何地方构造**；`send_message` 为空实现 |
| 教师在**交互台**看到学生提问 | ❌ 未接通 | 目前仅日志可见（上屏待 S2.3） |
| 教师**远程**切换学生模式 | ❌ 未接通 | 仅存在于遗留 `p2p.rs`，当前 `SectionMsg` 协议无此消息 |
| 学生端多前端（TUI/微信/企微/TG） | ❌ 未接通 | `ask`/`chat` 目前只能在 `live` 交互台使用 |

---

## 快速开始

> 💡 **两个等价入口**：`rhermes-teacher <cmd>` ≡ `rhermes edu teacher <cmd>`；`rhermes-stu <cmd>` ≡ `rhermes edu student <cmd>`。
> 下文以更短的 `rhermes-teacher` / `rhermes-stu` 为准，并给出 `edu` 兼容写法。

### 教师端

#### 1. 初始化教师身份

```bash
rhermes-teacher init-teacher 张老师 密码     # 推荐（别名 create-teacher）
# 兼容写法：rhermes edu teacher init 张老师 密码
```

> ⚠️ **必须用 `init-teacher`**（不是 `init`）：`rhermes init` 是通用「交互式配置向导」，会顶掉教师初始化。
> 姓名与密码可省略（省略则交互式询问）；**无 TTY 的脚本场景务必显式给出**，否则会静默取到空串。

#### 2. 创建课程

```bash
rhermes-teacher course create CS101 "Python 编程基础"
rhermes-teacher course create CS201 "数据结构"
rhermes-teacher course list
```

#### 3. 创建班级

```bash
rhermes-teacher class create CS101 计算机2301
rhermes-teacher class create CS101 计算机2302
```

#### 4. 创建课次

```bash
rhermes-teacher lesson create CS101 计算机2301 1 "变量与数据类型"
rhermes-teacher lesson create CS101 计算机2301 2 "条件语句"
rhermes-teacher lesson create CS101 计算机2301 3 "循环结构"
```

#### 5. 添加学生

单个添加（**5 个参数**，含密码）：
```bash
rhermes-teacher student add 2024001 张三 CS101 计算机2301 123456
```

> ⚠️ **只给 4 个参数时**（学号/姓名/课程码/班级），无 TTY 环境会静默使用兜底密码 `123456`；交互式环境会提示输入。

批量导入（CSV 格式：`学号,姓名,密码`，密码可省略，默认 `123456`）：
```bash
rhermes-teacher student import students.csv CS101 计算机2301
```

CSV 文件示例：
```csv
2024001,张三,pass123
2024002,李四,mypass
2024003,王五
```

#### 6. 查看花名册

```bash
rhermes-teacher roster              # 列出全部课程/班级
rhermes-teacher roster CS101        # 查看 CS101 的花名册
# 兼容写法：rhermes edu teacher list [课程码]
```

> ⚠️ `roster` **只列出班级与课次，不逐行列出学生**；学生表名为 `edu_students`。

#### 7. 启动教师仪表板

```bash
rhermes-teacher dashboard
```

浏览器打开 `http://localhost:8080` 查看学生学习数据。

#### 8. （核心）托管教学班 —— P2P 在线托管

这是教育版的主线用法：教师起一个**长期在线**的进程，托管若干「课程 × 班级」教学班，学生即可接入。

```bash
rhermes-teacher serve CS101 计算机2301 CS201 计算机2302   # 指定托管项
rhermes-teacher serve --all                              # 该老师名下全部教学班
rhermes-teacher serve CS101 计算机2301 --offline          # 离网直连（无中继/发现）
```

启动后：
1. 先打印**老师地址**（学生填配置要用）
2. 逐班入班：建 gossip 会话 + 广播签名白名单
3. 进入 `edu> ` 交互台，可用 `list` / `announce` / `chat` / `addr` / `help` / `quit`

> ⚠️ **教师端不会把学生提问打到交互台上**，但自 v0.7.16 起会：把 `收到提问 …` 写进日志，并在配置了 AI 时**自动答疑并广播回答**（署教师真名，单问 60s 超时、并发不互相阻塞）。

#### 9. 打印老师地址（让学生抄配置）

```bash
rhermes-teacher addr
# 兼容写法：rhermes edu teacher addr
```

输出：身份文件路径、12 位指纹、64 位老师地址，以及可粘贴的**学生配置片段**。
> ⚠️ 该命令**只给地址/公钥，不含 ip:port**；需要完整 `ip:port` 请看 `serve` 启动横幅。

---

### 学生端

#### 1. 配置 AI Provider

首次使用需要配置 API Key（学生自带）：
```bash
rhermes init
```

#### 2. 认证登录

```bash
rhermes-stu login 2024001 密码
```

#### 3. 进入课堂（P2P 教学班）

在 `config.toml` 里按老师给的片段配置多位老师，然后：

```bash
rhermes-stu live              # 一次接入配置中的所有教学班
rhermes-stu live --offline    # 离网直连
```

进入交互台后可用：`ask <问题>`、`chat <内容>`、`list`、`quit` 等。

> ⚠️ `ask` / `chat` **不是 CLI 子命令**，而是 `live` 交互台**内部**的命令。

学生配置片段示例：
```toml
[[edu.student.teachers]]
teacher = "<老师的 64 位地址>"
student_no = "2024001"

[edu.teacher]
account = "张老师"        # 教师端：必须与 init-teacher 时填写的标识完全一致
```

#### 4. 查看状态 / 课程 / 档案 / 报告

```bash
rhermes-stu status
rhermes-stu courses
rhermes-stu profile
rhermes-stu report
rhermes-stu join CS101
```

#### 5. 切换课程（交互台内）

```
/sw              # 列出已加入的课程
/sw CS101        # 切换到 CS101
/sw CS101#3      # 切换到 CS101 的第 3 次课
```

---

## 学习模式

| 模式 | 说明 | 工具限制 |
|------|------|---------|
| 🔬 `explore` 探索模式 | 完整 AI，自由提问 | 全部允许（按课程白名单） |
| 🧭 `scaffold` 引导模式 | 苏格拉底式追问，不直接给答案 | 全部允许 |
| 🔒 `locked` 考试模式 | AI 禁用或仅提供查阅 | 只读工具 |

本地切换：`rhermes-stu mode explore` / `mode scaffold`，或交互台内 `/mode <模式>`。
> ⚠️ 「**教师远程**切换学生模式」当前**未接通**（仅遗留 `p2p.rs` 里有 `ModeSwitch`，现行 `SectionMsg` 协议不含该消息）。

---

## 通道配置

教育版学生**预期**可经任意通道与 AI 交互，但各通道成熟度不一。

### Telegram 通道 ✅

在 `.env` 中配置：
```
TELEGRAM_BOT_TOKEN=你的token
```

### 微信个号通道 ✅

```bash
rhermes init    # 配置微信通道并扫码登录
```
详见 `docs/wechat-setup.md`。

### 企业微信通道 ✅ 文本可用

```bash
rhermes gateway setup    # 选「企业微信 Bot」→ 填 BotID → 填 Secret（明文显示便于核对）
```

- 走**智能机器人 WebSocket 长连接**（API 模式）：免公网回调、免 AES 加解密、支持单聊问答
- 配置项：`[channels.wecom]` 的 `bot_id` + `.env` 的 `WECOM_BOT_SECRET`；`mode` 现只有 `stream`
- ⚠️ 旧的 **webhook + 自建应用轮询链路已删除**（v0.7.34）：存量配置里的 `webhook_url` / `corp_id` / `agent_id` / `WECOM_SECRET` **不再生效**
- ⚠️ 同一机器人只允许 1 个长连接，**一机一 bot**；当前为一问一答，暂无打字机流式效果
- 详见 `docs/wechat-setup.md` 第二节与 `docs/decisions/D19-wecom-stream.md`

### QQ 通道 ⚠️ 未收尾

1. 在 [QQ 开放平台](https://q.qq.com) 注册 Bot，获取 AppID 和 AppSecret
2. 在 `.env` 中配置：
   ```
   QQ_BOT_APP_ID=你的AppID
   QQ_BOT_APP_SECRET=你的AppSecret
   ```
3. 在 `config.toml` 中启用：
   ```toml
   [channels.qq]
   enabled = true
   allow_private_chat = true
   ```

> ⚠️ **已知缺口**：`QqChannel::state_connected()` 恒返回 `false`，连接状态无法反映真实情况。

### Web 网页对话通道 ❌ 半成品

`docs` 旧版曾称「学生通过浏览器访问 `http://localhost:8090` 与 AI 交互」——**该能力当前不可用**：

- `ChannelsConfig` 只含 `wecom` / `wechat` / `telegram` / `qq`，**没有 `[channels.web]`**
- `WebChannel` 存在于 `src/channel/web/mod.rs`，但**全项目没有任何地方构造它**（未注册进 `ChannelManager`）
- 其 `send_message()` 是**空实现**（仅注释「回复通过 WebSocket/轮询获取」），`/api/reply` 无写入方

> 目前唯一可用的 Web 界面是**教师仪表板**（`dashboard`，端口 **8080**），用于查看学习数据，不提供对话。

---

## 反思系统

对话结束后，系统会生成针对性的反思提示（示例）：

- "你刚才用了 read_file 工具——你为什么选择先读文件而不是直接搜索？"
- "AI 给出的方案中，你觉得哪部分可能是错的？你会怎么验证？"
- "如果不用 AI，你会怎么解决这个问题？第一步做什么？"

反思综合评分（0-1，对应 0-100%）权重（`src/edu/reflection.rs::calculate_overall`）：

| 维度 | 权重 | 计算依据 |
|------|------|---------|
| 反思深度 `depth` | **30%** | 字数、关键词、结构 |
| 提问质量 `question_quality` | **25%** | 对话轮数反推 |
| 工具使用合理性 `tool_usage_quality` | **25%** | 默认中等 |
| AI 透明度理解 `ai_transparency` | **20%** | — |

---

## CLI 命令参考

### 教师命令（`rhermes-teacher` ≡ `rhermes edu teacher`）

| 命令 | 说明 |
|------|------|
| `init-teacher [姓名] [密码]`（别名 `create-teacher`） | 初始化教师身份。⚠️ 不是 `init` |
| `course create <码> <名>` / `course list` | 创建 / 列出课程 |
| `class create <码> <班>` | 创建班级 |
| `lesson create <码> <班> <序号> <主题>` | 创建课次 |
| `student add <学号> <姓名> <课程码> <班级> [密码]` | 添加学生 |
| `student import <CSV> <课程码> <班级>` | 批量导入 |
| `roster [课程码]` | 课程 / 花名册 |
| `dashboard` | 启动 Web 仪表板（:8080） |
| `serve [<码> <班>…] [--all] [--offline]` | 长期在线托管教学班 |
| `addr` | 打印本机老师地址（供学生填配置） |

### 学生命令（`rhermes-stu` ≡ `rhermes edu student`）

| 命令 | 说明 |
|------|------|
| `login <学号> <密码>` | 认证登录 |
| `join <课程码>` | 加入课程 |
| `live [--offline]` | 进入课堂，同时接入多位老师的多个教学班 |
| `status` | 查看学习状态 |
| `courses` | 可选课程列表 |
| `profile` | 个人学习档案 |
| `report` | 成长报告 |
| `mode [explore\|scaffold]` | 切换 / 查看学习模式 |

### 仅 `rhermes edu` 形式提供

| 命令 | 说明 |
|------|------|
| `rhermes edu sync <CSV> <roster_dir> <工号> <姓名> <学期>` | 教务名单同步（离线 CSV） |
| `rhermes edu sync <https://教务地址> <token> <工号> <姓名> <学期>` | 教务名单同步（HTTP） |
| `rhermes edu revoke <课程码> <班级> <学号>` | 撤销成员并轮换 Topic |
| `rhermes edu announce <课程码> <班级> <标题> <正文>` | 班内广播（短命进程，离线学生收不到） |
| `rhermes edu auth <login\|verify>` | 命令行认证 |

> ⚠️ `rhermes edu join/status/courses/profile/report/mode` 这些透传形式目前多为**占位提示**（会引导你去用 `rhermes-stu`）；请优先用 `rhermes-stu`。

---

## 数据存储

教育数据存储于 **`home/edu.db`**（SQLite，路径以**可执行文件目录**为锚）：教师、课程、班级、课次、学生（密码 argon2 哈希）、选课记录、学习日志、认证会话、教学班成员表（`edu_section_members`）。

### 成长报告

学生的成长报告（Markdown）包含：总体统计（学习次数、平均质量、Token 用量）、成长趋势（提问质量上升/下降/稳定）、最近 5 条学习记录详情。

---

## 已知未完成项（诚实清单）

详见 `docs/edu-p2p-e2e-flow.md` §10 与 `docs/edu-p2p-design.md` §12：

- 作业文件分发（blobs）：代码就绪，**无 CLI 入口**
- 恢复授权：`restore_member` 存在，**无 CLI 入口**
- 教师端提问**上屏**：待 S2.3（现仅日志可见）
- 学生端多前端（TUI / 微信 / 企微 / TG）：三驱动收敛进行中（S2.1 ✅ / S2.2 ⏳ / S2.3–2.5 ⬜）
- `teacher_id = 1` 硬编码 **12 处**（多老师共库会串数据，待 S2.4）
- `edu_section_members` 无 `teacher_no`：教师身份以 `name` 对齐，重名会歧义
- 遗留模块 `src/edu/p2p.rs`（761 行，含 `ClassroomMessage::ModeSwitch`）：**现行架构已不用**，仅 `mod p2p;` 声明，待清理
