//! 企微智能机器人长连接通道（D19 stream 模式，原生实现）
//!
//! 协议（官方 101833/101463）：主动 wss 连出 `openws.work.weixin.qq.com`，
//! `aibot_subscribe` 认证（`{bot_id, secret}`，ack `errcode=0` 即认证通过）→
//! 收 `aibot_msg_callback` 推送 → `aibot_respond_msg` 同一 streamId 流式回复
//! （finish=false 打字机 → true 定格）。`ping` 30s 心跳。
//!
//! ## 为什么不用 wecom-aibot-rust-sdk（实测结论，2026-10-02）
//! 同凭证同帧同网络：SDK 订阅被服务端静默 reset；本项目 tokio-tungstenite
//! （rustls、不带 `Sec-WebSocket-Protocol` 头）直连订阅成功（errcode=0）。
//! 差异锁定在 SDK 的握手特征（子协议头/TLS 栈）。故按 D19 §2.1 的隔离
//! 预案自研 `NativeStreamApi`（~300 行）替换之，协议帧与官方完全同构。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::channel::{Channel, InboundMessage};
use crate::core::Config;

// ---------------------------------------------------------------------------
// 协议类型（与官方帧同构；零第三方依赖）
// ---------------------------------------------------------------------------

/// 协议命令
pub mod wscmd {
    pub const SUBSCRIBE: &str = "aibot_subscribe";
    pub const HEARTBEAT: &str = "ping";
    pub const RESPONSE: &str = "aibot_respond_msg";
    pub const SEND_MSG: &str = "aibot_send_msg";
    pub const CALLBACK: &str = "aibot_msg_callback";
    pub const EVENT_CALLBACK: &str = "aibot_event_callback";
}

/// WebSocket 帧（统一信封：`{cmd, headers:{req_id}, body, errcode, errmsg}`）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WsFrame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    pub headers: WsFrameHeaders,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errcode: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errmsg: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WsFrameHeaders {
    pub req_id: String,
}

/// 生成协议 req_id：`{prefix}_{毫秒时间戳}_{8位随机}`（对齐官方 SDK 形态）
fn generate_req_id(prefix: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let rand: String = (0..8)
        .map(|_| {
            let n = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
                % 36) as u8;
            std::char::from_digit(n as u32, 36).unwrap_or('0')
        })
        .collect();
    format!("{prefix}_{ts}_{rand}")
}

// ---------------------------------------------------------------------------
// 连接状态
// ---------------------------------------------------------------------------

/// 连接状态机（std 原子/锁，可在 async 上下文安全轮询读取）
#[derive(Debug, Clone, PartialEq)]
enum LinkState {
    Disconnected,
    Connecting,
    Authenticated,
    /// 认证失败（携带服务端 errmsg 或本地原因）
    Failed(String),
}

struct Shared {
    state: Mutex<LinkState>,
    /// 文本消息回调（parse → frames 记录 → 注入路由；闭包内全同步）
    text_handlers: Mutex<Vec<Arc<dyn Fn(&WsFrame) + Send + Sync>>>,
}

impl Shared {
    fn state(&self) -> LinkState {
        self.state.lock().unwrap().clone()
    }
    fn set_state(&self, s: LinkState) {
        *self.state.lock().unwrap() = s;
    }
}

/// 内部命令（驱动 → 连接任务）
enum Cmd {
    /// 发送一帧（respond / send_msg；已构造完成）
    Send(WsFrame),
    Shutdown,
}

// ---------------------------------------------------------------------------
// 原生长连接客户端
// ---------------------------------------------------------------------------

/// 原生企微长连接实现（替代第三方 SDK；握手特征经真机验证被服务端接受）
pub struct NativeStreamApi {
    shared: Arc<Shared>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    task: JoinHandle<()>,
}

impl NativeStreamApi {
    pub fn new(bot_id: &str, secret: &str, ws_url: &str) -> Self {
        // rustls CryptoProvider：进程级一次性显式安装（幂等）。
        // 依赖树同时存在 aws-lc-rs 与 ring 时"自动确定"必然失败并 panic
        // （真机实测 tokio-rt-worker panic）——显式装 ring 一劳永逸。
        let _ = rustls::crypto::ring::default_provider().install_default();

        let shared = Arc::new(Shared {
            state: Mutex::new(LinkState::Disconnected),
            text_handlers: Mutex::new(Vec::new()),
        });
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_link(
            bot_id.to_string(),
            secret.to_string(),
            ws_url.to_string(),
            Arc::clone(&shared),
            cmd_rx,
        ));
        Self { shared, cmd_tx, task }
    }

    /// 发送一帧（内部）
    fn send_frame(&self, frame: WsFrame) -> Result<(), String> {
        self.cmd_tx
            .send(Cmd::Send(frame))
            .map_err(|_| "长连接任务已退出".to_string())
    }

    /// 构造 respond 帧（透传原帧 req_id + 同一 streamId）
    pub fn respond_stream_frame(frame: &WsFrame, stream_id: &str, content: &str, finish: bool) -> WsFrame {
        WsFrame {
            cmd: Some(wscmd::RESPONSE.into()),
            headers: WsFrameHeaders { req_id: frame.headers.req_id.clone() },
            body: Some(serde_json::json!({
                "msgtype": "stream",
                "stream": { "id": stream_id, "finish": finish, "content": content },
            })),
            errcode: None,
            errmsg: None,
        }
    }

    /// 构造主动发送帧（text）
    pub fn send_text_frame(chatid: &str, text: &str) -> WsFrame {
        WsFrame {
            cmd: Some(wscmd::SEND_MSG.into()),
            headers: WsFrameHeaders { req_id: generate_req_id(wscmd::SEND_MSG) },
            body: Some(serde_json::json!({
                "chatid": chatid,
                "msgtype": "text",
                "text": { "content": text },
            })),
            errcode: None,
            errmsg: None,
        }
    }
}

/// 连接任务：建连 → subscribe → 收发循环 + 心跳 → 断线指数退避重连
async fn run_link(bot_id: String, secret: String, ws_url: String, shared: Arc<Shared>, mut cmd_rx: mpsc::UnboundedReceiver<Cmd>) {
    let mut backoff_secs = 1u64;
    loop {
        shared.set_state(LinkState::Connecting);
        tracing::info!("wecom 长连接: 正在连接 {ws_url}");
        // 已验证被服务端接受的握手特征：项目 tokio-tungstenite（rustls），
        // 不带 Sec-WebSocket-Protocol 头
        let (ws, _) = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("wecom 长连接握手失败: {e}");
                shared.set_state(LinkState::Failed(format!("WS 握手失败: {e}")));
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(30);
                continue;
            }
        };
        backoff_secs = 1;
        let (mut sink, mut read) = ws.split();

        // subscribe 认证
        let sub = WsFrame {
            cmd: Some(wscmd::SUBSCRIBE.into()),
            headers: WsFrameHeaders { req_id: generate_req_id(wscmd::SUBSCRIBE) },
            body: Some(serde_json::json!({ "bot_id": bot_id, "secret": secret })),
            errcode: None,
            errmsg: None,
        };
        let sub_req_id = sub.headers.req_id.clone();
        if sink.send(Message::Text(serde_json::to_string(&sub).unwrap_or_default())).await.is_err() {
            continue; // 发送失败 → 重连
        }

        let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sub_ack_seen = false;

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(txt))) => {
                            let Ok(frame) = serde_json::from_str::<WsFrame>(&txt) else { continue };
                            let rid = frame.headers.req_id.as_str();
                            if rid.starts_with(wscmd::SUBSCRIBE) || (sub_ack_seen && rid.starts_with(wscmd::HEARTBEAT)) {
                                // subscribe / ping 的 ack
                                if rid == sub_req_id {
                                    match frame.errcode.unwrap_or(-1) {
                                        0 => {
                                            shared.set_state(LinkState::Authenticated);
                                            tracing::info!("wecom 长连接: 认证通过（免回调模式）");
                                        }
                                        c => {
                                            let m = frame.errmsg.clone().unwrap_or_default();
                                            shared.set_state(LinkState::Failed(format!(
                                                "服务端拒绝订阅 errcode={c} errmsg={m}\
                                                 （核对 BotID/Secret 配对；Secret 是长连接专用密钥，\
                                                 与接收 URL 的 Token/AESKey 不同）"
                                            )));
                                            let _ = sink.close().await;
                                            // 认证被拒属致命错误：退出任务（不无限重试）
                                            return;
                                        }
                                    }
                                    sub_ack_seen = true;
                                }
                                continue;
                            }
                            match frame.cmd.as_deref() {
                                Some(wscmd::CALLBACK) | Some(wscmd::EVENT_CALLBACK) => {
                                    // 仅分发文本消息（P1 范围）
                                    let is_text = frame.body.as_ref()
                                        .and_then(|b| b.get("msgtype"))
                                        .and_then(|v| v.as_str()) == Some("text");
                                    if is_text {
                                        let handlers = shared.text_handlers.lock().unwrap().clone();
                                        for h in &handlers {
                                            h(&frame);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            tracing::warn!("wecom 长连接断开: {e}");
                            break;
                        }
                        None => {
                            tracing::warn!("wecom 长连接: 对端关闭");
                            break;
                        }
                    }
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(Cmd::Send(frame)) => {
                            if sink.send(Message::Text(serde_json::to_string(&frame).unwrap_or_default())).await.is_err() {
                                break;
                            }
                        }
                        Some(Cmd::Shutdown) | None => {
                            let _ = sink.close().await;
                            shared.set_state(LinkState::Disconnected);
                            return;
                        }
                    }
                }
                _ = heartbeat.tick() => {
                    let ping = WsFrame {
                        cmd: Some(wscmd::HEARTBEAT.into()),
                        headers: WsFrameHeaders { req_id: generate_req_id(wscmd::HEARTBEAT) },
                        body: None, errcode: None, errmsg: None,
                    };
                    if sink.send(Message::Text(serde_json::to_string(&ping).unwrap_or_default())).await.is_err() {
                        break;
                    }
                }
            }
        }
        // 断线 → 退避重连
        shared.set_state(LinkState::Disconnected);
        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

// ---------------------------------------------------------------------------
// API trait（通道消费；保持 D19 隔离边界，未来可再换实现）
// ---------------------------------------------------------------------------

/// 企微长连接协议门面
#[async_trait]
pub trait WeComStreamApi: Send + Sync {
    /// 建连并等待认证（errcode=0 ack）；失败返回服务端原始错误
    async fn connect(&self) -> Result<(), String>;
    /// 注册文本消息回调（同步闭包；解析与转发必须无 await）
    async fn on_text_message<F: Fn(&WsFrame) + Send + Sync + 'static>(&self, handler: F);
    /// 流式回复：透传原帧 req_id + 同一 streamId（finish=false 增量 / true 定格）
    async fn reply_stream(&self, frame: &WsFrame, stream_id: &str, content: &str, finish: bool) -> Result<(), String>;
    /// 主动发送文本（无需原帧；前提：用户先给机器人发过消息）
    async fn send_text(&self, chatid: &str, text: &str) -> Result<(), String>;
    /// 断开并结束连接任务
    async fn disconnect(&self);
}

#[async_trait]
impl WeComStreamApi for NativeStreamApi {
    async fn connect(&self) -> Result<(), String> {
        // 轮询自家原子状态（无 blocking_lock，async 上下文安全）
        for _ in 0..100 {
            match self.shared.state() {
                LinkState::Authenticated => return Ok(()),
                LinkState::Failed(e) => return Err(e),
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        Err("认证未确认（10s 内未收到 subscribe ack）".into())
    }

    async fn on_text_message<F: Fn(&WsFrame) + Send + Sync + 'static>(&self, handler: F) {
        self.shared.text_handlers.lock().unwrap().push(Arc::new(handler));
    }

    async fn reply_stream(&self, frame: &WsFrame, stream_id: &str, content: &str, finish: bool) -> Result<(), String> {
        self.send_frame(Self::respond_stream_frame(frame, stream_id, content, finish))
    }

    async fn send_text(&self, chatid: &str, text: &str) -> Result<(), String> {
        self.send_frame(Self::send_text_frame(chatid, text))
    }

    async fn disconnect(&self) {
        let _ = self.cmd_tx.send(Cmd::Shutdown);
    }
}

impl Drop for NativeStreamApi {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Cmd::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// 通道
// ---------------------------------------------------------------------------

/// 企微智能机器人长连接通道（Channel trait 适配）
pub struct WeComStreamChannel {
    api: NativeStreamApi,
    /// chat_id → 最新入站消息帧（流式回复需透传原帧 req_id）
    latest_frames: Arc<Mutex<HashMap<String, WsFrame>>>,
    /// 白名单（与配置 allow_from 同源；空 = 全放行）
    allow_from: Vec<String>,
}

impl WeComStreamChannel {
    pub fn new(config: &Config) -> Self {
        let w = &config.channels.wecom;
        let ws_url = if w.ws_url.is_empty() {
            "wss://openws.work.weixin.qq.com".to_string()
        } else {
            w.ws_url.clone()
        };
        Self {
            api: NativeStreamApi::new(&w.bot_id, &w.bot_secret, &ws_url),
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
        let content = body.get("text")?.as_object()?.get("content")?.as_str()?.to_string();
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
    pub async fn reply_chat(&self, chat_id: &str, stream_id: &str, content: &str, finish: bool) -> Result<(), String> {
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
            if let Err(e) = self.api.connect().await {
                tracing::error!("WeComStreamChannel: {e}");
                return;
            }

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
                    frames.lock().unwrap().insert(chat_id.clone(), frame.clone());
                    let _ = tx.send(InboundMessage::new("wecom", &chat_id, &content));
                })
                .await;

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
// 向导辅助（D19：配置入口统一长连接）
// ---------------------------------------------------------------------------

/// 试连校验（同步上下文可用）：独立线程 + 多线程 runtime（worker 驱动收发循环），
/// 试连成功后主动断开，不占用机器人的唯一长连接。
pub fn try_connect_blocking(bot_id: &str, secret: &str) -> Result<(), String> {
    let (b, s) = (bot_id.to_string(), secret.to_string());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| format!("创建运行时失败: {e}"))?;
        // 多线程 runtime：block_on 期间/之后 worker 持续驱动收发循环与重连退避
        let result = rt.block_on(async {
            let api = NativeStreamApi::new(&b, &s, "wss://openws.work.weixin.qq.com");
            let r = api.connect().await;
            api.disconnect().await; // 试连即断，不占用唯一连接
            r
        });
        let _ = rt.shutdown_timeout(Duration::from_secs(2));
        result
    })
    .join()
    .map_err(|_| "试连线程异常".to_string())?
}

/// 把 `WECOM_BOT_SECRET` 写入 .env（替换旧行；同时清理旧 WECOM_SECRET 残留）
pub fn write_bot_secret_to_env(config_path: &std::path::Path, secret: &str) -> Result<(), String> {
    let env_path = config_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(".env");
    let existing = std::fs::read_to_string(&env_path).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("WECOM_SECRET=") && !t.starts_with("WECOM_BOT_SECRET=")
        })
        .map(|l| l.to_string())
        .collect();
    if !lines.is_empty() && !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
        lines.push(String::new());
    }
    lines.push(format!("WECOM_BOT_SECRET={secret}"));
    std::fs::write(&env_path, lines.join("\n") + "\n").map_err(|e| format!("写入 .env 失败: {e}"))
}

// ---------------------------------------------------------------------------
// 集成测试：本地 ws mock 模拟企微网关（subscribe ack / 推送 / 收回复帧）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// mock 网关：回 subscribe/ping ack，记录 respond/send 帧
    async fn spawn_mock_gateway() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
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
                        if cmd == wscmd::RESPONSE || cmd == wscmd::SEND_MSG {
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

    /// 全链路：subscribe 认证（原生客户端，非 mock 视角）→ 回复通路两帧（false→true）
    #[tokio::test]
    async fn test_stream_roundtrip_with_mock_gateway() {
        let (url, received) = spawn_mock_gateway().await;
        let channel = WeComStreamChannel::new(&stream_config(&url));

        // 等原生客户端认证通过（mock 回 ack errcode=0）
        for _ in 0..50 {
            if channel.api.shared.state() == LinkState::Authenticated {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(channel.api.shared.state(), LinkState::Authenticated, "mock 应回 subscribe ack");

        // 帧解析（chatid/text/sender）
        let cb = serde_json::from_str::<WsFrame>(&serde_json::json!({
            "cmd": wscmd::CALLBACK,
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

        // 流式回复两帧
        channel.latest_frames.lock().unwrap().insert("CHAT_001".into(), cb);
        let sid = generate_req_id("stream");
        channel.reply_chat("CHAT_001", &sid, "Rust 的所有权是", false).await.unwrap();
        channel.reply_chat("CHAT_001", &sid, "Rust 的所有权是一套内存管理规则……", true).await.unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;
        let frames = received.lock().unwrap().clone();
        assert_eq!(frames.len(), 2, "mock 应收到两帧 respond: {frames:?}");
        let st1 = frames[0].get("stream").unwrap();
        assert_eq!(st1.get("finish").and_then(|v| v.as_bool()), Some(false));
        let st2 = frames[1].get("stream").unwrap();
        assert_eq!(st2.get("finish").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(st1.get("id"), st2.get("id"), "streamId 必须贯穿同一流");

        channel.api.disconnect().await;
    }

    /// 非文本帧/缺字段解析为 None（不进路由）
    #[test]
    fn test_parse_rejects_non_text() {
        let img = serde_json::from_str::<WsFrame>(&serde_json::json!({
            "cmd": wscmd::CALLBACK,
            "headers": { "req_id": "cb_2" },
            "body": { "msgtype": "image", "chatid": "C", "image": { "url": "x" } }
        }).to_string()).unwrap();
        assert!(WeComStreamChannel::parse_text_frame(&img).is_none());

        let no_chat = serde_json::from_str::<WsFrame>(&serde_json::json!({
            "cmd": wscmd::CALLBACK,
            "headers": { "req_id": "cb_3" },
            "body": { "msgtype": "text", "text": { "content": "hi" } }
        }).to_string()).unwrap();
        assert!(WeComStreamChannel::parse_text_frame(&no_chat).is_none());
    }
}
