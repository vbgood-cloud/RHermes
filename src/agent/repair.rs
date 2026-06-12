//! RHermes Tool-Call Repair Pipeline
//!
//! 四道工序修复 DeepSeek 模型在 tool-call 上的常见问题：
#![allow(dead_code)]
//!
//! 1. **Flatten** — 参数嵌套过深时转 dot-notation，dispatch 时还原
//! 2. **Scavenge** — 从 reasoning_content 捞取模型忘记发出的 tool-call
//! 3. **Truncation** — 检测并补全截断的 JSON
//! 4. **Storm** — 抑制相同 (tool, args) 的重复调用


use std::time::{Duration, Instant};

use serde_json::Value;

// ---------------------------------------------------------------------------
// 修复后的结果
// ---------------------------------------------------------------------------

/// 经过 Repair Pipeline 处理后的结果
#[derive(Debug, Clone)]
pub struct RepairedResponse {
    /// 修复后的内容文本
    pub content: Option<String>,
    /// 从中提取的 tool_calls
    pub tool_calls: Vec<RepairedToolCall>,
    /// 是否注入了反思指令
    pub injected_reflection: bool,
    /// 各阶段的修复动作
    pub actions: Vec<RepairAction>,
}

#[derive(Debug, Clone)]
pub struct RepairedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RepairAction {
    Flattened,
    RestoredNested,
    Scavenged(String),
    TruncationFixed,
    StormSuppressed(String),
}

// ---------------------------------------------------------------------------
// 1. Flatten — 参数压平/还原
// ---------------------------------------------------------------------------

/// 参数压平器
///
/// 模型在处理深层嵌套参数时容易丢字段。
/// 发送给模型时压平：`{"a.b": 1}`，收到后还原：`{"a": {"b": 1}}`
pub struct FlattenRepair;

impl FlattenRepair {
    /// 压平嵌套参数（发送给模型前使用）
    pub fn flatten(args: &Value) -> Value {
        let mut flat = serde_json::Map::new();
        Self::flatten_recursive(args, "", &mut flat);
        Value::Object(flat)
    }

    fn flatten_recursive(
        value: &Value,
        prefix: &str,
        result: &mut serde_json::Map<String, Value>,
    ) {
        match value {
            Value::Object(map) => {
                for (key, val) in map {
                    let new_key = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    Self::flatten_recursive(val, &new_key, result);
                }
            }
            _ => {
                result.insert(prefix.to_string(), value.clone());
            }
        }
    }

    /// 还原压平的参数（收到模型响应后使用）
    pub fn unflatten(flat: &Value) -> Value {
        match flat {
            Value::Object(map) => {
                let mut result = serde_json::Map::new();
                for (key, val) in map {
                    let parts: Vec<&str> = key.split('.').collect();
                    Self::set_nested(&mut result, &parts, val.clone());
                }
                Value::Object(result)
            }
            other => other.clone(),
        }
    }

    fn set_nested(map: &mut serde_json::Map<String, Value>, parts: &[&str], value: Value) {
        if parts.len() == 1 {
            map.insert(parts[0].to_string(), value);
        } else {
            let entry = map
                .entry(parts[0].to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Value::Object(inner) = entry {
                Self::set_nested(inner, &parts[1..], value);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Scavenge — 从 reasoning 捞取 tool-call
// ---------------------------------------------------------------------------

/// 工具调用回收器
///
/// DeepSeek 模型有时会在 `reasoning_content` 或 `<think>` 块中生成 tool-call JSON，
/// 但忘记在最终的 `content` 或 `tool_calls` 字段中发出。
/// Scavenge 从这些区域中正则匹配并提取。
pub struct ScavengeRepair;

impl ScavengeRepair {
    /// 从文本中扫描丢失的 tool-call JSON
    pub fn scavenge(text: &str) -> Vec<RepairedToolCall> {
        let mut found = Vec::new();

        // 模式 1: 匹配 <tool_call>{...}</tool_call> 块
        let mut pos = 0;
        let tag = "<tool_call>";
        let end_tag = "</tool_call>";
        while let Some(start) = text[pos..].find(tag) {
            let abs_start = pos + start + tag.len();
            if let Some(end) = text[abs_start..].find(end_tag) {
                let json_str = &text[abs_start..abs_start + end];
                if let Ok(val) = serde_json::from_str::<Value>(json_str.trim()) {
                    if let Some(call) = Self::parse_tool_call(&val) {
                        found.push(call);
                    }
                }
                pos = abs_start + end + end_tag.len();
            } else {
                break;
            }
        }

        // 模式 2: 匹配 ```json{...}``` 或 ```json\n{...}\n```
        pos = 0;
        let json_marker = "```json";
        let code_end = "```";
        while let Some(start) = text[pos..].find(json_marker) {
            let abs_start = pos + start + json_marker.len();
            if let Some(end) = text[abs_start..].find(code_end) {
                let json_str = &text[abs_start..abs_start + end];
                if let Ok(val) = serde_json::from_str::<Value>(json_str.trim()) {
                    if let Some(call) = Self::parse_tool_call(&val) {
                        found.push(call);
                    }
                }
                pos = abs_start + end + code_end.len();
            } else {
                break;
            }
        }

        // 模式 3: 在 <think> 块中搜索 JSON
        if let Some(think_content) = Self::extract_think_block(text) {
            Self::scan_json_objects(&think_content, &mut found);
        }

        found
    }

    /// 在文本中扫描所有 JSON 对象
    fn scan_json_objects(text: &str, found: &mut Vec<RepairedToolCall>) {
        let mut pos = 0;
        while let Some(start) = text[pos..].find('{') {
            let abs_start = pos + start;
            let remaining = &text[abs_start..];
            // 尝试解析 JSON
            match serde_json::from_str::<Value>(remaining) {
                Ok(val) => {
                    if let Some(call) = Self::parse_tool_call(&val) {
                        found.push(call);
                    }
                    // 跳过已解析的 JSON
                    let json_str = serde_json::to_string(&val).unwrap_or_default();
                    pos = abs_start + json_str.len();
                }
                Err(_) => {
                    pos = abs_start + 1;
                }
            }
        }
    }

    fn parse_tool_call(val: &Value) -> Option<RepairedToolCall> {
        let name = val.get("name")?.as_str()?;
        let args = val.get("arguments")?;

        let args_value = if args.is_string() {
            // arguments 可能是 JSON 字符串
            let args_str = args.as_str()?;
            serde_json::from_str(args_str).unwrap_or(Value::Object(serde_json::Map::new()))
        } else {
            args.clone()
        };

        Some(RepairedToolCall {
            id: format!("scavenged_{}", name),
            name: name.to_string(),
            arguments: args_value,
        })
    }

    fn extract_think_block(text: &str) -> Option<String> {
        // <think>...</think> 或 思考 块
        if let Some(start) = text.find("<think>") {
            let content = &text[start + 7..];
            if let Some(end) = content.find("</think>") {
                return Some(content[..end].to_string());
            }
        }
        // 也检查 reasoning_content 区域
        None
    }
}

// ---------------------------------------------------------------------------
// 3. Truncation — JSON 截断补全
// ---------------------------------------------------------------------------

/// JSON 截断修复器
///
/// 当模型达到 max_tokens 时，JSON 可能在中间被截断。
/// 检测不完整的 JSON 并尝试补全。
pub struct TruncationRepair;

impl TruncationRepair {
    /// 检测并修复截断的 JSON
    pub fn repair(text: &str) -> String {
        if !Self::is_truncated(text) {
            return text.to_string();
        }

        // 找到最后一个不完整的 JSON 结构
        let mut result = text.to_string();

        // 尝试修复
        result = Self::close_braces(&result);
        result = Self::close_brackets(&result);
        result = Self::close_quotes(&result);

        result
    }

    /// 检测是否被截断
    fn is_truncated(text: &str) -> bool {
        let opens_braces = text.matches('{').count();
        let closes_braces = text.matches('}').count();
        let opens_brackets = text.matches('[').count();
        let closes_brackets = text.matches(']').count();

        opens_braces != closes_braces || opens_brackets != closes_brackets
    }

    fn close_braces(text: &str) -> String {
        let opens = text.matches('{').count();
        let closes = text.matches('}').count();
        if opens > closes {
            text.to_string() + &"}".repeat(opens - closes)
        } else {
            text.to_string()
        }
    }

    fn close_brackets(text: &str) -> String {
        let opens = text.matches('[').count();
        let closes = text.matches(']').count();
        if opens > closes {
            text.to_string() + &"]".repeat(opens - closes)
        } else {
            text.to_string()
        }
    }

    fn close_quotes(text: &str) -> String {
        // 如果引号未闭合，直接追加
        let mut result = text.to_string();
        let in_string = Self::in_open_string(&result);
        if in_string {
            result.push('"');
        }
        result
    }

    fn in_open_string(text: &str) -> bool {
        let mut in_str = false;
        let mut escaped = false;
        for ch in text.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_str = !in_str;
            }
        }
        in_str
    }
}

// ---------------------------------------------------------------------------
// 4. Storm Suppression — 重复调用抑制
// ---------------------------------------------------------------------------

type ArgsSignature = String;

/// 风暴抑制器
///
/// 滑动窗口检测相同的 (tool_name, args_signature) 组合。
/// 如果在窗口内重复，抑制该调用并注入反思轮次。
pub struct StormSuppression {
    /// 历史调用记录 (tool_name, args_signature, timestamp)
    history: Vec<(String, ArgsSignature, Instant)>,
    /// 窗口大小（秒）
    window_secs: u64,
    /// 最大重复次数
    max_repeats: u32,
    /// 被抑制的总次数
    suppressed_count: u64,
}

impl Default for StormSuppression {
    fn default() -> Self {
        Self {
            history: Vec::new(),
            window_secs: 5,
            max_repeats: 1,
            suppressed_count: 0,
        }
    }
}

impl StormSuppression {
    pub fn new(window_secs: u64, max_repeats: u32) -> Self {
        Self {
            history: Vec::new(),
            window_secs,
            max_repeats,
            suppressed_count: 0,
        }
    }

    /// 检查并过滤 tool-call，返回 (允许通过的调用, 是否发生了抑制)
    pub fn filter(&mut self, calls: Vec<RepairedToolCall>) -> (Vec<RepairedToolCall>, bool) {
        let now = Instant::now();
        let mut suppressed = false;

        // 清理过期记录
        self.history
            .retain(|(_, _, time)| now.duration_since(*time) < Duration::from_secs(self.window_secs));

        let mut allowed = Vec::new();

        for call in calls {
            let sig = Self::args_signature(&call.arguments);
            let repeat_count = self
                .history
                .iter()
                .filter(|(n, s, _)| n == &call.name && s == &sig)
                .count() as u32;

            if repeat_count >= self.max_repeats {
                // 抑制
                self.suppressed_count += 1;
                suppressed = true;
            } else {
                // 记录并放行
                self.history.push((call.name.clone(), sig, now));
                allowed.push(call);
            }
        }

        (allowed, suppressed)
    }

    /// 被抑制的总次数
    pub fn suppressed_count(&self) -> u64 {
        self.suppressed_count
    }

    fn args_signature(args: &Value) -> String {
        // 用 JSON 的规范字符串表示作为签名
        serde_json::to_string(args).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// RepairPipeline — 四道工序串联
// ---------------------------------------------------------------------------

/// Tool-Call Repair Pipeline
///
/// 每次模型响应后调用 `repair()`，按顺序执行四道工序。
pub struct RepairPipeline {
    storm: StormSuppression,
}

impl Default for RepairPipeline {
    fn default() -> Self {
        Self {
            storm: StormSuppression::default(),
        }
    }
}

impl RepairPipeline {
    /// 创建一个新的 RepairPipeline
    pub fn new(window_secs: u64, max_repeats: u32) -> Self {
        Self {
            storm: StormSuppression::new(window_secs, max_repeats),
        }
    }

    /// 执行完整的修复流程
    pub fn repair(&mut self, content: &str) -> RepairedResponse {
        let mut actions = Vec::new();

        // ---- 工序 1: Flatten — 参数还原 ----
        // Flatten 在发送给模型前做压平，收到响应后还原
        // （参数还原在 dispatcher 中处理）

        // ---- 工序 2: Scavenge — 从 reasoning 捞取 ----
        let scavenged = ScavengeRepair::scavenge(content);
        for call in &scavenged {
            actions.push(RepairAction::Scavenged(call.name.clone()));
        }

        // ---- 工序 3: Truncation — 补全截断 ----
        let repaired_content = TruncationRepair::repair(content);
        if repaired_content != content {
            actions.push(RepairAction::TruncationFixed);
        }

        // ---- 工序 4: Storm — 抑制重复 ----
        let (filtered_calls, stormed) = self.storm.filter(scavenged);
        let injected_reflection = stormed;

        RepairedResponse {
            content: Some(repaired_content),
            tool_calls: filtered_calls,
            injected_reflection,
            actions,
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Flatten ----

    #[test]
    fn test_flatten_simple() {
        let args = serde_json::json!({"path": "src/main.rs"});
        let flat = FlattenRepair::flatten(&args);
        assert_eq!(flat, args); // 无嵌套时不变
    }

    #[test]
    fn test_flatten_nested() {
        let args = serde_json::json!({
            "file": {
                "path": "src/main.rs",
                "range": "10-20"
            }
        });
        let flat = FlattenRepair::flatten(&args);
        assert_eq!(flat["file.path"], "src/main.rs");
        assert_eq!(flat["file.range"], "10-20");
    }

    #[test]
    fn test_unflatten() {
        let flat = serde_json::json!({
            "file.path": "src/main.rs",
            "file.range": "10-20"
        });
        let unflat = FlattenRepair::unflatten(&flat);
        assert_eq!(unflat["file"]["path"], "src/main.rs");
        assert_eq!(unflat["file"]["range"], "10-20");
    }

    #[test]
    fn test_flatten_roundtrip() {
        let original = serde_json::json!({
            "level1": {
                "level2": {
                    "value": 42
                }
            }
        });
        let flat = FlattenRepair::flatten(&original);
        let unflat = FlattenRepair::unflatten(&flat);
        assert_eq!(unflat, original);
    }

    // ---- Scavenge ----

    #[test]
    fn test_scavenge_tool_call_tag() {
        let text = r#"我先思考一下。<tool_call>{"name": "read_file", "arguments": {"path": "test.txt"}}</tool_call>然后继续。"#;
        let calls = ScavengeRepair::scavenge(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
    }

    #[test]
    fn test_scavenge_json_block() {
        let text = r#"让我用 JSON 格式输出：
```json
{"name": "search_content", "arguments": {"pattern": "foo"}}
```"#;
        let calls = ScavengeRepair::scavenge(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search_content");
    }

    #[test]
    fn test_scavenge_no_false_positive() {
        let text = "这是一个普通回复，没有工具调用。";
        let calls = ScavengeRepair::scavenge(text);
        assert!(calls.is_empty());
    }

    // ---- Truncation ----

    #[test]
    fn test_truncation_no_truncation() {
        let text = r#"{"name": "read_file", "arguments": {"path": "test.txt"}}"#;
        let repaired = TruncationRepair::repair(text);
        assert_eq!(repaired, text);
    }

    #[test]
    fn test_truncation_missing_brace() {
        let text = r#"{"name": "read_file", "arguments": {"path": "test.txt""#;
        let repaired = TruncationRepair::repair(text);
        assert!(repaired.ends_with('}'));
        // 应该补全为合法的 JSON
        assert!(serde_json::from_str::<Value>(&repaired).is_ok());
    }

    #[test]
    fn test_truncation_is_truncated() {
        assert!(TruncationRepair::is_truncated("{{}"));
        assert!(TruncationRepair::is_truncated("{[}"));
        assert!(!TruncationRepair::is_truncated("{}"));
        assert!(!TruncationRepair::is_truncated("normal text"));
    }

    // ---- Storm Suppression ----

    #[test]
    fn test_storm_allows_first_call() {
        let mut storm = StormSuppression::default();
        let calls = vec![RepairedToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "test.txt"}),
        }];
        let (allowed, suppressed) = storm.filter(calls);
        assert_eq!(allowed.len(), 1);
        assert!(!suppressed);
    }

    #[test]
    fn test_storm_suppresses_duplicate() {
        let mut storm = StormSuppression::new(60, 1);
        let call = RepairedToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "test.txt"}),
        };

        let (allowed, _) = storm.filter(vec![call.clone()]);
        assert_eq!(allowed.len(), 1);

        let (allowed2, suppressed) = storm.filter(vec![call]);
        assert_eq!(allowed2.len(), 0);
        assert!(suppressed);
    }

    #[test]
    fn test_storm_allows_different_args() {
        let mut storm = StormSuppression::new(60, 1);
        let call1 = RepairedToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "a.txt"}),
        };
        let call2 = RepairedToolCall {
            id: "call_2".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "b.txt"}),
        };

        let (allowed, _) = storm.filter(vec![call1]);
        assert_eq!(allowed.len(), 1);

        let (allowed2, suppressed) = storm.filter(vec![call2]);
        assert_eq!(allowed2.len(), 1); // 不同参数，放行
        assert!(!suppressed);
    }

    #[test]
    fn test_storm_suppressed_count() {
        let mut storm = StormSuppression::new(60, 1);
        let call = RepairedToolCall {
            id: "call_1".into(),
            name: "test".into(),
            arguments: serde_json::json!({"x": 1}),
        };

        storm.filter(vec![call.clone()]);
        storm.filter(vec![call]);
        assert_eq!(storm.suppressed_count(), 1);
    }

    // ---- Pipeline 集成 ----

    #[test]
    fn test_pipeline_scavenge_and_storm() {
        let mut pipeline = RepairPipeline::new(60, 1);
        let text = r#"<tool_call>{"name": "read_file", "arguments": {"path": "a.txt"}}</tool_call>"#;

        let result = pipeline.repair(text);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "read_file");
    }

    #[test]
    fn test_pipeline_deduplicates() {
        let mut pipeline = RepairPipeline::new(60, 1); // 允许 1 次重复，第 2 次开始抑制
        let text = r#"<tool_call>{"name": "read_file", "arguments": {"path": "a.txt"}}</tool_call>"#;

        let r1 = pipeline.repair(text);
        assert_eq!(r1.tool_calls.len(), 1);

        let r2 = pipeline.repair(text);
        assert_eq!(r2.tool_calls.len(), 0); // 被抑制
        assert!(r2.injected_reflection);
    }

    #[test]
    fn test_pipeline_truncation_repair() {
        let mut pipeline = RepairPipeline::new(5, 1);
        // 模拟截断的 JSON
        let text = "一些思考内容 {\"name\": \"read_file\"";
        let result = pipeline.repair(text);
        assert!(result.actions.contains(&RepairAction::TruncationFixed));
    }
}
