//! RHermes DeepSeek API 客户端
//!
//! 封装 DeepSeek Chat Completion API 的调用，支持：
//! - 同步请求 & SSE 流式响应
//! - 自动重试（rate limit / 超时 / 网络错误）
//! - Token 用量追踪

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};


use crate::config::Config;

// ---------------------------------------------------------------------------
// 请求 / 响应结构
// ---------------------------------------------------------------------------

/// Chat Completion 请求体
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ApiMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

/// API 消息格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub role: String,
    pub content: String,
}

/// Chat Completion 响应
#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ResponseMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseMessage {
    pub role: String,
    pub content: Option<String>,
}

/// Token 用量
#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)]
    pub prompt_cache_hit_tokens: u32,
    #[serde(default)]
    pub prompt_cache_miss_tokens: u32,
}

/// SSE 流式事件
#[derive(Debug)]
pub enum StreamEvent {
    /// 普通文本块
    Chunk(String),
    /// 最终完整响应
    Done(ResponseMessage, Option<Usage>),
    /// 错误
    Error(String),
}

/// 余额响应
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceResponse {
    /// 是否无限额度
    #[serde(default)]
    pub is_infinite: bool,
    /// 当前可用余额（元）
    #[serde(default)]
    pub balance: f64,
    /// 总余额
    #[serde(default)]
    pub total_balance: f64,
}

/// API → TUI 事件（GUI 友好格式）
#[derive(Debug)]
pub enum ApiEvent {
    /// 流式文本块
    StreamChunk(String),
    /// 流结束
    Done,
    /// 收到用量信息
    Usage(Usage),
    /// 工具调用（名称和参数字符串）
    ToolCalls(Vec<ToolCallData>),
    /// 余额查询结果（元）
    Balance(f64),
    /// 错误
    Error(String),
}

/// 流式工具调用数据
#[derive(Debug, Clone)]
pub struct ToolCallData {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

// ---------------------------------------------------------------------------
// API 客户端
// ---------------------------------------------------------------------------

/// DeepSeek API 客户端
#[derive(Debug, Clone)]
pub struct DeepSeekClient {
    http: reqwest::Client,
    config: Config,
}

impl DeepSeekClient {
    /// 从配置创建客户端
    pub fn new(config: Config) -> Self {
        let timeout = Duration::from_secs(config.timeout_secs);

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("创建 HTTP 客户端失败");

        Self { http, config }
    }

    /// 直接指定参数创建客户端
    pub fn with_params(api_key: String, base_url: String, model: String) -> Self {
        let config = Config {
            api_key,
            model,
            base_url,
            ..Default::default()
        };
        Self::new(config)
    }

    /// 聊天完成（非流式）
    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError> {
        let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));

        let mut req = request;
        req.stream = false;

        let response = self
            .http
            .post(&url)
            .headers(self.build_headers())
            .json(&req)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if !response.status().is_success() {
            return Err(ApiError::HttpStatus(response.status().as_u16()));
        }

        response.json().await.map_err(ApiError::Parse)
    }

    /// 聊天完成（SSE 流式）
    ///
    /// 通过 `tx` 发送流式事件，函数返回时流结束。
    pub async fn chat_stream(
        &self,
        request: ChatRequest,
        tx: tokio::sync::mpsc::UnboundedSender<ApiEvent>,
    ) -> Result<(), ApiError> {
        let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));

        let mut req = request;
        req.stream = true;

        let response = self
            .http
            .post(&url)
            .headers(self.build_headers())
            .json(&req)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let _ = tx.send(ApiEvent::Error(format!("HTTP {status}")));
            return Err(ApiError::HttpStatus(status));
        }

        // 解析 SSE 流
        let mut buffer = String::new();
        let mut stream = response.bytes_stream();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(ApiError::Request)?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            // 处理 buffer 中的 SSE 事件
            while let Some(event_end) = buffer.find("\n\n") {
                let event = buffer[..event_end].to_string();
                buffer = buffer[event_end + 2..].to_string();

                // 处理单个事件（可能有多行 data: ...）
                for line in event.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            continue;
                        }

                        // 解析 JSON
                        match serde_json::from_str::<StreamChunk>(data) {
                            Ok(chunk_data) => {
                                // 转发 usage（最后一个 chunk 携带）
                                if let Some(ref usage) = chunk_data.usage {
                                    let _ = tx.send(ApiEvent::Usage(usage.clone()));
                                }

                                if let Some(choice) = chunk_data.choices.first() {
                                    let content = choice.delta.content.as_deref().unwrap_or("");
                                    if !content.is_empty() {
                                        if tx
                                            .send(ApiEvent::StreamChunk(content.to_string()))
                                            .is_err()
                                        {
                                            return Ok(());
                                        }
                                    }
                                    // 检测工具调用结束
                                    if let Some(ref reason) = choice.finish_reason {
                                        if reason == "tool_calls" {
                                            if let Some(ref calls) = choice.delta.tool_calls {
                                                let tool_data: Vec<ToolCallData> = calls.iter().map(|tc| ToolCallData {
                                                    id: tc.id.clone().unwrap_or_default(),
                                                    name: tc.function.as_ref().and_then(|f| f.name.clone()).unwrap_or_default(),
                                                    arguments: tc.function.as_ref().and_then(|f| f.arguments.clone()).unwrap_or_default(),
                                                }).collect();
                                                if !tool_data.is_empty() {
                                                    let _ = tx.send(ApiEvent::ToolCalls(tool_data));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                // 忽略无法解析的行
                            }
                        }
                    }
                }
            }
        }

        // 发送完成事件
        let _ = tx.send(ApiEvent::Done);

        Ok(())
    }

    /// 带自动重试的聊天完成
    pub async fn chat_with_retry(&self, request: ChatRequest) -> Result<ChatResponse, ApiError> {
        let max_retries = self.config.max_retries;
        let mut last_error = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                // 指数退避
                let delay = Duration::from_millis(500 * 2u64.pow(attempt - 1));
                tokio::time::sleep(delay).await;
            }

            match self.chat(request.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    // 只有可重试的错误才重试
                    if e.is_retryable() {
                        last_error = Some(e);
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        Err(last_error.unwrap_or(ApiError::RetryExhausted))
    }

    /// 查询账户余额（人民币元）
    pub async fn get_balance(&self) -> Result<f64, ApiError> {
        let url = format!("{}/user/balance", self.config.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .headers(self.build_headers())
            .send()
            .await
            .map_err(ApiError::Request)?;
        if !resp.status().is_success() {
            return Err(ApiError::HttpStatus(resp.status().as_u16()));
        }
        let balance_resp: BalanceResponse = resp.json().await.map_err(ApiError::Parse)?;
        if balance_resp.is_infinite {
            Ok(f64::MAX) // 无限额度
        } else {
            Ok(balance_resp.balance)
        }
    }

    /// 获取当前使用的模型名
    pub fn model(&self) -> &str {
        &self.config.model
    }

    // ---- 辅助方法 ----

    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.config.api_key))
                .expect("无效的 API Key"),
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers
    }
}

// ---------------------------------------------------------------------------
// SSE 流式 chunk
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct StreamChunk {
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    pub delta: StreamDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamToolCall {
    pub index: i32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    #[serde(default)]
    pub function: Option<StreamToolCallFunction>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamToolCallFunction {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ApiError {
    /// HTTP 请求失败（网络/超时）
    Request(reqwest::Error),
    /// HTTP 状态码错误（4xx/5xx）
    HttpStatus(u16),
    /// 响应解析失败
    Parse(reqwest::Error),
    /// 重试耗尽
    RetryExhausted,
}

impl ApiError {
    /// 是否可重试
    pub fn is_retryable(&self) -> bool {
        match self {
            // 网络错误可重试
            Self::Request(_) => true,
            // 5xx 可重试，4xx 不可重试
            Self::HttpStatus(status) => *status >= 500,
            // 解析错误不可重试
            Self::Parse(_) => false,
            Self::RetryExhausted => false,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "网络请求失败: {e}"),
            Self::HttpStatus(s) => write!(f, "HTTP {s}"),
            Self::Parse(e) => write!(f, "响应解析失败: {e}"),
            Self::RetryExhausted => write!(f, "重试次数耗尽"),
        }
    }
}

impl std::error::Error for ApiError {}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_cache_fields() {
        let json = r#"{
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_cache_hit_tokens": 80,
            "prompt_cache_miss_tokens": 20
        }"#;

        let usage: Usage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.prompt_cache_hit_tokens, 80);
        assert_eq!(usage.prompt_cache_miss_tokens, 20);
    }

    #[test]
    fn test_usage_cache_fields_default_zero() {
        // 没有缓存字段时，默认为 0
        let json = r#"{
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
        }"#;

        let usage: Usage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.prompt_cache_hit_tokens, 0);
        assert_eq!(usage.prompt_cache_miss_tokens, 0);
    }

    #[test]
    fn test_chat_request_serialization() {
        let req = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![ApiMessage {
                role: "user".into(),
                content: "hello".into(),
            }],
            stream: true,
            max_tokens: Some(4096),
            temperature: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"model\":\"deepseek-v4-flash\""));
        assert!(json.contains("\"stream\":true"));
        assert!(json.contains("\"max_tokens\":4096"));
        assert!(!json.contains("\"temperature\"")); // None 不序列化
    }

    #[test]
    fn test_api_error_is_retryable() {
        // reqwest::Error 无法直接构造，用 HttpStatus 5xx 模拟可重试错误
        assert!(ApiError::HttpStatus(500).is_retryable());

        assert!(ApiError::HttpStatus(500).is_retryable());
        assert!(ApiError::HttpStatus(502).is_retryable());
        assert!(ApiError::HttpStatus(503).is_retryable());

        assert!(!ApiError::HttpStatus(400).is_retryable());
        assert!(!ApiError::HttpStatus(401).is_retryable());
        assert!(!ApiError::HttpStatus(403).is_retryable());
        assert!(!ApiError::HttpStatus(429).is_retryable());

        assert!(!ApiError::RetryExhausted.is_retryable());
    }

    #[test]
    fn test_stream_chunk_parsing() {
        let sse_data = r#"{"choices":[{"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}"#;
        let chunk: StreamChunk = serde_json::from_str(sse_data).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
        assert!(chunk.choices[0].finish_reason.is_none());
    }

    #[test]
    fn test_stream_chunk_with_tool_call() {
        let sse_data = r#"{"choices":[{"delta":{"content":null},"finish_reason":"tool_calls"}]}"#;
        let chunk: StreamChunk = serde_json::from_str(sse_data).unwrap();
        assert!(chunk.choices[0].delta.content.is_none());
        assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("tool_calls"));
    }
}
