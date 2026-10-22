//! A short plan of the project: what is where.
//!
//! The model used to spend turns looking for files: one call to list the
//! folder, another to list the folder inside it, another to read what it
//! wanted. The plan says the same thing in a few hundred tokens, so the first
//! call is already the right one.
//!
//! It is a plan, not the truth of the project: the files change while the
//! session runs, and the prompt says so.

use std::path::Path;

/// Folders that are never worth a line: builds, caches and version control
/// hold thousands of files that say nothing about the project.
const SKIP: [&str; 16] = [
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".next",
    ".turbo",
];

/// How many folders deep the plan goes. Three is where the shape of a project
/// shows: `crates/core/src` is deeper than that, and a Rust or TypeScript
/// project says nothing useful above it.
const DEPTH: usize = 3;

/// How many files of one folder are named.
const FILES_PER_FOLDER: usize = 10;

/// How many folders are named in the plan.
const FOLDERS: usize = 10;

/// How many entries are looked at before the walk gives up.
const MAX_ENTRIES: usize = 4000;

/// The plan of the project, or an empty string when there is nothing to say.
pub fn project_outline(cwd: &Path, max_chars: usize) -> String {
    let mut out = String::new();
    let mut budget = max_chars;

    let (root_files, _) = read_folder(cwd, 0);
    if !root_files.is_empty() {
        let line = format!("{}\n", root_files.join(", "));
        budget = budget.saturating_sub(line.len());
        out.push_str(&line);
    }

    // The folders with files in them, the ones with the most first: that is
    // where the work is.
    let mut folders: Vec<(String, Vec<String>, usize)> = Vec::new();
    collect_folders(cwd, "", 1, &mut folders);
    folders.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    for (path, files, count) in folders.iter().take(FOLDERS) {
        let line = format!("{path}/ ({count} files): {}\n", files.join(", "));
        if budget < line.len() {
            break;
        }
        budget -= line.len();
        out.push_str(&line);
    }

    out.trim_end().to_string()
}

/// The folders under a path that have files of their own.
fn collect_folders(root: &Path, prefix: &str, depth: usize, out: &mut Vec<(String, Vec<String>, usize)>) {
    if depth > DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || SKIP.contains(&name.as_str()) {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            names.push(name);
        }
    }
    names.sort();
    for name in names {
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let dir = root.join(&name);
        let (files, count) = read_folder(&dir, 0);
        if !files.is_empty() {
            out.push((path.clone(), files, count));
        }
        collect_folders(&dir, &path, depth + 1, out);
    }
}

/// The names of the files in a folder, and how many there are.
fn read_folder(path: &Path, depth: usize) -> (Vec<String>, usize) {
    let Ok(entries) = std::fs::read_dir(path) else { return (Vec::new(), 0) };
    let mut files = Vec::new();
    let mut count = 0usize;
    for entry in entries.flatten() {
        count += 1;
        if count > MAX_ENTRIES {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || SKIP.contains(&name.as_str()) {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            let _ = depth;
            continue;
        }
        files.push(name);
    }
    let count = files.len();
    files.sort();
    let mut shown = files;
    if shown.len() > FILES_PER_FOLDER {
        let more = shown.len() - FILES_PER_FOLDER;
        shown.truncate(FILES_PER_FOLDER);
        shown.push(format!("… and {more} more"));
    }
    (shown, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fa-outline-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crates/core/src")).unwrap();
        std::fs::create_dir_all(dir.join("crates/tui/src")).unwrap();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.join("README.md"), "# readme").unwrap();
        std::fs::write(dir.join("crates/core/src/lib.rs"), "pub fn x() {}").unwrap();
        std::fs::write(dir.join("crates/tui/src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("target/debug/big"), "noise").unwrap();
        std::fs::write(dir.join(".git/config"), "noise").unwrap();
        dir
    }

    #[test]
    fn the_plan_says_what_is_where_and_not_what_is_noise() {
        let dir = project("plan");
        let outline = project_outline(&dir, 4000);
        assert!(outline.contains("Cargo.toml"), "{outline}");
        assert!(outline.contains("README.md"), "{outline}");
        // The folder that has the code in it, not the one that only holds it.
        assert!(outline.contains("crates/core/src/"), "{outline}");
        assert!(outline.contains("lib.rs"), "{outline}");
        assert!(!outline.contains("target"), "the build folder is not the project: {outline}");
        assert!(!outline.contains(".git"), "the version control is not the project: {outline}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_is_not_there_gives_nothing() {
        assert_eq!(project_outline(Path::new("/nowhere/at/all"), 4000), "");
    }

    #[test]
    fn the_plan_stays_inside_its_budget() {
        let dir = project("budget");
        let outline = project_outline(&dir, 40);
        assert!(outline.len() <= 60, "the plan grew past its budget: {}", outline.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

