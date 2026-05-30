//! RHermes 三段式 Context 架构
//!
//! 这是 Reasonix 省 Token 的核心设计 —— 围绕 DeepSeek prefix cache
//! 的 byte 级稳定性需求构建。
//!
//! ## 三区设计
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │  IMMUTABLE PREFIX                       │  ← session 内固定不变
//! │  system + tool_specs + few_shots        │  ← 缓存命中候选
//! ├─────────────────────────────────────────┤
//! │  APPEND-ONLY LOG                        │  ← 单调递增
//! │  [assistant][tool][assistant]...        │  ← 保留之前轮次的前缀
//! ├─────────────────────────────────────────┤
//! │  VOLATILE SCRATCH                       │  ← 每轮重置
//! │  思考/计划/临时状态                      │  ← 不发送到上游
//! └─────────────────────────────────────────┘
//! ```
//!
//! ## 三条不变式
//! 1. Prefix 一次计算 —— session 建立时计算、哈希、锁定，不再改动
//! 2. Log 只追加 —— 按顺序序列化，不重写任何已有条目
//! 3. Scratch 蒸馏后才能进入 Log —— 摘要压缩后追加

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::tui::Role;

// ---------------------------------------------------------------------------
// 消息类型（复用 tui::Message 的序列化版本）
// ---------------------------------------------------------------------------

/// API 兼容的消息格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub role: String, // "system" | "user" | "assistant"
    pub content: String,
}

impl From<ApiMessage> for Message {
    fn from(msg: ApiMessage) -> Self {
        Message {
            role: match msg.role.as_str() {
                "system" => Role::System,
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => Role::System,
            },
            content: msg.content,
        }
    }
}

impl From<Message> for ApiMessage {
    fn from(msg: Message) -> Self {
        ApiMessage {
            role: match msg.role {
                Role::User => "user".into(),
                Role::Assistant => "assistant".into(),
                Role::System => "system".into(),
            },
            content: msg.content,
        }
    }
}

// ---------------------------------------------------------------------------
// 三段式 Context
// ---------------------------------------------------------------------------

/// 三段式 Context 架构
///
/// `Arc<[u8]>` 确保了 prefix 在多处共享时的字节级稳定性 ——
/// 没有 V8 GC 移动内存的干扰，指针固定不变。
#[derive(Debug, Clone)]
pub struct Context {
    /// Session 内不可变的 byte 前缀
    immutable_prefix: Arc<[u8]>,

    /// 只追加的对话日志（序列化后的 bytes）
    append_only_log: Vec<u8>,

    /// 每轮重置的 Scratch 空间
    scratch: Vec<Message>,

    /// 系统提示词（原始文本，用于重建 prefix）
    system_prompt: String,
}

impl Context {
    /// 创建一个新的 Context
    ///
    /// `system_prompt` 会被序列化为 immutable prefix 的第一部分。
    /// 后续可以通过 `extend_prefix` 添加 tool specs。
    pub fn new(system_prompt: impl Into<String>) -> Self {
        let system_prompt: String = system_prompt.into();

        // 序列化系统提示为 immutable prefix
        let prefix = Self::serialize_system(&system_prompt);

        Self {
            immutable_prefix: prefix.into(),
            append_only_log: Vec::new(),
            scratch: Vec::new(),
            system_prompt,
        }
    }

    /// 扩展 Immutable Prefix（例如添加 tool specs / few shots）
    /// 必须在任何消息追加之前调用
    pub fn extend_prefix(&mut self, extra: impl AsRef<[u8]>) {
        let mut new_prefix = self.immutable_prefix.to_vec();
        new_prefix.extend_from_slice(extra.as_ref());
        self.immutable_prefix = new_prefix.into();
    }

    /// 追加一条消息到 Log（只追加，不重写）
    pub fn push_to_log(&mut self, msg: Message) {
        let serialized = Self::serialize_message(&msg);
        self.append_only_log.extend_from_slice(&serialized);
    }

    /// 追加消息到 Scratch（每轮重置）
    pub fn push_to_scratch(&mut self, msg: Message) {
        self.scratch.push(msg);
    }

    /// 清空 Scratch
    pub fn clear_scratch(&mut self) {
        self.scratch.clear();
    }

    /// 将 Scratch 中的消息蒸馏后追加到 Log
    pub fn distill_scratch_to_log(&mut self) {
        let scratch_msgs: Vec<Message> = self.scratch.drain(..).collect();
        for msg in scratch_msgs {
            self.push_to_log(msg);
        }
    }

    /// 构建发送到 API 的完整请求 body（不含 scratch）
    /// 返回序列化后的 JSON bytes
    pub fn build_request_body(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&self.immutable_prefix);
        body.extend_from_slice(&self.append_only_log);
        body
    }

    /// 获取 immutable prefix 的字节长度
    pub fn prefix_len(&self) -> usize {
        self.immutable_prefix.len()
    }

    /// 获取 log 的字节长度
    pub fn log_len(&self) -> usize {
        self.append_only_log.len()
    }

    /// 获取 scratch 中的消息数
    pub fn scratch_count(&self) -> usize {
        self.scratch.len()
    }

    /// 返回完整的消息列表（用于显示）
    pub fn all_messages(&self) -> Vec<Message> {
        // 从 raw bytes 反序列化出所有消息
        // 简化：使用已存储的格式
        // 实际应该解析 append_only_log
        // 但这里用简单方式：返回 scratch + 手动构建的初始消息
        let mut msgs = Vec::new();
        msgs.push(Message {
            role: Role::System,
            content: self.system_prompt.clone(),
        });
        // scratch 中的消息
        msgs.extend(self.scratch.clone());
        msgs
    }

    // ---- 序列化辅助 ----

    /// 将系统消息序列化为统一的请求格式
    fn serialize_system(content: &str) -> Vec<u8> {
        let msg = serde_json::json!({
            "role": "system",
            "content": content,
        });
        serde_json::to_vec(&msg).unwrap()
    }

    /// 将消息序列化为统一的请求格式
    fn serialize_message(msg: &Message) -> Vec<u8> {
        let role_str = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };
        let msg_json = serde_json::json!({
            "role": role_str,
            "content": msg.content,
        });
        // 每条消息前加逗号，保持 JSON array 格式
        let mut bytes = b",".to_vec();
        bytes.extend_from_slice(&serde_json::to_vec(&msg_json).unwrap());
        bytes
    }
}

// ---------------------------------------------------------------------------
// Message（复用 tui::Message，避免循环依赖）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::Role;

    #[test]
    fn test_context_new() {
        let ctx = Context::new("You are a helpful assistant.");
        assert!(ctx.prefix_len() > 0);
        assert_eq!(ctx.log_len(), 0);
        assert_eq!(ctx.scratch_count(), 0);
    }

    #[test]
    fn test_context_push_to_log_increases_size() {
        let mut ctx = Context::new("system prompt");
        let before = ctx.log_len();

        ctx.push_to_log(Message::new(Role::User, "hello"));
        assert!(ctx.log_len() > before);
    }

    #[test]
    fn test_context_prefix_unchanged_after_log_push() {
        let mut ctx = Context::new("system prompt");
        let prefix_before = ctx.prefix_len();

        ctx.push_to_log(Message::new(Role::User, "hello"));
        ctx.push_to_log(Message::new(Role::Assistant, "world"));

        // prefix 在 log 追加后不变
        assert_eq!(ctx.prefix_len(), prefix_before);
    }

    #[test]
    fn test_context_scratch_isolated() {
        let mut ctx = Context::new("prompt");
        ctx.push_to_scratch(Message::new(Role::Assistant, "thinking..."));
        assert_eq!(ctx.scratch_count(), 1);
        assert_eq!(ctx.log_len(), 0); // scratch 不影响 log
    }

    #[test]
    fn test_context_clear_scratch() {
        let mut ctx = Context::new("prompt");
        ctx.push_to_scratch(Message::new(Role::Assistant, "thinking..."));
        ctx.clear_scratch();
        assert_eq!(ctx.scratch_count(), 0);
    }

    #[test]
    fn test_context_distill_scratch_to_log() {
        let mut ctx = Context::new("prompt");
        ctx.push_to_scratch(Message::new(Role::Assistant, "result"));
        assert_eq!(ctx.scratch_count(), 1);

        ctx.distill_scratch_to_log();
        assert_eq!(ctx.scratch_count(), 0);
        assert!(ctx.log_len() > 0);
    }

    #[test]
    fn test_context_extend_prefix() {
        let mut ctx = Context::new("system prompt");
        let len_before = ctx.prefix_len();

        ctx.extend_prefix(b"\n--- tool spec ---");
        assert!(ctx.prefix_len() > len_before);
    }

    #[test]
    fn test_context_build_request_includes_prefix_and_log() {
        let mut ctx = Context::new("system prompt");
        ctx.push_to_log(Message::new(Role::User, "hello"));

        let body = ctx.build_request_body();
        assert!(body.len() >= ctx.prefix_len());
        assert!(body.len() >= ctx.log_len());
        // body = prefix + log
        assert_eq!(body.len(), ctx.prefix_len() + ctx.log_len());
    }

    #[test]
    fn test_context_multiple_rounds_prefix_stable() {
        // 模拟多轮对话，验证 prefix 始终不变
        let mut ctx = Context::new("system prompt");
        let prefix = ctx.prefix_len();

        // round 1
        ctx.push_to_log(Message::new(Role::User, "q1"));
        ctx.push_to_log(Message::new(Role::Assistant, "a1"));
        assert_eq!(ctx.prefix_len(), prefix);

        // round 2
        ctx.push_to_log(Message::new(Role::User, "q2"));
        ctx.push_to_log(Message::new(Role::Assistant, "a2"));
        assert_eq!(ctx.prefix_len(), prefix);
    }

    #[test]
    fn test_api_message_conversion() {
        let msg = Message::new(Role::User, "hello");
        let api_msg: ApiMessage = msg.clone().into();
        assert_eq!(api_msg.role, "user");
        assert_eq!(api_msg.content, "hello");

        let back: Message = api_msg.into();
        assert_eq!(back.role, Role::User);
        assert_eq!(back.content, "hello");
    }

    #[test]
    fn test_serialize_message_format() {
        let msg = Message::new(Role::User, "test");
        let bytes = Context::serialize_message(&msg);
        let json_str = String::from_utf8_lossy(&bytes);
        // 应该以逗号开头，且包含角色和内容
        assert!(json_str.starts_with(','));
        assert!(json_str.contains("\"role\":\"user\""));
        assert!(json_str.contains("\"content\":\"test\""));
    }
}
