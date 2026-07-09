# RHermes Edu 教育版详细实现规划

> **版本**: v1.0 | **更新**: 2026-07-08 | **分支**: `edu`

## 目录

- [1. 架构总览](#1-架构总览)
- [2. 课程管理数据模型](#2-课程管理数据模型)
- [3. 学习模式系统](#3-学习模式系统)
- [4. 反思系统与 AI 素养培养](#4-反思系统与-ai-素养培养)
- [5. 技术选型](#5-技术选型)
- [6. 实现步骤（含测试）](#6-实现步骤含测试)

---

## 1. 架构总览

```
rhermes（同一二进制）
│
├── 通用模式（默认，零改动）
│   ├── TUI 终端编程
│   ├── Gateway 守护进程
│   ├── 所有通道（微信/企微/Telegram/QQ/Web）
│   └── 全部 22 个工具
│
├── 教育模式（rhermes edu ...）
│   ├── 学生版（rhermes edu student）
│   │   ├── 多课程管理 + /sw 切换课程
│   │   ├── 学习模式系统（探索/引导/考试）
│   │   ├── 反思系统 + AI 素养培养
│   │   ├── 所有通道都可用（无限制）
│   │   ├── 学号+密码认证连接教师服务端（iroh）
│   │   └── Classroom 通道（iroh P2P）
│   │
│   ├── 教师版（rhermes edu teacher）
│   │   ├── 班级管理 + 课程管理（多课次）
│   │   ├── 学生名单管理（学号+密码）
│   │   ├── Web 仪表板（axum + WebSocket 实时推送）
│   │   ├── 学习分析报告
│   │   ├── 远程模式切换
│   │   └── Classroom 通道（iroh 课堂创建者）
│   │
│   └── QQ 通道（所有模式共用，官方 Bot API）
│
└── 共享基础设施（两种模式共用）
    ├── AgentSession + ToolDispatcher + Context
    ├── MemorySystem (SQLite + FTS5)
    ├── ProviderPool + Transport
    └── Config + PathManager
```

### 关键设计原则

| 原则 | 说明 |
|------|------|
| 通用模式零改动 | 不带 `edu` 子命令时完全走现有逻辑，不加载任何 edu 代码 |
| 同一二进制 | 学生版/教师版/通用模式在同一二进制中，通过 CLI 参数区分 |
| 学生自带 API Key | 学生用自己的 DeepSeek/智谱 Key；未来可统一网关路由 |
| 所有通道共享 | QQ/微信/Telegram/Web 通道在两种模式下都可用 |
| 本地 SQLite | 教师端数据存储在本地 SQLite，通过 iroh 供学生认证连接 |

---

## 2. 课程管理数据模型

### 实体关系

```
教师 (1)
├── 课程 A "Python 编程基础"
│   ├── 班级 1 (计算机2301) → 课次 1, 2, 3...
│   └── 班级 2 (计算机2302) → 课次 1, 2, 3...
├── 课程 B "数据结构"
│   └── 班级 3 (软件2301) → 课次 1, 2, 3...
└── 课程 C "AI 素养导论"
    ├── 班级 1 (计算机2301) → 课次 1, 2...
    └── 班级 4 (信管2301) → 课次 1, 2...
```

- 一个教师可以上 **多门不同的课程**
- 一门课程可以给 **多个不同的班级** 上
- 一个班级有 **多个学生**
- 一门课（在某班级中）分 **多次课次** 进行
- 同一个学生可能 **同时上多门课**

### SQLite 表结构

```sql
-- 教师表
CREATE TABLE IF NOT EXISTS edu_teachers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    node_id TEXT,                    -- iroh EndpointId
    password_hash TEXT NOT NULL,     -- bcrypt 哈希
    created_at TEXT NOT NULL
);

-- 课程表（教师创建的课程定义）
CREATE TABLE IF NOT EXISTS edu_courses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    course_code TEXT UNIQUE NOT NULL,  -- 如 "CS101"
    name TEXT NOT NULL,                -- 如 "Python 编程基础"
    teacher_id INTEGER NOT NULL REFERENCES edu_teachers(id),
    description TEXT DEFAULT '',
    tools_whitelist TEXT DEFAULT '[]', -- JSON 数组：允许的工具名
    allowed_modes TEXT DEFAULT '["explore","scaffold"]', -- JSON 数组
    created_at TEXT NOT NULL
);

-- 班级表（一门课可以给多个班级上）
CREATE TABLE IF NOT EXISTS edu_classes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,               -- 如 "计算机2301"
    course_id INTEGER NOT NULL REFERENCES edu_courses(id),
    created_at TEXT NOT NULL
);

-- 课次表（一个班级的一门课分多次课次）
CREATE TABLE IF NOT EXISTS edu_lessons (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    course_id INTEGER NOT NULL REFERENCES edu_courses(id),
    class_id INTEGER NOT NULL REFERENCES edu_classes(id),
    lesson_num INTEGER NOT NULL,      -- 课次序号 1, 2, 3...
    topic TEXT DEFAULT '',            -- 本次课主题
    mode_override TEXT DEFAULT '',    -- 课次级模式覆盖（空=用课程默认）
    created_at TEXT NOT NULL,
    UNIQUE(course_id, class_id, lesson_num)
);

-- 学生表（归属班级，但可跨班选课）
CREATE TABLE IF NOT EXISTS edu_students (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    student_no TEXT UNIQUE NOT NULL,  -- 学号
    name TEXT NOT NULL,
    password_hash TEXT NOT NULL,      -- bcrypt 哈希
    primary_class_id INTEGER REFERENCES edu_classes(id), -- 主班
    created_at TEXT NOT NULL
);

-- 选课表（学生可同时上多门课）
CREATE TABLE IF NOT EXISTS edu_enrollments (
    student_id INTEGER NOT NULL REFERENCES edu_students(id),
    course_id INTEGER NOT NULL REFERENCES edu_courses(id),
    enrolled_at TEXT NOT NULL,
    PRIMARY KEY(student_id, course_id)
);

-- 学习日志（按课次记录）
CREATE TABLE IF NOT EXISTS edu_learning_journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    student_id INTEGER NOT NULL REFERENCES edu_students(id),
    course_id INTEGER NOT NULL REFERENCES edu_courses(id),
    lesson_num INTEGER NOT NULL,
    topic TEXT DEFAULT '',            -- 对话主题
    tool_calls TEXT DEFAULT '[]',     -- JSON：工具调用记录
    reflection TEXT DEFAULT '',       -- 学生反思回答
    quality_score REAL DEFAULT 0,     -- 问题质量评分 0-1
    reflection_depth REAL DEFAULT 0,  -- 反思深度评分 0-1
    token_usage INTEGER DEFAULT 0,
    duration_secs INTEGER DEFAULT 0,
    created_at TEXT NOT NULL
);

-- 认证会话表（学生登录后的会话）
CREATE TABLE IF NOT EXISTS edu_sessions (
    token TEXT PRIMARY KEY,           -- 随机 token
    student_id INTEGER NOT NULL REFERENCES edu_students(id),
    current_course_id INTEGER,        -- 当前激活的课程
    current_lesson_num INTEGER,       -- 当前课次
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL
);
```

---

## 3. 学习模式系统

### 三种模式

| 模式 | System Prompt 风格 | 工具限制 | 适用场景 |
|------|-------------------|---------|---------|
| `explore` 探索模式 | 标准 AI 助手 | 全部允许（按课程白名单） | 自由探索、项目实践 |
| `scaffold` 引导模式 | 苏格拉底式追问，不直接给答案 | 全部允许 | 课堂练习、作业辅导 |
| `locked` 考试模式 | AI 禁用或仅提供查阅 | 只读工具（read_file/search/glob/get_time） | 考试、测验 |

### scaffold 模式 System Prompt

```
你是一位苏格拉底式教学助手。你的目标不是给学生答案，而是通过追问引导他们自己思考。

严格规则：
1. 永远不要直接给出完整的解决方案
2. 先问学生"你觉得应该从哪里开始？"
3. 如果学生卡住，给出方向性提示而非答案
4. 鼓励学生解释他们的思考过程
5. 在关键处追问"你确定吗？你验证过吗？"
6. 如果学生坚持要答案，引导他们分解问题
```

### /sw 命令

```
/sw              → 列出已加入的课程 + 当前课程
/sw CS101        → 切换到 CS101 课程
/sw CS101#3      → 切换到 CS101 的第 3 次课
```

切换后 session key 变为 `channel:chat_id:course_id#lesson_num`，获得独立的 AgentSession。

---

## 4. 反思系统与 AI 素养培养

### 反思流程

```
学生与 AI 对话
    ↓
on_done 触发
    ↓
AI 根据对话内容生成针对性反思问题（非模板）
    ↓
学生回答反思
    ↓
AI 评估反思深度（0-1 评分）
    ↓
写入 edu_learning_journal
```

### 反思问题示例（AI 生成）

- "你刚才用了 read_file 工具——你为什么选择先读文件而不是直接搜索？"
- "AI 给出的方案中，你觉得哪部分可能是错的？你会怎么验证？"
- "如果不用 AI，你会怎么解决这个问题？第一步做什么？"

### AI 素养评估维度

| 维度 | 评估方式 | 分值 |
|------|---------|------|
| 提问清晰度 | 学生提问是否清晰、具体 | 0-1 |
| 思维深度 | 是否有分析性、批判性思考 | 0-1 |
| 工具使用合理性 | 是否选择了正确的工具和参数 | 0-1 |
| 反思深度 | 反思回答的质量 | 0-1 |
| AI 透明度理解 | 是否理解了 AI 的思考过程和局限 | 0-1 |

---

## 5. 技术选型

| 组件 | 选型 | 版本 | 理由 |
|------|------|------|------|
| P2P 网络 | `iroh` | 1.0+ | 纯 Rust、内置 NAT 穿透、无管理员权限 |
| P2P 广播 | `iroh-gossip` | 0.101+ | 课堂消息分发 |
| Web 服务器 | `axum` | 0.8 | 轻量、tokio 原生、WebSocket |
| Web 静态资源 | `include_str!` / `rust-embed` | - | 编译进二进制 |
| QQ 通道 | 官方 Bot API + `qq-bot-rs` | 0.1+ | 合规模式 |
| 密码哈希 | `argon2` | 0.5+ | 纯 Rust、现代安全 |
| 数据库 | `rusqlite` (bundled) | 0.32+ | 已有依赖 |

---

## 6. 实现步骤（含测试）

### Phase 1: 创建 edu 分支 + 双模式框架 + 配置结构

**目标**: 建立 edu 模块骨架，通用模式零改动。

**步骤**:

- `git checkout -b edu` 创建分支
- `main.rs` 加 `Edu` 子命令到 `Commands` 枚举：`Edu { command: EduCommand }`
  - `EduCommand::Student` / `EduCommand::Teacher` / `EduCommand::Join { code: String }`
- `core/config.rs` 新增 `EduConfig` 结构体
  ```rust
  pub struct EduConfig {
      pub role: String,           // "student" / "teacher" / ""
      pub teacher_node_id: String,// 学生连接教师用
      pub student_no: String,     // 学号
      pub default_mode: String,   // "explore"
  }
  ```
- `Config` 加 `edu: EduConfig` 字段（`#[serde(default)]`）
- `core/mod.rs` 导出 `EduConfig`
- 新建 `src/edu/mod.rs`（模块入口）
- `main.rs` 加 `mod edu;`
- 不带 `edu` 子命令时完全走现有逻辑

**测试**:

- `test_edu_config_default`: `EduConfig::default()` 所有字段为空/默认
- `test_edu_config_save_load`: 带 `[edu]` section 的 config.toml 写入→读取 roundtrip
- `test_general_mode_unchanged`: 不带 `edu` 时 `EduConfig.role == ""`
- **验证方法**: `cargo test edu_config` + `cargo run` 确认通用模式正常

---

### Phase 2: EduStore SQLite + 密码哈希

**目标**: 建立教育数据存储层。

**步骤**:

- Cargo.toml 加 `argon2 = "0.5"`
- 新建 `src/edu/store.rs`
  - `EduStore` 结构体（持有 `rusqlite::Connection`）
  - `EduStore::open(path)` → 建表（上述 8 张表）
  - `EduError` 枚举（仿 `MemoryError`）
  - 教师 CRUD: `create_teacher`, `get_teacher`, `verify_teacher_password`
  - 课程 CRUD: `create_course`, `get_course`, `list_courses_by_teacher`
  - 班级 CRUD: `create_class`, `get_classes_by_course`
  - 课次 CRUD: `create_lesson`, `get_lessons`
  - 学生 CRUD: `create_student`, `get_student`, `verify_student_password`
  - 选课: `enroll`, `get_student_courses`
  - 日志: `write_journal`, `get_student_journal`
  - 会话: `create_session`, `validate_session`, `expire_session`
- 密码哈希: `hash_password(plain) -> String` + `verify_password(plain, hash) -> bool`

**测试**:

- `setup_edu_store()`: 返回 `(TempDir, EduStore)`（仿 `memory.rs` 模式）
- `test_teacher_crud`: 创建教师 → 查询 → 验证密码
- `test_password_hash_verify`: `hash_password("abc123")` → `verify_password("abc123", hash) == true` → `verify_password("wrong", hash) == false`
- `test_course_crud`: 创建课程 → 查询 → 按教师列出
- `test_class_and_lesson`: 创建班级+课次 → 查询关联
- `test_student_enrollment`: 创建学生 → 选课 → 查询选课列表
- `test_learning_journal`: 写日志 → 查询 → 按课次过滤
- `test_session_create_validate_expire`: 创建→验证→过期
- **验证方法**: `cargo test edu_store`

---

### Phase 3: 学生身份 + 课程管理 + 认证

**目标**: 教师端能管理课程/班级/学生，学生能认证连接。

**步骤**:

- `src/edu/teacher.rs`: 教师端管理逻辑
  - `rhermes edu teacher init`: 初始化教师身份（姓名+密码）
  - `rhermes edu teacher course create <课程码> <课程名>`: 创建课程
  - `rhermes edu teacher class create <课程码> <班级名>`: 创建班级
  - `rhermes edu teacher lesson create <课程码> <班级名> <序号> <主题>`: 创建课次
  - `rhermes edu teacher student add <学号> <姓名> <班级> [--password]`: 添加学生
  - `rhermes edu teacher student import <CSV路径>`: 批量导入学生
  - `rhermes edu teacher list`: 列出课程/班级/学生
- `src/edu/auth.rs`: 认证模块
  - `AuthToken` 结构体（token + student_id + expires_at）
  - `authenticate(store, student_no, password) -> Option<AuthToken>`
  - `generate_token() -> String`（随机 32 字节 hex）
- init 向导支持教育模式：选学生→输入教师地址+学号+密码

**测试**:

- `test_teacher_init`: 初始化教师 → `verify_teacher_password` 通过
- `test_course_create`: 创建课程 → 查询确认
- `test_student_add_and_import`: 单个添加 + CSV 批量导入
- `test_authenticate_success`: 正确学号+密码 → 返回 token
- `test_authenticate_wrong_password`: 错误密码 → None
- `test_authenticate_expired`: 手动设过期时间 → `validate_session` 返回 false
- **验证方法**: `cargo test edu_teacher` + `cargo test edu_auth`

---

### Phase 4: 课程切换 + per-course AgentSession + 学习模式

**目标**: 学生切换课程时获得独立的 AI 会话和工具集。

**步骤**:

- `agent/router.rs`: SessionRouter 加 `course_id` 到 session key
  - `"channel:chat_id"` → `"channel:chat_id:course_id#lesson_num"`
  - 从 `inbound.metadata["course_id"]` 和 `inbound.metadata["lesson_num"]` 读取
- SessionRouter 加 `HashMap<String, CourseProfile>`（course_id → prompt/tools/mode）
- `dispatch()` 时按 course_id 查找 CourseProfile，决定 system_prompt
- `/sw` 命令处理：
  - 无参数：列出已加入课程
  - `/sw CS101`：切换当前课程，更新 metadata
  - `/sw CS101#3`：切换到指定课次
- `CourseProfile` 结构体：
  ```rust
  struct CourseProfile {
      system_prompt: String,
      allowed_tools: Vec<String>,
      mode: LearnMode, // explore / scaffold / locked
  }
  ```
- `scaffold` 模式注入苏格拉底式 System Prompt
- `locked` 模式：`builtin_registry()` 只注册只读工具
- `tools/builtin.rs`: 加 `builtin_registry_filtered(allowed: &[String])` 函数

**测试**:

- `test_session_key_with_course`: 同一用户不同课程 → 不同 session key → 不同 AgentSession
- `test_sw_command_list`: `/sw` 列出课程
- `test_sw_command_switch`: `/sw CS101` 切换后 metadata 包含 course_id
- `test_scaffold_prompt`: scaffold 模式注入的 prompt 包含"苏格拉底"
- `test_locked_tools`: locked 模式注册表只有只读工具
- `test_filtered_registry`: `builtin_registry_filtered(["read_file","glob"])` 只含 2 个工具
- **验证方法**: `cargo test edu_course` + 手动 TUI 测试 `/sw`

---

### Phase 5: 反思系统 + AI 素养评估

**目标**: 每次对话后自动生成反思，评估学生 AI 素养。

**步骤**:

- `src/edu/reflection.rs`: 反思模块
  - `generate_reflection_prompt(conversation_summary: &str, tools_used: &[String]) -> String`
    - 调用 AI 生成针对性反思问题（不是固定模板）
  - `evaluate_reflection(reflection_text: &str, conversation: &str) -> ReflectionScore`
    - AI 评估反思深度，返回 0-1 分值
  - `evaluate_question_quality(question: &str) -> f64`
    - 评估学生提问的清晰度和深度
- `agent/event_sink.rs`: 加 `on_reflection` trait 方法
- `edu/reflection.rs`: 实现 `EduReflectionSink` 包装现有 Sink
  - `on_done` 后触发反思流程
  - 反思回答写入 `edu_learning_journal`
- 学生 TUI 显示：
  - AI 思考链和工具调用过程（已有 EventSink 机制）
  - 反思提示和回答区
  - 成长曲线（从 learning_journal 读取）

**测试**:

- `test_reflection_prompt_generation`: 给定对话摘要 → 生成非空反思提示
- `test_reflection_evaluation`: 有深度的反思 → 高分；敷衍回答 → 低分
- `test_question_quality`: 清晰提问 → 高分；模糊提问 → 低分
- `test_journal_write_with_reflection`: 写日志包含反思回答和评分
- `test_growth_report_export`: 导出 Markdown 成长报告包含数据
- **验证方法**: `cargo test edu_reflection`

---

### Phase 6: QQ 通道（官方 Bot API）

**目标**: 学生通过 QQ 与 AI 交互。

**步骤**:

- Cargo.toml 加 `qq-bot-rs = "0.1"`（或自实现 WebSocket Gateway）
- `core/config.rs` 加 `QqConfig`：
  ```rust
  pub struct QqConfig {
      pub enabled: bool,
      pub app_id: String,        // 从 .env QQ_BOT_APP_ID 读取
      pub app_secret: String,    // 从 .env QQ_BOT_APP_SECRET 读取
      pub allowed_groups: Vec<String>,
      pub allow_private_chat: bool,
  }
  ```
- 新建 `src/channel/qq/mod.rs` 实现 Channel trait
  - 认证：POST `https://bots.qq.com/app/getAppAccessToken` → AccessToken（7200s 自动刷新）
  - 接收：GET `https://api.sgroup.qq.com/gateway` → WSS URL → WebSocket 连接
    - Identify（携带 token + intents）
    - 心跳（按服务端返回的 heartbeat_interval）
    - 事件分发：`GROUP_AT_MESSAGE_CREATE`、`C2C_MESSAGE_CREATE`
    - 自动重连 + Resume
  - 发送：POST `/v2/groups/{openid}/messages`、`/v2/users/{openid}/messages`
  - 群聊 @bot 触发（解析消息文本去除 @bot 前缀）
  - 私聊直接触发
- `ChannelsConfig` 加 `qq: QqConfig`
- `channel/mod.rs` 加 `pub mod qq;`
- init 向导引导配置 QQ Bot

**测试**:

- `test_qq_config_default`: 默认未启用
- `test_qq_config_save_load`: config.toml roundtrip
- `test_qq_token_caching`: AccessToken 缓存 + 过期刷新逻辑（mock 时间）
- `test_qq_message_parse`: 解析群聊 @bot 消息 → 提取纯文本
- `test_qq_at_mention_extract`: "@bot 你好" → "你好"
- `test_qq_channel_status`: `status()` 返回连接状态
- **验证方法**: `cargo test qq_channel` + 沙箱环境端到端测试

---

### Phase 7: Web 通道 + 教师仪表板

**目标**: 学生通过浏览器交互，教师通过 Web 面板监控。

**步骤**:

- Cargo.toml 加 `axum = { version = "0.8", features = ["ws"] }`、`tower-http`
- 新建 `src/channel/web/mod.rs`：Web 通道
  - axum 服务器（端口可配，默认 8090）
  - `GET /`：学生聊天 Web UI（嵌入式 HTML）
  - `POST /api/chat`：学生发消息（创建 InboundMessage）
  - `GET /ws`：流式回复（WebSocket SSE）
  - `GET /api/courses`：学生课程列表
  - `POST /api/sw`：切换课程
- 教师仪表板（`rhermes edu teacher` → Web 服务 `:8080`）：
  - `GET /`：教师仪表板 HTML（`include_str!`）
  - `GET /ws`：WebSocket 实时推送学生活动
  - `GET /api/students`：学生列表 + 学习数据 JSON
  - `GET /api/student/:id/report`：学生评估报告
  - `POST /api/mode/:student_id`：远程切换模式
  - `POST /api/notify`：发送通知
  - 仪表板 UI 功能：
    - 实时活动流
    - 在线学生列表 + 学习状态
    - 班级/课程/学生管理
    - 学习分析图表
    - 异常预警面板
- Web 安全：首次启动生成随机管理 Token，URL 携带 `?token=xxx`

**测试**:

- `test_web_channel_config`: 配置 roundtrip
- `test_web_chat_endpoint`: POST /api/chat → 返回 200
- `test_web_ws_upgrade`: GET /ws → WebSocket 握手成功
- `test_dashboard_student_list`: GET /api/students → JSON 数组
- `test_dashboard_mode_switch`: POST /api/mode/:id → 数据库更新
- `test_dashboard_token_auth`: 无 token → 401；有 token → 200
- **验证方法**: `cargo test web_channel` + 浏览器手动测试

---

### Phase 8: iroh P2P 课堂网络 + Classroom 通道

**目标**: 教师和学生通过 iroh P2P 连接，实时通信。

**步骤**:

- Cargo.toml 加 `iroh = "1.0"`、`iroh-gossip = "0.101"`
- 新建 `src/edu/p2p.rs`：iroh 网络层
  - `EduEndpoint`：封装 iroh `Endpoint`
  - 教师：`create_classroom(course_code) -> TopicId`
    - 创建 iroh Endpoint + gossip topic
    - 输出课程码（= topic ID 编码）供学生加入
  - 学生：`join_classroom(course_code, teacher_node_id) -> ()`
    - 连接教师 Endpoint
    - 加入 gossip topic
  - 消息类型（JSON over QUIC/gossip）：
    - `Heartbeat`: 学生在线心跳
    - `ActivityReport`: 工具调用/问题/反思上报
    - `Notification`: 教师通知
    - `ModeSwitch`: 教师远程切换模式
    - `HelpRequest`: 学生求助
- 新建 `src/channel/classroom/mod.rs` 实现 Channel trait
  - `start()`: 监听 iroh gossip 事件 → 创建 InboundMessage
  - `send_message()`: 通过 iroh 发送到指定学生
- `InboundMessage.metadata` 携带 student_id / course_id / lesson_num
- 离线缓存：断网时本地运行，重连后批量同步日志

**测试**:

- `test_iroh_endpoint_create`: 创建 Endpoint → 获取 NodeId
- `test_classroom_create_join`: 教师创建课堂 → 学生加入 → 收到欢迎消息
- `test_heartbeat`: 学生发心跳 → 教师收到 → 更新在线列表
- `test_activity_report`: 学生上报活动 → 教师仪表板显示
- `test_mode_switch`: 教师发 ModeSwitch → 学生模式变更
- `test_offline_cache`: 断网 → 本地缓存 → 重连后同步
- **验证方法**: `cargo test edu_p2p` + 双端集成测试

---

### Phase 9: 集成测试 + 文档 + 发布

**目标**: 端到端验证，文档完善。

**步骤**:

- 集成测试场景：
  1. 教师初始化 → 创建课程 CS101 → 创建班级 → 添加学生 → 创建课次
  2. 学生认证登录 → 选课 → 切换到 CS101#1
  3. 学生用 AI 对话（explore 模式）→ 工具调用 → 反思
  4. 教师切换学生模式为 scaffold → 学生收到 → AI 变为引导式
  5. 教师查看仪表板 → 看到学生活动 + 学习分析
  6. 导出学生评估报告
- QQ 通道端到端测试（沙箱）
- Web 仪表板端到端测试
- 文档：
  - `docs/edu-guide.md`：教师使用手册
  - `docs/edu-student-guide.md`：学生使用手册
  - `docs/edu-architecture.md`：技术架构文档
- 编译 release + 部署

**测试**:

- `test_e2e_teacher_student_flow`: 完整教学流程
- `test_e2e_qq_channel`: QQ 消息收发
- `test_e2e_web_dashboard`: 仪表板功能
- **验证方法**: 手动完整流程 + `cargo test --release`

---

## 实施路线图

```
Phase 1-2（框架+存储）        ← 基础骨架，所有功能依赖此层
    ↓
Phase 3（认证+管理）          ← 教师能管理，学生能连接
    ↓
Phase 4（课程切换+学习模式）   ← 核心教育价值
    ↓
Phase 5（反思+素养评估）       ← 教育学深度
    ↓
Phase 6（QQ通道）             ← 学生可用通道
    ↓
Phase 7（Web通道+仪表板）      ← 可视化监控
    ↓
Phase 8（iroh P2P）           ← 实时联网
    ↓
Phase 9（集成+文档）           ← 发布
```

### 各 Phase 预估工作量

| Phase | 新增文件 | 新增代码行 | 难度 | 可独立测试 |
|:-----:|:-------:|:---------:|:----:|:---------:|
| 1 | 2 | ~200 | 低 | ✅ |
| 2 | 1 | ~500 | 中 | ✅ |
| 3 | 2 | ~400 | 中 | ✅ |
| 4 | 1 | ~350 | 高 | ✅ |
| 5 | 1 | ~300 | 中 | ✅ |
| 6 | 1 | ~400 | 高 | ✅ |
| 7 | 2 | ~600 | 中 | ✅ |
| 8 | 2 | ~500 | 高 | ✅ |
| 9 | 0 | ~200 | 低 | ✅ |
| **合计** | **12** | **~3450** | - | - |

---

## 补充说明

### 需要注意的风险

| 风险 | 说明 | 缓解措施 |
|------|------|---------|
| QQ Bot 审核 | 官方 API 需要在 `q.qq.com` 注册审核 | 先用沙箱开发，审核期间用 Web/Telegram 通道替代 |
| iroh Windows 兼容 | iroh CI 以 Linux 为主 | 提前在 Windows 上验证基本连接 |
| 学生 API Key 成本 | DeepSeek 按量计费 | 引导学生使用 deepseek-v4-flash（最便宜）；未来统一网关后可设额度 |
| 二进制体积 | iroh + axum + qq-bot-rs 增加体积 | 可考虑 feature flag 控制 edu 功能编译 |
| SQLite 并发 | 教师端多学生同时写入 | 使用 WAL 模式（已在 MemorySystem 中实践） |

### 未来扩展（V2）

- 统一 API 网关（One API / New API 路由，学生无需自带 Key）
- 作业自动批改（AI 评估代码作业 + 测试用例）
- 课堂实时投票/问答
- 学习路径推荐（基于学习日志的个性化推荐）
- 多教师协作（多个教师共享一个教学平台）
