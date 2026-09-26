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

    /// 剥离思考流残留：session 层把 reasoning 用 format_thinking_block 注入正文流，
    /// TUI 能特殊渲染，外部通道（微信/企微/QQ/web）必须剪掉，否则内部规划暴露给学生。
    /// 正则必须匹配 session.rs::format_thinking_block 的真实输出（🤔 **思考过程**），
    /// 回归样例见本文件 tests。
    fn strip_thinking(text: &str) -> String {
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = RE.get_or_init(|| {
            regex::Regex::new(r"(?s)🤔\s*\*\*思考过程.*?(?:\r?\n\r?\n|\z)").expect("静态正则必合法")
        });
        let mut s = re.replace_all(text, "").to_string();
        // 成对 <think>…</think>：flush 时已是完整文本，整块删除内容而非仅删标记
        static THINK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let think = THINK.get_or_init(|| {
            regex::Regex::new(r"(?s)<think>.*?</think>").expect("静态正则必合法")
        });
        s = think.replace_all(&s, "").to_string();
        // 跨 chunk 残留的孤立标记：仅删字面量
        s = s.replace("<think>", "").replace("</think>", "");
        s.trim_start_matches(['\r', '\n', ' ']).to_string()
    }

    async fn flush_buffer(&self) {
        let text = {
            let mut buf = self.buffer.lock().unwrap();
            if buf.is_empty() { return; }
            std::mem::take(&mut *buf)
        };
        let text = Self::strip_thinking(&text);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// B1 回归：strip_thinking 必须剥掉 session.rs::format_thinking_block 的真实输出。
    /// （v0.7.16 曾因编辑器二次编码把正则写成乱码，函数静默失效、一个字都剥不掉。）
    #[test]
    fn strip_thinking_removes_thinking_block() {
        let sample = "🤔 **思考过程** (52字)\n> 学生问的是循环队列的入队……\n\n正文回答在这里。";
        assert_eq!(ChannelSink::strip_thinking(sample), "正文回答在这里。");
    }

    /// CRLF 变体：session.rs 在 CRLF 工作区下会输出 \r\n 行尾。
    #[test]
    fn strip_thinking_handles_crlf() {
        let sample = "🤔 **思考过程**\r\n> abc\r\n\r\n正文";
        assert_eq!(ChannelSink::strip_thinking(sample), "正文");
    }

    /// 只有思考块 → 剥离后为空。
    #[test]
    fn strip_thinking_block_only() {
        let sample = "🤔 **思考过程**\n> 全是思考\n\n";
        assert_eq!(ChannelSink::strip_thinking(sample), "");
    }

    /// 普通文本不受影响；<think> 标签字面量仍被清理。
    #[test]
    fn strip_thinking_plain_text_untouched() {
        assert_eq!(ChannelSink::strip_thinking("普通回答"), "普通回答");
        assert_eq!(ChannelSink::strip_thinking("<think>x</think>正文"), "正文");
    }
}
