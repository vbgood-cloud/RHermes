//! Gateway 交互式配置向导

use std::path::Path;

use dialoguer::{Confirm, Input, Select, theme::ColorfulTheme};

use crate::core::Config;
use crate::core::WeComConfig;
use crate::core::WeChatConfig;

/// 运行 Gateway 交互式配置向导
///
/// 引导用户配置：通道选择 → 通道参数 → Gateway 自身参数
pub fn run_gateway_setup(config_path: &Path) -> Result<(), String> {
    println!();
    println!("┌────────────────────────────────────────────┐");
    println!("│       RHermes Gateway 配置向导 v{}      │", env!("CARGO_PKG_VERSION"));
    println!("├────────────────────────────────────────────┤");
    println!("│  配置 Gateway 守护进程的通道和运行参数       │");
    println!("└────────────────────────────────────────────┘");
    println!();

    // ── 检测现有配置 ──
    let mut config = Config::load(config_path).unwrap_or_default();

    if config.is_configured() {
        println!("⚠ 检测到已有 AI 配置");
        if !Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("是否修改 Gateway 配置?")
            .default(true)
            .interact().map_err(|e| e.to_string())?
        {
            println!("🛑 配置已取消");
            return Ok(());
        }
        println!();
    }

    // ── 步骤 1: 选择要启用的通道 ──
    println!("【步骤 1/3】选择要启用的消息通道（可多选，空格选中后回车确认）");
    println!();

    let channel_options = &[
        "wechat  — 微信个号（扫码登录，推荐）",
        "wecom   — 企业微信 Bot",
    ];
    let selections = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("选择通道（上下键选择，回车确认；之后可用 gateway channel 命令修改）")
        .items(channel_options)
        .default(0)
        .interact().map_err(|e| e.to_string())?;

    let enable_wechat = selections == 0 || selections == 0; // 单选的 index
    let enable_wecom = selections == 1;

    // 支持多选：由于 dialoguer Select 是单选，这里简化成每个通道单独确认
    let enable_wechat = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("启用 微信个号（扫码登录）?")
        .default(true)
        .interact().map_err(|e| e.to_string())?;

    let enable_wecom = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("启用 企业微信 Bot?")
        .default(false)
        .interact().map_err(|e| e.to_string())?;

    println!();

    // ── 步骤 2: 配置各通道参数 ──

    // 微信个号
    let mut wechat = WeChatConfig::default();
    if enable_wechat {
        println!("【步骤 2/3】配置微信个号通道参数");
        println!();

        let token_path: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Token 保存路径（扫码成功后自动保存，下次免扫码）")
            .default("home/wechat_token.txt".into())
            .interact_text().map_err(|e| e.to_string())?;

        let poll_interval: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("消息轮询间隔（秒）")
            .default("2".into())
            .interact_text().map_err(|e| e.to_string())?;

        let proxy_use = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("是否需要代理?")
            .default(false)
            .interact().map_err(|e| e.to_string())?;

        let proxy = if proxy_use {
            Some(Input::with_theme(&ColorfulTheme::default())
                .with_prompt("代理地址")
                .default("http://127.0.0.1:7890".into())
                .interact_text().map_err(|e| e.to_string())?)
        } else {
            None
        };

        wechat.enabled = true;
        wechat.token_path = token_path;
        wechat.poll_interval_secs = poll_interval.parse().unwrap_or(2);
        wechat.proxy = proxy;

        println!();
    }

    // 企业微信
    let mut wecom = WeComConfig::default();
    if enable_wecom {
        println!("【步骤 2/3】配置企业微信通道参数");
        println!("  ⚠ 需要先在企业微信管理后台创建自建应用和群机器人");
        println!();

        let webhook_url: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("群机器人 Webhook URL")
            .default("".into())
            .interact_text().map_err(|e| e.to_string())?;

        println!();
        println!("  以下信息用于接收消息（企业微信自建应用）");
        let corp_id: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("企业 ID (CorpID)")
            .default("".into())
            .interact_text().map_err(|e| e.to_string())?;

        let agent_id: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("应用 AgentId")
            .default("".into())
            .interact_text().map_err(|e| e.to_string())?;

        println!();
        println!("  Secret 将保存到 .env 文件（不会写入 config.toml）");
        let secret: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("应用 Secret")
            .default("".into())
            .interact_text().map_err(|e| e.to_string())?;

        let poll_interval: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("消息轮询间隔（秒）")
            .default("5".into())
            .interact_text().map_err(|e| e.to_string())?;

        wecom.enabled = true;
        wecom.webhook_url = webhook_url;
        wecom.corp_id = corp_id;
        wecom.agent_id = agent_id;
        wecom.secret = secret;
        wecom.poll_interval_secs = poll_interval.parse().unwrap_or(5);

        println!();
    }

    // ── 步骤 3: Gateway 自身参数 ──
    println!("【步骤 3/3】Gateway 进程参数");
    println!();

    let pid_file: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("PID 文件路径（用于 stop/status 命令）")
        .default("home/gateway.pid".into())
        .interact_text().map_err(|e| e.to_string())?;

    let log_file: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("日志文件路径")
        .default("home/gateway.log".into())
        .interact_text().map_err(|e| e.to_string())?;

    println!();

    // ── 保存配置 ──
    config.channels.wechat = wechat;
    config.channels.wecom = wecom;
    config.gateway.pid_file = pid_file;
    config.gateway.log_file = log_file;

    let mut channels = Vec::new();
    if enable_wechat { channels.push("wechat".into()); }
    if enable_wecom { channels.push("wecom".into()); }
    config.gateway.channels = channels;

    config.save(config_path).map_err(|e| format!("保存配置失败: {e}"))?;

    // 保存 Secret 到 .env（仅企业微信）
    if enable_wecom && !config.channels.wecom.secret.is_empty() {
        let env_content = format!("WECOM_SECRET={}\n", config.channels.wecom.secret);
        let env_path = config_path.parent().unwrap_or(Path::new(".")).join(".env");
        // 追加，不覆盖已有的 API Key
        let mut existing = std::fs::read_to_string(&env_path).unwrap_or_default();
        if !existing.contains("WECOM_SECRET") {
            existing.push_str(&env_content);
            std::fs::write(&env_path, existing).map_err(|e| format!("写入 .env 失败: {e}"))?;
        }
    }

    println!("┌────────────────────────────────────────────┐");
    println!("│          ✅ Gateway 配置完成！               │");
    println!("├────────────────────────────────────────────┤");
    println!("│  通道:");
    if enable_wechat { println!("│     ✅ 微信个号"); }
    if enable_wecom { println!("│     ✅ 企业微信"); }
    println!("│  PID 文件: {}", config.gateway.pid_file);
    println!("│  日志文件: {}", config.gateway.log_file);
    println!("├────────────────────────────────────────────┤");
    println!("│  ▶ 启动: rhermes gateway start              │");
    println!("│  ⏹ 停止: rhermes gateway stop               │");
    println!("│  📊 状态: rhermes gateway status             │");
    println!("│  📡 通道: rhermes gateway channel list       │");
    println!("└────────────────────────────────────────────┘");

    Ok(())
}
