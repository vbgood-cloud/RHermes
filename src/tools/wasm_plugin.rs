//! WASM 插件工具 — 基于 Extism 运行时
//!
//! 仿照 McpRemoteTool 模式：每个 .wasm 文件包装为一个 Tool trait 实现。
//! 插件必须导出 4 个函数：info_name / info_description / info_parameters / execute

use std::path::Path;
use std::sync::Arc;

use extism::{Manifest, Plugin, Wasm};
use serde_json::Value;

use crate::core::WasmPluginConfig;
use crate::tools::{ParamDef, Tool, ToolError};

/// WASM 插件工具包装器
pub struct WasmPluginTool {
    name: String,
    description: String,
    parameters: Vec<ParamDef>,
    wasm_bytes: Vec<u8>,
}

impl WasmPluginTool {
    /// 从 .wasm 文件加载并预提取元数据
    pub fn load(path: &Path, _config: &WasmPluginConfig) -> Result<Self, String> {
        let wasm_bytes = std::fs::read(path)
            .map_err(|e| format!("读取 {path:?} 失败: {e}"))?;

        // 启动一次提取元数据
        let manifest = Manifest::new([Wasm::data(wasm_bytes.clone())]);
        let mut plugin = Plugin::new(&manifest, [], true)
            .map_err(|e| format!("加载插件失败: {e}"))?;

        // 约定：插件导出 4 个无参字符串函数
        let name: String = plugin.call("info_name", "")
            .unwrap_or_else(|_| {
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

        tracing::info!("[WASM] 加载插件: {} → wasm_{}", path.display(), name);

        Ok(Self {
            name,
            description,
            parameters,
            wasm_bytes,
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

        // Extism Plugin 不实现 Send，直接在 async 上下文中创建并调用
        let manifest = Manifest::new([Wasm::data(wasm_bytes)]);
        let mut plugin = Plugin::new(&manifest, [], true)
            .map_err(|e| ToolError::ExecutionFailed(format!("WASM 创建实例失败: {e}")))?;

        let result = plugin
            .call::<String, String>("execute", args_str)
            .map_err(|e| ToolError::ExecutionFailed(format!("WASM execute 失败: {e}")))?;

        Ok(result)
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
