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

## 二、企业微信（WeCom Bot）

适合团队使用，通过企业微信群机器人接收通知，通过自建应用推送消息。

### 前置准备

1. 登录[企业微信管理后台](https://work.weixin.qq.com/wework_admin/frame)
2. 创建一个**自建应用**（应用管理 → 创建应用）
3. 获取以下信息：
   - **CorpID**（我的企业 → 企业信息 → 企业 ID）
   - **AgentId**（应用管理 → 你的应用 → AgentId）
   - **Secret**（应用管理 → 你的应用 → Secret）
4. 创建一个**群机器人**（群聊 → 右键 → 添加群机器人 → 获取 Webhook URL）

### 1. 修改配置文件

```toml
[channels.wecom]
enabled = true

# 发送消息：群机器人 Webhook URL
webhook_url = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=xxx"

# 接收消息：企业自建应用信息
corp_id = "wwxxxxxxxxxxxx"
agent_id = "1000001"

# 消息轮询间隔（秒）
poll_interval_secs = 5

# 限制接收哪些用户的消息（留空=全部接收）
allow_from = ["UserID1", "UserID2"]
```

### 2. 配置 .env 文件

在 `config.toml` 同级目录下的 `.env` 文件中添加：

```env
WECOM_SECRET=your-secret-here
```

### 3. 启动

```bash
rhermes
```

### 注意事项

- **发送消息**：通过 Webhook 发送到群聊，所有人可见
- **接收消息**：通过自建应用的消息推送接口轮询，只有发给应用的消息才会被接收到
- `allow_from`：按企业微信的 UserID 过滤，不配置则接收所有用户的消息

### 完整配置示例

```toml
[channels.wecom]
enabled = true
webhook_url = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=693a91f6-7d1e-4b2e-8d3c-5f9e0a1b2c3d"
corp_id = "ww1234567890abcdef"
agent_id = "1000001"
poll_interval_secs = 5
allow_from = ["ChenJie", "LiMing"]

# .env
# WECOM_SECRET=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

---

## 三、同时启用多个通道

RHermes 支持 TUI + 微信 + 企业微信同时运行：

```toml
[channels.wechat]
enabled = true
token_path = "home/wechat_token.txt"

[channels.wecom]
enabled = true
webhook_url = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=xxx"
corp_id = "wwxxxxxxxxxxxx"
agent_id = "1000001"
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
A: 检查配置：
- `corp_id` 和 `secret` 是否正确
- 应用是否在企业微信管理后台设置为"可见范围"
- 先通过 Webhook 发送一条测试消息确认 Webhook URL 是否正确

### Q: Token 过期了怎么办？
A: 微信个号的 token 落盘持久化，正常情况长期可用、无需刷新。**若运行中 token 失效**（接口报错码含 `token` / `401` / 错误码），程序会**自动清除本地 token 并重新走扫码登录流程**——此时需要**重新用微信扫码**，无法无人值守自愈，因此长期挂机场景请在终端可见处运行。企业微信的 `access_token` 到期前自动刷新（默认按返回的 `expires_in` 提前续期），无需人工干预。
