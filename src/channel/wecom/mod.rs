//! 企业微信（WeCom）Bot 通道
//!
//! 通过企业微信群机器人 Webhook 发送消息，通过应用消息推送接收消息。
//!
//! ## 配置
//! ```toml
//! [channels.wecom]
//! enabled = true
//! webhook_url = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=xxx"
//! corp_id = "ww..."
//! agent_id = "1000001"
//! poll_interval_secs = 5
//! ```
//!
//! ## .env
//! WECOM_SECRET=xxx  # 应用 Secret

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

use crate::channel::{Channel, InboundMessage};
use crate::core::Config;

// ---------------------------------------------------------------------------
// API 响应结构
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    errcode: i32,
    errmsg: String,
    access_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    errcode: i32,
    errmsg: String,
}

#[derive(Debug, Deserialize)]
struct MessageListResponse {
    errcode: i32,
    errmsg: String,
    msg_list: Option<Vec<WeComMessage>>,
}

#[derive(Debug, Deserialize)]
struct WeComMessage {
    msgid: Option<String>,
    sender: String,
    msgtype: String,
    text: Option<TextContent>,
    create_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TextContent {
    content: String,
}

#[derive(Debug, Serialize)]
struct WebhookPayload {
    msgtype: String,
    text: WebhookText,
}

#[derive(Debug, Serialize)]
struct WebhookText {
    content: String,
}

// ---------------------------------------------------------------------------
// WeComChannel
// ---------------------------------------------------------------------------

/// 企业微信 Bot 通道
pub struct WeComChannel {
    config: Arc<Config>,
    client: Client,
}

impl WeComChannel {
    /// 创建新的企业微信通道
    pub fn new(config: &Config) -> Self {
        let client = crate::core::http_client::create_proxied_client(
            &config.proxy, "wecom", Duration::from_secs(15),
        );
        Self {
            config: Arc::new(config.clone()),
            client,
        }
    }

    /// 通过 Webhook 发送文本消息
    async fn send_webhook(&self, text: &str) -> Result<(), String> {
        let wecom = &self.config.channels.wecom;
        if wecom.webhook_url.is_empty() {
            return Err("企业微信 Webhook URL 未配置".into());
        }

        // 企业微信单个消息最长 4096 字节
        for chunk in split_text(text, 4000) {
            let payload = WebhookPayload {
                msgtype: "text".into(),
                text: WebhookText { content: chunk },
            };

            let resp = self
                .client
                .post(&wecom.webhook_url)
                .json(&payload)
                .send()
                .await
                .map_err(|e| format!("Webhook 发送失败: {e}"))?;

            let result: MessageResponse = resp
                .json()
                .await
                .map_err(|e| format!("Webhook 响应解析失败: {e}"))?;

            if result.errcode != 0 {
                return Err(format!("Webhook 返回错误: {} - {}", result.errcode, result.errmsg));
            }
        }
        Ok(())
    }

    /// 获取 access_token（用于拉取消息）
    async fn get_access_token(&self) -> Result<String, String> {
        let wecom = &self.config.channels.wecom;
        if wecom.corp_id.is_empty() || wecom.secret.is_empty() {
            return Err("企业微信 CorpID 或 Secret 未配置".into());
        }

        let url = format!(
            "https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid={}&corpsecret={}",
            wecom.corp_id, wecom.secret
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("获取 access_token 失败: {e}"))?;

        let result: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Token 响应解析失败: {e}"))?;

        if result.errcode != 0 {
            return Err(format!("Token 错误: {} - {}", result.errcode, result.errmsg));
        }

        result.access_token.ok_or_else(|| "Token 响应中缺少 access_token".into())
    }

    /// 轮询消息
    async fn poll_messages(&self, token: &str) -> Result<Vec<WeComMessage>, String> {
        let wecom = &self.config.channels.wecom;
        let url = format!(
            "https://qyapi.weixin.qq.com/cgi-bin/message/list?access_token={}&agentid={}",
            token, wecom.agent_id
        );

        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| format!("消息轮询失败: {e}"))?;

        let result: MessageListResponse = resp
            .json()
            .await
            .map_err(|e| format!("消息响应解析失败: {e}"))?;

        if result.errcode != 0 {
            return Err(format!("消息查询错误: {} - {}", result.errcode, result.errmsg));
        }

        Ok(result.msg_list.unwrap_or_default())
    }

    /// 检查是否允许来自该 sender 的消息
    fn is_allowed(&self, sender: &str) -> bool {
        let allow_from = &self.config.channels.wecom.allow_from;
        if allow_from.is_empty() {
            return true;
        }
        allow_from.iter().any(|a| a == sender)
    }
}

#[async_trait]
impl Channel for WeComChannel {
    fn start(
        self: Arc<Self>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        let has_poll_config = !self.config.channels.wecom.secret.is_empty()
            && !self.config.channels.wecom.corp_id.is_empty();

        tokio::spawn(async move {
            tracing::info!("WeComChannel: 已启动 (poll={})", has_poll_config);

            if !has_poll_config {
                // 没有轮询配置，仅 Webhook 发送可用
                std::future::pending::<()>().await;
                return;
            }

            let poll_interval = Duration::from_secs(self.config.channels.wecom.poll_interval_secs);
            let mut token: Option<String> = None;
            let mut token_expires = std::time::Instant::now();

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        tracing::info!("WeComChannel: 收到关闭信号");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        // 每隔 poll_interval 秒轮询一次
                        let elapsed = token_expires.elapsed();
                        if elapsed < Duration::from_secs(self.config.channels.wecom.poll_interval_secs) {
                            continue;
                        }
                        token_expires = std::time::Instant::now();

                        // 刷新 token（每 7200 秒过期）
                        if token.is_none() || elapsed >= Duration::from_secs(7000) {
                            match self.get_access_token().await {
                                Ok(t) => {
                                    tracing::debug!("WeCom: 获取 access_token 成功");
                                    token = Some(t);
                                }
                                Err(e) => {
                                    tracing::warn!("WeCom: 获取 token 失败: {e}");
                                    continue;
                                }
                            }
                        }

                        // 拉取消息
                        if let Some(ref tk) = token {
                            match self.poll_messages(tk).await {
                                Ok(msgs) => {
                                    for msg in msgs {
                                        if msg.msgtype != "text" {
                                            continue;
                                        }
                                        if !self.is_allowed(&msg.sender) {
                                            tracing::debug!("WeCom: 忽略来自 {} 的消息", msg.sender);
                                            continue;
                                        }
                                        let content = msg.text.as_ref()
                                            .map(|t| t.content.clone())
                                            .unwrap_or_default();
                                        if content.is_empty() {
                                            continue;
                                        }
                                        let inbound = InboundMessage::new(
                                            "wecom",
                                            &msg.sender,
                                            &content,
                                        );
                                        if inbound_tx.send(inbound).is_err() {
                                            tracing::warn!("WeCom: inbound_tx 已关闭");
                                            break;
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("WeCom: 消息轮询失败: {e}");
                                    // token 可能过期，清除以刷新
                                    token = None;
                                }
                            }
                        }
                    }
                }
            }

            tracing::info!("WeComChannel: 已停止");
        })
    }

    async fn send_message(&self, _chat_id: &str, text: &str) -> Result<(), String> {
        self.send_webhook(text).await
    }

    fn name(&self) -> &'static str {
        "wecom"
    }
}

/// 按长度拆分文本（微信单条消息限制）
fn split_text(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut split_at = max_len.min(remaining.len());
        while !remaining.is_char_boundary(split_at) {
            split_at -= 1;
        }
        if split_at == 0 {
            split_at = remaining.len();
        }
        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }
    chunks
}
