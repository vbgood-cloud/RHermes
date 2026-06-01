//! RHermes 内置工具实现
//!
//! 每个工具都实现了 `Tool` trait，并声明 `parallel_safe` 标志。

use std::path::Path;
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
        assert_eq!(reg.len(), 6);
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("write_file").is_some());
        assert!(reg.get("run_command").is_some());
        assert!(reg.get("search_content").is_some());
        assert!(reg.get("glob").is_some());
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
