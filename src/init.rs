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
    println!("│  2. 配置 DeepSeek API Key                  │");
    println!("│  3. 选择模型                               │");
    println!("│  4. 确认保存                               │");
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

    // ── 步骤 3: API Key ──
    println!("【步骤 2/4】配置 DeepSeek API Key");
    println!("   获取地址: https://platform.deepseek.com/api_keys");
    println!();

    let api_key: String = loop {
        let input: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("请输入你的 DeepSeek API Key")
            .allow_empty(false)
            .interact_text()?;

        let trimmed = input.trim().to_string();

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
    };

    println!();

    // ── 步骤 4: 模型选择 ──
    println!("【步骤 3/4】选择模型");
    println!();

    let model_options = &[
        "deepseek-v4-flash  — 日常开发，成本最低 (推荐)",
        "deepseek-v4-pro    — 复杂任务，更强推理能力",
        "自定义模型名称",
    ];

    let model_idx = dialoguer::Select::with_theme(&ColorfulTheme::default())
        .with_prompt("请选择默认模型")
        .items(model_options)
        .default(0)
        .interact()?;

    let model = match model_idx {
        0 => "deepseek-v4-flash".to_string(),
        1 => "deepseek-v4-pro".to_string(),
        _ => {
            Input::with_theme(&ColorfulTheme::default())
                .with_prompt("输入模型名称")
                .default("deepseek-v4-flash".into())
                .interact_text()?
        }
    };

    println!();

    // ── 步骤 5: 可选配置 ──
    println!("【步骤 4/4】可选配置");
    println!();

    let base_url: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("API 地址（使用默认请直接回车）")
        .default("https://api.deepseek.com".into())
        .interact_text()?;

    println!();

    // ── 保存配置 ──
    let config = Config {
        api_key: api_key.clone(),
        api: crate::core::ApiConfig {
            model,
            base_url,
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
    println!("│  模型:     {}", config.api.model);
    println!("│  API Key:  sk-...{}", &api_key[api_key.len().saturating_sub(4)..]);
    println!("├────────────────────────────────────────────┤");
    println!("│  ✅ API Key 已保存到 .env 文件              │");
    println!("│  运行 rhermes code 开始编程！               │");
    println!("└────────────────────────────────────────────┘");

    Ok(())
}
