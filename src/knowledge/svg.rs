//! SVG 图谱渲染 — 浏览器主视图
//!
//! 掌握度四档配色（点亮逻辑）：
//!   0     灰   #9ca3af  未学习
//!   1-49  黄   #fbbf24  初识
//!   50-79 浅绿 #4ade80  掌握中
//!   80+   亮绿 #16a34a  精通
//! 内嵌 CSS：hover 放大 + title 摘要 + 进度条头部。

use std::fmt::Write as _;

use super::layout::{canvas_size, compute_positions};
use super::store::{mastery_stage, stage_name, GraphSnapshot, KbStats};

const STAGE_COLORS: [&str; 4] = ["#9ca3af", "#fbbf24", "#4ade80", "#16a34a"];
const STAGE_BG: [&str; 4] = ["#f3f4f6", "#fef9c3", "#dcfce7", "#bbf7d0"];
const NODE_W: f64 = 170.0;
const NODE_H: f64 = 54.0;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 渲染完整 SVG 文档（自带 HTML 包装 + 自动刷新 meta，便于浏览器直接打开）
pub fn render_svg(snapshot: &GraphSnapshot, stats: &KbStats, out_path: &std::path::Path) -> std::io::Result<()> {
    let svg = build_svg(snapshot, stats);
    let html = format!(
        "<!DOCTYPE html>\n<html lang='zh'>\n<head>\n<meta charset='utf-8'>\n<title>{} · 知识图谱</title>\n<style>\nbody{{margin:0;background:#0f172a;font-family:system-ui,'Noto Sans SC',sans-serif}}\nsvg{{display:block;margin:0 auto}}\n</style>\n</head>\n<body>\n{}\n</body>\n</html>\n",
        escape(&snapshot.topic),
        svg
    );
    std::fs::write(out_path, html)
}

pub fn build_svg(snapshot: &GraphSnapshot, stats: &KbStats) -> String {
    let positions = compute_positions(snapshot);
    let (w, h) = canvas_size(snapshot, &positions);
    let mut s = String::with_capacity(16 * 1024);

    // ── defs + 样式 ──
    let _ = writeln!(s, "<svg xmlns='http://www.w3.org/2000/svg' width='{w}' height='{h:.0}' viewBox='0 0 {w} {h:.0}'>");
    let _ = writeln!(s, "<defs><marker id='arrow' markerWidth='10' markerHeight='8' refX='9' refY='4' orient='auto'><path d='M0,0 L10,4 L0,8 z' fill='#64748b'/></marker></defs>");
    let _ = writeln!(s, "<style>
.node rect {{ stroke-width:2; rx:10; cursor:pointer; transition: all .18s; }}
.node:hover rect {{ stroke-width:3; filter: drop-shadow(0 4px 10px rgba(0,0,0,.45)); }}
.node:hover text {{ font-weight: 700; }}
.edge {{ stroke:#475569; stroke-width:1.6; fill:none; marker-end:url(#arrow); opacity:.85; }}
.rel {{ font-size:9px; fill:#94a3b8; }}
.nname {{ font-size:13px; fill:#e2e8f0; text-anchor:middle; dominant-baseline:middle; }}
.nmastery {{ font-size:10px; text-anchor:middle; dominant-baseline:middle; }}
.title {{ font-size:18px; fill:#f8fafc; font-weight:700; }}
.subtitle {{ font-size:11px; fill:#94a3b8; }}
.bar-bg {{ fill:#1e293b; rx:6; }}
.bar-fill {{ rx:6; }}
</style>");

    // ── 背景标题 + 进度条 ──
    let _ = writeln!(s, "<rect width='100%' height='100%' fill='#0f172a'/>");
    let _ = writeln!(s, "<text x='{:.0}' y='30' text-anchor='middle' class='title'>{}</text>",
        w / 2.0, escape(&snapshot.topic));
    let pct = if stats.total_nodes > 0 { stats.avg_mastery } else { 0 };
    let lit_pct = if stats.total_nodes > 0 { stats.lit_nodes * 100 / stats.total_nodes } else { 0 };
    let _ = writeln!(s,
        "<text x='{:.0}' y='48' text-anchor='middle' class='subtitle'>学习进度 {}/{} 节点点亮 ({}%) · 平均掌握度 {}% · 测验 {} 次 · 平均 {}%</text>",
        w / 2.0, stats.lit_nodes, stats.total_nodes, lit_pct, pct, stats.quiz_total, stats.quiz_avg);
    let bar_w = (w - 160.0).min(800.0);
    let bar_x = (w - bar_w) / 2.0;
    let _ = writeln!(s, "<rect x='{:.0}' y='56' width='{:.0}' height='10' class='bar-bg'/>", bar_x, bar_w);
    let _ = writeln!(s, "<rect x='{:.0}' y='56' width='{:.0}' height='10' fill='#22c55e' class='bar-fill'/>",
        bar_x, bar_w * lit_pct as f64 / 100.0);

    // ── 边（先画，压在节点下）──
    for e in &snapshot.edges {
        let (Some((fx, fy)), Some((tx, ty))) = (positions.get(&e.from), positions.get(&e.to)) else { continue };
        // 边锚点：从节点右/上边到目标左/下边（简化为方框边缘中点连线）
        let (x1, y1) = (*fx + NODE_W / 2.0, *fy + NODE_H / 2.0);
        let (x2, y2) = (*tx + NODE_W / 2.0, *ty - 2.0);
        let mx = (x1 + x2) / 2.0;
        let my = (y1 + y2) / 2.0;
        let _ = writeln!(s, "<path class='edge' d='M{x1:.0},{y1:.0} C {x1:.0} {my:.0}, {x2:.0} {my:.0}, {x2:.0} {y2:.0}'/>");
        if !e.relation.is_empty() && e.relation != "相关" {
            let _ = writeln!(s, "<text x='{mx:.0}' y='{my:.0}' class='rel' text-anchor='middle'>{}</text>", escape(&e.relation));
        }
    }

    // ── 节点 ──
    for n in &snapshot.nodes {
        let Some((x, y)) = positions.get(&n.name) else { continue };
        let stage = mastery_stage(n.mastery);
        let color = STAGE_COLORS[stage as usize];
        let bg = STAGE_BG[stage as usize];
        let label = escape(&n.name);
        let summary = escape(&n.summary);
        let stage_txt = stage_name(stage);
        let title = format!("【{}】{}（掌握度 {}% · {}）\n{}", stage_txt, n.name, n.mastery, stage_txt, n.summary);
        let _ = writeln!(s, "<g class='node'>");
        let _ = writeln!(s, "<title>{}</title>", escape(&title));
        let _ = writeln!(s, "<rect x='{x:.0}' y='{y:.0}' width='{NODE_W:.0}' height='{NODE_H:.0}' rx='10' fill='{bg}' stroke='{color}' opacity='0.95'/>");
        // 掌握度进度条（节点内底部）
        let inner_w = NODE_W - 16.0;
        let _ = writeln!(s, "<rect x='{:.0}' y='{:.0}' width='{inner_w:.0}' height='5' rx='2.5' fill='#cbd5e1' opacity='0.55'/>", x + 8.0, y + NODE_H - 11.0);
        let _ = writeln!(s, "<rect x='{:.0}' y='{:.0}' width='{:.0}' height='5' rx='2.5' fill='{color}'/>", x + 8.0, y + NODE_H - 11.0, inner_w * n.mastery as f64 / 100.0);
        // 名称（过长截断到 ~10 个汉字宽）
        let name_disp = truncate_cn(&n.name, 12);
        let _ = writeln!(s, "<text x='{:.0}' y='{:.0}' class='nname'>{}</text>", x + NODE_W / 2.0, y + 20.0, escape(&name_disp));
        let _ = writeln!(s, "<text x='{:.0}' y='{:.0}' class='nmastery' fill='{color}'>{} · {}%</text>",
            x + NODE_W / 2.0, y + 36.0, stage_txt, n.mastery);
        let _ = writeln!(s, "</g>");
    }

    // ── 图例 ──
    let ly = h - 28.0;
    let mut lx = 40.0;
    for (i, c) in STAGE_COLORS.iter().enumerate() {
        let _ = writeln!(s, "<rect x='{lx:.0}' y='{ly:.0}' width='12' height='12' rx='3' fill='{}'/>", c);
        let _ = writeln!(s, "<text x='{:.0}' y='{:.0}' class='subtitle'>{}</text>", lx + 17.0, ly + 10.0, stage_name(i as u8));
        lx += 17.0 + stage_name(i as u8).chars().count() as f64 * 12.0 + 26.0;
    }

    let _ = writeln!(s, "</svg>");
    s
}

/// 中文安全截断（按字符）
fn truncate_cn(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let t: String = s.chars().take(max_chars - 1).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::store::{EdgeRow, NodeRow};

    fn snap() -> GraphSnapshot {
        GraphSnapshot {
            topic: "测试库".into(),
            nodes: vec![
                NodeRow { id: 1, name: "基础概念".into(), summary: "底层知识".into(), layer: 0, mastery: 0, review_count: 0, quiz_count: 0 },
                NodeRow { id: 2, name: "中级主题".into(), summary: "中间层".into(), layer: 1, mastery: 35, review_count: 1, quiz_count: 1 },
                NodeRow { id: 3, name: "高级应用".into(), summary: "顶层".into(), layer: 2, mastery: 90, review_count: 3, quiz_count: 3 },
            ],
            edges: vec![
                EdgeRow { from: "基础概念".into(), to: "中级主题".into(), relation: "依赖".into() },
                EdgeRow { from: "中级主题".into(), to: "高级应用".into(), relation: "包含".into() },
            ],
        }
    }

    fn st() -> KbStats {
        KbStats {
            topic: "测试库".into(),
            total_nodes: 3,
            lit_nodes: 2,
            mastered_nodes: 1,
            avg_mastery: 41,
            quiz_total: 4,
            quiz_avg: 72,
            learn_steps: 5,
            today_steps: 1,
            weakest: vec![("中级主题".into(), 35)],
            quiz_today: 2,
        }
    }

    #[test]
    fn test_svg_contains_all_nodes() {
        let svg = build_svg(&snap(), &st());
        assert!(svg.contains("基础概念"));
        assert!(svg.contains("中级主题"));
        assert!(svg.contains("高级应用"));
        assert!(svg.contains("</svg>"));
        // 四档图例
        assert!(svg.contains("未学习"));
        assert!(svg.contains("精通"));
    }

    #[test]
    fn test_svg_stage_colors() {
        let svg = build_svg(&snap(), &st());
        assert!(svg.contains("#9ca3af")); // 灰
        assert!(svg.contains("#fbbf24")); // 黄
        assert!(svg.contains("#16a34a")); // 亮绿
    }

    #[test]
    fn test_svg_progress_header() {
        let svg = build_svg(&snap(), &st());
        assert!(svg.contains("学习进度 2/3"));
        assert!(svg.contains("平均掌握度 41%"));
    }

    #[test]
    fn test_escape() {
        assert_eq!(escape("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
    }

    #[test]
    fn test_truncate_cn() {
        assert_eq!(truncate_cn("短", 12), "短");
        let long = "这是一个非常非常长的节点名称需要被截断处理";
        let t = truncate_cn(long, 12);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 12);
    }

    #[test]
    fn test_render_svg_file() {
        let mut p = std::env::temp_dir();
        p.push(format!("kb-svg-test-{}.html", std::process::id()));
        render_svg(&snap(), &st(), &p).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.starts_with("<!DOCTYPE html>"));
        assert!(content.contains("<svg"));
        let _ = std::fs::remove_file(&p);
    }
}
