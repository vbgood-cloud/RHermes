//! iroh 网络层装配：双 ALPN + Router + 接入控制 Hook
//!
//! 对应 `docs/edu-p2p-design.md` §4.2 / §4.3。
//!
//! 取代旧的 `p2p.rs::TeacherP2P::listen_loop()`（手动 `endpoint.accept()` 循环）。
//! **理由**：gossip / blobs 需要把它们的 ALPN 注册进 `Router`，而 `Router` 会接管
//! `endpoint.accept()`，二者不能并存 —— 这是引入群组通信的硬前提。

pub mod app_proto;
pub mod auth_proto;

use std::path::PathBuf;
use std::sync::Arc;

use iroh::{Endpoint, EndpointId};
use iroh_blobs::{BlobsProtocol, store::mem::MemStore};
use iroh_gossip::Gossip;
use iroh::protocol::Router;

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
    router: Router,
}

impl P2pNode {
    /// 老师端节点：注册 4 类协议（auth / app / gossip / blobs）。
    pub async fn teacher(db_path: PathBuf, registry: AuthRegistry) -> anyhow::Result<Self> {
        let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
            .alpns(vec![
                ALPN_AUTH.to_vec(),
                ALPN_APP.to_vec(),
                iroh_gossip::ALPN.to_vec(),
                iroh_blobs::ALPN.to_vec(),
            ])
            .hooks(WhitelistHook::teacher(registry.clone()))
            .bind()
            .await?;

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let blobs = Arc::new(MemStore::new());
        let local_id = endpoint.id().to_string();

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
                app_proto::AppHandler::new(db_path, registry.clone(), local_id),
            )
            .spawn();

        Ok(Self {
            endpoint,
            gossip,
            blobs,
            registry,
            router,
        })
    }

    /// 学生端节点：注册 gossip / blobs（用于接收群组消息与拉文件）+ 认证出站。
    ///
    /// 学生端同样安装 `WhitelistHook`（§5.4 R2 分布式白名单），
    /// 以拦截「已被撤销的同伴」通过学生间接入 swarm。
    pub async fn student(registry: AuthRegistry) -> anyhow::Result<Self> {
        let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
            .alpns(vec![
                ALPN_AUTH.to_vec(),
                iroh_gossip::ALPN.to_vec(),
                iroh_blobs::ALPN.to_vec(),
            ])
            .hooks(WhitelistHook::student(registry.clone()))
            .bind()
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
            router,
        })
    }

    /// 本节点 EndpointId（= 身份公钥，用作课程码 / 引导地址）
    pub fn node_id(&self) -> EndpointId {
        self.endpoint.id()
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
