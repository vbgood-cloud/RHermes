//! Jev 判断层 — HTTP 客户端
//!
//! `POST {base_url}/v1/systemone`，Bearer 认证，失败重试 1 次。
//! 网络/超时/非 2xx 一律返回 `JudgeError`，由上层（mod.rs 门面）吞成回退。

use std::time::Duration;

use crate::core::{Config, ProxyConfig};
use crate::judge::types::{JudgeRequest, JudgeResponse};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 判断层错误（对齐全库惯例：自定义枚举）
#[derive(Debug)]
pub enum JudgeError {
    /// 网络/超时等传输层错误
    Http(String),
    /// API 返回非 2xx（400 校验错误 / 401 未授权 / 5xx）
    Api { status: u16, body: String },
    /// 响应体反序列化失败（API 结构变动）
    Deserialize(String),
}

impl std::fmt::Display for JudgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "判断层网络错误: {e}"),
            Self::Api { status, body } => {
                let brief: String = body.chars().take(200).collect();
                write!(f, "判断层 API 错误(HTTP {status}): {brief}")
            }
            Self::Deserialize(e) => write!(f, "判断层响应解析失败: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------

/// System One API 客户端（无状态，可 Clone 共享）
#[derive(Clone)]
pub struct JudgeClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl JudgeClient {
    /// 从全局配置构建：HTTP 客户端走 D17 代理工厂（feature = "jev"）
    pub fn from_config(config: &Config) -> Self {
        let timeout = Duration::from_secs(config.jev.timeout_secs.max(1));
        Self {
            http: crate::core::http_client::create_proxied_client(
                &config.proxy,
                "jev",
                timeout,
            ),
            base_url: config.jev.base_url.trim_end_matches('/').to_string(),
            api_key: config.jev.api_key.clone(),
        }
    }

    /// 测试/脚本用：显式指定代理配置与超时
    pub fn new(proxy: &ProxyConfig, base_url: &str, api_key: &str, timeout: Duration) -> Self {
        Self {
            http: crate::core::http_client::create_proxied_client(proxy, "jev", timeout),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// 发起判断请求；传输层错误自动重试 1 次（判断便宜，重试成本低）
    pub async fn ask(&self, request: &JudgeRequest) -> Result<JudgeResponse, JudgeError> {
        let url = format!("{}/v1/systemone", self.base_url);

        let mut last_err = None;
        for attempt in 0..2 {
            if attempt == 1 {
                tracing::debug!("判断层请求失败，重试 1 次: {last_err:?}");
            }
            match self.ask_once(&url, request).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    // 4xx 属于请求本身的问题，重试无意义
                    let retryable = !matches!(&e, JudgeError::Api { status, .. } if *status < 500);
                    last_err = Some(e);
                    if !retryable {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or(JudgeError::Http("未知错误".into())))
    }

    /// 单次请求
    async fn ask_once(
        &self,
        url: &str,
        request: &JudgeRequest,
    ) -> Result<JudgeResponse, JudgeError> {
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
            .map_err(|e| JudgeError::Http(e.to_string()))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(JudgeError::Api { status, body });
        }

        resp.json::<JudgeResponse>()
            .await
            .map_err(|e| JudgeError::Deserialize(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// 测试：本地 axum mock server 覆盖四路径（成功/400/401/超时）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use axum::response::IntoResponse;
    use crate::judge::types::Question;

    /// 起一个本地 mock server，返回 (base_url, shutdown)
    async fn spawn_mock(
        status: axum::http::StatusCode,
        delay: Option<Duration>,
    ) -> (String, tokio::sync::oneshot::Sender<()>) {
        let app = axum::Router::new().route(
            "/v1/systemone",
            axum::routing::post(move || async move {
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
                if status == axum::http::StatusCode::OK {
                    // 官方样例结构的成功响应
                    axum::Json(serde_json::json!({
                        "model": "jev-test",
                        "answers": { "q1": { "type": "noul", "noul": 0.9 } },
                        "usage": { "input_tokens": 10, "output_tokens": 2 }
                    }))
                    .into_response()
                } else {
                    (status, r#"{"error":"mock"}"#).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { let _ = rx.await; })
                .await
                .unwrap();
        });
        (format!("http://{addr}"), tx)
    }

    fn sample_request() -> JudgeRequest {
        let mut questions = HashMap::new();
        questions.insert("q1".to_string(), Question::noul("测试问题"));
        JudgeRequest {
            state: serde_json::Value::String("测试 state".into()),
            model: "jev-latest".into(),
            questions,
        }
    }

    fn direct_client(base_url: &str, timeout: Duration) -> JudgeClient {
        // Auto 模式 + 无代理 URL → 直连
        JudgeClient::new(&ProxyConfig::default(), base_url, "test-key", timeout)
    }

    #[tokio::test]
    async fn test_ask_success() {
        let (url, shutdown) = spawn_mock(axum::http::StatusCode::OK, None).await;
        let client = direct_client(&url, Duration::from_secs(2));
        let resp = client.ask(&sample_request()).await.unwrap();
        assert_eq!(resp.model, "jev-test");
        assert_eq!(resp.answers.get("q1").unwrap().as_noul(), Some(0.9));
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn test_ask_400_no_retry_and_propagates_status() {
        let (url, shutdown) = spawn_mock(axum::http::StatusCode::BAD_REQUEST, None).await;
        let client = direct_client(&url, Duration::from_secs(2));
        let err = client.ask(&sample_request()).await.unwrap_err();
        match err {
            JudgeError::Api { status, .. } => assert_eq!(status, 400),
            other => panic!("期望 Api 错误，实际: {other:?}"),
        }
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn test_ask_401_unauthorized() {
        let (url, shutdown) = spawn_mock(axum::http::StatusCode::UNAUTHORIZED, None).await;
        let client = direct_client(&url, Duration::from_secs(2));
        let err = client.ask(&sample_request()).await.unwrap_err();
        match err {
            JudgeError::Api { status, .. } => assert_eq!(status, 401),
            other => panic!("期望 Api 错误，实际: {other:?}"),
        }
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn test_ask_timeout_returns_http_error() {
        // 服务端延迟 3s，客户端超时 1s（ask 内部重试 1 次后仍超时）
        let (url, shutdown) =
            spawn_mock(axum::http::StatusCode::OK, Some(Duration::from_secs(3))).await;
        let client = direct_client(&url, Duration::from_secs(1));
        let err = client.ask(&sample_request()).await.unwrap_err();
        assert!(matches!(err, JudgeError::Http(_)), "实际: {err:?}");
        let _ = shutdown.send(());
    }
}
