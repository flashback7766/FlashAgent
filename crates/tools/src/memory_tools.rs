//! Long-term memory tools (`memory_read`, `memory_create`, `memory_update`,
//! `memory_remove`).
//!
//! One fact per file under `memory/`, with `MEMORY.md` as the index that every
//! prompt carries. Project memory lives in the working directory; global
//! memory in `~/.flashagent`, so it follows the user across projects and
//! across models. In autonomous `/goal` mode writes are refused: a long run
//! must not rewrite what the user knows to be true.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use flashagent_core::memory_store::{slugify, today, Entry, Kind, Scope, Store};
use serde::Deserialize;

use crate::ToolError;

fn store(cwd: &Path, scope: Scope) -> Result<Store, ToolError> {
    Store::for_scope(scope, cwd)
        .ok_or_else(|| ToolError::Other("could not determine the home directory for global memory".into()))
}

/// Arguments for `memory_read`.
#[derive(Debug, Deserialize)]
pub struct MemoryReadArgs {
    /// Memory scope to read: "project", "global", or "all" (default).
    pub scope: Option<String>,
    /// Name of a single memory to read in full; omitted, the index comes back.
    pub name: Option<String>,
}

/// Reads the index, or one memory in full.
pub fn memory_read(cwd: &Path, args: MemoryReadArgs) -> Result<String, ToolError> {
    let raw_scope = args.scope.as_deref().unwrap_or("all");
    let scopes: Vec<Scope> = if raw_scope.eq_ignore_ascii_case("all") {
        vec![Scope::Project, Scope::Global]
    } else {
        vec![Scope::parse(Some(raw_scope))]
    };

    if let Some(name) = args.name.as_deref().filter(|n| !n.trim().is_empty()) {
        for scope in &scopes {
            let Ok(store) = store(cwd, *scope) else { continue };
            if let Some(entry) = store.get(name) {
                return Ok(format!(
                    "=== {} memory: {} ({}, recorded {}) ===\n{}",
                    scope.label(),
                    entry.name,
                    entry.kind.label(),
                    entry.recorded,
                    entry.body
                ));
            }
        }
        return Ok(format!("No memory named '{name}'. Read without a name to see what is there."));
    }

    let mut out = Vec::new();
    for scope in scopes {
        let Ok(store) = store(cwd, scope) else { continue };
        let entries = store.list();
        if entries.is_empty() {
            continue;
        }
        let lines: Vec<String> = entries
            .iter()
            .map(|e| format!("- {} ({}, recorded {}) — {}", e.name, e.kind.label(), e.recorded, e.description))
            .collect();
        out.push(format!("=== {} memory ===\n{}", scope.label(), lines.join("\n")));
    }

    if out.is_empty() {
        Ok("Nothing has been remembered yet.".to_string())
    } else {
        out.push("Read one in full with memory_read and its name.".to_string());
        Ok(out.join("\n\n"))
    }
}

/// Arguments for `memory_create` and `memory_update`.
#[derive(Debug, Deserialize)]
pub struct MemoryWriteArgs {
    /// Short title; it becomes the memory's name.
    pub title: String,
    /// The fact itself, in full sentences.
    pub content: String,
    /// One line saying what this memory is about, for the index.
    pub description: Option<String>,
    /// "preference", "decision", "reference" or "work".
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// "project" (default) or "global".
    pub scope: Option<String>,
}

/// Arguments for `memory_remove`.
#[derive(Debug, Deserialize)]
pub struct MemoryRemoveArgs {
    /// Name (or title) of the memory to forget.
    pub title: String,
    /// "project" (default) or "global".
    pub scope: Option<String>,
}

fn check_goal_mutation(is_goal_mode: &Arc<AtomicBool>, op: &str) -> Result<(), ToolError> {
    if is_goal_mode.load(Ordering::Relaxed) {
        return Err(ToolError::Other(format!(
            "{op} is disabled in autonomous /goal mode (memory is read-only there, so a long run cannot rewrite what you know)."
        )));
    }
    Ok(())
}

/// Where a memory belongs when the model did not say.
///
/// How the user likes to work follows them into every project; everything
/// else is about this codebase. Left to the schema default, a small model
/// files preferences under the project, where the next project never sees
/// them.
fn default_scope(args: &MemoryWriteArgs) -> Scope {
    match args.scope.as_deref() {
        Some(explicit) => Scope::parse(Some(explicit)),
        None if args.kind.as_deref().map(Kind::parse) == Some(Kind::Preference) => Scope::Global,
        None => Scope::Project,
    }
}

fn entry_from(args: &MemoryWriteArgs, existing: Option<&Entry>) -> Entry {
    let description = args
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        // Without a description the index line would say nothing, so the
        // first sentence of the fact stands in for one.
        .unwrap_or_else(|| first_sentence(&args.content));
    Entry {
        name: slugify(&args.title),
        description,
        kind: args.kind.as_deref().map(Kind::parse).unwrap_or(Kind::Decision),
        // Correcting a fact does not change when it was first learned.
        recorded: existing.map(|e| e.recorded.clone()).filter(|r| !r.is_empty()).unwrap_or_else(today),
        body: args.content.trim().to_string(),
    }
}

fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = flat.find(". ").map(|i| i + 1).unwrap_or(flat.len());
    let sentence = flat[..cut].trim_end_matches('.').to_string();
    if sentence.chars().count() > 120 {
        sentence.chars().take(117).collect::<String>() + "..."
    } else {
        sentence
    }
}

/// Remembers something new.
pub fn memory_create(
    cwd: &Path,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryWriteArgs,
) -> Result<String, ToolError> {
    check_goal_mutation(is_goal_mode, "memory_create")?;
    let scope = default_scope(&args);
    let store = store(cwd, scope)?;
    let entry = entry_from(&args, None);

    if store.get(&entry.name).is_some() {
        return Err(ToolError::Other(format!(
            "'{}' is already remembered; use memory_update to correct it",
            entry.name
        )));
    }
    // Two memories about the same thing is how a memory file becomes useless.
    if let Some(similar) = store.find_similar(&entry.description) {
        return Err(ToolError::Other(format!(
            "'{}' already covers this ({}). Use memory_update on that name instead of writing a second one.",
            similar.name, similar.description
        )));
    }

    let path = store.save(&entry).map_err(|e| ToolError::Other(format!("failed to write memory: {e}")))?;
    Ok(format!("Remembered '{}' in {} memory ({}).", entry.name, scope.label(), path.display()))
}

/// Corrects something already remembered.
pub fn memory_update(
    cwd: &Path,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryWriteArgs,
) -> Result<String, ToolError> {
    check_goal_mutation(is_goal_mode, "memory_update")?;
    let scope = default_scope(&args);
    let store = store(cwd, scope)?;
    let existing = store.get(&slugify(&args.title));
    let entry = entry_from(&args, existing.as_ref());
    store.save(&entry).map_err(|e| ToolError::Other(format!("failed to write memory: {e}")))?;
    Ok(match existing {
        Some(_) => format!("Updated '{}' in {} memory.", entry.name, scope.label()),
        None => format!("Remembered '{}' in {} memory (there was nothing by that name).", entry.name, scope.label()),
    })
}

/// Forgets something.
pub fn memory_remove(
    cwd: &Path,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryRemoveArgs,
) -> Result<String, ToolError> {
    check_goal_mutation(is_goal_mode, "memory_remove")?;
    let scope = Scope::parse(args.scope.as_deref());
    let store = store(cwd, scope)?;
    let removed = store
        .remove(&args.title)
        .map_err(|e| ToolError::Other(format!("failed to update memory: {e}")))?;
    if removed {
        Ok(format!("Forgot '{}' from {} memory.", slugify(&args.title), scope.label()))
    } else {
        Err(ToolError::Other(format!(
            "no memory named '{}' in {} memory",
            slugify(&args.title),
            scope.label()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal_off() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn write(title: &str, content: &str) -> MemoryWriteArgs {
        MemoryWriteArgs {
            title: title.to_string(),
            content: content.to_string(),
            description: None,
            kind: Some("decision".into()),
            scope: Some("project".into()),
        }
    }

    #[test]
    fn something_remembered_can_be_read_back() {
        let dir = tempfile::tempdir().unwrap();
        memory_create(dir.path(), &goal_off(), write("Tests use nextest", "The suite is run with cargo nextest.")).unwrap();
        let listed = memory_read(dir.path(), MemoryReadArgs { scope: Some("project".into()), name: None }).unwrap();
        assert!(listed.contains("tests-use-nextest"), "{listed}");
        let one = memory_read(
            dir.path(),
            MemoryReadArgs { scope: Some("project".into()), name: Some("tests-use-nextest".into()) },
        )
        .unwrap();
        assert!(one.contains("cargo nextest"), "{one}");
    }

    #[test]
    fn the_same_fact_is_not_remembered_twice() {
        let dir = tempfile::tempdir().unwrap();
        memory_create(dir.path(), &goal_off(), write("Deploys need approval", "Production deploys need approval from the release owner.")).unwrap();
        let again = memory_create(
            dir.path(),
            &goal_off(),
            write("Approval for deploys", "Production deploys need approval before they run."),
        );
        let msg = format!("{:?}", again.unwrap_err());
        assert!(msg.contains("memory_update"), "it must point at the existing one: {msg}");
    }

    #[test]
    fn correcting_a_fact_keeps_when_it_was_learned() {
        let dir = tempfile::tempdir().unwrap();
        memory_create(dir.path(), &goal_off(), write("Editor", "The user edits with helix.")).unwrap();
        let store = Store::new(dir.path());
        let first = store.get("editor").unwrap();
        memory_update(dir.path(), &goal_off(), write("Editor", "The user edits with neovim.")).unwrap();
        let second = store.get("editor").unwrap();
        assert_eq!(second.recorded, first.recorded, "a correction is not a new fact");
        assert!(second.body.contains("neovim"));
    }

    #[test]
    fn a_preference_with_no_scope_given_follows_the_user_everywhere() {
        // Observed: a 2B model recorded "I always run tests with nextest" as
        // a fact about this project, where the next project never sees it.
        let args = MemoryWriteArgs {
            title: "Test runner".into(),
            content: "The user always runs tests with cargo nextest.".into(),
            description: None,
            kind: Some("preference".into()),
            scope: None,
        };
        assert_eq!(default_scope(&args), Scope::Global);

        let explicit = MemoryWriteArgs { scope: Some("project".into()), ..args };
        assert_eq!(default_scope(&explicit), Scope::Project, "an explicit choice is still honoured");

        let decision = MemoryWriteArgs {
            title: "Release flow".into(),
            content: "Betas ship on the rolling beta tag.".into(),
            description: None,
            kind: Some("decision".into()),
            scope: None,
        };
        assert_eq!(default_scope(&decision), Scope::Project);
    }

    #[test]
    fn a_goal_run_cannot_rewrite_memory() {
        let dir = tempfile::tempdir().unwrap();
        let goal = Arc::new(AtomicBool::new(true));
        assert!(memory_create(dir.path(), &goal, write("x", "y")).is_err());
        assert!(memory_update(dir.path(), &goal, write("x", "y")).is_err());
        assert!(memory_remove(dir.path(), &goal, MemoryRemoveArgs { title: "x".into(), scope: None }).is_err());
    }

    #[test]
    fn forgetting_something_that_is_not_there_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let err = memory_remove(dir.path(), &goal_off(), MemoryRemoveArgs { title: "ghost".into(), scope: None });
        assert!(format!("{:?}", err.unwrap_err()).contains("no memory named"));
    }

    #[test]
    fn an_index_line_is_written_even_when_the_model_gives_no_description() {
        let dir = tempfile::tempdir().unwrap();
        memory_create(
            dir.path(),
            &goal_off(),
            write("Release flow", "Beta builds ship on the rolling beta tag. Stable ships from v1.0.0 onwards."),
        )
        .unwrap();
        let index = std::fs::read_to_string(Store::new(dir.path()).index_path()).unwrap();
        assert!(index.contains("Beta builds ship on the rolling beta tag"), "{index}");
        assert!(!index.contains("Stable ships from"), "the index is one line, not the whole fact:\n{index}");
    }
}
