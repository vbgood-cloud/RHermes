# 课程模板与「头」独立修改

> 让同一门课程导入到不同的教学组（头）后，每个头可以独立修改课程参数，互不影响。

## 背景概念

### 什么是「头」

**「头」**= 一起上同一门课的一个群体（教学组/教学班）。

一个「头」可能跨多个行政班，也可能只是某个班的部分人。总之，**一起上这一门课的人组在一起就是一个头**。

举例：人工智能概论这门课，信工一班和信工二班一起上 → 这是一个头。同一门课的另一个时段是信工三班单独上 → 另一个头。

### 为什么需要这个功能

改造前，课程参数（工具白名单、描述、允许模式）存在 `Course` 表，**全局共享**。修改课程的工具白名单，所有班级立刻受影响——没法做到「头 A 改了不影响头 B」。

改造后，课程作为**模板**，每个头导入后获得独立的参数副本。

---

## 数据模型

### 新增表：`edu_class_course_overrides`

| 字段 | 类型 | 说明 |
|------|------|------|
| `class_id` | INTEGER | 头（班级）ID，关联 `edu_classes` |
| `course_id` | INTEGER | 课程 ID，关联 `edu_courses` |
| `tools_whitelist_override` | TEXT | 工具白名单覆盖。`NULL` = 继承模板，否则该头独立 |
| `description_override` | TEXT | 课程描述覆盖。`NULL` = 继承模板 |
| `allowed_modes_override` | TEXT | 允许学习模式覆盖。`NULL` = 继承模板 |
| `imported_at` | TEXT | 课程导入到该头的时间 |

主键：`(class_id, course_id)`

### 「模板 + 覆盖」机制

采用**覆盖而非全量复制**：

```
课程模板（edu_courses）          头A的覆盖                    头B的覆盖
┌─────────────────────┐       ┌──────────────────┐         ┌──────────────────┐
│ tools_whitelist     │       │ tools = [...覆盖] │         │ tools = NULL     │ → 用模板
│ description         │       │ desc = NULL       │ → 用模板 │ desc = "专属描述" │
│ allowed_modes       │       │ modes = NULL      │ → 用模板 │ modes = NULL     │ → 用模板
└─────────────────────┘       └──────────────────┘         └──────────────────┘
```

- 某字段的覆盖值为 `NULL` → 该头继承模板值（模板更新时该头自动跟进）
- 某字段的覆盖值为非 `NULL` → 该头用独立值（不受模板和其他头影响）

这样做的好处：教师改了课程模板，没改过的头自动跟进；只有显式修改过的头保持自己的版本。

---

## 命令一览

> `/import` 和 `/set` 是**教师端**斜杠命令，在 TUI 模式和 Gateway 模式下均可使用。
>
> - **TUI 模式**：直接在输入框输入斜杠命令（`config.toml` 中 `[edu] role = "teacher"`）
> - **Gateway 模式**：通过各通道（Telegram/QQ/Web 等）发送斜杠命令

| 命令 | 作用 |
|------|------|
| `/import <课程码> <班级名>` | 将课程导入到该头，之后可独立修改 |
| `/set <课程码> <班级名> <字段> <值>` | 修改该头的课程参数 |

`/set` 的 `<字段>` 可选值：

| 字段名（简写） | 全名 | 值格式 |
|------|------|------|
| `tools` | tools_whitelist | JSON 数组，如 `["read_file","glob"]` |
| `desc` | description | 任意文本（可含空格） |
| `modes` | allowed_modes | JSON 数组，如 `["explore","scaffold"]` |

---

## 完整使用流程

### 场景

人工智能概论（AI101）有两个头：
- **信工一班+信工二班**合上（头名「AI合班」）
- **信工三班**单独上（头名「信工三班」）

要求：两个头的工具白名单、描述、模式各自独立，互不影响。

### 第一步：创建课程模板

课程作为模板，参数是各头的默认值。

```
/course create AI101 人工智能概论
```

> 此时 `tools_whitelist` 默认为 `"[]"`（空白名单 = 全部工具允许），`description` 为空，`allowed_modes` 为 `["explore","scaffold"]`。

### 第二步：创建头（班级）

```
/class create AI101 AI合班
/class create AI101 信工三班
```

每个头就是一个班级记录（`edu_classes` 表）。

### 第三步：导入课程到头

```
/import AI101 AI合班
/import AI101 信工三班
```

输出示例：

```
✅ 课程 AI101 已导入到「AI合班」
该头现在可以独立修改课程参数，不影响其他头。
修改: /set AI101 AI合班 <tools|desc|modes> <值>
```

> 导入时所有覆盖字段初始化为 `NULL`（完全继承模板）。此时两个头的参数与模板一致。

### 第四步：按头独立修改

**「AI合班」**给学生更多工具：

```
/set AI101 AI合班 tools ["read_file","write_file","run_command","glob"]
```

**「信工三班」**是考试班，只允许只读工具，并修改描述：

```
/set AI101 信工三班 tools ["read_file","glob"]
/set AI101 信工三班 desc 信工三班专属：考试模式，工具受限
```

### 验证隔离效果

修改完之后：
- ✅「AI合班」的学生 → 可用 `write_file`、`run_command`
- ✅「信工三班」的学生 → 只有 `read_file`、`glob`
- ✅ 课程模板本身 → 未被修改，仍是默认值

两个头互不影响，模板也保持干净。

### 第五步：添加学生到头

```
/student add 2024001 张三 AI101 AI合班
/student add 2024002 李四 AI101 信工三班
```

学生通过 `/auth login` 认证后，系统自动记录其所在头（`primary_class_id`）。

### 第六步：学生切换课程（自动应用头的参数）

学生端：

```
/auth login 2024001 <密码>
/sw AI101
```

系统自动读取「课程模板 + 该学生所在头的覆盖」合并值，构建 `CourseProfile`：
- 张三（在「AI合班」）→ 工具白名单含 `write_file`
- 李四（在「信工三班」）→ 工具白名单只有 `read_file`

学生无需关心自己在哪个头，系统自动处理。

---

## 修改规则详解

### `/set` 只影响指定的头

```
/set AI101 AI合班 tools ["read_file","glob","write_file"]
```

执行后：
- ✅「AI合班」的工具白名单变为指定的值
- ❌「信工三班」不受影响
- ❌ 课程模板不受影响

### 未覆盖的字段自动继承模板

如果只改了 `tools`，没改 `desc` 和 `modes`：

```
/set AI101 AI合班 tools ["read_file"]
```

那么「AI合班」的 `desc` 和 `modes` 仍跟随模板。

> **注意**：当前版本没有 `/course update` 命令来修改课程模板本身。模板参数（`tools_whitelist` / `description` / `allowed_modes`）在创建课程时设定（默认工具全部允许、描述为空、模式为 explore+scaffold）。如果需要修改模板，可以直接编辑数据库 `edu_courses` 表。
>
> 覆盖机制保证：一旦未来模板被修改（无论通过何种方式），未覆盖的字段会自动跟进，已覆盖的字段保持独立。

### 重复导入不会覆盖已有修改

对一个头重复 `/import`：

```
/import AI101 AI合班    # 第一次
/set AI101 AI合班 tools [...]   # 修改
/import AI101 AI合班    # 第二次（不会清除之前的修改）
```

第二次 `/import` 使用 `INSERT OR IGNORE`，如果该头已导入过课程，直接跳过，**不会清除已有的覆盖修改**。

### 未导入就修改会报错

```
/set AI101 新头 tools ["read_file"]
```

如果「新头」没有先 `/import`，会报错：

```
❌ 未找到: 头(class_id=5)未导入课程(course_id=1)，请先 /import
```

必须先 `/import`，再 `/set`。

---

## 学生端行为

学生端无需任何额外操作。系统通过以下流程自动应用头的参数：

1. 学生 `/auth login <学号> <密码>` → 系统记录 `student_id` + `primary_class_id`（所在头）
2. 学生 `/sw <课程码>` 切换课程 → 系统用 `resolve_course_for_class(class_id, course)` 合并模板 + 头覆盖
3. 学生 `/mode <模式>` 切换模式 → 同样读取合并后的课程参数

**如果学生未认证或没有 `primary_class_id`** → 降级为使用课程模板默认值，不影响使用。

---

## 实现细节

### Store 层（`src/edu/store.rs`）

| 方法 | 说明 |
|------|------|
| `import_course_to_class(class_id, course_id)` | 导入课程到头，创建覆盖记录（全 NULL） |
| `get_class_course_override(class_id, course_id)` | 读取覆盖记录 |
| `update_class_course_override(class_id, course_id, tools, desc, modes)` | 更新覆盖字段（`None` 的字段不更新） |
| `resolve_course_for_class(class_id, &course)` | 合并模板 + 覆盖，返回生效的 `Course` |

### TeacherManager 层（`src/edu/teacher.rs`）

封装了通过课程码 + 班级名查找 ID 的逻辑：

| 方法 | 说明 |
|------|------|
| `import_course_to_class(course_code, class_name)` | 按名查找 class_id 后调 store |
| `update_class_course_override(course_code, class_name, field, value)` | field 支持 `tools`/`desc`/`modes` 简写 |
| `resolve_course_for_class(course_code, class_name)` | 合并并返回生效参数 |

### Router 层（`src/agent/router.rs`）

- `/import`、`/set` 命令在 `handle_teacher_slash` 中处理
- `/sw` Switch 分支和 `/mode` 分支在构建 `CourseProfile` 前调用 `resolve_course_for_class`

### 错误处理（`EduError`）

新增 `Db(String)` 变体，用于内部数据库逻辑错误。

---

## 常见问题

### Q: 一个头能导入多门课吗？

当前设计中，一个「头」（班级）在创建时就绑定了一门课程（`edu_classes.course_id`）。`/import` 只能把**该头所属的那门课**导入。这符合「一起上这一门课的人组在一起称为一个头」的定义。

如果同一批学生要上另一门课，应创建另一个头（班级）。

### Q: 课程模板修改后，已覆盖的头会自动更新吗？

- **被覆盖的字段**：不会自动更新。该头保持自己的值。
- **未被覆盖的字段**（覆盖为 `NULL`）：会自动跟随模板更新。

### Q: 能修改课次（topic）吗？

课次（`edu_lessons` 表）本身就按 `(course_id, class_id, lesson_num)` 唯一约束——也就是说，课次主题**已经按头独立**了。教师用 `/lesson create <课程码> <班级> <序号> <主题>` 为每个头单独创建课次，天然互不影响。

`/set` 命令处理的是课程级参数（工具白名单、描述、模式），课次不在此范围内。

### Q: 课件文档（PPT/代码/讲义）能按头独立吗？

当前版本只支持**参数层**的独立修改（工具白名单、描述、允许模式）。课件文档内容的独立存储是后续扩展功能，数据模型已预留扩展空间。
