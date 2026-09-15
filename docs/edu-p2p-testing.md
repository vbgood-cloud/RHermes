# 教育版 P2P 通信 —— 完整测试方法与路径

> 适用版本：**v0.7.10+**（含 P1–P4 去中心化教学班通信）
> 对应设计：`docs/edu-p2p-design.md`（§4 双 ALPN / §5 Topic 隔离 / §5.4 R3 撤销）

---

## 0. 一条命令跑通（TL;DR）

```bash
cd /e/lab/RHermes
RH_SKIP_WINRESOURCE=1 cargo test --test edu_p2p_e2e -- --nocapture
```

**预期末行**：

```
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

这一条命令就把「认证 → 入班 → 群发 → 越权拦截 → 白名单 → 撤销轮换」五条链路
全部跑完了，**不依赖外网、不需要中继**（两个节点绑在 loopback 上直连）。

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
| **L1 单元** | 纯逻辑：Topic 派生 / 签名验签 / 纪元单调 / 撤销 SQL | `RH_SKIP_WINRESOURCE=1 cargo test --lib edu::` | 95+ | 毫秒级，无网络 |
| **L2 集成（本页主角）** | 真实 iroh 端点 + gossip + 双 ALPN，**离网** | `cargo test --test edu_p2p_e2e -- --nocapture` | 1（含 6 组断言） | ~11 秒，无外网 |
| **L3 手工** | 真机多进程 / 同机不同目录 | 见 §5 | — | 验收用 |

**全量回归**：

```bash
RH_SKIP_WINRESOURCE=1 cargo test
# 期望：350 单元 + 1 edu_p2p_e2e + 2 kb_e2e = 353 passed / 0 failed
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

---

## 6. 发版前检查清单

```bash
# ① 编译（快）
RH_SKIP_WINRESOURCE=1 cargo check --all-targets

# ② 全量测试（353 通过）
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
| **学生侧长驻 CLI** | 未实现（`StudentRuntime` 只有库入口，没有 `edu join-live` / `edu recv`） | 无法开多终端手工演示"老师边讲、学生边收"；需靠 L2 测试覆盖 |
| **blobs 作业文件分发** | 代码在 `blobs.rs`，L2 未断言（只测了 gossip 元数据通道） | 文件链路需补一条 e2e 断言 |
| **跨机真实网络** | L2 全在 loopback；`presets::N0` 的 Relay/发现路径未在 CI 验证 | 校园网/公网场景建议 §5 真机各跑一次 |
| **`edu announce` 的常驻形态** | 目前是短命进程（广播完就退） | 离线学生收不到；需长驻老师节点才能覆盖 |

---

## 附：本次联调修复的 4 个真实缺陷

| 编号 | 缺陷 | 危害 |
|---|---|---|
| **A** | `sections_for_student` 未过滤 `revoked_at` | 被撤销学生重新认证即拿到轮换后的新 Topic，**撤销完全失效** |
| **B** | `TopicRotate` 广播携带 `new_topic_id` | 该消息在**旧 Topic** 上广播，被撤销者就在场 → 立刻跟着订阅新 Topic |
| **C** | `WhitelistHook` 未区分连接方向 | 本机出站连接被自己拒掉 → `/class-app`、gossip 全部不可用 |
| **D** | 学生"先订阅、后拿白名单" | gossip 无转发，老师广播丢包 → 入班后长期收不到任何消息 |

A/B 由 L1 单测**测不出来**（单测只覆盖纯逻辑，不触发联网重入），
C/D 只有在**真实双端点**下才暴露 —— 这正是 L2 端到端测试的价值所在。
