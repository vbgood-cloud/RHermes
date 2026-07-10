//! QQ Bot API 封装
//!
//! 官方 QQ 开放平台 Bot API。
//! - 认证: AppID + AppSecret → AccessToken（7200s 自动刷新）
//! - 接收: WebSocket Gateway（Identify + 心跳 + 事件）
//! - 发送: REST API（群聊 + C2C）

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

/// QQ Bot API 客户端
pub struct QqApi {
    client: reqwest::Client,
    app_id: String,
    app_secret: String,
    base_url: String,
    /// 缓存的 access_token + 过期时间
    token_cache: Arc<Mutex<Option<TokenCache>>>,
}

struct TokenCache {
    token: String,
    expires_at: Instant,
}

#[derive(Debug)]
pub enum QqError {
    Http(String),
    Api(String),
    Auth(String),
}

impl std::fmt::Display for QqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QqError::Http(e) => write!(f, "HTTP 错误: {e}"),
            QqError::Api(e) => write!(f, "QQ API 错误: {e}"),
            QqError::Auth(e) => write!(f, "QQ 认证错误: {e}"),
        }
    }
}

impl From<reqwest::Error> for QqError {
    fn from(e: reqwest::Error) -> Self {
        QqError::Http(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// API 响应类型
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct GatewayResponse {
    url: String,
}

// ---------------------------------------------------------------------------
// 事件类型
// ---------------------------------------------------------------------------

/// 从 WebSocket 收到的原始事件 JSON
#[derive(Debug, Clone, Deserialize)]
pub struct QqEvent {
    #[serde(rename = "t")]
    pub event_type: Option<String>,
    #[serde(rename = "s")]
    pub seq: Option<u64>,
    #[serde(rename = "d")]
    pub data: Option<serde_json::Value>,
    #[serde(rename = "op")]
    pub op_code: Option<u32>,
}

/// 群聊 @bot 消息
#[derive(Debug, Clone, Deserialize)]
pub struct GroupMessage {
    pub id: String,
    #[serde(rename = "group_openid")]
    pub group_openid: String,
    pub content: String,
    #[serde(rename = "author")]
    pub author: Option<GroupAuthor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GroupAuthor {
    pub member_openid: Option<String>,
}

/// C2C 私聊消息
#[derive(Debug, Clone, Deserialize)]
pub struct C2cMessage {
    pub id: String,
    pub content: String,
    #[serde(rename = "author")]
    pub author: Option<C2cAuthor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct C2cAuthor {
    pub user_openid: Option<String>,
}

// ---------------------------------------------------------------------------
// QqApi 实现
// ---------------------------------------------------------------------------

impl QqApi {
    pub fn new(app_id: &str, app_secret: &str, sandbox: bool) -> Self {
        let base_url = if sandbox {
            "https://sandbox.api.sgroup.qq.com".to_string()
        } else {
            "https://api.sgroup.qq.com".to_string()
        };

        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            app_id: app_id.to_string(),
            app_secret: app_secret.to_string(),
            base_url,
            token_cache: Arc::new(Mutex::new(None)),
        }
    }

    /// 获取 access_token（带缓存）
    pub async fn get_token(&self) -> Result<String, QqError> {
        // 检查缓存
        {
            let cache = self.token_cache.lock().unwrap();
            if let Some(ref tc) = *cache {
                if tc.expires_at > Instant::now() + Duration::from_secs(60) {
                    return Ok(tc.token.clone());
                }
            }
        }

        // 请求新 token
        let resp = self
            .client
            .post("https://bots.qq.com/app/getAppAccessToken")
            .json(&json!({
                "appId": self.app_id,
                "clientSecret": self.app_secret,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(QqError::Auth(format!(
                "获取 AccessToken 失败: {}",
                resp.status()
            )));
        }

        let token_resp: TokenResponse = resp.json().await?;
        let token = token_resp.access_token;
        let expires_at = Instant::now() + Duration::from_secs(token_resp.expires_in);

        // 更新缓存
        {
            let mut cache = self.token_cache.lock().unwrap();
            *cache = Some(TokenCache { token: token.clone(), expires_at });
        }

        Ok(token)
    }

    /// 获取 WebSocket Gateway URL
    pub async fn get_gateway(&self) -> Result<String, QqError> {
        let token = self.get_token().await?;
        let resp = self
            .client
            .get(&format!("{}/gateway", self.base_url))
            .header("Authorization", format!("QQBot {token}"))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(QqError::Api(format!(
                "获取 Gateway 失败: {}",
                resp.status()
            )));
        }

        let gw: GatewayResponse = resp.json().await?;
        Ok(gw.url)
    }

    /// 发送群聊消息
    pub async fn send_group_message(
        &self,
        group_openid: &str,
        content: &str,
        msg_id: Option<&str>,
    ) -> Result<(), QqError> {
        let token = self.get_token().await?;
        let url = format!("{}/v2/groups/{}/messages", self.base_url, group_openid);

        let mut payload = json!({
            "content": content,
            "msg_type": 0,
        });
        if let Some(id) = msg_id {
            payload["msg_id"] = json!(id);
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&payload)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(QqError::Api(format!("发送群消息失败: {body}")));
        }

        Ok(())
    }

    /// 发送 C2C 私聊消息
    pub async fn send_c2c_message(
        &self,
        user_openid: &str,
        content: &str,
        msg_id: Option<&str>,
    ) -> Result<(), QqError> {
        let token = self.get_token().await?;
        let url = format!("{}/v2/users/{}/messages", self.base_url, user_openid);

        let mut payload = json!({
            "content": content,
            "msg_type": 0,
        });
        if let Some(id) = msg_id {
            payload["msg_id"] = json!(id);
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&payload)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(QqError::Api(format!("发送私聊消息失败: {body}")));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qq_api_create() {
        let api = QqApi::new("123456", "secret", false);
        assert_eq!(api.app_id, "123456");
        assert_eq!(api.base_url, "https://api.sgroup.qq.com");
    }

    #[test]
    fn test_qq_api_sandbox() {
        let api = QqApi::new("123456", "secret", true);
        assert_eq!(api.base_url, "https://sandbox.api.sgroup.qq.com");
    }

    #[test]
    fn test_qq_event_parse() {
        let json_str = r#"{"op":0,"t":"GROUP_AT_MESSAGE_CREATE","s":1,"d":{"id":"msg1","group_openid":"group1","content":"你好"}}"#;
        let event: QqEvent = serde_json::from_str(json_str).unwrap();
        assert_eq!(event.event_type, Some("GROUP_AT_MESSAGE_CREATE".to_string()));
        assert!(event.data.is_some());
    }

    #[test]
    fn test_group_message_parse() {
        let json_str = r#"{"id":"msg1","group_openid":"group1","content":"你好","author":{"member_openid":"user1"}}"#;
        let msg: GroupMessage = serde_json::from_str(json_str).unwrap();
        assert_eq!(msg.content, "你好");
        assert_eq!(msg.group_openid, "group1");
    }

    #[test]
    fn test_c2c_message_parse() {
        let json_str = r#"{"id":"msg2","content":"私聊","author":{"user_openid":"user2"}}"#;
        let msg: C2cMessage = serde_json::from_str(json_str).unwrap();
        assert_eq!(msg.content, "私聊");
    }
}
