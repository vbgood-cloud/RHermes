//! RHermes 子 Agent 系统
//!
//! 允许主 Agent 将子任务委托给独立的子 Agent 执行。
//! 子 Agent 在隔离的 tokio task 中运行，拥有自己的 API 请求。
//!
//! ## 使用场景
//!
//! - **并行调研**: 同时搜索多个文件，分别分析后汇总
//! - **深度分析**: 对单个文件进行深入分析，不影响主对话上下文
//! - **嵌套任务**: 子 Agent 可以再派生子 Agent（受深度限制）

use crate::api::{ChatRequest, DeepSeekClient};
use crate::core::Config;
use crate::api::default_tools;

/// 子 Agent 的执行结果
#[derive(Debug, Clone)]
pub struct SubAgentResult {
    /// 子 Agent 的输出文本
    pub output: String,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
    /// 是否成功
    pub success: bool,
}

/// 运行一个子 Agent 执行指定任务
///
/// 子 Agent 会创建一个独立的 API 调用，使用专门的 system prompt，
/// 专注于完成给定的任务并返回简洁结论。
pub async fn run_sub_agent(
    task: &str,
    context: &str,
    config: &Config,
) -> SubAgentResult {
    let start = std::time::Instant::now();

    // 构建子 Agent 的系统提示
    let system_prompt = format!(
        "你是一个专注的助手。请完成以下任务，直接给出结果，不要多余的解释。\
         \n\n## 任务\n{task}\
         \n\n## 上下文\n{context}\
         \n\n请基于以上信息完成任务并返回结果。"
    );

    let client = DeepSeekClient::new(config.clone());

    let request = ChatRequest {
        model: config.api.model.clone(),
        messages: vec![
            crate::api::ApiMessage {
                role: "system".into(),
                content: system_prompt,
            },
        ],
        stream: false,
        max_tokens: Some(2048),
        temperature: None,
        tools: Some(default_tools()),
    };

    match client.chat(request).await {
        Ok(response) => {
            let elapsed = start.elapsed().as_millis() as u64;
            if let Some(choice) = response.choices.first() {
                let text = choice.message.content.clone().unwrap_or_default();
                SubAgentResult {
                    output: text,
                    duration_ms: elapsed,
                    success: true,
                }
            } else {
                SubAgentResult {
                    output: "子 Agent 未返回结果".into(),
                    duration_ms: elapsed,
                    success: false,
                }
            }
        }
        Err(e) => {
            let elapsed = start.elapsed().as_millis() as u64;
            SubAgentResult {
                output: format!("子 Agent 执行失败: {e}"),
                duration_ms: elapsed,
                success: false,
            }
        }
    }
}

/// 自动技能提炼 Agent
///
/// 审查对话，决定是否创建或更新技能。最多执行 max_iterations 轮。
pub async fn auto_refine_skill(
    user_msg: &str,
    assistant_msg: &str,
    config: &Config,
) -> SubAgentResult {
    let start = std::time::Instant::now();
    let max_iterations = 8u32;

    let system_prompt = format!(
        "你是一个技能提炼助手。审查以下对话，判断是否有可固化的经验。\n\
        规则：\n\
        1. 如果对话中有经过试错、调整方向或用户期望不同做法的经验，用 skill_manage 创建或更新技能\n\
        2. 如果没什么值得保存的，回复 'Nothing to save.'\n\
        3. 最多执行 {max_iterations} 轮工具调用\n\
        4. 技能名称用小写英文+短横线\n\n\
        当前对话：\n用户: {user_msg}\nAI: {assistant_msg}"
    );

    let client = DeepSeekClient::new(config.clone());
    let tools = default_tools();

    let mut messages = vec![
        crate::api::ApiMessage {
            role: "system".into(),
            content: system_prompt,
        },
    ];

    for round in 0..max_iterations {
        let request = ChatRequest {
            model: config.api.model.clone(),
            messages: messages.clone(),
            stream: false,
            max_tokens: Some(4096),
            temperature: None,
            tools: Some(tools.clone()),
        };

        match client.chat(request).await {
            Ok(response) => {
                if let Some(choice) = response.choices.first() {
                    // 检查 tool_calls
                    if let Some(ref calls) = choice.message.tool_calls {
                        for call in calls {
                            let tool_name = &call.function.name;
                            let args_str = &call.function.arguments;

                            // 执行 skill_manage 工具
                            if tool_name == "skill_manage" {
                                if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_str) {
                                    let engine_arc = match crate::tools::get_global_skill_engine() {
                                        Some(e) => e,
                                        None => {
                                            messages.push(crate::api::ApiMessage {
                                                role: "assistant".into(),
                                                content: "技能引擎未初始化".into(),
                                            });
                                            continue;
                                        }
                                    };
                                    let mut engine = engine_arc.lock().unwrap_or_else(|e| e.into_inner());

                                    let name = args["name"].as_str().unwrap_or("unknown");
                                    let description = args["description"].as_str().unwrap_or("");
                                    let body = args["body"].as_str().unwrap_or("");
                                    let category = args.get("category").and_then(|v| v.as_str());

                                    if engine.get(name).is_some() {
                                        let _ = engine.update_skill(name, Some(description), Some(body), None, None);
                                        messages.push(crate::api::ApiMessage {
                                            role: "assistant".into(),
                                            content: format!("已更新技能「{name}」"),
                                        });
                                    } else {
                                        let skill_body = format!("---\nname: {name}\ndescription: {description}\nrun_as: subagent\n---\n\n# {name}\n\n{body}");
                                        let _ = engine.create_with_category(name, category, description, &skill_body, crate::agent::RunAs::Subagent);
                                        messages.push(crate::api::ApiMessage {
                                            role: "assistant".into(),
                                            content: format!("已创建技能「{name}」"),
                                        });
                                    }
                                }
                            } else {
                                messages.push(crate::api::ApiMessage {
                                    role: "assistant".into(),
                                    content: format!("工具「{tool_name}」执行失败（自动提炼仅支持 skill_manage）"),
                                });
                            }
                        }
                        // 继续下一轮
                        continue;
                    }

                    // 没有 tool_calls → 最终回复
                    let text = choice.message.content.clone().unwrap_or_default();
                    let elapsed = start.elapsed().as_millis() as u64;
                    let is_ok = !text.contains("失败") && !text.contains("Nothing to save");
                    return SubAgentResult {
                        output: text,
                        duration_ms: elapsed,
                        success: is_ok,
                    };
                }
            }
            Err(e) => {
                let elapsed = start.elapsed().as_millis() as u64;
                return SubAgentResult {
                    output: format!("自动提炼失败: {e}"),
                    duration_ms: elapsed,
                    success: false,
                };
            }
        }
    }

    let elapsed = start.elapsed().as_millis() as u64;
    SubAgentResult {
        output: "自动提炼超过最大轮次".into(),
        duration_ms: elapsed,
        success: false,
    }
}

/// 自动记忆提炼 Agent
///
/// 审查对话是否包含值得持久化的用户事实，调用 memory 工具写入 USER.md。
pub async fn auto_refine_memory(
    user_msg: &str,
    assistant_msg: &str,
    config: &Config,
) -> SubAgentResult {
    let start = std::time::Instant::now();
    let system_prompt = format!(
        "你是用户画像助手。审查以下对话中是否有值得跨会话记住的用户偏好、纠正或个人信息。\n\
        规则：\n\
        1. 如果发现值得记住的用户事实，用 memory(action='add', target='user', content='...') 记录下来\n\
        2. 如果没有值得记录的内容，回复 'Nothing to save.'\n\
        3. 只记录明确表达的用户事实，不要猜测或推断\n\
        4. 内容要简洁完整，如'用户偏好 Python 而非 JavaScript'\n\n\
        当前对话：\n用户: {user_msg}\nAI: {assistant_msg}"
    );

    let client = DeepSeekClient::new(config.clone());
    let tools = default_tools();

    let request = ChatRequest {
        model: config.api.model.clone(),
        messages: vec![crate::api::ApiMessage {
            role: "system".into(),
            content: system_prompt,
        }],
        stream: false,
        max_tokens: Some(2048),
        temperature: None,
        tools: Some(tools),
    };

    match client.chat(request).await {
        Ok(response) => {
            let elapsed = start.elapsed().as_millis() as u64;
            if let Some(choice) = response.choices.first() {
                let text = choice.message.content.clone().unwrap_or_default();
                // 注意：如果模型返回了 tool_calls（调用了 memory 工具），
                // 这里只记录文本回复。memory 工具调用由 Agent Loop 处理。
                SubAgentResult {
                    output: format!("记忆审查: {text}"),
                    duration_ms: elapsed,
                    success: !text.contains("Nothing to save"),
                }
            } else {
                SubAgentResult {
                    output: "记忆审查无结果".into(),
                    duration_ms: elapsed,
                    success: false,
                }
            }
        }
        Err(e) => {
            let elapsed = start.elapsed().as_millis() as u64;
            SubAgentResult {
                output: format!("记忆审查失败: {e}"),
                duration_ms: elapsed,
                success: false,
            }
        }
    }
}

/// 并行运行多个子 Agent
///
/// 所有子 Agent 同时启动，等待全部完成后返回结果。
pub async fn run_parallel(
    tasks: Vec<(String, String)>,
    config: &Config,
) -> Vec<SubAgentResult> {
    let mut handles = Vec::new();

    for (task, context) in tasks {
        let config = config.clone();
        handles.push(tokio::spawn(async move {
            run_sub_agent(&task, &context, &config).await
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(SubAgentResult {
                output: format!("子 Agent 任务崩溃: {e}"),
                duration_ms: 0,
                success: false,
            }),
        }
    }
    results
}
