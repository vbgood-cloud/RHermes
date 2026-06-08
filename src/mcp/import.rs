//! MCP 配置导入 — 解析标准 JSON 格式并写入 config.toml
//!
//! 支持从文件导入或交互粘贴 JSON 内容。
//!
//! 支持的 JSON 格式：
//! ```json
//! {
//!   "mcpServers": {
//!     "server-name": {
//!       "type": "http",
//!       "url": "https://...",
//!       "headers": { "Authorization": "Bearer xxx" },
//!       "command": "npx",
//!       "args": ["..."],
//!       "env": { "KEY": "VALUE" }
//!     }
//!   }
//! }
//! ```

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use crate::core::{Config, McpServerConfig};

/// 导入标准 JSON 配置文件
pub fn import_from_file(json_path: &str, config_path: &Path, force: bool) -> Result<(), String> {
    let content = std::fs::read_to_string(json_path)
        .map_err(|e| format!("读取文件失败: {e}"))?;
    parse_and_import(&content, config_path, force)
}

/// 交互式粘贴 JSON 配置
///
/// 用系统编辑器（记事本/vim）打开临时文件，用户粘贴 JSON 后保存即可。
pub fn import_interactive(config_path: &Path, force: bool) -> Result<(), String> {
    println!("┌────────────────────────────────────────────┐");
    println!("│     📥 MCP 配置粘贴导入                      │");
    println!("├────────────────────────────────────────────┤");
    println!("│  将打开系统文本编辑器，请粘贴 JSON 配置后    │");
    println!("│  保存并关闭编辑器（File → Save → Exit）。    │");
    println!("│                                            │");
    println!("│  格式示例:                                  │");
    println!("│  {{\"mcpServers\":{{                          │");
    println!("│    \"name\":{{\"url\":\"...\",\"headers\":{{}}}}  │");
    println!("│  }}}}                                       │");
    println!("└────────────────────────────────────────────┘");
    println!();

    // 创建临时文件写入模板
    let tmp_path = std::env::temp_dir().join("rhermes-mcp-import.json");
    let template = "{\n  \"mcpServers\": {\n    \"server-name\": {\n      \"type\": \"http\",\n      \"url\": \"https://...\",\n      \"headers\": {\n        \"Authorization\": \"Bearer xxx\"\n      }\n    }\n  }\n}\n";
    let mut file = std::fs::File::create(&tmp_path)
        .map_err(|e| format!("创建临时文件失败: {e}"))?;
    file.write_all(template.as_bytes())
        .map_err(|e| format!("写入模板失败: {e}"))?;
    drop(file);

    // 用系统编辑器打开
    if let Err(e) = open_editor(&tmp_path) {
        return Err(format!("打开编辑器失败: {e}"));
    }

    // 读取编辑后的内容
    let content = std::fs::read_to_string(&tmp_path)
        .map_err(|e| format!("读取编辑内容失败: {e}"))?;

    // 清理临时文件
    let _ = std::fs::remove_file(&tmp_path);

    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed == template.trim() {
        return Err("内容未修改或为空".into());
    }

    parse_and_import(trimmed, config_path, force)
}

/// 跨平台打开系统编辑器
#[cfg(target_os = "windows")]
fn open_editor(path: &Path) -> Result<(), String> {
    let status = std::process::Command::new("notepad")
        .arg(path)
        .status()
        .map_err(|e| format!("启动记事本失败: {e}"))?;
    if !status.success() {
        return Err("编辑器未正常退出".into());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn open_editor(path: &Path) -> Result<(), String> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .map_err(|e| format!("启动编辑器失败: {e}"))?;
    if !status.success() {
        return Err("编辑器未正常退出".into());
    }
    Ok(())
}

/// 解析 JSON 内容并导入到配置
fn parse_and_import(content: &str, config_path: &Path, force: bool) -> Result<(), String> {
    let json: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("JSON 解析失败: {e}"))?;

    let servers_obj = json.get("mcpServers")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "JSON 中缺少 'mcpServers' 字段".to_string())?;

    if servers_obj.is_empty() {
        return Err("mcpServers 为空".into());
    }

    let mut config = Config::load(config_path).unwrap_or_default();
    let mut imported: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut overwritten: Vec<String> = Vec::new();

    for (name, server_json) in servers_obj {
        if config.mcp.servers.contains_key(name) {
            if force {
                // 覆盖模式：先移除再插入
                config.mcp.servers.remove(name);
                match parse_mcp_server(server_json) {
                    Ok(sc) => {
                        config.mcp.servers.insert(name.clone(), sc);
                        overwritten.push(name.clone());
                    }
                    Err(e) => {
                        skipped.push(format!("{name} ({e})"));
                    }
                }
            } else {
                skipped.push(name.clone());
            }
            continue;
        }
        match parse_mcp_server(server_json) {
            Ok(sc) => {
                config.mcp.servers.insert(name.clone(), sc);
                imported.push(name.clone());
            }
            Err(e) => {
                skipped.push(format!("{name} ({e})"));
            }
        }
    }

    if !imported.is_empty() || !overwritten.is_empty() {
        config.mcp.enabled = true;
    }
    config.save(config_path).map_err(|e| format!("保存配置失败: {e}"))?;

    // 打印结果
    println!();
    println!("┌────────────────────────────────────────────┐");
    println!("│          📥 MCP 导入完成！                   │");
    println!("├────────────────────────────────────────────┤");
    if !imported.is_empty() {
        println!("│  ✅ 成功导入:");
        for name in &imported { println!("│     · {name}"); }
    }
    if !overwritten.is_empty() {
        println!("│  🔄 已覆盖:");
        for name in &overwritten { println!("│     · {name}"); }
    }
    if !skipped.is_empty() {
        println!("│  ⏭️ 跳过:");
        for name in &skipped { println!("│     · {name}"); }
    }
    println!("│  MCP 状态: {}", if config.mcp.enabled { "✅ 已启用" } else { "⏹ 已禁用" });
    println!("│  已配置 Server: {} 个", config.mcp.servers.len());
    println!("│  🔄 重启程序后生效                          │");
    println!("└────────────────────────────────────────────┘");

    Ok(())
}

/// 解析单个 MCP Server 配置
fn parse_mcp_server(server: &serde_json::Value) -> Result<McpServerConfig, String> {
    let mut sc = McpServerConfig {
        command: None,
        args: Vec::new(),
        env: HashMap::new(),
        url: None,
        message_url: None,
        headers: HashMap::new(),
        server_type: String::new(),
        parallel_safe: false,
        tool_parallel_safe: std::collections::HashMap::new(),
    };

    // 判断类型
    let server_type = server.get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    sc.server_type = server_type.to_string();

    // URL（HTTP 或 SSE 模式）
    if let Some(url) = server.get("url").and_then(|v| v.as_str()) {
        sc.url = Some(url.to_string());
    }

    // message_url（消息发送地址，Direct 模式使用）
    if let Some(msg_url) = server.get("messageUrl")
        .or_else(|| server.get("message_url"))
        .and_then(|v| v.as_str())
    {
        if !msg_url.is_empty() {
            sc.message_url = Some(msg_url.to_string());
        }
    }

    // Headers（HTTP 模式）
    if let Some(headers) = server.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(val) = v.as_str() {
                sc.headers.insert(k.clone(), val.to_string());
            }
        }
    }

    // Command + Args（stdio 模式）
    if let Some(cmd) = server.get("command").and_then(|v| v.as_str()) {
        sc.command = Some(cmd.to_string());
    }
    if let Some(args) = server.get("args").and_then(|v| v.as_array()) {
        for arg in args {
            if let Some(s) = arg.as_str() {
                sc.args.push(s.to_string());
            }
        }
    }

    // 环境变量
    if let Some(env) = server.get("env").and_then(|v| v.as_object()) {
        for (k, v) in env {
            if let Some(val) = v.as_str() {
                sc.env.insert(k.clone(), val.to_string());
            }
        }
    }

    // 验证：至少需要有 url 或 command
    if sc.url.is_none() && sc.command.is_none() {
        return Err("缺少 url 或 command 字段".to_string());
    }

    Ok(sc)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_http_server() {
        let json = serde_json::json!({
            "type": "http",
            "url": "https://example.com/mcp",
            "headers": { "Authorization": "Bearer test123" }
        });
        let config = parse_mcp_server(&json).unwrap();
        assert_eq!(config.url.unwrap(), "https://example.com/mcp");
        assert_eq!(config.headers.get("Authorization").unwrap(), "Bearer test123");
        assert_eq!(config.server_type, "http");
    }

    #[test]
    fn test_parse_stdio_server() {
        let json = serde_json::json!({
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": { "GITHUB_TOKEN": "ghp_xxx" }
        });
        let config = parse_mcp_server(&json).unwrap();
        assert_eq!(config.command.unwrap(), "npx");
        assert_eq!(config.args.len(), 2);
        assert_eq!(config.env.get("GITHUB_TOKEN").unwrap(), "ghp_xxx");
    }

    #[test]
    fn test_parse_missing_url_and_command() {
        let json = serde_json::json!({});
        let result = parse_mcp_server(&json);
        assert!(result.is_err());
    }

    #[test]
    fn test_import_empty_servers() {
        let json = serde_json::json!({ "mcpServers": {} });
        assert!(parse_mcp_servers_only(&json).is_err());
    }

    fn parse_mcp_servers_only(json: &serde_json::Value) -> Result<(), String> {
        let servers = json.get("mcpServers")
            .and_then(|v| v.as_object())
            .ok_or_else(|| "缺少 mcpServers".to_string())?;
        if servers.is_empty() {
            return Err("为空".into());
        }
        Ok(())
    }
}
