//! Transport trait —— 协议适配层抽象
//!
//! 定义 AI API 调用的通用接口，当前实现为 DeepSeek HTTP Transport。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{
    ApiError, ApiEvent, BalanceResponse, ChatRequest, ChatResponse, StreamChunk, ToolCallData,
};
use crate::core::Config;

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// 协议适配层：将不同 AI API 的底层 HTTP 调用抽象为统一接口
#[async_trait]
pub trait Transport: Send + Sync {
    /// 聊天完成（非流式）
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError>;

    /// 聊天完成（SSE 流式），通过 tx 发送事件
    async fn chat_stream(
        &self,
        request: ChatRequest,
        tx: UnboundedSender<ApiEvent>,
    ) -> Result<(), ApiError>;

    /// 查询账户余额
    async fn get_balance(&self) -> Result<f64, ApiError>;

    /// 获取当前使用的模型名称
    fn model_name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// DeepSeek Transport
// ---------------------------------------------------------------------------

/// DeepSeek API 的 HTTP Transport 实现
pub struct DeepSeekTransport {
    http: reqwest::Client,
    config: Arc<Config>,
}

impl DeepSeekTransport {
    /// 从配置创建 Transport
    pub fn new(config: &Config) -> Self {
        let timeout = Duration::from_secs(config.request.timeout_secs);
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("创建 HTTP 客户端失败");
        Self {
            http,
            config: Arc::new(config.clone()),
        }
    }

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

    fn base_url(&self) -> &str {
        self.config.api.base_url.trim_end_matches('/')
    }
}

#[async_trait]
impl Transport for DeepSeekTransport {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError> {
        let url = format!("{}/chat/completions", self.base_url());

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

    async fn chat_stream(
        &self,
        request: ChatRequest,
        tx: UnboundedSender<ApiEvent>,
    ) -> Result<(), ApiError> {
        let url = format!("{}/chat/completions", self.base_url());

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

            while let Some(event_end) = buffer.find("\n\n") {
                let event = buffer[..event_end].to_string();
                buffer = buffer[event_end + 2..].to_string();

                for line in event.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            continue;
                        }

                        match serde_json::from_str::<StreamChunk>(data) {
                            Ok(chunk_data) => {
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
                                    if let Some(ref calls) = choice.delta.tool_calls {
                                        let tool_data: Vec<ToolCallData> = calls
                                            .iter()
                                            .map(|tc| ToolCallData {
                                                id: tc.id.clone().unwrap_or_default(),
                                                name: tc
                                                    .function
                                                    .as_ref()
                                                    .and_then(|f| f.name.clone())
                                                    .unwrap_or_default(),
                                                arguments: tc
                                                    .function
                                                    .as_ref()
                                                    .and_then(|f| f.arguments.clone())
                                                    .unwrap_or_default(),
                                            })
                                            .collect();
                                        if !tool_data.is_empty() {
                                            let _ = tx.send(ApiEvent::ToolCalls(tool_data));
                                        }
                                    }
                                }
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }

        let _ = tx.send(ApiEvent::Done);
        Ok(())
    }

    async fn get_balance(&self) -> Result<f64, ApiError> {
        let url = format!("{}/user/balance", self.base_url());
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
        for info in &balance_resp.balance_infos {
            if info.currency == "CNY" {
                if let Ok(b) = info.total_balance.parse::<f64>() {
                    return Ok(b);
                }
            }
        }
        Ok(0.0)
    }

    fn model_name(&self) -> &str {
        &self.config.api.model
    }
}
