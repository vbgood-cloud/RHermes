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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// 通道状态
// ---------------------------------------------------------------------------

/// 通道运行时状态快照
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelStatus {
    /// 通道名称
    pub name: String,
    /// 是否已连接/登录
    pub connected: bool,
    /// 人类可读的状态详情（如 bot 用户名、登录账号等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 累计处理的消息数
    pub msg_count: u64,
    /// 最后一次错误信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 通道运行时状态追踪器（供各通道嵌入使用）
pub struct ChannelState {
    /// 是否已连接/登录
    connected: AtomicBool,
    /// 累计消息数
    msg_count: AtomicU64,
    /// 最后错误
    last_error: Mutex<Option<String>>,
}

impl ChannelState {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            msg_count: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }

    /// 标记为已连接
    pub fn set_connected(&self, v: bool) {
        self.connected.store(v, Ordering::Relaxed);
    }

    /// 增加消息计数
    pub fn inc_msg(&self) {
        self.msg_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录最后错误
    pub fn set_error(&self, e: impl Into<String>) {
        if let Ok(mut buf) = self.last_error.lock() {
            *buf = Some(e.into());
        }
    }

    /// 清除错误
    pub fn clear_error(&self) {
        if let Ok(mut buf) = self.last_error.lock() {
            *buf = None;
        }
    }

    /// 生成快照
    pub fn snapshot(&self, name: &str, detail: Option<String>) -> ChannelStatus {
        ChannelStatus {
            name: name.to_string(),
            connected: self.connected.load(Ordering::Relaxed),
            detail,
            msg_count: self.msg_count.load(Ordering::Relaxed),
            last_error: self.last_error.lock().ok().and_then(|b| b.clone()),
        }
    }
}

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

    /// 返回通道当前运行时状态
    ///
    /// 默认实现返回“未知”状态，各通道应重写此方法。
    fn status(&self) -> ChannelStatus {
        ChannelStatus {
            name: self.name().to_string(),
            connected: false,
            detail: None,
            msg_count: 0,
            last_error: None,
        }
    }

    /// 发送打字状态（typing indicator）
    ///
    /// 默认空实现，支持此功能的通道（如 Telegram）应重写。
    async fn send_typing(&self, _chat_id: &str) -> Result<(), String> {
        Ok(())
    }
}
