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
    pub guardrails_enabled: bool,
    pub guardrails_max_retries: u32,
    pub guardrails_storm_window_secs: u64,
    pub guardrails_storm_max_repeats: u32,
}

impl SessionConfig {
    pub fn from_config(config: &crate::core::Config) -> Self {
        Self {
            max_rounds: config.agent.max_rounds,
            compress_ratio: config.agent.compression_ratio,
            creation_nudge_interval: config.agent.creation_nudge_interval,
            memory_nudge_interval: config.agent.memory_nudge_interval,
            tool_result_max_chars: config.display.tool_result_max_chars,
            guardrails_enabled: config.agent.guardrails_enabled,
            guardrails_max_retries: config.agent.guardrails_max_retries,
            guardrails_storm_window_secs: config.agent.guardrails_storm_window_secs,
            guardrails_storm_max_repeats: config.agent.guardrails_storm_max_repeats,
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
    /// 护栏管线（跨轮次持久，StormSuppression 需要历史）
    repair_pipeline: Option<crate::agent::repair::RepairPipeline>,
    /// 护栏重试计数（跨轮累加）
    guardrail_retry_count: u32,
    /// edu 学习模式：工具白名单（空 = 全部允许，由 router 在切课时设置）
    allowed_tools: Option<Vec<String>>,
    /// edu 学习模式：当前学习模式名（explore / scaffold / locked / None=通用模式）
    learn_mode: Option<String>,
    /// edu 反思闭环：上一轮结束时生成的反思提示上下文（等待学生下一轮回答）
    ///
    /// 方案 B：handle_message 入口检测此字段。命中则把当前 user_msg 当作反思回答，
    /// 调用 evaluate_reflection 评分，结果写入 reflection_outbox 供 router 落库。
    pending_reflection: Option<PendingReflection>,
    /// edu 反思闭环：本轮产出的反思评分结果（router 在 handle_message 返回后读取并落库）
    reflection_outbox: Option<ReflectionRecord>,
}

/// 等待学生回答的反思上下文
#[derive(Clone, Debug)]
struct PendingReflection {
    /// 上一轮对话的摘要（AI 回答前 200 字）
    conversation_summary: String,
    /// 上一轮用过的工具名
    tools_used: Vec<String>,
    /// 学习模式
    mode: String,
    /// 上一轮对话长度（用于 evaluate_reflection 的提问质量评估）
    conversation_length: usize,
}

/// 本轮产出的反思评分记录（供 router 落库）
#[derive(Clone, Debug)]
pub struct ReflectionRecord {
    /// 反思原文（学生的回答）
    pub reflection_text: String,
    /// 综合评分（0.0-1.0）
    pub overall_score: f64,
    /// 反思深度（0.0-1.0）
    pub depth: f64,
    /// 上一轮用过的工具（CSV 格式，供 journal.tool_calls 字段）
    pub tools_csv: String,
    /// 上一轮对话摘要（供 journal.topic 字段）
    pub topic: String,
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

        // 初始化护栏管线
        let repair_pipeline = if config.guardrails_enabled {
            Some(crate::agent::repair::RepairPipeline::new(
                config.guardrails_storm_window_secs,
                config.guardrails_storm_max_repeats,
            ))
        } else {
            None
        };

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
            repair_pipeline,
            guardrail_retry_count: 0,
            allowed_tools: None,
            learn_mode: None,
            pending_reflection: None,
            reflection_outbox: None,
        }
    }

    /// 获取 session_id
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// edu 学习模式控制：热更新 system_prompt + 工具白名单 + 模式名
    ///
    /// 由 SessionRouter 在 /sw 切课或 /mode 切换时调用。
    /// - `prompt`: 新的系统提示词（如 scaffold 的苏格拉底式 prompt）
    /// - `allowed_tools`: 工具白名单（None=不限，Some(空 vec)=全禁，Some(列表)=只允许这些）
    /// - `mode`: 学习模式名（"explore" / "scaffold" / "locked"）
    pub fn set_learn_mode(
        &mut self,
        prompt: Option<String>,
        allowed_tools: Option<Vec<String>>,
        mode: Option<String>,
    ) {
        if let Some(p) = prompt {
            self.context.set_system_prompt(p);
        }
        self.allowed_tools = allowed_tools;
        self.learn_mode = mode;
        // 切换课程/学习模式时清除上一轮遗留的反思上下文，
        // 避免旧 pending_reflection 污染新课程（下一条消息被误判为对旧课程的反思回答）。
        self.pending_reflection = None;
    }

    /// 当前学习模式名
    pub fn learn_mode(&self) -> Option<&str> {
        self.learn_mode.as_deref()
    }

    /// 取出本轮产出的反思评分记录（router 在 handle_message 返回后调用）
    ///
    /// 取出后清空 outbox，避免重复落库。
    pub fn take_reflection_record(&mut self) -> Option<ReflectionRecord> {
        self.reflection_outbox.take()
    }

    /// 清除待处理的反思（如切换课程/模式时调用，避免旧反思污染新一轮）
    pub fn clear_pending_reflection(&mut self) {
        self.pending_reflection = None;
    }

    /// 处理用户消息（完整的 Agent Loop）
    ///
    /// 包含多轮工具调用、上下文压缩、记忆召回/写入、技能提炼等全部能力。
    pub async fn handle_message(&mut self, user_msg: &str) {
        // 1. 用户消息 → Context
        self.context.push_to_log(Message::new(
            crate::tui::Role::User, user_msg,
        ));

        // 1.5 edu 反思闭环（方案 B）：检测上一轮遗留的 pending_reflection
        //
        // 若存在且当前消息不是 slash 命令，则把这条消息当作学生对上一轮反思提示的回答，
        // 调用 evaluate_reflection 评分，输出反馈，存入 outbox 供 router 落库，然后直接返回
        // （反思是元认知活动，不再触发 AI 回应，避免干扰 + 省 token）。
        if let Some(pr) = self.pending_reflection.take() {
            if !user_msg.trim().is_empty() && !user_msg.trim().starts_with('/') {
                use crate::edu::reflection::evaluate_reflection;
                let conv_len = pr.conversation_length + user_msg.len();
                let mut score = evaluate_reflection(user_msg, conv_len);
                score.calculate_overall();

                // 存入 outbox 供 router 落库
                self.reflection_outbox = Some(ReflectionRecord {
                    reflection_text: user_msg.to_string(),
                    overall_score: score.overall,
                    depth: score.depth,
                    tools_csv: pr.tools_used.join(","),
                    topic: pr.conversation_summary,
                });

                // 反馈给学生
                let feedback = format!(
                    "\n📝 **反思已记录**\n\
                     综合评分：{:.2}/1.00 ｜ 反思深度：{:.2}\n\n\
                     _（{}）_\n\n---",
                    score.overall,
                    score.depth,
                    if score.depth > 0.6 {
                        "深刻的反思！你分析了'为什么'，这是高质量学习的关键"
                    } else if score.depth > 0.3 {
                        "不错的开始，下次试着多问自己'为什么这样做'"
                    } else {
                        "反思偏简单，试着解释你的思考过程和理由"
                    }
                );
                self.sink.on_chunk(&feedback).await;
                self.sink.on_done().await;
                tracing::info!(
                    "edu 反思已评分 (深度={:.2}, 综合={:.2})",
                    score.depth,
                    score.overall
                );
                return;
            }
            // 是 slash 命令则保留 pending 不消费（放回去）
            else {
                self.pending_reflection = Some(pr);
            }
        }

        let max_rounds = self.config.max_rounds;
        let compress_ratio = self.config.compress_ratio;
        let creation_nudge_interval = self.config.creation_nudge_interval;
        let memory_nudge_interval = self.config.memory_nudge_interval;
        let tool_result_max_chars = self.config.tool_result_max_chars;

        let mut round = 0u32;
        let mut tool_call_counter: u32 = 0;
        // edu 反思用：收集本次对话使用的所有工具名
        let mut tools_used_this_turn: Vec<String> = Vec::new();
        loop {
            round += 1;
            if round > max_rounds {
                tracing::warn!("Agent Loop 超过 {} 轮，强制终止", max_rounds);
                self.sink.on_error(&format!("工具调用次数过多（超过 {} 轮），已终止", max_rounds)).await;
                break;
            }
            let mut final_text = String::new();
            let mut tool_calls: Vec<ToolCallData> = Vec::new();
            let mut choice_message_tool_calls: Option<Vec<crate::api::ResponseToolCall>> = None;

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
                    reasoning_effort: None,
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
                if let Ok(mem_lock) = mem.lock() {
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

            // 3. 调用 API（先发送 typing 状态）
            self.sink.on_typing().await;

            let request = ChatRequest {
                model: self.transport.model_name().to_string(),
                messages,
                stream: false,
                max_tokens: Some(4096),
                temperature: None,
                tools: Some(crate::tools::all_tool_defs()),
                reasoning_effort: infer_reasoning_effort(user_msg).map(|s| s.to_string()),
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
                            choice_message_tool_calls = Some(calls.clone());
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

            // 4.5 护栏管线 — 修复 + 校验 tool_calls
            if !tool_calls.is_empty() && self.repair_pipeline.is_some() {
                let pipeline = self.repair_pipeline.as_mut().unwrap();
                let api_calls = choice_message_tool_calls.as_deref();
                let repaired = pipeline.repair_with_api(&final_text, api_calls);

                // 记录修复动作
                for action in &repaired.actions {
                    tracing::debug!("护栏修复: {:?}", action);
                }

                // 如果有 dispatcher（持有 registry），执行校验
                if let Some(ref dispatcher) = self.dispatcher {
                    let registry = dispatcher.registry();
                    let registry_arc = std::sync::Arc::new(registry.clone());
                    let validator = crate::agent::guardrails::ResponseValidator::new(registry_arc);
                    let validation = validator.validate(&repaired.tool_calls);

                    if !validation.is_ok() && self.guardrail_retry_count < self.config.guardrails_max_retries {
                        // 有校验错误且未达重试上限 → 注入纠正消息
                        let nudge = crate::agent::guardrails::NudgeBuilder::build(
                            &validation.errors,
                            dispatcher.registry(),
                        );
                        tracing::warn!("护栏校验失败，注入纠正消息:\n{nudge}");
                        self.context.push_to_log(Message::new(
                            crate::tui::Role::System,
                            &nudge,
                        ));
                        self.guardrail_retry_count += 1;
                        continue; // 重新调用 API
                    }

                    // 校验通过或达到重试上限 → 用 valid_calls
                    tool_calls = validation.valid_calls.iter().map(|c| ToolCallData {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        arguments: c.arguments.to_string(),
                    }).collect();
                } else {
                    // 无 dispatcher → 直接用修复后的 calls
                    tool_calls = repaired.tool_calls.iter().map(|c| ToolCallData {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        arguments: c.arguments.to_string(),
                    }).collect();
                }

                if tool_calls.is_empty() && !repaired.tool_calls.is_empty() {
                    tracing::warn!("护栏：所有工具调用被校验拦截");
                }
            }

            // 4.6 edu 工具白名单拦截（locked 模式等）
            if !tool_calls.is_empty() {
                if let Some(ref allow) = self.allowed_tools {
                    let original_count = tool_calls.len();
                    let mut rejected: Vec<String> = Vec::new();
                    tool_calls.retain(|tc| {
                        let ok = allow.iter().any(|a| a == &tc.name);
                        if !ok { rejected.push(tc.name.clone()); }
                        ok
                    });
                    if !rejected.is_empty() {
                        let msg = format!(
                            "⚠️ 当前学习模式（{}）下以下工具不可用: {}\n请仅使用允许的工具，或改用允许的方式回答。",
                            self.learn_mode.as_deref().unwrap_or("locked"),
                            rejected.join(", ")
                        );
                        tracing::info!("edu 白名单拦截 {} 个工具: {}", rejected.len(), rejected.join(","));
                        self.context.push_to_log(Message::new(
                            crate::tui::Role::System,
                            &msg,
                        ));
                    }
                    if tool_calls.is_empty() && original_count > 0 {
                        // 所有工具都被拦截 → 跳过本轮工具执行，让模型重新回答
                        tracing::warn!("edu 白名单：本轮所有工具调用被拦截");
                        continue;
                    }
                }
            }

            // 5. 工具调用执行
            if !tool_calls.is_empty() {
                tracing::info!("开始执行 {} 个工具调用", tool_calls.len());
                for tc in &tool_calls {
                    tracing::info!("  工具: {}({})", tc.name, tc.arguments);
                }
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

                    // edu 反思：收集本轮用过的工具名
                    for r in &results {
                        if !tools_used_this_turn.contains(&r.name) {
                            tools_used_this_turn.push(r.name.clone());
                        }
                    }

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
                let _ = std::mem::replace(&mut tool_call_counter, 0);
                let nudge_msg = user_msg.to_string();
                let nudge_text = final_text.clone();
                let _se = self.skill_engine.clone();
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
            // 6c. edu 反思提示（仅 edu 学习模式）
            if self.learn_mode.is_some() && !user_msg.is_empty() {
                let mode = self.learn_mode.clone().unwrap_or_default();
                // 注意：final_text.len() 是字节数，[..200] 按字节切会在中文字符边界 panic。
                // 改为按字符数截断，保证 UTF-8 安全。
                let summary = if final_text.chars().count() > 200 {
                    let mut s: String = final_text.chars().take(200).collect();
                    s.push_str("...");
                    s
                } else {
                    final_text.clone()
                };
                let prompt = crate::edu::reflection::generate_reflection_prompt(
                    &summary,
                    &tools_used_this_turn,
                    &mode,
                );
                // 通过 sink 输出反思提示（追加在回答之后）
                let reflection_text = format!(
                    "\n\n---\n🤔 **学习反思**\n{}\n\n_（提示：好的反思分析\"为什么\"，而不只是总结）_",
                    prompt.question,
                );
                self.sink.on_chunk(&reflection_text).await;
                tracing::info!("edu 反思提示已生成 (模式={}, 工具={:?})", mode, tools_used_this_turn);

                // 设置 pending_reflection，下一轮 handle_message 入口会检测并评分
                self.pending_reflection = Some(PendingReflection {
                    conversation_summary: summary,
                    tools_used: tools_used_this_turn.clone(),
                    mode,
                    conversation_length: final_text.chars().count() + user_msg.chars().count(),
                });
            }

            self.sink.on_done().await;
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// reasoning_effort 推断（P2 动态思考强度）
//
// 根据用户消息特征推断合适的思考强度，对 omniroute/GLM-5.2 等支持 thinking
// 的模型生效；对不支持的模型发送该参数会被忽略（无副作用）。
//
// 判断依据：关键词 + 消息长度
// ---------------------------------------------------------------------------

fn infer_reasoning_effort(user_msg: &str) -> Option<&'static str> {
    let msg = user_msg.trim();
    let len = msg.chars().count();

    // 1. 复杂推理类（分析/设计/重构/对比/架构/why）→ 高 effort 思考更深（优先级最高）
    let complex_markers = [
        "分析", "设计", "重构", "优化", "对比", "比较", "为什么", "为何", "权衡",
        "architecture", "架构", "debug", "调试", "根因", "explain", "方案设计",
        "review", "审查", "评估",
    ];
    if complex_markers.iter().any(|m| msg.to_lowercase().contains(m)) || len > 500 {
        return Some("high");
    }

    // 2. 简单工具调用类（读取/查看/运行/list/cat 等）→ 低 effort 省 tokens
    let simple_markers = [
        "读取", "查看", "看下", "看一下", "运行", "执行", "列出", "list", "show", "cat",
        "ls ", "状态", "status", "当前目录", "pwd", "是什么", "在哪", "多少",
    ];
    if simple_markers.iter().any(|m| msg.to_lowercase().contains(m)) {
        return Some("low");
    }

    // 3. 极短消息（打招呼、简单确认，≤ 2 字符或命中 trivial 词表）→ 不触发思考
    let trivial_markers = ["hi", "hello", "你好", "嗯", "ok", "好的", "谢谢", "继续", "再见"];
    if len <= 2 || trivial_markers.iter().any(|m| msg.eq_ignore_ascii_case(m)) {
        return None;
    }

    // 4. 默认（中等对话、工具编排）→ medium
    Some("medium")
}

