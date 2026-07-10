//! 课程切换 + 学习模式系统
//!
//! 管理课程上下文（CourseProfile），支持 /sw 命令切换课程。
//! 三种学习模式：explore（探索）/ scaffold（引导）/ locked（考试）。

use crate::edu::store::{Course, EduStore, EduError};

/// 学习模式
#[derive(Debug, Clone, PartialEq)]
pub enum LearnMode {
    /// 探索模式：完整 AI，自由提问
    Explore,
    /// 引导模式：苏格拉底式追问，不直接给答案
    Scaffold,
    /// 考试模式：只读工具（read_file/search/glob/get_time）
    Locked,
}

impl LearnMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "scaffold" | "引导" => LearnMode::Scaffold,
            "locked" | "locked" | "考试" => LearnMode::Locked,
            _ => LearnMode::Explore,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LearnMode::Explore => "explore",
            LearnMode::Scaffold => "scaffold",
            LearnMode::Locked => "locked",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            LearnMode::Explore => "🔬",
            LearnMode::Scaffold => "🧭",
            LearnMode::Locked => "🔒",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            LearnMode::Explore => "探索模式：完整 AI，自由提问",
            LearnMode::Scaffold => "引导模式：苏格拉底式追问，不直接给答案",
            LearnMode::Locked => "考试模式：只读工具",
        }
    }
}

/// scaffold 模式的苏格拉底式 System Prompt
pub const SCAFFOLD_PROMPT: &str = r#"你是一位苏格拉底式教学助手。你的目标不是给学生答案，而是通过追问引导他们自己思考。

严格规则：
1. 永远不要直接给出完整的解决方案
2. 先问学生“你觉得应该从哪里开始？”
3. 如果学生卡住，给出方向性提示而非答案
4. 鼓励学生解释他们的思考过程
5. 在关键处追问“你确定吗？你验证过吗？”
6. 如果学生坚持要答案，引导他们分解问题
7. 肯定学生的正确思路，但要他们自己走到结论
"#;

/// 课程上下文（决定该课程的 system_prompt、工具、模式）
#[derive(Debug, Clone)]
pub struct CourseProfile {
    pub course_code: String,
    pub course_name: String,
    pub lesson_num: Option<i64>,
    pub mode: LearnMode,
    pub allowed_tools: Vec<String>,
    pub system_prompt_override: Option<String>,
}

impl CourseProfile {
    /// 根据课程配置和学习模式构建
    pub fn from_course(course: &Course, mode: LearnMode, lesson_num: Option<i64>) -> Self {
        // 解析工具白名单
        let allowed_tools: Vec<String> = serde_json::from_str(&course.tools_whitelist)
            .unwrap_or_default();

        // 按模式决定 system prompt
        let system_prompt_override = match mode {
            LearnMode::Scaffold => Some(SCAFFOLD_PROMPT.to_string()),
            LearnMode::Locked => Some(
                "你是一个只读查阅助手。你只能帮助用户查阅和理解信息，不能给出完整的解决方案或代码实现。".to_string(),
            ),
            LearnMode::Explore => None,
        };

        // locked 模式只保留只读工具
        let allowed_tools = if mode == LearnMode::Locked {
            allowed_tools
                .into_iter()
                .filter(|t| matches!(t.as_str(), "read_file" | "search_content" | "glob" | "get_current_time" | "read_pdf" | "read_excel" | "read_docx" | "read_pptx"))
                .collect()
        } else if allowed_tools.is_empty() {
            // 空白名单 = 全部允许
            vec![]
        } else {
            allowed_tools
        };

        Self {
            course_code: course.course_code.clone(),
            course_name: course.name.clone(),
            lesson_num,
            mode,
            allowed_tools,
            system_prompt_override,
        }
    }

    /// 获取该课程使用的 system prompt
    pub fn system_prompt(&self, default_prompt: &str) -> String {
        self.system_prompt_override
            .clone()
            .unwrap_or_else(|| default_prompt.to_string())
    }

    /// session key 后缀（channel:chat_id 之后的课程标识部分）
    pub fn session_suffix(&self) -> String {
        if let Some(ln) = self.lesson_num {
            format!(":{}#{}", self.course_code, ln)
        } else {
            format!(":{}", self.course_code)
        }
    }

    /// 图标 + 描述
    pub fn status_line(&self) -> String {
        let lesson = self
            .lesson_num
            .map(|n| format!("#{}", n))
            .unwrap_or_default();
        format!(
            "{} {}{} — {}",
            self.mode.icon(),
            self.course_code,
            lesson,
            self.course_name
        )
    }
}

/// /sw 命令解析结果
pub enum SwCommand {
    /// 列出已加入的课程
    List,
    /// 切换课程
    Switch { course_code: String, lesson_num: Option<i64> },
    /// 无效命令
    Invalid(String),
}

/// 解析 /sw 命令
pub fn parse_sw_command(input: &str) -> SwCommand {
    let rest = input.trim_start_matches("/sw").trim();
    if rest.is_empty() {
        return SwCommand::List;
    }

    // 格式: CS101 或 CS101#3
    if let Some((code, num_str)) = rest.split_once('#') {
        let code = code.trim().to_string();
        let num: Option<i64> = num_str.trim().parse().ok();
        SwCommand::Switch {
            course_code: code,
            lesson_num: num,
        }
    } else {
        SwCommand::Switch {
            course_code: rest.trim().to_string(),
            lesson_num: None,
        }
    }
}

/// 构建课程列表的回复文本
pub fn format_course_list(courses: &[Course], current: Option<&str>) -> String {
    if courses.is_empty() {
        return "📚 你还没有加入任何课程。".to_string();
    }
    let mut lines = vec!["📚 已加入的课程:".to_string()];
    for c in courses {
        let marker = if current == Some(c.course_code.as_str()) {
            " ← 当前"
        } else {
            ""
        };
        lines.push(format!("  {} {} {}{}", c.id, c.course_code, c.name, marker));
    }
    lines.push(format!("\n切换: /sw <课程码> 或 /sw <课程码>#<课次>"));
    lines.join("\n")
}

/// 学生当前的课程上下文管理器
pub struct CourseContext {
    /// 当前激活的课程码
    pub current_course: Option<String>,
    /// 当前课次
    pub current_lesson: Option<i64>,
    /// 当前学习模式
    pub current_mode: LearnMode,
}

impl Default for CourseContext {
    fn default() -> Self {
        Self {
            current_course: None,
            current_lesson: None,
            current_mode: LearnMode::Explore,
        }
    }
}

impl CourseContext {
    /// 获取当前的 CourseProfile（需要从 store 加载课程信息）
    pub fn build_profile(&self, course: &Course) -> CourseProfile {
        CourseProfile::from_course(course, self.current_mode.clone(), self.current_lesson)
    }

    /// session key 的课程后缀
    pub fn session_suffix(&self) -> String {
        if let Some(ref code) = self.current_course {
            if let Some(ln) = self.current_lesson {
                format!(":{}#{}", code, ln)
            } else {
                format!(":{}", code)
            }
        } else {
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_learn_mode_from_str() {
        assert_eq!(LearnMode::from_str("explore"), LearnMode::Explore);
        assert_eq!(LearnMode::from_str("scaffold"), LearnMode::Scaffold);
        assert_eq!(LearnMode::from_str("locked"), LearnMode::Locked);
        assert_eq!(LearnMode::from_str("unknown"), LearnMode::Explore);
    }

    #[test]
    fn test_sw_command_parse_list() {
        match parse_sw_command("/sw") {
            SwCommand::List => {}
            _ => panic!("应该是 List"),
        }
    }

    #[test]
    fn test_sw_command_parse_switch() {
        match parse_sw_command("/sw CS101") {
            SwCommand::Switch { course_code, lesson_num } => {
                assert_eq!(course_code, "CS101");
                assert_eq!(lesson_num, None);
            }
            _ => panic!("应该是 Switch"),
        }
    }

    #[test]
    fn test_sw_command_parse_switch_with_lesson() {
        match parse_sw_command("/sw CS101#3") {
            SwCommand::Switch { course_code, lesson_num } => {
                assert_eq!(course_code, "CS101");
                assert_eq!(lesson_num, Some(3));
            }
            _ => panic!("应该是 Switch"),
        }
    }

    #[test]
    fn test_course_profile_explore() {
        let course = Course {
            id: 1,
            course_code: "TS101".into(),
            name: "测试课程".into(),
            teacher_id: 1,
            description: String::new(),
            tools_whitelist: "[]".into(),
            allowed_modes: "[\"explore\",\"scaffold\"]".into(),
            created_at: String::new(),
        };
        let profile = CourseProfile::from_course(&course, LearnMode::Explore, Some(2));
        assert_eq!(profile.course_code, "TS101");
        assert_eq!(profile.lesson_num, Some(2));
        assert!(profile.system_prompt_override.is_none());
        assert!(profile.allowed_tools.is_empty()); // 空=全部允许
    }

    #[test]
    fn test_course_profile_scaffold() {
        let course = Course {
            id: 1,
            course_code: "TS101".into(),
            name: "测试".into(),
            teacher_id: 1,
            description: String::new(),
            tools_whitelist: "[]".into(),
            allowed_modes: "[]".into(),
            created_at: String::new(),
        };
        let profile = CourseProfile::from_course(&course, LearnMode::Scaffold, None);
        assert!(profile.system_prompt_override.is_some());
        let prompt = profile.system_prompt_override.unwrap();
        assert!(prompt.contains("苏格拉底"));
    }

    #[test]
    fn test_course_profile_locked_tools() {
        let course = Course {
            id: 1,
            course_code: "TS101".into(),
            name: "测试".into(),
            teacher_id: 1,
            description: String::new(),
            tools_whitelist: "[\"read_file\",\"write_file\",\"run_command\",\"glob\"]".into(),
            allowed_modes: "[]".into(),
            created_at: String::new(),
        };
        let profile = CourseProfile::from_course(&course, LearnMode::Locked, None);
        // locked 只保留只读工具
        assert!(profile.allowed_tools.contains(&"read_file".to_string()));
        assert!(profile.allowed_tools.contains(&"glob".to_string()));
        assert!(!profile.allowed_tools.contains(&"write_file".to_string()));
        assert!(!profile.allowed_tools.contains(&"run_command".to_string()));
    }

    #[test]
    fn test_session_suffix() {
        let ctx = CourseContext {
            current_course: Some("CS101".into()),
            current_lesson: Some(3),
            ..Default::default()
        };
        assert_eq!(ctx.session_suffix(), ":CS101#3");

        let ctx2 = CourseContext {
            current_course: Some("CS101".into()),
            current_lesson: None,
            ..Default::default()
        };
        assert_eq!(ctx2.session_suffix(), ":CS101");

        let ctx3 = CourseContext::default();
        assert_eq!(ctx3.session_suffix(), "");
    }

    #[test]
    fn test_format_course_list() {
        let courses = vec![
            Course {
                id: 1, course_code: "CS101".into(), name: "Python".into(),
                teacher_id: 1, description: String::new(), tools_whitelist: "[]".into(),
                allowed_modes: "[]".into(), created_at: String::new(),
            },
            Course {
                id: 2, course_code: "CS201".into(), name: "数据结构".into(),
                teacher_id: 1, description: String::new(), tools_whitelist: "[]".into(),
                allowed_modes: "[]".into(), created_at: String::new(),
            },
        ];
        let result = format_course_list(&courses, Some("CS101"));
        assert!(result.contains("CS101"));
        assert!(result.contains("Python"));
        assert!(result.contains("← 当前"));
    }
}
