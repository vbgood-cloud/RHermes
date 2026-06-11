//! RHermes 定时任务调度器
//!
//! 在 Gateway 模式下基于 cron 表达式定时执行 Agent 任务，
//! 结果可推送到指定 Channel。

use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{Mutex, Semaphore};

use crate::agent::AgentSession;
use crate::agent::EventSink;
use crate::agent::MemorySystem;
use crate::agent::SessionConfig;
use crate::agent::SkillEngine;
use crate::channel::ChannelManager;
use crate::core::Config;
use crate::core::ScheduledTaskConfig;
use crate::provider::Transport;
use crate::tools::ToolDispatcher;

/// 定时任务运行时
struct ScheduledTask {
    config: ScheduledTaskConfig,
    schedule: cron::Schedule,
}

/// 共享资源（从 Gateway 传入）
pub struct SchedulerShared {
    pub dispatcher: Option<ToolDispatcher>,
    pub memory: Option<Arc<StdMutex<MemorySystem>>>,
    pub skill_engine: Option<Arc<StdMutex<SkillEngine>>>,
    pub transport: Arc<dyn Transport>,
    pub channel_mgr: Arc<ChannelManager>,
    pub system_prompt: String,
    pub session_config: SessionConfig,
}

/// 定时任务调度器
pub struct Scheduler {
    tasks: Vec<ScheduledTask>,
    shared: SchedulerShared,
    semaphore: Arc<Semaphore>,
}

impl Scheduler {
    /// 从共享资源创建调度器（Gateway 集成用）
    pub fn with_shared(config: &Config, shared: SchedulerShared) -> Option<Self> {
        let sc = &config.scheduler;
        if !sc.enabled || sc.tasks.is_empty() {
            return None;
        }

        let mut tasks = Vec::new();
        for t in &sc.tasks {
            if !t.enabled {
                continue;
            }
            match t.cron.parse::<cron::Schedule>() {
                Ok(schedule) => {
                    tasks.push(ScheduledTask {
                        config: t.clone(),
                        schedule,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "[Scheduler] 任务 '{}' cron 表达式无效 '{}': {e}，跳过",
                        t.name, t.cron
                    );
                }
            }
        }

        if tasks.is_empty() {
            tracing::warn!("[Scheduler] 没有有效的定时任务，调度器未启动");
            return None;
        }

        let semaphore = Arc::new(Semaphore::new(sc.max_concurrent_tasks.max(1)));

        tracing::info!(
            "[Scheduler] 已加载 {} 个定时任务，最大并发 {}",
            tasks.len(),
            sc.max_concurrent_tasks
        );

        Some(Self {
            tasks,
            shared,
            semaphore,
        })
    }

    /// 启动所有定时任务（返回 JoinHandle，由 Gateway 持有确保生命周期）
    pub fn start(self) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();

        for task in self.tasks {
            let sem = self.semaphore.clone();
            let transport = self.shared.transport.clone();
            let dispatcher = self.shared.dispatcher.clone();
            let channel_mgr = self.shared.channel_mgr.clone();
            let system_prompt = self.shared.system_prompt.clone();
            let session_config = self.shared.session_config.clone();

            let name = task.config.name.clone();
            let prompt = task.config.prompt.clone();
            let target = task.config.target.clone();
            let schedule = task.schedule;

            // 计算首次触发时间用于日志
            let now = chrono::Utc::now();
            let next = schedule.upcoming(chrono::Utc).next();
            if let Some(next_time) = next {
                let local = next_time.with_timezone(&chrono::Local);
                tracing::info!(
                    "[Scheduler][{name}] 下次触发: {}",
                    local.format("%Y-%m-%d %H:%M:%S %Z")
                );
            }

            let handle = tokio::spawn(async move {
                // 等待到下一个整分钟，避免启动时立即触发
                let now = chrono::Local::now();
                let sec = now.format("%S").to_string().parse::<u64>().unwrap_or(0);
                let min_offset = 60 - sec;
                if min_offset > 5 && min_offset < 60 {
                    tokio::time::sleep(tokio::time::Duration::from_secs(min_offset)).await;
                }

                for next_time in schedule.upcoming(chrono::Utc) {
                    let now_utc = chrono::Utc::now();
                    if let Ok(dur) = (next_time - now_utc).to_std() {
                        tracing::info!(
                            "[Scheduler][{name}] 等待 {}s 后触发",
                            dur.as_secs()
                        );
                        tokio::time::sleep(dur).await;
                    }

                    // 并发控制
                    let _permit = sem.acquire().await;
                    let local = next_time.with_timezone(&chrono::Local);
                    tracing::info!(
                        "[Scheduler][{name}] ⏰ 触发: {}",
                        local.format("%Y-%m-%d %H:%M:%S")
                    );

                    // 创建独立的 AgentSession 执行任务
                    let sink = Arc::new(CaptureSink { result: Mutex::new(String::new()) });
                    let mut session = AgentSession::new(
                        format!("scheduler_{name}"),
                        system_prompt.clone(),
                        dispatcher.clone(),
                        None,  // memory
                        None,  // skill_engine
                        transport.clone(),
                        sink.clone(),
                        session_config.clone(),
                        None,  // debug
                    );

                    session.handle_message(&prompt).await;
                    let response = sink.take_result().await;
                    tracing::info!("[Scheduler][{name}] ✅ 完成 ({:.100}...)", response);

                    // 推送到目标渠道
                    if !target.is_empty() {
                        if let Some((ch, chat_id)) = target.split_once(':') {
                            if let Some(channel) = channel_mgr.get(ch) {
                                let msg = format!("⏰ 定时任务 [{name}]\n\n{response}");
                                if let Err(e) = channel.send_message(chat_id, &msg).await {
                                    tracing::warn!("[Scheduler][{name}] 推送失败: {e}");
                                }
                            } else {
                                tracing::warn!("[Scheduler][{name}] 目标渠道不存在: {ch}");
                            }
                        }
                    }

                    drop(_permit);
                    // 短暂睡眠防止同分钟内重复触发
                    tokio::time::sleep(tokio::time::Duration::from_secs(58)).await;
                }
            });

            handles.push(handle);
        }

        handles
    }
}

/// 捕获最终回复的 EventSink
struct CaptureSink {
    result: Mutex<String>,
}

impl CaptureSink {
    async fn take_result(&self) -> String {
        let mut guard = self.result.lock().await;
        std::mem::take(&mut *guard)
    }
}

#[async_trait::async_trait]
impl EventSink for CaptureSink {
    async fn on_chunk(&self, text: &str) {
        let mut guard = self.result.lock().await;
        guard.push_str(text);
    }
    async fn on_tool_calls(&self, _calls: &[crate::api::ToolCallData]) {}
    async fn on_tool_result(&self, _name: &str, _output: &str, _duration_ms: u64, _success: bool) {}
    async fn on_done(&self) {}
    async fn on_error(&self, error: &str) {
        tracing::warn!("[Scheduler] Agent 错误: {error}");
    }
}
