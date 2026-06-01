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
