//! 课程创建向导（步骤状态机）
//!
//! 在所有 channel（TUI/Gateway/Telegram/QQ/Web）中通过 /setup 命令触发。
//! 采用步骤状态机：每一步返回提示文本，用户回复后进入下一步。
//! 用户随时可输入 /setup cancel 取消。

use std::path::Path;

use crate::edu::store::EduStore;

/// 向导步骤
#[derive(Debug, Clone, PartialEq)]
pub enum SetupStep {
    CourseCode,
    CourseName,
    ClassName,
    ClassNameLoop,
    LessonTopic,
    LessonLoop,
    StudentNo,
    StudentName,
    StudentPassword,
    StudentLoop,
    Publish,
    Done,
}

/// 向导状态
#[derive(Debug, Clone)]
pub struct SetupState {
    pub step: SetupStep,
    pub course_code: String,
    pub course_name: String,
    pub course_id: Option<i64>,
    pub classes: Vec<(i64, String)>,
    pub lessons: Vec<(i64, i64, String)>,
    pub lesson_num: i64,
    pub current_student_no: String,
    pub student_count: usize,
}

impl SetupState {
    pub fn new() -> Self {
        Self {
            step: SetupStep::CourseCode,
            course_code: String::new(),
            course_name: String::new(),
            course_id: None,
            classes: Vec::new(),
            lessons: Vec::new(),
            lesson_num: 1,
            current_student_no: String::new(),
            student_count: 0,
        }
    }
}

/// 获取当前步骤的提示文本
pub fn step_prompt(state: &SetupState) -> String {
    match state.step {
        SetupStep::CourseCode => {
            "🎓 课程创建向导（输入 /setup cancel 可随时取消）\n\n\
             【步骤 1/5】创建课程\n\
             请输入课程码（如 CS101）:".to_string()
        }
        SetupStep::CourseName => {
            format!("✅ 课程码: {}\n\n请输入课程名称:", state.course_code)
        }
        SetupStep::ClassName | SetupStep::ClassNameLoop => {
            let count = state.classes.len();
            let prefix = if state.step == SetupStep::ClassName {
                "【步骤 2/5】创建班级\n".to_string()
            } else {
                String::new()
            };
            format!("{prefix}已创建 {} 个班级\n\n请输入班级名称（直接回车结束）:", count)
        }
        SetupStep::LessonTopic | SetupStep::LessonLoop => {
            let prefix = if state.step == SetupStep::LessonTopic {
                "【步骤 3/5】创建课次\n".to_string()
            } else {
                String::new()
            };
            format!("{prefix}已创建 {} 个课次\n\n第{}次课主题（直接回车结束）:", state.lessons.len(), state.lesson_num)
        }
        SetupStep::StudentNo | SetupStep::StudentLoop => {
            let prefix = if state.step == SetupStep::StudentNo {
                "【步骤 4/5】添加学生\n".to_string()
            } else {
                String::new()
            };
            format!("{prefix}已添加 {} 个学生\n\n请输入学号（直接回车结束）:", state.student_count)
        }
        SetupStep::StudentName => {
            format!("✅ 学号: {}\n\n请输入姓名:", state.current_student_no)
        }
        SetupStep::StudentPassword => {
            "请输入密码（直接回车=默认 123456）:".to_string()
        }
        SetupStep::Publish => {
            let class_count = state.classes.len();
            let lesson_count = state.lessons.len();
            if class_count == 0 || lesson_count == 0 {
                return "【步骤 5/5】跳过发布（无班级或无课次）\n".to_string();
            }
            format!(
                "【步骤 5/5】发布课次到班级\n\n\
                 班级: {} 个\n\
                 课次: {} 个\n\n\
                 发布方式:\n  1. 全部发布\n  2. 跳过（稍后手动 /class publish）\n\n\
                 请输入选择（1/2）:",
                class_count, lesson_count
            )
        }
        SetupStep::Done => {
            summary_text(state)
        }
    }
}

/// 处理用户在当前步骤的回复，返回回复文本并推进状态
pub fn handle_step_reply(state: &mut SetupState, input: &str, db_path: &Path) -> String {
    // 空输入处理
    let input = input.trim();

    match state.step {
        SetupStep::CourseCode => {
            if input.is_empty() {
                return "⚠️ 课程码不能为空，请重新输入:".to_string();
            }
            state.course_code = input.to_string();
            state.step = SetupStep::CourseName;
            step_prompt(state)
        }
        SetupStep::CourseName => {
            if input.is_empty() {
                return "⚠️ 课程名称不能为空，请重新输入:".to_string();
            }
            state.course_name = input.to_string();

            // 创建课程
            let store = match EduStore::open(db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ 数据库错误: {e}"),
            };
            match store.create_course(&state.course_code, &state.course_name, 1) {
                Ok(_) => {
                    if let Some(course) = store.get_course(&state.course_code).ok().flatten() {
                        state.course_id = Some(course.id);
                    }
                    state.step = SetupStep::ClassName;
                    format!("✅ 课程已创建: {} {}\n\n{}", state.course_code, state.course_name, step_prompt(state))
                }
                Err(e) => format!("❌ 课程创建失败: {e}\n\n请重新输入课程码（或 /setup cancel 取消）:"),
            }
        }
        SetupStep::ClassName | SetupStep::ClassNameLoop => {
            if input.is_empty() {
                // 结束班级创建
                state.step = SetupStep::LessonTopic;
                return format!("✅ 共创建 {} 个班级\n\n{}", state.classes.len(), step_prompt(state));
            }
            // 创建班级
            let store = match EduStore::open(db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let course_id = state.course_id.unwrap_or(1);
            match store.create_class(input, course_id) {
                Ok(class) => {
                    state.classes.push((class.id, input.to_string()));
                    state.step = SetupStep::ClassNameLoop;
                    format!("✅ 班级已创建: {}\n\n{}", input, step_prompt(state))
                }
                Err(e) => format!("❌ {e}\n\n请重新输入班级名称（或回车结束）:"),
            }
        }
        SetupStep::LessonTopic | SetupStep::LessonLoop => {
            if input.is_empty() {
                // 结束课次创建
                state.step = SetupStep::StudentNo;
                return format!("✅ 共创建 {} 个课次\n\n{}", state.lessons.len(), step_prompt(state));
            }
            // 创建课次
            let store = match EduStore::open(db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let course_id = state.course_id.unwrap_or(1);
            match store.create_lesson_v2(course_id, state.lesson_num, input) {
                Ok(id) => {
                    state.lessons.push((id, state.lesson_num, input.to_string()));
                    state.lesson_num += 1;
                    state.step = SetupStep::LessonLoop;
                    format!("✅ 第{}次课: {}\n\n{}", state.lesson_num - 1, input, step_prompt(state))
                }
                Err(e) => format!("❌ {e}\n\n请重新输入主题:"),
            }
        }
        SetupStep::StudentNo | SetupStep::StudentLoop => {
            if input.is_empty() {
                // 结束学生添加
                state.step = SetupStep::Publish;
                return format!("✅ 共添加 {} 个学生\n\n{}", state.student_count, step_prompt(state));
            }
            state.current_student_no = input.to_string();
            state.step = SetupStep::StudentName;
            step_prompt(state)
        }
        SetupStep::StudentName => {
            if input.is_empty() {
                return "⚠️ 姓名不能为空，请重新输入:".to_string();
            }
            let name = input.to_string();

            // 密码
            state.step = SetupStep::StudentPassword;
            // 临时存储姓名
            state.current_student_no = format!("{}|{}", state.current_student_no, name);
            step_prompt(state)
        }
        SetupStep::StudentPassword => {
            let password = if input.is_empty() { "123456" } else { input };

            // 解析学号|姓名
            let (student_no, student_name) = if let Some((no, name)) = state.current_student_no.split_once('|') {
                (no.to_string(), name.to_string())
            } else {
                (state.current_student_no.clone(), "未知".to_string())
            };
            state.current_student_no = student_no.clone();

            // 创建学生
            let store = match EduStore::open(db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let course_id = state.course_id.unwrap_or(1);
            let class_id = if state.classes.len() == 1 {
                Some(state.classes[0].0)
            } else {
                None // 多班或无班时不自动关联
            };

            match store.create_student(&student_no, &student_name, password, class_id) {
                Ok(_) => {
                    // 自动选课
                    if let Some(student) = store.get_student(&student_no).ok().flatten() {
                        let _ = store.enroll(student.id, course_id);
                    }
                    state.student_count += 1;
                    state.step = SetupStep::StudentLoop;
                    format!("✅ 学生已添加: {} {}\n\n{}", student_no, student_name, step_prompt(state))
                }
                Err(e) => format!("❌ {e}\n\n请重新输入学号（或回车结束）:"),
            }
        }
        SetupStep::Publish => {
            let store = match EduStore::open(db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let course_id = state.course_id.unwrap_or(1);

            match input {
                "1" => {
                    // 全部发布
                    let mut count = 0;
                    for (class_id, _) in &state.classes {
                        for (lesson_id, _, _) in &state.lessons {
                            let _ = store.publish_to_class(*class_id, course_id, "lesson", *lesson_id);
                            count += 1;
                        }
                    }
                    state.step = SetupStep::Done;
                    format!("✅ 已发布 {count} 项内容到 {} 个班级\n\n{}", state.classes.len(), step_prompt(state))
                }
                "2" | _ => {
                    state.step = SetupStep::Done;
                    format!("⏭️ 跳过发布\n\n{}", step_prompt(state))
                }
            }
        }
        SetupStep::Done => {
            summary_text(state)
        }
    }
}

/// 生成汇总文本
fn summary_text(state: &SetupState) -> String {
    let mut result = String::new();
    result.push_str("┌─ 🎉 课程创建完成！ ───────────────────┐\n");
    result.push_str(&format!("│  课程: {} {}\n", state.course_code, state.course_name));
    result.push_str(&format!("│  班级: {} 个\n", state.classes.len()));
    if !state.classes.is_empty() {
        for (_, name) in &state.classes {
            result.push_str(&format!("│    · {name}\n"));
        }
    }
    result.push_str(&format!("│  课次: {} 讲\n", state.lessons.len()));
    if !state.lessons.is_empty() {
        for (_, num, topic) in &state.lessons {
            result.push_str(&format!("│    第{num}次: {topic}\n"));
        }
    }
    result.push_str(&format!("│  学生: {} 人\n", state.student_count));
    result.push_str("│\n");
    result.push_str("│  后续命令:\n");
    result.push_str("│    /lesson list 课程码  — 查看课次\n");
    result.push_str("│    /class status 课程码 班级  — 发布状态\n");
    result.push_str("│    /class publish lesson 课程码 班级 序号  — 发布课次\n");
    result.push_str("└────────────────────────────────────────┘\n");
    result
}

/// TUI 模式的交互式向导（使用 dialoguer）
pub fn run_course_setup(db_path: &Path) -> String {
    use dialoguer::{Confirm, Input, Select, theme::ColorfulTheme};

    let theme = ColorfulTheme::default();
    let mut state = SetupState::new();

    // 课程码
    let course_code: String = Input::with_theme(&theme)
        .with_prompt("课程码（如 CS101）")
        .interact_text()
        .unwrap_or_default();
    if course_code.is_empty() { return "⚠️ 已取消".to_string(); }
    state.course_code = course_code;

    // 课程名
    let course_name: String = Input::with_theme(&theme)
        .with_prompt("课程名称")
        .interact_text()
        .unwrap_or_default();
    if course_name.is_empty() { return "⚠️ 已取消".to_string(); }
    state.course_name = course_name;

    // 创建课程
    let store = match EduStore::open(db_path) {
        Ok(s) => s,
        Err(e) => return format!("❌ {e}"),
    };
    match store.create_course(&state.course_code, &state.course_name, 1) {
        Ok(_) => {
            if let Some(c) = store.get_course(&state.course_code).ok().flatten() {
                state.course_id = Some(c.id);
            }
        }
        Err(e) => return format!("❌ {e}"),
    }

    // 班级
    loop {
        let name: String = Input::with_theme(&theme)
            .with_prompt("班级名称（回车结束）")
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();
        if name.is_empty() { break; }
        if let Ok(cls) = store.create_class(&name, state.course_id.unwrap_or(1)) {
            state.classes.push((cls.id, name));
        }
        if !Confirm::with_theme(&theme).with_prompt("继续创建班级？").default(false).interact().unwrap_or(false) { break; }
    }

    // 课次
    let mut num = 1i64;
    loop {
        let topic: String = Input::with_theme(&theme)
            .with_prompt(format!("第{num}次课主题（回车结束）"))
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();
        if topic.is_empty() { break; }
        if let Ok(id) = store.create_lesson_v2(state.course_id.unwrap_or(1), num, &topic) {
            state.lessons.push((id, num, topic));
        }
        num += 1;
    }

    // 学生
    loop {
        let no: String = Input::with_theme(&theme)
            .with_prompt("学号（回车结束）")
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();
        if no.is_empty() { break; }
        let name: String = Input::with_theme(&theme).with_prompt("姓名").interact_text().unwrap_or_default();
        let pwd: String = Input::with_theme(&theme).with_prompt("密码（回车=123456）").allow_empty(true).interact_text().unwrap_or_default();
        let pwd = if pwd.is_empty() { "123456" } else { &pwd };
        let class_id = if state.classes.len() == 1 { Some(state.classes[0].0) } else { None };
        if store.create_student(&no, &name, pwd, class_id).is_ok() {
            if let Some(s) = store.get_student(&no).ok().flatten() {
                let _ = store.enroll(s.id, state.course_id.unwrap_or(1));
            }
            state.student_count += 1;
        }
        if !Confirm::with_theme(&theme).with_prompt("继续添加学生？").default(false).interact().unwrap_or(false) { break; }
    }

    // 发布
    if !state.classes.is_empty() && !state.lessons.is_empty() {
        let opts = vec!["全部发布", "跳过"];
        let idx = Select::with_theme(&theme).with_prompt("发布方式").items(&opts).default(0).interact().unwrap_or(1);
        if idx == 0 {
            for (cid, _) in &state.classes {
                for (lid, _, _) in &state.lessons {
                    let _ = store.publish_to_class(*cid, state.course_id.unwrap_or(1), "lesson", *lid);
                }
            }
        }
    }

    state.step = SetupStep::Done;
    summary_text(&state)
}

/// 非交互模式引导文本（保留兼容）
pub fn setup_guide_text() -> String {
    "🎓 课程创建向导\n\n\
     输入 /setup 开始交互式向导（在所有通道中可用）\n\n\
     或手动执行:\n  /course create CS101 Python\n  /class create CS101 计算机2301\n  /lesson create CS101 1 变量\n  /student add 2024001 张三 CS101 计算机2301".to_string()
}
