//! ASCII 二维码渲染器
//!
//! 将文本编码为 QR 码，输出可在终端显示的 ASCII 艺术字。

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// 将二维码文本渲染为 ASCII 行（用于 TUI 显示）
///
/// 输出格式：带边框的 ██/空格 QR 码，每行已包装为 `Line`
pub fn render_ascii_qr(text: &str) -> Vec<Line<'static>> {
    // 编码为 QR 码
    let qr = match qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) {
        Ok(qr) => qr,
        Err(_) => {
            return vec![Line::from(Span::styled(
                "⚠ 二维码生成失败",
                Style::default().fg(Color::Red),
            ))];
        }
    };

    let size = qr.size() as usize;
    let module_count = size + 2; // +2 边距

    let dark_style = Style::default().fg(Color::White).bg(Color::Black);
    let light_style = Style::default().fg(Color::Black).bg(Color::White);
    let border_style = Style::default().fg(Color::DarkGray);

    let mut lines = Vec::new();

    // 顶部边框
    let border = "┌".to_string() + &"─".repeat(module_count * 2) + "┐";
    lines.push(Line::from(Span::styled(border, border_style)));

    // 逐行渲染 QR 码
    for y in 0..size {
        let mut line = String::from("│");
        line.push('　'); // 左侧边距（全角空格 = 2 列）
        for x in 0..size {
            let is_dark = qr.get_module(x as i32, y as i32);
            if is_dark {
                line.push('█');
                line.push('█');
            } else {
                line.push(' ');
                line.push(' ');
            }
        }
        line.push('　'); // 右侧边距
        line.push('│');
        lines.push(Line::from(Span::styled(line, dark_style)));
    }

    // 底部边框
    let border = "└".to_string() + &"─".repeat(module_count * 2) + "┘";
    lines.push(Line::from(Span::styled(border, border_style)));

    // 提示文字
    lines.push(Line::from(Span::styled(
        "📱 请用微信扫描上方二维码登录",
        Style::default().fg(Color::Cyan).add_modifier(ratatui::style::Modifier::BOLD),
    )));

    lines
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_ascii_qr_returns_lines() {
        let lines = render_ascii_qr("test-qr-data");
        // 应该有边框 + QR 行 + 提示
        assert!(lines.len() >= 5, "QR 码行数应 ≥ 5, 实际 {}", lines.len());
        // 检查第一个非边框行（跳过提示行从后往前找边框）
        let top_border = lines.iter().find(|l| l.spans[0].content.starts_with('┌'));
        assert!(top_border.is_some(), "应包含顶部边框 ┌");
        let bottom_border = lines.iter().rev().skip(1).find(|l| l.spans[0].content.starts_with('└'));
        assert!(bottom_border.is_some(), "应包含底部边框 └");
    }

    #[test]
    fn test_render_ascii_qr_hint() {
        let lines = render_ascii_qr("hello");
        let last = &lines[lines.len() - 1];
        assert!(last.spans[0].content.contains("📱"), "最后一行应包含提示");
    }
}

