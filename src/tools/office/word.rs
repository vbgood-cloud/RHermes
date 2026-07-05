//! Word 读写工具
//!
//! - read_docx: 使用 docx-rs 读取 .docx 纯文本
//! - write_docx: 使用 docx-rs 生成 .docx（支持简单 Markdown 语法）

use async_trait::async_trait;
use serde_json::Value;

use crate::tools::office::check_workspace;
use crate::tools::{ParamDef, ParamType, Tool, ToolError};

// ---- read_docx ----

/// 读取 Word 文档，提取纯文本内容
pub struct ReadDocx;

#[async_trait]
impl Tool for ReadDocx {
    fn name(&self) -> String {
        "read_docx".into()
    }
    fn description(&self) -> String {
        "读取 Word(.docx) 文件，返回纯文本内容".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "Word 文件路径(.docx)"),
            ParamDef::optional("max_chars", ParamType::Integer, "最多返回字符数（默认 50000）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = crate::tools::get_string_arg(&args, "path")?;
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(50000) as usize;

        let abs = check_workspace(&path).map_err(ToolError::ExecutionFailed)?;

        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            // read_docx 接受 &[u8]
            let buf = std::fs::read(&abs).map_err(|e| format!("读取文件失败: {e}"))?;
            let docx = docx_rs::read_docx(&buf)
                .map_err(|e| format!("解析 docx 失败: {e}"))?;

            // 遍历文档段落，提取文本
            let mut text = String::new();
            let paragraphs = &docx.document.children;
            let mut para_count = 0usize;

            for child in paragraphs {
                use docx_rs::DocumentChild;
                match child {
                    DocumentChild::Paragraph(p) => {
                        para_count += 1;
                        extract_text_from_paragraph(p, &mut text);
                        text.push('\n');
                    }
                    DocumentChild::Table(t) => {
                        para_count += 1;
                        text.push_str("[表格]\n");
                        for tchild in &t.rows {
                            use docx_rs::TableChild;
                            if let TableChild::TableRow(row) = tchild {
                                for tcell in &row.cells {
                                    use docx_rs::TableRowChild;
                                    if let TableRowChild::TableCell(cell) = tcell {
                                        for content in &cell.children {
                                            use docx_rs::TableCellContent;
                                            if let TableCellContent::Paragraph(p) = content {
                                                extract_text_from_paragraph(p, &mut text);
                                                text.push('\t');
                                            }
                                        }
                                    }
                                }
                                text.push('\n');
                            }
                        }
                    }
                    _ => {}
                }
            }

            let total_chars = text.chars().count();
            let show_chars = total_chars.min(max_chars);
            let preview: String = text.chars().take(show_chars).collect();
            let truncated = if total_chars > show_chars {
                format!("（截断，共 {total_chars} 字符）")
            } else {
                String::new()
            };

            Ok(format!(
                "📄 Word ({para_count} 段落, 显示 {show_chars}/{total_chars} 字符){truncated}:\n{preview}"
            ))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("docx 线程崩溃: {e}")))?
        .map_err(ToolError::ExecutionFailed)?;

        Ok(result)
    }
}

/// 从段落中提取文本（遍历 Run → RunChild::Text）
fn extract_text_from_paragraph(para: &docx_rs::Paragraph, text: &mut String) {
    for child in &para.children {
        use docx_rs::ParagraphChild;
        match child {
            ParagraphChild::Run(run) => {
                for rc in &run.children {
                    use docx_rs::RunChild;
                    if let RunChild::Text(t) = rc {
                        text.push_str(&t.text);
                    }
                }
            }
            ParagraphChild::Hyperlink(h) => {
                for child in &h.children {
                    use docx_rs::ParagraphChild;
                    if let ParagraphChild::Run(run) = child {
                        for rc in &run.children {
                            use docx_rs::RunChild;
                            if let RunChild::Text(t) = rc {
                                text.push_str(&t.text);
                            }
                        }
                    }
                }
            }
            ParagraphChild::Insert(ins) => {
                for ic in &ins.children {
                    use docx_rs::InsertChild;
                    if let InsertChild::Run(run) = ic {
                        for rc in &run.children {
                            use docx_rs::RunChild;
                            if let RunChild::Text(t) = rc {
                                text.push_str(&t.text);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ---- write_docx ----

/// 将文本（支持简单 Markdown）写入 Word 文档
pub struct WriteDocx;

#[async_trait]
impl Tool for WriteDocx {
    fn name(&self) -> String {
        "write_docx".into()
    }
    fn description(&self) -> String {
        "将内容写入 Word(.docx) 文件，支持简单 Markdown 语法（#/##/### 标题、**加粗**、列表）".into()
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "输出文件路径(.docx)"),
            ParamDef::required("content", ParamType::String, "文档内容（支持 Markdown 语法）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = crate::tools::get_string_arg(&args, "path")?;
        let content = crate::tools::get_string_arg(&args, "content")?;

        let abs = check_workspace(&path).map_err(ToolError::ExecutionFailed)?;

        let result = tokio::task::spawn_blocking(move || -> Result<usize, String> {
            use docx_rs::{Docx, Paragraph, Run};

            let mut docx = Docx::new();
            let mut para_count = 0;

            for line in content.lines() {
                let trimmed = line.trim();

                // 跳过空行（添加空段落保持间距）
                if trimmed.is_empty() {
                    docx = docx.add_paragraph(Paragraph::new());
                    para_count += 1;
                    continue;
                }

                let (style, text) = parse_markdown_line(trimmed);

                let paragraph = build_paragraph(&style, &text);
                docx = docx.add_paragraph(paragraph);
                para_count += 1;
            }

            let file = std::fs::File::create(&abs).map_err(|e| format!("创建文件失败: {e}"))?;
            docx.build()
                .pack(file)
                .map_err(|e| format!("生成 docx 失败: {e}"))?;

            Ok(para_count)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("docx 写入线程崩溃: {e}")))?
        .map_err(ToolError::ExecutionFailed)?;

        Ok(format!(
            "✅ 已生成 {path} ({count} 段落)",
            count = result
        ))
    }
}

/// Markdown 行的样式
enum MdStyle {
    Heading1,
    Heading2,
    Heading3,
    ListItem,
    Normal,
}

/// 解析一行 Markdown，返回 (样式, 纯文本)
fn parse_markdown_line(line: &str) -> (MdStyle, String) {
    if line.starts_with("### ") {
        (MdStyle::Heading3, line[4..].to_string())
    } else if line.starts_with("## ") {
        (MdStyle::Heading2, line[3..].to_string())
    } else if line.starts_with("# ") {
        (MdStyle::Heading1, line[2..].to_string())
    } else if line.starts_with("- ") || line.starts_with("* ") {
        // 用 ASCII 项目符号避免编码问题
        (MdStyle::ListItem, format!("- {}", &line[2..]))
    } else {
        (MdStyle::Normal, line.to_string())
    }
}

/// 根据样式构建 docx 段落（解析 **加粗**）
fn build_paragraph(style: &MdStyle, text: &str) -> docx_rs::Paragraph {
    use docx_rs::Paragraph;

    let mut para = Paragraph::new();

    // 设置标题样式
    match style {
        MdStyle::Heading1 => para = para.style("Heading1"),
        MdStyle::Heading2 => para = para.style("Heading2"),
        MdStyle::Heading3 => para = para.style("Heading3"),
        _ => {}
    }

    // 解析 **加粗** 段落
    let parts = parse_bold_segments(text);
    for (segment, is_bold) in parts {
        let mut run = docx_rs::Run::new().add_text(segment);
        if is_bold {
            run = run.bold();
        }
        para = para.add_run(run);
    }

    para
}

/// 解析文本中的 **加粗** 标记，返回 (文本, 是否加粗) 的列表
fn parse_bold_segments(text: &str) -> Vec<(String, bool)> {
    let mut result = Vec::new();
    let mut remaining = text;

    while let Some(start) = remaining.find("**") {
        // 前面的普通文本
        if start > 0 {
            result.push((remaining[..start].to_string(), false));
        }
        remaining = &remaining[start + 2..];

        if let Some(end) = remaining.find("**") {
            // 加粗文本
            result.push((remaining[..end].to_string(), true));
            remaining = &remaining[end + 2..];
        } else {
            // 没有匹配的 **，当作普通文本
            result.push((format!("**{remaining}"), false));
            return result;
        }
    }

    if !remaining.is_empty() {
        result.push((remaining.to_string(), false));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::GLOBAL_WORKSPACE;

    /// 确保 GLOBAL_WORKSPACE 已初始化
    fn ensure_workspace() {
        GLOBAL_WORKSPACE.get_or_init(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| ".".to_string())
        });
    }

    #[test]
    fn test_parse_bold() {
        let segments = parse_bold_segments("Hello **world** end");
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0], ("Hello ".into(), false));
        assert_eq!(segments[1], ("world".into(), true));
        assert_eq!(segments[2], (" end".into(), false));
    }

    #[test]
    fn test_parse_bold_no_match() {
        let segments = parse_bold_segments("普通文本");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0], ("普通文本".into(), false));
    }

    #[test]
    fn test_write_read_docx_roundtrip() {
        ensure_workspace();
        let ws = GLOBAL_WORKSPACE.get().unwrap();
        let test_dir = format!("{ws}/target/tmp_office_test");
        std::fs::create_dir_all(&test_dir).unwrap();
        let path_str = format!("{test_dir}/test_docx.docx");

        // 写
        let tool = WriteDocx;
        let args = serde_json::json!({
            "path": &path_str,
            "content": "# 标题\n\n这是**加粗**文本。\n\n## 二级标题\n\n- 项目1\n- 项目2"
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(tool.execute(args));
        assert!(result.is_ok(), "写入失败: {:?}", result);

        // 读
        let read_tool = ReadDocx;
        let args = serde_json::json!({ "path": &path_str });
        let result = rt.block_on(read_tool.execute(args));
        assert!(result.is_ok(), "读取失败: {:?}", result);
        let content = result.unwrap();
        assert!(content.contains("加粗"), "内容缺少加粗: {content}");
        assert!(content.contains("项目1"), "内容缺少项目1: {content}");

        // 清理
        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_dir(&test_dir);
    }
}
