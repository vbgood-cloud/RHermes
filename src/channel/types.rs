//! 通道消息类型

/// 外部投递给 Agent 核心的入站消息
#[derive(Debug, Clone)]
pub struct InboundMessage {
    /// 来源通道名（如 "tui", "weixin", "telegram"）
    pub channel: String,
    /// 会话 ID（用于回复时传给 send_message）
    pub chat_id: String,
    /// 发送者 ID
    pub sender_id: String,
    /// 消息文本内容
    pub content: String,
    /// 时间戳（毫秒）
    pub timestamp_ms: i64,
}

impl InboundMessage {
    pub fn new(channel: impl Into<String>, chat_id: impl Into<String>, content: impl Into<String>) -> Self {
        let chat_id_str = chat_id.into();
        Self {
            channel: channel.into(),
            chat_id: chat_id_str.clone(),
            sender_id: chat_id_str,
            content: content.into(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// 出站消息（Agent 回复）
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    /// 目标通道名
    pub channel: String,
    /// 目标会话 ID
    pub chat_id: String,
    /// 回复内容
    pub content: String,
}
