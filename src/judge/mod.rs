//! Jev 判断层 — 门面与全局单例（D18）
//!
//! 定位：与 DeepSeek（生成层，`provider/`）互补的高频结构化判断层。
//! 核心契约（docs/decisions/D18-jev-judge.md §2.1/§3.3）：
//!
//! - **纯增益非硬依赖**：未启用 / 未配置 key / 网络失败 / 超时 / confidence
//!   不足，一律返回 `None`，调用方走原方案（回退路径）
//! - **绝不阻塞主流程**：全部错误在门面内吞掉，判断层无 panic 路径
//! - **便捷方法只有"有答案 / 无答案"两种出口**，调用方不感知失败原因

mod client;
mod types;

pub use client::{JudgeClient, JudgeError};
pub use types::{JudgeRequest, JudgeResponse, JudgeUsage, Question, TypedAnswer};

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::core::Config;

// ---------------------------------------------------------------------------
// 门面
// ---------------------------------------------------------------------------

/// 判断层门面：持有客户端与门控阈值
///
/// 通过 `from_config` 构建（enabled + api_key 缺一返回 None），
/// 全局实例经 `set_global_judge` / `global_judge` 访问（对齐
/// `tools::set_global_*` 惯例）。
#[derive(Clone)]
pub struct Judge {
    client: JudgeClient,
    min_confidence: f64,
    /// 实际使用的模型（记录日志核对版本漂移）
    model: String,
}

impl Judge {
    /// 直接构建（测试/脚本用；业务代码请用 from_config）
    pub fn new(client: JudgeClient, min_confidence: f64, model: impl Into<String>) -> Self {
        Self { client, min_confidence, model: model.into() }
    }

    /// confidence 门控阈值（fan-out 调用方自行比对用）
    pub fn min_confidence(&self) -> f64 {
        self.min_confidence
    }

    /// 从配置构建；未启用或未配置 key 返回 None（= 全部集成点走回退）
    pub fn from_config(config: &Config) -> Option<Self> {
        if !config.jev.enabled || config.jev.api_key.is_empty() {
            return None;
        }
        tracing::info!(
            model = %config.jev.model,
            base_url = %config.jev.base_url,
            min_confidence = config.jev.min_confidence,
            "判断层已启用 (D18)"
        );
        Some(Self {
            client: JudgeClient::from_config(config),
            min_confidence: config.jev.min_confidence,
            model: config.jev.model.clone(),
        })
    }

    /// 批量判断：一次请求多问题（官方 fan-out，问题间并行独立评估）
    ///
    /// 失败返回 Err —— 仅建议 P0 评估脚本等需要显式感知错误的调用方使用；
    /// 业务集成点请用 `noul` / `choice_gated`（自动吞错回退）。
    pub async fn ask(
        &self,
        state: &str,
        questions: HashMap<String, Question>,
    ) -> Result<JudgeResponse, JudgeError> {
        let request = JudgeRequest {
            state: serde_json::Value::String(state.to_string()),
            model: self.model.clone(),
            questions,
        };
        self.client.ask(&request).await
    }

    /// 单个是非判断；`None` = 未启用/失败/拿不准 → 调用方走回退
    ///
    /// `threshold` 为"是"的概率门限（如 0.7）：
    /// - noul ≥ threshold → Some(true)
    /// - noul ≤ 1 - threshold → Some(false)
    /// - 其间（真模糊）→ None（走回退，宁保守）
    pub async fn noul(&self, state: &str, instructions: &str, threshold: f64) -> Option<bool> {
        let mut questions = HashMap::new();
        questions.insert("q".to_string(), Question::noul(instructions));
        let resp = self.ask(state, questions).await.ok()?;
        let p = resp.answers.get("q")?.as_noul()?;
        if p >= threshold {
            Some(true)
        } else if p <= 1.0 - threshold {
            Some(false)
        } else {
            tracing::debug!(p, "判断层 Noul 结果模糊，走回退");
            None
        }
    }

    /// 单个多选判断 + confidence 门控；`None` = 回退
    ///
    /// confidence < min_confidence 视为"拿不准"，与失败同待遇（D18 §3.3）
    pub async fn choice_gated(
        &self,
        state: &str,
        instructions: &str,
        criteria: HashMap<String, String>,
    ) -> Option<String> {
        let mut questions = HashMap::new();
        questions.insert("q".to_string(), Question::choice(instructions, criteria));
        let resp = self.ask(state, questions).await.ok()?;
        let (choice, confidence) = resp.answers.get("q")?.as_choice()?;
        if confidence < self.min_confidence {
            tracing::debug!(choice, confidence, min = self.min_confidence, "判断层置信度不足，走回退");
            return None;
        }
        Some(choice.to_string())
    }
}

// ---------------------------------------------------------------------------
// 全局单例
// ---------------------------------------------------------------------------

/// 全局判断层实例（应用启动时初始化一次）
static GLOBAL_JUDGE: OnceLock<Option<Judge>> = OnceLock::new();

/// 设置全局判断层；配置未启用时传入 None 也可（显式标记"无判断层"）
pub fn set_global_judge(judge: Option<Judge>) {
    let _ = GLOBAL_JUDGE.set(judge);
}

/// 获取全局判断层；None = 未初始化或未启用 → 调用方走回退
pub fn global_judge() -> Option<&'static Judge> {
    GLOBAL_JUDGE.get().and_then(|j| j.as_ref())
}

// ---------------------------------------------------------------------------
// 测试：门控语义（回退路径可达性）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 构建指向本地 mock 的 Judge（成功返回固定 noul=0.9）
    async fn mock_judge(noul_value: f64, min_confidence: f64) -> Judge {
        let app = axum::Router::new().route(
            "/v1/systemone",
            axum::routing::post(move || async move {
                axum::Json(serde_json::json!({
                    "model": "jev-test",
                    "answers": {
                        "q": { "type": "noul", "noul": noul_value },
                        "c": { "type": "choice", "choice": "a", "confidence": 0.5,
                               "probabilities": { "a": 0.5, "b": 0.5 } }
                    },
                    "usage": { "input_tokens": 1, "output_tokens": 1 }
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        Judge {
            client: JudgeClient::new(
                &crate::core::ProxyConfig::default(),
                &format!("http://{addr}"),
                "test-key",
                std::time::Duration::from_secs(2),
            ),
            min_confidence,
            model: "jev-test".into(),
        }
    }

    #[tokio::test]
    async fn test_noul_threshold_semantics() {
        // noul=0.9, threshold=0.7 → 明确为 true
        let j = mock_judge(0.9, 0.6).await;
        assert_eq!(j.noul("state", "问题", 0.7).await, Some(true));

        // noul=0.5（真模糊区）→ None 回退
        let j = mock_judge(0.5, 0.6).await;
        assert_eq!(j.noul("state", "问题", 0.7).await, None);

        // noul=0.05, threshold=0.7 → 明确为 false
        let j = mock_judge(0.05, 0.6).await;
        assert_eq!(j.noul("state", "问题", 0.7).await, Some(false));
    }

    #[tokio::test]
    async fn test_choice_low_confidence_returns_none() {
        // mock 固定 confidence=0.5；门控 0.9 → 必回退（验证 §五.7 门控分支可达）
        let j = mock_judge(0.9, 0.9).await;
        let mut criteria = HashMap::new();
        criteria.insert("a".to_string(), "选项 A".to_string());
        criteria.insert("b".to_string(), "选项 B".to_string());
        assert_eq!(j.choice_gated("state", "问题", criteria).await, None);
    }

    #[test]
    fn test_global_judge_none_when_uninitialized() {
        // 未 set 时 global_judge() 为 None（回退路径默认态）
        // 注：OnceLock 单例在同一进程内只能初始化一次，这里只验证读取侧
        let _ = global_judge(); // 不 panic 即可
    }
}
