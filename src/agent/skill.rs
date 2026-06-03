//! RHermes 技能系统
//!
//! 基于 Markdown playbook 的可复用技能引擎，兼容 agentskills.io 格式。
//!
//! ## 功能
//! - Markdown 格式技能定义（含 YAML frontmatter）
//! - inline / subagent 两种运行模式
//! - 自动统计：使用次数、成功率、平均耗时
//! - 进化建议：基于使用数据自动优化

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 运行模式
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum RunAs {
    /// body 追加到父级日志中执行
    Inline,
    /// 在隔离的子 Agent 中执行
    Subagent,
}

impl RunAs {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Subagent => "subagent",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "subagent" => Self::Subagent,
            _ => Self::Inline,
        }
    }
}

// ---------------------------------------------------------------------------
// Usage Telemetry — 使用量驱动进化
// ---------------------------------------------------------------------------

/// 技能的 .usage.json sidecar 数据
///
/// 与 .md 文件同目录存放，原子写入，best-effort 不破坏主流程。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageTelemetry {
    /// 执行调用次数
    pub use_count: u64,
    /// 被查看/列举次数
    pub view_count: u64,
    /// 被打补丁次数
    pub patch_count: u64,
    /// 上次使用时间（RFC3339）
    pub last_used_at: Option<String>,
    /// 创建时间（RFC3339）
    pub created_at: Option<String>,
    /// 归档时间（RFC3339）
    pub archived_at: Option<String>,
}

impl UsageTelemetry {
    /// 创建新的 telemetry（首次创建技能时）
    pub fn new() -> Self {
        Self {
            use_count: 0,
            view_count: 0,
            patch_count: 0,
            last_used_at: None,
            created_at: Some(Utc::now().to_rfc3339()),
            archived_at: None,
        }
    }

    /// 加载 .usage.json sidecar
    pub fn load(skill_path: &Path) -> Self {
        let path = usage_file_path(skill_path);
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| {
                // 兼容旧数据：从 .md 文件注解中迁移
                let content = fs::read_to_string(skill_path).unwrap_or_default();
                let mut t = Self::new();
                for line in content.lines() {
                    let line = line.trim();
                    if let Some(v) = line.strip_prefix("# use_count: ") {
                        t.use_count = v.trim().parse().unwrap_or(0);
                    }
                    if let Some(v) = line.strip_prefix("# last_used: ") {
                        t.last_used_at = Some(v.trim().to_string());
                    }
                }
                t
            })
    }

    /// 保存 .usage.json（best-effort）
    pub fn save(&self, skill_path: &Path) {
        let path = usage_file_path(skill_path);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(&path, &json);
        }
    }

    /// 获取最后使用距今的天数
    pub fn days_since_last_used(&self) -> Option<i64> {
        self.last_used_at.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| (Utc::now() - dt.with_timezone(&Utc)).num_days())
        })
    }
}

/// 获取 .usage.json 路径（与 .md 同目录同文件名）
fn usage_file_path(skill_path: &Path) -> PathBuf {
    let parent = skill_path.parent().unwrap_or(Path::new("."));
    let stem = skill_path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
    parent.join(format!("{stem}.usage.json"))
}

// ---------------------------------------------------------------------------
// 技能定义
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Skill {
    /// 技能名称（标识符）
    pub name: String,
    /// 分类目录（如 "analysis" "utils"，空 = 根目录）
    pub category: Option<String>,
    /// 一句话描述
    pub description: String,
    /// Markdown 正文
    pub body: String,
    /// 运行模式
    pub run_as: RunAs,
    /// 允许使用的工具列表（空 = 全部允许）
    pub allowed_tools: Vec<String>,
    /// 子 Agent 模型覆盖（仅 subagent 模式有效）
    pub model: Option<String>,

    // ---- 进化数据 ----
    /// 使用次数
    pub use_count: u64,
    /// 成功次数
    pub success_count: u64,
    /// 失败次数
    pub fail_count: u64,
    /// 上次使用时间
    pub last_used: Option<String>,
    /// 累计耗时（毫秒）
    pub total_duration_ms: u64,
}

impl Skill {
    /// 成功率 (0.0 ~ 1.0)
    pub fn success_rate(&self) -> f64 {
        let total = self.use_count;
        if total == 0 {
            return 0.0;
        }
        self.success_count as f64 / total as f64
    }

    /// 平均耗时（毫秒）
    pub fn avg_duration_ms(&self) -> u64 {
        if self.use_count == 0 {
            return 0;
        }
        self.total_duration_ms / self.use_count
    }

    /// 是否健康（使用次数 > 0 且成功率 > 0.5）
    pub fn is_healthy(&self) -> bool {
        self.use_count > 0 && self.success_rate() > 0.5
    }

    /// 是否需要关注（使用次数少或成功率低）
    pub fn needs_attention(&self) -> bool {
        self.use_count > 0 && self.success_rate() < 0.3
    }

    /// 执行技能
    pub async fn run(&self, arguments: &str) -> crate::agent::SubAgentResult {
        let config = crate::core::Config::load(Path::new(""))
            .unwrap_or_default();
        let context = format!("{}\n\n## 任务\n{}", self.body, arguments);
        crate::agent::run_sub_agent(&context, "", &config).await
    }
}

// ---------------------------------------------------------------------------
// 技能引擎
// ---------------------------------------------------------------------------

/// 获取技能文件路径（按目录分类）
fn skill_file_path(base: &Path, name: &str, category: Option<&str>) -> PathBuf {
    if let Some(cat) = category {
        base.join(cat).join(format!("{name}.md"))
    } else {
        base.join(format!("{name}.md"))
    }
}

/// 技能引擎 — 管理技能的加载、创建、执行统计和进化
pub struct SkillEngine {
    /// 技能目录路径
    dir: PathBuf,
    /// 技能缓存（name → Skill）
    skills: HashMap<String, Skill>,
}

impl SkillEngine {
    /// 从指定目录加载所有技能
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, SkillError> {
        let dir = dir.as_ref().to_path_buf();

        // 确保目录存在
        fs::create_dir_all(&dir).map_err(SkillError::Io)?;

        let mut engine = Self {
            dir,
            skills: HashMap::new(),
        };
        engine.reload()?;
        Ok(engine)
    }

    /// 重新加载所有技能（从磁盘，递归扫描子目录）
    pub fn reload(&mut self) -> Result<(), SkillError> {
        self.skills.clear();
        let dir = self.dir.clone();
        if !dir.exists() {
            return Ok(());
        }
        self.load_dir_recursive(&dir)?;
        Ok(())
    }

    /// 递归加载目录下的所有 .md 技能文件
    fn load_dir_recursive(&mut self, dir: &Path) -> Result<(), SkillError> {
        if !dir.is_dir() {
            return Ok(());
        }
        let entries = fs::read_dir(dir).map_err(SkillError::Io)?;
        for entry in entries {
            let entry = entry.map_err(SkillError::Io)?;
            let path = entry.path();
            if path.is_dir() {
                self.load_dir_recursive(&path)?;
                continue;
            }
            // 只处理 .md 文件
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match Self::load_skill_file(&path) {
                Ok(skill) => {
                    self.skills.insert(skill.name.clone(), skill);
                }
                Err(e) => {
                    eprintln!("[Skill] 跳过无效技能文件 {}: {e}", path.display());
                }
            }
        }
        Ok(())
    }

    // ---- 读取 ----

    /// 获取技能
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// 获取可变引用
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Skill> {
        self.skills.get_mut(name)
    }

    /// 更新技能内容（打补丁方式，只覆盖指定字段）
    pub fn update_skill(
        &mut self,
        name: &str,
        new_description: Option<&str>,
        new_body: Option<&str>,
        new_allowed_tools: Option<&[String]>,
        new_model: Option<&str>,
    ) -> Result<(), SkillError> {
        let key = name.to_string();
        if !self.skills.contains_key(&key) {
            return Err(SkillError::NotFound(name.into()));
        }

        // 克隆旧数据，修改后写回
        let mut skill = self.skills[&key].clone();
        if let Some(desc) = new_description {
            skill.description = desc.to_string();
        }
        if let Some(body) = new_body {
            skill.body = body.to_string();
        }
        if let Some(tools) = new_allowed_tools {
            skill.allowed_tools = tools.to_vec();
        }
        if let Some(model) = new_model {
            let m = model.to_string();
            skill.model = if m.is_empty() { None } else { Some(m) };
        }

        // 写回磁盘
        self.save_skill_to_disk(&skill)?;
        self.skills.insert(key, skill);
        Ok(())
    }

    /// 列出所有技能
    pub fn list(&self) -> Vec<&Skill> {
        let mut skills: Vec<&Skill> = self.skills.values().collect();
        skills.sort_by(|a, b| b.use_count.cmp(&a.use_count));
        skills
    }

    /// 列出并记录查看
    pub fn list_and_record(&mut self, viewer: &str) -> Vec<&Skill> {
        let names: Vec<String> = self.skills.keys().cloned().collect();
        for name in &names {
            self.record_view(name);
        }
        self.list()
    }

    /// 搜索技能
    pub fn search(&self, query: &str) -> Vec<&Skill> {
        let q = query.to_lowercase();
        self.skills
            .values()
            .filter(|s| {
                s.name.to_lowercase().contains(&q)
                    || s.description.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// 技能数量
    pub fn count(&self) -> usize {
        self.skills.len()
    }

    // ---- 创建 ----

    /// 创建新技能
    pub fn create(
        &mut self,
        name: &str,
        description: &str,
        body: &str,
        run_as: RunAs,
    ) -> Result<&Skill, SkillError> {
        self.create_with_category(name, None, description, body, run_as)
    }

    /// 创建新技能（指定目录分类）
    pub fn create_with_category(
        &mut self,
        name: &str,
        category: Option<&str>,
        description: &str,
        body: &str,
        run_as: RunAs,
    ) -> Result<&Skill, SkillError> {
        // 名称校验
        if name.is_empty() {
            return Err(SkillError::InvalidName("技能名称不能为空".into()));
        }
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return Err(SkillError::InvalidName(
                "技能名称只能包含字母、数字、下划线和连字符".into(),
            ));
        }
        if self.skills.contains_key(name) {
            return Err(SkillError::AlreadyExists(name.into()));
        }
        // 分类校验
        if let Some(cat) = category {
            if !cat.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                return Err(SkillError::InvalidName("分类名称只能包含字母、数字、下划线和连字符".into()));
            }
        }

        let skill = Skill {
            name: name.to_string(),
            category: category.map(|s| s.to_string()),
            description: description.to_string(),
            body: body.to_string(),
            run_as,
            allowed_tools: Vec::new(),
            model: None,
            use_count: 0,
            success_count: 0,
            fail_count: 0,
            last_used: None,
            total_duration_ms: 0,
        };

        // 写入文件
        self.save_skill_to_disk(&skill)?;

        self.skills.insert(skill.name.clone(), skill);
        Ok(self.skills.get(name).unwrap())
    }

    // ---- 删除 ----

    /// 删除技能
    pub fn delete(&mut self, name: &str) -> Result<bool, SkillError> {
        let skill = match self.skills.get(name) {
            Some(s) => s.clone(),
            None => return Ok(false),
        };
        let file_path = skill_file_path(&self.dir, &skill.name, skill.category.as_deref());
        if file_path.exists() {
            fs::remove_file(&file_path).map_err(SkillError::Io)?;
        }
        self.skills.remove(name);
        Ok(true)
    }

    // ---- 使用记录 ----

    /// 记录技能使用情况
    pub fn record_usage(
        &mut self,
        name: &str,
        success: bool,
        duration_ms: u64,
    ) -> Result<(), SkillError> {
        let sk_path = {
            let skill = self
                .skills
                .get_mut(name)
                .ok_or_else(|| SkillError::NotFound(name.into()))?;

            skill.use_count += 1;
            if success {
                skill.success_count += 1;
            } else {
                skill.fail_count += 1;
            }
            skill.total_duration_ms += duration_ms;
            skill.last_used = Some(Utc::now().to_rfc3339());

            skill_file_path(&self.dir, &skill.name, skill.category.as_deref())
        };

        // 更新磁盘文件（在可变借用释放后执行）
        if let Some(skill) = self.skills.get(name) {
            // 更新 .md
            self.save_skill_to_disk(skill)?;
            // 更新 .usage.json sidecar
            let mut telemetry = UsageTelemetry::load(&sk_path);
            telemetry.use_count = skill.use_count;
            telemetry.last_used_at = skill.last_used.clone();
            telemetry.save(&sk_path);
        }
        Ok(())
    }

    /// 记录技能被查看
    pub fn record_view(&mut self, name: &str) {
        let sk_path = if let Some(skill) = self.skills.get_mut(name) {
            skill_file_path(&self.dir, &skill.name, skill.category.as_deref())
        } else {
            return;
        };
        let mut telemetry = UsageTelemetry::load(&sk_path);
        telemetry.view_count += 1;
        telemetry.save(&sk_path);
    }

    /// 记录技能被更新（补丁）
    pub fn record_patch(&mut self, name: &str) {
        let sk_path = if let Some(skill) = self.skills.get_mut(name) {
            skill_file_path(&self.dir, &skill.name, skill.category.as_deref())
        } else {
            return;
        };
        let mut telemetry = UsageTelemetry::load(&sk_path);
        telemetry.patch_count += 1;
        telemetry.save(&sk_path);
    }

    // ---- 进化建议 ----

    /// 获取进化建议
    pub fn suggest_optimizations(&self) -> Vec<String> {
        let mut suggestions = Vec::new();

        for skill in self.skills.values() {
            if skill.use_count == 0 {
                suggestions.push(format!("📌 「{}」从未使用，考虑删除", skill.name));
            } else if skill.needs_attention() {
                suggestions.push(format!(
                    "⚠️ 「{}」成功率仅 {:.0}%，需要优化",
                    skill.name,
                    skill.success_rate() * 100.0
                ));
            } else if skill.use_count > 10 && skill.success_rate() > 0.95 {
                suggestions.push(format!(
                    "⭐ 「{}」表现优异 (成功率 {:.0}%)，可以推广",
                    skill.name,
                    skill.success_rate() * 100.0
                ));
            }
        }

        if suggestions.is_empty() {
            suggestions.push("✅ 所有技能运行正常，无需优化".into());
        }

        suggestions
    }

    // ---- 磁盘读写 ----

    fn load_skill_file(path: &Path) -> Result<Skill, SkillError> {
        let content = fs::read_to_string(path).map_err(SkillError::Io)?;
        Self::parse_skill_markdown(&content, path)
    }

    fn parse_skill_markdown(content: &str, path: &Path) -> Result<Skill, SkillError> {
        // 提取文件名作为默认名称
        let default_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();

        // 解析 YAML frontmatter (--- ... ---)
        let content = content.trim();
        let (frontmatter, body) = if content.starts_with("---") {
            if let Some(end) = content[3..].find("\n---") {
                let fm = &content[3..3 + end];
                let rest = content[3 + end + 4..].trim();
                (Some(fm), rest)
            } else {
                (None, content)
            }
        } else {
            (None, content)
        };

        let mut name = default_name;
        let mut category: Option<String> = None;
        let mut description = String::new();
        let mut run_as = RunAs::Inline;
        let mut allowed_tools: Vec<String> = Vec::new();
        let mut model: Option<String> = None;

        if let Some(fm) = frontmatter {
            for line in fm.lines() {
                let line = line.trim();
                if let Some((key, value)) = line.split_once(':') {
                    let key = key.trim();
                    let value = value.trim().trim_matches('"');

                    match key {
                        "name" => name = value.to_string(),
                        "category" => {
                            if !value.is_empty() && value != "null" {
                                category = Some(value.to_string());
                            }
                        }
                        "description" => description = value.to_string(),
                        "run_as" => run_as = RunAs::from_str(value),
                        "model" => model = Some(value.to_string()),
                        "allowed_tools" => {
                            allowed_tools = value
                                .split(',')
                                .map(|s| s.trim().trim_matches('[').trim_matches(']').trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                        }
                        _ => {}
                    }
                }
            }
        }

        // 第一行非空行作为描述的后备
        if description.is_empty() {
            description = body.lines().find(|l| !l.is_empty()).unwrap_or("").to_string();
        }

        // 从路径推断分类（如果 frontmatter 没指定）
        if category.is_none() {
            if let Some(parent) = path.parent() {
                let skills_dir = Path::new("skills");
                if parent != skills_dir && parent.file_name().is_some() {
                    category = parent.file_name().and_then(|s| s.to_str()).map(|s| s.to_string());
                }
            }
        }

        Ok(Skill {
            name,
            category,
            description,
            body: body.to_string(),
            run_as,
            allowed_tools,
            model,
            use_count: 0,
            success_count: 0,
            fail_count: 0,
            last_used: None,
            total_duration_ms: 0,
        })
    }

    fn save_skill_to_disk(&self, skill: &Skill) -> Result<(), SkillError> {
        let file_path = skill_file_path(&self.dir, &skill.name, skill.category.as_deref());

        // 确保父目录存在
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).map_err(SkillError::Io)?;
        }

        // 构建 frontmatter
        let mut fm = String::new();
        fm.push_str(&format!("description: \"{}\"\n", skill.description));
        if let Some(ref cat) = skill.category {
            fm.push_str(&format!("category: {cat}\n"));
        }
        fm.push_str(&format!("run_as: {}\n", skill.run_as.as_str()));

        if !skill.allowed_tools.is_empty() {
            fm.push_str(&format!(
                "allowed_tools: [{}]\n",
                skill.allowed_tools.join(", ")
            ));
        }
        if let Some(ref model) = skill.model {
            fm.push_str(&format!("model: {model}\n"));
        }

        // 进化数据以注释形式写入
        fm.push_str(&format!("# use_count: {}\n", skill.use_count));
        fm.push_str(&format!("# success_count: {}\n", skill.success_count));
        fm.push_str(&format!("# fail_count: {}\n", skill.fail_count));
        if let Some(ref last) = skill.last_used {
            fm.push_str(&format!("# last_used: {last}\n"));
        }
        fm.push_str(&format!("# total_duration_ms: {}\n", skill.total_duration_ms));

        let content = format!("---\n{fm}---\n\n{}\n", skill.body);
        fs::write(&file_path, content).map_err(SkillError::Io)?;

        // 写入 .usage.json sidecar（首次创建时）
        let usage_path = usage_file_path(&file_path);
        if !usage_path.exists() {
            let telemetry = UsageTelemetry::new();
            telemetry.save(&file_path);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum SkillError {
    Io(std::io::Error),
    InvalidName(String),
    AlreadyExists(String),
    NotFound(String),
    Parse(String),
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误: {e}"),
            Self::InvalidName(msg) => write!(f, "无效名称: {msg}"),
            Self::AlreadyExists(name) => write!(f, "技能「{name}」已存在"),
            Self::NotFound(name) => write!(f, "技能「{name}」不存在"),
            Self::Parse(msg) => write!(f, "解析错误: {msg}"),
        }
    }
}

impl std::error::Error for SkillError {}

impl From<std::io::Error> for SkillError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_engine() -> (tempfile::TempDir, SkillEngine) {
        let tmp = tempfile::tempdir().unwrap();
        let engine = SkillEngine::load(tmp.path().join("skills")).unwrap();
        (tmp, engine)
    }

    #[test]
    fn test_create_and_get() {
        let (_tmp, mut engine) = setup_engine();
        engine
            .create("test-skill", "测试技能", "执行步骤 1\n步骤 2", RunAs::Inline)
            .unwrap();

        let skill = engine.get("test-skill").unwrap();
        assert_eq!(skill.description, "测试技能");
        assert_eq!(skill.run_as, RunAs::Inline);
    }

    #[test]
    fn test_create_duplicate() {
        let (_tmp, mut engine) = setup_engine();
        engine
            .create("dup", "test", "body", RunAs::Inline)
            .unwrap();
        let err = engine
            .create("dup", "test", "body", RunAs::Inline)
            .unwrap_err();
        assert!(matches!(err, SkillError::AlreadyExists(_)));
    }

    #[test]
    fn test_delete() {
        let (_tmp, mut engine) = setup_engine();
        engine
            .create("del-me", "test", "body", RunAs::Inline)
            .unwrap();

        assert!(engine.delete("del-me").unwrap());
        assert!(!engine.delete("nonexistent").unwrap());
    }

    #[test]
    fn test_list_and_count() {
        let (_tmp, mut engine) = setup_engine();
        assert_eq!(engine.count(), 0);

        engine
            .create("a", "skill a", "body", RunAs::Inline)
            .unwrap();
        engine
            .create("b", "skill b", "body", RunAs::Subagent)
            .unwrap();

        assert_eq!(engine.count(), 2);
        assert_eq!(engine.list().len(), 2);
    }

    #[test]
    fn test_search() {
        let (_tmp, mut engine) = setup_engine();
        engine
            .create("rust-helper", "Rust 编程助手", "body", RunAs::Inline)
            .unwrap();
        engine
            .create("python-helper", "Python 助手", "body", RunAs::Inline)
            .unwrap();

        assert_eq!(engine.search("rust").len(), 1);
        assert_eq!(engine.search("helper").len(), 2);
        assert_eq!(engine.search("nonexistent").len(), 0);
    }

    #[test]
    fn test_record_usage() {
        let (_tmp, mut engine) = setup_engine();
        engine
            .create("test", "test", "body", RunAs::Inline)
            .unwrap();

        engine.record_usage("test", true, 100).unwrap();
        engine.record_usage("test", true, 200).unwrap();
        engine.record_usage("test", false, 50).unwrap();

        let skill = engine.get("test").unwrap();
        assert_eq!(skill.use_count, 3);
        assert_eq!(skill.success_count, 2);
        assert_eq!(skill.fail_count, 1);
        assert!((skill.success_rate() - 2.0 / 3.0).abs() < 0.01);
        assert_eq!(skill.avg_duration_ms(), 116); // (100+200+50)/3
    }

    #[test]
    fn test_success_rate() {
        let skill = Skill {
            use_count: 10,
            success_count: 8,
            fail_count: 2,
            total_duration_ms: 1000,
            ..dummy_skill()
        };
        assert!((skill.success_rate() - 0.8).abs() < 0.01);
        assert!(skill.is_healthy());
        assert!(!skill.needs_attention());
    }

    #[test]
    fn test_needs_attention() {
        let skill = Skill {
            use_count: 10,
            success_count: 2,
            fail_count: 8,
            total_duration_ms: 1000,
            ..dummy_skill()
        };
        assert!(skill.needs_attention());
        assert!(!skill.is_healthy());
    }

    #[test]
    fn test_reload_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");

        // 创建引擎并添加技能
        let mut engine = SkillEngine::load(&dir).unwrap();
        engine
            .create("persist", "持久化技能", "内容", RunAs::Inline)
            .unwrap();

        // 重新加载
        let engine2 = SkillEngine::load(&dir).unwrap();
        assert_eq!(engine2.count(), 1);
        assert_eq!(engine2.get("persist").unwrap().description, "持久化技能");
    }

    #[test]
    fn test_frontmatter_parsing() {
        let content = r#"---
description: "代码审查"
run_as: subagent
allowed_tools: [read_file, search_content]
model: deepseek-v4-pro
---

审查步骤：
1. 读取文件
2. 检查安全性
"#;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("code-review.md");
        fs::write(&path, content).unwrap();

        let skill = SkillEngine::load_skill_file(&path).unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.description, "代码审查");
        assert_eq!(skill.run_as, RunAs::Subagent);
        assert_eq!(skill.allowed_tools.len(), 2);
        assert_eq!(skill.model.as_deref(), Some("deepseek-v4-pro"));
    }

    #[test]
    fn test_load_rejects_invalid_name() {
        let (_tmp, mut engine) = setup_engine();
        let err = engine
            .create("invalid name!!!", "test", "body", RunAs::Inline)
            .unwrap_err();
        assert!(matches!(err, SkillError::InvalidName(_)));
    }

    fn dummy_skill() -> Skill {
        Skill {
            name: "dummy".into(),
            category: None,
            description: "".into(),
            body: "".into(),
            run_as: RunAs::Inline,
            allowed_tools: vec![],
            model: None,
            use_count: 0,
            success_count: 0,
            fail_count: 0,
            last_used: None,
            total_duration_ms: 0,
        }
    }
}
