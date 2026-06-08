//! MCP 交互式配置向导

use std::collections::HashMap;
use std::path::Path;

use dialoguer::{Confirm, Input, Select, theme::ColorfulTheme};

use crate::core::{Config, McpServerConfig};

/// 运行 MCP 交互式配置向导
pub fn run_mcp_setup(config_path: &Path) -> Result<(), String> {
    println!();
    println!("┌────────────────────────────────────────────┐");
    println!("│       RHermes MCP 配置向导 v{}         │", env!("CARGO_PKG_VERSION"));
    println!("├────────────────────────────────────────────┤");
    println!("│  MCP (Model Context Protocol) 让 AI 能够   │");
    println!("│  调用远程工具，如 GitHub API、文件系统等    │");
    println!("│                                            │");
    println!("│  你需要先安装 MCP Server 才能使用。         │");
    println!("│  常用 MCP Server:                           │");
    println!("│  · GitHub: npm @modelcontextprotocol/github │");
    println!("│  · 文件系统: npm @modelcontextprotocol/fs   │");
    println!("└────────────────────────────────────────────┘");
    println!();

    // ── 加载现有配置 ──
    let mut config = Config::load(config_path).unwrap_or_default();

    // 显示现有 MCP Server
    if !config.mcp.servers.is_empty() {
        println!("当前已配置的 MCP Server:");
        for (name, server) in &config.mcp.servers {
            let cmd = server.command.as_deref().unwrap_or("(SSE模式)");
            let args = server.args.join(" ");
            let args_str = if args.is_empty() { String::new() } else { format!(" {}", args) };
            println!("  · {name} → {cmd}{args_str}");
        }
        println!();

        if !Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("是否修改 MCP 配置?")
            .default(true)
            .interact().map_err(|e| e.to_string())?
        {
            println!("🛑 MCP 配置已取消");
            return Ok(());
        }
        println!();
    }

    // ── 步骤 1: 是否启用 MCP ──
    let enabled = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("启用 MCP?")
        .default(config.mcp.enabled || config.mcp.servers.is_empty())
        .interact().map_err(|e| e.to_string())?;
    println!();

    if !enabled {
        config.mcp.enabled = false;
        config.mcp.servers.clear();
        config.save(config_path).map_err(|e| format!("保存配置失败: {e}"))?;
        println!("✅ MCP 已禁用");
        return Ok(());
    }

    // ── 步骤 2: 添加 MCP Server ──
    let mut servers: HashMap<String, McpServerConfig> = config.mcp.servers.clone();

    loop {
        println!("【步骤 2/2】添加 MCP Server（可添加多个, 完成后选择「完成」）");
        println!();

        let actions = &[
            "添加新的 MCP Server",
            "完成配置",
        ];
        let choice = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("操作")
            .items(actions)
            .default(0)
            .interact().map_err(|e| e.to_string())?;

        if choice == 1 { break; }

        // 输入 Server 名称
        let name: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Server 名称（如 github, filesystem）")
            .default("".into())
            .interact_text().map_err(|e| e.to_string())?;
        if name.is_empty() {
            println!("⚠ Server 名称不能为空");
            continue;
        }

        // 选择连接方式
        let mode_options = &[
            "stdio 模式 — 启动本地子进程（推荐）",
            "SSE 模式 — 连接远程 HTTP URL",
        ];
        let mode_idx = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("连接方式")
            .items(mode_options)
            .default(0)
            .interact().map_err(|e| e.to_string())?;

        let mut server = McpServerConfig {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: None,
            message_url: None,
            headers: HashMap::new(),
            server_type: String::new(),
            parallel_safe: false,
            tool_parallel_safe: HashMap::new(),
        };

        if mode_idx == 0 {
            // stdio 模式
            let command: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("启动命令（如 npx、node、python）")
                .default("npx".into())
                .interact_text().map_err(|e| e.to_string())?;

            let args_str: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("命令参数（空格分隔，如 -y @modelcontextprotocol/server-github）")
                .default("".into())
                .interact_text().map_err(|e| e.to_string())?;

            let args: Vec<String> = if args_str.trim().is_empty() {
                Vec::new()
            } else {
                args_str.split(' ').map(|s| s.to_string()).collect()
            };

            server.command = Some(command);
            server.args = args;

            // 可选：环境变量
            let add_env = Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt("是否需要设置环境变量?")
                .default(false)
                .interact().map_err(|e| e.to_string())?;

            if add_env {
                loop {
                    let env_key: String = Input::with_theme(&ColorfulTheme::default())
                        .with_prompt("环境变量 KEY（留空结束）")
                        .default("".into())
                        .interact_text().map_err(|e| e.to_string())?;
                    if env_key.is_empty() { break; }

                    let env_val: String = Input::with_theme(&ColorfulTheme::default())
                        .with_prompt(&format!("环境变量 VALUE（{env_key}）"))
                        .default("".into())
                        .interact_text().map_err(|e| e.to_string())?;
                    server.env.insert(env_key, env_val);
                }
            }
        } else {
            // SSE 模式
            let url: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("SSE URL（如 http://localhost:3000/sse）")
                .default("".into())
                .interact_text().map_err(|e| e.to_string())?;
            if url.is_empty() {
                println!("⚠ SSE URL 不能为空");
                continue;
            }
            server.url = Some(url);
        }

        // 并行安全
        let parallel_safe = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("此 Server 的工具是否可安全并行执行?")
            .default(false)
            .interact().map_err(|e| e.to_string())?;
        server.parallel_safe = parallel_safe;

        servers.insert(name.clone(), server);
        println!("✅ MCP Server「{name}」已添加");
        println!();
    }

    // ── 保存配置 ──
    config.mcp.enabled = enabled;
    config.mcp.servers = servers;

    config.save(config_path).map_err(|e| format!("保存配置失败: {e}"))?;

    println!();
    println!("┌────────────────────────────────────────────┐");
    println!("│          ✅ MCP 配置完成！                   │");
    println!("├────────────────────────────────────────────┤");
    println!("│  MCP 状态: {}", if enabled { "✅ 已启用" } else { "⏹ 已禁用" });
    let count = config.mcp.servers.len();
    println!("│  已配置 Server: {} 个", count);
    for (name, server) in &config.mcp.servers {
        let mode = if server.command.is_some() { "stdio" } else { "SSE" };
        println!("│  · {name} ({mode})");
    }
    println!("├────────────────────────────────────────────┤");
    println!("│  📋 查看: rhermes mcp list                  │");
    println!("│  ❌ 删除: rhermes mcp remove <名称>          │");
    println!("│  🛠 重新配置: rhermes mcp setup              │");
    println!("└────────────────────────────────────────────┘");

    Ok(())
}
