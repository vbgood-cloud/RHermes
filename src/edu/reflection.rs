//! 反思系统 + AI 素养评估
//!
//! 每次对话结束后自动生成针对性反思提示，评估学生的 AI 素养。
//! 反思由 AI 根据对话内容生成（非固定模板），避免流于形式。

use serde::{Deserialize, Serialize};

use crate::edu::store::EduStore;

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

/// 反思评分结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionScore {
    /// 反思深度（0-1）：学生是否深入思考了自己的学习过程
    pub depth: f64,
    /// 提问质量（0-1）：学生在对话中提问的清晰度和深度
    pub question_quality: f64,
    /// 工具使用合理性（0-1）：是否选择了正确的工具和参数
    pub tool_usage_quality: f64,
    /// AI 透明度理解（0-1）：是否理解了 AI 的工作原理和局限
    pub ai_transparency: f64,
    /// 综合评分（0-1）
    pub overall: f64,
}

impl Default for ReflectionScore {
    fn default() -> Self {
        Self {
            depth: 0.5,
            question_quality: 0.5,
            tool_usage_quality: 0.5,
            ai_transparency: 0.5,
            overall: 0.5,
        }
    }
}

impl ReflectionScore {
    /// 从各维度计算综合评分
    pub fn calculate_overall(&mut self) {
        self.overall = (self.depth * 0.3
            + self.question_quality * 0.25
            + self.tool_usage_quality * 0.25
            + self.ai_transparency * 0.2)
            .clamp(0.0, 1.0);
    }
}

/// 反思提示（由 AI 生成）
#[derive(Debug, Clone)]
pub struct ReflectionPrompt {
    /// 反思问题文本
    pub question: String,
    /// 引导提示（可选）
    pub hint: Option<String>,
}

// ---------------------------------------------------------------------------
// 反思提示生成
// ---------------------------------------------------------------------------

/// 根据对话内容和工具使用情况生成反思提示
///
/// 这不是固定模板——而是根据对话主题、工具调用动态选择最相关的反思维度。
/// 在实际使用中，可以调用 AI 来生成更精准的反思提示。
/// 当前版本使用基于规则的智能选择 + 模板组合。
pub fn generate_reflection_prompt(
    conversation_summary: &str,
    tools_used: &[String],
    mode: &str,
) -> ReflectionPrompt {
    // 根据使用的工具选择反思维度
    let tool_dimension = if tools_used.is_empty() {
        "工具选择"
    } else if tools_used.iter().any(|t| t.contains("search") || t.contains("web_")) {
        "信息检索"
    } else if tools_used.iter().any(|t| t.contains("read_file") || t.contains("read_pdf")) {
        "信息理解"
    } else if tools_used.iter().any(|t| t.contains("write") || t.contains("run_command")) {
        "动手实践"
    } else {
        "工具选择"
    };

    // 根据模式调整反思方向
    let mode_intro = match mode {
        "scaffold" => "在刚才的引导式学习中，",
        "locked" => "在刚才的查阅过程中，",
        _ => "在刚才的对话中，",
    };

    // 组合反思提示
    let question = format!(
        "{mode_intro}你主要用 AI 做了什么？\n\n\
         请反思以下问题：\n\
         1. 关于{tool_dimension}：你为什么这样使用工具？有没有更好的方式？\n\
         2. 关于 AI 的回答：你觉得 AI 给出的方案中，哪部分可能是错的？你会怎么验证？\n\
         3. 关于自己的思考：如果不用 AI，你会怎么解决这个问题？第一步做什么？\n\n\
         请简要回答（2-3 句话即可）。"
    );

    let hint = Some(format!("提示：好的反思不只是总结，而是分析\"为什么\"和\"怎么样\"。"));

    ReflectionPrompt { question, hint }
}

/// 预设的反思提示库（按场景分类）
pub static REFLECTION_TEMPLATES: &[(&str, &str)] = &[
    ("工具使用", "你刚才使用了 {tools} 工具。你为什么选择这个工具？如果换成其他工具会怎样？"),
    ("AI 幻觉", "AI 给出的回答中，你觉得哪些信息你可能需要验证？你会用什么方式验证？"),
    ("问题分解", "如果不用 AI，你会怎样把这个问题分解成更小的步骤？"),
    ("思维过程", "在整个过程中，你改变了哪些想法？是什么让你改变想法的？"),
    ("效率优化", "如果再来一次，你会怎样更高效地使用 AI 完成这个任务？"),
    ("批判思考", "AI 的回答有没有遗漏什么重要的情况？你补充了什么？"),
];

/// 从模板库中随机选择一个反思提示
pub fn pick_template_reflection(tools_used: &[String]) -> String {
    let idx = if tools_used.is_empty() {
        0
    } else {
        // 简单的伪随机：用工具数量取模
        tools_used.len() % REFLECTION_TEMPLATES.len()
    };

    let (_, template) = REFLECTION_TEMPLATES[idx];
    let tools_str = if tools_used.is_empty() {
        "这些".to_string()
    } else {
        tools_used.join(", ")
    };
    template.replace("{tools}", &tools_str)
}

// ---------------------------------------------------------------------------
// 反思评估
// ---------------------------------------------------------------------------

/// 评估学生的反思回答
///
/// 基于文本特征做启发式评估（实际使用中可以调用 AI 更精准评估）。
pub fn evaluate_reflection(reflection_text: &str, conversation_length: usize) -> ReflectionScore {
    let text = reflection_text.trim();
    let char_count = text.chars().count();
    let word_count = text.split_whitespace().count();

    let mut score = ReflectionScore::default();

    // 反思深度：基于字数、关键词、结构
    let depth_keywords = ["因为", "所以", "如果", "发现", "意识到", "学到了", "错误", "改进", "验证", "更", "应该"];
    let keyword_hits = depth_keywords.iter()
        .filter(|kw| text.contains(*kw))
        .count();

    score.depth = if char_count < 10 {
        0.1 // 太短，敷衍
    } else if char_count < 30 {
        0.3
    } else {
        let base = 0.4;
        let keyword_bonus = (keyword_hits as f64 * 0.12).min(0.4);
        let length_bonus = if char_count > 80 { 0.15 } else { 0.0 };
        (base + keyword_bonus + length_bonus).min(1.0)
    };

    // 提问质量：基于对话长度反推（对话越长，说明学生越投入）
    score.question_quality = if conversation_length < 50 {
        0.3
    } else if conversation_length < 200 {
        0.5
    } else if conversation_length < 500 {
        0.7
    } else {
        0.85
    };

    // 工具使用合理性：默认中等
    score.tool_usage_quality = 0.6;

    // AI 透明度：检查是否提到了 AI 的局限或验证
    let transparency_keywords = ["验证", "AI 错", "不准确", "检查", "确认", "局限", "可能错"];
    let transparency_hits = transparency_keywords.iter()
        .filter(|kw| text.contains(*kw))
        .count();
    score.ai_transparency = (0.3 + transparency_hits as f64 * 0.2).min(1.0);

    score.calculate_overall();
    score
}

/// 评估学生提问质量
///
/// 分析问题的结构性特征。
pub fn evaluate_question_quality(question: &str) -> f64 {
    let q = question.trim();
    let char_count = q.chars().count();

    if char_count < 5 {
        return 0.1; // 太短
    }

    let mut score: f64 = 0.3; // 基础分

    // 有具体上下文（提到了具体文件/函数/概念）
    if q.contains('.') || q.contains("::") || q.contains("函数") || q.contains("变量") || q.contains("文件") {
        score += 0.2;
    }

    // 有明确的问题描述
    let question_words = ["怎么", "为什么", "是什么", "如何", "哪里", "哪个", "哪种", "区别", "比较"];
    if question_words.iter().any(|w| q.contains(w)) {
        score += 0.15;
    }

    // 有尝试/错误描述
    if q.contains("尝试") || q.contains("报错") || q.contains("失败") || q.contains("但是") {
        score += 0.15;
    }

    // 长度合理（不太短也不太长）
    if char_count > 20 && char_count < 200 {
        score += 0.1;
    }

    // 有代码引用
    if q.contains("```") || q.contains("fn ") || q.contains("def ") || q.contains("class ") {
        score += 0.1;
    }

    score.min(1.0)
}

// ---------------------------------------------------------------------------
// 成长报告
// ---------------------------------------------------------------------------

/// 生成学生学习成长报告（Markdown）
pub fn generate_growth_report(
    student_name: &str,
    student_no: &str,
    journals: &[crate::edu::store::LearningJournal],
) -> String {
    if journals.is_empty() {
        return format!("# 学习成长报告\n\n**学生**: {} ({})\n\n暂无学习记录。\n", student_name, student_no);
    }

    let mut md = format!("# 学习成长报告\n\n");
    md.push_str(&format!("**学生**: {} ({})\n", student_name, student_no));
    md.push_str(&format!("**记录数**: {} 条\n\n", journals.len()));

    // 统计
    let total_sessions = journals.len();
    let avg_quality: f64 = journals.iter().map(|j| j.quality_score).sum::<f64>() / total_sessions as f64;
    let avg_reflection: f64 = journals.iter().map(|j| j.reflection_depth).sum::<f64>() / total_sessions as f64;
    let total_tokens: i64 = journals.iter().map(|j| j.token_usage).sum();
    let total_duration: i64 = journals.iter().map(|j| j.duration_secs).sum();

    md.push_str("## 📊 总体统计\n\n");
    md.push_str(&format!("| 指标 | 值 |\n|------|----|\n"));
    md.push_str(&format!("| 学习次数 | {} |\n", total_sessions));
    md.push_str(&format!("| 平均提问质量 | {:.0}% |\n", avg_quality * 100.0));
    md.push_str(&format!("| 平均反思深度 | {:.0}% |\n", avg_reflection * 100.0));
    md.push_str(&format!("| Token 总用量 | {} |\n", total_tokens));
    md.push_str(&format!("| 学习总时长 | {} 分钟 |\n\n", total_duration / 60));

    // 成长趋势（最近 vs 之前）
    if journals.len() >= 2 {
        let mid = journals.len() / 2;
        let earlier_avg: f64 = journals[..mid].iter().map(|j| j.quality_score).sum::<f64>() / mid as f64;
        let recent_avg: f64 = journals[mid..].iter().map(|j| j.quality_score).sum::<f64>() / (journals.len() - mid) as f64;
        let trend = if recent_avg > earlier_avg + 0.05 {
            "📈 上升"
        } else if recent_avg < earlier_avg - 0.05 {
            "📉 下降"
        } else {
            "➡️ 稳定"
        };
        md.push_str(&format!("## 📈 成长趋势\n\n提问质量趋势: {} ({:.0}% → {:.0}%)\n\n", trend, earlier_avg * 100.0, recent_avg * 100.0));
    }

    // 最近记录
    md.push_str("## 📝 最近学习记录\n\n");
    for j in journals.iter().rev().take(5) {
        md.push_str(&format!("### {} (课次{})\n", j.topic, j.lesson_num));
        md.push_str(&format!("- 提问质量: {:.0}%\n", j.quality_score * 100.0));
        md.push_str(&format!("- 反思深度: {:.0}%\n", j.reflection_depth * 100.0));
        if !j.reflection.is_empty() {
            md.push_str(&format!("- 反思: {}\n", j.reflection));
        }
        md.push_str("\n");
    }

    md
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_reflection_prompt() {
        let prompt = generate_reflection_prompt("讨论了 Rust 的所有权", &["read_file".to_string()], "explore");
        assert!(!prompt.question.is_empty());
        assert!(prompt.question.contains("工具"));
        assert!(prompt.question.contains("AI"));
    }

    #[test]
    fn test_generate_reflection_scaffold_mode() {
        let prompt = generate_reflection_prompt("Python 循环", &[], "scaffold");
        assert!(prompt.question.contains("引导式学习"));
    }

    #[test]
    fn test_evaluate_reflection_shallow() {
        let score = evaluate_reflection("没什么", 100);
        assert!(score.depth < 0.3); // 敷衍回答 → 低分
    }

    #[test]
    fn test_evaluate_reflection_deep() {
        let score = evaluate_reflection(
            "我发现自己一开始就直接问 AI 要代码，没有先思考。因为这个问题其实可以分解为两步。如果下次遇到，我应该先自己分析。",
            200,
        );
        assert!(score.depth > 0.5); // 深度反思 → 高分
    }

    #[test]
    fn test_evaluate_question_quality_short() {
        let score = evaluate_question_quality("help");
        assert!(score < 0.3);
    }

    #[test]
    fn test_evaluate_question_quality_detailed() {
        let score = evaluate_question_quality("我的 Cargo.toml 里 serde 版本冲突，编译报错说 version solving failed，但我试了 cargo update 没用");
        assert!(score > 0.5);
    }

    #[test]
    fn test_reflection_score_calculate() {
        let mut score = ReflectionScore {
            depth: 0.8,
            question_quality: 0.6,
            tool_usage_quality: 0.7,
            ai_transparency: 0.5,
            overall: 0.0,
        };
        score.calculate_overall();
        assert!(score.overall > 0.6);
        assert!(score.overall <= 1.0);
    }

    #[test]
    fn test_growth_report_empty() {
        let report = generate_growth_report("张三", "2024001", &[]);
        assert!(report.contains("暂无学习记录"));
    }

    #[test]
    fn test_growth_report_with_data() {
        let journals = vec![
            crate::edu::store::LearningJournal {
                id: 1, student_id: 1, course_id: 1, lesson_num: 1,
                topic: "Rust 所有权".into(), tool_calls: "[]".into(),
                reflection: "学会了所有权转移".into(),
                quality_score: 0.7, reflection_depth: 0.6,
                token_usage: 1000, duration_secs: 300,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            crate::edu::store::LearningJournal {
                id: 2, student_id: 1, course_id: 1, lesson_num: 2,
                topic: "Rust 借用".into(), tool_calls: "[]".into(),
                reflection: "理解了借用规则".into(),
                quality_score: 0.8, reflection_depth: 0.75,
                token_usage: 1200, duration_secs: 400,
                created_at: "2026-01-02T00:00:00Z".into(),
            },
        ];
        let report = generate_growth_report("张三", "2024001", &journals);
        assert!(report.contains("学习成长报告"));
        assert!(report.contains("75%")); // (0.7+0.8)/2
        assert!(report.contains("Rust 所有权"));
        assert!(report.contains("成长趋势"));
    }

    #[test]
    fn test_pick_template_reflection() {
        let r = pick_template_reflection(&["read_file".to_string()]);
        assert!(!r.is_empty());
    }
}
