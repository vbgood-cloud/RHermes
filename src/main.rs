//! RHermes — Reasonix 的省 Token 肌肉 + Hermes 的自进化大脑 + Rust 的零开销骨架
//!
//! 终端 AI 编程 Agent，融合极致 Token 缓存优化与自进化学习闭环。

mod api;
mod config;
mod context;
mod dispatcher;
mod path;
mod tool;
mod tools;
mod tui;

use dispatcher::ToolDispatcher;
use path::PathManager;
use tools::builtin_registry;
use tui::App;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

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
    let config = match config::Config::load(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[RHermes] 配置加载失败: {e}");
            config::Config::default()
        }
    };

    // 创建 TUI（传入 dispatcher）
    let mut app = App::new(path_mgr.mode().name(), dispatcher);

    // 如果已有 API Key，初始化 API 客户端
    if config.is_configured() {
        tracing::info!("API Key 已配置，初始化 DeepSeek 客户端");
        app.init_api(config, &path_mgr);
    } else {
        tracing::warn!("未检测到 API Key，运行在模拟模式");
        let _ = config;

        app.messages.push(tui::Message::system(
            "⚠ 未检测到 API Key。请创建配置文件或设置环境变量。",
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
