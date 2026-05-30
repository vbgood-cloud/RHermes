//! RHermes 内置工具实现
//!
//! 每个工具都实现了 `Tool` trait，并声明 `parallel_safe` 标志。

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use crate::tool::{
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
        let path = get_optional_string(&args, "path").unwrap_or_else(|| ".".into());
        let glob = get_optional_string(&args, "glob");

        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("-n").arg(&pattern).arg(&path);

        if let Some(g) = glob {
            cmd.arg("-g").arg(&g);
        }

        let output = cmd.output().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("搜索失败: {e}"))
        })?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            Ok(if stdout.is_empty() {
                format!("未找到匹配 \"{pattern}\" 的内容")
            } else {
                format!("找到 {} 处匹配:\n{stdout}", stdout.lines().count())
            })
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Ok(format!("搜索完成（无匹配或出错）: {stderr}"))
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
            ParamDef::required("command", ParamType::String, "要执行的命令"),
            ParamDef::optional("timeout", ParamType::Integer, "超时秒数（默认 60）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let command = get_string_arg(&args, "command")?;
        let timeout = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(60);

        let cmd = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output();

        let output = tokio::time::timeout(Duration::from_secs(timeout), cmd)
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
// 注册所有内置工具
// ---------------------------------------------------------------------------

use crate::tool::ToolRegistry;

/// 创建包含所有内置工具的注册表
pub fn builtin_registry() -> ToolRegistry {
    ToolRegistry::new()
        .register(ReadFile)
        .register(SearchContent)
        .register(Glob)
        .register(WriteFile)
        .register(RunCommand)
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
        assert_eq!(reg.len(), 5);
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("write_file").is_some());
        assert!(reg.get("run_command").is_some());
        assert!(reg.get("search_content").is_some());
        assert!(reg.get("glob").is_some());
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
