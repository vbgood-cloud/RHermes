//! RHermes DeepSeek API 客户端
//!
//! 封装 DeepSeek Chat Completion API 的调用，支持：
//! - 同步请求 & SSE 流式响应
//! - 自动重试（rate limit / 超时 / 网络错误）
//! - Token 用量追踪

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};


use crate::core::Config;
// Re-export ApiMessage from core (single definition)
pub use crate::core::ApiMessage;
use crate::provider::Transport;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
}

/// 工具定义（OpenAI 格式）
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 构建默认工具列表
#[deprecated(note = "请使用 tools_from_registry 替代，由 all_tool_defs() 统一调用")]
pub fn default_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "read_file".into(),
                description: "读取文件内容，支持 head/tail/range 参数".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "文件路径"},
                        "head": {"type": "integer", "description": "只返回前 N 行"},
                        "tail": {"type": "integer", "description": "只返回后 N 行"},
                        "range": {"type": "string", "description": "行范围如 50-100"}
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "write_file".into(),
                description: "写入文件（创建或覆盖），自动创建父目录".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "文件路径"},
                        "content": {"type": "string", "description": "文件内容"}
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "search_content".into(),
                description: "在文件中搜索文本模式，返回匹配的文件:行号（基于 ripgrep）".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "搜索模式（支持正则）"},
                        "path": {"type": "string", "description": "搜索目录（默认项目根）"},
                        "glob": {"type": "string", "description": "文件名过滤"},
                        "context_lines": {"type": "integer", "description": "上下文行数（默认 0，最大 3）"},
                        "max_results": {"type": "integer", "description": "最大结果数（默认 200，最大 1000）"}
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "run_command".into(),
                description: "在 shell 中执行命令".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "要执行的命令（Windows 用 cmd /c）"},
                        "timeout": {"type": "integer", "description": "超时秒数"},
                        "cwd": {"type": "string", "description": "工作目录（默认当前目录）"}
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "glob".into(),
                description: "按 glob 模式列出文件".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "glob 模式如 **/*.rs"},
                        "path": {"type": "string", "description": "搜索目录"}
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "get_current_time".into(),
                description: "获取当前日期和时间".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "web_search".into(),
                description: "搜索网络获取最新信息。返回标题、摘要和链接。支持中英文查询。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "搜索关键词"},
                        "max_results": {"type": "integer", "description": "最大结果数（默认 5，最大 10）"}
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "web_fetch".into(),
                description: "获取网页内容并提取可读文本".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "要获取的网页 URL"},
                        "max_chars": {"type": "integer", "description": "最大字符数（默认 5000）"}
                    },
                    "required": ["url"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "skill_list".into(),
                description: "列出所有已安装的技能名称和描述".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "skill_search".into(),
                description: "按关键词搜索已安装的技能".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string", "description": "搜索关键词"}},
                    "required": ["query"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "skill_create".into(),
                description: "创建新的可复用技能，让 AI 不断积累最佳实践".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "技能名称（小写英文+短横线）"},
                        "description": {"type": "string", "description": "一句话描述该技能的用途"},
                        "body": {"type": "string", "description": "技能正文 Markdown，描述执行步骤和注意事项"},
                        "category": {"type": "string", "description": "技能分类目录（如 analysis/utils/debug）"},
                        "allowed_tools": {"type": "string", "description": "允许的工具列表，逗号分隔"}
                    },
                    "required": ["name", "description", "body"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "skill_patch".into(),
                description: "更新已有技能的内容（打补丁进化），保留使用统计".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "要更新的技能名称"},
                        "description": {"type": "string", "description": "新的描述"},
                        "body": {"type": "string", "description": "新的技能正文"},
                        "allowed_tools": {"type": "string", "description": "新的工具列表，逗号分隔"}
                    },
                    "required": ["name"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "run_skill".into(),
                description: "执行一个已安装的技能并返回结果".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "技能名称"},
                        "arguments": {"type": "string", "description": "传给技能的任务描述"}
                    },
                    "required": ["name", "arguments"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "delegate_task".into(),
                description: "将子任务委托给独立的子 Agent 执行，返回分析结果".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task": {"type": "string", "description": "要子 Agent 完成的任务描述"},
                        "context": {"type": "string", "description": "额外的上下文信息"}
                    },
                    "required": ["task"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "read_pdf".into(),
                description: "读取 PDF 文件，返回纯文本内容".into(),
                parameters: serde_json::json!({"type": "object","properties": {"path": {"type": "string", "description": "PDF 文件路径"}},"required": ["path"]}),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "skill_manage".into(),
                description: "创建或更新技能，名称存在则 patch，不存在则 create".into(),
                parameters: serde_json::json!({"type": "object","properties": {"name": {"type": "string","description": "技能名称"},"description": {"type": "string","description": "技能描述"},"body": {"type": "string","description": "技能正文 Markdown"},"category": {"type": "string","description": "分类目录"}},"required": ["name","description","body"]}),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "memory".into(),
                description: "读写管理记忆文件 MEMORY.md 和 USER.md。action: add/replace/remove/read。target: user/memory。".into(),
                parameters: serde_json::json!({"type": "object","properties": {"action": {"type": "string","description": "操作: add(添加)/replace(替换)/remove(删除)/read(读取)"},"target": {"type": "string","description": "目标: user(USER.md)/memory(MEMORY.md)"},"content": {"type": "string","description": "要添加/替换的内容（add/replace 时必填）"},"old_text": {"type": "string","description": "要匹配的旧文本子串（replace/remove 时必填）"}},"required": ["action","target"]}),
            },
        },
    ]
}

/// 从 ToolRegistry 动态生成工具定义列表
pub fn tools_from_registry(registry: &crate::tools::ToolRegistry) -> Vec<ToolDef> {
    registry.all_entries().iter().map(|(name, tool)| {
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: name.clone(),
                description: tool.description(),
                parameters: param_defs_to_json(tool.parameters()),
            },
        }
    }).collect()
}

/// 将 ParamDef 列表转换为 JSON Schema
pub fn param_defs_to_json(params: Vec<crate::tools::ParamDef>) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for p in &params {
        let type_str = match p.param_type {
            crate::tools::ParamType::String => "string",
            crate::tools::ParamType::Integer => "integer",
            crate::tools::ParamType::Float => "number",
            crate::tools::ParamType::Boolean => "boolean",
            crate::tools::ParamType::Array => "array",
            crate::tools::ParamType::Object => "object",
        };
        properties.insert(
            p.name.clone(),
            serde_json::json!({
                "type": type_str,
                "description": p.description
            }),
        );
        if p.required {
            required.push(p.name.clone());
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
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
    #[serde(default)]
    pub tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    pub function: ResponseToolFunction,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseToolFunction {
    pub name: String,
    pub arguments: String,
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

/// 余额信息
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceInfo {
    pub currency: String,
    #[serde(default)]
    pub total_balance: String,
}

/// 余额响应
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceResponse {
    #[serde(default)]
    pub is_available: bool,
    #[serde(default)]
    pub balance_infos: Vec<BalanceInfo>,
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
        let timeout = Duration::from_secs(config.request.timeout_secs);

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
            api: crate::core::ApiConfig { model, base_url },
            ..Default::default()
        };
        Self::new(config)
    }

    /// 聊天完成（非流式）
    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError> {
        let url = format!("{}/chat/completions", self.config.api.base_url.trim_end_matches('/'));

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
            let preview: String = body.chars().take(500).collect();
            tracing::error!("API 返回 HTTP {}: {}", status, preview);
            return Err(ApiError::HttpStatus(status, preview));
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
        let url = format!("{}/chat/completions", self.config.api.base_url.trim_end_matches('/'));

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
            let preview: String = body.chars().take(500).collect();
            tracing::error!("API 返回 HTTP {}: {}", status, preview);
            let _ = tx.send(ApiEvent::Error(format!("HTTP {status}: {preview}")));
            return Err(ApiError::HttpStatus(status, preview));
        }

        // 解析 SSE 流
        let mut buffer = String::new();
        let mut stream = response.bytes_stream();
        // 工具调用累计器（跨 chunk 合并）
        let mut tool_acc: HashMap<i32, ToolCallData> = HashMap::new();

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
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            // 流结束，发送剩余的工具调用
                            flush_tool_acc(&mut tool_acc, &tx);
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
                                    // 转发文本块
                                    let content = choice.delta.content.as_deref().unwrap_or("");
                                    if !content.is_empty() {
                                        if tx
                                            .send(ApiEvent::StreamChunk(content.to_string()))
                                            .is_err()
                                        {
                                            return Ok(());
                                        }
                                    }

                                    // 合并工具调用（跨 chunk 按 index 组装）
                                    if let Some(ref calls) = choice.delta.tool_calls {
                                        for tc in calls {
                                            let entry = tool_acc.entry(tc.index).or_insert_with(|| ToolCallData {
                                                id: String::new(),
                                                name: String::new(),
                                                arguments: String::new(),
                                            });
                                            if let Some(ref id) = tc.id {
                                                if !id.is_empty() {
                                                    entry.id = id.clone();
                                                }
                                            }
                                            if let Some(ref func) = tc.function {
                                                if let Some(ref name) = func.name {
                                                    if !name.is_empty() {
                                                        entry.name = name.clone();
                                                    }
                                                }
                                                if let Some(ref args) = func.arguments {
                                                    if !args.is_empty() {
                                                        entry.arguments.push_str(args);
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // finish_reason == "tool_calls" → 发送合并后的工具调用
                                    if choice.finish_reason.as_deref() == Some("tool_calls") {
                                        flush_tool_acc(&mut tool_acc, &tx);
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

        // 流结束，发送完成事件（以及可能残留的工具调用）
        flush_tool_acc(&mut tool_acc, &tx);
        let _ = tx.send(ApiEvent::Done);

        Ok(())
    }

    /// 带自动重试的聊天完成
    pub async fn chat_with_retry(&self, request: ChatRequest) -> Result<ChatResponse, ApiError> {
        let max_retries = self.config.request.max_retries;
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
        let url = format!("{}/user/balance", self.config.api.base_url.trim_end_matches('/'));
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
            let preview: String = body.chars().take(500).collect();
            return Err(ApiError::HttpStatus(status, preview));
        }
        let balance_resp: BalanceResponse = resp.json().await.map_err(ApiError::Parse)?;
        // 提取 CNY 余额
        for info in &balance_resp.balance_infos {
            if info.currency == "CNY" {
                if let Ok(b) = info.total_balance.parse::<f64>() {
                    return Ok(b);
                }
            }
        }
        Ok(0.0)
    }

    /// 获取当前使用的模型名
    pub fn model(&self) -> &str {
        &self.config.api.model
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
// Transport trait 实现（兼容 Provider 层）
// ---------------------------------------------------------------------------

#[async_trait]
impl Transport for DeepSeekClient {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError> {
        self.chat(request).await
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
        tx: tokio::sync::mpsc::UnboundedSender<ApiEvent>,
    ) -> Result<(), ApiError> {
        self.chat_stream(request, tx).await
    }

    async fn get_balance(&self) -> Result<f64, ApiError> {
        self.get_balance().await
    }

    fn model_name(&self) -> String {
        self.model().to_string()
    }
}

/// 将工具调用累计器中的内容排序后发送
/// 按 index 排序确保顺序正确
fn flush_tool_acc(
    tool_acc: &mut HashMap<i32, ToolCallData>,
    tx: &tokio::sync::mpsc::UnboundedSender<ApiEvent>,
) {
    if tool_acc.is_empty() {
        return;
    }
    let mut merged: Vec<(i32, ToolCallData)> = tool_acc.drain().collect();
    merged.sort_by_key(|(index, _)| *index);
    let tool_data: Vec<ToolCallData> = merged.into_iter().map(|(_, td)| td).collect();
    let _ = tx.send(ApiEvent::ToolCalls(tool_data));
}

// ---------------------------------------------------------------------------
// SSE 流式 chunk
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct StreamChunk {
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StreamChoice {
    pub delta: StreamDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StreamDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StreamToolCall {
    pub index: i32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    #[serde(default)]
    pub function: Option<StreamToolCallFunction>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StreamToolCallFunction {
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
    /// HTTP 状态码错误（4xx/5xx），附带响应体摘要
    HttpStatus(u16, String),
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
            Self::HttpStatus(status, _) => *status >= 500,
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
            Self::HttpStatus(s, body) => {
                if body.is_empty() {
                    write!(f, "HTTP {s}")
                } else {
                    write!(f, "HTTP {s}: {body}")
                }
            }
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
            tools: None,
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
        assert!(ApiError::HttpStatus(500, String::new()).is_retryable());

        assert!(ApiError::HttpStatus(500, String::new()).is_retryable());
        assert!(ApiError::HttpStatus(502, String::new()).is_retryable());
        assert!(ApiError::HttpStatus(503, String::new()).is_retryable());

        assert!(!ApiError::HttpStatus(400, String::new()).is_retryable());
        assert!(!ApiError::HttpStatus(401, String::new()).is_retryable());
        assert!(!ApiError::HttpStatus(403, String::new()).is_retryable());
        assert!(!ApiError::HttpStatus(429, String::new()).is_retryable());

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
