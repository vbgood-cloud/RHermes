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
    fn model_name(&self) -> String;

    /// 热切换模型（默认空实现，向后兼容）
    fn set_model(&self, _model: &str) {}
}

// ---------------------------------------------------------------------------
// DeepSeek Transport
// ---------------------------------------------------------------------------

/// DeepSeek API 的 HTTP Transport 实现
pub struct DeepSeekTransport {
    http: reqwest::Client,
    config: Arc<Config>,
    /// 模型热切换 override；为 None 时 fallback 到 config.api.model
    model_override: Arc<std::sync::RwLock<Option<String>>>,
}

impl DeepSeekTransport {
    /// 从配置创建 Transport
    pub fn new(config: &Config) -> Self {
        let timeout = Duration::from_secs(config.request.timeout_secs);
        let http = crate::core::http_client::create_proxied_client(
            &config.proxy, "llm", timeout,
        );
        Self {
            http,
            config: Arc::new(config.clone()),
            model_override: Arc::new(std::sync::RwLock::new(None)),
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
        let url = self.config.api.base_url.trim_end_matches('/');
        tracing::debug!(
            "[transport] base_url={url}, model={}, api_key={}",
            self.config.api.model,
            if self.config.api_key.len() > 8 {
                format!("{}...{}", &self.config.api_key[..4], &self.config.api_key[self.config.api_key.len()-4..])
            } else {
                "(empty)".to_string()
            }
        );
        url
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
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            let _preview: String = body.chars().take(500).collect();
            return Err(ApiError::HttpStatus(status, _preview));
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
            let body = response.text().await.unwrap_or_default();
            let _preview: String = body.chars().take(500).collect();
            let _ = tx.send(ApiEvent::Error(format!("HTTP {status}: {_preview}")));
            return Err(ApiError::HttpStatus(status, _preview));
        }

        // 解析 SSE 流
        // 解析 OMNIRoute 在 SSE 流末尾返回的注释行元数据
        // 例如 ": x-omniroute-model=glm-4.7-flash" ": x-omniroute-latency-ms=70"
        let mut provider_meta = crate::api::ProviderMeta::default();

        let mut buffer = String::new();
        // DSML 格式检测: 底层模型可能用 DSML 标记返回 tool_calls
        let mut in_dsml = false;
        let mut dsml_buffer = String::new();
        // P0 fix: 累积流式 tool_calls（SSE 中 name 和 arguments 分多块返回）
        let mut acc_tool_calls: std::collections::BTreeMap<i32, (String, String, String)> = std::collections::BTreeMap::new();
        let mut stream = response.bytes_stream();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(ApiError::Request)?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            while let Some(event_end) = buffer.find("\n\n") {
                let event = buffer[..event_end].to_string();
                buffer = buffer[event_end + 2..].to_string();

                for line in event.lines() {
                    // OMNIRoute 注释行（": x-omniroute-xxx=yyy"）— 累积元数据
                    if let Some(rest) = line.strip_prefix(": x-omniroute-") {
                        if let Some((k, v)) = rest.split_once('=') {
                            let v = v.trim();
                            match k {
                                "model" => provider_meta.routed_model = Some(v.to_string()),
                                "provider" => provider_meta.provider = Some(v.to_string()),
                                "latency-ms" => provider_meta.latency_ms = v.parse().ok(),
                                "response-cost" => provider_meta.cost = v.parse().ok(),
                                "cache-hit" => provider_meta.cache_hit = Some(v == "true"),
                                "tokens-in" => provider_meta.tokens_in = v.parse().ok(),
                                "tokens-out" => provider_meta.tokens_out = v.parse().ok(),
                                _ => {}
                            }
                        }
                        continue;
                    }
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
                                        // DSML 检测: 底层模型可能以 DSML 格式返回 tool_calls
                                        // 一旦检测到 DSML 标记，停止发送文本，累积到 dsml_buffer
                                        if content.contains("DSML") || in_dsml {
                                            in_dsml = true;
                                            dsml_buffer.push_str(content);
                                            continue;
                                        }
                                        if tx
                                            .send(ApiEvent::StreamChunk(content.to_string()))
                                            .is_err()
                                        {
                                            return Ok(());
                                        }
                                    }
                                    // OMNIRoute / GLM / DeepSeek-R1 的思考流，独立分发
                                    if let Some(ref reasoning) = choice.delta.reasoning_content {
                                        if !reasoning.is_empty() {
                                        }
                                    }
                                    // P0 fix: 累积 tool_calls delta（不直接发送）
                                    if let Some(ref calls) = choice.delta.tool_calls {
                                        for tc in calls {
                                            let idx = tc.index;
                                            let entry = acc_tool_calls.entry(idx).or_insert_with(|| (String::new(), String::new(), String::new()));
                                            if let Some(ref id) = tc.id { entry.0 = id.clone(); }
                                            if let Some(ref f) = tc.function {
                                                if let Some(ref name) = f.name { entry.1 = name.clone(); }
                                                if let Some(ref args) = f.arguments { entry.2.push_str(args); }
                                            }
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

        // DSML 解析: 如果检测到 DSML 格式的 tool_calls，解析并追加到 acc_tool_calls
        if in_dsml && !dsml_buffer.is_empty() {
            tracing::debug!("[DSML] 检测到底层模型 DSML 格式 tool_calls ({} 字符)", dsml_buffer.len());
            if let Some(calls) = parse_dsml_tool_calls(&dsml_buffer) {
                tracing::debug!("[DSML] 解析出 {} 个 tool_calls", calls.len());
                for (i, (name, args)) in calls.into_iter().enumerate() {
                    let idx = 1000 + i as i32; // 避免和已有索引冲突
                    acc_tool_calls.insert(idx, (format!("dsml_{i}"), name, args));
                }
            } else {
                tracing::warn!("[DSML] 解析失败，原样输出");
                let _ = tx.send(ApiEvent::StreamChunk(dsml_buffer));
            }
        }

        // P0 fix: 流结束前发送累积的 tool_calls
        if !acc_tool_calls.is_empty() {
            let tool_data: Vec<ToolCallData> = acc_tool_calls.values()
                .map(|(id, name, args)| ToolCallData {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: args.clone(),
                })
                .collect();
            tracing::debug!("流式累积 {} 个 tool_calls", tool_data.len());
            let _ = tx.send(ApiEvent::ToolCalls(tool_data));
        }

        // 流结束前发送 ProviderMeta（如果收集到了任意字段）
        if provider_meta.routed_model.is_some()
            || provider_meta.provider.is_some()
            || provider_meta.latency_ms.is_some()
            || provider_meta.cost.is_some()
            || provider_meta.cache_hit.is_some()
        {
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
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            let _preview: String = body.chars().take(500).collect();
            return Err(ApiError::HttpStatus(status, _preview));
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

    fn model_name(&self) -> String {
        if let Ok(g) = self.model_override.read() {
            if let Some(m) = g.as_ref() {
                return m.clone();
            }
        }
        self.config.api.model.clone()
    }

    fn set_model(&self, model: &str) {
        if let Ok(mut g) = self.model_override.write() {
            *g = Some(model.to_string());
            tracing::info!("[Transport] 模型热切换: {model}");
        }
    }
}

/// 解析 DSML 格式的 tool_calls
///
/// 底层模型（DeepSeek 系列）可能用 DSML 标记返回工具调用:
/// <｜DSML｜tool_calls>
/// <｜DSML｜invoke name="function_name">
/// <｜DSML｜parameter name="param_name">value</｜DSML｜parameter>
/// </｜DSML｜invoke>
/// </｜DSML｜tool_calls>
///
/// 返回 Vec<(function_name, arguments_json)>
fn parse_dsml_tool_calls(text: &str) -> Option<Vec<(String, String)>> {
    let mut calls = Vec::new();

    // 按 invoke 分割
    let segments: Vec<&str> = text.split("<｜DSML｜invoke").collect();

    for segment in segments.iter().skip(1) {
        // 提取函数名: name="xxx"
        let name_match = segment
            .find("name=\"")
            .and_then(|pos| {
                let rest = &segment[pos + 6..];
                rest.find("\"").map(|end| rest[..end].to_string())
            });

        let Some(name) = name_match else { continue };

        if name.is_empty() {
            continue;
        }

        // 提取参数
        let mut params = serde_json::Map::new();
        let param_parts: Vec<&str> = segment.split("<｜DSML｜parameter").collect();

        for param_seg in param_parts.iter().skip(1) {
            // 提取参数名
            let param_name = param_seg
                .find("name=\"")
                .and_then(|pos| {
                    let rest = &param_seg[pos + 6..];
                    rest.find("\"").map(|end| rest[..end].to_string())
                });

            let Some(param_name) = param_name else { continue };

            // 提取参数值（> 和 </｜DSML｜parameter> 之间）
            let value = if let Some(gt) = param_seg.find('>') {
                let after = &param_seg[gt + 1..];
                if let Some(end) = after.find("</｜DSML｜parameter>") {
                    after[..end].to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            // 尝试解析为 JSON 值，否则作为字符串
            let json_value: serde_json::Value = serde_json::from_str(&value)
                .unwrap_or(serde_json::Value::String(value));

            params.insert(param_name, json_value);
        }

        let args = serde_json::Value::Object(params).to_string();
        calls.push((name, args));
    }

    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}