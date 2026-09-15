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

/// 一个已认证节点绑定的成员信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberBinding {
    pub username: String,
    pub display_name: String,
    /// 行政班级，如「信工2201」（同一教学班可含多个行政班）
    pub admin_class: String,
    /// 该节点被授权可参与的教学班 id 列表
    pub sections: Vec<i64>,
}

impl MemberBinding {
    pub fn new(
        username: impl Into<String>,
        display_name: impl Into<String>,
        admin_class: impl Into<String>,
        sections: Vec<i64>,
    ) -> Self {
        Self {
            username: username.into(),
            display_name: display_name.into(),
            admin_class: admin_class.into(),
            sections,
        }
    }

    /// 是否属于指定教学班（Handler 层细粒度校验用）
    pub fn belongs_to(&self, section_id: i64) -> bool {
        self.sections.contains(&section_id)
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

    #[tokio::test]
    async fn test_grant_and_check() {
        let reg = AuthRegistry::new();
        let id = dummy_id(1);
        assert!(!reg.is_authorized(&id).await);

        reg.grant(id, MemberBinding::new("2024001", "张三", "信工2201", vec![10, 11]))
            .await;
        assert!(reg.is_authorized(&id).await);
        assert_eq!(reg.len().await, 1);

        let b = reg.binding(&id).await.unwrap();
        assert_eq!(b.username, "2024001");
        assert!(b.belongs_to(10));
        assert!(!b.belongs_to(99));
    }

    #[tokio::test]
    async fn test_revoke_removes() {
        let reg = AuthRegistry::new();
        let id = dummy_id(2);
        reg.grant(id, MemberBinding::new("2024002", "李四", "电气2201", vec![20]))
            .await;
        assert!(reg.revoke(&id).await);
        assert!(!reg.is_authorized(&id).await);
        // 二次撤销返回 false
        assert!(!reg.revoke(&id).await);
    }

    #[tokio::test]
    async fn test_load_from_and_snapshot() {
        let reg = AuthRegistry::new();
        let a = dummy_id(3);
        let b = dummy_id(4);
        reg.load_from(vec![
            (a, MemberBinding::new("A", "甲", "信工2201", vec![1])),
            (b, MemberBinding::new("B", "乙", "信工2202", vec![1, 2])),
        ])
        .await;
        assert_eq!(reg.len().await, 2);
        let snap = reg.snapshot().await;
        assert_eq!(snap.len(), 2);
    }
}
