//! RHermes 配置模块
//!
//! 配置分离：
//! - `config.toml` — 非敏感配置（模型/地址/超时等）
//! - `.env` — 敏感配置（API Key）
//!
//! `.env` 文件格式:
//! ```env
//! DEEPSEEK_API_KEY=sk-xxxxxxxxxxxxxxxx
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

// ---- 常量 ----

/// `.env` 文件中 API Key 的键名
const ENV_KEY_NAME: &str = "DEEPSEEK_API_KEY";

/// 环境变量中 base_url 的键名（优先级高于 config.toml）
const ENV_BASE_URL: &str = "DEEPSEEK_BASE_URL";

/// `.env` 文件名
const ENV_FILE_NAME: &str = ".env";

// ---------------------------------------------------------------------------
// 配置结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// DeepSeek API Key（仅从 .env 读取，永不写入 config.toml）
    #[serde(default, skip)]
    pub api_key: String,

    /// API 配置（向后兼容，等价于 providers.deepseek）
    #[serde(default)]
    pub api: ApiConfig,

    /// 多 Provider 配置表（新增，优先级高于 api）
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// 请求配置
    #[serde(default)]
    pub request: RequestConfig,

    /// 记忆与笔记配置
    #[serde(default)]
    pub memory: MemoryConfig,

    /// 调试配置
    #[serde(default)]
    pub debug: DebugConfig,

    /// 显示与截断配置
    #[serde(default)]
    pub display: DisplayConfig,

    /// Agent 行为配置
    #[serde(default)]
    pub agent: AgentConfig,

    /// Provider Pool 配置（熔断器）
    #[serde(default)]
    pub provider_pool: ProviderPoolConfig,

    /// 消息通道配置
    #[serde(default)]
    pub channels: ChannelsConfig,

    /// 网络代理配置
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// Gateway 守护进程配置
    #[serde(default)]
    pub gateway: GatewayConfig,

    /// 定时任务调度器配置
    #[serde(default)]
    pub scheduler: SchedulerConfig,

    /// WASM 插件配置
    #[serde(default)]
    pub wasm: WasmPluginConfig,

    /// MCP 客户端配置
    #[serde(default)]
    pub mcp: McpConfig,
    /// 搜索引擎配置
    #[serde(default)]
    pub search: SearchConfig,
}

// ---------------------------------------------------------------------------
// 代理配置
// ---------------------------------------------------------------------------

/// 代理模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// 全部走代理（忽略 rules，但 no_proxy 仍生效）
    All,
    /// 全部不走代理
    Off,
    /// 按 rules 功能开关各自决定
    Auto,
}

impl Default for ProxyMode {
    fn default() -> Self {
        Self::Auto
    }
}

/// 网络代理配置
///
/// 三种模式：
/// - `all`：所有 HTTP 请求走代理
/// - `off`：不使用代理
/// - `auto`：按 `[proxy.rules]` 功能开关各自决定
///
/// `no_proxy` 列表排除不需要代理的域名/IP，reqwest 原生支持。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    #[serde(default)]
    pub mode: ProxyMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub no_proxy: Vec<String>,
    /// 功能开关（仅 mode="auto" 时生效）
    /// key = 功能名（llm/web_search/web_fetch/wechat/wecom/telegram/command）
    #[serde(default)]
    pub rules: HashMap<String, bool>,
}

impl ProxyConfig {
    /// 判断指定功能是否需要走代理
    pub fn need_proxy(&self, feature: &str) -> bool {
        match self.mode {
            ProxyMode::All => self.url.is_some(),
            ProxyMode::Off => false,
            ProxyMode::Auto => {
                self.rules.get(feature).copied().unwrap_or(false) && self.url.is_some()
            }
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            mode: ProxyMode::Auto,
            url: None,
            no_proxy: Vec::new(),
            rules: HashMap::new(),
        }
    }
}

/// 消息通道配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelsConfig {
    /// 企业微信配置
    #[serde(default)]
    pub wecom: WeComConfig,
    /// 微信个号 iLink Bot 通道配置
    #[serde(default)]
    pub wechat: WeChatConfig,
    /// Telegram Bot 通道配置
    #[serde(default)]
    pub telegram: TelegramConfig,
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            wecom: WeComConfig::default(),
            wechat: WeChatConfig::default(),
            telegram: TelegramConfig::default(),
        }
    }
}

/// 企业微信 Bot 通道配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeComConfig {
    /// 是否启用
    #[serde(default)]
    pub enabled: bool,
    /// Webhook URL（发送消息用）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub webhook_url: String,
    /// 企业 ID（接收消息用）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub corp_id: String,
    /// 应用 Agent ID（接收消息用）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_id: String,
    /// 应用 Secret（接收消息用，仅从 .env 读取）
    #[serde(default, skip)]
    pub secret: String,
    /// Webhook 关键词（用于验证）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub webhook_key: String,
    /// 允许的发送者列表（空=全部允许）
    #[serde(default)]
    pub allow_from: Vec<String>,
    /// 消息轮询间隔（秒）
    #[serde(default = "default_wecom_poll_interval")]
    pub poll_interval_secs: u64,
}

fn default_wecom_poll_interval() -> u64 { 5 }

impl Default for WeComConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            webhook_url: String::new(),
            corp_id: String::new(),
            agent_id: String::new(),
            secret: String::new(),
            webhook_key: String::new(),
            allow_from: Vec::new(),
            poll_interval_secs: default_wecom_poll_interval(),
        }
    }
}

/// 微信个号 iLink Bot 通道配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeChatConfig {
    /// 是否启用
    #[serde(default)]
    pub enabled: bool,
    /// 登录 token（扫码登录后自动获取，可持久化）
    #[serde(default, skip)]
    pub bot_token: String,
    /// Proxy 代理地址（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// 轮询间隔（秒）
    #[serde(default = "default_wechat_poll_interval")]
    pub poll_interval_secs: u64,
    /// Token 刷新后自动保存的路径
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_path: String,
}

fn default_wechat_poll_interval() -> u64 { 2 }

impl Default for WeChatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            proxy: None,
            poll_interval_secs: default_wechat_poll_interval(),
            token_path: String::new(),
        }
    }
}

/// Telegram Bot 通道配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// 是否启用
    #[serde(default)]
    pub enabled: bool,
    /// Bot Token（优先从 .env 的 TELEGRAM_BOT_TOKEN 读取）
    #[serde(default, skip)]
    pub bot_token: String,
    /// 允许的 chat_id 列表（空=全部允许）
    #[serde(default)]
    pub allowed_chats: Vec<String>,
    /// Long Polling 超时（秒）
    #[serde(default = "default_telegram_poll_timeout")]
    pub poll_timeout_secs: u32,
}

fn default_telegram_poll_timeout() -> u32 { 30 }

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            allowed_chats: Vec::new(),
            poll_timeout_secs: default_telegram_poll_timeout(),
        }
    }
}

/// Gateway 守护进程配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// 是否启用 gateway 模式（供 rhermes gateway start 使用）
    #[serde(default)]
    pub enabled: bool,
    /// PID 文件路径
    #[serde(default = "default_gateway_pid_file")]
    pub pid_file: String,
    /// 日志文件路径
    #[serde(default = "default_gateway_log_file")]
    pub log_file: String,
    /// Gateway 启动时自动启用的通道列表
    #[serde(default)]
    pub channels: Vec<String>,
}

fn default_gateway_pid_file() -> String { "home/gateway.pid".into() }
fn default_gateway_log_file() -> String { "home/gateway.log".into() }

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pid_file: default_gateway_pid_file(),
            log_file: default_gateway_log_file(),
            channels: vec!["wechat".into()],
        }
    }
}

/// 定时任务调度配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// 是否启用定时任务
    #[serde(default)]
    pub enabled: bool,
    /// 最多同时执行的任务数
    #[serde(default = "default_scheduler_max_concurrent")]
    pub max_concurrent_tasks: usize,
    /// 定时任务列表
    #[serde(default)]
    pub tasks: Vec<ScheduledTaskConfig>,
}

fn default_scheduler_max_concurrent() -> usize { 5 }

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent_tasks: default_scheduler_max_concurrent(),
            tasks: Vec::new(),
        }
    }
}

/// 单个定时任务配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTaskConfig {
    /// 任务名称（用于日志标识）
    pub name: String,
    /// cron 表达式 "分 时 日 月 周"
    pub cron: String,
    /// 发给 Agent 的提示词
    pub prompt: String,
    /// 结果推送目标 "channel:chat_id"（空 = 只记日志）
    #[serde(default)]
    pub target: String,
    /// 是否启用
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool { true }

/// WASM 插件配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmPluginConfig {
    /// 是否启用 WASM 插件
    #[serde(default)]
    pub enabled: bool,
    /// 插件目录（相对于 RHermes 运行目录）
    #[serde(default = "default_wasm_plugins_dir")]
    pub plugins_dir: String,
    /// 单个插件执行超时（毫秒）
    #[serde(default = "default_wasm_timeout_ms")]
    pub timeout_ms: u64,
    /// 允许的最大内存（字节），默认 32MB
    #[serde(default = "default_wasm_max_memory")]
    pub max_memory: u64,
}

fn default_wasm_plugins_dir() -> String { "plugins".into() }
fn default_wasm_timeout_ms() -> u64 { 30_000 }
fn default_wasm_max_memory() -> u64 { 32 * 1024 * 1024 }

impl Default for WasmPluginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            plugins_dir: default_wasm_plugins_dir(),
            timeout_ms: default_wasm_timeout_ms(),
            max_memory: default_wasm_max_memory(),
        }
    }
}

/// MCP 客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub servers: std::collections::HashMap<String, McpServerConfig>,
    /// 是否启用 Sampling 能力（默认关闭，安全敏感）
    #[serde(default)]
    pub sampling_enabled: bool,
    /// Sampling 请求的 max_tokens 上限（默认 1024）
    #[serde(default = "default_sampling_max_tokens")]
    pub sampling_max_tokens: u32,
}

fn default_sampling_max_tokens() -> u32 { 1024 }

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            servers: std::collections::HashMap::new(),
            sampling_enabled: false,
            sampling_max_tokens: default_sampling_max_tokens(),
        }
    }
}

/// 搜索引擎配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Serper API Key（仅从 .env 读取）
    #[serde(default, skip)]
    pub serper_api_key: String,
    /// 搜索超时（秒）
    #[serde(default = "default_search_timeout")]
    pub timeout_secs: u64,
    /// 搜索结果缓存大小
    #[serde(default = "default_search_cache_size")]
    pub cache_size: usize,
    /// 缓存 TTL（秒）
    #[serde(default = "default_search_cache_ttl")]
    pub cache_ttl_secs: u64,
    /// HTTP 代理地址（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

fn default_search_timeout() -> u64 { 15 }
fn default_search_cache_size() -> usize { 100 }
fn default_search_cache_ttl() -> u64 { 600 }

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            serper_api_key: String::new(),
            timeout_secs: default_search_timeout(),
            cache_size: default_search_cache_size(),
            cache_ttl_secs: default_search_cache_ttl(),
            proxy: None,
        }
    }
}

/// 单个 MCP Server 的配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// 消息发送 URL（Direct 模式，SSE 不可用时使用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_url: Option<String>,
    /// HTTP 请求头
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    /// Server 类型: stdio / sse / http（自动推断）
    #[serde(default)]
    pub server_type: String,
    #[serde(default)]
    pub parallel_safe: bool,
    /// 工具级别的并行安全配置（按工具名覆盖 Server 级别设置）
    #[serde(default)]
    pub tool_parallel_safe: std::collections::HashMap<String, bool>,
}

/// Provider Pool 配置（熔断器）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPoolConfig {
    /// 熔断器阈值：连续失败 N 次后断开
    #[serde(default = "default_circuit_breaker_threshold")]
    pub circuit_breaker_threshold: u32,
    /// 熔断器冷却时间（秒）
    #[serde(default = "default_circuit_breaker_cooldown")]
    pub circuit_breaker_cooldown_secs: u64,
}

impl Default for ProviderPoolConfig {
    fn default() -> Self {
        Self {
            circuit_breaker_threshold: default_circuit_breaker_threshold(),
            circuit_breaker_cooldown_secs: default_circuit_breaker_cooldown(),
        }
    }
}

fn default_circuit_breaker_threshold() -> u32 { 3 }
fn default_circuit_breaker_cooldown() -> u64 { 30 }

/// 单个 Provider 的配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// API Key（仅从 .env 读取，永不写入或读取自 config.toml）
    #[serde(skip)]
    pub api_key: String,
    /// API 基础 URL（不配置则根据 provider 名称使用默认值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 模型名称（可选，默认使用 agents.defaults.model）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// API 协议类型：openai（默认）、anthropic、gemini、ollama
    #[serde(default = "default_api_type")]
    pub api_type: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: None,
            model: None,
            api_type: default_api_type(),
        }
    }
}

fn default_api_type() -> String { "openai".into() }

/// API 相关配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// 模型名称
    #[serde(default = "default_model")]
    pub model: String,

    /// API 基础 URL
    #[serde(default = "default_base_url")]
    pub base_url: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            base_url: default_base_url(),
        }
    }
}

/// 请求相关配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestConfig {
    /// 请求超时（秒）
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// 最大重试次数
    #[serde(default = "default_retries")]
    pub max_retries: u32,
}

impl Default for RequestConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_timeout(),
            max_retries: default_retries(),
        }
    }
}

/// 记忆与笔记配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// MEMORY.md 最大字符数（超出后删除旧条目）
    #[serde(default = "default_memory_md_chars")]
    pub max_memory_md_chars: usize,
    /// USER.md 最大字符数
    #[serde(default = "default_user_md_chars")]
    pub max_user_md_chars: usize,
    /// 是否启用用户画像记忆文件（USER.md），默认开启
    #[serde(default = "default_user_profile_enabled")]
    pub user_profile_enabled: bool,
}

fn default_user_profile_enabled() -> bool { true }

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_memory_md_chars: default_memory_md_chars(),
            max_user_md_chars: default_user_md_chars(),
            user_profile_enabled: true,
        }
    }
}

fn default_memory_md_chars() -> usize { 2200 }
fn default_user_md_chars() -> usize { 1375 }

/// 调试配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugConfig {
    /// 是否启用调试追踪（默认关闭）
    #[serde(default)]
    pub enabled: bool,
    /// 调试环缓冲区大小
    #[serde(default = "default_debug_buffer")]
    pub buffer_size: usize,
}

impl Default for DebugConfig {
    fn default() -> Self {
        Self { enabled: false, buffer_size: default_debug_buffer() }
    }
}

fn default_debug_buffer() -> usize { 500 }

/// 显示与截断配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// 工具结果最大字符数（超出后截断）
    #[serde(default = "default_tool_result_max_chars")]
    pub tool_result_max_chars: usize,
    /// read_pdf 预览最大字符数
    #[serde(default = "default_read_pdf_max_chars")]
    pub read_pdf_max_chars: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            tool_result_max_chars: default_tool_result_max_chars(),
            read_pdf_max_chars: default_read_pdf_max_chars(),
        }
    }
}

fn default_tool_result_max_chars() -> usize { 15000 }
fn default_read_pdf_max_chars() -> usize { 30000 }

/// Agent 行为配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent Loop 最大轮次（工具调用次数）
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,

    /// 上下文压缩阈值比例（0.0~1.0，默认 0.8 即 80%）
    #[serde(default = "default_compression_ratio")]
    pub compression_ratio: f64,

    /// 自动技能提炼间隔（工具调用次数，0=禁用，默认 15）
    #[serde(default = "default_creation_nudge_interval")]
    pub creation_nudge_interval: u32,

    /// 自动记忆提炼间隔（对话轮次，0=禁用，默认 10）
    #[serde(default = "default_memory_nudge_interval")]
    pub memory_nudge_interval: u32,

    /// 默认 Provider 名称（如 "deepseek"、"openai"），空则自动推断
    #[serde(default)]
    pub default_provider: String,

    /// 安全工作目录 — Agent 文件操作限制在此目录树下（空=不限制）
    #[serde(default)]
    pub workspace: String,
    /// 允许的命令前缀白名单（如 ["ls", "cat", "git", "cargo"]，空=不限制）
    #[serde(default)]
    pub command_allowed_prefixes: Vec<String>,
    /// 是否要求用户确认非白名单命令（默认 true）
    #[serde(default = "default_command_require_confirm")]
    pub command_require_confirm: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_rounds: default_max_rounds(),
            compression_ratio: default_compression_ratio(),
            creation_nudge_interval: default_creation_nudge_interval(),
            memory_nudge_interval: default_memory_nudge_interval(),
            default_provider: String::new(),
            workspace: String::new(),
            command_allowed_prefixes: Vec::new(),
            command_require_confirm: true,
        }
    }
}

fn default_compression_ratio() -> f64 { 0.8 }
fn default_creation_nudge_interval() -> u32 { 15 }
fn default_memory_nudge_interval() -> u32 { 10 }
fn default_command_require_confirm() -> bool { true }

// ---- 默认值 ----

fn default_model() -> String {
    "deepseek-v4-flash".into()
}

fn default_base_url() -> String {
    "https://api.deepseek.com".into()
}

fn default_timeout() -> u64 {
    60
}

fn default_retries() -> u32 {
    3
}

fn default_max_rounds() -> u32 {
    50
}

impl Default for Config {
    fn default() -> Self {
        let mut providers = std::collections::HashMap::new();
        providers.insert("deepseek".into(), ProviderConfig::default());
        Self {
            api_key: String::new(),
            api: ApiConfig::default(),
            providers,
            request: RequestConfig::default(),
            memory: MemoryConfig::default(),
            debug: DebugConfig::default(),
            display: DisplayConfig::default(),
            agent: AgentConfig::default(),
            provider_pool: ProviderPoolConfig::default(),
            channels: ChannelsConfig::default(),
            gateway: GatewayConfig::default(),
            scheduler: SchedulerConfig::default(),
            wasm: WasmPluginConfig::default(),
            mcp: McpConfig::default(),
            search: SearchConfig::default(),
            proxy: ProxyConfig::default(),
        }
    }
}

impl Config {
    /// 从指定路径加载配置（config.toml + .env）
    ///
    /// 加载顺序：
    /// 1. 读取 `config.toml`（非敏感配置）
    /// 2. 读取同目录下的 `.env`（API Key）
    pub fn load(config_path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let config_dir = config_path.as_ref().parent().unwrap_or(Path::new("."));

        // 1. 加载 config.toml
        let mut cfg = match std::fs::read_to_string(config_path.as_ref()) {
            Ok(content) => toml::from_str(&content).map_err(ConfigError::Parse)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(ConfigError::Io(e)),
        };

        // 2. 从 .env 加载所有 Provider 的 API Key
        //    格式: {PROVIDER_NAME_UPPER}_API_KEY=sk-xxx
        //    如: DEEPSEEK_API_KEY, OPENAI_API_KEY, SILICONFLOW_API_KEY
        let env_path = config_dir.join(ENV_FILE_NAME);
        if let Ok(content) = std::fs::read_to_string(&env_path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim().to_string();
                    // 兼容旧版 DEEPSEEK_API_KEY → Config.api_key
                    if key == "DEEPSEEK_API_KEY" {
                        cfg.api_key = value.clone();
                        // 同步到 providers.deepseek
                        let ds = cfg.providers.entry("deepseek".into()).or_default();
                        if ds.api_key.is_empty() {
                            ds.api_key = value;
                        }
                        continue;
                    }
                    // 通用格式: {PROVIDER}_API_KEY → providers.{provider_lower}.api_key
                    if let Some(rest) = key.strip_suffix("_API_KEY") {
                        if !rest.is_empty() {
                            let provider_name = rest.to_lowercase();
                            let entry = cfg.providers.entry(provider_name).or_default();
                            entry.api_key = value.clone();
                        }
                    }
                    // SERPER_API_KEY → search.serper_api_key
                    if key == "SERPER_API_KEY" {
                        cfg.search.serper_api_key = value.clone();
                    }
                    // SEARCH_PROXY → search.proxy
                    if key == "SEARCH_PROXY" {
                        cfg.search.proxy = Some(value.clone());
                    }
                    // TELEGRAM_BOT_TOKEN → channels.telegram.bot_token
                    if key == "TELEGRAM_BOT_TOKEN" {
                        cfg.channels.telegram.bot_token = value.clone();
                    }
                }
            }
        }

        // 3. 向后兼容：如果 [providers] 为空但 [api] 有配置，自动迁移
        if cfg.providers.is_empty() && !cfg.api.model.is_empty() {
            let mut deepseek = ProviderConfig::default();
            if !cfg.api_key.is_empty() {
                deepseek.api_key = cfg.api_key.clone();
            }
            if cfg.api.base_url != default_base_url() {
                deepseek.base_url = Some(cfg.api.base_url.clone());
            }
            deepseek.model = Some(cfg.api.model.clone());
            cfg.providers.insert("deepseek".into(), deepseek);
        } else if cfg.providers.is_empty() {
            // 默认初始化 deepseek
            cfg.providers.insert("deepseek".into(), ProviderConfig::default());
        }

        // 3c. 向后兼容：迁移 channels.wechat.proxy → proxy
        if let Some(ref wechat_proxy) = cfg.channels.wechat.proxy {
            if !wechat_proxy.is_empty() {
                if cfg.proxy.url.is_none() {
                    cfg.proxy.url = Some(wechat_proxy.clone());
                }
                cfg.proxy.rules.insert("wechat".to_string(), true);
            }
        }

        // 4. 环境变量覆盖 base_url（优先级：环境变量 > config.toml > 默认值）
        if let Ok(val) = std::env::var(ENV_BASE_URL) {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                cfg.api.base_url = trimmed.clone();
                // 同步到 providers 中的 deepseek
                if let Some(ds) = cfg.providers.get_mut("deepseek") {
                    ds.base_url = Some(trimmed);
                }
            }
        }

        // 5. 将 .env 的 api_key 同步到 providers.deepseek（如果没设）
        if !cfg.api_key.is_empty() {
            if let Some(ds) = cfg.providers.get_mut("deepseek") {
                if ds.api_key.is_empty() {
                    ds.api_key = cfg.api_key.clone();
                }
            }
        }

        Ok(cfg)
    }

    // 保存非敏感配置到 `config.toml`（不会写入 API Key）
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        std::fs::write(path.as_ref(), content).map_err(ConfigError::Io)
    }

    /// 保存带完整注释的配置文件（保留注释，只替换用户修改过的值）
    pub fn save_annotated(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let template = Self::generate_annotated_template();
        let compact = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        let merged = merge_template_with_config(&template, &compact);
        std::fs::write(path.as_ref(), merged).map_err(ConfigError::Io)
    }

    /// 保存所有 Provider 的 API Key 到 `.env` 文件
    pub fn save_api_key(&self, config_path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let config_dir = config_path.as_ref().parent().unwrap_or(Path::new("."));
        let env_path = config_dir.join(ENV_FILE_NAME);
        let mut content = String::new();
        content.push_str("# RHermes Provider API Keys\n");
        // 兼容旧版：deepseek api key
        if !self.api_key.is_empty() {
            content.push_str(&format!("{}={}\n", ENV_KEY_NAME, self.api_key));
        }
        // 新版：每个 provider 一条
        for (name, provider) in &self.providers {
            if !provider.api_key.is_empty() {
                let env_var = format!("{}_API_KEY", name.to_uppercase());
                content.push_str(&format!("{env_var}={}\n", provider.api_key));
            }
        }
        std::fs::write(&env_path, &content).map_err(ConfigError::Io)?;

        // 安全: 设置 .env 文件权限为仅 owner 可读写（Unix）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600)) {
                tracing::warn!("⚠ 无法设置 .env 文件权限: {e}");
            }
        }
        // Windows: 文件 ACL 由 NTFS 管理

        Ok(())
    }

    /// 是否已配置（有 API Key）
    pub fn is_configured(&self) -> bool {
        // 检查所有 provider 是否有 api_key
        for p in self.providers.values() {
            if !p.api_key.is_empty() {
                return true;
            }
        }
        !self.api_key.is_empty()
    }

    /// 生成带完整注释的 config.toml 模板
    pub fn generate_template(path: impl AsRef<Path>) -> Result<(), std::io::Error> {
        let template = Self::generate_annotated_template();
        std::fs::write(path, template)
    }

    /// 生成带完整注释的 config.toml 模板字符串
    ///
    /// 所有值来自 Config::default()，注释硬编码为中文。
    /// 空值字段用注释形式展示（# key = value）。
    pub fn generate_annotated_template() -> String {
        let d = Config::default();
        let mut s = String::new();

        // 文件头
        s.push_str("# RHermes 配置文件\n");
        s.push_str("#\n");
        s.push_str("# 首次使用: rhermes config-init 生成此文件\n");
        s.push_str("# API Key 等敏感信息请放在同目录的 .env 文件中\n");
        s.push_str("#\n\n");

        // ── API 配置 ──
        s.push_str("# ── API 配置（向后兼容，等价于 [providers.deepseek]）──\n");
        s.push_str("[api]\n");
        s.push_str(&format!("# 模型名称\nmodel = {:?}\n", d.api.model));
        s.push_str(&format!("# API 基础 URL（DeepSeek: https://api.deepseek.com, 智谱: https://open.bigmodel.cn/api/paas/v4, OpenAI: https://api.openai.com/v1）\nbase_url = {:?}\n\n", d.api.base_url));

        // ── 多 Provider 配置 ──
        s.push_str("# ── 多 Provider 配置 ──\n");
        s.push_str("# 每个 Provider 对应一个 AI 服务商，格式: [providers.{name}]\n");
        s.push_str("# API Key 从 .env 读取: {NAME}_API_KEY=xxx\n");
        s.push_str("# 支持的 provider: deepseek, openai, zhipu, siliconflow, moonshot, ollama 等\n");
        s.push_str("[providers.deepseek]\n");
        s.push_str("# API 协议类型: openai（默认）/ anthropic / gemini / ollama\n");
        s.push_str(&format!("api_type = {:?}\n", d.providers.get("deepseek").map(|p| &p.api_type).unwrap_or(&default_api_type())));
        s.push_str("# 可选覆盖 base_url 和 model:\n");
        s.push_str("# base_url = \"https://api.deepseek.com\"\n");
        s.push_str("# model = \"deepseek-v4-flash\"\n\n");

        s.push_str("# 示例：更多 Provider 配置（取消注释启用）\n");
        s.push_str("# [providers.zhipu]\n");
        s.push_str("# api_type = \"openai\"\n");
        s.push_str("# base_url = \"https://open.bigmodel.cn/api/paas/v4\"\n");
        s.push_str("# model = \"glm-5.1\"\n\n");
        s.push_str("# [providers.openai]\n");
        s.push_str("# api_type = \"openai\"\n");
        s.push_str("# base_url = \"https://api.openai.com/v1\"\n");
        s.push_str("# model = \"gpt-4o\"\n\n");

        // ── 请求配置 ──
        s.push_str("# ── 请求配置 ──\n");
        s.push_str("[request]\n");
        s.push_str("# API 请求超时（秒）\n");
        s.push_str(&format!("timeout_secs = {}\n", d.request.timeout_secs));
        s.push_str("# 请求失败最大重试次数\n");
        s.push_str(&format!("max_retries = {}\n\n", d.request.max_retries));

        // ── 记忆配置 ──
        s.push_str("# ── 记忆与笔记配置 ──\n");
        s.push_str("[memory]\n");
        s.push_str("# MEMORY.md 最大字符数（超出后自动删除旧条目）\n");
        s.push_str(&format!("max_memory_md_chars = {}\n", d.memory.max_memory_md_chars));
        s.push_str("# USER.md 最大字符数\n");
        s.push_str(&format!("max_user_md_chars = {}\n", d.memory.max_user_md_chars));
        s.push_str("# 是否启用用户画像文件（USER.md）\n");
        s.push_str(&format!("user_profile_enabled = {}\n\n", d.memory.user_profile_enabled));

        // ── 调试配置 ──
        s.push_str("# ── 调试配置 ──\n");
        s.push_str("[debug]\n");
        s.push_str("# 是否启用调试追踪\n");
        s.push_str(&format!("enabled = {}\n", d.debug.enabled));
        s.push_str("# 调试环缓冲区大小（条数）\n");
        s.push_str(&format!("buffer_size = {}\n\n", d.debug.buffer_size));

        // ── 显示配置 ──
        s.push_str("# ── 显示与截断配置 ──\n");
        s.push_str("[display]\n");
        s.push_str("# 工具返回结果最大字符数（超出后截断）\n");
        s.push_str(&format!("tool_result_max_chars = {}\n", d.display.tool_result_max_chars));
        s.push_str("# read_pdf 预览最大字符数\n");
        s.push_str(&format!("read_pdf_max_chars = {}\n\n", d.display.read_pdf_max_chars));

        // ── Agent 配置 ──
        s.push_str("# ── Agent 行为配置 ──\n");
        s.push_str("[agent]\n");
        s.push_str("# Agent Loop 最大轮次（工具调用次数上限）\n");
        s.push_str(&format!("max_rounds = {}\n", d.agent.max_rounds));
        s.push_str("# 上下文压缩阈值比例（0.0~1.0，达到后触发压缩）\n");
        s.push_str(&format!("compression_ratio = {}\n", d.agent.compression_ratio));
        s.push_str("# 自动技能提炼间隔（工具调用次数，0=禁用）\n");
        s.push_str(&format!("creation_nudge_interval = {}\n", d.agent.creation_nudge_interval));
        s.push_str("# 自动记忆提炼间隔（对话轮次，0=禁用）\n");
        s.push_str(&format!("memory_nudge_interval = {}\n", d.agent.memory_nudge_interval));
        s.push_str("# 默认 Provider 名称（如 \"deepseek\" / \"zhipu\"，空则根据模型名自动推断）\n");
        if d.agent.default_provider.is_empty() {
            s.push_str("# default_provider = \"\"\n");
        } else {
            s.push_str(&format!("default_provider = {:?}\n", d.agent.default_provider));
        }
        s.push_str("# 安全工作目录（Agent 文件操作限制在此目录下，空=不限制）\n");
        if d.agent.workspace.is_empty() {
            s.push_str("# workspace = \"\"\n");
        } else {
            s.push_str(&format!("workspace = {:?}\n", d.agent.workspace));
        }
        if d.agent.command_allowed_prefixes.is_empty() {
            s.push_str("# 命令白名单（如 [\"ls\", \"cat\", \"git\", \"cargo\"]，空=不限制）\n");
            s.push_str("# command_allowed_prefixes = [\"ls\", \"cat\", \"git\"]\n");
        } else {
            let prefixes: String = d.agent.command_allowed_prefixes.iter()
                .map(|p| format!("\"{}\"", p))
                .collect::<Vec<_>>()
                .join(", ");
            s.push_str(&format!("command_allowed_prefixes = [{}]\n", prefixes));
        }
        s.push_str(&format!("command_require_confirm = {}\n", d.agent.command_require_confirm));
        s.push('\n');

        // ── Provider Pool ──
        s.push_str("# ── Provider Pool 配置（熔断器）──\n");
        s.push_str("[provider_pool]\n");
        s.push_str("# 熔断器阈值：连续失败 N 次后断开该 Provider\n");
        s.push_str(&format!("circuit_breaker_threshold = {}\n", d.provider_pool.circuit_breaker_threshold));
        s.push_str("# 熔断器冷却时间（秒）\n");
        s.push_str(&format!("circuit_breaker_cooldown_secs = {}\n\n", d.provider_pool.circuit_breaker_cooldown_secs));

        // ── 通道配置 ──
        s.push_str("# ── 消息通道配置 ──\n\n");

        // wecom
        s.push_str("# 企业微信 Bot 通道\n[channels.wecom]\n");
        s.push_str(&format!("enabled = {}\n", d.channels.wecom.enabled));
        s.push_str("# Webhook URL（发送消息用）\n# webhook_url = \"\"\n");
        s.push_str("# 企业 ID（接收消息用）\n# corp_id = \"\"\n");
        s.push_str("# 应用 Agent ID（接收消息用）\n# agent_id = \"\"\n");
        s.push_str("# Webhook 关键词\n# webhook_key = \"\"\n");
        s.push_str("# 允许的发送者列表（空=全部允许）\n# allow_from = []\n");
        s.push_str(&format!("poll_interval_secs = {}\n\n", d.channels.wecom.poll_interval_secs));

        // wechat
        s.push_str("# 微信个号 iLink Bot 通道\n[channels.wechat]\n");
        s.push_str(&format!("enabled = {}\n", d.channels.wechat.enabled));
        s.push_str("# 代理地址（可选，支持 http:// 和 socks5://）\n# proxy = \"socks5://127.0.0.1:1080\"\n");
        s.push_str(&format!("poll_interval_secs = {}\n", d.channels.wechat.poll_interval_secs));
        s.push_str("# Token 刷新后自动保存路径\n# token_path = \"\"\n\n");

        // telegram
        s.push_str("# Telegram Bot 通道\n[channels.telegram]\n");
        s.push_str(&format!("enabled = {}\n", d.channels.telegram.enabled));
        s.push_str("# Bot Token 请在 .env 中设置 TELEGRAM_BOT_TOKEN\n");
        s.push_str("# allowed_chats = []\n");
        s.push_str(&format!("poll_timeout_secs = {}\n\n", d.channels.telegram.poll_timeout_secs));

        // ── 代理 ──
        s.push_str("# ── 网络代理 ──\n[proxy]\n");
        s.push_str("# 代理模式: \"all\"（全部走代理）/ \"off\"（不走代理）/ \"auto\"（按 rules 决定）\n");
        let mode_str = match d.proxy.mode {
            crate::core::ProxyMode::All => "all",
            crate::core::ProxyMode::Off => "off",
            crate::core::ProxyMode::Auto => "auto",
        };
        s.push_str(&format!("mode = {:?}\n", mode_str));
        s.push_str("# 代理 URL（支持 http:// 和 socks5://），取消注释启用\n");
        s.push_str("# url = \"socks5://127.0.0.1:1080\"\n");
        s.push_str("# 不走代理的域名/IP（支持 *. 通配符，推荐配置：）\n");
        s.push_str("# no_proxy = [\n");
        s.push_str("#     \"api.deepseek.com\",\n");
        s.push_str("#     \"open.bigmodel.cn\",\n");
        s.push_str("#     \"*.weixin.qq.com\",\n");
        s.push_str("#     \"qyapi.weixin.qq.com\",\n");
        s.push_str("#     \"localhost\",\n");
        s.push_str("#     \"127.0.0.1\",\n");
        s.push_str("# ]\n\n");

        // proxy.rules
        s.push_str("# 功能开关（仅 mode=\"auto\" 时生效）\n[proxy.rules]\n");
        let default_rules = ["llm", "web_search", "web_fetch", "wechat", "wecom", "telegram", "command"];
        let default_values = [true, true, true, false, false, true, false];
        let rule_comments = [
            "# AI API 请求（DeepSeek/GLM 由 no_proxy 排除）",
            "# 搜索工具（DDG/Serper，国内不可达）",
            "# 网页抓取（国内域名由 no_proxy 排除）",
            "# 微信通道",
            "# 企业微信",
            "# Telegram 通道（国内不可达）",
            "# 子进程 HTTP_PROXY 环境变量注入",
        ];
        for (i, name) in default_rules.iter().enumerate() {
            s.push_str(&format!("{} = {}   {}\n", name, default_values[i], rule_comments[i]));
        }
        s.push('\n');

        // ── Gateway ──
        s.push_str("# ── Gateway 守护进程配置 ──\n[gateway]\n");
        s.push_str("# 是否启用 gateway 模式\n");
        s.push_str(&format!("enabled = {}\n", d.gateway.enabled));
        s.push_str("# 启动时自动启用的通道列表\n");
        let channels_str: Vec<String> = d.gateway.channels.iter().map(|c| format!("\"{}\"", c)).collect();
        s.push_str(&format!("channels = [{}]\n", channels_str.join(", ")));
        s.push_str(&format!("# PID 文件路径\npid_file = {:?}\n", d.gateway.pid_file));
        s.push_str(&format!("# 日志文件路径\nlog_file = {:?}\n\n", d.gateway.log_file));

        // ── Scheduler ──
        s.push_str("# ── 定时任务调度器 ──\n[scheduler]\n");
        s.push_str(&format!("enabled = {}\n", d.scheduler.enabled));
        s.push_str("# 最多同时执行的任务数\n");
        s.push_str(&format!("max_concurrent_tasks = {}\n", d.scheduler.max_concurrent_tasks));
        if d.scheduler.tasks.is_empty() {
            s.push_str("\n# 定时任务示例：\n");
            s.push_str("# [[scheduler.tasks]]\n");
            s.push_str("# name = \"morning_standup\"\n");
            s.push_str("# cron = \"0 9 * * 1-5\"\n");
            s.push_str("# prompt = \"梳理昨天的工作，列出今天的待办\"\n");
            s.push_str("# target = \"\"  # 可选 \"channel:chat_id\"\n");
            s.push_str("# enabled = true\n\n");
        } else {
            for task in &d.scheduler.tasks {
                s.push_str(&format!("\n[[scheduler.tasks]]\n"));
                s.push_str(&format!("name = {:?}\n", task.name));
                s.push_str(&format!("cron = {:?}\n", task.cron));
                s.push_str(&format!("prompt = {:?}\n", task.prompt));
                if !task.target.is_empty() {
                    s.push_str(&format!("target = {:?}\n", task.target));
                }
                s.push_str(&format!("enabled = {}\n", task.enabled));
            }
            s.push('\n');
        }

        // ── WASM ──
        s.push_str("# ── WASM 插件配置 ──\n[wasm]\n");
        s.push_str(&format!("enabled = {}\n", d.wasm.enabled));
        s.push_str(&format!("# 插件目录\nplugins_dir = {:?}\n", d.wasm.plugins_dir));
        s.push_str("# 单个插件执行超时（毫秒）\n");
        s.push_str(&format!("timeout_ms = {}\n", d.wasm.timeout_ms));
        s.push_str("# 允许的最大内存（字节）\n");
        s.push_str(&format!("max_memory = {}\n\n", d.wasm.max_memory));

        // ── MCP ──
        s.push_str("# ── MCP 客户端配置 ──\n[mcp]\n");
        s.push_str(&format!("enabled = {}\n", d.mcp.enabled));
        s.push_str("# Sampling 能力（允许 MCP Server 通过 Agent 发起 LLM 请求，默认关闭）\n");
        s.push_str(&format!("sampling_enabled = {}\n", d.mcp.sampling_enabled));
        s.push_str("# Sampling 请求的 max_tokens 上限\n");
        s.push_str(&format!("sampling_max_tokens = {}\n\n", d.mcp.sampling_max_tokens));

        s.push_str("# MCP Server 示例配置（取消注释启用）：\n");
        s.push_str("# [mcp.servers.fs]\n");
        s.push_str("# command = \"npx\"\n");
        s.push_str("# args = [\"-y\", \"@anthropic/mcp-server-filesystem\", \"/path/to/dir\"]\n\n");

        // ── 搜索 ──
        s.push_str("# ── 搜索引擎配置 ──\n[search]\n");
        s.push_str("# Serper API Key 请在 .env 中设置 SERPER_API_KEY\n");
        s.push_str(&format!("# 搜索超时（秒）\ntimeout_secs = {}\n", d.search.timeout_secs));
        s.push_str(&format!("# 搜索结果缓存大小（条数）\ncache_size = {}\n", d.search.cache_size));
        s.push_str(&format!("# 缓存 TTL（秒）\ncache_ttl_secs = {}\n\n", d.search.cache_ttl_secs));

        s
    }
}

/// 将用户配置的值合并到带注释的模板中
///
/// 逐行扫描模板，对每个 `key = value` 或 `# key = ` 行，
/// 在 compact_toml 中查找对应 key，找到且值不同则取消注释并替换。
fn merge_template_with_config(template: &str, compact_toml: &str) -> String {
    // 解析紧凑 TOML 为层级 map
    let compact_value: toml::Value = match toml::from_str(compact_toml) {
        Ok(v) => v,
        Err(_) => return template.to_string(), // fallback
    };

    let mut result = String::new();
    let mut current_section: Vec<String> = Vec::new();
    let mut in_commented_section = false;

    for line in template.lines() {
        let trimmed = line.trim();
        let leading = &line[..line.len() - line.trim_start().len()];

        // 检测 commented section 行 (如 # [providers.zhipu])
        if trimmed.starts_with("# [") && trimmed.ends_with(']') {
            in_commented_section = true;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // 检测 active section 行
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section_name = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            current_section = section_name.split('.').map(|s| s.trim().to_string()).collect();
            in_commented_section = false;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // 在注释示例 section 内，保持原样
        if in_commented_section {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // 检测 key = value 或 # key = value 行
        let is_commented = trimmed.starts_with("# ");
        let content = if is_commented { &trimmed[2..] } else { trimmed };

        if let Some(eq_pos) = content.find('=') {
            let key = content[..eq_pos].trim();

            // 跳过非配置行
            if key.starts_with('[') || key.starts_with('#') || key.starts_with("//") || key.starts_with('─') {
                result.push_str(line);
                result.push('\n');
                continue;
            }

            // 在 compact_toml 中查找该 key
            let compact_val = find_value_in_toml(&compact_value, &current_section, key);

            if let Some(val_str) = compact_val {
                // 取消注释并替换值
                let new_line = format!("{}{} = {}", leading, key, val_str);
                result.push_str(&new_line);
                result.push('\n');
            } else {
                // 值相同或未找到，保持原样
                result.push_str(line);
                result.push('\n');
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

/// 在 TOML Value 中按 section 路径查找 key 的值，返回值的字符串表示
fn find_value_in_toml(value: &toml::Value, section: &[String], key: &str) -> Option<String> {
    let mut current = value;

    // 遍历 section 路径
    for seg in section {
        match current {
            toml::Value::Table(table) => {
                current = table.get(seg)?;
            }
            _ => return None,
        }
    }

    // 在最终 table 中查找 key
    match current {
        toml::Value::Table(table) => {
            let val = table.get(key)?;
            // 用 toml 格式输出值
            Some(toml::to_string(&toml::Value::Table({
                let mut m = toml::map::Map::new();
                m.insert(key.to_string(), val.clone());
                m
            }))
            .ok()?
            .lines()
            .next()?
            .splitn(2, '=')
            .nth(1)?
            .trim()
            .to_string())
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Serialize(toml::ser::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误: {e}"),
            Self::Parse(e) => write!(f, "配置解析错误: {e}"),
            Self::Serialize(e) => write!(f, "配置序列化错误: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_config_default() {
        let cfg = Config::default();
        assert!(cfg.api_key.is_empty());
        assert_eq!(cfg.api.model, "deepseek-v4-flash");
        assert_eq!(cfg.api.base_url, "https://api.deepseek.com");
        assert_eq!(cfg.request.timeout_secs, 60);
        assert_eq!(cfg.request.max_retries, 3);
        assert!(!cfg.is_configured());
    }

    #[test]
    fn test_config_load_not_found() {
        let cfg = Config::load("/nonexistent/path/config.toml").unwrap();
        assert!(cfg.api_key.is_empty()); // 返回默认
        assert_eq!(cfg.api.model, "deepseek-v4-flash");
    }

    #[test]
    fn test_config_save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");
        let env_path = tmp.path().join(".env");

        let cfg = Config {
            api_key: "sk-test-key-12345".into(),
            api: ApiConfig {
                model: "deepseek-v4-pro".into(),
                ..Default::default()
            },
            ..Default::default()
        };

        // 保存 toml + .env
        cfg.save(&toml_path).unwrap();
        cfg.save_api_key(&toml_path).unwrap();

        assert!(toml_path.exists());
        assert!(env_path.exists());

        // 验证 .env 内容
        let env_content = fs::read_to_string(&env_path).unwrap();
        assert!(env_content.contains("sk-test-key-12345"));

        // 重新加载，验证 api_key 来自 .env
        let loaded = Config::load(&toml_path).unwrap();
        assert_eq!(loaded.api_key, "sk-test-key-12345");
        assert_eq!(loaded.api.model, "deepseek-v4-pro");
        assert!(loaded.is_configured());
    }

    #[test]
    fn test_config_toml_does_not_contain_api_key() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");

        let cfg = Config {
            api_key: "sk-secret-key".into(),
            ..Default::default()
        };

        cfg.save(&toml_path).unwrap();

        // toml 文件中不应该包含 api_key
        let content = fs::read_to_string(&toml_path).unwrap();
        assert!(!content.contains("api_key ="), "toml 不应包含 api_key 字段");
        assert!(!content.contains("sk-secret-key"), "toml 不应包含密钥原文");
    }

    #[test]
    fn test_env_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");
        let env_path = tmp.path().join(".env");

        // 创建一个 .env 文件
        fs::write(&env_path, "# RHermes config\nDEEPSEEK_API_KEY=sk-from-env\n").unwrap();

        // 创建空的 config.toml
        let cfg = Config::default();
        cfg.save(&toml_path).unwrap();

        let loaded = Config::load(&toml_path).unwrap();
        assert_eq!(loaded.api_key, "sk-from-env");
    }

    #[test]
    fn test_env_with_whitespace() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");
        let env_path = tmp.path().join(".env");

        fs::write(
            &env_path,
            "  DEEPSEEK_API_KEY = sk-trimmed-key  \n",
        )
        .unwrap();

        let cfg = Config::default();
        cfg.save(&toml_path).unwrap();

        let loaded = Config::load(&toml_path).unwrap();
        assert_eq!(loaded.api_key, "sk-trimmed-key");
    }

    #[test]
    fn test_no_env_file_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");

        let cfg = Config::default();
        cfg.save(&toml_path).unwrap();

        // 没有 .env 文件
        let loaded = Config::load(&toml_path).unwrap();
        assert!(loaded.api_key.is_empty());
    }

    #[test]
    fn test_config_roundtrip_no_api_key_in_toml() {
        let original = Config {
            api_key: "sk-test".into(),
            api: ApiConfig {
                model: "deepseek-v4-flash".into(),
                base_url: "https://custom.api.com".into(),
            },
            providers: {
                let mut p = std::collections::HashMap::new();
                p.insert("deepseek".into(), ProviderConfig {
                    api_key: "sk-test".into(),
                    base_url: Some("https://custom.api.com".into()),
                    model: Some("deepseek-v4-flash".into()),
                    ..Default::default()
                });
                p
            },
            request: RequestConfig {
                timeout_secs: 120,
                max_retries: 5,
            },
            memory: MemoryConfig {
                max_memory_md_chars: 2200,
                max_user_md_chars: 1375,
                user_profile_enabled: false,
            },
            debug: DebugConfig {
                enabled: false,
                buffer_size: 500,
            },
            display: DisplayConfig {
                tool_result_max_chars: 15000,
                read_pdf_max_chars: 30000,
            },
            agent: AgentConfig {
                max_rounds: 50,
                compression_ratio: 0.8,
                creation_nudge_interval: 15,
                memory_nudge_interval: 10,
                default_provider: String::new(),
                workspace: String::new(),
                command_allowed_prefixes: Vec::new(),
                command_require_confirm: true,
            },
            provider_pool: ProviderPoolConfig::default(),
            channels: ChannelsConfig::default(),
            gateway: GatewayConfig::default(),
            scheduler: SchedulerConfig::default(),
            wasm: WasmPluginConfig::default(),
            mcp: McpConfig::default(),
            search: SearchConfig::default(),
            proxy: ProxyConfig::default(),
        };

        let toml_str = toml::to_string_pretty(&original).unwrap();
        // 所有 api_key 字段都不应出现在 toml 中（因 #[serde(skip_serializing)]）
        assert!(!toml_str.contains("api_key ="), "api_key 不应出现在 toml 中");

        let restored: Config = toml::from_str(&toml_str).unwrap();
        // api_key 没有被序列化，所以会是默认值
        assert!(restored.api_key.is_empty());
        // providers 中的 api_key 同样不会被序列化
        assert!(restored.providers.get("deepseek").map(|p| p.api_key.as_str()).unwrap_or("").is_empty());
        assert_eq!(restored.api.model, "deepseek-v4-flash");
        assert_eq!(restored.api.base_url, "https://custom.api.com");
    }

    #[test]
    fn test_generate_annotated_template_contains_all_sections() {
        let template = Config::generate_annotated_template();
        let sections = ["[api]", "[request]", "[memory]", "[debug]", "[display]", "[agent]",
                        "[provider_pool]", "[channels.wecom]", "[channels.wechat]", "[channels.telegram]",
                        "[proxy]", "[proxy.rules]", "[gateway]", "[mcp]", "[search]"];
        for section in &sections {
            assert!(template.contains(section), "模板应包含 section: {section}");
        }
    }

    #[test]
    fn test_generate_annotated_template_has_comments() {
        let template = Config::generate_annotated_template();
        let comment_count = template.lines().filter(|l| l.trim().starts_with('#')).count();
        assert!(comment_count >= 30, "模板应有至少 30 个注释行，实际有 {comment_count}");
    }

    #[test]
    fn test_save_annotated_preserves_values() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");

        let cfg = Config {
            request: RequestConfig {
                timeout_secs: 120,
                max_retries: 5,
                ..Default::default()
            },
            ..Default::default()
        };

        cfg.save_annotated(&toml_path).unwrap();

        // 验证保存后的文件包含注释
        let content = std::fs::read_to_string(&toml_path).unwrap();
        assert!(content.contains("timeout_secs = 120"));
        assert!(content.contains("#"));
        assert!(content.contains("[request]"));

        // 重新加载，验证值不变
        let loaded = Config::load(&toml_path).unwrap();
        assert_eq!(loaded.request.timeout_secs, 120);
        assert_eq!(loaded.request.max_retries, 5);
    }
}
