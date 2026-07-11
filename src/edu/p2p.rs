//! iroh P2P 课堂网络
//!
//! 教师创建课堂 → 生成课程码 → 学生通过课程码加入。
//! 基于 iroh Endpoint 实现 P2P 连接，内置 NAT 穿透。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// ALPN 协议标识（教师和学生必须一致）
const EDU_ALPN: &[u8] = b"rhermes-edu/1";

/// 课堂消息类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClassroomMessage {
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
    /// 课程同步（教师→学生）
    CourseSync {
        course_code: String,
        lessons: Vec<LessonBrief>,
        assignments: Vec<AssignmentBrief>,
        published_lessons: Vec<i64>,
        published_assignments: Vec<i64>,
    },
    /// 学生在线心跳
    Heartbeat {
        student_no: String,
        student_name: String,
        timestamp: String,
    },
    /// 活动上报（学生→教师）
    ActivityReport {
        student_no: String,
        course_code: String,
        lesson_num: i64,
        tool_name: String,
        success: bool,
        duration_ms: u64,
        timestamp: String,
    },
    /// 教师通知（教师→学生）
    Notification {
        title: String,
        content: String,
        timestamp: String,
    },
    /// 模式切换指令（教师→学生）
    ModeSwitch {
        mode: String,
        timestamp: String,
    },
    /// 学生求助（学生→教师）
    HelpRequest {
        student_no: String,
        question: String,
        timestamp: String,
    },
    /// 作业提交（学生→教师）
    SubmitAssignment {
        student_no: String,
        assignment_id: i64,
        content: String,
        timestamp: String,
    },
}

/// 课程简要信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CourseBrief {
    pub course_code: String,
    pub name: String,
}

/// 课次简要信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonBrief {
    pub lesson_num: i64,
    pub topic: String,
}

/// 作业简要信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignmentBrief {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub due_date: String,
}

/// 课程码编码/解码
pub fn encode_course_code(node_id: &str) -> String {
    let short: String = node_id.chars().take(12).collect();
    short.to_uppercase()
}

pub fn validate_course_code(code: &str) -> bool {
    code.len() >= 8 && code.chars().all(|c| c.is_ascii_alphanumeric())
}

pub fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// 消息序列化
// ---------------------------------------------------------------------------

pub fn encode_message(msg: &ClassroomMessage) -> Vec<u8> {
    serde_json::to_vec(msg).unwrap_or_default()
}

pub fn decode_message(data: &[u8]) -> Option<ClassroomMessage> {
    serde_json::from_slice(data).ok()
}

// ---------------------------------------------------------------------------
// 教师端：监听连接 + 处理认证
// ---------------------------------------------------------------------------

/// 教师端 P2P 服务
pub struct TeacherP2P {
    endpoint: iroh::Endpoint,
    node_id_str: String,
    course_code: String,
}

impl TeacherP2P {
    /// 创建教师端点并开始监听
    pub async fn new() -> Result<Self, String> {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .alpns(vec![EDU_ALPN.to_vec()])
            .bind()
            .await
            .map_err(|e| format!("iroh Endpoint 创建失败: {e}"))?;

        let node_id_str = endpoint.id().to_string();
        let course_code = encode_course_code(&node_id_str);

        tracing::info!("教师 P2P 端点: NodeId={node_id_str}, 课程码={course_code}");

        Ok(Self {
            endpoint,
            node_id_str,
            course_code,
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id_str
    }

    pub fn course_code(&self) -> &str {
        &self.course_code
    }

    /// 启动监听循环（处理学生连接）
    pub async fn listen_loop(&self, db_path: &std::path::Path) {
        tracing::info!("教师 P2P 监听已启动");

        loop {
            match self.endpoint.accept().await {
                Some(incoming) => {
                    let conn = match incoming.await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("P2P 连接失败: {e}");
                            continue;
                        }
                    };

                    let db_path = db_path.to_path_buf();
                    tokio::spawn(async move {
                        if let Err(e) = handle_student_connection(conn, &db_path).await {
                            tracing::warn!("学生连接处理失败: {e}");
                        }
                    });
                }
                None => {
                    tracing::info!("P2P 端点已关闭");
                    break;
                }
            }
        }
    }
}

/// 处理单个学生连接
async fn handle_student_connection(
    conn: iroh::endpoint::Connection,
    db_path: &std::path::Path,
) -> Result<(), String> {
    tracing::info!("学生 P2P 连接进入");

    loop {
        // 接收学生的消息
        let (mut send, mut recv) = conn.accept_bi().await.map_err(|e| format!("accept_bi 失败: {e}"))?;

        let data = recv
            .read_to_end(1024 * 1024)
            .await
            .map_err(|e| format!("读取失败: {e}"))?;

        let Some(msg) = decode_message(&data) else {
            tracing::warn!("无法解析学生消息");
            continue;
        };

        let reply = process_student_message(msg, db_path);
        let reply_bytes = encode_message(&reply);

        use futures_util::SinkExt;
        send.write_all(&reply_bytes)
            .await
            .map_err(|e| format!("回复失败: {e}"))?;
        send.finish().map_err(|e| format!("finish 失败: {e}"))?;
    }
}

/// 处理学生消息，返回回复
fn process_student_message(
    msg: ClassroomMessage,
    db_path: &std::path::Path,
) -> ClassroomMessage {
    match msg {
        ClassroomMessage::AuthRequest { student_no, password } => {
            // 验证学生认证
            let store = match crate::edu::store::EduStore::open(db_path) {
                Ok(s) => s,
                Err(e) => {
                    return ClassroomMessage::AuthResponse {
                        success: false,
                        token: None,
                        student_name: None,
                        courses: vec![],
                        message: format!("数据库错误: {e}"),
                    };
                }
            };

            match crate::edu::auth::authenticate(&store, &student_no, &password) {
                Ok(result) => {
                    // 获取学生选课列表
                    let courses = store
                        .get_student_courses(result.student_id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|c| CourseBrief {
                            course_code: c.course_code,
                            name: c.name,
                        })
                        .collect();

                    ClassroomMessage::AuthResponse {
                        success: true,
                        token: Some(result.token),
                        student_name: Some(result.student_name),
                        courses,
                        message: "认证成功".to_string(),
                    }
                }
                Err(e) => ClassroomMessage::AuthResponse {
                    success: false,
                    token: None,
                    student_name: None,
                    courses: vec![],
                    message: e.to_string(),
                },
            }
        }

        ClassroomMessage::CourseSync { course_code, .. } => {
            // 学生请求课程内容同步
            let store = match crate::edu::store::EduStore::open(db_path) {
                Ok(s) => s,
                Err(_) => return ClassroomMessage::Notification {
                    title: "同步失败".into(),
                    content: "数据库错误".into(),
                    timestamp: now_ts(),
                },
            };

            let course = store.get_course(&course_code).ok().flatten();
            let Some(course) = course else {
                return ClassroomMessage::Notification {
                    title: "同步失败".into(),
                    content: format!("课程 '{course_code}' 不存在"),
                    timestamp: now_ts(),
                };
            };

            let lessons: Vec<LessonBrief> = store
                .get_lessons_v2(course.id)
                .unwrap_or_default()
                .into_iter()
                .map(|l| LessonBrief {
                    lesson_num: l.lesson_num,
                    topic: l.topic,
                })
                .collect();

            let assignments: Vec<AssignmentBrief> = store
                .get_assignments(course.id)
                .unwrap_or_default()
                .into_iter()
                .map(|a| AssignmentBrief {
                    id: a.id,
                    title: a.title,
                    description: a.description,
                    due_date: a.due_date,
                })
                .collect();

            // 获取所有班级的已发布内容（简化：返回课程级全部，学生端按需过滤）
            ClassroomMessage::CourseSync {
                course_code: course.course_code,
                lessons,
                assignments,
                published_lessons: vec![], // 学生端通过班级过滤
                published_assignments: vec![],
            }
        }

        ClassroomMessage::Heartbeat { student_no, student_name, .. } => {
            tracing::debug!("心跳: {student_no} ({student_name})");
            ClassroomMessage::Notification {
                title: "ACK".into(),
                content: "ok".into(),
                timestamp: now_ts(),
            }
        }

        ClassroomMessage::ActivityReport { student_no, course_code, .. } => {
            tracing::info!("活动上报: {student_no} @ {course_code}");
            ClassroomMessage::Notification {
                title: "ACK".into(),
                content: "received".into(),
                timestamp: now_ts(),
            }
        }

        ClassroomMessage::HelpRequest { student_no, question, .. } => {
            tracing::info!("求助: {student_no}: {question}");
            ClassroomMessage::Notification {
                title: "求助已收到".into(),
                content: "教师会尽快回复".into(),
                timestamp: now_ts(),
            }
        }

        ClassroomMessage::SubmitAssignment { student_no, assignment_id, content, .. } => {
            let store = match crate::edu::store::EduStore::open(db_path) {
                Ok(s) => s,
                Err(_) => return ClassroomMessage::Notification {
                    title: "提交失败".into(),
                    content: "数据库错误".into(),
                    timestamp: now_ts(),
                },
            };

            let student = store.get_student(&student_no).ok().flatten();
            let Some(student) = student else {
                return ClassroomMessage::Notification {
                    title: "提交失败".into(),
                    content: "学生不存在".into(),
                    timestamp: now_ts(),
                };
            };

            match store.submit_assignment(assignment_id, student.id, &content, "") {
                Ok(_) => ClassroomMessage::Notification {
                    title: "提交成功".into(),
                    content: "作业已提交".into(),
                    timestamp: now_ts(),
                },
                Err(e) => ClassroomMessage::Notification {
                    title: "提交失败".into(),
                    content: e.to_string(),
                    timestamp: now_ts(),
                },
            }
        }

        // 这些是教师→学生的消息，学生不应发送
        _ => ClassroomMessage::Notification {
            title: "错误".into(),
            content: "不支持的请求类型".into(),
            timestamp: now_ts(),
        },
    }
}

// ---------------------------------------------------------------------------
// 学生端：连接教师 + 认证
// ---------------------------------------------------------------------------

/// 学生端 P2P 连接
pub struct StudentConnection {
    endpoint: iroh::Endpoint,
    /// 教师端地址（连接后设置）
    teacher_addr: Option<iroh::EndpointAddr>,
}

/// 学生认证结果（P2P 方式）
#[derive(Debug)]
pub struct P2pAuthResult {
    pub success: bool,
    pub token: Option<String>,
    pub student_name: Option<String>,
    pub courses: Vec<CourseBrief>,
    pub message: String,
}

impl StudentConnection {
    /// 学生创建端点
    pub async fn new() -> Result<Self, String> {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .bind()
            .await
            .map_err(|e| format!("iroh Endpoint 创建失败: {e}"))?;

        Ok(Self {
            endpoint,
            teacher_addr: None,
        })
    }

    /// 通过完整 NodeID 连接教师
    pub async fn connect_by_node_id(&mut self, node_id_str: &str) -> Result<(), String> {
        tracing::info!("正在连接教师 NodeID: {node_id_str}...");

        let key: iroh::PublicKey = node_id_str
            .parse()
            .map_err(|e| format!("无效的 NodeID: {e}"))?;
        let addr = iroh::EndpointAddr::new(key);

        self.teacher_addr = Some(addr);
        tracing::info!("教师地址已设置");
        Ok(())
    }

    /// 通过 iroh EndpointAddr 连接教师
    pub async fn connect_by_addr(&mut self, addr: iroh::EndpointAddr) -> Result<(), String> {
        self.teacher_addr = Some(addr);
        Ok(())
    }

    /// 发送消息并等待回复
    async fn send_and_receive(&self, msg: ClassroomMessage) -> Result<ClassroomMessage, String> {
        let addr = self
            .teacher_addr
            .as_ref()
            .ok_or_else(|| "未设置教师地址".to_string())?
            .clone();

        let conn = self
            .endpoint
            .connect(addr, EDU_ALPN)
            .await
            .map_err(|e| format!("连接教师失败: {e}"))?;

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| format!("open_bi 失败: {e}"))?;

        let data = encode_message(&msg);
        send.write_all(&data)
            .await
            .map_err(|e| format!("发送失败: {e}"))?;
        send.finish()
            .map_err(|e| format!("finish 失败: {e}"))?;

        let reply_data = recv
            .read_to_end(1024 * 1024)
            .await
            .map_err(|e| format!("读取回复失败: {e}"))?;

        decode_message(&reply_data).ok_or_else(|| "无法解析回复".to_string())
    }

    /// 发送认证请求
    pub async fn authenticate(
        &self,
        student_no: &str,
        password: &str,
    ) -> Result<P2pAuthResult, String> {
        let msg = ClassroomMessage::AuthRequest {
            student_no: student_no.to_string(),
            password: password.to_string(),
        };

        let reply = self.send_and_receive(msg).await?;

        match reply {
            ClassroomMessage::AuthResponse {
                success,
                token,
                student_name,
                courses,
                message,
            } => Ok(P2pAuthResult {
                success,
                token,
                student_name,
                courses,
                message,
            }),
            _ => Err("意外的回复类型".to_string()),
        }
    }

    /// 请求课程内容同步
    pub async fn sync_course(&self, course_code: &str) -> Result<ClassroomMessage, String> {
        let msg = ClassroomMessage::CourseSync {
            course_code: course_code.to_string(),
            lessons: vec![],
            assignments: vec![],
            published_lessons: vec![],
            published_assignments: vec![],
        };
        self.send_and_receive(msg).await
    }

    /// 发送心跳
    pub async fn heartbeat(&self, student_no: &str, student_name: &str) -> Result<(), String> {
        let msg = ClassroomMessage::Heartbeat {
            student_no: student_no.to_string(),
            student_name: student_name.to_string(),
            timestamp: now_ts(),
        };
        let _ = self.send_and_receive(msg).await;
        Ok(())
    }

    /// 提交作业
    pub async fn submit_assignment(
        &self,
        student_no: &str,
        assignment_id: i64,
        content: &str,
    ) -> Result<String, String> {
        let msg = ClassroomMessage::SubmitAssignment {
            student_no: student_no.to_string(),
            assignment_id,
            content: content.to_string(),
            timestamp: now_ts(),
        };
        let reply = self.send_and_receive(msg).await?;
        match reply {
            ClassroomMessage::Notification { content, .. } => Ok(content),
            _ => Err("意外的回复".to_string()),
        }
    }

    /// 发送求助
    pub async fn help_request(&self, student_no: &str, question: &str) -> Result<String, String> {
        let msg = ClassroomMessage::HelpRequest {
            student_no: student_no.to_string(),
            question: question.to_string(),
            timestamp: now_ts(),
        };
        let reply = self.send_and_receive(msg).await?;
        match reply {
            ClassroomMessage::Notification { content, .. } => Ok(content),
            _ => Err("意外的回复".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP 降级连接（当 iroh P2P 不可用时）
// ---------------------------------------------------------------------------

/// HTTP 降级认证
pub async fn http_authenticate(
    teacher_url: &str,
    student_no: &str,
    password: String,
) -> Result<P2pAuthResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(&format!("{teacher_url}/api/edu/auth"))
        .json(&serde_json::json!({
            "student_no": student_no,
            "password": password,
        }))
        .send()
        .await
        .map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析失败: {e}"))?;

    Ok(P2pAuthResult {
        success: json.get("success").and_then(|v| v.as_bool()).unwrap_or(false),
        token: json.get("token").and_then(|v| v.as_str()).map(String::from),
        student_name: json.get("student_name").and_then(|v| v.as_str()).map(String::from),
        courses: json
            .get("courses")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        Some(CourseBrief {
                            course_code: c.get("course_code")?.as_str()?.to_string(),
                            name: c.get("name")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        message: json.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_message() {
        let msg = ClassroomMessage::Heartbeat {
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

    #[test]
    fn test_course_sync_serialization() {
        let msg = ClassroomMessage::CourseSync {
            course_code: "CS101".into(),
            lessons: vec![LessonBrief { lesson_num: 1, topic: "变量".into() }],
            assignments: vec![AssignmentBrief { id: 1, title: "作业1".into(), description: "".into(), due_date: "".into() }],
            published_lessons: vec![1],
            published_assignments: vec![1],
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        match decoded {
            ClassroomMessage::CourseSync { course_code, lessons, assignments, .. } => {
                assert_eq!(course_code, "CS101");
                assert_eq!(lessons.len(), 1);
                assert_eq!(assignments.len(), 1);
            }
            _ => panic!("应该是 CourseSync"),
        }
    }

    #[test]
    fn test_submit_assignment_serialization() {
        let msg = ClassroomMessage::SubmitAssignment {
            student_no: "2024001".into(),
            assignment_id: 1,
            content: "我的答案".into(),
            timestamp: now_ts(),
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        match decoded {
            ClassroomMessage::SubmitAssignment { student_no, assignment_id, content, .. } => {
                assert_eq!(student_no, "2024001");
                assert_eq!(assignment_id, 1);
                assert_eq!(content, "我的答案");
            }
            _ => panic!("应该是 SubmitAssignment"),
        }
    }
}
