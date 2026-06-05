//! Channel 抽象层 — 消息收发接口
//!
//! 定义统一的 Channel trait，支持多种消息来源：
//! - TUI (ratatui 终端界面)
//! - 微信/企业微信
//! - Telegram / Discord
//! - 任何实现 Channel trait 的输入源

mod manager;
mod types;

pub use manager::ChannelManager;
pub use types::InboundMessage;

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// 消息通道的统一抽象
///
/// 每个 Channel 实现：
/// - `start()`: 启动消息接收循环
/// - `send_message()`: 发送文本回复
/// - `name()`: 返回通道名称
#[async_trait]
pub trait Channel: Send + Sync {
    /// 启动通道的消息接收循环
    fn start(
        self: Arc<Self>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> JoinHandle<()>;

    /// 向指定会话发送文本消息
    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String>;

    /// 通道名称
    fn name(&self) -> &'static str;
}
