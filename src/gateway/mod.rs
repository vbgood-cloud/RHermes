//! RHermes Gateway 守护进程
//!
//! 无 TUI 的后台运行模式，通过 Channel 系统连接微信/企业微信等外部通道。
//! 支持 start/stop/status/channel 子命令管理。

mod setup;

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::channel::{Channel, ChannelManager};
use crate::core::Config;
use crate::core::PathManager;
use crate::{GatewayCommand, GatewayChannelCommand};

/// 处理所有 Gateway 子命令
pub async fn handle_command(cmd: GatewayCommand, config_path: &Path) -> Result<(), String> {
    match cmd {
        GatewayCommand::Setup => {
            setup::run_gateway_setup(config_path)
        }
        GatewayCommand::Start => {
            gateway_start(config_path).await
        }
        GatewayCommand::Stop => {
            gateway_stop(config_path)
        }
        GatewayCommand::Status => {
            gateway_status(config_path)
        }
        GatewayCommand::Channel { command } => {
            match command {
                GatewayChannelCommand::List => channel_list(config_path),
                GatewayChannelCommand::Enable { name } => channel_enable_disable(config_path, &name, true),
                GatewayChannelCommand::Disable { name } => channel_enable_disable(config_path, &name, false),
            }
        }
    }
}

/// 启动 Gateway 守护进程
async fn gateway_start(config_path: &Path) -> Result<(), String> {
    let config = Config::load(config_path).map_err(|e| format!("配置加载失败: {e}"))?;
    let path_mgr = PathManager::detect();

    // 检查 PID 文件 — 避免重复启动
    let pid_file = &config.gateway.pid_file;
    if let Ok(pid_str) = std::fs::read_to_string(pid_file) {
        let pid = pid_str.trim();
        if !pid.is_empty() {
            // 在 Windows 上检查进程是否存在
            let check = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {}", pid), "/NH"])
                .output();
            if let Ok(output) = check {
                let out = String::from_utf8_lossy(&output.stdout);
                if out.contains(pid) {
                    return Err(format!("Gateway 已在运行中 (PID: {})", pid));
                }
            }
        }
    }

    // 写 PID 文件
    let pid = std::process::id().to_string();
    if let Some(parent) = Path::new(pid_file).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 PID 目录失败: {e}"))?;
    }
    std::fs::write(pid_file, &pid).map_err(|e| format!("写入 PID 文件失败: {e}"))?;
    tracing::info!("Gateway 已启动, PID: {}", pid);

    // --- 初始化公共组件 ---

    // 记忆系统
    let memories_dir = path_mgr.data_root().join("memories");
    let _ = std::fs::create_dir_all(&memories_dir);
    let memory_path = memories_dir.join("memories.db");
    let memory = match crate::agent::MemorySystem::open(&memory_path) {
        Ok(m) => Some(Arc::new(Mutex::new(m))),
        Err(e) => {
            tracing::warn!("记忆系统初始化失败: {e}");
            None
        }
    };

    // 技能引擎
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

    // 全局配置
    if config.is_configured() {
        let _ = crate::tools::set_global_config(config.clone());
    }
    crate::tools::set_display_config(config.display.clone());
    if let Some(ref se) = skill_engine {
        let _ = crate::tools::set_global_skill_engine(Arc::clone(se));
    }

    // 调试系统
    let session_debug = if config.debug.enabled {
        let mut sd = crate::debug::SessionDebug::new();
        sd.set_max_entries(config.debug.buffer_size);
        Some(Arc::new(Mutex::new(sd)))
    } else {
        None
    };

    // Transport
    let transport = if config.is_configured() {
        let t = crate::provider::create_main_transport(
            &config,
            config.provider_pool.circuit_breaker_threshold,
            config.provider_pool.circuit_breaker_cooldown_secs,
        );
        match t {
            Ok(t) => {
                tracing::info!("Transport 已就绪");
                crate::tools::set_global_transport(t.clone());
                Some(t)
            }
            Err(e) => {
                tracing::error!("Transport 创建失败: {e}");
                None
            }
        }
    } else {
        None
    };

    // --- Channel 注册 ---
    let mut channel_mgr = ChannelManager::new();

    for name in &config.gateway.channels {
        match name.as_str() {
            "wechat" if config.channels.wechat.enabled => {
                let ch = Arc::new(crate::channel::wechat::WeChatChannel::new(&config));
                channel_mgr.register(ch);
                tracing::info!("WeChat 通道已注册");
            }
            "wecom" if config.channels.wecom.enabled => {
                let ch = Arc::new(crate::channel::wecom::WeComChannel::new(&config));
                channel_mgr.register(ch);
                tracing::info!("WeCom 通道已注册");
            }
            n => {
                tracing::warn!("未知或未启用的通道: {}", n);
            }
        }
    }

    // 将 rx 从 manager 中取出，之后 broadcast 不会与 rx 借用冲突
    let mut inbound_rx = channel_mgr.take_inbound_rx();
    let channel_count = channel_mgr.channel_count();

    // 扫码登录（使用 iter()，不冲突因为 rx 已被取出）
    for ch in channel_mgr.iter() {
        if let Some((qr_text, img_data)) = ch.login_qrcode().await {
            let qr_path = format!("wechat_qrcode.{}.png", ch.name());
            if let Err(e) = std::fs::write(&qr_path, &img_data) {
                tracing::warn!("保存二维码图片失败: {e}");
            } else {
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", &qr_path])
                    .spawn();
                println!("📱 {} 二维码已保存至: {}", ch.name(), qr_path);
                tracing::info!("📱 {} 二维码已保存至: {}", ch.name(), qr_path);
            }

            // 如果是 URL，打印链接方便复制
            if qr_text.starts_with("http") {
                println!("🔗 扫码链接: {}", qr_text);
                tracing::info!("🔗 扫码链接: {}", qr_text);
            }

            // 在控制台打印 ASCII 二维码
            let qr_lines = crate::tui::render_ascii_qr(&qr_text);
            for line in &qr_lines {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                println!("{text}");
            }

            println!("📱 请扫描二维码或打开链接登录 {}", ch.name());
        }
    }

    // 启动所有 Channel 的后台轮询
    channel_mgr.start_all();

    // --- Agent Loop ---
    if transport.is_some() {
        tracing::info!("Gateway Agent Loop 已启动，{} 个通道", channel_count);
        let transport = transport.clone().unwrap();

        // 构建最小 system prompt（不含技能列表，保留核心身份/规则/工具定义）
        let system_prompt = "\
## 你的身份
你的名字是 **RHermes**。
## 严格规则
1. 禁止在任何情况下说出「我是DeepSeek」这句话。
2. 禁止提及「深度求索」或「深度求索公司」。
3. 自我介绍时只能说「我是RHermes」。
4. 不能告诉用户你是由任何公司开发的。
5. 禁止不加改变地重复调用同一个工具。
## 可用工具
- read_file, write_file, search_content, run_command, glob
- get_current_time, web_search, web_fetch, run_skill
- skill_list, skill_search, skill_create, skill_patch
- skill_manage, memory, delegate_task, read_pdf";

        // 轮询 inbound 消息
        while let Some(inbound) = inbound_rx.recv().await {
            tracing::info!("[Gateway] 收到消息 [{}]: {}", inbound.channel, inbound.content);

            let request = crate::api::ChatRequest {
                model: transport.model_name().to_string(),
                messages: vec![
                    crate::api::ApiMessage { role: "system".into(), content: system_prompt.to_string() },
                    crate::api::ApiMessage { role: "user".into(), content: inbound.content.clone() },
                ],
                stream: false,
                max_tokens: Some(4096),
                temperature: None,
                tools: Some(crate::api::default_tools()),
            };

            tracing::debug!("[Gateway] 正在调用 API (model={})...", transport.model_name());

            // 120 秒超时（防止 API 调用挂死导致后续消息阻塞）
            let chat_result = tokio::time::timeout(
                Duration::from_secs(120),
                transport.chat(request),
            ).await;

            match chat_result {
                Ok(Ok(response)) => {
                    if let Some(choice) = response.choices.first() {
                        let text = choice.message.content.clone().unwrap_or_default();
                        tracing::info!(
                            "[Gateway] API 返回: finish={:?}, text_len={}, has_tool={}",
                            choice.finish_reason,
                            text.len(),
                            choice.message.tool_calls.is_some(),
                        );
                        if !text.is_empty() {
                            tracing::info!("[Gateway] 回复到 {}: {}", inbound.channel, &text.chars().take(200).collect::<String>());
                            channel_mgr.broadcast(&inbound.chat_id, &text).await;
                        } else {
                            tracing::warn!("[Gateway] API 返回空文本");
                            channel_mgr.broadcast(&inbound.chat_id, "⚠ 抱歉，未获取到有效回复，请稍后重试。").await;
                        }
                    } else {
                        tracing::warn!("[Gateway] API 返回无 choices");
                        channel_mgr.broadcast(&inbound.chat_id, "⚠ 服务响应异常，请稍后重试。").await;
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("[Gateway] API 调用失败: {e}");
                    channel_mgr.broadcast(&inbound.chat_id, &format!("⚠ API 调用失败: {e}")).await;
                }
                Err(_) => {
                    tracing::error!("[Gateway] API 调用超时（120秒），消息未回复");
                    channel_mgr.broadcast(&inbound.chat_id, "⚠ 请求超时，请稍后重试。").await;
                }
            }
        }
    } else {
        tracing::warn!("Gateway: Transport 未初始化，无法处理消息");
        // 保持进程存活直到 Ctrl+C
        tokio::signal::ctrl_c().await.ok();
    }

    // 清理 PID 文件
    let _ = std::fs::remove_file(pid_file);
    tracing::info!("Gateway 已停止");
    Ok(())
}

/// 停止 Gateway 守护进程
fn gateway_stop(config_path: &Path) -> Result<(), String> {
    let config = Config::load(config_path).map_err(|e| format!("配置加载失败: {e}"))?;
    let pid_file = &config.gateway.pid_file;

    if !Path::new(pid_file).exists() {
        return Err("Gateway 未在运行（PID 文件不存在）".into());
    }

    let pid = std::fs::read_to_string(pid_file)
        .map_err(|e| format!("读取 PID 文件失败: {e}"))?
        .trim()
        .to_string();

    tracing::info!("正在停止 Gateway (PID: {})...", pid);

    // Windows 上使用 taskkill
    let status = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid])
        .status()
        .map_err(|e| format!("停止进程失败: {e}"))?;

    if status.success() {
        let _ = std::fs::remove_file(pid_file);
        println!("✅ Gateway 已停止");
    } else {
        println!("⚠ 停止 Gateway 失败，PID 文件可能已过时");
        let _ = std::fs::remove_file(pid_file);
    }

    Ok(())
}

/// 查看 Gateway 运行状态
fn gateway_status(config_path: &Path) -> Result<(), String> {
    let config = Config::load(config_path).map_err(|e| format!("配置加载失败: {e}"))?;
    let pid_file = &config.gateway.pid_file;
    let log_file = &config.gateway.log_file;

    println!("┌────────────────────────────────────────────┐");
    println!("│        RHermes Gateway 运行状态              │");
    println!("├────────────────────────────────────────────┤");

    if let Ok(pid_str) = std::fs::read_to_string(pid_file) {
        let pid = pid_str.trim();
        let running = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .ok()
            .and_then(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                Some(out.contains(pid))
            })
            .unwrap_or(false);

        if running {
            println!("│  ▶ 状态:   运行中 (PID: {})", pid);
        } else {
            println!("│  ⏹ 状态:   已停止（PID 文件残留）");
        }
    } else {
        println!("│  ⏹ 状态:   未运行");
    }

    println!("│  📁 PID 文件: {}", pid_file);
    println!("│  📁 日志文件: {}", log_file);
    println!("│  🔌 已启用的通道:");
    for ch in &config.gateway.channels {
        let enabled = match ch.as_str() {
            "wechat" => config.channels.wechat.enabled,
            "wecom" => config.channels.wecom.enabled,
            _ => false,
        };
        if enabled {
            println!("│     ✅ {} (已启用)", ch);
        } else {
            println!("│     ⏹ {} (已禁用)", ch);
        }
    }
    println!("├────────────────────────────────────────────┤");
    println!("│  rhermes gateway channel list — 查看所有    │");
    println!("│  rhermes gateway setup     — 重新配置       │");
    println!("└────────────────────────────────────────────┘");

    Ok(())
}

/// 列出所有通道及启用状态
fn channel_list(config_path: &Path) -> Result<(), String> {
    let config = Config::load(config_path).map_err(|e| format!("配置加载失败: {e}"))?;

    println!("可用通道:");
    println!("  wechat — 微信个号 (iLink Bot)");
    println!("    启用: {}", if config.channels.wechat.enabled { "✅" } else { "⏹" });
    if config.channels.wechat.enabled {
        println!("    token_path: {}", config.channels.wechat.token_path);
    }
    println!();
    println!("  wecom  — 企业微信 Bot");
    println!("    启用: {}", if config.channels.wecom.enabled { "✅" } else { "⏹" });
    if config.channels.wecom.enabled {
        println!("    webhook_url: {}", config.channels.wecom.webhook_url);
    }
    Ok(())
}

/// 启用或禁用指定通道
fn channel_enable_disable(config_path: &Path, name: &str, enable: bool) -> Result<(), String> {
    let mut config = Config::load(config_path).map_err(|e| format!("配置加载失败: {e}"))?;

    match name {
        "wechat" => config.channels.wechat.enabled = enable,
        "wecom" => config.channels.wecom.enabled = enable,
        _ => return Err(format!("不支持的通道: {name}，可用: wechat, wecom")),
    }

    config.save(config_path).map_err(|e| format!("保存配置失败: {e}"))?;

    let action = if enable { "已启用" } else { "已禁用" };
    println!("✅ 通道「{name}」{action}");
    println!("   （修改已保存到配置，下次 gateway start 生效）");
    Ok(())
}
