//! The `memory` tool, shaped like Claude Code's: one entry point with a
//! command — `view`, `create`, `str_replace`, `insert`, `delete`, `rename`
//! — and one extra command for the migration, `import`, which copies an old
//! fact into the new store without retyping its body.
//!
//! Project memory lives under `~/.flashagent/projects/<slug>/memory/`, global
//! under `~/.flashagent/memory/`. Writes are refused during `/goal`: a long
//! unattended run must not rewrite what the user knows to be true.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use flashagent_core::memory_store::{
    file_name, import_legacy, legacy_inventory, Kind, Scope, Store, INDEX,
};
use serde::Deserialize;

use crate::ToolError;

fn store(cwd: &Path, home: Option<&Path>, scope: Scope) -> Result<Store, ToolError> {
    match home {
        Some(home) => Ok(Store::for_scope_at(home, scope, cwd)),
        None => Store::for_scope(scope, cwd)
            .ok_or_else(|| ToolError::Other("could not determine the home directory for global memory".into())),
    }
}

fn home_or_env(home: Option<&Path>) -> Result<PathBuf, ToolError> {
    match home {
        Some(home) => Ok(home.to_path_buf()),
        None => flashagent_core::memory_store::home_dir()
            .ok_or_else(|| ToolError::Other("could not determine the home directory".into())),
    }
}

fn err(e: impl std::fmt::Display) -> ToolError {
    ToolError::Other(e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct MemoryArgs {
    /// view | create | str_replace | insert | delete | rename | import
    pub command: String,
    /// project | global | all (view only) | legacy-project | legacy-global
    pub scope: Option<String>,
    /// The topic file, e.g. "build-uses-nextest" or "MEMORY.md".
    pub path: Option<String>,
    /// create: the body. Frontmatter is added when it is not there.
    pub file_text: Option<String>,
    /// create: the one index line this fact gets.
    pub description: Option<String>,
    /// create: user | feedback | project | reference.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// str_replace
    pub old_str: Option<String>,
    pub new_str: Option<String>,
    /// insert: after this 1-based line; 0 puts the text at the top.
    pub insert_line: Option<usize>,
    pub insert_text: Option<String>,
    /// rename
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    /// import: the old scope and the new name.
    pub from: Option<String>,
    pub to: Option<String>,
}

fn mutation_guard(is_goal_mode: &Arc<AtomicBool>, command: &str) -> Result<(), ToolError> {
    if is_goal_mode.load(Ordering::Relaxed) {
        return Err(ToolError::Other(format!(
            "memory {command} is disabled in autonomous /goal mode (memory is read-only there, so a long run cannot rewrite what you know)."
        )));
    }
    Ok(())
}

fn required(value: Option<&str>, what: &str) -> Result<String, ToolError> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolError::Other(format!("memory: {what} is required")))
}

pub fn memory(
    cwd: &Path,
    home: Option<&Path>,
    is_goal_mode: &Arc<AtomicBool>,
    args: MemoryArgs,
) -> Result<String, ToolError> {
    let command = args.command.trim().to_ascii_lowercase();
    match command.as_str() {
        "view" | "list" | "read" => view(cwd, home, &args),
        "create" | "str_replace" | "insert" | "delete" | "rename" | "import" => {
            mutation_guard(is_goal_mode, &command)?;
            match command.as_str() {
                "create" => create(cwd, home, &args),
                "str_replace" => replace(cwd, home, &args),
                "insert" => insert(cwd, home, &args),
                "delete" => delete(cwd, home, &args),
                "rename" => rename(cwd, home, &args),
                "import" => import(cwd, home, &args),
                _ => unreachable!(),
            }
        }
        other => Err(ToolError::Other(format!(
            "unknown memory command '{other}'; use view, create, str_replace, insert, delete, rename or import"
        ))),
    }
}

fn view(cwd: &Path, home: Option<&Path>, args: &MemoryArgs) -> Result<String, ToolError> {
    let raw_scope = args.scope.as_deref().unwrap_or("all").trim().to_ascii_lowercase();
    if raw_scope == "legacy-project" || raw_scope == "legacy-global" {
        let scope = if raw_scope == "legacy-global" { Scope::Global } else { Scope::Project };
        return view_legacy(cwd, home, scope, args.path.as_deref());
    }
    if let Some(path) = args.path.as_deref().filter(|p| !p.trim().is_empty()) {
        let scopes: Vec<Scope> = if raw_scope == "all" {
            vec![Scope::Project, Scope::Global]
        } else {
            vec![Scope::parse(Some(&raw_scope))]
        };
        for scope in scopes {
            let store = store(cwd, home, scope)?;
            if store.get(path).is_some() || file_name(path).ok().as_deref() == Some(INDEX) {
                return store.view(Some(path)).map_err(err);
            }
        }
        return Ok(format!(
            "No memory named '{path}' in the project or global store. View without a path to see what is there."
        ));
    }
    if raw_scope == "all" {
        let mut parts = Vec::new();
        for scope in [Scope::Project, Scope::Global] {
            parts.push(store(cwd, home, scope)?.view(None).map_err(err)?);
        }
        Ok(parts.join("\n\n"))
    } else {
        store(cwd, home, Scope::parse(Some(&raw_scope)))?.view(None).map_err(err)
    }
}

fn view_legacy(cwd: &Path, home: Option<&Path>, scope: Scope, path: Option<&str>) -> Result<String, ToolError> {
    let home = home_or_env(home)?;
    let inventory = legacy_inventory(cwd, &home);
    if let Some(name) = path.filter(|p| !p.trim().is_empty()) {
        let Some(entry) = inventory.find(scope, name) else {
            return Ok(format!("No old {} memory named '{name}'.", scope.label()));
        };
        if !entry.has_file {
            return Ok(format!(
                "{} — {} (recorded {}; index only, the file is gone)",
                entry.name, entry.description, entry.recorded
            ));
        }
        let text = std::fs::read_to_string(&entry.path).map_err(err)?;
        return Ok(format!(
            "=== legacy {} memory: {} (source: {}) ===\n{}",
            scope.label(),
            entry.name,
            entry.path.display(),
            text.trim_end()
        ));
    }
    let entries = inventory.by_scope(scope);
    if entries.is_empty() {
        return Ok(format!("No old {} memory to migrate.", scope.label()));
    }
    let mut out = format!("=== legacy {} memory ({}) ===\n", scope.label(), entries.len());
    for entry in entries {
        out.push_str(&format!(
            "- {} ({}, recorded {}) — {}{}\n",
            entry.name,
            entry.kind.label(),
            entry.recorded,
            entry.description,
            if entry.has_file { "" } else { " [index only; file gone]" }
        ));
    }
    out.push_str(&format!(
        "\nRead one with command \"view\", scope \"legacy-{}\" and its path.\n",
        scope.label()
    ));
    Ok(out)
}

fn create(cwd: &Path, home: Option<&Path>, args: &MemoryArgs) -> Result<String, ToolError> {
    let path = required(args.path.as_deref(), "path")?;
    let text = required(args.file_text.as_deref(), "file_text")?;
    let kind = args.kind.as_deref().map(Kind::parse);
    let raw_scope = args.scope.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if raw_scope.is_some_and(|s| s.eq_ignore_ascii_case("all")) {
        return Err(ToolError::Other("memory: a write needs one scope: project or global".into()));
    }
    let scope = flashagent_core::memory::default_scope(raw_scope, kind);
    let store = store(cwd, home, scope)?;
    let outcome = store
        .create(&path, &text, args.description.as_deref(), kind)
        .map_err(err)?;
    let mut out = format!(
        "Created {} memory '{}' ({} line(s)) and linked it from {INDEX}.",
        scope.label(),
        outcome.entry.path(),
        outcome.entry.line_count()
    );
    if let Some(similar) = outcome.similar {
        out.push_str(&format!(
            "\nA similar fact already exists: '{}' — {}. If this is the same fact, correct that one and delete this one with the memory tool.",
            similar.path(),
            similar.description
        ));
    }
    Ok(out)
}

fn target_store(cwd: &Path, home: Option<&Path>, args: &MemoryArgs, kind: Option<Kind>) -> Result<Store, ToolError> {
    let raw = args.scope.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if raw.is_some_and(|s| s.eq_ignore_ascii_case("all")) {
        return Err(ToolError::Other("memory: a write needs one scope: project or global".into()));
    }
    let scope = flashagent_core::memory::default_scope(raw, kind);
    store(cwd, home, scope)
}

/// A correction does not need the scope repeated: find the file in the
/// project first, then in global. An explicit scope always wins.
fn existing_store(cwd: &Path, home: Option<&Path>, args: &MemoryArgs) -> Result<Store, ToolError> {
    if args.scope.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        return target_store(cwd, home, args, None);
    }
    let Some(path) = args.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
        return target_store(cwd, home, args, None);
    };
    let project = store(cwd, home, Scope::Project)?;
    if project.get(path).is_some() {
        return Ok(project);
    }
    let global = store(cwd, home, Scope::Global)?;
    if global.get(path).is_some() {
        return Ok(global);
    }
    Ok(project)
}

fn replace(cwd: &Path, home: Option<&Path>, args: &MemoryArgs) -> Result<String, ToolError> {
    let path = required(args.path.as_deref(), "path")?;
    let old = required(args.old_str.as_deref(), "old_str")?;
    let new = args.new_str.as_deref().unwrap_or("");
    let store = existing_store(cwd, home, args)?;
    store.replace(&path, &old, new).map_err(err)?;
    Ok(format!("Replaced the text in {} memory '{}'.", store.scope().label(), path))
}

fn insert(cwd: &Path, home: Option<&Path>, args: &MemoryArgs) -> Result<String, ToolError> {
    let path = required(args.path.as_deref(), "path")?;
    let text = required(args.insert_text.as_deref(), "insert_text")?;
    let after = args.insert_line.unwrap_or(0);
    let store = existing_store(cwd, home, args)?;
    store.insert(&path, after, &text).map_err(err)?;
    Ok(format!(
        "Inserted {} line(s) into {} memory '{path}' after line {after}.",
        text.lines().count(),
        store.scope().label()
    ))
}

fn delete(cwd: &Path, home: Option<&Path>, args: &MemoryArgs) -> Result<String, ToolError> {
    let path = required(args.path.as_deref(), "path")?;
    let store = existing_store(cwd, home, args)?;
    if store.delete(&path).map_err(err)? {
        Ok(format!("Deleted {} memory '{}' and its index line.", store.scope().label(), path))
    } else {
        Ok(format!("There was no '{path}' in {} memory.", store.scope().label()))
    }
}

fn rename(cwd: &Path, home: Option<&Path>, args: &MemoryArgs) -> Result<String, ToolError> {
    let old = required(args.old_path.as_deref().or(args.path.as_deref()), "old_path")?;
    let new = required(args.new_path.as_deref(), "new_path")?;
    let store = existing_store(cwd, home, args)?;
    store.rename(&old, &new).map_err(err)?;
    Ok(format!("Renamed {} memory '{old}' to '{new}'.", store.scope().label()))
}

fn import(cwd: &Path, home: Option<&Path>, args: &MemoryArgs) -> Result<String, ToolError> {
    let from = Scope::parse(args.from.as_deref().or(args.scope.as_deref()));
    let path = required(args.path.as_deref(), "path")?;
    let kind = args.kind.as_deref().map(Kind::parse);
    let target = match args.scope.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        // `from` names the source. Without a target the fact stays where it
        // was, unless its type says it belongs to the person: a user
        // preference out of a project store goes global.
        None if kind.is_some_and(Kind::is_personal) => Scope::Global,
        None => from,
        Some(raw) if raw.eq_ignore_ascii_case("project") || raw.eq_ignore_ascii_case("global") => {
            Scope::parse(Some(raw))
        }
        // A single `scope` was given for an import and names the source.
        Some(_) => flashagent_core::memory::default_scope(args.scope.as_deref(), kind),
    };
    let target_store = store(cwd, home, target)?;
    let home = home_or_env(home)?;
    let entry = import_legacy(
        cwd,
        &home,
        &target_store,
        flashagent_core::memory_store::LegacyImport {
            scope: from,
            name: &path,
            to: args.to.as_deref(),
            kind,
            description: args.description.as_deref(),
        },
    )
    .map_err(err)?;
    Ok(format!(
        "Imported old {} memory '{}' as {} memory '{}' ({} line(s), recorded {}).",
        from.label(),
        path,
        target.label(),
        entry.path(),
        entry.line_count(),
        entry.recorded
    ))
}

/// The migration instruction the agent receives. It is bounded: it lists the
/// old entries, says where the backup is, and leaves every decision about
/// scope, type and wording to the model.
pub fn migration_prompt(inventory: &flashagent_core::LegacyInventory, backup: &Path) -> String {
    format!(
        "Migrate my old FlashAgent memory to the new Claude-Code-style memory. Work through every old entry, \
         do not stop at the first few.\n\n\
         Old entries ({}):\n{}\n\
         Read an old body with the memory tool: command \"view\", scope \"legacy-project\" or \"legacy-global\", path \"<name>\". \
         Copy one with command \"import\", from \"project\"|\"global\", path \"<name>\", optional to \"<new-name>\", \
         scope \"project\"|\"global\", type \"user\"|\"feedback\"|\"project\"|\"reference\", description \"one line\".\n\n\
         Rules: choose the new scope and type from the content, not from the old label; merge entries that say the same thing \
         (import one, then update it and delete the duplicate); keep dates and reasons; never invent a fact. \
         The old files stay on disk and a backup is at {}. Do not delete anything. \
         When every entry has been imported or deliberately merged, reply with a short report: moved, merged, left.",
        inventory.count(),
        inventory.listing(),
        backup.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_core::memory_store::legacy_inventory;

    fn goal_off() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn args(command: &str) -> MemoryArgs {
        MemoryArgs {
            command: command.into(),
            scope: None,
            path: None,
            file_text: None,
            description: None,
            kind: None,
            old_str: None,
            new_str: None,
            insert_line: None,
            insert_text: None,
            old_path: None,
            new_path: None,
            from: None,
            to: None,
        }
    }

    #[test]
    fn a_fact_can_be_created_read_replaced_and_deleted() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let mut a = args("create");
        a.path = Some("tests-use-nextest".into());
        a.file_text = Some("The suite is run with cargo nextest.".into());
        a.description = Some("the suite uses nextest".into());
        a.kind = Some("project".into());
        let said = memory(cwd.path(), Some(home.path()), &goal_off(), a).unwrap();
        assert!(said.contains("Created project memory"), "{said}");

        let mut v = args("view");
        v.scope = Some("project".into());
        v.path = Some("tests-use-nextest".into());
        let seen = memory(cwd.path(), Some(home.path()), &goal_off(), v).unwrap();
        assert!(seen.contains("cargo nextest"), "{seen}");

        let mut r = args("str_replace");
        r.path = Some("tests-use-nextest".into());
        r.old_str = Some("cargo nextest".into());
        r.new_str = Some("cargo nextest --all".into());
        memory(cwd.path(), Some(home.path()), &goal_off(), r).unwrap();
        let mut v = args("view");
        v.path = Some("tests-use-nextest".into());
        assert!(memory(cwd.path(), Some(home.path()), &goal_off(), v).unwrap().contains("--all"));

        let mut d = args("delete");
        d.path = Some("tests-use-nextest".into());
        assert!(memory(cwd.path(), Some(home.path()), &goal_off(), d).unwrap().contains("Deleted"));
    }

    #[test]
    fn a_personal_fact_defaults_to_global_and_a_project_fact_to_project() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let mut a = args("create");
        a.path = Some("prefers-short-replies".into());
        a.file_text = Some("The user prefers short replies.".into());
        a.kind = Some("user".into());
        assert!(memory(cwd.path(), Some(home.path()), &goal_off(), a).unwrap().contains("global memory"));

        let mut b = args("create");
        b.path = Some("build-is-just".into());
        b.file_text = Some("The build is driven by just.".into());
        assert!(memory(cwd.path(), Some(home.path()), &goal_off(), b).unwrap().contains("project memory"));
    }

    #[test]
    fn a_write_is_refused_during_a_goal() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let mut a = args("create");
        a.path = Some("x".into());
        a.file_text = Some("x".into());
        let on = Arc::new(AtomicBool::new(true));
        let err = memory(cwd.path(), Some(home.path()), &on, a).unwrap_err().to_string();
        assert!(err.contains("disabled in autonomous /goal mode"), "{err}");
    }

    #[test]
    fn a_path_outside_the_store_is_refused_out_loud() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        for bad in ["../../etc/passwd", "/tmp/x", "a/b"] {
            let mut a = args("create");
            a.path = Some(bad.into());
            a.file_text = Some("nothing".into());
            assert!(memory(cwd.path(), Some(home.path()), &goal_off(), a).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn an_old_entry_is_listed_and_imported_without_its_body_being_retyped() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cwd.path().join("memory")).unwrap();
        std::fs::write(
            cwd.path().join("memory").join("old.md"),
            "---\nname: old\ndescription: old fact\ntype: preference\nrecorded: 2025-02-03\n---\n\nKeep this body.\n",
        )
        .unwrap();
        let inventory = legacy_inventory(cwd.path(), home.path());
        assert_eq!(inventory.count(), 1);

        let mut v = args("view");
        v.scope = Some("legacy-project".into());
        assert!(memory(cwd.path(), Some(home.path()), &goal_off(), v).unwrap().contains("old fact"));

        let mut i = args("import");
        i.from = Some("project".into());
        i.scope = Some("project".into());
        i.path = Some("old".into());
        i.to = Some("new-name".into());
        i.kind = Some("feedback".into());
        let said = memory(cwd.path(), Some(home.path()), &goal_off(), i).unwrap();
        assert!(said.contains("Imported old project memory 'old'"), "{said}");
        let mut v = args("view");
        v.scope = Some("project".into());
        v.path = Some("new-name".into());
        let body = memory(cwd.path(), Some(home.path()), &goal_off(), v).unwrap();
        assert!(body.contains("Keep this body."), "{body}");
        assert!(body.contains("type: feedback"), "{body}");
    }

    #[test]
    fn the_migration_prompt_names_every_old_entry_and_the_backup() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cwd.path().join("memory")).unwrap();
        std::fs::write(cwd.path().join("memory").join("a.md"), "one").unwrap();
        std::fs::write(cwd.path().join("memory").join("b.md"), "two").unwrap();
        let inventory = legacy_inventory(cwd.path(), home.path());
        let prompt = migration_prompt(&inventory, Path::new("/backup/here"));
        assert!(prompt.contains("Old entries (2)"), "{prompt}");
        assert!(prompt.contains("- a ") && prompt.contains("- b "), "{prompt}");
        assert!(prompt.contains("/backup/here"), "{prompt}");
    }
}
