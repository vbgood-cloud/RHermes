//! 课程创建向导
//!
//! 交互式引导教师完成课程/班级/课次/学生/发布的完整流程。
//! 在所有 channel（TUI/Gateway/Telegram/QQ/Web）中通过 /setup 命令触发。
//!
//! 由于 channel 环境差异（TUI 有终端交互，Gateway/Telegram 只有文本），
//! 向导采用"步骤式文本交互"：
//! - TUI: dialoguer 交互输入
//! - Gateway/Telegram: 返回第一步提示，用户回复后继续下一步（通过状态机）

use std::path::Path;

use crate::edu::store::EduStore;

/// 运行课程创建向导（TUI 模式，使用 dialoguer 交互）
pub fn run_course_setup(db_path: &Path) -> String {
    use dialoguer::{Confirm, Input, Select, theme::ColorfulTheme};

    let theme = ColorfulTheme::default();
    let mut result = String::new();

    result.push_str("┌─ 🎓 课程创建向导 ─────────────────────┐\n\n");

    // 打开数据库
    let store = match EduStore::open(db_path) {
        Ok(s) => s,
        Err(e) => return format!("❌ 数据库打开失败: {e}"),
    };

    // 1. 创建课程
    result.push_str("【步骤 1/5】创建课程\n");
    let course_code: String = Input::with_theme(&theme)
        .with_prompt("课程码（如 CS101）")
        .interact_text()
        .unwrap_or_default();
    let course_name: String = Input::with_theme(&theme)
        .with_prompt("课程名称")
        .interact_text()
        .unwrap_or_default();

    if course_code.is_empty() || course_name.is_empty() {
        result.push_str("⚠️ 课程码和名称不能为空，向导已取消。\n");
        return result;
    }

    match store.create_course(&course_code, &course_name, 1) {
        Ok(_) => result.push_str(&format!("✅ 课程已创建: {course_code} {course_name}\n\n")),
        Err(e) => {
            result.push_str(&format!("❌ 课程创建失败: {e}\n"));
            return result;
        }
    }

    // 获取课程 ID
    let course = store.get_course(&course_code).ok().flatten();
    let Some(course) = course else {
        result.push_str("❌ 课程创建后查询失败\n");
        return result;
    };

    // 2. 创建班级（可多个）
    result.push_str("【步骤 2/5】创建班级\n");
    let mut classes = Vec::new();
    loop {
        let class_name: String = Input::with_theme(&theme)
            .with_prompt("班级名称（直接回车结束）")
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();

        if class_name.is_empty() {
            break;
        }

        match store.create_class(&class_name, course.id) {
            Ok(class) => {
                result.push_str(&format!("✅ 班级已创建: {class_name}\n"));
                classes.push((class.id, class_name.clone()));
            }
            Err(e) => result.push_str(&format!("❌ 班级创建失败: {e}\n")),
        }

        let more = Confirm::with_theme(&theme)
            .with_prompt("再创建一个班级？")
            .default(false)
            .interact()
            .unwrap_or(false);
        if !more {
            break;
        }
    }
    result.push_str("\n");

    // 3. 创建课次（可多个）
    result.push_str("【步骤 3/5】创建课次\n");
    let mut lessons = Vec::new();
    let mut lesson_num = 1i64;
    loop {
        let topic: String = Input::with_theme(&theme)
            .with_prompt(format!("第{lesson_num}次课主题（直接回车结束）"))
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();

        if topic.is_empty() {
            break;
        }

        match store.create_lesson_v2(course.id, lesson_num, &topic) {
            Ok(id) => {
                result.push_str(&format!("✅ 第{lesson_num}次课: {topic}\n"));
                lessons.push((id, lesson_num, topic));
            }
            Err(e) => result.push_str(&format!("❌ 课次创建失败: {e}\n")),
        }

        lesson_num += 1;
    }
    result.push_str("\n");

    // 4. 添加学生（可多个）
    result.push_str("【步骤 4/5】添加学生\n");
    let mut student_count = 0;
    loop {
        let student_no: String = Input::with_theme(&theme)
            .with_prompt("学号（直接回车结束）")
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();

        if student_no.is_empty() {
            break;
        }

        let student_name: String = Input::with_theme(&theme)
            .with_prompt("姓名")
            .interact_text()
            .unwrap_or_default();

        // 选择班级
        let class_id = if classes.len() == 1 {
            Some(classes[0].0)
        } else if classes.is_empty() {
            None
        } else {
            let class_names: Vec<&str> = classes.iter().map(|(_, n)| n.as_str()).collect();
            let idx = Select::with_theme(&theme)
                .with_prompt("选择班级")
                .items(&class_names)
                .default(0)
                .interact()
                .unwrap_or(0);
            Some(classes[idx].0)
        };

        // 密码
        let password: String = Input::with_theme(&theme)
            .with_prompt("密码（默认 123456）")
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();
        let password = if password.is_empty() { "123456" } else { &password };

        match store.create_student(&student_no, &student_name, password, class_id) {
            Ok(_) => {
                // 自动选课
                let _ = store.enroll(
                    store.get_student(&student_no).ok().flatten().map(|s| s.id).unwrap_or(0),
                    course.id,
                );
                result.push_str(&format!("✅ 学生已添加: {student_no} {student_name}\n"));
                student_count += 1;
            }
            Err(e) => result.push_str(&format!("❌ 学生添加失败: {e}\n")),
        }

        let more = Confirm::with_theme(&theme)
            .with_prompt("再添加一个学生？")
            .default(false)
            .interact()
            .unwrap_or(false);
        if !more {
            break;
        }
    }
    result.push_str("\n");

    // 5. 发布课次到班级
    if !classes.is_empty() && !lessons.is_empty() {
        result.push_str("【步骤 5/5】发布课次到班级\n");

        let publish_options = vec![
            "全部发布（所有课次+作业到所有班级）",
            "逐班发布（选择班级后逐个发布）",
            "跳过（稍后手动 /class publish）",
        ];
        let pub_idx = Select::with_theme(&theme)
            .with_prompt("发布方式")
            .items(&publish_options)
            .default(0)
            .interact()
            .unwrap_or(2);

        match pub_idx {
            0 => {
                // 全部发布
                for (class_id, class_name) in &classes {
                    for (lesson_id, lesson_num, _) in &lessons {
                        let _ = store.publish_to_class(*class_id, course.id, "lesson", *lesson_id);
                    }
                    result.push_str(&format!("✅ 已发布全部课次到 {class_name}\n"));
                }
            }
            1 => {
                // 逐班发布
                for (class_id, class_name) in &classes {
                    let confirm = Confirm::with_theme(&theme)
                        .with_prompt(format!("发布到 {class_name}？"))
                        .default(true)
                        .interact()
                        .unwrap_or(false);
                    if confirm {
                        for (lesson_id, _, _) in &lessons {
                            let _ = store.publish_to_class(*class_id, course.id, "lesson", *lesson_id);
                        }
                        result.push_str(&format!("✅ 已发布到 {class_name}\n"));
                    }
                }
            }
            _ => {
                result.push_str("⏭️ 跳过发布，稍后可用 /class publish 手动发布\n");
            }
        }
    }
    result.push_str("\n");

    // 汇总
    result.push_str("┌─ 🎉 课程创建完成！ ───────────────────┐\n");
    result.push_str(&format!("│  课程: {course_code} {course_name}\n"));
    result.push_str(&format!("│  班级: {} 个\n", classes.len()));
    result.push_str(&format!("│  课次: {} 讲\n", lessons.len()));
    result.push_str(&format!("│  学生: {} 人\n", student_count));
    if !classes.is_empty() {
        result.push_str(&format!("│  课程码: {}\n", course_code));
    }
    result.push_str("│\n");
    result.push_str("│  可用命令:\n");
    result.push_str("│    /lesson list 课程码  — 查看课次\n");
    result.push_str("│    /class status 课程码 班级  — 查看发布状态\n");
    result.push_str("│    /class publish lesson 课程码 班级 序号  — 发布课次\n");
    result.push_str("└────────────────────────────────────────┘\n");

    result
}

/// 非交互模式下的向导提示（Gateway/Telegram/QQ 等无终端环境）
///
/// 返回帮助文本，引导教师使用斜杠命令逐步创建
pub fn setup_guide_text() -> String {
    "🎓 课程创建向导（非交互模式）\n\n\
     请按顺序执行以下命令：\n\n\
     1️⃣ 创建课程:\n   /course create CS101 Python编程基础\n\n\
     2️⃣ 创建班级:\n   /class create CS101 计算机2301\n\n\
     3️⃣ 创建课次:\n   /lesson create CS101 1 变量与数据类型\n   /lesson create CS101 2 条件语句\n\n\
     4️⃣ 添加学生:\n   /student add 2024001 张三 CS101 计算机2301\n\n\
     5️⃣ 发布课次:\n   /class publish lesson CS101 计算机2301 1\n   /class publish all CS101 计算机2301\n\n\
     💡 在 TUI 模式下输入 /setup 可使用交互式向导".to_string()
}
