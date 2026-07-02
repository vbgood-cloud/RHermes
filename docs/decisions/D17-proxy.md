# D17: 分层网络代理方案

> 决策编号: D17
> 日期: 2026-06-07
> 状态: 设计完成（方案 C 已选定）
> 前置: D13（统一通道架构）、D14（安全加固）

## 一、现状分析

### 1.1 网络请求点清单（含未来 Telegram）

| 功能 | 源码位置 | 请求目标 | 国内可达 | 代理需求 |
|------|---------|---------|---------|---------|
| AI API | `src/provider/transport.rs:54-59` | `api.deepseek.com` 等 | 看 Provider | 按 Provider 区分 |
| 微信通道 | `src/channel/wechat/mod.rs:187-194` | `ilinkai.weixin.qq.com` | ✅ | 通常不需要 |
| 企微通道 | `src/channel/wecom/mod.rs:91-95` | `qyapi.weixin.qq.com` | ✅ | 通常不需要 |
| Telegram（未来） | 待实现 | `api.telegram.org` | ❌ | 必须代理 |
| 搜索工具 | `src/tools/builtin.rs:618` `reqwest::get()` | DDG/Serper/Brave | ❌ | 必须代理 |
| 网页抓取 | `src/tools/builtin.rs:739` `reqwest::get()` | 任意 URL | 看目标 | 按域名区分 |
| 命令执行 | `src/tools/builtin.rs:520` `Command::new()` | 子进程 | 看命令 | 按需注入 |

### 1.2 现有代码问题

- 5 个 HTTP 客户端中只有 `WeChatChannel` 支持代理（`channels.wechat.proxy`）
- `WebSearch`/`WebFetch` 用 `reqwest::get()` 全局函数，无法注入代理
- 没有全局代理配置
- 没有 no\_proxy 排除机制
- 无法区分"国内直连"和"国外走代理"

---

## 二、决策：方案 C

### 2.1 三层模式

```text
mode = "all"  → 全部走代理（忽略 rules 和 no_proxy）
mode = "off"  → 全部不走代理（忽略 rules 和 no_proxy）
mode = "auto" → 按 rules 功能开关 + no_proxy 域名排除
```

### 2.2 功能开关（mode="auto" 时生效）

| 功能 | key | 默认值 | 说明 |
|------|-----|--------|------|
| AI API | `llm` | `false` | 所有 Provider 统一 |
| 搜索工具 | `web_search` | `false` | DDG/Serper/Brave |
| 网页抓取 | `web_fetch` | `false` | 任意 URL |
| 微信通道 | `wechat` | `false` | iLink Bot |
| 企微通道 | `wecom` | `false` | Webhook + 消息轮询 |
| Telegram | `telegram` | `true` | Bot API（未来） |
| 子进程 | `command` | `false` | HTTP\_PROXY 环境变量注入 |

### 2.3 no\_proxy 域名排除

reqwest 原生支持 `NoProxy`，一个列表覆盖所有功能：

```text
no_proxy = [
  "api.deepseek.com",    # DeepSeek 国内可达
  "open.bigmodel.cn",    # 智谱国内可达
  "*.weixin.qq.com",     # 微信/企微
  "localhost",
  "127.0.0.1",
  "10.*",
]
```

### 2.4 优先级

```text
all 模式  → 全走代理（no_proxy 仍生效）
off 模式  → 全不走代理
auto 模式 → rules[feature] = true  → 走代理（检查 no_proxy）
           rules[feature] = false → 不走代理
```

---

## 三、架构设计

### 3.1 配置结构

```rust
pub enum ProxyMode { All, Off, Auto }
pub struct ProxyConfig {
    pub mode: ProxyMode,
    pub url: Option<String>,
    pub no_proxy: Vec<String>,
    pub rules: HashMap<String, bool>,
}
```

### 3.2 HttpClient 工厂

```rust
// src/core/http_client.rs
pub fn create_proxied_client(
    config: &ProxyConfig, feature: &str, timeout: Duration,
) -> reqwest::Client
```

### 3.3 各模块改造

| 模块 | feature key |
|------|-------------|
| `DeepSeekTransport` | `llm` |
| `WeChatChannel` | `wechat` |
| `WeComChannel` | `wecom` |
| `WebSearch` / `WebFetch` | `web_search` / `web_fetch` |
| `RunCommand`（环境变量注入） | `command` |
| Telegram（未来） | `telegram` |

---

## 四、配置文件格式

```toml
[proxy]
mode = "auto"
url = "socks5://127.0.0.1:1080"
no_proxy = ["api.deepseek.com", "open.bigmodel.cn", "*.weixin.qq.com", "localhost", "127.0.0.1"]

[proxy.rules]
llm = true
web_search = true
web_fetch = true
wechat = false
wecom = false
telegram = true
command = false
```

旧格式 `channels.wechat.proxy` 仍支持（自动迁移到 `rules.wechat=true + url`）。

---

## 五、验证方案

1. 无 `[proxy]` 节 → 所有功能正常（回归）
2. `mode = "all"` → 所有 HTTP 请求走代理
3. `mode = "off"` → 所有 HTTP 请求不走代理
4. `mode = "auto"` + `rules.llm = true` + `no_proxy = ["api.deepseek.com"]` → DeepSeek 直连，OpenAI 走代理
5. `mode = "auto"` + `rules.command = true` → `run_command("curl https://httpbin.org/ip")` 返回代理 IP
6. 旧的 `channels.wechat.proxy` → 自动迁移，微信通道走代理
