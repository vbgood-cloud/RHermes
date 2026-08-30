//! P28 统一插件系统 — Plugin trait + PluginRouter
//!
//! 四种适配器统一抽象：
//!   - ExtismPlugin   (.wasm 沙盒，经 WasmPluginTool + host functions)
//!   - SkillMdPlugin  (SKILL.md 上下文注入)
//!   - Native/Mcp     (现有 Tool 直挂 ToolRegistry，不重复包装——见 mod.rs 底部说明)
//!
//! PluginRouter：注册/发现/路由/健康检查。通过 OnceLock 全局共享，
//! run_plugin 工具（builtin.rs）从全局取 router 调用插件。

mod extism_adapter;
mod skill_md_adapter;
mod types;

pub use extism_adapter::ExtismPlugin;
pub use skill_md_adapter::SkillMdPlugin;
pub use types::{PluginDescriptor, PluginEntry, PluginError, PluginOutput, PluginSource, RegistryToml, WasmSandboxConfig};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::tools::wasm_host_functions::HostAccessConfig;

/// 统一插件特征——比 Tool 更高级的抽象（描述符 + 健康检查 + 热重载）
#[async_trait]
pub trait Plugin: Send + Sync {
    /// 插件描述符
    async fn descriptor(&self) -> PluginDescriptor;
    /// 执行插件
    async fn execute(&self, input: &Value) -> Result<PluginOutput, PluginError>;
    /// 健康检查
    async fn health(&self) -> Result<bool, PluginError>;
    /// 热重载（可选能力，不支持返回 Err）
    async fn reload(&self) -> Result<(), PluginError> {
        Err(PluginError::Config("此插件不支持热重载".into()))
    }
}

/// 插件路由器：name → Arc<dyn Plugin>
#[derive(Clone, Default)]
pub struct PluginRouter {
    plugins: Arc<RwLock<HashMap<String, Arc<dyn Plugin>>>>,
}

impl PluginRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册插件（重名覆盖）
    pub async fn register(&self, plugin: Arc<dyn Plugin>) {
        let name = plugin.descriptor().await.name;
        self.plugins.write().await.insert(name, plugin);
    }

    /// 按名称调用插件
    pub async fn call(&self, name: &str, input: &Value) -> Result<PluginOutput, PluginError> {
        let plugins = self.plugins.read().await;
        let plugin = plugins
            .get(name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;
        plugin.execute(input).await
    }

    /// 列出所有插件描述符
    pub async fn list_descriptors(&self) -> Vec<PluginDescriptor> {
        let plugins = self.plugins.read().await;
        let mut descs: Vec<PluginDescriptor> =
            futures_util::future::join_all(plugins.values().map(|p| p.descriptor())).await;
        descs.sort_by(|a, b| a.name.cmp(&b.name));
        descs
    }

    /// 插件数量
    pub async fn count(&self) -> usize {
        self.plugins.read().await.len()
    }

    /// 健康检查所有插件
    pub async fn health_all(&self) -> Vec<(String, bool)> {
        let plugins = self.plugins.read().await;
        let mut results = Vec::new();
        for (name, plugin) in plugins.iter() {
            let healthy = plugin.health().await.unwrap_or(false);
            results.push((name.clone(), healthy));
        }
        results
    }
}

// ---------------------------------------------------------------------------
// 全局 PluginRouter（OnceLock，进程内单例）
// ---------------------------------------------------------------------------

static GLOBAL_PLUGIN_ROUTER: std::sync::OnceLock<PluginRouter> =
    std::sync::OnceLock::new();

/// 获取全局 PluginRouter（未初始化时返回空 router 的引用）
pub fn global_router() -> &'static PluginRouter {
    GLOBAL_PLUGIN_ROUTER.get_or_init(PluginRouter::new)
}

/// 初始化全局 PluginRouter（从 registry.toml + plugins/ 目录加载）
///
/// 返回已加载的插件数。registry.toml 不存在时静默返回 0（可选配置）。
pub async fn init_plugin_router(plugins_dir: &Path, sandbox: &WasmSandboxConfig) -> usize {
    let router = global_router();
    let count = load_registry(plugins_dir, sandbox, router).await;
    if count > 0 {
        tracing::info!("PluginRouter 已就绪，加载 {} 个插件", count);
    }
    count
}

/// 从 plugins/registry.toml + 目录扫描加载
///
/// 优先级：registry.toml 显式声明 > 目录自动扫描
/// - registry.toml 存在：只加载其中 enabled=true 的条目
/// - registry.toml 不存在：扫描 *.wasm（无权限声明 = 最小权限）+ *.md
pub async fn load_registry(plugins_dir: &Path, sandbox: &WasmSandboxConfig, router: &PluginRouter) -> usize {
    if !plugins_dir.exists() {
        return 0;
    }
    let registry_path = plugins_dir.join("registry.toml");
    let mut loaded = 0;

    if registry_path.exists() {
        match std::fs::read_to_string(&registry_path) {
            Ok(content) => match toml::from_str::<RegistryToml>(&content) {
                Ok(reg) => {
                    for entry in reg.plugins.iter().filter(|e| e.enabled) {
                        match load_entry(entry, plugins_dir, sandbox) {
                            Ok(plugin) => {
                                router.register(plugin).await;
                                loaded += 1;
                            }
                            Err(e) => tracing::warn!("插件加载失败 {} ({})：{e}", entry.path, entry.plugin_type),
                        }
                    }
                }
                Err(e) => tracing::warn!("registry.toml 解析失败: {e}"),
            },
            Err(e) => tracing::warn!("registry.toml 读取失败: {e}"),
        }
        return loaded;
    }

    // 目录扫描模式（向后兼容 v0.7.0 行为）
    if let Ok(entries) = std::fs::read_dir(plugins_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            match ext {
                "wasm" => {
                    let access = HostAccessConfig::default(); // 扫描模式无权限声明
                    let sb = WasmSandboxConfig {
                        max_memory: sandbox.max_memory,
                        timeout_ms: sandbox.timeout_ms,
                    };
                    match ExtismPlugin::from_wasm_file(&path, &access, &sb) {
                        Ok(p) => {
                            router.register(Arc::new(p)).await;
                            loaded += 1;
                        }
                        Err(e) => tracing::warn!("Wasm 插件扫描加载失败 {}: {e}", path.display()),
                    }
                }
                "md" => match SkillMdPlugin::from_file(&path, None) {
                    Ok(p) => {
                        router.register(Arc::new(p)).await;
                        loaded += 1;
                    }
                    Err(e) => tracing::warn!("SkillMd 插件加载失败 {}: {e}", path.display()),
                }
                _ => {}
            }
        }
    }
    loaded
}

/// 加载单个 registry 条目
fn load_entry(entry: &PluginEntry, plugins_dir: &Path, sandbox: &WasmSandboxConfig) -> Result<Arc<dyn Plugin>, PluginError> {
    let path = resolve_path(plugins_dir, &entry.path);
    match entry.plugin_type.as_str() {
        "wasm" => {
            let access = HostAccessConfig::new(
                entry.allowed_hosts.clone(),
                entry.allowed_paths.clone(),
                entry.allow_exec,
            );
            let plugin = ExtismPlugin::from_wasm_file(&path, &access, sandbox)?;
            Ok(Arc::new(plugin))
        }
        "skill_md" | "skill" => {
            let plugin = SkillMdPlugin::from_file(&path, entry.name_override.clone())?;
            Ok(Arc::new(plugin))
        }
        "native" => Err(PluginError::Config(
            "native 类型插件经 ToolRegistry 直挂，无需在 registry.toml 配置".into(),
        )),
        "mcp" => Err(PluginError::Config(
            "mcp 类型插件经 McpRemoteTool 自动注册，无需在 registry.toml 配置".into(),
        )),
        other => Err(PluginError::Config(format!("未知插件类型: {other}"))),
    }
}

/// 相对路径解析（基于 plugins_dir）
fn resolve_path(base: &Path, relative: &str) -> PathBuf {
    let path = Path::new(relative);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_path() {
        let base = Path::new("/data/plugins");
        assert_eq!(resolve_path(base, "echo.wasm"), PathBuf::from("/data/plugins/echo.wasm"));
        assert_eq!(
            resolve_path(base, "/abs/path.md"),
            PathBuf::from("/abs/path.md")
        );
    }

    #[tokio::test]
    async fn test_router_register_and_call() {
        let dir = std::env::temp_dir();
        let p = dir.join("router-test-skill.md");
        std::fs::write(&p, "---\nname: rt-skill\ndescription: 测试\n---\n内容ABC").unwrap();

        let router = PluginRouter::new();
        let plugin = SkillMdPlugin::from_file(&p, None).unwrap();
        router.register(Arc::new(plugin)).await;

        assert_eq!(router.count().await, 1);
        let out = router.call("rt-skill", &serde_json::json!({})).await.unwrap();
        assert!(out.success);
        assert!(out.output.contains("内容ABC"));

        // 未找到
        assert!(matches!(
            router.call("nope", &serde_json::json!({})).await,
            Err(PluginError::NotFound(_))
        ));

        let descs = router.list_descriptors().await;
        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].name, "rt-skill");

        let health = router.health_all().await;
        assert_eq!(health[0], ("rt-skill".to_string(), true));

        let _ = std::fs::remove_file(&p);
    }
}
