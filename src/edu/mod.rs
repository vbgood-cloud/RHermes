//! RHermes 教育版模块
//!
//! 教育模式入口，包含学生版和教师版功能。
//! 通过 `rhermes edu student` / `rhermes edu teacher` 启动。
//!
//! 通用模式（不带 edu 子命令）完全不受影响。

pub mod store;

use std::path::Path;

/// 处理 edu 子命令
///
/// 在 main.rs 的 match 中调用，传入子命令名称和参数。
pub async fn handle_edu(command: &str, args: &[String], config_path: &Path) {
    match command {
        "student" => {
            println!("🎓 启动学生模式...");
            println!("   配置文件: {}", config_path.display());
            println!("   （教育版功能开发中）");
        }
        "teacher" => {
            println!("👩‍🏫 启动教师模式...");
            println!("   配置文件: {}", config_path.display());
            println!("   （教育版功能开发中）");
        }
        "join" => {
            let code = args.first().cloned().unwrap_or_default();
            println!("🔗 加入课程: {code}");
            println!("   （教育版功能开发中）");
        }
        "status" => {
            println!("📊 学习状态");
            println!("   （教育版功能开发中）");
        }
        _ => {
            eprintln!("未知的教育子命令: {command}");
        }
    }
}
