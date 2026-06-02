//! RHermes Office 文档工具
//!
//! 读写分离策略：
//! - 读取：Rust 原生（zip + quick-xml + calamine + pdf-extract）
//! - 写入：Python 子进程（python-docx / python-pptx / openpyxl）
//! - 模板填充：Rust 原生（ZIP + XML 替换）

use std::io::{BufReader, Read, Write};
use std::process::Command;

use async_trait::async_trait;
use calamine::Reader;
use serde_json::Value;

use crate::tools::registry::{ParamDef, ParamType, Tool, ToolError, ToolRegistry};

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 从 docx/pptx XML 中提取指定标签的文本
fn extract_xml_text(xml: &str, tag: &str) -> Vec<String> {
    let mut texts = Vec::new();
    let mut in_tag = false;
    let search_open = format!("<{}", tag);
    let search_close = format!("</{}>", tag);
    
    for line in xml.lines() {
        if let Some(pos) = line.find(&search_open) {
            let rest = &line[pos..];
            if let Some(end) = rest.find('>') {
                let content = &rest[end+1..];
                if let Some(close) = content.find(&search_close) {
                    texts.push(content[..close].to_string());
                    in_tag = false;
                    continue;
                }
                in_tag = true;
                if let Some(close) = content.rfind(&search_close) {
                    texts.push(content[..close].to_string());
                    in_tag = false;
                } else if let Some(close) = line.rfind(&search_close) {
                    if let Some(start) = line[pos..].find('>') {
                        let content = &line[pos+start+1..close];
                        texts.push(content.to_string());
                    }
                    in_tag = false;
                }
            }
        } else if in_tag {
            if let Some(close) = line.find(&search_close) {
                texts.push(line[..close].to_string());
                in_tag = false;
            } else {
                texts.push(line.to_string());
            }
        }
    }
    texts
}

/// 写入 Python 脚本并执行
async fn run_python_script(script: &str) -> Result<String, ToolError> {
    let tmp_path = std::env::temp_dir().join(format!("rhermes_{}.py", std::process::id()));
    std::fs::write(&tmp_path, script)
        .map_err(|e| ToolError::ExecutionFailed(format!("写入临时脚本失败: {e}")))?;

    let output = Command::new("python")
        .arg(&tmp_path)
        .output()
        .map_err(|e| ToolError::ExecutionFailed(format!("执行 Python 失败: {e}")))?;

    let _ = std::fs::remove_file(&tmp_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError::ExecutionFailed(format!("Python 错误: {stderr}")));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(stdout.trim().to_string())
}

/// 标准化路径（用于 Python 传参）
fn normalize_path(path: &str) -> String {
    path.replace('\\', "\\\\")
}

// ---------------------------------------------------------------------------
// 读取工具
// ---------------------------------------------------------------------------

/// 读取 .docx 文件纯文本
pub struct ReadDocx;

#[async_trait]
impl Tool for ReadDocx {
    fn name(&self) -> &'static str { "read_docx" }
    fn description(&self) -> &'static str { "读取 Word .docx 文件，返回纯文本内容" }
    fn parallel_safe(&self) -> bool { true }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![ParamDef::required("path", ParamType::String, ".docx 文件路径")]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::MissingParam("path".into()))?;

        let file = std::fs::File::open(path)
            .map_err(|e| ToolError::ExecutionFailed(format!("打开文件失败: {e}")))?;

        let mut archive = zip::ZipArchive::new(BufReader::new(file))
            .map_err(|e| ToolError::ExecutionFailed(format!("解压 ZIP 失败: {e}")))?;

        let mut xml = String::new();
        archive.by_name("word/document.xml")
            .map_err(|_| ToolError::ExecutionFailed("未找到 word/document.xml".into()))?
            .read_to_string(&mut xml)
            .map_err(|e| ToolError::ExecutionFailed(format!("读取 XML 失败: {e}")))?;

        let texts = extract_xml_text(&xml, "w:t");
        // 按段落分组（w:p 之间插空行）
        let mut result = String::new();
        let mut para_count = 0;
        for t in &texts {
            result.push_str(t);
            para_count += 1;
            if para_count % 5 == 0 {
                result.push('\n');
            }
        }

        Ok(format!("📄 {} 个文本段落:\n{}", texts.len(), result))
    }
}

/// 读取 .pptx 文件纯文本
pub struct ReadPptx;

#[async_trait]
impl Tool for ReadPptx {
    fn name(&self) -> &'static str { "read_pptx" }
    fn description(&self) -> &'static str { "读取 PowerPoint .pptx 文件，按幻灯片返回文本" }
    fn parallel_safe(&self) -> bool { true }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![ParamDef::required("path", ParamType::String, ".pptx 文件路径")]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::MissingParam("path".into()))?;

        let file = std::fs::File::open(path)
            .map_err(|e| ToolError::ExecutionFailed(format!("打开文件失败: {e}")))?;

        let mut archive = zip::ZipArchive::new(BufReader::new(file))
            .map_err(|e| ToolError::ExecutionFailed(format!("解压 ZIP 失败: {e}")))?;

        let mut result = String::new();
        let mut slide_num = 1;

        loop {
            let slide_path = format!("ppt/slides/slide{slide_num}.xml");
            let mut entry = match archive.by_name(&slide_path) {
                Ok(e) => e,
                Err(_) => break,
            };
            let mut xml = String::new();
            entry.read_to_string(&mut xml)
                .map_err(|e| ToolError::ExecutionFailed(format!("读取 slide XML 失败: {e}")))?;

            let texts = extract_xml_text(&xml, "a:t");
            if !texts.is_empty() {
                result.push_str(&format!("\n## 幻灯片 {}\n", slide_num));
                for t in &texts {
                    result.push_str(t);
                    result.push('\n');
                }
            }
            slide_num += 1;
        }

        if result.is_empty() {
            Ok("未找到任何幻灯片内容。".into())
        } else {
            Ok(format!("📊 {} 张幻灯片:\n{}", slide_num - 1, result))
        }
    }
}

/// 读取 .xlsx 文件
pub struct ReadXlsx;

#[async_trait]
impl Tool for ReadXlsx {
    fn name(&self) -> &'static str { "read_xlsx" }
    fn description(&self) -> &'static str { "读取 Excel .xlsx 文件，返回表格数据" }
    fn parallel_safe(&self) -> bool { true }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, ".xlsx 文件路径"),
            ParamDef::optional("sheet", ParamType::String, "工作表名称（默认第一个）"),
        ]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::MissingParam("path".into()))?;

        // calamine 打开
        let mut workbook = calamine::open_workbook_auto(path)
            .map_err(|e| ToolError::ExecutionFailed(format!("打开 Excel 失败: {e}")))?;

        let sheet_name = args.get("sheet")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // 确定工作表
        let names = workbook.sheet_names().to_vec();
        let target = sheet_name.as_ref()
            .or_else(|| names.first())
            .ok_or_else(|| ToolError::ExecutionFailed("没有找到任何工作表".into()))?
            .clone();

        let range = workbook.worksheet_range(&target)
            .map_err(|e| ToolError::ExecutionFailed(format!("读取工作表失败: {e}")))?;

        let mut result = format!("📊 工作表「{target}」({}行 × {}列):\n", range.height(), range.width());
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(|c| match c {
                calamine::Data::String(s) => s.clone(),
                calamine::Data::Float(f) => format!("{f}"),
                calamine::Data::Int(i) => format!("{i}"),
                calamine::Data::Bool(b) => format!("{b}"),
                calamine::Data::DateTime(d) => d.to_string(),
                calamine::Data::DateTimeIso(s) => s.clone(),
                calamine::Data::DurationIso(s) => s.clone(),
                calamine::Data::Error(e) => format!("#{e}"),
                _ => String::new(),
            }).collect();
            result.push_str(&cells.join("\t"));
            result.push('\n');
        }

        Ok(result)
    }
}

/// 读取 .pdf 文件
pub struct ReadPdf;

#[async_trait]
impl Tool for ReadPdf {
    fn name(&self) -> &'static str { "read_pdf" }
    fn description(&self) -> &'static str { "读取 PDF 文件，返回纯文本内容" }
    fn parallel_safe(&self) -> bool { true }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![ParamDef::required("path", ParamType::String, "PDF 文件路径")]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::MissingParam("path".into()))?;

        let text = pdf_extract::extract_text(path)
            .map_err(|e| ToolError::ExecutionFailed(format!("读取 PDF 失败: {e}")))?;

        let preview: String = text.chars().take(5000).collect();
        let total_chars = text.chars().count();
        Ok(format!("📄 PDF ({total_chars} 字符，显示前 5000):\n{preview}"))
    }
}

// ---------------------------------------------------------------------------
// 写入工具
// ---------------------------------------------------------------------------

/// 创建 Word 文档
pub struct WriteDocx;

#[async_trait]
impl Tool for WriteDocx {
    fn name(&self) -> &'static str { "write_docx" }
    fn description(&self) -> &'static str { "创建 Word .docx 文件，支持标题/段落/表格" }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "输出 .docx 文件路径"),
            ParamDef::required("sections", ParamType::String, "JSON 数组：[{heading,content,level}]"),
        ]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::MissingParam("path".into()))?;
        let sections_json = args["sections"].as_str()
            .ok_or_else(|| ToolError::MissingParam("sections".into()))?;

        let path_esc = normalize_path(path);
        let script = format!(r#"
import json, sys
from docx import Document
from docx.shared import Pt, Inches

doc = Document()
sections = json.loads("""{sections_json}""")

for sec in sections:
    heading = sec.get("heading", "")
    content = sec.get("content", "")
    level = sec.get("level", 1)
    
    if heading:
        doc.add_heading(heading, level=level)
    if content:
        for para in content.split("\n"):
            if para.strip():
                doc.add_paragraph(para.strip())
    
    table_data = sec.get("table")
    if table_data and len(table_data) > 0:
        table = doc.add_table(rows=len(table_data), cols=len(table_data[0]))
        table.style = "Table Grid"
        for i, row_data in enumerate(table_data):
            for j, cell_val in enumerate(row_data):
                table.cell(i, j).text = str(cell_val)

doc.save(r"{path_esc}")
print("OK")
"#);

        run_python_script(&script).await?;
        Ok(format!("✅ Word 文档已创建: {path}"))
    }
}

/// 创建 PowerPoint 文档
pub struct WritePptx;

#[async_trait]
impl Tool for WritePptx {
    fn name(&self) -> &'static str { "write_pptx" }
    fn description(&self) -> &'static str { "创建 PowerPoint .pptx 文件，支持标题和列表" }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "输出 .pptx 文件路径"),
            ParamDef::required("slides", ParamType::String, "JSON 数组：[{title,bullets,note}]"),
        ]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::MissingParam("path".into()))?;
        let slides_json = args["slides"].as_str()
            .ok_or_else(|| ToolError::MissingParam("slides".into()))?;

        let path_esc = normalize_path(path);
        let script = format!(r#"
import json, sys
from pptx import Presentation
from pptx.util import Inches, Pt

prs = Presentation()
slides_data = json.loads("""{slides_json}""")

for slide_data in slides_data:
    slide = prs.slides.add_slide(prs.slide_layouts[1])  # title + content
    title = slide.shapes.title
    if title and slide_data.get("title"):
        title.text = slide_data["title"]
    
    content = slide.placeholders[1]
    if content:
        bullets = slide_data.get("bullets", [])
        text_frame = content.text_frame
        for i, bullet in enumerate(bullets):
            if i == 0:
                text_frame.text = bullet
            else:
                p = text_frame.add_paragraph()
                p.text = bullet
                p.level = 0
    
    note = slide_data.get("note")
    if note:
        slide.notes_slide.notes_text_frame.text = note

prs.save(r"{path_esc}")
print("OK")
"#);

        run_python_script(&script).await?;
        Ok(format!("✅ PPT 文档已创建: {path}"))
    }
}

/// 创建 Excel 文档
pub struct WriteXlsx;

#[async_trait]
impl Tool for WriteXlsx {
    fn name(&self) -> &'static str { "write_xlsx" }
    fn description(&self) -> &'static str { "创建 Excel .xlsx 文件，支持多 sheet 和数据写入" }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "输出 .xlsx 文件路径"),
            ParamDef::required("sheets", ParamType::String, "JSON 数组：[{name,rows[[cell]]}]"),
        ]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::MissingParam("path".into()))?;
        let sheets_json = args["sheets"].as_str()
            .ok_or_else(|| ToolError::MissingParam("sheets".into()))?;

        let path_esc = normalize_path(path);
        let script = format!(r#"
import json, sys
from openpyxl import Workbook

wb = Workbook()
sheets_data = json.loads("""{sheets_json}""")

for i, sheet_data in enumerate(sheets_data):
    if i == 0:
        ws = wb.active
    else:
        ws = wb.create_sheet()
    
    ws.title = sheet_data.get("name", f"Sheet{{i+1}}")
    rows = sheet_data.get("rows", [])
    for row_idx, row_data in enumerate(rows, 1):
        for col_idx, cell_val in enumerate(row_data, 1):
            ws.cell(row=row_idx, column=col_idx, value=cell_val)

wb.save(r"{path_esc}")
print("OK")
"#);

        run_python_script(&script).await?;
        Ok(format!("✅ Excel 文档已创建: {path}"))
    }
}

// ---------------------------------------------------------------------------
// 模板填充工具
// ---------------------------------------------------------------------------

/// 填充 Word 模板（{{占位符}} 替换）
pub struct FillDocxTemplate;

#[async_trait]
impl Tool for FillDocxTemplate {
    fn name(&self) -> &'static str { "fill_docx_template" }
    fn description(&self) -> &'static str { "填充 Word 模板中的 {{占位符}}，输出新文档" }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("template", ParamType::String, "模板 .docx 文件路径"),
            ParamDef::required("output", ParamType::String, "输出 .docx 文件路径"),
            ParamDef::required("vars", ParamType::String, "JSON 对象：{\"key\": \"value\"}"),
        ]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let template = args["template"].as_str()
            .ok_or_else(|| ToolError::MissingParam("template".into()))?;
        let output = args["output"].as_str()
            .ok_or_else(|| ToolError::MissingParam("output".into()))?;
        let vars_json = args["vars"].as_str()
            .ok_or_else(|| ToolError::MissingParam("vars".into()))?;

        let vars: std::collections::HashMap<String, String> = serde_json::from_str(vars_json)
            .map_err(|e| ToolError::ExecutionFailed(format!("解析 vars JSON 失败: {e}")))?;

        // 打开模板 ZIP
        let file = std::fs::File::open(template)
            .map_err(|e| ToolError::ExecutionFailed(format!("打开模板失败: {e}")))?;
        let mut archive = zip::ZipArchive::new(BufReader::new(file))
            .map_err(|e| ToolError::ExecutionFailed(format!("解压模板失败: {e}")))?;

        // 创建输出 ZIP
        let out_file = std::fs::File::create(output)
            .map_err(|e| ToolError::ExecutionFailed(format!("创建输出文件失败: {e}")))?;
        let mut writer = zip::ZipWriter::new(out_file);

        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)
                .map_err(|e| ToolError::ExecutionFailed(format!("读取 ZIP 条目失败: {e}")))?;
            let name = entry.name().to_string();

            let mut data = Vec::new();
            entry.read_to_end(&mut data)
                .map_err(|e| ToolError::ExecutionFailed(format!("读取条目失败: {e}")))?;

            if name == "word/document.xml" || name == "word/header1.xml" || name == "word/footer1.xml" {
                let mut xml = String::from_utf8(data)
                    .map_err(|_| ToolError::ExecutionFailed("XML 不是合法 UTF-8".into()))?;
                for (key, value) in &vars {
                    xml = xml.replace(&format!("{{{{{}}}}}", key), value);
                }
                writer.start_file(&name, options)
                    .map_err(|e| ToolError::ExecutionFailed(format!("写入 ZIP 失败: {e}")))?;
                writer.write_all(xml.as_bytes())
                    .map_err(|e| ToolError::ExecutionFailed(format!("写入 ZIP 失败: {e}")))?;
            } else {
                writer.start_file(&name, options)
                    .map_err(|e| ToolError::ExecutionFailed(format!("写入 ZIP 失败: {e}")))?;
                writer.write_all(&data)
                    .map_err(|e| ToolError::ExecutionFailed(format!("写入 ZIP 失败: {e}")))?;
            }
        }

        writer.finish()
            .map_err(|e| ToolError::ExecutionFailed(format!("完成 ZIP 失败: {e}")))?;

        Ok(format!("✅ 模板已填充并保存: {output}"))
    }
}

// ---------------------------------------------------------------------------
// 注册辅助
// ---------------------------------------------------------------------------

/// 注册所有 Office 工具到注册表
#[allow(dead_code)]
pub fn register_all(mut registry: ToolRegistry) -> ToolRegistry {
    registry = registry.register(ReadDocx);
    registry = registry.register(ReadPptx);
    registry = registry.register(ReadXlsx);
    registry = registry.register(ReadPdf);
    registry = registry.register(WriteDocx);
    registry = registry.register(WritePptx);
    registry = registry.register(WriteXlsx);
    registry = registry.register(FillDocxTemplate);
    registry
}
