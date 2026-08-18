//! WASM 插件工具 — 基于 Extism 运行时
//!
//! 仿照 McpRemoteTool 模式：每个 .wasm 文件包装为一个 Tool trait 实现。
//! 插件必须导出 4 个函数：info_name / info_description / info_parameters / execute
//!
//! v0.7.0：接入 Host Functions 安全网关（wasm_host_functions.rs）
//! - 每个插件在 data_root/plugins/<name>.host.toml 声明权限
//! - 未声明权限的插件仅能纯计算 + host_log
//! - Manifest 强制 timeout_ms / max_memory（来自 WasmPluginConfig）

use std::path::Path;
use std::sync::Arc;

use extism::{Manifest, Plugin, Wasm};
use serde::Deserialize;
use serde_json::Value;

use super::wasm_host_functions::{build_host_functions, HostAccessConfig};
use crate::core::WasmPluginConfig;
use crate::tools::{ParamDef, Tool, ToolError};

/// 每插件的权限声明文件（与 .wasm 同目录同名）
/// 示例：
/// ```toml
/// allowed_hosts = ["api.example.com"]
/// allowed_paths = ["data/"]
/// allow_exec = false
/// ```
#[derive(Debug, Default, Deserialize)]
struct HostToml {
    #[serde(default)]
    allowed_hosts: Vec<String>,
    #[serde(default)]
    allowed_paths: Vec<String>,
    #[serde(default)]
    allow_exec: bool,
}

/// 从 .wasm 旁的 <name>.host.toml 读取权限声明（不存在 = 无权限）
fn load_access_config(wasm_path: &Path) -> HostAccessConfig {
    let stem = wasm_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let cfg_path = wasm_path.with_file_name(format!("{stem}.host.toml"));
    match std::fs::read_to_string(&cfg_path) {
        Ok(content) => match toml::from_str::<HostToml>(&content) {
            Ok(t) => HostAccessConfig::new(t.allowed_hosts, t.allowed_paths, t.allow_exec),
            Err(e) => {
                tracing::warn!("[WASM] {} 权限文件解析失败（按无权限处理）: {e}", cfg_path.display());
                HostAccessConfig::default()
            }
        },
        Err(_) => HostAccessConfig::default(), // 无声明 = 最小权限
    }
}

/// 构建带安全限制的 Manifest
fn build_manifest(wasm_bytes: &[u8], config: &WasmPluginConfig, access: &HostAccessConfig) -> Manifest {
    let pages = (config.max_memory / 65536).max(1) as u32; // bytes → pages，至少 1 页
    let mut manifest = Manifest::new([Wasm::data(wasm_bytes.to_vec())])
        .with_memory_max(pages)
        .with_timeout(std::time::Duration::from_millis(config.timeout_ms));
    // Extism 内置 HTTP 能力层：把 host 白名单同样应用到 manifest（双保险）
    if !access.allowed_hosts.is_empty() {
        let hosts: Vec<String> = access.allowed_hosts.iter().cloned().collect();
        manifest = manifest.with_allowed_hosts(hosts.into_iter());
    } else {
        manifest = manifest.disallow_all_hosts();
    }
    manifest
}

/// WASM 插件工具包装器
pub struct WasmPluginTool {
    name: String,
    description: String,
    parameters: Vec<ParamDef>,
    wasm_bytes: Vec<u8>,
    access: HostAccessConfig,
    config: WasmPluginConfig,
}

impl WasmPluginTool {
    /// 从 .wasm 文件加载并预提取元数据（权限来自 <name>.host.toml）
    pub fn load(path: &Path, config: &WasmPluginConfig) -> Result<Self, String> {
        Self::load_with_access(path, config, load_access_config(path))
    }

    /// 从 .wasm 文件加载，权限显式注入（Plugin 系统 registry.toml 用）
    pub fn load_with_access(
        path: &Path,
        config: &WasmPluginConfig,
        access: HostAccessConfig,
    ) -> Result<Self, String> {
        let wasm_bytes = std::fs::read(path)
            .map_err(|e| format!("读取 {path:?} 失败: {e}"))?;

        // 启动一次提取元数据
        let manifest = build_manifest(&wasm_bytes, config, &access);
        let mut plugin = Plugin::new(&manifest, build_host_functions(&access), true)
            .map_err(|e| format!("加载插件失败: {e}"))?;

        // 约定：插件导出 4 个无参字符串函数
        let name: String = plugin.call("info_name", "").unwrap_or_else(|_| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });

        let description: String = plugin
            .call("info_description", "")
            .unwrap_or_default();

        let params_json: String = plugin
            .call("info_parameters", "")
            .unwrap_or_else(|_| "[]".to_string());
        let parameters: Vec<ParamDef> =
            serde_json::from_str(&params_json).unwrap_or_default();

        let host_summary = if access.allowed_hosts.is_empty()
            && access.allowed_paths.is_empty()
            && !access.allow_exec
        {
            "最小权限(仅计算)".to_string()
        } else {
            format!(
                "hosts={:?} paths={:?} exec={}",
                access.allowed_hosts, access.allowed_paths, access.allow_exec
            )
        };
        tracing::info!("[WASM] 加载插件: {} → wasm_{} [{host_summary}]", path.display(), name);

        Ok(Self {
            name,
            description,
            parameters,
            wasm_bytes,
            access,
            config: config.clone(),
        })
    }
}

#[async_trait::async_trait]
impl Tool for WasmPluginTool {
    fn name(&self) -> String {
        format!("wasm_{}", self.name)
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ParamDef> {
        self.parameters.clone()
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let args_str = serde_json::to_string(&args)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let wasm_bytes = self.wasm_bytes.clone();
        let access = self.access.clone();
        let config = self.config.clone();

        // spawn_blocking：Extism Plugin 含阻塞 Wasm 调用 + 潜在阻塞 IO（host fn）
        let result = tokio::task::spawn_blocking(move || {
            let manifest = build_manifest(&wasm_bytes, &config, &access);
            let mut plugin = Plugin::new(&manifest, build_host_functions(&access), true)
                .map_err(|e| ToolError::ExecutionFailed(format!("WASM 创建实例失败: {e}")))?;

            plugin
                .call::<String, String>("execute", args_str)
                .map_err(|e| ToolError::ExecutionFailed(format!("WASM execute 失败: {e}")))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("WASM 任务 join 失败: {e}")))?;

        result
    }
}

/// 扫描插件目录，返回所有 WasmPluginTool
pub fn load_plugins(plugins_dir: &str, config: &WasmPluginConfig) -> Vec<Arc<dyn Tool>> {
    let dir = match std::fs::read_dir(plugins_dir) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("[WASM] 扫描插件目录 '{}' 失败: {e}", plugins_dir);
            return Vec::new();
        }
    };

    dir.filter_map(|entry| {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "wasm") {
            match WasmPluginTool::load(&path, config) {
                Ok(tool) => {
                    tracing::info!("[WASM] 注册工具: {}", tool.name());
                    Some(Arc::new(tool) as Arc<dyn Tool>)
                }
                Err(e) => {
                    tracing::warn!("[WASM] 加载 {} 失败: {e}", path.display());
                    None
                }
            }
        } else {
            None
        }
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_access_config_missing_file() {
        // 不存在的 host.toml → 最小权限
        let cfg = load_access_config(Path::new("/nonexistent/ghost.wasm"));
        assert!(cfg.allowed_hosts.is_empty());
        assert!(cfg.allowed_paths.is_empty());
        assert!(!cfg.allow_exec);
    }

    #[test]
    fn test_load_access_config_valid() {
        let dir = std::env::temp_dir();
        let wasm = dir.join("rhermes-test-access.wasm");
        std::fs::write(&wasm, b"fake").unwrap();
        let toml_path = dir.join("rhermes-test-access.host.toml");
        std::fs::write(
            &toml_path,
            "allowed_hosts = [\"api.example.com\"]\nallowed_paths = [\"/tmp\"]\nallow_exec = false\n",
        )
        .unwrap();

        let cfg = load_access_config(&wasm);
        assert!(cfg.is_host_allowed("api.example.com"));
        assert!(!cfg.is_host_allowed("evil.com"));
        assert!(cfg.allowed_paths.iter().any(|p| p.ends_with("tmp")));
        assert!(!cfg.allow_exec);

        let _ = std::fs::remove_file(&wasm);
        let _ = std::fs::remove_file(&toml_path);
    }

    #[test]
    fn test_load_access_config_invalid_toml() {
        let dir = std::env::temp_dir();
        let wasm = dir.join("rhermes-test-bad.wasm");
        std::fs::write(&wasm, b"fake").unwrap();
        let toml_path = dir.join("rhermes-test-bad.host.toml");
        std::fs::write(&toml_path, "not valid toml {{{{").unwrap();

        let cfg = load_access_config(&wasm);
        assert!(cfg.allowed_hosts.is_empty()); // 降级为最小权限

        let _ = std::fs::remove_file(&wasm);
        let _ = std::fs::remove_file(&toml_path);
    }
}
