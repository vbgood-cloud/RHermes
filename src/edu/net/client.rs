//! `/class-auth/1.0` 与 `/class-app/1.0` 的**客户端**实现（学生侧）
//!
//! 服务端 Handler 在 `auth_proto.rs` / `app_proto.rs`，本模块补上另一半 ——
//! 没有它，双 ALPN 方案只有"能接客的门"，没有"能敲门的客户端"。
//!
//! ## 线上格式（与服务端严格对应）
//!
//! 单条双向流，一问一答：
//!
//! ```text
//! 客户端 open_bi() → 写 JSON → finish()
//! 服务端读到 EOF   → 回写 JSON → finish() → conn.closed().await（等客户端关闭）
//! 客户端读到 EOF   → conn.close(0, "done")   ← 必须显式关，否则服务端一直挂着
//! ```
//!
//! ⚠️ 顺序不能颠倒：服务端把「读到 EOF」当作请求结束的标志，所以客户端**必须**
//! `finish()` 之后才可能拿到响应；而服务端在 `closed()` 上等客户端，所以客户端
//! 读完响应要主动 `close()`。

use std::time::Duration;

use iroh::{Endpoint, EndpointAddr};
use iroh::endpoint::VarInt;

use super::app_proto::{AppRequest, AppResponse};
use super::auth_proto::{AuthRequest, AuthResponse};
use super::{ALPN_APP, ALPN_AUTH};

/// 响应读上限（票据含多个教学班时也不算大，256 KiB 绰绰有余）
const MAX_RESPONSE: usize = 256 * 1024;

/// 认证要跑 Argon2，给足时间
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
/// 应用层普通请求
const APP_TIMEOUT: Duration = Duration::from_secs(15);

/// 一次一问一答的通用实现。
async fn round_trip<Req, Resp>(
    endpoint: &Endpoint,
    teacher: EndpointAddr,
    alpn: &[u8],
    req: &Req,
    timeout: Duration,
) -> anyhow::Result<Resp>
where
    Req: serde::Serialize,
    Resp: serde::de::DeserializeOwned,
{
    let fut = async {
        let conn = endpoint.connect(teacher, alpn).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(&serde_json::to_vec(req)?).await?;
        send.finish()?;
        let raw = recv.read_to_end(MAX_RESPONSE).await?;
        let resp: Resp = serde_json::from_slice(&raw)?;
        // 服务端在 `conn.closed().await` 上等我们 —— 必须显式关闭
        conn.close(VarInt::from_u32(0), b"done");
        Ok(resp)
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| anyhow::anyhow!("请求超时（{}s）", timeout.as_secs()))?
}

/// 学生 → 老师：提交学号密码，换回会话 token 与**教学班票据**（含 TopicId）。
///
/// 此时学生尚未进白名单，靠 `/class-auth/1.0` 的开放规则放行。
/// 成功后老师把 `conn.remote_id()`（不可伪造）写入白名单 —— 之后学生才可能
/// 连上 `/class-app/1.0` 与 gossip。
pub async fn authenticate(
    endpoint: &Endpoint,
    teacher: EndpointAddr,
    username: &str,
    password: &str,
) -> anyhow::Result<AuthResponse> {
    let req = AuthRequest {
        username: username.to_string(),
        password: password.to_string(),
        claimed_endpoint: Some(endpoint.id().to_string()),
    };
    round_trip(endpoint, teacher, ALPN_AUTH, &req, AUTH_TIMEOUT).await
}

/// 学生 → 老师：应用层调用（需已在白名单内，否则被 `WhitelistHook` 在握手期拒绝）。
pub async fn app_call(
    endpoint: &Endpoint,
    teacher: EndpointAddr,
    req: &AppRequest,
) -> anyhow::Result<AppResponse> {
    round_trip(endpoint, teacher, ALPN_APP, req, APP_TIMEOUT).await
}

/// 便捷封装：刷新教学班票据（Topic 轮换后取新 Topic）。
///
/// 这是被撤销者**拿不到**新 Topic 的关键 —— 他已被移出白名单，
/// `endpoint.connect(.., ALPN_APP)` 会在握手期被拒。
pub async fn refresh_tickets(
    endpoint: &Endpoint,
    teacher: EndpointAddr,
) -> anyhow::Result<Vec<crate::edu::model::SectionTicket>> {
    match app_call(endpoint, teacher, &AppRequest::RefreshTickets).await? {
        AppResponse::Tickets { tickets } => Ok(tickets),
        AppResponse::Denied { message } => anyhow::bail!("老师拒绝了票据刷新：{message}"),
        AppResponse::Error { message } => anyhow::bail!("老师返回错误：{message}"),
        other => anyhow::bail!("意外的响应类型：{other:?}"),
    }
}

/// 便捷封装：我是谁（确认身份与已授权教学班）
pub async fn who_am_i(
    endpoint: &Endpoint,
    teacher: EndpointAddr,
) -> anyhow::Result<AppResponse> {
    app_call(endpoint, teacher, &AppRequest::WhoAmI).await
}

/// 便捷封装：拉取本班当前签名成员表（入班时立即同步用）。
///
/// 返回的 `SignedAllowlist` 已由老师私钥签名，调用方须再走
/// `allowlist::apply_signed_allowlist` 做三重校验（签名 / 签发者 / 纪元单调）。
pub async fn fetch_allowlist(
    endpoint: &Endpoint,
    teacher: EndpointAddr,
    section_id: i64,
) -> anyhow::Result<crate::edu::allowlist::SignedAllowlist> {
    match app_call(endpoint, teacher, &AppRequest::CurrentAllowlist { section_id }).await? {
        AppResponse::Allowlist { signed, .. } => Ok(postcard::from_bytes(&signed)?),
        AppResponse::Denied { message } => anyhow::bail!("老师拒绝下发成员表：{message}"),
        AppResponse::Error { message } => anyhow::bail!("老师返回错误：{message}"),
        other => anyhow::bail!("意外的响应类型：{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 客户端与服务端共用同一套 DTO —— 序列化必须能往返
    #[test]
    fn test_auth_request_serializable() {
        let req = AuthRequest {
            username: "2024001".into(),
            password: "pw".into(),
            claimed_endpoint: Some("abc".into()),
        };
        let raw = serde_json::to_vec(&req).unwrap();
        let back: AuthRequest = serde_json::from_slice(&raw).unwrap();
        assert_eq!(back.username, "2024001");
        assert_eq!(back.claimed_endpoint.as_deref(), Some("abc"));
    }

    #[test]
    fn test_app_request_serializable() {
        let req = AppRequest::AskQuestion {
            section_id: 7,
            question: "为什么".into(),
        };
        let raw = serde_json::to_vec(&req).unwrap();
        assert!(String::from_utf8_lossy(&raw).contains("\"op\":\"AskQuestion\""));
        let back: AppRequest = serde_json::from_slice(&raw).unwrap();
        match back {
            AppRequest::AskQuestion { section_id, .. } => assert_eq!(section_id, 7),
            _ => panic!("应为 AskQuestion"),
        }
    }

    #[test]
    fn test_response_types_deserializable() {
        // 服务端产出的响应，客户端必须能解
        let auth_fail = AuthResponse {
            ok: false,
            message: "bad".into(),
            token: None,
            tickets: Vec::new(),
        };
        let raw = serde_json::to_vec(&auth_fail).unwrap();
        let back: AuthResponse = serde_json::from_slice(&raw).unwrap();
        assert!(!back.ok);

        let app_denied = AppResponse::Denied {
            message: "越权".into(),
        };
        let raw = serde_json::to_vec(&app_denied).unwrap();
        let back: AppResponse = serde_json::from_slice(&raw).unwrap();
        assert!(matches!(back, AppResponse::Denied { .. }));
    }
}
