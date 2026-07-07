//! Agent 事件输出抽象
//!
//! 定义 EventSink trait，将 Agent Loop 的事件输出与具体消费端解耦。
//! - TuiSink: 通过 mpsc channel 推送给 TUI 渲染
//! - ChannelSink: 通过 ChannelManager 发送到微信/企微等外部通道

use std::sync::Mutex;

use async_trait::async_trait;

use crate::api::{ApiEvent, ToolCallData, Usage};
use crate::channel::ChannelManager;

/// Agent 事件消费者
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn on_chunk(&self, text: &str);
    async fn on_tool_calls(&self, calls: &[ToolCallData]);
    async fn on_tool_result(&self, name: &str, output: &str, duration_ms: u64, success: bool);
    async fn on_done(&self);
    async fn on_error(&self, error: &str);
    async fn on_usage(&self, _usage: &Usage) {}
    async fn on_balance(&self, _balance: f64) {}
    /// 通知 sink 发送打字状态（typing indicator）
    async fn on_typing(&self) {}
}

// ---------------------------------------------------------------------------
// TuiSink
// ---------------------------------------------------------------------------

pub struct TuiSink {
    event_tx: tokio::sync::mpsc::UnboundedSender<ApiEvent>,
}

impl TuiSink {
    pub fn new(event_tx: tokio::sync::mpsc::UnboundedSender<ApiEvent>) -> Self {
        Self { event_tx }
    }
}

#[async_trait]
impl EventSink for TuiSink {
    async fn on_chunk(&self, text: &str) {
        let _ = self.event_tx.send(ApiEvent::StreamChunk(text.to_string()));
    }
    async fn on_tool_calls(&self, calls: &[ToolCallData]) {
        let _ = self.event_tx.send(ApiEvent::ToolCalls(calls.to_vec()));
    }
    async fn on_tool_result(&self, _name: &str, _output: &str, _duration_ms: u64, _success: bool) {}
    async fn on_done(&self) {
        let _ = self.event_tx.send(ApiEvent::Done);
    }
    async fn on_error(&self, error: &str) {
        let _ = self.event_tx.send(ApiEvent::Error(error.to_string()));
    }
    async fn on_usage(&self, usage: &Usage) {
        let _ = self.event_tx.send(ApiEvent::Usage(usage.clone()));
    }
    async fn on_balance(&self, balance: f64) {
        let _ = self.event_tx.send(ApiEvent::Balance(balance));
    }
}

// ---------------------------------------------------------------------------
// ChannelSink
// ---------------------------------------------------------------------------

/// 外部通道事件接收器 — 文本缓冲后通过 Channel.send_message 发送
pub struct ChannelSink {
    channel_mgr: std::sync::Arc<ChannelManager>,
    channel_name: String,
    chat_id: String,
    buffer: Mutex<String>,
}

impl ChannelSink {
    pub fn new(
        channel_mgr: std::sync::Arc<ChannelManager>,
        channel_name: String,
        chat_id: String,
    ) -> Self {
        Self { channel_mgr, channel_name, chat_id, buffer: Mutex::new(String::new()) }
    }

    async fn flush_buffer(&self) {
        let text = {
            let mut buf = self.buffer.lock().unwrap();
            if buf.is_empty() { return; }
            std::mem::take(&mut *buf)
        };
        if let Some(ch) = self.channel_mgr.get(&self.channel_name) {
            if let Err(e) = ch.send_message(&self.chat_id, &text).await {
                tracing::warn!("{} flush_buffer 发送失败 (chat_id={}): {}", self.channel_name, self.chat_id, e);
            }
        } else {
            tracing::warn!("通道 {} 未注册，无法发送消息", self.channel_name);
        }
    }
}

#[async_trait]
impl EventSink for ChannelSink {
    async fn on_chunk(&self, text: &str) {
        if let Ok(mut buf) = self.buffer.lock() { buf.push_str(text); }
    }
    async fn on_tool_calls(&self, calls: &[ToolCallData]) {
        let details: String = calls.iter()
            .map(|c| format!("{}({})", c.name, c.arguments))
            .collect::<Vec<_>>()
            .join(", ");
        if let Some(ch) = self.channel_mgr.get(&self.channel_name) {
            if let Err(e) = ch.send_message(&self.chat_id, &format!("🔧 正在执行: {}", details)).await {
                tracing::warn!("{} on_tool_calls 发送失败: {}", self.channel_name, e);
            }
        }
    }
    async fn on_tool_result(&self, name: &str, _output: &str, duration_ms: u64, success: bool) {
        let icon = if success { "✅" } else { "❌" };
        if let Some(ch) = self.channel_mgr.get(&self.channel_name) {
            if let Err(e) = ch.send_message(&self.chat_id, &format!("  {} {} ({}ms)", icon, name, duration_ms)).await {
                tracing::warn!("{} on_tool_result 发送失败: {}", self.channel_name, e);
            }
        }
    }
    async fn on_done(&self) { self.flush_buffer().await; }
    async fn on_typing(&self) {
        if let Some(ch) = self.channel_mgr.get(&self.channel_name) {
            if let Err(e) = ch.send_typing(&self.chat_id).await {
                tracing::debug!("{} send_typing 失败: {}", self.channel_name, e);
            }
        }
    }
    async fn on_error(&self, error: &str) {
        if let Some(ch) = self.channel_mgr.get(&self.channel_name) {
            if let Err(e) = ch.send_message(&self.chat_id, &format!("❌ {}", error)).await {
                tracing::warn!("{} on_error 发送失败: {}", self.channel_name, e);
            }
        }
    }
}
