//! RHermes 内置工具实现
//!
//! 每个工具都实现了 `Tool` trait，并声明 `parallel_safe` 标志。

use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use walkdir::WalkDir;

use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;

use crate::tools::{
    get_optional_string, get_string_arg, ParamDef, ParamType, Tool, ToolError,
};
use crate::mcp::McpAdapterManager;
use crate::tools::search::bing::BingEngine;
use crate::tools::search::duckduckgo::DuckDuckGoEngine;
use crate::tools::search::{MultiEngineSearcher, SearchCache};

// ---------------------------------------------------------------------------
// 安全常量与检查函数
// ---------------------------------------------------------------------------

/// 危险命令模式 — 大小写不敏感匹配
/// 危险命令模式 — 大小写不敏感匹配（增强型黑名单 v2）
const DANGEROUS_PATTERNS: &[&str] = &[
    // ---- 删除与格式化 ----
    "rm -rf",
    "rm -r /",
    "rm -rf /",
    "rm -rf ~",
    "del /s /q c:",
    "del /s /q d:",
    "del /s /q e:",
    "del /s /q f:",
    "del /s /q a:",
    "del /s /q b:",
    "rmdir /s /q",
    "format ",
    "mkfs.",
    "dd if=",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "net user",
    "net localgroup",
    "passwd",
    "chmod -r 777 /",
    "chown -r",
    "attrib +h",
    "icacls /grant",
    "curl | sh",
    "curl | bash",
    "wget | sh",
    "wget | bash",
    "powershell -enc",
    "powershell -encodedcommand",
    "pwsh -enc",
    "taskkill /f",
    "kill -9 1",
    "reg delete",
    "reg add",
    "sc delete",
    "net stop",
    "crontab -r",
    "cipher /w",
    "wbadmin",
    "diskpart",
    "bcdedit",
    "vssadmin",
    "mklink",
    "> /etc/passwd",
    "> /etc/shadow",
    "> /dev/",
];

/// 命令链危险子命令模式 — 在管道/分号/链式操作中检测
const DANGEROUS_CHAIN_PATTERNS: &[&str] = &[
    "rm -rf",
    "rm -r ",
    "del /s /q",
    "rmdir /s /q",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "net user",
    "net localgroup",
    "passwd",
    "chmod 777",
    "chown -r",
    "format ",
    "mkfs.",
    "dd if=",
    "powershell -enc",
    "powershell -encodedcommand",
    "pwsh -enc",
    "reg delete",
    "reg add",
    "taskkill /f",
    "kill -9",
    "cipher /w",
    "diskpart",
    "bcdedit",
    "vssadmin",
];

/// 检查命令是否匹配危险模式
/// 先做空格归一化（多空格 → 单空格）防止绕过
fn is_dangerous_command(cmd: &str) -> Option<&'static str> {
    let lower = normalize_spaces(&cmd.to_lowercase());
    for pattern in DANGEROUS_PATTERNS {
        if lower.contains(&pattern.to_lowercase()) {
            return Some(pattern);
        }
    }
    None
}

/// 检查子命令是否匹配命令链危险模式（同样做空格归一化）
fn is_dangerous_command_chain(cmd: &str) -> Option<&'static str> {
    let lower = normalize_spaces(&cmd.to_lowercase());
    for pattern in DANGEROUS_CHAIN_PATTERNS {
        if lower.contains(&pattern.to_lowercase()) {
            return Some(pattern);
        }
    }
    None
}

/// 空格归一化：连续的空白字符（空格/tab）折叠为单个空格
fn normalize_spaces(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_was_space {
                result.push(' ');
                prev_was_space = true;
            }
        } else {
            result.push(ch);
            prev_was_space = false;
        }
    }
    result
}

/// 提取 $(...) 子 Shell 中的命令内容（简单实现，不处理嵌套）
fn extract_subshell_content(cmd: &str) -> Option<String> {
    if let Some(start) = cmd.find("$(") {
        let rest = &cmd[start + 2..];
        if let Some(end) = rest.find(')') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// 检测命令链 / Shell 元字符中的危险嵌套
///
/// 防止通过管道、分号、&&、||、命令替换等方式绕过黑名单。
/// 例: `echo hello && rm -rf /` 或 `powershell -enc base64payload`
/// 不会误杀正常命令如 `echo hello && echo world`。
fn is_shell_meta_dangerous(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_lowercase();

    // 1. 检测命令替换: $(...) —— 提取子命令并递归检查
    if lower.contains("$(") {
        if let Some(content) = extract_subshell_content(cmd) {
            if let Some(pattern) = is_dangerous_command(&content) {
                return Some(pattern);
            }
            if let Some(pattern) = is_dangerous_command_chain(&content) {
                return Some(pattern);
            }
        }
    }

    // 2. 检测反引号命令替换（简单但有效的启发式）
    if lower.contains('`') {
        return Some("反引号命令替换");
    }

    // 3. 检测管道 (|) 后的危险子命令
    if let Some(after_pipe) = lower.split('|').nth(1) {
        let trimmed = after_pipe.trim_start();
        if let Some(pattern) = is_dangerous_command_chain(trimmed) {
            return Some(pattern);
        }
    }

    // 4. 检测分号 (;) 分割后的危险子命令
    for part in lower.split(';') {
        let trimmed = part.trim();
        if trimmed.is_empty() { continue; }
        if let Some(pattern) = is_dangerous_command_chain(trimmed) {
            return Some(pattern);
        }
    }

    // 5. 检测 && 后的危险子命令
    for part in lower.split("&&") {
        let trimmed = part.trim();
        if trimmed.is_empty() { continue; }
        if let Some(pattern) = is_dangerous_command_chain(trimmed) {
            return Some(pattern);
        }
    }

    // 6. 检测 || 后的危险子命令
    for part in lower.split("||") {
        let trimmed = part.trim();
        if trimmed.is_empty() { continue; }
        if let Some(pattern) = is_dangerous_command_chain(trimmed) {
            return Some(pattern);
        }
    }

    // 7. 检测 cmd /c、bash -c、sh -c 包装的危险命令
    for wrapper in &["cmd /c ", "cmd /c\t", "bash -c ", "sh -c "] {
        if lower.contains(wrapper) {
            let idx = lower.find(wrapper).unwrap();
            let sub_cmd = &lower[idx + wrapper.len()..];
            let sub_cmd = sub_cmd.trim_start_matches('"').trim_start_matches('\'');
            if let Some(pattern) = is_dangerous_command(sub_cmd) {
                return Some(pattern);
            }
            if let Some(pattern) = is_dangerous_command_chain(sub_cmd) {
                return Some(pattern);
            }
        }
    }

    // 8. 检测 base64 解码管道执行
    if lower.contains("base64") && (lower.contains(" | bash") || lower.contains(" | sh")) {
        return Some("base64管道执行");
    }

    None
}

// ---------------------------------------------------------------------------
// 全局 MCP 管理器（用于优雅关闭）
// ---------------------------------------------------------------------------

/// MCP 连接管理器全局实例，确保在进程退出前关闭所有 MCP 连接
static GLOBAL_MCP_MANAGER: OnceLock<McpAdapterManager> = OnceLock::new();

/// 受保护文件 — Agent 不可覆盖
const PROTECTED_FILES: &[&str] = &[
    "config.toml", ".env", "config.yaml", "config.json",
    "credentials.json", "secrets.toml", "secrets.env",
    ".ssh/id_rsa", ".ssh/id_ed25519", ".ssh/authorized_keys",
    "/etc/passwd", "/etc/shadow", "/etc/hosts",
];

/// 检查路径是否指向受保护文件
///
/// 匹配规则（先归一化正斜杠 + 小写）：
/// - 以 `/<protected>` 结尾  → 匹配（如 `a/.env` → 匹配 `.env`）
/// - 以 `/<protected>` 结尾  → 匹配（如 `a/etc/passwd` → 匹配 `/etc/passwd`）
/// - 整个路径就是 protected → 匹配（如 `.env`）
///
/// 不使用 `contains()` 避免误报（如 `config.toml.bak` 被错判为 `config.toml`）。
fn is_protected_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_lowercase();
    PROTECTED_FILES.iter().any(|p| {
        let p = p.to_lowercase();
        normalized == p || normalized.ends_with(&format!("/{p}"))
    })
}

/// 全局工作目录配置，由 main/init 时设置
///
/// 入参为空字符串时自动取 `std::env::current_dir()` 作为默认值，
/// 确保 GLOBAL_WORKSPACE 始终有值，避免边界检查静默跳过。
pub static GLOBAL_WORKSPACE: OnceLock<String> = OnceLock::new();

/// 设置全局工作目录（启动时由 main.rs / gateway 调用）
///
/// 返回实际使用的工作目录路径（归一化为正斜杠）。
pub fn set_workspace(path: String) -> String {
    let ws = if path.is_empty() {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());
        tracing::info!("工作目录未配置，默认使用当前目录: {cwd}");
        cwd
    } else {
        path
    };
    // 统一 normalize: 正斜杠 + 小写（与各工具中的检查逻辑保持一致）
    let normalized = ws.replace('\\', "/");
    let _ = GLOBAL_WORKSPACE.set(normalized.clone());
    normalized
}

// ---------------------------------------------------------------------------
// read_file — 并行安全
// ---------------------------------------------------------------------------

/// 解析 range 参数，格式 "start-end"（行号从 1 开始）
/// 返回 Some((start, end)) 或 None（格式无效）
fn parse_range(range: &str) -> Option<(usize, usize)> {
    let (s, e) = range.split_once('-')?;
    let start: usize = s.trim().parse().ok()?;
    let end: usize = e.trim().parse().ok()?;
    if start == 0 || end == 0 || start > end {
        return None;
    }
    Some((start, end))
}

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> String {
        "read_file".into()
    }
    fn description(&self) -> String {
        "读取文件内容，可指定行范围（head/tail/range）".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "文件路径"),
            ParamDef::optional("head", ParamType::Integer, "只返回前 N 行"),
            ParamDef::optional("tail", ParamType::Integer, "只返回后 N 行"),
            ParamDef::optional("range", ParamType::String, "行范围如 \"50-100\""),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = get_string_arg(&args, "path")?;

        // 安全检查: 工作目录边界（读取也需限制）
        let ws = GLOBAL_WORKSPACE.get().expect("GLOBAL_WORKSPACE 未初始化");
        let abs = if Path::new(&path).is_absolute() { path.clone() }
            else { format!("{}/{}", ws.trim_end_matches('/'), path) };
        let normalized = abs.replace('\\', "/").to_lowercase();
        let ws_norm = ws.to_lowercase();
        if !normalized.starts_with(&ws_norm) {
            return Err(ToolError::ExecutionFailed(format!("⛔ 路径 '{path}' 超出工作目录 '{ws}'")));
        }
        if is_protected_path(&path) {
            return Err(ToolError::ExecutionFailed(format!("⛔ 路径 '{path}' 是受保护文件，不可 Agent 读取")));
        }

        let content = tokio::fs::read_to_string(&path).await.map_err(ToolError::Io)?;

        let head = args.get("head").and_then(|v| v.as_u64());
        let tail = args.get("tail").and_then(|v| v.as_u64());
        let range = args.get("range").and_then(|v| v.as_str());

        let result = if let Some(n) = head {
            content.lines().take(n as usize).collect::<Vec<_>>().join("\n")
        } else if let Some(n) = tail {
            let lines: Vec<&str> = content.lines().collect();
            let len = lines.len();
            lines[len.saturating_sub(n as usize)..].join("\n")
        } else if let Some(range_str) = range {
            // 解析 range 参数，格式如 "50-100"
            if let Some((start, end)) = parse_range(range_str) {
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                let s = start.saturating_sub(1).min(total);
                let e = end.min(total).max(s);
                lines[s..e].join("\n")
            } else {
                return Err(ToolError::InvalidParam(format!("无效的 range 格式: '{range_str}'，期望如 '50-100'")));
            }
        } else {
            content
        };

        Ok(format!("[{path}] ({} 字节)\n{result}", result.len()))
    }
}

/// 递归收集文件（跳过二进制和隐藏目录）
fn collect_files(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // 跳过隐藏目录 .git, node_modules, target 等
        if path.is_dir() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with('.') || s == "node_modules" || s == "target" || s == "build" {
                continue;
            }
            collect_files(&path, files)?;
        } else if path.is_file() {
            // 只搜索文本文件（跳过 .exe .dll .png 等）
            if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                match ext.as_str() {
                    "exe" | "dll" | "png" | "jpg" | "gif" | "ico" | "svg"
                    | "woff" | "woff2" | "ttf" | "eot" | "bin" | "o" | "obj"
                    | "pyc" | "pyo" | "lock" | "sum" => continue,
                    _ => {}
                }
            }
            files.push(path);
        }
    }
    Ok(())
}

/// 将 glob 模式转换为编译后的正则表达式
fn glob_to_regex(glob: &str) -> Result<Regex, String> {
    let mut re_str = String::with_capacity(glob.len() + 4);
    re_str.push('^');
    for ch in glob.chars() {
        match ch {
            '*' => re_str.push_str(".*"),
            '?' => re_str.push('.'),
            '.' => re_str.push_str("\\."),
            '\\' => re_str.push_str("\\\\"),
            '/' => re_str.push('/'),
            c if c.is_ascii_punctuation() && c != '_' && c != '|' => {
                re_str.push('\\');
                re_str.push(c);
            }
            c => re_str.push(c),
        }
    }
    re_str.push('$');
    Regex::new(&re_str).map_err(|e| format!("glob 模式无效: {e}"))
}

// ---------------------------------------------------------------------------
// search_content — 并行安全（基于 ripgrep 库）
// ---------------------------------------------------------------------------

use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ignore::WalkBuilder;

pub struct SearchContent;

#[async_trait]
impl Tool for SearchContent {
    fn name(&self) -> String {
        "search_content".into()
    }
    fn description(&self) -> String {
        "在文件中搜索文本模式，返回匹配的文件:行号（基于 ripgrep）".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("pattern", ParamType::String, "搜索模式（支持正则）"),
            ParamDef::optional("path", ParamType::String, "搜索目录（默认项目根）"),
            ParamDef::optional("glob", ParamType::String, "文件名过滤"),
            ParamDef::optional("context_lines", ParamType::Integer, "上下文行数（默认 0，最大 3）"),
            ParamDef::optional("max_results", ParamType::Integer, "最大结果数（默认 200，最大 1000）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let pattern = get_string_arg(&args, "pattern")?;
        let search_path = get_optional_string(&args, "path").unwrap_or_else(|| ".".into());
        let ws = GLOBAL_WORKSPACE.get().expect("GLOBAL_WORKSPACE 未初始化");
        let sp = if Path::new(&search_path).is_absolute() { search_path.clone() }
            else { format!("{}/{}", ws.trim_end_matches('/'), search_path) };
        let n = sp.replace('\\', "/").to_lowercase();
        let wn = ws.to_lowercase();
        if !n.starts_with(&wn) {
            return Err(ToolError::ExecutionFailed(format!("⛔ 搜索路径 '{search_path}' 超出工作目录 '{ws}'")));
        }
        let glob_filter = get_optional_string(&args, "glob");
        // 预编译 glob 正则（避免遍历时每个文件重复编译）
        let glob_re = match &glob_filter {
            Some(g) => Some(glob_to_regex(g).map_err(|e| ToolError::InvalidParam(e))?),
            None => None,
        };
        let context_lines = args
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .min(3) as usize;
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(200)
            .min(1000) as usize;

        // spawn_blocking：grep-searcher 是同步 API
        let result = tokio::task::spawn_blocking(move || {
            let matcher = RegexMatcher::new(&pattern).map_err(|e| {
                format!("正则无效: {e}")
            })?;

            let mut builder = SearcherBuilder::new();
            builder.line_number(true);
            if context_lines > 0 {
                builder.before_context(context_lines);
                builder.after_context(context_lines);
            }
            let mut searcher = builder.build();

            let mut results: Vec<String> = Vec::new();
            let path = std::path::Path::new(&search_path);

            if path.is_file() {
                searcher.search_path(&matcher, path, UTF8(|lnum, line| {
                    if results.len() >= max_results { return Ok(false); }
                    results.push(format!("{}:{}:{}", path.display(), lnum, line));
                    Ok(true)
                })).map_err(|e| format!("搜索失败: {e}"))?;
            } else {
                let walk = WalkBuilder::new(path)
                    .hidden(false)
                    .git_ignore(true)
                    .build();
                use ignore::DirEntry;
                for entry in walk.filter_map(|e: Result<DirEntry, ignore::Error>| e.ok()) {
                    if results.len() >= max_results { break; }
                    let file_path = entry.path().to_path_buf();
                    if let Some(ref gre) = glob_re {
                        let name = file_path.to_string_lossy();
                        if !glob_matches(gre, &name) { continue; }
                    }
                    let _ = searcher.search_path(&matcher, &file_path, UTF8(|lnum, line| {
                        if results.len() >= max_results { return Ok(false); }
                        results.push(format!("{}:{}:{}", file_path.display(), lnum, line));
                        Ok(true)
                    }));
                }
            }

            if results.is_empty() {
                Ok(format!("未找到匹配 \"{pattern}\" 的内容"))
            } else {
                Ok(format!("找到 {} 处匹配:\n{}", results.len(), results.join("\n")))
            }
        }).await
        .map_err(|e| ToolError::ExecutionFailed(format!("搜索线程崩溃: {e}")))?
        .map_err(|e| ToolError::ExecutionFailed(e))?;

        Ok(result)
    }
}

/// 使用 glob 模式过滤文件路径（用于 search_content 遍历）
fn glob_matches(glob_re: &Regex, file_rel_path: &str) -> bool {
    let name = file_rel_path.replace('\\', "/");
    glob_re.is_match(&name)
}

// ---------------------------------------------------------------------------
// read_pdf — 并行安全
// ---------------------------------------------------------------------------

/// 全局显示配置（由 main.rs 设置，供 read_pdf 读取预览上限）
static GLOBAL_DISPLAY_CONFIG: OnceLock<crate::core::DisplayConfig> = OnceLock::new();

/// 设置全局显示配置
pub fn set_display_config(cfg: crate::core::DisplayConfig) {
    let _ = GLOBAL_DISPLAY_CONFIG.set(cfg);
}

pub struct ReadPdf;

#[async_trait]
impl Tool for ReadPdf {
    fn name(&self) -> String { "read_pdf".into() }
    fn description(&self) -> String { "读取 PDF 文件，返回纯文本内容".into() }
    fn parallel_safe(&self) -> bool { true }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![ParamDef::required("path", ParamType::String, "PDF 文件路径")]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = get_string_arg(&args, "path")?;
        let start = std::time::Instant::now();

        let max_chars = GLOBAL_DISPLAY_CONFIG.get()
            .map(|c| c.read_pdf_max_chars).unwrap_or(30000);

        // spawn_blocking：pdf-extract 是同步 API
        let text = tokio::task::spawn_blocking(move || {
            pdf_extract::extract_text(&path)
                .map_err(|e| format!("读取 PDF 失败: {e}"))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("PDF 线程崩溃: {e}")))?
        .map_err(|e| ToolError::ExecutionFailed(e))?;

        let elapsed = start.elapsed();
        let total_chars = text.chars().count();
        let show_chars = total_chars.min(max_chars);
        let preview: String = text.chars().take(show_chars).collect();
        let truncated = if total_chars > show_chars { format!("（截断，共 {total_chars} 字符）") } else { String::new() };
        Ok(format!("📄 PDF (显示 {show_chars}/{total_chars} 字符，耗时 {:.1}s){truncated}:\n{preview}", elapsed.as_secs_f64()))
    }
}

// ---------------------------------------------------------------------------
// glob — 并行安全
// ---------------------------------------------------------------------------

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> String {
        "glob".into()
    }
    fn description(&self) -> String {
        "按 glob 模式列出文件".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("pattern", ParamType::String, "glob 模式如 '**/*.rs'"),
            ParamDef::optional("path", ParamType::String, "搜索目录"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let pattern = get_string_arg(&args, "pattern")?;
        let root = get_optional_string(&args, "path").unwrap_or_else(|| ".".into());
        let ws = GLOBAL_WORKSPACE.get().expect("GLOBAL_WORKSPACE 未初始化");
        let ar = if Path::new(&root).is_absolute() { root.clone() }
            else { format!("{}/{}", ws.trim_end_matches('/'), root) };
        let n = ar.replace('\\', "/").to_lowercase();
        let wn = ws.to_lowercase();
        if !n.starts_with(&wn) {
            return Err(ToolError::ExecutionFailed(format!("⛔ glob 路径 '{root}' 超出工作目录 '{ws}'")));
        }

        // Convert glob pattern to regex
        let re = glob_to_regex(&pattern)
            .map_err(|e| ToolError::ExecutionFailed(e))?;

        const MAX_RESULTS: usize = 500;

        let result = tokio::task::spawn_blocking(move || {
            let root_path = std::path::Path::new(&root);
            let mut files: Vec<String> = Vec::new();

            if root_path.is_file() {
                let name = root_path.to_string_lossy();
                if re.is_match(&name) {
                    files.push(name.to_string());
                }
            } else {
                for entry in WalkDir::new(root_path)
                    .follow_links(false)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if files.len() >= MAX_RESULTS {
                        break;
                    }
                    let path = entry.path();
                    if path.is_file() {
                        let name = path.to_string_lossy();
                        if re.is_match(&name) {
                            files.push(name.to_string());
                        }
                    }
                }
            }

            if files.is_empty() {
                format!("No files matching \"{}\" found", pattern)
            } else {
                format!("Found {} files:\n{}", files.len(), files.join("\n"))
            }
        }).await
        .map_err(|e| ToolError::ExecutionFailed(format!("glob thread crashed: {e}")))?;

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// write_file — 非并行安全（副作用）
// ---------------------------------------------------------------------------

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> String {
        "write_file".into()
    }
    fn description(&self) -> String {
        "写入文件（创建或覆盖）".into()
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "文件路径"),
            ParamDef::required("content", ParamType::String, "文件内容"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = get_string_arg(&args, "path")?;

        // 安全检查: 配置文件保护
        if is_protected_path(&path) {
            tracing::warn!("⛔ 写保护: 拒绝写入受保护文件 {path}");
            return Err(ToolError::ExecutionFailed(
                format!("⛔ 安全策略: 路径 '{path}' 是受保护文件（配置/密钥/系统文件），不可通过 Agent 修改。")
            ));
        }

        // 安全检查: 工作目录边界
        let ws = GLOBAL_WORKSPACE.get().expect("GLOBAL_WORKSPACE 未初始化");
        let abs = if Path::new(&path).is_absolute() {
            path.clone()
        } else {
            format!("{}/{}", ws.trim_end_matches('/'), path)
        };
        let normalized = abs.replace('\\', "/").to_lowercase();
        let ws_norm = ws.to_lowercase();
        if !normalized.starts_with(&ws_norm) {
            tracing::warn!("⛔ 路径越界: {path} 不在工作目录 {ws} 内");
            return Err(ToolError::ExecutionFailed(
                format!("⛔ 安全策略: 路径 '{path}' 超出工作目录 '{ws}'。")
            ));
        }

        let content = get_string_arg(&args, "content")?;

        // 确保父目录存在
        if let Some(parent) = Path::new(&path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(ToolError::Io)?;
        }

        // 原子写入：先写 tmp 文件再 rename，避免崩溃时产生损坏文件
        let tmp_path = format!("{path}.write_file.tmp");
        tokio::fs::write(&tmp_path, &content)
            .await
            .map_err(ToolError::Io)?;
        tokio::fs::rename(&tmp_path, &path)
            .await
            .map_err(|e| {
                // rename 失败时尽力清理 tmp 文件
                let _ = std::fs::remove_file(&tmp_path);
                ToolError::Io(e)
            })?;

        Ok(format!("已写入 {} ({} 字节)", path, content.len()))
    }
}

// ---------------------------------------------------------------------------
// run_command — 非并行安全（副作用）
// ---------------------------------------------------------------------------

pub struct RunCommand {
    proxy_url: Option<String>,
}

impl RunCommand {
    pub fn new(proxy_url: Option<String>) -> Self {
        Self { proxy_url }
    }
}

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> String {
        "run_command".into()
    }
    fn description(&self) -> String {
        "在 shell 中执行命令".into()
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("command", ParamType::String, "要执行的命令（Windows 用 cmd，其他用 sh）"),
            ParamDef::optional("timeout", ParamType::Integer, "超时秒数（默认 60）"),
            ParamDef::optional("cwd", ParamType::String, "工作目录（默认当前目录）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let command = get_string_arg(&args, "command")?;

        // 安全检查: 拦截危险命令
        if let Some(pattern) = is_dangerous_command(&command) {
            tracing::warn!("⛔ 拦截危险命令: 匹配 '{pattern}' → {command}");
            return Err(ToolError::ExecutionFailed(
                format!("⛔ 安全策略: 命令被拦截（匹配危险模式 '{pattern}'）。如确需执行请联系管理员。")
            ));
        }
        // 安全检查: 拦截命令链 / Shell 元字符嵌套危险命令
        if let Some(pattern) = is_shell_meta_dangerous(&command) {
            tracing::warn!("⛔ 拦截命令链危险: 匹配 '{}': {command}", pattern);
            return Err(ToolError::ExecutionFailed(
                format!("⛔ 安全策略: 命令被拦截（检测到命令链/Shell元字符危险嵌套 '{}'）。如确需执行请联系管理员。", pattern)
            ));
        }

        // 安全检查: 命令白名单
        if let Some(cfg) = GLOBAL_CONFIG.get() {
            if !cfg.agent.command_allowed_prefixes.is_empty() {
                let first_word = command.split_whitespace().next().unwrap_or("");
                // 同时检查完整命令和路径的文件名（如 /usr/bin/git → git）
                let cmd_basename = std::path::Path::new(first_word)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(first_word);
                let allowed = cfg.agent.command_allowed_prefixes.iter()
                    .any(|prefix| {
                        first_word == prefix.as_str()
                            || first_word.starts_with(prefix.as_str())
                            || cmd_basename == prefix.as_str()
                    });
                if !allowed {
                    tracing::warn!(
                        "⛔ 命令不在白名单中: {command} (允许: {:?})",
                        cfg.agent.command_allowed_prefixes
                    );
                    if cfg.agent.command_require_confirm {
                        return Err(ToolError::ExecutionFailed(
                            format!("⛔ 命令「{}」不在允许列表中，如需执行请联系管理员添加白名单", command)
                        ));
                    }
                    tracing::warn!("⚠ 命令不在白名单但已放行（command_require_confirm=false）");
                }
            }
        }

        let timeout = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(60);

        // Windows 用 cmd /c，其他用 sh -c
        #[cfg(target_os = "windows")]
        let shell = "cmd";
        #[cfg(not(target_os = "windows"))]
        let shell = "sh";

        #[cfg(target_os = "windows")]
        let flag = "/c";
        #[cfg(not(target_os = "windows"))]
        let flag = "-c";

        let mut cmd = tokio::process::Command::new(shell);
        cmd.arg(flag).arg(&command);
        // 重要：必须断开 stdin，否则子进程会抢走 TUI 的键盘输入
        cmd.stdin(std::process::Stdio::null());

        // 注入代理环境变量（如果配置了 command 代理）
        if let Some(ref proxy) = self.proxy_url {
            cmd.env("HTTP_PROXY", proxy);
            cmd.env("HTTPS_PROXY", proxy);
            cmd.env("ALL_PROXY", proxy);
            if let Some(cfg) = GLOBAL_CONFIG.get() {
                if !cfg.proxy.no_proxy.is_empty() {
                    cmd.env("NO_PROXY", cfg.proxy.no_proxy.join(","));
                }
            }
        }

        // 可选的工作目录
        if let Some(cwd) = get_optional_string(&args, "cwd") {
            cmd.current_dir(&cwd);
        }

        let output = tokio::time::timeout(Duration::from_secs(timeout), cmd.output())
            .await
            .map_err(|_| ToolError::ExecutionFailed("命令超时".into()))?
            .map_err(|e| ToolError::ExecutionFailed(format!("命令执行失败: {e}")))?;

        let mut result = String::new();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stdout.is_empty() {
            result.push_str(&format!("[stdout]\n{stdout}"));
        }
        if !stderr.is_empty() {
            result.push_str(&format!("[stderr]\n{stderr}"));
        }
        if !output.status.success() {
            result.push_str(&format!("\n[退出码: {}]", output.status.code().unwrap_or(-1)));
        }

        if result.is_empty() {
            result = "命令执行完毕（无输出）".into();
        }

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// get_current_time — 并行安全（纯计算）
// ---------------------------------------------------------------------------

/// 获取当前时间
pub struct GetCurrentTime;

#[async_trait]
impl Tool for GetCurrentTime {
    fn name(&self) -> String {
        "get_current_time".into()
    }
    fn description(&self) -> String {
        "获取当前日期和时间".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![]
    }
    async fn execute(&self, _args: Value) -> Result<String, ToolError> {
        let local = chrono::Local::now();
        let offset = local.offset();
        let utc_secs = offset.local_minus_utc();
        let sign = if utc_secs >= 0 { "+" } else { "-" };
        let abs = utc_secs.abs();
        let hours = abs / 3600;
        let minutes = (abs % 3600) / 60;
        let dt = local.format("%Y-%m-%d %H:%M:%S");
        Ok(format!("{dt} (UTC{sign}{hours:02}:{minutes:02})"))
    }
}

// ---------------------------------------------------------------------------
// web_search — 非并行安全（网络请求）
// ---------------------------------------------------------------------------

/// 全局搜索引擎实例（懒初始化）
static GLOBAL_SEARCHER: OnceLock<MultiEngineSearcher> = OnceLock::new();

fn get_searcher() -> &'static MultiEngineSearcher {
    GLOBAL_SEARCHER.get_or_init(|| {
        let config = GLOBAL_CONFIG.get().map(|c| &c.search);
        let proxy = config.and_then(|s| s.proxy.as_deref());
        let timeout = config.map(|s| std::time::Duration::from_secs(s.timeout_secs))
            .unwrap_or_else(|| std::time::Duration::from_secs(10));
        let cache_size = config.map(|s| s.cache_size).unwrap_or(100);
        let cache_ttl = config.map(|s| std::time::Duration::from_secs(s.cache_ttl_secs))
            .unwrap_or_else(|| std::time::Duration::from_secs(600));

        let mut engines: Vec<Box<dyn crate::tools::search::SearchEngine>> = Vec::new();

        // Serper（付费，最可靠，有 API Key 时优先）
        if let Some(api_key) = config.and_then(|s| {
            if s.serper_api_key.is_empty() { None } else { Some(s.serper_api_key.as_str()) }
        }) {
            engines.push(Box::new(crate::tools::search::serper::SerperEngine::new(
                api_key.to_string(), proxy,
            )));
        }

        // DuckDuckGo（免费 HTML 解析）
        engines.push(Box::new(DuckDuckGoEngine::new(proxy)));

        // Bing（免费 HTML 解析，DDG 降级）
        engines.push(Box::new(BingEngine::new(proxy)));

        let cache = SearchCache::new(cache_size, cache_ttl);
        MultiEngineSearcher::new(engines, cache, timeout)
    })
}

/// 搜索网络信息
///
/// 使用 DuckDuckGo HTML 搜索（免费、无需 API Key）。
/// 支持多引擎降级和结果缓存。
pub struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> String {
        "web_search".into()
    }
    fn description(&self) -> String {
        "搜索网络获取最新信息。返回标题、摘要和链接。支持中英文查询。".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("query", ParamType::String, "搜索关键词"),
            ParamDef::optional("max_results", ParamType::Integer, "最大结果数（默认 5，最大 10）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let query = get_string_arg(&args, "query")?;
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(10) as usize;

        let searcher = get_searcher();
        Ok(searcher.search(&query, max_results).await)
    }
}

// ---------------------------------------------------------------------------
// web_fetch — 非并行安全（网络请求）
// ---------------------------------------------------------------------------

/// 获取网页内容
///
/// 下载指定 URL 的内容并提取可读文本。
pub struct WebFetch {
    http: reqwest::Client,
}

impl WebFetch {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> String {
        "web_fetch".into()
    }
    fn description(&self) -> String {
        "获取网页内容并提取可读文本".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("url", ParamType::String, "要获取的网页 URL"),
            ParamDef::optional("max_chars", ParamType::Integer, "最大字符数（默认 5000）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let url = get_string_arg(&args, "url")?;

        // 1. URL scheme 检查：只允许 http/https
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(ToolError::ExecutionFailed(
                "不支持的 URL 协议，仅支持 http/https".into(),
            ));
        }

        // 1.5 SSRF 防护：检查目标主机是否为内网/元数据地址
        {
            let host = url.split("://").nth(1).unwrap_or("")
                .split('/').next().unwrap_or("")
                .split(':').next().unwrap_or("").to_lowercase();
            let blocked = ["localhost", "127.0.0.1", "0.0.0.0", "[::1]",
                "169.254.169.254", "metadata.google.internal", "metadata.azure.com"];
            for b in &blocked {
                if host == *b { return Err(ToolError::ExecutionFailed(
                    format!("⛔ SSRF: 不允许访问内网地址 '{host}'"))); }
            }
            let parts: Vec<&str> = host.split('.').collect();
            if parts.len() == 4 {
                if let (Ok(a), Ok(b)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
                    let priv_ip = a == 10 || (a == 172 && b >= 16 && b <= 31)
                        || (a == 192 && b == 168) || (a == 100 && b >= 64 && b <= 127) || a == 0;
                    if priv_ip { return Err(ToolError::ExecutionFailed(
                        format!("⛔ SSRF: 不允许访问私有 IP '{host}'"))); }
                }
            }
        }

        // 2. 扩展名预检：拒绝已知的二进制格式
        let ext = url
            .split('?')
            .next()
            .unwrap_or(&url)
            .rsplit('.')
            .next()
            .map(|e| e.to_lowercase())
            .filter(|e| {
                matches!(
                    e.as_str(),
                    "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx"
                        | "zip" | "tar" | "gz" | "rar" | "7z" | "bz2"
                        | "exe" | "msi" | "dmg" | "iso"
                        | "mp3" | "mp4" | "avi" | "mkv" | "mov" | "wmv" | "flv"
                        | "ogg" | "wav" | "flac" | "ape"
                        | "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "ico" | "svg"
                        | "ttf" | "woff" | "woff2" | "eot"
                )
            });
        if let Some(ext) = ext {
            return Err(ToolError::ExecutionFailed(format!(
                "该 URL 指向 .{} 文件，web_fetch 仅支持文本网页。如需处理此文件请使用专门的工具",
                ext
            )));
        }

        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(5000) as usize;

        let resp = self.http
            .get(&url)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("请求失败: {e}")))?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // 3. Content-Type 检查
        let ct_check = content_type.split(';').next().unwrap_or(&content_type).trim().to_lowercase();
        if !ct_check.is_empty()
            && !ct_check.starts_with("text/")
            && ct_check != "application/json"
            && ct_check != "application/xml"
            && ct_check != "application/javascript"
            && ct_check != "application/xhtml+xml"
        {
            if ct_check.starts_with("image/")
                || ct_check.starts_with("audio/")
                || ct_check.starts_with("video/")
                || ct_check == "application/octet-stream"
            {
                return Err(ToolError::ExecutionFailed(format!(
                    "目标文件类型为 {content_type}，web_fetch 仅支持文本网页"
                )));
            }
        }

        // 4. Content-Length 检查（>5MB 拒绝）
        const MAX_SIZE: u64 = 5 * 1024 * 1024;
        if let Some(cl) = resp.headers().get("content-length") {
        if let Some(size) = cl.to_str().ok().and_then(|s| s.parse::<u64>().ok()) {
                if size > MAX_SIZE {
                    return Err(ToolError::ExecutionFailed(format!(
                        "目标文件过大（{} 字节），web_fetch 限制 5MB",
                        size
                    )));
                }
            }
        }

        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("读取响应失败: {e}")))?;

        // 提取可读文本
        let text = if content_type.contains("text/html") {
            extract_article_text(&body)
        } else {
            body.clone()
        };

        // 截断
        let text: String = text.chars().take(max_chars).collect();
        let total_len = body.len();
        let text_len = text.len();

        let extra = if text_len < total_len {
            format!("\n...(显示 {} 字符，共 {} 字符)", text_len, total_len)
        } else {
            String::new()
        };
        let result = format!(
            "[{}] HTTP {status} · {content_type}\n{}{}",
            url, text, extra,
        );
        Ok(format!("<untrusted>\n{result}\n</untrusted>"))
    }
}

/// 简单的 HTML 标签剥离
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;

    let lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let lower_bytes = lower.as_bytes();

    let mut i = 0;
    while i < bytes.len() {
        if !in_tag && bytes[i] == b'<' {
            // 检测 <script 和 <style
            let rest_lower = &lower[i..];
            in_script = rest_lower.starts_with("<script");
            in_style = rest_lower.starts_with("<style");
            in_tag = true;
            i += 1;
            continue;
        }
        if in_tag && bytes[i] == b'>' {
            // 检测 </script> 和 </style>
            if (in_script && lower_bytes[i.saturating_sub(8)..=i].windows(9).any(|w| w == b"</script>"))
                || (in_style && lower_bytes[i.saturating_sub(6)..=i].windows(7).any(|w| w == b"</style>"))
            {
                in_script = false;
                in_style = false;
            }
            in_tag = false;
            i += 1;
            if !in_script && !in_style {
                result.push(' ');
            }
            continue;
        }
        if in_tag || in_script || in_style {
            i += 1;
            continue;
        }
        // 合并多个空白
        if bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n' || bytes[i] == b'\r' {
            if !result.ends_with(' ') {
                result.push(' ');
            }
        } else {
            result.push(bytes[i] as char);
        }
        i += 1;
    }

    result.trim().to_string()
}

/// 用 scraper 提取网页正文（优先 article/main，移除噪声元素）
fn extract_article_text(html: &str) -> String {
    let document = scraper::Html::parse_document(html);
    let body_sel = scraper::Selector::parse("body").unwrap();
    let remove_sel = scraper::Selector::parse(
        "script, style, nav, header, footer, aside, noscript, iframe, form, button",
    )
    .unwrap();

    // 优先从 article 或 main 提取
    let main_sel = scraper::Selector::parse("article, main, [role='main']").unwrap();
    let root = document
        .select(&main_sel)
        .next()
        .or_else(|| document.select(&body_sel).next());

    let Some(root) = root else {
        return strip_html_tags(html);
    };

    // 收集文本
    let mut parts = Vec::new();
    collect_text(&root, &remove_sel, &mut parts);
    let text = parts.join("\n");

    if text.trim().is_empty() {
        // fallback: 用旧方法
        return strip_html_tags(html);
    }
    text
}

/// 递归收集文本，跳过需要移除的元素
fn collect_text(
    node: &scraper::ElementRef<'_>,
    remove_sel: &scraper::Selector,
    parts: &mut Vec<String>,
) {
    use scraper::ElementRef;

    for child in node.children() {
        if let Some(child_ref) = ElementRef::wrap(child) {
            let tag = child_ref.value().name();
            if matches!(tag, "script" | "style" | "nav" | "header" | "footer"
                | "aside" | "noscript" | "iframe" | "form" | "button")
            {
                continue;
            }
            collect_text(&child_ref, remove_sel, parts);
        } else if let Some(text) = child.value().as_text() {
            let text = text.trim();
            if !text.is_empty() {
                parts.push(text.to_string());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// delegate_task — 非并行安全（网络请求）
// ---------------------------------------------------------------------------

/// 将子任务委托给独立的子 Agent 执行
///
/// 子 Agent 会创建一个独立的 API 调用，专注于完成给定任务并返回结果。
/// 适合需要深度分析或并行调研的场景。
pub struct DelegateTask;

#[async_trait]
impl Tool for DelegateTask {
    fn name(&self) -> String {
        "delegate_task".into()
    }
    fn description(&self) -> String {
        "将子任务委托给独立的子 Agent 执行，返回分析结果".into()
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("task", ParamType::String, "要子 Agent 完成的任务描述"),
            ParamDef::optional("context", ParamType::String, "额外的上下文信息"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let task = get_string_arg(&args, "task")?;
        let context = get_optional_string(&args, "context").unwrap_or_default();

        let config = GLOBAL_CONFIG.get().ok_or_else(|| {
            ToolError::ExecutionFailed("子 Agent 配置未初始化".into())
        })?;
        let transport = GLOBAL_TRANSPORT.get().ok_or_else(|| {
            ToolError::ExecutionFailed("子 Agent Transport 未初始化".into())
        })?;

        let result = crate::agent::run_sub_agent(&task, &context, config, transport.clone()).await;

        if result.success {
            Ok(format!(
                "【子 Agent 结果】\n{}\n【耗时: {}ms】",
                result.output, result.duration_ms
            ))
        } else {
            Ok(format!("【子 Agent 失败】\n{}", result.output))
        }
    }
}

// ---------------------------------------------------------------------------
// run_skill — 非并行安全（子 Agent 调用）
// ---------------------------------------------------------------------------

/// 执行已安装的技能
pub struct RunSkill;

#[async_trait]
impl Tool for RunSkill {
    fn name(&self) -> String {
        "run_skill".into()
    }
    fn description(&self) -> String {
        "执行一个已安装的技能并返回结果".into()
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("name", ParamType::String, "技能名称"),
            ParamDef::required("arguments", ParamType::String, "传给技能的任务描述"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let name = get_string_arg(&args, "name")?;
        let arguments = get_string_arg(&args, "arguments")?;

        let engine_arc = GLOBAL_SKILL_ENGINE.get().ok_or_else(|| {
            ToolError::ExecutionFailed("技能引擎未初始化".into())
        })?;

        // 查找技能（锁作用域，在 await 前释放）
        let skill = {
            let engine = engine_arc.lock().map_err(|e| {
                ToolError::ExecutionFailed(format!("技能引擎锁定失败: {e}"))
            })?;
            engine.get(&name).cloned().ok_or_else(|| {
                ToolError::ExecutionFailed(format!("技能「{name}」不存在"))
            })?
        };

        let result = skill.run(&arguments).await;

        // 记录使用情况
        if let Ok(mut engine) = engine_arc.lock() {
            let _ = engine.record_usage(&name, result.success, result.duration_ms as u64);
        }

        if result.success {
            Ok(format!("【技能「{name}」结果】\n{}\n【耗时: {}ms】", result.output, result.duration_ms))
        } else {
            Ok(format!("【技能「{name}」执行失败】\n{}", result.output))
        }
    }
}

// ---------------------------------------------------------------------------
// skill_list — 并行安全（纯查询）
// ---------------------------------------------------------------------------
pub struct SkillList;

#[async_trait]
impl Tool for SkillList {
    fn name(&self) -> String { "skill_list".into() }
    fn description(&self) -> String { "列出所有已安装的技能名称和描述".into() }
    fn parallel_safe(&self) -> bool { true }
    fn parameters(&self) -> Vec<ParamDef> { vec![] }
    async fn execute(&self, _args: Value) -> Result<String, ToolError> {
        let engine_arc = GLOBAL_SKILL_ENGINE.get().ok_or_else(|| ToolError::ExecutionFailed("技能引擎未初始化".into()))?;
        let engine = engine_arc.lock().map_err(|e| ToolError::ExecutionFailed(format!("锁定失败: {e}")))?;
        let skills = engine.list();
        if skills.is_empty() {
            Ok("暂无已安装的技能。可以通过 skill_create 创建新技能。".into())
        } else {
            let mut out = format!("可用技能 ({}):", skills.len());
            for s in &skills {
                out.push_str(&format!("\n- {}: {} (使用 {} 次, 成功率 {:.0}%)", s.name, s.description, s.use_count, s.success_rate() * 100.0));
            }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------
// skill_search — 并行安全（纯查询）
// ---------------------------------------------------------------------------
pub struct SkillSearch;

#[async_trait]
impl Tool for SkillSearch {
    fn name(&self) -> String { "skill_search".into() }
    fn description(&self) -> String { "按关键词搜索已安装的技能".into() }
    fn parallel_safe(&self) -> bool { true }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![ParamDef::required("query", ParamType::String, "搜索关键词")]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let query = get_string_arg(&args, "query")?;
        let engine_arc = GLOBAL_SKILL_ENGINE.get().ok_or_else(|| ToolError::ExecutionFailed("技能引擎未初始化".into()))?;
        let engine = engine_arc.lock().map_err(|e| ToolError::ExecutionFailed(format!("锁定失败: {e}")))?;
        let results = engine.search(&query);
        if results.is_empty() {
            Ok(format!("未找到与「{query}」相关的技能"))
        } else {
            let mut out = format!("找到 {} 个相关技能:", results.len());
            for s in &results { out.push_str(&format!("\n- {}: {} ({:.0}%)", s.name, s.description, s.success_rate() * 100.0)); }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------
// skill_create — 非并行安全（写磁盘）
// ---------------------------------------------------------------------------
pub struct SkillCreate;

#[async_trait]
impl Tool for SkillCreate {
    fn name(&self) -> String { "skill_create".into() }
    fn description(&self) -> String { "创建新的可复用技能，让 AI 不断积累最佳实践".into() }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("name", ParamType::String, "技能名称（小写英文+短横线）"),
            ParamDef::required("description", ParamType::String, "一句话描述该技能的用途"),
            ParamDef::required("body", ParamType::String, "技能正文 Markdown，描述执行步骤和注意事项"),
            ParamDef::optional("category", ParamType::String, "技能分类目录（如 analysis/utils/debug），不填则放根目录"),
            ParamDef::optional("allowed_tools", ParamType::String, "允许的工具列表，逗号分隔"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let name = get_string_arg(&args, "name")?;
        let description = get_string_arg(&args, "description")?;
        let body = get_string_arg(&args, "body")?;
        let category = get_optional_string(&args, "category");
        let category_str = category.as_deref();
        let allowed_tools = get_optional_string(&args, "allowed_tools").unwrap_or_default();
        let engine_arc = GLOBAL_SKILL_ENGINE.get().ok_or_else(|| ToolError::ExecutionFailed("技能引擎未初始化".into()))?;
        let tools_str = if allowed_tools.is_empty() { String::new() } else {
            format!("\nallowed_tools:\n  - {}", allowed_tools.split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>().join("\n  - "))
        };
        let skill_body = format!("---\nname: {name}\ndescription: {description}\nrun_as: subagent{tools_str}\n---\n\n# {name}\n\n{body}");
        { let mut engine = engine_arc.lock().map_err(|e| ToolError::ExecutionFailed(format!("锁定失败: {e}")))?;
          engine.create_with_category(&name, category_str, &description, &skill_body, crate::agent::RunAs::Subagent).map_err(|e| ToolError::ExecutionFailed(format!("创建失败: {e}")))?; }
        Ok(format!("✅ 技能「{name}」已创建！\n描述: {description}\n\n现在可以使用 run_skill(name=\"{name}\") 来调用此技能。"))
    }
}

// ---------------------------------------------------------------------------
// skill_patch — 非并行安全（写磁盘）
// ---------------------------------------------------------------------------

/// 更新已有技能的内容（打补丁进化）
pub struct SkillPatch;

#[async_trait]
impl Tool for SkillPatch {
    fn name(&self) -> String { "skill_patch".into() }
    fn description(&self) -> String { "更新已有技能的内容（打补丁进化），保留使用统计".into() }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("name", ParamType::String, "要更新的技能名称"),
            ParamDef::optional("description", ParamType::String, "新的描述，留空不覆盖"),
            ParamDef::optional("body", ParamType::String, "新的技能正文 Markdown，留空不覆盖"),
            ParamDef::optional("allowed_tools", ParamType::String, "新的工具列表，逗号分隔，留空不覆盖"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let name = get_string_arg(&args, "name")?;
        let new_description = get_optional_string(&args, "description");
        let new_body = get_optional_string(&args, "body");
        let allowed_tools_str = get_optional_string(&args, "allowed_tools");

        let new_allowed_tools: Option<Vec<String>> = allowed_tools_str.map(|s| {
            s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
        });

        let engine_arc = GLOBAL_SKILL_ENGINE.get().ok_or_else(|| {
            ToolError::ExecutionFailed("技能引擎未初始化".into())
        })?;

        {
            let mut engine = engine_arc.lock().map_err(|e| {
                ToolError::ExecutionFailed(format!("锁定失败: {e}"))
            })?;

            engine.update_skill(
                &name,
                new_description.as_deref(),
                new_body.as_deref(),
                new_allowed_tools.as_deref(),
                None,
            ).map_err(|e| ToolError::ExecutionFailed(format!("更新技能失败: {e}")))?;
        }

        Ok(format!("✅ 技能「{name}」已更新！使用记录（次数/成功率）已保留。"))
    }
}

// ---------------------------------------------------------------------------
// memory — 非并行安全（写磁盘）
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 公共记忆写入函数（memory 工具 + /note + 系统内部统一使用）
// ---------------------------------------------------------------------------

/// 从磁盘读取记忆文件全部内容
pub fn memory_read_all(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// 原子写入：tmp → rename（USER.md/MEMORY.md 的唯一写入入口）
pub fn memory_atomic_write(path: &std::path::Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("写入失败: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("原子替换失败: {e}"))?;
    Ok(())
}

/// § 分隔截断：超出 max_chars 时删除最旧条目
pub fn memory_truncate_by_section(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let parts: Vec<&str> = content.split('§').collect();
    let mut kept = String::new();
    for part in parts {
        let test = if kept.is_empty() { part.to_string() } else { format!("§{}", part) };
        if kept.len() + test.len() <= max_chars {
            kept.push_str(&test);
        }
    }
    kept
}

/// 安全扫描 + 查重
fn memory_safety_check(content: &str) -> Result<(), String> {
    if content.trim().is_empty() {
        return Err("内容不能为空".into());
    }
    Ok(())
}

/// 记忆工具：读写管理 MEMORY.md 和 USER.md（双文件存储）
pub struct Memory;

impl Memory {
    /// 从磁盘读取全部内容
    fn read_all(path: &std::path::Path) -> String {
        memory_read_all(path)
    }

    /// 原子写入：tmp → rename
    fn atomic_write(path: &std::path::Path, content: &str) -> Result<(), String> {
        memory_atomic_write(path, content)
    }

    /// 安全扫描 + 查重
    fn safety_check(content: &str) -> Result<(), String> {
        memory_safety_check(content)
    }
}

#[async_trait]
impl Tool for Memory {
    fn name(&self) -> String { "memory".into() }
    fn description(&self) -> String {
        "读写管理记忆文件 MEMORY.md 和 USER.md。action: add(添加)/replace(替换)/remove(删除)/read(读取)。target: user(USER.md) 或 memory(MEMORY.md)。".into()
    }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("action", ParamType::String,
                "操作类型: add(添加)/replace(替换)/remove(删除)/read(读取)"),
            ParamDef::required("target", ParamType::String,
                "目标文件: user(USER.md 用户信息)/memory(MEMORY.md 笔记)"),
            ParamDef::optional("content", ParamType::String,
                "要添加或替换的内容（add/replace 时必填）"),
            ParamDef::optional("old_text", ParamType::String,
                "要匹配的旧文本子串（replace/remove 时必填）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let action = get_string_arg(&args, "action")?;
        let target = get_string_arg(&args, "target")?;
        let content = get_optional_string(&args, "content").unwrap_or_default();
        let old_text = get_optional_string(&args, "old_text").unwrap_or_default();

        // 解析目标文件
        let path_mgr = crate::core::PathManager::detect();
        let memories_dir = path_mgr.data_root().join("memories");
        std::fs::create_dir_all(&memories_dir)
            .map_err(|e| ToolError::ExecutionFailed(format!("创建目录失败: {e}")))?;

        let (file_name, max_chars) = match target.as_str() {
            "user" => {
                // USER.md 需要配置显式启用
                let cfg = crate::tools::get_global_config();
                let enabled = cfg.as_ref().map(|c| c.memory.user_profile_enabled).unwrap_or(false);
                if !enabled {
                    return Err(ToolError::ExecutionFailed(
                        "用户画像记忆(USER.md)未启用。请在 config.toml 中设置 [memory] user_profile_enabled = true 后重试。".into()));
                }
                ("USER.md", 1375)
            }
            "memory" => ("MEMORY.md", 2200),
            _ => return Err(ToolError::ExecutionFailed(
                format!("不支持的 target: {target}，仅支持 user 和 memory"))),
        };
        let file_path = memories_dir.join(file_name);
        // 延迟创建：文件不存在时自动创建（首次调用时）
        if !file_path.exists() {
            std::fs::write(&file_path, "")
                .map_err(|e| ToolError::ExecutionFailed(format!("创建记忆文件失败: {e}")))?;
        }

        match action.as_str() {
            "add" => {
                if content.is_empty() {
                    return Err(ToolError::ExecutionFailed("content 不能为空".into()));
                }
                let entry = format!("§ {}", content);
                Self::safety_check(&entry)
                    .map_err(|e| ToolError::ExecutionFailed(e))?;

                let mut current = Self::read_all(&file_path);

                // 查重：完全相同的条目不重复写入
                for line in current.lines() {
                    if line == entry.trim() {
                        return Ok("⏭ 条目已存在，跳过".into());
                    }
                }

                current.push_str(&entry);
                current = memory_truncate_by_section(&current, max_chars);

                Self::atomic_write(&file_path, &current)
                    .map_err(|e| ToolError::ExecutionFailed(e))?;
                tracing::info!("记忆已追加到 {}: {}", file_name, content);
                Ok(format!("✅ 已记住：{}", content))
            }

            "replace" => {
                if old_text.is_empty() {
                    return Err(ToolError::ExecutionFailed("replace 操作需要 old_text 参数".into()));
                }
                if content.is_empty() {
                    return Err(ToolError::ExecutionFailed("replace 操作需要 content 参数".into()));
                }

                let current = Self::read_all(&file_path);
                if !current.contains(&old_text) {
                    return Ok(format!("❌ 未找到包含「{old_text}」的条目，当前条目数: {}", current.matches('§').count()));
                }

                let new_content = current.replace(&old_text, &content);
                Self::atomic_write(&file_path, &new_content)
                    .map_err(|e| ToolError::ExecutionFailed(e))?;
                tracing::info!("记忆已替换: {old_text} → {content}");
                Ok(format!("✅ 已替换：{old_text} → {content}"))
            }

            "remove" => {
                if old_text.is_empty() {
                    return Err(ToolError::ExecutionFailed("remove 操作需要 old_text 参数".into()));
                }

                let current = Self::read_all(&file_path);
                if !current.contains(&old_text) {
                    return Ok(format!("❌ 未找到包含「{old_text}」的条目"));
                }

                let before_count = current.matches('§').count();
                let new_content = current.replace(&old_text, "");
                // 清理多余的 § 分隔符
                let cleaned: String = new_content.split('§')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| {
                        let trimmed = s.trim();
                        if trimmed.starts_with(' ') { format!("§{}", trimmed) } else { format!("§ {}", trimmed) }
                    })
                    .collect();
                let after_count = cleaned.matches('§').count();

                Self::atomic_write(&file_path, &cleaned)
                    .map_err(|e| ToolError::ExecutionFailed(e))?;
                tracing::info!("记忆已删除: {old_text} (移除 {})", before_count - after_count);
                Ok(format!("✅ 已删除包含「{old_text}」的条目"))
            }

            "read" => {
                let current = Self::read_all(&file_path);
                let entry_count = current.matches('§').count();
                if current.trim().is_empty() {
                    Ok(format!("📄 {} 为空（{}/{} 字符）", file_name, 0, max_chars))
                } else {
                    Ok(format!("📄 {} 共 {} 条记录（{}/{} 字符）\n{}",
                        file_name, entry_count, current.len(), max_chars, current.trim()))
                }
            }

            _ => Err(ToolError::ExecutionFailed(
                format!("不支持的 action: {action}，仅支持 add/replace/remove/read"))),
        }
    }
}

// ---------------------------------------------------------------------------
// skill_manage — 非并行安全（写磁盘）
// ---------------------------------------------------------------------------

/// 合并 skill_create + skill_patch：名称存在则更新，不存在则创建
pub struct SkillManage;

#[async_trait]
impl Tool for SkillManage {
    fn name(&self) -> String { "skill_manage".into() }
    fn description(&self) -> String { "创建或更新技能，名称存在则 patch，不存在则 create".into() }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("name", ParamType::String, "技能名称（小写英文+短横线）"),
            ParamDef::required("description", ParamType::String, "一句话描述该技能的用途"),
            ParamDef::required("body", ParamType::String, "技能正文 Markdown"),
            ParamDef::optional("category", ParamType::String, "技能分类目录"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let name = get_string_arg(&args, "name")?;
        let description = get_string_arg(&args, "description")?;
        let body = get_string_arg(&args, "body")?;
        let category = get_optional_string(&args, "category");
        let engine_arc = GLOBAL_SKILL_ENGINE.get().ok_or_else(|| ToolError::ExecutionFailed("技能引擎未初始化".into()))?;

        let mut engine = engine_arc.lock().map_err(|e| ToolError::ExecutionFailed(format!("锁定失败: {e}")))?;

        // 检查技能是否存在
        if engine.get(&name).is_some() {
            // 存在 → patch
            engine.update_skill(&name, Some(&description), Some(&body), None, None)
                .map_err(|e| ToolError::ExecutionFailed(format!("更新技能失败: {e}")))?;
            Ok(format!("✅ 技能「{name}」已更新（自动提炼）"))
        } else {
            // 不存在 → create
            let category_str = category.as_deref();
            let skill_body = format!("---\nname: {name}\ndescription: {description}\nrun_as: subagent\n---\n\n# {name}\n\n{body}");
            engine.create_with_category(&name, category_str, &description, &skill_body, crate::agent::RunAs::Subagent)
                .map_err(|e| ToolError::ExecutionFailed(format!("创建技能失败: {e}")))?;
            Ok(format!("✅ 技能「{name}」已创建（自动提炼）"))
        }
    }
}

/// 全局技能引擎（供所有 skill_* 工具使用）
static GLOBAL_SKILL_ENGINE: OnceLock<Arc<std::sync::Mutex<crate::agent::SkillEngine>>> = OnceLock::new();

/// 设置全局技能引擎
pub fn set_global_skill_engine(engine: Arc<std::sync::Mutex<crate::agent::SkillEngine>>) -> bool {
    GLOBAL_SKILL_ENGINE.set(engine).is_ok()
}

/// 获取全局技能引擎引用
pub fn get_global_skill_engine() -> Option<Arc<std::sync::Mutex<crate::agent::SkillEngine>>> {
    GLOBAL_SKILL_ENGINE.get().cloned()
}

/// 获取全局配置
pub fn get_global_config() -> Option<crate::core::Config> {
    GLOBAL_CONFIG.get().cloned()
}

// ---------------------------------------------------------------------------
// 注册所有内置工具
// ---------------------------------------------------------------------------

/// 全局配置（子 Agent 工具使用）
static GLOBAL_CONFIG: OnceLock<crate::core::Config> = OnceLock::new();

/// 设置全局配置（在 main.rs 中调用）
pub fn set_global_config(config: crate::core::Config) {
    let _ = GLOBAL_CONFIG.set(config);
}

/// 全局 Transport（Provider Pool）
static GLOBAL_TRANSPORT: OnceLock<Arc<dyn crate::provider::Transport>> = OnceLock::new();

/// 设置全局 Transport
pub fn set_global_transport(transport: Arc<dyn crate::provider::Transport>) -> bool {
    GLOBAL_TRANSPORT.set(transport).is_ok()
}

/// 获取全局 Transport
pub fn get_global_transport() -> Option<Arc<dyn crate::provider::Transport>> {
    GLOBAL_TRANSPORT.get().cloned()
}

use crate::tools::ToolRegistry;
use crate::api::ToolDef;

/// 创建包含所有内置工具的注册表
pub fn builtin_registry(config: &crate::core::Config) -> ToolRegistry {
    let fetch_client = crate::core::http_client::create_proxied_client(
        &config.proxy, "web_fetch", std::time::Duration::from_secs(30),
    );
    let command_proxy = if config.proxy.need_proxy("command") {
        config.proxy.url.clone()
    } else {
        None
    };

    ToolRegistry::new()
        .register(ReadFile)
        .register(SearchContent)
        .register(Glob)
        .register(WriteFile)
        .register(RunCommand::new(command_proxy))
        .register(GetCurrentTime)
        .register(RunSkill)
        .register(SkillList)
        .register(SkillSearch)
        .register(SkillCreate)
        .register(SkillPatch)
        .register(WebSearch)
        .register(WebFetch::new(fetch_client))
        .register(DelegateTask)
        .register(ReadPdf)
        .register(SkillManage)
        .register(Memory)
}

// ---------------------------------------------------------------------------
// MCP 融合注册
// ---------------------------------------------------------------------------

/// 全局注册表实例（包含内置工具 + MCP 远程工具）
/// 在 full_registry() 完成后初始化，供 all_tool_defs() 使用
static GLOBAL_REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();

/// 获取所有工具定义（内置 + MCP）
///
/// 从 GLOBAL_REGISTRY 动态生成 ToolDef，确保与注册表一致。
/// 未初始化时 fallback 到 default_tools() 作为兼容后备。
pub fn all_tool_defs() -> Vec<ToolDef> {
    if let Some(registry) = GLOBAL_REGISTRY.get() {
        crate::api::tools_from_registry(registry)
    } else {
        #[allow(deprecated)]
        crate::api::default_tools()
    }
}

/// MCP 连接报告，包含成功/失败的 Server 列表
#[derive(Default)]
pub struct McpConnectReport {
    pub connected_servers: Vec<String>,
    pub failed_servers: Vec<(String, String)>,
    pub total_tools: usize,
}

/// 创建包含内置工具 + MCP 远程工具的完整注册表
///
/// 返回 (registry, report)，report 包含各 MCP Server 的连接结果。
pub async fn full_registry(mcp_config: &crate::core::McpConfig) -> (ToolRegistry, McpConnectReport) {
    let config = GLOBAL_CONFIG.get().cloned().unwrap_or_default();
    let mut registry = builtin_registry(&config);
    let mut report = McpConnectReport {
        connected_servers: Vec::new(),
        failed_servers: Vec::new(),
        total_tools: 0,
    };

    if !mcp_config.enabled || mcp_config.servers.is_empty() {
        let _ = GLOBAL_REGISTRY.set(registry.clone());
        return (registry, report);
    }

    let mut manager = crate::mcp::McpAdapterManager::new();
    let mut mcp_tool_count: usize = 0;

    // 手动逐一连接每个 Server，收集成功/失败结果
    for (name, config) in &mcp_config.servers {
        match crate::mcp::McpAdapter::connect(name.clone(), config).await {
            Ok(adapter) => {
                let tool_count = adapter.tools().len();
                mcp_tool_count += tool_count;
                report.connected_servers.push(format!("{} ({} 工具)", name, tool_count));

                let parallel_safe = adapter.parallel_safe();
                let tool_parallel_config = adapter.tool_parallel_safe().clone();

                let adapter_arc = Arc::new(adapter);

                for tool_info in adapter_arc.tools() {
                    let remote_tool = crate::mcp::McpRemoteTool::new(
                        name,
                        &tool_info.original_name,
                        tool_info.description.clone(),
                        tool_info.input_schema.clone(),
                        Arc::clone(&adapter_arc),
                        parallel_safe,
                        &tool_parallel_config,
                    );
                    registry = registry.register(remote_tool);
                }

                manager.adapters_mut().push(adapter_arc);
            }
            Err(e) => {
                report.failed_servers.push((name.clone(), e.to_string()));
                tracing::warn!("MCP [{}] 连接失败: {}", name, e);
            }
        }
    }

    report.total_tools = mcp_tool_count;
    let server_count = manager.adapters().len();

    // 保存 manager 到全局静态，以便进程退出时优雅关闭
    if let Err(_) = GLOBAL_MCP_MANAGER.set(manager) {
        tracing::warn!("GLOBAL_MCP_MANAGER 已存在（理论上不会发生）");
    }

    tracing::info!(
        "MCP 已就绪 · {} 个 Server · {} 个远程工具",
        server_count,
        mcp_tool_count,
    );

    // 启动后台健康检查任务（每 5 分钟检查一次，首次 30 秒后开始）
    if server_count > 0 {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                if let Some(manager) = GLOBAL_MCP_MANAGER.get() {
                    for adapter in manager.adapters() {
                        if let Err(e) = adapter.health_check().await {
                            tracing::warn!("MCP [{}] 健康检查失败: {}", adapter.server_name(), e);
                            if let Err(re) = adapter.reconnect().await {
                                tracing::error!("MCP [{}] 重连失败: {}", adapter.server_name(), re);
                            }
                        }
                    }
                }
            }
        });
    }

    // ── WASM 插件注册 ──
    if config.wasm.enabled {
        let wasm_tools = crate::tools::wasm_plugin::load_plugins(
            &config.wasm.plugins_dir,
            &config.wasm,
        );
        if !wasm_tools.is_empty() {
            registry = registry.register_all(wasm_tools);
        }
    }

    // 保存最终注册表到全局，供 all_tool_defs() 动态生成 ToolDef
    let _ = GLOBAL_REGISTRY.set(registry.clone());
    (registry, report)
}

/// 关闭所有 MCP 连接（释放子进程、关闭 SSE 等）
///
/// 应在进程退出前调用，确保 MCP Server 被优雅关闭。
pub async fn shutdown_mcp() {
    if let Some(manager) = GLOBAL_MCP_MANAGER.get() {
        manager.shutdown_all().await;
        tracing::info!("MCP 连接已关闭");
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn test_read_file_metadata() {
        let tool = ReadFile;
        assert_eq!(tool.name(), "read_file");
        assert!(tool.parallel_safe());
        assert!(tool.parameters().len() >= 2);
    }

    #[test]
    fn test_write_file_not_parallel_safe() {
        let tool = WriteFile;
        assert!(!tool.parallel_safe());
    }

    #[test]
    fn test_run_command_not_parallel_safe() {
        let tool = RunCommand::new(None);
        assert!(!tool.parallel_safe());
    }

    #[test]
    fn test_search_content_parallel_safe() {
        let tool = SearchContent;
        assert!(tool.parallel_safe());
    }

    #[test]
    fn test_builtin_registry() {
        let reg = builtin_registry(&crate::core::Config::default());
        assert_eq!(reg.len(), 17);
        assert!(reg.get("read_pdf").is_some());
        assert!(reg.get("skill_manage").is_some());
        assert!(reg.get("memory").is_some());
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("write_file").is_some());
        assert!(reg.get("run_command").is_some());
        assert!(reg.get("search_content").is_some());
        assert!(reg.get("glob").is_some());
        assert!(reg.get("get_current_time").is_some());
        assert!(reg.get("run_skill").is_some());
        assert!(reg.get("skill_list").is_some());
        assert!(reg.get("skill_search").is_some());
        assert!(reg.get("skill_create").is_some());
        assert!(reg.get("skill_patch").is_some());
        assert!(reg.get("web_search").is_some());
        assert!(reg.get("web_fetch").is_some());
        assert!(reg.get("delegate_task").is_some());
    }

    #[test]
    fn test_parallel_safe_classification() {
        let reg = builtin_registry(&crate::core::Config::default());
        let safe = reg.parallel_safe_names();
        assert!(safe.iter().any(|s| s == "read_file"));
        assert!(safe.iter().any(|s| s == "search_content"));
        assert!(safe.iter().any(|s| s == "glob"));
        assert!(!safe.iter().any(|s| s == "write_file"));
        assert!(!safe.iter().any(|s| s == "run_command"));
    }

    #[test]
    fn test_dangerous_command_blocked() {
        let blocked = ["rm -rf /", "format C:", "shutdown /s", "net user hacker pwd /add"];
        for cmd in &blocked {
            assert!(is_dangerous_command(cmd).is_some(), "应拦截: {}", cmd);
        }
        let allowed = ["cargo build", "npm install", "python main.py", "ls -la", "git status"];
        for cmd in &allowed {
            assert!(is_dangerous_command(cmd).is_none(), "不应拦截: {}", cmd);
        }
    }

    #[test]
    fn test_protected_path_blocked() {
        let blocked = ["config.toml", ".env", "/etc/passwd", ".ssh/id_rsa"];
        for p in &blocked {
            assert!(is_protected_path(p), "应保护: {}", p);
        }
        let allowed = ["src/main.rs", "README.md", "output.txt", "data/config_backup.md"];
        for p in &allowed {
            assert!(!is_protected_path(p), "不应保护: {}", p);
        }
    }

    #[test]
    fn test_untrusted_wrapping() {
        let content = "Hello World";
        let wrapped = format!("<untrusted>\n{}\n</untrusted>", content);
        assert!(wrapped.starts_with("<untrusted>"));
        assert!(wrapped.ends_with("</untrusted>"));
        assert!(wrapped.contains("Hello World"));
    }
}
