//! Jev 判断层 — 类型定义（请求/响应 serde）
//!
//! 对应 TypeSafe System One API（docs.typesafe.ai）：
//! `POST /v1/systemone`，请求体 `{state, model, questions}`，
//! 响应体 `{model, answers, usage}`。
//!
//! 官方工程约束（见 D18 §1.3）：
//! - 问题 ID 不发给模型 → 完整问题必须写进 instructions
//! - score 弱数值校准 → 禁止档位间插值，只做阈值判断

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 请求类型
// ---------------------------------------------------------------------------

/// 三种问题原语：Choice（多选一）/ Score（档位评分）/ Noul（是非概率）
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// 是非判断 → 0–1 概率，无 confidence 字段
    Noul {
        /// 完整的问题描述
        instructions: String,
        /// 可选 true/false 描述
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<HashMap<String, String>>,
    },
    /// 多选一 → 选中项 + 每项概率 + confidence（criteria ≤255 项）
    Choice {
        /// 完整的问题描述
        instructions: String,
        /// 选项 → 描述（建议含 other/unclear 兜底项）
        criteria: HashMap<String, String>,
    },
    /// 档位评分 → 概率加权档位 + confidence（2–10 个有序档）
    ///
    /// ⚠️ score 弱数值校准：只做阈值判断，禁止档位间插值
    Score {
        /// 完整的问题描述
        instructions: String,
        /// 有序档位描述（index 0 为第一档）
        criteria: Vec<String>,
    },
}

impl Question {
    /// 便捷构造：Noul 问题
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul { instructions: instructions.into(), criteria: None }
    }

    /// 便捷构造：Choice 问题
    pub fn choice(instructions: impl Into<String>, criteria: HashMap<String, String>) -> Self {
        Self::Choice { instructions: instructions.into(), criteria }
    }

    /// 便捷构造：Score 问题
    pub fn score(instructions: impl Into<String>, criteria: Vec<String>) -> Self {
        Self::Score { instructions: instructions.into(), criteria }
    }
}

/// System One 请求体
#[derive(Debug, Serialize)]
pub struct JudgeRequest {
    /// 待判断的内容：字符串或结构化 JSON（对话日志/记录/应用状态）
    pub state: serde_json::Value,
    /// 模型（jev-latest 或锁定版本如 jev-1.13.0）
    pub model: String,
    /// 命名问题映射；key 为调用方自定 ID，答案按同 ID 返回。
    /// 多问题并行独立评估，几乎不增加延迟（官方 fan-out 模式）
    pub questions: HashMap<String, Question>,
}

// ---------------------------------------------------------------------------
// 响应类型
// ---------------------------------------------------------------------------

/// 类型化答案（三原语之一）
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TypedAnswer {
    /// 是非概率（"是"的概率；无 confidence 字段）
    Noul { noul: f64 },
    /// 选中项 + 分布集中度（confidence 是分布形状，不是正确性）
    Choice {
        choice: String,
        confidence: f64,
        probabilities: HashMap<String, f64>,
    },
    /// 概率加权档位 + confidence（⚠️ 禁止插值）
    Score {
        score: f64,
        confidence: f64,
        #[serde(default)]
        probabilities: HashMap<String, f64>,
    },
}

impl TypedAnswer {
    /// Noul 概率（非 Noul 类型返回 None）
    pub fn as_noul(&self) -> Option<f64> {
        match self {
            Self::Noul { noul } => Some(*noul),
            _ => None,
        }
    }

    /// (选中项, confidence)（非 Choice 类型返回 None）
    pub fn as_choice(&self) -> Option<(&str, f64)> {
        match self {
            Self::Choice { choice, confidence, .. } => Some((choice.as_str(), *confidence)),
            _ => None,
        }
    }

    /// (档位分, confidence)（非 Score 类型返回 None）
    ///
    /// ⚠️ score 只可做阈值比较，不可当连续分数使用
    pub fn as_score(&self) -> Option<(f64, f64)> {
        match self {
            Self::Score { score, confidence, .. } => Some((*score, *confidence)),
            _ => None,
        }
    }
}

/// token 用量（仅输入计费，输出免费）
#[derive(Debug, Clone, Deserialize)]
pub struct JudgeUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// System One 响应体
#[derive(Debug, Deserialize)]
pub struct JudgeResponse {
    /// 实际应答的模型版本（记录日志用于核对版本漂移）
    pub model: String,
    /// 按请求中的问题 ID 返回类型化答案
    pub answers: HashMap<String, TypedAnswer>,
    pub usage: JudgeUsage,
}

// ---------------------------------------------------------------------------
// 测试：官方 quickstart 样例 round-trip
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 官方 quickstart 请求体样例（混三原语）→ 序列化格式校验
    #[test]
    fn test_request_serialization_matches_official_schema() {
        let mut questions = HashMap::new();
        questions.insert(
            "is_urgent".to_string(),
            Question::noul("The message conveys urgency or time-sensitivity"),
        );
        let mut criteria = HashMap::new();
        criteria.insert("billing".to_string(), "Payment or subscription issues".to_string());
        criteria.insert("technical".to_string(), "Bugs or integration problems".to_string());
        questions.insert(
            "department".to_string(),
            Question::choice("Which team should handle this", criteria),
        );
        questions.insert(
            "frustration".to_string(),
            Question::score(
                "How frustrated the customer appears",
                vec![
                    "Calm, just stating facts".to_string(),
                    "Frustrated but civil".to_string(),
                    "Very angry, strong language".to_string(),
                ],
            ),
        );

        let req = JudgeRequest {
            state: serde_json::Value::String("help, payouts failing".into()),
            model: "jev-latest".into(),
            questions,
        };

        let json = serde_json::to_value(&req).unwrap();
        // tag 小写 + 字段结构对齐官方 schema
        assert_eq!(json["questions"]["is_urgent"]["type"], "noul");
        assert_eq!(json["questions"]["department"]["type"], "choice");
        assert_eq!(json["questions"]["frustration"]["type"], "score");
        // noul 无 criteria 时不序列化（官方允许省略）
        assert!(json["questions"]["is_urgent"].get("criteria").is_none());
        // choice criteria 为 map
        assert!(json["questions"]["department"]["criteria"].is_object());
        // score criteria 为有序数组
        assert!(json["questions"]["frustration"]["criteria"].is_array());
    }

    /// 官方 quickstart 响应体样例 → 反序列化校验
    #[test]
    fn test_response_deserialization_official_sample() {
        let body = r#"{
  "model": "jev-1.13.0",
  "answers": {
    "department": {
      "type": "choice",
      "choice": "technical",
      "confidence": 0.78,
      "probabilities": { "technical": 0.85, "sales": 0.0, "billing": 0.15 }
    },
    "frustration": {
      "type": "score",
      "score": 1.0,
      "confidence": 1.0,
      "legend": { "0": "Calm", "1": "Frustrated", "2": "Very angry" },
      "probabilities": { "0": 0.0, "1": 1.0, "2": 0.0 }
    },
    "is_urgent": { "type": "noul", "noul": 1.0 }
  },
  "usage": { "input_tokens": 392, "output_tokens": 65 }
}"#;
        let resp: JudgeResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.model, "jev-1.13.0");
        assert_eq!(resp.usage.input_tokens, 392);

        let urgent = resp.answers.get("is_urgent").unwrap();
        assert_eq!(urgent.as_noul(), Some(1.0));

        let dept = resp.answers.get("department").unwrap();
        let (choice, confidence) = dept.as_choice().unwrap();
        assert_eq!(choice, "technical");
        assert!((confidence - 0.78).abs() < 1e-9);

        let frus = resp.answers.get("frustration").unwrap();
        let (score, _) = frus.as_score().unwrap();
        assert_eq!(score, 1.0);
    }

    /// 错误类型问题 ID：answers 里不含该 key（调用方按 get 取，不 panic）
    #[test]
    fn test_missing_answer_key_is_none() {
        let body = r#"{
  "model": "jev-1.13.0",
  "answers": { "only": { "type": "noul", "noul": 0.3 } },
  "usage": { "input_tokens": 1, "output_tokens": 1 }
}"#;
        let resp: JudgeResponse = serde_json::from_str(body).unwrap();
        assert!(resp.answers.get("missing").is_none());
        assert_eq!(resp.answers.get("only").unwrap().as_noul(), Some(0.3));
    }
}
