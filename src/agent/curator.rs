//! RHermes Curator — 自治技能生命周期管理
//!
//! ## 状态机
//!
//! ```text
//! active ──30天未用──→ stale ──90天未用──→ archived
//!   ↑                                        │
//!   └────────── 重新使用 ──────────────────────┘
//! ```
//!
//! ## 触发方式
//!
//! 启动时检查：
//! - 上次运行 > 7 天
//! - agent 闲置 > 2 小时（暂不实现，仅启动时触发）
//!
//! ## LLM Review Pass
//!
//! 扫描同前缀/同域的 skill 群，合并到 umbrella skill。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;

use crate::core::Config;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 技能从 active → stale 的天数
const STALE_DAYS: i64 = 30;

/// 技能从 stale → archived 的天数
const ARCHIVE_DAYS: i64 = 90;

/// 上次 curator 运行的标记文件名
const CURATOR_MARKER: &str = ".curator_last_run";

/// 归档目录名
const ARCHIVE_DIR: &str = "_archived";

/// Umbrella 技能分类
#[derive(Debug, Clone, PartialEq)]
pub enum UmbrellaAction {
    /// 内容被合并进 umbrella，归档但保留
    Consolidated,
    /// 纯过期，直接归档
    Pruned,
}

/// 技能生命周期状态
#[derive(Debug, Clone, PartialEq)]
pub enum SkillStatus {
    Active,
    Stale,
    Archived,
}

// ---------------------------------------------------------------------------
// Curator
// ---------------------------------------------------------------------------

/// 技能生命周期管理器
pub struct Curator {
    /// 技能目录
    skills_dir: PathBuf,
    /// 全局配置
    config: Config,
    /// 创建时间（用于测量运行耗时）
    created_at: Instant,
}

impl Curator {
    /// 创建新的 Curator
    pub fn new(skills_dir: PathBuf, config: Config) -> Self {
        Self {
            skills_dir,
            config,
            created_at: Instant::now(),
        }
    }

    /// 运行完整的 curator 检查流程
    pub fn run(&self) -> CuratorReport {
        let start = Instant::now();
        let mut report = CuratorReport::new();

        // 1. 检查是否需要运行
        if !self.should_run() {
            report.message = "跳过：上次运行不足 7 天".into();
            return report;
        }

        // 2. 创建快照
        match self.snapshot() {
            Ok(snap) => report.snapshot_path = Some(snap),
            Err(e) => {
                report.errors.push(format!("快照失败: {e}"));
                return report;
            }
        }

        // 3. 扫描技能状态
        let states = self.scan_skill_states();

        // 4. 归档过期的技能
        for (name, skill_path, status) in &states {
            match status {
                SkillStatus::Archived => {
                    match self.archive_skill(name, skill_path) {
                        Ok(()) => report.archived.push(name.clone()),
                        Err(e) => report.errors.push(format!("归档「{name}」失败: {e}")),
                    }
                }
                SkillStatus::Stale => {
                    match self.mark_stale(name, skill_path) {
                        Ok(()) => report.stale.push(name.clone()),
                        Err(e) => report.errors.push(format!("标记「{name}」过期失败: {e}")),
                    }
                }
                SkillStatus::Active => {}
            }
        }

        // 5. 写入运行标记
        self.write_marker();

        report.message = format!(
            "已检查 {} 个技能 · {} 个归档 · {} 个标记过期",
            states.len(),
            report.archived.len(),
            report.stale.len(),
        );
        report.duration_ms = start.elapsed().as_millis() as u64;
        report
    }

    /// 判断是否应该运行 curator
    fn should_run(&self) -> bool {
        let marker_path = self.skills_dir.join(CURATOR_MARKER);
        if !marker_path.exists() {
            return true; // 从未运行过
        }
        let content = match fs::read_to_string(&marker_path) {
            Ok(c) => c,
            Err(_) => return true,
        };
        let last_run = match content.trim().parse::<i64>() {
            Ok(ts) => ts,
            Err(_) => return true,
        };
        let now = Utc::now().timestamp();
        // 超过 7 天未运行
        now - last_run > 7 * 24 * 3600
    }

    /// 写入运行标记
    fn write_marker(&self) {
        let marker_path = self.skills_dir.join(CURATOR_MARKER);
        let now = Utc::now().timestamp().to_string();
        let _ = fs::write(&marker_path, &now);
    }

    /// 扫描所有技能的生命周期状态
    fn scan_skill_states(&self) -> Vec<(String, PathBuf, SkillStatus)> {
        let _now = Utc::now();
        let mut results = Vec::new();

        // 递归扫描技能文件
        let entries = match self.collect_skill_files(&self.skills_dir) {
            Ok(e) => e,
            Err(_) => return results,
        };

        for (name, path) in &entries {
            // 不在归档目录中的技能才检查
            if path.to_string_lossy().contains(&format!("/{}/", ARCHIVE_DIR))
                || path.to_string_lossy().contains(&format!("\\{}\\", ARCHIVE_DIR))
            {
                continue;
            }

            // 从 .usage.json 读取 telemetry
            let telemetry = crate::agent::UsageTelemetry::load(path);

            // 跳过钉住的技能
            if telemetry.pinned {
                continue;
            }

            let status = match telemetry.days_since_last_used() {
                Some(days) if days >= ARCHIVE_DAYS => SkillStatus::Archived,
                Some(days) if days >= STALE_DAYS => SkillStatus::Stale,
                _ => SkillStatus::Active,
            };

            results.push((name.clone(), path.clone(), status));
        }

        results
    }

    /// 将技能归档（移到 _archived 目录）
    fn archive_skill(&self, name: &str, path: &Path) -> Result<(), String> {
        let archive_dir = self.skills_dir.join(ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir).map_err(|e| e.to_string())?;

        let dest = archive_dir.join(format!("{name}.md"));
        // 用 rename 移动文件
        fs::rename(path, &dest).map_err(|e| format!("移动文件失败: {e}"))?;

        // 在源位置留下标记文件
        let marker = format!("# 此技能已归档到 _archived/{name}.md\n# 如需恢复，移回 skills/ 目录即可\n");
        let _ = fs::write(path.with_extension("md.archived"), &marker);

        Ok(())
    }

    /// 标记技能为过期（在 frontmatter 中添加 status: stale）
    fn mark_stale(&self, _name: &str, path: &Path) -> Result<(), String> {
        let content = fs::read_to_string(path).map_err(|e| e.to_string())?;

        if content.contains("status: stale") || content.contains("status: archived") {
            return Ok(()); // 已经标记过
        }

        // 在 frontmatter 中添加 status: stale
        let new_content = if content.starts_with("---") {
            // frontmatter 存在，在 --- 后插入 status
            if let Some(end) = content[3..].find("\n---") {
                let before = &content[..3 + end];
                let after = &content[3 + end..];
                format!("{before}\nstatus: stale{after}")
            } else {
                content
            }
        } else {
            format!("---\nstatus: stale\n---\n\n{content}")
        };

        fs::write(path, &new_content).map_err(|e| e.to_string())
    }

    /// 创建快照（复制到时间戳目录）
    fn snapshot(&self) -> Result<String, String> {
        let snapshot_dir = self.skills_dir.join("_snapshots");
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let snap_path = snapshot_dir.join(format!("skills_{timestamp}"));
        fs::create_dir_all(&snap_path).map_err(|e| e.to_string())?;

        let files = self.collect_skill_files(&self.skills_dir)?;
        for (name, path) in &files {
            let dest = snap_path.join(format!("{name}.md"));
            fs::copy(path, &dest).map_err(|e| format!("复制 {name} 失败: {e}"))?;
        }

        Ok(snap_path.to_string_lossy().to_string())
    }

    // ---- 辅助方法 ----

    /// 递归收集技能文件
    fn collect_skill_files(&self, dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
        let mut files = Vec::new();
        if !dir.is_dir() {
            return Ok(files);
        }
        let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                // 跳过归档目录和快照目录
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name == ARCHIVE_DIR || name == "_snapshots" || name.starts_with('.') {
                    continue;
                }
                let sub = self.collect_skill_files(&path)?;
                files.extend(sub);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md")
                && !path.extension().and_then(|e| e.to_str()).map_or(false, |e| e == "archived")
            {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    files.push((stem.to_string(), path));
                }
            }
        }
        Ok(files)
    }


}

// ---------------------------------------------------------------------------
// Curator 报告
// ---------------------------------------------------------------------------

/// Curator 运行报告
#[derive(Debug, Clone)]
pub struct CuratorReport {
    /// 摘要消息
    pub message: String,
    /// 已归档的技能
    pub archived: Vec<String>,
    /// 已标记过期的技能
    pub stale: Vec<String>,
    /// 错误列表
    pub errors: Vec<String>,
    /// 快照路径
    pub snapshot_path: Option<String>,
    /// 耗时
    pub duration_ms: u64,
}

impl CuratorReport {
    fn new() -> Self {
        Self {
            message: String::new(),
            archived: Vec::new(),
            stale: Vec::new(),
            errors: Vec::new(),
            snapshot_path: None,
            duration_ms: 0,
        }
    }

    /// 是否成功（无错误）
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }

    /// 格式化输出
    pub fn format(&self) -> String {
        let mut out = format!("🧹 Curator 执行完毕 ({:.1}s)\n", self.duration_ms as f64 / 1000.0);
        out.push_str(&format!("   {}\n", self.message));

        if !self.archived.is_empty() {
            out.push_str(&format!("   📦 已归档: {}\n", self.archived.join(", ")));
        }
        if !self.stale.is_empty() {
            out.push_str(&format!("   ⏳ 已标记过期: {}\n", self.stale.join(", ")));
        }
        if let Some(ref snap) = self.snapshot_path {
            out.push_str(&format!("   💾 快照: {snap}\n"));
        }
        if !self.errors.is_empty() {
            for err in &self.errors {
                out.push_str(&format!("   ⚠ {err}\n"));
            }
        }
        out
    }
}



// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_skills_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        (tmp, skills_dir)
    }

    fn create_skill(dir: &Path, name: &str, last_used_days_ago: Option<i64>) {
        let md_path = dir.join(format!("{name}.md"));
        let content = format!("---\ndescription: \"{name}\"\nrun_as: subagent\n---\n\n# {name}\n\nBody\n");
        fs::write(&md_path, &content).unwrap();

        // 同时写入 .usage.json sidecar
        let usage = crate::agent::UsageTelemetry {
            use_count: if last_used_days_ago.is_some() { 5 } else { 0 },
            view_count: 0,
            patch_count: 0,
            last_used_at: last_used_days_ago.map(|days| {
                (Utc::now() - chrono::Duration::days(days)).to_rfc3339()
            }),
            created_at: Some(Utc::now().to_rfc3339()),
            archived_at: None,
            pinned: false,
        };
        let usage_path = dir.join(format!("{name}.usage.json"));
        fs::write(&usage_path, serde_json::to_string_pretty(&usage).unwrap()).unwrap();
    }

    #[test]
    fn test_should_run_first_time() {
        let (_tmp, skills_dir) = setup_skills_dir();
        let config = Config::default();
        let curator = Curator::new(skills_dir, config);
        assert!(curator.should_run());
    }

    #[test]
    fn test_should_run_after_7_days() {
        let (_tmp, skills_dir) = setup_skills_dir();
        let marker_path = skills_dir.join(CURATOR_MARKER);
        let old_ts = (Utc::now() - chrono::Duration::days(8)).timestamp().to_string();
        fs::write(&marker_path, &old_ts).unwrap();

        let config = Config::default();
        let curator = Curator::new(skills_dir, config);
        assert!(curator.should_run());
    }

    #[test]
    fn test_should_not_run_recently() {
        let (_tmp, skills_dir) = setup_skills_dir();
        let marker_path = skills_dir.join(CURATOR_MARKER);
        let recent_ts = Utc::now().timestamp().to_string();
        fs::write(&marker_path, &recent_ts).unwrap();

        let config = Config::default();
        let curator = Curator::new(skills_dir, config);
        assert!(!curator.should_run());
    }

    #[test]
    fn test_scan_skill_states() {
        let (_tmp, skills_dir) = setup_skills_dir();
        // 活跃技能
        create_skill(&skills_dir, "active-skill", Some(1));
        // 过期技能（35 天）
        create_skill(&skills_dir, "stale-skill", Some(35));
        // 归档技能（95 天）
        create_skill(&skills_dir, "archive-skill", Some(95));
        // 无 last_used
        create_skill(&skills_dir, "no-usage", None);

        let config = Config::default();
        let curator = Curator::new(skills_dir, config);
        let states = curator.scan_skill_states();

        assert_eq!(states.len(), 4);

        for (name, _, status) in &states {
            match name.as_str() {
                "active-skill" | "no-usage" => assert_eq!(*status, SkillStatus::Active),
                "stale-skill" => assert_eq!(*status, SkillStatus::Stale),
                "archive-skill" => assert_eq!(*status, SkillStatus::Archived),
                _ => panic!("未知技能: {name}"),
            }
        }
    }

    #[test]
    fn test_archive_skill() {
        let (_tmp, skills_dir) = setup_skills_dir();
        create_skill(&skills_dir, "test-skill", Some(100));

        let config = Config::default();
        let path = skills_dir.join("test-skill.md");
        let curator = Curator::new(skills_dir.clone(), config);
        curator.archive_skill("test-skill", &path).unwrap();

        // 原文件应该被移动
        assert!(!path.exists());
        // 归档目录中应该有
        let archive_dir = skills_dir.join(ARCHIVE_DIR);
        assert!(archive_dir.join("test-skill.md").exists());
    }

    #[test]
    fn test_mark_stale() {
        let (_tmp, skills_dir) = setup_skills_dir();
        create_skill(&skills_dir, "test-stale", Some(40));

        let config = Config::default();
        let path = skills_dir.join("test-stale.md");
        let curator = Curator::new(skills_dir.clone(), config);
        curator.mark_stale("test-stale", &path).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("status: stale"));
    }

    #[test]
    fn test_full_run() {
        let (_tmp, skills_dir) = setup_skills_dir();
        create_skill(&skills_dir, "active", Some(1));
        create_skill(&skills_dir, "old", Some(100));

        let config = Config::default();
        let curator = Curator::new(skills_dir, config);
        let report = curator.run();

        assert!(report.archived.contains(&"old".to_string()));
        assert!(report.archived.len() == 1);
        assert!(report.is_success());
    }


}
