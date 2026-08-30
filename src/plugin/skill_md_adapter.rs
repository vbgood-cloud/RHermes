//! SkillMdAdapter — Markdown 技能适配 Plugin trait
//!
//! SKILL.md 是"上下文注入型"插件：execute 返回技能内容本身（给 LLM 阅读），
//! 不执行任何副作用。与 agent::SkillEngine 的技能文件格式兼容（YAML frontmatter + Markdown）。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;

use super::types::{PluginDescriptor, PluginError, PluginOutput, PluginSource};

/// Markdown 技能插件
pub struct SkillMdPlugin {
    descriptor: PluginDescriptor,
    content: String,
    path: PathBuf,
}

impl SkillMdPlugin {
    /// 从 .md 文件加载（解析 frontmatter 提取 name/description）
    pub fn from_file(path: &Path, name_override: Option<String>) -> Result<Self, PluginError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| PluginError::Config(format!("读取 {} 失败: {e}", path.display())))?;

        // 解析 YAML frontmatter（--- name: ... ---）
        let (fm_name, fm_desc, fm_version) = parse_frontmatter(&content);

        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("skill");
        let name = name_override.or(fm_name).unwrap_or_else(|| file_stem.to_string());

        Ok(Self {
            descriptor: PluginDescriptor {
                name,
                description: fm_desc.unwrap_or_else(|| format!("Markdown 技能: {}", path.display())),
                source: PluginSource::SkillMd { path: path.display().to_string() },
                parallel_safe: true, // 纯读取，无副作用
                version: fm_version,
            },
            content,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 技能正文（供上下文注入）
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// 解析 `---\nkey: value\n---` frontmatter（宽松解析，无需 yaml crate 完整功能）
fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut name = None;
    let mut desc = None;
    let mut version = None;
    let mut in_fm = false;
    for line in content.lines() {
        let t = line.trim();
        if t == "---" {
            if in_fm {
                break; // frontmatter 结束
            }
            in_fm = true;
            continue;
        }
        if !in_fm {
            continue;
        }
        if let Some(v) = t.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = t.strip_prefix("description:") {
            desc = Some(v.trim().trim_matches('"').to_string());
        } else if let Some(v) = t.strip_prefix("version:") {
            version = Some(v.trim().to_string());
        }
    }
    (name, desc, version)
}

#[async_trait]
impl super::Plugin for SkillMdPlugin {
    async fn descriptor(&self) -> PluginDescriptor {
        self.descriptor.clone()
    }

    async fn execute(&self, _input: &Value) -> Result<PluginOutput, PluginError> {
        // 技能插件 = 上下文注入：返回全文给 LLM
        Ok(PluginOutput::ok(self.content.clone()))
    }

    async fn health(&self) -> Result<bool, PluginError> {
        Ok(self.path.exists())
    }

    async fn reload(&self) -> Result<(), PluginError> {
        // SkillMd 内容在构造时快照——重载需重建实例（由 PluginRouter::reload 驱动）
        Err(PluginError::Config("SkillMd 热重载需通过 PluginRouter::reload 实现".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::Plugin; // trait 方法引入作用域

    #[test]
    fn test_parse_frontmatter() {
        let md = "---\nname: my-skill\ndescription: \"测试技能\"\nversion: 1.2\n---\n\n# 正文";
        let (n, d, v) = parse_frontmatter(md);
        assert_eq!(n.as_deref(), Some("my-skill"));
        assert_eq!(d.as_deref(), Some("测试技能"));
        assert_eq!(v.as_deref(), Some("1.2"));
    }

    #[test]
    fn test_parse_frontmatter_missing() {
        let md = "# 没有 frontmatter 的文档";
        let (n, d, v) = parse_frontmatter(md);
        assert!(n.is_none());
        assert!(d.is_none());
        assert!(v.is_none());
    }

    #[tokio::test]
    async fn test_skill_md_plugin_load_and_execute() {
        let dir = std::env::temp_dir();
        let p = dir.join("rhermes-test-skill.md");
        std::fs::write(&p, "---\nname: demo-skill\ndescription: 演示\n---\n\n# 步骤\n1. xxx").unwrap();

        let plugin = SkillMdPlugin::from_file(&p, None).unwrap();
        let d = plugin.descriptor().await;
        assert_eq!(d.name, "demo-skill");
        assert_eq!(d.description, "演示");

        let out = plugin.execute(&serde_json::json!({})).await.unwrap();
        assert!(out.success);
        assert!(out.output.contains("# 步骤"));

        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn test_skill_md_name_override() {
        let dir = std::env::temp_dir();
        let p = dir.join("rhermes-test-skill2.md");
        std::fs::write(&p, "---\nname: inner-name\n---\n内容").unwrap();

        let plugin = SkillMdPlugin::from_file(&p, Some("override-name".into())).unwrap();
        assert_eq!(plugin.descriptor().await.name, "override-name");
        let _ = std::fs::remove_file(&p);
    }
}
