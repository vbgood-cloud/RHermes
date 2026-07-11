# 学生→教师 连接方案（详细）

> **分支**: `edu` | **基于**: Phase 1-9 + iroh P2P (Phase 8)

## 连接架构

```
教师端（启动后）
├── iroh Endpoint（P2P 节点，自动 NAT 穿透）
│   └── NodeID → 课程码（6位短码）
├── axum Web 服务器 :8080（HTTP 降级备选）
└── SQLite edu.db（学生/课程/班级数据）

学生端（连接流程）
├── 输入课程码 → iroh DNS 查找 → 连接教师 Endpoint
│   ├── 成功 → iroh QUIC 加密通道
│   │   ├── 认证（学号+密码）
│   │   ├── 接收课程列表
│   │   ├── 接收已发布的课次/作业
│   │   └── 定期上报活动数据
│   └── 失败 → HTTP 降级（教师 IP:8080）
│       ├── POST /api/auth
│       ├── GET /api/courses
│       └── GET /api/published
└── 本地缓存（离线可用）
    ├── config.toml: teacher_node_id, auth_token
    └── SQLite: 课程/课次/作业缓存
```

## 连接流程

### 1. 教师端启动

```
rhermes（教师模式）
  ↓
iroh Endpoint 自动创建 → NodeID 生成
axum Web 服务器启动 :8080
课程码 = encode_course_code(NodeID)  // 如 "A1B2C3"
  ↓
控制台显示：
  📡 课程码: A1B2C3
  🌐 Web 地址: http://192.168.1.100:8080
  将课程码告诉学生，学生输入即可连接
```

### 2. 学生端首次连接

```
学生启动 rhermes（学生模式）
  ↓
检测 config.toml 中 teacher_node_id 为空
  ↓
提示："输入教师的课程码: "
  ↓
decode → iroh DNS 查找教师 Endpoint
  ↓
iroh QUIC 连接教师
  ↓
发送 AuthRequest { student_no, password }
  ↓
教师验证 → 返回 AuthResponse {
    success: true,
    token: "abc...",
    student_name: "张三",
    courses: [CourseBrief { code: "CS101", name: "Python" }]
}
  ↓
保存到 config.toml: teacher_node_id, auth_token, student_no
保存到本地缓存: 课程列表
```

### 3. 学生端后续启动

```
rhermes（学生模式）
  ↓
读 config.toml: teacher_node_id + auth_token
  ↓
iroh 连接教师 → 验证 token
  ├── 成功 → 拉取最新课程/课次/作业
  └── 失败（教师离线）→ 使用本地缓存离线模式
  ↓
开始学习
```

### 4. 数据同步

| 方向 | 数据 | 时机 |
|------|------|------|
| 教师→学生 | 课次/作业发布通知 | 实时（iroh 推送） |
| 教师→学生 | 课程内容更新 | 实时（iroh 推送） |
| 教师→学生 | 模式切换指令 | 实时（iroh 推送） |
| 学生→教师 | 活动报告（工具调用、提问） | 每 30 秒心跳 |
| 学生→教师 | 作业提交 | 提交时立即 |
| 学生→教师 | 反思记录 | 每次对话后 |

### 5. HTTP 降级

当 iroh P2P 连接失败时（如校园网封锁 UDP）：

```
学生输入课程码 → iroh 连接失败
  ↓
提示："P2P 连接失败，尝试 HTTP 连接"
  ↓
学生输入教师 IP 地址（如 192.168.1.100:8080）
  ↓
POST http://192.168.1.100:8080/api/auth
  Body: { student_no: "2024001", password: "xxx" }
  → { token: "abc...", courses: [...] }
  ↓
后续通信走 HTTP（轮询代替推送）
```

### 6. 离线模式

```
学生连不上教师
  ↓
使用本地缓存中的课程/课次数据
  ↓
学习活动记录在本地 SQLite
  ↓
上线后自动同步到教师
```

## iroh 课程码机制

### 编码方案

```
教师 NodeID: "ki7a3bvc9x2m...（64字符）"
  ↓ encode_course_code()
课程码: "KI7A3BVC9X2M"（前12位大写）
  ↓
学生输入: KI7A3BVC9X2M
  ↓
iroh DNS 查找: ki7a3bvc9x2m.dns.iroh.link
  ↓
获取完整 NodeID + Relay URL
  ↓
iroh 自动连接（NAT 穿透）
```

### 简化方案（如果 DNS 查找不可用）

教师同时在 axum Web 服务器上暴露课程码→NodeID 映射：

```
学生输入课程码
  ↓
HTTP GET http://<教师IP>:8080/api/resolve/A1B2C3
  → { node_id: "ki7a3bvc9x2m...", relay_url: "..." }
  ↓
iroh 用完整 NodeID + Relay URL 连接
```

## 安全设计

| 层级 | 机制 |
|------|------|
| 传输层 | iroh QUIC + TLS（端到端加密） |
| 认证层 | 学号 + argon2 密码验证 |
| 会话层 | Token（24h 有效，存 config.toml） |
| 权限层 | 学生只能看到已发布的内容 |
| 数据层 | 学生本地数据与教师数据物理隔离（各 SQLite） |
