//! RHermes 内置工具实现
//!
//! 每个工具都实现了 `Tool` trait，并声明 `parallel_safe` 标志。

use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;

use crate::tools::{
    get_optional_string, get_string_arg, ParamDef, ParamType, Tool, ToolError,
};

// ---------------------------------------------------------------------------
// read_file — 并行安全
// ---------------------------------------------------------------------------

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "读取文件内容，可指定行范围（head/tail/range）"
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
        let content = tokio::fs::read_to_string(&path).await.map_err(ToolError::Io)?;

        let head = args.get("head").and_then(|v| v.as_u64());
        let tail = args.get("tail").and_then(|v| v.as_u64());

        let result = if let Some(n) = head {
            content.lines().take(n as usize).collect::<Vec<_>>().join("\n")
        } else if let Some(n) = tail {
            let lines: Vec<&str> = content.lines().collect();
            let len = lines.len();
            lines[len.saturating_sub(n as usize)..].join("\n")
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

/// 将 glob 模式转换为正则（简单版本）
fn glob_to_regex(glob: &str) -> Regex {
    let mut re = String::with_capacity(glob.len() + 4);
    re.push('^');
    for ch in glob.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' => re.push_str("\\."),
            '\\' => re.push_str("\\\\"),
            '|' => re.push('|'),
            c if c.is_ascii_punctuation() => {
                re.push('\\');
                re.push(c);
            }
            c => re.push(c),
        }
    }
    re.push('$');
    Regex::new(&re).unwrap_or_else(|_| Regex::new(".*").unwrap())
}

// ---------------------------------------------------------------------------
// search_content — 并行安全
// ---------------------------------------------------------------------------

pub struct SearchContent;

#[async_trait]
impl Tool for SearchContent {
    fn name(&self) -> &'static str {
        "search_content"
    }
    fn description(&self) -> &'static str {
        "在文件中搜索文本模式，返回匹配的文件:行号"
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("pattern", ParamType::String, "搜索模式（支持正则）"),
            ParamDef::optional("path", ParamType::String, "搜索目录（默认项目根）"),
            ParamDef::optional("glob", ParamType::String, "文件名过滤"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let pattern = get_string_arg(&args, "pattern")?;
        let search_path = get_optional_string(&args, "path").unwrap_or_else(|| ".".into());
        let glob_filter = get_optional_string(&args, "glob");

        // 编译正则
        let re = Regex::new(&pattern).map_err(|e| {
            ToolError::ExecutionFailed(format!("正则无效: {e}"))
        })?;

        // 收集要搜索的文件
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let root = std::path::Path::new(&search_path);
        if root.is_file() {
            files.push(root.to_path_buf());
        } else {
            collect_files(root, &mut files).map_err(ToolError::Io)?;
        }

        // 过滤 glob
        if let Some(ref g) = glob_filter {
            let glob_re = glob_to_regex(g);
            files.retain(|f| {
                let name = f.to_string_lossy();
                glob_re.is_match(&name)
            });
        }

        // 限制搜索文件数（防止爆炸）
        if files.len() > 5000 {
            files.truncate(5000);
        }

        // 逐文件搜索
        let max_results = 200;
        let mut results: Vec<String> = Vec::new();

        for file_path in &files {
            if results.len() >= max_results {
                break;
            }
            let content = match tokio::fs::read_to_string(file_path).await {
                Ok(c) => c,
                Err(_) => continue, // 跳过二进制/不可读文件
            };
            for (line_no, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    results.push(format!("{}:{}:{}", file_path.display(), line_no + 1, line));
                    if results.len() >= max_results {
                        break;
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(format!("未找到匹配 \"{pattern}\" 的内容"))
        } else {
            Ok(format!("找到 {} 处匹配:\n{}", results.len(), results.join("\n")))
        }
    }
}

// ---------------------------------------------------------------------------
// glob — 并行安全
// ---------------------------------------------------------------------------

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "按 glob 模式列出文件"
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
        let path = get_optional_string(&args, "path").unwrap_or_else(|| ".".into());

        let mut cmd = tokio::process::Command::new("fd");
        cmd.arg("--glob").arg(&pattern).arg(&path);

        let output = cmd.output().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("glob 失败: {e}"))
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let files: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        Ok(format!("找到 {} 个文件:\n{}", files.len(), files.join("\n")))
    }
}

// ---------------------------------------------------------------------------
// write_file — 非并行安全（副作用）
// ---------------------------------------------------------------------------

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "写入文件（创建或覆盖）"
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
        let content = get_string_arg(&args, "content")?;

        // 确保父目录存在
        if let Some(parent) = Path::new(&path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(ToolError::Io)?;
        }

        tokio::fs::write(&path, &content)
            .await
            .map_err(ToolError::Io)?;

        Ok(format!("已写入 {} ({} 字节)", path, content.len()))
    }
}

// ---------------------------------------------------------------------------
// run_command — 非并行安全（副作用）
// ---------------------------------------------------------------------------

pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &'static str {
        "run_command"
    }
    fn description(&self) -> &'static str {
        "在 shell 中执行命令"
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
    fn name(&self) -> &'static str {
        "get_current_time"
    }
    fn description(&self) -> &'static str {
        "获取当前日期和时间"
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![]
    }
    async fn execute(&self, _args: Value) -> Result<String, ToolError> {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S (UTC+8)");
        Ok(now.to_string())
    }
}

// ---------------------------------------------------------------------------
// web_search — 非并行安全（网络请求）
// ---------------------------------------------------------------------------

/// 搜索网络信息
///
/// 使用 DuckDuckGo Instant Answer API，无需 API Key。
/// 返回搜索结果摘要和相关链接。
pub struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }
    fn description(&self) -> &'static str {
        "搜索网络获取最新信息"
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("query", ParamType::String, "搜索关键词"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let query = get_string_arg(&args, "query")?;
        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
            urlencoding(&query)
        );

        let resp = reqwest::get(&url)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("搜索请求失败: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("解析搜索结果失败: {e}")))?;

        let mut results = Vec::new();

        // 摘要
        if let Some(abstract_text) = body["AbstractText"].as_str() {
            if !abstract_text.is_empty() {
                results.push(format!("📝 摘要: {abstract_text}"));
                if let Some(src) = body["AbstractSource"].as_str() {
                    results.push(format!("   来源: {src}"));
                }
                if let Some(url) = body["AbstractURL"].as_str() {
                    results.push(format!("   链接: {url}"));
                }
                results.push(String::new());
            }
        }

        // 答案
        if let Some(answer) = body["Answer"].as_str() {
            if !answer.is_empty() {
                results.push(format!("💡 答案: {answer}"));
                if let Some(url) = body["AnswerURL"].as_str() {
                    if !url.is_empty() {
                        results.push(format!("   链接: {url}"));
                    }
                }
                results.push(String::new());
            }
        }

        // 相关结果
        if let Some(topics) = body["RelatedTopics"].as_array() {
            for topic in topics.iter().take(8) {
                if let Some(text) = topic["Text"].as_str() {
                    if let Some(url) = topic["FirstURL"].as_str() {
                        results.push(format!("🔗 {text}"));
                        results.push(format!("   {url}"));
                    } else {
                        results.push(format!("🔗 {text}"));
                    }
                }
                // 处理嵌套的 Topics
                if let Some(sub_topics) = topic["Topics"].as_array() {
                    for sub in sub_topics.iter().take(3) {
                        if let Some(text) = sub["Text"].as_str() {
                            if let Some(url) = sub["FirstURL"].as_str() {
                                results.push(format!("  • {text}"));
                                results.push(format!("    {url}"));
                            }
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(format!("未找到与「{query}」相关的搜索结果"))
        } else {
            Ok(format!("搜索结果「{query}」:\n{}", results.join("\n")))
        }
    }
}

/// URL 编码（简单版本，仅编码中文和特殊字符）
fn urlencoding(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push_str("%20"),
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// web_fetch — 非并行安全（网络请求）
// ---------------------------------------------------------------------------

/// 获取网页内容
///
/// 下载指定 URL 的内容并提取可读文本。
pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "获取网页内容并提取可读文本"
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("url", ParamType::String, "要获取的网页 URL"),
            ParamDef::optional("max_chars", ParamType::Integer, "最大字符数（默认 5000）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let url = get_string_arg(&args, "url")?;
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(5000) as usize;

        let resp = reqwest::get(&url)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("请求失败: {e}")))?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("读取响应失败: {e}")))?;

        // 提取可读文本（简单 HTML 标签剥离）
        let text = if content_type.contains("text/html") {
            strip_html_tags(&body)
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
        Ok(format!(
            "[{}] HTTP {status} · {content_type}\n{}{}",
            url, text, extra,
        ))
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
    fn name(&self) -> &'static str {
        "delegate_task"
    }
    fn description(&self) -> &'static str {
        "将子任务委托给独立的子 Agent 执行，返回分析结果"
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

        let result = crate::agent::run_sub_agent(&task, &context, config).await;

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
    fn name(&self) -> &'static str {
        "run_skill"
    }
    fn description(&self) -> &'static str {
        "执行一个已安装的技能并返回结果"
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
    fn name(&self) -> &'static str { "skill_list" }
    fn description(&self) -> &'static str { "列出所有已安装的技能名称和描述" }
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
    fn name(&self) -> &'static str { "skill_search" }
    fn description(&self) -> &'static str { "按关键词搜索已安装的技能" }
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
    fn name(&self) -> &'static str { "skill_create" }
    fn description(&self) -> &'static str { "创建新的可复用技能，让 AI 不断积累最佳实践" }
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

/// 全局技能引擎（供所有 skill_* 工具使用）
static GLOBAL_SKILL_ENGINE: OnceLock<Arc<std::sync::Mutex<crate::agent::SkillEngine>>> = OnceLock::new();

/// 设置全局技能引擎
pub fn set_global_skill_engine(engine: Arc<std::sync::Mutex<crate::agent::SkillEngine>>) -> bool {
    GLOBAL_SKILL_ENGINE.set(engine).is_ok()
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

use crate::tools::ToolRegistry;

/// 创建包含所有内置工具的注册表
pub fn builtin_registry() -> ToolRegistry {
    ToolRegistry::new()
        .register(ReadFile)
        .register(SearchContent)
        .register(Glob)
        .register(WriteFile)
        .register(RunCommand)
        .register(GetCurrentTime)
        .register(RunSkill)
        .register(SkillList)
        .register(SkillSearch)
        .register(SkillCreate)
        .register(WebSearch)
        .register(WebFetch)
        .register(DelegateTask)
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
        let tool = RunCommand;
        assert!(!tool.parallel_safe());
    }

    #[test]
    fn test_search_content_parallel_safe() {
        let tool = SearchContent;
        assert!(tool.parallel_safe());
    }

    #[test]
    fn test_builtin_registry() {
        let reg = builtin_registry();
        assert_eq!(reg.len(), 13);
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
        assert!(reg.get("web_search").is_some());
        assert!(reg.get("web_fetch").is_some());
        assert!(reg.get("delegate_task").is_some());
    }

    #[test]
    fn test_parallel_safe_classification() {
        let reg = builtin_registry();
        let safe = reg.parallel_safe_names();
        assert!(safe.contains(&"read_file"));
        assert!(safe.contains(&"search_content"));
        assert!(safe.contains(&"glob"));
        assert!(!safe.contains(&"write_file"));
        assert!(!safe.contains(&"run_command"));
    }
}
