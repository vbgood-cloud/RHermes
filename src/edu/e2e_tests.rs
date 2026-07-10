//! 教育版集成测试
//!
//! 端到端流程：教师初始化 → 创建课程/班级/学生 → 学生认证 → 记录学习日志 → 生成报告

#[cfg(test)]
mod e2e_tests {
    use crate::edu::{
        auth,
        course::{CourseContext, LearnMode, format_course_list},
        reflection::{evaluate_question_quality, evaluate_reflection, generate_growth_report},
        store::EduStore,
        teacher::TeacherManager,
    };

    /// E2E：完整教学流程
    #[test]
    fn test_e2e_full_teaching_flow() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("edu.db");
        let mgr = TeacherManager::new(&db_path).unwrap();

        // 1. 教师初始化
        let teacher_id = mgr.init_teacher("王老师", "teacher_pass").unwrap();
        assert!(teacher_id > 0);

        // 2. 创建课程
        let course = mgr.create_course(teacher_id, "CS101", "Python 编程基础").unwrap();

        // 3. 创建班级
        let class = mgr.create_class("CS101", "计算机2301").unwrap();

        // 4. 创建课次
        mgr.create_lesson("CS101", "计算机2301", 1, "变量与数据类型").unwrap();
        mgr.create_lesson("CS101", "计算机2301", 2, "条件语句").unwrap();

        // 5. 添加学生
        let student = mgr
            .add_student("2024001", "张三", "stu123", Some("计算机2301"), Some("CS101"))
            .unwrap();

        // 6. 验证选课
        let courses = mgr.store.get_student_courses(student.id).unwrap();
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].course_code, "CS101");

        // 7. 学生认证
        let store = EduStore::open(&db_path).unwrap();
        let auth_result = auth::authenticate(&store, "2024001", "stu123").unwrap();
        assert_eq!(auth_result.student_name, "张三");
        assert!(!auth_result.token.is_empty());

        // 8. 错误密码认证
        let auth_fail = auth::authenticate(&store, "2024001", "wrong");
        assert!(auth_fail.is_err());

        // 9. 记录学习日志
        store
            .write_journal(
                student.id,
                course.id,
                1,
                "变量与数据类型",
                "[{\"tool\":\"read_file\",\"success\":true}]",
                "我学会了变量的定义和使用",
                0.75,
                0.6,
                1200,
                180,
            )
            .unwrap();

        store
            .write_journal(
                student.id,
                course.id,
                2,
                "条件语句",
                "[{\"tool\":\"write_file\",\"success\":true}]",
                "条件语句让我能控制程序流程",
                0.82,
                0.75,
                1500,
                220,
            )
            .unwrap();

        // 10. 查询学习日志
        let journals = store.get_student_journal(student.id, Some(course.id)).unwrap();
        assert_eq!(journals.len(), 2);
        assert_eq!(journals[0].topic, "变量与数据类型");

        // 11. 生成成长报告
        let report = generate_growth_report("张三", "2024001", &journals);
        assert!(report.contains("学习成长报告"));
        assert!(report.contains("张三"));
        assert!(report.contains("变量与数据类型")); // 课次主题
    }

    /// E2E：课程切换流程
    #[test]
    fn test_e2e_course_switching() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("edu.db");
        let mgr = TeacherManager::new(&db_path).unwrap();

        // 教师创建两门课程
        let teacher = mgr.init_teacher("李老师", "pass").unwrap();
        mgr.create_course(teacher, "CS101", "Python").unwrap();
        mgr.create_course(teacher, "CS201", "数据结构").unwrap();
        mgr.create_class("CS101", "计算机2301").unwrap();
        mgr.create_class("CS201", "计算机2301").unwrap();

        // 学生选两门课
        let student = mgr
            .add_student("2024999", "测试生", "p", Some("计算机2301"), Some("CS101"))
            .unwrap();
        // 查询 CS201 课程再选课
        let cs201 = mgr.store.get_course("CS201").unwrap().unwrap();
        mgr.store.enroll(student.id, cs201.id).unwrap();

        // 查询选课列表
        let courses = mgr.store.get_student_courses(student.id).unwrap();
        assert_eq!(courses.len(), 2);

        // 课程列表显示
        let list = format_course_list(&courses, Some("CS101"));
        assert!(list.contains("CS101"));
        assert!(list.contains("CS201"));
        assert!(list.contains("← 当前"));
    }

    /// E2E：学习模式切换
    #[test]
    fn test_e2e_learning_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = TeacherManager::new(&tmp.path().join("edu.db")).unwrap();
        let teacher = mgr.init_teacher("老师", "p").unwrap();
        let course = mgr.create_course(teacher, "TS101", "测试").unwrap();

        // explore 模式
        let explore = crate::edu::course::CourseProfile::from_course(&course, LearnMode::Explore, Some(1));
        assert!(explore.system_prompt_override.is_none());
        assert!(explore.allowed_tools.is_empty()); // 空 = 全部允许

        // scaffold 模式
        let scaffold = crate::edu::course::CourseProfile::from_course(&course, LearnMode::Scaffold, Some(2));
        assert!(scaffold.system_prompt_override.is_some());
        let prompt = scaffold.system_prompt_override.unwrap();
        assert!(prompt.contains("苏格拉底"));

        // locked 模式
        let locked = crate::edu::course::CourseProfile::from_course(&course, LearnMode::Locked, Some(3));
        // locked 限制只读工具
        for tool in &locked.allowed_tools {
            assert!(
                matches!(tool.as_str(), "read_file" | "search_content" | "glob" | "get_current_time" | "read_pdf" | "read_excel" | "read_docx" | "read_pptx"),
                "locked 模式不应包含 '{}' 工具",
                tool
            );
        }
    }

    /// E2E：反思评估流程
    #[test]
    fn test_e2e_reflection_evaluation() {
        // 学生对话 → 生成反思 → 评估
        let tools = vec!["read_file".to_string(), "search_content".to_string()];

        // 生成反思提示
        let prompt = generate_reflection_prompt_with_tools("讨论了 Rust 所有权", &tools, "explore");
        assert!(prompt.question.contains("工具"));
        assert!(!prompt.hint.is_none());

        // 评估学生的反思回答
        let good_reflection = "我发现自己一开始就直接问 AI 要代码，没有先思考。因为这个问题其实可以分解为两步。如果下次遇到，我应该先自己分析。";
        let score_good = evaluate_reflection(good_reflection, 200);
        assert!(score_good.depth > 0.5);
        assert!(score_good.overall > 0.5);

        // 评估敷衍回答
        let bad_reflection = "没什么";
        let score_bad = evaluate_reflection(bad_reflection, 200);
        assert!(score_bad.depth < 0.3);

        // 评估提问质量
        let good_question = "我的 Cargo.toml 里 serde 版本冲突，编译报错说 version solving failed";
        let q_score = evaluate_question_quality(good_question);
        assert!(q_score > 0.5);
    }

    /// 辅助：带工具的反思提示生成
    fn generate_reflection_prompt_with_tools(
        summary: &str,
        tools: &[String],
        mode: &str,
    ) -> crate::edu::reflection::ReflectionPrompt {
        crate::edu::reflection::generate_reflection_prompt(summary, tools, mode)
    }

    /// E2E：CSV 批量导入
    #[test]
    fn test_e2e_csv_import() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = TeacherManager::new(&tmp.path().join("edu.db")).unwrap();
        mgr.init_teacher("老师", "p").unwrap();
        mgr.create_course(1, "CS301", "操作系统").unwrap();
        mgr.create_class("CS301", "OS班").unwrap();

        let csv_path = tmp.path().join("students.csv");
        std::fs::write(&csv_path, "2024001,张三,pass1\n2024002,李四,pass2\n# 注释行\n2024003,王五\n")
            .unwrap();

        let count = mgr.import_students(&csv_path, "CS301", "OS班").unwrap();
        assert_eq!(count, 3); // 3 个有效行（注释被跳过）

        // 验证学生可以认证
        let store = EduStore::open(&tmp.path().join("edu.db")).unwrap();
        assert!(store.verify_student_password("2024001", "pass1").unwrap());
        assert!(store.verify_student_password("2024003", "123456").unwrap()); // 默认密码
    }
}
