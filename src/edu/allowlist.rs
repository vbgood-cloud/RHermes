//! 分布式签名白名单（设计文档 §5.4 的 **R2 治标层**，与 R1 Topic 轮换组合为 **R3**）
//!
//! 为什么需要它：仅把被撤销者移出老师本地白名单，只能挡住它**直连老师**。
//! gossip 是网状传播的 —— 它仍知道 TopicId，可直连**其他学生**继续收发消息。
//! 因此学生端也必须能独立判断「谁是本班合法成员」。
//!
//! 机制：
//! - 老师用**自己的节点私钥**（Ed25519）对本班当前有效成员表签名后经 gossip 广播；
//! - 学生端本地 `verify()` 校验签名必须出自本班老师的 EndpointId（来自票据，可信）；
//! - 校验通过才把成员写入本地 `AuthRegistry`，供 `WhitelistHook` 拦截同伴连接。
//!
//! ⚠️ 两个必须的防御：
//! 1. **签发者校验**：`signed.teacher` 必须等于学生已知的老师 EndpointId，
//!    否则恶意学生可拿自己的私钥签一份「只有我自己」的表。
//! 2. **纪元单调（防回放）**：被撤销者可以回放**旧的**签名表重新把自己写回白名单。
//!    因此 `AllowlistState` 记住每班已见的最高纪元，只接受 `epoch >= known`。
//!
//! 签名载荷用 postcard 确定性序列化（结构体按字段顺序，无自描述开销）。

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use iroh::{EndpointId, PublicKey, SecretKey, Signature};
use iroh_gossip::Gossip;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::authz::{AuthRegistry, MemberBinding};
use super::gossip::{SectionMsg, SectionSession};
use super::model::{Member, MemberRole};
use super::store::EduStore;

/// 白名单载荷版本。结构变更时递增，旧版本一律拒绝加载。
pub const ALLOWLIST_VERSION: u32 = 1;

/// 签名成员表中的一个条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    /// Iroh EndpointId（Ed25519 公钥 hex）—— 唯一身份根
    pub endpoint_id: String,
    /// 学号 / 工号
    pub username: String,
    pub display_name: String,
    /// 行政班级（老师为空串）
    pub admin_class: String,
    /// "teacher" | "student"
    pub role: String,
}

impl AllowlistEntry {
    /// 该条目绑定的 EndpointId（hex 非法时返回 `None`）
    pub fn endpoint(&self) -> Option<EndpointId> {
        EndpointId::from_str(&self.endpoint_id).ok()
    }
}

/// 老师签名的成员表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAllowlist {
    pub version: u32,
    pub section_id: i64,
    /// 与 `edu_classes.topic_epoch` 对齐；学生只接受 `>=` 本地已知纪元（防回放）
    pub epoch: u32,
    /// 签发者 = 老师 EndpointId（hex）
    pub teacher: String,
    pub issued_at: String,
    /// 按 `endpoint_id` 升序排列（保证签名确定性）
    pub entries: Vec<AllowlistEntry>,
    /// Ed25519 签名（64 字节），覆盖**除本字段外**的全部内容
    pub signature: Vec<u8>,
}

/// 签名载荷：`SignedAllowlist` 去掉 `signature` 后的确定性表示。
#[derive(Debug, Serialize)]
struct AllowlistPayload<'a> {
    version: u32,
    section_id: i64,
    epoch: u32,
    teacher: &'a str,
    issued_at: &'a str,
    entries: &'a [AllowlistEntry],
}

impl SignedAllowlist {
    /// 生成未签名骨架（`signature` 为空）。
    ///
    /// `entries` 会被按 `endpoint_id` 排序以固定签名载荷。
    pub fn unsigned(
        section_id: i64,
        epoch: u32,
        teacher: impl Into<String>,
        mut entries: Vec<AllowlistEntry>,
    ) -> Self {
        entries.sort_by(|a, b| a.endpoint_id.cmp(&b.endpoint_id));
        Self {
            version: ALLOWLIST_VERSION,
            section_id,
            epoch,
            teacher: teacher.into(),
            issued_at: chrono::Utc::now().to_rfc3339(),
            entries,
            signature: Vec::new(),
        }
    }

    /// 待签名的确定性字节。
    pub fn payload_bytes(&self) -> Vec<u8> {
        let payload = AllowlistPayload {
            version: self.version,
            section_id: self.section_id,
            epoch: self.epoch,
            teacher: &self.teacher,
            issued_at: &self.issued_at,
            entries: &self.entries,
        };
        postcard::to_allocvec(&payload).expect("白名单载荷序列化不应失败")
    }

    /// 用老师节点私钥签名（消费 self，返回带签名版本）。
    pub fn sign(mut self, key: &SecretKey) -> Self {
        let sig = key.sign(&self.payload_bytes());
        self.signature = sig.to_bytes().to_vec();
        self
    }

    /// 校验签名是否确实出自 `self.teacher` 声明的 EndpointId。
    ///
    /// 注意：这**只**证明「表是老师签的」，**不**证明「表是新鲜的」——
    /// 新鲜度由 `AllowlistState` 的纪元单调性负责（防旧表回放）。
    pub fn verify(&self) -> bool {
        if self.version != ALLOWLIST_VERSION {
            return false;
        }
        if self.signature.len() != 64 {
            return false;
        }
        let Ok(pk) = PublicKey::from_str(&self.teacher) else {
            return false;
        };
        let mut arr = [0u8; 64];
        arr.copy_from_slice(&self.signature);
        let sig = Signature::from_bytes(&arr);
        pk.verify(&self.payload_bytes(), &sig).is_ok()
    }

    /// 表中是否含某节点
    pub fn contains(&self, id: &EndpointId) -> bool {
        let hex = id.to_string();
        self.entries.iter().any(|e| e.endpoint_id == hex)
    }

    /// 全部可解析的 EndpointId
    pub fn endpoints(&self) -> Vec<EndpointId> {
        self.entries.iter().filter_map(|e| e.endpoint()).collect()
    }

    /// 学生数（不含老师）
    pub fn student_count(&self) -> usize {
        self.entries.iter().filter(|e| e.role == "student").count()
    }

    /// 从成员列表构建（**纯函数**，便于单测）。
    ///
    /// - 老师条目由 `teacher_endpoint` 强制纳入（成员行的 endpoint 在老师首次绑定前为空）；
    /// - 学生仅纳入 `authorized == true` 且已绑定 `endpoint_id` 者。
    pub fn build_from_members(
        section_id: i64,
        epoch: u32,
        teacher_endpoint: &str,
        members: &[Member],
    ) -> Self {
        let mut entries: Vec<AllowlistEntry> = Vec::new();

        // 老师：取成员行里的姓名信息（若有），endpoint 用调用方提供的权威值。
        // 注意：老师成员行的 `username` 是工号/姓名而非 EndpointId，故按角色匹配。
        let teacher_row = members.iter().find(|m| m.role == MemberRole::Teacher);
        entries.push(AllowlistEntry {
            endpoint_id: teacher_endpoint.to_string(),
            username: teacher_row
                .map(|m| m.username.clone())
                .unwrap_or_else(|| teacher_endpoint.to_string()),
            display_name: teacher_row
                .map(|m| m.display_name.clone())
                .unwrap_or_else(|| "老师".to_string()),
            admin_class: String::new(),
            role: "teacher".to_string(),
        });

        for m in members {
            if m.role != MemberRole::Student {
                continue;
            }
            if !m.authorized {
                continue;
            }
            let Some(ep) = m.endpoint_id.as_ref() else {
                continue;
            };
            if ep == teacher_endpoint {
                continue;
            }
            entries.push(AllowlistEntry {
                endpoint_id: ep.clone(),
                username: m.username.clone(),
                display_name: m.display_name.clone(),
                admin_class: m.admin_class.clone(),
                role: "student".to_string(),
            });
        }

        Self::unsigned(section_id, epoch, teacher_endpoint, entries)
    }

    /// 从数据库读取成员并构建。
    pub fn build(
        store: &EduStore,
        section_id: i64,
        epoch: u32,
        teacher_endpoint: &str,
    ) -> anyhow::Result<Self> {
        let members = store.list_section_members(section_id)?;
        Ok(Self::build_from_members(
            section_id,
            epoch,
            teacher_endpoint,
            &members,
        ))
    }
}

/// 纪元单调记录（学生端防旧表回放）。
///
/// 每班记住已接受的**最高**纪元；仅接受 `epoch >= known` 的表。
#[derive(Debug, Clone, Default)]
pub struct AllowlistState {
    inner: Arc<RwLock<HashMap<i64, u32>>>,
}

impl AllowlistState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 试探某纪元的表是否可接受（不改变状态）。
    pub async fn accepts(&self, section_id: i64, epoch: u32) -> bool {
        self.inner
            .read()
            .await
            .get(&section_id)
            .map(|&known| epoch >= known)
            .unwrap_or(true)
    }

    /// 接受并记录纪元；若低于已知纪元返回 `false`（拒绝回放）。
    pub async fn accept(&self, section_id: i64, epoch: u32) -> bool {
        let mut w = self.inner.write().await;
        let known = w.get(&section_id).copied();
        match known {
            Some(k) if epoch < k => false,
            _ => {
                w.insert(section_id, epoch);
                true
            }
        }
    }

    /// 已知最高纪元
    pub async fn current_epoch(&self, section_id: i64) -> Option<u32> {
        self.inner.read().await.get(&section_id).copied()
    }
}

/// 学生端：校验并应用老师广播的签名白名单。
///
/// 三重校验：签名有效 → 签发者是本班老师 → 纪元不倒退。
/// 通过后用整表**替换**该班在本地 `AuthRegistry` 中的授权（先清旧后写新）。
///
/// 返回表中成员数（老师 + 学生）。
pub async fn apply_signed_allowlist(
    signed: &SignedAllowlist,
    expected_teacher: &EndpointId,
    registry: &AuthRegistry,
    state: &AllowlistState,
) -> Result<usize, String> {
    if !signed.verify() {
        return Err("白名单签名无效".to_string());
    }
    if signed.teacher != expected_teacher.to_string() {
        return Err(format!(
            "白名单签发者不符：期望 {expected_teacher}，实为 {}",
            signed.teacher
        ));
    }
    if !state.accept(signed.section_id, signed.epoch).await {
        return Err(format!(
            "白名单纪元倒退（收到 {}，已知 {}）—— 已拒绝旧表回放",
            signed.epoch,
            state
                .current_epoch(signed.section_id)
                .await
                .unwrap_or(u32::MAX)
        ));
    }

    // 先撤销该班旧成员（防止已退出者残留），再写入新表
    let stale: Vec<EndpointId> = {
        let snap = registry.snapshot().await;
        snap.into_iter()
            .filter(|(_, b)| b.belongs_to(signed.section_id))
            .map(|(id, _)| id)
            .collect()
    };
    for id in stale {
        registry.revoke(&id).await;
    }

    let mut applied = 0usize;
    for entry in &signed.entries {
        let Some(id) = entry.endpoint() else {
            tracing::warn!(
                "[section {}] 白名单条目 EndpointId 非法: {}",
                signed.section_id,
                entry.endpoint_id
            );
            continue;
        };
        // 保留该节点在其他班已有的绑定，仅追加本班
        let mut sections = registry
            .binding(&id)
            .await
            .map(|b| b.sections)
            .unwrap_or_default();
        if !sections.contains(&signed.section_id) {
            sections.push(signed.section_id);
        }
        registry
            .grant(
                id,
                MemberBinding::new(
                    entry.username.clone(),
                    entry.display_name.clone(),
                    entry.admin_class.clone(),
                    sections,
                ),
            )
            .await;
        applied += 1;
    }
    Ok(applied)
}

/// 撤销结果。
#[derive(Debug, Clone)]
pub struct RevokeOutcome {
    /// 被撤销学生此前绑定的 EndpointId（若有）
    pub revoked_endpoint: Option<String>,
    pub old_epoch: u32,
    pub new_epoch: u32,
    /// 新 Topic 的 hex
    pub new_topic_id: String,
    /// 轮换后重新签发的白名单
    pub allowlist: SignedAllowlist,
}

/// **R3 组合撤销**（治本 + 治标）：
///
/// 1. 落库撤销该学生（清 endpoint 绑定 + `authorized = 0`）；
/// 2. 内存 `AuthRegistry` 同步移除该 EndpointId（老师端直连立即失效）；
/// 3. `topic_epoch += 1` —— 旧 Topic 作废（治本）；
/// 4. 向**旧 Topic** 广播 `TopicRotate`，让在线剩余成员切到新 Topic；
/// 5. 用老师私钥对**新**成员表签名，向**新 Topic** 广播 `AllowlistUpdate`（治标）；
/// 6. 返回新纪元的 `SectionSession`，调用方应改用它继续广播。
///
/// 离线成员下次认证时会从 `/class-auth` 票据拿到新纪元与新 Topic，无需依赖第 4 步。
pub async fn revoke_and_rotate(
    store: &EduStore,
    registry: &AuthRegistry,
    gossip: &Gossip,
    secret_key: &SecretKey,
    section_id: i64,
    username: &str,
    reason: &str,
) -> anyhow::Result<(RevokeOutcome, SectionSession)> {
    let section = store
        .get_section(section_id)?
        .ok_or_else(|| anyhow::anyhow!("教学班 {section_id} 不存在"))?;
    let seed = section.gossip_seed.clone();
    let old_epoch = section.topic_epoch;

    // ① 落库撤销
    let revoked_endpoint = store.revoke_member(section_id, username)?;

    // ② 内存白名单同步移除
    if let Some(ep) = revoked_endpoint.as_ref() {
        if let Ok(id) = EndpointId::from_str(ep) {
            registry.revoke(&id).await;
        }
    }

    // ③ 纪元轮换
    let new_epoch = store.rotate_topic_epoch(section_id)?;
    let new_topic = super::gossip::derive_topic(&seed, new_epoch);

    // ④ 旧 Topic 上广播轮换通知（尽力而为：离线成员靠下次认证补票）
    let old_topic_id = hex::encode(super::gossip::derive_topic(&seed, old_epoch));
    if let Ok((old_session, _rx)) =
        SectionSession::host(gossip, section_id, &seed, old_epoch).await
    {
        let notice = SectionMsg::TopicRotate {
            new_epoch,
            new_topic_id: hex::encode(new_topic),
            reason: reason.to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) = old_session.broadcast(&notice).await {
            tracing::warn!("[section {section_id}] TopicRotate 广播失败（不影响轮换）: {e}");
        }
        let _ = old_topic_id; // 仅用于日志语义，旧 Topic 自此不再使用
    }

    // ⑤ 新表签名 + 新 Topic 广播
    let teacher_ep = secret_key.public().to_string();
    let allowlist = SignedAllowlist::build(store, section_id, new_epoch, &teacher_ep)?
        .sign(secret_key);

    let (new_session, _rx) =
        SectionSession::host(gossip, section_id, &seed, new_epoch).await?;
    new_session
        .broadcast_allowlist(section_id, new_epoch, &allowlist)
        .await?;

    Ok((
        RevokeOutcome {
            revoked_endpoint,
            old_epoch,
            new_epoch,
            new_topic_id: hex::encode(new_topic),
            allowlist,
        },
        new_session,
    ))
}

/// 恢复成员授权（撤销的逆操作）：重新写入 `authorized = 1` 不轮换 Topic。
///
/// ⚠️ 由于撤销时已轮换 Topic，该学生需**重新认证**以获取新票据；
/// 本函数只负责把 DB 状态改回可授权，并把他重新纳入签名白名单。
pub async fn restore_to_allowlist(
    store: &EduStore,
    secret_key: &SecretKey,
    section_id: i64,
    username: &str,
) -> anyhow::Result<SignedAllowlist> {
    store.set_member_authorized(section_id, username, true)?;
    let section = store
        .get_section(section_id)?
        .ok_or_else(|| anyhow::anyhow!("教学班 {section_id} 不存在"))?;
    let teacher_ep = secret_key.public().to_string();
    Ok(
        SignedAllowlist::build(store, section_id, section.topic_epoch, &teacher_ep)?
            .sign(secret_key),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (tempfile::TempDir, EduStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        (tmp, store)
    }

    fn dummy_key(seed: u8) -> SecretKey {
        let mut k = [0u8; 32];
        k[0] = seed;
        k[31] = seed.wrapping_mul(11).wrapping_add(3);
        SecretKey::from_bytes(&k)
    }

    fn make_member(
        role: MemberRole,
        username: &str,
        name: &str,
        ep: Option<&str>,
        authorized: bool,
    ) -> Member {
        Member {
            id: 0,
            section_id: 1,
            user_id: 0,
            role,
            username: username.to_string(),
            display_name: name.to_string(),
            admin_class: String::new(),
            endpoint_id: ep.map(|s| s.to_string()),
            authorized,
            joined_at: String::new(),
            last_seen_at: None,
        }
    }

    #[test]
    fn test_sign_and_verify_roundtrip() {
        let key = dummy_key(7);
        let teacher = key.public().to_string();
        let teacher_id = key.public();

        let members = vec![
            make_member(MemberRole::Teacher, "T001", "张老师", Some(&teacher), true),
            make_member(
                MemberRole::Student,
                "2024001",
                "张三",
                Some(&dummy_key(9).public().to_string()),
                true,
            ),
        ];
        let signed = SignedAllowlist::build_from_members(1, 3, &teacher, &members).sign(&key);

        assert!(signed.verify(), "合法签名应通过校验");
        assert_eq!(signed.epoch, 3);
        assert_eq!(signed.student_count(), 1);
        assert!(signed.contains(&teacher_id));
        assert_eq!(signed.entries.len(), 2);
    }

    #[test]
    fn test_tamper_detected() {
        let key = dummy_key(5);
        let teacher = key.public().to_string();
        let members = vec![make_member(MemberRole::Teacher, "T", "师", Some(&teacher), true)];
        let mut signed = SignedAllowlist::build_from_members(1, 1, &teacher, &members).sign(&key);
        assert!(signed.verify());

        // 篡改纪元 → 签名失效
        signed.epoch = 99;
        assert!(!signed.verify(), "篡改载荷后签名应失效");
    }

    #[test]
    fn test_wrong_signer_rejected() {
        let teacher_key = dummy_key(2);
        let attacker = dummy_key(3);
        let teacher = teacher_key.public().to_string();

        // 攻击者用自己私钥签，但把 teacher 字段伪造成真老师
        let members = vec![make_member(MemberRole::Teacher, "T", "师", Some(&teacher), true)];
        let forked = SignedAllowlist::build_from_members(1, 1, &teacher, &members);
        let forged = forked.sign(&attacker);
        assert!(!forged.verify(), "非签发者私钥签的表必须被拒");
    }

    #[test]
    fn test_build_excludes_unauthorized_and_unbound() {
        let key = dummy_key(4);
        let teacher = key.public().to_string();
        let s1 = dummy_key(10).public().to_string();
        let s2 = dummy_key(11).public().to_string();
        let s3 = dummy_key(12).public().to_string();

        let members = vec![
            make_member(MemberRole::Teacher, "T", "师", Some(&teacher), true),
            make_member(MemberRole::Student, "A", "甲", Some(&s1), true), // 收入
            make_member(MemberRole::Student, "B", "乙", Some(&s2), false), // 未授权 → 排除
            make_member(MemberRole::Student, "C", "丙", None, true),      // 未绑定 → 排除
            make_member(MemberRole::Student, "D", "丁", Some(&s3), true), // 收入
        ];
        let signed = SignedAllowlist::build_from_members(1, 0, &teacher, &members);
        assert_eq!(signed.student_count(), 2, "仅收入已授权且已绑定的学生");
    }

    #[tokio::test]
    async fn test_state_rejects_epoch_rollback() {
        let state = AllowlistState::new();
        assert!(state.accept(1, 5).await);
        assert!(state.accept(1, 5).await, "同纪元重复接受应幂等通过");
        assert!(state.accept(1, 6).await, "更高纪元应接受");
        assert!(!state.accept(1, 4).await, "更低纪元必须拒绝（防回放）");
        assert_eq!(state.current_epoch(1).await, Some(6));
        // 不同班互不影响
        assert!(state.accept(2, 1).await);
    }

    #[tokio::test]
    async fn test_apply_signed_allowlist_enforces_teacher_and_epoch() {
        let teacher_key = dummy_key(21);
        let teacher_id = teacher_key.public();
        let teacher_hex = teacher_id.to_string();
        let stu_a = dummy_key(31);
        let stu_b = dummy_key(32);

        let members = vec![
            make_member(MemberRole::Teacher, "T", "师", Some(&teacher_hex), true),
            make_member(MemberRole::Student, "A", "甲", Some(&stu_a.public().to_string()), true),
            make_member(MemberRole::Student, "B", "乙", Some(&stu_b.public().to_string()), true),
        ];
        let signed = SignedAllowlist::build_from_members(7, 2, &teacher_hex, &members).sign(&teacher_key);

        let registry = AuthRegistry::new();
        let state = AllowlistState::new();
        let n = apply_signed_allowlist(&signed, &teacher_id, &registry, &state)
            .await
            .unwrap();
        assert_eq!(n, 3);
        assert!(registry.is_authorized(&stu_a.public()).await);
        assert!(registry.is_authorized(&stu_b.public()).await);

        // 签发者不符 → 拒绝
        let other = dummy_key(99).public();
        let err = apply_signed_allowlist(&signed, &other, &registry, &state)
            .await
            .unwrap_err();
        assert!(err.contains("签发者不符"), "err = {err}");

        // 旧纪元回放 → 拒绝
        let stale = SignedAllowlist::build_from_members(7, 1, &teacher_hex, &members).sign(&teacher_key);
        let err = apply_signed_allowlist(&stale, &teacher_id, &registry, &state)
            .await
            .unwrap_err();
        assert!(err.contains("纪元倒退"), "err = {err}");
    }

    #[tokio::test]
    async fn test_apply_removes_revoked_member() {
        let teacher_key = dummy_key(41);
        let teacher_id = teacher_key.public();
        let teacher_hex = teacher_id.to_string();
        let stu = dummy_key(42);

        // 第一版：含学生
        let m1 = vec![
            make_member(MemberRole::Teacher, "T", "师", Some(&teacher_hex), true),
            make_member(MemberRole::Student, "A", "甲", Some(&stu.public().to_string()), true),
        ];
        let v1 = SignedAllowlist::build_from_members(9, 1, &teacher_hex, &m1).sign(&teacher_key);

        let registry = AuthRegistry::new();
        let state = AllowlistState::new();
        apply_signed_allowlist(&v1, &teacher_id, &registry, &state).await.unwrap();
        assert!(registry.is_authorized(&stu.public()).await);

        // 第二版：学生被撤销（epoch 递增）
        let m2 = vec![make_member(MemberRole::Teacher, "T", "师", Some(&teacher_hex), true)];
        let v2 = SignedAllowlist::build_from_members(9, 2, &teacher_hex, &m2).sign(&teacher_key);
        apply_signed_allowlist(&v2, &teacher_id, &registry, &state).await.unwrap();

        assert!(
            !registry.is_authorized(&stu.public()).await,
            "新表应剔除已撤销学生"
        );
    }

    #[test]
    fn test_postcard_roundtrip() {
        let key = dummy_key(8);
        let teacher = key.public().to_string();
        let members = vec![
            make_member(MemberRole::Teacher, "T", "师", Some(&teacher), true),
            make_member(MemberRole::Student, "A", "甲", Some(&dummy_key(50).public().to_string()), true),
        ];
        let signed = SignedAllowlist::build_from_members(1, 4, &teacher, &members).sign(&key);

        let bytes = postcard::to_allocvec(&signed).unwrap();
        let back: SignedAllowlist = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, signed);
        assert!(back.verify(), "postcard 往返后签名仍应有效");
    }

    /// 端到端撤销：建班 → 绑学生 → 撤销 → 轮换 → 新表不含该学生。
    #[tokio::test]
    async fn test_revoke_flow_store_side() {
        let (_tmp, store) = test_store();
        let teacher = store.create_teacher("张老师", "pw").unwrap();
        let course = store.create_course("CS201", "数据结构", teacher.id).unwrap();
        let section = store.create_class("信工班", course.id).unwrap();

        store
            .create_student("2024001", "张三", "123456", Some(section.id))
            .unwrap();
        let ep = dummy_key(60).public().to_string();
        store.bind_endpoint("2024001", &ep).unwrap();

        // 撤销前：新表含该学生
        let before =
            SignedAllowlist::build_from_members(section.id, 0, "teacher-ep", &store.list_section_members(section.id).unwrap());
        assert_eq!(before.student_count(), 1);

        let revoked = store.revoke_member(section.id, "2024001").unwrap();
        assert_eq!(revoked.as_deref(), Some(ep.as_str()));
        let new_epoch = store.rotate_topic_epoch(section.id).unwrap();
        assert_eq!(new_epoch, 1);

        // 撤销后：新表不含该学生
        let after = SignedAllowlist::build_from_members(
            section.id,
            new_epoch,
            "teacher-ep",
            &store.list_section_members(section.id).unwrap(),
        );
        assert_eq!(after.student_count(), 0, "撤销后学生应被剔除");
    }
}
