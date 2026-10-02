//! 企微通道 — 智能机器人长连接（D19，唯一模式）
//!
//! 企业微信只做 API 模式：主动 wss 连出 `openws.work.weixin.qq.com`，
//! `aibot_subscribe` 认证 → `aibot_msg_callback` 推送 → `aibot_respond_msg`
//! 流式回复。零 URL 回调、零公网暴露、零消息加解密。
//! （旧 webhook + 轮询模式已整体移除；存量配置的 `mode = "poll"`
//! 会被 serde `other` 静默映射为 Stream，多余旧字段自动忽略。）
//!
//! 对 `ChannelManager` / `SessionRouter` 侧完全无感——注册名恒为 `"wecom"`。

mod stream;

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::channel::{Channel, InboundMessage};
use crate::core::Config;

pub use stream::{NativeStreamApi, WeComStreamApi, WeComStreamChannel, WsFrame};
pub use stream::{try_connect_blocking, write_bot_secret_to_env};

/// 企微通道门面：凭证齐备走长连接；缺失则禁用并指路（不静默降级）
pub enum WeComChannel {
    Stream(Arc<WeComStreamChannel>),
    /// 凭证缺失禁用（bot_id / WECOM_BOT_SECRET 未配置）
    Disabled,
}

impl WeComChannel {
    pub fn new(config: &Config) -> Self {
        let w = &config.channels.wecom;
        if w.bot_id.is_empty() || w.bot_secret.is_empty() {
            tracing::error!(
                "wecom: 长连接凭证缺失（bot_id / WECOM_BOT_SECRET），通道禁用。\
                 运行 rhermes gateway setup 重新配置"
            );
            Self::Disabled
        } else {
            tracing::info!("wecom: 智能机器人长连接模式（API 模式，免回调）");
            Self::Stream(Arc::new(WeComStreamChannel::new(config)))
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
            WeComChannel::Disabled => tokio::spawn(async {
                // 凭证缺失：空转等待关闭信号（通道不收不发）
                let _ = std::future::pending::<()>().await;
            }),
        }
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String> {
        match self {
            WeComChannel::Stream(s) => s.send_message(chat_id, text).await,
            WeComChannel::Disabled => {
                Err("wecom 通道未配置（缺少 bot_id / WECOM_BOT_SECRET）".into())
            }
        }
    }

    fn name(&self) -> &'static str {
        "wecom"
    }
}
