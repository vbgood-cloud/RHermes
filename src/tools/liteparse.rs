//! LiteParse 文档解析工具
//!
//! 封装 LiteParse Rust crate，提供 3 个内置工具：
//! - `parse_document`: 解析文档提取文本/Markdown/JSON
//! - `screenshot_document`: 生成文档页面截图
//! - `check_document_complexity`: 判断文档是否需要 OCR
//!
//! 支持格式：PDF / DOCX / XLSX / PPTX / 图片（PNG/JPEG）

use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::LiteParseSettings;
use crate::tools::{
    get_optional_string, get_string_arg, ParamDef, ParamType, Tool, ToolError,
};

// ---------------------------------------------------------------------------
// 全局 LiteParse 实例（lazy init）
// ---------------------------------------------------------------------------

static GLOBAL_LITEPARSE: OnceLock<Arc<liteparse::LiteParse>> = OnceLock::new();

/// 初始化全局 LiteParse 实例（在 main.rs 启动时调用）
pub fn init_liteparse(settings: &LiteParseSettings) {
    if !settings.enabled {
        tracing::info!("[liteparse] 文档解析工具未启用");
        return;
    }

    let config = liteparse::LiteParseConfig {
        ocr_enabled: settings.ocr_enabled,
        ocr_language: settings.ocr_language.clone(),
        output_format: liteparse::OutputFormat::Markdown,
        dpi: settings.dpi,
        max_pages: settings.max_pages,
        quiet: true,
        tessdata_path: settings.tessdata_path.clone(),
        ocr_server_url: settings.ocr_server_url.clone(),
        ..Default::default()
    };

    let parser = liteparse::LiteParse::new(config);
    let _ = GLOBAL_LITEPARSE.set(Arc::new(parser));
    tracing::info!("[liteparse] 文档解析引擎已就绪");
}

fn get_liteparse() -> Result<Arc<liteparse::LiteParse>, ToolError> {
    GLOBAL_LITEPARSE.get().cloned().ok_or_else(|| {
        ToolError::ExecutionFailed(
            "LiteParse 未初始化。请检查配置 [liteparse] enabled = true，并确保 pdfium 动态库可用。"
                .into(),
        )
    })
}

/// 解析文档为纯文本（供 /learn 大文件管线直接调用，绕过 Tool 层）
/// 返回 markdown 文本；失败返回错误信息
pub async fn parse_document_text(file_path: &str) -> Result<String, String> {
    let parser = get_liteparse().map_err(|e| e.to_string())?;
    let result = parser
        .parse(file_path)
        .await
        .map_err(|e| format!("文档解析失败: {e}"))?;
    Ok(result.text)
}

// ---------------------------------------------------------------------------
// 路径安全检查
// ---------------------------------------------------------------------------

const PROTECTED_FILES: &[&str] = &[
    "config.toml", ".env", "config.yaml", "config.json",
    "credentials.json", "secrets.toml", "secrets.env",
    ".ssh/id_rsa", ".ssh/id_ed25519", ".ssh/authorized_keys",
    "/etc/passwd", "/etc/shadow", "/etc/hosts",
];

fn is_protected_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_lowercase();
    PROTECTED_FILES.iter().any(|p| {
        let p = p.to_lowercase();
        normalized == p || normalized.ends_with(&format!("/{p}"))
    })
}

fn check_file_path(path: &str) -> Result<String, ToolError> {
    if !Path::new(path).exists() {
        return Err(ToolError::ExecutionFailed(format!(
            "文档文件不存在: {path}"
        )));
    }

    let ws = crate::tools::builtin::GLOBAL_WORKSPACE
        .get()
        .map(|s| s.clone())
        .unwrap_or_default();
    if !ws.is_empty() {
        let abs = if Path::new(path).is_absolute() {
            path.to_string()
        } else {
            format!("{}/{}", ws.trim_end_matches('/'), path)
        };
        let normalized = abs.replace('\\', "/").to_lowercase();
        let ws_norm = ws.to_lowercase();
        if !normalized.starts_with(&ws_norm) {
            return Err(ToolError::ExecutionFailed(format!(
                "⛔ 路径 '{path}' 超出工作目录 '{ws}'"
            )));
        }
    }

    if is_protected_path(path) {
        return Err(ToolError::ExecutionFailed(format!(
            "⛔ 路径 '{path}' 是受保护文件"
        )));
    }

    Ok(path.to_string())
}

// ---------------------------------------------------------------------------
// 工具1: ParseDocument
// ---------------------------------------------------------------------------

pub struct ParseDocument;

#[async_trait]
impl Tool for ParseDocument {
    fn name(&self) -> String { "parse_document".into() }
    fn description(&self) -> String {
        "Parse a document (PDF, DOCX, XLSX, PPTX, image) and extract text as markdown, plain text, or structured JSON. Supports OCR for scanned documents.".into()
    }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("file_path", ParamType::String, "文档文件路径（PDF/DOCX/XLSX/PPTX/PNG/JPEG）"),
            ParamDef::optional("format", ParamType::String, "输出格式: markdown / text / json（默认 markdown）"),
            ParamDef::optional("ocr_language", ParamType::String, "OCR 语言（如 eng / chi_sim / chi_sim+eng）"),
            ParamDef::optional("page_range", ParamType::String, "页码范围，如 '1-5,10,15-20'"),
            ParamDef::optional("max_pages", ParamType::Integer, "最大页数"),
            ParamDef::optional("no_ocr", ParamType::Boolean, "跳过 OCR"),
            ParamDef::optional("password", ParamType::String, "加密文档密码"),
        ]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let file_path = get_string_arg(&args, "file_path")?;
        let file_path = check_file_path(&file_path)?;

        let parser = get_liteparse()?;

        let format = get_optional_string(&args, "format").unwrap_or_else(|| "markdown".into());
        let ocr_language = get_optional_string(&args, "ocr_language");
        let page_range = get_optional_string(&args, "page_range");
        let max_pages = args.get("max_pages").and_then(|v| v.as_i64()).map(|v| v as usize);
        let no_ocr = args.get("no_ocr").and_then(|v| v.as_bool()).unwrap_or(false);
        let password = get_optional_string(&args, "password");

        // 如果参数与全局配置不同，创建临时实例
        let use_temp = ocr_language.is_some() || max_pages.is_some() || no_ocr || page_range.is_some();
        let result = if use_temp {
            let mut temp_config = liteparse::LiteParseConfig {
                output_format: liteparse::OutputFormat::Markdown,
                dpi: 150.0,
                max_pages: max_pages.unwrap_or(50),
                quiet: true,
                ..Default::default()
            };
            if let Some(lang) = ocr_language {
                temp_config.ocr_language = lang;
            }
            temp_config.ocr_enabled = !no_ocr;
            if let Some(pr) = page_range {
                temp_config.target_pages = Some(pr);
            }
            if let Some(pwd) = &password {
                temp_config.password = Some(pwd.clone());
            }
            // 根据 format 设置输出格式
            match format.as_str() {
                "text" => temp_config.output_format = liteparse::OutputFormat::Text,
                "json" => temp_config.output_format = liteparse::OutputFormat::Json,
                _ => {}
            }
            let temp_parser = liteparse::LiteParse::new(temp_config);
            temp_parser.parse(&file_path).await
        } else {
            parser.parse(&file_path).await
        };

        match result {
            Ok(parse_result) => {
                let content = parse_result.text;
                Ok(format!("📄 文档解析完成 ({}, {} 页)\n{}", file_path, parse_result.pages.len(), content))
            }
            Err(e) => Err(ToolError::ExecutionFailed(format!("解析失败: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// 工具2: ScreenshotDocument
// ---------------------------------------------------------------------------

pub struct ScreenshotDocument;

#[async_trait]
impl Tool for ScreenshotDocument {
    fn name(&self) -> String { "screenshot_document".into() }
    fn description(&self) -> String {
        "Generate page screenshots of a document (PDF/DOCX/PPTX). Returns image info.".into()
    }
    fn parallel_safe(&self) -> bool { false }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("file_path", ParamType::String, "文档文件路径"),
            ParamDef::optional("pages", ParamType::String, "页码，如 '1,3,5'（默认全部）"),
            ParamDef::optional("dpi", ParamType::Integer, "渲染 DPI（默认 150）"),
        ]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let file_path = get_string_arg(&args, "file_path")?;
        let file_path = check_file_path(&file_path)?;

        let parser = get_liteparse()?;

        let pages_str = get_optional_string(&args, "pages");
        let page_numbers: Option<Vec<u32>> = pages_str.as_ref().map(|s| {
            s.split(',')
                .filter_map(|n| n.trim().parse::<u32>().ok())
                .collect()
        });

        let result = parser.screenshot(&file_path, page_numbers).await;

        match result {
            Ok(screenshots) => {
                let mut lines = Vec::new();
                for shot in &screenshots {
                    lines.push(format!(
                        "  第{}页: {}x{} ({} 字节)",
                        shot.page_num, shot.width, shot.height, shot.image_bytes.len()
                    ));
                }
                Ok(format!("📸 截图完成: {} 页\n{}", screenshots.len(), lines.join("\n")))
            }
            Err(e) => Err(ToolError::ExecutionFailed(format!("截图失败: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// 工具3: CheckDocumentComplexity
// ---------------------------------------------------------------------------

pub struct CheckDocumentComplexity;

#[async_trait]
impl Tool for CheckDocumentComplexity {
    fn name(&self) -> String { "check_document_complexity".into() }
    fn description(&self) -> String {
        "Analyze document complexity to determine if OCR is needed. Returns per-page stats.".into()
    }
    fn parallel_safe(&self) -> bool { true }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("file_path", ParamType::String, "文档文件路径"),
        ]
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let file_path = get_string_arg(&args, "file_path")?;
        let file_path = check_file_path(&file_path)?;

        let parser = get_liteparse()?;

        let input = liteparse::types::PdfInput::Path(file_path.clone());
        let result = parser.is_complex(input).await;

        match result {
            Ok(stats) => {
                let mut lines = vec![format!("📊 文档复杂度分析: {}", file_path)];
                for stat in &stats {
                    lines.push(format!(
                        "  第{}页: text_items={}, needs_ocr={}, layout={}",
                        stat.page_number,
                        stat.text_length,
                        stat.text_coverage,
                        stat.has_substantial_images,
                    ));
                }
                Ok(lines.join("\n"))
            }
            Err(e) => Err(ToolError::ExecutionFailed(format!("复杂度分析失败: {e}"))),
        }
    }
}
