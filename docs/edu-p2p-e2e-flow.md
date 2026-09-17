# 教师端 × 学生端 —— 端到端测试全流程

> 适用版本：**v0.7.15**（edu P1–P4 + 多老师拓扑 + 配置驱动 + 三驱动收敛 S2.1/S2.2）
> 定位：本页是**操作手册**（照着敲 → 对照期望输出 → 判定通过/失败）。
> 分层原理、日志对照表、历史缺陷附录见 `docs/edu-p2p-testing.md`；设计见 `docs/edu-p2p-design.md`。

---

## 0. 这份流程在测什么

一条完整的「老师开班 → 学生进班 → 班内通信 → 出问题（撤销）→ 收尾」主线，
**教师端与学生端各跑一个真实进程**，只用 CLI，不碰 Rust API。

| 段 | 覆盖 | 用例数 |
|---|---|---|
| **A 教师端** | 初始化 → 建课 → 建班 → 加学生 → 查名册 → 起托管 → 交互台命令 | T-01 ~ T-09 |
| **B 学生端** | 写配置 → 进课堂 → 认证接入 → 交互台命令 → 退出 | S-01 ~ S-06 |
| **C 双向联调** | 老师广播 → 学生收到；学生提问 → **同学**收到；白名单/纪元 | I-01 ~ I-05 |
| **D 撤销与重连** | 撤销 → 轮换 → 其余学生自动跟随 → 被撤销者再也进不来 | R-01 ~ R-05 |
| **E 多老师多班** | 一位学生同时进两位老师的班，互不串台 | M-01 ~ M-03 |
| **F 作业文件分发** | ⛔ **尚未接线到 CLI**（见 §7） | F-01 |

**自动化的等价物**：A/B/C/D 段的核心链路在 `tests/edu_p2p_e2e.rs` 里已由 4 个用例
（`edu_p2p_revocation_end_to_end` / `edu_p2p_multi_teacher_end_to_end` /
`edu_p2p_config_driven_multi_teacher` / `edu_p2p_section_host_drives_notices_and_send`）
以代码断言覆盖。本页的价值是**手工可复现**：确认「编译出来的 exe 在便携目录里真能用」。

> v0.7.13 的三个缺陷（父目录不建、子命令被顶掉、参数按位错读）**全部**是这一层才暴露的 —— 单测与 e2e 都测不到。

---

## 1. 准备（一次就好）

### 1.1 编译与目录

```bash
cd /e/lab/RHermes
export ROOT=/e/lab/RHermes
export RH_SKIP_WINRESOURCE=1

RH_SKIP_WINRESOURCE=1 cargo build --bins
# 期望末行：Finished `dev` profile [unoptimized + debuginfo] target(s)

# ⚠️ 无 --config 参数：PathManager::detect() 以「可执行文件所在目录」为锚。
#    所以 config.toml 必须与 exe 同级，数据落在同级的 home/ 下。
#    模拟多台机器 = 建多个便携目录。
export E2E=$ROOT/target/e2e        # 在 target/ 下，天然不进版本库
rm -rf $E2E && mkdir -p $E2E/teacher-a $E2E/teacher-b $E2E/stu-a $E2E/stu-b
for d in teacher-a teacher-b stu-a stu-b; do
  cp $ROOT/target/debug/rhermes-teacher.exe $E2E/$d/
  cp $ROOT/target/debug/rhermes-stu.exe     $E2E/$d/
done
```

### 1.2 命令面速查（**先看这张表，命名有两个坑**）

| 端 | 入口 | 说明 |
|---|---|---|
| 教师 | `rhermes-teacher <子命令>` | 专用二进制，等价于 `rhermes edu teacher <子命令>` |
| 教师 | `rhermes-teacher init-teacher <姓名> <密码>` | ⚠️ **坑 1**：不能写 `init` —— `init` 被通用配置向导（API Key/模型）占用，clap 会静默改名 |
| 教师 | `rhermes-teacher course/class/lesson/student/roster` | 教学管理 |
| 教师 | `rhermes-teacher serve [--all｜--offline｜<课程码> <班级>...]` | 长期在线托管（**唯一能让学生收到消息的方式**，理由见 §3 T-06） |
| 教师 | `rhermes-teacher addr` | 只打印本机地址，不起节点 |
| 教师 | `rhermes-teacher edu announce/revoke/sync` | 公告 / 撤销 / 教务同步（短命进程） |
| 学生 | `rhermes-stu live [--offline]` | 按配置接入全部老师（等价 `rhermes edu student live`） |
| 学生 | `rhermes-stu login/status/courses/profile/report/mode` | 单机侧（本页不覆盖） |

> **坑 2**：`ask` / `chat` **不是命令行参数**，是 `live` 进去之后的**交互台命令**。
> 直接敲 `rhermes-stu ask ...` 会被 clap 拒。

### 1.3 教师端配置（`$E2E/teacher-a/config.toml`）

```toml
[edu]
enabled = true
role = "teacher"

[edu.teacher]
# ⚠️ 必须与 `init-teacher` 的第一个参数**逐字一致**：本版教务表只有 name 列。
#    不一致 → `addr` / `serve` 报「教务库里没有名为 'xxx' 的老师」。
account      = "张老师"
display_name = "张老师"

# 托管清单：优先级「命令行 positional > 本清单 > 全部」
#   - 写了本清单 → `serve --offline` 就托管本清单里的项（`--offline` 只是模式开关，与清单无关）
#   - 删掉本清单，或加 `--all` → 托管该老师名下**全部**教学班
[[edu.teacher.serve]]
course = "CS101"
class  = "计算机2301"
```

### 1.4 学生端配置（`$E2E/stu-a/config.toml`）—— 先占位，T-06 后回填

```toml
[edu]
enabled = true
role = "student"

[edu.student]
student_no   = "2024001"
display_name = "张三"

[[edu.student.teachers]]
teacher  = "<老师地址：`teacher addr` 可提前拿到，或从 T-06 横幅抄>"
password = "pw101"
offline  = true          # 等价于命令行加 --offline
addr     = "127.0.0.1:<T-06 横幅里的端口；IP 用 127.0.0.1 即可>"
```

---

## 2. 判定符号

| 记号 | 含义 |
|---|---|
| ✅ **PASS** | 输出与「期望」逐字或语义一致 |
| ❌ **FAIL** | 见该用例的「失败排查」列，或跳 §8 速查表 |
| ⚠️ | 已知限制 / 非缺陷的预期现象 |

---

## 3. A 段：教师端流程

所有命令都在 `$E2E/teacher-a/` 下执行（`cwd` 必须是 exe 所在目录，否则数据会落到别处）。

### T-01 初始化教师身份

```bash
cd $E2E/teacher-a
./rhermes-teacher.exe init-teacher 张老师 tpw
```

| | |
|---|---|
| 期望 | `✅ 教师 '张老师' 创建成功 (ID: 1)` |
| 产物 | `$E2E/teacher-a/home/edu.db`（同时自动建 `home/`） |
| 失败排查 | 报 `unexpected argument '张老师' found` → 你敲的是 `init` 而不是 `init-teacher`（坑 1）<br>报 `数据库打开失败: unable to open database file` → 老版本缺陷 I；本版 `EduStore::open` 会 `create_dir_all(parent)` |
| **PASS** | rc=0 且出现 `✅ 教师 '张老师' 创建成功 (ID: 1)` |

### T-02 / T-03 建课、建班

```bash
./rhermes-teacher.exe course create CS101 "Python 编程基础"
./rhermes-teacher.exe class  create CS101 计算机2301
```

| | |
|---|---|
| 期望 | `✅ 课程创建成功: CS101 Python 编程基础 (1)`<br>`✅ 班级创建成功: 计算机2301 → CS101 (1)` |
| **PASS** | 两行都出现，且括号里是 id（课程 1、班级 1） |
| ⚠️ | 本版 `create_course` 固定用 `teacher_id = 1`（第一个教师）。一位老师一套凭据时无碍；多老师共用一个库时需改（见 §10） |

### T-04 加学生（**两种写法都要试**）

```bash
# 先补齐第二门课与第二个班（与 T-02/T-03 同款）
./rhermes-teacher.exe course create CS102 "数据结构与算法"
./rhermes-teacher.exe class  create CS102 计算机2302

# 5 参数：<学号> <姓名> <密码> <课程码> <班级名>   ← 脚本/CI 用
./rhermes-teacher.exe student add 2024001 张三 pw101 CS101 计算机2301

# 4 参数：<学号> <姓名> <课程码> <班级名>          ← 密码交互式询问
./rhermes-teacher.exe student add 2024002 李四 CS102 计算机2302
# （出现「为 李四 设置密码」提示时输入 pw102）
```

| | |
|---|---|
| 期望 | `✅ 学生添加成功: 2024001 张三 (1)` + 第二行 `     已自动选课: CS101`（李四同理，选课 CS102） |
| ⚠️ **回归要点** | 5 参数写法若把**密码当课程码**读，会报「课程 'pw101' 不存在」—— 这是 v0.7.13 缺陷 K 的症状，**指错方向**。本版两种写法都由纯函数 `parse_student_add` 分流（4 / 5 参数各一分支），有 4 条单测锁死 |
| ⚠️ **非交互环境必须用 5 参数** | 4 参数写法靠 `dialoguer` 询问密码；**无 TTY（管道 / CI）时拿不到输入**，会静默落到兑底密码（代码里是 `123456`）→ 学生随后按 `pw102` 认证会失败。实测无 TTY 仍会报 `✅ 学生添加成功`，**不报错**，所以坑很隐蔽。脚本里一律用 5 参数 |
| **PASS** | 两条都 rc=0，出现 `✅ 学生添加成功: …` 与 `已自动选课: …`，且 5 参数那条**没有**报「课程 'pw101' 不存在」 |

### T-05 查名册（两条命令）

```bash
./rhermes-teacher.exe course list
./rhermes-teacher.exe roster CS101
```

| | |
|---|---|
| 期望（`course list`） | `📚 课程列表:`，然后每行 `  <课程码> <课程名> — <id>` |
| 期望（`roster CS101`） | `📋 课程花名册: CS101 Python 编程基础` → 空行 → `  班级: 计算机2301 (1)` |
| ⚠️ 注意 | `roster` 目前**只列班级与课次，不列学生名单** —— 要确认学生登记成功，看 T-04 的 `✅ 学生添加成功` 回执，或直接查库 ↓ |
| **PASS** | 课程列表含 CS101/CS102；`roster CS101` 标题里的课程名与 T-02 一致 |

用 sqlite 交叉验证（表名是 **`edu_students`**，不是 `students`）：

```bash
python - <<'PY'
import sqlite3
db = sqlite3.connect(r"E:/lab/RHermes/target/e2e/teacher-a/home/edu.db")
print(db.execute("SELECT student_no, name, primary_class_id FROM edu_students").fetchall())
PY
# 期望：[('2024001', '张三', 1), ('2024002', '李四', 2)]
```

### T-06 启动托管服务（**核心步骤**）

```bash
./rhermes-teacher.exe serve --offline
```

期望横幅（记下 `老师地址` 与 `ip:…:PORT` 两行，T-07 要用）：

```
👩‍🏫 教学班托管服务已启动
   老师     : 张老师（id=1）
   身份     : 张老师 · dfac5082c709
   模式     : 离网直连（无中继）
   老师地址 : c85c79a9c709e4fe…bab7          ← 64 位，记作 TEACHER_A
              ip:192.168.1.101:62949         ← 记作 PORT_A
              ip:127.0.0.1:62949

   ✅ [1] CS101 / 计算机2301 · 学生 0 人 · epoch 0

   把「老师地址」填进学生的 [[edu.student.teachers]] 即可接入。
   输入 help 查看可用命令，quit 退出。

edu> _
```

| | |
|---|---|
| ⚠️ **为什么必须是 `serve`** | 另有 `rhermes-teacher edu announce <课程码> <班级> <标题> <正文>` 也能广播，但它是**短命进程**（广播完即退），且走 N0 中继而非离线直连 → **离线学生收不到**。要让学生真收到，只用 `serve` 里的交互命令或让学生在线 |
| ⚠️ **为什么是「学生 0 人」** | 白名单只收「已授权**且已绑定 EndpointId**」的学生；学生**认证前**就是 0 人。这不是 bug |
| 失败排查 | `教务库里没有名为 '张老师' 的老师` → `[edu.teacher].account` 与 `init-teacher` 不一致<br>空清单 → `serve` 参数与 `[[edu.teacher.serve]]` 都没给 |
| **PASS** | 出现 `✅ [1] CS101 / 计算机2301`，并可交互（出现 `edu> `） |

> 这个进程**保持不关**，B/C/D 段都要它在线。另开一个终端做后续步骤。

### T-07 / T-08 交互台命令

在 `edu>` 提示符下逐条敲：

```
list
addr
whoami
refresh
```

| 命令 | 期望输出 |
|---|---|
| `list` | 表头 `   id   课程        班级                      学期          状态`，下面一行 `   1    CS101       计算机2301                -             在线` |
| `addr` | 64 位老师地址 + 若干 `   ip:…`（**端口只有这里能给**） |
| `whoami` | `   张老师（id=1）· dfac5082c709` |
| `refresh` | `   🔄 已刷新 1 个班的白名单` |

> **取老师地址的两条路**（实测）：`rhermes-teacher addr`（**子命令**，不必先起 serve）会打印
> 身份文件路径 / 12 位指纹 / 64 位老师地址，并**直接给出学生配置片段**：
>
> ```
> 👩‍🏫 张老师
>    身份文件 : E:\lab\RHermes\target\e2e\teacher-a\home\edu_identities\dfac5082.key
>    指纹     : 8d40cd5969ad
>    老师地址 : 8d40cd5969adad83……d75ae3
>
>    学生配置片段：
>    [[edu.student.teachers]]
>    teacher = "8d40cd5969adad83……d75ae3"
> ```
>
> ⚠️ 但它**不含 `ip:port`** —— 直连地址（`--offline` 场景必填）只能从 `serve` 横幅里抄。

| | |
|---|---|
| **PASS** | 四条都给出上述形态；`whoami` 的指纹与 `addr` 的地址前 8 位**同源**（都来自同一把持久化钥匙） |
| ⚠️ | 学生还没接入时 `list` 里是 `在线`（班已入班）但横幅人数仍是 0，两者不矛盾：前者=本端会话就绪，后者=白名单里已绑定的学生数 |

### T-09 老师广播公告（**T-06 的交互台里**）

```
announce CS101 计算机2301 调课|周日补课
```

| | |
|---|---|
| 期望 | `   📣 已广播到 [1] CS101 / 计算机2301` |
| 语法 | `<课程码> <班级> <正文>`；正文里用 `\|` 分隔标题与正文（`split_title_body`） |
| **PASS** | 出现 `📣 已广播到 [1] CS101 / 计算机2301`；学生端（I-01）应收到 |

---

## 4. B 段：学生端流程

### S-01 回填学生配置

把 T-07 拿到的 `TEACHER_A` / `PORT_A` 填进 `$E2E/stu-a/config.toml`（模板见 §1.4）。
再复制一份给同学：`cp -r $E2E/stu-a $E2E/stu-b`，把 `student_no` 改成 `2024002`、
`display_name` 改成 `李四`、`password` 改成 `pw102`。

> ⚠️ 两份配置的 `student_no` **必须不同** —— 身份文件按学号命名（`home/edu_identities/<学号>.key`），
> 同名会共用同一把钥匙，两个「学生」就变成同一台设备。

### S-02 进入课堂

```bash
cd $E2E/stu-a
./rhermes-stu.exe live --offline
```

期望：

```
🎓 启动学生模式...
   数据库: E:\lab\RHermes\target\e2e\stu-a\home/edu.db

🎒 学生端进入课堂
   学号     : 2024001（张三）
   身份     : E:\lab\RHermes\target\e2e\stu-a\home\edu_identities\2024001.key · 78479f075ac9
   模式     : 离网直连（无中继）

   ✅ 已认证 c85c79a9 · 拿到 1 个教学班票据

   📚 已接入 1 个教学班：
      [c85c79a9#1] CS101 · 计算机2301

   输入 help 查看命令，quit 退出。
```

| | |
|---|---|
| 关键点 | `[c85c79a9#1]` 是 **`<老师地址前 8 位>#<该库内班 id>`** 的复合键 —— 班 id 在每位老师各自的库里都从 1 开始，所以**必须带老师前缀**才不串班 |
| 产物 | `home/edu_identities/2024001.key`（64 位 hex，**只此一份**，两位老师看到同一 EndpointId） |
| 失败排查 | `没有任何一位老师接入成功` → 看前面的 `⚠️  接入失败：…（原因）`；常见是地址/端口抄错、密码错、或该生不在花名册（认证成功但拿不到票据） |
| **PASS** | 出现 `✅ 已认证 … 拿到 1 个教学班票据` + `已接入 1 个教学班` |

### S-03 / S-04 交互台命令

```
help
list
```

| 命令 | 期望 |
|---|---|
| `help` | 四行：`list` / `ask <课程码> <班级> <问题>` / `chat <课程码> <班级> <内容>` / `quit` |
| `list` | 表头 `   课程        班级                      老师`，一行 `   CS101      计算机2301                  c85c79a9` |

| | |
|---|---|
| 失败排查 | `❌ 没有接入 'CS101 / 计算机2301'` → 课程码/班级名与配置里票据不一致，先 `list` 核对 |
| **PASS** | `list` 里能看到 S-02 接入的那个班 |

### S-05 发提问 / 讨论（**关键：只有同学能看见**）

在 S-02 的交互台里敲：

```
ask  CS101 计算机2301 红黑树删除为什么要分四种情况
chat CS101 计算机2301 同学们好
```

| | |
|---|---|
| 期望 | `   ✅ 已发送到 CS101 / 计算机2301` |
| ⚠️ **重要限制** | 教师端 `serve` 的交互台**只发不收**（`TeacherRuntime` 没有收件循环）→ **老师在终端里看不到这条提问**。今天的可观测路径是**同班同学**：`stu-b` 应收到 `📨 [CS101 / 计算机2301] ❓ 张三: 红黑树删除为什么要分四种情况` |
| **PASS** | 发送侧出现 `✅ 已发送到 …`；接收侧（stu-b）出现对应 `📨` 行 |

### S-06 退出

```
quit
```

| | |
|---|---|
| 期望 | `👋 已离开课堂`，进程 rc=0 |
| ⚠️ | 退出路径走 `SectionHost::shutdown`＝`send(Shutdown)` + `task.await` + **5 秒超时兜底**，最坏 5 秒内必定退出；若挂死超过 10 秒，视为 FAIL 并记 `Endpoint dropped without calling Endpoint::close` |
| **PASS** | 10 秒内退出，rc=0 |

---

## 5. C 段：双向联调（**两个学生同时在线的窗口内做**）

同时开着 `stu-a`（张三）与 `stu-b`（李四）两个 `live` 进程，然后：

### I-01 老师 → 全体学生（公告）

在 T-06 的 `edu>` 里：

```
announce CS101 计算机2301 调课|周日补课
```

| | |
|---|---|
| 期望（两个学生**都**收到） | `📨 [CS101 / 计算机2301] 📢 调课：周日补课` |
| **PASS** | stu-a 与 stu-b 各出现一行，内容一致 |

### I-02 白名单 / 纪元可见

学生刚认证接入时，服务端会 `publish_allowlist`。学生侧应出现：

```
🔐 [CS101 / 计算机2301] 成员表已更新：3 人 · epoch 0
```

| | |
|---|---|
| 人数含义 | 老师 + 2 名学生 = **3**（老师也在表里） |
| 若显示 `成员白名单已更新（纪元 N）`（**无人数**） | 这是「兜底路径」文案：`AllowlistUpdate` 消息被当普通班内发言收了，而非 runtime 解析出的人数。属异常，需查 §8 |
| **PASS** | 至少一端出现带人数的 `🔐 … 3 人 · epoch 0` |

### I-03 学生 → 全班（提问/讨论）

在 stu-a 敲 S-05 的两条命令 → 期望 **stu-b 收到 `📨`**，两侧内容一致。
（老师端看不到，理由见 S-05；这是当前实现的**已知边界**，不是缺陷 —— 教师侧收件待接。）

### I-04 学生 → 学生（互相可见）

在 stu-b 回一条 `ask`，确认 stu-a 也能收到 → 证明 Topic 是**多播**而非点对点。

### I-05 老师刷新白名单不影响通信

在 `edu>` 敲 `refresh` → 期望 `🔄 已刷新 1 个班的白名单`；学生侧可再次出现
`🔐 … epoch 0`（纪元不变＝同一批成员重签）。**通信不得中断**。

| | |
|---|---|
| **PASS** | I-01~I-05 全部满足；任一学生进程掉线/重连即为 FAIL |

---

## 6. D 段：撤销与重连（R3 组合撤销）

### R-01 老师撤销张三

在 `edu>` 里**保持服务在线**，另开终端执行（撤销是短命进程，可与 serve 共存）：

```bash
cd $E2E/teacher-a
./rhermes-teacher.exe edu revoke CS101 计算机2301 2024001 "连续缺勤"
```

期望：

```
🔄 启动 P2P 节点并执行撤销...
✅ 已撤销 2024001
   Topic 纪元: 0 → 1
   新 Topic: <64 位 hex>
   撤销前绑定节点: <64 位 hex>          ← 张三认证过，所以有值
   重新签发白名单: 2 个节点（含老师）
```

| | |
|---|---|
| 5 条不变量 | ① 纪元 `0 → 1`；② 新 Topic 与旧的不同；③ 被撤销设备被回传；④ 新白名单只剩 老师+李四 = **2**；⑤ DB 里张三 `revoked_at` 非空 |
| ⚠️ | 「撤销前绑定节点」若显示 `（该生尚未绑定设备）` → 该生从未认证过（手工建的学生就是如此）。**不是错误** |
| **PASS** | 逐字匹配上面 5 行 |

### R-02 其余学生自动跟随轮换

李四（stu-b）应收到：

```
🔄 [CS101 / 计算机2301] Topic 已轮换 → epoch 1
```

| | |
|---|---|
| 机制 | `TopicRotate` 只广播**纪元**（不带新 TopicId）；新 Topic 只能经认证信道 `/class-app → RefreshTickets` 拿 —— 这样被撤销者即使在场也拿不到新 Topic |
| **PASS** | stu-b 出现 `🔄 … epoch 1` |

### R-03 被撤销者收不到公告（终局验证）

在 `edu>` 里再发一条：

```
announce CS101 计算机2301 轮换后公告|张三不应收到
```

| | |
|---|---|
| 期望 | stu-b **收到** `📨 … 轮换后公告：张三不应收到`；stu-a（仍在旧 Topic）**收不到** |
| **PASS** | 一收一不收 —— 这是撤销真正生效的**唯一终局证据** |

### R-04 被撤销者无法重新进入

```bash
cd $E2E/stu-a && ./rhermes-stu.exe live --offline
```

| | |
|---|---|
| 期望 | 报错 `没有任何一位老师接入成功：\n   <前8位>（认证通过但无有效教学班（可能已被撤销））` |
| ⚠️ | 要点：**密码是对的**，但 `sections_for_student` 已过滤 `revoked_at` → 没有有效教学班 → 不下发票据。这正是缺陷 A 的修复点 |
| **PASS** | 认证被拒且**未**打印 `已接入 … 个教学班` |

### R-05 恢复授权（⚠️ **CLI 做不到，只能手工改库**）

先说结论：**没有 CLI 入口能「撤销撤销」**。三条路都堵死：

| 尝试 | 结果 |
|---|---|
| `student add` 重新登记 | ❌ 无效。`upsert_section_member` 明确**不覆盖** `authorized`/`endpoint_id`，也**不碰** `revoked_at` |
| 重新 `live` 认证 | ❌ 无效。`bind_endpoint` 虽会置 `authorized = 1`，但**不清 `revoked_at`**，而 `sections_for_student` 过滤的正是它 |
| Rust API `TeacherRuntime::restore_member` | ✅ 有效（内部 `set_member_authorized(true)` 会 `revoked_at = NULL`），**但没有任何 CLI 调用它** |

所以今天只能手工改库，作为**状态机语义**的验证：

```bash
python - <<'PY'
import sqlite3
db = sqlite3.connect(r"E:/lab/RHermes/target/e2e/teacher-a/home/edu.db")
db.execute("UPDATE edu_section_members SET authorized=1, revoked_at=NULL "
           "WHERE username='2024001'")
db.commit()
print(db.execute("SELECT username, authorized, revoked_at FROM edu_section_members "
                 "WHERE username='2024001'").fetchall())
PY
# 期望：[('2024001', 1, None)]
```

随后 stu-a 再次 `live --offline`：

| | |
|---|---|
| 期望 | `✅ 已认证 <前8位> · 拿到 1 个教学班票据` —— 注意 Topic **已是 epoch 1**（撤销时轮换过），所以必须重新认证才能拿到新票据 |
| **PASS** | 能重新接入；`list` 里出现 `[<前8位>#1] CS101 · 计算机2301` |
| 待补 | 给 `serve` 交互台加一条 `restore <课程码> <班级> <学号>`（调 `TeacherRuntime::restore_member`），见 §10 |

---

## 7. E 段：多老师多班（M 段）与 F 段（作业分发）

### M-01 ~ M-03 一位学生、两位老师

```bash
# 教师 B：另建一个便携目录（换 account / 换课程码）
cd $E2E/teacher-b
./rhermes-teacher.exe init-teacher 李老师 tpw
./rhermes-teacher.exe course create CS102 "数据结构与算法"
./rhermes-teacher.exe class  create CS102 计算机2302
./rhermes-teacher.exe student add 2024001 张三 pw101 CS102 计算机2302   # 同一学号，另一位老师处再登记
./rhermes-teacher.exe serve --offline        # 记下 TEACHER_B / PORT_B
```

学生 `stu-a` 的配置追加第二个 `[[edu.student.teachers]]`（填 `TEACHER_B` / `PORT_B`），
再 `live --offline`：

| 用例 | 判据 |
|---|---|
| **M-01** 同时接入两班 | 出现**两条** `✅ 已认证 <A前8位> · 拿到 1 个…` 与 `<B前8位> · 拿到 …`；`📚 已接入 2 个教学班` |
| **M-02** 身份唯一 | 两位老师看到**同一个 EndpointId**；本地只有一份身份文件；两位老师的**指纹互不相同**（每位老师一套凭据） |
| **M-03** 不串台 | 教师 A 广播 → 学生只出现 `📨 [CS101 / 计算机2301] …`；教师 B 广播 → 只出现 `📨 [CS102 / 计算机2302] …`。**任一越界即 FAIL** |
| 附：撤销 A 班 | 学生在 B 班的授权**必须保留**（缺陷 F/G 的回归点） |

### F 段：作业文件分发（F-01）—— ⛔ 当前**无法用 CLI 测试**

| 项 | 现状 |
|---|---|
| 代码 | `blobs.rs::publish_assignment_file` → `gossip.rs::broadcast_assignment`（`SectionMsg::AssignmentPosted`，只带 blob 哈希 + 票据，文件走 blobs 拉取） |
| 调用链 | `TeacherRuntime::publish_assignment_file` → `blobs::publish_assignment_file` —— **再往上没有 CLI 入口**，只存在于 Rust API |
| 已验 | 单测 3 条（blob 哈希稳定性 / 往返读写），**未**覆盖「发布 → 学生下载」全链路 |
| 结论 | **本段不可手工执行**。要覆盖它，需先给 `serve` 交互台加一条 `assignment <课程码> <班级> <文件路径>` 命令，再补一条 e2e 断言（学生侧 `blobs::fetch` 校验字节一致） |
| 今天能做的替代验证 | 学生侧在 `live` 里会收到 `📨 … 📝 新作业：<标题>（截止 <日期>）` —— 但**只有元数据通道**，没有 CLI 能触发它 |

---

## 8. 失败速查（症状 → 根因 → 处理）

| 症状 | 根因 | 处理 |
|---|---|---|
| `unexpected argument '<姓名>' found` | 用了 `init`（通用配置向导占名），不是 `init-teacher` | 改敲 `init-teacher` |
| `数据库打开失败: unable to open database file` | 便携目录里 `home/` 不存在（老版缺陷 I） | 升级到 ≥ v0.7.13；本版 `EduStore::open` 自动 `create_dir_all` |
| `课程 'pw101' 不存在` | 参数按位错读（缺陷 K 的症状） | 检查是不是 5 参数写法被当成 4 参数；本版由 `parse_student_add` 分流，若仍出现请提 issue |
| `教务库里没有名为 'xxx' 的老师` | `[edu.teacher].account` ≠ `init-teacher` 的第一个参数 | 两者改成逐字一致 |
| 学生 `已认证` 但 `拿到 0 个教学班票据` | 该生不在花名册，或已被撤销 | `student add` 重新登记；被撤销则看 §R-05 |
| `没有任何一位老师接入成功` | 地址/端口错 / 密码错 / 全部被拒 | 看紧跟的 `⚠️  接入失败：…（原因）`，逐条排除 |
| 学生接入成功但**收不到公告** | ① 老师在用短命进程 `edu announce`（离线学生收不到）② 学生先订阅后拿白名单（缺陷 D）③ gossip 无 store-and-forward，连接建立前的广播已丢 | ① 改用 `serve` 的 `announce` ②③ 升级到 ≥ v0.7.10，并且**订阅前先发第一条公告** |
| `dial failed: Connection was rejected locally` | `EndpointHooks::after_handshake` 是**双向**的，白名单没区分方向（缺陷 C） | 升级到 ≥ v0.7.10（修复：只拦 Server 侧） |
| 撤销后张三仍能收发 | `sections_for_student` 未过滤 `revoked_at`（缺陷 A）/ `TopicRotate` 带了新 TopicId（缺陷 B） | 升级到 ≥ v0.7.10 |
| 撤一个人，该生在**别的老师**的班也掉了 | 撤销按「整节点」而非「按班」（缺陷 F/G） | 升级到 ≥ v0.7.11（`revoke_from_section`） |
| `Endpoint dropped without calling Endpoint::close` | 进程/测试未走 `shutdown()` | 用 `quit` 正常退出；测试里显式 `shutdown()` |
| 老师终端看不到学生提问 | **已知边界**：`serve` 只发不收（`TeacherRuntime` 无收件循环） | 用同学视角验证（§S-05）；教师侧收件待 S2.3 接 |
| 输出里出现 `INFO rhermes::…` 干扰比对 | 默认日志级别 | 比对前过滤 `INFO`；或 `export RUST_LOG=warn` |

---

## 9. 一键回归（提交前）

```bash
cd /e/lab/RHermes
export RH_SKIP_WINRESOURCE=1

# ① 编译（快）
cargo check --all-targets

# ② 全量（期望 404 passed / 0 failed：398 单元 + 4 e2e + 2 kb）
cargo test

# ③ P2P 端到端单独复跑（看日志，~21s，不需要外网）
RUST_LOG=rhermes=info cargo test --test edu_p2p_e2e -- --nocapture

# ④ 本页 A/B 段的黑盒自动化等价物（脚本不进版本库，放在 target/ 下）
#    建便携目录 → 真 exe 跑 init-teacher / 建课建班 / 加学生 / serve --offline / live --offline
python target/smoke/run_e2e.py        # 若该脚本仍在（预期 rc=0）

# ⑤ 版本号（默认只递增 patch）
grep '^version' Cargo.toml
```

**②③ 必须全绿才可提交。** 若 ③ 失败，先按 §8 与 `docs/edu-p2p-testing.md §4` 对照日志。

---

## 10. 已知未覆盖（诚实清单）

| 项 | 现状 | 影响 |
|---|---|---|
| **作业文件分发（blobs）** | 代码就绪，无 CLI 入口，e2e 未断言 | 见 §7 F 段 |
| **教师端收件** | `serve` 只发不收 | 老师看不到学生提问；待接 |
| **跨机真实网络** | 本页全在 loopback + `--offline`；N0 中继/发现未验 | 校园网/公网建议按 §1 各跑一次真机 |
| **学生端多前端** | 只有 REPL 已就绪；TUI 与微信/企微/TG 尚未接（三驱动收敛进行中，设计 §12） | `ask/chat` 目前只能在 `live` 交互台里用 |
| **`teacher_id = 1` 硬编码** | `edu/mod.rs` 3 处 + `router.rs` 1 处 + `dashboard.rs` 1 处 | 一位老师一套库时无碍；共库多老师会串数据（设计 §12 S2.4） |
| **`edu_section_members` 无 `teacher_no`** | 教师身份以 `name` 对齐（历史表只有 `name` 列） | 重名老师会歧义 |
| **`edu announce` 短命进程** | 广播完即退 | 离线学生收不到；长驻场景用 `serve` 的 `announce` |
| **恢复授权（撤销的逆操作）** | `TeacherRuntime::restore_member` 存在但**无 CLI 入口**（`allowlist` 单测覆盖了库侧语义） | 撤销后只能手工改库恢复，见 §R-05 |
