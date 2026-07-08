//! RHermes init 命令 —— 交互式初始化向导
//!
//! 引导用户完成首次配置：API Key、模型选择、部署方式。

use std::path::Path;

use dialoguer::{Confirm, Input, theme::ColorfulTheme};

use crate::core::Config;
use crate::core::PathManager;

/// 运行 init 初始化向导
pub fn run_init() -> Result<(), Box<dyn std::error::Error>> {
    println!();
    println!("┌────────────────────────────────────────────┐");
    println!("│         RHermes 初始化向导 v{}         │", env!("CARGO_PKG_VERSION"));
    println!("├────────────────────────────────────────────┤");
    println!("│  本向导将引导你完成首次配置：               │");
    println!("│  1. 创建数据目录                           │");
    println!("│  2. 选择 AI 提供商                         │");
    println!("│  3. 确认 API 地址                          │");
    println!("│  4. 配置 API Key                           │");
    println!("│  5. 查询并选择模型                         │");
    println!("└────────────────────────────────────────────┘");
    println!();

    // ── 检测现有配置 ──
    let existing_config = Config::load(
        PathManager::detect().config_path()
    ).unwrap_or_default();

    let existing_provider = existing_config.agent.default_provider.clone();

    // 显示已有配置概况（遍历 providers，而非顶层字段）
    if existing_config.is_configured() {
        println!("ℹ 检测到已有配置:");
        for (name, p) in &existing_config.providers {
            if p.api_key.is_empty() && name != "ollama" { continue; }
            let key_hint = if p.api_key.is_empty() {
                "(无 Key)".to_string()
            } else {
                format!("{}...{}",
                    &p.api_key[..3.min(p.api_key.len())],
                    &p.api_key[p.api_key.len().saturating_sub(4)..])
            };
            let model = p.model.as_deref().unwrap_or("-");
            let url = p.base_url.as_deref().unwrap_or("-");
            println!("   [{name}] Key: {key_hint}  模型: {model}  地址: {url}");
        }
        println!();
    }

    // ── 步骤 1: 创建数据目录 ──
    println!("【步骤 1/5】数据目录");
    println!("   📦 所有数据保存在 ./home/ 目录");
    println!();

    let cwd = std::env::current_dir()?;
    let home_dir = cwd.join("home");
    if home_dir.exists() {
        println!("   ℹ home/ 目录已存在: {}", home_dir.display());
    } else {
        println!("   📁 将创建 home/ 目录: {}", home_dir.display());
    }
    let path_mgr = PathManager::with_root(cwd.clone());
    path_mgr.ensure_dirs()?;

    println!();

    // ── 步骤 2: Provider 选择 ──
    println!("【步骤 2/5】选择 AI 提供商");
    println!();

    let provider_options = &[
        "deepseek    — DeepSeek V4 (默认，推荐)",
        "zhipu       — 智谱 GLM-4/GLM-5",
        "openai      — OpenAI GPT-4o 等",
        "siliconflow — 硅基流动（托管多种模型）",
        "ollama      — Ollama 本地模型",
        "lmstudio    — LM Studio 本地模型",
        "newapi      — New API / One API 代理",
        "其他 (自定义)",
    ];

    let default_provider_idx = match existing_provider.as_str() {
        "deepseek" => 0,
        "zhipu" => 1,
        "openai" => 2,
        "siliconflow" => 3,
        "ollama" => 4,
        "lmstudio" => 5,
        "newapi" => 6,
        _ => 0,
    };

    let provider_idx = dialoguer::Select::with_theme(&ColorfulTheme::default())
        .with_prompt("请选择 AI 提供商")
        .items(provider_options)
        .default(default_provider_idx)
        .interact()?;

    let (provider_name, default_base_url) = match provider_idx {
        0 => ("deepseek".to_string(), "https://api.deepseek.com"),
        1 => ("zhipu".to_string(), "https://open.bigmodel.cn/api/paas/v4"),
        2 => ("openai".to_string(), "https://api.openai.com/v1"),
        3 => ("siliconflow".to_string(), "https://api.siliconflow.cn/v1"),
        4 => ("ollama".to_string(), "http://localhost:11434/v1"),
        5 => ("lmstudio".to_string(), "http://localhost:1234/v1"),
        6 => ("newapi".to_string(), "http://localhost:3000/v1"),
        _ => {
            let theme = ColorfulTheme::default();
            let name: String = Input::with_theme(&theme)
                .with_prompt("输入提供商名称（如 moonshot、groq）")
                .default("custom".into())
                .interact_text()?;
            (name, "https://api.openai.com/v1")
        }
    };

    println!();

    // 从已有配置中提取当前选中 provider 的配置
    // （只读 providers[provider_name]，不碰其他 provider 的数据）
    let existing_p = existing_config.providers.get(&provider_name);
    let existing_api_key = existing_p
        .map(|p| p.api_key.clone())
        .unwrap_or_default();
    let existing_model = existing_p
        .and_then(|p| p.model.clone())
        .unwrap_or_default();
    let existing_base_url = existing_p
        .and_then(|p| p.base_url.clone())
        .unwrap_or_default();

    // ── 步骤 3: 确认 Base URL ──
    println!("【步骤 3/5】确认 API 地址");
    println!();

    // 已有 base_url 优先使用（来自当前 provider 的配置）
    let url_default = if !existing_base_url.is_empty() {
        existing_base_url.clone()
    } else {
        default_base_url.to_string()
    };

    let base_url: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("API 地址")
        .default(url_default)
        .interact_text()?;

    // 规范化：去掉末尾斜杠
    let base_url = base_url.trim_end_matches('/').to_string();

    println!();

    // ── 步骤 4: API Key ──
    // 本地服务（ollama / lmstudio）不需要 API Key，可跳过
    let is_local_provider = matches!(provider_name.as_str(), "ollama" | "lmstudio");

    let api_key: String = if is_local_provider {
        println!("【步骤 4/5】API Key（本地服务，可跳过）");
        let theme = ColorfulTheme::default();
        let input: String = Input::with_theme(&theme)
            .with_prompt("API Key（直接回车跳过）")
            .allow_empty(true)
            .interact_text()?;
        input.trim().to_string()
    } else {
        println!("【步骤 4/5】配置 {provider_name} API Key");
        match provider_name.as_str() {
            "deepseek" => println!("   获取地址: https://platform.deepseek.com/api_keys"),
            "zhipu" => println!("   获取地址: https://open.bigmodel.cn/usercenter/apikeys"),
            "openai" => println!("   获取地址: https://platform.openai.com/api-keys"),
            "siliconflow" => println!("   获取地址: https://cloud.siliconflow.cn/account/ak"),
            "newapi" => println!("   在 New API 管理面板创建令牌"),
            _ => {}
        }
        println!();

        // 是否有已有 Key 可保留
        let has_existing_key = !existing_api_key.is_empty();
        let key_hint = if has_existing_key {
            format!(
                "（当前: {}...{}，直接回车保留原值）",
                &existing_api_key[..3.min(existing_api_key.len())],
                &existing_api_key[existing_api_key.len().saturating_sub(4)..]
            )
        } else {
            String::new()
        };

        loop {
            let prompt_text = if provider_name == "deepseek" {
                format!("请输入你的 DeepSeek API Key{key_hint}")
            } else {
                format!("请输入你的 {provider_name} API Key{key_hint}")
            };
            let theme = ColorfulTheme::default();
            let input: String = Input::with_theme(&theme)
                .with_prompt(&prompt_text)
                .allow_empty(has_existing_key)
                .interact_text()?;

            let trimmed = input.trim().to_string();
            if trimmed.is_empty() && has_existing_key {
                break existing_api_key.clone();
            }

            if provider_name == "deepseek" {
                if trimmed.starts_with("sk-") && trimmed.len() >= 10 {
                    break trimmed;
                } else if trimmed.starts_with("sk-") {
                    println!("   ⚠ API Key 格式正确但长度偏短，请确认");
                    if Confirm::with_theme(&ColorfulTheme::default())
                        .with_prompt("继续使用?")
                        .default(true)
                        .interact()?
                    {
                        break trimmed;
                    }
                } else {
                    println!("   ⚠ API Key 应以 sk- 开头");
                    if Confirm::with_theme(&ColorfulTheme::default())
                        .with_prompt("继续使用?")
                        .default(false)
                        .interact()?
                    {
                        break trimmed;
                    }
                }
            } else {
                if trimmed.len() >= 8 {
                    break trimmed;
                } else {
                    println!("   ⚠ API Key 长度偏短，请确认");
                    if Confirm::with_theme(&ColorfulTheme::default())
                        .with_prompt("继续使用?")
                        .default(true)
                        .interact()?
                    {
                        break trimmed;
                    }
                }
            }
        }
    };

    println!();

    // ── 步骤 5: 查询并选择模型 ──
    println!("【步骤 5/5】选择模型");
    println!();

    let model = select_model(&base_url, &api_key, &existing_model, &provider_name)?;

    println!();

    // ── 步骤 6: 系统配置（每项可跳过）──
    println!("【系统配置】以下每项均可跳过，直接回车使用默认值");
    println!();

    // 以已有配置为基础
    let config_agent_base = existing_config.agent.clone();
    let mut agent_config = config_agent_base;
    let mut proxy_config = existing_config.proxy.clone();
    let mut telegram_config = existing_config.channels.telegram.clone();
    let mut wecom_config = existing_config.channels.wecom.clone();
    let mut wechat_config = existing_config.channels.wechat.clone();

    // ── 大项 A: 工作目录与安全 ──
    let configure_security = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("配置工作目录与命令安全？（限制 Agent 的文件操作范围）")
        .default(false)
        .interact()?;

    if configure_security {
        let ws_default = if !agent_config.workspace.is_empty() {
            agent_config.workspace.clone()
        } else {
            cwd.to_string_lossy().to_string()
        };
        let workspace: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("工作目录（Agent 文件操作限制在此目录，留空=不限制）")
            .default(ws_default)
            .allow_empty(true)
            .interact_text()?;
        agent_config.workspace = workspace.trim().to_string();

        let cmd_default = agent_config.command_allowed_prefixes.join(", ");
        let cmd_input: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("允许的命令前缀（逗号分隔，如 git,cargo,ls；留空=不限制）")
            .default(cmd_default)
            .allow_empty(true)
            .interact_text()?;
        agent_config.command_allowed_prefixes = if cmd_input.trim().is_empty() {
            Vec::new()
        } else {
            cmd_input.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        };
    }
    println!();

    // ── 大项 B: 网络代理 ──
    let configure_proxy = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("配置网络代理？（科学上网/内网穿透等）")
        .default(false)
        .interact()?;

    if configure_proxy {
        let proxy_mode_options = &[
            "auto — 按功能规则决定是否走代理（推荐）",
            "all  — 所有请求走代理",
            "off  — 不使用代理",
        ];
        let proxy_idx = dialoguer::Select::with_theme(&ColorfulTheme::default())
            .with_prompt("代理模式")
            .items(proxy_mode_options)
            .default(0)
            .interact()?;

        proxy_config.mode = match proxy_idx {
            0 => crate::core::ProxyMode::Auto,
            1 => crate::core::ProxyMode::All,
            _ => crate::core::ProxyMode::Off,
        };

        if matches!(proxy_config.mode, crate::core::ProxyMode::Auto | crate::core::ProxyMode::All) {
            let url_default = proxy_config.url.clone().unwrap_or_default();
            let url_input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("代理地址（如 socks5://127.0.0.1:1080 或 http://127.0.0.1:7890）")
                .default(url_default)
                .allow_empty(true)
                .interact_text()?;
            proxy_config.url = if url_input.trim().is_empty() { None } else { Some(url_input.trim().to_string()) };
        }
    }
    println!();

    // ── 大项 C: 消息通道（Telegram / 企业微信 / 微信个号）──
    let channel_options = &[
        "Telegram Bot    — 通过 Telegram 与 AI 对话",
        "企业微信 (WeCom) — 企业微信群机器人",
        "微信个号 (WeChat) — iLink Bot 扫码登录",
        "跳过（不配置任何通道）",
    ];

    let configure_channels = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("配置消息通道？（通过 IM 平台与 AI 对话）")
        .default(false)
        .interact()?;

    if configure_channels {
        loop {
            let ch_idx = dialoguer::Select::with_theme(&ColorfulTheme::default())
                .with_prompt("选择要配置的通道（可重复配置多个）")
                .items(channel_options)
                .default(3)
                .interact()?;

            match ch_idx {
                0 => {
                    // Telegram
                    telegram_config.enabled = true;
                    println!("   ℹ Bot Token 请写入 .env 文件: TELEGRAM_BOT_TOKEN=你的token");
                    let chats_input: String = Input::with_theme(&ColorfulTheme::default())
                        .with_prompt("允许的 chat_id（逗号分隔，留空=允许所有人）")
                        .default(telegram_config.allowed_chats.join(", "))
                        .allow_empty(true)
                        .interact_text()?;
                    telegram_config.allowed_chats = if chats_input.trim().is_empty() {
                        Vec::new()
                    } else {
                        chats_input.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                    };
                    println!("   ✅ Telegram 已启用");
                }
                1 => {
                    // 企业微信
                    wecom_config.enabled = true;
                    wecom_config.webhook_url = Input::with_theme(&ColorfulTheme::default())
                        .with_prompt("Webhook URL（企业微信群机器人地址）")
                        .default(wecom_config.webhook_url.clone())
                        .allow_empty(true)
                        .interact_text()?;
                    wecom_config.corp_id = Input::with_theme(&ColorfulTheme::default())
                        .with_prompt("企业 ID（corp_id，接收消息用，可留空）")
                        .default(wecom_config.corp_id.clone())
                        .allow_empty(true)
                        .interact_text()?;
                    wecom_config.agent_id = Input::with_theme(&ColorfulTheme::default())
                        .with_prompt("应用 Agent ID（可留空）")
                        .default(wecom_config.agent_id.clone())
                        .allow_empty(true)
                        .interact_text()?;
                    if !wecom_config.corp_id.is_empty() {
                        println!("   ℹ 应用 Secret 请写入 .env 文件: WECOM_SECRET=你的secret");
                    }
                    println!("   ✅ 企业微信已启用");
                }
                2 => {
                    // 微信个号
                    wechat_config.enabled = true;
                    if wechat_config.token_path.is_empty() {
                        wechat_config.token_path = "home/wechat_token.txt".to_string();
                    }
                    let poll_input: String = Input::with_theme(&ColorfulTheme::default())
                        .with_prompt("轮询间隔秒数")
                        .default(wechat_config.poll_interval_secs.to_string())
                        .interact_text()?;
                    if let Ok(n) = poll_input.trim().parse::<u64>() {
                        wechat_config.poll_interval_secs = n;
                    }

                    // 直接在 init 中扫码登录
                    let do_login = Confirm::with_theme(&ColorfulTheme::default())
                        .with_prompt("现在扫码登录微信？")
                        .default(true)
                        .interact()?;

                    if do_login {
                        println!("   ⏳ 正在获取二维码...");
                        let temp_config = Config {
                            channels: crate::core::ChannelsConfig {
                                wechat: wechat_config.clone(),
                                ..Default::default()
                            },
                            proxy: existing_config.proxy.clone(),
                            ..Default::default()
                        };
                        let ch = crate::channel::wechat::WeChatChannel::new(&temp_config);
                        match tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(ch.login_flow())
                        }) {
                            Ok(token) => {
                                ch.save_token(&token);
                                println!("   ✅ 微信扫码登录成功！");
                            }
                            Err(e) => {
                                println!("   ⚠ 扫码登录失败: {e}");
                                println!("   ℹ 可稍后通过 gateway start 时扫码登录");
                            }
                        }
                    } else {
                        println!("   ℹ 跳过扫码，启动 gateway 时会自动弹出二维码");
                    }
                    println!("   ✅ 微信个号已配置");
                }
                _ => break, // 跳过/完成
            }
            println!();
        }
    }
    println!();
    // 以已有 providers 为基础，保留其他 provider 的配置（如先配了 zhipu 再配 deepseek）
    let mut providers = existing_config.providers.clone();
    providers.insert(provider_name.clone(), crate::core::ProviderConfig {
        api_key: api_key.clone(),
        base_url: Some(base_url.clone()),
        model: Some(model.clone()),
        ..Default::default()
    });

    let mut config = Config {
        // api_key 字段仅在 deepseek 时同步，保持向后兼容
        // （save_api_key 已不再依赖此字段，但 load 时旧逻辑会读它）
        api_key: if provider_name == "deepseek" {
            api_key.clone()
        } else {
            existing_config.api_key.clone()
        },
        api: crate::core::ApiConfig {
            model: model.clone(),
            base_url: base_url.clone(),
        },
        providers,
        agent: crate::core::AgentConfig {
            default_provider: provider_name.clone(),
            ..agent_config
        },
        proxy: proxy_config,
        channels: crate::core::ChannelsConfig {
            telegram: telegram_config,
            wecom: wecom_config,
            wechat: wechat_config,
        },
        gateway: crate::core::GatewayConfig {
            enabled: true,
            channels: {
                // 根据各通道 enabled 状态自动构建 gateway.channels 列表
                let mut chs = Vec::new();
                // 复用已构建的 channels（但所有权已移走，重新检查 existing + 新值）
                // 这里从最终 channels 值重建
                chs
            },
            ..existing_config.gateway
        },
        ..existing_config
    };

    // 根据最终通道 enabled 状态更新 gateway.channels
    let mut gw_channels = Vec::new();
    if config.channels.telegram.enabled {
        gw_channels.push("telegram".to_string());
    }
    if config.channels.wechat.enabled {
        gw_channels.push("wechat".to_string());
    }
    if config.channels.wecom.enabled {
        gw_channels.push("wecom".to_string());
    }
    config.gateway.channels = gw_channels;

    let config_path = path_mgr.config_path();

    if let Err(e) = config.save(&config_path) {
        eprintln!("❌ 配置保存失败: {e}");
        return Err(e.into());
    }

    if let Err(e) = config.save_api_key(&config_path) {
        eprintln!("❌ API Key 保存失败: {e}");
        return Err(e.into());
    }

    let env_path = config_path.parent().unwrap_or(Path::new(".")).join(".env");

    println!("┌────────────────────────────────────────────┐");
    println!("│          ✅ 初始化完成！                    │");
    println!("├────────────────────────────────────────────┤");
    println!("│  数据目录: {}", path_mgr.data_root().display());
    println!("│  配置文件: {}", config_path.display());
    println!("│  密钥文件: {}", env_path.display());
    println!("│  提供商:   {}", provider_name);
    println!("│  API 地址: {}", base_url);
    println!("│  模型:     {}", config.api.model);
    println!("│  API Key:  {}...{}", &api_key[..5.min(api_key.len())], &api_key[api_key.len().saturating_sub(4)..]);
    println!("├────────────────────────────────────────────┤");
    println!("│  ✅ API Key 已保存到 .env 文件              │");
    println!("│  直接运行 rhermes 即可开始编程！           │");
    println!("└────────────────────────────────────────────┘");

    Ok(())
}

// ---------------------------------------------------------------------------
// 模型查询与选择
// ---------------------------------------------------------------------------

/// 调用 OpenAI 兼容的 GET /v1/models 端点获取可用模型列表
///
/// 对于支持 type/sub_type 查询参数的 Provider（如 SiliconFlow），
/// 会自动添加过滤参数只返回聊天模型。
async fn fetch_models(base_url: &str, api_key: &str, provider_name: &str) -> Result<Vec<String>, String> {
    // SiliconFlow 支持 ?type=text&sub_type=chat 过滤聊天模型
    let query = match provider_name {
        "siliconflow" => "?type=text&sub_type=chat",
        _ => "",
    };
    let url = format!("{}/models{query}", base_url.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败: {e}"))?;

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API 返回 {status}: {body}"));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 JSON 失败: {e}"))?;

    // OpenAI 标准格式: { "data": [ { "id": "model-name", ... }, ... ] }
    let models = json
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if models.is_empty() {
        return Err("API 返回的模型列表为空".into());
    }

    // 按字母排序
    let mut models = models;
    models.sort();
    Ok(models)
}

/// 选择模型：先尝试在线查询，失败则 fallback 到硬编码列表 + 自定义输入
fn select_model(
    base_url: &str,
    api_key: &str,
    existing_model: &str,
    provider_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // 尝试在线查询模型列表
    // 使用 block_in_place + Handle::current() 在已有 tokio 运行时中安全地 block_on，
    // 避免 "Cannot start a runtime from within a runtime" panic
    println!("   ⏳ 正在查询可用模型列表...");
    let online_models = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(fetch_models(base_url, api_key, provider_name))
    });

    let model = match online_models {
        Ok(models) => {
            println!("   ✅ 查询到 {} 个可用模型", models.len());
            println!();

            // 已有模型在列表中则默认选中
            let default_idx = models
                .iter()
                .position(|m| m == existing_model)
                .unwrap_or(0);

            let theme = ColorfulTheme::default();

            // 构建选项：全部模型 + 自定义输入
            let mut options: Vec<String> = models.clone();
            options.push("✏ 自定义输入模型名称".to_string());

            let custom_idx = options.len() - 1;

            let idx = dialoguer::Select::with_theme(&theme)
                .with_prompt("请选择默认模型（↑↓ 浏览）")
                .items(&options)
                .default(default_idx)
                .interact()?;

            if idx == custom_idx {
                // 自定义输入
                let default_name = if !existing_model.is_empty() {
                    existing_model.to_string()
                } else {
                    "deepseek-v4-flash".to_string()
                };
                Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("输入模型名称")
                    .default(default_name)
                    .interact_text()?
            } else {
                models[idx].clone()
            }
        }
        Err(e) => {
            println!("   ⚠ 在线查询失败: {e}");
            println!("   ↩ 回退到预设模型列表");
            println!();
            select_model_fallback(provider_name, existing_model)?
        }
    };

    Ok(model)
}

/// Fallback：使用硬编码的预设模型列表
fn select_model_fallback(
    provider_name: &str,
    existing_model: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let find_model_idx = |options: &[&str]| -> usize {
        for (i, opt) in options.iter().enumerate() {
            if let Some(name) = opt.split_once(' ').map(|(n, _)| n) {
                if name == existing_model {
                    return i;
                }
            }
        }
        0
    };

    let (model_options, default_idx): (Vec<&str>, usize) = match provider_name {
        "zhipu" => {
            let opts = vec![
                "glm-4-flash     — 轻量快速，适合日常",
                "glm-4-plus      — 更强能力，复杂任务",
                "glm-5           — 最新旗舰模型",
                "✏ 自定义模型名称",
            ];
            let idx = find_model_idx(&opts);
            (opts, idx)
        }
        "openai" => {
            let opts = vec![
                "gpt-4o          — 最强多模态 (推荐)",
                "gpt-4o-mini     — 轻量低成本",
                "o3-mini         — 推理模型",
                "✏ 自定义模型名称",
            ];
            let idx = find_model_idx(&opts);
            (opts, idx)
        }
        "siliconflow" => {
            let opts = vec![
                "deepseek-ai/DeepSeek-V3         — DeepSeek V3 (推荐)",
                "deepseek-ai/DeepSeek-R1         — DeepSeek R1 推理模型",
                "Qwen/Qwen2.5-Coder-32B-Instruct — Qwen 编程模型",
                "Pro/zai-org/GLM-4.7             — 智谱 GLM-4.7",
                "✏ 自定义模型名称",
            ];
            let idx = find_model_idx(&opts);
            (opts, idx)
        }
        _ => {
            // ollama / lmstudio / newapi / 自定义 provider → 直接输入模型名
            let default_name = if !existing_model.is_empty() {
                existing_model.to_string()
            } else {
                "llama3.2".to_string()
            };
            let model: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("输入模型名称")
                .default(default_name)
                .interact_text()?;
            return Ok(model);
        }
    };

    let theme = ColorfulTheme::default();
    let model_idx = dialoguer::Select::with_theme(&theme)
        .with_prompt("请选择默认模型")
        .items(&model_options)
        .default(default_idx)
        .interact()?;

    let model = if model_idx == model_options.len() - 1 {
        let default_name = if !existing_model.is_empty() {
            existing_model.to_string()
        } else {
            "deepseek-v4-flash".to_string()
        };
        Input::with_theme(&ColorfulTheme::default())
            .with_prompt("输入模型名称")
            .default(default_name)
            .interact_text()?
    } else {
        model_options[model_idx]
            .split_once(' ')
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| "deepseek-v4-flash".into())
    };

    Ok(model)
}
