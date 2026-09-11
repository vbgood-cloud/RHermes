//! 知识库学习系统 — 数据层
//!
//! SQLite 存储：topics / nodes / edges / quiz_log / sessions / question_bank(预留)
//! 掌握度模型：EMA 更新（新测验权重 60%）+ 24h 防刷分
//! 遗忘曲线：指数衰减 `retention = 2^(-Δt/S)`，S 为记忆半衰期（天），随复习质量倍增

use std::collections::HashMap;
use std::path::Path;

use chrono::{NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

/// 导出文件格式标识
pub const EXPORT_FORMAT: &str = "rhermes-kb";
/// 导出文件格式版本（v2 新增 stability/next_review_at；v3 新增 easiness，均向后兼容）
pub const EXPORT_VERSION: u32 = 3;

/// 复习红线默认值（effective 低于此触发到期复习）；可通过 config.toml 的 `[knowledge].review_floor` 覆盖
pub const REVIEW_FLOOR_DEFAULT: i64 = 60;
/// 记忆半衰期初值（天）
pub const S_INIT: f64 = 1.0;
/// 半衰期钳制下界（天）
pub const S_MIN: f64 = 0.5;
/// 半衰期钳制上界（天）
pub const S_MAX: f64 = 180.0;
/// SM-2 难度系数（EF）初值；2.5 为中性基准，此时对半衰期增长无额外调制
pub const EF_INIT: f64 = 2.5;
/// 难度系数下界（SM-2 原版下限：低于此不再下降）
pub const EF_MIN: f64 = 1.3;
/// 难度系数上界（防止难度系数无限膨胀）
pub const EF_MAX: f64 = 2.8;

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
    stability REAL DEFAULT 1.0,
    next_review_at TEXT,
    easiness REAL DEFAULT 2.5,
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
    migrate(&conn)?;
    Ok(conn)
}

/// 一次性迁移（PRAGMA user_version 跟踪）。
/// v1: nodes 表新增 stability / next_review_at；按 review_count 回填 stability。
/// v2: nodes 表新增 easiness（SM-2 难度系数），默认 2.5（中性）。
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v < 1 {
        // 新增列（幂等：重复执行会被 SQLite 报错，但幂等语义通过 PRAGMA 守护）
        let _ = conn.execute("ALTER TABLE nodes ADD COLUMN stability REAL DEFAULT 1.0", []);
        let _ = conn.execute("ALTER TABLE nodes ADD COLUMN next_review_at TEXT", []);
        // 回填 stability：复习次数越多半衰期越长（power(2, rc-1)，钳制 [0.5, 180]）
        let _ = conn.execute(
            "UPDATE nodes SET stability = MAX(0.5, MIN(180.0, power(2, MAX(0, review_count - 1)))) WHERE review_count > 0",
            [],
        );
        // 回填 next_review_at：last_review + stability 天（信息性字段，用于显示「下次复习」）
        let _ = conn.execute(
            "UPDATE nodes SET next_review_at = datetime(last_review, '+' || CAST(ROUND(stability) AS INTEGER) || ' days') WHERE last_review IS NOT NULL",
            [],
        );
        conn.execute("PRAGMA user_version = 1", [])?;
    }
    if v < 2 {
        // v2: 新增 easiness（SM-2 难度系数），旧库默认 2.5（中性，对半衰期无额外调制）
        let _ = conn.execute("ALTER TABLE nodes ADD COLUMN easiness REAL DEFAULT 2.5", []);
        conn.execute("PRAGMA user_version = 2", [])?;
    }
    Ok(())
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
    pub mastery: i64,             // 历史掌握度（EMA，测验时更新，不衰减）
    pub review_count: i64,
    pub quiz_count: i64,
    /// 记忆半衰期（天），默认 1.0
    pub stability: f64,
    /// 距上次复习的整天数（None = 从未复习）
    pub last_review: Option<String>,
    /// 当前有效掌握度 = mastery × 2^(-Δt/stability)，在 snapshot/row_to_node 时按当前时间计算
    pub effective_mastery: i64,
    /// SM-2 难度系数（1.3–2.8，初值 2.5），只调制半衰期的增长率，不直接决定间隔
    pub easiness: f64,
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
// 遗忘曲线辅助（纯函数，便于单测）
// ---------------------------------------------------------------------------

/// 艾宾浩斯保持率：`2^(-Δt/S)`，S = 记忆半衰期（天）。
/// Δt ≤ 0 视为 1.0（刚复习或未来时间，防御性）。
/// S ≤ 0 视为 0（异常值）。
pub fn retention(stability: f64, days_since: f64) -> f64 {
    if stability <= 0.0 { return 0.0; }
    if days_since <= 0.0 { return 1.0; }
    0.5_f64.powf(days_since / stability)
}

/// 解析 SQLite `datetime('now')` 格式（`YYYY-MM-DD HH:MM:SS`）为 UTC naive。
fn parse_sqlite_dt(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S").ok()
}

/// 当前时间距 `last_review` 的整天数（向下取整）。None = 从未复习或解析失败。
pub fn days_since_now(last_review: Option<&str>) -> Option<i64> {
    let s = last_review?;
    let lr = parse_sqlite_dt(s)?;
    let now = Utc::now().naive_utc();
    let secs = (now - lr).num_seconds();
    if secs <= 0 { return Some(0); }
    Some((secs as f64 / 86400.0).floor() as i64)
}

/// 计算有效掌握度：未学返回 0；从未复习（last_review=None）或今天刚复习返回 mastery；
/// 否则 `round(mastery * 2^(-Δt/S))` 钳制 [0, 100]。
pub fn compute_effective(mastery: i64, stability: f64, last_review: Option<&str>) -> i64 {
    if mastery <= 0 { return 0; }
    let days = match days_since_now(last_review) {
        Some(d) if d > 0 => d as f64,
        _ => return mastery,
    };
    let r = retention(stability, days);
    let eff = (mastery as f64 * r).round() as i64;
    eff.clamp(0, 100)
}

/// 测验得分 → 半衰期增长因子
fn stability_factor(score: i64) -> f64 {
    if score >= 90 { 2.5 }
    else if score >= 70 { 2.0 }
    else if score >= 50 { 1.5 }
    else { 0.7 }
}

/// 测验得分 → SM-2 质量分 q（0–5，100 分制按 20 分一档折算，四舍五入）
pub fn quality(score: i64) -> i64 {
    (score.clamp(0, 100) as f64 / 20.0).round() as i64
}

/// SM-2 难度系数递推：`EF' = EF + 0.1 - (5-q)(0.08 + 0.02(5-q))`，钳制 [EF_MIN, EF_MAX]。
/// q=5 → +0.10（变简单）；q=4 → 0；q=3 → -0.14；q=0 → -0.80（变难）。
pub fn next_easiness(current: f64, score: i64) -> f64 {
    let d = (5 - quality(score)) as f64;
    (current + 0.1 - d * (0.08 + 0.02 * d)).clamp(EF_MIN, EF_MAX)
}

/// 按「分档增长因子 × EF 难度乘数」更新半衰期并钳制到 [S_MIN, S_MAX]。
/// EF 以 EF_INIT(2.5) 为中性：EF=2.5 时乘数为 1，行为与引入 EF 前完全一致（向后兼容锚点）。
pub fn next_stability(current: f64, score: i64, easiness: f64) -> f64 {
    let ef_mult = (easiness / EF_INIT).clamp(0.5, 1.2);
    (current * stability_factor(score) * ef_mult).clamp(S_MIN, S_MAX)
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

/// 选点结果：节点 + 是否到期复习轮 + 衰减信息
#[derive(Debug, Clone)]
pub struct NextNode {
    pub node: NodeRow,
    /// true = 到期复习（effective < review_floor），Agent 应先提问检验而非重新讲解
    pub is_review: bool,
    /// 距上次复习的整天数（未学过为 None）
    pub days_since: Option<i64>,
    /// 当前有效掌握度（= mastery × 2^(-Δt/S)）
    pub effective_mastery: i64,
}

/// 选取下一个该学的节点。
/// 调度优先级：① 到期复习（effective < review_floor，effective 升序）
/// → ② 未学（mastery=0，layer/id 升序）
/// → ③ 薄弱巩固（60 ≤ effective < 80，effective 升序）
/// → ④ None（全部 effective ≥80，学习完成；遗忘随时间让节点重新掉入①）
pub fn next_node(conn: &Connection, tid: i64, review_floor: i64) -> rusqlite::Result<Option<NextNode>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, summary, layer, mastery, review_count, quiz_count, stability, last_review, easiness
         FROM nodes WHERE topic_id = ?1",
    )?;
    let nodes: Vec<NodeRow> = stmt.query_map(params![tid], row_to_node)?.filter_map(|r| r.ok()).collect();

    // ① 到期复习
    let mut due: Vec<&NodeRow> = nodes.iter().filter(|n| n.mastery > 0 && n.effective_mastery < review_floor).collect();
    if let Some(n) = pick_due(&mut due) {
        return Ok(Some(build_next(n, true, review_floor)));
    }

    // ② 未学
    let mut new_nodes: Vec<&NodeRow> = nodes.iter().filter(|n| n.mastery == 0).collect();
    if let Some(n) = pick_new(&mut new_nodes) {
        return Ok(Some(build_next(n, false, review_floor)));
    }

    // ③ 薄弱巩固
    let mut weak: Vec<&NodeRow> = nodes.iter().filter(|n| n.mastery > 0 && n.effective_mastery >= review_floor && n.effective_mastery < 80).collect();
    if let Some(n) = pick_due(&mut weak) {
        return Ok(Some(build_next(n, false, review_floor)));
    }

    Ok(None)
}

/// 按名称取节点（含到期状态与有效掌握度；用户点名学习/复习时使用）
pub fn find_node(conn: &Connection, tid: i64, name: &str, review_floor: i64) -> rusqlite::Result<Option<NextNode>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, summary, layer, mastery, review_count, quiz_count, stability, last_review, easiness
         FROM nodes WHERE topic_id = ?1 AND name = ?2",
    )?;
    let n: Option<NodeRow> = stmt.query_row(params![tid, name], row_to_node).ok();
    Ok(n.map(|n| {
        let eff = n.effective_mastery;
        let is_review = n.mastery > 0 && eff < review_floor;
        let days_since = if n.mastery > 0 { days_since_now(n.last_review.as_deref()) } else { None };
        NextNode { node: n, is_review, days_since, effective_mastery: eff }
    }))
}

/// 当前到期该复习的节点数（统计与进度提示用）
pub fn due_review_count(conn: &Connection, tid: i64, review_floor: i64) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, name, summary, layer, mastery, review_count, quiz_count, stability, last_review, easiness
         FROM nodes WHERE topic_id = ?1",
    )?;
    let count = stmt
        .query_map(params![tid], row_to_node)?
        .filter_map(|r| r.ok())
        .filter(|n| n.mastery > 0 && n.effective_mastery < review_floor)
        .count();
    Ok(count)
}

fn pick_due<'a>(v: &mut Vec<&'a NodeRow>) -> Option<&'a NodeRow> {
    v.sort_by_key(|n| n.effective_mastery);
    v.first().copied()
}

fn pick_new<'a>(v: &mut Vec<&'a NodeRow>) -> Option<&'a NodeRow> {
    v.sort_by(|a, b| a.layer.cmp(&b.layer).then(a.id.cmp(&b.id)));
    v.first().copied()
}

fn build_next(n: &NodeRow, is_review: bool, _review_floor: i64) -> NextNode {
    let days_since = if n.mastery > 0 { days_since_now(n.last_review.as_deref()) } else { None };
    NextNode { node: n.clone(), is_review, days_since, effective_mastery: n.effective_mastery }
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
        "UPDATE nodes SET mastery = 0, review_count = 0, quiz_count = 0, last_review = NULL,
         stability = 1.0, next_review_at = NULL, easiness = 2.5 WHERE topic_id = ?1",
        params![tid],
    )?;
    conn.execute(
        "DELETE FROM quiz_log WHERE node_id IN (SELECT id FROM nodes WHERE topic_id = ?1)",
        params![tid],
    )?;
    conn.execute("DELETE FROM sessions WHERE topic_id = ?1", params![tid])?;
    Ok((nodes, quiz, sessions))
}

/// 记录测验结果并 EMA 更新掌握度；同时按得分档次更新稳定性 S 并设置下次复习时间。
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

    let old: (i64, i64, f64, f64) = conn.query_row(
        "SELECT mastery, review_count, stability, easiness FROM nodes WHERE id = ?1",
        params![node_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?)),
    )?;
    let new_mastery = if old.1 == 0 {
        score
    } else {
        (old.0 * 40 + score * 60) / 100
    };
    let new_stability = next_stability(old.2, score, old.3);
    let new_easiness = next_easiness(old.3, score);
    let s_int = new_stability.round() as i64;
    conn.execute(
        "UPDATE nodes SET mastery = ?1, review_count = review_count + 1,
         quiz_count = quiz_count + 1, last_review = datetime('now'),
         stability = ?2, next_review_at = datetime('now', '+' || ?3 || ' days'), easiness = ?4
         WHERE id = ?5",
        params![new_mastery, new_stability, s_int, new_easiness, node_id],
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
    let mastery: i64 = r.get(4)?;
    let stability: f64 = r.get(7).unwrap_or(S_INIT);
    let last_review: Option<String> = r.get(8)?;
    let easiness: f64 = r.get(9).unwrap_or(EF_INIT);
    let effective_mastery = compute_effective(mastery, stability, last_review.as_deref());
    Ok(NodeRow {
        id: r.get(0)?,
        name: r.get(1)?,
        summary: r.get(2)?,
        layer: r.get(3)?,
        mastery,
        review_count: r.get(5)?,
        quiz_count: r.get(6)?,
        stability,
        last_review,
        effective_mastery,
        easiness,
    })
}

pub fn snapshot(conn: &Connection, tid: i64) -> rusqlite::Result<GraphSnapshot> {
    let topic: String = conn.query_row("SELECT name FROM topics WHERE id = ?1", params![tid], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT id, name, summary, layer, mastery, review_count, quiz_count, stability, last_review, easiness
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
    pub lit_nodes: usize,             // mastery > 0（曾学过的节点数，不衰减）
    pub mastered_nodes: usize,        // effective >= 80（当前仍达标的节点数）
    pub avg_mastery: i64,             // 当前有效掌握度均值（会随遗忘下降）
    pub avg_retention: i64,           // 平均记忆保持率（0-100）
    pub quiz_total: i64,
    pub quiz_avg: i64,
    pub learn_steps: i64,
    pub today_steps: i64,
    pub weakest: Vec<(String, i64)>,  // 有效掌握度 <80 中最弱 3 个
    pub quiz_today: i64,
    pub due_reviews: usize,           // 到期该复习的节点数（effective < review_floor）
    pub due_review_names: Vec<(String, i64)>, // 到期复习清单前 5（名, effective）
}

pub fn stats(conn: &Connection, tid: i64, review_floor: i64) -> rusqlite::Result<KbStats> {
    let topic: String = conn.query_row("SELECT name FROM topics WHERE id = ?1", params![tid], |r| r.get(0))?;
    let (total, lit): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), SUM(mastery > 0) FROM nodes WHERE topic_id = ?1",
        params![tid],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
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

    let due_reviews = due_review_count(conn, tid, review_floor)?;

    // 加载全量节点（row_to_node 已计算 effective_mastery）
    let mut stmt = conn.prepare(
        "SELECT id, name, summary, layer, mastery, review_count, quiz_count, stability, last_review, easiness
         FROM nodes WHERE topic_id = ?1",
    )?;
    let all_nodes: Vec<NodeRow> = stmt.query_map(params![tid], row_to_node)?.filter_map(|r| r.ok()).collect();

    let mastered = all_nodes.iter().filter(|n| n.mastery > 0 && n.effective_mastery >= 80).count();
    let avg_eff = if all_nodes.is_empty() {
        0.0
    } else {
        all_nodes.iter().map(|n| n.effective_mastery as f64).sum::<f64>() / all_nodes.len() as f64
    };
    let avg_retention_pct = if all_nodes.is_empty() {
        0
    } else {
        let sum_r: f64 = all_nodes.iter().map(|n| {
            if n.mastery == 0 {
                0.0
            } else {
                let days = days_since_now(n.last_review.as_deref()).unwrap_or(0) as f64;
                retention(n.stability, days)
            }
        }).sum();
        ((sum_r / all_nodes.len() as f64) * 100.0).round() as i64
    };

    // 到期复习清单（effective 升序取前 5）
    let mut due_nodes: Vec<&NodeRow> = all_nodes.iter()
        .filter(|n| n.mastery > 0 && n.effective_mastery < review_floor).collect();
    due_nodes.sort_by_key(|n| n.effective_mastery);
    let due_review_names: Vec<(String, i64)> = due_nodes.iter().take(5)
        .map(|n| (n.name.clone(), n.effective_mastery)).collect();

    // 薄弱（effective <80）按 effective 升序取前 3
    let mut weak_nodes: Vec<&NodeRow> = all_nodes.iter()
        .filter(|n| n.mastery > 0 && n.effective_mastery < 80).collect();
    weak_nodes.sort_by_key(|n| n.effective_mastery);
    let weakest: Vec<(String, i64)> = weak_nodes.iter().take(3)
        .map(|n| (n.name.clone(), n.effective_mastery)).collect();

    Ok(KbStats {
        topic,
        total_nodes: total as usize,
        lit_nodes: lit as usize,
        mastered_nodes: mastered,
        avg_mastery: avg_eff.round() as i64,
        avg_retention: avg_retention_pct,
        quiz_total,
        quiz_avg: quiz_avg.round() as i64,
        learn_steps: steps,
        today_steps,
        weakest,
        quiz_today,
        due_reviews,
        due_review_names,
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
    /// 记忆半衰期（天）；v1 旧文件无此字段时默认 1.0
    #[serde(default)]
    pub stability: Option<f64>,
    /// 预计下次复习时间（信息性）；v1 旧文件无此字段时为 None
    #[serde(default)]
    pub next_review_at: Option<String>,
    /// SM-2 难度系数；v3 之前的旧文件无此字段时回退 2.5（中性）
    #[serde(default)]
    pub easiness: Option<f64>,
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
            "SELECT name, mastery, review_count, quiz_count, last_review, stability, next_review_at, easiness
             FROM nodes WHERE topic_id = ?1 ORDER BY id ASC",
        )?;
        let lnodes: Vec<LearnNodeOut> = stmt
            .query_map(params![tid], |r| {
                Ok(LearnNodeOut {
                    name: r.get(0)?,
                    mastery: r.get(1)?,
                    review_count: r.get(2)?,
                    quiz_count: r.get(3)?,
                    last_review: r.get(4)?,
                    stability: r.get(5)?,
                    next_review_at: r.get(6)?,
                    easiness: r.get(7)?,
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
                "UPDATE nodes SET mastery = ?1, review_count = ?2, quiz_count = ?3, last_review = ?4,
                 stability = COALESCE(?5, 1.0), next_review_at = ?6, easiness = COALESCE(?7, 2.5)
                 WHERE id = ?8",
                params![ln.mastery, ln.review_count, ln.quiz_count, ln.last_review, ln.stability, ln.next_review_at, ln.easiness, nid],
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
        let n = next_node(&conn, tid, 60).unwrap().unwrap();
        assert_eq!(n.node.name, "基础"); // layer 0 优先
        assert!(!n.is_review); // 未学习节点不是复习轮
    }

    #[test]
    fn test_due_review_scheduling() {
        let conn = test_db();
        let tid = sample(&conn);
        let base: i64 = conn
            .query_row("SELECT id FROM nodes WHERE topic_id = ?1 AND name = '基础'", params![tid], |r| r.get(0))
            .unwrap();
        let adv: i64 = conn
            .query_row("SELECT id FROM nodes WHERE topic_id = ?1 AND name = '进阶'", params![tid], |r| r.get(0))
            .unwrap();

        // 基础已达标（85）今日刚复习 → effective=85 > 60 未到期，应继续学新点「进阶」
        conn.execute(
            "UPDATE nodes SET mastery = 85, review_count = 1, quiz_count = 1, last_review = datetime('now'),
             stability = 1.0 WHERE id = ?1",
            params![base],
        )
        .unwrap();
        let n = next_node(&conn, tid, 60).unwrap().unwrap();
        assert_eq!(n.node.name, "进阶");
        assert!(!n.is_review);

        // 基础改为 8 天前复习 → effective = 85 × 2^(-8/1) ≈ 0.3 < 60 → 到期
        conn.execute("UPDATE nodes SET last_review = datetime('now','-8 day') WHERE id = ?1", params![base]).unwrap();
        let n = next_node(&conn, tid, 60).unwrap().unwrap();
        assert_eq!(n.node.name, "基础");
        assert!(n.is_review);
        assert!(n.days_since.unwrap() >= 8);

        // 进阶也达标（90）但复习 2 次（stability=2），12 天前复习 → effective=90×2^(-12/2)≈5.6 < 60 到期
        conn.execute(
            "UPDATE nodes SET mastery = 90, review_count = 2, quiz_count = 2, last_review = datetime('now','-12 day'),
             stability = 2.0 WHERE id = ?1",
            params![adv],
        )
        .unwrap();
        let n = next_node(&conn, tid, 60).unwrap().unwrap();
        assert_eq!(n.node.name, "基础");
        assert!(n.is_review);

        assert_eq!(due_review_count(&conn, tid, 60).unwrap(), 2);
    }

    #[test]
    fn test_stability_growth_and_decay() {
        let conn = test_db();
        let tid = sample(&conn);
        let base: i64 = conn
            .query_row("SELECT id FROM nodes WHERE topic_id = ?1 AND name = '基础'", params![tid], |r| r.get(0))
            .unwrap();
        // 用严格递增分数避开 24h 防刷分（每日最高分生效）
        // 第 1 次测验前 EF 仍为中性 2.5 → 难度乘数 1.0，与引入 EF 前行为完全一致
        let (_m, applied) = record_quiz(&conn, base, 30, "q", "a").unwrap(); // <50
        assert!(applied);
        let s: f64 = conn.query_row("SELECT stability FROM nodes WHERE id = ?1", params![base], |r| r.get(0)).unwrap();
        assert!((s - 0.7).abs() < 0.01, "<50 应 ×0.7（1.0×0.7×1.0=0.7），得 {s}");
        // 30 分 → q=2 → EF 由 2.5 降到 2.18，之后难度乘数 2.18/2.5=0.872 作用于稳定性
        let ef: f64 = conn.query_row("SELECT easiness FROM nodes WHERE id = ?1", params![base], |r| r.get(0)).unwrap();
        assert!((ef - 2.18).abs() < 1e-9, "30 分后 EF 应为 2.18，得 {ef}");

        let (_m, applied) = record_quiz(&conn, base, 80, "q", "a").unwrap(); // 70-89
        assert!(applied);
        let s: f64 = conn.query_row("SELECT stability FROM nodes WHERE id = ?1", params![base], |r| r.get(0)).unwrap();
        // 0.7 × 2.0（70-89 档）× 0.872（EF 乘数）≈ 1.2208
        assert!((s - 1.2208).abs() < 0.01, "70-89 档 ×2.0 经 EF(2.18) 调制（0.7×2.0×0.872≈1.2208），得 {s}");
        // 80 分 → q=4 → EF 不变（仍 2.18）
        let ef: f64 = conn.query_row("SELECT easiness FROM nodes WHERE id = ?1", params![base], |r| r.get(0)).unwrap();
        assert!((ef - 2.18).abs() < 1e-9, "80 分后 EF 维持 2.18，得 {ef}");

        let (_m, applied) = record_quiz(&conn, base, 95, "q", "a").unwrap(); // ≥90
        assert!(applied);
        let s: f64 = conn.query_row("SELECT stability FROM nodes WHERE id = ?1", params![base], |r| r.get(0)).unwrap();
        // 1.2208 × 2.5（≥90 档）× 0.872 ≈ 2.6613
        assert!((s - 2.6613).abs() < 0.01, "≥90 档 ×2.5 经 EF(2.18) 调制（1.2208×2.5×0.872≈2.6613），得 {s}");

        // 保持率与有效掌握度
        let r0 = retention(2.0, 0.0);
        assert!((r0 - 1.0).abs() < 0.001);
        let r1 = retention(2.0, 2.0);
        assert!((r1 - 0.5).abs() < 0.001, "半衰期 2 天，2 天后保持率应为 0.5");
        let r7 = retention(7.0, 7.0);
        assert!((r7 - 0.5).abs() < 0.001, "半衰期 7 天，7 天后保持率应为 0.5");

        // mastery=80, S=1.0, 1 天前 → effective = 80 × 2^(-1/1) = 40 < 60 到期
        conn.execute("UPDATE nodes SET mastery = 80, stability = 1.0, last_review = datetime('now','-1 day') WHERE id = ?1", params![base]).unwrap();
        assert_eq!(due_review_count(&conn, tid, 60).unwrap(), 1);
    }

    #[test]
    fn test_retention_decay() {
        // 纯函数测试：retention 公式
        assert!((retention(1.0, 0.0) - 1.0).abs() < 0.001);
        assert!((retention(1.0, 1.0) - 0.5).abs() < 0.001);
        assert!((retention(2.0, 2.0) - 0.5).abs() < 0.001);
        assert!(retention(0.0, 1.0) < 0.001);
        // 钳制（EF 中性）
        let s = next_stability(100.0, 95, EF_INIT);
        assert!(s <= S_MAX, "stability 不应超过 S_MAX");
        let s = next_stability(0.3, 30, EF_INIT);
        assert!(s >= S_MIN, "stability 不应低于 S_MIN");
    }

    #[test]
    fn test_sm2_quality_mapping() {
        assert_eq!(quality(0), 0);
        assert_eq!(quality(49), 2);  // 2.45 → 2
        assert_eq!(quality(50), 3);  // 2.5 → 3
        assert_eq!(quality(70), 4);  // 3.5 → 4
        assert_eq!(quality(89), 4);  // 4.45 → 4
        assert_eq!(quality(90), 5);  // 4.5 → 5
        assert_eq!(quality(100), 5);
    }

    #[test]
    fn test_sm2_easiness_recurrence() {
        // q=5（满分）→ +0.10；q=4 → 0；q=0（全错）→ -0.80
        assert!((next_easiness(2.5, 100) - 2.6).abs() < 1e-9);
        assert!((next_easiness(2.5, 80) - 2.5).abs() < 1e-9);
        assert!((next_easiness(2.5, 0) - 1.7).abs() < 1e-9);
        // 连续低分不击穿下界
        let mut ef = EF_INIT;
        for _ in 0..20 { ef = next_easiness(ef, 0); }
        assert!((ef - EF_MIN).abs() < 1e-9, "EF 应被钳制在 EF_MIN，实际 {ef}");
        // 连续满分不突破上界
        let mut ef = EF_INIT;
        for _ in 0..20 { ef = next_easiness(ef, 100); }
        assert!((ef - EF_MAX).abs() < 1e-9, "EF 应被钳制在 EF_MAX，实际 {ef}");
    }

    #[test]
    fn test_ef_modulates_stability() {
        // EF 中性（2.5）时乘数为 1，与引入 EF 前行为完全一致（向后兼容锚点）
        let neutral = next_stability(4.0, 100, EF_INIT);
        assert!((neutral - 4.0 * 2.5).abs() < 1e-9, "EF=2.5 时乘数应为 1，实际 {neutral}");
        // 高 EF（简单）→ 半衰期增长更快；低 EF（难）→ 更慢
        let easy = next_stability(4.0, 100, EF_MAX);
        let hard = next_stability(4.0, 100, EF_MIN);
        assert!(easy > neutral, "EF 高应使半衰期增长更快");
        assert!(hard < neutral, "EF 低应使半衰期增长更慢");
    }

    #[test]
    fn test_record_quiz_updates_easiness() {
        let conn = test_db();
        let tid = sample(&conn);
        let node_id: i64 = conn.query_row(
            "SELECT id FROM nodes WHERE topic_id = ?1 AND name = '基础'", params![tid], |r| r.get(0),
        ).unwrap();
        // 首次满分：S 用旧 EF（中性 2.5）算 → 1.0 × 2.5 × 1 = 2.5；EF 更新 2.5 → 2.6
        record_quiz(&conn, node_id, 100, "q", "a").unwrap();
        let (ef, s): (f64, f64) = conn.query_row(
            "SELECT easiness, stability FROM nodes WHERE id = ?1", params![node_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert!((ef - 2.6).abs() < 1e-9, "首次满分后 EF 应为 2.6，实际 {ef}");
        assert!((s - 2.5).abs() < 1e-9, "S 应为 1.0 × 2.5 = 2.5，实际 {s}");
    }

    #[test]
    fn test_migrate_adds_easiness() {
        let conn = test_db();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 2, "新建库应迁移到 user_version = 2");
        let tid = sample(&conn);
        let ef: f64 = conn.query_row(
            "SELECT easiness FROM nodes WHERE topic_id = ?1 AND name = '基础'", params![tid], |r| r.get(0),
        ).unwrap();
        assert!((ef - EF_INIT).abs() < 1e-9, "新节点 EF 应为中性 2.5，实际 {ef}");
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
        let s = stats(&conn, tid, 60).unwrap();
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
        let s0 = stats(&conn, tid, 60).unwrap();
        assert_eq!(s0.lit_nodes, 1);

        // 复位
        let (nodes, quiz, sessions) = reset_topic_progress(&conn, tid).unwrap();
        assert_eq!(nodes, 2); // 结构保留
        assert_eq!(quiz, 1);
        assert_eq!(sessions, 1);

        // 复位后：进度归零，结构仍在
        let s1 = stats(&conn, tid, 60).unwrap();
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
        assert_eq!(data.version, EXPORT_VERSION);
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
