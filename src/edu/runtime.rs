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
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::api::{Event, GossipReceiver};
use iroh_gossip::{Gossip, TopicId};
use tokio::sync::mpsc;

use super::allowlist::{
    apply_signed_allowlist, revoke_and_rotate, AllowlistState, RevokeOutcome, SignedAllowlist,
};
use super::authz::{AuthRegistry, MemberBinding};
use super::gossip::{self, SectionMsg, SectionSession};
use super::model::SectionTicket;
use super::net::{client, P2pNode};
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
    /// Topic 已随老师轮换而切换（新 Topic 经认证信道取回，不随广播下发）
    TopicRotated {
        section_id: i64,
        new_epoch: u32,
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
        Self::start_with(db_path, false).await
    }

    /// 离网模式（测试 / 校内无外网）：`presets::Minimal`，只靠显式地址直连。
    pub async fn start_offline(db_path: PathBuf) -> anyhow::Result<Self> {
        Self::start_with(db_path, true).await
    }

    pub async fn start_with(db_path: PathBuf, offline: bool) -> anyhow::Result<Self> {
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

        let node = if offline {
            P2pNode::teacher_offline(db_path.clone(), registry.clone()).await?
        } else {
            P2pNode::teacher(db_path.clone(), registry.clone()).await?
        };
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
    /// 老师完整地址（含直连 IP / 中继）。轮换后刷票据要用它重新连接。
    pub teacher_addr: EndpointAddr,
    pub tickets: Vec<SectionTicket>,
    sessions: HashMap<i64, SessionSlot>,
    pub allowlist_state: AllowlistState,
    pub registry: AuthRegistry,
    pub events: mpsc::UnboundedReceiver<SectionEvent>,
}

impl StudentRuntime {
    /// 凭票据接入全部教学班（生产路径：只知老师 EndpointId，靠 N0 发现解析地址）。
    pub async fn connect(tickets: Vec<SectionTicket>) -> anyhow::Result<Self> {
        let teacher_id = Self::teacher_from_tickets(&tickets)?;
        Self::connect_at(tickets, EndpointAddr {
            id: teacher_id,
            addrs: Default::default(),
        }, false)
        .await
    }

    /// 离网测试路径：显式给出老师地址，不启用中继与发现。
    pub async fn connect_offline(
        tickets: Vec<SectionTicket>,
        teacher_addr: EndpointAddr,
    ) -> anyhow::Result<Self> {
        Self::connect_at(tickets, teacher_addr, true).await
    }

    /// 完整入口：`teacher_addr` 承载「怎么连上老师」，用于轮换后刷新票据。
    pub async fn connect_at(
        tickets: Vec<SectionTicket>,
        teacher_addr: EndpointAddr,
        offline: bool,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!tickets.is_empty(), "没有可用教学班票据");
        let registry = AuthRegistry::new();
        let node = if offline {
            P2pNode::student_offline(registry.clone()).await?
        } else {
            P2pNode::student(registry.clone()).await?
        };
        Self::from_node(node, tickets, teacher_addr).await
    }

    /// 复用一棵**已建好的**学生节点接入教学班。
    ///
    /// 这是生产正解：学生必须先用**自己的**节点完成 `/class-auth` 认证
    /// （老师把 `remote_id` 写进白名单），再用**同一个** `EndpointId` 接 gossip ——
    /// 否则认证过的身份和入班的身份不是同一把钥匙，Hook 会拒之门外。
    pub async fn from_node(
        node: P2pNode,
        tickets: Vec<SectionTicket>,
        teacher_addr: EndpointAddr,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!tickets.is_empty(), "没有可用教学班票据");

        // 离网模式下 gossip 只认显式地址 —— 先把老师地址塞进本机地址簿
        node.add_peer_addr(teacher_addr.clone());

        let teacher_id = teacher_addr.id;
        let registry = node.registry.clone();
        let allowlist_state = AllowlistState::new();

        let (tx, rx) = mpsc::unbounded_channel();
        let mut sessions = HashMap::new();

        for t in &tickets {
            // ⚠️ 顺序至关重要：先同步白名单，再订阅 Topic。
            //
            // 学生的 `WhitelistHook` 用**本地白名单**决定放不放行 INBOUND 连接。
            // 若先订阅，老师/同学的回拨会在握手期被拒（实测日志：
            // `拒绝未授权节点 … 接入协议 /iroh-gossip/1` → `dial failed: rejected locally`），
            // gossip 因此判定该对端不可用，连接反复重建、消息大批丢失。
            // 走认证信道主动拉一次，就能拿到"入场券"。
            match client::fetch_allowlist(&node.endpoint, teacher_addr.clone(), t.section_id).await {
                Ok(sl) => {
                    match apply_signed_allowlist(&sl, &teacher_id, &registry, &allowlist_state).await
                    {
                        Ok(n) => tracing::info!(
                            "[section {}] 入班即同步白名单：{n} 个成员（epoch={}）",
                            t.section_id,
                            sl.epoch
                        ),
                        Err(e) => tracing::warn!("[section {}] 入班白名单被拒: {e}", t.section_id),
                    }
                }
                Err(e) => tracing::warn!("[section {}] 拉取白名单失败: {e}", t.section_id),
            }

            let topic = gossip::topic_from_ticket(t)?;
            let (session, recv) =
                SectionSession::join_topic(&node.gossip, t.section_id, topic, vec![teacher_id])
                    .await?;
            spawn_section_loop(
                recv,
                node.gossip.clone(),
                node.endpoint.clone(),
                t.section_id,
                teacher_addr.clone(),
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
            teacher_addr,
            tickets,
            sessions,
            allowlist_state,
            registry,
            events: rx,
        })
    }

    /// 从票据的 `bootstrap` 解析老师 EndpointId
    fn teacher_from_tickets(tickets: &[SectionTicket]) -> anyhow::Result<EndpointId> {
        let hex = tickets
            .first()
            .and_then(|t| t.bootstrap.first())
            .ok_or_else(|| anyhow::anyhow!("票据缺少 bootstrap（老师 EndpointId）"))?;
        EndpointId::from_str(hex)
            .map_err(|e| anyhow::anyhow!("bootstrap 非法 EndpointId {hex}: {e}"))
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
/// - `TopicRotate`：**不信任广播里的 Topic**，改走 `/class-app/1.0` 认证信道
///   `RefreshTickets` 取回新票据 → 派生新 Topic → 重订阅。
///
/// ⚠️ 为什么 `TopicRotate` 不下发新 Topic：轮换通知是在**旧 Topic**上广播的，
/// 而旧 Topic 里正坐着刚被撤销的学生。若把新 Topic 塞进广播，被撤销者也能立刻
/// 重订阅，撤销即失效。走认证信道则被撤销者在握手期就被 `WhitelistHook` 拒之门外。
pub fn spawn_section_loop(
    mut rx: GossipReceiver,
    gossip: Gossip,
    endpoint: Endpoint,
    section_id: i64,
    teacher_addr: EndpointAddr,
    registry: AuthRegistry,
    state: AllowlistState,
    tx: mpsc::UnboundedSender<SectionEvent>,
) -> tokio::task::JoinHandle<()> {
    let teacher_id = teacher_addr.id;
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
                        SectionMsg::TopicRotate { new_epoch, .. } => {
                            // 新 Topic 不随广播下发（旧 Topic 内有被撤销者），
                            // 改走认证信道刷新票据：被撤销者在此处被 Hook 拒绝。
                            match super::net::client::refresh_tickets(
                                &endpoint,
                                teacher_addr.clone(),
                            )
                            .await
                            {
                                Ok(tickets) => match tickets
                                    .iter()
                                    .find(|t| t.section_id == section_id)
                                    .cloned()
                                {
                                    Some(t) => match gossip::topic_from_ticket(&t) {
                                        Ok(topic) => match SectionSession::join_topic(
                                            &gossip,
                                            section_id,
                                            topic,
                                            vec![teacher_id],
                                        )
                                        .await
                                        {
                                            Ok((_s, new_rx)) => {
                                                rx = new_rx;
                                                // 新纪元的成员表也主动拉一次，不依赖广播是否到达
                                                if let Ok(sl) = super::net::client::fetch_allowlist(
                                                    &endpoint,
                                                    teacher_addr.clone(),
                                                    section_id,
                                                )
                                                .await
                                                {
                                                    if let Err(e) = apply_signed_allowlist(
                                                        &sl, &teacher_id, &registry, &state,
                                                    )
                                                    .await
                                                    {
                                                        tracing::warn!(
                                                            "[section {section_id}] 轮换后白名单被拒: {e}"
                                                        );
                                                    }
                                                }
                                                tracing::info!(
                                                    "[section {section_id}] 已随老师轮换至 epoch {new_epoch}（票据刷新成功）"
                                                );
                                                let _ = tx.send(SectionEvent::TopicRotated {
                                                    section_id,
                                                    new_epoch,
                                                });
                                            }
                                            Err(e) => tracing::warn!(
                                                "[section {section_id}] 切换 Topic 失败: {e}"
                                            ),
                                        },
                                        Err(e) => tracing::warn!(
                                            "[section {section_id}] 新票据派生 Topic 失败: {e}"
                                        ),
                                    },
                                    None => tracing::warn!(
                                        "[section {section_id}] 刷新后的票据不含本班（可能已被移出）"
                                    ),
                                },
                                Err(e) => tracing::warn!(
                                    "[section {section_id}] 轮换后刷新票据被拒（很可能已被移出教学班）: {e}"
                                ),
                            }
                        }
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
