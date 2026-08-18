//! 知识库学习系统 — 数据层
//!
//! SQLite 存储：topics / nodes / edges / quiz_log / sessions / question_bank(预留)
//! 掌握度模型：EMA 更新（新测验权重 60%）+ 24h 防刷分

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

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
    fn test_stage() {
        assert_eq!(mastery_stage(0), 0);
        assert_eq!(mastery_stage(49), 1);
        assert_eq!(mastery_stage(50), 2);
        assert_eq!(mastery_stage(80), 3);
    }
}
