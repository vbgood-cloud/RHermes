//! MCP HTTP/SSE 传输层
//!
//! 支持两种 HTTP 传输方式：
//! - SSE 模式：GET 建立长连接，SSE 流接收响应（标准 MCP 协议）
//! - 直连 POST 模式：直接 POST JSON-RPC，从响应体获取结果（兼容模式）

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::Mutex;

use super::transport::McpError;

// ---------------------------------------------------------------------------
// SSE 事件解析
// ---------------------------------------------------------------------------

/// 解析一个 SSE block（按 \n\n 分割后的内容），返回 (event_type, data)
///
/// 支持标准 SSE 字段：
/// - event: 事件类型（默认空串代表 message）
/// - data: 数据内容（多行 data 用 \n 连接，符合 SSE 标准）
/// - id: 事件 ID（忽略）
/// - retry: 重连时间（忽略）
fn parse_sse_block(block: &str) -> (String, String) {
    let mut event_type = String::new();
    let mut data = String::new();

    for line in block.lines() {
        if let Some(val) = line.strip_prefix("event:") {
            event_type = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("data:") {
            let trimmed = val.trim();
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(trimmed);
        }
        // 忽略 id: 和 retry: 行
    }

    (event_type, data)
}

/// 从 data 字符串中提取 endpoint URL
///
/// 优先级：
/// 1. data 本身是裸 URL（以 http 开头）
/// 2. data 是 JSON，取 str 值或 uri 字段
/// 3. 整个 data 当 URL
fn extract_endpoint_url(data: &str) -> Option<String> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 裸 URL
    if trimmed.starts_with("http") {
        return Some(trimmed.to_string());
    }
    // JSON 解析
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if let Some(url) = v.as_str() {
            return Some(url.to_string());
        }
        if let Some(url) = v.get("uri").and_then(|u| u.as_str()) {
            return Some(url.to_string());
        }
    }
    // 兜底：整个 data 当 URL
    Some(trimmed.to_string())
}

// ---------------------------------------------------------------------------
// McpSseTransport — SSE 长连接模式
// ---------------------------------------------------------------------------

pub struct McpSseTransport {
    message_url: String,
    client: reqwest::Client,
    headers: reqwest::header::HeaderMap,
    next_id: AtomicU64,
    pending: std::sync::Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Result<Value, McpError>>>>>,
    _sse_task: tokio::task::JoinHandle<()>,
    notification_rx: Option<tokio::sync::mpsc::Receiver<Value>>,
}

impl McpSseTransport {
    pub async fn connect(
        sse_url: &str,
        custom_headers: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut headers = reqwest::header::HeaderMap::new();
        for (k, v) in custom_headers {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| McpError::Config(format!("无效请求头 '{k}': {e}")))?;
            headers.insert(name, v.parse().map_err(|e| McpError::Config(format!("无效请求头值: {e}")))?);
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build().map_err(|e| McpError::Transport(format!("HTTP 客户端创建失败: {e}")))?;

        let response = client.get(sse_url).headers(headers.clone()).send().await
            .map_err(|e| McpError::Transport(format!("连接 SSE 失败: {e}")))?;
        if !response.status().is_success() {
            return Err(McpError::Transport(format!("SSE 连接返回 HTTP {}", response.status())));
        }

        let (notification_tx, notification_rx) = tokio::sync::mpsc::channel(32);
        let (message_url_tx, mut message_url_rx) = tokio::sync::mpsc::channel(1);
        let pending = std::sync::Arc::new(Mutex::new(
            HashMap::<u64, tokio::sync::oneshot::Sender<Result<Value, McpError>>>::new()
        ));
        let pending_clone = pending.clone();

        let sse_task = tokio::spawn(async move {
            let mut buffer = String::new();
            let mut endpoint_found = false;
            let mut stream = response.bytes_stream();

            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result { Ok(c) => c, Err(e) => { tracing::warn!("MCP SSE 流错误: {e}"); break; } };
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buffer.find("\n\n") {
                    let block = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    let (event_type, data) = parse_sse_block(&block);
                    let data = data.trim().to_string();

                    if data.is_empty() {
                        continue;
                    }

                    if !endpoint_found {
                        // 还没找到 endpoint：event_type == "endpoint" 或 data 以 http 开头
                        if event_type == "endpoint" || data.starts_with("http") {
                            if let Some(url) = extract_endpoint_url(&data) {
                                let _ = message_url_tx.try_send(url);
                                endpoint_found = true;
                            }
                        }
                        continue;
                    }

                    // 已找到 endpoint：所有后续事件都是 JSON-RPC
                    if let Ok(resp) = serde_json::from_str::<Value>(&data) {
                        if let Some(id) = resp.get("id").and_then(|v| v.as_u64()) {
                            let has_method = resp.get("method").is_some();
                            if has_method {
                                // Server→Client 请求
                                tracing::debug!("MCP SSE server request: {:?}", resp);
                            } else {
                                // 响应
                                let mut p = pending_clone.lock().await;
                                if let Some(tx) = p.remove(&id) {
                                    if let Some(error) = resp.get("error") {
                                        let _ = tx.send(Err(McpError::Server(error.clone())));
                                    } else {
                                        let _ = tx.send(Ok(resp));
                                    }
                                }
                            }
                        } else if resp.get("method").is_some() {
                            // 通知 — 转发到 notification channel
                            let _ = notification_tx.try_send(resp);
                        }
                    }
                }
            }
        });

        let message_url = tokio::time::timeout(Duration::from_secs(30), message_url_rx.recv()).await
            .map_err(|_| McpError::Transport("SSE 连接超时: 未收到 endpoint 事件".into()))?
            .ok_or_else(|| McpError::Transport("SSE 流已关闭".into()))?;

        tracing::info!("MCP SSE 连接成功: message_url={}", message_url);
        Ok(Self { message_url, client, headers, next_id: AtomicU64::new(1), pending, _sse_task: sse_task, notification_rx: Some(notification_rx) })
    }

    /// 取出 notification 接收端（供外部处理通知的后台 task 使用）
    pub fn take_notification_rx(&mut self) -> Option<tokio::sync::mpsc::Receiver<Value>> { self.notification_rx.take() }

    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let (tx, rx) = tokio::sync::oneshot::channel();
        { let mut p = self.pending.lock().await; p.insert(id, tx); }
        let resp = self.client.post(&self.message_url).headers(self.headers.clone()).json(&msg).send().await
            .map_err(|e| McpError::Transport(format!("HTTP 请求失败: {e}")))?;
        if !resp.status().is_success() {
            let mut p = self.pending.lock().await; p.remove(&id);
            return Err(McpError::Transport(format!("HTTP POST 返回 {}", resp.status())));
        }
        match tokio::time::timeout(Duration::from_secs(120), rx).await {
            Ok(Ok(Ok(result))) => {
                if let Some(error) = result.get("error") { return Err(McpError::Server(error.clone())); }
                Ok(result.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(McpError::Transport("响应通道已关闭".into())),
            Err(_) => {
                let mut p = self.pending.lock().await; p.remove(&id);
                drop(p);
                let _ = self.cancel_request_inner(id, "请求超时（120秒）").await;
                Err(McpError::Transport("请求超时（120秒）".into()))
            }
        }
    }

    /// 取消指定 ID 的请求（发送取消通知 + 从 pending 移除）
    pub async fn cancel_request(&self, id: u64, reason: &str) -> Result<(), McpError> {
        let mut p = self.pending.lock().await;
        p.remove(&id);
        drop(p);
        self.cancel_request_inner(id, reason).await
    }

    async fn cancel_request_inner(&self, id: u64, reason: &str) -> Result<(), McpError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {
                "requestId": id,
                "reason": reason,
            }
        });
        self.client.post(&self.message_url).headers(self.headers.clone()).json(&msg).send().await
            .map_err(|e| McpError::Transport(format!("取消请求发送失败: {e}")))?;
        Ok(())
    }

    pub async fn send_notification(&self, method: &str, params: Value) -> Result<(), McpError> {
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.client.post(&self.message_url).headers(self.headers.clone()).json(&msg).send().await
            .map_err(|e| McpError::Transport(format!("通知发送失败: {e}")))?;
        Ok(())
    }

    /// 发送 JSON-RPC 响应（回复 Server 发来的请求）
    pub async fn send_response(&self, id: u64, result: Value) -> Result<(), McpError> {
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        self.client.post(&self.message_url).headers(self.headers.clone()).json(&msg).send().await
            .map_err(|e| McpError::Transport(format!("响应发送失败: {e}")))?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), McpError> { Ok(()) }
}

// ---------------------------------------------------------------------------
// McpDirectTransport — 直连 POST 模式（兼容非标准 MCP Server）
// ---------------------------------------------------------------------------

/// 直连 POST 传输 — 适用于不支持 SSE 的 MCP Server
pub struct McpDirectTransport {
    url: String,
    client: reqwest::Client,
    headers: reqwest::header::HeaderMap,
    next_id: AtomicU64,
}

impl McpDirectTransport {
    /// 创建直连 POST 传输
    /// `post_url` — 消息发送地址（优先使用 message_url，未设置则用 url）
    pub fn new(post_url: &str, custom_headers: &HashMap<String, String>) -> Result<Self, McpError> {
        let mut headers = reqwest::header::HeaderMap::new();
        for (k, v) in custom_headers {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| McpError::Config(format!("无效请求头 '{k}': {e}")))?;
            headers.insert(name, v.parse().map_err(|e| McpError::Config(format!("无效请求头值: {e}")))?);
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build().map_err(|e| McpError::Transport(format!("HTTP 客户端创建失败: {e}")))?;
        Ok(Self { url: post_url.to_string(), client, headers, next_id: AtomicU64::new(1) })
    }

    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});

        let resp = self.client.post(&self.url).headers(self.headers.clone()).json(&msg).send().await
            .map_err(|e| McpError::Transport(format!("HTTP 请求失败: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!("HTTP {}: {}", status, &text[..text.len().min(200)])));
        }

        let body: Value = resp.json().await
            .map_err(|e| McpError::Parse(format!("解析响应体失败: {e}")))?;

        tracing::debug!("MCP Direct [{method}] 响应: {}", &body.to_string()[..body.to_string().len().min(300)]);

        if let Some(error) = body.get("error") { return Err(McpError::Server(error.clone())); }
        // 兼容非标准格式：无 result 字段时返回整个 body
        Ok(body.get("result").cloned().unwrap_or(body))
    }

    pub async fn send_notification(&self, method: &str, params: Value) -> Result<(), McpError> {
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        let resp = self.client.post(&self.url).headers(self.headers.clone()).json(&msg).send().await
            .map_err(|e| McpError::Transport(format!("通知发送失败: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!("通知 HTTP {}: {}", status, &text[..text.len().min(200)])));
        }
        Ok(())
    }

    /// 发送 JSON-RPC 响应（回复 Server 发来的请求）
    pub async fn send_response(&self, id: u64, result: Value) -> Result<(), McpError> {
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        self.client.post(&self.url).headers(self.headers.clone()).json(&msg).send().await
            .map_err(|e| McpError::Transport(format!("响应发送失败: {e}")))?;
        Ok(())
    }

    /// 取消指定 ID 的请求
    pub async fn cancel_request(&self, _id: u64, _reason: &str) -> Result<(), McpError> { Ok(()) }

    pub async fn shutdown(&self) -> Result<(), McpError> { Ok(()) }
}

// ---------------------------------------------------------------------------
// TransportWrapper
// ---------------------------------------------------------------------------

pub enum McpTransportWrapper {
    Stdio(super::transport::McpTransport),
    Sse(McpSseTransport),
    Direct(McpDirectTransport),
}

impl McpTransportWrapper {
    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        match self {
            Self::Stdio(t) => t.send_request(method, params).await,
            Self::Sse(t) => t.send_request(method, params).await,
            Self::Direct(t) => t.send_request(method, params).await,
        }
    }
    pub async fn cancel_request(&self, id: u64, reason: &str) -> Result<(), McpError> {
        match self {
            Self::Stdio(t) => t.cancel_request(id, reason).await,
            Self::Sse(t) => t.cancel_request(id, reason).await,
            Self::Direct(t) => t.cancel_request(id, reason).await,
        }
    }
    pub fn take_notification_rx(&mut self) -> Option<tokio::sync::mpsc::Receiver<Value>> {
        match self {
            Self::Stdio(t) => t.take_notification_rx(),
            Self::Sse(t) => t.take_notification_rx(),
            Self::Direct(_t) => None,
        }
    }
    pub async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        match self {
            Self::Stdio(t) => t.send_notification(method, params).await,
            Self::Sse(t) => t.send_notification(method, params).await,
            Self::Direct(t) => t.send_notification(method, params).await,
        }
    }
    pub async fn send_response(&mut self, id: u64, result: Value) -> Result<(), McpError> {
        match self {
            Self::Stdio(t) => t.send_response(id, result).await,
            Self::Sse(t) => t.send_response(id, result).await,
            Self::Direct(t) => t.send_response(id, result).await,
        }
    }
    pub async fn shutdown(&mut self) -> Result<(), McpError> {
        match self { Self::Stdio(t) => t.shutdown().await, Self::Sse(t) => t.shutdown().await, Self::Direct(t) => t.shutdown().await }
    }

    pub async fn connect(config: &crate::core::McpServerConfig, server_name: &str) -> Result<Self, McpError> {
        // stdio 模式
        if let Some(cmd) = &config.command {
            tracing::info!("MCP [{}] 使用 stdio 传输", server_name);
            let t = super::transport::McpTransport::spawn_stdio(cmd, &config.args, &config.env).await?;
            return Ok(Self::Stdio(t));
        }

        let url = config.url.as_deref().ok_or_else(|| {
            McpError::Config(format!("MCP Server '{server_name}' 缺少 command 或 url"))
        })?;

        // 尝试 SSE 模式，失败则回退到直连 POST
        let mut headers = reqwest::header::HeaderMap::new();
        for (k, v) in &config.headers {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| McpError::Config(format!("无效请求头 '{k}': {e}")))?;
            headers.insert(name, v.parse().map_err(|e| McpError::Config(format!("无效请求头值: {e}")))?);
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build().map_err(|e| McpError::Transport(format!("HTTP 客户端创建失败: {e}")))?;

        // 用 GET 探测是否支持 SSE
        match client.get(url).headers(headers.clone()).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!("MCP [{}] 使用 HTTP/SSE 传输: {}", server_name, url);
                let transport = McpSseTransport::connect(url, &config.headers).await?;
                return Ok(Self::Sse(transport));
            }
            Ok(resp) => {
                tracing::info!("MCP [{}] SSE 不可用 (HTTP {}), 使用直连 POST", server_name, resp.status());
            }
            Err(e) => {
                tracing::info!("MCP [{}] SSE 连接失败 ({}), 使用直连 POST", server_name, e);
            }
        }

        tracing::info!("MCP [{}] 使用直连 POST 模式: {}", server_name, url);
        // 优先使用 message_url（如果配置了的话），否则使用 url 自身
        let post_url = config.message_url.as_deref().unwrap_or(url);
        if post_url != url {
            tracing::info!("MCP [{}] 消息发送地址: {}", server_name, post_url);
        }
        let t = McpDirectTransport::new(post_url, &config.headers)?;
        Ok(Self::Direct(t))
    }
}
