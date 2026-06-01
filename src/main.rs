//! RHermes — Reasonix 的省 Token 肌肉 + Hermes 的自进化大脑 + Rust 的零开销骨架
//!
//! 终端 AI 编程 Agent，融合极致 Token 缓存优化与自进化学习闭环。

mod agent;
mod api;
mod core;
mod cost;
mod init;
mod tools;
mod tui;

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;

use clap::{Parser, Subcommand};
use crate::core::Config;
use crate::core::PathManager;
use tools::builtin_registry;
use tools::ToolDispatcher;
use tui::App;

// ---------------------------------------------------------------------------
// CLI 入口
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "rhermes")]
#[command(about = "Reasonix x Hermes — 自进化的终端 AI 编程 Agent", long_about = None)]
#[command(version)]
struct Cli {
    /// 恢复上一次会话内容
    #[arg(short = 'r', long)]
    resume: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 🚀 启动编程 Agent（默认）
    Code {
        /// 项目目录（默认当前目录）
        #[arg(short, long)]
        dir: Option<String>,
    },
    /// ⚙️ 交互式初始化向导（API Key / 模型 / 部署方式）
    Init,
}

// ---------------------------------------------------------------------------
// 主函数
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // 日志文件 + 控制台
    let log_file = OpenOptions::new()
        .create(true).append(true).open("rhermes.log").ok();
    let _ = log_file.as_ref().map(|mut f| writeln!(f, "--- RHermes v{} ---", env!("CARGO_PKG_VERSION")));

    // 同时输出到 stderr（控制台）和 rhermes.log
    let make_writer = move || -> Box<dyn Write + Send + 'static> {
        if let Ok(file) = OpenOptions::new().create(true).append(true).open("rhermes.log") {
            Box::new(file)
        } else {
            Box::new(std::io::stderr())
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rhermes=debug".into()),
        )
        .with_ansi(false)
        .with_writer(make_writer)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Init) => {
            // 运行 init 向导
            if let Err(e) = init::run_init() {
                eprintln!("[RHermes] 初始化失败: {e}");
                std::process::exit(1);
            }
        }
        _ => {
            // 默认：启动编程 Agent (code)
            run_code(cli.resume).await;
        }
    }
}

// ---------------------------------------------------------------------------
// code 子命令
// ---------------------------------------------------------------------------

async fn run_code(resume: bool) {
    // 检测部署模式并初始化路径管理器
    let path_mgr = PathManager::detect();

    // 确保标准目录结构存在
    if let Err(e) = path_mgr.ensure_dirs() {
        eprintln!("[RHermes] 目录初始化失败: {e}");
        std::process::exit(1);
    }

    tracing::info!(
        "RHermes v{} 启动 · 部署模式: {} · 数据目录: {}",
        env!("CARGO_PKG_VERSION"),
        path_mgr.mode().name(),
        path_mgr.data_root().display(),
    );

    // 初始化工具系统
    let registry = builtin_registry();
    let dispatcher = ToolDispatcher::new(registry.clone());

    tracing::info!(
        "工具注册表已就绪 · {} 个工具 · {} 个可并行",
        registry.len(),
        registry.parallel_safe_names().len(),
    );

    // 加载配置
    let config_path = path_mgr.config_path();
    let config = match Config::load(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[RHermes] 配置加载失败: {e}");
            Config::default()
        }
    };

    // 初始化记忆系统
    let memory_path = path_mgr.data_root().join("memories.db");
    let memory = match crate::agent::MemorySystem::open(&memory_path) {
        Ok(m) => {
            tracing::info!("记忆系统已就绪: {}", memory_path.display());
            Some(Arc::new(Mutex::new(m)))
        }
        Err(e) => {
            tracing::warn!("记忆系统初始化失败: {e}");
            None
        }
    };

    // 设置全局配置（供子 Agent 工具使用）
    if config.is_configured() {
        let _ = crate::tools::set_global_config(config.clone());
    }

    // 创建 TUI
    let mut app = App::new(path_mgr.mode().name(), dispatcher, memory, resume);

    // 如果已有 API Key，初始化 API 客户端
    if config.is_configured() {
        tracing::info!("API Key 已配置，初始化 DeepSeek 客户端");
        app.init_api(config, &path_mgr);
    } else {
        tracing::warn!("未检测到 API Key，运行在模拟模式");
        app.messages.push(tui::Message::system(
            "⚠ 未配置 API Key。输入 /init 启动初始化向导，或运行 rhermes init。",
        ));
        app.messages.push(tui::Message::system(format!(
            "   配置文件路径: {}",
            config_path.display()
        )));
    }

    // 运行 TUI
    if let Err(e) = app.run().await {
        eprintln!("[RHermes] TUI 错误: {e}");
        std::process::exit(1);
    }
}
