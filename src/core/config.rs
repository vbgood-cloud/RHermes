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

    /// Gateway 守护进程配置
    #[serde(default)]
    pub gateway: GatewayConfig,
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
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            wecom: WeComConfig::default(),
            wechat: WeChatConfig::default(),
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
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_rounds: default_max_rounds(),
            compression_ratio: default_compression_ratio(),
            creation_nudge_interval: default_creation_nudge_interval(),
            memory_nudge_interval: default_memory_nudge_interval(),
            default_provider: String::new(),
        }
    }
}

fn default_compression_ratio() -> f64 { 0.8 }
fn default_creation_nudge_interval() -> u32 { 15 }
fn default_memory_nudge_interval() -> u32 { 10 }

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
                            entry.api_key = value;
                        }
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

    /// 保存非敏感配置到 `config.toml`（不会写入 API Key）
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        std::fs::write(path.as_ref(), content).map_err(ConfigError::Io)
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
        std::fs::write(&env_path, content).map_err(ConfigError::Io)
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
            },
            provider_pool: ProviderPoolConfig::default(),
            channels: ChannelsConfig::default(),
            gateway: GatewayConfig::default(),
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
}
