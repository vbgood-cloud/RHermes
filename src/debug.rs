//! RHermes 调试与日志系统
//!
//! ## 功能
//!
//! - **会话追踪**：记录每轮对话的 tool_calls、token 用量、错误
//! - **调试导出**：`rhermes debug export <id>` 生成 JSON 报告
//! - **结构化日志**：JSON 格式日志（可选）
//!
//! ## 用法
//!
//! ```text
//! rhermes debug export <session-id> --output debug-report.json
//! ```

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 调试环缓冲区最大条目数
const MAX_DEBUG_ENTRIES: usize = 500;

// ---------------------------------------------------------------------------
// 调试条目
// ---------------------------------------------------------------------------

/// 调试日志中的单条记录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DebugEntry {
    /// Agent Loop 轮次
    Round {
        round: u32,
        user_msg: String,
        assistant_msg: String,
        duration_ms: u64,
    },
    /// 工具调用
    ToolCall {
        name: String,
        args: String,
        result_preview: String,
        duration_ms: u64,
        success: bool,
    },
    /// Token 使用
    TokenUsage {
        input_tokens: u32,
        output_tokens: u32,
        cache_hit: u32,
        cache_miss: u32,
    },
    /// 错误
    Error {
        source: String,
        message: String,
    },
    /// 上下文压缩
    Compression {
        before_bytes: usize,
        after_bytes: usize,
        summary_len: usize,
    },
}

// ---------------------------------------------------------------------------
// Session Debug
// ---------------------------------------------------------------------------

/// 会话级调试捕获器
pub struct SessionDebug {
    /// 会话唯一 ID
    pub session_id: String,
    /// 调试条目环缓冲区
    entries: Vec<(Instant, DebugEntry)>,
    /// 统计汇总
    pub stats: DebugStats,
    /// 创建时间
    created_at: Instant,
}

/// 调试统计汇总
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DebugStats {
    pub total_rounds: u32,
    pub total_tool_calls: u32,
    pub total_errors: u32,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cache_hit: u32,
    pub total_cache_miss: u32,
    pub total_cost_cny: f64,
    pub total_duration_secs: f64,
    pub compression_count: u32,
}

impl SessionDebug {
    /// 创建新的会话调试器
    pub fn new() -> Self {
        Self {
            session_id: generate_session_id(),
            entries: Vec::with_capacity(128),
            stats: DebugStats::default(),
            created_at: Instant::now(),
        }
    }

    /// 记录轮次
    pub fn record_round(&mut self, round: u32, user_msg: &str, assistant_msg: &str, duration_ms: u64) {
        self.stats.total_rounds += 1;
        self.push(DebugEntry::Round {
            round,
            user_msg: truncate(user_msg, 200),
            assistant_msg: truncate(assistant_msg, 500),
            duration_ms,
        });
    }

    /// 记录工具调用
    pub fn record_tool_call(&mut self, name: &str, args: &str, result_preview: &str, duration_ms: u64, success: bool) {
        self.stats.total_tool_calls += 1;
        self.push(DebugEntry::ToolCall {
            name: name.to_string(),
            args: truncate(args, 200),
            result_preview: truncate(result_preview, 300),
            duration_ms,
            success,
        });
    }

    /// 记录 Token 使用
    pub fn record_token_usage(&mut self, input: u32, output: u32, cache_hit: u32, cache_miss: u32) {
        self.stats.total_input_tokens += input;
        self.stats.total_output_tokens += output;
        self.stats.total_cache_hit += cache_hit;
        self.stats.total_cache_miss += cache_miss;
        self.push(DebugEntry::TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_hit,
            cache_miss,
        });
    }

    /// 记录错误
    pub fn record_error(&mut self, source: &str, message: &str) {
        self.stats.total_errors += 1;
        self.push(DebugEntry::Error {
            source: source.to_string(),
            message: message.to_string(),
        });
    }

    /// 记录压缩
    pub fn record_compression(&mut self, before_bytes: usize, after_bytes: usize, summary_len: usize) {
        self.stats.compression_count += 1;
        self.push(DebugEntry::Compression {
            before_bytes,
            after_bytes,
            summary_len,
        });
    }

    /// 导出调试报告
    pub fn export(&self, output_path: &Path) -> Result<(), DebugError> {
        let elapsed = self.created_at.elapsed();
        let mut stats = self.stats.clone();
        stats.total_duration_secs = elapsed.as_secs_f64();

        let entries_json: Vec<serde_json::Value> = self.entries.iter().map(|(ts, entry)| {
            serde_json::json!({
                "timestamp": format_instant(*ts, self.created_at),
                "entry": entry,
            })
        }).collect();

        let report = serde_json::json!({
            "rhermes_version": env!("CARGO_PKG_VERSION"),
            "session_id": self.session_id,
            "exported_at": Utc::now().to_rfc3339(),
            "duration_secs": elapsed.as_secs_f64(),
            "stats": stats,
            "entries": entries_json,
        });

        let json_str = serde_json::to_string_pretty(&report)
            .map_err(|e| DebugError::Serialize(e.to_string()))?;
        fs::write(output_path, json_str)
            .map_err(|e| DebugError::Io(e.to_string()))?;
        Ok(())
    }

    fn push(&mut self, entry: DebugEntry) {
        if self.entries.len() >= MAX_DEBUG_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push((Instant::now(), entry));
    }
}

/// 生成短会话 ID
fn generate_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{:x}", ms)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...（截断）", &s[..max])
    } else {
        s.to_string()
    }
}

fn format_instant(ts: Instant, since: Instant) -> String {
    let d = match ts.checked_duration_since(since) {
        Some(d) => d,
        None => Duration::from_secs(0),
    };
    format!("+{:.1}s", d.as_secs_f64())
}

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum DebugError {
    Io(String),
    Serialize(String),
}

impl std::fmt::Display for DebugError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误: {e}"),
            Self::Serialize(e) => write!(f, "序列化错误: {e}"),
        }
    }
}

impl std::error::Error for DebugError {}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_non_empty() {
        let d = SessionDebug::new();
        assert!(!d.session_id.is_empty());
    }

    #[test]
    fn test_record_tool_call() {
        let mut d = SessionDebug::new();
        d.record_tool_call("read_file", r#"{"path":"test.txt"}"#, "content: ok", 100, true);
        assert_eq!(d.stats.total_tool_calls, 1);
        assert_eq!(d.entries.len(), 1);
    }

    #[test]
    fn test_record_error() {
        let mut d = SessionDebug::new();
        d.record_error("api", "timeout");
        assert_eq!(d.stats.total_errors, 1);
    }

    #[test]
    fn test_export_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut d = SessionDebug::new();
        d.record_tool_call("test", "{}", "ok", 10, true);
        let path = tmp.path().join("debug.json");
        d.export(&path).unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("test"));
        assert!(content.contains("session_id"));
    }

    #[test]
    fn test_ring_buffer_capped() {
        let mut d = SessionDebug::new();
        for i in 0..MAX_DEBUG_ENTRIES + 10 {
            d.record_tool_call(&format!("t{i}"), "{}", "ok", 1, true);
        }
        assert_eq!(d.entries.len(), MAX_DEBUG_ENTRIES);
        // 最旧的被移除
        let first = match &d.entries.first().unwrap().1 {
            DebugEntry::ToolCall { name, .. } => name.clone(),
            _ => String::new(),
        };
        assert_eq!(first, format!("t{}", 10));
    }

    #[test]
    fn test_stats_accumulation() {
        let mut d = SessionDebug::new();
        d.record_token_usage(100, 50, 30, 70);
        d.record_token_usage(200, 100, 60, 140);
        assert_eq!(d.stats.total_input_tokens, 300);
        assert_eq!(d.stats.total_output_tokens, 150);
        assert_eq!(d.stats.total_cache_hit, 90);
    }
}
