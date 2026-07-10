# RHermes Edu 教育版使用指南

## 快速开始

### 教师端

#### 1. 初始化教师身份

```bash
rhermes edu teacher init
```

输入教师姓名和密码。

#### 2. 创建课程

```bash
rhermes edu teacher course create CS101 "Python 编程基础"
rhermes edu teacher course create CS201 "数据结构"
```

#### 3. 创建班级

```bash
rhermes edu teacher class create CS101 计算机2301
rhermes edu teacher class create CS101 计算机2302
```

#### 4. 创建课次

```bash
rhermes edu teacher lesson create CS101 计算机2301 1 "变量与数据类型"
rhermes edu teacher lesson create CS101 计算机2301 2 "条件语句"
rhermes edu teacher lesson create CS101 计算机2301 3 "循环结构"
```

#### 5. 添加学生

单个添加：
```bash
rhermes edu teacher student add 2024001 张三 CS101 计算机2301
```

批量导入（CSV 格式：`学号,姓名,密码`，密码可省略，默认 `123456`）：
```bash
rhermes edu teacher student import students.csv CS101 计算机2301
```

CSV 文件示例：
```csv
2024001,张三,pass123
2024002,李四,mypass
2024003,王五
```

#### 6. 查看花名册

```bash
rhermes edu teacher list              # 列出所有课程
rhermes edu teacher list CS101        # 查看课程花名册
```

#### 7. 启动教师仪表板

```bash
rhermes edu teacher dashboard
```

浏览器打开 `http://localhost:8080` 查看学生学习数据。

---

### 学生端

#### 1. 配置 AI Provider

首次使用需要配置 API Key（学生自带）：
```bash
rhermes init
```

#### 2. 认证登录

```bash
rhermes edu student
```

输入学号和密码完成认证。

#### 3. 切换课程

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

教师可远程切换学生的模式。

---

## 通道配置

教育版支持所有通道，学生可选择任意通道与 AI 交互。

### QQ 通道

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

### Web 通道

学生通过浏览器访问 `http://localhost:8090` 与 AI 交互。

### Telegram 通道

在 `.env` 中配置：
```
TELEGRAM_BOT_TOKEN=你的token
```

### 微信通道

```bash
rhermes init    # 配置微信通道并扫码登录
```

---

## 反思系统

每次对话结束后，AI 会自动生成针对性的反思提示：

- "你刚才用了 read_file 工具——你为什么选择先读文件而不是直接搜索？"
- "AI 给出的方案中，你觉得哪部分可能是错的？你会怎么验证？"
- "如果不用 AI，你会怎么解决这个问题？第一步做什么？"

反思评分维度（0-100%）：
- 反思深度（30%）
- 提问质量（25%）
- 工具使用合理性（25%）
- AI 透明度理解（20%）

---

## CLI 命令参考

### 教师命令

| 命令 | 说明 |
|------|------|
| `rhermes edu teacher init` | 初始化教师身份 |
| `rhermes edu teacher course create <码> <名>` | 创建课程 |
| `rhermes edu teacher course list` | 列出课程 |
| `rhermes edu teacher class create <码> <班>` | 创建班级 |
| `rhermes edu teacher lesson create <码> <班> <序号> <主题>` | 创建课次 |
| `rhermes edu teacher student add <学号> <姓名> <课程码> <班级>` | 添加学生 |
| `rhermes edu teacher student import <CSV> <课程码> <班级>` | 批量导入 |
| `rhermes edu teacher list [课程码]` | 课程/花名册 |
| `rhermes edu teacher dashboard` | 启动 Web 仪表板 |

### 学生命令

| 命令 | 说明 |
|------|------|
| `rhermes edu student` | 启动学生模式（认证 + 交互） |
| `rhermes edu auth login <学号> <密码>` | 命令行认证 |
| `rhermes edu auth verify <token>` | 验证 token |
| `rhermes edu join <课程码>` | 加入课程 |
| `rhermes edu status` | 查看学习状态 |
| `/sw` | 列出/切换课程 |

---

## 数据存储

所有教育数据存储在 `home/edu.db`（SQLite），包括：
- 教师信息
- 课程/班级/课次
- 学生信息（密码使用 argon2 哈希）
- 选课记录
- 学习日志
- 认证会话

### 成长报告

学生的成长报告（Markdown 格式）包含：
- 总体统计（学习次数、平均质量、Token 用量）
- 成长趋势（提问质量上升/下降/稳定）
- 最近 5 条学习记录详情
