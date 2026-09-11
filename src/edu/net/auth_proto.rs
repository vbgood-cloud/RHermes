//! `/class-auth/1.0` 认证协议处理器（老师端）
//!
//! 对应 `docs/edu-p2p-design.md` §4.4。
//!
//! 流程：
//! 1. 学生用 `ALPN_AUTH` 连上老师（此时尚未进入白名单，靠 §4.2 的 open 规则放行）；
//! 2. 学生提交 `{username, password}`；
//! 3. 老师用既有 `auth::authenticate`（Argon2）校验；
//! 4. 成功后：
//!    a. 把 `conn.remote_id()`（不可伪造的 Ed25519 公钥）写入 `AuthRegistry` 白名单；
//!    b. 把该学生的 EndpointId 落库（设备绑定）；
//!    c. 下发其被授权的**全部教学班票据**（含 TopicId，只在加密信道里传）。
//!
//! 之后学生走 `/class-app/1.0` 与 gossip 时，会被 `WhitelistHook` 放行。

use std::path::PathBuf;

use iroh::endpoint::Connection;
use iroh::protocol::ProtocolHandler;
use serde::{Deserialize, Serialize};

use crate::edu::authz::{AuthRegistry, MemberBinding};
use crate::edu::model::SectionTicket;
use crate::edu::store::EduStore;

/// 认证请求（学生 → 老师）
#[derive(Debug, Deserialize)]
pub struct AuthRequest {
    pub username: String,
    pub password: String,
    /// 客户端自述的 EndpointId，仅用于日志；**权威身份取自 TLS 握手的 remote_id**
    #[serde(default)]
    pub claimed_endpoint: Option<String>,
}

/// 认证响应（老师 → 学生）
#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub ok: bool,
    pub message: String,
    /// 会话 token：用于 `/class-app/1.0` 内的细粒度操作鉴权
    pub token: Option<String>,
    /// 该学生被授权的全部教学班票据
    pub tickets: Vec<SectionTicket>,
}

impl AuthResponse {
    fn fail(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: msg.into(),
            token: None,
            tickets: Vec::new(),
        }
    }
}

/// `ProtocolHandler` 的 supertrait 要求 `Debug + Send + Sync + 'static`
#[derive(Debug)]
pub struct AuthHandler {
    db_path: PathBuf,
    registry: AuthRegistry,
    /// 本机（老师）EndpointId —— iroh 1.0 的 `Connection` 不暴露本地 id，故构造时注入
    local_id: String,
}

impl AuthHandler {
    pub fn new(db_path: PathBuf, registry: AuthRegistry, local_id: String) -> Self {
        Self {
            db_path,
            registry,
            local_id,
        }
    }

    /// 组装某学生的教学班票据（TopicId 在此下发，仅经加密信道）
    fn build_tickets(
        store: &EduStore,
        username: &str,
        teacher_endpoint: &str,
    ) -> Vec<SectionTicket> {
        let rows = store.sections_for_student(username).unwrap_or_default();
        rows.into_iter()
            .map(|r| {
                let topic = crate::edu::gossip::derive_topic(&r.gossip_seed, r.topic_epoch);
                SectionTicket {
                    section_id: r.section_id,
                    section_name: r.section_name,
                    course_code: r.course_code,
                    course_name: r.course_name,
                    topic_id: hex::encode(topic),
                    topic_epoch: r.topic_epoch,
                    bootstrap: vec![teacher_endpoint.to_string()],
                    provider: teacher_endpoint.to_string(),
                    blob_namespace: hex::encode(crate::edu::gossip::derive_topic(
                        &r.gossip_seed,
                        0,
                    )),
                }
            })
            .collect()
    }
}

impl ProtocolHandler for AuthHandler {
    async fn accept(&self, conn: Connection) -> Result<(), iroh::protocol::AcceptError> {
        // 身份以 TLS 握手的 remote_id 为准，客户端无法伪造
        let remote = conn.remote_id();
        let teacher_id = self.local_id.clone();

        // 所有错误都在内部消化：返回 Ok 即让 Router 正常关闭连接
        let result: anyhow::Result<()> = async {
            let (mut send, mut recv) = conn.accept_bi().await?;

            // 认证请求上限 16 KiB（用户名 + 密码，绰绰有余）
            let raw = recv.read_to_end(16 * 1024).await?;
            let req: AuthRequest = serde_json::from_slice(&raw)?;

            let store = EduStore::open(&self.db_path)?;

            let resp = match crate::edu::auth::authenticate(
                &store,
                &req.username,
                &req.password,
            ) {
                Ok(auth) => {
                    // ① 查该学生被授权的教学班
                    let sections = store
                        .sections_for_student(&req.username)
                        .unwrap_or_default();
                    let admin_class =
                        store.admin_class_of(&req.username).unwrap_or_default();

                    // ② 写白名单（内存热路径）
                    let binding = MemberBinding::new(
                        req.username.clone(),
                        auth.student_name.clone(),
                        admin_class,
                        sections.iter().map(|s| s.section_id).collect(),
                    );
                    self.registry.grant(remote, binding).await;

                    // ③ 落库：设备绑定（EndpointId）
                    if let Err(e) = store.bind_endpoint(&req.username, &remote.to_string()) {
                        tracing::warn!("绑定 EndpointId 落库失败: {e}");
                    }

                    // ④ 组装并下发教学班票据
                    let tickets = Self::build_tickets(&store, &req.username, &teacher_id);
                    tracing::info!(
                        "认证成功: {} ({}) endpoint={} 教学班 {} 个",
                        req.username,
                        auth.student_name,
                        remote,
                        tickets.len()
                    );

                    AuthResponse {
                        ok: true,
                        message: "认证成功".into(),
                        token: Some(auth.token),
                        tickets,
                    }
                }
                Err(e) => {
                    tracing::warn!("认证失败: {} from {remote}: {e}", req.username);
                    AuthResponse::fail(format!("学号或密码错误：{e}"))
                }
            };

            send.write_all(&serde_json::to_vec(&resp)?).await?;
            send.finish()?;
            // 给对端读完的机会再关闭
            conn.closed().await;
            Ok(())
        }
        .await;

        if let Err(e) = result {
            tracing::warn!("auth 协议处理异常: {e:#}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_request_parsing() {
        let raw = br#"{"username":"2024001","password":"pw"}"#;
        let req: AuthRequest = serde_json::from_slice(raw).unwrap();
        assert_eq!(req.username, "2024001");
        assert!(req.claimed_endpoint.is_none());
    }

    #[test]
    fn test_auth_response_fail_shape() {
        let r = AuthResponse::fail("bad");
        assert!(!r.ok);
        assert!(r.tickets.is_empty());
        assert!(r.token.is_none());
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":false"));
    }
}
