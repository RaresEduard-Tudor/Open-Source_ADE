//! Shared skills — Claude-Code-style instruction folders.
//!
//! Skills live in `~/.config/ade/skills/<name>/SKILL.md` (global) and
//! `./.ade/skills/<name>/SKILL.md` (project). Each `SKILL.md` has YAML-ish
//! frontmatter (`name`, `description`) and a body of instructions.
//!
//! Progressive disclosure: only name+description go into the system prompt; the
//! agent calls the `use_skill` tool to pull a skill's full body on demand. Every
//! agent shares the same folder — no per-model copies.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::provider::ToolSpec;
use crate::tools::{Tool, ToolContext, ToolRegistry};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub body: String,
}

/// Parse a SKILL.md into (name, description, body). `name` falls back to the
/// directory name when frontmatter omits it.
fn parse_skill_md(text: &str, fallback_name: &str) -> (String, String, String) {
    let mut name = fallback_name.to_string();
    let mut description = String::new();

    let rest = text.strip_prefix("---");
    if let Some(after) = rest {
        if let Some(end) = after.find("\n---") {
            let front = &after[..end];
            for line in front.lines() {
                if let Some(v) = line.strip_prefix("name:") {
                    name = v.trim().trim_matches('"').to_string();
                } else if let Some(v) = line.strip_prefix("description:") {
                    description = v.trim().trim_matches('"').to_string();
                }
            }
            let body = after[end + 4..].trim_start_matches('-').trim_start().to_string();
            return (name, description, body);
        }
    }
    (name, description, text.trim().to_string())
}

/// Holds all discovered skills and serves the `use_skill` tool.
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Scan the global and project skill directories.
    pub fn discover(project_root: &std::path::Path) -> SkillRegistry {
        let mut skills = Vec::new();
        if let Some(cfg) = dirs::config_dir() {
            Self::scan_dir(&cfg.join("ade").join("skills"), &mut skills);
        }
        Self::scan_dir(&project_root.join(".ade").join("skills"), &mut skills);
        SkillRegistry { skills }
    }

    fn scan_dir(dir: &std::path::Path, out: &mut Vec<Skill>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let md = path.join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&md) else { continue };
            let fallback = entry.file_name().to_string_lossy().to_string();
            let (name, description, body) = parse_skill_md(&text, &fallback);
            // Project skills override same-named global skills.
            out.retain(|s: &Skill| s.name != name);
            out.push(Skill { name, description, dir: path, body });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn list(&self) -> &[Skill] {
        &self.skills
    }

    /// System-prompt fragment advertising available skills (name + description).
    pub fn system_prompt(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut s = String::from(
            "Available skills (call use_skill with the name to load full instructions):\n",
        );
        for sk in &self.skills {
            s.push_str(&format!("- {}: {}\n", sk.name, sk.description));
        }
        Some(s)
    }

    /// Register the `use_skill` tool, sharing the skill data.
    pub fn register_tool(self: &Arc<Self>, reg: &mut ToolRegistry) {
        if self.skills.is_empty() {
            return;
        }
        reg.register(Box::new(UseSkill { registry: self.clone() }));
    }
}

struct UseSkill {
    registry: Arc<SkillRegistry>,
}

#[async_trait]
impl Tool for UseSkill {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "use_skill".into(),
            description: "Load a skill's full instructions by name.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        }
    }
    async fn execute(&self, args: &Value, _ctx: &ToolContext) -> Result<String> {
        let name = args["name"]
            .as_str()
            .ok_or_else(|| Error::Tool("use_skill: missing 'name'".into()))?;
        let skill = self
            .registry
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| Error::Tool(format!("unknown skill '{name}'")))?;
        Ok(format!(
            "# Skill: {}\nDirectory: {}\n\n{}",
            skill.name,
            skill.dir.display(),
            skill.body
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let md = "---\nname: \"commit\"\ndescription: write a commit\n---\nDo the thing.";
        let (n, d, b) = parse_skill_md(md, "fallback");
        assert_eq!(n, "commit");
        assert_eq!(d, "write a commit");
        assert_eq!(b, "Do the thing.");
    }

    #[test]
    fn falls_back_without_frontmatter() {
        let (n, d, b) = parse_skill_md("just body", "myskill");
        assert_eq!(n, "myskill");
        assert!(d.is_empty());
        assert_eq!(b, "just body");
    }
}
