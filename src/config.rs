//! RHermes 配置模块
//!
//! 加载和保存 `config.toml` 配置文件。
//! 支持 DeepSeek API Key、模型选择、base_url 等核心配置。

use std::path::Path;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 配置结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// DeepSeek API Key
    pub api_key: String,

    /// 模型名称
    /// 可选: "deepseek-v4-flash" | "deepseek-v4-pro"
    #[serde(default = "default_model")]
    pub model: String,

    /// API 基础 URL
    #[serde(default = "default_base_url")]
    pub base_url: String,

    /// 请求超时（秒）
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// 最大重试次数
    #[serde(default = "default_retries")]
    pub max_retries: u32,
}

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

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: default_model(),
            base_url: default_base_url(),
            timeout_secs: default_timeout(),
            max_retries: default_retries(),
        }
    }
}

impl Config {
    /// 从指定路径加载配置
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let content = match std::fs::read_to_string(path.as_ref()) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 文件不存在时返回默认配置
                return Ok(Self::default());
            }
            Err(e) => return Err(ConfigError::Io(e)),
        };

        toml::from_str(&content).map_err(ConfigError::Parse)
    }

    /// 保存配置到指定路径
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        std::fs::write(path.as_ref(), content).map_err(ConfigError::Io)
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


    #[test]
    fn test_config_default() {
        let cfg = Config::default();
        assert!(cfg.api_key.is_empty());
        assert_eq!(cfg.model, "deepseek-v4-flash");
        assert_eq!(cfg.base_url, "https://api.deepseek.com");
        assert_eq!(cfg.timeout_secs, 60);
        assert_eq!(cfg.max_retries, 3);
        assert!(!cfg.is_configured());
    }

    #[test]
    fn test_config_load_not_found() {
        let cfg = Config::load("/nonexistent/path/config.toml").unwrap();
        assert!(cfg.api_key.is_empty()); // 返回默认
    }

    #[test]
    fn test_config_save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");

        let cfg = Config {
            api_key: "sk-test-key-12345".into(),
            model: "deepseek-v4-pro".into(),
            ..Default::default()
        };

        cfg.save(&path).unwrap();
        assert!(path.exists());

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.api_key, "sk-test-key-12345");
        assert_eq!(loaded.model, "deepseek-v4-pro");
        assert!(loaded.is_configured());
    }

    #[test]
    fn test_config_roundtrip() {
        // 验证 toml 序列化/反序列化完整的配置
        let original = Config {
            api_key: "sk-test".into(),
            model: "deepseek-v4-flash".into(),
            base_url: "https://custom.api.com".into(),
            timeout_secs: 120,
            max_retries: 5,
        };

        let toml_str = toml::to_string_pretty(&original).unwrap();
        let restored: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(restored.api_key, "sk-test");
        assert_eq!(restored.model, "deepseek-v4-flash");
        assert_eq!(restored.base_url, "https://custom.api.com");
        assert_eq!(restored.timeout_secs, 120);
        assert_eq!(restored.max_retries, 5);
    }
}
