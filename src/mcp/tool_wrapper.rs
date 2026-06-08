//! MCP 远程工具的 Tool trait 适配器

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::tools::{ParamDef, ParamType, Tool, ToolError};

use super::adapter::McpAdapter;

/// MCP 远程工具的 Tool trait 适配器
pub struct McpRemoteTool {
    full_name: String,
    description: String,
    parameters: Vec<ParamDef>,
    parallel_safe: bool,
    original_name: String,
    adapter: Arc<McpAdapter>,
}

impl McpRemoteTool {
    pub fn new(
        server_name: &str,
        original_name: &str,
        description: String,
        input_schema: Value,
        adapter: Arc<McpAdapter>,
        server_parallel_safe: bool,
        tool_parallel_config: &HashMap<String, bool>,
    ) -> Self {
        let full_name = format!("mcp__{}__{}", server_name, original_name);
        let parameters = Self::parse_params(&input_schema);
        // per-tool 配置优先，fallback 到 Server 级别
        let parallel_safe = tool_parallel_config.get(original_name)
            .copied()
            .unwrap_or(server_parallel_safe);
        Self { full_name, description, parameters, parallel_safe, original_name: original_name.to_string(), adapter }
    }

    fn parse_params(schema: &Value) -> Vec<ParamDef> {
        let properties = match schema.get("properties").and_then(|p| p.as_object()) {
            Some(obj) => obj, None => return Vec::new(),
        };
        let required: Vec<String> = schema.get("required")
            .and_then(|r| r.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        properties.iter().map(|(name, prop_schema)| {
            let type_str = prop_schema.get("type").and_then(|t| t.as_str()).unwrap_or("string");
            let desc = prop_schema.get("description").and_then(|d| d.as_str()).unwrap_or("");
            let param_type = match type_str {
                "integer" => ParamType::Integer, "number" => ParamType::Float,
                "boolean" => ParamType::Boolean, "array" => ParamType::Array,
                "object" => ParamType::Object, _ => ParamType::String,
            };
            let is_required = required.iter().any(|r| r == name);
            if is_required {
                ParamDef::required(name.as_str(), param_type, desc)
            } else {
                ParamDef::optional(name.as_str(), param_type, desc)
            }
        }).collect()
    }
}

#[async_trait]
impl Tool for McpRemoteTool {
    fn name(&self) -> String { self.full_name.clone() }
    fn description(&self) -> String { self.description.clone() }
    fn parallel_safe(&self) -> bool { self.parallel_safe }
    fn parameters(&self) -> Vec<ParamDef> { self.parameters.clone() }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        tracing::info!("MCP 工具调用: {} (server={})", self.full_name, self.adapter.server_name());
        match self.adapter.call_tool(&self.original_name, args).await {
            Ok(result) => Ok(result),
            Err(e) => {
                tracing::warn!("MCP 工具调用失败: {} — {}", self.full_name, e);
                Err(ToolError::ExecutionFailed(format!("MCP 工具调用失败: {e}")))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_params_from_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "搜索关键词"},
                "limit": {"type": "integer", "description": "最大结果数"}
            },
            "required": ["query"]
        });
        let params = McpRemoteTool::parse_params(&schema);
        assert_eq!(params.len(), 2);
        assert!(params.iter().any(|p| p.name == "query" && p.required));
        assert!(params.iter().any(|p| p.name == "limit" && !p.required));
    }

    #[test]
    fn test_parse_empty_schema() {
        let schema = serde_json::json!({"type": "object"});
        let params = McpRemoteTool::parse_params(&schema);
        assert!(params.is_empty());
    }
}
