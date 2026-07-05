//! PowerPoint 读取工具
//!
//! 使用 zip + quick-xml 解析 .pptx（本质是 ZIP 容器），
//! 提取每页幻灯片的文本内容。

use std::io::Read;

use async_trait::async_trait;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::Value;

use crate::tools::office::check_workspace;
use crate::tools::{ParamDef, ParamType, Tool, ToolError};

/// 读取 PowerPoint 文件，提取每页文字
pub struct ReadPptx;

#[async_trait]
impl Tool for ReadPptx {
    fn name(&self) -> String {
        "read_pptx".into()
    }
    fn description(&self) -> String {
        "读取 PowerPoint(.pptx) 文件，提取每页幻灯片的文字内容".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "PowerPoint 文件路径(.pptx)"),
            ParamDef::optional("max_slides", ParamType::Integer, "最多返回的幻灯片数（默认 100）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = crate::tools::get_string_arg(&args, "path")?;
        let max_slides = args
            .get("max_slides")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;

        let abs = check_workspace(&path).map_err(ToolError::ExecutionFailed)?;

        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let file = std::fs::File::open(&abs).map_err(|e| format!("打开文件失败: {e}"))?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|e| format!("解析 ZIP 失败: {e}"))?;

            // 收集所有 ppt/slides/slideN.xml 文件
            let mut slide_files: Vec<(usize, Vec<u8>)> = Vec::new();

            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| format!("读取 ZIP 条目失败: {e}"))?;
                let name = entry.name().to_string();

                // 匹配 ppt/slides/slideN.xml
                if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                    // 提取 slide 编号
                    let slide_num = extract_slide_number(&name).unwrap_or(0);
                    let mut buf = Vec::new();
                    entry
                        .read_to_end(&mut buf)
                        .map_err(|e| format!("读取 slide XML 失败: {e}"))?;
                    slide_files.push((slide_num, buf));
                }
            }

            // 按 slide 编号排序
            slide_files.sort_by_key(|(n, _)| *n);

            let total = slide_files.len();
            if total == 0 {
                return Ok(format!("📑 PPTX 文件中没有找到幻灯片"));
            }

            let limited = total.min(max_slides);
            let mut output = format!("📑 PPTX ({total} 页):\n");

            for (i, (num, xml)) in slide_files.iter().take(limited).enumerate() {
                output.push_str(&format!("\n── 第 {} 页 ──\n", i + 1));
                let texts = extract_text_from_slide(xml);
                for t in &texts {
                    output.push_str(t);
                    output.push('\n');
                }
            }

            if total > limited {
                output.push_str(&format!(
                    "\n（截断，共 {total} 页，显示前 {limited} 页）"
                ));
            }

            Ok(output)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("pptx 线程崩溃: {e}")))?
        .map_err(ToolError::ExecutionFailed)?;

        Ok(result)
    }
}

/// 从文件名 ppt/slides/slide5.xml 提取编号 5
fn extract_slide_number(name: &str) -> Option<usize> {
    // name 形如 "ppt/slides/slide1.xml"
    let basename = name.rsplit('/').next()?;
    // basename = "slide1.xml"
    let num_part = basename.strip_prefix("slide")?;
    // num_part = "1.xml"
    let end = num_part.find('.')?;
    num_part[..end].parse().ok()
}

/// 从 slide XML 中提取所有 <a:t>...</a:t> 文本
fn extract_text_from_slide(xml: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut texts = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = e.name();
                if local.as_ref() == b"a:t" {
                    // 读取文本内容
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        let text = t.unescape().unwrap_or_default().to_string();
                        if !text.trim().is_empty() {
                            texts.push(text);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    texts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_slide_number() {
        assert_eq!(extract_slide_number("ppt/slides/slide1.xml"), Some(1));
        assert_eq!(extract_slide_number("ppt/slides/slide10.xml"), Some(10));
        assert_eq!(extract_slide_number("ppt/slides/foo.xml"), None);
    }

    #[test]
    fn test_extract_text_from_slide() {
        let xml = br#"<?xml version="1.0"?>
        <sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
            <a:t>Title</a:t>
            <a:t>Content1</a:t>
            <a:t>Content2</a:t>
        </sld>"#;
        let texts = extract_text_from_slide(xml);
        assert_eq!(texts.len(), 3);
        assert_eq!(texts[0], "Title");
        assert_eq!(texts[1], "Content1");
    }
}
