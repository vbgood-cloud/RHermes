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

    let _enable_wechat = selections == 0;
    let _enable_wecom = selections == 1;

    // 支持多选：由于 dialoguer Select 是单选，这里简化成每个通道单独确认
    let enable_wechat = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("启用 微信个号（扫码登录）?")
        .default(true)
        .interact().map_err(|e| e.to_string())?;

    let enable_wecom = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("启用 企业微信 Bot?")
        .default(false)
        .interact().map_err(|e| e.to_string())?;

    let enable_telegram = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("启用 Telegram Bot?")
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

    // 企业微信（D19：智能机器人长连接，免回调；不再使用 webhook/自建应用）
    let mut wecom = WeComConfig::default();
    let mut wecom_secret_to_save: Option<String> = None;
    if enable_wecom {
        println!("【步骤 2/3】配置企业微信通道（智能机器人长连接）");
        println!("  前置：企微管理后台 → 应用管理 → 智能机器人 → 创建机器人");
        println!("  ⚠ 后台必须选择「使用长连接」，不要配置回调地址（两者互斥）");
        println!();

        let bot_id: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("机器人 BotID")
            .default(String::new())
            .interact_text().map_err(|e| e.to_string())?;

        // Secret 用明文输入：粘贴时可核对完整性（853000 凭证错误多为复制不完整所致）
        let secret: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("机器人 Secret（明文显示以便核对；注意周边环境）")
            .default(String::new())
            .interact_text().map_err(|e| e.to_string())?;
        let mut keep_config = !secret.trim().is_empty();
        if !keep_config {
            println!("   ⚠ Secret 为空，已跳过企业微信配置");
            println!();
        } else {
            // 即时试连：真连官方网关做 subscribe 认证
            print!("⏳ 正在校验凭证（连接企微长连接网关）…");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            match crate::channel::wecom::try_connect_blocking(&bot_id, &secret) {
                Ok(()) => println!("\r✅ 凭证有效，机器人已可上线                    "),
                Err(e) => {
                    println!("\r⚠ 试连失败：{e}                    ");
                    keep_config = Confirm::with_theme(&ColorfulTheme::default())
                        .with_prompt("仍保存该配置？")
                        .default(false)
                        .interact().map_err(|e| e.to_string())?;
                    if !keep_config {
                        println!("   已跳过企业微信配置");
                    }
                    println!();
                }
            }
        }
        if keep_config {
            wecom.enabled = true;
            wecom.mode = crate::core::WeComMode::Stream;
            wecom.bot_id = bot_id;
            wecom_secret_to_save = Some(secret);
        }
        println!();
    }

    // Telegram
    let mut telegram = crate::core::TelegramConfig::default();
    if enable_telegram {
        println!("【步骤 2/3】配置 Telegram Bot 通道参数");
        println!("  ⚠ Bot Token 请在 .env 文件中设置 TELEGRAM_BOT_TOKEN");
        println!();

        let allowed_chats_str: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("允许的 chat_id 列表（逗号分隔，留空=全部允许）")
            .default("".into())
            .interact_text().map_err(|e| e.to_string())?;

        let poll_timeout: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Long Polling 超时（秒）")
            .default("30".into())
            .interact_text().map_err(|e| e.to_string())?;

        telegram.enabled = true;
        telegram.allowed_chats = allowed_chats_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        telegram.poll_timeout_secs = poll_timeout.parse().unwrap_or(30);

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
    config.channels.telegram = telegram;

    // 构建 channels 列表
    let mut channels = Vec::new();
    if enable_wechat { channels.push("wechat".into()); }
    if enable_wecom { channels.push("wecom".into()); }
    if enable_telegram { channels.push("telegram".into()); }
    config.gateway.channels = channels;

    config.gateway.pid_file = pid_file;
    config.gateway.log_file = log_file;

    config.save(config_path).map_err(|e| format!("保存配置失败: {e}"))?;

    // 智能机器人 Secret 写入 .env（D19 长连接；写入点已在校验后，此处兜底保存）
    if let Some(secret) = wecom_secret_to_save {
        crate::channel::wecom::write_bot_secret_to_env(config_path, &secret)?;
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
