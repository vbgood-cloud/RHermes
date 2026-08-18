//! Plugin 系统核心类型 — P28 统一插件抽象
//!
//! Plugin 是比 Tool 更高级的抽象：带描述符/来源/健康检查/热重载。
//! 四种适配器（Wasm / SkillMd / Native / Mcp）统一实现本 trait，
//! 由 PluginRouter 统一注册、发现、路由。

use serde::{Deserialize, Serialize};

/// 插件描述符，标识一个插件的名称、来源和能力
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDescriptor {
    /// 插件唯一名称
    pub name: String,
    /// 人类可读描述
    pub description: String,
    /// 插件来源类型
    pub source: PluginSource,
    /// 是否支持并行调用
    pub parallel_safe: bool,
    /// 插件版本（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// 插件来源类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginSource {
    /// Extism Wasm 沙盒插件
    Wasm { path: String },
    /// Rust 原生内置工具
    Native,
    /// MCP 远程工具
    Mcp { server: String },
    /// Markdown 技能（上下文注入）
    SkillMd { path: String },
}

impl std::fmt::Display for PluginSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wasm { path } => write!(f, "wasm:{}", short_path(path)),
            Self::Native => write!(f, "native"),
            Self::Mcp { server } => write!(f, "mcp:{}", server),
            Self::SkillMd { path } => write!(f, "skill_md:{}", short_path(path)),
        }
    }
}

fn short_path(path: &str) -> &str {
    // 只保留文件名部分，避免描述符过长
    path.rsplit('/').next().unwrap_or(path)
}

/// 插件执行的标准输出结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginOutput {
    pub success: bool,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PluginOutput {
    pub fn ok(output: String) -> Self {
        Self { success: true, output, error: None }
    }

    pub fn err(error: impl Into<String>) -> Self {
        Self { success: false, output: String::new(), error: Some(error.into()) }
    }
}

/// Plugin 系统统一错误
#[derive(Debug)]
pub enum PluginError {
    /// 配置/加载错误（registry.toml 解析失败、wasm 不存在等）
    Config(String),
    /// 插件未找到
    NotFound(String),
    /// 执行失败
    ExecutionFailed(String),
    /// 沙盒内部错误（Extism）
    Extism(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(m) => write!(f, "插件配置错误: {m}"),
            Self::NotFound(n) => write!(f, "插件未找到: {n}"),
            Self::ExecutionFailed(m) => write!(f, "插件执行失败: {m}"),
            Self::Extism(m) => write!(f, "Wasm 沙盒错误: {m}"),
        }
    }
}

impl std::error::Error for PluginError {}

/// registry.toml 单个插件条目
#[derive(Debug, Clone, Deserialize)]
pub struct PluginEntry {
    /// 插件类型：wasm | skill_md | skill
    #[serde(rename = "type")]
    pub plugin_type: String,
    /// 路径（相对 plugins/ 目录或绝对路径）
    pub path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 名称覆盖（可选，默认从文件提取）
    #[serde(default)]
    pub name_override: Option<String>,
    /// Wasm 网络白名单（wasm 类型专用）
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Wasm 文件白名单前缀（wasm 类型专用）
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// Wasm 命令执行（wasm 类型专用，默认 false）
    #[serde(default)]
    pub allow_exec: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// registry.toml 根结构
#[derive(Debug, Default, Deserialize)]
pub struct RegistryToml {
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
}

/// Wasm 沙盒全局配置（Plugin 系统视角）
#[derive(Debug, Clone)]
pub struct WasmSandboxConfig {
    pub max_memory: u64,
    pub timeout_ms: u64,
}

impl Default for WasmSandboxConfig {
    fn default() -> Self {
        Self { max_memory: 32 * 1024 * 1024, timeout_ms: 30_000 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_source_display() {
        let s = PluginSource::Wasm { path: "/home/u/data/plugins/echo.wasm".into() };
        assert_eq!(s.to_string(), "wasm:echo.wasm");
        let s = PluginSource::SkillMd { path: "skills/coding.md".into() };
        assert_eq!(s.to_string(), "skill_md:coding.md");
        assert_eq!(PluginSource::Native.to_string(), "native");
    }

    #[test]
    fn test_plugin_source_serde_roundtrip() {
        let s = PluginSource::Mcp { server: "github".into() };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"mcp\""));
        let back: PluginSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn test_plugin_output() {
        let ok = PluginOutput::ok("done".into());
        assert!(ok.success);
        let err = PluginOutput::err("boom".to_string());
        assert_eq!(err.error.as_deref(), Some("boom"));
    }

    #[test]
    fn test_registry_toml_parse() {
        let toml_str = r#"
[[plugins]]
type = "wasm"
path = "echo.wasm"
enabled = true
allowed_hosts = ["api.example.com"]

[[plugins]]
type = "skill_md"
path = "../skills/coding.md"
"#;
        let r: RegistryToml = toml::from_str(toml_str).unwrap();
        assert_eq!(r.plugins.len(), 2);
        assert_eq!(r.plugins[0].plugin_type, "wasm");
        assert_eq!(r.plugins[0].allowed_hosts, vec!["api.example.com"]);
        assert!(r.plugins[1].enabled); // 默认 true
    }

    #[test]
    fn test_registry_toml_empty() {
        let r: RegistryToml = toml::from_str("").unwrap();
        assert!(r.plugins.is_empty());
    }
}
