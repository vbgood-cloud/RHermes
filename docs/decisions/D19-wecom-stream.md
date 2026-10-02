# D19: 企微机器人切换智能机器人长连接（stream 模式，免回调）

> 决策编号: D19
> 日期: 2026-10-01
> 状态: 设计完成（待实施）
> 前置: D17（代理分层 — openws 域名国内可达，默认直连）

## 一、现状分析

### 1.1 现有 wecom 通道的实现（`src/channel/wecom/mod.rs`，298 行）

| 方向 | 现状 | 问题 |
|------|------|------|
| 出站（发消息） | 群机器人 `webhook_url` POST（`qyapi.weixin.qq.com/cgi-bin/webhook/send`） | 只能发**群机器人**，无法回复单聊；与入站不是同一身份 |
| 入站（收消息） | 自建应用 `corp_id + secret` 换 token → `message/list` **每 5 秒轮询** | 延迟高（最坏 5s+）；轮询空转；企微官方并不推荐此接口做实时消息 |
| 流式输出 | 无 | webhook 是一次性 POST，没有打字机能力 |

两套身份（群机器人 webhook / 自建应用）割裂，且"收到消息→回复"需要用户先在企微后台配置**回调 URL**（需公网可达 + AES 加解密）才能低延迟——这正是要消除的东西。

### 1.2 官方新能力：智能机器人长连接（stream 模式）

企业微信「智能机器人」提供 **WebSocket 长连接**接入（官方文档 101463）：

```text
开发者主动 wss 连接 openws.work.weixin.qq.com
  → aibot_subscribe（BotID + Secret 认证，业务层无需 AES 加解密）
  → 收 aibot_msg_callback（用户消息推送，单聊/群聊@）
  → aibot_respond_msg 回复：同一 streamId 反复 finish=false（打字机）
                             → 最后一次 finish=true（定格）
```

| 维度 | 长连接（新） | 回调 URL（旧） |
|------|------------|--------------|
| 公网暴露 | **不需要**（ outbound 发起） | 需要公网 IP + 域名 |
| 加解密 | wss 层自带，业务层零处理 | URL Token/AESKey 校验解密 |
| 延迟 | 毫秒级推送 | HTTP 回调 |
| 流式回复 | 原生支持（20480 字节/条） | 不支持 |

### 1.3 协议硬约束（实施必须遵守）

1. 后台必须选「使用长连接」——与「回调地址」**互斥**，选错只能重建机器人
2. **同一机器人仅允许 1 个长连接**（第二处连接触发 disconnected_event 踢掉前者）
3. 流窗口 **6 分钟**：streamId 超时未更新 → errcode 846608，需降级主动发送
4. 内容上限 20480 字节（UTF-8）；被动回复流式内容以**纯文本**为稳（Markdown 在 finish=true 的 msg_item 可用）
5. 频率 ~30 条/分钟、1000 条/小时；心跳 ping 建议 30s
6. 可见范围必须包含使用成员，否则消息不路由
7. （Node 生态踩坑）TLS ClientHello 需带 ALPN `http/1.1`，否则网关 400 —— 引入 SDK 可规避

## 二、决策

### 2.1 选型：引入 `wecom-aibot-rust-sdk` 而非自写协议栈

| 项 | SDK（wecom-aibot-rust-sdk 1.0.2） | 自写（tokio-tungstenite 已在依赖树） |
|----|----------------------------------|------------------------------------|
| 认证/心跳/指数退避重连（1s→30s） | ✅ 内置 | ~300 行状态机 |
| ALPN 握手坑 | ✅ 已处理 | 需 rustls 手配 ALPN，易踩 400 |
| 流式回复/模板卡片/主动推送 | ✅ 全覆盖 | 按需 ~200 行 |
| 文件下载 AES-256-CBC 解密 | ✅ 内置 | 暂不需要（可不做） |
| 新增依赖 | 1 个第三方 crate | 0 |

**决策：引入 SDK，但用 trait 包一层**（`WecomStreamApi`），屏蔽 SDK 类型不外泄——SDK 停更时可换实现而不动通道层。

### 2.2 双模式共存，stream 为默认增量

```text
[channels.wecom] mode = "stream"  → 新链路（BotID/Secret，长连接）
                        "poll"   → 旧链路保留（webhook + 轮询），缺省回落
```

## 三、架构设计

### 3.1 模块结构

```text
src/channel/wecom/
├── mod.rs        通道门面：按 mode 分派 Stream / Poll 实现（旧代码整体降级为 poll.rs）
├── stream.rs     WeComStreamChannel：Channel trait 适配（新）
└── poll.rs       原 webhook + 轮询实现平移（不动逻辑）
```

### 3.2 收发链路

```text
入站：aibot_msg_callback → WeComStreamChannel.start(inbound_tx)
      → InboundMessage { channel:"wecom", chat_id: 会话ID, content } → SessionRouter（现有通路零改动）
出站（被动流式）：AgentSession.sink = WecomStreamSink
      on_chunk  → 节流合并（≥800ms 或 ≥64 字节增量）→ reply_stream(finish=false)
      on_done   → reply_stream(finish=true)
      on_error  → 确保流关闭（finish=true 兜底，防"思考中"永久悬挂）
出站（主动推送）：Channel.send_message → aibot_send_msg
      （S2.3 课堂推送、课程切换通知等直接可用；前提：用户先给机器人发过消息）
```

### 3.3 WecomStreamSink 节流与降级（核心状态机）

```text
Agent 流式输出 ≠ 企微 stream 帧频率 —— 必须节流：
  chunk 缓冲 → 每 800ms flush 一次（或累计 64 字节）→ finish=false
  on_done → 立即 flush + finish=true

6 分钟窗口守护：
  每条消息处理启动 watchdog；
  距上次 stream 更新 >5min 仍无新内容 → 先发一条 finish=false 心跳文本（"...继续思考中"）
  收到 errcode 846608 / 流已关 → 降级：剩余内容一次性 aibot_send_msg 主动发送
```

### 3.4 通道注册与网关

- `ChannelManager` 注册名仍为 `"wecom"`（对 router / S2.3 推送零感知）
- gateway 模式天然满足「单连接唯一」约束（一机一 bot）；TUI 直连模式启动时若检测 disconnected_event，提示「另一个实例正在使用该机器人」

## 四、关于「扫码连接」的澄清与替代体验

**企微智能机器人协议中没有扫码帧**——扫码登录是微信**个人号**（wechat 通道 / iLink Bot）的机制。企微侧的最短路径是：

1. 管理员在企微管理后台「应用管理 → 智能机器人」创建机器人（一次）
2. 复制 BotID + Secret（Secret 仅显示一次）
3. 设置可见范围（必须包含使用成员）
4. 成员在企微客户端：单聊直接搜机器人 / 群里「添加群机器人」

为此提供 **`rhermes wecom setup` 交互向导**（对齐 `mcp setup` 惯例）：输入 BotID → Secret（密码态输入，写入 .env `WECOM_BOT_SECRET`）→ 即时试连校验（subscribe 成功/失败明确反馈）→ 打印成员端使用指引。配置体验上做到「一条命令 + 两行输入即上线」，等价于扫码的省事程度。

> 若需求本质是「个人微信扫码进群用机器人」，那是 `channel/wechat`（iLink）通道的范畴，与本决策分开演进。

## 五、配置文件格式

`config.toml`：

```toml
[channels.wecom]
enabled = true
mode = "stream"            # stream（新，长连接）| poll（旧，webhook+轮询，缺省回落）
bot_id = "wwwxxxx"          # 智能机器人 BotID（非 corpid）
# 旧字段（poll 模式继续使用）：
# webhook_url / corp_id / poll_interval_secs / allowed_users ...
```

`.env`：

```text
WECOM_BOT_SECRET=xxx        # 智能机器人 Secret（仅创建时可见）
```

`config.rs`：`WeComConfig` 增加 `mode: WeComMode`（enum，默认 Poll 保兼容）与 `bot_id: String`；`.env` 解析增加 `WECOM_BOT_SECRET` 分支。

## 六、验证方案

1. **单元**：Sink 节流合并（chunks → 帧序列断言）、6 分钟 watchdog 状态机（时钟注入模拟）、846608 降级分支、mode 分派
2. **集成**：本地起 ws mock server（tokio-tungstenite server，已有依赖）模拟 subscribe ack / msg_callback / errcode 846608 —— 覆盖「收到消息→流式回复→定格」全链路，不需要真实企微
3. **真机**（P3，需管理后台配合）：单聊问答（流式打字机）、群聊 @、S2.3 课堂推送到 wecom、断网重连恢复、双实例互踢提示
4. **回归**：`mode = "poll"` 全行为与现状一致（旧配置文件零改动可跑）

## 七、风险与对策

| 风险 | 对策 |
|------|------|
| SDK 停更 / 协议演进 | trait 隔离（WecomStreamApi），SDK 版本锁死 1.0.2；替换实现只动一个文件 |
| ALPN / 握手 400 | SDK 已处理；若自建 mock 复现不了真机问题，抓 wss 握手包比对 |
| 流假死（忘记 finish=true） | on_error/panic 路径统一走「确保关流」守卫（RAII guard） |
| 双实例互踢 | disconnected_event → 明确日志 + TUI 提示；部署约定一机一 bot |
| 频率超限（30/min） | 节流合并天然降帧；超限退避并在日志计数 |
| 用户误配回调模式 | setup 向导试连失败时给出「后台需选择长连接」的定向提示 |
| 旧用户回归 | mode 缺省 = poll，旧字段全部保留 |

## 八、实施阶段

| 阶段 | 内容 | 验收 |
|------|------|------|
| P1 | `WecomStreamApi` trait + SDK 接入 + stream.rs 通道（收发/重连）+ ws mock 集成测试 | mock 全链路绿 |
| P2 | `WecomStreamSink`（节流/watchdog/降级）+ gateway/TUI 注册 + `wecom setup` 向导 + 配置解析 | 单测 + 回归 417 全绿 |
| P3 | 真机验证（单聊/群聊/推送/重连） | 用户在企微后台创建机器人后联调 |

预计规模：stream.rs ~250 行、sink ~150 行、向导 ~80 行、poll.rs 平移（0 逻辑改动）、测试 ~200 行。
