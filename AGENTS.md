# RHermes — 终端 AI 编程 Agent（Rust）

DeepSeek API 驱动的终端 AI Agent，三段式 Context 缓存优化、并行工具调度、长期记忆与自主技能进化。

- **入口**: `src/main.rs` → `run_code()` 引导 → `tui::App::run()` 主循环
- **部署**: 便携模式 (`home/` 目录旁) / 传统模式 (`%APPDATA%/rhermes`)，自动检测，绿色部署
- **配置**: `config.toml`(非敏感) + `.env`(API Key)

## Commands

| 命令 | 说明 |
|------|------|
| `cargo build` | 编译 debug |
| `cargo build --release` | 编译 release |
| `cargo run` | 直接运行（自动检测配置，无配置则进初始化向导） |
| `cargo run -- init` | 初始化向导（API Key + 模型） |
| `cargo run -- debug export [session-id]` | 导出调试报告 |
| `cargo test` | 运行全部 119+ 个单元测试 |

## Architecture

```
src/
├── main.rs           CLI 入口 (clap), 引导初始化/运行
├── core/             基础设施
│   ├── config.rs     TOML + .env 配置加载
│   ├── context.rs    三段式 Context (prefix/log/scratch)
│   └── path.rs       PathManager (portable/traditional 绿色模式)
├── agent/            智能体逻辑
│   ├── memory.rs     长期记忆 (SQLite+FTS5)
│   ├── memory_manager.rs   MemoryProvider trait + 多 provider 路由
│   ├── skill.rs      技能引擎 (Markdown 文件)
│   ├── curator.rs    技能生命周期管理 (active→stale→archived)
│   ├── repair.rs     Tool-Call 修复流水线 (flatten/scavenge/truncation/storm)
│   └── task.rs       子 Agent 系统
├── api/              DeepSeek API 客户端 (同步/流式, 自动重试)
├── tools/            工具系统
│   ├── registry.rs   注册表 + Tool trait + 参数定义
│   ├── builtin.rs    17 个内置工具实现 (~3500 行)
│   └── dispatcher.rs 并行调度器 (parallel-safe vs serial)
├── tui/mod.rs        ratatui 终端 UI (~2000 行)
├── cost.rs           成本控制 (model tiers / 压缩)
├── debug.rs          调试日志缓冲区
└── init.rs           交互式初始化向导
```

## Conventions

- **注释**: 中文 `//!` 模块注释 + `//` 代码注释；段落分隔使用 `// ----...`
- **错误处理**: 自定义错误枚举 + `String`，使用 `map_err`/`map_err(|e| ...)` 转换
- **并行安全**: 工具通过 `parallel_safe()` 标记，dispatcher 据此分组调度
- **异步**: 全部 `tokio`；标准模式：`#[tokio::main]` + `async fn`
- **测试**: `#[cfg(test)] mod tests { ... }` 内联在文件底部；使用 `tempfile` 做 IO 测试
- **模块导出**: `mod.rs` 统一 `pub use` 重导出，模块内 `mod` 声明
- **数据目录**: 便携模式 `home/`，传统模式 OS 标准目录；所有路径通过 `PathManager` 获取
- **全局状态**: `tools::set_global_*()` 函数设置单例（config/skill_engine/display_config）
- **性能**: 大文件只读头部/尾部；`read_file` 受 `max_chars` 限制；Context 自动压缩

## Notes

<!-- 临时记录、待办、快速笔记放在这里 -->
- D17: 分层网络代理方案 — 全局三模式 + 功能开关 + no\_proxy 排除 > 决策编号: D17 > 日期: 2026-06-07 > 状态: 设计完成（方案 C 已选定） > 前置: D13（统一通道架构）、D14（安全加固） ## 一、现状分析 ### 1.1 网络请求点清单（含未来 Telegram） | 功能 | 源码位置 | 请求目标 | 国内可达 | 代理需求 | |------|---------|---------|---------|---------| | AI API | `src/provider/transport.rs:54-59` | `api.deepseek.com` 等 | 看 Provider | 按 Provider 区分 | | 微信通道 | `src/channel/wechat/mod.rs:187-194` | `ilinkai.weixin.qq.com` | ✅ | 通常不需要 | | 企微通道 | `src/channel/wecom/mod.rs:91-95` | `qyapi.weixin.qq.com` | ✅ | 通常不需要 | | Telegram（未来） | 待实现 | `api.telegram.org` | ❌ | 必须代理 | | 搜索工具 | `src/tools/builtin.rs:618` `reqwest::get()` | DDG/Serper/Brave | ❌ | 必须代理 | | 网页抓取 | `src/tools/builtin.rs:739` `reqwest::get()` | 任意 URL | 看目标 | 按域名区分 | | 命令执行 | `src/tools/builtin.rs:520` `Command::new()` | 子进程 | 看命令 | 按需注入 | ### 1.2 现有代码问题 - 5 个 HTTP 客户端中只有 `WeChatChannel` 支持代理（`channels.wechat.proxy`） - `WebSearch`/`WebFetch` 用 `reqwest::get()` 全局函数，无法注入代理 - 没有全局代理配置 - 没有 no\_proxy 排除机制 - 无法区分"国内直连"和"国外走代理" --- ## 二、决策：方案 C ### 2.1 三层模式 ``` mode = "all" → 全部走代理（忽略 rules 和 no_proxy） mode = "off" → 全部不走代理（忽略 rules 和 no_proxy） mode = "auto" → 按 rules 功能开关 + no_proxy 域名排除 ``` ### 2.2 功能开关（mode="auto" 时生效） | 功能 | key | 默认值 | 说明 | |------|-----|--------|------| | AI API | `llm` | `false` | 所有 Provider 统一 | | 搜索工具 | `web_search` | `false` | DDG/Serper/Brave | | 网页抓取 | `web_fetch` | `false` | 任意 URL | | 微信通道 | `wechat` | `false` | iLink Bot | | 企微通道 | `wecom` | `false` | Webhook + 消息轮询 | | Telegram | `telegram` | `true` | Bot API（未来） | | 子进程 | `command` | `false` | HTTP\_PROXY 环境变量注入 | ### 2.3 no\_proxy 域名排除 reqwest 原生支持 `NoProxy`，一个列表覆盖所有功能： ``` no_proxy = [ "api.deepseek.com", # DeepSeek 国内可达 "open.bigmodel.cn", # 智谱国内可达 "*.weixin.qq.com", # 微信/企微 "localhost", "127.0.0.1", "10.*", ] ``` **效果**：`llm = true` 时，DeepSeek/GLM 匹配 no\_proxy 直连，OpenAI/Claude 不匹配走代理。`web_fetch = true` 时，国内域名匹配 no\_proxy 直连，国外走代理。 ### 2.4 优先级 ``` all 模式 → 全走代理（no_proxy 仍生效） off 模式 → 全不走代理 auto 模式 → rules[feature] = true → 走代理（检查 no_proxy） rules[feature] = false → 不走代理 ``` ### 2.5 不做什么 - 不做 SOCKS5 代理服务器 - 不做代理认证（暂不支持 username:password） - 不做 per-request 代理切换（按 Client 粒度） - 不做 GFWList 自动判断（手动维护 no\_proxy） --- ## 三、架构设计 ### 3.1 配置结构 ```rust /// 代理模式 pub enum ProxyMode { All, // 全部走代理 Off, // 全部不走代理 Auto, // 按 rules 各自决定 } /// 代理配置 pub struct ProxyConfig { pub mode: ProxyMode, pub url: Option<String>, pub no_proxy: Vec<String>, pub rules: HashMap<String, bool>, } ``` ### 3.2 HttpClient 工厂 ```rust // src/core/http_client.rs pub fn create_proxied_client( config: &ProxyConfig, feature: &str, // "llm", "web_search", "wechat" 等 timeout: Duration, ) -> reqwest::Client { let need_proxy = match config.mode { ProxyMode::All => config.url.is_some(), ProxyMode::Off => false, ProxyMode::Auto => config.rules.get(feature).copied().unwrap_or(false) && config.url.is_some(), }; let mut builder = reqwest::Client::builder().timeout(timeout); if need_proxy { if let Some(ref url) = config.url { let mut proxy = reqwest::Proxy::all(url).expect("代理地址无效"); // no_proxy 排除 if !config.no_proxy.is_empty() { let no_proxy = reqwest::NoProxy::from_string(&config.no_proxy.join(",")); proxy = proxy.no_proxy(no_proxy); // 注入排除列表 } builder = builder.proxy(proxy); } } builder.build().expect("创建 HTTP 客户端失败") } ``` ### 3.3 各模块改造 | 模块 | 当前代码 | 改造方式 | feature key | |------|---------|---------|-------------| | `DeepSeekTransport::new` | `Client::builder().timeout()` | `create_proxied_client(&config.proxy, "llm", timeout)` | `llm` | | `WeChatChannel::new` | 手动 proxy 注入 | `create_proxied_client(&config.proxy, "wechat", 15s)` | `wechat` | | `WeComChannel::new` | `Client::builder().timeout()` | `create_proxied_client(&config.proxy, "wecom", 15s)` | `wecom` | | `WebSearch` | `reqwest::get()` | 持有 `reqwest::Client`，注册时创建 | `web_search` | | `WebFetch` | `reqwest::get()` | 持有 `reqwest::Client`，注册时创建 | `web_fetch` | | `RunCommand` | `Command::new()` | 按 `command` 开关注入环境变量 | `command` | | Telegram（未来） | — | `create_proxied_client(&config.proxy, "telegram", 15s)` | `telegram` | ### 3.4 WebSearch/WebFetch 改造 从 `reqwest::get()` 全局函数改为持有 `reqwest::Client`： ```rust // 改前 pub struct WebSearch; // execute 中：reqwest::get(&url) // 改后 pub struct WebSearch { http: reqwest::Client } // execute 中：self.http.get(&url) ``` Client 在 `builtin_registry()` 中通过 Config 创建并注入。 ### 3.5 RunCommand 环境变量注入 ```rust // mode=auto 且 rules.command=true 时 if need_command_proxy { cmd.env("HTTP_PROXY", &config.proxy.url); cmd.env("HTTPS_PROXY", &config.proxy.url); cmd.env("ALL_PROXY", &config.proxy.url); cmd.env("NO_PROXY", config.proxy.no_proxy.join(",")); } ``` --- ## 四、配置文件格式 ```toml # ── 网络代理 ── [proxy] # 三种模式： # "all" — 所有请求走代理 # "off" — 不使用代理 # "auto" — 按 rules 各自决定（默认） mode = "auto" url = "socks5://127.0.0.1:1080" # 不走代理的域名/IP（reqwest 原生 NoProxy 支持） # 通配符：*.example.com 匹配子域名 no_proxy = [ "api.deepseek.com", "open.bigmodel.cn", "*.weixin.qq.com", "qyapi.weixin.qq.com", "localhost", "127.0.0.1", ] # 功能开关（仅 mode="auto" 时生效） [proxy.rules] llm = true # AI API（DeepSeek/GLM 由 no_proxy 排除） web_search = true # 搜索工具（DDG 国内不可达） web_fetch = true # 网页抓取（国内域名由 no_proxy 排除） wechat = false # 微信通道 wecom = false # 企业微信 telegram = true # Telegram 通道（国内不可达） command = false # 子进程 HTTP_PROXY 注入 ``` ### 向后兼容 ```toml # 旧格式仍然支持（自动迁移到 rules.wechat=true + url） [channels.wechat] proxy = "socks5://127.0.0.1:1080" ``` --- ## 五、改动量估算 | 改动项 | 类型 | 文件 | 行数 | |--------|------|------|------| | ProxyConfig + ProxyMode | 新增 | `src/core/config.rs` | ~50 | | Config 字段 + Default | 修改 | `src/core/config.rs` | ~10 | | 向后兼容迁移 | 修改 | `src/core/config.rs` | ~10 | | HttpClient 工厂 | 新增 | `src/core/http_client.rs` | ~40 | | DeepSeekTransport | 修改 | `src/provider/transport.rs` | ~5 | | WeChatChannel 简化 | 修改 | `src/channel/wechat/mod.rs` | ~10 | | WeComChannel | 修改 | `src/channel/wecom/mod.rs` | ~5 | | WebSearch 持有 Client | 修改 | `src/tools/builtin.rs` | ~15 | | WebFetch 持有 Client | 修改 | `src/tools/builtin.rs` | ~15 | | RunCommand 注入 | 修改 | `src/tools/builtin.rs` | ~15 | | builtin\_registry 签名 | 修改 | `src/tools/builtin.rs` | ~15 | | Cargo.toml socks feature | 修改 | `Cargo.toml` | ~1 | | **合计** | | | **~191 行** | --- ## 六、依赖项 - reqwest `socks` feature（SOCKS5 代理支持） - 无其他新依赖 --- ## 七、验证方案 1. 无 `[proxy]` 节 → 所有功能正常（回归） 2. `mode = "all"` → 所有 HTTP 请求走代理 3. `mode = "off"` → 所有 HTTP 请求不走代理 4. `mode = "auto"` + `rules.llm = true` + `no_proxy = ["api.deepseek.com"]` → DeepSeek 直连，OpenAI 走代理 5. `mode = "auto"` + `rules.command = true` → `run_command("curl https://httpbin.org/ip")` 返回代理 IP 6. 旧的 `channels.wechat.proxy` → 自动迁移，微信通道走代理 结合现有代码给出最佳方案
