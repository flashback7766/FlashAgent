//! Memory tools (`memory_read`, `memory_create`, `memory_update`,
//! `memory_remove`). Project memory lives in the working directory, global in
//! `~/.flashagent`. Writes are refused during `/goal`: a long unattended run
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

#[derive(Debug, Deserialize)]
pub struct MemoryReadArgs {
    /// "project", "global", or "all" (default).
    pub scope: Option<String>,
    /// Omitted: the index is returned.
    pub name: Option<String>,
}

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

#[derive(Debug, Deserialize)]
pub struct MemoryWriteArgs {
    /// Becomes the memory's name.
    pub title: String,
    pub content: String,
    /// For the index.
    pub description: Option<String>,
    /// "preference", "decision", "reference" or "work".
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// "project" (default) or "global".
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MemoryRemoveArgs {
    /// Name or title.
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

/// Preferences follow the user into every project; the rest is about this
/// codebase. Left to the schema default, small models filed preferences under
/// the project.
fn default_scope(args: &MemoryWriteArgs) -> Scope {
    match args.scope.as_deref() {
        Some(explicit) => Scope::parse(Some(explicit)),
        None if args.kind.as_deref().map(Kind::parse) == Some(Kind::Preference) => Scope::Global,
        None => Scope::Project,
    }
}

/// The memory an update or removal is about: in the scope named, or else
/// wherever one by that title already is, project first. Writing a second one
/// in the default scope left the old fact standing beside the correction.
fn locate(cwd: &Path, scope: Option<&str>, title: &str) -> Result<Option<(Scope, Store, Entry)>, ToolError> {
    let scopes = match scope {
        Some(explicit) => vec![Scope::parse(Some(explicit))],
        None => vec![Scope::Project, Scope::Global],
    };
    for scope in scopes {
        let Ok(store) = store(cwd, scope) else { continue };
        if let Some(entry) = store.get(title) {
            return Ok(Some((scope, store, entry)));
        }
    }
    Ok(None)
}

fn entry_from(args: &MemoryWriteArgs, existing: Option<&Entry>) -> Entry {
    let description = args
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        // Without a description the index line says nothing; the first sentence
        // stands in.
        .unwrap_or_else(|| first_sentence(&args.content));
    Entry {
        // An existing memory keeps its file name and kind unless told otherwise.
        name: existing.map_or_else(|| slugify(&args.title), |e| e.name.clone()),
        description,
        kind: args.kind.as_deref().map(Kind::parse).or(existing.map(|e| e.kind)).unwrap_or(Kind::Decision),
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
    // Duplicates are how a memory store becomes useless.
    if let Some(similar) = store.find_similar(&entry.description) {
        return Err(ToolError::Other(format!(
            "'{}' already covers this ({}). Use memory_update on that name instead of writing a second one.",
            similar.name, similar.description
        )));
    }

    let path = store.save(&entry).map_err(|e| ToolError::Other(format!("failed to write memory: {e}")))?;
    Ok(format!("Remembered '{}' in {} memory ({}).", entry.name, scope.label(), path.display()))
}

pub fn memory_update(
    cwd: &Path,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryWriteArgs,
) -> Result<String, ToolError> {
    check_goal_mutation(is_goal_mode, "memory_update")?;
    let (scope, store, existing) = match locate(cwd, args.scope.as_deref(), &args.title)? {
        Some((scope, store, entry)) => (scope, store, Some(entry)),
        None => {
            let scope = default_scope(&args);
            (scope, store(cwd, scope)?, None)
        }
    };
    let entry = entry_from(&args, existing.as_ref());
    store.save(&entry).map_err(|e| ToolError::Other(format!("failed to write memory: {e}")))?;
    Ok(match existing {
        Some(_) => format!("Updated '{}' in {} memory.", entry.name, scope.label()),
        None => format!("Remembered '{}' in {} memory (there was nothing by that name).", entry.name, scope.label()),
    })
}

pub fn memory_remove(
    cwd: &Path,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryRemoveArgs,
) -> Result<String, ToolError> {
    check_goal_mutation(is_goal_mode, "memory_remove")?;
    let (scope, removed) = match locate(cwd, args.scope.as_deref(), &args.title)? {
        Some((scope, store, _)) => {
            (scope, store.remove(&args.title).map_err(|e| ToolError::Other(format!("failed to update memory: {e}")))?)
        }
        None => (Scope::parse(args.scope.as_deref()), false),
    };
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
        // Observed: a 2B model filed "I always run tests with nextest" under the project.
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
    fn an_update_corrects_the_memory_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let entry = Entry {
            name: "editor".into(),
            description: "The user edits with helix".into(),
            kind: Kind::Preference,
            recorded: today(),
            body: "helix".into(),
        };
        Store::new(&project).save(&entry).unwrap();
        // As a model sends it: no type, no scope.
        let correction = MemoryWriteArgs { kind: None, scope: None, ..write("Editor", "The user edits with neovim.") };
        memory_update(&project, &goal_off(), correction).unwrap();
        let entries = Store::new(&project).list();
        assert_eq!(entries.len(), 1, "no second memory beside the corrected one");
        assert_eq!(entries[0].kind, Kind::Preference, "its kind is kept");
        assert!(entries[0].body.contains("neovim"));
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
