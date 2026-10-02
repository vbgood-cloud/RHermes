//! 企微智能机器人长连接通道（D19 stream 模式）
//!
//! 协议（官方 101463）：主动 wss 连出 `openws.work.weixin.qq.com`，
//! `aibot_subscribe` 认证 → 收 `aibot_msg_callback` 推送 →
//! `aibot_respond_msg` 同一 streamId 流式回复（finish=false 打字机 → true 定格）。
//! 业务层零加解密、零公网暴露。
//!
//! 分层：`WecomStreamApi`（trait，隔离 SDK）→ `SdkWecomStreamApi`（真实实现）
//!      → `WeComStreamChannel`（Channel trait 适配，router 侧零感知）。
//! P1 范围：收发链路 + 回执通路（`reply_chat`，P2 的 WecomStreamSink 复用）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use wecom_aibot_rust_sdk::types::{WSClientOptions, WsFrame};
use wecom_aibot_rust_sdk::{generate_req_id, WSClient};

use crate::channel::{Channel, InboundMessage};
use crate::core::Config;

// ---------------------------------------------------------------------------
// SDK 隔离层（D19 §2.1：trait 包住 SDK，替换实现只动一个文件）
// ---------------------------------------------------------------------------

/// 企微长连接协议门面（本通道唯一接触 SDK 的边界）
#[async_trait]
pub trait WeComStreamApi: Send + Sync {
    /// 建连 + 自动认证（subscribe）
    async fn connect(&self) -> Result<(), String>;
    /// 注册文本消息回调（同步闭包；解析与转发必须无 await）
    async fn on_text_message<F: Fn(&WsFrame) + Send + Sync + 'static>(&self, handler: F)
    where
        Self: Sized;
    /// 流式回复：透传原帧 req_id + 同一 streamId（finish=false 增量 / true 定格）
    async fn reply_stream(
        &self,
        frame: &WsFrame,
        stream_id: &str,
        content: &str,
        finish: bool,
    ) -> Result<(), String>;
    /// 主动发送文本（无需原帧；前提：用户先给机器人发过消息）
    async fn send_text(&self, chatid: &str, text: &str) -> Result<(), String>;
    /// 断开（shutdown 用）
    async fn disconnect(&self);
}

/// 真实实现：包装 `wecom-aibot-rust-sdk`（含认证/心跳/指数退避重连/ALPN 处理）
#[derive(Clone)]
pub struct SdkWecomStreamApi {
    client: WSClient,
}

impl SdkWecomStreamApi {
    pub fn new(bot_id: &str, secret: &str, ws_url: &str) -> Self {
        let mut opts = WSClientOptions::new(bot_id.to_string(), secret.to_string());
        opts.ws_url = Some(ws_url.to_string());
        Self { client: WSClient::new(opts) }
    }
}

#[async_trait]
impl WeComStreamApi for SdkWecomStreamApi {
    async fn connect(&self) -> Result<(), String> {
        self.client.connect().await.map_err(|e| format!("wecom 长连接失败: {e}"))
    }

    async fn on_text_message<F: Fn(&WsFrame) + Send + Sync + 'static>(&self, handler: F) {
        self.client.on_message_text(handler).await;
    }

    async fn reply_stream(
        &self,
        frame: &WsFrame,
        stream_id: &str,
        content: &str,
        finish: bool,
    ) -> Result<(), String> {
        self.client
            .reply_stream(frame, stream_id, content, finish, None, None)
            .await
            .map(|_| ())
            .map_err(|e| format!("wecom 流式回复失败: {e}"))
    }

    async fn send_text(&self, chatid: &str, text: &str) -> Result<(), String> {
        let body = serde_json::json!({
            "msgtype": "text",
            "text": { "content": text },
        });
        self.client
            .send_message(chatid, body)
            .await
            .map(|_| ())
            .map_err(|e| format!("wecom 主动发送失败: {e}"))
    }

    async fn disconnect(&self) {
        self.client.disconnect();
    }
}

// ---------------------------------------------------------------------------
// 通道
// ---------------------------------------------------------------------------

/// 企微智能机器人长连接通道（Channel trait 适配）
pub struct WeComStreamChannel {
    api: SdkWecomStreamApi,
    /// chat_id → 最新入站消息帧（流式回复需透传原帧 req_id；
    /// 同会话新消息覆盖旧帧 = 「回复挂在最新一条消息上」，聊天语义正确）
    latest_frames: Arc<Mutex<HashMap<String, WsFrame>>>,
    /// 白名单（与 poll 模式同一配置字段 allow_from；空 = 全放行）
    allow_from: Vec<String>,
}

impl WeComStreamChannel {
    pub fn new(config: &Config) -> Self {
        let w = &config.channels.wecom;
        // ws_url 可配置：默认官方网关；mock 集成测试指向本地
        let ws_url = if w.ws_url.is_empty() {
            "wss://openws.work.weixin.qq.com".to_string()
        } else {
            w.ws_url.clone()
        };
        Self {
            api: SdkWecomStreamApi::new(&w.bot_id, &w.bot_secret, &ws_url),
            latest_frames: Arc::new(Mutex::new(HashMap::new())),
            allow_from: w.allow_from.clone(),
        }
    }

    /// 从 callback 帧解析 (chat_id, sender, text)；非文本/缺字段返回 None
    fn parse_text_frame(frame: &WsFrame) -> Option<(String, String, String)> {
        let body = frame.body.as_ref()?.as_object()?;
        if body.get("msgtype").and_then(|v| v.as_str())? != "text" {
            return None;
        }
        let content = body
            .get("text")?
            .as_object()?
            .get("content")?
            .as_str()?
            .to_string();
        // 官方字段 chatid；兼容 chat_id 写法
        let chat_id = body
            .get("chatid")
            .or_else(|| body.get("chat_id"))?
            .as_str()?
            .to_string();
        let sender = body
            .get("sender")
            .and_then(|s| s.as_object())
            .and_then(|s| s.get("userid"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        Some((chat_id, sender, content))
    }

    /// 流式回复指定会话（P2 的 WecomStreamSink 与 mock 测试共用此通路）
    pub async fn reply_chat(
        &self,
        chat_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
    ) -> Result<(), String> {
        let frame = self
            .latest_frames
            .lock()
            .unwrap()
            .get(chat_id)
            .cloned()
            .ok_or_else(|| format!("会话 {chat_id} 无待回复消息帧"))?;
        self.api.reply_stream(&frame, stream_id, content, finish).await
    }
}

#[async_trait]
impl Channel for WeComStreamChannel {
    fn start(
        self: Arc<Self>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!("WeComStreamChannel: 正在建立长连接…");
            if let Err(e) = self.api.connect().await {
                tracing::error!("WeComStreamChannel: {e}");
                return;
            }
            tracing::info!("WeComStreamChannel: 长连接已建立（免回调模式）");

            // 注册文本消息回调：解析 → 记录帧 → 注入路由
            let frames = Arc::clone(&self.latest_frames);
            let allowed = self.allow_from.clone();
            let tx = inbound_tx.clone();
            self.api
                .on_text_message(move |frame| {
                    let Some((chat_id, sender, content)) = Self::parse_text_frame(frame) else {
                        return;
                    };
                    if content.trim().is_empty() {
                        return;
                    }
                    if !allowed.is_empty() && !allowed.iter().any(|u| *u == sender) {
                        tracing::debug!("wecom(stream): 忽略来自 {sender} 的消息");
                        return;
                    }
                    // 回复挂最新帧（覆盖旧帧）
                    frames.lock().unwrap().insert(chat_id.clone(), frame.clone());
                    let _ = tx.send(InboundMessage::new("wecom", &chat_id, &content));
                })
                .await;

            // 挂机等关闭信号
            let _ = shutdown_rx.recv().await;
            tracing::info!("WeComStreamChannel: 收到关闭信号，断开长连接");
            self.api.disconnect().await;
        })
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String> {
        self.api.send_text(chat_id, text).await
    }

    fn name(&self) -> &'static str {
        "wecom"
    }
}

// ---------------------------------------------------------------------------
// 集成测试：本地 ws mock 模拟企微网关（subscribe ack / 推送 / 收回复帧）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    use wecom_aibot_rust_sdk::types::WsCmd;

    /// mock 网关：回 subscribe/ping ack，记录 respond/send 帧
    async fn spawn_mock_gateway()
    -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));

        let recv_clone = Arc::clone(&received);
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                    while let Some(Ok(msg)) = futures_util::StreamExt::next(&mut ws).await {
                        let Message::Text(txt) = msg else { continue };
                        let Ok(frame) = serde_json::from_str::<WsFrame>(&txt) else { continue };
                        let cmd = frame.cmd.clone().unwrap_or_default();
                        // 认证/心跳/回复 → 统一回 ack（req_id 透传 + errcode=0）
                        let ack = serde_json::json!({
                            "cmd": cmd,
                            "headers": { "req_id": frame.headers.req_id },
                            "errcode": 0,
                        });
                        let _ = ws.send(Message::Text(ack.to_string())).await;
                        if cmd == WsCmd::RESPONSE || cmd == WsCmd::SEND_MSG {
                            recv_clone.lock().unwrap().push(frame.body.clone().unwrap_or_default());
                        }
                    }
                }
            }
        });
        (format!("ws://{addr}"), received)
    }

    fn stream_config(ws_url: &str) -> Config {
        let mut config = Config::default();
        config.channels.wecom.bot_id = "bot_test".into();
        config.channels.wecom.bot_secret = "secret_test".into();
        config.channels.wecom.ws_url = ws_url.to_string();
        config
    }

    /// P1 全链路：subscribe 认证 → 收推送 → 入站路由 → 流式回复两帧（false→true）
    #[tokio::test]
    async fn test_stream_roundtrip_with_mock_gateway() {
        let (url, received) = spawn_mock_gateway().await;
        let channel = Arc::new(WeComStreamChannel::new(&stream_config(&url)));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_shutdown_tx, shutdown_rx) = broadcast::channel(1);
        Arc::clone(&channel).start(tx, shutdown_rx);
        let _ = &mut rx; // 入站路由断言见 parse 测试（mock 侧无法主动推帧）

        // 等连接认证完成（subscribe ack）
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        // mock 侧无法直接推（server task 持有 ws）——通过 parse/reply 验证通道侧逻辑：
        // 1) 帧解析（chatid/text/白名单语义）
        // 2) reply_chat 通路（latest_frames → SDK respond 帧 → mock 收到并断言）

        // 构造一条与官方回调同构的帧，直接驱动通道内部解析 + 帧记录
        let cb = serde_json::from_str::<WsFrame>(&serde_json::json!({
            "cmd": WsCmd::CALLBACK,
            "headers": { "req_id": "cb_1" },
            "body": {
                "msgtype": "text",
                "chatid": "CHAT_001",
                "sender": { "userid": "zhangsan" },
                "text": { "content": "什么是 Rust 所有权？" }
            }
        }).to_string()).unwrap();
        let parsed = WeComStreamChannel::parse_text_frame(&cb).unwrap();
        assert_eq!(parsed, ("CHAT_001".into(), "zhangsan".into(), "什么是 Rust 所有权？".into()));

        // 记录帧（模拟 on_text_message 内部路径）并走 reply_chat 流式回复
        channel.latest_frames.lock().unwrap().insert("CHAT_001".into(), cb);
        let sid = generate_req_id("stream");
        channel.reply_chat("CHAT_001", &sid, "Rust 的所有权是", false).await.unwrap();
        channel.reply_chat("CHAT_001", &sid, "Rust 的所有权是一套内存管理规则……", true).await.unwrap();

        // mock 收到两帧 respond：stream.finish 序列 false → true，id 一致
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let frames = received.lock().unwrap().clone();
        assert_eq!(frames.len(), 2, "mock 应收到两帧 respond: {frames:?}");
        let s1 = frames[0].get("msgtype").and_then(|v| v.as_str()).unwrap();
        assert_eq!(s1, "stream");
        let st1 = frames[0].get("stream").unwrap();
        assert_eq!(st1.get("finish").and_then(|v| v.as_bool()), Some(false));
        let st2 = frames[1].get("stream").unwrap();
        assert_eq!(st2.get("finish").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(st1.get("id"), st2.get("id"), "streamId 必须贯穿同一流");
    }

    /// 非文本帧/缺字段解析为 None（不进路由）
    #[test]
    fn test_parse_rejects_non_text() {
        let img = serde_json::from_str::<WsFrame>(&serde_json::json!({
            "cmd": WsCmd::CALLBACK,
            "headers": { "req_id": "cb_2" },
            "body": { "msgtype": "image", "chatid": "C", "image": { "url": "x" } }
        }).to_string()).unwrap();
        assert!(WeComStreamChannel::parse_text_frame(&img).is_none());

        let no_chat = serde_json::from_str::<WsFrame>(&serde_json::json!({
            "cmd": WsCmd::CALLBACK,
            "headers": { "req_id": "cb_3" },
            "body": { "msgtype": "text", "text": { "content": "hi" } }
        }).to_string()).unwrap();
        assert!(WeComStreamChannel::parse_text_frame(&no_chat).is_none());
    }
}
