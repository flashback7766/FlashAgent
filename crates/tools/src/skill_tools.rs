//! `skill`: the model can find and read a project's skills without waiting
//! for the owner to type `/skill:<name>`. A skill is instructions, not code:
//! running one returns its text, and the model follows it in the same turn.

use std::path::{Path, PathBuf};

use crate::ToolError;

fn skill_dirs(cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.join(".agents").join("skills")];
    if let Some(home) = home {
        dirs.push(home.join(".flashagent").join("skills"));
    } else if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        dirs.push(PathBuf::from(home).join(".flashagent").join("skills"));
    }
    dirs
}

fn description(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        for key in ["description:", "Description:", "Trigger:"] {
            if let Some(rest) = line.strip_prefix(key) {
                let text = rest.trim().trim_matches('"').trim();
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

fn find(cwd: &Path, home: Option<&Path>, name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    let file = format!("{name}.md");
    skill_dirs(cwd, home).into_iter().map(|dir| dir.join(&file)).find(|path| path.is_file())
}

pub fn skill(cwd: &Path, home: Option<&Path>, action: &str, name: Option<&str>) -> Result<String, ToolError> {
    match action.trim().to_ascii_lowercase().as_str() {
        "list" => {
            let mut rows = Vec::new();
            for dir in skill_dirs(cwd, home) {
                let Ok(entries) = std::fs::read_dir(&dir) else { continue };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("md") {
                        continue;
                    }
                    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
                    if rows.iter().any(|(known, _)| known == stem) {
                        continue;
                    }
                    rows.push((stem.to_string(), description(&path).unwrap_or_default()));
                }
            }
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            if rows.is_empty() {
                return Ok("No skills yet. Add .agents/skills/<name>.md to the project, or ~/.flashagent/skills/<name>.md for every project.".to_string());
            }
            Ok(rows
                .into_iter()
                .map(|(name, what)| if what.is_empty() { format!("- {name}") } else { format!("- {name} — {what}") })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "run" => {
            let name = name.map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| {
                ToolError::Other("skill run needs a name; `skill` with action list shows them".into())
            })?;
            let path = find(cwd, home, name).ok_or_else(|| {
                ToolError::Other(format!("no skill named '{name}'; `skill` with action list shows them"))
            })?;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| ToolError::Other(format!("skill {name}: {e}")))?;
            Ok(crate::truncate_output(format!("Execute skill: {name}\n\nSkill Instructions:\n{text}")))
        }
        other => Err(ToolError::Other(format!("skill: unknown action '{other}'; use list or run"))),
    }
}
