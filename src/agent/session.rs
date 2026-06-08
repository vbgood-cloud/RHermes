//! Agent Session — 提取自理型 Agent Loop
//!
//! 将 TUI 的 `init_api` 中内联的 ~600 行 Agent Loop 封装为独立的
//! `AgentSession` 结构体，通过 `EventSink` 输出事件。
//!
//! TUI 模式和 Gateway 模式共用相同的 `handle_message()` 逻辑。

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::agent::event_sink::EventSink;
use crate::agent::MemorySystem;
use crate::agent::SkillEngine;
use crate::api::{ApiMessage, ChatRequest, ToolCallData};
use crate::core::Context;
use crate::core::Message;
use crate::provider::Transport;
use crate::tools::{ToolCall, ToolDispatcher};

/// Session 配置
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub max_rounds: u32,
    pub compress_ratio: f64,
    pub creation_nudge_interval: u32,
    pub memory_nudge_interval: u32,
    pub tool_result_max_chars: usize,
}

impl SessionConfig {
    pub fn from_config(config: &crate::core::Config) -> Self {
        Self {
            max_rounds: config.agent.max_rounds,
            compress_ratio: config.agent.compression_ratio,
            creation_nudge_interval: config.agent.creation_nudge_interval,
            memory_nudge_interval: config.agent.memory_nudge_interval,
            tool_result_max_chars: config.display.tool_result_max_chars,
        }
    }
}

/// Agent 会话
///
/// 每个会话维护一个三段式 Context，独立处理消息。
pub struct AgentSession {
    session_id: String,
    context: Context,
    dispatcher: Option<ToolDispatcher>,
    memory: Option<Arc<Mutex<MemorySystem>>>,
    skill_engine: Option<Arc<Mutex<SkillEngine>>>,
    transport: Arc<dyn Transport>,
    sink: Arc<dyn EventSink>,
    config: SessionConfig,
    session_debug: Option<Arc<Mutex<crate::debug::SessionDebug>>>,
}

impl AgentSession {
    /// 创建新的 Agent 会话
    pub fn new(
        session_id: String,
        system_prompt: String,
        dispatcher: Option<ToolDispatcher>,
        memory: Option<Arc<Mutex<MemorySystem>>>,
        skill_engine: Option<Arc<Mutex<SkillEngine>>>,
        transport: Arc<dyn Transport>,
        sink: Arc<dyn EventSink>,
        config: SessionConfig,
        debug: Option<Arc<Mutex<crate::debug::SessionDebug>>>,
    ) -> Self {
        let context = Context::new(system_prompt);
        Self {
            session_id,
            context,
            dispatcher,
            memory,
            skill_engine,
            transport,
            sink,
            config,
            session_debug: debug,
        }
    }

    /// 获取 session_id
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 处理用户消息（完整的 Agent Loop）
    ///
    /// 包含多轮工具调用、上下文压缩、记忆召回/写入、技能提炼等全部能力。
    pub async fn handle_message(&mut self, user_msg: &str) {
        // 1. 用户消息 → Context
        self.context.push_to_log(Message::new(
            crate::tui::Role::User, user_msg,
        ));

        let max_rounds = self.config.max_rounds;
        let compress_ratio = self.config.compress_ratio;
        let creation_nudge_interval = self.config.creation_nudge_interval;
        let memory_nudge_interval = self.config.memory_nudge_interval;
        let tool_result_max_chars = self.config.tool_result_max_chars;

        let mut round = 0u32;
        let mut tool_call_counter: u32 = 0;
        loop {
            round += 1;
            if round > max_rounds {
                tracing::warn!("Agent Loop 超过 {} 轮，强制终止", max_rounds);
                self.sink.on_error(&format!("工具调用次数过多（超过 {} 轮），已终止", max_rounds)).await;
                break;
            }
            let mut final_text = String::new();
            let mut tool_calls: Vec<ToolCallData> = Vec::new();

            // 2a. 每 5 轮展示进化建议
            if round % 5 == 0 && round > 0 {
                if let Some(ref se) = self.skill_engine {
                    if let Ok(engine) = se.lock() {
                        let suggestions = engine.suggest_optimizations();
                        if suggestions.len() > 1 || !suggestions[0].starts_with("✅") {
                            let msg = format!("📊 进化建议:\n{}", suggestions.join("\n"));
                            self.context.push_to_log(Message::new(
                                crate::tui::Role::System, &msg,
                            ));
                        }
                    }
                }
            }

            // 2b. 上下文压缩检查
            const CONTEXT_WINDOW: usize = 128000;
            if self.context.needs_compress(CONTEXT_WINDOW, compress_ratio) {
                tracing::info!("Context 达到 80% 阈值，触发压缩");
                self.sink.on_chunk("⏳ 压缩历史记录...").await;
                let history_text: String = self.context.get_messages()
                    .iter()
                    .skip(1)
                    .map(|m| {
                        let role_label = match m.role.as_str() {
                            "user" => "用户",
                            "assistant" => "AI",
                            _ => "系统",
                        };
                        let preview: String = m.content.chars().take(500).collect();
                        format!("{}: {}", role_label, preview)
                    })
                    .collect::<Vec<_>>()
                    .join("\n---\n");
                let sys_prompt = "你是一个对话摘要助手。请将以下历史对话按 6 段结构总结，每段 1-2 行，用中文。如果某段无内容就写\"无\"。6 段为: Goal, Decisions & rationale, Files & code, Commands & outcomes, Errors & fixes, Pending & next step。只输出摘要，不要额外说明。";
                let sub_request = ChatRequest {
                    model: self.transport.model_name().to_string(),
                    messages: vec![
                        ApiMessage { role: "system".into(), content: sys_prompt.into() },
                        ApiMessage { role: "user".into(), content: history_text },
                    ],
                    stream: false,
                    max_tokens: Some(1024),
                    temperature: None,
                    tools: None,
                };
                let summary = match self.transport.chat(sub_request).await {
                    Ok(resp) => resp.choices.first()
                        .and_then(|c| c.message.content.as_ref())
                        .cloned()
                        .unwrap_or_else(|| "压缩失败".into()),
                    Err(e) => format!("压缩失败: {e}"),
                };
                let msg_count = self.context.scratch_count();
                let ctx_len = self.context.prefix_len() + self.context.log_len();
                let summary_for_archive = summary.clone();
                let session_id = self.session_debug.as_ref()
                    .and_then(|d| d.lock().ok())
                    .map(|d| d.session_id.clone())
                    .unwrap_or_else(|| "unknown".into());
                self.context.compress(CONTEXT_WINDOW, compress_ratio, |_| summary);
                crate::core::archive_compression(
                    &std::path::Path::new("."),
                    &session_id,
                    round,
                    msg_count,
                    ctx_len / 2,
                    &summary_for_archive,
                );
                self.sink.on_chunk("✅ 压缩完成\n").await;
            }

            // 2c. 记忆召回
            if let Some(ref mem) = self.memory {
                if let Ok(mut mem_lock) = mem.lock() {
                    if let Ok(results) = mem_lock.search(user_msg, 5) {
                        if !results.is_empty() {
                            let recall: String = results.iter()
                                .map(|e| format!("- [{}] {}", e.memory_type.as_str(), e.content))
                                .collect::<Vec<_>>()
                                .join("\n");
                            tracing::debug!("召回 {} 条记忆", results.len());
                            self.context.push_to_log(Message::new(
                                crate::tui::Role::System,
                                &format!("【相关记忆】\n{}", recall),
                            ));
                        }
                    }
                }
            }

            let messages: Vec<ApiMessage> = self.context.get_messages();

            // 3. 调用 API
            let request = ChatRequest {
                model: self.transport.model_name().to_string(),
                messages,
                stream: false,
                max_tokens: Some(4096),
                temperature: None,
                tools: Some(crate::tools::all_tool_defs()),
            };

            let chat_result = tokio::time::timeout(
                Duration::from_secs(120),
                self.transport.chat(request),
            ).await;

            match chat_result {
                Ok(Ok(response)) => {
                    if let Some(choice) = response.choices.first() {
                        tracing::debug!(
                            "API 响应: finish_reason={:?}, text_len={}, has_tool_calls={}",
                            choice.finish_reason,
                            choice.message.content.as_ref().map(|s| s.len()).unwrap_or(0),
                            choice.message.tool_calls.is_some(),
                        );
                        final_text = choice.message.content.clone().unwrap_or_default();
                        if !final_text.is_empty() {
                            self.sink.on_chunk(&final_text).await;
                        }
                        if let Some(ref calls) = choice.message.tool_calls {
                            tool_calls = calls.iter().map(|tc| ToolCallData {
                                id: tc.id.clone(),
                                name: tc.function.name.clone(),
                                arguments: tc.function.arguments.clone(),
                            }).collect();
                            if !tool_calls.is_empty() {
                                tracing::debug!("检测到 {} 个工具调用", tool_calls.len());
                                self.sink.on_tool_calls(&tool_calls).await;
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("API 调用失败: {e}");
                    self.sink.on_error(&format!("API 错误: {e}")).await;
                }
                Err(_) => {
                    tracing::error!("API 调用超时（120s）");
                    self.sink.on_error("API 请求超时（120秒），请检查网络或 API 服务状态").await;
                }
            }

            tracing::debug!("Context 消息数: {}", self.context.scratch_count());

            // 5. 工具调用执行
            if !tool_calls.is_empty() {
                tracing::info!("开始执行 {} 个工具调用", tool_calls.len());
                let calls_to_dispatch: Vec<ToolCall> = tool_calls
                    .iter()
                    .map(|tc| ToolCall {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments: serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null),
                    })
                    .collect();

                if let Some(ref dispatcher) = self.dispatcher {
                    let results = dispatcher.dispatch(calls_to_dispatch).await;
                    tracing::info!("工具执行完成: {} 个结果", results.len());
                    tool_call_counter += results.len() as u32;

                    // 安全检查: 全局工具调用次数限制
                    const MAX_TOTAL_TOOL_CALLS: u32 = 200;
                    if tool_call_counter > MAX_TOTAL_TOOL_CALLS {
                        tracing::warn!("⛔ 工具调用总数超限 ({})，强制终止 Agent Loop", tool_call_counter);
                        self.sink.on_error(&format!(
                            "工具调用次数过多（{} 次），可能存在循环，已强制终止。", tool_call_counter
                        )).await;
                        break;
                    }

                    if let Some(ref d) = self.session_debug {
                        if let Ok(mut dbg) = d.lock() {
                            for r in &results {
                                dbg.record_tool_call(&r.name, "", &r.output, r.duration_ms, r.success);
                            }
                        }
                    }

                    let mut has_delegate = false;
                    for r in &results {
                        tracing::debug!("工具结果: {} ({}ms, success={})", r.name, r.duration_ms, r.success);
                        if r.name == "delegate_task" {
                            has_delegate = true;
                            final_text = r.output.clone();
                            if !final_text.is_empty() {
                                self.sink.on_chunk(&final_text).await;
                            }
                            continue;
                        }
                        if r.name == "skill_patch" || r.name == "skill_create" {
                            self.sink.on_chunk(&format!("\n🧬 {}\n", r.output)).await;
                            self.sink.on_done().await;
                            self.context.push_to_log(Message::new(
                                crate::tui::Role::System,
                                &r.output,
                            ));
                            continue;
                        }
                        let mut output = r.output.clone();
                        let lines_before = output.lines().count();
                        if output.len() > tool_result_max_chars {
                            let truncated: String = output.chars().take(tool_result_max_chars).collect();
                            let lines_after = truncated.lines().count();
                            output = format!("{}\n... (共{}行, 截断{}行)", truncated, lines_before, lines_before - lines_after);
                        }
                        let result_msg = if r.success {
                            format!("工具「{}」执行成功 ({}ms):\n{}", r.name, r.duration_ms, output)
                        } else {
                            format!("工具「{}」执行失败:\n{}", r.name, output)
                        };
                        self.context.push_to_log(Message::new(
                            crate::tui::Role::User,
                            &result_msg,
                        ));
                        self.sink.on_tool_result(&r.name, &r.output, r.duration_ms, r.success).await;
                    }

                    if has_delegate {
                        self.sink.on_done().await;
                        break;
                    }
                    continue;
                }
            }

            // 6. 最终文本回复
            tracing::info!("Agent Loop 完成, final_text_len={}", final_text.len());

            if let Some(ref d) = self.session_debug {
                if let Ok(mut dbg) = d.lock() {
                    dbg.record_round(round, user_msg, &final_text, 0);
                }
            }

            if !final_text.is_empty() {
                self.context.push_to_log(Message::new(
                    crate::tui::Role::Assistant,
                    &final_text,
                ));
            }
            // 6a. 记忆写入
            if !final_text.is_empty() && !user_msg.is_empty() {
                if let Some(ref mem) = self.memory {
                    if let Ok(mut mem_lock) = mem.lock() {
                        let tags = vec!["auto", "conversation"];
                        let _ = mem_lock.remember(
                            &format!("【问题】{}\n【回答】{}", user_msg, final_text),
                            &tags, "rhermes",
                        );
                        tracing::debug!("记忆已写入");
                    }
                }
            }
            // 6b. 自动技能提炼
            if creation_nudge_interval > 0 && tool_call_counter >= creation_nudge_interval && !user_msg.is_empty() {
                tool_call_counter = 0;
                let nudge_msg = user_msg.to_string();
                let nudge_text = final_text.clone();
                let se = self.skill_engine.clone();
                let refine_transport = self.transport.clone();
                let config = crate::tools::get_global_config();
                tokio::spawn(async move {
                    if let Some(cfg) = config {
                        let result = crate::agent::auto_refine_skill(
                            &nudge_msg, &nudge_text, &cfg, refine_transport,
                        ).await;
                        tracing::info!("自动技能提炼结果: {} ({}ms)", result.output, result.duration_ms);
                    }
                });
            }
            // 6b2. 自动记忆提炼
            if memory_nudge_interval > 0 && round % memory_nudge_interval == 0 && !final_text.is_empty() {
                let mem_msg = user_msg.to_string();
                let mem_text = final_text.clone();
                let mem_transport = self.transport.clone();
                let config = crate::tools::get_global_config();
                tokio::spawn(async move {
                    if let Some(cfg) = config {
                        let result = crate::agent::auto_refine_memory(
                            &mem_msg, &mem_text, &cfg, mem_transport,
                        ).await;
                        tracing::info!("自动记忆提炼结果: {} ({}ms)", result.output, result.duration_ms);
                    }
                });
            }
            self.sink.on_done().await;
            break;
        }
    }
}
