//! Web 通道 — 学生通过浏览器与 AI 交互
//!
//! 启动一个 axum Web 服务器，提供聊天 UI + REST API。
//! 在通用模式和教育模式下都可用。

use std::sync::Arc;
use std::sync::Mutex;
use std::collections::HashMap;

use axum::{
    extract::{State, ws::{Message, WebSocket, WebSocketUpgrade}},
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
    extract::Path,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::channel::{Channel, ChannelStatus};
use crate::channel::types::InboundMessage;

/// Web 通道
pub struct WebChannel {
    /// 入站消息发送端
    inbound_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<InboundMessage>>>>,
    /// 在线状态
    state: crate::channel::ChannelState,
    /// Web 服务器端口
    port: u16,
    /// 管理 Token
    admin_token: Arc<Mutex<String>>,
}

impl WebChannel {
    pub fn new(port: u16) -> Self {
        Self {
            inbound_tx: Arc::new(Mutex::new(None)),
            state: crate::channel::ChannelState::new(),
            port,
            admin_token: Arc::new(Mutex::new(generate_token())),
        }
    }

    /// 获取管理 Token（用于首次访问认证）
    pub fn admin_token(&self) -> String {
        self.admin_token.lock().unwrap().clone()
    }
}

fn generate_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 共享状态
#[derive(Clone)]
struct WebState {
    inbound_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<InboundMessage>>>>,
    /// 回复消息缓存（chat_id → 最新回复）
    replies: Arc<Mutex<HashMap<String, String>>>,
}

#[async_trait::async_trait]
impl Channel for WebChannel {
    fn name(&self) -> &'static str {
        "web"
    }

    fn start(
        self: Arc<Self>,
        inbound_tx: tokio::sync::mpsc::UnboundedSender<InboundMessage>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        // 保存 inbound_tx
        {
            let mut tx = self.inbound_tx.lock().unwrap();
            *tx = Some(inbound_tx);
        }

        let port = self.port;
        let web_state = WebState {
            inbound_tx: self.inbound_tx.clone(),
            replies: Arc::new(Mutex::new(HashMap::new())),
        };

        tracing::info!("Web 通道启动，端口 {port}");

        tokio::spawn(async move {
            let app = Router::new()
                .route("/", get(index))
                .route("/api/chat", post(chat))
                .route("/api/reply/:chat_id", get(get_reply))
                .with_state(web_state);

            let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
                .await
                .unwrap();

            tokio::select! {
                _ = axum::serve(listener, app) => {
                    tracing::info!("Web 服务器已停止");
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("Web 通道收到关闭信号");
                }
            }
        })
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String> {
        // Web 通道的回复通过 WebSocket/轮询获取，这里不直接发送
        // 实际实现中可以写入共享状态供 /api/reply 读取
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.state.snapshot("web", Some(format!("端口 {}", self.port)))
    }
}

// ---------------------------------------------------------------------------
// HTTP Handlers
// ---------------------------------------------------------------------------

async fn index() -> impl IntoResponse {
    Html(CHAT_HTML)
}

#[derive(Deserialize)]
struct ChatRequest {
    chat_id: String,
    message: String,
    #[serde(default)]
    course_suffix: String,
}

#[derive(Serialize)]
struct ChatResponse {
    success: bool,
    error: Option<String>,
}

async fn chat(
    State(state): State<WebState>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    let tx = {
        let tx = state.inbound_tx.lock().unwrap();
        tx.clone()
    };

    let Some(tx) = tx else {
        return Json(ChatResponse {
            success: false,
            error: Some("Web 通道未就绪".into()),
        });
    };

    let mut inbound = InboundMessage::new("web", &req.chat_id, &req.message);
    if !req.course_suffix.is_empty() {
        inbound.metadata.insert("course_suffix".to_string(), req.course_suffix);
    }

    match tx.send(inbound) {
        Ok(()) => Json(ChatResponse {
            success: true,
            error: None,
        }),
        Err(e) => Json(ChatResponse {
            success: false,
            error: Some(format!("发送失败: {e}")),
        }),
    }
}

async fn get_reply(
    State(state): State<WebState>,
    Path(chat_id): Path<String>,
) -> impl IntoResponse {
    let replies = state.replies.lock().unwrap();
    let reply = replies.get(&chat_id).cloned().unwrap_or_default();
    Json(serde_json::json!({ "reply": reply }))
}

// ---------------------------------------------------------------------------
// 内嵌 HTML（学生聊天 UI）
// ---------------------------------------------------------------------------

const CHAT_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>RHermes AI</title>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, sans-serif; background: #f0f2f5; height: 100vh; display: flex; flex-direction: column; }
.header { background: #1890ff; color: white; padding: 12px 20px; font-size: 18px; font-weight: bold; }
.chat-box { flex: 1; overflow-y: auto; padding: 20px; }
.msg { max-width: 70%; margin-bottom: 12px; padding: 10px 16px; border-radius: 8px; word-wrap: break-word; }
.msg.user { background: #1890ff; color: white; margin-left: auto; }
.msg.ai { background: white; color: #333; border: 1px solid #e0e0e0; }
.input-box { display: flex; padding: 12px; background: white; border-top: 1px solid #e0e0e0; }
.input-box input { flex: 1; padding: 10px; border: 1px solid #d9d9d9; border-radius: 4px; font-size: 14px; }
.input-box button { margin-left: 8px; padding: 10px 24px; background: #1890ff; color: white; border: none; border-radius: 4px; cursor: pointer; font-size: 14px; }
.input-box button:hover { background: #40a9ff; }
.typing { color: #999; font-style: italic; }
</style>
</head>
<body>
<div class="header">RHermes AI 助手</div>
<div class="chat-box" id="chatBox"></div>
<div class="input-box">
  <input type="text" id="msgInput" placeholder="输入消息..." onkeypress="if(event.key==='Enter')send()">
  <button onclick="send()">发送</button>
</div>
<script>
const chatId = 'web_' + Math.random().toString(36).substr(2, 9);
const chatBox = document.getElementById('chatBox');
const msgInput = document.getElementById('msgInput');

function addMsg(text, isUser) {
  const div = document.createElement('div');
  div.className = 'msg ' + (isUser ? 'user' : 'ai');
  div.textContent = text;
  chatBox.appendChild(div);
  chatBox.scrollTop = chatBox.scrollHeight;
}

async function send() {
  const msg = msgInput.value.trim();
  if (!msg) return;
  msgInput.value = '';
  addMsg(msg, true);

  // 显示 typing
  const typing = document.createElement('div');
  typing.className = 'msg ai typing';
  typing.textContent = '正在思考...';
  chatBox.appendChild(typing);

  try {
    const resp = await fetch('/api/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ chat_id: chatId, message: msg })
    });
    const data = await resp.json();
    typing.remove();
    if (data.success) {
      // 轮询回复
      pollReply();
    } else {
      addMsg('错误: ' + (data.error || '未知'), false);
    }
  } catch(e) {
    typing.remove();
    addMsg('网络错误: ' + e, false);
  }
}

let pollCount = 0;
function pollReply() {
  pollCount = 0;
  const interval = setInterval(async () => {
    pollCount++;
    if (pollCount > 120) { clearInterval(interval); return; }
    try {
      const resp = await fetch('/api/reply/' + chatId);
      const data = await resp.json();
      if (data.reply) {
        addMsg(data.reply, false);
        clearInterval(interval);
      }
    } catch(e) {}
  }, 1000);
}
</script>
</body>
</html>"#;
