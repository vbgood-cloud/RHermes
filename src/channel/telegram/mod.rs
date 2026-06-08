//! Telegram Bot 通道
//!
//! 通过 Long Polling 接收消息，sendMessage 发送回复。
//! 复用 create_proxied_client 工厂走代理。

mod api;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::channel::{Channel, InboundMessage};
use crate::core::Config;
use api::{TelegramApi, TgError};

pub struct TelegramChannel {
    api: TelegramApi,
    allowed_chats: Vec<String>,
    poll_timeout: u32,
}

impl TelegramChannel {
    pub fn new(config: &Config) -> Result<Self, String> {
        let token = if !config.channels.telegram.bot_token.is_empty() {
            config.channels.telegram.bot_token.clone()
        } else {
            return Err("Telegram bot_token 未配置（config.toml 或 .env 的 TELEGRAM_BOT_TOKEN）".into());
        };

        let poll_timeout = config.channels.telegram.poll_timeout_secs;
        let client = crate::core::http_client::create_proxied_client(
            &config.proxy,
            "telegram",
            Duration::from_secs(poll_timeout as u64 + 10),
        );

        let api = TelegramApi::new(client, token);

        Ok(Self {
            api,
            allowed_chats: config.channels.telegram.allowed_chats.clone(),
            poll_timeout,
        })
    }

    fn is_allowed(&self, chat_id: &str) -> bool {
        if self.allowed_chats.is_empty() { return true; }
        self.allowed_chats.iter().any(|a| a == chat_id)
    }

    fn format_sender(msg: &api::TgMessage) -> String {
        if let Some(user) = &msg.from {
            let name = &user.first_name;
            match &user.username {
                Some(uname) => format!("{name} (@{uname})"),
                None => name.clone(),
            }
        } else {
            "unknown".into()
        }
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn start(
        self: Arc<Self>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        tracing::info!("Telegram Long Polling 启动");

        tokio::spawn(async move {
            // 启动时验证 Bot Token
            match self.api.get_me().await {
                Ok(bot) => {
                    tracing::info!("Telegram Bot 已连接: {} (@{})", bot.first_name, bot.username);
                }
                Err(e) => {
                    tracing::error!("Telegram Bot 验证失败: {e}");
                    return;
                }
            }

            let mut offset = 0i64;

            loop {
                // 检查 shutdown 信号
                if shutdown_rx.try_recv().is_ok() {
                    tracing::info!("Telegram 通道收到 shutdown 信号");
                    break;
                }

                match self.api.get_updates(offset, self.poll_timeout).await {
                    Ok(updates) => {
                        for update in updates {
                            offset = update.update_id + 1;

                            if let Some(msg) = &update.message {
                                if let Some(text) = &msg.text {
                                    let chat_id = msg.chat.id.to_string();

                                    if !self.is_allowed(&chat_id) {
                                        tracing::warn!(
                                            "Telegram 消息被过滤 (chat_id={})",
                                            chat_id
                                        );
                                        continue;
                                    }

                                    tracing::info!(
                                        "Telegram 收到消息: [{}] {}",
                                        Self::format_sender(msg),
                                        if text.len() > 50 {
                                            format!("{}...", &text[..50])
                                        } else {
                                            text.clone()
                                        }
                                    );

                                    let inbound = InboundMessage::new(
                                        "telegram",
                                        &chat_id,
                                        text.clone(),
                                    );

                                    if inbound_tx.send(inbound).is_err() {
                                        tracing::error!("Telegram inbound_tx 已关闭");
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Err(TgError::Http(e)) if e.is_timeout() => {
                        // Long Polling 超时是正常的，继续下一轮
                        continue;
                    }
                    Err(e) => {
                        tracing::error!("Telegram getUpdates 失败: {e}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }

            tracing::info!("Telegram 通道已停止");
        })
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String> {
        if text.len() <= 4096 {
            self.api.send_message(chat_id, text, None).await
                .map_err(|e| format!("{e}"))
        } else {
            let mut remaining = text;
            while !remaining.is_empty() {
                let cut = if remaining.len() > 4000 {
                    let mut pos = 4000;
                    while !remaining.is_char_boundary(pos) && pos > 0 {
                        pos -= 1;
                    }
                    pos
                } else {
                    remaining.len()
                };
                let chunk = &remaining[..cut];
                self.api.send_message(chat_id, chunk, None).await
                    .map_err(|e| format!("{e}"))?;
                remaining = &remaining[cut..];
            }
            Ok(())
        }
    }

    fn name(&self) -> &'static str {
        "telegram"
    }
}
