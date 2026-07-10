//! RHermes 教育版模块
//!
//! 教育模式入口，包含学生版和教师版功能。
//! 通过 `rhermes edu student` / `rhermes edu teacher` 启动。
//!
//! 通用模式（不带 edu 子命令）完全不受影响。

pub mod auth;
pub mod course;
pub mod dashboard;
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
