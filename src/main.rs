//! RHermes — Reasonix 的省 Token 肌肉 + Hermes 的自进化大脑 + Rust 的零开销骨架
//!
//! 终端 AI 编程 Agent，融合极致 Token 缓存优化与自进化学习闭环。

mod agent;
mod api;
mod channel;
mod core;
mod cost;
mod debug;
mod gateway;
mod init;
mod provider;
mod tools;
mod tui;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use clap::{Parser, Subcommand};
use crate::channel::{Channel, ChannelManager};
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
    /// ⚙️ 交互式初始化向导（API Key / 模型）
    Init,
    /// 🔍 调试工具
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
    /// 🌐 Gateway 守护进程模式
    Gateway {
        #[command(subcommand)]
        command: GatewayCommand,
    },
}

#[derive(Subcommand)]
enum GatewayCommand {
    /// 🛠 交互式配置 Gateway（通道选择 + 参数设置）
    Setup,
    /// ▶ 启动 Gateway 守护进程
    Start,
    /// ⏹ 停止 Gateway 守护进程
    Stop,
    /// 📊 查看 Gateway 运行状态
    Status,
    /// 📡 管理通道启停
    Channel {
        #[command(subcommand)]
        command: GatewayChannelCommand,
    },
}

#[derive(Subcommand)]
enum GatewayChannelCommand {
    /// 列出所有通道及其启用状态
    List,
    /// 启用指定通道
    Enable {
        /// 通道名称（wechat / wecom）
        name: String,
    },
    /// 禁用指定通道
    Disable {
        /// 通道名称（wechat / wecom）
        name: String,
    },
}

#[derive(Subcommand)]
enum DebugCommand {
    /// 导出调试报告
    Export {
        /// 会话 ID（留空自动使用最近的 session.json）
        session_id: Option<String>,
        /// 输出文件路径
        #[arg(short, long, default_value = "debug-report.json")]
        output: String,
    },
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

    // 检查配置是否存在，不存在则引导初始化
    let config_path = PathManager::detect().config_path();
    let needs_init = !config_path.exists();

    match cli.command {
        Some(Commands::Init) => {
            if let Err(e) = init::run_init() {
                eprintln!("[RHermes] 初始化失败: {e}");
                std::process::exit(1);
            }
        }
        Some(Commands::Debug { command }) => {
            match command {
                DebugCommand::Export { session_id, output } => {
                    export_debug(session_id, &output).unwrap_or_else(|e| {
                        eprintln!("[RHermes] 调试导出失败: {e}");
                        std::process::exit(1);
                    });
                }
            }
        }
        Some(Commands::Gateway { command }) => {
            if let Err(e) = crate::gateway::handle_command(command, &config_path).await {
                eprintln!("[RHermes] Gateway 错误: {e}");
                std::process::exit(1);
            }
        }
        _ => {
            if needs_init {
                println!("📋 未检测到配置文件，正在启动初始化向导...");
                if let Err(e) = init::run_init() {
                    eprintln!("[RHermes] 初始化失败: {e}");
                    std::process::exit(1);
                }
            }
            run_code(cli.resume).await;
        }
    }
}

// ---------------------------------------------------------------------------
// debug export 命令
// ---------------------------------------------------------------------------

fn export_debug(session_id: Option<String>, output: &str) -> Result<(), debug::DebugError> {
    let path = PathManager::detect().data_root().join("debug");
    if !path.exists() {
        eprintln!("[RHermes] 未找到调试数据目录: {}", path.display());
        eprintln!("   请先运行 rhermes code 进行对话");
        std::process::exit(1);
    }

    let session_id = session_id.unwrap_or_else(|| "latest".into());

    // 查找调试文件
    let debug_file = if session_id == "latest" {
        // 找最新文件
        let mut entries: Vec<_> = fs::read_dir(&path)
            .map_err(|e| debug::DebugError::Io(e.to_string()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
            .collect();
        entries.sort_by_key(|e| e.path().metadata().ok().map(|m| m.modified().ok()));
        entries.last().map(|e| e.path()).ok_or_else(|| {
            debug::DebugError::Io("没有找到调试文件".into())
        })?
    } else {
        path.join(format!("session-{session_id}.json"))
    };

    // 复制到输出路径（如果不同）
    let output_path = Path::new(output);
    if debug_file != output_path {
        fs::copy(&debug_file, output_path)
            .map_err(|e| debug::DebugError::Io(e.to_string()))?;
    }
    println!("✅ 调试报告已导出: {}", output_path.display());
    Ok(())
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
    let memories_dir = path_mgr.data_root().join("memories");
    let _ = std::fs::create_dir_all(&memories_dir);
    let memory_path = memories_dir.join("memories.db");
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

    // 初始化技能引擎
    let skills_dir = path_mgr.skills_dir();
    let skill_engine = match crate::agent::SkillEngine::load(&skills_dir) {
        Ok(se) => {
            tracing::info!("技能引擎已就绪: {} 个技能", se.count());
            Some(Arc::new(Mutex::new(se)))
        }
        Err(e) => {
            tracing::warn!("技能引擎加载失败: {e}");
            None
        }
    };

    // 设置全局配置（供子 Agent 工具使用）
    if config.is_configured() {
        let _ = crate::tools::set_global_config(config.clone());
    }

    // 设置全局显示配置（供 read_pdf 等工具使用）
    crate::tools::set_display_config(config.display.clone());

    // 设置全局技能引擎（供 run_skill 工具使用）
    if let Some(ref se) = skill_engine {
        let _ = crate::tools::set_global_skill_engine(Arc::clone(se));
    }

    // 初始化调试系统（按配置启用）
    let session_debug = if config.debug.enabled {
        let mut sd = debug::SessionDebug::new();
        sd.set_max_entries(config.debug.buffer_size);
        Some(Arc::new(Mutex::new(sd)))
    } else {
        None
    };
    let session_debug_default = Arc::new(Mutex::new(debug::SessionDebug::new()));
    let session_debug: Arc<Mutex<debug::SessionDebug>> = session_debug.unwrap_or(session_debug_default);

    // 运行 Curator 技能生命周期管理
    tracing::info!("Curator 技能检查...");
    let curator = crate::agent::Curator::new(skills_dir.clone(), config.clone());
    let curator_report = curator.run();
    tracing::info!("{}", curator_report.message);

    // ---- Channel 系统初始化 ----
    use crate::tui::channel::TuiChannel;
    let mut channel_mgr = ChannelManager::new();
    let tui_channel = Arc::new(TuiChannel);
    channel_mgr.register(tui_channel.clone());

    // 企业微信通道
    if config.channels.wecom.enabled {
        let wecom_channel = Arc::new(crate::channel::wecom::WeComChannel::new(&config));
        channel_mgr.register(wecom_channel);
        tracing::info!("企业微信通道已注册");
    }

    // 微信个号通道
    if config.channels.wechat.enabled {
        let wechat_channel = Arc::new(crate::channel::wechat::WeChatChannel::new(&config));
        channel_mgr.register(wechat_channel);
        tracing::info!("微信个号通道已注册");
    }

    // 创建 TUI
    let config_path_buf = config_path.clone();
    let max_memory_md_chars = config.memory.max_memory_md_chars;
    let mut app = App::new(path_mgr.mode().name(), dispatcher, memory, skill_engine, resume, config_path_buf, max_memory_md_chars, memories_dir, session_debug);

    // 将 TUI 接入 Channel 系统
    let inbound_tx = channel_mgr.inbound_tx();
    TuiChannel::attach(&mut app, inbound_tx);

    // ── 扫码登录：检查所有 Channel 是否需要二维码 ──
    for ch in channel_mgr.iter() {
        if let Some((qr_text, img_data)) = ch.login_qrcode().await {
            // 保存二维码图片
            let qr_path = "wechat_qrcode.png";
            if let Err(e) = std::fs::write(qr_path, &img_data) {
                tracing::warn!("保存二维码图片失败: {e}");
            } else {
                // 跨平台打开图片查看器
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", qr_path])
                    .spawn();
                tracing::info!("二维码已保存至: {}", qr_path);
            }
            // 在 TUI 中显示 ASCII 二维码
            app.messages.push(tui::Message::system(format!(
                "📱 微信扫码登录 — 二维码已保存到: {}",
                qr_path
            )));
            let qr_lines = crate::tui::render_ascii_qr(&qr_text);
            for line in &qr_lines {
                app.messages.push(tui::Message::system(
                    line.spans.iter().map(|s| s.content.as_ref()).collect::<Vec<_>>().join(""),
                ));
            }
            app.messages.push(tui::Message::system(
                "⏳ 等待扫码... 扫码成功后自动开始接收消息",
            ));
        }
    }

    // 如果已有 API Key，初始化 API 客户端
    if config.is_configured() {
        tracing::info!("API Key 已配置，初始化 Provider Transport");

        // 使用 ProviderFactory 自动选择 Transport
        let transport = match crate::provider::create_main_transport(
            &config,
            config.provider_pool.circuit_breaker_threshold,
            config.provider_pool.circuit_breaker_cooldown_secs,
        ) {
            Ok(t) => {
                tracing::info!("Transport 已就绪: model={}, provider={}",
                    config.api.model,
                    config.agent.default_provider.as_str(),
                );
                t
            }
            Err(e) => {
                tracing::error!("Transport 创建失败: {e}");
                // fallback: 直接创建 DeepSeekTransport
                let transport: Arc<dyn crate::provider::Transport> = Arc::new(
                    crate::provider::ProviderPool::single(
                        Arc::new(crate::provider::DeepSeekTransport::new(&config)),
                        config.provider_pool.circuit_breaker_threshold,
                        config.provider_pool.circuit_breaker_cooldown_secs,
                    ),
                );
                transport
            }
        };

        app.init_api(config, transport.clone(), &path_mgr);

        // 设置全局 Transport（供子 Agent 工具使用）
        crate::tools::set_global_transport(transport);
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
