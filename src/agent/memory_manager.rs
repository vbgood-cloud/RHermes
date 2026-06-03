//! RHermes MemoryManager — 记忆系统编排层
//!
//! ## 架构
//!
//! ```text
//! Agent Loop: prefetch → inject → intercept → sync → flush
//!                    │
//!            MemoryManager（编排层）
//!           "内置 + 至多一个外部 Provider"
//!           工具 schema 合并 / 生命周期钩子广播
//!                    │
//!            ┌───────┴───────┐
//!            │               │
//!    BuiltinProvider   ExternalProvider(trait)
//!    MEMORY.md/USER.md  honcho/mem0 等
//!    MemoryStore+FTS5
//! ```

use std::sync::Arc;
use std::sync::Mutex;

use crate::agent::memory::{MemoryEntry, MemoryError, MemorySystem, MemoryType, StoreMemory, UserProfile};

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// 记忆提供者接口
///
/// 所有 Provider 必须实现这 5 个生命周期方法。
pub trait MemoryProvider: Send + Sync {
    /// 预取：根据用户输入快速搜索相关记忆
    fn prefetch(&self, query: &str, limit: usize) -> Vec<MemoryEntry>;

    /// 注入：将记忆合并到上下文字符串
    fn inject(&self, entries: &[MemoryEntry]) -> String;

    /// 拦截：检查工具调用是否需要记忆辅助
    fn intercept(&self, tool_name: &str, tool_args: &str) -> Option<String>;

    /// 同步：写入新记忆
    fn sync(&self, content: &str, tags: &[&str], project: &str);

    /// 冲刷：持久化到磁盘
    fn flush(&self) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// BuiltinProvider
// ---------------------------------------------------------------------------

/// 内置记忆提供者（SQLite + FTS5 + MEMORY.md + USER.md）
pub struct BuiltinProvider {
    inner: Arc<Mutex<MemorySystem>>,
    memories_dir: Option<std::path::PathBuf>,
    max_memory_md_chars: usize,
}

impl BuiltinProvider {
    pub fn new(
        memory: Arc<Mutex<MemorySystem>>,
        memories_dir: Option<std::path::PathBuf>,
        max_memory_md_chars: usize,
    ) -> Self {
        Self {
            inner: memory,
            memories_dir,
            max_memory_md_chars,
        }
    }
}

impl MemoryProvider for BuiltinProvider {
    fn prefetch(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        if let Ok(mem) = self.inner.lock() {
            mem.search(query, limit).unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    fn inject(&self, entries: &[MemoryEntry]) -> String {
        if entries.is_empty() {
            return String::new();
        }
        let mut result = "【相关记忆】\n".to_string();
        for entry in entries {
            let tag_str = if entry.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", entry.tags.join(", "))
            };
            let preview: String = entry.content.chars().take(200).collect();
            result.push_str(&format!("- {}{}\n", preview, tag_str));
        }
        result
    }

    fn intercept(&self, tool_name: &str, _tool_args: &str) -> Option<String> {
        // 工具拦截：当用户调用搜索/读取类工具时，可以自动补充记忆
        match tool_name {
            "search_content" | "read_file" | "glob" => {
                // 对于这些工具，不需要自动记忆辅助
                None
            }
            _ => None,
        }
    }

    fn sync(&self, content: &str, tags: &[&str], project: &str) {
        if let Ok(mut mem) = self.inner.lock() {
            let _ = mem.remember(content, tags, project);
        }
    }

    fn flush(&self) -> Result<(), String> {
        // 持久化 MEMORY.md 和 USER.md
        if let Some(ref dir) = self.memories_dir {
            if let Ok(mem) = self.inner.lock() {
                let md_path = dir.join("MEMORY.md");
                let _ = mem.export_memory_md(&md_path, "rhermes", 50);
                if self.max_memory_md_chars > 0 {
                    let _ = mem.save_profile_with_limit(
                        &mem.load_profile().unwrap_or_default(),
                        Some(&dir.join("USER.md")),
                        self.max_memory_md_chars,
                    );
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 外部 Provider 占位（未来扩展）
// ---------------------------------------------------------------------------

/// 外部记忆提供者类型（预留）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExternalProviderType {
    Honcho,
    Mem0,
    MemGpt,
    Memary,
    Letta,
    Zep,
    Graphiti,
    Custom,
}

impl ExternalProviderType {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Honcho => "honcho",
            Self::Mem0 => "mem0",
            Self::MemGpt => "memgpt",
            Self::Memary => "memary",
            Self::Letta => "letta",
            Self::Zep => "zep",
            Self::Graphiti => "graphiti",
            Self::Custom => "custom",
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryManager — 编排层
// ---------------------------------------------------------------------------

/// 记忆管理器 — 编排内置 + 至多一个外部 Provider
pub struct MemoryManager {
    /// 内置提供者（始终存在）
    builtin: Box<dyn MemoryProvider>,
    /// 外部提供者（至多一个）
    external: Option<Box<dyn MemoryProvider>>,
    /// 外部提供者类型
    external_type: Option<ExternalProviderType>,
}

impl MemoryManager {
    /// 创建仅内置的 MemoryManager
    pub fn new_builtin(builtin: Box<dyn MemoryProvider>) -> Self {
        Self {
            builtin,
            external: None,
            external_type: None,
        }
    }

    /// 创建带外部 Provider 的 MemoryManager
    pub fn new_with_external(
        builtin: Box<dyn MemoryProvider>,
        external: Box<dyn MemoryProvider>,
        ext_type: ExternalProviderType,
    ) -> Self {
        Self {
            builtin,
            external: Some(external),
            external_type: Some(ext_type),
        }
    }

    /// 获取外部提供者类型
    pub fn external_type(&self) -> Option<ExternalProviderType> {
        self.external_type
    }

    // ---- 生命周期钩子 ----

    /// Prefetch：搜索相关记忆（合并内置 + 外部结果）
    pub fn prefetch(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        let mut results = self.builtin.prefetch(query, limit);
        if let Some(ref ext) = self.external {
            let ext_results = ext.prefetch(query, limit);
            results.extend(ext_results);
            // 去重，按相关性排序（简化：取前 limit 条）
            results.truncate(limit);
        }
        results
    }

    /// Inject：将记忆注入上下文字符串
    pub fn inject(&self, entries: &[MemoryEntry]) -> String {
        self.builtin.inject(entries)
    }

    /// Intercept：工具调用拦截
    pub fn intercept(&self, tool_name: &str, tool_args: &str) -> Option<String> {
        let builtin_result = self.builtin.intercept(tool_name, tool_args);
        if builtin_result.is_some() {
            return builtin_result;
        }
        if let Some(ref ext) = self.external {
            return ext.intercept(tool_name, tool_args);
        }
        None
    }

    /// Sync：写入记忆（同时写入内置 + 外部）
    pub fn sync(&self, content: &str, tags: &[&str], project: &str) {
        self.builtin.sync(content, tags, project);
        if let Some(ref ext) = self.external {
            ext.sync(content, tags, project);
        }
    }

    /// Flush：持久化
    pub fn flush(&self) -> Result<(), String> {
        self.builtin.flush()?;
        if let Some(ref ext) = self.external {
            ext.flush()?;
        }
        Ok(())
    }

    /// 完整生命周期：prefetch → inject → sync → flush
    pub fn full_cycle(&self, query: &str, new_content: Option<(&str, &[&str], &str)>) -> String {
        // 1. Prefetch
        let entries = self.prefetch(query, 3);

        // 2. Inject
        let injected = self.inject(&entries);

        // 3. Sync 新内容
        if let Some((content, tags, project)) = new_content {
            self.sync(content, tags, project);
        }

        // 4. Flush（每 10 次调用持久化一次，这里简化）
        let _ = self.flush();

        injected
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::memory::*;
    use std::sync::{Arc, Mutex};

    fn setup_manager() -> MemoryManager {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let ms = MemorySystem::open(&path).unwrap();
        let provider = BuiltinProvider::new(
            Arc::new(Mutex::new(ms)),
            Some(tmp.path().to_path_buf()),
            5000,
        );
        MemoryManager::new_builtin(Box::new(provider))
    }

    #[test]
    fn test_prefetch_empty() {
        let manager = setup_manager();
        let entries = manager.prefetch("test", 10);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_sync_and_prefetch() {
        let manager = setup_manager();
        manager.sync("Rust 的所有权系统", &["rust", "memory"], "rhermes");
        manager.sync("tokio 异步运行时", &["rust", "async"], "rhermes");

        let entries = manager.prefetch("rust", 10);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_inject_format() {
        let manager = setup_manager();
        let entries = vec![MemoryEntry {
            id: 1,
            memory_type: MemoryType::Working,
            content: "测试记忆内容".into(),
            tags: vec!["test".into()],
            project: "rhermes".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            last_accessed: "2026-01-01T00:00:00Z".into(),
            access_count: 1,
        }];
        let result = manager.inject(&entries);
        assert!(result.contains("相关记忆"));
        assert!(result.contains("测试记忆内容"));
    }

    #[test]
    fn test_full_cycle() {
        let manager = setup_manager();
        let result = manager.full_cycle("rust", Some(("测试", &["test"], "rhermes")));
        // 空数据库，prefetch 无结果，inject 返回空
        assert!(result.is_empty());

        // 验证 sync 写入成功
        let entries = manager.prefetch("test", 10);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_intercept() {
        let manager = setup_manager();
        assert!(manager.intercept("read_file", r#"{"path":"test.txt"}"#).is_none());
        assert!(manager.intercept("unknown_tool", "").is_none());
    }
}
