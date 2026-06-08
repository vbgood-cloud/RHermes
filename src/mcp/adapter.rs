//! MCP Adapter — 管理与单个 MCP Server 的连接
//!
//! 职责：
//! 1. 启动时连接（initialize 握手 + tools/list）
//! 2. 运行时调用（tools/call）
//! 3. 关闭时清理

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;

use super::config::McpServerConfig;
use super::transport::McpError;
use super::sse_transport::McpTransportWrapper;

/// MCP 日志级别（与 MCP 协议对齐）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum McpLogLevel {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
}

impl McpLogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
            Self::Alert => "alert",
            Self::Emergency => "emergency",
        }
    }
}

/// 将 MCP 日志级别映射到 tracing 级别
fn map_mcp_log_level(level: &str) -> &'static str {
    match level {
        "emergency" | "alert" | "critical" => "ERROR",
        "error" => "ERROR",
        "warning" => "WARN",
        "notice" | "info" => "INFO",
        "debug" => "DEBUG",
        _ => "INFO",
    }
}

/// MCP 工具信息
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub original_name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 管理与一个 MCP Server 的完整生命周期
pub struct McpAdapter {
    server_name: String,
    config: McpServerConfig,
    transport: Arc<Mutex<McpTransportWrapper>>,
    tools: Vec<McpToolInfo>,
    parallel_safe: bool,
}

impl McpAdapter {
    /// 连接 MCP Server 并完成初始化握手
    pub async fn connect(
        server_name: String,
        config: &McpServerConfig,
    ) -> Result<Self, McpError> {
        tracing::info!("MCP [{}] 正在连接...", server_name);
        let mut transport = McpTransportWrapper::connect(config, &server_name).await?;

        let is_direct = matches!(transport, McpTransportWrapper::Direct(_));
        let mut tools = Vec::new();

        // initialize 握手（Direct 模式下不需要标准 MCP 握手）
        match transport.send_request("initialize", serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "rhermes", "version": env!("CARGO_PKG_VERSION") }
        })).await {
            Ok(init_result) => {
                let server_info = init_result.get("serverInfo")
                    .and_then(|s| s.get("name")).and_then(|n| n.as_str()).unwrap_or("unknown");
                tracing::info!("MCP [{}] 已连接: server={}", server_name, server_info);
                let _ = transport.send_notification("notifications/initialized", serde_json::json!({})).await;
            }
            Err(e) if is_direct => {
                tracing::warn!("MCP [{}] initialize 握手跳过 ({})，直连模式可能不需要", server_name, e);
            }
            Err(e) => return Err(e),
        }

        // tools/list
        match transport.send_request("tools/list", serde_json::json!({})).await {
            Ok(tools_result) => {
                tracing::debug!("MCP [{}] tools/list 响应: {}", server_name,
                    &tools_result.to_string()[..tools_result.to_string().len().min(500)]);
                tools = Self::parse_tools_list(&tools_result);
                tracing::info!("MCP [{}] 发现 {} 个工具", server_name, tools.len());
            }
            Err(e) => {
                tracing::warn!("MCP [{}] tools/list 查询失败: {}", server_name, e);
            }
        }

        // 从 transport 取出 notification 接收端并启动后台处理 task
        let notification_rx = transport.take_notification_rx();
        let server_name_clone = server_name.clone();
        if let Some(mut notif_rx) = notification_rx {
            tokio::spawn(async move {
                while let Some(notif) = notif_rx.recv().await {
                    let method = notif.get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    match method.as_str() {
                        "notifications/tools/list_changed" => {
                            tracing::info!("MCP [{}] 工具列表已变更（刷新需要重启程序）", server_name_clone);
                        }
                        "notifications/message" => {
                            // MCP 日志通知 — 转发到 tracing
                            let params = notif.get("params");
                            let level = params.and_then(|p| p.get("level"))
                                .and_then(|l| l.as_str()).unwrap_or("info");
                            let data = params.and_then(|p| p.get("data"))
                                .map(|d| {
                                    if let Some(s) = d.as_str() { s.to_string() }
                                    else { d.to_string() }
                                }).unwrap_or_default();
                            match level {
                                "emergency" | "alert" | "critical" | "error" => {
                                    tracing::error!("[MCP/{}] {}", server_name_clone, data);
                                }
                                "warning" => {
                                    tracing::warn!("[MCP/{}] {}", server_name_clone, data);
                                }
                                "notice" | "info" => {
                                    tracing::info!("[MCP/{}] {}", server_name_clone, data);
                                }
                                _ => {
                                    tracing::debug!("[MCP/{}] {}", server_name_clone, data);
                                }
                            }
                        }
                        _ => {
                            tracing::debug!("MCP [{}] notification: {}", server_name_clone, method);
                        }
                    }
                }
            });
        }

        Ok(Self {
            server_name,
            config: config.clone(),
            transport: Arc::new(Mutex::new(transport)),
            tools,
            parallel_safe: config.parallel_safe,
        })
    }

    /// 调用远程工具（外部接口，失败时自动重连一次）
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        match self.try_call_tool(name, &arguments).await {
            Ok(result) => Ok(result),
            Err(e @ McpError::Io(_)) | Err(e @ McpError::Transport(_)) => {
                tracing::warn!("MCP [{}] 调用失败 ({})，尝试重连...", self.server_name, e);
                if let Err(re) = self.reconnect().await {
                    tracing::error!("MCP [{}] 重连失败: {}", self.server_name, re);
                    return Err(e);
                }
                // 重连成功后重试一次
                self.try_call_tool(name, &arguments).await
            }
            Err(e) => Err(e),
        }
    }

    /// 直接调用远程工具（内部实现，不做重连）
    async fn try_call_tool(&self, name: &str, arguments: &Value) -> Result<String, McpError> {
        let transport = self.transport.lock().await;
        let result = transport.send_request("tools/call", serde_json::json!({
            "name": name, "arguments": arguments,
        })).await?;
        Self::extract_text_result(&result)
    }

    /// 重连：清理旧连接 → 重新 connect → 重新 initialize 握手
    pub async fn reconnect(&self) -> Result<(), McpError> {
        let mut transport = self.transport.lock().await;
        // 关闭旧连接（忽略错误）
        let _ = transport.shutdown().await;

        // 建立新连接
        let mut new_transport = McpTransportWrapper::connect(&self.config, &self.server_name).await?;

        // 重新 initialize 握手
        let is_direct = matches!(new_transport, McpTransportWrapper::Direct(_));
        match new_transport.send_request("initialize", serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "rhermes", "version": env!("CARGO_PKG_VERSION") }
        })).await {
            Ok(init_result) => {
                let server_info = init_result.get("serverInfo")
                    .and_then(|s| s.get("name")).and_then(|n| n.as_str()).unwrap_or("unknown");
                tracing::info!("MCP [{}] 重连成功: server={}", self.server_name, server_info);
                let _ = new_transport.send_notification("notifications/initialized", serde_json::json!({})).await;
            }
            Err(e) if is_direct => {
                tracing::warn!("MCP [{}] 重连 initialize 跳过 ({})", self.server_name, e);
            }
            Err(e) => return Err(e),
        }

        // 替换 transport
        *transport = new_transport;
        tracing::info!("MCP [{}] 重连成功", self.server_name);
        Ok(())
    }

    /// 获取工具列表
    pub fn tools(&self) -> &[McpToolInfo] { &self.tools }
    pub fn server_name(&self) -> &str { &self.server_name }
    pub fn server_name_owned(&self) -> String { self.server_name.clone() }
    pub fn parallel_safe(&self) -> bool { self.parallel_safe }
    pub fn tool_parallel_safe(&self) -> &std::collections::HashMap<String, bool> { &self.config.tool_parallel_safe }

    /// 关闭连接
    pub async fn shutdown(&self) -> Result<(), McpError> {
        self.transport.lock().await.shutdown().await
    }

    /// 健康检查：用 timeout(10s) 调用 tools/list 检测连接是否正常
    pub async fn health_check(&self) -> Result<(), McpError> {
        let transport = self.transport.lock().await;
        tokio::time::timeout(std::time::Duration::from_secs(10), transport.send_request("tools/list", serde_json::json!({})))
            .await
            .map_err(|_| McpError::Transport("健康检查超时（10秒）".into()))?
            .map(|_| ())
    }

    /// 取消正在进行的请求（向 Server 发送取消通知）
    pub async fn cancel_request(&self, request_id: u64, reason: &str) -> Result<(), McpError> {
        let transport = self.transport.lock().await;
        transport.cancel_request(request_id, reason).await
    }

    /// 设置 Server 的日志级别（MCP logging/setLevel）
    pub async fn set_log_level(&self, level: McpLogLevel) -> Result<(), McpError> {
        let transport = self.transport.lock().await;
        match transport.send_request("logging/setLevel", serde_json::json!({
            "level": level.as_str(),
        })).await {
            Ok(_) => {
                tracing::debug!("MCP [{}] 日志级别已设为 {}", self.server_name, level.as_str());
                Ok(())
            }
            Err(e) => {
                tracing::debug!("MCP [{}] 不支持 logging/setLevel: {}", self.server_name, e);
                Err(e)
            }
        }
    }

    fn parse_tools_list(result: &Value) -> Vec<McpToolInfo> {
        let tools_array = match result.get("tools").and_then(|t| t.as_array()) {
            Some(arr) => arr, None => return Vec::new(),
        };
        tools_array.iter().filter_map(|tool| {
            let original_name = tool.get("name")?.as_str()?.to_string();
            let description = tool.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
            let input_schema = tool.get("inputSchema").cloned()
                .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));
            Some(McpToolInfo { original_name, description, input_schema })
        }).collect()
    }

    fn extract_text_result(result: &Value) -> Result<String, McpError> {
        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            let texts: Vec<String> = content.iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else { None }
                }).collect();
            if !texts.is_empty() { return Ok(texts.join("\n")); }
        }
        Ok(serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string()))
    }
}

// ---------------------------------------------------------------------------
// MCP Adapter Manager
// ---------------------------------------------------------------------------

/// 管理所有 MCP Server 的连接
pub struct McpAdapterManager {
    adapters: Vec<Arc<McpAdapter>>,
}

impl McpAdapterManager {
    pub fn new() -> Self { Self { adapters: Vec::new() } }

    /// 连接所有配置的 MCP Server
    pub async fn connect_all(&mut self, servers: &std::collections::HashMap<String, McpServerConfig>) {
        for (name, config) in servers {
            match McpAdapter::connect(name.clone(), config).await {
                Ok(adapter) => self.adapters.push(Arc::new(adapter)),
                Err(e) => tracing::warn!("MCP [{}] 连接失败: {}", name, e),
            }
        }
    }

    pub fn adapters(&self) -> &[Arc<McpAdapter>] { &self.adapters }
    pub fn adapters_mut(&mut self) -> &mut Vec<Arc<McpAdapter>> { &mut self.adapters }

    pub async fn shutdown_all(&self) {
        for adapter in &self.adapters {
            if let Err(e) = adapter.shutdown().await {
                tracing::warn!("MCP [{}] 关闭失败: {}", adapter.server_name(), e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_tools_list() {
        let result = json!({
            "tools": [
                { "name": "create_issue", "description": "Create a GitHub issue",
                  "inputSchema": { "type": "object", "properties": {
                      "title": {"type": "string", "description": "Issue title"},
                  }, "required": ["title"] } },
                { "name": "search_repos", "description": "Search repositories" }
            ]
        });
        let tools = McpAdapter::parse_tools_list(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].original_name, "create_issue");
    }

    #[test]
    fn test_parse_empty_tools_list() {
        let result = json!({});
        let tools = McpAdapter::parse_tools_list(&result);
        assert!(tools.is_empty());
    }

    #[test]
    fn test_extract_text_result() {
        let result = json!({
            "content": [
                {"type": "text", "text": "Issue #42 created"},
                {"type": "text", "text": "URL: https://github.com/..."}
            ]
        });
        let text = McpAdapter::extract_text_result(&result).unwrap();
        assert!(text.contains("Issue #42 created"));
    }
}
