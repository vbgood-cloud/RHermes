//! Office 文档处理工具
//!
//! 支持 Excel(.xlsx)、Word(.docx)、PowerPoint(.pptx) 的读取和写入。
//! - Excel: calamine (读) + rust_xlsxwriter (写)
//! - Word: docx-rs (读写)
//! - PPTX: zip + quick-xml (只读)

mod excel;
mod word;
mod pptx;

pub use excel::{ReadExcel, WriteExcel};
pub use word::{ReadDocx, WriteDocx};
pub use pptx::ReadPptx;

/// 检查路径是否在工作目录边界内
///
/// 复用 builtin.rs 中的 GLOBAL_WORKSPACE 沙箱策略。
/// 返回 Ok(绝对路径) 或 Err(错误消息)。
fn check_workspace(path: &str) -> Result<String, String> {
    let ws_raw = crate::tools::GLOBAL_WORKSPACE
        .get()
        .expect("GLOBAL_WORKSPACE 未初始化")
        .to_string();

    // 将 ws 解析为绝对路径（相对路径拼接 cwd）
    let ws = if std::path::Path::new(&ws_raw).is_absolute() {
        ws_raw.clone()
    } else {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        // ws 相对于 exe 所在目录（可移动模式）
        format!(
            "{}/{}",
            cwd.trim_end_matches('/'),
            ws_raw.trim_start_matches('/')
        )
    };

    let abs = if std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        format!("{}/{}", ws.trim_end_matches('/'), path)
    };

    let normalized = abs.replace('\\', "/").to_lowercase();
    let ws_norm = ws.replace('\\', "/").to_lowercase();

    if !normalized.starts_with(&ws_norm) {
        return Err(format!("⛔ 路径 '{path}' 超出工作目录 '{ws}'"));
    }

    Ok(abs)
}
