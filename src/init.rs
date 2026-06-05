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
    println!("│  1. 选择部署方式                           │");
    println!("│  2. 选择 AI 提供商（DeepSeek/智谱等）       │");
    println!("│  3. 配置 API Key                           │");
    println!("│  4. 选择模型                               │");
    println!("│  5. 确认保存                               │");
    println!("└────────────────────────────────────────────┘");
    println!();

    // ── 步骤 1: 检测现有配置 ──
    let existing_config = Config::load(
        PathManager::detect().config_path()
    ).unwrap_or_default();

    if existing_config.is_configured() {
        println!("⚠ 检测到已有配置:");
        println!("   API Key: sk-...{}", &existing_config.api_key[existing_config.api_key.len().saturating_sub(4)..]);
        println!("   模型:    {}", existing_config.api.model);
        println!();

        if !Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("是否覆盖现有配置?")
            .default(false)
            .interact()?
        {
            println!("🛑 初始化已取消，现有配置保持不变。");
            return Ok(());
        }
        println!();
    }

    // ── 步骤 2: 便携式模式（唯一支持的部署方式） ──
    println!("【步骤 1/4】部署模式");
    println!("   📦 便携式模式 — 所有数据保存在 ./home/ 目录");
    println!();

    let cwd = std::env::current_dir()?;
    let home_dir = cwd.join("home");
    if home_dir.exists() {
        println!("   ℹ home/ 目录已存在: {}", home_dir.display());
    } else {
        println!("   📁 将创建 home/ 目录: {}", home_dir.display());
    }
    let path_mgr = PathManager::with_root(cwd);
    path_mgr.ensure_dirs()?;

    println!();

    // ── 步骤 3: Provider 选择 ──
    println!("【步骤 3/5】选择 AI 提供商");
    println!();

    let provider_options = &[
        "deepseek    — DeepSeek V4 (默认，推荐)",
        "zhipu       — 智谱 GLM-4/GLM-5",
        "openai      — OpenAI GPT-4o 等",
        "siliconflow — 硅基流动（托管多种模型）",
        "其他 (自定义)",
    ];

    let provider_idx = dialoguer::Select::with_theme(&ColorfulTheme::default())
        .with_prompt("请选择 AI 提供商")
        .items(provider_options)
        .default(0)
        .interact()?;

    let (provider_name, provider_base_url) = match provider_idx {
        0 => ("deepseek".to_string(), "https://api.deepseek.com".to_string()),
        1 => ("zhipu".to_string(), "https://open.bigmodel.cn/api/paas/v4".to_string()),
        2 => ("openai".to_string(), "https://api.openai.com/v1".to_string()),
        3 => ("siliconflow".to_string(), "https://api.siliconflow.cn/v1".to_string()),
        _ => {
            let name: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("输入提供商名称（如 moonshot、groq）")
                .default("custom".into())
                .interact_text()?;
            let url: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("输入 API 地址")
                .default("https://api.openai.com/v1".into())
                .interact_text()?;
            (name, url)
        }
    };

    println!();

    // ── 步骤 4: API Key ──
    println!("【步骤 4/5】配置 {provider_name} API Key");
    match provider_name.as_str() {
        "deepseek" => println!("   获取地址: https://platform.deepseek.com/api_keys"),
        "zhipu" => println!("   获取地址: https://open.bigmodel.cn/usercenter/apikeys"),
        "openai" => println!("   获取地址: https://platform.openai.com/api-keys"),
        "siliconflow" => println!("   获取地址: https://cloud.siliconflow.cn/"),
        _ => {}
    }
    println!();

    let api_key: String = loop {
        let prompt_text = if provider_name == "deepseek" {
            "请输入你的 DeepSeek API Key".to_string()
        } else {
            format!("请输入你的 {} API Key", provider_name)
        };
        let input: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt(&prompt_text)
            .allow_empty(false)
            .interact_text()?;

        let trimmed = input.trim().to_string();

        if provider_name == "deepseek" {
            // DeepSeek Key 以 sk- 开头
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
            // 其他 Provider 只要非空即可
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
    };

    println!();

    // ── 步骤 5: 模型选择 ──
    println!("【步骤 5/5】选择模型");
    println!();

    let (model_options, default_idx): (&[&str], usize) = match provider_name.as_str() {
        "zhipu" => (&[
            "glm-4-flash     — 轻量快速，适合日常",
            "glm-4-plus      — 更强能力，复杂任务",
            "glm-5           — 最新旗舰模型",
            "自定义模型名称",
        ], 0),
        "openai" => (&[
            "gpt-4o          — 最强多模态 (推荐)",
            "gpt-4o-mini     — 轻量低成本",
            "o3-mini         — 推理模型",
            "自定义模型名称",
        ], 0),
        "siliconflow" => (&[
            "deepseek-v4-flash — 通过硅基流动调用",
            "Qwen/Qwen2.5-Coder-32B-Instruct",
            "Pro/deepseek-ai/DeepSeek-V3",
            "自定义模型名称",
        ], 0),
        _ => (&[
            "deepseek-v4-flash  — 日常开发，成本最低 (推荐)",
            "deepseek-v4-pro    — 复杂任务，更强推理能力",
            "自定义模型名称",
        ], 0),
    };

    let model_idx = dialoguer::Select::with_theme(&ColorfulTheme::default())
        .with_prompt("请选择默认模型")
        .items(model_options)
        .default(default_idx)
        .interact()?;

    let model = match model_idx {
        x if x == model_options.len() - 1 => {
            // 自定义
            Input::with_theme(&ColorfulTheme::default())
                .with_prompt("输入模型名称")
                .default(model_options[0].split_once(' ').map(|(n, _)| n.to_string()).unwrap_or_else(|| "deepseek-v4-flash".into()))
                .interact_text()?
        }
        _ => {
            model_options[model_idx].split_once(' ').map(|(n, _)| n.to_string()).unwrap_or_else(|| "deepseek-v4-flash".into())
        }
    };

    println!();

    // ── 步骤 6: 可选配置 ──
    println!("【步骤 6/6】可选配置（直接回车使用默认值）");
    println!();

    let base_url: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("API 地址")
        .default(provider_base_url.clone())
        .interact_text()?;

    println!();

    // ── 保存配置 ──
    let mut providers = std::collections::HashMap::new();
    providers.insert(provider_name.clone(), crate::core::ProviderConfig {
        api_key: api_key.clone(),
        base_url: Some(base_url.clone()),
        model: Some(model.clone()),
        ..Default::default()
    });

    let config = Config {
        api_key: api_key.clone(),
        api: crate::core::ApiConfig {
            model: model.clone(),
            base_url: base_url.clone(),
        },
        providers,
        agent: crate::core::AgentConfig {
            default_provider: provider_name.clone(),
            ..Default::default()
        },
        ..Default::default()
    };

    let config_path = path_mgr.config_path();

    // 保存非敏感配置到 config.toml
    if let Err(e) = config.save(&config_path) {
        eprintln!("❌ 配置保存失败: {e}");
        return Err(e.into());
    }

    // 保存 API Key 到 .env
    if let Err(e) = config.save_api_key(&config_path) {
        eprintln!("❌ API Key 保存失败: {e}");
        return Err(e.into());
    }

    let env_path = config_path.parent().unwrap_or(Path::new(".")).join(".env");

    println!("┌────────────────────────────────────────────┐");
    println!("│          ✅ 初始化完成！                    │");
    println!("├────────────────────────────────────────────┤");
    println!("│  部署方式: 便携式模式 📦");
    println!("│  数据目录: {}", path_mgr.data_root().display());
    println!("│  配置文件: {}", config_path.display());
    println!("│  密钥文件: {}", env_path.display());
    println!("│  提供商:   {}", provider_name);
    println!("│  模型:     {}", config.api.model);
    println!("│  API Key:  {}...{}", &api_key[..5.min(api_key.len())], &api_key[api_key.len().saturating_sub(4)..]);
    println!("├────────────────────────────────────────────┤");
    println!("│  ✅ API Key 已保存到 .env 文件              │");
    println!("│  运行 rhermes code 开始编程！               │");
    println!("└────────────────────────────────────────────┘");

    Ok(())
}
