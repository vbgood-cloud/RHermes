//! iroh 网络层装配：双 ALPN + Router + 接入控制 Hook
//!
//! 对应 `docs/edu-p2p-design.md` §4.2 / §4.3。
//!
//! 取代旧的 `p2p.rs::TeacherP2P::listen_loop()`（手动 `endpoint.accept()` 循环）。
//! **理由**：gossip / blobs 需要把它们的 ALPN 注册进 `Router`，而 `Router` 会接管
//! `endpoint.accept()`，二者不能并存 —— 这是引入群组通信的硬前提。

pub mod app_proto;
pub mod auth_proto;
pub mod client;

use std::path::PathBuf;
use std::sync::Arc;

use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use iroh_blobs::{BlobsProtocol, store::mem::MemStore};
use iroh_gossip::Gossip;
use iroh::protocol::Router;
use iroh::address_lookup::memory::MemoryLookup;

use super::authz::{AuthRegistry, WhitelistHook};

/// 认证协议：允许未认证连接接入（白名单唯一入口）
pub const ALPN_AUTH: &[u8] = b"/class-auth/1.0";

/// 应用协议：仅白名单节点可接入（作业提交、答疑单点请求、票据刷新）
pub const ALPN_APP: &[u8] = b"/class-app/1.0";

/// 一个已装配好的 P2P 节点。
///
/// 老师端与学生端共用同一套装配，差异只在「注册了哪些 ProtocolHandler」。
pub struct P2pNode {
    pub endpoint: Endpoint,
    pub gossip: Gossip,
    pub blobs: Arc<MemStore>,
    pub registry: AuthRegistry,
    /// 离网模式下的手工地址簿（`presets::Minimal` 没有任何发现服务，
    /// gossip 仅凭 `EndpointId` 无法解析出对端 socket 地址 —— 必须显式注入）。
    /// 生产模式（N0）为 `None`，走中继 + DNS 发现。
    pub lookup: Option<MemoryLookup>,
    router: Router,
}

impl P2pNode {
    /// 装配 Endpoint。
    ///
    /// - `offline = false`（生产）：`presets::N0` —— 含 n0 公有 Relay + DNS 发现，跨网可达；
    /// - `offline = true`（测试 / 校内无外网）：`presets::Minimal` —— 无中继、无发现，
    ///   只能靠显式 `EndpointAddr` 直连。**端到端测试必须用这个**，否则会依赖外网。
    ///
    /// 返回 `(endpoint, lookup)`：`lookup` 在离网模式下非空，调用方须把对端
    /// `EndpointAddr` 注册进去（`add_peer_addr`），否则 gossip 找不到人。
    /// ⚠️ `secret_key` 是「每位老师 / 每位学生一套凭据」的落点：不注入时 iroh 会
    /// **每次随机生成**，`EndpointId` 随之改变 —— 老师的白名单、对端地址簿全部作废。
    /// 生产路径必须传入 [`crate::edu::identity`] 里持久化的那把钥匙。
    async fn build_endpoint(
        alpns: Vec<Vec<u8>>,
        hook: WhitelistHook,
        offline: bool,
        secret_key: Option<SecretKey>,
    ) -> anyhow::Result<(Endpoint, Option<MemoryLookup>)> {
        let lookup = if offline { Some(MemoryLookup::new()) } else { None };
        let builder = if offline {
            Endpoint::builder(iroh::endpoint::presets::Minimal)
        } else {
            Endpoint::builder(iroh::endpoint::presets::N0)
        };
        let mut builder = builder.alpns(alpns).hooks(hook);
        if let Some(key) = secret_key {
            builder = builder.secret_key(key);
        }
        if let Some(l) = &lookup {
            builder = builder.address_lookup(l.clone());
        }
        Ok((builder.bind().await?, lookup))
    }

    /// 老师端节点：注册 4 类协议（auth / app / gossip / blobs）。
    pub async fn teacher(db_path: PathBuf, registry: AuthRegistry) -> anyhow::Result<Self> {
        Self::teacher_with(db_path, registry, false).await
    }

    /// 老师端节点（离网模式，供测试使用）
    pub async fn teacher_offline(
        db_path: PathBuf,
        registry: AuthRegistry,
    ) -> anyhow::Result<Self> {
        Self::teacher_with(db_path, registry, true).await
    }

    /// 老师端节点（**带持久化身份**，生产多老师场景的唯一入口）。
    ///
    /// `secret_key = None` 时行为与 [`P2pNode::teacher`] 一致（每次随机身份）。
    pub async fn teacher_with_key(
        db_path: PathBuf,
        registry: AuthRegistry,
        offline: bool,
        secret_key: Option<SecretKey>,
    ) -> anyhow::Result<Self> {
        Self::teacher_with_key_inner(db_path, registry, offline, secret_key).await
    }

    /// 学生端节点（**带持久化身份**）。
    pub async fn student_with_key(
        registry: AuthRegistry,
        offline: bool,
        secret_key: Option<SecretKey>,
    ) -> anyhow::Result<Self> {
        Self::student_with_key_inner(registry, offline, secret_key).await
    }

    async fn teacher_with(
        db_path: PathBuf,
        registry: AuthRegistry,
        offline: bool,
    ) -> anyhow::Result<Self> {
        Self::teacher_with_key_inner(db_path, registry, offline, None).await
    }

    async fn teacher_with_key_inner(
        db_path: PathBuf,
        registry: AuthRegistry,
        offline: bool,
        secret_key: Option<SecretKey>,
    ) -> anyhow::Result<Self> {
        let (endpoint, lookup) = Self::build_endpoint(
            vec![
                ALPN_AUTH.to_vec(),
                ALPN_APP.to_vec(),
                iroh_gossip::ALPN.to_vec(),
                iroh_blobs::ALPN.to_vec(),
            ],
            WhitelistHook::teacher(registry.clone()),
            offline,
            secret_key,
        )
        .await?;

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let blobs = Arc::new(MemStore::new());
        let local_id = endpoint.id().to_string();
        let secret_key = endpoint.secret_key().clone();

        let router = Router::builder(endpoint.clone())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&*blobs, None))
            .accept(
                ALPN_AUTH,
                auth_proto::AuthHandler::new(
                    db_path.clone(),
                    registry.clone(),
                    local_id.clone(),
                ),
            )
            .accept(
                ALPN_APP,
                app_proto::AppHandler::new(
                    db_path,
                    registry.clone(),
                    local_id,
                    secret_key,
                ),
            )
            .spawn();

        Ok(Self {
            endpoint,
            gossip,
            blobs,
            registry,
            lookup,
            router,
        })
    }

    /// 学生端节点：注册 gossip / blobs（用于接收群组消息与拉文件）+ 认证出站。
    ///
    /// 学生端同样安装 `WhitelistHook`（§5.4 R2 分布式白名单），
    /// 以拦截「已被撤销的同伴」通过学生间接入 swarm。
    pub async fn student(registry: AuthRegistry) -> anyhow::Result<Self> {
        Self::student_with(registry, false).await
    }

    /// 学生端节点（离网模式，供测试使用）
    pub async fn student_offline(registry: AuthRegistry) -> anyhow::Result<Self> {
        Self::student_with(registry, true).await
    }

    async fn student_with(registry: AuthRegistry, offline: bool) -> anyhow::Result<Self> {
        Self::student_with_key_inner(registry, offline, None).await
    }

    async fn student_with_key_inner(
        registry: AuthRegistry,
        offline: bool,
        secret_key: Option<SecretKey>,
    ) -> anyhow::Result<Self> {
        let (endpoint, lookup) = Self::build_endpoint(
            vec![
                ALPN_AUTH.to_vec(),
                iroh_gossip::ALPN.to_vec(),
                iroh_blobs::ALPN.to_vec(),
            ],
            WhitelistHook::student(registry.clone()),
            offline,
            secret_key,
        )
        .await?;

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let blobs = Arc::new(MemStore::new());

        let router = Router::builder(endpoint.clone())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&*blobs, None))
            .spawn();

        Ok(Self {
            endpoint,
            gossip,
            blobs,
            registry,
            lookup,
            router,
        })
    }

    /// 离网模式下把对端 `EndpointAddr` 注册进本地地址簿。
    ///
    /// 生产模式（N0）是空操作 —— 中继 + DNS 发现会自动补全地址。
    pub fn add_peer_addr(&self, addr: EndpointAddr) {
        if let Some(l) = &self.lookup {
            l.add_endpoint_info(addr);
        }
    }

    /// 本节点 EndpointId（= 身份公钥，用作课程码 / 引导地址）
    pub fn node_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// 本节点完整地址（id + 当前直连/中继地址），用于打印「老师地址」给学生填配置。
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// 本节点身份私钥（签发白名单 / 派生 Topic 用）
    pub fn secret_key(&self) -> &SecretKey {
        self.endpoint.secret_key()
    }

    /// 课程码：EndpointId 前 12 位大写（沿用既有 `p2p::encode_course_code` 的语义）
    pub fn course_code(&self) -> String {
        super::p2p::encode_course_code(&self.endpoint.id().to_string())
    }

    /// 等待本机对外可达（Relay / 直连地址就绪）
    pub async fn online(&self) {
        let _ = self.endpoint.online().await;
    }

    /// 优雅关闭：Router 会通知同伴本节点离线
    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.router.shutdown().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alpn_values() {
        assert_eq!(ALPN_AUTH, b"/class-auth/1.0");
        assert_eq!(ALPN_APP, b"/class-app/1.0");
        // 与既有旧协议区分开：旧的是 rhermes-edu/1
        assert_ne!(ALPN_AUTH, super::super::p2p::EDU_ALPN);
    }
}
