//! 企微通道 — 模式门面（D19）
//!
//! 按 `[channels.wecom] mode` 分派：
//! - `stream`：智能机器人 WebSocket 长连接（免回调，D19 新链路）→ [`stream::WeComStreamChannel`]
//! - `poll`：旧链路 webhook 发送 + message/list 轮询（缺省回落，零改动兼容）→ [`poll::WeComPollChannel`]
//!
//! 对 `ChannelManager` / `SessionRouter` 侧完全无感——注册名恒为 `"wecom"`。

mod poll;
mod stream;

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::channel::{Channel, InboundMessage};
use crate::core::Config;

pub use stream::{SdkWecomStreamApi, WeComStreamApi, WeComStreamChannel};
pub use stream::{try_connect_blocking, write_bot_secret_to_env};
/// 企微通道门面：构造时按 mode 选定实现，start/send_message 全部转发
pub enum WeComChannel {
    Stream(Arc<WeComStreamChannel>),
    Poll(Arc<poll::WeComPollChannel>),
    /// 凭证缺失禁用（D19：不回落 poll，明确指路）
    Disabled,
}

impl WeComChannel {
    pub fn new(config: &Config) -> Self {
        let w = &config.channels.wecom;
        // D19 配置统一：长连接（API 模式）为唯一推荐路径。
        // 凭证缺失不回落 poll——明确报错禁用并指路（配置入口已不再产出旧字段）
        match w.mode {
            crate::core::WeComMode::Stream => {
                if w.bot_id.is_empty() || w.bot_secret.is_empty() {
                    tracing::error!(
                        "wecom: 长连接凭证缺失（bot_id / WECOM_BOT_SECRET），通道禁用。\
                         运行 rhermes gateway setup 重新配置"
                    );
                    Self::Disabled
                } else {
                    tracing::info!("wecom: stream 模式（智能机器人长连接，免回调）");
                    Self::Stream(Arc::new(WeComStreamChannel::new(config)))
                }
            }
            crate::core::WeComMode::Poll => Self::Poll(Arc::new(poll::WeComPollChannel::new(config))),
        }
    }
}

#[async_trait]
impl Channel for WeComChannel {
    fn start(
        self: Arc<Self>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> JoinHandle<()> {
        match self.as_ref() {
            WeComChannel::Stream(s) => Arc::clone(s).start(inbound_tx, shutdown_rx),
            WeComChannel::Poll(p) => Arc::clone(p).start(inbound_tx, shutdown_rx),
            WeComChannel::Disabled => tokio::spawn(async {
                // 凭证缺失：空转等待关闭信号（通道不收不发）
                let _ = std::future::pending::<()>().await;
            }),
        }
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String> {
        match self {
            WeComChannel::Stream(s) => s.send_message(chat_id, text).await,
            WeComChannel::Poll(p) => p.send_message(chat_id, text).await,
            WeComChannel::Disabled => {
                Err("wecom 通道未配置（缺少 bot_id / WECOM_BOT_SECRET）".into())
            }
        }
    }

    fn name(&self) -> &'static str {
        "wecom"
    }
}
