//! 知识库学习系统 — 数据层
//!
//! SQLite 存储：topics / nodes / edges / quiz_log / sessions / question_bank(预留)
//! 掌握度模型：EMA 更新（新测验权重 60%）+ 24h 防刷分

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

/// 导出文件格式标识
pub const EXPORT_FORMAT: &str = "rhermes-kb";
/// 导出文件格式版本（当前 v1）
pub const EXPORT_VERSION: u32 = 1;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS topics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE NOT NULL,
    source TEXT NOT NULL DEFAULT 'topic',
    created_at TEXT DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    topic_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    summary TEXT DEFAULT '',
    layer INTEGER DEFAULT 0,
    mastery INTEGER DEFAULT 0,
    review_count INTEGER DEFAULT 0,
    quiz_count INTEGER DEFAULT 0,
    last_review TEXT,
    UNIQUE(topic_id, name)
);
CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    topic_id INTEGER NOT NULL,
    from_node TEXT NOT NULL,
    to_node TEXT NOT NULL,
    relation TEXT DEFAULT '相关'
);
CREATE TABLE IF NOT EXISTS quiz_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id INTEGER NOT NULL,
    question TEXT DEFAULT '',
    answer TEXT DEFAULT '',
    score INTEGER NOT NULL,
    source TEXT DEFAULT 'agent',
    created_at TEXT DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    topic_id INTEGER NOT NULL,
    node_name TEXT NOT NULL,
    created_at TEXT DEFAULT (datetime('now'))
);
-- 预留：标准化题库（MVP 不填充，kb_quiz --draw 抽题用）
CREATE TABLE IF NOT EXISTS question_bank (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id INTEGER NOT NULL,
    q_type TEXT DEFAULT 'choice',
    question TEXT NOT NULL,
    options TEXT DEFAULT '[]',
    answer TEXT NOT NULL,
    explanation TEXT DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_nodes_topic ON nodes(topic_id);
CREATE INDEX IF NOT EXISTS idx_edges_topic ON edges(topic_id);
CREATE INDEX IF NOT EXISTS idx_quiz_node ON quiz_log(node_id);
"#;

/// 打开（并迁移 schema）知识库数据库
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// 输入类型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize)]
pub struct NodeIn {
    pub name: String,
    pub summary: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EdgeIn {
    pub from: String,
    pub to: String,
    pub relation: String,
}

// ---------------------------------------------------------------------------
// 快照类型（渲染层输入）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NodeRow {
    pub id: i64,
    pub name: String,
    pub summary: String,
    pub layer: i64,
    pub mastery: i64,
    pub review_count: i64,
    pub quiz_count: i64,
}

#[derive(Debug, Clone)]
pub struct EdgeRow {
    pub from: String,
    pub to: String,
    pub relation: String,
}

#[derive(Debug, Clone, Default)]
pub struct GraphSnapshot {
    pub topic: String,
    pub nodes: Vec<NodeRow>,
    pub edges: Vec<EdgeRow>,
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// 创建知识库（重名报错）
pub fn create_topic(conn: &Connection, name: &str, source: &str) -> rusqlite::Result<i64> {
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM topics WHERE name = ?1", params![name], |r| r.get(0))
        .optional()?;
    if let Some(id) = existing {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!("知识库 '{name}' 已存在 (id={id})，如需重建请先删除"),
        )));
    }
    conn.execute("INSERT INTO topics (name, source) VALUES (?1, ?2)", params![name, source])?;
    Ok(conn.last_insert_rowid())
}

pub fn topic_id(conn: &Connection, name: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT id FROM topics WHERE name = ?1", params![name], |r| r.get(0))
        .optional()
}

/// 批量写入节点（返回成功数；重名跳过）
pub fn add_nodes(conn: &Connection, tid: i64, nodes: &[NodeIn]) -> rusqlite::Result<usize> {
    let mut n = 0;
    for node in nodes {
        let changed = conn.execute(
            "INSERT OR IGNORE INTO nodes (topic_id, name, summary) VALUES (?1, ?2, ?3)",
            params![tid, node.name, node.summary],
        )?;
        n += changed;
    }
    Ok(n)
}

/// 批量写入边（引用的节点不存在则跳过，返回 (成功, 跳过)）
pub fn add_edges(conn: &Connection, tid: i64, edges: &[EdgeIn]) -> rusqlite::Result<(usize, usize)> {
    let names: std::collections::HashSet<String> = conn
        .prepare("SELECT name FROM nodes WHERE topic_id = ?1")?
        .query_map(params![tid], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    let mut ok = 0;
    let mut skipped = 0;
    for e in edges {
        if names.contains(&e.from) && names.contains(&e.to) && e.from != e.to {
            conn.execute(
                "INSERT INTO edges (topic_id, from_node, to_node, relation) VALUES (?1,?2,?3,?4)",
                params![tid, e.from, e.to, e.relation],
            )?;
            ok += 1;
        } else {
            skipped += 1;
        }
    }
    Ok((ok, skipped))
}

/// 重算所有节点的拓扑层级（Kahn 分层；环与孤立点放最后可达层）
pub fn recompute_layers(conn: &Connection, tid: i64) -> rusqlite::Result<()> {
    let snapshot = snapshot(conn, tid)?;
    let layers = super::layout::compute_layers(&snapshot);
    for node in &snapshot.nodes {
        if let Some(l) = layers.get(&node.name) {
            conn.execute("UPDATE nodes SET layer = ?1 WHERE id = ?2", params![l, node.id])?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 学习与测验
// ---------------------------------------------------------------------------

/// 选取下一个该学的节点：layer ASC → mastery ASC → review_count ASC → id ASC
pub fn next_node(conn: &Connection, tid: i64) -> rusqlite::Result<Option<NodeRow>> {
    conn.query_row(
        "SELECT id, name, summary, layer, mastery, review_count, quiz_count FROM nodes
         WHERE topic_id = ?1 AND mastery < 80
         ORDER BY layer ASC, mastery ASC, review_count ASC, id ASC LIMIT 1",
        params![tid],
        row_to_node,
    )
    .optional()
}

/// 记录一次学习（session 步数）
pub fn log_session(conn: &Connection, tid: i64, node_name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sessions (topic_id, node_name) VALUES (?1, ?2)",
        params![tid, node_name],
    )?;
    Ok(())
}

/// 复位某知识库的学习进度：掌握度/复习/测验计数归零，清空测验史与会话记录。
/// 保留知识结构（topics/nodes/edges）不变。返回 (节点数, 测验记录数, 会话数)。
pub fn reset_topic_progress(conn: &Connection, tid: i64) -> rusqlite::Result<(usize, usize, usize)> {
    let nodes: usize = conn.query_row(
        "SELECT COUNT(*) FROM nodes WHERE topic_id = ?1",
        params![tid],
        |r| r.get(0),
    )?;
    let quiz: usize = conn.query_row(
        "SELECT COUNT(*) FROM quiz_log WHERE node_id IN (SELECT id FROM nodes WHERE topic_id = ?1)",
        params![tid],
        |r| r.get(0),
    )?;
    let sessions: usize = conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE topic_id = ?1",
        params![tid],
        |r| r.get(0),
    )?;
    conn.execute(
        "UPDATE nodes SET mastery = 0, review_count = 0, quiz_count = 0, last_review = NULL WHERE topic_id = ?1",
        params![tid],
    )?;
    conn.execute(
        "DELETE FROM quiz_log WHERE node_id IN (SELECT id FROM nodes WHERE topic_id = ?1)",
        params![tid],
    )?;
    conn.execute("DELETE FROM sessions WHERE topic_id = ?1", params![tid])?;
    Ok((nodes, quiz, sessions))
}

/// 记录测验结果并 EMA 更新掌握度。
/// 返回 (新掌握度, 是否生效)。24h 内同节点取最高分（防刷分）。
pub fn record_quiz(
    conn: &Connection,
    node_id: i64,
    score: i64,
    question: &str,
    answer: &str,
) -> rusqlite::Result<(i64, bool)> {
    let score = score.clamp(0, 100);
    let today_best: Option<i64> = conn
        .query_row(
            "SELECT MAX(score) FROM quiz_log WHERE node_id = ?1 AND date(created_at) = date('now')",
            params![node_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();

    if let Some(best) = today_best {
        if score <= best {
            return Ok((node_mastery(conn, node_id)?, false)); // 今日已有更高分，不生效
        }
    }

    let old: (i64, i64) = conn.query_row(
        "SELECT mastery, review_count FROM nodes WHERE id = ?1",
        params![node_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )?;
    let new_mastery = if old.1 == 0 {
        score
    } else {
        (old.0 * 40 + score * 60) / 100
    };
    conn.execute(
        "UPDATE nodes SET mastery = ?1, review_count = review_count + 1,
         quiz_count = quiz_count + 1, last_review = datetime('now') WHERE id = ?2",
        params![new_mastery, node_id],
    )?;
    conn.execute(
        "INSERT INTO quiz_log (node_id, question, answer, score) VALUES (?1,?2,?3,?4)",
        params![node_id, question, answer, score],
    )?;
    Ok((new_mastery, true))
}

pub fn node_mastery(conn: &Connection, node_id: i64) -> rusqlite::Result<i64> {
    conn.query_row("SELECT mastery FROM nodes WHERE id = ?1", params![node_id], |r| r.get(0))
}

/// 从预留题库抽题（MVP 通常为空）
pub fn draw_questions(conn: &Connection, node_id: i64, count: usize) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT question, options, answer FROM question_bank WHERE node_id = ?1 ORDER BY RANDOM() LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![node_id, count as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ---------------------------------------------------------------------------
// 快照与统计
// ---------------------------------------------------------------------------

fn row_to_node(r: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        id: r.get(0)?,
        name: r.get(1)?,
        summary: r.get(2)?,
        layer: r.get(3)?,
        mastery: r.get(4)?,
        review_count: r.get(5)?,
        quiz_count: r.get(6)?,
    })
}

pub fn snapshot(conn: &Connection, tid: i64) -> rusqlite::Result<GraphSnapshot> {
    let topic: String = conn.query_row("SELECT name FROM topics WHERE id = ?1", params![tid], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT id, name, summary, layer, mastery, review_count, quiz_count
         FROM nodes WHERE topic_id = ?1 ORDER BY layer ASC, id ASC",
    )?;
    let nodes: Vec<NodeRow> = stmt.query_map(params![tid], row_to_node)?.filter_map(|r| r.ok()).collect();

    let mut stmt = conn.prepare(
        "SELECT from_node, to_node, relation FROM edges WHERE topic_id = ?1",
    )?;
    let edges: Vec<EdgeRow> = stmt
        .query_map(params![tid], |r| {
            Ok(EdgeRow { from: r.get(0)?, to: r.get(1)?, relation: r.get(2)? })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(GraphSnapshot { topic, nodes, edges })
}

#[derive(Debug, Clone, Default)]
pub struct KbStats {
    pub topic: String,
    pub total_nodes: usize,
    pub lit_nodes: usize,       // mastery > 0
    pub mastered_nodes: usize,  // mastery >= 80
    pub avg_mastery: i64,       // 全部节点均值
    pub quiz_total: i64,
    pub quiz_avg: i64,
    pub learn_steps: i64,
    pub today_steps: i64,
    pub weakest: Vec<(String, i64)>, // 点亮节点中最弱 3 个
    pub quiz_today: i64,
}

pub fn stats(conn: &Connection, tid: i64) -> rusqlite::Result<KbStats> {
    let topic: String = conn.query_row("SELECT name FROM topics WHERE id = ?1", params![tid], |r| r.get(0))?;
    let (total, lit, mastered, avg): (i64, i64, i64, f64) = conn.query_row(
        "SELECT COUNT(*), SUM(mastery > 0), SUM(mastery >= 80), IFNULL(AVG(mastery), 0)
         FROM nodes WHERE topic_id = ?1",
        params![tid],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0), r.get::<_, Option<i64>>(2)?.unwrap_or(0), r.get::<_, f64>(3)?)),
    )?;
    let (quiz_total, quiz_avg): (i64, f64) = conn.query_row(
        "SELECT COUNT(*), IFNULL(AVG(q.score), 0) FROM quiz_log q
         JOIN nodes n ON q.node_id = n.id WHERE n.topic_id = ?1",
        params![tid],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
    )?;
    let (steps, today_steps, quiz_today): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), SUM(date(created_at) = date('now')), 0 FROM sessions WHERE topic_id = ?1",
        params![tid],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0), r.get::<_, i64>(2)?)),
    )?;
    let quiz_today: i64 = conn.query_row(
        "SELECT COUNT(*) FROM quiz_log q JOIN nodes n ON q.node_id = n.id
         WHERE n.topic_id = ?1 AND date(q.created_at) = date('now')",
        params![tid],
        |r| r.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT name, mastery FROM nodes WHERE topic_id = ?1 AND mastery > 0 AND mastery < 80
         ORDER BY mastery ASC LIMIT 3",
    )?;
    let weakest: Vec<(String, i64)> = stmt
        .query_map(params![tid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(KbStats {
        topic,
        total_nodes: total as usize,
        lit_nodes: lit as usize,
        mastered_nodes: mastered as usize,
        avg_mastery: avg.round() as i64,
        quiz_total,
        quiz_avg: quiz_avg.round() as i64,
        learn_steps: steps,
        today_steps,
        weakest,
        quiz_today,
    })
}

pub fn list_topics(conn: &Connection) -> rusqlite::Result<Vec<(String, String, usize, usize, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT t.name, t.source,
                (SELECT COUNT(*) FROM nodes n WHERE n.topic_id = t.id),
                (SELECT COUNT(*) FROM nodes n WHERE n.topic_id = t.id AND n.mastery > 0),
                IFNULL(CAST((SELECT AVG(n.mastery) FROM nodes n WHERE n.topic_id = t.id) AS INTEGER), 0)
         FROM topics t ORDER BY t.id DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, usize>(2)?, r.get::<_, usize>(3)?, r.get::<_, i64>(4)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// 指定节点的关联信息（前置/后续）
pub fn node_context(conn: &Connection, tid: i64, name: &str) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    let mut prereq = Vec::new();
    let mut stmt = conn.prepare("SELECT from_node FROM edges WHERE topic_id = ?1 AND to_node = ?2")?;
    for r in stmt.query_map(params![tid, name], |r| r.get::<_, String>(0))?.filter_map(|r| r.ok()) {
        prereq.push(r);
    }
    let mut next = Vec::new();
    let mut stmt = conn.prepare("SELECT to_node FROM edges WHERE topic_id = ?1 AND from_node = ?2")?;
    for r in stmt.query_map(params![tid, name], |r| r.get::<_, String>(0))?.filter_map(|r| r.ok()) {
        next.push(r);
    }
    Ok((prereq, next))
}

/// 掌握度 → 分档（0-3）。终端符号/SVG 颜色共用。
pub fn mastery_stage(mastery: i64) -> u8 {
    match mastery {
        0 => 0,       // 未学习
        1..=49 => 1,  // 初识
        50..=79 => 2, // 掌握中
        _ => 3,       // 精通
    }
}

pub fn stage_name(stage: u8) -> &'static str {
    match stage {
        0 => "未学习",
        1 => "初识",
        2 => "掌握中",
        _ => "精通",
    }
}

// ---------------------------------------------------------------------------
// 导出与导入
// ---------------------------------------------------------------------------

/// 导出文件顶层结构（.kb.json）
///
/// 设计要点：表间关联一律用 node name（库内 UNIQUE(topic_id, name) 保证唯一），
/// 不导出自增 ID —— 导入时重新生成，天然避免 ID 冲突。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KbExport {
    pub format: String,
    pub version: u32,
    pub exported_at: String,
    pub topic: TopicOut,
    pub graph: GraphOut,
    /// 学习记录（--kb 纯库导出时为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learning: Option<LearningOut>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TopicOut {
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphOut {
    pub nodes: Vec<NodeOut>,
    pub edges: Vec<EdgeOut>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NodeOut {
    pub name: String,
    pub summary: String,
    pub layer: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EdgeOut {
    pub from: String,
    pub to: String,
    pub relation: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LearningOut {
    /// 节点掌握度状态（mastery/review_count/quiz_count/last_review）
    pub nodes: Vec<LearnNodeOut>,
    /// 测验历史（保留全量以维持 24h 防刷分逻辑一致）
    pub quiz_log: Vec<QuizLogOut>,
    /// 学习会话记录
    pub sessions: Vec<SessionOut>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LearnNodeOut {
    pub name: String,
    pub mastery: i64,
    pub review_count: i64,
    pub quiz_count: i64,
    pub last_review: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuizLogOut {
    pub node: String,
    pub question: String,
    pub answer: String,
    pub score: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionOut {
    pub node: String,
    pub created_at: String,
}

/// 导入结果报告
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub topic: String,
    pub nodes_imported: usize,
    pub edges_ok: usize,
    pub edges_skipped: usize,
    pub quiz_log_imported: usize,
    pub sessions_imported: usize,
    pub with_learning: bool,
}

/// 导出知识库为可序列化结构
///
/// - `with_learning = true`：附带学习记录（掌握度状态 + 测验史 + 学习会话）
/// - `with_learning = false`：纯库导出（只含节点/关系/层级），适合分享给他人从零学
pub fn export_topic(conn: &Connection, tid: i64, with_learning: bool) -> rusqlite::Result<KbExport> {
    let (name, source): (String, String) = conn.query_row(
        "SELECT name, source FROM topics WHERE id = ?1",
        params![tid],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;

    let mut stmt = conn.prepare(
        "SELECT name, summary, layer FROM nodes WHERE topic_id = ?1 ORDER BY layer ASC, id ASC",
    )?;
    let nodes: Vec<NodeOut> = stmt
        .query_map(params![tid], |r| {
            Ok(NodeOut { name: r.get(0)?, summary: r.get(1)?, layer: r.get(2)? })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut stmt = conn.prepare("SELECT from_node, to_node, relation FROM edges WHERE topic_id = ?1")?;
    let edges: Vec<EdgeOut> = stmt
        .query_map(params![tid], |r| {
            Ok(EdgeOut { from: r.get(0)?, to: r.get(1)?, relation: r.get(2)? })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let learning = if with_learning {
        let mut stmt = conn.prepare(
            "SELECT name, mastery, review_count, quiz_count, last_review FROM nodes WHERE topic_id = ?1 ORDER BY id ASC",
        )?;
        let lnodes: Vec<LearnNodeOut> = stmt
            .query_map(params![tid], |r| {
                Ok(LearnNodeOut {
                    name: r.get(0)?,
                    mastery: r.get(1)?,
                    review_count: r.get(2)?,
                    quiz_count: r.get(3)?,
                    last_review: r.get(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        let mut stmt = conn.prepare(
            "SELECT n.name, q.question, q.answer, q.score, q.created_at
             FROM quiz_log q JOIN nodes n ON q.node_id = n.id
             WHERE n.topic_id = ?1 ORDER BY q.id ASC",
        )?;
        let quiz_log: Vec<QuizLogOut> = stmt
            .query_map(params![tid], |r| {
                Ok(QuizLogOut {
                    node: r.get(0)?,
                    question: r.get(1)?,
                    answer: r.get(2)?,
                    score: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        let mut stmt = conn.prepare(
            "SELECT node_name, created_at FROM sessions WHERE topic_id = ?1 ORDER BY id ASC",
        )?;
        let sessions: Vec<SessionOut> = stmt
            .query_map(params![tid], |r| {
                Ok(SessionOut { node: r.get(0)?, created_at: r.get(1)? })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Some(LearningOut { nodes: lnodes, quiz_log, sessions })
    } else {
        None
    };

    let exported_at: String = conn.query_row("SELECT datetime('now')", [], |r| r.get(0))?;
    Ok(KbExport {
        format: EXPORT_FORMAT.to_string(),
        version: EXPORT_VERSION,
        exported_at,
        topic: TopicOut { name, source },
        graph: GraphOut { nodes, edges },
        learning,
    })
}

/// 导入知识库（完整还原；文件带 learning 且节点状态可迁移则一并导入）
///
/// - `new_name`：换名导入（目标库重名时用）
/// - 目标库已存在 → 报错（与 create_topic 重名语义一致）
/// - 边引用无效节点 → 复用 add_edges 自动跳过并计数
/// - 导入完成后重算拓扑层级，保证图结构一致
pub fn import_topic(
    conn: &Connection,
    data: &KbExport,
    new_name: Option<&str>,
) -> Result<ImportReport, String> {
    // 1. 格式校验
    if data.format != EXPORT_FORMAT {
        return Err(format!(
            "格式不匹配：{}/v{}（当前支持 {}）",
            data.format, data.version, EXPORT_FORMAT
        ));
    }
    if data.version > EXPORT_VERSION {
        return Err(format!(
            "导出文件版本 v{} 高于当前支持的 v{}，请升级 RHermes 后重试",
            data.version, EXPORT_VERSION
        ));
    }
    if data.graph.nodes.is_empty() {
        return Err("导出文件不含任何知识点".to_string());
    }
    let name = new_name
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(&data.topic.name);
    if topic_id(conn, name).map_err(|e| e.to_string())?.is_some() {
        return Err(format!(
            "知识库 '{name}' 已存在；用 /learn import <路径> --as <新名> 换名导入"
        ));
    }

    // 2. 事务写入
    let tx = conn.unchecked_transaction().map_err(|e| format!("开启事务失败: {e}"))?;
    tx.execute(
        "INSERT INTO topics (name, source) VALUES (?1, ?2)",
        params![name, data.topic.source],
    )
    .map_err(|e| format!("创建知识库失败: {e}"))?;
    let tid = tx.last_insert_rowid();

    // 学习记录按 node name 建索引
    let learn: HashMap<String, &LearnNodeOut> = data
        .learning
        .as_ref()
        .map(|l| l.nodes.iter().map(|n| (n.name.clone(), n)).collect())
        .unwrap_or_default();

    let mut report = ImportReport {
        topic: name.to_string(),
        with_learning: data.learning.is_some(),
        ..Default::default()
    };

    for node in &data.graph.nodes {
        tx.execute(
            "INSERT INTO nodes (topic_id, name, summary, layer) VALUES (?1, ?2, ?3, ?4)",
            params![tid, node.name, node.summary, node.layer],
        )
        .map_err(|e| format!("写入节点 '{}' 失败: {e}", node.name))?;
        let nid = tx.last_insert_rowid();
        if let Some(ln) = learn.get(&node.name) {
            let _ = tx.execute(
                "UPDATE nodes SET mastery = ?1, review_count = ?2, quiz_count = ?3, last_review = ?4 WHERE id = ?5",
                params![ln.mastery, ln.review_count, ln.quiz_count, ln.last_review, nid],
            );
        }
        report.nodes_imported += 1;
    }

    let edges_in: Vec<EdgeIn> = data
        .graph
        .edges
        .iter()
        .map(|e| EdgeIn { from: e.from.clone(), to: e.to.clone(), relation: e.relation.clone() })
        .collect();
    let (ok, skip) = add_edges(&tx, tid, &edges_in).map_err(|e| format!("写入关系失败: {e}"))?;
    report.edges_ok = ok;
    report.edges_skipped = skip;

    if let Some(l) = &data.learning {
        for q in &l.quiz_log {
            let nid: Option<i64> = tx
                .query_row(
                    "SELECT id FROM nodes WHERE topic_id = ?1 AND name = ?2",
                    params![tid, q.node],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(nid) = nid {
                tx.execute(
                    "INSERT INTO quiz_log (node_id, question, answer, score, created_at) VALUES (?1,?2,?3,?4,?5)",
                    params![nid, q.question, q.answer, q.score, q.created_at],
                )
                .map_err(|e| format!("写入测验记录失败: {e}"))?;
                report.quiz_log_imported += 1;
            }
        }
        for s in &l.sessions {
            tx.execute(
                "INSERT INTO sessions (topic_id, node_name, created_at) VALUES (?1, ?2, ?3)",
                params![tid, s.node, s.created_at],
            )
            .map_err(|e| format!("写入学习会话失败: {e}"))?;
            report.sessions_imported += 1;
        }
    }

    // 3. 重算拓扑层级（防导出后图被修改导致层级漂移）
    recompute_layers(&tx, tid).map_err(|e| format!("重算层级失败: {e}"))?;
    tx.commit().map_err(|e| format!("提交事务失败: {e}"))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!("kb-test-{}-{}.db", std::process::id(), n));
        let _ = std::fs::remove_file(&p);
        open(&p).unwrap()
    }

    fn sample(conn: &Connection) -> i64 {
        let tid = create_topic(conn, "t1", "topic").unwrap();
        add_nodes(conn, tid, &[
            NodeIn { name: "基础".into(), summary: "b".into() },
            NodeIn { name: "进阶".into(), summary: "a".into() },
        ]).unwrap();
        add_edges(conn, tid, &[EdgeIn { from: "基础".into(), to: "进阶".into(), relation: "依赖".into() }]).unwrap();
        recompute_layers(conn, tid).unwrap();
        tid
    }

    #[test]
    fn test_create_duplicate() {
        let conn = test_db();
        assert!(create_topic(&conn, "x", "topic").is_ok());
        assert!(create_topic(&conn, "x", "topic").is_err()); // 重名
    }

    #[test]
    fn test_layers_kahn() {
        let conn = test_db();
        let tid = sample(&conn);
        let snap = snapshot(&conn, tid).unwrap();
        let m: HashMap<String, i64> = snap.nodes.iter().map(|n| (n.name.clone(), n.layer)).collect();
        assert_eq!(m["基础"], 0);
        assert_eq!(m["进阶"], 1);
    }

    #[test]
    fn test_edge_validation() {
        let conn = test_db();
        let tid = sample(&conn);
        let (ok, skip) = add_edges(&conn, tid, &[
            EdgeIn { from: "基础".into(), to: "不存在".into(), relation: "x".into() },
            EdgeIn { from: "基础".into(), to: "基础".into(), relation: "自环".into() },
        ]).unwrap();
        assert_eq!((ok, skip), (0, 2)); // 不存在/自环都跳过
    }

    #[test]
    fn test_next_node_order() {
        let conn = test_db();
        let tid = sample(&conn);
        let n = next_node(&conn, tid).unwrap().unwrap();
        assert_eq!(n.name, "基础"); // layer 0 优先
    }

    #[test]
    fn test_ema_and_antibrush() {
        let conn = test_db();
        let tid = sample(&conn);
        let node_id: i64 = conn.query_row(
            "SELECT id FROM nodes WHERE topic_id = ?1 AND name = '基础'", params![tid], |r| r.get(0),
        ).unwrap();
        // 首次：直接取分
        let (m, applied) = record_quiz(&conn, node_id, 80, "q", "a").unwrap();
        assert!(applied);
        assert_eq!(m, 80);
        // 同日低分：不生效
        let (m2, applied2) = record_quiz(&conn, node_id, 60, "q", "a").unwrap();
        assert!(!applied2);
        assert_eq!(m2, 80);
        // 同日高分：EMA (80*40+100*60)/100 = 92
        let (m3, applied3) = record_quiz(&conn, node_id, 100, "q", "a").unwrap();
        assert!(applied3);
        assert_eq!(m3, 92);
    }

    #[test]
    fn test_stats() {
        let conn = test_db();
        let tid = sample(&conn);
        let s = stats(&conn, tid).unwrap();
        assert_eq!(s.total_nodes, 2);
        assert_eq!(s.lit_nodes, 0);
        assert_eq!(s.avg_mastery, 0);
    }

    #[test]
    fn test_reset_progress() {
        let conn = test_db();
        let tid = sample(&conn);
        // 制造学习记录：掌握度 + 测验史 + 学习会话
        let node_id: i64 = conn
            .query_row(
                "SELECT id FROM nodes WHERE topic_id = ?1 AND name = '基础'",
                params![tid],
                |r| r.get(0),
            )
            .unwrap();
        record_quiz(&conn, node_id, 80, "q", "a").unwrap();
        log_session(&conn, tid, "基础").unwrap();

        // 复位前：有进度
        let s0 = stats(&conn, tid).unwrap();
        assert_eq!(s0.lit_nodes, 1);

        // 复位
        let (nodes, quiz, sessions) = reset_topic_progress(&conn, tid).unwrap();
        assert_eq!(nodes, 2); // 结构保留
        assert_eq!(quiz, 1);
        assert_eq!(sessions, 1);

        // 复位后：进度归零，结构仍在
        let s1 = stats(&conn, tid).unwrap();
        assert_eq!(s1.total_nodes, 2); // 节点数不变
        assert_eq!(s1.lit_nodes, 0);
        assert_eq!(s1.avg_mastery, 0);
        let snap = snapshot(&conn, tid).unwrap();
        assert_eq!(snap.edges.len(), 1); // 关系保留
    }

    #[test]
    fn test_stage() {
        assert_eq!(mastery_stage(0), 0);
        assert_eq!(mastery_stage(49), 1);
        assert_eq!(mastery_stage(50), 2);
        assert_eq!(mastery_stage(80), 3);
    }

    #[test]
    fn test_export_import_roundtrip() {
        let conn = test_db();
        let tid = sample(&conn);
        // 制造学习记录：掌握度 + 测验史 + 学习会话
        let node_id: i64 = conn
            .query_row(
                "SELECT id FROM nodes WHERE topic_id = ?1 AND name = '基础'",
                params![tid],
                |r| r.get(0),
            )
            .unwrap();
        record_quiz(&conn, node_id, 80, "q1", "a1").unwrap();
        log_session(&conn, tid, "基础").unwrap();

        // 完整导出
        let data = export_topic(&conn, tid, true).unwrap();
        assert_eq!(data.format, "rhermes-kb");
        assert_eq!(data.version, 1);
        assert_eq!(data.graph.nodes.len(), 2);
        assert_eq!(data.graph.edges.len(), 1);
        let learning = data.learning.as_ref().expect("完整导出应含学习记录");
        assert_eq!(learning.quiz_log.len(), 1);
        assert_eq!(learning.sessions.len(), 1);

        // JSON 序列化/反序列化往返
        let json = serde_json::to_string(&data).unwrap();
        let data2: KbExport = serde_json::from_str(&json).unwrap();
        assert_eq!(data2.topic.name, data.topic.name);

        // 换名导入
        let rep = import_topic(&conn, &data2, Some("t2")).unwrap();
        assert_eq!(rep.topic, "t2");
        assert_eq!(rep.nodes_imported, 2);
        assert_eq!(rep.edges_ok, 1);
        assert_eq!(rep.edges_skipped, 0);
        assert!(rep.with_learning);
        assert_eq!(rep.quiz_log_imported, 1);
        assert_eq!(rep.sessions_imported, 1);

        // 验证掌握度迁移 + 层级重算
        let tid2 = topic_id(&conn, "t2").unwrap().unwrap();
        let snap = snapshot(&conn, tid2).unwrap();
        let n = snap.nodes.iter().find(|x| x.name == "基础").unwrap();
        assert_eq!(n.mastery, 80);
        assert_eq!(n.review_count, 1);
        assert_eq!(n.layer, 0);

        // 重名导入报错
        assert!(import_topic(&conn, &data2, Some("t2")).is_err());
    }

    #[test]
    fn test_export_kb_only() {
        let conn = test_db();
        let tid = sample(&conn);
        let node_id: i64 = conn
            .query_row("SELECT id FROM nodes WHERE topic_id = ?1 LIMIT 1", params![tid], |r| r.get(0))
            .unwrap();
        record_quiz(&conn, node_id, 90, "q", "a").unwrap();

        // 纯库导出：无 learning
        let data = export_topic(&conn, tid, false).unwrap();
        assert!(data.learning.is_none());
        assert_eq!(data.graph.nodes.len(), 2);

        // 导入后新库掌握度全 0
        let rep = import_topic(&conn, &data, Some("t2")).unwrap();
        assert!(!rep.with_learning);
        assert_eq!(rep.quiz_log_imported, 0);
        let tid2 = topic_id(&conn, "t2").unwrap().unwrap();
        let snap = snapshot(&conn, tid2).unwrap();
        assert!(snap.nodes.iter().all(|n| n.mastery == 0));
    }

    #[test]
    fn test_import_rejects_bad_format() {
        let conn = test_db();
        let data = KbExport {
            format: "other".to_string(),
            version: 1,
            exported_at: "x".to_string(),
            topic: TopicOut { name: "a".to_string(), source: "topic".to_string() },
            graph: GraphOut {
                nodes: vec![NodeOut { name: "n".to_string(), summary: String::new(), layer: 0 }],
                edges: vec![],
            },
            learning: None,
        };
        assert!(import_topic(&conn, &data, None).is_err());

        let mut data = data.clone();
        data.format = EXPORT_FORMAT.to_string();
        data.version = 99;
        assert!(import_topic(&conn, &data, None).is_err()); // 版本过高

        let mut data = data.clone();
        data.version = 1;
        data.graph.nodes.clear();
        assert!(import_topic(&conn, &data, None).is_err()); // 空节点
    }
}
