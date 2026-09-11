//! 教学通信的三级数据模型：课程 → 教学班 → 成员
//!
//! 对应 `docs/edu-p2p-design.md` §3。三级结构的语义：
//!
//! ```text
//! Course（抽象课程，如「数据结构 CS201」）
//!   └── Section（教学班，真正的授课单元，如「数据结构-信工班」）
//!         └── Member（成员：老师或学生，含 EndpointId + 用户名 + 行政班级）
//! ```
//!
//! 关键设计：**Course 不绑定老师**，`teacher_id` 下移到 `Section`。
//! 这样同一个老师可以为同一门课开多个教学班；不同老师也可以各开各的班。
//!
//! 因决策 1B（规范重构）：`edu_classes` 表即 Section 表 —— 表名保留 `edu_classes`
//! 以免波及既有 SQL，但语义上它就是教学班。本模块的 `Section` 结构体是它的强类型视图。
//!
//! ⚠️ 成员归属以**新增的 `edu_section_members` 表**为准，而不是旧的 `edu_enrollments`：
//! `edu_enrollments` 挂在**课程级**（无 `class_id`），无法判定学生属于该课程的哪个教学班，
//! 因此**故意不做迁移**，避免凭空猜测。旧表保持原样仅作历史存档；教务同步（P4）负责填入正确归属。

use serde::{Deserialize, Serialize};

/// 成员角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberRole {
    Teacher,
    Student,
}

impl MemberRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemberRole::Teacher => "teacher",
            MemberRole::Student => "student",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "teacher" => Some(MemberRole::Teacher),
            "student" => Some(MemberRole::Student),
            _ => None,
        }
    }
}

/// ① 抽象课程：如「数据结构」（CS201）。
///
/// 与授课老师**解耦** —— 同一门课可由不同老师开不同教学班。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Course {
    pub id: i64,
    /// 课程代码，如 "CS201"
    pub code: String,
    /// 课程名，如 "数据结构"
    pub name: String,
    pub description: String,
    /// 课程级默认工具白名单（JSON 数组字符串，Section 可覆盖）
    pub tools_whitelist: String,
    /// 课程级允许的学习模式（JSON 数组字符串）
    pub allowed_modes: String,
}

/// ② 教学班：真正的授课单元，如「数据结构-信工班」（张老师，信工2201+2202）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Section {
    pub id: i64,
    pub course_id: i64,
    /// 任课老师（同一老师可带多个教学班）
    pub teacher_id: i64,
    /// 教学班名，如 "数据结构-信工班"
    pub name: String,
    /// 学期，如 "2026-2027-1"
    pub term: String,
    /// 该教学班独立的 Gossip Topic 种子（32 字节）。
    ///
    /// **视为密钥**：真实 Topic = `derive_topic(seed, topic_epoch)`，仅经认证信道下发。
    pub gossip_seed: Vec<u8>,
    /// Topic 纪元。撤销成员时 +1 → 真实 Topic 变化，旧 Topic 作废（设计文档 §5.4 治本方案）。
    pub topic_epoch: u32,
    pub created_at: String,
}

impl Section {
    /// 真实 Gossip Topic（32 字节）
    pub fn topic(&self) -> [u8; 32] {
        super::gossip::derive_topic(&self.gossip_seed, self.topic_epoch)
    }
}

/// ③ 成员：老师与学生统一建模，挂在 `Section` 上。
///
/// 同一学生加入 N 个教学班 → N 行 Member（`section_id` 不同）。
/// 同一教学班含多个行政班 → 由各行各自的 `admin_class` 表达。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub id: i64,
    pub section_id: i64,
    /// 指向 `edu_teachers.id` 或 `edu_students.id`
    pub user_id: i64,
    pub role: MemberRole,
    /// 学号 / 工号
    pub username: String,
    pub display_name: String,
    /// 行政班级，如「信工2201」（老师为空串）
    pub admin_class: String,
    /// Iroh EndpointId（Ed25519 公钥 hex）。首次认证成功后写入 = 设备绑定。
    pub endpoint_id: Option<String>,
    /// 白名单状态
    pub authorized: bool,
    pub joined_at: String,
    pub last_seen_at: Option<String>,
}

/// 下发给已认证学生的教学班票据。
///
/// 只在 `/class-auth/1.0` 的 **加密信道**内下发；TopicId 本身即能力凭据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SectionTicket {
    pub section_id: i64,
    pub section_name: String,
    pub course_code: String,
    pub course_name: String,
    /// hex 编码的 32 字节 Topic
    pub topic_id: String,
    pub topic_epoch: u32,
    /// 引导节点：老师自身 EndpointId（学生据此接入 swarm）
    pub bootstrap: Vec<String>,
    /// 作业 blob 的分发入口（老师 EndpointId）
    pub provider: String,
    /// 该班独立的 blob 命名空间标识（hex）
    pub blob_namespace: String,
}

/// 学习会话中的身份上下文（`/class-auth/1.0` 成功后由老师端签发）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionIdentity {
    pub username: String,
    pub display_name: String,
    pub admin_class: String,
    pub role: MemberRole,
    pub token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_roundtrip() {
        assert_eq!(MemberRole::parse("teacher"), Some(MemberRole::Teacher));
        assert_eq!(MemberRole::parse("student"), Some(MemberRole::Student));
        assert_eq!(MemberRole::parse("boss"), None);
        assert_eq!(MemberRole::Teacher.as_str(), "teacher");
    }

    #[test]
    fn test_section_topic_is_deterministic_and_epoch_sensitive() {
        let sec = Section {
            id: 1,
            course_id: 1,
            teacher_id: 1,
            name: "数据结构-信工班".into(),
            term: "2026-1".into(),
            gossip_seed: vec![7u8; 32],
            topic_epoch: 0,
            created_at: String::new(),
        };
        let t0a = sec.topic();
        let t0b = sec.topic();
        assert_eq!(t0a, t0b, "同 seed + 同 epoch 必须得到同一 Topic");

        let mut sec2 = sec.clone();
        sec2.topic_epoch = 1;
        assert_ne!(t0a, sec2.topic(), "epoch 变化必须导致 Topic 变化（撤销语义）");
    }

    #[test]
    fn test_member_serde() {
        let m = Member {
            id: 1,
            section_id: 10,
            user_id: 2,
            role: MemberRole::Student,
            username: "2024001".into(),
            display_name: "张三".into(),
            admin_class: "信工2201".into(),
            endpoint_id: Some("ab".repeat(32)),
            authorized: true,
            joined_at: "2026-09-11T00:00:00Z".into(),
            last_seen_at: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: Member = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }
}
