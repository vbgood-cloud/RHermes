# 教育版 P2P 通信 —— 完整测试方法与路径

> 适用版本：**v0.7.12+**（含 P1–P4 去中心化教学班通信 + 多老师拓扑 + 配置驱动）
> 对应设计：`docs/edu-p2p-design.md`（§4 双 ALPN / §5 Topic 隔离 / §5.4 R3 撤销 /
> §10.7 多老师缺陷 / §10.8 身份持久化与配置驱动）

---

## 0. 一条命令跑通（TL;DR）

```bash
cd /e/lab/RHermes
RH_SKIP_WINRESOURCE=1 cargo test --test edu_p2p_e2e -- --nocapture
```

**预期末行**：

```
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

3 个端到端用例各自覆盖一条主线：

| 用例 | 覆盖 |
|---|---|
| `edu_p2p_revocation_end_to_end` | 认证 → 入班 → 群发 → 越权拦截 → 白名单 → 撤销轮换 |
| `edu_p2p_multi_teacher_end_to_end` | 一位学生同时接入**两位老师**（同一把钥匙，两班互不串台） |
| `edu_p2p_config_driven_multi_teacher` | **配置文件**驱动：老师持久化凭据 + 学生多老师清单 |

全部**不依赖外网、不需要中继**（节点绑在 loopback 上直连）。

---

## 1. 环境准备（Windows + Git Bash）

```bash
cd /e/lab/RHermes

# ① 必须跳过 Windows 资源文件编译（否则 winres 会拖很久）
export RH_SKIP_WINRESOURCE=1

# ② 让测试日志可见（ts 化关键事件：入班 / 白名单 / 拒绝 / 轮换）
export RUST_LOG=rhermes=info

# ③ 首次会编译 ~2 分钟，之后增量 ~10 秒
```

> ⚠️ **沙箱问题**：`cargo test` 链接测试二进制时要在 `target/debug/incremental/**` 下
> 读写 `.o` 文件，沙箱会拒绝。若从 WorkBuddy 里跑，需放开沙箱；纯命令行终端不受影响。
> `cargo check` 不需要。

---

## 2. 测试分层（三层塔）

| 层 | 范围 | 命令 | 数量 | 特征 |
|---|---|---|---|---|
| **L1 单元** | 纯逻辑：Topic 派生 / 签名验签 / 纪元单调 / 撤销 SQL / 身份持久化 / 配置解析 / 托管项解析 | `RH_SKIP_WINRESOURCE=1 cargo test --lib edu::` | 123 | 毫秒级，无网络 |
| **L2 集成（本页主角）** | 真实 iroh 端点 + gossip + 双 ALPN，**离网** | `cargo test --test edu_p2p_e2e -- --nocapture` | 3（含 20+ 组断言） | ~21 秒，无外网 |
| **L3 手工** | 真机多进程 / 同机不同目录 | 见 §5 | — | 验收用 |

其中 edu 单元测试按模块分布（2026-09-16 实测）：

| 模块 | 数量 | 模块 | 数量 |
|---|---|---|---|
| `store` | 20 | `allowlist` | 10 |
| `reflection` | 10 | `course` | 9 |
| `identity` / `serve` / `net` / `p2p` | 8 ×4 | `registrar` | 7 |
| `auth` / `client_app` / `teacher` | 5 ×3 | `authz` | 4 |
| `blobs` / `gossip` / `model` | 3 ×3 | `runtime` | 2 |

**全量回归**：

```bash
RH_SKIP_WINRESOURCE=1 cargo test
# 期望：377 单元 + 3 edu_p2p_e2e + 2 kb_e2e = 382 passed / 0 failed
```

---

## 3. L2 端到端测试做了哪些事

测试文件：`tests/edu_p2p_e2e.rs`（函数 `edu_p2p_revocation_end_to_end`）

场景：1 老师 + 1 课程 + 1 教学班 + 2 学生（张三 / 李四）+ 1 个未认证旁观者。

### 3.1 五条链路与断言

| # | 链路 | 测试动作 | 关键断言 | 成功时的日志锚点 |
|---|---|---|---|---|
| 1 | **认证链** `/class-auth/1.0` | 学生提交学号密码 | `resp.ok == true`、票据非空、`topic_id` 非空 | `认证成功: 2024001 (张三) endpoint=… 教学班 1 个` |
| 2 | **入班同步** `/class-app/1.0` | 拉取签名成员表 → 再订阅 Topic | 学生本地 `registry` 含老师与同学 | `入班即同步白名单：3 个成员（epoch=0）` |
| 3 | **群组通信** gossip | 老师 `announce` | 学生收到 `SectionEvent::Message(Announce)` | `白名单已应用（epoch=0，3 个节点）` / `邻居加入: …` |
| 4 | **越权拦截** | 未认证节点连 `/class-app` | 连接**必须失败** | `拒绝未授权节点 3132cfa1… 接入协议 /class-app/1.0` |
| 5 | **白名单**（R2） | 老师 `publish_allowlist` | `student_count == 2`，学生事件 `(members=3, epoch=0)` | 同上 |
| 6 | **R3 撤销** | `revoke_member(张三)` | 见下 | 见下 |

### 3.2 撤销链（最关键的一组，7 条断言）

```rust
let outcome = teacher.revoke_member(section_id, "2024001", "端到端测试撤销").await?;
```

| 断言 | 含义 | 对应日志 |
|---|---|---|
| `old_epoch == 0`、`new_epoch == 1` | Topic 纪元递增（旧 Topic 作废） | `已撤销 2024001：epoch 0 → 1` |
| `revoked_endpoint == Some(张三的 EndpointId)` | 撤销接口回传被撤销设备 | — |
| `allowlist.student_count() == 1` 且 `!contains(张三)` | 新签名表已剔除该生 | — |
| `sections_for_student("2024001").is_empty()` | **DB 侧**不再有有效教学班 | — |
| `sections_for_student("2024002").len() == 1` | 其余学生不受影响 | — |
| 张三**重新认证**（密码正确）→ `ok == false` 且票据为空 | 密码对 ≠ 放行 | `认证通过但无有效教学班（可能已被撤销）: 2024001` |
| 张三用旧设备刷票 → 失败 | 认证信道被 Hook 挡住 | `拒绝未授权节点 4cddda00… 接入协议 /class-app/1.0` |
| 李四收到 `TopicRotated{new_epoch: 1}` | 剩余成员自动轮换 | `已随老师轮换至 epoch 1（票据刷新成功）` |
| 张三**收不到** `TopicRotated` | 他刷不到新票据 → 永远停在旧 Topic | `轮换后刷新票据被拒（很可能已被移出教学班）` |
| 轮换后公告：李四收到、张三收不到 | **终局验证**：撤销真正生效 | `公告已广播：轮换后公告` + `接收循环结束` |

### 3.3 为什么能离线跑

| 机制 | 生产 | 测试 |
|---|---|---|
| Endpoint preset | `presets::N0`（n0 公有 Relay + DNS 发现） | `presets::Minimal`（无中继无发现） |
| 地址解析 | DNS / Relay 自动发现 | `MemoryLookup` 手工地址簿（`P2pNode::add_peer_addr`） |
| 入口 | `TeacherRuntime::start` / `StudentRuntime::connect` | `start_offline` / `connect_offline` / `from_node` |

### 3.4 多老师回归：三个「临时回退必失败」的防线

多老师场景的缺陷**全都不会在单老师测试里出现**，所以每条修复都配了负控验证
（把修复临时改回旧写法 → 测试必须 FAILED），确认测试真的在守这件事，而不是恰好通过。

| 用例 / 断言 | 对应的修复 | 临时回退后实测 |
|---|---|---|
| `edu_p2p_multi_teacher_end_to_end`：`rt.sections().len() == 2` | `SectionKey` 全局班键（§10.7） | 回退成只用第一位老师的 id 作键 → **FAILED**：`应同时接入两个班: left 1, right 2` |
| `edu_p2p_revocation_end_to_end`：轮换前后 `session_topic` 必须不同 | 缺陷 E（轮换丢发送槽） | 回退 `let _ = new_session;` → **FAILED**（两个 TopicId 相同） |
| `test_reapply_section_keeps_other_sections`：其他班归属不得被抹掉 | 缺陷 F（撤销误伤其他班） | 回退 `revoke_from_section` → `revoke` → **FAILED**（panic「其他班归属不得被抹掉（缺陷 F）」） |

> 负控验证的做法：改回旧代码 → 跑单测 → 确认 FAILED → 改回修复 → 复跑通过。
> 这一步能挡住「写了个恒真断言」这类假防线。

---

## 4. 日志对照表（症状 → 根因 → 处理）

> 这张表是本次联调**真实踩过的坑**，排查时优先对照。

| 症状（日志） | 根因 | 修复 |
|---|---|---|
| `dial failed: Connection was rejected locally`（**本机自己拒自己**） | iroh 1.0.2 的 `EndpointHooks::after_handshake` 对**连接两端都会调用**。白名单不区分方向时，本机主动发起的 `/class-app`、gossip 会被自己的 Hook 拒掉 | `authz.rs`：`if conn.side().is_client() { Accept }` —— **只拦 Server（入站）侧** |
| 老师侧 `closed by peer: unauthorized: endpoint not in allowlist` | 同上（学生出站被自己拒 → 老师看到对端 403 关闭） | 同上 |
| 学生入班后收不到白名单/公告 | ① 学生**先订阅 Topic、后拿白名单**，Hook 空表 → 拒掉老师回拨；② gossip **无 store-and-forward**，连接建立前的广播直接丢弃 | ① `StudentRuntime::from_node` **先拉白名单再订阅**；② `SectionSession::join_topic` 有 bootstrap 时等 `joined()`；③ 新增 `/class-app` 的 `CurrentAllowlist` 拉取路径 |
| `等待 Topic 连接超时（15s）` | 上面的连锁反应（连不上老师） | 同上 |
| 撤销后张三仍能收发 | ① `sections_for_student` 未过滤被撤销者 → 重新认证又拿新 Topic；② `TopicRotate` 广播里带了 `new_topic_id` → 被撤销者立刻跟着订阅 | ① 新增 `revoked_at` 列并过滤；② **`TopicRotate` 不再携带新 TopicId**，改走认证信道 `RefreshTickets` |
| 轮换通知偶发收不到 | 广播只入队，旧 Topic 会话随即被 drop | `allowlist.rs`：后台任务保持旧 Topic 订阅并**重播 3 次**（~0.8s）再放手 |
| `Endpoint dropped without calling Endpoint::close` | 测试结束未 `shutdown()` | 测试末尾显式 `shutdown()`（正常收尾时不再出现） |

---

## 5. L3 手工验证（补充证据）

L2 已经把链路跑通。若想在**真机 / 真实目录**再验一遍副作用，用以下命令。

### 5.1 先建一个教务场景

```bash
cd /e/lab/RHermes
export RH_SKIP_WINRESOURCE=1

# 建库（写在 <项目>/home/edu.db）
cargo run -- edu teacher init 王老师 teacher_pass
cargo run -- edu teacher course create CS101 "Python 编程基础"
cargo run -- edu teacher class create CS101 计算机2301

# 批量加学生（CSV：学号,姓名,密码）
printf '2024001,张三,pw1\n2024002,李四,pw2\n' > /tmp/students.csv
cargo run -- edu teacher student import /tmp/students.csv CS101 计算机2301
cargo run -- edu teacher list
```

### 5.2 教务名单同步（P4，离线 CSV 两路）

```bash
mkdir -p /tmp/roster
printf '2024001,张三\n2024002,李四\n' > /tmp/roster/计算机2301.csv
printf 'CS101,计算机2301,计算机2301.csv\n' > /tmp/sections.csv

cargo run -- edu sync /tmp/sections.csv /tmp/roster T001 王老师 2026秋
# 期望：✅ 教务同步完成 + 汇总报告（新建/更新/跳过计数）
```

### 5.3 班内广播（P2）

```bash
cargo run -- edu announce CS101 计算机2301 "第 3 周安排" "周五小测，范围 1-2 章"
# 期望：✅ 公告已广播到「计算机2301」: 第 3 周安排
```

> 说明：该命令会临时启动老师节点、广播、再关闭（**短命进程**）。
> 要看学生是否真收到，请用 §0 的 L2 测试（学生节点在其中常驻）。

### 5.4 撤销成员（P4 / R3）—— 手工核验副作用

```bash
cargo run -- edu revoke CS101 计算机2301 2024001 "连续缺勤"
# 期望输出：
#   ✅ 已撤销 2024001
#      Topic 纪元: 0 → 1
#      新 Topic: <64 位 hex>
#      撤销前绑定节点: （该生尚未绑定设备）  ← 手工建的学生没认证过，所以为空
#      重新签发白名单: 2 个节点（含老师）
```

**用 sqlite 交叉验证**（三条不变量）：

```bash
python - <<'PY'
import sqlite3
db = sqlite3.connect(r"E:/lab/RHermes/home/edu.db")
print("① 被撤销者的成员行：")
print(db.execute("SELECT username, authorized, endpoint_id, revoked_at "
                 "FROM edu_section_members WHERE username='2024001'").fetchall())
print("② 教学班纪元：")
print(db.execute("SELECT id, name, topic_epoch FROM edu_classes").fetchall())
print("③ 有效成员（authorized=1 且未撤销）：")
print(db.execute("SELECT username, role FROM edu_section_members "
                 "WHERE authorized=1 AND revoked_at IS NULL").fetchall())
PY
```

期望：

| 检查 | 期望结果 |
|---|---|
| ① | `authorized=0`、`endpoint_id` 为 `None`、**`revoked_at` 非空** |
| ② | `topic_epoch` **= 1**（已轮换） |
| ③ | 只剩老师 + 李四，**张三不在** |

### 5.5 恢复授权（撤销的逆操作）

```bash
cargo run -- edu teacher student add 2024001 张三 pw1 CS101 计算机2301
# 注意：撤销时已轮换 Topic，该生必须**重新认证**才能拿到新票据
```

### 5.6 配置驱动的多老师（v0.7.12）——两终端 5 分钟演示

⚠️ **没有 `--config` 参数**：`PathManager::detect()` 以**可执行文件所在目录**为锚点 ——
`config.toml` 与它同级，数据根 `home/`（含 `edu.db`、`edu_identities/`）也在它下面。
所以「模拟多台机器」的做法是**建多个便携目录**，各自放一份可执行文件 + config.toml。

```bash
# 0. 编译一次，准备三个便携目录
RH_SKIP_WINRESOURCE=1 cargo build
mkdir -p /tmp/demo-a /tmp/demo-b /tmp/demo-stu
for d in /tmp/demo-a /tmp/demo-b /tmp/demo-stu; do
  cp target/debug/rhermes.exe target/debug/rhermes-teacher.exe \
     target/debug/rhermes-stu.exe "$d/"
done
```

> 图省事也可以直接 `cargo run`：此时 exe_dir = `target/debug/`，
> 配置就落在 `target/debug/config.toml`、数据在 `target/debug/home/`。

```bash
# ── A 机：老师 1 ────────────────────────────────
cd /tmp/demo-a
cat > config.toml <<'EOF'
[edu]
enabled = true
role = "teacher"

[edu.teacher]
account = "t001"
display_name = "张老师"

[[edu.teacher.serve]]
course = "CS101"
class  = "计算机2301"
EOF

# 建库（写 /tmp/demo-a/home/edu.db）+ 建课程 + 建班
./rhermes-teacher.exe init
./rhermes-teacher.exe course create CS101 "Python 编程基础"
./rhermes-teacher.exe class  create CS101 计算机2301

# ① 打印老师地址（学生要抄进配置）
./rhermes-teacher.exe addr
#   👩‍🏫 张老师
#      身份文件 : /tmp/demo-a/home/edu_identities/t001.key
#      指纹     : <12 位 hex>
#      老师地址 : <64 位 hex>        ← 记作 TEACHER_A

# ② 启动托管服务（保持在线），记下横幅里的 ip:127.0.0.1:PORT
./rhermes-teacher.exe serve --offline
#   👩‍🏫 教学班托管服务已启动
#      身份     : t001 · <指纹>
#      老师地址 : <TEACHER_A>
#               ip:127.0.0.1:PORT     ← 记作 PORT_A
#      ✅ [1] CS101 / 计算机2301 · 学生 1 人 · epoch 0
#   edu> _
```

B 机同理（`account = "t002"`、换成另一个课程码），拿到 `TEACHER_B` 与 `PORT_B`。

```bash
# ── 学生机 ──────────────────────────────────────
cd /tmp/demo-stu
cat > config.toml <<'EOF'
[edu]
role = "student"

[edu.student]
student_no   = "2024001"
display_name = "张三"

[[edu.student.teachers]]
teacher  = "<TEACHER_A>"
addr     = "127.0.0.1:<PORT_A>"
password = "pw"

[[edu.student.teachers]]
teacher  = "<TEACHER_B>"
addr     = "127.0.0.1:<PORT_B>"
password = "pw"
EOF

./rhermes-stu.exe live --offline
#   🎒 学生端进入课堂
#      身份 : /tmp/demo-stu/home/edu_identities/2024001.key · <指纹>
#      ✅ 已认证 <TEACHER_A 前 8 位> · 拿到 1 个教学班票据
#      ✅ 已认证 <TEACHER_B 前 8 位> · 拿到 1 个教学班票据
#      📚 已接入 2 个教学班
#   live> _
```

**核对要点**

| 要看什么 | 期望 |
|---|---|
| 两位老师指纹 | **互不相同**（每位老师一套凭据） |
| 学生身份文件 | 只有一份 `2024001.key`，两位老师看到**同一个 EndpointId** |
| 老师侧日志 | 各自出现 `认证成功: 2024001 (张三) … 教学班 1 个` |
| `edu> announce CS101 计算机2301 调课\|周日补课` | 学生端只出现 `📨 [CS101 / 计算机2301] …`，**不会串到 B 班** |
| 重启老师（不带 `--secret_key`） | 指纹不变，学生无需重新认证 |

---

## 6. 发版前检查清单

```bash
# ① 编译（快）
RH_SKIP_WINRESOURCE=1 cargo check --all-targets

# ② 全量测试（382 通过：377 单元 + 3 e2e + 2 kb）
RH_SKIP_WINRESOURCE=1 cargo test

# ③ P2P 端到端单独复跑（看日志）
RH_SKIP_WINRESOURCE=1 RUST_LOG=rhermes=info \
  cargo test --test edu_p2p_e2e -- --nocapture

# ④ 版本号（默认只递增 patch）
grep '^version' Cargo.toml
```

**必须全绿才可提交**。若 L2 失败，先按 §4 对照日志，再决定是否放行。

---

## 7. 已知未覆盖 / 后续项

| 项 | 现状 | 影响 |
|---|---|---|
| **blobs 作业文件分发** | 代码在 `blobs.rs`，L2 未断言（只测了 gossip 元数据通道） | 文件链路需补一条 e2e 断言 |
| **跨机真实网络** | L2 全在 loopback；`presets::N0` 的 Relay/发现路径未在 CI 验证 | 校园网/公网场景建议 §5 真机各跑一次 |
| **TUI / 渠道驱动** | 学生端目前只有 REPL（`student live`）；TUI 斜杠命令与微信/企微/TG 尚未接 | 三端需收敛到同一个 `SectionHost`（见设计 §11.3） |
| **`edu announce` 的短命进程** | 仍为「广播完就退」 | 离线学生收不到；长驻场景请用 `teacher serve` 的 `announce` |
| **`edu_section_members` 无 `teacher_no`** | 老师身份以 `name` 对齐（历史表只有 `name` 列） | 重名老师会歧义；后续迁移需加 `teacher_no` |

---

## 附 A：v0.7.10 联调修复的 4 个真实缺陷

| 编号 | 缺陷 | 危害 |
|---|---|---|
| **A** | `sections_for_student` 未过滤 `revoked_at` | 被撤销学生重新认证即拿到轮换后的新 Topic，**撤销完全失效** |
| **B** | `TopicRotate` 广播携带 `new_topic_id` | 该消息在**旧 Topic** 上广播，被撤销者就在场 → 立刻跟着订阅新 Topic |
| **C** | `WhitelistHook` 未区分连接方向 | 本机出站连接被自己拒掉 → `/class-app`、gossip 全部不可用 |
| **D** | 学生"先订阅、后拿白名单" | gossip 无转发，老师广播丢包 → 入班后长期收不到任何消息 |

A/B 由 L1 单测**测不出来**（单测只覆盖纯逻辑，不触发联网重入），
C/D 只有在**真实双端点**下才暴露 —— 这正是 L2 端到端测试的价值所在。

## 附 B：v0.7.11 多老师拓扑修复的 5 个缺陷

| 编号 | 缺陷 | 危害 |
|---|---|---|
| **班键** | 教材班 `section_id` 被当作全局唯一 | 每位老师各自一份 `edu.db`，班 id 都从 1 开始 → 学生选两位老师时两个「1 班」互相覆盖 |
| **E** | `TopicRotate` 只换了接收端、没换发送槽 | 轮换后老师的广播仍发往旧 Topic，在线学生再也收不到 |
| **F** | `apply_signed_allowlist` 用整条 `revoke(id)` | 撤一个班，顺带把该生在别的老师的班也退了 |
| **G** | `revoke_and_rotate` 同样整条 `revoke(id)` | 同上（老师主动撤销路径） |
| **H** | `WhoAmI` 返回该老师全部班 + 跨老师班 id 相同 | 串班：把别人的班当成自己的报给客户端 |

修复统一收敛到 **`SectionKey = (老师 EndpointId, 该库内班 id)`**（见设计 §10.7）。

## 附 C：v0.7.12 配置驱动的两个设计要点

| 要点 | 说明 |
|---|---|
| 身份必须持久化 | `Endpoint::builder().bind()` 默认每次随机生成身份；不落盘则老师的白名单/对端地址簿**每次重启全废**（`src/edu/identity.rs`） |
| 文件名清洗不能塌缩 | 非法字符若一律替换成 `_`，`张三`/`李四` 都会变成 `__` → **两位老师共用一套凭据**；发生替换时须追加内容指纹 |
