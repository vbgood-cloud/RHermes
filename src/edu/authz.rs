//! 授权与接入控制（EndpointId 白名单）
//!
//! 设计要点（对应 `docs/edu-p2p-design.md` §4 / §10.2）：
//!
//! - **身份根**：iroh 的 TLS 握手使用节点自身密钥，`Connection::remote_id()` 拿到的
//!   `EndpointId` 是密码学可信的 Ed25519 公钥，客户端无法伪造。
//! - **双层拦截**：
//!   1. `WhitelistHook`（Hook 层，粗筛）——在握手完成时按 ALPN 决定放行 / 拒绝；
//!   2. `net::app_proto`（Handler 层，细筛）——校验「已认证用户是否属于该教学班」。
//! - 本模块只负责第 1 层与白名单的存储。
//!
//! ⚠️ 两个 API 约束（iroh 1.0.2）：
//! - `EndpointHooks` **不是 dyn 兼容 trait**，因此必须用具体类型安装；
//! - 官方明确要求 Hook 内**不得持有 `Endpoint`**（会形成引用计数环导致 Endpoint 永不释放），
//!   所以白名单以共享状态（`Arc<RwLock<..>>`）注入，而不是把 Endpoint 塞进 Hook。

use std::collections::HashMap;
use std::sync::Arc;

use iroh::endpoint::{AfterHandshakeOutcome, Connection, EndpointHooks};
use iroh::EndpointId;
use tokio::sync::RwLock;

use super::model::SectionKey;

/// 一个已认证节点绑定的成员信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberBinding {
    pub username: String,
    pub display_name: String,
    /// 行政班级，如「信工2201」（同一教学班可含多个行政班）
    pub admin_class: String,
    /// 该节点被授权可参与的教学班列表。
    ///
    /// ⚠️ 元素是 [`SectionKey`] 而不是裸 `section_id` —— 每位老师各自一份 edu.db，
    /// 教学班主键都从 1 开始；学生同时选修多位老师时不带老师身份会互相覆盖。
    pub sections: Vec<SectionKey>,
}

impl MemberBinding {
    pub fn new(
        username: impl Into<String>,
        display_name: impl Into<String>,
        admin_class: impl Into<String>,
        sections: Vec<SectionKey>,
    ) -> Self {
        Self {
            username: username.into(),
            display_name: display_name.into(),
            admin_class: admin_class.into(),
            sections,
        }
    }

    /// 是否属于指定教学班（Handler 层细粒度校验用）
    pub fn belongs_to(&self, key: &SectionKey) -> bool {
        self.sections.contains(key)
    }
}

/// 全局授权表：`EndpointId` → 成员绑定。
///
/// 内存热路径 + 由调用方负责落库（`EduStore::bind_endpoint`），
/// 进程重启后由 `AuthRegistry::load_from` 预热。
#[derive(Debug, Clone, Default)]
pub struct AuthRegistry {
    inner: Arc<RwLock<HashMap<EndpointId, MemberBinding>>>,
}

impl AuthRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 授权（认证成功后调用）
    pub async fn grant(&self, id: EndpointId, binding: MemberBinding) {
        self.inner.write().await.insert(id, binding);
    }

    /// 撤销授权。
    ///
    /// ⚠️ 仅移除白名单**不足以**阻止其继续参与 gossip（网状传播，且其他学生未必装有 Hook）。
    /// 必须配合教学班 Topic 轮换 —— 见 `gossip::rotate_topic` 与设计文档 §5.4。
    /// 返回 `true` 表示确实移除了一个条目。
    pub async fn revoke(&self, id: &EndpointId) -> bool {
        self.inner.write().await.remove(id).is_some()
    }

    /// 从**某一个教学班**撤销该节点的归属，保留其在其他教学班（含其他老师）的授权。
    ///
    /// ⚠️ 撤销的正确粒度是「教学班」而不是「节点」。一个学生可以同时选同一老师的
    /// 多门课、或不同老师的课；从 A 班撤销不应把他从 B 班一并踢出，否则：
    /// - 白名单整表刷新时（`apply_signed_allowlist` 会先清本班旧成员）会把该生在
    ///   其他班的授权一起抹掉 → 本地 Hook 拒绝那些班的同伴 → 那些班 gossip 断开；
    /// - 老师撤销 A 班成员时，会连带吊销他在同一老师 B 班的权限。
    ///
    /// 仅当该节点不再属于任何教学班时才整条移除（此时才真正「离开所有班」）。
    ///
    /// 返回 `true` 表示本地条目确有变化。
    pub async fn revoke_from_section(&self, id: &EndpointId, key: &SectionKey) -> bool {
        let mut w = self.inner.write().await;
        let changed = match w.get_mut(id) {
            Some(b) => {
                let before = b.sections.len();
                b.sections.retain(|s| s != key);
                b.sections.len() != before
            }
            // 本就不在白名单 → 无事发生
            None => return false,
        };
        if !changed {
            return false;
        }
        if w.get(id).map(|b| b.sections.is_empty()).unwrap_or(false) {
            w.remove(id);
        }
        true
    }

    pub async fn is_authorized(&self, id: &EndpointId) -> bool {
        self.inner.read().await.contains_key(id)
    }

    /// 取绑定信息（Handler 层细粒度校验用）
    pub async fn binding(&self, id: &EndpointId) -> Option<MemberBinding> {
        self.inner.read().await.get(id).cloned()
    }

    /// 当前已授权节点数
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    /// 批量预热（进程启动时从 DB 恢复）
    pub async fn load_from(&self, entries: impl IntoIterator<Item = (EndpointId, MemberBinding)>) {
        let mut w = self.inner.write().await;
        for (id, b) in entries {
            w.insert(id, b);
        }
    }

    /// 快照（用于签名成员表广播，见 §5.4 R2）
    pub async fn snapshot(&self) -> Vec<(EndpointId, MemberBinding)> {
        self.inner
            .read()
            .await
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }
}

/// Endpoint Hook：按 ALPN 决定是否要求白名单。
///
/// - `open_alpns`：直接放行的 ALPN（典型：`/class-auth/1.0`，否则无人能完成首次认证）
/// - `gated_alpns`：必须命中白名单才放行的 ALPN（典型：`/class-app/1.0` + gossip + blobs）
/// - 未出现在两个列表中的 ALPN：放行，交由 `Router` 的分发器处理
#[derive(Debug, Clone)]
pub struct WhitelistHook {
    registry: AuthRegistry,
    open_alpns: Vec<Vec<u8>>,
    gated_alpns: Vec<Vec<u8>>,
}

impl WhitelistHook {
    pub fn new(
        registry: AuthRegistry,
        open_alpns: Vec<Vec<u8>>,
        gated_alpns: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            registry,
            open_alpns,
            gated_alpns,
        }
    }

    /// 老师端默认配置：认证协议开放，应用层 + gossip + blobs 全部受控。
    pub fn teacher(registry: AuthRegistry) -> Self {
        Self::new(
            registry,
            vec![super::net::ALPN_AUTH.to_vec()],
            vec![
                super::net::ALPN_APP.to_vec(),
                iroh_gossip::ALPN.to_vec(),
                iroh_blobs::ALPN.to_vec(),
            ],
        )
    }

    /// 学生端默认配置（§5.4 R2 分布式白名单）：
    /// 认证协议开放（学生要能主动连老师），其余全部要求白名单。
    pub fn student(registry: AuthRegistry) -> Self {
        Self::new(
            registry,
            vec![super::net::ALPN_AUTH.to_vec()],
            vec![
                super::net::ALPN_APP.to_vec(),
                iroh_gossip::ALPN.to_vec(),
                iroh_blobs::ALPN.to_vec(),
            ],
        )
    }
}

impl EndpointHooks for WhitelistHook {
    async fn after_handshake(&self, conn: &Connection) -> AfterHandshakeOutcome {
        // ⚠️⚠️ iroh 1.0.2 的 Hook 对**连接两端都会调用**（`endpoint/connection.rs`
        //     里 accept 与 connect 共用同一个 `after_handshake` 调用点，日志里能看到
        //     `side = ?conn.side()`）。若不区分方向，本机主动发起的出站连接会被
        //     **自己** 按白名单拒掉 —— 实测表现：
        //       · 学生发 `/class-app` 拉票据 → `Connection was rejected locally`
        //       · 老师侧看到 `closed by peer: unauthorized: endpoint not in allowlist`
        //       · 学生发 gossip 回拨 → `dial failed: Connection was rejected locally`
        //     白名单的语义是「**谁能进得来**」，因此只在 `Server`（被连接方）强制；
        //     `Client`（本机主动发起）一律放行 —— 出站是我们自己的选择，且对端仍会拦。
        if conn.side().is_client() {
            return AfterHandshakeOutcome::Accept;
        }

        // iroh 1.0.2：`Connection<HandshakeCompleted>::alpn()` 返回 `&[u8]`
        let alpn: &[u8] = conn.alpn();

        // ① 开放协议：直接放行（唯一入口，保证首次认证可行）
        if self.open_alpns.iter().any(|a| a.as_slice() == alpn) {
            return AfterHandshakeOutcome::Accept;
        }

        // ② 受控协议：必须命中白名单
        if self.gated_alpns.iter().any(|a| a.as_slice() == alpn) {
            let remote = conn.remote_id();
            if self.registry.is_authorized(&remote).await {
                return AfterHandshakeOutcome::Accept;
            }
            tracing::warn!(
                "拒绝未授权节点 {remote} 接入协议 {}",
                String::from_utf8_lossy(alpn)
            );
            return AfterHandshakeOutcome::Reject {
                error_code: iroh::endpoint::VarInt::from_u32(403),
                reason: b"unauthorized: endpoint not in allowlist".to_vec(),
            };
        }

        // ③ 其余：放行
        AfterHandshakeOutcome::Accept
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_id(seed: u8) -> EndpointId {
        // 用确定性字节构造一个合法 EndpointId（Ed25519 公钥）
        let mut key = [0u8; 32];
        key[0] = seed;
        key[31] = seed.wrapping_mul(7);
        // 全零/非法点会被拒绝，这里用 SecretKey 反推公钥更稳
        let sk = iroh::SecretKey::from_bytes(&key);
        sk.public()
    }

    /// 测试用班键：默认归同一个老师（seed=200）
    fn sk(section_id: i64) -> SectionKey {
        SectionKey::new(dummy_id(200), section_id)
    }

    #[tokio::test]
    async fn test_grant_and_check() {
        let reg = AuthRegistry::new();
        let id = dummy_id(1);
        assert!(!reg.is_authorized(&id).await);

        reg.grant(id, MemberBinding::new("2024001", "张三", "信工2201", vec![sk(10), sk(11)]))
            .await;
        assert!(reg.is_authorized(&id).await);
        assert_eq!(reg.len().await, 1);

        let b = reg.binding(&id).await.unwrap();
        assert_eq!(b.username, "2024001");
        assert!(b.belongs_to(&sk(10)));
        assert!(!b.belongs_to(&sk(99)));
        // 同班 id、不同老师 ⇒ 必须是不同的班（班 id 只在各自库内唯一）
        assert!(!b.belongs_to(&SectionKey::new(dummy_id(201), 10)));
    }

    #[tokio::test]
    async fn test_revoke_removes() {
        let reg = AuthRegistry::new();
        let id = dummy_id(2);
        reg.grant(id, MemberBinding::new("2024002", "李四", "电气2201", vec![sk(20)]))
            .await;
        assert!(reg.revoke(&id).await);
        assert!(!reg.is_authorized(&id).await);
        // 二次撤销返回 false
        assert!(!reg.revoke(&id).await);
    }

    /// 缺陷 F/G 回归：撤销必须按**教学班**粒度，不能整条移除节点。
    ///
    /// 一个学生可以同时选同一老师的多门课、或不同老师的课。从 A 班撤销（或白名单
    /// 整表刷新时清旧成员）不得把他从 B 班一并踢出，否则 B 班的握手会被本地 Hook
    /// 拒绝、gossip 连接反复重建。
    #[tokio::test]
    async fn test_revoke_from_section_keeps_other_sections() {
        let reg = AuthRegistry::new();
        let id = dummy_id(5);
        // 该生同时在 10、20 两个班
        reg.grant(id, MemberBinding::new("2024005", "王五", "信工2201", vec![sk(10), sk(20)]))
            .await;

        // 撤销「本就不属于」的班 → 无变化
        assert!(!reg.revoke_from_section(&id, &sk(99)).await);
        assert_eq!(reg.binding(&id).await.unwrap().sections.len(), 2);
        // 同班 id、不同老师 → 不应命中
        assert!(!reg.revoke_from_section(&id, &SectionKey::new(dummy_id(201), 10)).await);
        assert_eq!(reg.binding(&id).await.unwrap().sections.len(), 2);

        // 撤 10 班 → 20 班保留，节点仍授权在册
        assert!(reg.revoke_from_section(&id, &sk(10)).await);
        let b = reg.binding(&id).await.unwrap();
        assert_eq!(b.sections, vec![sk(20)], "其他班归属必须保留");
        assert!(reg.is_authorized(&id).await, "仍属于其他班，节点不得被整体移除");

        // 重复撤销同一班 → false（幂等）
        assert!(!reg.revoke_from_section(&id, &sk(10)).await);

        // 再撤 20 → 已不属于任何班 → 整条移除
        assert!(reg.revoke_from_section(&id, &sk(20)).await);
        assert!(!reg.is_authorized(&id).await);

        // 节点已不在白名单 → false
        assert!(!reg.revoke_from_section(&id, &sk(20)).await);
    }

    #[tokio::test]
    async fn test_load_from_and_snapshot() {
        let reg = AuthRegistry::new();
        let a = dummy_id(3);
        let b = dummy_id(4);
        reg.load_from(vec![
            (a, MemberBinding::new("A", "甲", "信工2201", vec![sk(1)])),
            (b, MemberBinding::new("B", "乙", "信工2202", vec![sk(1), sk(2)])),
        ])
        .await;
        assert_eq!(reg.len().await, 2);
        let snap = reg.snapshot().await;
        assert_eq!(snap.len(), 2);
    }
}
