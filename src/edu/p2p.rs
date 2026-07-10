//! iroh P2P 课堂网络
//!
//! 教师创建课堂 → 生成课程码 → 学生通过课程码加入。
//! 基于 iroh Endpoint 实现 P2P 连接，内置 NAT 穿透。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// 课堂消息类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClassroomMessage {
    /// 学生在线心跳
    Heartbeat {
        student_id: i64,
        student_no: String,
        student_name: String,
        timestamp: String,
    },
    /// 活动上报
    ActivityReport {
        student_id: i64,
        course_id: i64,
        lesson_num: i64,
        tool_name: String,
        success: bool,
        duration_ms: u64,
        timestamp: String,
    },
    /// 教师通知
    Notification {
        title: String,
        content: String,
        timestamp: String,
    },
    /// 模式切换指令
    ModeSwitch {
        student_id: Option<i64>, // None = 全班
        mode: String,           // explore / scaffold / locked
        timestamp: String,
    },
    /// 学生求助
    HelpRequest {
        student_id: i64,
        question: String,
        timestamp: String,
    },
    /// 认证请求（学生→教师）
    AuthRequest {
        student_no: String,
        password: String,
    },
    /// 认证响应（教师→学生）
    AuthResponse {
        success: bool,
        token: Option<String>,
        student_name: Option<String>,
        courses: Vec<CourseBrief>,
        message: String,
    },
    /// 学生加入课堂
    JoinClassroom {
        student_no: String,
        student_name: String,
    },
}

/// 课程简要信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CourseBrief {
    pub course_code: String,
    pub name: String,
}

/// 课程码编码/解码（将 iroh NodeId 编码为短码供学生输入）
pub fn encode_course_code(node_id: &str) -> String {
    // 取 NodeId 的前 12 个字符作为课程码
    let short: String = node_id.chars().take(12).collect();
    short.to_uppercase()
}

/// 验证课程码格式
pub fn validate_course_code(code: &str) -> bool {
    code.len() >= 8 && code.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 当前时间戳
pub fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// EduEndpoint — 封装 iroh Endpoint
// ---------------------------------------------------------------------------

/// 教育版 P2P 端点
pub struct EduEndpoint {
    /// iroh 端点（延迟初始化）
    endpoint: Option<iroh::Endpoint>,
    /// 角色：teacher / student
    role: String,
}

impl EduEndpoint {
    /// 创建教师端点
    pub async fn new_teacher() -> Result<Self, String> {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .bind()
            .await
            .map_err(|e| format!("iroh Endpoint 创建失败: {e}"))?;

        let node_id = endpoint.id().to_string();
        tracing::info!("教师 P2P 端点已创建: NodeId = {node_id}");
        println!("📡 教师端点已启动");
        println!("   NodeId: {node_id}");
        println!("   课程码: {}", encode_course_code(&node_id));

        Ok(Self {
            endpoint: Some(endpoint),
            role: "teacher".to_string(),
        })
    }

    /// 创建学生端点
    pub async fn new_student() -> Result<Self, String> {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .bind()
            .await
            .map_err(|e| format!("iroh Endpoint 创建失败: {e}"))?;

        tracing::info!("学生 P2P 端点已创建: NodeId = {}", endpoint.id());

        Ok(Self {
            endpoint: Some(endpoint),
            role: "student".to_string(),
        })
    }

    /// 获取 NodeId
    pub fn node_id(&self) -> Option<String> {
        self.endpoint.as_ref().map(|e| e.id().to_string())
    }

    /// 获取课程码（教师用）
    pub fn course_code(&self) -> Option<String> {
        self.node_id().map(|id| encode_course_code(&id))
    }

    /// 获取端点引用
    pub fn endpoint(&self) -> Option<&iroh::Endpoint> {
        self.endpoint.as_ref()
    }
}

// ---------------------------------------------------------------------------
// 消息序列化辅助
// ---------------------------------------------------------------------------

/// 序列化课堂消息为 JSON bytes
pub fn encode_message(msg: &ClassroomMessage) -> Vec<u8> {
    serde_json::to_vec(msg).unwrap_or_default()
}

/// 反序列化课堂消息
pub fn decode_message(data: &[u8]) -> Option<ClassroomMessage> {
    serde_json::from_slice(data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_message() {
        let msg = ClassroomMessage::Heartbeat {
            student_id: 1,
            student_no: "2024001".into(),
            student_name: "张三".into(),
            timestamp: now_ts(),
        };
        let encoded = encode_message(&msg);
        assert!(!encoded.is_empty());

        let decoded = decode_message(&encoded).unwrap();
        match decoded {
            ClassroomMessage::Heartbeat { student_no, .. } => {
                assert_eq!(student_no, "2024001");
            }
            _ => panic!("应该是 Heartbeat"),
        }
    }

    #[test]
    fn test_encode_course_code() {
        let node_id = "abcdefghijklmnopqrstuvwxyz123456";
        let code = encode_course_code(node_id);
        assert_eq!(code.len(), 12);
        assert_eq!(code, "ABCDEFGHIJKL");
    }

    #[test]
    fn test_validate_course_code() {
        assert!(validate_course_code("ABCDEF1234"));
        assert!(!validate_course_code("short"));
        assert!(!validate_course_code("特殊字符!!"));
    }

    #[test]
    fn test_message_serialization_notification() {
        let msg = ClassroomMessage::Notification {
            title: "测试通知".into(),
            content: "明天考试".into(),
            timestamp: now_ts(),
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        match decoded {
            ClassroomMessage::Notification { title, .. } => {
                assert_eq!(title, "测试通知");
            }
            _ => panic!("应该是 Notification"),
        }
    }

    #[test]
    fn test_message_serialization_auth() {
        let req = ClassroomMessage::AuthRequest {
            student_no: "2024001".into(),
            password: "secret".into(),
        };
        let encoded = encode_message(&req);
        let decoded = decode_message(&encoded).unwrap();
        match decoded {
            ClassroomMessage::AuthRequest { student_no, password } => {
                assert_eq!(student_no, "2024001");
                assert_eq!(password, "secret");
            }
            _ => panic!("应该是 AuthRequest"),
        }
    }

    #[test]
    fn test_message_serialization_mode_switch() {
        let msg = ClassroomMessage::ModeSwitch {
            student_id: None,
            mode: "scaffold".into(),
            timestamp: now_ts(),
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        match decoded {
            ClassroomMessage::ModeSwitch { mode, .. } => {
                assert_eq!(mode, "scaffold");
            }
            _ => panic!("应该是 ModeSwitch"),
        }
    }
}
