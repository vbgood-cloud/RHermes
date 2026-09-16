//! `/class-app/1.0` 应用协议处理器（老师端）
//!
//! 对应 `docs/edu-p2p-design.md` §10.2「2C 双层拦截」的**第二层**。
//!
//! - Hook 层（`WhitelistHook`）只回答「这个 EndpointId 认证过吗」；
//! - Handler 层回答「这个**已认证**用户，是否有权操作**这个教学班**」。
//!
//! 防的是：张三认证过了（在白名单里），但他没选「数据结构-电气班」，
//! 却拿自己的 token 去提交电气班的作业 / 冒名提问。
//!
//! 校验链：`conn.remote_id()` → `AuthRegistry` 绑定 → `binding.belongs_to(section_id)`。

use std::path::PathBuf;

use iroh::endpoint::Connection;
use iroh::protocol::ProtocolHandler;
use serde::{Deserialize, Serialize};

use crate::edu::authz::AuthRegistry;
use crate::edu::model::SectionKey;
use crate::edu::store::EduStore;

/// 应用层请求（学生 → 老师）
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum AppRequest {
    /// 我是谁（用于客户端确认身份与授权班级）
    WhoAmI,
    /// 刷新教学班票据（例如 Topic 轮换后重新取 TopicId）
    RefreshTickets,
    /// 拉取本班当前**签名成员表**。
    ///
    /// 学生入班那一刻就调用它：gossip 是"尽力而为"的广播，若老师的
    /// `AllowlistUpdate` 恰好早于学生订阅（或中途丢包），学生本地白名单会长期为空，
    /// 导致它把老师/同学的回拨一律拒掉。走认证信道**主动拉取**才能保证即时一致。
    CurrentAllowlist { section_id: i64 },
    /// 提问（单点投递给老师，与 gossip 广播互补）
    AskQuestion { section_id: i64, question: String },
    /// 提交作业
    SubmitAssignment {
        section_id: i64,
        assignment_id: i64,
        content: String,
    },
}

/// 应用层响应（老师 → 学生）
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AppResponse {
    Ok {
        message: String,
    },
    Identity {
        username: String,
        display_name: String,
        admin_class: String,
        sections: Vec<i64>,
    },
    Tickets {
        tickets: Vec<crate::edu::model::SectionTicket>,
    },
    /// postcard 序列化的 `allowlist::SignedAllowlist`（学生端校验签名后应用）
    Allowlist {
        section_id: i64,
        epoch: u32,
        signed: Vec<u8>,
    },
    Denied {
        message: String,
    },
    Error {
        message: String,
    },
}

/// `ProtocolHandler` 的 supertrait 要求 `Debug + Send + Sync + 'static`
#[derive(Debug)]
pub struct AppHandler {
    db_path: PathBuf,
    registry: AuthRegistry,
    /// 本机（老师）EndpointId —— iroh 1.0 的 `Connection` 不暴露本地 id，故构造时注入
    local_id: String,
    /// 老师节点私钥：用于对成员表签名（`CurrentAllowlist` 下发时可被学生独立验签）
    secret_key: iroh::SecretKey,
}

impl AppHandler {
    pub fn new(
        db_path: PathBuf,
        registry: AuthRegistry,
        local_id: String,
        secret_key: iroh::SecretKey,
    ) -> Self {
        Self {
            db_path,
            registry,
            local_id,
            secret_key,
        }
    }

    /// 生成并签名本班当前成员表，返回 `(epoch, postcard 字节)`
    fn signed_allowlist(&self, section_id: i64) -> anyhow::Result<(u32, Vec<u8>)> {
        let store = EduStore::open(&self.db_path)?;
        let sec = store
            .get_section(section_id)?
            .ok_or_else(|| anyhow::anyhow!("教学班 {section_id} 不存在"))?;
        let signed = crate::edu::allowlist::SignedAllowlist::build(
            &store,
            section_id,
            sec.topic_epoch,
            &self.local_id,
        )?
        .sign(&self.secret_key);
        Ok((sec.topic_epoch, postcard::to_allocvec(&signed)?))
    }
}

impl ProtocolHandler for AppHandler {
    async fn accept(&self, conn: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let remote = conn.remote_id();

        let result: anyhow::Result<()> = async {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let raw = recv.read_to_end(1024 * 1024).await?;
            let req: AppRequest = serde_json::from_slice(&raw)?;

            // ── 第二层校验：EndpointId 必须已认证 ──
            let Some(binding) = self.registry.binding(&remote).await else {
                // 理论上 Hook 已拦截；这里兜底（例如 Hook 被旁路或连接复用于其他 ALPN）
                tracing::warn!("app 层拒绝：{remote} 不在白名单");
                let resp = AppResponse::Denied {
                    message: "未认证节点".into(),
                };
                send.write_all(&serde_json::to_vec(&resp)?).await?;
                send.finish()?;
                conn.closed().await;
                return Ok(());
            };

            let resp = match req {
                AppRequest::WhoAmI => AppResponse::Identity {
                    username: binding.username.clone(),
                    display_name: binding.display_name.clone(),
                    admin_class: binding.admin_class.clone(),
                    // 只回报**本老师**名下的教学班：班 id 在各自的库里会重复，
                    // 不按老师过滤会误导学生
                    sections: binding
                        .sections
                        .iter()
                        .filter(|k| k.teacher == self.secret_key.public())
                        .map(|k| k.section_id)
                        .collect(),
                },

                AppRequest::RefreshTickets => {
                    let store = EduStore::open(&self.db_path)?;
                    let tickets = self.tickets_for(&store, &binding.username)?;
                    AppResponse::Tickets { tickets }
                }

                AppRequest::CurrentAllowlist { section_id } => {
                    if !binding.belongs_to(&SectionKey::new(self.secret_key.public(), section_id)) {
                        tracing::warn!(
                            "越权拦截：{} 不属于教学班 {}",
                            binding.username,
                            section_id
                        );
                        AppResponse::Denied {
                            message: format!("你不属于教学班 {section_id}"),
                        }
                    } else {
                        match self.signed_allowlist(section_id) {
                            Ok((epoch, signed)) => AppResponse::Allowlist {
                                section_id,
                                epoch,
                                signed,
                            },
                            Err(e) => AppResponse::Error {
                                message: format!("生成成员表失败: {e}"),
                            },
                        }
                    }
                }

                AppRequest::AskQuestion { section_id, question } => {
                    if !binding.belongs_to(&SectionKey::new(self.secret_key.public(), section_id)) {
                        tracing::warn!(
                            "越权拦截：{} 不属于教学班 {}",
                            binding.username,
                            section_id
                        );
                        AppResponse::Denied {
                            message: format!("你不属于教学班 {section_id}"),
                        }
                    } else {
                        tracing::info!(
                            "[section {section_id}] 提问 {} ({}): {}",
                            binding.username,
                            binding.display_name,
                            question
                        );
                        AppResponse::Ok {
                            message: "提问已收到".into(),
                        }
                    }
                }

                AppRequest::SubmitAssignment {
                    section_id,
                    assignment_id,
                    content,
                } => {
                    if !binding.belongs_to(&SectionKey::new(self.secret_key.public(), section_id)) {
                        tracing::warn!(
                            "越权拦截：{} 不属于教学班 {}",
                            binding.username,
                            section_id
                        );
                        AppResponse::Denied {
                            message: format!("你不属于教学班 {section_id}"),
                        }
                    } else {
                        let store = EduStore::open(&self.db_path)?;
                        let student = store.get_student(&binding.username).ok().flatten();
                        match student {
                            Some(st) => {
                                match store
                                    .submit_assignment(assignment_id, st.id, &content, "")
                                {
                                    Ok(_) => AppResponse::Ok {
                                        message: "作业已提交".into(),
                                    },
                                    Err(e) => AppResponse::Error {
                                        message: format!("提交失败: {e}"),
                                    },
                                }
                            }
                            None => AppResponse::Error {
                                message: "学生不存在".into(),
                            },
                        }
                    }
                }
            };

            send.write_all(&serde_json::to_vec(&resp)?).await?;
            send.finish()?;
            conn.closed().await;
            Ok(())
        }
        .await;

        if let Err(e) = result {
            tracing::warn!("app 协议处理异常: {e:#}");
        }
        Ok(())
    }
}

impl AppHandler {
    /// 组装票据（Topic 轮换后客户端可用 `RefreshTickets` 重新取）
    fn tickets_for(
        &self,
        store: &EduStore,
        username: &str,
    ) -> anyhow::Result<Vec<crate::edu::model::SectionTicket>> {
        let teacher_id = self.local_id.clone();
        let rows = store.sections_for_student(username).unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|r| {
                let topic = crate::edu::gossip::derive_topic(&r.gossip_seed, r.topic_epoch);
                crate::edu::model::SectionTicket {
                    section_id: r.section_id,
                    section_name: r.section_name,
                    course_code: r.course_code,
                    course_name: r.course_name,
                    topic_id: hex::encode(topic),
                    topic_epoch: r.topic_epoch,
                    bootstrap: vec![teacher_id.clone()],
                    provider: teacher_id.clone(),
                    blob_namespace: hex::encode(crate::edu::gossip::derive_topic(
                        &r.gossip_seed,
                        0,
                    )),
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_request_parsing() {
        // 注意：字节串字面量 `br#"..."#` 不允许非 ASCII，中文要用普通字符串 + as_bytes()
        let raw = r#"{"op":"AskQuestion","section_id":7,"question":"为什么"}"#;
        let req: AppRequest = serde_json::from_slice(raw.as_bytes()).unwrap();
        match req {
            AppRequest::AskQuestion { section_id, question } => {
                assert_eq!(section_id, 7);
                assert_eq!(question, "为什么");
            }
            _ => panic!("应为 AskQuestion"),
        }
    }

    #[test]
    fn test_app_response_serde() {
        let r = AppResponse::Denied {
            message: "nope".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"kind\":\"Denied\""));
    }
}
