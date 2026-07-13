//! 护栏系统 — 响应验证器 + 纠正消息构建器
//!
//! 在 RepairPipeline 之后校验 tool_calls 的合法性：
//! - 工具名是否存在
//! - 必填参数是否齐全
//! 校验失败时构建纠正消息注入 Context，让模型自我修正。

use std::sync::Arc;

use crate::agent::repair::RepairedToolCall;
use crate::tools::{ParamDef, ToolRegistry};

/// 校验错误类型
#[derive(Debug, Clone)]
pub enum ValidationError {
    /// 工具名不存在
    UnknownTool(String),
    /// 缺少必填参数
    MissingRequiredParam { tool: String, param: String },
}

/// 校验结果
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// 通过校验的调用
    pub valid_calls: Vec<RepairedToolCall>,
    /// 失败的调用及其错误
    pub errors: Vec<ValidationError>,
}

impl ValidationResult {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// 响应验证器
pub struct ResponseValidator {
    registry: Arc<ToolRegistry>,
}

impl ResponseValidator {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }

    /// 校验一组 RepairedToolCall
    pub fn validate(&self, calls: &[RepairedToolCall]) -> ValidationResult {
        let mut valid_calls = Vec::new();
        let mut errors = Vec::new();

        for call in calls {
            let Some(tool) = self.registry.get(&call.name) else {
                errors.push(ValidationError::UnknownTool(call.name.clone()));
                continue;
            };

            // 检查必填参数
            let params: Vec<ParamDef> = tool.parameters();
            let mut param_errors = false;
            for pd in &params {
                if pd.required {
                    if call.arguments.get(&pd.name).is_none() {
                        errors.push(ValidationError::MissingRequiredParam {
                            tool: call.name.clone(),
                            param: pd.name.clone(),
                        });
                        param_errors = true;
                    }
                }
            }

            if !param_errors {
                valid_calls.push(call.clone());
            }
        }

        ValidationResult { valid_calls, errors }
    }
}

/// 纠正消息构建器
pub struct NudgeBuilder;

impl NudgeBuilder {
    /// 根据校验错误构建纠正提示文本
    pub fn build(errors: &[ValidationError], registry: &ToolRegistry) -> String {
        if errors.is_empty() {
            return String::new();
        }

        let mut lines = vec!["⚠️ 工具调用校验失败，请修正后重试：".to_string()];

        for err in errors {
            match err {
                ValidationError::UnknownTool(name) => {
                    let available: Vec<String> = registry.all_names().into_iter().take(20).collect();
                    let avail_str = if available.len() < registry.all_names().len() {
                        format!("{}...", available.join(", "))
                    } else {
                        available.join(", ")
                    };
                    lines.push(format!(
                        "  ❌ 工具 '{name}' 不存在。可用工具: {avail_str}"
                    ));
                }
                ValidationError::MissingRequiredParam { tool, param } => {
                    lines.push(format!(
                        "  ❌ 工具 '{tool}' 缺少必填参数 '{param}'"
                    ));
                }
            }
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolRegistry, ReadFile, WriteFile};

    fn setup_validator() -> (ResponseValidator, Arc<ToolRegistry>) {
        let registry = Arc::new(ToolRegistry::new()
            .register(ReadFile)
            .register(WriteFile));
        let validator = ResponseValidator::new(registry.clone());
        (validator, registry)
    }

    #[test]
    fn test_valid_tool_call_passes() {
        let (validator, _) = setup_validator();
        let call = RepairedToolCall {
            id: "test1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "test.txt"}),
        };
        let result = validator.validate(&[call]);
        assert!(result.is_ok());
        assert_eq!(result.valid_calls.len(), 1);
    }

    #[test]
    fn test_unknown_tool_blocked() {
        let (validator, _) = setup_validator();
        let call = RepairedToolCall {
            id: "test2".into(),
            name: "nonexistent_tool".into(),
            arguments: serde_json::json!({}),
        };
        let result = validator.validate(&[call]);
        assert!(!result.is_ok());
        assert_eq!(result.errors.len(), 1);
        match &result.errors[0] {
            ValidationError::UnknownTool(name) => assert_eq!(name, "nonexistent_tool"),
            _ => panic!("应该是 UnknownTool"),
        }
    }

    #[test]
    fn test_missing_required_param_blocked() {
        let (validator, _) = setup_validator();
        // read_file 需要 path 参数
        let call = RepairedToolCall {
            id: "test3".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}), // 缺少 path
        };
        let result = validator.validate(&[call]);
        assert!(!result.is_ok());
        assert_eq!(result.errors.len(), 1);
        match &result.errors[0] {
            ValidationError::MissingRequiredParam { tool, param } => {
                assert_eq!(tool, "read_file");
                assert_eq!(param, "path");
            }
            _ => panic!("应该是 MissingRequiredParam"),
        }
    }

    #[test]
    fn test_nudge_builder_unknown_tool() {
        let (_, registry) = setup_validator();
        let errors = vec![ValidationError::UnknownTool("bad_tool".into())];
        let nudge = NudgeBuilder::build(&errors, &registry);
        assert!(nudge.contains("校验失败"));
        assert!(nudge.contains("bad_tool"));
        assert!(nudge.contains("可用工具"));
    }

    #[test]
    fn test_nudge_builder_missing_param() {
        let (_, registry) = setup_validator();
        let errors = vec![ValidationError::MissingRequiredParam {
            tool: "write_file".into(),
            param: "path".into(),
        }];
        let nudge = NudgeBuilder::build(&errors, &registry);
        assert!(nudge.contains("write_file"));
        assert!(nudge.contains("path"));
    }

    #[test]
    fn test_nudge_builder_empty() {
        let (_, registry) = setup_validator();
        let nudge = NudgeBuilder::build(&[], &registry);
        assert!(nudge.is_empty());
    }
}
