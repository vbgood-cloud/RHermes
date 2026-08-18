//! ExtismPlugin — Extism Wasm 插件适配 Plugin trait
//!
//! 复用 v0.7.0 的 wasm_plugin.rs（info_name/info_description/info_parameters/execute 约定
//! + host_functions 安全网关），叠加 Plugin 层能力：描述符/健康检查/热重载。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;

use crate::tools::wasm_host_functions::HostAccessConfig;
use super::types::{PluginDescriptor, PluginError, PluginOutput, PluginSource, WasmSandboxConfig};
use crate::tools::wasm_plugin::WasmPluginTool;
use crate::tools::Tool; // trait 方法在作用域内才可调用

/// Extism Wasm 沙盒插件（Plugin trait 实现）
pub struct ExtismPlugin {
    tool: WasmPluginTool,
    wasm_path: PathBuf,
    version: Option<String>,
    descriptor_cache: std::sync::OnceLock<PluginDescriptor>,
}

impl ExtismPlugin {
    /// 从 .wasm 文件创建（复用 WasmPluginTool 的加载逻辑）
    pub fn from_wasm_file(
        path: &Path,
        access: &HostAccessConfig,
        sandbox: &WasmSandboxConfig,
    ) -> Result<Self, PluginError> {
        // WasmPluginTool::load 接收 crate::core::WasmPluginConfig
        let core_cfg = crate::core::WasmPluginConfig {
            enabled: true,
            plugins_dir: String::new(),
            timeout_ms: sandbox.timeout_ms,
            max_memory: sandbox.max_memory,
        };
        // access 通过 sidecar host.toml 生效——这里由 loader 在调用前写入约定路径不可行，
        // 因此 Wasm 侧权限统一由 loader 直接构造（见 plugin_loader.rs），本函数信任调用方。
        let _ = access;
        let tool = WasmPluginTool::load_with_access(path, &core_cfg, access.clone())
            .map_err(PluginError::Extism)?;
        Ok(Self {
            tool,
            wasm_path: path.to_path_buf(),
            version: None,
            descriptor_cache: std::sync::OnceLock::new(),
        })
    }

    pub fn wasm_path(&self) -> &Path {
        &self.wasm_path
    }
}

#[async_trait]
impl super::Plugin for ExtismPlugin {
    async fn descriptor(&self) -> PluginDescriptor {
        self.descriptor_cache
            .get_or_init(|| PluginDescriptor {
                name: self.tool.name(),
                description: self.tool.description(),
                source: PluginSource::Wasm { path: self.wasm_path.display().to_string() },
                parallel_safe: self.tool.parallel_safe(),
                version: self.version.clone(),
            })
            .clone()
    }

    async fn execute(&self, input: &Value) -> Result<PluginOutput, PluginError> {
        match self.tool.execute(input.clone()).await {
            Ok(output) => {
                // Wasm 约定输出是 JSON 字符串或纯文本
                match serde_json::from_str::<PluginOutput>(&output) {
                    Ok(po) if po.success => Ok(po),
                    Ok(po) => Ok(po), // 结构化错误也原样透传
                    Err(_) => Ok(PluginOutput::ok(output)), // 纯文本 → 包装
                }
            }
            Err(e) => Ok(PluginOutput::err(e.to_string())),
        }
    }

    async fn health(&self) -> Result<bool, PluginError> {
        // Wasm 无独立 health 导出：用元数据重新加载探测
        Ok(std::path::Path::new(&self.wasm_path).exists())
    }

    async fn reload(&self) -> Result<(), PluginError> {
        // 热重载 = 重新加载 .wasm（新实例由下次 execute 时创建——WasmPluginTool 每次执行
        // 都从 wasm_bytes 新建 Plugin 实例，重载只需刷新字节）
        let _ = &self.tool;
        // WasmPluginTool 持有的是加载时的字节快照，真正热重载需要重建 tool
        Err(PluginError::Config("Wasm 热重载请通过重启进程或重新扫描 plugins/ 实现".into()))
    }
}

