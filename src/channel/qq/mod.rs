//! QQ Bot 通道
//!
//! 通过 QQ 官方 Bot API 收发消息。
//! - 接收: WebSocket Gateway（事件推送）
//! - 发送: REST API（群聊 + C2C 私聊）

pub mod api;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::channel::{Channel, ChannelStatus, InboundMessage};
use crate::core::Config;

use api::{GroupMessage, QqApi, QqError};

pub struct QqChannel {
    api: QqApi,
    allowed_groups: Vec<String>,
    allow_private: bool,
    state: crate::channel::ChannelState,
}

impl QqChannel {
    pub fn new(config: &Config) -> Result<Self, String> {
        let qq = &config.channels.qq;
        if qq.app_id.is_empty() || qq.app_secret.is_empty() {
            return Err("QQ Bot 未配置（在 .env 中设置 QQ_BOT_APP_ID 和 QQ_BOT_APP_SECRET）".into());
        }

        Ok(Self {
            api: QqApi::new(&qq.app_id, &qq.app_secret, qq.sandbox),
            allowed_groups: qq.allowed_groups.clone(),
            allow_private: qq.allow_private_chat,
            state: crate::channel::ChannelState::new(),
        })
    }

    fn is_group_allowed(&self, group_openid: &str) -> bool {
        if self.allowed_groups.is_empty() {
            return true;
        }
        self.allowed_groups.iter().any(|g| g == group_openid)
    }
}

#[async_trait]
impl Channel for QqChannel {
    fn name(&self) -> &'static str {
        "qq"
    }

    fn start(
        self: Arc<Self>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        tracing::info!("QQ 通道启动");

        tokio::spawn(async move {
            // 获取 Gateway URL
            let gateway_url = match self.api.get_gateway().await {
                Ok(url) => {
                    tracing::info!("QQ Gateway: {url}");
                    self.state.set_connected(true);
                    self.state.clear_error();
                    url
                }
                Err(e) => {
                    tracing::error!("QQ 获取 Gateway 失败: {e}");
                    self.state.set_error(format!("Gateway 获取失败: {e}"));
                    return;
                }
            };

            // 获取 access_token
            let token = match self.api.get_token().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("QQ 获取 Token 失败: {e}");
                    self.state.set_error(format!("Token 获取失败: {e}"));
                    return;
                }
            };

            // 连接 WebSocket
            let ws_url = gateway_url.replace("https://", "wss://").replace("http://", "ws://");

            loop {
                if shutdown_rx.try_recv().is_ok() {
                    tracing::info!("QQ 通道收到关闭信号");
                    break;
                }

                let ws_stream = match tokio_tungstenite::connect_async(&ws_url).await {
                    Ok((s, _)) => s,
                    Err(e) => {
                        tracing::error!("QQ WebSocket 连接失败: {e}");
                        self.state.set_error(format!("WS 连接失败: {e}"));
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };

                tracing::info!("QQ WebSocket 已连接");

                let (mut ws_sender, mut ws_receiver) = ws_stream.split();
                let (hb_tx, mut hb_rx) = mpsc::unbounded_channel::<()>();
                let mut heartbeat_interval = 30u64;
                let mut session_id = String::new();
                let mut last_seq: Option<u64> = None;
                let mut identified = false;

                // 心跳 token
                let hb_token = token.clone();
                let hb_api = self.clone();

                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx.recv() => {
                            tracing::info!("QQ 通道收到关闭信号");
                            break;
                        }
                        Some(_) = hb_rx.recv() => {
                            // 发送心跳
                            let heartbeat = serde_json::json!({"op": 1, "d": null});
                            if let Ok(hb) = serde_json::to_string(&heartbeat) {
                                if ws_sender.send(WsMessage::Text(hb.into())).await.is_err() {
                                    tracing::warn!("QQ 心跳发送失败");
                                    break;
                                }
                            }
                            continue;
                        }
                        msg_result = ws_receiver.next() => {
                            let Some(msg_result) = msg_result else { break; };

                    let msg = match msg_result {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!("QQ WS 读取错误: {e}");
                            break;
                        }
                    };

                    let text = match msg {
                        WsMessage::Text(t) => t.to_string(),
                        WsMessage::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                        WsMessage::Ping(_) => continue,
                        WsMessage::Close(_) => {
                            tracing::info!("QQ WS 连接关闭");
                            break;
                        }
                        _ => continue,
                    };

                    // 解析事件
                    let event: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let op = event.get("op").and_then(|v| v.as_u64()).unwrap_or(0);
                    match op {
                        10 => {
                            // Hello — 获取心跳间隔
                            if let Some(d) = event.get("d") {
                                heartbeat_interval =
                                    d.get("heartbeat_interval").and_then(|v| v.as_u64()).unwrap_or(30000);
                            }
                            // 发送 Identify
                            let identify = serde_json::json!({
                                "op": 2,
                                "d": {
                                    "token": format!("QQBot {hb_token}"),
                                    "intents": 1 | (1 << 25) | (1 << 30),
                                }
                            });
                            if let Ok(identify_str) = serde_json::to_string(&identify) {
                                let _ = ws_sender.send(WsMessage::Text(identify_str.into())).await;
                            }
                            // 标记已识别，启动定时心跳（通过单独的 channel）
                            let interval_secs = (heartbeat_interval / 1000).max(10);
                            let _hb_tx = hb_tx.clone();
                            tokio::spawn(async move {
                                loop {
                                    tokio::time::sleep(Duration::from_secs(interval_secs)).await;
                                    if _hb_tx.send(()).is_err() { break; }
                                }
                            });
                        }
                        0 => {
                            // Dispatch — 事件推送
                            let event_type = event.get("t").and_then(|v| v.as_str()).unwrap_or("");
                            let seq = event.get("s").and_then(|v| v.as_u64());
                            last_seq = seq;

                            match event_type {
                                "READY" => {
                                    if let Some(d) = event.get("d") {
                                        session_id = d.get("session_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or_default()
                                            .to_string();
                                    }
                                    identified = true;
                                    tracing::info!("QQ Bot 已就绪 (session: {session_id})");
                                }
                                "GROUP_AT_MESSAGE_CREATE" => {
                                    if let Some(d) = event.get("d") {
                                        if let Ok(group_msg) = serde_json::from_value::<GroupMessage>(d.clone()) {
                                            self.handle_group_message(&group_msg, &inbound_tx, &hb_api);
                                        }
                                    }
                                }
                                "C2C_MESSAGE_CREATE" => {
                                    if let Some(d) = event.get("d") {
                                        let content = d.get("content").and_then(|v| v.as_str()).unwrap_or("");
                                        let user_openid = d.get("author")
                                            .and_then(|a| a.get("user_openid"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();
                                        let msg_id = d.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();

                                        if hb_api.allow_private {
                                            hb_api.state.inc_msg();
                                            let inbound = InboundMessage::new("qq", &user_openid, content);
                                            if inbound_tx.send(inbound).is_err() {
                                                tracing::warn!("QQ inbound_tx 已关闭");
                                                break;
                                            }
                                        }
                                    }
                                }
                                _ => {
                                    tracing::debug!("QQ 事件: {event_type}");
                                }
                            }
                        }
                        11 => {
                            // Heartbeat ACK
                            tracing::trace!("QQ 心跳 ACK");
                        }
                        _ => {
                            tracing::debug!("QQ OP: {op}");
                        }
                    }
                    }
                    }  // end select
                }  // end loop

                tracing::warn!("QQ WebSocket 断开，5 秒后重连...");
                self.state.set_connected(false);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }

            tracing::info!("QQ 通道已停止");
        })
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String> {
        // chat_id 可能是 group_openid 或 user_openid
        // 优先尝试群消息，失败再试 C2C
        match self.api.send_group_message(chat_id, text, None).await {
            Ok(()) => Ok(()),
            Err(QqError::Api(_)) => {
                // 可能不是群，试 C2C
                self.api
                    .send_c2c_message(chat_id, text, None)
                    .await
                    .map_err(|e| format!("{e}"))
            }
            Err(e) => Err(format!("{e}")),
        }
    }

    fn status(&self) -> ChannelStatus {
        let detail = if self.state_connected() {
            Some("已连接".to_string())
        } else {
            Some("未连接".to_string())
        };
        self.state.snapshot("qq", detail)
    }
}

// 辅助方法（避免 trait 中 &self 与 &Arc<Self> 冲突）
impl QqChannel {
    fn state_connected(&self) -> bool {
        false // 由 state 原子变量管理，这里简化
    }

    fn handle_group_message(
        &self,
        msg: &GroupMessage,
        inbound_tx: &mpsc::UnboundedSender<InboundMessage>,
        _channel: &Arc<Self>,
    ) {
        if !self.is_group_allowed(&msg.group_openid) {
            tracing::debug!("QQ 群 {} 被过滤", msg.group_openid);
            return;
        }

        // 去掉 @bot 的内容（QQ 中 @bot 会在内容前后加空格）
        let content = msg.content.trim().to_string();
        if content.is_empty() {
            return;
        }

        self.state.inc_msg();
        let chat_id = msg.group_openid.clone();
        let mut inbound = InboundMessage::new("qq", &chat_id, &content);
        inbound.metadata.insert("msg_id".to_string(), msg.id.clone());
        if let Some(ref author) = msg.author {
            if let Some(ref openid) = author.member_openid {
                inbound.metadata.insert("sender_id".to_string(), openid.clone());
            }
        }

        if inbound_tx.send(inbound).is_err() {
            tracing::warn!("QQ inbound_tx 已关闭");
        }
    }
}
