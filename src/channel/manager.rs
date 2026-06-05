//! ChannelManager — 管理多个 Channel 的生命周期

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::channel::{Channel, InboundMessage};

/// 通道管理器
///
/// 持有多个 Channel 实例，统一管理启动/停止生命周期。
/// 所有 Channel 的入站消息汇聚到同一个 `inbound_rx` 供 Agent 消费。
pub struct ChannelManager {
    /// 已注册的通道列表
    channels: Vec<Arc<dyn Channel>>,
    /// 入站消息发送端（Agent 消费 rx 端）
    inbound_tx: mpsc::UnboundedSender<InboundMessage>,
    /// 入站消息接收端
    inbound_rx: mpsc::UnboundedReceiver<InboundMessage>,
    /// 关闭广播信号
    shutdown_tx: broadcast::Sender<()>,
}

impl ChannelManager {
    /// 创建新的 ChannelManager
    pub fn new() -> Self {
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _) = broadcast::channel(16);
        Self {
            channels: Vec::new(),
            inbound_tx,
            inbound_rx,
            shutdown_tx,
        }
    }

    /// 注册一个 Channel（调用 start 前注册）
    pub fn register(&mut self, channel: Arc<dyn Channel>) {
        self.channels.push(channel);
    }

    /// 启动所有已注册的 Channel
    ///
    /// 每个 Channel 在自己的 tokio task 中运行，
    /// 通过共享的 inbound_tx 向 Agent 投递消息。
    pub fn start_all(&self) {
        let inbound_tx = self.inbound_tx.clone();
        let shutdown_tx = self.shutdown_tx.clone();
        for ch in &self.channels {
            let ch_name = ch.name();
            let ch_clone = ch.clone();
            let tx = inbound_tx.clone();
            let rx = shutdown_tx.subscribe();
            tokio::spawn(async move {
                tracing::info!("Channel[{ch_name}] 已启动");
                let handle = ch_clone.start(tx, rx);
                handle.await.ok();
                tracing::info!("Channel[{ch_name}] 已停止");
            });
        }
    }

    /// 获取入站消息接收端（供 Agent Loop 消费）
    pub fn inbound_rx(&mut self) -> &mut mpsc::UnboundedReceiver<InboundMessage> {
        &mut self.inbound_rx
    }

    /// 向所有 Channel 广播消息
    pub async fn broadcast(&self, chat_id: &str, text: &str) {
        for ch in &self.channels {
            if let Err(e) = ch.send_message(chat_id, text).await {
                tracing::warn!("Channel[{}] 发送失败: {e}", ch.name());
            }
        }
    }

    /// 获取指定名称的 Channel（用于定向发送）
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Channel>> {
        self.channels.iter().find(|c| c.name() == name)
    }

    /// 发送关闭信号，停止所有 Channel
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }

    /// 获取入站消息发送端（供外部注入消息）
    pub fn inbound_tx(&self) -> mpsc::UnboundedSender<InboundMessage> {
        self.inbound_tx.clone()
    }

    /// 已注册的通道数量
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
}
