//! MCP 传输层 — JSON-RPC over stdio
//!
//! 启动 MCP Server 子进程，通过 stdin/stdout 进行 JSON-RPC 通信。
//! 使用 ID 路由机制处理异步响应，支持 Server 在响应之间发送通知，
//! 以及主动取消正在进行的请求。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum McpError {
    Io(std::io::Error),
    Parse(String),
    Transport(String),
    Server(Value),
    Config(String),
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误: {e}"),
            Self::Parse(e) => write!(f, "JSON 解析失败: {e}"),
            Self::Transport(e) => write!(f, "传输错误: {e}"),
            Self::Server(v) => write!(f, "Server 错误: {}", v),
            Self::Config(e) => write!(f, "配置错误: {e}"),
        }
    }
}

impl From<std::io::Error> for McpError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}

// ---------------------------------------------------------------------------
// MCP Transport
// ---------------------------------------------------------------------------

/// MCP Server 的传输连接，使用 ID 路由异步处理 stdio 响应
pub struct McpTransport {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>>,
    _read_task: JoinHandle<()>,
    notification_rx: Option<tokio::sync::mpsc::Receiver<Value>>,
}

impl McpTransport {
    /// 启动子进程并建立连接
    pub async fn spawn_stdio(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(command);
        cmd.args(args).envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            #[allow(unused_imports)]
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take()
            .ok_or_else(|| McpError::Transport("无法获取子进程 stdin".into()))?;
        let stdout = child.stdout.take()
            .ok_or_else(|| McpError::Transport("无法获取子进程 stdout".into()))?;
        let stderr = child.stderr.take()
            .ok_or_else(|| McpError::Transport("无法获取子进程 stderr".into()))?;

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = pending.clone();

        let (notification_tx, notification_rx) = tokio::sync::mpsc::channel(32);

        // 后台 stderr 读取 task：将 MCP Server 的错误输出写入 tracing
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            use tokio::io::AsyncBufReadExt;
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.is_empty() {
                    tracing::debug!("MCP Server stderr: {line}");
                }
            }
        });

        // 后台读取 task：持续从 stdout 读取 JSON-RPC 响应/通知，按 id 路由
        let read_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Err(e) => {
                        tracing::warn!("MCP stdio 读取错误: {e}");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(trimmed) {
                            Ok(val) => {
                                if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
                                    let has_method = val.get("method").is_some();
                                    if has_method {
                                        // Server→Client 请求（如 sampling/createMessage）
                                        tracing::debug!("MCP stdio server request: {:?}", val);
                                    } else {
                                        // 响应 — 从 pending 中取出对应的 Sender 发送结果
                                        let mut p = pending_clone.lock().await;
                                        if let Some(tx) = p.remove(&id) {
                                            if let Some(error) = val.get("error") {
                                                let _ = tx.send(Err(McpError::Server(error.clone())));
                                            } else {
                                                let _ = tx.send(Ok(val));
                                            }
                                        }
                                    }
                                } else if val.get("method").is_some() {
                                    // 通知 — 转发到 notification channel
                                    let _ = notification_tx.try_send(val);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("MCP stdio 解析 JSON 失败: {e}, line={trimmed}");
                            }
                        }
                    }
                }
            }
            tracing::debug!("MCP stdio 后台读取任务退出");
        });

        Ok(Self {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            next_id: AtomicU64::new(1),
            pending,
            _read_task: read_task,
            notification_rx: Some(notification_rx),
        })
    }

    /// 取出 notification 接收端（供外部处理通知的后台 task 使用）
    pub fn take_notification_rx(&mut self) -> Option<tokio::sync::mpsc::Receiver<Value>> {
        self.notification_rx.take()
    }

    /// 发送 JSON-RPC 通知（不等待响应）
    pub async fn send_notification(&self, method: &str, params: Value) -> Result<(), McpError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "method": method, "params": params,
        });
        self.write_line(&msg).await
    }

    /// 发送 JSON-RPC 响应（回复 Server 发来的请求，如 sampling/createMessage）
    pub async fn send_response(&self, id: u64, result: Value) -> Result<(), McpError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "result": result,
        });
        self.write_line(&msg).await
    }

    /// 发送 JSON-RPC 请求并等待响应（ID 路由匹配）
    ///
    /// 将请求注册到 pending map 后写入 stdin，
    /// 后台 task 收到匹配 id 的响应后通过 oneshot channel 送回。
    /// 超时时自动发送取消通知到 Server。
    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        });
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().await;
            p.insert(id, tx);
        }
        self.write_line(&msg).await?;

        match tokio::time::timeout(Duration::from_secs(120), rx).await {
            Ok(Ok(Ok(val))) => {
                if let Some(error) = val.get("error") {
                    return Err(McpError::Server(error.clone()));
                }
                Ok(val.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(McpError::Transport("响应通道已关闭".into())),
            Err(_) => {
                // 超时：从 pending 移除并发送取消通知
                let mut p = self.pending.lock().await;
                p.remove(&id);
                drop(p);
                let _ = self.cancel_request_inner(id, "请求超时（120秒）").await;
                Err(McpError::Transport("请求超时（120秒）".into()))
            }
        }
    }

    /// 取消指定 ID 的请求（向 Server 发送取消通知 + 从 pending 移除）
    pub async fn cancel_request(&self, id: u64, reason: &str) -> Result<(), McpError> {
        let mut p = self.pending.lock().await;
        p.remove(&id);
        drop(p);
        self.cancel_request_inner(id, reason).await
    }

    /// 内部：向 Server 发送取消通知（不操作 pending）
    async fn cancel_request_inner(&self, id: u64, reason: &str) -> Result<(), McpError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {
                "requestId": id,
                "reason": reason,
            }
        });
        self.write_line(&msg).await
    }

    async fn write_line(&self, msg: &Value) -> Result<(), McpError> {
        let mut line = serde_json::to_string(msg)
            .map_err(|e| McpError::Parse(format!("序列化失败: {e}")))?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// 关闭连接
    ///
    /// Kill 子进程 → 管道关闭 → 后台读取 task 自然退出
    pub async fn shutdown(&mut self) -> Result<(), McpError> {
        let _ = self.child.kill().await;
        Ok(())
    }
}

impl Drop for McpTransport {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_error_display() {
        let err = McpError::Transport("连接超时".into());
        assert!(err.to_string().contains("传输错误"));
    }

    #[test]
    fn test_mcp_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let mcp_err: McpError = io_err.into();
        assert!(matches!(mcp_err, McpError::Io(_)));
    }
}
