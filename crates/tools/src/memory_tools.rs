//! Project and global memory management tools (`memory_read`, `memory_create`, `memory_update`, `memory_remove`).
//!
//! Complies with PHILOSOPHY.md §8: project `./MEMORY.md` and user global `~/.flashagent/MEMORY.md`.
//! In autonomous `/goal` mode, mutations are blocked to protect long-term knowledge.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use serde::Deserialize;

use crate::ToolError;

fn resolve_global_memory_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| PathBuf::from(h).join(".flashagent").join("MEMORY.md"))
}

fn resolve_path(cwd: &Path, scope: Option<&str>) -> Result<PathBuf, ToolError> {
    match scope.unwrap_or("project") {
        "global" => resolve_global_memory_path()
            .ok_or_else(|| ToolError::Other("could not determine home directory for global memory".into())),
        _ => Ok(cwd.join("MEMORY.md")),
    }
}

/// Arguments for `memory_read`.
#[derive(Debug, Deserialize)]
pub struct MemoryReadArgs {
    /// Memory scope to read: "project", "global", or "all" (default).
    pub scope: Option<String>,
}

/// Reads project or global memory.
pub fn memory_read(cwd: &Path, args: MemoryReadArgs) -> Result<String, ToolError> {
    let scope = args.scope.as_deref().unwrap_or("all");
    let mut out = Vec::new();

    if scope == "project" || scope == "all" {
        let proj_path = cwd.join("MEMORY.md");
        if proj_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&proj_path) {
                out.push(format!("=== Project Memory (./MEMORY.md) ===\n{}", content.trim()));
            }
        } else if scope == "project" {
            out.push("Project memory file (./MEMORY.md) does not exist yet.".to_string());
        }
    }

    if scope == "global" || scope == "all" {
        if let Some(glob_path) = resolve_global_memory_path() {
            if glob_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&glob_path) {
                    out.push(format!("=== Global Memory (~/.flashagent/MEMORY.md) ===\n{}", content.trim()));
                }
            } else if scope == "global" {
                out.push("Global memory file (~/.flashagent/MEMORY.md) does not exist yet.".to_string());
            }
        }
    }

    if out.is_empty() {
        Ok("No memory documents found in project or global configuration.".to_string())
    } else {
        Ok(out.join("\n\n"))
    }
}

/// Arguments for `memory_create` and `memory_update`.
#[derive(Debug, Deserialize)]
pub struct MemoryWriteArgs {
    /// Section title (e.g. "Coding Conventions", "API Endpoints").
    pub title: String,
    /// Detailed notes or guidelines for this section.
    pub content: String,
    /// "project" (default) or "global".
    pub scope: Option<String>,
}

/// Arguments for `memory_remove`.
#[derive(Debug, Deserialize)]
pub struct MemoryRemoveArgs {
    /// Section title to remove.
    pub title: String,
    /// "project" (default) or "global".
    pub scope: Option<String>,
}

fn check_goal_mutation(is_goal_mode: &Arc<AtomicBool>, op: &str) -> Result<(), ToolError> {
    if is_goal_mode.load(Ordering::Relaxed) {
        return Err(ToolError::Other(format!(
            "{op} is disabled in autonomous /goal mode (memory is read-only in goal mode to protect long-term knowledge)."
        )));
    }
    Ok(())
}

/// Creates a new memory section.
pub fn memory_create(
    cwd: &Path,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryWriteArgs,
) -> Result<String, ToolError> {
    check_goal_mutation(is_goal_mode, "memory_create")?;

    let path = resolve_path(cwd, args.scope.as_deref())?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let title_header = format!("## {}", args.title.trim());

    if existing.lines().any(|l| l.trim().eq_ignore_ascii_case(&title_header)) {
        return Err(ToolError::Other(format!(
            "memory section '{}' already exists; use memory_update to modify it",
            args.title
        )));
    }

    let mut new_text = existing.trim_end().to_string();
    if !new_text.is_empty() {
        new_text.push_str("\n\n");
    }
    new_text.push_str(&format!("## {}\n{}\n", args.title.trim(), args.content.trim()));

    std::fs::write(&path, &new_text)
        .map_err(|e| ToolError::Other(format!("failed to write to {}: {e}", path.display())))?;

    Ok(format!(
        "Successfully created memory section '{}' in {}",
        args.title,
        path.display()
    ))
}

/// Updates an existing memory section (or appends if not found).
pub fn memory_update(
    cwd: &Path,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryWriteArgs,
) -> Result<String, ToolError> {
    check_goal_mutation(is_goal_mode, "memory_update")?;

    let path = resolve_path(cwd, args.scope.as_deref())?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let title_header = format!("## {}", args.title.trim());

    let mut updated_lines = Vec::new();
    let mut in_target_section = false;
    let mut found = false;

    for line in existing.lines() {
        if line.trim().starts_with("## ") {
            if line.trim().eq_ignore_ascii_case(&title_header) {
                in_target_section = true;
                found = true;
                updated_lines.push(format!("## {}", args.title.trim()));
                updated_lines.push(args.content.trim().to_string());
                continue;
            } else {
                in_target_section = false;
            }
        }

        if !in_target_section {
            updated_lines.push(line.to_string());
        }
    }

    if !found {
        if !updated_lines.is_empty() {
            updated_lines.push(String::new());
        }
        updated_lines.push(format!("## {}", args.title.trim()));
        updated_lines.push(args.content.trim().to_string());
    }

    let mut new_text = updated_lines.join("\n");
    new_text.push('\n');

    std::fs::write(&path, &new_text)
        .map_err(|e| ToolError::Other(format!("failed to write to {}: {e}", path.display())))?;

    Ok(format!(
        "Successfully updated memory section '{}' in {}",
        args.title,
        path.display()
    ))
}

/// Removes an existing memory section.
pub fn memory_remove(
    cwd: &Path,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryRemoveArgs,
) -> Result<String, ToolError> {
    check_goal_mutation(is_goal_mode, "memory_remove")?;

    let path = resolve_path(cwd, args.scope.as_deref())?;
    if !path.exists() {
        return Err(ToolError::Other(format!(
            "memory file '{}' does not exist",
            path.display()
        )));
    }

    let existing = std::fs::read_to_string(&path)
        .map_err(|e| ToolError::Other(format!("failed to read {}: {e}", path.display())))?;
    let title_header = format!("## {}", args.title.trim());

    let mut updated_lines = Vec::new();
    let mut in_target_section = false;
    let mut found = false;

    for line in existing.lines() {
        if line.trim().starts_with("## ") {
            if line.trim().eq_ignore_ascii_case(&title_header) {
                in_target_section = true;
                found = true;
                continue;
            } else {
                in_target_section = false;
            }
        }

        if !in_target_section {
            updated_lines.push(line.to_string());
        }
    }

    if !found {
        return Err(ToolError::Other(format!(
            "memory section '{}' not found in {}",
            args.title,
            path.display()
        )));
    }

    let mut new_text = updated_lines.join("\n").trim().to_string();
    if !new_text.is_empty() {
        new_text.push('\n');
    }

    std::fs::write(&path, &new_text)
        .map_err(|e| ToolError::Other(format!("failed to write to {}: {e}", path.display())))?;

    Ok(format!(
        "Successfully removed memory section '{}' from {}",
        args.title,
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_crud_cycle() {
        let temp = crate::testing::tempdir();
        let flag = Arc::new(AtomicBool::new(false));

        // Create
        let res = memory_create(
            &temp,
            &flag,
            MemoryWriteArgs {
                title: "Arch Rules".into(),
                content: "Use M3 design.".into(),
                scope: Some("project".into()),
            },
        )
        .unwrap();
        assert!(res.contains("Successfully created"));

        // Read
        let read = memory_read(
            &temp,
            MemoryReadArgs {
                scope: Some("project".into()),
            },
        )
        .unwrap();
        assert!(read.contains("## Arch Rules"));
        assert!(read.contains("Use M3 design."));

        // Update
        let update = memory_update(
            &temp,
            &flag,
            MemoryWriteArgs {
                title: "Arch Rules".into(),
                content: "Use M3 Expressive design.".into(),
                scope: Some("project".into()),
            },
        )
        .unwrap();
        assert!(update.contains("Successfully updated"));

        let read2 = memory_read(
            &temp,
            MemoryReadArgs {
                scope: Some("project".into()),
            },
        )
        .unwrap();
        assert!(read2.contains("Use M3 Expressive design."));

        // Remove
        let remove = memory_remove(
            &temp,
            &flag,
            MemoryRemoveArgs {
                title: "Arch Rules".into(),
                scope: Some("project".into()),
            },
        )
        .unwrap();
        assert!(remove.contains("Successfully removed"));

        let read3 = memory_read(
            &temp,
            MemoryReadArgs {
                scope: Some("project".into()),
            },
        )
        .unwrap();
        assert!(!read3.contains("Arch Rules"));
    }

    #[test]
    fn test_memory_goal_mode_mutation_blocked() {
        let temp = crate::testing::tempdir();
        let flag = Arc::new(AtomicBool::new(true));

        let res = memory_create(
            &temp,
            &flag,
            MemoryWriteArgs {
                title: "Test".into(),
                content: "Data".into(),
                scope: Some("project".into()),
            },
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("disabled in autonomous /goal mode"));
    }
}
