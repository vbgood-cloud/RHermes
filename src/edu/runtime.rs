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
use std::sync::Arc;

use futures_util::StreamExt;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::api::{Event, GossipReceiver};
use iroh_gossip::{Gossip, TopicId};
use tokio::sync::{mpsc, RwLock};

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

/// 教学班的全局标识（定义见 [`crate::edu::model::SectionKey`]）。
///
/// 这里重新导出，使用方可以直接写 `runtime::SectionKey`。
pub use super::model::SectionKey;

/// 学生端收到的教学班事件。
#[derive(Debug)]
pub enum SectionEvent {
    /// 普通业务消息（作业/提问/回答/讨论/公告）。
    ///
    /// ⚠️ 必须带 `SectionKey`：一个学生可以同时听多门课、多个老师的课，
    /// 上层（TUI / 渠道 / REPL）要靠它把消息归到正确的班级。
    Message { key: SectionKey, msg: SectionMsg },
    /// 老师广播的签名白名单已校验并写入本地注册表
    AllowlistApplied {
        key: SectionKey,
        epoch: u32,
        members: usize,
    },
    /// Topic 已随老师轮换而切换（新 Topic 经认证信道取回，不随广播下发）
    TopicRotated { key: SectionKey, new_epoch: u32 },
    /// 接收循环结束（连接断开 / 流关闭）
    Closed { key: SectionKey },
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

        let node = if offline {
            P2pNode::teacher_offline(db_path.clone(), registry.clone()).await?
        } else {
            P2pNode::teacher(db_path.clone(), registry.clone()).await?
        };

        // 预热白名单：从 DB 恢复「已授权的 EndpointId → 成员绑定」
        //
        // ⚠️ 必须在**本机身份确定之后**做 —— 班键是 (老师 EndpointId, 班 id)，
        //    而每位老师各自一份 edu.db，班 id 都从 1 开始，不带老师身份会串班。
        {
            let me = node.node_id();
            let store = EduStore::open(&db_path)?;
            let mut by_ep: HashMap<EndpointId, MemberBinding> = HashMap::new();
            for (ep, username, disp, admin, section_id) in store.authorized_bindings()? {
                let Ok(id) = EndpointId::from_str(&ep) else {
                    tracing::warn!("跳过非法 EndpointId：{ep}");
                    continue;
                };
                let key = SectionKey::new(me.clone(), section_id);
                match by_ep.get_mut(&id) {
                    Some(b) => {
                        if !b.sections.contains(&key) {
                            b.sections.push(key);
                        }
                    }
                    None => {
                        by_ep.insert(id, MemberBinding::new(username, disp, admin, vec![key]));
                    }
                }
            }
            let n = by_ep.len();
            registry.load_from(by_ep).await;
            tracing::info!("白名单预热完成：{n} 个已授权节点");
        }

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

/// 一次「加入某位老师的课」的凭据集合。
///
/// 一个学生可以持有多份 `Enrollment`（多位老师），每份自带该老师的可达地址；
/// 票据里的 `bootstrap` 必须与 `teacher_addr.id` 一致，否则视为串票。
#[derive(Debug, Clone)]
pub struct Enrollment {
    /// 该老师的完整可达地址（离线模式含直连 IP:PORT；N0 模式可只含 EndpointId）
    pub teacher_addr: EndpointAddr,
    /// 该老师名下的教学班票据
    pub tickets: Vec<SectionTicket>,
}

/// 某教学班**当前纪元**的会话槽，跨任务共享。
///
/// 轮换由接收循环在后台任务里完成，而 `broadcast` 在 `StudentRuntime` 上调用 ——
/// 二者必须看到同一个会话。若只换接收端而不换发送端，轮换后学生仍会往废弃 Topic
/// 发消息：全班收不到，而滞留在旧 Topic 的被撤销者反而能收到（缺陷 E）。
type SharedSession = Arc<RwLock<SectionSession>>;

/// 学生端连接：凭票据接入若干教学班，事件经 `mpsc` 上报。
pub struct StudentRuntime {
    pub node: P2pNode,
    /// 本学生接入的全部老师（按首次出现顺序去重）
    pub teachers: Vec<EndpointAddr>,
    /// 教学班 → 该班所属老师（含可达地址）。轮换刷新票据、白名单校验都按此查表。
    pub section_teacher: HashMap<SectionKey, EndpointAddr>,
    pub tickets: Vec<SectionTicket>,
    sessions: HashMap<SectionKey, SharedSession>,
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

    /// 完整入口（单老师）：`teacher_addr` 承载「怎么连上老师」，用于轮换后刷新票据。
    pub async fn connect_at(
        tickets: Vec<SectionTicket>,
        teacher_addr: EndpointAddr,
        offline: bool,
    ) -> anyhow::Result<Self> {
        Self::connect_multi(vec![Enrollment { teacher_addr, tickets }], offline).await
    }

    /// **多老师**入口：每位老师一份凭据，各自认证、各自入班。
    ///
    /// 学生的 EndpointId 全程只有一个（一台设备一把钥匙），在每位老师处分别被
    /// 白名单收录；老师之间互不感知。任一老师拒绝只影响其名下班级。
    pub async fn connect_multi(
        enrollments: Vec<Enrollment>,
        offline: bool,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!enrollments.is_empty(), "没有可用教学班票据");
        let registry = AuthRegistry::new();
        let node = if offline {
            P2pNode::student_offline(registry.clone()).await?
        } else {
            P2pNode::student(registry.clone()).await?
        };
        Self::from_node_multi(node, enrollments).await
    }

    /// 复用一棵**已建好的**学生节点接入教学班（单老师包装）。
    ///
    /// 这是生产正解：学生必须先用**自己的**节点完成 `/class-auth` 认证
    /// （老师把 `remote_id` 写进白名单），再用**同一个** `EndpointId` 接 gossip ——
    /// 否则认证过的身份和入班的身份不是同一把钥匙，Hook 会拒之门外。
    pub async fn from_node(
        node: P2pNode,
        tickets: Vec<SectionTicket>,
        teacher_addr: EndpointAddr,
    ) -> anyhow::Result<Self> {
        Self::from_node_multi(node, vec![Enrollment { teacher_addr, tickets }]).await
    }

    /// 复用一棵**已建好的**学生节点接入**多位老师**名下的多个教学班。
    ///
    /// 每张票据自带签发老师的 `bootstrap`，因此「哪个班归哪位老师」由票据决定；
    /// 调用方只需补齐每位老师的**可达地址**（离线模式含 IP:PORT）。二者必须一致，
    /// 否则判为串票并直接报错。
    pub async fn from_node_multi(
        node: P2pNode,
        enrollments: Vec<Enrollment>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!enrollments.is_empty(), "没有可用教学班票据");

        // 离网模式下 gossip 只认显式地址 —— 先把每位老师的地址塞进本机地址簿
        for e in &enrollments {
            node.add_peer_addr(e.teacher_addr.clone());
        }

        let registry = node.registry.clone();
        let allowlist_state = AllowlistState::new();

        let (tx, rx) = mpsc::unbounded_channel();
        let mut sessions: HashMap<SectionKey, SharedSession> = HashMap::new();
        let mut section_teacher: HashMap<SectionKey, EndpointAddr> = HashMap::new();
        let mut teachers: Vec<EndpointAddr> = Vec::new();
        let mut all_tickets: Vec<SectionTicket> = Vec::new();

        for e in &enrollments {
            let teacher_id = e.teacher_addr.id;
            anyhow::ensure!(
                !e.tickets.is_empty(),
                "老师 {teacher_id} 名下没有可用教学班票据"
            );
            if !teachers.iter().any(|t| t.id == teacher_id) {
                teachers.push(e.teacher_addr.clone());
            }

            for t in &e.tickets {
                // 全局键：不同老师的班 id 会重复，必须带上老师身份
                let key = SectionKey::new(teacher_id, t.section_id);

                // 票据必须由本次连接用的这位老师签发，防止票据张冠李戴
                let boot = t.bootstrap.first().ok_or_else(|| {
                    anyhow::anyhow!(
                        "教学班 {} 的票据缺少 bootstrap（老师 EndpointId）",
                        t.section_id
                    )
                })?;
                anyhow::ensure!(
                    boot == &teacher_id.to_string(),
                    "教学班 {} 的票据签发者（{boot}）与提供的老师地址（{teacher_id}）不一致",
                    t.section_id
                );

                if sessions.contains_key(&key) {
                    tracing::warn!("教学班 {key} 重复出现，已跳过");
                    continue;
                }

                // ⚠️ 顺序至关重要：先同步白名单，再订阅 Topic。
                //
                // 学生的 `WhitelistHook` 用**本地白名单**决定放不放行 INBOUND 连接。
                // 若先订阅，老师/同学的回拨会在握手期被拒（实测日志：
                // `拒绝未授权节点 … 接入协议 /iroh-gossip/1` → `dial failed: rejected locally`），
                // gossip 因此判定该对端不可用，连接反复重建、消息大批丢失。
                // 走认证信道主动拉一次，就能拿到"入场券"。
                match client::fetch_allowlist(&node.endpoint, e.teacher_addr.clone(), t.section_id)
                    .await
                {
                    Ok(sl) => {
                        // 期望签发者是**本班**老师，而非全局唯一老师（缺陷 H：
                        // 第二个老师的白名单曾因「单一期望签发者」被直接拒绝）
                        match apply_signed_allowlist(&sl, &teacher_id, &registry, &allowlist_state)
                            .await
                        {
                            Ok(n) => tracing::info!(
                                "[class {key}] 入班即同步白名单：{n} 个成员（epoch={}）",
                                sl.epoch
                            ),
                            Err(err) => {
                                tracing::warn!("[class {key}] 入班白名单被拒: {err}")
                            }
                        }
                    }
                    Err(err) => tracing::warn!("[class {key}] 拉取白名单失败: {err}"),
                }

                let topic = gossip::topic_from_ticket(t)?;
                let (session, recv) = SectionSession::join_topic(
                    &node.gossip,
                    t.section_id,
                    topic,
                    vec![teacher_id],
                )
                .await?;
                let slot: SharedSession = Arc::new(RwLock::new(session));
                spawn_section_loop(
                    recv,
                    node.gossip.clone(),
                    node.endpoint.clone(),
                    key.clone(),
                    e.teacher_addr.clone(),
                    slot.clone(),
                    registry.clone(),
                    allowlist_state.clone(),
                    tx.clone(),
                );
                sessions.insert(key.clone(), slot);
                section_teacher.insert(key.clone(), e.teacher_addr.clone());
                tracing::info!(
                    "已接入教学班 {key}（{} {}，epoch={}）",
                    t.course_code,
                    t.section_name,
                    t.topic_epoch
                );
            }

            all_tickets.extend(e.tickets.iter().cloned());
        }

        Ok(Self {
            node,
            teachers,
            section_teacher,
            tickets: all_tickets,
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
    ///
    /// ⚠️ 发送句柄取自**共享槽**：轮换后 `spawn_section_loop` 会把新会话写回该槽，
    /// 因此这里始终发往当前纪元 Topic（缺陷 E）。
    pub async fn broadcast(&self, key: &SectionKey, msg: &SectionMsg) -> anyhow::Result<()> {
        let slot = self
            .sessions
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("未接入教学班 {key}"))?;
        let session = slot.read().await;
        session.broadcast(msg).await
    }

    /// 已接入的教学班（按老师 + 班 id 稳定排序，便于展示）
    pub fn sections(&self) -> Vec<SectionKey> {
        let mut keys: Vec<SectionKey> = self.sessions.keys().cloned().collect();
        keys.sort_by_key(|k| (k.teacher.to_string(), k.section_id));
        keys
    }

    /// 某班所属老师
    pub fn teacher_of(&self, key: &SectionKey) -> Option<&EndpointAddr> {
        self.section_teacher.get(key)
    }

    /// 某班**当前发送端**所在的 Topic。
    ///
    /// 轮换后必须变成新纪元派生的 Topic；若仍停在旧 Topic，说明共享槽没有被写回。
    pub async fn session_topic(&self, key: &SectionKey) -> Option<TopicId> {
        let slot = self.sessions.get(key)?;
        Some(slot.read().await.topic_id())
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
///
/// ⚠️ `slot` 是**发送句柄的共享槽**：轮换拿到新会话后必须写回，否则
/// `StudentRuntime::broadcast` 会一直用旧 Topic 的发送端（缺陷 E）。
pub fn spawn_section_loop(
    mut rx: GossipReceiver,
    gossip: Gossip,
    endpoint: Endpoint,
    key: SectionKey,
    teacher_addr: EndpointAddr,
    slot: SharedSession,
    registry: AuthRegistry,
    state: AllowlistState,
    tx: mpsc::UnboundedSender<SectionEvent>,
) -> tokio::task::JoinHandle<()> {
    let teacher_id = teacher_addr.id;
    // 循环内大量使用 `section_id`（票据查询 / 连接 / 日志）；全局键另存为 `key`
    let section_id = key.section_id;
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
                                            key: key.clone(),
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
                                            Ok((new_session, new_rx)) => {
                                                // ⚠️ 关键：把新纪元的会话写回**共享槽**。
                                                //    `broadcast` 从这里取发送句柄；只换 `rx`
                                                //    而不换发送端的话，学生轮换后就再也发不出
                                                //    消息，且消息会落到仍滞留着被撤销者的旧
                                                //    Topic 上（缺陷 E）。
                                                *slot.write().await = new_session;
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
                                                    key: key.clone(),
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
                            let _ = tx.send(SectionEvent::Message {
                                key: key.clone(),
                                msg: other,
                            });
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
        let _ = tx.send(SectionEvent::Closed { key: key.clone() });
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
