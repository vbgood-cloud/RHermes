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

use std::path::Path;

use serde::{Deserialize, Serialize};

// ---- 常量 ----

/// `.env` 文件中 API Key 的键名
const ENV_KEY_NAME: &str = "DEEPSEEK_API_KEY";

/// `.env` 文件名
const ENV_FILE_NAME: &str = ".env";

// ---------------------------------------------------------------------------
// 配置结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// DeepSeek API Key（仅从 .env 读取，不写入 config.toml）
    #[serde(default, skip_serializing)]
    pub api_key: String,

    /// API 配置
    #[serde(default)]
    pub api: ApiConfig,

    /// 请求配置
    #[serde(default)]
    pub request: RequestConfig,

    /// 记忆与笔记配置
    #[serde(default)]
    pub memory: MemoryConfig,

    /// 调试配置
    #[serde(default)]
    pub debug: DebugConfig,

    /// Agent 行为配置
    #[serde(default)]
    pub agent: AgentConfig,
}

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
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { max_memory_md_chars: default_memory_md_chars() }
    }
}

fn default_memory_md_chars() -> usize { 5000 }

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

/// Agent 行为配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent Loop 最大轮次（工具调用次数）
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,

    /// 上下文压缩阈值比例（0.0~1.0，默认 0.8 即 80%）
    #[serde(default = "default_compression_ratio")]
    pub compression_ratio: f64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_rounds: default_max_rounds(),
            compression_ratio: default_compression_ratio(),
        }
    }
}

fn default_compression_ratio() -> f64 { 0.8 }

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
        Self {
            api_key: String::new(),
            api: ApiConfig::default(),
            request: RequestConfig::default(),
            memory: MemoryConfig::default(),
            debug: DebugConfig::default(),
            agent: AgentConfig::default(),
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

        // 2. 从 .env 加载 API Key
        let env_path = config_dir.join(ENV_FILE_NAME);
        if let Ok(content) = std::fs::read_to_string(&env_path) {
            for line in content.lines() {
                let line = line.trim();
                // 跳过空行和注释
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                // 解析 KEY=VALUE
                if let Some((key, value)) = line.split_once('=') {
                    if key.trim() == ENV_KEY_NAME {
                        cfg.api_key = value.trim().to_string();
                        break;
                    }
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

    /// 保存 API Key 到 `.env` 文件
    pub fn save_api_key(&self, config_path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let config_dir = config_path.as_ref().parent().unwrap_or(Path::new("."));
        let env_path = config_dir.join(ENV_FILE_NAME);
        let content = format!("{}={}\n", ENV_KEY_NAME, self.api_key);
        std::fs::write(&env_path, content).map_err(ConfigError::Io)
    }

    /// 是否已配置（有 API Key）
    pub fn is_configured(&self) -> bool {
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
            request: RequestConfig {
                timeout_secs: 120,
                max_retries: 5,
            },
            memory: MemoryConfig {
                max_memory_md_chars: 5000,
            },
            debug: DebugConfig {
                enabled: false,
                buffer_size: 500,
            },
            agent: AgentConfig {
                max_rounds: 50,
                compression_ratio: 0.8,
            },
        };

        let toml_str = toml::to_string_pretty(&original).unwrap();
        // api_key 不应该出现在 toml 中
        assert!(!toml_str.contains("api_key"));

        let restored: Config = toml::from_str(&toml_str).unwrap();
        // api_key 没有被序列化，所以会是默认值
        assert!(restored.api_key.is_empty());
        assert_eq!(restored.api.model, "deepseek-v4-flash");
        assert_eq!(restored.api.base_url, "https://custom.api.com");
    }
}
