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

pub mod wechat;
pub mod wecom;
pub mod telegram;

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
/// - `login_qrcode()`: 可选，返回登录二维码（文本 + PNG 图片字节）
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

    /// 向指定会话发送格式化消息（Markdown 等）
    ///
    /// 默认实现回退为纯文本 send_message。
    /// 支持格式的通道（如 Telegram）应重写此方法以使用 MarkdownV2 等。
    async fn send_formatted(&self, chat_id: &str, text: &str) -> Result<(), String> {
        self.send_message(chat_id, text).await
    }

    /// 可选：返回登录二维码（二维码文本, PNG 字节）
    /// 返回 None 表示该通道不需要扫码登录
    async fn login_qrcode(&self) -> Option<(String, Vec<u8>)> {
        None
    }
}
