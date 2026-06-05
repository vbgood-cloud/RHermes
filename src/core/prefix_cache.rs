//! Prefix Cache 管理器
//!
//! 管理三层 system prompt 前缀的构建：
//!
//! - **Layer 1 (Stable)**: 身份/规则/记忆指引 —— session 内固定不变
//! - **Layer 2 (Context)**: USER.md + MEMORY.md 内容 —— 可跨 session 更新
//! - **Layer 3 (Volatile)**: 时间/画像摘要/AGENTS.md —— session 内不变
//!
//! 三层合并后冻结为 `Arc<[u8]>`，用于 DeepSeek prefix cache 命中优化。

use std::sync::Arc;

/// Prefix Cache 管理器
///
/// 负责构建和维护三层不可变前缀，在 session 启动时一次性冻结。
#[derive(Debug, Clone)]
pub struct PrefixCacheManager {
    /// Layer 1: 稳定层（身份/规则/记忆指引）
    layer1: String,
    /// Layer 2: 上下文层（USER.md + MEMORY.md）
    layer2: String,
    /// Layer 3: 易变层（时间/画像/AGENTS.md）
    layer3: String,
    /// 冻结后的完整前缀 bytes（全部三层合并 + 序列化后的 system 消息）
    frozen: Option<Arc<[u8]>>,
}

impl PrefixCacheManager {
    /// 创建新的 PrefixCacheManager
    pub fn new(layer1: impl Into<String>) -> Self {
        Self {
            layer1: layer1.into(),
            layer2: String::new(),
            layer3: String::new(),
            frozen: None,
        }
    }

    /// 设置 Layer 2（上下文记忆层）
    pub fn set_layer2(&mut self, content: impl Into<String>) {
        self.layer2 = content.into();
    }

    /// 设置 Layer 3（易变层）
    pub fn set_layer3(&mut self, content: impl Into<String>) {
        self.layer3 = content.into();
    }

    /// 冻结所有层为不可变前缀
    /// 必须在任何消息追加之前调用
    pub fn freeze(&mut self) -> Arc<[u8]> {
        let full = format!("{}{}{}", self.layer1, self.layer2, self.layer3);
        let serialized = Self::serialize_system(&full);
        let arc: Arc<[u8]> = serialized.into();
        self.frozen = Some(arc.clone());
        arc
    }

    /// 获取已冻结的前缀（必须在 freeze 之后调用）
    pub fn frozen_prefix(&self) -> Option<&Arc<[u8]>> {
        self.frozen.as_ref()
    }

    /// 获取 Layer 1 文本
    pub fn layer1(&self) -> &str {
        &self.layer1
    }

    /// 获取 Layer 2 文本
    pub fn layer2(&self) -> &str {
        &self.layer2
    }

    /// 获取 Layer 3 文本
    pub fn layer3(&self) -> &str {
        &self.layer3
    }

    /// 获取完整 system prompt 文本（三层合并）
    pub fn full_system_prompt(&self) -> String {
        format!("{}{}{}", self.layer1, self.layer2, self.layer3)
    }

    /// 将系统消息序列化为统一的请求格式
    fn serialize_system(content: &str) -> Vec<u8> {
        let msg = serde_json::json!({
            "role": "system",
            "content": content,
        });
        serde_json::to_vec(&msg).unwrap()
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_cache_new() {
        let mut mgr = PrefixCacheManager::new("system prompt");
        let frozen = mgr.freeze();
        assert!(frozen.len() > 0);
        assert!(mgr.frozen_prefix().is_some());
    }

    #[test]
    fn test_prefix_cache_layers() {
        let mut mgr = PrefixCacheManager::new("layer1\n");
        mgr.set_layer2("layer2\n");
        mgr.set_layer3("layer3");
        assert_eq!(mgr.full_system_prompt(), "layer1\nlayer2\nlayer3");
    }

    #[test]
    fn test_prefix_cache_frozen_immutable() {
        let mut mgr = PrefixCacheManager::new("base");
        let frozen = mgr.freeze();
        let len_before = frozen.len();
        // 冻后续修改不应影响已冻结的 prefix
        mgr.set_layer2("extra".to_string());
        assert_eq!(frozen.len(), len_before);
    }

    #[test]
    fn test_prefix_cache_serialized_format() {
        let mut mgr = PrefixCacheManager::new("You are a helpful assistant.");
        let frozen = mgr.freeze();
        let json_str = String::from_utf8_lossy(&frozen);
        assert!(json_str.contains("\"role\":\"system\""));
        assert!(json_str.contains("\"content\":\"You are a helpful assistant.\""));
    }
}
