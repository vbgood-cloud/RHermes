//! RHermes 教育版模块
//!
//! 教育模式入口，包含学生版和教师版功能。
//! 通过 `rhermes edu student` / `rhermes edu teacher` 启动。
//!
//! 通用模式（不带 edu 子命令）完全不受影响。

pub mod auth;
pub mod course;
pub mod dashboard;
pub mod e2e_tests;
pub mod p2p;
pub mod reflection;
pub mod store;
pub mod teacher;

use std::path::Path;

/// 处理 edu 子命令
pub async fn handle_edu(command: &str, args: &[String], config_path: &Path) {
    let db_path = config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("home/edu.db");

    match command {
        "student" => {
            println!("🎓 启动学生模式...");
            println!("   数据库: {}", db_path.display());
            // 如果有子命令（如 auth），走对应流程
            if let Some(sub) = args.first() {
                match sub.as_str() {
                    "auth" => {
                        auth::handle_auth_command(&args[1..], &db_path);
                    }
                    "login" => {
                        auth::handle_auth_command(&["login".to_string()], &db_path);
                    }
                    _ => {
                        println!("   未知子命令: {sub}");
                    }
                }
            } else {
                // 默认走交互式认证
                println!("   请先认证...");
                match auth::interactive_auth(&db_path) {
                    Ok(result) => {
                        println!("✅ 欢迎回来, {}!", result.student_name);
                        println!("   （学生主界面开发中 — Phase 4+）");
                    }
                    Err(e) => {
                        eprintln!("❌ 认证失败: {e}");
                    }
                }
            }
        }
        "teacher" => {
            // 特殊处理 dashboard 子命令
            if let Some(sub) = args.first() {
                if sub == "dashboard" {
                    println!("📊 启动教师仪表板...");
                    let dashboard = dashboard::TeacherDashboard::new(8080, &db_path);
                    dashboard.run().await;
                    return;
                }
            }
            teacher::handle_teacher_command(args, &db_path);
        }
        "join" => {
            let code = args.first().cloned().unwrap_or_default();
            println!("🔗 加入课程: {code}");
            println!("   （Phase 4+ 实现）");
        }
        "status" => {
            println!("📊 学习状态");
            println!("   （Phase 4+ 实现）");
        }
        "auth" => {
            auth::handle_auth_command(args, &db_path);
        }
        _ => {
            eprintln!("未知的教育子命令: {command}");
            println!();
            println!("可用命令:");
            println!("  rhermes edu student [auth|login]  学生模式");
            println!("  rhermes edu teacher <init|course|class|lesson|student|list>  教师管理");
            println!("  rhermes edu auth <login|verify>   认证");
            println!("  rhermes edu join <课程码>          加入课程");
            println!("  rhermes edu status                 学习状态");
        }
    }
}

/// 处理教育模式斜杠命令（TUI 和 Gateway 共用）
///
/// 返回回复文本（空字符串=静默成功）。
/// 根据 config.edu.role 自动判断教师/学生命令。
pub fn handle_slash_command(input: &str, config_path: &Path) -> String {
    let config = crate::core::Config::load(config_path).unwrap_or_default();
    let role = config.edu.role.as_str();
    let db_path = config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("home/edu.db");

    let parts: Vec<&str> = input.trim().splitn(2, char::is_whitespace).collect();
    let cmd = parts[0];
    let args_str = parts.get(1).copied().unwrap_or("");
    let args: Vec<&str> = args_str.split_whitespace().collect();

    match cmd {
        // ===== 教师命令 =====
        "/course" if role == "teacher" => {
            let mgr = match teacher::TeacherManager::new(&db_path) {
                Ok(m) => m,
                Err(e) => return format!("❌ {e}"),
            };
            let action = args.get(0).copied().unwrap_or("");
            match action {
                "create" => {
                    let code = args.get(1).copied().unwrap_or("");
                    let name = args.get(2).copied().unwrap_or("");
                    if code.is_empty() || name.is_empty() {
                        return "用法: /course create <课程码> <课程名>".to_string();
                    }
                    match mgr.create_course(1, code, name) {
                        Ok(_) => String::new(),
                        Err(e) => format!("❌ {e}"),
                    }
                }
                "list" | "" => {
                    match mgr.list_courses(1) {
                        Ok(courses) => {
                            if courses.is_empty() {
                                "（无课程）".to_string()
                            } else {
                                let mut buf = "📚 课程列表:\n".to_string();
                                for c in &courses {
                                    buf.push_str(&format!("  {} {}\n", c.course_code, c.name));
                                }
                                buf
                            }
                        }
                        Err(e) => format!("❌ {e}"),
                    }
                }
                _ => "用法: /course <create <码> <名> | list>".to_string(),
            }
        }
        "/class" if role == "teacher" => {
            let mgr = match teacher::TeacherManager::new(&db_path) {
                Ok(m) => m,
                Err(e) => return format!("❌ {e}"),
            };
            let code = args.get(1).copied().unwrap_or("");
            let name = args.get(2).copied().unwrap_or("");
            if code.is_empty() || name.is_empty() {
                return "用法: /class create <课程码> <班级名>".to_string();
            }
            match mgr.create_class(code, name) {
                Ok(_) => String::new(),
                Err(e) => format!("❌ {e}"),
            }
        }
        "/lesson" if role == "teacher" => {
            let mgr = match teacher::TeacherManager::new(&db_path) {
                Ok(m) => m,
                Err(e) => return format!("❌ {e}"),
            };
            let code = args.get(1).copied().unwrap_or("");
            let class_name = args.get(2).copied().unwrap_or("");
            let num_str = args.get(3).copied().unwrap_or("");
            let topic = args.get(4).copied().unwrap_or("");
            if code.is_empty() || class_name.is_empty() || num_str.is_empty() {
                return "用法: /lesson create <课程码> <班级> <序号> <主题>".to_string();
            }
            let num: i64 = num_str.parse().unwrap_or(1);
            match mgr.create_lesson(code, class_name, num, topic) {
                Ok(_) => String::new(),
                Err(e) => format!("❌ {e}"),
            }
        }
        "/student" if role == "teacher" => {
            let mgr = match teacher::TeacherManager::new(&db_path) {
                Ok(m) => m,
                Err(e) => return format!("❌ {e}"),
            };
            let action = args.get(0).copied().unwrap_or("");
            match action {
                "add" => {
                    let no = args.get(1).copied().unwrap_or("");
                    let name = args.get(2).copied().unwrap_or("");
                    let course = args.get(3).copied().unwrap_or("");
                    let class_name = args.get(4).copied().unwrap_or("");
                    if no.is_empty() || name.is_empty() || course.is_empty() || class_name.is_empty() {
                        return "用法: /student add <学号> <姓名> <课程码> <班级名>".to_string();
                    }
                    match mgr.add_student(no, name, "123456", Some(class_name), Some(course)) {
                        Ok(_) => String::new(),
                        Err(e) => format!("❌ {e}"),
                    }
                }
                _ => "用法: /student add <学号> <姓名> <课程码> <班级名>".to_string(),
            }
        }
        "/roster" if role == "teacher" => {
            let mgr = match teacher::TeacherManager::new(&db_path) {
                Ok(m) => m,
                Err(e) => return format!("❌ {e}"),
            };
            let code = args.get(0).copied().unwrap_or("");
            if code.is_empty() {
                return "用法: /roster <课程码>".to_string();
            }
            // 收集 list_roster 的输出（它用 println）
            // 简化版：直接查数据库
            match mgr.store.get_course(code) {
                Ok(Some(course)) => {
                    let classes = mgr.store.get_classes_by_course(course.id).unwrap_or_default();
                    let mut buf = format!("📋 {} {}\n", course.course_code, course.name);
                    for class in &classes {
                        buf.push_str(&format!("\n  班级: {}\n", class.name));
                        let lessons = mgr.store.get_lessons(course.id, class.id).unwrap_or_default();
                        for l in &lessons {
                            buf.push_str(&format!("    第{}次: {}\n", l.lesson_num, l.topic));
                        }
                    }
                    buf
                }
                Ok(None) => format!("课程 '{code}' 不存在"),
                Err(e) => format!("❌ {e}"),
            }
        }

        // ===== 学生命令 =====
        "/courses" if role == "student" => {
            let store = match store::EduStore::open(&db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let courses = store.list_courses_by_teacher(1).unwrap_or_default();
            if courses.is_empty() {
                "📚 暂无可用课程".to_string()
            } else {
                let mut buf = "📚 可用课程:\n".to_string();
                for c in &courses {
                    buf.push_str(&format!("  {} {}\n", c.course_code, c.name));
                }
                buf.push_str("\n用 /sw <课程码> 切换课程");
                buf
            }
        }
        "/auth" if role == "student" => {
            let action = args.get(0).copied().unwrap_or("");
            match action {
                "login" => {
                    let no = args.get(1).copied().unwrap_or("");
                    let pwd = args.get(2).copied().unwrap_or("");
                    if no.is_empty() || pwd.is_empty() {
                        return "用法: /auth login <学号> <密码>".to_string();
                    }
                    let store = match store::EduStore::open(&db_path) {
                        Ok(s) => s,
                        Err(e) => return format!("❌ {e}"),
                    };
                    match auth::authenticate(&store, no, pwd) {
                        Ok(result) => {
                            // 保存 token 到 config
                            let mut cfg = crate::core::Config::load(config_path).unwrap_or_default();
                            cfg.edu.auth_token = result.token;
                            let _ = cfg.save(config_path);
                            format!("✅ 认证成功！欢迎, {}", result.student_name)
                        }
                        Err(e) => format!("❌ {e}"),
                    }
                }
                _ => "用法: /auth login <学号> <密码>".to_string(),
            }
        }
        "/mode" if role == "student" => {
            let mode = args.get(0).copied().unwrap_or("");
            if mode.is_empty() {
                "当前模式: explore\n可切换: /mode <explore|scaffold>".to_string()
            } else {
                format!("✅ 学习模式已切换为: {mode}")
            }
        }
        "/profile" if role == "student" => {
            "📊 学习档案\n   （需要先认证 — 输入 /auth login <学号> <密码>）".to_string()
        }
        "/report" if role == "student" => {
            "📝 成长报告\n   （需要先认证 — 输入 /auth login <学号> <密码>）".to_string()
        }

        // ===== 教师制作模式命令 =====
        "/lesson" if role == "teacher" => {
            let store = match store::EduStore::open(&db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let action = args.get(0).copied().unwrap_or("");
            match action {
                "create" => {
                    let course = args.get(1).copied().unwrap_or("");
                    let num_str = args.get(2).copied().unwrap_or("");
                    let topic = args.get(3).copied().unwrap_or("");
                    if course.is_empty() || num_str.is_empty() {
                        return "用法: /lesson create <课程码> <序号> <主题>".to_string();
                    }
                    let c = store.get_course(course).ok().flatten();
                    let Some(c) = c else { return format!("❌ 课程 '{course}' 不存在") };
                    let num: i64 = num_str.parse().unwrap_or(1);
                    match store.create_lesson_v2(c.id, num, topic) {
                        Ok(_) => format!("✅ 课次创建: {course} 第{num}次 {topic}"),
                        Err(e) => format!("❌ {e}"),
                    }
                }
                "edit" => {
                    let course = args.get(1).copied().unwrap_or("");
                    let num_str = args.get(2).copied().unwrap_or("");
                    let topic = args.get(3).copied().unwrap_or("");
                    if course.is_empty() || num_str.is_empty() {
                        return "用法: /lesson edit <课程码> <序号> <新主题>".to_string();
                    }
                    let c = store.get_course(course).ok().flatten();
                    let Some(c) = c else { return format!("❌ 课程 '{course}' 不存在") };
                    let num: i64 = num_str.parse().unwrap_or(1);
                    match store.update_lesson_topic(c.id, num, topic) {
                        Ok(_) => format!("✅ 课次已修改（所有班级自动同步）: {course} 第{num}次 → {topic}"),
                        Err(e) => format!("❌ {e}"),
                    }
                }
                "list" | "" => {
                    let course = args.get(1).copied().unwrap_or("");
                    let c = store.get_course(course).ok().flatten();
                    let Some(c) = c else { return format!("用法: /lesson list <课程码>") };
                    let lessons = store.get_lessons_v2(c.id).unwrap_or_default();
                    if lessons.is_empty() { return "（无课次）".to_string(); }
                    let mut buf = format!("📋 {} 课次列表:\n", c.course_code);
                    for l in &lessons {
                        buf.push_str(&format!("  第{}次: {}\n", l.lesson_num, l.topic));
                    }
                    buf
                }
                _ => "用法: /lesson <create|edit|list> ...".to_string(),
            }
        }
        "/assignment" if role == "teacher" => {
            let store = match store::EduStore::open(&db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let action = args.get(0).copied().unwrap_or("");
            match action {
                "create" => {
                    let course = args.get(1).copied().unwrap_or("");
                    let title = args.get(2).copied().unwrap_or("");
                    let desc = args.get(3).copied().unwrap_or("");
                    if course.is_empty() || title.is_empty() {
                        return "用法: /assignment create <课程码> <标题> [描述]".to_string();
                    }
                    let c = store.get_course(course).ok().flatten();
                    let Some(c) = c else { return format!("❌ 课程 '{course}' 不存在") };
                    match store.create_assignment(c.id, title, desc) {
                        Ok(id) => format!("✅ 作业创建: {title} (ID:{id})"),
                        Err(e) => format!("❌ {e}"),
                    }
                }
                "list" | "" => {
                    let course = args.get(1).copied().unwrap_or("");
                    let c = store.get_course(course).ok().flatten();
                    let Some(c) = c else { return format!("用法: /assignment list <课程码>") };
                    let assignments = store.get_assignments(c.id).unwrap_or_default();
                    if assignments.is_empty() { return "（无作业）".to_string(); }
                    let mut buf = format!("📋 {} 作业列表:\n", c.course_code);
                    for a in &assignments {
                        buf.push_str(&format!("  [{}] {} — {}\n", a.id, a.title, a.description));
                    }
                    buf
                }
                "set" => {
                    let id_str = args.get(1).copied().unwrap_or("");
                    let field = args.get(2).copied().unwrap_or("");
                    let value = args.get(3).copied().unwrap_or("");
                    let id: i64 = id_str.parse().unwrap_or(0);
                    if id == 0 || field.is_empty() { return "用法: /assignment set <id> due <日期>".to_string(); }
                    match field {
                        "due" => match store.update_assignment(id, None, None, Some(value)) {
                            Ok(_) => format!("✅ 截止日期已设: {value}"),
                            Err(e) => format!("❌ {e}"),
                        },
                        _ => "支持: /assignment set <id> due <日期>".to_string(),
                    }
                }
                _ => "用法: /assignment <create|list|set> ...".to_string(),
            }
        }

        // ===== 教师上课模式命令 =====
        "/class" if role == "teacher" => {
            let store = match store::EduStore::open(&db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let action = args.get(0).copied().unwrap_or("");
            match action {
                "publish" => {
                    let ptype = args.get(1).copied().unwrap_or("");
                    let course = args.get(2).copied().unwrap_or("");
                    let class_name = args.get(3).copied().unwrap_or("");
                    let target = args.get(4).copied().unwrap_or(""); // lesson 序号 或 assignment id 或 "all"/"upto"
                    let target2 = args.get(5).copied().unwrap_or("");
                    if course.is_empty() || class_name.is_empty() {
                        return "用法: /class publish <lesson|assignment|all|upto> <课程码> <班级> [序号/id]".to_string();
                    }
                    let c = store.get_course(course).ok().flatten();
                    let Some(c) = c else { return format!("❌ 课程 '{course}' 不存在") };
                    let classes = store.get_classes_by_course(c.id).unwrap_or_default();
                    let cls = classes.iter().find(|cl| cl.name == class_name);
                    let Some(cls) = cls else { return format!("❌ 班级 '{class_name}' 不存在") };

                    match ptype {
                        "lesson" => {
                            let lesson_num: i64 = target.parse().unwrap_or(0);
                            let lessons = store.get_lessons_v2(c.id).unwrap_or_default();
                            let lesson = lessons.iter().find(|l| l.lesson_num == lesson_num);
                            let Some(lesson) = lesson else { return format!("❌ 课次 {lesson_num} 不存在") };
                            store.publish_to_class(cls.id, c.id, "lesson", lesson.id).unwrap();
                            format!("✅ 已发布: {course} {class_name} 第{lesson_num}次")
                        }
                        "assignment" => {
                            let aid: i64 = target.parse().unwrap_or(0);
                            store.publish_to_class(cls.id, c.id, "assignment", aid).unwrap();
                            format!("✅ 已发布: {course} {class_name} 作业{aid}")
                        }
                        "all" => {
                            let lessons = store.get_lessons_v2(c.id).unwrap_or_default();
                            let assignments = store.get_assignments(c.id).unwrap_or_default();
                            let mut count = 0;
                            for l in &lessons {
                                store.publish_to_class(cls.id, c.id, "lesson", l.id).unwrap();
                                count += 1;
                            }
                            for a in &assignments {
                                store.publish_to_class(cls.id, c.id, "assignment", a.id).unwrap();
                                count += 1;
                            }
                            format!("✅ 已发布全部 {count} 项内容到 {class_name}")
                        }
                        "upto" => {
                            let upto: i64 = target.parse().unwrap_or(0);
                            let lessons = store.get_lessons_v2(c.id).unwrap_or_default();
                            let mut count = 0;
                            for l in &lessons {
                                if l.lesson_num <= upto {
                                    store.publish_to_class(cls.id, c.id, "lesson", l.id).unwrap();
                                    count += 1;
                                }
                            }
                            format!("✅ 已发布前 {upto} 次课 ({count} 项) 到 {class_name}")
                        }
                        _ => "用法: /class publish <lesson|assignment|all|upto> <课程码> <班级> [序号/id]".to_string(),
                    }
                }
                "status" => {
                    let course = args.get(1).copied().unwrap_or("");
                    let class_name = args.get(2).copied().unwrap_or("");
                    if course.is_empty() || class_name.is_empty() {
                        return "用法: /class status <课程码> <班级>".to_string();
                    }
                    let c = store.get_course(course).ok().flatten();
                    let Some(c) = c else { return format!("❌ 课程 '{course}' 不存在") };
                    let classes = store.get_classes_by_course(c.id).unwrap_or_default();
                    let cls = classes.iter().find(|cl| cl.name == class_name);
                    let Some(cls) = cls else { return format!("❌ 班级 '{class_name}' 不存在") };

                    let all_lessons = store.get_lessons_v2(c.id).unwrap_or_default();
                    let all_assignments = store.get_assignments(c.id).unwrap_or_default();
                    let published_lessons = store.get_published_lessons(cls.id, c.id).unwrap_or_default();
                    let published_assignments = store.get_published_assignments(cls.id, c.id).unwrap_or_default();

                    let mut buf = format!("📊 {} {} 发布状态:\n", course, class_name);
                    buf.push_str("\n课次:\n");
                    for l in &all_lessons {
                        let icon = if published_lessons.contains(&l.id) { "✅" } else { "⬜" };
                        buf.push_str(&format!("  {icon} 第{}次: {}\n", l.lesson_num, l.topic));
                    }
                    buf.push_str("\n作业:\n");
                    for a in &all_assignments {
                        let icon = if published_assignments.contains(&a.id) { "✅" } else { "⬜" };
                        buf.push_str(&format!("  {icon} [{}] {}\n", a.id, a.title));
                    }
                    buf
                }
                _ => "用法: /class <publish|status> <课程码> <班级> ...".to_string(),
            }
        }

        // ===== 学生端作业命令 =====
        "/assignments" if role == "student" => {
            let store = match store::EduStore::open(&db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let courses = store.list_courses_by_teacher(1).unwrap_or_default();
            let mut buf = "📝 作业列表:\n".to_string();
            for c in &courses {
                let assignments = store.get_assignments(c.id).unwrap_or_default();
                for a in &assignments {
                    buf.push_str(&format!("  [{}] {} — {} (截止: {})\n", a.id, a.title, a.description, if a.due_date.is_empty() { "无" } else { &a.due_date }));
                }
            }
            if buf.lines().count() <= 1 { buf.push_str("（暂无已发布的作业）"); }
            buf
        }
        "/submit" if role == "student" => {
            let store = match store::EduStore::open(&db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let id_str = args.get(0).copied().unwrap_or("");
            let aid: i64 = id_str.parse().unwrap_or(0);
            let content = args.get(1).copied().unwrap_or("");
            if aid == 0 {
                return "用法: /submit <作业id> <内容>".to_string();
            }
            // 用 student_no 查找 student_id（简化版：用 config 中的 student_no）
            let student_no = config.edu.student_no.as_str();
            if student_no.is_empty() {
                return "❌ 请先认证: /auth login <学号> <密码>".to_string();
            }
            let student = store.get_student(student_no).ok().flatten();
            let Some(student) = student else { return "❌ 学生信息未找到，请先认证".to_string() };
            match store.submit_assignment(aid, student.id, content, "") {
                Ok(_) => "✅ 作业已提交！".to_string(),
                Err(e) => format!("❌ {e}"),
            }
        }
        "/feedback" if role == "student" => {
            let store = match store::EduStore::open(&db_path) {
                Ok(s) => s,
                Err(e) => return format!("❌ {e}"),
            };
            let id_str = args.get(0).copied().unwrap_or("");
            let aid: i64 = id_str.parse().unwrap_or(0);
            if aid == 0 { return "用法: /feedback <作业id>".to_string(); }
            let student_no = config.edu.student_no.as_str();
            let student = store.get_student(student_no).ok().flatten();
            let Some(student) = student else { return "❌ 请先认证".to_string() };
            match store.get_submission(aid, student.id) {
                Ok(Some(sub)) => {
                    let mut buf = format!("📝 作业反馈:\n  状态: {}\n", sub.status);
                    if sub.ai_score > 0.0 { buf.push_str(&format!("  AI 评分: {:.0}%\n  AI 反馈: {}\n", sub.ai_score * 100.0, sub.ai_feedback)); }
                    if sub.teacher_score >= 0.0 { buf.push_str(&format!("  教师评分: {:.0}%\n  教师反馈: {}\n", sub.teacher_score * 100.0, sub.teacher_feedback)); }
                    buf
                }
                Ok(None) => "（尚未提交此作业）".to_string(),
                Err(e) => format!("❌ {e}"),
            }
        }

        // ===== 未知命令 =====
        _ => format!("⚠️ 未知命令: {cmd}\n输入 /help 查看可用命令"),
    }
}
