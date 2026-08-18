//! 知识库学习系统 — 模块入口
//!
//! 组成：
//!   store  — SQLite 数据层（topics/nodes/edges/quiz_log/sessions/question_bank）
//!   layout — 拓扑分层 + 坐标布局
//!   svg    — 浏览器图谱（主视图，四档配色）
//!   bento  — 学习战绩面板（终端 + HTML）
//!
//! 工具入口在 builtin.rs：kb_create / kb_graph / kb_learn / kb_quiz / kb_stats / kb_list

pub mod bento;
pub mod layout;
pub mod store;
pub mod svg;

use std::path::PathBuf;

/// 知识库数据库路径（data_root/knowledge/kb.db）
pub fn db_path() -> PathBuf {
    crate::core::PathManager::detect().data_root().join("knowledge").join("kb.db")
}

/// 打开数据库（惰性建目录+schema）
pub fn open_db() -> rusqlite::Result<rusqlite::Connection> {
    store::open(&db_path())
}

/// 图谱 HTML 输出目录（data_root/knowledge/graphs/）
pub fn graphs_dir() -> PathBuf {
    let d = crate::core::PathManager::detect().data_root().join("knowledge").join("graphs");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Bento HTML 输出目录
pub fn bento_dir() -> PathBuf {
    let d = crate::core::PathManager::detect().data_root().join("knowledge").join("bento");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// 工具注册（在 builtin_registry 中调用）
pub fn kb_tools() -> Vec<std::sync::Arc<dyn crate::tools::Tool>> {
    vec![
        std::sync::Arc::new(crate::tools::KbCreate),
        std::sync::Arc::new(crate::tools::KbGraph),
        std::sync::Arc::new(crate::tools::KbLearn),
        std::sync::Arc::new(crate::tools::KbQuiz),
        std::sync::Arc::new(crate::tools::KbStatsTool),
        std::sync::Arc::new(crate::tools::KbList),
    ]
}
