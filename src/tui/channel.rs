//! TUI 通道适配器
//!
//! 将 TUI 包装为 Channel trait 实现，使终端界面可与其他消息渠道
//! （微信、Telegram 等）统一通过 ChannelManager 管理。

use async_trait::async_trait;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};

use crate::channel::{Channel, InboundMessage};
use crate::tui::App;

/// TUI 通道
///
/// 保持与现有 App 的完全兼容，同时实现 Channel trait。
/// 当需要多渠道并行时，TuiChannel 可以像微信一样注册到 ChannelManager。
pub struct TuiChannel;

impl TuiChannel {
    /// 将 App 接入 Channel 系统
    ///
    /// 在 App 上设置 channel_inbound_tx，使 Enter 消息通过 Channel 发送。
    pub fn attach(app: &mut App, inbound_tx: mpsc::UnboundedSender<InboundMessage>) {
        app.set_channel_inbound(inbound_tx);
    }

    /// 创建新 App 并挂接 Channel
    pub fn new_app(
        mode: &str,
        dispatcher: crate::tools::ToolDispatcher,
        memory: Option<std::sync::Arc<std::sync::Mutex<crate::agent::MemorySystem>>>,
        skill_engine: Option<std::sync::Arc<std::sync::Mutex<crate::agent::SkillEngine>>>,
        resume: bool,
        config_path: std::path::PathBuf,
        max_memory_md_chars: usize,
        memories_dir: std::path::PathBuf,
        debug: std::sync::Arc<std::sync::Mutex<crate::debug::SessionDebug>>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
    ) -> App {
        let mut app = App::new(
            mode, dispatcher, memory, skill_engine, resume, config_path,
            max_memory_md_chars, memories_dir, debug,
        );
        app.set_channel_inbound(inbound_tx);
        app
    }
}

#[async_trait]
impl Channel for TuiChannel {
    fn start(
        self: Arc<Self>,
        _inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        _shutdown_rx: broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        // TUI 的主循环由外部 main.rs 的 app.run() 驱动
        // 此处不需要启动额外 task
        tokio::spawn(async move {
            tracing::info!("TuiChannel: 等待外部 run() 调用");
            // 实际渲染循环在 App::run() 中, 这里只做占位
            std::future::pending::<()>().await;
        })
    }

    async fn send_message(&self, _chat_id: &str, text: &str) -> Result<(), String> {
        // TUI 的消息显示由 handle_api_events 处理
        tracing::debug!("TuiChannel 收到出站消息: {:.60}", text);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "tui"
    }
}
