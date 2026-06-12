//! RHermes 工具系统
//!
//! 工具注册表 + 元数据模型 + 内置工具实现。
//!
//! 每个工具有 `parallel_safe` 标志：
//! - `true`  → 可与其他 parallel_safe 工具并行执行（读文件、搜索）
//! - `false` → 必须串行执行（写文件、执行命令）

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};


use serde_json::Value;

// ---------------------------------------------------------------------------
// 参数类型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParamType {
    String,
    Integer,
    Float,
    Boolean,
    Array,
    Object,
}

impl std::fmt::Display for ParamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String => write!(f, "string"),
            Self::Integer => write!(f, "integer"),
            Self::Float => write!(f, "float"),
            Self::Boolean => write!(f, "boolean"),
            Self::Array => write!(f, "array"),
            Self::Object => write!(f, "object"),
        }
    }
}

// ---------------------------------------------------------------------------
// 参数定义
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDef {
    pub name: String,
    pub param_type: ParamType,
    pub description: String,
    pub required: bool,
}

impl ParamDef {
    pub fn new(
        name: &str,
        param_type: ParamType,
        description: &str,
        required: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            param_type,
            description: description.to_string(),
            required,
        }
    }

    pub fn required(name: &str, param_type: ParamType, description: &str) -> Self {
        Self::new(name, param_type, description, true)
    }

    pub fn optional(name: &str, param_type: ParamType, description: &str) -> Self {
        Self::new(name, param_type, description, false)
    }
}

// ---------------------------------------------------------------------------
// 工具调用
// ---------------------------------------------------------------------------

/// 来自 API 的 tool_call 请求
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// 工具执行结果
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub success: bool,
    pub output: String,
    pub duration_ms: u64,
}

impl ToolResult {
    pub fn success(call_id: String, name: String, output: String, duration_ms: u64) -> Self {
        Self {
            call_id,
            name,
            success: true,
            output,
            duration_ms,
        }
    }

    pub fn error(call_id: String, name: String, error: String) -> Self {
        Self {
            call_id,
            name,
            success: false,
            output: error,
            duration_ms: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// 工具特征
// ---------------------------------------------------------------------------

/// 所有工具必须实现的特征
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// 工具名称（用于 API tool_calls 匹配）
    fn name(&self) -> String;

    /// 工具描述（用于模型理解何时调用）
    fn description(&self) -> String;

    /// 是否可并行执行
    fn parallel_safe(&self) -> bool;

    /// 参数定义列表
    fn parameters(&self) -> Vec<ParamDef>;

    /// 执行工具调用
    async fn execute(&self, args: Value) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// 工具错误
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ToolError {
    MissingParam(String),
    InvalidParam(String),
    ExecutionFailed(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingParam(p) => write!(f, "缺少必需参数: {p}"),
            Self::InvalidParam(p) => write!(f, "参数无效: {p}"),
            Self::ExecutionFailed(e) => write!(f, "执行失败: {e}"),
            Self::Io(e) => write!(f, "IO 错误: {e}"),
        }
    }
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::error::Error for ToolError {}

// ---------------------------------------------------------------------------
// 工具注册表
// ---------------------------------------------------------------------------

/// 全局工具注册表
/// 所有工具在启动时注册，运行期只读
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    /// 创建空的注册表
    pub fn new() -> Self {
        Self {
            tools: Arc::new(HashMap::new()),
        }
    }

    /// 注册一个工具
    pub fn register<T: Tool + 'static>(mut self, tool: T) -> Self {
        let name = tool.name();
        let tools = Arc::make_mut(&mut self.tools);
        tools.insert(name, Arc::new(tool));
        self
    }

    /// 批量注册工具
    pub fn register_all(self, tools: Vec<Arc<dyn Tool>>) -> Self {
        let mut map = HashMap::new();
        for tool in tools {
            map.insert(tool.name(), tool);
        }
        // 合并已有的工具
        let mut combined = (*self.tools).clone();
        combined.extend(map);
        Self {
            tools: Arc::new(combined),
        }
    }

    /// 根据名称获取工具
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// 获取所有可并行工具的名称
    pub fn parallel_safe_names(&self) -> Vec<String> {
        self.tools
            .values()
            .filter(|t| t.parallel_safe())
            .map(|t| t.name())
            .collect()
    }

    /// 获取所有工具的名称
    pub fn all_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 工具数量
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 获取所有工具的（名称, 工具）对
    pub fn all_entries(&self) -> Vec<(String, Arc<dyn Tool>)> {
        self.tools.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 辅助函数：从 JSON Value 提取字符串参数
// ---------------------------------------------------------------------------

pub fn get_string_arg(args: &Value, name: &str) -> Result<String, ToolError> {
    args.get(name)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| ToolError::MissingParam(name.into()))
}

pub fn get_optional_string(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(|v| v.as_str()).map(String::from)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个测试用的模拟工具
    struct MockTool {
        name: String,
        safe: bool,
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> String {
            self.name.clone()
        }
        fn description(&self) -> String {
            "A mock tool for testing".into()
        }
        fn parallel_safe(&self) -> bool {
            self.safe
        }
        fn parameters(&self) -> Vec<ParamDef> {
            vec![ParamDef::required("input", ParamType::String, "test input")]
        }
        async fn execute(&self, args: Value) -> Result<String, ToolError> {
            let input = get_string_arg(&args, "input")?;
            Ok(format!("mock: {input}"))
        }
    }

    #[tokio::test]
    async fn test_tool_registry_register_and_get() {
        let registry = ToolRegistry::new()
            .register(MockTool { name: "mock_read".into(), safe: true })
            .register(MockTool { name: "mock_write".into(), safe: false });

        assert_eq!(registry.len(), 2);

        let read_tool = registry.get("mock_read").unwrap();
        assert!(read_tool.parallel_safe());

        let write_tool = registry.get("mock_write").unwrap();
        assert!(!write_tool.parallel_safe());
    }

    #[tokio::test]
    async fn test_tool_execution() {
        let tool = MockTool { name: "test".into(), safe: true };
        let args = serde_json::json!({"input": "hello"});
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result, "mock: hello");
    }

    #[tokio::test]
    async fn test_tool_missing_param() {
        let tool = MockTool { name: "test".into(), safe: true };
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_registry_parallel_safe_names() {
        let registry = ToolRegistry::new()
            .register(MockTool { name: "read".into(), safe: true })
            .register(MockTool { name: "search".into(), safe: true })
            .register(MockTool { name: "write".into(), safe: false });

        let safe_names = registry.parallel_safe_names();
        assert!(safe_names.iter().any(|n| n == "read"));
        assert!(safe_names.iter().any(|n| n == "search"));
        assert!(!safe_names.iter().any(|n| n == "write"));
    }

    #[test]
    fn test_get_string_arg() {
        let val = serde_json::json!({"path": "src/main.rs", "optional": null});
        assert_eq!(get_string_arg(&val, "path").unwrap(), "src/main.rs");
        assert!(get_string_arg(&val, "missing").is_err());
    }
}
