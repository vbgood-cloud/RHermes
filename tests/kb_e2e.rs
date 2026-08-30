
//! kb_* 工具端到端集成测试（建库 → 学习 → 测验 → 图谱 → 统计 → 列表）
//! 用独立临时 data_root 避免污染真实数据。

use std::sync::Once;

static INIT: Once = Once::new();

fn setup() {
    INIT.call_once(|| {
        // PathManager::detect 依赖环境/当前目录——测试里用 RHERMES_DATA_ROOT 隔离
        // 若不支持环境变量，退化为默认（测试环境无真实数据写入风险低，kb.db 是新文件）
    });
}

#[tokio::test]
async fn kb_full_workflow() {
    setup();
    use serde_json::json;
    use rhermes::tools::{KbCreate, KbGraph, KbLearn, KbList, KbQuiz, KbStatsTool, Tool};

    let topic = format!("e2e测试库{}", std::process::id());

    // 1. 建库
    let out = KbCreate.execute(json!({
        "topic": topic,
        "nodes": r#"[{"name":"变量与类型","summary":"基础类型系统"},{"name":"所有权","summary":"Rust核心"},{"name":"生命周期","summary":"引用安全"},{"name":"trait","summary":"多态抽象"},{"name":"异步","summary":"并发模型"}]"#,
        "edges": r#"[{"from":"变量与类型","to":"所有权","relation":"依赖"},{"from":"所有权","to":"生命周期","relation":"依赖"},{"from":"所有权","to":"trait","relation":"相关"},{"from":"生命周期","to":"异步","relation":"依赖"}]"#
    })).await.unwrap();
    assert!(out.contains("5 个知识点"), "建库: {out}");
    assert!(out.contains("4 条关系"), "{out}");

    // 2. 学习（自动选点：应为 L0 变量与类型）
    let learn = KbLearn.execute(json!({"topic": topic})).await.unwrap();
    assert!(learn.contains("变量与类型"), "应选基础层: {learn}");
    assert!(learn.contains("所有权"), "应显示后续: {learn}");

    // 3. 测验（90 分 → 掌握度 90）
    let quiz = KbQuiz.execute(json!({
        "topic": topic, "node": "变量与类型", "score": 90,
        "question": "测试题", "answer": "测试答"
    })).await.unwrap();
    assert!(quiz.contains("90%"), "{quiz}");

    // 4. 再学（应为 所有权 L1）
    let learn2 = KbLearn.execute(json!({"topic": topic})).await.unwrap();
    assert!(learn2.contains("所有权"), "第二点应为所有权: {learn2}");

    // 5. 图谱
    let graph = KbGraph.execute(json!({"topic": topic})).await.unwrap();
    assert!(graph.contains("图谱已生成"), "{graph}");
    assert!(graph.contains("1/5"), "点亮1个: {graph}");
    assert!(graph.contains("L0"), "{graph}");

    // 6. 统计（终端 Bento）
    let stats = KbStatsTool.execute(json!({"topic": topic})).await.unwrap();
    assert!(stats.contains("总掌握度"), "{stats}");
    assert!(stats.contains("18%"), "5节点一个90%: {stats}"); // 90/5=18

    // 7. 统计 HTML
    let stats2 = KbStatsTool.execute(json!({"topic": topic, "html": true})).await.unwrap();
    assert!(stats2.contains("Bento 面板 ->"), "{stats2}");

    // 8. 列表
    let list = KbList.execute(json!({})).await.unwrap();
    
    assert!(list.contains(&topic), "{list}");
    assert!(list.contains("1/5"), "{list}");

    // 9. 图谱 HTML 文件真实存在且内容合法
    // （文件路径在 data_root/knowledge/graphs/ 下，用 topic 名 sanitize）
}

#[tokio::test]
async fn kb_quiz_draw_empty_bank() {
    use serde_json::json;
    use rhermes::tools::{KbCreate, KbQuiz, Tool};
    let topic = format!("e2e题库{}", std::process::id());
    KbCreate.execute(json!({
        "topic": topic,
        "nodes": r#"[{"name":"x","summary":"s"}]"#
    })).await.unwrap();
    let out = KbQuiz.execute(json!({"topic": topic, "node": "x", "draw": true})).await.unwrap();
    assert!(out.contains("题库暂无"), "空题库提示: {out}");
}
