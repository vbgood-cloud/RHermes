//! D18 P1 端到端验证：生产判断链路
//!
//! 运行方式（需内网 winnow 可达 + key）：
//! ```text
//! $env:TYPESAFE_API_KEY = "c46f..."; $env:JEV_BASE_URL = "http://10.126.126.3:11435"
//! cargo test --test jev_p1_e2e --release -- --nocapture --ignored
//! ```
//!
//! 验证链路：Config[jev] → Judge::from_config → set_global_judge
//!         → ToolDispatcher.dispatch → Memory 工具 add 门控 → winnow 判断
//!         → 落库 / 拒绝（含 MEMORY.md 磁盘状态断言）

use rhermes::core::{Config, JevConfig};
use rhermes::judge::{Judge, set_global_judge};
use rhermes::tools::{ToolCall, ToolDispatcher, builtin_registry};

/// 构建 winnow 生产配置（端点/模型可用环境变量覆盖）
fn winnow_config() -> Config {
    let base_url =
        std::env::var("JEV_BASE_URL").unwrap_or_else(|_| "http://10.126.126.3:11435".into());
    let api_key = std::env::var("TYPESAFE_API_KEY")
        .unwrap_or_else(|_| "c46fbe87cc25af4066858a458af1c48acb0e647e677eb964".into());
    let model = std::env::var("JEV_MODEL").unwrap_or_else(|_| "winnow:e4b".into());

    let mut config = Config::default();
    config.jev = JevConfig {
        enabled: true,
        model,
        base_url,
        api_key,
        timeout_secs: 10,
        min_confidence: 0.6,
    };
    config
}

/// memory add 工具调用
fn memory_add(content: &str) -> ToolCall {
    ToolCall {
        id: "e2e_call".into(),
        name: "memory".into(),
        arguments: serde_json::json!({
            "action": "add",
            "target": "memory",
            "content": content,
        }),
    }
}

/// 测试二进制旁的 MEMORY.md 路径（与 Memory 工具内部 PathManager 逻辑一致）
fn memory_md_path() -> std::path::PathBuf {
    rhermes::core::PathManager::detect()
        .data_root()
        .join("memories")
        .join("MEMORY.md")
}

#[tokio::test]
#[ignore = "需要内网 winnow 端点可达"]
async fn p1_memory_gate_end_to_end() {
    let config = winnow_config();
    // from_config 返回 Option：enabled+key 齐备时为 Some；断言启用成功
    let judge = Judge::from_config(&config).expect("判断层应启用（enabled+key 已配置）");
    set_global_judge(Some(judge));

    let dispatcher = ToolDispatcher::new(builtin_registry(&config));

    // 干净起点：删除上次 E2E 残留
    let md = memory_md_path();
    let _ = std::fs::remove_file(&md);

    // ── 场景 1：临时信息应被拒绝 ─────────────────────────────
    let r = dispatcher.dispatch(vec![memory_add("临时 TODO：E2E 测试后记得清理桌面")]).await;
    let out1 = &r[0].output;
    println!("[临时信息] -> {out1}");
    assert!(
        out1.contains("未保存"),
        "临时 TODO 应被判断层拒绝，实际: {out1}"
    );
    let disk1 = std::fs::read_to_string(&md).unwrap_or_default();
    assert!(!disk1.contains("E2E 测试后记得清理桌面"), "磁盘不应包含被拒内容");

    // ── 场景 2：长期价值信息应落库 ───────────────────────────
    let r = dispatcher
        .dispatch(vec![memory_add("用户偏好：E2E 验证通过，代码注释一律用中文")])
        .await;
    let out2 = &r[0].output;
    println!("[长期价值] -> {out2}");
    assert!(out2.contains("已记住"), "用户偏好应被保存，实际: {out2}");
    let disk2 = std::fs::read_to_string(&md).unwrap_or_default();
    assert!(disk2.contains("代码注释一律用中文"), "磁盘应包含已保存内容");

    // 清理：抹掉 E2E 写入的条目，恢复干净状态
    let _ = std::fs::remove_file(&md);
    println!("\n✅ D18 P1 端到端通过：临时信息被拒（未落库）、长期价值落库（MEMORY.md 已验证）");
}

/// D18 P2 端到端：判断层参与技能生命周期巡检
///
/// - "expired-api-v0.6-migration"（95 天未用）：时间规则必判 Archived（>90），强断言
/// - "deepseek-prefix-cache-tuning"（45 天未用）：时间规则判 Stale；判断层结论打印人工检视
#[tokio::test]
#[ignore = "需要内网 winnow 端点可达"]
async fn p2_curator_judge_override() {
    let config = winnow_config();
    let judge = Judge::from_config(&config).expect("判断层应启用");
    set_global_judge(Some(judge));

    // 临时技能目录 + 两个长期未用的技能
    let tmp = tempfile::TempDir::new().unwrap();
    for (name, days) in [
        ("expired-api-v0.6-migration", 95i64),
        ("deepseek-prefix-cache-tuning", 45),
    ] {
        let content = format!("---\ndescription: \"{name}\"\n---\n\n# {name}\n\nBody\n");
        std::fs::write(tmp.path().join(format!("{name}.md")), content).unwrap();
        let usage = rhermes::agent::UsageTelemetry {
            use_count: 5,
            view_count: 0,
            patch_count: 0,
            last_used_at: Some((chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339()),
            created_at: None,
            archived_at: None,
            pinned: false,
        };
        std::fs::write(
            tmp.path().join(format!("{name}.usage.json")),
            serde_json::to_string_pretty(&usage).unwrap(),
        )
        .unwrap();
    }

    let curator = rhermes::agent::Curator::new(tmp.path().to_path_buf(), config);
    let report = curator.run_with_judge().await;

    println!("\n[P2 巡检报告] {}", report.format());
    println!("[P2 归档列表] {:?}", report.archived);
    println!("[P2 过期列表] {:?}", report.stale);

    assert!(report.errors.is_empty(), "巡检不应有错误: {:?}", report.errors);
    // 强断言：语义明确该归档的技能被处理（无论来自时间规则还是判断层）
    assert!(
        report.archived.iter().any(|n| n.contains("v0.6")),
        "过期 API 迁移技能应被归档，实际归档: {:?}",
        report.archived
    );
    println!("✅ D18 P2 端到端通过：判断层参与巡检且语义归档生效");
}
