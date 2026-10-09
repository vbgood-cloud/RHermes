# D19: 企微机器人切换智能机器人长连接（API 模式，免回调）

> 决策编号: D19
> 日期: 2026-10-01（2026-10-09 按代码复核更新）
> 状态: **已实施** —— v0.7.29 起步，v0.7.40 定型（TLS 切 `native-tls`）
> 前置: D17（代理分层 — openws 域名国内可达，默认直连）
> 代码: `src/channel/wecom/mod.rs`（93 行门面）+ `src/channel/wecom/stream.rs`（1234 行原生客户端）

## 〇、实施结果速览（2026-10-09 代码复核）

| 能力 | 状态 | 落点 |
|------|------|------|
| 建连 + `aibot_subscribe` 认证 | ✅ | `NativeStreamApi::connect`（必须等到 ack `errcode=0` 才算成功，不再"假成功"） |
| `ping` 30s 心跳 | ✅ | `run_link` 内 `interval(30s)`，`MissedTickBehavior::Delay` |
| 断线指数退避重连（1s → 30s 封顶） | ✅ | `run_link` 循环 |
| 入站 `aibot_msg_callback` → 路由 | ✅ | `parse_text_frame` → `InboundMessage::new("wecom", ..)` |
| 出站**主动**发送 `aibot_send_msg` | ✅ | `Channel::send_message` |
| 出站**流式打字机**（`aibot_respond_msg` + `finish` 翻转） | ⚠️ **通路就绪、Sink 未接线** | `reply_chat(chat_id, stream_id, content, finish)` 已可用；但**全项目无 `WecomStreamSink` 实现**，Agent 流式输出仍走一次性主动发送 |
| 6 分钟流窗口守护 / errcode `846608` 降级 | ❌ **未实现** | 全项目无 `846608`、无 watchdog |
| 配置入口统一（webhook 退役） | ✅ | v0.7.30；`WeComConfig` 中旧 webhook/自建应用字段**已物理删除** |
| 向导（明文 Secret + 即时试连） | ✅ | `gateway/setup.rs` + `try_connect_blocking` + `write_bot_secret_to_env` |
| 状态上报（不再空等 30s） | ✅ | 覆写 `Channel::status()` → `link_status()` |
| 本地 ws mock 集成测试 | ✅ | `stream.rs` 内 10+ 个 `#[tokio::test]`（见 §六） |
| 真机回归（单聊 / 群聊@ / 主动推送 / 重连恢复） | ⚠️ 部分完成 | v0.7.31–v0.7.40 提交显示认证与建连链路已真机排障（含 TLS 指纹复测）；完整回归清单**未见记录** |

## 一、现状分析（决策时，2026-10-01 的历史背景）

### 1.1 改造前 wecom 通道的实现

| 方向 | 当时现状 | 问题 |
|------|------|------|
| 出站（发消息） | 群机器人 `webhook_url` POST（`qyapi.weixin.qq.com/cgi-bin/webhook/send`） | 只能发**群机器人**，无法回复单聊；与入站不是同一身份 |
| 入站（收消息） | 自建应用 `corp_id + secret` 换 token → `message/list` **每 5 秒轮询** | 延迟高（最坏 5s+）；轮询空转；企微官方并不推荐此接口做实时消息 |
| 流式输出 | 无 | webhook 是一次性 POST，没有打字机能力 |

两套身份（群机器人 webhook / 自建应用）割裂，且「收到消息→回复」需要用户先在企微后台配置**回调 URL**（需公网可达 + AES 加解密）才能低延迟——这正是要消除的东西。

> 📌 上述 webhook + 轮询代码**已于 v0.7.34 整体删除**，本节仅作决策背景保留。

### 1.2 官方能力：智能机器人长连接（API 模式）

企业微信「智能机器人」提供 **WebSocket 长连接**接入（官方文档 101463 / 101833）：

```text
开发者主动 wss 连接 openws.work.weixin.qq.com
  → aibot_subscribe（BotID + Secret 认证，业务层无需 AES 加解密）
  → 收 aibot_msg_callback（用户消息推送，单聊/群聊@）
  → aibot_respond_msg 回复：同一 streamId 反复 finish=false（打字机）
                             → 最后一次 finish=true（定格）
  → aibot_send_msg 主动发送（不依赖某条入站消息）
  → ping 心跳（实现取 30s）
```

| 维度 | 长连接（新） | 回调 URL（旧） |
|------|------------|--------------|
| 公网暴露 | **不需要**（outbound 发起） | 需要公网 IP + 域名 |
| 加解密 | wss 层自带，业务层零处理 | URL Token/AESKey 校验解密 |
| 延迟 | 毫秒级推送 | HTTP 回调 |
| 流式回复 | 原生支持（20480 字节/条） | 不支持 |

### 1.3 协议硬约束（实施必须遵守，仍然有效）

1. 后台必须选「使用长连接」——与「回调地址」**互斥**，选错只能重建机器人
2. **同一机器人仅允许 1 个长连接**（第二处连接触发 `disconnected_event` 踢掉前者）
3. 流窗口 **6 分钟**：streamId 超时未更新 → errcode `846608`，需降级为主动发送
4. 内容上限 20480 字节（UTF-8）；被动回复以**纯文本**为稳
5. 频率 ~30 条/分钟、1000 条/小时；心跳 `ping` 建议 30s
6. 可见范围必须包含使用成员，否则消息不路由
7. ⚠️ **TLS ClientHello 指纹**：企微网关按握手特征区别对待（详见 §2.1），Rust 侧必须用 `native-tls`，不能用 `rustls`

## 二、决策

### 2.1 传输栈选型：**两度翻转，最终自研原生客户端**

这是本决策最有价值的部分——两次结论都是实测得出的，且**第一次是错的**：

| 阶段 | 做法 | 实测结论 |
|------|------|----------|
| v0.7.29 首发 | 引入第三方 `wecom-aibot-rust-sdk = "1.0.2"`，用 `WecomStreamApi` trait 包一层 | 订阅被服务端**静默 reset**；当时归因为「SDK 的子协议头不兼容」 |
| v0.7.37 | 改 `tokio-tungstenite` 直连（rustls）——一度成功 `errcode=0` | 10-04 复测证明那次成功是**节点差异 / 灰度偶然** |
| v0.7.40 | **确诊**：企微网关按 **TLS ClientHello 指纹** 区别对待 | python ssl（OpenSSL + ALPN `http/1.1`）连续 5 次全成功；`rustls`/ring 连上即被 reset → **`tokio-tungstenite` 必须启用 `native-tls`（Windows SChannel）** |

**最终决策：自研 `NativeStreamApi`（`stream.rs`，协议帧与官方完全同构），弃用第三方 SDK。**
trait 隔离（`WeComStreamApi`）在选型翻转时发挥了作用——替换实现只动通道层，`ChannelManager` / `SessionRouter` 零感知。

教训：**「换实现后成功」不等于「原因就是实现」**。要对同一变量做重复对照（本例最终靠 python 对照实验才定位到 TLS 指纹）。

### 2.2 「双模式共存」——已废弃

原计划保留 `mode = "stream" | "poll"` 双模式、`poll` 作缺省回落。**v0.7.34 起废弃**：

```rust
// src/core/config.rs —— 现在只有一个变体
pub enum WeComMode {
    /// 智能机器人 WebSocket 长连接（免回调）——唯一模式
    #[serde(other)]
    Stream,
}
```

存量 `config.toml` 里的 `mode = "poll"` 经 `#[serde(other)]` **静默映射为 Stream**，多余旧字段被 serde 忽略，不影响加载。

## 三、架构设计（按当前代码）

### 3.1 模块结构

```text
src/channel/wecom/
├── mod.rs        93 行：模式门面 WeComChannel { Stream, Disabled }，注册名恒为 "wecom"
└── stream.rs   1234 行：NativeStreamApi（原生客户端）+ WeComStreamChannel + mock 测试
```

（原设计中的 `poll.rs` 已随 v0.7.34 删除。）

### 3.2 凭证缺失不静默降级

`WeComChannel::new` 检查 `bot_id` 与 `bot_secret`；缺失则走 **`Disabled`** 变体（不回落任何旧链路）：

- `start()` → 空转等待关闭信号（不收不发）
- `send_message()` → 返回 `Err("wecom 通道未配置…")`
- **必须覆写 `Channel::status()`**，否则 `ChannelManager` 拿到 trait 默认实现（全 `false`）→ gateway 的「等待通道就绪」会**空等满 30 秒**、状态栏恒显 `⏳ wecom`

### 3.3 收发链路

```text
入站：aibot_msg_callback → parse_text_frame（chat_id / sender / content）
      → allow_from 过滤 → InboundMessage { channel:"wecom", chat_id, content } → SessionRouter
出站（主动）：Channel::send_message → aibot_send_msg
出站（流式）：reply_chat(chat_id, stream_id, content, finish) —— 通路已就绪，Sink 未接线
```

⚠️ **`on_text_message` 注册必须在 `connect()` 之前**：否则存在竞态——订阅刚通过、服务端随即推送的几条消息会在 handler 注册完成前到达 `run_link`，因 `text_handlers` 为空被丢弃（用户表现为「刚连上时发的消息没反应」）。

### 3.4 `parse_text_frame` 的真实字段（真机抓取，v0.7.40 修复）

企微实际推送形如：

```json
{ "aibotid": "...", "chattype": "single", "from": { "userid": "..." },
  "msgid": "...", "msgtype": "text",
  "response_url": "https://qyapi.weixin.qq.com/cgi-bin/aibot/response?...",
  "text": { "content": "你好" } }
```

⚠️ 早期实现**臆测**了 `chatid` / `sender.userid` 两个**并不存在**的字段 → 每条推送都以「缺 chatid」被丢弃，表现为「连接正常但机器人毫无反应」。

现行规则：
- 发送者取 `from.userid`（`sender` 仅作兼容回退）
- 会话键优先取 `chatid`/`chat_id`/`roomid`/`room_id`；**单聊推送不带 chatid**（只有 `chattype=single`），此时**回退用 `from.userid` 作为会话键**
- 丢弃原因一律走 `tracing::debug!` 输出，避免「静默不响应」无从排查

### 3.5 尚未实现的 Sink（打字机）

原设计的 `WecomStreamSink`（`on_chunk` 节流合并 ≥800ms/≥64 字节 → `finish=false`；`on_done` → `finish=true`；`on_error` 确保关流；6 分钟 watchdog + `846608` 降级）**均未落地**。

现状：Agent 回复走 `Channel::send_message` → `aibot_send_msg` 一次性发出，**没有打字机效果**，也不存在流悬挂风险。`reply_chat()` 已备好，接 Sink 即可启用。

## 四、关于「扫码连接」的澄清与替代体验

**企微智能机器人协议中没有扫码帧**——扫码登录是微信**个人号**（wechat 通道 / iLink Bot）的机制。企微侧的最短路径：

1. 管理员在企微管理后台「应用管理 → 智能机器人」创建机器人（一次）
2. 复制 BotID + Secret（Secret 仅显示一次）
3. 设置可见范围（必须包含使用成员）
4. 成员在企微客户端：单聊直接搜机器人 / 群里「添加群机器人」

对应入口是 **`rhermes gateway setup`**（不是 `rhermes wecom setup`）：选企微 → 填 BotID → 填 Secret（**明文输入**，便于粘贴核对，v0.7.35 起）→ **即时试连**（`try_connect_blocking`：成功/失败明确反馈，服务端 errcode 直接透传）→ 打印成员端指引。

> 试连成功后必须 `disconnect()` **再 `force_close()`**：若连接任务此刻阻塞在 `read.next()`，`Shutdown` 迟迟不被处理，残留连接会占用「机器人唯一长连接槽位」并与 gateway 互踢。

> 若需求本质是「个人微信扫码进群用机器人」，那是 `channel/wechat`（iLink）通道的范畴，与本决策分开演进。

## 五、配置文件格式（当前实际字段）

`config.toml`：

```toml
[channels.wecom]
enabled = true
mode = "stream"     # 唯一取值；存量 "poll" 由 serde(other) 静默映射为 stream
bot_id = "wwwxxxx"  # 智能机器人 BotID（非 corpid）
# ws_url = ""       # 可覆盖长连接网关（默认 wss://openws.work.weixin.qq.com；测试指向本地 mock）
# allow_from = []   # 允许的发送者 userid 列表；空 = 全部放行
```

`.env`：

```text
WECOM_BOT_SECRET=xxx    # 智能机器人 Secret（仅创建时可见）
```

`WeComConfig` 实际字段：`enabled` · `mode` · `bot_id` · `bot_secret`（`#[serde(skip)]`，只从 `.env` 读）· `ws_url` · `allow_from`。
**旧 webhook / 自建应用字段（`webhook_url` / `corp_id` / `poll_interval_secs` / `allowed_users`）已物理删除**，存量配置里的多余字段由 serde 静默忽略。

## 六、验证方案与现状

1. **单元 / 集成（已实现，`stream.rs` 内）**：本地 ws mock 网关模拟企业微信，覆盖
   - `spawn_mock_gateway` / `spawn_mock_gateway_recording_cmds` / `spawn_mock_gateway_rewriting_req_id` / `spawn_mock_gateway_missing_errcode`
   - `test_subscribe_ack_matched_by_cmd_when_req_id_differs`（ack 按 `cmd` 匹配，不依赖 req_id 回显）
   - `test_subscribe_ack_missing_errcode_reports_protocol_error`
   - `test_no_ping_racing_with_subscribe`
   - `test_force_close_terminates_link_task`
   - `test_stream_roundtrip_with_mock_gateway`
   - `test_status_reflects_link_state` / `test_status_reports_failure_detail`
   - `test_real_payload_reaches_inbound`（真机抓包字段 → 入站）
   - `test_parse_*`（显式 chat_id 优先 / legacy `sender` 回退 / 非文本拒绝）
   - `test_generate_req_id_*`（形态与跨快速调用唯一性）
2. **真机（部分完成）**：v0.7.31–v0.7.40 提交显示认证与建连链路已真机排障（含 TLS 指纹复测）。**单聊问答、群聊 @、主动推送、断网重连恢复的完整回归清单未见记录，仍待补**。
3. **回归**：无 poll 链路可回归（旧实现已删除）；存量 `mode = "poll"` 配置的加载兼容性由 `serde(other)` 覆盖。

## 七、风险与对策（按现状更新）

| 风险 | 现状 / 对策 |
|------|------|
| **TLS 指纹被网关拒** | 已确诊；用 `native-tls`。⚠️ 若将来有人为"减依赖"把 `tokio-tungstenite` 换回 `rustls`，会**直接复现连上即 reset**，必须在代码注释与提交信息里保留警告 |
| 第三方 SDK 停更 / 协议演进 | 风险已消失（已弃用 SDK，自研实现零第三方协议依赖） |
| 双实例互踢 | 服务端踢旧连接（`disconnected_event`）；部署约定一机一 bot；试连路径用 `force_close` 兜底 |
| 流假死（忘记 `finish=true`） | 当前**无流式发送**故不存在；接 Sink 时必须带 RAII 关流守卫 |
| 频率超限（30/min） | 目前一进一出，无合并需求；接 Sink 时需节流 |
| 用户误配回调模式 | setup 向导试连失败时给出「后台需选择长连接」的定向提示 |
| 字段变更导致静默丢弃 | 丢弃原因走 `debug!` 输出；`aibotid` 等新字段按需补 |

## 八、实施阶段与完成情况

| 阶段 | 内容 | 状态 |
|------|------|------|
| P1 | `WeComStreamApi` trait + 通道（收发/重连）+ ws mock 集成测试 | ✅ v0.7.29（当时基于 SDK） |
| P1' | **传输栈返工**：确诊 TLS 指纹 → 自研 `NativeStreamApi` + `native-tls` | ✅ v0.7.37–v0.7.40 |
| P2 | `WecomStreamSink`（节流/watchdog/846608 降级） | ❌ **未做**（`reply_chat` 通路已备） |
| P2' | 配置统一（webhook 退役）+ `gateway setup` 向导 + 状态上报 | ✅ v0.7.30 / v0.7.35 / v0.7.38 |
| P3 | 真机验证（单聊/群聊/推送/重连） | ⚠️ 部分（认证与建连已验，其余待补） |

## 九、演进时间线

| 版本 | 提交 | 内容 |
|------|------|------|
| v0.7.29 | `f0d1647` | D19 P1 — 长连接通道（基于第三方 SDK，mock 全链路验证） |
| v0.7.30 | `58ad8d3` | 配置统一 — 所有入口切长连接，webhook 退役 |
| v0.7.31 | `ceb8a8c` | 修复「假成功」— `connect` 升级为**等认证确认** |
| v0.7.32 | `318bb83` | 修复认证校验在 async 上下文 panic（`blocking_lock` 限制） |
| v0.7.33 | `f0ee623` | 认证失败详情透传 — 服务端 errcode 直接呈现 |
| v0.7.34 | `1b048eb` | **移除 poll 模式**，只保留长连接 |
| v0.7.35 | `449a056` | 向导 Secret 改**明文输入**（853000 多为复制不完整） |
| v0.7.37 | `bd3ed42` | **确诊并根治** — 自研原生客户端替换第三方 SDK |
| v0.7.38 | `ced5042` | 修 rustls CryptoProvider panic |
| v0.7.40 | `0bbb529` | **TLS 切 `native-tls`** — rustls 指纹被网关拒（复测确诊） |
