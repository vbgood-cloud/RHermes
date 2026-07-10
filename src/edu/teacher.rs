//! 教师端管理逻辑
//!
//! 教师通过 CLI 命令管理课程、班级、课次、学生。

use std::path::Path;

use crate::edu::store::{Class, Course, EduStore, EduError, Lesson, Student};

/// 教师管理器
pub struct TeacherManager {
    pub store: EduStore,
}

impl TeacherManager {
    pub fn new(db_path: &Path) -> Result<Self, EduError> {
        Ok(Self {
            store: EduStore::open(db_path)?,
        })
    }

    /// 初始化教师身份
    pub fn init_teacher(&self, name: &str, password: &str) -> Result<i64, EduError> {
        let teacher = self.store.create_teacher(name, password)?;
        println!("✅ 教师 '{}' 创建成功 (ID: {})", teacher.name, teacher.id);
        Ok(teacher.id)
    }

    /// 创建课程
    pub fn create_course(
        &self,
        teacher_id: i64,
        code: &str,
        name: &str,
    ) -> Result<Course, EduError> {
        let course = self
            .store
            .create_course(code, name, teacher_id)?;
        println!("✅ 课程创建成功: {} {} ({})", course.course_code, course.name, course.id);
        Ok(course)
    }

    /// 创建班级
    pub fn create_class(&self, course_code: &str, class_name: &str) -> Result<Class, EduError> {
        let course = self
            .store
            .get_course(course_code)?
            .ok_or_else(|| EduError::NotFound(format!("课程 '{course_code}' 不存在")))?;
        let class = self.store.create_class(class_name, course.id)?;
        println!("✅ 班级创建成功: {} → {} ({})", class_name, course_code, class.id);
        Ok(class)
    }

    /// 创建课次
    pub fn create_lesson(
        &self,
        course_code: &str,
        class_name: &str,
        lesson_num: i64,
        topic: &str,
    ) -> Result<Lesson, EduError> {
        let course = self
            .store
            .get_course(course_code)?
            .ok_or_else(|| EduError::NotFound(format!("课程 '{course_code}' 不存在")))?;

        // 找班级
        let classes = self.store.get_classes_by_course(course.id)?;
        let class = classes
            .iter()
            .find(|c| c.name == class_name)
            .ok_or_else(|| EduError::NotFound(format!("班级 '{class_name}' 不属于课程 '{course_code}'")))?;

        let lesson = self.store.create_lesson(course.id, class.id, lesson_num, topic)?;
        println!(
            "✅ 课次创建成功: {} {} 第{}次课: {}",
            course_code, class_name, lesson_num, topic
        );
        Ok(lesson)
    }

    /// 添加学生（单个）
    pub fn add_student(
        &self,
        student_no: &str,
        name: &str,
        password: &str,
        class_name: Option<&str>,
        course_code: Option<&str>,
    ) -> Result<Student, EduError> {
        // 如果指定了班级+课程，找到 class_id
        let class_id = if let (Some(cn), Some(cc)) = (class_name, course_code) {
            let course = self
                .store
                .get_course(cc)?
                .ok_or_else(|| EduError::NotFound(format!("课程 '{cc}' 不存在")))?;
            let classes = self.store.get_classes_by_course(course.id)?;
            classes
                .iter()
                .find(|c| c.name == cn)
                .map(|c| c.id)
                .ok_or_else(|| EduError::NotFound(format!("班级 '{cn}' 不存在")))
        } else {
            Err(EduError::NotFound("必须指定课程码和班级名".into()))
        }?;

        let student = self
            .store
            .create_student(student_no, name, password, Some(class_id))?;
        println!("✅ 学生添加成功: {} {} ({})", student_no, name, student.id);

        // 自动选课
        if let Some(cc) = course_code {
            if let Some(course) = self.store.get_course(cc)? {
                self.store.enroll(student.id, course.id)?;
                println!("   已自动选课: {}", cc);
            }
        }

        Ok(student)
    }

    /// 批量导入学生（CSV 格式：学号,姓名,密码）
    pub fn import_students(
        &self,
        csv_path: &Path,
        course_code: &str,
        class_name: &str,
    ) -> Result<usize, EduError> {
        let content = std::fs::read_to_string(csv_path)
            .map_err(|e| EduError::NotFound(format!("读取 CSV 失败: {e}")))?;

        let mut count = 0;
        for (line_no, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() < 2 {
                eprintln!("⚠️ 第{}行格式错误（需要 学号,姓名[,密码]）", line_no + 1);
                continue;
            }
            let no = parts[0].trim();
            let name = parts[1].trim();
            let pwd = if parts.len() >= 3 {
                parts[2].trim()
            } else {
                "123456" // 默认密码
            };

            match self.add_student(no, name, pwd, Some(class_name), Some(course_code)) {
                Ok(_) => count += 1,
                Err(e) => eprintln!("⚠️ 第{}行导入失败: {e}", line_no + 1),
            }
        }
        println!("✅ 批量导入完成: {count} 个学生");
        Ok(count)
    }

    /// 列出教师的课程
    pub fn list_courses(&self, teacher_id: i64) -> Result<Vec<Course>, EduError> {
        let courses = self.store.list_courses_by_teacher(teacher_id)?;
        if courses.is_empty() {
            println!("（无课程）");
        } else {
            println!("📚 课程列表:");
            for c in &courses {
                println!("  {} {} — {}", c.course_code, c.name, c.id);
            }
        }
        Ok(courses)
    }

    /// 列出课程的班级和学生
    pub fn list_roster(&self, course_code: &str) -> Result<(), EduError> {
        let course = self
            .store
            .get_course(course_code)?
            .ok_or_else(|| EduError::NotFound(format!("课程 '{course_code}' 不存在")))?;

        let classes = self.store.get_classes_by_course(course.id)?;
        println!("📋 课程花名册: {} {}", course_code, course.name);

        for class in &classes {
            println!("\n  班级: {} ({})", class.name, class.id);
            let lessons = self.store.get_lessons(course.id, class.id)?;
            if !lessons.is_empty() {
                println!("    课次:");
                for l in &lessons {
                    println!("      第{}次: {}", l.lesson_num, l.topic);
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CLI 命令处理
// ---------------------------------------------------------------------------

/// 处理教师子命令
pub fn handle_teacher_command(args: &[String], db_path: &Path) {
    if args.is_empty() {
        print_teacher_help();
        return;
    }

    let mgr = match TeacherManager::new(db_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("❌ 数据库打开失败: {e}");
            return;
        }
    };

    match args[0].as_str() {
        "init" => {
            let name = args.get(1).cloned().unwrap_or_else(|| {
                dialoguer::Input::new()
                    .with_prompt("教师姓名")
                    .interact_text()
                    .unwrap_or_default()
            });
            let password = dialoguer::Password::new()
                .with_prompt("设置密码")
                .interact()
                .unwrap_or_default();
            if let Err(e) = mgr.init_teacher(&name, &password) {
                eprintln!("❌ {e}");
            }
        }
        "course" => {
            let action = args.get(1).map(|s| s.as_str()).unwrap_or("");
            match action {
                "create" => {
                    let code = args.get(2).cloned().unwrap_or_default();
                    let name = args.get(3).cloned().unwrap_or_default();
                    if code.is_empty() || name.is_empty() {
                        eprintln!("用法: rhermes edu teacher course create <课程码> <课程名>");
                        return;
                    }
                    // 默认用 teacher_id=1（第一个教师）
                    if let Err(e) = mgr.create_course(1, &code, &name) {
                        eprintln!("❌ {e}");
                    }
                }
                "list" => {
                    if let Err(e) = mgr.list_courses(1) {
                        eprintln!("❌ {e}");
                    }
                }
                _ => {
                    eprintln!("用法: rhermes edu teacher course <create|list> ...");
                }
            }
        }
        "class" => {
            let code = args.get(2).cloned().unwrap_or_default();
            let name = args.get(3).cloned().unwrap_or_default();
            if code.is_empty() || name.is_empty() {
                eprintln!("用法: rhermes edu teacher class create <课程码> <班级名>");
                return;
            }
            if let Err(e) = mgr.create_class(&code, &name) {
                eprintln!("❌ {e}");
            }
        }
        "lesson" => {
            let code = args.get(2).cloned().unwrap_or_default();
            let class_name = args.get(3).cloned().unwrap_or_default();
            let num_str = args.get(4).cloned().unwrap_or_default();
            let topic = args.get(5).cloned().unwrap_or_default();
            if code.is_empty() || class_name.is_empty() || num_str.is_empty() {
                eprintln!("用法: rhermes edu teacher lesson create <课程码> <班级> <序号> <主题>");
                return;
            }
            let num: i64 = num_str.parse().unwrap_or(1);
            if let Err(e) = mgr.create_lesson(&code, &class_name, num, &topic) {
                eprintln!("❌ {e}");
            }
        }
        "student" => {
            let action = args.get(1).map(|s| s.as_str()).unwrap_or("");
            match action {
                "add" => {
                    let no = args.get(2).cloned().unwrap_or_default();
                    let name = args.get(3).cloned().unwrap_or_default();
                    let course = args.get(4).cloned().unwrap_or_default();
                    let class_name = args.get(5).cloned().unwrap_or_default();
                    if no.is_empty() || name.is_empty() || course.is_empty() || class_name.is_empty() {
                        eprintln!("用法: rhermes edu teacher student add <学号> <姓名> <课程码> <班级名>");
                        return;
                    }
                    let password = dialoguer::Password::new()
                        .with_prompt(format!("为 {name} 设置密码"))
                        .interact()
                        .unwrap_or_else(|_| "123456".to_string());
                    if let Err(e) = mgr.add_student(&no, &name, &password, Some(&class_name), Some(&course)) {
                        eprintln!("❌ {e}");
                    }
                }
                "import" => {
                    let csv = args.get(2).cloned().unwrap_or_default();
                    let course = args.get(3).cloned().unwrap_or_default();
                    let class_name = args.get(4).cloned().unwrap_or_default();
                    if csv.is_empty() || course.is_empty() || class_name.is_empty() {
                        eprintln!("用法: rhermes edu teacher student import <CSV路径> <课程码> <班级名>");
                        return;
                    }
                    if let Err(e) = mgr.import_students(Path::new(&csv), &course, &class_name) {
                        eprintln!("❌ {e}");
                    }
                }
                _ => {
                    eprintln!("用法: rhermes edu teacher student <add|import> ...");
                }
            }
        }
        "list" => {
            let course_code = args.get(1).cloned();
            if let Some(code) = course_code {
                if let Err(e) = mgr.list_roster(&code) {
                    eprintln!("❌ {e}");
                }
            } else if let Err(e) = mgr.list_courses(1) {
                eprintln!("❌ {e}");
            }
        }
        _ => {
            print_teacher_help();
        }
    }
}

fn print_teacher_help() {
    println!("👩‍🏫 教师管理命令:");
    println!();
    println!("  rhermes edu teacher init                    初始化教师身份");
    println!("  rhermes edu teacher course create <码> <名>  创建课程");
    println!("  rhermes edu teacher course list              列出课程");
    println!("  rhermes edu teacher class create <码> <班>   创建班级");
    println!("  rhermes edu teacher lesson create <码> <班> <序号> <主题>  创建课次");
    println!("  rhermes edu teacher student add <学号> <姓名> <课程码> <班级>  添加学生");
    println!("  rhermes edu teacher student import <CSV> <课程码> <班级>     批量导入");
    println!("  rhermes edu teacher list [课程码]            列出课程/花名册");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_mgr() -> (tempfile::TempDir, TeacherManager) {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = TeacherManager::new(&tmp.path().join("edu.db")).unwrap();
        (tmp, mgr)
    }

    #[test]
    fn test_teacher_init_and_course() {
        let (_tmp, mgr) = setup_mgr();
        let tid = mgr.init_teacher("测试老师", "pass123").unwrap();
        assert!(tid > 0);

        let course = mgr.create_course(tid, "TS101", "测试课程").unwrap();
        assert_eq!(course.course_code, "TS101");
    }

    #[test]
    fn test_teacher_class_lesson_student() {
        let (_tmp, mgr) = setup_mgr();
        let teacher = mgr.init_teacher("老师", "p").unwrap();
        let course = mgr.create_course(teacher, "TS201", "测试").unwrap();
        let class = mgr.create_class("TS201", "一班").unwrap();

        let lesson = mgr.create_lesson("TS201", "一班", 1, "第一课").unwrap();
        assert_eq!(lesson.lesson_num, 1);

        let student = mgr
            .add_student("2024999", "测试生", "spwd", Some("一班"), Some("TS201"))
            .unwrap();
        assert_eq!(student.student_no, "2024999");

        // 验证选课
        let courses = mgr.store.get_student_courses(student.id).unwrap();
        assert_eq!(courses.len(), 1);
    }

    #[test]
    fn test_import_students_csv() {
        let (_tmp, mgr) = setup_mgr();
        mgr.init_teacher("老师", "p").unwrap();
        mgr.create_course(1, "TS301", "导入测试").unwrap();
        mgr.create_class("TS301", "导入班").unwrap();

        let csv_path = _tmp.path().join("students.csv");
        std::fs::write(&csv_path, "2024001,张三,pass1\n2024002,李四,pass2\n2024003,王五\n")
            .unwrap();

        let count = mgr.import_students(&csv_path, "TS301", "导入班").unwrap();
        assert_eq!(count, 3);

        // 验证学生存在
        let s1 = mgr.store.get_student("2024001").unwrap().unwrap();
        assert_eq!(s1.name, "张三");
    }
}
