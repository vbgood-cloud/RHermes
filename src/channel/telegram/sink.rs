//! Telegram 专用 EventSink
//!
//! 在 ChannelSink 基础上增加 Telegram 专属行为：
//! - 思考过程（tool call 前的流式输出）单独发送为一条消息
//! - 最终回复使用 MarkdownV2 格式发送
//! - 工具调用/结果/错误实时发送

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::agent::EventSink;
use crate::api::ToolCallData;
use crate::channel::ChannelManager;

/// Telegram 事件接收器
///
/// 消息发送流程：
/// 1. on_chunk → 分阶段缓冲（tool call 前 = 思考，tool call 后 = 回复）
/// 2. on_tool_calls → 刷新思考缓冲区 + 发送工具调用信息
/// 3. on_tool_result → 发送工具执行结果
/// 4. on_done → 使用 MarkdownV2 发送最终回复
/// 5. on_error → 发送错误信息
pub struct TelegramSink {
    channel_mgr: Arc<ChannelManager>,
    chat_id: String,
    /// 思考阶段缓冲（tool call 前的流式输出）
    thinking_buf: Mutex<String>,
    /// 最终回复缓冲（tool call 后的流式输出）
    reply_buf: Mutex<String>,
    /// 是否已经触发过 tool calls（区分思考/回复阶段）
    has_tool_calls: Mutex<bool>,
}

impl TelegramSink {
    pub fn new(
        channel_mgr: Arc<ChannelManager>,
        chat_id: String,
    ) -> Self {
        Self {
            channel_mgr,
            chat_id,
            thinking_buf: Mutex::new(String::new()),
            reply_buf: Mutex::new(String::new()),
            has_tool_calls: Mutex::new(false),
        }
    }

    /// 发送纯文本消息到 Telegram
    async fn send_plain(&self, text: &str) {
        if let Some(ch) = self.channel_mgr.get("telegram") {
            if let Err(e) = ch.send_message(&self.chat_id, text).await {
                tracing::warn!("Telegram send_plain 失败 (chat_id={}): {}", self.chat_id, e);
            }
        } else {
            tracing::warn!("Telegram 通道未注册，无法发送消息");
        }
    }

    /// 发送 MarkdownV2 格式化的消息到 Telegram
    async fn send_formatted(&self, text: &str) {
        if let Some(ch) = self.channel_mgr.get("telegram") {
            if let Err(e) = ch.send_formatted(&self.chat_id, text).await {
                tracing::warn!("Telegram send_formatted 失败 (chat_id={}): {}", self.chat_id, e);
            }
        } else {
            tracing::warn!("Telegram 通道未注册，无法发送消息");
        }
    }

    /// 清空思考缓冲区并发送
    async fn flush_thinking(&self) {
        let text = {
            let mut buf = self.thinking_buf.lock().unwrap();
            if buf.is_empty() { return; }
            std::mem::take(&mut *buf)
        };
        // 添加思考前缀
        let msg = format!("💭 思考过程\n\n{}", text);
        self.send_plain(&msg).await;
    }

    /// 清空回复缓冲区并发送（使用 MarkdownV2）
    async fn flush_reply(&self) {
        let text = {
            let mut buf = self.reply_buf.lock().unwrap();
            if buf.is_empty() { return; }
            std::mem::take(&mut *buf)
        };
        self.send_formatted(&text).await;
    }
}

#[async_trait]
impl EventSink for TelegramSink {
    async fn on_chunk(&self, text: &str) {
        let has_tool_calls = *self.has_tool_calls.lock().unwrap();
        if has_tool_calls {
            // 最终回复阶段
            if let Ok(mut buf) = self.reply_buf.lock() {
                buf.push_str(text);
            }
        } else {
            // 思考阶段
            if let Ok(mut buf) = self.thinking_buf.lock() {
                buf.push_str(text);
            }
        }
    }

    async fn on_tool_calls(&self, calls: &[ToolCallData]) {
        // 刷新思考缓冲区（第一次触发 tool calls 时发送）
        if !*self.has_tool_calls.lock().unwrap() {
            self.flush_thinking().await;
        }
        *self.has_tool_calls.lock().unwrap() = true;

        // 发送工具调用信息
        let details: String = calls.iter()
            .map(|c| format!("{}({})", c.name, c.arguments))
            .collect::<Vec<_>>()
            .join("\n");
        let msg = format!("🔧 正在执行: \n{}", details);
        self.send_plain(&msg).await;
    }

    async fn on_tool_result(&self, name: &str, _output: &str, duration_ms: u64, success: bool) {
        let icon = if success { "✅" } else { "❌" };
        let msg = format!("  {} {} ({}ms)", icon, name, duration_ms);
        self.send_plain(&msg).await;
    }

    async fn on_done(&self) {
        // 如果从未触发 tool calls，思考缓冲即是最终回复
        if !*self.has_tool_calls.lock().unwrap() {
            let text = {
                let mut buf = self.thinking_buf.lock().unwrap();
                if buf.is_empty() { return; }
                std::mem::take(&mut *buf)
            };
            self.send_formatted(&text).await;
        } else {
            self.flush_reply().await;
        }
    }

    async fn on_error(&self, error: &str) {
        // 如果思考阶段出错，丢弃思考缓冲
        if !*self.has_tool_calls.lock().unwrap() {
            let _ = self.thinking_buf.lock().unwrap().clear();
        }
        let msg = format!("❌ {}", error);
        self.send_plain(&msg).await;
    }
}
