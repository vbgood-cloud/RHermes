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
        }
    }

    /// 路由一条入站消息到对应的 AgentSession
    pub async fn dispatch(&mut self, inbound: InboundMessage) {
        let key = format!("{}:{}", inbound.channel, inbound.chat_id);

        // 如果 session 不存在，创建一个新的
        if !self.sessions.contains_key(&key) {
            let sink = Arc::new(ChannelSink::new(
                self.channel_mgr.clone(),
                inbound.channel.clone(),
                inbound.chat_id.clone(),
            )) as Arc<dyn EventSink>;

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

    /// 获取会话数量
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}
