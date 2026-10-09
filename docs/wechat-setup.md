# 微信连接配置指南

RHermes 支持两种微信接入方式：

1. **微信个号（iLink Bot API）** — 扫码登录你的个人微信，**推荐**
2. **企业微信（WeCom Bot）** — 通过企业微信群机器人收发消息

---

## 一、微信个号（扫码登录）⭐ 推荐

无需注册应用，启动时扫码即可登录你的个人微信号。

### 1. 修改配置文件

编辑 `config.toml`，添加：

```toml
[channels.wechat]
enabled = true
# token 自动保存路径（扫码成功后自动写入，下次启动免扫码）
token_path = "home/wechat_token.txt"
# 消息轮询间隔（秒，默认 2）
poll_interval_secs = 2
# 如果网络需要代理，可配置（可选）
# proxy = "http://127.0.0.1:7890"
```

### 2. 启动程序

```bash
rhermes
```

启动后，TUI 界面会：

1. ✅ 自动显示 **ASCII 二维码**（以 ██ 空格矩阵形式）
2. ✅ 将二维码保存为 `wechat_qrcode.png`，并**在 Windows 上自动用系统图片查看器打开**
   （实现：`cmd /C start "" wechat_qrcode.png`；非 Windows 平台该调用会静默失败，需手动打开文件）
3. ✅ 显示提示 `📱 微信扫码登录 — 二维码已保存到: wechat_qrcode.png`

> 💡 **Gateway 模式（`rhermes gateway`）**下文件名带通道后缀：`wechat_qrcode.wechat.png`，便于多通道并存。

### 3. 扫码登录

用手机微信扫描终端中显示的二维码，或打开弹出的二维码图片扫码。

- 扫码后终端提示自动更新（`Scanned` → `Confirmed`）
- 登录成功后 token 保存到 `token_path` 指定的文件
- **下次启动时自动恢复登录状态，无需再次扫码**

> ⚠️ **关于二维码图片格式**：文件扩展名固定为 `.png`，但**内容可能是 BMP**。
> 当 iLink 接口返回的是**扫码链接**时，程序用本地 `qrcodegen` 生成**未压缩的 BMP 位图**写入该文件；
> 只有接口直接返回 base64 图片时才落盘 PNG。两者所有图片查看器都能打开，**若某些工具按扩展名严格校验格式而报错，改扩展名为 `.bmp` 即可**。

### 4. 发送消息给 RHermes

登录完成后，向你的微信发送消息，RHermes 会自动接收并处理。

### 完整配置示例

```toml
[channels.wechat]
enabled = true
token_path = "home/wechat_token.txt"
poll_interval_secs = 2
# proxy = "http://127.0.0.1:7890"
```

---

## 二、企业微信（智能机器人 · 长连接）

企业微信走**智能机器人长连接**（官方称 API 模式）：程序主动连出 `wss://openws.work.weixin.qq.com`，**不需要公网 IP、不需要配回调 URL、业务层不做 AES 加解密**，且原生支持单聊问答。

> ⚠️ **旧链路已整体移除**：`webhook_url`（群机器人）与 `corp_id` / `agent_id` / `WECOM_SECRET`（自建应用 `message/list` 轮询）自 **v0.7.34** 起**全部失效**。存量 `config.toml` 里的这些字段会被静默忽略——不报错，但也不生效。

### 前置准备

1. 登录[企业微信管理后台](https://work.weixin.qq.com/wework_admin/frame)
2. 「应用管理 → **智能机器人**」创建一个机器人
3. 接入方式必须选 **「使用长连接」**（与「回调地址」**互斥**，选错只能重建机器人）
4. 记录 **BotID** 与 **Secret**（Secret 仅创建时显示一次）
5. 设置**可见范围**，必须包含要使用的成员，否则消息不会路由到机器人

### 1. 一键配置（推荐）

```bash
rhermes gateway setup
```

选择「企业微信 Bot」后依次填 BotID、Secret（**明文显示**，便于粘贴核对），程序会**即时试连**并把结果直接反馈给你（服务端 errcode 原样透传）。成功后自动写入：

- `config.toml` → `[channels.wecom]` 的 `enabled` / `mode` / `bot_id`
- `.env` → `WECOM_BOT_SECRET`

### 2. 手工配置

`config.toml`：

```toml
[channels.wecom]
enabled = true
mode = "stream"           # 唯一取值（存量 "poll" 会被 serde 静默映射为 stream）
bot_id = "wwwxxxxxxxx"     # 智能机器人 BotID（**不是** corpid）
# ws_url = ""              # 可选：覆盖长连接网关地址（测试指向本地 mock）
# allow_from = []          # 可选：允许的发送者 userid 列表，空 = 全部放行
```

`.env`（与 `config.toml` 同级）：

```env
WECOM_BOT_SECRET=your-secret-here
```

### 3. 启动

```bash
rhermes gateway start      # 守护进程模式（推荐，长连接常驻）
```

### 注意事项

- **一机一 bot**：同一机器人**只允许 1 个长连接**，第二处连接会把前一个踢掉。不要在多台机器 / 多个实例上用同一套 BotID+Secret。
- **成员侧**：企微客户端里单聊直接搜机器人；群里用「添加群机器人」。
- **TLS 指纹**：企微网关按 TLS ClientHello 特征区别对待，Rust 侧**必须用 `native-tls`**。这是编译期固定的依赖特性——升级依赖时**不要**把 `tokio-tungstenite` 换回 `rustls`，否则表现为「连上即被服务端重置」。
- **回复形态**：当前为**一问一答**（走 `aibot_send_msg` 主动发送），**暂无打字机流式效果**。
- **连接状态**：`rhermes gateway status` 与启动状态表里的 wecom 状态取自长连接真实状态。若显示「未配置 BotID / WECOM_BOT_SECRET，通道已禁用」，说明凭证缺失——通道**不会**静默降级到其它链路。
- 完整技术细节（协议、TLS 指纹确诊过程、未完成项）见 `docs/decisions/D19-wecom-stream.md`。

---

## 三、同时启用多个通道

RHermes 支持 TUI + 微信 + 企业微信同时运行：

```toml
[channels.wechat]
enabled = true
token_path = "home/wechat_token.txt"

[channels.wecom]
enabled = true
mode = "stream"
bot_id = "wwwxxxxxxxx"
# .env: WECOM_BOT_SECRET=xxxxxxxx
```

三个通道（TUI、微信、企业微信）的消息会汇聚到同一个 Agent Loop，统一处理。

---

## 四、常见问题

### Q: 微信二维码过期怎么办？
A: 过期由**服务端**决定，程序不做本地计时。二维码状态轮询为每 2 秒一次，当服务端返回 `Expired` 时程序会**自动重新获取**新二维码（终端与图片同步刷新），无需手动操作。另外连续 5 次查询失败也会触发重新获取。

### Q: 扫码后没有反应？
A: 检查终端日志（`rhermes.log`）中是否有错误信息。常见原因：
- 网络代理问题（配置 `[channels.wechat] proxy`）
- 微信版本不兼容（请更新微信到最新版）

### Q: 企业微信收不到消息？
A: 按顺序排查：
- `.env` 里的 `WECOM_BOT_SECRET` 是否**完整**（复制不全是最常见原因，服务端会回 `853000` 类凭证错误）
- 机器人后台的接入方式是否选了 **「使用长连接」**（与回调地址互斥）
- 机器人的**可见范围**是否包含你本人
- 是否有**另一个实例**正占用同一个机器人的唯一长连接（会互相踢下线）
- 连接本身是否成功：`rhermes gateway status` 看 wecom 的 `connected` 与 `detail`；日志（`rhermes.log`）里搜 `wecom`
- 日志里若出现 `wecom 推送未进入路由（<原因>）`，括号内就是真实的丢弃原因（非文本消息 / 字段缺失 / 被 `allow_from` 过滤）

### Q: Token 过期了怎么办？
A: 微信个号的 token 落盘持久化，正常情况长期可用、无需刷新。**若运行中 token 失效**（接口报错码含 `token` / `401` / 错误码），程序会**自动清除本地 token 并重新走扫码登录流程**——此时需要**重新用微信扫码**，无法无人值守自愈，因此长期挂机场景请在终端可见处运行。企业微信走长连接，**不涉及 access_token 轮换**（认证在 `aibot_subscribe` 一次性完成，断线由指数退避自动重连）。
