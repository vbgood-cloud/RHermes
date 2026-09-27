//! D18 P0：Jev 中文判断质量评估（门槛测试）
//!
//! 运行方式（需要真实 API key + 外网访问）：
//! ```text
//! $env:TYPESAFE_API_KEY = "ts-xxx"
//! cargo test --test jev_p0_eval --release -- --nocapture --ignored
//! ```
//!
//! 通过标准（D18 §五.P0）：
//! - Noul 方向正确率 ≥ 85%（按 0.5 分界）
//! - Choice 首选正确率 ≥ 80%
//! - 不达标 → 冻结 P1-P3 集成
//!
//! 测试集：20 条真实风格中文 state（口语/中英混合），
//! 场景对应 P1（记忆保存 Noul ×12）与 P2（技能生命周期 Choice ×8）。
//! 记忆样本附带 Score 观察问（档位打印，不计入判定 —— score 弱校准禁插值）。

use std::collections::HashMap;

use rhermes::core::ProxyConfig;
use rhermes::judge::{Judge, JudgeClient, Question};

// ---------------------------------------------------------------------------
// 评估样本
// ---------------------------------------------------------------------------

/// P1 场景：记忆是否值得长期保存（Noul，期望 bool）
const MEMORY_SAMPLES: &[(&str, bool)] = &[
    ("用户明确说：以后所有 Rust 代码注释都用中文写", true),
    ("帮我记住：这个项目的部署路径固定是 e:\\ai\\new", true),
    ("今天的主要任务是修复登录 bug，目前已经完成一半，还剩测试没跑", false),
    ("临时记一下 TODO：明天下班顺路买牛奶", false),
    ("用户的 API 配置：base_url https://api.deepseek.com，模型 deepseek-chat", true),
    ("这轮对话我一共搜索了三个网页，结果感觉都不太相关", false),
    ("记住：用户公司名称是星河科技，对接人是张经理", true),
    ("调试报告已导出到 debug_report.zip，大小 2MB", false),
    ("用户说他受不了长篇大论，回答必须尽量精简", true),
    ("刚才跑完测试，373 个测试全部通过，全绿", false),
    ("用户访问外网统一走 socks5://127.0.0.1:1080 这个代理", true),
    ("那个文件刚才写错了，我重新写一遍就好", false),
];

/// P2 场景：技能生命周期（Choice: active/stale/archived，期望选项）
const SKILL_SAMPLES: &[(&str, &str)] = &[
    ("技能：Excel 批量合并工具。最近 2 天被使用了 5 次，用户频繁处理报表", "active"),
    ("技能：旧版微信 iLink API 调试方法。该 API 半年前已下线，45 天未使用", "archived"),
    ("技能：PDF 文本抽取。30 天未使用，但文档处理需求在这类项目中仍然常见", "stale"),
    ("技能：Windows 下用 taskkill 强杀占用进程。昨天刚用过一次", "active"),
    ("技能：v0.6 旧配置格式迁移步骤。v0.7 已全量替换该格式，60 天未使用", "archived"),
    ("技能：DeepSeek 前缀缓存优化技巧。上周刚用过，项目仍在深度使用 DeepSeek", "active"),
    ("技能：某编程比赛的提交规范。比赛已于上月结束，50 天未使用", "archived"),
    ("技能：Telegram bot 部署流程。用户计划下月启用该通道，25 天未使用", "stale"),
];

/// 保存判断问题（P1 计划接入的实际问法）
const MEMORY_QUESTION: &str = "该内容是否具有跨会话的长期价值（用户偏好、事实、配置、联系人等知识）？排除：任务进度、临时 TODO、已完成的工作日志、一次性操作记录。";

/// 生命周期问题（P2 计划接入的实际问法）
const SKILL_QUESTION: &str = "根据该技能的描述与最近使用情况，它应处于哪个生命周期状态？";

/// Score 观察问（不计入判定，仅打印分布）
const SCORE_QUESTION: &str = "该内容的表述清晰度与信息完整度处于哪一档？";
const SCORE_LEVELS: &[&str] = &["表述含糊，信息缺失", "基本清晰，略有歧义", "清晰完整"];

fn skill_criteria() -> HashMap<String, String> {
    HashMap::from([
        ("active".to_string(), "仍被频繁需要，最近有使用或明确有持续需求".to_string()),
        ("stale".to_string(), "疑似过时，一段时间未用但需求可能仍在，待观察".to_string()),
        ("archived".to_string(), "依赖已废弃或场景已结束，应归档".to_string()),
    ])
}

// ---------------------------------------------------------------------------
// 评估执行
// ---------------------------------------------------------------------------

/// 单条评估结果（统计用）
struct EvalRow {
    state: String,
    /// (期望, 实际, 是否正确)
    noul: Option<(bool, f64, bool)>,
    choice: Option<(String, String, bool)>,
    clarity: Option<f64>,
    error: Option<String>,
}

#[tokio::test]
#[ignore = "需要 TYPESAFE_API_KEY 与外网访问；D18 P0 门槛测试"]
async fn p0_chinese_quality_gate() {
    let api_key = std::env::var("TYPESAFE_API_KEY")
        .expect("未设置 TYPESAFE_API_KEY 环境变量（从 console.typesafe.ai/keys 获取）");

    // P0 直连官方端点（评估环境不走代理配置）；超时放宽到 30s 避免网络抖动误判
    let judge = Judge::new(
        JudgeClient::new(
            &ProxyConfig::default(),
            "https://api.typesafe.ai",
            &api_key,
            std::time::Duration::from_secs(30),
        ),
        0.0, // P0 阶段不做门控，观察原始分布
        "jev-latest",
    );

    // 并发评估全部样本（限 5 并发，避免限流）
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(5));
    let mut set = tokio::task::JoinSet::new();

    for (state, expect) in MEMORY_SAMPLES {
        let j = judge.clone();
        let sem = semaphore.clone();
        let state = state.to_string();
        let expect = *expect;
        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let mut qs = HashMap::new();
            qs.insert("memory".to_string(), Question::noul(MEMORY_QUESTION));
            qs.insert(
                "clarity".to_string(),
                Question::score(SCORE_QUESTION, SCORE_LEVELS.iter().map(|s| s.to_string()).collect()),
            );
            let resp = j.ask(&state, qs).await;
            (state, expect.to_string(), resp)
        });
    }
    for (state, expect) in SKILL_SAMPLES {
        let j = judge.clone();
        let sem = semaphore.clone();
        let state = state.to_string();
        let expect = expect.to_string();
        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let mut qs = HashMap::new();
            qs.insert("lifecycle".to_string(), Question::choice(SKILL_QUESTION, skill_criteria()));
            let resp = j.ask(&state, qs).await;
            (state, expect, resp)
        });
    }

    // 收集与统计
    let mut rows: Vec<EvalRow> = Vec::new();
    let mut input_tokens = 0u64;
    while let Some(joined) = set.join_next().await {
        let (state, expect, resp): (String, String, _) = joined.unwrap();
        match resp {
            Ok(r) => {
                input_tokens += r.usage.input_tokens;
                let mut row = EvalRow { state, noul: None, choice: None, clarity: None, error: None };
                if let Some(p) = r.answers.get("memory").and_then(|a| a.as_noul()) {
                    let got = p >= 0.5;
                    let expect_bool = expect == "true";
                    let ok = got == expect_bool;
                    row.noul = Some((expect_bool, p, ok));
                }
                if let Some((choice, _)) = r.answers.get("lifecycle").and_then(|a| a.as_choice()) {
                    let ok = choice == expect;
                    row.choice = Some((expect, choice.to_string(), ok));
                }
                row.clarity = r.answers.get("clarity").and_then(|a| a.as_score()).map(|(s, _)| s);
                rows.push(row);
            }
            Err(e) => rows.push(EvalRow {
                state,
                noul: None,
                choice: None,
                clarity: None,
                error: Some(e.to_string()),
            }),
        }
    }

    // 输出报告
    println!("\n================ D18 P0 中文判断质量报告 ================\n");
    let mut noul_ok = 0;
    let mut noul_total = 0;
    let mut choice_ok = 0;
    let mut choice_total = 0;
    for row in &rows {
        if let Some(err) = &row.error {
            println!("⚠️  请求失败: {err} | {}", row.state);
            continue;
        }
        if let Some((expect, p, ok)) = row.noul {
            noul_total += 1;
            noul_ok += ok as usize;
            let clarity = row.clarity.map(|c| format!("清晰度={c:.1}")).unwrap_or_default();
            println!("{} [Noul p={p:.2}] 期望保存={expect} | {}  {clarity}", if ok { "✅" } else { "❌" }, row.state);
        }
        if let Some((expect, got, ok)) = &row.choice {
            choice_total += 1;
            choice_ok += *ok as usize;
            println!("{} [Choice] 期望={expect} 得={got} | {}", if *ok { "✅" } else { "❌" }, row.state);
        }
    }

    let noul_rate = if noul_total > 0 { noul_ok as f64 / noul_total as f64 } else { 0.0 };
    let choice_rate = if choice_total > 0 { choice_ok as f64 / choice_total as f64 } else { 0.0 };
    println!("\n----------------------------------------------------------");
    println!("Noul   方向正确率: {noul_ok}/{noul_total} = {:.1}%  (门槛 85%)", noul_rate * 100.0);
    println!("Choice 首选正确率: {choice_ok}/{choice_total} = {:.1}%  (门槛 80%)", choice_rate * 100.0);
    println!("累计 input_tokens: {input_tokens} (约 ${:.4})", input_tokens as f64 * 0.042 / 1_000_000.0);
    println!("==========================================================\n");

    // 失败样本占比过高（网络错误 >20%）时拒绝下结论
    let failed = rows.iter().filter(|r| r.error.is_some()).count();
    assert!(failed * 5 <= rows.len(), "网络失败 {failed}/{} 条，结果不可信，请重跑", rows.len());

    assert!(noul_rate >= 0.85, "D18 P0 未达标：Noul 正确率 {:.1}% < 85%，冻结 P1-P3", noul_rate * 100.0);
    assert!(choice_rate >= 0.80, "D18 P0 未达标：Choice 正确率 {:.1}% < 80%，冻结 P1-P3", choice_rate * 100.0);
}
