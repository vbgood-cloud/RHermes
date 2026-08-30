//! Bento 面板渲染 — 学习战绩（终端 ANSI 版 + SVG 版）

use std::fmt::Write as _;

use super::store::{mastery_stage, stage_name, KbStats};

/// 终端 Bento（Unicode 框线 + ANSI 色，2×3 格）
pub fn render_terminal(stats: &KbStats) -> String {
    let mut s = String::with_capacity(1024);
    let bar = progress_bar(stats.avg_mastery, 10);
    let lit_pct = if stats.total_nodes > 0 { stats.lit_nodes * 100 / stats.total_nodes } else { 0 };
    let node_bar = format!("{}/{}", stats.lit_nodes, stats.total_nodes);

    // 颜色
    const RESET: &str = "\x1b[0m";
    const BOLD: &str = "\x1b[1m";
    const GREEN: &str = "\x1b[32m";
    const YELLOW: &str = "\x1b[33m";
    const CYAN: &str = "\x1b[36m";
    const GRAY: &str = "\x1b[90m";
    const RED: &str = "\x1b[31m";
    let mastery_color = match stats.avg_mastery {
        80..=100 => GREEN,
        50..=79 => CYAN,
        1..=49 => YELLOW,
        _ => GRAY,
    };

    let weakest = if stats.weakest.is_empty() {
        "— 全部掌握或未开始 —".to_string()
    } else {
        stats
            .weakest
            .iter()
            .map(|(n, m)| format!("{n}({m}%)"))
            .collect::<Vec<_>>()
            .join(" · ")
    };

    let _ = writeln!(s, "{BOLD}╭──────────────────────┬────────────────┬────────────────╮{RESET}");
    let _ = writeln!(s, "│ {CYAN}🎯 总掌握度{RESET}        │ {CYAN}📚 节点{RESET}       │ {CYAN}✅ 测验{RESET}        │");
    let _ = writeln!(s, "│   {BOLD}{mastery_color}{:>3}%{RESET}           │    {BOLD}{:>4}{RESET}       │  {BOLD}{:>3}%{RESET} 正确率    │",
        stats.avg_mastery, node_bar, stats.quiz_avg);
    let _ = writeln!(s, "│   {mastery_color}{bar}{RESET} │  点亮 {lit_pct}%      │  共 {:>3} 次      │", stats.quiz_total);
    let _ = writeln!(s, "{BOLD}├──────────────────────┼────────────────┼────────────────┤{RESET}");
    let _ = writeln!(s, "│ {CYAN}⏱ 学习步数{RESET}        │ {CYAN}📅 今日{RESET}       │ {RED}⚠ 待攻克{RESET}        │");
    let _ = writeln!(s, "│      {:>5}          │   {:>4} 步       │  {YELLOW}{}{RESET}  │",
        stats.learn_steps, stats.today_steps, truncate(&weakest, 14));
    let _ = writeln!(s, "│   累计讲解次数       │  今日测验 {} 次  │  （掌握<80%）    │", stats.quiz_today);
    let _ = writeln!(s, "{BOLD}╰──────────────────────┴────────────────┴────────────────╯{RESET}");
    s
}

/// SVG 版 Bento（CSS Grid 2×3，浏览器查看）
pub fn render_svg_bento(stats: &KbStats, out_path: &std::path::Path) -> std::io::Result<()> {
    let html = build_bento_html(stats);
    std::fs::write(out_path, html)
}

fn build_bento_html(stats: &KbStats) -> String {
    let lit_pct = if stats.total_nodes > 0 { stats.lit_nodes * 100 / stats.total_nodes } else { 0 };
    let weakest = if stats.weakest.is_empty() {
        "— 全部掌握或未开始 —".to_string()
    } else {
        stats.weakest.iter().map(|(n, m)| format!("{n}（{m}%）")).collect::<Vec<_>>().join("、")
    };
    let mastery_color = match stats.avg_mastery {
        80..=100 => "#22c55e",
        50..=79 => "#06b6d4",
        1..=49 => "#eab308",
        _ => "#6b7280",
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="zh">
<head><meta charset="utf-8"><title>{topic} · 学习战绩</title>
<style>
body{{margin:0;background:#0f172a;display:flex;justify-content:center;align-items:center;min-height:100vh;font-family:system-ui,'Noto Sans SC',sans-serif}}
.grid{{display:grid;grid-template-columns:repeat(3,220px);grid-auto-rows:150px;gap:14px;padding:24px}}
.cell{{background:#1e293b;border:1px solid #334155;border-radius:16px;padding:18px;display:flex;flex-direction:column;justify-content:center}}
.label{{color:#94a3b8;font-size:12px;margin-bottom:8px}}
.value{{color:#f8fafc;font-size:34px;font-weight:700}}
.sub{{color:#64748b;font-size:11px;margin-top:6px}}
.bar{{height:8px;background:#334155;border-radius:4px;margin-top:10px;overflow:hidden}}
.bar>i{{display:block;height:100%;border-radius:4px;background:{mastery_color}}}
.weak{{color:#fbbf24;font-size:14px;line-height:1.7}}
h1{{color:#f8fafc;text-align:center;font-size:18px;margin:0 0 4px}}
</style></head>
<body>
<div>
<h1>📚 {topic} · 学习战绩</h1>
<div class="grid">
  <div class="cell"><div class="label">🎯 总掌握度</div><div class="value" style="color:{mastery_color}">{avg}%</div><div class="bar"><i style="width:{avg}%"></i></div></div>
  <div class="cell"><div class="label">📚 节点点亮</div><div class="value">{lit}/{tot}</div><div class="sub">点亮率 {lit_pct}% · 精通 {mastered} 个</div></div>
  <div class="cell"><div class="label">✅ 测验正确率</div><div class="value">{qavg}%</div><div class="sub">累计 {qtotal} 次 · 今日 {qtoday} 次</div></div>
  <div class="cell"><div class="label">⏱ 学习步数</div><div class="value">{steps}</div><div class="sub">每次讲解+验证记 1 步</div></div>
  <div class="cell"><div class="label">📅 今日</div><div class="value">{today} 步</div><div class="sub">保持节奏，持续点亮</div></div>
  <div class="cell"><div class="label">⚠ 待攻克（掌握&lt;80%）</div><div class="weak">{weakest}</div></div>
</div>
</div>
</body></html>
"#,
        topic = escape(&stats.topic),
        avg = stats.avg_mastery,
        mastery_color = mastery_color,
        lit = stats.lit_nodes,
        tot = stats.total_nodes,
        lit_pct = lit_pct,
        mastered = stats.mastered_nodes,
        qavg = stats.quiz_avg,
        qtotal = stats.quiz_total,
        qtoday = stats.quiz_today,
        steps = stats.learn_steps,
        today = stats.today_steps,
        weakest = escape(&weakest),
    )
}

fn progress_bar(pct: i64, width: usize) -> String {
    let filled = (pct as usize * width / 100).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_chars - 1).collect::<String>())
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> KbStats {
        KbStats {
            topic: "Rust异步".into(),
            total_nodes: 32,
            lit_nodes: 24,
            mastered_nodes: 15,
            avg_mastery: 61,
            quiz_total: 42,
            quiz_avg: 91,
            learn_steps: 38,
            today_steps: 5,
            weakest: vec![("Select宏".into(), 22), ("Pin/Unpin".into(), 41)],
            quiz_today: 6,
        }
    }

    #[test]
    fn test_terminal_bento() {
        let out = render_terminal(&st());
        assert!(out.contains("总掌握度"));
        assert!(out.contains("61%"));
        assert!(out.contains("24/32"));
        assert!(out.contains("Select宏"));
        assert!(out.contains("╰"));
    }

    #[test]
    fn test_bento_html() {
        let mut p = std::env::temp_dir();
        p.push(format!("kb-bento-test-{}.html", std::process::id()));
        render_svg_bento(&st(), &p).unwrap();
        let c = std::fs::read_to_string(&p).unwrap();
        assert!(c.contains("学习战绩"));
        assert!(c.contains("grid-template-columns:repeat(3,220px)"));
        assert!(c.contains("Select宏"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_progress_bar() {
        assert_eq!(progress_bar(61, 10), "██████░░░░");
        assert_eq!(progress_bar(100, 5), "█████");
        assert_eq!(progress_bar(0, 5), "░░░░░");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("abcdefg", 5), "abcd…");
        assert_eq!(truncate("abc", 5), "abc");
    }

    #[test]
    fn test_empty_weakest() {
        let mut s = st();
        s.weakest.clear();
        let out = render_terminal(&s);
        assert!(out.contains("全部掌握或未开始"));
    }
}
