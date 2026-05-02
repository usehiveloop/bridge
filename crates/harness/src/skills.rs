//! Write skill files into the harness's discovery directory before session start.

use bridge_core::skill::SkillDefinition;
use std::path::Path;
use tracing::warn;

/// Write each skill as `<root>/skills/<id>/SKILL.md` with optional supporting files.
///
/// Best-effort: failures are logged, not propagated. The harness will simply
/// not see the skill if the write fails.
pub fn write_skills(root: &Path, skills: &[SkillDefinition]) {
    let skills_dir = root.join("skills");
    if let Err(e) = std::fs::create_dir_all(&skills_dir) {
        warn!(path = %skills_dir.display(), error = %e, "skills root creation failed");
        return;
    }

    for skill in skills {
        let dir = skills_dir.join(&skill.id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(skill = %skill.id, error = %e, "skill dir creation failed");
            continue;
        }

        let mut body = String::new();
        body.push_str("---\n");
        // Both Claude Code and OpenCode key the skill by `name`, which
        // by convention matches the directory name (== skill.id). Title
        // goes nowhere structured — fold it into the body if needed.
        body.push_str(&format!("name: {}\n", skill.id));
        body.push_str(&format!("description: {}\n", skill.description));
        if let Some(fm) = &skill.frontmatter {
            if let Some(eff) = &fm.effort {
                body.push_str(&format!("effort: {}\n", eff));
            }
            if let Some(ctx) = &fm.context {
                body.push_str(&format!("context: {}\n", ctx));
            }
            if let Some(tools) = &fm.allowed_tools {
                body.push_str(&format!("allowed-tools: {}\n", tools.join(",")));
            }
        }
        body.push_str("---\n\n");
        body.push_str(&skill.content);

        let skill_md = dir.join("SKILL.md");
        if let Err(e) = std::fs::write(&skill_md, body) {
            warn!(skill = %skill.id, error = %e, "SKILL.md write failed");
        }

        for (rel, contents) in &skill.files {
            let target = dir.join(rel);
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&target, contents) {
                warn!(skill = %skill.id, file = rel, error = %e, "skill file write failed");
            }
        }
    }
}
