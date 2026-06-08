# 网络代理配置

RHermes 支持全局三模式网络代理，所有 HTTP 请求通过统一的 `create_proxied_client` 工厂创建，无需为每个功能单独配置代理。

## 配置位置

```toml
# home/config.toml
[proxy]
mode = "auto"
url = "socks5://127.0.0.1:1080"

no_proxy = [
    "api.deepseek.com",
    "open.bigmodel.cn",
    "*.weixin.qq.com",
    "qyapi.weixin.qq.com",
    "localhost",
    "127.0.0.1",
]

[proxy.rules]
llm = true
web_search = true
web_fetch = true
wechat = false
wecom = false
telegram = true
command = false
```

也可以通过 `.env` 文件配置搜索相关环境变量（无需改 toml）：

```env
SERPER_API_KEY=your_serper_key    # 启用 Serper 付费搜索引擎
SEARCH_PROXY=http://127.0.0.1:7890  # 覆盖搜索专用代理
```

## 三模式说明

| 模式 | 值 | 说明 |
|------|-----|------|
| 全部走代理 | `"all"` | 所有请求走代理（`no_proxy` 排除仍生效） |
| 全部不走代理 | `"off"` | 所有请求直连，忽略 rules |
| 按功能开关 | `"auto"` | 按 `[proxy.rules]` 各自决定（默认值） |

## 决策流程

```
create_proxied_client(config, feature, timeout)
        │
        ▼
  mode = ?
  ├── "all"  → url 有值 ? 走代理 : 不走代理
  ├── "off"  → 不走代理
  └── "auto" → rules[feature] == true && url 有值 ?
                  ├── 是 → 走代理（检查 no_proxy 排除）
                  └── 否 → 不走代理
```

## 功能开关列表

仅在 `mode = "auto"` 时生效。

| 功能 | key | 默认值 | 影响范围 |
|------|------|--------|---------|
| AI API 调用 | `llm` | `false` | DeepSeekTransport（所有 Provider） |
| 网络搜索 | `web_search` | `false` | DuckDuckGo / Bing / Serper 搜索引擎 |
| 网页抓取 | `web_fetch` | `false` | WebFetch 工具 |
| 微信通道 | `wechat` | `false` | 微信 iLink Bot |
| 企业微信 | `wecom` | `false` | 企业微信 Webhook + 消息轮询 |
| Telegram | `telegram` | `true` | Telegram Bot API（预留） |
| 子进程 | `command` | `false` | run_command 注入 HTTP_PROXY 环境变量 |

## no_proxy 域名排除

`no_proxy` 列表对所有功能生效。请求的目标域名匹配此列表时，**不走代理，直连**。支持通配符（如 `*.weixin.qq.com`）。

典型效果：配置 `llm = true` 且 `no_proxy` 含 `api.deepseek.com` 时，DeepSeek 请求直连，OpenAI/Claude 走代理。

## 场景示例

### 场景 1：科学上网，全部走代理

```toml
[proxy]
mode = "all"
url = "socks5://127.0.0.1:1080"
no_proxy = [
    "api.deepseek.com",
    "*.weixin.qq.com",
    "localhost",
    "127.0.0.1",
]
```

所有请求走代理，DeepSeek/微信/企微由 no_proxy 排除直连，其余全走代理。

### 场景 2：仅搜索国外网站走代理

```toml
[proxy]
mode = "auto"
url = "http://127.0.0.1:7890"
no_proxy = ["localhost", "127.0.0.1"]

[proxy.rules]
web_search = true
web_fetch = true
```

AI API、微信等全部直连，只有 web_search 和 web_fetch 走代理。

### 场景 3：国内环境，完全不用代理

不配置 `[proxy]` 节，或显式：

```toml
[proxy]
mode = "off"
```

所有请求直连。

### 场景 4：子进程使用代理

```toml
[proxy]
mode = "auto"
url = "http://127.0.0.1:7890"

[proxy.rules]
command = true
```

执行 `run_command("curl https://httpbin.org/ip")` 时，子进程会继承 HTTP_PROXY 环境变量。

## 向后兼容

旧版配置文件中的 `[channels.wechat] proxy = "..."` 会自动迁移到全局代理配置：

```toml
# 旧配置（仍然支持）
[channels.wechat]
proxy = "socks5://127.0.0.1:1080"
```

自动效果：`proxy.url = "socks5://127.0.0.1:1080"`，`rules.wechat = true`。

## 验证方法

1. 查看启动日志：`启用代理` 日志会显示哪个 feature 走了代理
2. 设置 `mode = "all"` + 一个有效的代理 URL → 确认所有外网请求通过代理
3. 设置 `mode = "off"` → 确认无代理日志
4. `web_search(query="test")` → 如果国内不可达，配 `web_search = true` 后应正常工作
