//! 教学班运行时：把 `P2pNode` / gossip 会话 / 白名单状态编排成可直接使用的对象。
//!
//! 这是 P2（群组通信）、P3（blob 分发）、P4（撤销 + 教务）的**装配层**：
//! 上层的 CLI / TUI / Gateway 只需持有 `TeacherRuntime` 或 `StudentRuntime`，
//! 不必关心 Topic 派生、纪元轮换、会话重建等细节。
//!
//! ```text
//! TeacherRuntime::start(db)          StudentRuntime::connect(tickets)
//!        │                                     │
//!        ├─ P2pNode::teacher（双 ALPN + Hook）   ├─ P2pNode::student（含 Hook）
//!        ├─ sessions: section → (epoch, 会话)    ├─ 每班一个会话 + 接收循环
//!        ├─ announce / publish / revoke          ├─ SectionEvent 流（含白名单自动应用）
//!        └─ revoke → 轮换 → 双 Topic 广播        └─ TopicRotate → 自动重订阅
//! ```
//!
//! ⚠️ 会话缓存以**纪元**为键：`topic_epoch` 变化即重建会话，否则会继续往废弃 Topic 广播。

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use futures_util::StreamExt;
use iroh::EndpointId;
use iroh_gossip::api::{Event, GossipReceiver};
use iroh_gossip::{Gossip, TopicId};
use tokio::sync::mpsc;

use super::allowlist::{
    apply_signed_allowlist, revoke_and_rotate, AllowlistState, RevokeOutcome, SignedAllowlist,
};
use super::authz::{AuthRegistry, MemberBinding};
use super::gossip::{self, SectionMsg, SectionSession};
use super::model::SectionTicket;
use super::net::P2pNode;
use super::store::EduStore;

/// 某教学班当前纪元下的会话槽
struct SessionSlot {
    epoch: u32,
    session: SectionSession,
}

/// 学生端收到的教学班事件。
#[derive(Debug)]
pub enum SectionEvent {
    /// 普通业务消息（作业/提问/回答/讨论/公告）
    Message(SectionMsg),
    /// 老师广播的签名白名单已校验并写入本地注册表
    AllowlistApplied {
        section_id: i64,
        epoch: u32,
        members: usize,
    },
    /// Topic 已随老师轮换而切换
    TopicRotated {
        section_id: i64,
        new_epoch: u32,
        new_topic_id: String,
    },
    /// 接收循环结束（连接断开 / 流关闭）
    Closed { section_id: i64 },
}

// ===========================================================================
// 老师端运行时
// ===========================================================================

/// 老师端运行时：持有节点、按班维护会话，提供广播 / 发布 / 撤销入口。
pub struct TeacherRuntime {
    pub node: P2pNode,
    db_path: PathBuf,
    registry: AuthRegistry,
    sessions: HashMap<i64, SessionSlot>,
    pub allowlist_state: AllowlistState,
}

impl TeacherRuntime {
    /// 启动老师节点：恢复既有授权绑定 → 装配 P2P 节点。
    pub async fn start(db_path: PathBuf) -> anyhow::Result<Self> {
        let registry = AuthRegistry::new();

        // 预热白名单：从 DB 恢复「已授权的 EndpointId → 成员绑定」
        {
            let store = EduStore::open(&db_path)?;
            let mut by_ep: HashMap<EndpointId, MemberBinding> = HashMap::new();
            for (ep, username, disp, admin, section_id) in store.authorized_bindings()? {
                let Ok(id) = EndpointId::from_str(&ep) else {
                    tracing::warn!("跳过非法 EndpointId：{ep}");
                    continue;
                };
                by_ep
                    .entry(id)
                    .and_modify(|b| {
                        if !b.sections.contains(&section_id) {
                            b.sections.push(section_id);
                        }
                    })
                    .or_insert_with(|| MemberBinding::new(username, disp, admin, vec![section_id]));
            }
            let n = by_ep.len();
            registry.load_from(by_ep).await;
            tracing::info!("白名单预热完成：{n} 个已授权节点");
        }

        let node = P2pNode::teacher(db_path.clone(), registry.clone()).await?;
        Ok(Self {
            node,
            db_path,
            registry,
            sessions: HashMap::new(),
            allowlist_state: AllowlistState::new(),
        })
    }

    pub fn node_id(&self) -> EndpointId {
        self.node.node_id()
    }

    /// 读取某班当前 `(seed, epoch)`
    fn section_seed_epoch(&self, section_id: i64) -> anyhow::Result<(Vec<u8>, u32)> {
        let store = EduStore::open(&self.db_path)?;
        let sec = store
            .get_section(section_id)?
            .ok_or_else(|| anyhow::anyhow!("教学班 {section_id} 不存在"))?;
        Ok((sec.gossip_seed, sec.topic_epoch))
    }

    /// 确保某班存在**对应当前纪元**的会话；纪元变化时自动重建。
    pub async fn ensure_session(&mut self, section_id: i64) -> anyhow::Result<&SectionSession> {
        let (seed, epoch) = self.section_seed_epoch(section_id)?;
        let stale = match self.sessions.get(&section_id) {
            Some(slot) => slot.epoch != epoch,
            None => true,
        };
        if stale {
            let topic = TopicId::from_bytes(gossip::derive_topic(&seed, epoch));
            let (session, _rx) =
                SectionSession::host_topic(&self.node.gossip, section_id, topic).await?;
            if self.sessions.contains_key(&section_id) {
                tracing::info!("[section {section_id}] 会话随纪元轮换重建：epoch={epoch}");
            }
            self.sessions.insert(section_id, SessionSlot { epoch, session });
        }
        Ok(&self.sessions.get(&section_id).expect("刚插入").session)
    }

    /// 广播一条公告
    pub async fn announce(
        &mut self,
        section_id: i64,
        title: &str,
        body: &str,
    ) -> anyhow::Result<()> {
        let s = self.ensure_session(section_id).await?;
        let section_id = s.section_id;
        s.broadcast(&SectionMsg::announce(title, body)).await?;
        tracing::info!("[section {section_id}] 公告已广播：{title}");
        Ok(())
    }

    /// 广播一条自由消息（提问 / 回答 / 讨论）
    pub async fn broadcast_msg(&mut self, section_id: i64, msg: &SectionMsg) -> anyhow::Result<()> {
        let s = self.ensure_session(section_id).await?;
        s.broadcast(msg).await
    }

    /// **P3**：发布作业文件（blobs 内容寻址 + gossip 通知）
    pub async fn publish_assignment_file(
        &mut self,
        section_id: i64,
        assignment_id: i64,
        title: &str,
        bytes: &[u8],
        due_date: &str,
        teacher_name: &str,
    ) -> anyhow::Result<iroh_blobs::Hash> {
        // 先确保会话存在，再以**不可变**借用同时取 node 与 session
        // （不能把 ensure_session 的返回值留到后面用：那会锁住 &mut self）
        self.ensure_session(section_id).await?;
        let session = &self
            .sessions
            .get(&section_id)
            .expect("ensure_session 已插入")
            .session;
        super::blobs::publish_assignment_file(
            &self.node.blobs,
            &self.node.endpoint,
            session,
            assignment_id,
            title,
            bytes,
            due_date,
            teacher_name,
        )
        .await
    }

    /// **P4/R3**：撤销成员 → 轮换 Topic → 双 Topic 广播 → 会话切换到新纪元。
    pub async fn revoke_member(
        &mut self,
        section_id: i64,
        username: &str,
        reason: &str,
    ) -> anyhow::Result<RevokeOutcome> {
        let store = EduStore::open(&self.db_path)?;
        let (outcome, new_session) = {
            let key = self.node.endpoint.secret_key();
            revoke_and_rotate(
                &store,
                &self.registry,
                &self.node.gossip,
                key,
                section_id,
                username,
                reason,
            )
            .await?
        };
        self.sessions.insert(
            section_id,
            SessionSlot {
                epoch: outcome.new_epoch,
                session: new_session,
            },
        );
        tracing::info!(
            "[section {section_id}] 已撤销 {username}：epoch {} → {}",
            outcome.old_epoch,
            outcome.new_epoch
        );
        Ok(outcome)
    }

    /// **P4/R2**：主动对当前成员表签名并广播（用于恢复授权 / 周期性刷新）。
    pub async fn publish_allowlist(&mut self, section_id: i64) -> anyhow::Result<SignedAllowlist> {
        let (_, epoch) = self.section_seed_epoch(section_id)?;
        // 先确保会话存在（借入结束后再广播，避免同时可变借用 self）
        self.ensure_session(section_id).await?;
        let teacher_ep = self.node.endpoint.id().to_string();
        let signed = {
            let store = EduStore::open(&self.db_path)?;
            SignedAllowlist::build(&store, section_id, epoch, &teacher_ep)
                .map(|s| s.sign(self.node.endpoint.secret_key()))?
        };
        let s = self.sessions.get(&section_id).expect("ensure_session 已插入");
        s.session
            .broadcast_allowlist(section_id, epoch, &signed)
            .await?;
        Ok(signed)
    }

    /// 恢复成员授权并重新纳入签名白名单（撤销的逆操作）。
    pub async fn restore_member(
        &mut self,
        section_id: i64,
        username: &str,
    ) -> anyhow::Result<SignedAllowlist> {
        let signed = {
            let store = EduStore::open(&self.db_path)?;
            super::allowlist::restore_to_allowlist(
                &store,
                self.node.endpoint.secret_key(),
                section_id,
                username,
            )
            .await?
        };
        self.publish_allowlist(section_id).await?;
        Ok(signed)
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.node.shutdown().await
    }
}

// ===========================================================================
// 学生端运行时
// ===========================================================================

/// 学生端连接：凭票据接入若干教学班，事件经 `mpsc` 上报。
pub struct StudentRuntime {
    pub node: P2pNode,
    pub teacher_id: EndpointId,
    pub tickets: Vec<SectionTicket>,
    sessions: HashMap<i64, SessionSlot>,
    pub allowlist_state: AllowlistState,
    pub registry: AuthRegistry,
    pub events: mpsc::UnboundedReceiver<SectionEvent>,
}

impl StudentRuntime {
    /// 凭票据接入全部教学班，并为每班启动接收循环。
    ///
    /// `bootstrap` 取票据里的老师 EndpointId（票据来自认证信道，可信）。
    pub async fn connect(tickets: Vec<SectionTicket>) -> anyhow::Result<Self> {
        anyhow::ensure!(!tickets.is_empty(), "没有可用教学班票据");

        let registry = AuthRegistry::new();
        let allowlist_state = AllowlistState::new();
        let node = P2pNode::student(registry.clone()).await?;

        // 引导节点 = 首个票据的 bootstrap[0]（同一老师的多个班共享）
        let teacher_hex = tickets[0]
            .bootstrap
            .first()
            .ok_or_else(|| anyhow::anyhow!("票据缺少 bootstrap（老师 EndpointId）"))?;
        let teacher_id = EndpointId::from_str(teacher_hex)
            .map_err(|e| anyhow::anyhow!("bootstrap 非法 EndpointId {teacher_hex}: {e}"))?;

        let (tx, rx) = mpsc::unbounded_channel();
        let mut sessions = HashMap::new();

        for t in &tickets {
            let topic = gossip::topic_from_ticket(t)?;
            let (session, recv) =
                SectionSession::join_topic(&node.gossip, t.section_id, topic, vec![teacher_id])
                    .await?;
            spawn_section_loop(
                recv,
                node.gossip.clone(),
                t.section_id,
                teacher_id,
                registry.clone(),
                allowlist_state.clone(),
                tx.clone(),
            );
            sessions.insert(
                t.section_id,
                SessionSlot {
                    epoch: t.topic_epoch,
                    session,
                },
            );
            tracing::info!(
                "已接入教学班 {}（{} {}，epoch={}）",
                t.section_id,
                t.course_code,
                t.section_name,
                t.topic_epoch
            );
        }

        Ok(Self {
            node,
            teacher_id,
            tickets,
            sessions,
            allowlist_state,
            registry,
            events: rx,
        })
    }

    /// 向某班广播（学生也能发言 —— 同一 Topic 内完全对等）
    pub async fn broadcast(&self, section_id: i64, msg: &SectionMsg) -> anyhow::Result<()> {
        let slot = self
            .sessions
            .get(&section_id)
            .ok_or_else(|| anyhow::anyhow!("未接入教学班 {section_id}"))?;
        slot.session.broadcast(msg).await
    }

    /// 下一条事件（`None` 表示所有会话已关闭）
    pub async fn next_event(&mut self) -> Option<SectionEvent> {
        self.events.recv().await
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.node.shutdown().await
    }
}

// ===========================================================================
// 接收循环
// ===========================================================================

/// 学生端单教学班接收循环。
///
/// 自动处理两类**控制消息**（不上报给业务层）：
/// - `AllowlistUpdate`：校验签名 + 签发者 + 纪元单调后写入本地 `AuthRegistry`；
/// - `TopicRotate`：重订阅新 Topic（后续消息继续可达）。
pub fn spawn_section_loop(
    mut rx: GossipReceiver,
    gossip: Gossip,
    section_id: i64,
    teacher_id: EndpointId,
    registry: AuthRegistry,
    state: AllowlistState,
    tx: mpsc::UnboundedSender<SectionEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Some(ev) = rx.next().await else { break };
            match ev {
                Ok(Event::Received(m)) => {
                    let msg = match postcard::from_bytes::<SectionMsg>(&m.content) {
                        Ok(msg) => msg,
                        Err(e) => {
                            tracing::warn!("[section {section_id}] 消息解析失败: {e}");
                            continue;
                        }
                    };
                    match msg {
                        SectionMsg::AllowlistUpdate {
                            section_id: sid,
                            epoch,
                            signed,
                        } => match postcard::from_bytes::<SignedAllowlist>(&signed) {
                            Ok(sl) => {
                                match apply_signed_allowlist(&sl, &teacher_id, &registry, &state).await
                                {
                                    Ok(n) => {
                                        tracing::info!(
                                            "[section {sid}] 白名单已应用（epoch={epoch}，{n} 个节点）"
                                        );
                                        let _ = tx.send(SectionEvent::AllowlistApplied {
                                            section_id: sid,
                                            epoch,
                                            members: n,
                                        });
                                    }
                                    Err(e) => {
                                        tracing::warn!("[section {sid}] 白名单被拒: {e}")
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("[section {sid}] 白名单反序列化失败: {e}")
                            }
                        },
                        SectionMsg::TopicRotate {
                            new_epoch,
                            new_topic_id,
                            ..
                        } => match gossip::topic_from_hex(&new_topic_id) {
                            Some(topic) => {
                                match SectionSession::join_topic(
                                    &gossip,
                                    section_id,
                                    topic,
                                    vec![teacher_id],
                                )
                                .await
                                {
                                    Ok((_s, new_rx)) => {
                                        rx = new_rx;
                                        tracing::info!(
                                            "[section {section_id}] 已随老师轮换至 epoch {new_epoch}"
                                        );
                                        let _ = tx.send(SectionEvent::TopicRotated {
                                            section_id,
                                            new_epoch,
                                            new_topic_id,
                                        });
                                    }
                                    Err(e) => tracing::warn!(
                                        "[section {section_id}] 切换 Topic 失败: {e}"
                                    ),
                                }
                            }
                            None => tracing::warn!(
                                "[section {section_id}] 轮换通知里 topic 非法: {new_topic_id}"
                            ),
                        },
                        other => {
                            let _ = tx.send(SectionEvent::Message(other));
                        }
                    }
                }
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
        tracing::info!("[section {section_id}] 接收循环结束");
        let _ = tx.send(SectionEvent::Closed { section_id });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (tempfile::TempDir, EduStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        (tmp, store)
    }

    /// 会话槽的纪元语义：纪元变化必须触发重建（纯逻辑校验）。
    #[test]
    fn test_session_slot_epoch_semantics() {
        // 这里不构造真实 SessionSession（需要网络），只验证判定条件本身
        let cache: HashMap<i64, u32> = HashMap::new();
        // 未缓存 → 需要建
        assert!(cache.get(&1).map(|e| *e != 0).unwrap_or(true));
        let mut cache = HashMap::new();
        cache.insert(1i64, 0u32);
        // 已缓存同纪元 → 不需重建
        assert!(!cache.get(&1).map(|e| *e != 0).unwrap_or(true));
        // 纪元递增 → 需要重建
        assert!(cache.get(&1).map(|e| *e != 1).unwrap_or(true));
    }

    /// 老师撤销 → 会话表纪元同步（不触网，只验证 store 侧不变量）。
    #[tokio::test]
    async fn test_revoke_updates_epoch_in_store() {
        let (_tmp, store) = test_store();
        let t = store.create_teacher("张老师", "pw").unwrap();
        let c = store.create_course("CS201", "数据结构", t.id).unwrap();
        let sec = store.create_class("信工班", c.id).unwrap();
        store
            .create_student("2024001", "张三", "123456", Some(sec.id))
            .unwrap();

        let before = store.get_section(sec.id).unwrap().unwrap().topic_epoch;
        assert_eq!(before, 0);
        store.revoke_member(sec.id, "2024001").unwrap();
        let new_epoch = store.rotate_topic_epoch(sec.id).unwrap();
        assert_eq!(new_epoch, 1);
        assert_eq!(
            store.get_section(sec.id).unwrap().unwrap().topic_epoch,
            1
        );
    }
}
