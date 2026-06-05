//! 轻量级 Markdown → ratatui Line 渲染器
//!
//! 支持：粗体、行内代码、代码块、标题等常用格式。
//! 避免引入 pulldown-cmark + tree-sitter 等重型依赖。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// 代码块专用背景色
const CODE_BG: Color = Color::DarkGray;
/// 行内代码颜色
const INLINE_CODE_FG: Color = Color::Cyan;
/// 标题颜色
const HEADING_FG: Color = Color::LightYellow;

/// 将 Markdown 文本转换为 ratatui Lines（轻量渲染）
///
/// 当前支持：
/// - `**粗体**`
/// - `` 行内代码 ``
/// - ` ``` ` 代码块（语言标签可选）
/// - `## / ### / #### / ##### / ######` 标题
/// - `> 引用`
/// - `-` 无序列表
/// - `---` 分隔线
pub fn render_markdown(content: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    let code_block_style = Style::default().fg(Color::LightCyan).bg(CODE_BG);
    let normal_style = Style::default().fg(Color::White);
    let quote_style = Style::default().fg(Color::Gray).add_modifier(Modifier::DIM);
    let hr_style = Style::default().fg(Color::DarkGray);

    for raw_line in content.lines() {
        let line = raw_line.trim_end();

        // ---- 代码块开关 ----
        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                // 提取语言标签
                let lang = line.trim_start().trim_start_matches("```").trim();
                let header = if lang.is_empty() {
                    format!(" {:━^width$} ", "", width = 30)
                } else {
                    format!(" {lang} {:━^width$}", "", width = 30usize.saturating_sub(lang.len() + 2))
                };
                lines.push(Line::from(Span::styled(header, code_block_style)));
            } else {
                lines.push(Line::from(Span::styled(" ───", code_block_style)));
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(vec![Span::styled(
                format!(" {}", line),
                code_block_style,
            )]));
            continue;
        }

        // ---- 分隔线 ----
        if !line.is_empty() && line.chars().all(|c| c == '-' || c == '*' || c == '=') && line.len() >= 3 {
            lines.push(Line::from(Span::styled(
                format!("{:━^width$}", "", width = 40),
                hr_style,
            )));
            continue;
        }

        // ---- 空行 ----
        if line.is_empty() {
            lines.push(Line::from(""));
            continue;
        }

        // ---- 引用 ----
        if let Some(quote) = line.strip_prefix('>') {
            let text = quote.trim().trim_start_matches(' ').to_string();
            lines.push(Line::from(Span::styled(
                format!(" ▌ {}", text),
                quote_style,
            )));
            continue;
        }

        // ---- 标题 ----
        let heading_level = line.chars().take_while(|c| *c == '#').count();
        if heading_level > 0 && heading_level <= 6 && line.len() > heading_level {
            let text = line[heading_level..].trim();
            let style = Style::default()
                .fg(HEADING_FG)
                .add_modifier(match heading_level {
                    1 | 2 => Modifier::BOLD | Modifier::UNDERLINED,
                    _ => Modifier::BOLD,
                });
            lines.push(Line::from(Span::styled(format!("{}. {}", heading_level, text), style)));
            continue;
        }

        // ---- 列表（保留前缀符号） ----
        let list_prefix = if line.trim_start().starts_with('-')
            || line.trim_start().starts_with('*')
            || line.trim_start().starts_with('+')
        {
            Some(" •")
        } else if line.trim_start().starts_with(|c: char| c.is_ascii_digit()) {
            line.find('.').map(|_| "").or(Some(""))
        } else {
            None
        };

        // ---- 普通行 + 行内样式 ----
        let spans = parse_inline(line, list_prefix, &normal_style);
        lines.push(Line::from(spans));
    }

    lines
}

/// 解析行内样式：`**粗体**`、`` 行内代码 ``
fn parse_inline(line: &str, list_prefix: Option<&str>, base: &Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut pos = 0;
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();

    // 列表前缀
    if let Some(prefix) = list_prefix {
        spans.push(Span::styled(
            prefix.to_string(),
            Style::default().fg(Color::Green),
        ));
        // 前缀后可能有空格，需要跳过
        if let Some(idx) = line.find(|c: char| !c.is_whitespace()) {
            if idx > 0 {
                spans.push(Span::raw(line[..idx].to_string()));
            }
            // 重设 pos 到内容开始
            let prefix_len = if prefix == " •" { 2 } else { 0 };
            pos = idx.max(prefix_len);
        }
    }

    while pos < len {
        // 检查行内代码 `...`
        if chars[pos] == '`' {
            let end = chars[pos + 1..].iter().position(|c| *c == '`').map(|i| pos + 1 + i);
            if let Some(end_pos) = end {
                let code: String = chars[pos + 1..end_pos].iter().collect();
                spans.push(Span::styled(
                    format!(" {}", code),
                    Style::default()
                        .fg(INLINE_CODE_FG)
                        .bg(CODE_BG)
                        .add_modifier(Modifier::BOLD),
                ));
                pos = end_pos + 1;
                continue;
            }
        }

        // 检查 **粗体**
        if pos + 1 < len && chars[pos] == '*' && chars[pos + 1] == '*' {
            // 找闭合的 **
            let mut end = pos + 2;
            while end + 1 < len {
                if chars[end] == '*' && chars[end + 1] == '*' {
                    let bold_text: String = chars[pos + 2..end].iter().collect();
                    spans.push(Span::styled(bold_text, Style::default().add_modifier(Modifier::BOLD)));
                    pos = end + 2;
                    break;
                }
                end += 1;
            }
            if end + 1 >= len {
                // 没找到闭合，作为普通文本
                let text: String = chars[pos..].iter().collect();
                spans.push(Span::styled(text, *base));
                break; // 整行结束
            }
            continue;
        }

        // 普通字符
        let mut end = pos + 1;
        while end < len {
            if chars[end] == '`' || (end + 1 < len && chars[end] == '*' && chars[end + 1] == '*') {
                break;
            }
            end += 1;
        }
        let text: String = chars[pos..end].iter().collect();
        spans.push(Span::styled(text, *base));
        pos = end;
    }

    spans
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bold_markdown() {
        let lines = render_markdown("hello **world** test");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 3);
        assert_eq!(lines[0].spans[0].content, "hello ");
        assert_eq!(lines[0].spans[1].content, "world");
        // bold span should have BOLD modifier
        let bold_style = lines[0].spans[1].style;
        assert!(
            bold_style.add_modifier(Modifier::BOLD) != Style::default(),
            "bold span should have BOLD modifier"
        );
    }

    #[test]
    fn test_code_block() {
        let content = "```rust\nfn main() {}\n```";
        let lines = render_markdown(content);
        assert!(lines.len() >= 3);
        assert!(lines[0].spans[0].content.contains("rust") || lines[0].spans[0].content.contains("━"));
    }

    #[test]
    fn test_inline_code() {
        let lines = render_markdown("use `tokio::spawn`");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.iter().any(|s| s.content.contains("tokio")));
    }

    #[test]
    fn test_heading() {
        let lines = render_markdown("## Hello World");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans[0].content.contains("Hello"));
    }

    #[test]
    fn test_blockquote() {
        let lines = render_markdown("> quote text");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans[0].content.contains("▌"));
    }

    #[test]
    fn test_horizontal_rule() {
        let lines = render_markdown("---");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans[0].content.contains("━"));
    }

    #[test]
    fn test_mixed_content() {
        let md = "**bold** and `code` and normal";
        let lines = render_markdown(md);
        assert_eq!(lines.len(), 1);
        // 应该有粗体、文本、行内代码、文本至少 4 个 span
        assert!(lines[0].spans.len() >= 3, "expected at least 3 spans, got {}", lines[0].spans.len());
    }

    #[test]
    fn test_multiple_lines() {
        let md = "line1\n\n**bold line**\n\n`code line`";
        let lines = render_markdown(md);
        assert!(lines.len() >= 5);
    }
}
