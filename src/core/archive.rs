//! 上下文压缩归档 — 将压缩前的完整消息序列保存到 .jsonl 文件
//!
//! 每次触发上下文压缩时，自动将压缩前的消息序列写入按日期分片的
//! `compressions/YYYY-MM-DD.jsonl` 文件，用于调试和分析。

use std::path::Path;
use std::fs::{self, OpenOptions};
use std::io::Write;

use chrono::Local;
use serde::Serialize;

/// 单条压缩归档记录
#[derive(Debug, Serialize)]
struct CompressionRecord {
    /// 时间戳
    timestamp: String,
    /// 会话 ID
    session_id: String,
    /// Agent Loop 轮次
    round: u32,
    /// 压缩前的消息数量
    msg_count: usize,
    /// 压缩前估算 token 数
    before_token_est: usize,
    /// LLM 生成的摘要
    summary: String,
}

/// 将一次压缩事件归档到 .jsonl 文件
///
/// 文件路径: `archive_dir/compressions/YYYY-MM-DD.jsonl`
/// 每行一条 JSON 记录。
pub fn archive_compression(
    archive_dir: &Path,
    session_id: &str,
    round: u32,
    msg_count: usize,
    before_token_est: usize,
    summary: &str,
) {
    let compressions_dir = archive_dir.join("compressions");
    if let Err(e) = fs::create_dir_all(&compressions_dir) {
        tracing::warn!("创建压缩归档目录失败: {e}");
        return;
    }

    let date_str = Local::now().format("%Y-%m-%d").to_string();
    let file_path = compressions_dir.join(format!("{date_str}.jsonl"));

    let record = CompressionRecord {
        timestamp: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        session_id: session_id.to_string(),
        round,
        msg_count,
        before_token_est,
        summary: summary.chars().take(500).collect(),
    };

    let line = match serde_json::to_string(&record) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("序列化压缩记录失败: {e}");
            return;
        }
    };

    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
    {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{line}") {
                tracing::warn!("写入压缩归档失败: {e}");
            }
        }
        Err(e) => {
            tracing::warn!("打开压缩归档文件失败: {e}");
        }
    }
}
