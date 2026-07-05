//! SessionRouter — 多会话路由
//!
//! 管理多个 AgentSession，按 `channel:chat_id` 分配会话。
//! 每个外部消息来源自动获得或复用对应的会话实例。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::agent::event_sink::{ChannelSink, EventSink};
use crate::agent::session::{AgentSession, SessionConfig};
use crate::agent::MemorySystem;
use crate::agent::SkillEngine;
use crate::channel::telegram::TelegramSink;
use crate::channel::{ChannelManager, InboundMessage};
use crate::provider::Transport;
use crate::tools::ToolDispatcher;

/// 会话路由器 — 按 `channel:chat_id` 管理 AgentSession
pub struct SessionRouter {
    sessions: HashMap<String, AgentSession>,
    dispatcher: Option<ToolDispatcher>,
    memory: Option<Arc<Mutex<MemorySystem>>>,
    skill_engine: Option<Arc<Mutex<SkillEngine>>>,
    transport: Arc<dyn Transport>,
    channel_mgr: Arc<ChannelManager>,
    config: SessionConfig,
    system_prompt: String,
    debug: Option<Arc<Mutex<crate::debug::SessionDebug>>>,
    /// 配置文件路径（用于 /model 等斜杠命令）
    config_path: std::path::PathBuf,
}

impl SessionRouter {
    pub fn new(
        dispatcher: Option<ToolDispatcher>,
        memory: Option<Arc<Mutex<MemorySystem>>>,
        skill_engine: Option<Arc<Mutex<SkillEngine>>>,
        transport: Arc<dyn Transport>,
        channel_mgr: Arc<ChannelManager>,
        config: &SessionConfig,
        system_prompt: String,
        debug: Option<Arc<Mutex<crate::debug::SessionDebug>>>,
        config_path: std::path::PathBuf,
    ) -> Self {
        Self {
            sessions: HashMap::new(),
            dispatcher,
            memory,
            skill_engine,
            transport,
            channel_mgr,
            config: config.clone(),
            system_prompt,
            debug,
            config_path,
        }
    }

    /// 路由一条入站消息到对应的 AgentSession
    pub async fn dispatch(&mut self, inbound: InboundMessage) {
        let key = format!("{}:{}", inbound.channel, inbound.chat_id);

        // 拦截斜杠命令（Gateway 模式）
        if inbound.content.starts_with("/model") {
            let reply = self.handle_model_command(&inbound.content);
            if !reply.is_empty() {
                self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                return;
            }
        }

        // 如果 session 不存在，创建一个新的
        if !self.sessions.contains_key(&key) {
            let sink: Arc<dyn EventSink> = if inbound.channel == "telegram" {
                Arc::new(TelegramSink::new(
                    self.channel_mgr.clone(),
                    inbound.chat_id.clone(),
                ))
            } else {
                Arc::new(ChannelSink::new(
                    self.channel_mgr.clone(),
                    inbound.channel.clone(),
                    inbound.chat_id.clone(),
                ))
            };

            let session = AgentSession::new(
                key.clone(),
                self.system_prompt.clone(),
                self.dispatcher.clone(),
                self.memory.clone(),
                self.skill_engine.clone(),
                self.transport.clone(),
                sink,
                self.config.clone(),
                self.debug.clone(),
            );
            self.sessions.insert(key.clone(), session);
        }

        // 处理消息
        if let Some(session) = self.sessions.get_mut(&key) {
            session.handle_message(&inbound.content).await;
        }
    }

    /// 处理 /model 斜杠命令，返回回复文本（空字符串表示非 model 命令）
    fn handle_model_command(&self, input: &str) -> String {
        let rest = input.trim_start_matches("/model").trim();
        if rest.is_empty() {
            format!(
                "当前模型: {}\n切换: /model set <模型名称>\n列表: /model list",
                self.transport.model_name()
            )
        } else if let Some(new_model) = rest.strip_prefix("set ") {
            let new_model = new_model.trim();
            if new_model.is_empty() {
                "用法: /model set <模型名称>".into()
            } else {
                // 热切换 + 持久化到 config.toml
                self.transport.set_model(new_model);
                if let Ok(mut cfg) = crate::core::Config::load(&self.config_path) {
                    cfg.api.model = new_model.to_string();
                    let provider_name = if cfg.agent.default_provider.is_empty() {
                        "deepseek"
                    } else {
                        &cfg.agent.default_provider
                    };
                    if let Some(p) = cfg.providers.get_mut(provider_name) {
                        p.model = Some(new_model.to_string());
                    }
                    let _ = cfg.save(&self.config_path);
                }
                format!("✅ 模型已切换为: {new_model}（已立即生效）")
            }
        } else if rest == "list" {
            // 从 config.toml 列出已配置的 providers 和模型
            let cfg = crate::core::Config::load(&self.config_path).unwrap_or_default();
            let mut lines = Vec::new();
            for (name, p) in &cfg.providers {
                if p.api_key.is_empty() && !matches!(name.as_str(), "ollama" | "lmstudio") { continue; }
                let model = p.model.clone().unwrap_or_default();
                if model.is_empty() {
                    lines.push(format!("  · {name}: (使用默认模型)"));
                } else {
                    lines.push(format!("  · {name}: {model}"));
                }
            }
            let body = if lines.is_empty() { "(无已配置的 provider)".into() }
                       else { lines.join("\n") };
            format!("可用模型列表:\n{body}\n切换: /model set <模型名称>")
        } else {
            "用法:\n  /model            — 查看当前模型\n  /model set <名称> — 切换模型（立即生效）\n  /model list       — 列出可用模型".into()
        }
    }

    /// 直接回复到外部通道（不经过 AgentSession）
    async fn reply_to_channel(&self, channel: &str, chat_id: &str, text: &str) {
        if let Some(ch) = self.channel_mgr.get(channel) {
            let _ = ch.send_message(chat_id, text).await;
        }
    }

    /// 获取会话数量
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// 热切换模型（所有 session 共享同一个 transport，切换后立即生效）
    pub fn set_model(&self, model: &str) {
        self.transport.set_model(model);
    }
}
