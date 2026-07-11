# 教学任务安排与课程同步 — 详细实现方案

> **分支**: `edu` | **基于**: Phase 1-9 已完成的基础

## 1. 需求总结

### 核心概念

| 概念 | 说明 |
|------|------|
| **课程模板** | 教师创建的课程内容（课次+作业），是共享资源，不属于单个班级 |
| **班级发布列表** | 每个班级"订阅"一门课程后，教师控制哪些课次/作业对学生可见 |
| **制作模式** | 教师编辑课程内容（创建/修改课次、编写作业），不影响学生可见性 |
| **上课模式** | 教师进入某班级，控制发布节奏，查看学生进度，实时互动 |

### 示例场景

```
课程 CS101 "Python 编程基础"（共享模板）
├── 第1讲 "变量与数据类型"
├── 第2讲 "条件语句"
├── 第3讲 "循环结构"
├── 作业1 "计算器项目"
└── 作业2 "数据分析"

班级 计算机2301 的发布状态：
  ✅ 已发布: 第1讲, 第2讲
  ⬜ 待发布: 第3讲, 作业1, 作业2

班级 计算机2302 的发布状态：
  ✅ 已发布: 第1讲
  ⬜ 待发布: 第2-3讲, 作业1-2
```

---

## 2. 数据模型变更

### 新增/修改的表

```sql
-- 课程模板表（修改：去掉 class_id，课次是课程级共享资源）
-- edu_lessons 表改为课程级别（不再绑定 class_id）

-- 作业表（新增）
CREATE TABLE IF NOT EXISTS edu_assignments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    course_id INTEGER NOT NULL REFERENCES edu_courses(id),
    title TEXT NOT NULL,               -- 作业标题
    description TEXT DEFAULT '',       -- 作业描述/要求
    lesson_num INTEGER,                -- 关联课次（可选）
    due_date TEXT DEFAULT '',          -- 截止时间
    allowed_mode TEXT DEFAULT 'explore', -- 作业期间允许的学习模式
    max_attempts INTEGER DEFAULT 3,    -- 最大提交次数
    created_at TEXT NOT NULL
);

-- 班级发布状态表（新增）
-- 控制"课程模板中的哪些内容对某个班级可见"
CREATE TABLE IF NOT EXISTS edu_class_publish (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    class_id INTEGER NOT NULL REFERENCES edu_classes(id),
    course_id INTEGER NOT NULL REFERENCES edu_courses(id),
    content_type TEXT NOT NULL,        -- "lesson" 或 "assignment"
    content_id INTEGER NOT NULL,       -- lesson.id 或 assignment.id
    published_at TEXT NOT NULL,        -- 发布时间
    published_by TEXT DEFAULT '',      -- 发布方式: "auto" / "manual"
    UNIQUE(class_id, content_type, content_id)
);

-- 作业提交表（新增）
CREATE TABLE IF NOT EXISTS edu_submissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    assignment_id INTEGER NOT NULL REFERENCES edu_assignments(id),
    student_id INTEGER NOT NULL REFERENCES edu_students(id),
    content TEXT DEFAULT '',           -- 提交内容
    file_path TEXT DEFAULT '',         -- 提交文件路径
    submitted_at TEXT NOT NULL,
    -- 评估结果
    ai_score REAL DEFAULT 0,           -- AI 评分 0-1
    ai_feedback TEXT DEFAULT '',       -- AI 反馈
    teacher_score REAL DEFAULT -1,     -- 教师评分（-1=未评）
    teacher_feedback TEXT DEFAULT '',  -- 教师反馈
    evaluated_at TEXT DEFAULT '',
    status TEXT DEFAULT 'submitted',   -- submitted / ai_evaluated / teacher_evaluated
    UNIQUE(assignment_id, student_id)  -- 每个作业每个学生一条记录（覆盖提交）
);
```

### 数据模型变更说明

| 变更 | 说明 |
|------|------|
| `edu_lessons` 去掉 `class_id` | 课次变为课程级共享资源 |
| 新增 `edu_assignments` | 作业挂在课程下 |
| 新增 `edu_class_publish` | 班级发布列表（控制可见性） |
| 新增 `edu_submissions` | 学生作业提交 + AI/教师评估 |

---

## 3. 教师两个模式

### 制作模式 (`/mode craft`)

| 命令 | 说明 |
|------|------|
| `/course enter CS101` | 进入课程 CS101 的制作模式 |
| `/lesson create <序号> <主题>` | 创建课次（共享模板） |
| `/lesson edit <序号> <新主题>` | 修改课次主题 → 所有班级自动同步 |
| `/lesson delete <序号>` | 删除课次 |
| `/assignment create <标题>` | 创建作业 |
| `/assignment edit <id> <描述>` | 修改作业描述 → 所有班级自动同步 |
| `/assignment set due <id> <日期>` | 设置截止日期 |
| `/course exit` | 退出制作模式 |

### 上课模式 (`/mode teach`)

| 命令 | 说明 |
|------|------|
| `/class enter CS101 计算机2301` | 进入某班级的上课模式 |
| `/class publish lesson <序号>` | 发布指定课次到当前班级 |
| `/class publish assignment <id>` | 发布指定作业到当前班级 |
| `/class publish all` | 发布所有未发布的内容 |
| `/class publish upto <序号>` | 发布到指定课次为止（批量） |
| `/class unpublish <类型> <id>` | 撤回已发布的内容 |
| `/class status` | 查看当前班级的发布状态 + 学生进度 |
| `/class roster` | 查看学生列表 + 作业完成情况 |
| `/class exit` | 退出上课模式 |

---

## 4. 学生端命令

| 命令 | 说明 |
|------|------|
| `/courses` | 列出已加入的课程 |
| `/sw CS101` | 切换到课程 CS101 |
| `/sw CS101#2` | 切换到第2讲（仅已发布的课次） |
| `/assignments` | 列出已发布的作业 |
| `/submit <作业id> <内容>` | 提交作业 |
| `/submit <作业id> --file <路径>` | 提交文件作业 |
| `/feedback <作业id>` | 查看作业反馈 |

### 课次可见性

- 学生 `/sw` 切换课次时，只能看到**已发布**的课次
- 未发布的课次对学生不可见
- 作业只显示**已发布**的

---

## 5. 实时同步机制

### 修改课次/作业 → 自动同步

```
教师修改课次主题
    ↓
edu_lessons 表更新（只有一份，没有班级副本）
    ↓
所有班级自动看到新内容（因为引用的是同一行）
    ↓
不需要额外操作
```

**原理**：课次和作业是课程级数据，不存在班级级副本。班级通过 `edu_class_publish` 表控制可见性。修改源数据 → 所有引用方自动看到最新内容。

---

## 6. 实现步骤

### Phase A: 数据模型变更 + EduStore 扩展

- 修改 `edu_lessons` 表（去掉 class_id 约束）
- 新增 `edu_assignments`、`edu_class_publish`、`edu_submissions` 表
- EduStore 新增 CRUD 方法：
  - 作业：`create_assignment`、`get_assignments`、`update_assignment`
  - 发布：`publish_to_class`、`unpublish_from_class`、`get_published_content`、`is_published`
  - 提交：`submit_assignment`、`get_submission`、`update_submission_score`

**测试**:
- `test_assignment_crud`: 创建/查询/修改作业
- `test_publish_to_class`: 发布课次到班级 → 查询已发布列表
- `test_unpublish`: 撤回 → 不可见
- `test_submission_crud`: 提交作业 → AI 评分 → 教师评分
- `test_lesson_shared`: 修改课次 → 两个班级都看到新内容

### Phase B: 教师制作模式斜杠命令

- `/course enter/exit`: 进入/退出课程制作模式
- `/lesson create/edit/delete`: 课次管理
- `/assignment create/edit/set`: 作业管理
- `/mode craft/teach`: 切换制作/上课模式

**测试**:
- `test_craft_lesson`: 创建→编辑→删除课次
- `test_craft_assignment`: 创建→编辑→设置截止日期
- `test_mode_switch`: craft ↔ teach 切换

### Phase C: 教师上课模式 + 发布管理

- `/class enter/exit`: 进入/退出班级上课模式
- `/class publish`: 发布课次/作业到班级
- `/class status`: 查看发布状态
- `/class roster`: 查看学生作业进度

**测试**:
- `test_publish_lesson`: 发布课次到班级 → 查询确认
- `test_publish_batch`: `publish all` / `publish upto` 批量发布
- `test_class_status`: 发布状态 + 学生进度

### Phase D: 学生端作业 + 课次可见性

- `/assignments`: 列出已发布的作业
- `/submit`: 提交作业
- `/feedback`: 查看反馈
- `/sw`: 只允许切换到已发布的课次

**测试**:
- `test_student_assignments`: 未发布的作业不可见
- `test_submit_and_feedback`: 提交→AI评估→查看反馈
- `test_sw_published_only`: `/sw CS101#99`（未发布）→ 报错

### Phase E: 编译测试部署

- 全量回归测试
- 编译 release
- 部署

---

## 7. 斜杠命令完整清单（更新后）

### 教师命令

| 命令 | 模式 | 说明 |
|------|:----:|------|
| `/course enter <码>` | 任何 | 进入课程制作模式 |
| `/course exit` | 制作 | 退出制作模式 |
| `/course list` | 任何 | 列出课程 |
| `/course create <码> <名>` | 任何 | 创建课程 |
| `/lesson create <序号> <主题>` | 制作 | 创建课次 |
| `/lesson edit <序号> <新主题>` | 制作 | 修改课次 |
| `/lesson delete <序号>` | 制作 | 删除课次 |
| `/assignment create <标题>` | 制作 | 创建作业 |
| `/assignment edit <id> <描述>` | 制作 | 修改作业 |
| `/assignment set due <id> <日期>` | 制作 | 设置截止日期 |
| `/class enter <课程码> <班级>` | 任何 | 进入上课模式 |
| `/class exit` | 上课 | 退出上课模式 |
| `/class publish lesson <序号>` | 上课 | 发布课次 |
| `/class publish assignment <id>` | 上课 | 发布作业 |
| `/class publish all` | 上课 | 发布全部 |
| `/class publish upto <序号>` | 上课 | 批量发布 |
| `/class unpublish <类型> <id>` | 上课 | 撤回发布 |
| `/class status` | 上课 | 发布状态 + 学生进度 |
| `/class roster` | 上课 | 学生列表 |
| `/student add <学号> <名> <课程> <班>` | 任何 | 添加学生 |
| `/roster <课程码>` | 任何 | 查看花名册 |
| `/mode craft` | 任何 | 切换到制作模式 |
| `/mode teach` | 任何 | 切换到上课模式 |

### 学生命令

| 命令 | 说明 |
|------|------|
| `/courses` | 列出已加入课程 |
| `/sw <课程码>[#课次]` | 切换课程/课次（仅已发布） |
| `/assignments` | 列出已发布作业 |
| `/submit <id> <内容>` | 提交作业 |
| `/feedback <id>` | 查看作业反馈 |
| `/auth login <学号> <密码>` | 认证 |
| `/profile` | 学习档案 |
| `/report` | 成长报告 |
| `/mode <模式>` | 切换学习模式 |
