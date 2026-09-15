//! 每个教学班独立的 Gossip 群组通信
//!
//! 对应 `docs/edu-p2p-design.md` §5。
//!
//! - **隔离**：每个教学班一个独立 `TopicId`，不同班的消息物理隔离。
//! - **派生**：`topic = BLAKE3("rhermes-section-topic" || seed || epoch)`，
//!   这样撤销成员时只需 `epoch += 1` 即可整体废弃旧 Topic（§5.4 治本方案）。
//! - **对等**：同一 Topic 内师生消息完全对等，学生之间也能互收（需求 §五 学生间通信）。

use blake3::Hasher;
use futures_util::StreamExt;
use iroh::{EndpointId, endpoint::Connection};
use iroh_gossip::{
    api::{Event, GossipReceiver, GossipSender},
    Gossip, TopicId,
};
use serde::{Deserialize, Serialize};

use super::model::SectionTicket;

/// 由「教学班种子 + Topic 纪元」确定性派生 32 字节 TopicId。
///
/// 纪元递增即换 Topic —— 撤销成员的核心机制。
pub fn derive_topic(seed: &[u8], epoch: u32) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(b"rhermes-section-topic");
    h.update(seed);
    h.update(&epoch.to_le_bytes());
    *h.finalize().as_bytes()
}

/// 把 hex 字符串还原成 `TopicId`
pub fn topic_from_hex(hex_str: &str) -> Option<TopicId> {
    let raw = hex::decode(hex_str).ok()?;
    let arr: [u8; 32] = raw.try_into().ok()?;
    Some(TopicId::from_bytes(arr))
}

/// 把票据里的 hex topic 还原成 `TopicId`（学生侧使用）
pub fn topic_from_ticket(t: &SectionTicket) -> anyhow::Result<TopicId> {
    topic_from_hex(&t.topic_id).ok_or_else(|| anyhow::anyhow!("topic_id 非法或长度不是 32 字节"))
}

/// 教学班内广播的消息类型。
///
/// 序列化用 postcard（紧凑）。
///
/// ⚠️ **必须用外部标记（externally tagged）枚举** —— 即不要加 `#[serde(tag = "..")]`。
/// postcard 是非自描述格式，遇到 internally tagged / untagged 枚举会直接返回
/// `Error::WontImplement`（实测：加 `#[serde(tag="k")]` 后 roundtrip 立即失败）。
/// 外部标记下变体编号编码为 varint，正好是 postcard 的原生表示。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SectionMsg {
    /// 作业发布通知 —— **只带哈希与票据，不带文件本体**（文件走 iroh-blobs）
    AssignmentPosted {
        assignment_id: i64,
        title: String,
        blob_hash: String,
        blob_ticket: String,
        due_date: String,
        posted_by: String,
        ts: String,
    },
    /// 学生提问
    Question {
        from: String,
        display_name: String,
        admin_class: String,
        text: String,
        ts: String,
    },
    /// 老师或同学回答
    Answer {
        from: String,
        display_name: String,
        reply_to: String,
        text: String,
        ts: String,
    },
    /// 班内自由讨论
    Chat {
        from: String,
        display_name: String,
        text: String,
        ts: String,
    },
    /// 教师公告 / 模式切换通知
    Announce {
        title: String,
        body: String,
        ts: String,
    },
    /// Topic 轮换通知（撤销成员后广播给**剩余成员**）。
    ///
    /// ⚠️ **故意不带 `new_topic_id`**（安全设计，勿"优化"回去）：
    /// 本消息在**旧 Topic** 上广播，而被撤销者**正是旧 Topic 的成员** —— 他一定会收到。
    /// 若把新 Topic 直接写在里面，他就能立刻跟着订阅新 Topic；而此时其他学生的白名单
    /// 尚未更新（新签名表还在路上），握手仍会放行，他就能继续偷听。
    ///
    /// 正确做法：只告知「纪元变了」，剩余成员各自走 `/class-app` 的 `RefreshTickets`
    /// （**认证信道**，被撤销者已不在白名单，会被 Hook 拒绝）换取新 Topic。
    TopicRotate {
        new_epoch: u32,
        reason: String,
        ts: String,
    },
    /// 签名成员表更新（§5.4 R2 治标层）。
    ///
    /// 老师用节点私钥对「当前纪元有效成员表」签名后广播，学生端校验签名后
    /// 写入本地 `AuthRegistry`，用于拦截「已被撤销的同伴」的直连。
    AllowlistUpdate {
        section_id: i64,
        epoch: u32,
        /// postcard 序列化的 `allowlist::SignedAllowlist`
        signed: Vec<u8>,
    },
}

fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl SectionMsg {
    pub fn question(from: &str, display_name: &str, admin_class: &str, text: &str) -> Self {
        Self::Question {
            from: from.into(),
            display_name: display_name.into(),
            admin_class: admin_class.into(),
            text: text.into(),
            ts: now_ts(),
        }
    }

    pub fn answer(from: &str, display_name: &str, reply_to: &str, text: &str) -> Self {
        Self::Answer {
            from: from.into(),
            display_name: display_name.into(),
            reply_to: reply_to.into(),
            text: text.into(),
            ts: now_ts(),
        }
    }

    pub fn chat(from: &str, display_name: &str, text: &str) -> Self {
        Self::Chat {
            from: from.into(),
            display_name: display_name.into(),
            text: text.into(),
            ts: now_ts(),
        }
    }

    pub fn announce(title: &str, body: &str) -> Self {
        Self::Announce {
            title: title.into(),
            body: body.into(),
            ts: now_ts(),
        }
    }

    /// 人类可读的一行摘要（TUI / 日志用）
    pub fn summary(&self) -> String {
        match self {
            SectionMsg::AssignmentPosted { title, due_date, .. } => {
                format!("📝 新作业：{title}（截止 {due_date}）")
            }
            SectionMsg::Question { display_name, text, .. } => {
                format!("❓ {display_name}: {text}")
            }
            SectionMsg::Answer { display_name, text, .. } => {
                format!("💡 {display_name}: {text}")
            }
            SectionMsg::Chat { display_name, text, .. } => {
                format!("💬 {display_name}: {text}")
            }
            SectionMsg::Announce { title, body, .. } => {
                format!("📢 {title}：{body}")
            }
            SectionMsg::TopicRotate { new_epoch, .. } => {
                format!("🔄 教学班 Topic 已轮换至纪元 {new_epoch}")
            }
            SectionMsg::AllowlistUpdate { epoch, .. } => {
                format!("🔐 成员白名单已更新（纪元 {epoch}）")
            }
        }
    }
}

/// 一个教学班的 Gossip 会话（持有 sender 以便随时广播）。
pub struct SectionSession {
    pub section_id: i64,
    topic: TopicId,
    sender: GossipSender,
}

impl SectionSession {
    /// 老师侧：为每个自己任教的教学班建会话（无需 bootstrap，老师是 Topic 起点）。
    pub async fn host(gossip: &Gossip, section_id: i64, seed: &[u8], epoch: u32) -> anyhow::Result<(Self, GossipReceiver)> {
        Self::join(gossip, section_id, seed, epoch, Vec::new()).await
    }

    /// 学生侧：凭票据加入，`bootstrap` 为引导节点（通常是老师 EndpointId）。
    pub async fn join(
        gossip: &Gossip,
        section_id: i64,
        seed: &[u8],
        epoch: u32,
        bootstrap: Vec<EndpointId>,
    ) -> anyhow::Result<(Self, GossipReceiver)> {
        let topic = TopicId::from_bytes(derive_topic(seed, epoch));
        Self::join_topic(gossip, section_id, topic, bootstrap).await
    }

    /// 直接用已确定的 `TopicId` 加入（学生侧持票据时用 —— 学生只有 topic_id，没有种子）。
    ///
    /// ⚠️ 有引导节点时，本函数会**等到真正建立至少一条连接**再返回（内含 15s 上限，
    /// 超时也照常返回，仅放弃等待）。原因：gossip 是 best-effort，**不做 store-and-forward**，
    /// 若在连接建立前老师就广播，那条消息会被直接丢弃 —— 而这正是"入班后收不到
    /// 白名单/公告"的根因。等待 `joined()` 能把这类竞态压到可忽略。
    pub async fn join_topic(
        gossip: &Gossip,
        section_id: i64,
        topic: TopicId,
        bootstrap: Vec<EndpointId>,
    ) -> anyhow::Result<(Self, GossipReceiver)> {
        let wait_for_join = !bootstrap.is_empty();
        let mut sub = gossip.subscribe(topic, bootstrap).await?;
        if wait_for_join {
            match tokio::time::timeout(std::time::Duration::from_secs(15), sub.joined()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("[section {section_id}] 加入 Topic 失败: {e}"),
                Err(_) => tracing::warn!(
                    "[section {section_id}] 等待 Topic 连接超时（15s），继续但可能短暂不可达"
                ),
            }
        }
        let (sender, receiver) = sub.split();
        Ok((
            Self {
                section_id,
                topic,
                sender,
            },
            receiver,
        ))
    }

    /// 老师侧：直接以某 `TopicId` 建会话（无 bootstrap）
    pub async fn host_topic(
        gossip: &Gossip,
        section_id: i64,
        topic: TopicId,
    ) -> anyhow::Result<(Self, GossipReceiver)> {
        Self::join_topic(gossip, section_id, topic, Vec::new()).await
    }

    pub fn topic(&self) -> TopicId {
        self.topic
    }

    /// 广播一条教学班消息
    pub async fn broadcast(&self, msg: &SectionMsg) -> anyhow::Result<()> {
        let bytes = postcard::to_allocvec(msg)?;
        self.sender.broadcast(bytes.into()).await?;
        Ok(())
    }

    /// 作业发布便捷入口
    pub async fn broadcast_assignment(
        &self,
        assignment_id: i64,
        title: &str,
        blob_hash: &str,
        blob_ticket: &str,
        due_date: &str,
        posted_by: &str,
    ) -> anyhow::Result<()> {
        self.broadcast(&SectionMsg::AssignmentPosted {
            assignment_id,
            title: title.into(),
            blob_hash: blob_hash.into(),
            blob_ticket: blob_ticket.into(),
            due_date: due_date.into(),
            posted_by: posted_by.into(),
            ts: now_ts(),
        })
        .await
    }

    /// 广播签名成员表（§5.4 R2 治标层）
    pub async fn broadcast_allowlist(
        &self,
        section_id: i64,
        epoch: u32,
        signed: &super::allowlist::SignedAllowlist,
    ) -> anyhow::Result<()> {
        let bytes = postcard::to_allocvec(signed)?;
        self.broadcast(&SectionMsg::AllowlistUpdate {
            section_id,
            epoch,
            signed: bytes,
        })
        .await
    }
}

/// 把接收端流转换为「(消息, 来源节点)」回调。
///
/// 返回 `JoinHandle`，可 `abort()` 以停止。
pub fn spawn_receiver(
    mut rx: GossipReceiver,
    section_id: i64,
    on_msg: impl Fn(SectionMsg, EndpointId) + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(ev) = rx.next().await {
            match ev {
                Ok(Event::Received(m)) => match postcard::from_bytes::<SectionMsg>(&m.content) {
                    Ok(msg) => on_msg(msg, m.delivered_from),
                    Err(e) => tracing::warn!("[section {section_id}] 消息解析失败: {e}"),
                },
                Ok(Event::NeighborUp(id)) => {
                    tracing::info!("[section {section_id}] 邻居加入: {id}")
                }
                Ok(Event::NeighborDown(id)) => {
                    tracing::info!("[section {section_id}] 邻居离开: {id}")
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("[section {section_id}] gossip 事件错误: {e}");
                    break;
                }
            }
        }
        tracing::info!("[section {section_id}] gossip 接收循环结束");
    })
}

/// 从一次已建立的连接上收下 gossip 的附加能力（P4 分布式白名单签名表用）。
///
/// 目前仅做占位：真正的签名成员表会随后续批次接入。
pub async fn ping_peer_via(_conn: &Connection) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_topic_deterministic() {
        let seed = [3u8; 32];
        assert_eq!(derive_topic(&seed, 0), derive_topic(&seed, 0));
        assert_ne!(derive_topic(&seed, 0), derive_topic(&seed, 1));
        assert_ne!(derive_topic(&seed, 0), derive_topic(&[4u8; 32], 0));
    }

    #[test]
    fn test_section_msg_postcard_roundtrip() {
        let msg = SectionMsg::question("2024001", "张三", "信工2201", "红黑树怎么删？");
        let bytes = postcard::to_allocvec(&msg).unwrap();
        let back: SectionMsg = postcard::from_bytes(&bytes).unwrap();
        match back {
            SectionMsg::Question { from, text, .. } => {
                assert_eq!(from, "2024001");
                assert_eq!(text, "红黑树怎么删？");
            }
            _ => panic!("应为 Question"),
        }
    }

    #[test]
    fn test_section_msg_summary() {
        let m = SectionMsg::announce("本周安排", "周五小测");
        assert!(m.summary().contains("周五小测"));
        let a = SectionMsg::AssignmentPosted {
            assignment_id: 1,
            title: "实验一".into(),
            blob_hash: "deadbeef".into(),
            blob_ticket: "t".into(),
            due_date: "2026-09-20".into(),
            posted_by: "张老师".into(),
            ts: now_ts(),
        };
        assert!(a.summary().contains("实验一"));
    }
}
