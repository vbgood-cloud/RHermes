# 消息通道抽象层（`src/channel/`）

> 11 个文件 · 3399 行。把「消息从哪来、往哪回」抽象为 `Channel` trait，由 `ChannelManager` 统一管理生命周期。
> 代码：`channel/mod.rs`（trait + `ChannelStatus` + `ChannelState`）· `channel/manager.rs` · `channel/types.rs`
> 子通道：`wechat/` · `wecom/` · `telegram/` · `qq/` · `web/`
> 相关：微信/企微配置见 `docs/wechat-setup.md`；代理见 `docs/proxy.md`；Gateway 见 `docs/modules/gateway.md`

## `Channel` trait

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn start(self: Arc<Self>, inbound_tx: mpsc::UnboundedSender<InboundMessage>,
             shutdown_rx: broadcast::Receiver<()>) -> JoinHandle<()>;
    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String>;
    fn name(&self) -> &'static str;

    // 以下均有默认实现（可选覆写）
    async fn send_formatted(&self, chat_id: &str, text: &str) -> Result<(), String> { self.send_message(..).await }
    async fn login_qrcode(&self) -> Option<(String, Vec<u8>)> { None }
    fn status(&self) -> ChannelStatus { /* 全部 unknown */ }
    async fn send_typing(&self, _chat_id: &str) -> Result<(), String> { Ok(()) }
}
```

`ChannelStatus { name, connected, detail?, msg_count, last_error? }`（`Serialize`）
`ChannelState { connected: AtomicBool, msg_count: AtomicU64, last_error: Mutex<Option<String>> }` —— 各通道内嵌，`snapshot(name, detail)` 产出 `ChannelStatus`。

## `ChannelManager`

| 方法 | 说明 |
|------|------|
| `register(ch)` | 注册（须在 `start_all` 前） |
| `start_all()` | 每通道 spawn 一个 task：`tokio::spawn(ch.start(tx, shutdown_rx))` |
| `inbound_rx()` / `take_inbound_rx()` | 取入站接收端；`take_` 版解除与 manager 的借用关系（**调用后 manager 不再收新消息**，Gateway 用它避免与 `iter()` 冲突） |
| `inbound_tx()` | 拿发送端（供外部注入消息，如测试） |
| `broadcast(chat_id, text)` | 向**所有**通道发同一消息 |
| `get(name)` | 定向取通道（`SessionRouter` 回复用） |
| `shutdown()` | `broadcast::send(())` 停所有通道 |
| `all_status()` / `write_status_file(path)` | 状态快照 / 写 `channel_status.json` |
| `channel_count()` / `iter()` | 数量 / 迭代 |

**汇聚模型**：所有通道的入站消息都进**同一个** `mpsc::UnboundedSender<InboundMessage>`，由 Agent Loop（`SessionRouter`）统一消费，按 `(channel, chat_id)` 维护会话。

## 各通道实测成熟度

| 通道 | 状态 | 收发 | 关键事实 |
|------|------|------|---------|
| **微信个号** `wechat` | ✅ 可用 | 收发 | iLink Bot API（`/ilink/bot/*`）；扫码登录；token 落盘 `token_path`（默认 `home/wechat_token.txt`）；运行中 token 失效会**自动清 token 并重新扫码**；轮询 2s；支持 `[channels.wechat] proxy`（会迁移到全局 `[proxy]`） |
| **企业微信** `wecom` | ⚠️ 可用但受限 | 发（Webhook 群）/ 收（自建应用轮询） | `webhook_url` 发到群（所有人可见）；`corp_id`+`agent_id`+`.env` 的 `WECOM_SECRET` 轮询收；`access_token` 按 `expires_in` **提前 ≥7000s 续期**；`allow_from` 按 UserID 过滤；轮询 5s |
| **Telegram** `telegram` | ✅ 可用 | 收发 | Long Polling（`poll_timeout_secs`）；**唯一覆写 `send_formatted`（MarkdownV2 转义，解析失败回退纯文本）与 `send_typing` 的通道**；`allowed_chats` 白名单；token 从 `.env` 的 `TELEGRAM_BOT_TOKEN` |
| **QQ** `qq` | ⚠️ **未收尾** | 收（私聊/群） | Bot API + `sandbox` 开关；`allowed_groups` / `allow_private_chat`（默认 true）。🔴 **`state_connected()` 硬编码返回 `false`** → `status()` 无法反映真实连接状态 |
| **Web** `web` | ❌ 半成品 | — | `WebChannel` 存在于代码中，但 **全项目没有任何地方构造它**（`ChannelsConfig` 无 `web` 字段）；`send_message` 为**空实现**；`/api/reply` 无写入方。**当前不可用** |

> 注册路径只有一处：`gateway/mod.rs` 按 `[gateway].channels` 遍历。TUI 模式不注册任何外部通道。

## 配置片段

```toml
[channels.wechat]
enabled = true
token_path = "home/wechat_token.txt"
poll_interval_secs = 2

[channels.wecom]
enabled = true
webhook_url = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=xxx"
corp_id = "wwxxxx"
agent_id = "1000001"
allow_from = ["UserID1"]
poll_interval_secs = 5

[channels.telegram]
enabled = true
allowed_chats = []
poll_timeout_secs = 30    # 默认值见 config.rs

[channels.qq]
enabled = true
allow_private_chat = true
sandbox = false
```

`.env`：`WECOM_SECRET` / `TELEGRAM_BOT_TOKEN` / `QQ_BOT_APP_ID` / `QQ_BOT_APP_SECRET`。

## 已知边界

- **无「按用户路由到不同 Agent」能力**：所有通道汇入同一 Agent Loop，靠 `(channel, chat_id)` 分会话。
- **`send_formatted` 默认退化为纯文本**：只有 Telegram 实现了 MarkdownV2；微信/企微/QQ 的 Markdown 会**原样发出**（可能被客户端当纯文本显示）。
- **QQ 状态恒 false**（见上）。
- **Web 通道不可用**（见上）。
- **`ChannelState.connected` 由各通道自行维护**：没有统一的「健康探测」；`status.connected` 的准确度取决于实现（QQ 即反例）。
- **TUI 也是一个 `Channel`**：`src/tui/channel.rs::TuiChannel`（`name() == "tui"`），`lib.rs:843` 注册进 `ChannelManager`；
  `send_message` 把文本推入 `TUI_OUTBOUND` 全局队列，由 `App` 渲染循环上屏。`start()` 是**占位**（真正渲染循环在 `App::run()`）。
  这解释了 `router` 里 `inbound.channel == "tui"` 的判断以及 `reply_to_channel("tui", ..)` 能工作的原因。
- **TUI 模式只注册 `tui` / `wecom` / `wechat`**（`lib.rs:841-855`）：**Telegram 与 QQ 只在 Gateway 模式注册**，TUI 下无法启用。
- **`reply_to_channel` 静默失败**：`channel_mgr.get(name)` 返回 `None` 时不报错、不回落，回复直接丢弃（对未注册通道发回复会无声消失）。
