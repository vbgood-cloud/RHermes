//! Telegram Bot API 轻量封装
//!
//! 只封装需要的 API 端点，不过度抽象。
//! 所有请求通过 reqwest::Client 发出。

use serde::Deserialize;

// ---- 错误类型 ----

#[derive(Debug)]
pub enum TgError {
    Http(reqwest::Error),
    Api(String),
    Parse(String),
}

impl std::fmt::Display for TgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TgError::Http(e) => write!(f, "HTTP 错误: {e}"),
            TgError::Api(s) => write!(f, "Telegram API 错误: {s}"),
            TgError::Parse(s) => write!(f, "解析错误: {s}"),
        }
    }
}

impl From<reqwest::Error> for TgError {
    fn from(e: reqwest::Error) -> Self { TgError::Http(e) }
}

// ---- 数据类型 ----

#[derive(Debug, Deserialize)]
pub struct TgUpdate {
    pub update_id: i64,
    pub message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
pub struct TgMessage {
    pub message_id: i64,
    pub chat: TgChat,
    pub text: Option<String>,
    pub from: Option<TgUser>,
}

#[derive(Debug, Deserialize)]
pub struct TgChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Debug, Deserialize)]
pub struct TgUser {
    pub id: i64,
    pub first_name: String,
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgBotInfo {
    pub id: i64,
    pub first_name: String,
    pub username: String,
}

#[derive(Deserialize)]
struct TgResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

// ---- API 封装 ----

pub struct TelegramApi {
    client: reqwest::Client,
    token: String,
}

impl TelegramApi {
    pub fn new(client: reqwest::Client, token: String) -> Self {
        Self { client, token }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }

    /// 验证 Bot Token
    pub async fn get_me(&self) -> Result<TgBotInfo, TgError> {
        let url = self.api_url("getMe");
        let resp = self.client.get(&url).send().await?;
        let body: TgResponse<TgBotInfo> = resp.json().await
            .map_err(|e| TgError::Parse(format!("{e}")))?;
        if body.ok {
            body.result.ok_or_else(|| TgError::Api("getMe 返回空结果".into()))
        } else {
            Err(TgError::Api(body.description.unwrap_or_else(|| "未知错误".into())))
        }
    }

    /// Long Polling 获取更新
    pub async fn get_updates(
        &self,
        offset: i64,
        timeout: u32,
    ) -> Result<Vec<TgUpdate>, TgError> {
        let url = self.api_url("getUpdates");
        let resp = self.client
            .post(&url)
            .json(&serde_json::json!({
                "offset": offset,
                "timeout": timeout,
                "allowed_updates": ["message"]
            }))
            .timeout(std::time::Duration::from_secs(timeout as u64 + 10))
            .send()
            .await?;
        let body: TgResponse<Vec<TgUpdate>> = resp.json().await
            .map_err(|e| TgError::Parse(format!("{e}")))?;
        if body.ok {
            Ok(body.result.unwrap_or_default())
        } else {
            Err(TgError::Api(body.description.unwrap_or_else(|| "未知错误".into())))
        }
    }

    /// 发送文本消息
    pub async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
    ) -> Result<(), TgError> {
        let url = self.api_url("sendMessage");
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });
        if let Some(pm) = parse_mode {
            payload["parse_mode"] = serde_json::Value::String(pm.into());
        }
        let resp = self.client.post(&url).json(&payload).send().await?;
        let body: TgResponse<serde_json::Value> = resp.json().await
            .map_err(|e| TgError::Parse(format!("{e}")))?;
        if body.ok {
            Ok(())
        } else {
            Err(TgError::Api(body.description.unwrap_or_else(|| "发送失败".into())))
        }
    }

    /// 发送聊天动作（如 typing 状态）
    pub async fn send_chat_action(
        &self,
        chat_id: &str,
        action: &str,
    ) -> Result<(), TgError> {
        let url = self.api_url("sendChatAction");
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "action": action,
        });
        let resp = self.client.post(&url).json(&payload).send().await?;
        let body: TgResponse<serde_json::Value> = resp.json().await
            .map_err(|e| TgError::Parse(format!("{e}")))?;
        if body.ok {
            Ok(())
        } else {
            Err(TgError::Api(body.description.unwrap_or_else(|| "未知错误".into())))
        }
    }

    /// 发送文档（预留）
    #[allow(dead_code)]
    pub async fn send_document(
        &self,
        chat_id: &str,
        file_path: &str,
    ) -> Result<(), TgError> {
        let url = self.api_url("sendDocument");
        let file = tokio::fs::read(file_path).await
            .map_err(|e| TgError::Parse(format!("读取文件失败: {e}")))?;
        let file_name = std::path::Path::new(file_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let part = reqwest::multipart::Part::bytes(file)
            .file_name(file_name)
            .mime_str("application/octet-stream")
            .map_err(|e| TgError::Parse(format!("{e}")))?;
        let form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);
        let resp = self.client.post(&url).multipart(form).send().await?;
        let body: TgResponse<serde_json::Value> = resp.json().await
            .map_err(|e| TgError::Parse(format!("{e}")))?;
        if body.ok {
            Ok(())
        } else {
            Err(TgError::Api(body.description.unwrap_or_else(|| "发送文件失败".into())))
        }
    }
}
