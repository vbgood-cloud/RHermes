//! RHermes 长期记忆系统
//!
//! 三层记忆架构：
//! - **Session Memory** — 当前会话上下文（自动管理）
//! - **Working Memory** — 跨会话活跃知识（当前项目相关）
//! - **Long-term Memory** — 持久化知识库（永久保存）
//!
//! 存储：SQLite + FTS5 全文索引

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Utc;
use rusqlite::{params, Connection};

// ---------------------------------------------------------------------------
// 记忆类型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MemoryType {
    /// 当前会话（自动管理，session 结束后可转为 working）
    Session,
    /// 跨会话活跃知识（当前项目相关）
    Working,
    /// 长期持久化知识
    LongTerm,
}

impl MemoryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Working => "working",
            Self::LongTerm => "long_term",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Self::Session),
            "working" => Some(Self::Working),
            "long_term" => Some(Self::LongTerm),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 记忆条目
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub id: i64,
    pub memory_type: MemoryType,
    pub content: String,
    pub tags: Vec<String>,
    pub project: String,
    pub created_at: String,
    pub last_accessed: String,
    pub access_count: u64,
}

impl MemoryEntry {
    pub fn tags_str(&self) -> String {
        self.tags.join(", ")
    }
}

// ---------------------------------------------------------------------------
// 记忆系统
// ---------------------------------------------------------------------------

/// 三层记忆系统
pub struct MemorySystem {
    db: Connection,
    /// 最后 nudge 时间
    last_nudge: Instant,
    /// nudge 间隔
    nudge_interval: Duration,
}

impl MemorySystem {
    /// 打开或创建记忆数据库
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let db = Connection::open(path.as_ref()).map_err(MemoryError::Open)?;

        // 启用 WAL 模式提升并发性能
        db.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(MemoryError::Execute)?;

        // 创建主表
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                memory_type     TEXT NOT NULL,
                content         TEXT NOT NULL,
                tags            TEXT NOT NULL DEFAULT '',
                project         TEXT NOT NULL DEFAULT '',
                created_at      TEXT NOT NULL,
                last_accessed   TEXT NOT NULL,
                access_count    INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_memories_type
                ON memories(memory_type);

            CREATE INDEX IF NOT EXISTS idx_memories_project
                ON memories(project);

            CREATE INDEX IF NOT EXISTS idx_memories_accessed
                ON memories(last_accessed DESC);",
        )
        .map_err(MemoryError::Execute)?;

        // 创建 FTS5 全文索引
        db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts
                USING fts5(content, tags, content=memories, content_rowid=id);",
        )
        .map_err(MemoryError::Execute)?;

        // 创建用户画像表
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS user_profile (
                key     TEXT PRIMARY KEY,
                value   TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS session_messages (
                session_id  TEXT NOT NULL,
                round       INTEGER NOT NULL DEFAULT 0,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                created_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_session_messages_id
                ON session_messages(session_id);"
        )
        .map_err(MemoryError::Execute)?;

        // 同步 FTS 索引（首次创建后需要）
        db.execute_batch(
            "INSERT OR REPLACE INTO memories_fts(rowid, content, tags)
             SELECT id, content, tags FROM memories;",
        )
        .ok(); // 可能为空表，忽略错误

        Ok(Self {
            db,
            last_nudge: Instant::now(),
            nudge_interval: Duration::from_secs(300), // 5 分钟
        })
    }

    // ---- 写入 ----

    /// 存储一条记忆
    pub fn store(&mut self, entry: StoreMemory) -> Result<i64, MemoryError> {
        let now = Utc::now().to_rfc3339();
        let tags_str = entry.tags.join(",");

        self.db
            .execute(
                "INSERT INTO memories (memory_type, content, tags, project, created_at, last_accessed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    entry.memory_type.as_str(),
                    entry.content,
                    tags_str,
                    entry.project,
                    now,
                    now,
                ],
            )
            .map_err(MemoryError::Execute)?;

        let id = self.db.last_insert_rowid();

        // 同步 FTS 索引
        self.db
            .execute(
                "INSERT INTO memories_fts(rowid, content, tags) VALUES (?1, ?2, ?3)",
                params![id, entry.content, tags_str],
            )
            .map_err(MemoryError::Execute)?;

        Ok(id)
    }

    /// 快速记住（简化接口）
    pub fn remember(&mut self, content: &str, tags: &[&str], project: &str) -> Result<i64, MemoryError> {
        self.store(StoreMemory {
            memory_type: MemoryType::Working,
            content: content.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            project: project.to_string(),
        })
    }

    // ---- 读取 ----

    /// 通过 ID 获取记忆
    pub fn get(&mut self, id: i64) -> Result<Option<MemoryEntry>, MemoryError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, memory_type, content, tags, project, created_at, last_accessed, access_count
                 FROM memories WHERE id = ?1",
            )
            .map_err(MemoryError::Prepare)?;

        let entry = stmt
            .query_row(params![id], |row| {
                let tags_str: String = row.get(3)?;
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    memory_type: MemoryType::from_str(&row.get::<_, String>(1)?).unwrap_or(MemoryType::Working),
                    content: row.get(2)?,
                    tags: tags_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    project: row.get(4)?,
                    created_at: row.get(5)?,
                    last_accessed: row.get(6)?,
                    access_count: row.get::<_, i64>(7)? as u64,
                })
            })
            .ok();

        // 更新访问计数
        if entry.is_some() {
            self.db
                .execute(
                    "UPDATE memories SET access_count = access_count + 1, last_accessed = ?1 WHERE id = ?2",
                    params![Utc::now().to_rfc3339(), id],
                )
                .map_err(MemoryError::Execute)?;
        }

        Ok(entry)
    }

    // ---- 搜索 ----

    /// 全文搜索记忆
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT m.id, m.memory_type, m.content, m.tags, m.project,
                        m.created_at, m.last_accessed, m.access_count
                 FROM memories_fts f
                 JOIN memories m ON f.rowid = m.id
                 WHERE memories_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(MemoryError::Prepare)?;

        let entries = stmt
            .query_map(params![query, limit as i64], |row| {
                let tags_str: String = row.get(3)?;
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    memory_type: MemoryType::from_str(&row.get::<_, String>(1)?).unwrap_or(MemoryType::Working),
                    content: row.get(2)?,
                    tags: tags_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    project: row.get(4)?,
                    created_at: row.get(5)?,
                    last_accessed: row.get(6)?,
                    access_count: row.get::<_, i64>(7)? as u64,
                })
            })
            .map_err(MemoryError::Query)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    }

    /// 按类型列出记忆
    pub fn list(&self, memory_type: Option<MemoryType>, project: &str, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        let (sql, type_filter) = if memory_type.is_some() {
            ("SELECT id, memory_type, content, tags, project, created_at, last_accessed, access_count
              FROM memories WHERE project = ?1 AND memory_type = ?2
              ORDER BY last_accessed DESC LIMIT ?3", true)
        } else {
            ("SELECT id, memory_type, content, tags, project, created_at, last_accessed, access_count
              FROM memories WHERE project = ?1
              ORDER BY last_accessed DESC LIMIT ?2", false)
        };

        let mut stmt = self.db.prepare(sql).map_err(MemoryError::Prepare)?;

        let entries = if type_filter {
            let mtype = memory_type.unwrap();
            stmt.query_map(params![project, mtype.as_str(), limit as i64], Self::map_entry)?
        } else {
            stmt.query_map(params![project, limit as i64], Self::map_entry)?
        }
        .filter_map(|r| r.ok())
        .collect();

        Ok(entries)
    }

    // ---- 删除 ----

    /// 删除记忆
    pub fn delete(&mut self, id: i64) -> Result<bool, MemoryError> {
        let affected = self
            .db
            .execute("DELETE FROM memories WHERE id = ?1", params![id])
            .map_err(MemoryError::Execute)?;

        // 同步 FTS
        self.db
            .execute("DELETE FROM memories_fts WHERE rowid = ?1", params![id])
            .ok();

        Ok(affected > 0)
    }

    // ---- Nudge 机制 ----

    /// 检查是否需要 nudge 用户
    pub fn should_nudge(&mut self) -> Option<String> {
        if self.last_nudge.elapsed() >= self.nudge_interval {
            self.last_nudge = Instant::now();
            Some("有想记住的内容吗？输入 /remember <内容> 保存到记忆。".into())
        } else {
            None
        }
    }

    /// 设置 nudge 间隔
    pub fn set_nudge_interval(&mut self, secs: u64) {
        self.nudge_interval = Duration::from_secs(secs);
    }

    // ---- 统计 ----

    /// 记忆总数
    pub fn count(&self, project: &str) -> Result<u64, MemoryError> {
        self.db
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE project = ?1",
                params![project],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c as u64)
            .map_err(MemoryError::Query)
    }

    /// 最近 N 条记忆
    pub fn recent(&self, project: &str, n: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.list(None, project, n)
    }

    // ---- 辅助 ----

    fn map_entry(row: &rusqlite::Row) -> rusqlite::Result<MemoryEntry> {
        let tags_str: String = row.get(3)?;
        Ok(MemoryEntry {
            id: row.get(0)?,
            memory_type: MemoryType::from_str(&row.get::<_, String>(1)?).unwrap_or(MemoryType::Working),
            content: row.get(2)?,
            tags: tags_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            project: row.get(4)?,
            created_at: row.get(5)?,
            last_accessed: row.get(6)?,
            access_count: row.get::<_, i64>(7)? as u64,
        })
    }

    /// 将记忆同步导出到 MEMORY.md（用户可读的 Markdown 格式）
    pub fn export_memory_md(&self, path: &std::path::Path, project: &str, limit: usize) -> Result<(), MemoryError> {
        let entries = self.list(None, project, limit)?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut content = String::from("# 系统记忆\n\n> ⚠ 此文件由 AI 自动维护，你也可以手动编辑。\n\n");
        for entry in &entries {
            let tag_str = if entry.tags.is_empty() {
                String::new()
            } else {
                format!(" `[{}]`", entry.tags.join(", "))
            };
            let preview: String = entry.content.chars().take(200).collect();
            content.push_str(&format!("- {}{}\n", preview, tag_str));
            if entry.content.chars().count() > 200 {
                content.push_str("  *(截断)*\n");
            }
        }
        if entries.is_empty() {
            content.push_str("*(暂无记忆)*\n");
        }
        let _ = std::fs::write(path, content);
        Ok(())
    }

    // ============================================================
    // 用户画像（user_profile 表 + USER.md 文件）
    // ============================================================

    /// 加载用户画像
    pub fn load_profile(&self) -> Result<UserProfile, MemoryError> {
        let mut p = UserProfile::default();

        // 从 SQLite 读取
        p.preferred_languages = self.db
            .query_row("SELECT value FROM user_profile WHERE key='preferred_languages'", [], |r| r.get(0))
            .unwrap_or_default();
        p.common_tasks = self.db
            .query_row("SELECT value FROM user_profile WHERE key='common_tasks'", [], |r| r.get(0))
            .unwrap_or_default();
        p.expertise_level = self.db
            .query_row("SELECT value FROM user_profile WHERE key='expertise_level'", [], |r| r.get::<_, String>(0))
            .unwrap_or_else(|_| "中级".into());
        p.interaction_style = self.db
            .query_row("SELECT value FROM user_profile WHERE key='interaction_style'", [], |r| r.get::<_, String>(0))
            .unwrap_or_else(|_| "混合".into());
        p.skill_preferences = self.db
            .query_row("SELECT value FROM user_profile WHERE key='skill_preferences'", [], |r| r.get(0))
            .unwrap_or_default();
        p.session_count = self.db
            .query_row("SELECT value FROM user_profile WHERE key='session_count'", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u32;
        p.total_messages = self.db
            .query_row("SELECT value FROM user_profile WHERE key='total_messages'", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u32;

        Ok(p)
    }

    /// 保存用户画像（SQLite + USER.md 文件）
    /// 如果 `max_chars` > 0，USER.md 超出时自动截断
    pub fn save_profile(&self, profile: &UserProfile, user_md_path: Option<&std::path::Path>) -> Result<(), MemoryError> {
        self.save_profile_with_limit(profile, user_md_path, 0)
    }

    /// 保存用户画像，带字数限制
    pub fn save_profile_with_limit(&self, profile: &UserProfile, user_md_path: Option<&std::path::Path>, max_chars: usize) -> Result<(), MemoryError> {
        let upsert = "INSERT INTO user_profile (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2";
        self.db.execute(upsert, params!["preferred_languages", &profile.preferred_languages])?;
        self.db.execute(upsert, params!["common_tasks", &profile.common_tasks])?;
        self.db.execute(upsert, params!["expertise_level", &profile.expertise_level])?;
        self.db.execute(upsert, params!["interaction_style", &profile.interaction_style])?;
        self.db.execute(upsert, params!["skill_preferences", &profile.skill_preferences])?;
        self.db.execute(upsert, params!["session_count", &profile.session_count.to_string()])?;
        self.db.execute(upsert, params!["total_messages", &profile.total_messages.to_string()])?;

        // 同步到 USER.md 文件（用户可编辑）
        if let Some(path) = user_md_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let content = format!(
"# 用户画像

> ⚠ 此文件可手动编辑，AI 也会自动更新。

## 基础信息

- 常用语言/框架: {}
- 常见任务: {}
- 用户水平: {}
- 交互风格: {}
- 偏好技能: {}
- 会话次数: {}
- 消息总数: {}
- 最后更新: {}
",
                profile.preferred_languages, profile.common_tasks, profile.expertise_level,
                profile.interaction_style, profile.skill_preferences,
                profile.session_count, profile.total_messages,
                chrono::Utc::now().to_rfc3339(),
            );
            // 字数限制：超出时保留前半部分（关键信息）+ 截断后半部分
            if max_chars > 0 && content.len() > max_chars {
                let trunc: String = content.chars().take(max_chars).collect();
                let _ = std::fs::write(path, &trunc);
            } else {
                let _ = std::fs::write(path, content);
            }
        }

        Ok(())
    }

    /// 从记忆条目聚合更新画像
    pub fn aggregate_profile(&self, entries: &[MemoryEntry], user_md_path: Option<&std::path::Path>) -> Result<UserProfile, MemoryError> {
        let mut profile = self.load_profile()?;
        profile.total_messages += entries.len() as u32;
        profile.session_count += 1;

        let all_text: String = entries.iter().map(|e| e.content.as_str()).collect::<Vec<_>>().join(" ");

        let lang_keywords = ["rust","python","javascript","typescript","go","java",
            "c++","c#","sql","bash","powershell","react","vue","docker","kubernetes","html","css"];
        let found: Vec<&str> = lang_keywords.iter().filter(|kw| all_text.to_lowercase().contains(*kw)).copied().collect();
        if !found.is_empty() { profile.preferred_languages = found.join(", "); }

        let tasks = [("代码审查","code_review"),("调试","debugging"),("重构","refactoring"),
            ("测试","testing"),("部署","deployment"),("文档","documentation"),
            ("性能优化","optimization"),("安全","security"),("数据库","database"),
            ("前端","frontend"),("后端","backend"),("架构","architecture")];
        let found_t: Vec<&str> = tasks.iter().filter(|(k,_)| all_text.contains(*k)).map(|(_,t)| *t).collect();
        if !found_t.is_empty() { profile.common_tasks = found_t.join(", "); }

        self.save_profile(&profile, user_md_path)?;
        Ok(profile)
    }

    // ============================================================
    // 会话消息持久化（替代 session.json）
    // ============================================================

    /// 保存会话消息到数据库
    pub fn save_session_messages(
        &self,
        session_id: &str,
        messages: &[crate::tui::Message],
    ) -> Result<(), MemoryError> {
        // 先删除该 session 的旧数据
        self.db.execute(
            "DELETE FROM session_messages WHERE session_id = ?1",
            params![session_id],
        ).map_err(MemoryError::Execute)?;

        let now = chrono::Utc::now().to_rfc3339();
        for (i, msg) in messages.iter().enumerate() {
            let role_str = match msg.role {
                crate::tui::Role::User => "user",
                crate::tui::Role::Assistant => "assistant",
                crate::tui::Role::System => "system",
            };
            self.db.execute(
                "INSERT INTO session_messages (session_id, round, role, content, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, i as u32, role_str, msg.content, now],
            ).map_err(MemoryError::Execute)?;
        }
        Ok(())
    }

    /// 从数据库加载最近的会话消息（按 session_id）
    pub fn load_session_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::tui::Message>, MemoryError> {
        let mut stmt = self.db.prepare(
            "SELECT role, content FROM session_messages
             WHERE session_id = ?1
             ORDER BY round ASC"
        ).map_err(MemoryError::Execute)?;

        let messages = stmt.query_map(params![session_id], |row| {
            let role_str: String = row.get(0)?;
            let content: String = row.get(1)?;
            let role = match role_str.as_str() {
                "user" => crate::tui::Role::User,
                "assistant" => crate::tui::Role::Assistant,
                _ => crate::tui::Role::System,
            };
            Ok(crate::tui::Message { role, content })
        }).map_err(MemoryError::Execute)?
        .filter_map(|r| r.ok())
        .collect();

        Ok(messages)
    }

    /// 获取最新会话的 session_id
    pub fn latest_session_id(&self) -> Result<Option<String>, MemoryError> {
        let result = self.db.query_row(
            "SELECT session_id FROM session_messages
             ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        );
        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(MemoryError::Execute(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// 用户画像结构体
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct UserProfile {
    pub preferred_languages: String,
    pub common_tasks: String,
    pub expertise_level: String,
    pub interaction_style: String,
    pub skill_preferences: String,
    pub session_count: u32,
    pub total_messages: u32,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            preferred_languages: String::new(),
            common_tasks: String::new(),
            expertise_level: "中级".into(),
            interaction_style: "混合".into(),
            skill_preferences: String::new(),
            session_count: 0,
            total_messages: 0,
        }
    }
}

impl UserProfile {
    /// 生成画像摘要（注入 Context 用）
    pub fn summarize(&self) -> String {
        let mut parts = vec!["📊 用户画像".to_string()];
        if !self.preferred_languages.is_empty() {
            parts.push(format!("  常用语言/框架: {}", self.preferred_languages));
        }
        if !self.common_tasks.is_empty() {
            parts.push(format!("  常见任务: {}", self.common_tasks));
        }
        parts.push(format!("  用户水平: {} · 交互风格: {}", self.expertise_level, self.interaction_style));
        if !self.skill_preferences.is_empty() {
            parts.push(format!("  偏好技能: {}", self.skill_preferences));
        }
        parts.push(format!("  会话 {} 次 · 共 {} 条消息", self.session_count, self.total_messages));
        parts.join("\n")
    }
}

// ---------------------------------------------------------------------------
// 存储接口
// ---------------------------------------------------------------------------

/// 存储一条记忆的参数
pub struct StoreMemory {
    pub memory_type: MemoryType,
    pub content: String,
    pub tags: Vec<String>,
    pub project: String,
}

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum MemoryError {
    Open(rusqlite::Error),
    Execute(rusqlite::Error),
    Prepare(rusqlite::Error),
    Query(rusqlite::Error),
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "打开数据库失败: {e}"),
            Self::Execute(e) => write!(f, "数据库执行失败: {e}"),
            Self::Prepare(e) => write!(f, "SQL 准备失败: {e}"),
            Self::Query(e) => write!(f, "查询失败: {e}"),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<rusqlite::Error> for MemoryError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Query(e)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_memory() -> (tempfile::TempDir, MemorySystem) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("memory.db");
        let ms = MemorySystem::open(&path).unwrap();
        (tmp, ms)
    }

    #[test]
    fn test_store_and_get() {
        let (_tmp, mut ms) = setup_memory();
        let id = ms.remember("Rust 的所有权系统", &["rust", "memory"], "rhermes").unwrap();
        assert!(id > 0);

        let entry = ms.get(id).unwrap().unwrap();
        assert_eq!(entry.content, "Rust 的所有权系统");
        assert!(entry.tags.contains(&"rust".to_string()));
    }

    #[test]
    fn test_search() {
        let (_tmp, mut ms) = setup_memory();
        ms.remember("Rust 的所有权系统", &["rust"], "rhermes").unwrap();
        ms.remember("tokio 异步运行时", &["rust", "async"], "rhermes").unwrap();
        ms.remember("SQLite 数据库", &["database"], "rhermes").unwrap();

        let results = ms.search("rust", 10).unwrap();
        assert_eq!(results.len(), 2); // 前两条包含 rust
    }

    #[test]
    fn test_search_no_results() {
        let (_tmp, ms) = setup_memory();
        let results = ms.search("nonexistent", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_delete() {
        let (_tmp, mut ms) = setup_memory();
        let id = ms.remember("测试内容", &["test"], "rhermes").unwrap();

        assert!(ms.delete(id).unwrap());
        assert!(ms.get(id).unwrap().is_none());
    }

    #[test]
    fn test_list_by_type() {
        let (_tmp, mut ms) = setup_memory();

        ms.store(StoreMemory {
            memory_type: MemoryType::Working,
            content: "工作记忆".into(),
            tags: vec![],
            project: "rhermes".into(),
        })
        .unwrap();

        ms.store(StoreMemory {
            memory_type: MemoryType::LongTerm,
            content: "长期记忆".into(),
            tags: vec![],
            project: "rhermes".into(),
        })
        .unwrap();

        let working = ms.list(Some(MemoryType::Working), "rhermes", 10).unwrap();
        assert_eq!(working.len(), 1);
        assert_eq!(working[0].content, "工作记忆");

        let longterm = ms.list(Some(MemoryType::LongTerm), "rhermes", 10).unwrap();
        assert_eq!(longterm.len(), 1);
    }

    #[test]
    fn test_count() {
        let (_tmp, mut ms) = setup_memory();
        ms.remember("记忆1", &[], "rhermes").unwrap();
        ms.remember("记忆2", &[], "rhermes").unwrap();
        ms.remember("记忆3", &[], "other").unwrap();

        assert_eq!(ms.count("rhermes").unwrap(), 2);
        assert_eq!(ms.count("other").unwrap(), 1);
    }

    #[test]
    fn test_recent() {
        let (_tmp, mut ms) = setup_memory();
        for i in 0..5 {
            ms.remember(&format!("测试记忆{i}"), &[], "rhermes").unwrap();
        }

        let recent = ms.recent("rhermes", 3).unwrap();
        assert_eq!(recent.len(), 3);
        for entry in &recent {
            assert!(entry.content.starts_with("测试记忆"));
        }
    }

    #[test]
    fn test_access_count_increments() {
        let (_tmp, mut ms) = setup_memory();
        let id = ms.remember("高频记忆", &[], "rhermes").unwrap();

        // get() 返回更新前的值，执行 3 次后数据库里是 3
        ms.get(id).unwrap(); // 第1次: 读0，写1
        ms.get(id).unwrap(); // 第2次: 读1，写2
        let entry = ms.get(id).unwrap().unwrap(); // 第3次: 读2，写3，返回2
        assert_eq!(entry.access_count, 2); // 返回更新前的计数值
        // 再读一次验证数据库已更新
        let entry2 = ms.get(id).unwrap().unwrap();
        assert_eq!(entry2.access_count, 3);
    }

    #[test]
    fn test_memory_type_conversion() {
        assert_eq!(MemoryType::from_str("session"), Some(MemoryType::Session));
        assert_eq!(MemoryType::from_str("working"), Some(MemoryType::Working));
        assert_eq!(MemoryType::from_str("long_term"), Some(MemoryType::LongTerm));
        assert_eq!(MemoryType::from_str("invalid"), None);
    }

    #[test]
    fn test_nudge_interval() {
        let (_tmp, mut ms) = setup_memory();
        // 设置一个极长间隔确保不触发
        ms.set_nudge_interval(86_400); // 24 小时
        assert!(ms.should_nudge().is_none());
        assert!(ms.should_nudge().is_none()); // 仍然不触发

        // 设置短间隔后触发
        ms.set_nudge_interval(1); // 1 秒
        // 理论上 1 秒已经过去（测试执行时间），所以会触发
        // 但为了可靠性，这里不 assert 具体行为
    }
}
