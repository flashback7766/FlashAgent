//! Filesystem tools over a fixed working directory. Sandboxing is the
//! permission layer's job, not this module's.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ToolError;

#[derive(Debug, Deserialize)]
pub struct EditChunk {
    pub old_string: String,
    pub new_string: String,
    /// Every occurrence instead of exactly one.
    #[serde(default)]
    pub replace_all: bool,
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    flashagent_core::resolve_path(cwd, path)
}

/// Returns 1-based numbered lines, reading one line at a time so a small
/// window of a big log stays cheap. A non-text file says so plainly: "stream
/// did not contain valid UTF-8" does not tell the model to use another tool.
pub(crate) fn read_file(cwd: &Path, path: &str, offset: usize, limit: usize) -> Result<String, ToolError> {
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(resolve(cwd, path))
        .map_err(|e| ToolError::Other(format!("read {path}: {e}")))?;
    let mut reader = BufReader::new(file);
    let mut out = String::new();
    let mut line = Vec::new();
    let mut number = 0usize;
    while number < offset + limit {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Err(ToolError::Other(format!("read {path}: {e}"))),
        }
        number += 1;
        if number <= offset {
            continue;
        }
        let text = std::str::from_utf8(&line).map_err(|_| {
            ToolError::Other(format!(
                "read {path}: this is not a text file (line {number} is not valid UTF-8). \
                 Use view_image for a picture, or run_shell with a tool that reads this format."
            ))
        })?;
        let _ = writeln!(out, "{:>6}\t{}", number, text.trim_end_matches(['\n', '\r']));
    }
    Ok(if out.is_empty() { "(empty or past end of file)".into() } else { out })
}

/// Creates parent directories as needed.
pub(crate) fn write_file(cwd: &Path, path: &str, content: &str) -> Result<String, ToolError> {
    let file = resolve(cwd, path);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&file, content)?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}

/// No line numbers; used for diff previews.
pub(crate) fn read_raw(cwd: &Path, path: &str) -> Option<String> {
    std::fs::read_to_string(resolve(cwd, path)).ok()
}

fn normalize_line(s: &str) -> String {
    s.trim_end_matches(&['\r', '\n', ' ', '\t'][..])
        .replace(['‘', '’'], "'")
        .replace(['“', '”'], "\"")
}

/// Tolerates CRLF/LF, curly quotes and trailing whitespace differences.
pub(crate) fn find_actual_string(text: &str, needle: &str) -> Option<String> {
    if text.contains(needle) {
        return Some(needle.to_string());
    }

    let needle_lines: Vec<String> = needle.lines().map(normalize_line).collect();
    if needle_lines.is_empty() {
        return None;
    }

    let mut text_line_spans = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let len = line.len();
        text_line_spans.push((offset, offset + len, normalize_line(line)));
        offset += len;
    }

    let k = needle_lines.len();
    if text_line_spans.len() < k {
        return None;
    }

    let mut matches = Vec::new();
    for i in 0..=(text_line_spans.len() - k) {
        let mut all_match = true;
        for j in 0..k {
            if text_line_spans[i + j].2 != needle_lines[j] {
                all_match = false;
                break;
            }
        }
        if all_match {
            let start = text_line_spans[i].0;
            let end = text_line_spans[i + k - 1].1;
            let span_str = &text[start..end];
            let target_str = if !needle.ends_with('\n') && span_str.ends_with('\n') {
                span_str.trim_end_matches(&['\r', '\n'][..])
            } else {
                span_str
            };
            matches.push(target_str.to_string());
        }
    }

    if matches.len() == 1 {
        Some(matches.remove(0))
    } else {
        None
    }
}

/// Ambiguity is an error, not a guess. Shared by [`edit_file`] and the diff preview.
pub(crate) fn apply_edits(mut text: String, edits: &[EditChunk]) -> Result<String, ToolError> {
    for edit in edits {
        if edit.old_string.is_empty() {
            return Err(ToolError::Other("edit: old_string must not be empty".into()));
        }
        let target_string = if text.contains(&edit.old_string) {
            edit.old_string.clone()
        } else if let Some(actual) = find_actual_string(&text, &edit.old_string) {
            actual
        } else {
            return Err(ToolError::Other("edit: old_string not found".into()));
        };

        let count = text.matches(&target_string).count();
        if count == 0 {
            return Err(ToolError::Other("edit: old_string not found".into()));
        }
        if count > 1 && !edit.replace_all {
            return Err(ToolError::Other(format!(
                "edit: old_string matches {count} times; add more context or set replace_all"
            )));
        }
        text = if edit.replace_all {
            text.replace(&target_string, &edit.new_string)
        } else {
            text.replacen(&target_string, &edit.new_string, 1)
        };
    }
    Ok(text)
}

pub(crate) fn edit_file(cwd: &Path, path: &str, edits: &[EditChunk]) -> Result<String, ToolError> {
    let file = resolve(cwd, path);
    let text = std::fs::read_to_string(&file)
        .map_err(|e| ToolError::Other(format!("read {path}: {e}")))?;
    let mut checked = text.clone();
    let mut applied = 0usize;
    for edit in edits {
        if edit.old_string.is_empty() {
            return Err(ToolError::Other("edit: old_string must not be empty".into()));
        }
        let target_string = if checked.contains(&edit.old_string) {
            edit.old_string.clone()
        } else if let Some(actual) = find_actual_string(&checked, &edit.old_string) {
            actual
        } else {
            return Err(ToolError::Other(format!("edit: old_string not found in {path}")));
        };
        let count = checked.matches(&target_string).count();
        if count == 0 {
            return Err(ToolError::Other(format!("edit: old_string not found in {path}")));
        }
        if count > 1 && !edit.replace_all {
            return Err(ToolError::Other(format!(
                "edit: old_string matches {count} times in {path}; add more context or set replace_all"
            )));
        }
        applied += if edit.replace_all { count } else { 1 };
        checked = if edit.replace_all {
            checked.replace(&target_string, &edit.new_string)
        } else {
            checked.replacen(&target_string, &edit.new_string, 1)
        };
    }
    let text = apply_edits(text, edits)?;
    std::fs::write(&file, &text)?;
    Ok(format!("applied {applied} edit(s) to {path}"))
}

/// A batch is for a handful of related files, not the whole project.
pub(crate) const MAX_BATCH_FILES: usize = 20;

/// An unreadable file is reported in its place; only a call where none could
/// be read is an error.
pub(crate) fn read_files(cwd: &Path, files: &[(String, usize, usize)]) -> Result<String, ToolError> {
    if files.is_empty() {
        return Err(ToolError::Other("read_file: files is empty".into()));
    }
    if files.len() > MAX_BATCH_FILES {
        return Err(ToolError::Other(format!("read_file: at most {MAX_BATCH_FILES} files per call, got {}", files.len())));
    }
    let mut out = String::new();
    let mut failed = 0usize;
    for (path, offset, limit) in files {
        let _ = writeln!(out, "=== {path} ===");
        match read_file(cwd, path, *offset, *limit) {
            Ok(text) => out.push_str(&text),
            Err(e) => {
                failed += 1;
                let _ = write!(out, "error: {e}");
            }
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    let out = out.trim_end().to_string();
    if failed == files.len() {
        return Err(ToolError::Other(out));
    }
    Ok(out)
}

/// All edits are applied in memory first; if any does not fit, no file is
/// written. A half-made change across files is worse than none.
pub(crate) fn edit_files(cwd: &Path, files: &[(String, Vec<EditChunk>)]) -> Result<String, ToolError> {
    if files.is_empty() {
        return Err(ToolError::Other("edit_file: files is empty".into()));
    }
    if files.len() > MAX_BATCH_FILES {
        return Err(ToolError::Other(format!("edit_file: at most {MAX_BATCH_FILES} files per call, got {}", files.len())));
    }
    // (as named, resolved, new text, edits applied)
    let mut changed: Vec<(String, PathBuf, String, usize)> = Vec::new();
    let mut originals: Vec<String> = Vec::new();
    for (path, edits) in files {
        if edits.is_empty() {
            return Err(ToolError::Other(format!("nothing was changed: {path} has no edits")));
        }
        let file = resolve(cwd, path)
            .canonicalize()
            .map_err(|e| ToolError::Other(format!("nothing was changed: read {path}: {e}")))?;
        // The same file twice: the second edits apply to the first's result.
        let at = changed.iter().position(|(_, f, _, _)| *f == file);
        let text = match at {
            Some(i) => changed[i].2.clone(),
            None => std::fs::read_to_string(&file)
                .map_err(|e| ToolError::Other(format!("nothing was changed: read {path}: {e}")))?,
        };
        let original = at.is_none().then(|| text.clone());
        let new = apply_edits(text, edits).map_err(|e| ToolError::Other(format!("nothing was changed: {path}: {e}")))?;
        match at {
            Some(i) => {
                changed[i].2 = new;
                changed[i].3 += edits.len();
            }
            None => {
                originals.push(original.expect("a new target has its original text"));
                changed.push((path.clone(), file, new, edits.len()));
            }
        }
    }
    let mut written: Vec<String> = Vec::new();
    for (path, file, text, _) in &changed {
        if let Err(e) = std::fs::write(file, text) {
            let mut rollback_failed = Vec::new();
            for i in (0..written.len()).rev() {
                if let Err(rollback_error) = std::fs::write(&changed[i].1, &originals[i]) {
                    rollback_failed.push(format!("{}: {rollback_error}", changed[i].0));
                }
            }
            let done = if rollback_failed.is_empty() {
                format!("rolled back {} earlier file(s)", written.len())
            } else {
                format!("rollback failed for {}", rollback_failed.join(", "))
            };
            return Err(ToolError::Other(format!("write {path}: {e}; {done}")));
        }
        written.push(path.clone());
    }
    let total: usize = changed.iter().map(|c| c.3).sum();
    let list = changed.iter().map(|(p, _, _, n)| format!("{p} ({n})")).collect::<Vec<_>>().join(", ");
    Ok(format!("applied {total} edit(s) to {} file(s): {list}", changed.len()))
}

/// Directories are suffixed with `/`.
pub(crate) fn list_dir(cwd: &Path, path: &str) -> Result<String, ToolError> {
    let dir = resolve(cwd, path);
    let mut entries: Vec<String> = Vec::new();
    let rd = std::fs::read_dir(&dir).map_err(|e| ToolError::Other(format!("list {path}: {e}")))?;
    for entry in rd {
        let entry = entry.map_err(|e| ToolError::Other(format!("entry: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() {
            entries.push(format!("{name}/"));
        } else {
            entries.push(name);
        }
    }
    entries.sort();
    Ok(if entries.is_empty() { "(empty)".into() } else { entries.join("\n") })
}

/// Relative to the cwd, capped at 500 results.
pub(crate) fn glob_files(cwd: &Path, pattern: &str) -> Result<String, ToolError> {
    // glob does not expand `~` itself.
    let pattern = flashagent_core::expand_home(pattern);
    let pattern = pattern.as_ref();
    let absolute = Path::new(pattern).is_absolute();
    let fixed = pattern.split(['*', '?', '[', '{']).next().unwrap_or("");
    let confine = !absolute && flashagent_core::path_is_inside(cwd, if fixed.is_empty() { "." } else { fixed });
    let full = if absolute {
        pattern.to_string()
    } else {
        format!("{}/{pattern}", cwd.display())
    };
    let paths = glob::glob(&full).map_err(|e| ToolError::Other(format!("bad pattern: {e}")))?;
    let root = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut out: Vec<String> = Vec::new();
    for p in paths.flatten() {
        if confine && !p.canonicalize().is_ok_and(|resolved| resolved.starts_with(&root)) {
            continue;
        }
        out.push(p.display().to_string());
        if out.len() >= 500 {
            out.push("...[limit 500 reached]".into());
            break;
        }
    }
    Ok(if out.is_empty() { "no matches".into() } else { out.join("\n") })
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 32 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if matches!(
                name,
                ".git" | "target" | "node_modules" | ".venv" | "__pycache__" | ".cache" | "dist" | "build"
            ) {
                continue;
            }
            walk(&path, out, depth + 1);
        } else if kind.is_file() {
            out.push(path);
        }
    }
}

use rayon::prelude::*;

/// Returns `path:line:text`, up to 200 matches. Parallel with Rayon.
pub(crate) fn grep(
    cwd: &Path,
    pattern: &str,
    glob_filter: Option<&str>,
    case_insensitive: bool,
) -> Result<String, ToolError> {
    let mut builder = regex::RegexBuilder::new(pattern);
    builder.case_insensitive(case_insensitive);
    let re = builder.build().map_err(|e| ToolError::Other(format!("bad regex: {e}")))?;
    let matcher = glob_filter
        .map(glob::Pattern::new)
        .transpose()
        .map_err(|e| ToolError::Other(format!("bad glob: {e}")))?;

    let mut all_files = Vec::new();
    walk(cwd, &mut all_files, 0);

    let candidate_files: Vec<PathBuf> = all_files
        .into_iter()
        .filter(|file| {
            if let Some(m) = &matcher {
                let rel = file.strip_prefix(cwd).unwrap_or(file);
                let rel_match = m.matches(&rel.to_string_lossy());
                let name_match = file
                    .file_name()
                    .is_some_and(|n| m.matches(&n.to_string_lossy()));
                if !rel_match && !name_match {
                    return false;
                }
            }
            if let Ok(meta) = file.metadata() {
                if meta.len() > 10 * 1024 * 1024 {
                    return false; // Skip huge binary / data files over 10MB
                }
            }
            true
        })
        .collect();

    let hits: Vec<String> = candidate_files
        .par_iter()
        .flat_map(|file| {
            let Ok(bytes) = std::fs::read(file) else { return Vec::new() };
            let check_len = bytes.len().min(1024);
            if bytes[..check_len].contains(&0) {
                return Vec::new();
            }
            let Ok(text) = std::str::from_utf8(&bytes) else { return Vec::new() };
            let mut file_hits = Vec::new();
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    let rel = file.strip_prefix(cwd).unwrap_or(file);
                    let rel_norm = rel.to_string_lossy().replace('\\', "/");
                    file_hits.push(format!("{rel_norm}:{}:{}", i + 1, line.trim()));
                    if file_hits.len() >= 50 {
                        break;
                    }
                }
            }
            file_hits
        })
        .take_any(200)
        .collect();

    Ok(if hits.is_empty() { "no matches".into() } else { hits.join("\n") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tempdir;

    #[test]
    fn read_write_edit_roundtrip() {
        let dir = tempdir();
        write_file(&dir, "a.txt", "one\ntwo\nthree\n").unwrap();

        let text = read_file(&dir, "a.txt", 0, 2000).unwrap();
        assert!(text.contains("     1\tone"));
        let sliced = read_file(&dir, "a.txt", 1, 1).unwrap();
        assert!(sliced.contains("two"));
        assert!(!sliced.contains("one"));

        edit_file(
            &dir,
            "a.txt",
            &[EditChunk { old_string: "two".into(), new_string: "dos".into(), replace_all: false }],
        )
        .unwrap();
        assert!(std::fs::read_to_string(dir.join("a.txt")).unwrap().contains("dos"));

        let err = edit_file(
            &dir,
            "a.txt",
            &[EditChunk { old_string: "zzz".into(), new_string: "y".into(), replace_all: false }],
        )
        .unwrap_err();
        assert!(err.to_string().contains("not found"));

        std::fs::write(dir.join("a.txt"), "x x x\n").unwrap();
        let err = edit_file(
            &dir,
            "a.txt",
            &[EditChunk { old_string: "x".into(), new_string: "y".into(), replace_all: false }],
        )
        .unwrap_err();
        assert!(err.to_string().contains("3 times"));
    }

    #[test]
    fn edit_file_applies_edits_in_sequence() {
        let dir = tempdir();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();

        edit_file(
            &dir,
            "a.txt",
            &[
                EditChunk { old_string: "one".into(), new_string: "two".into(), replace_all: false },
                EditChunk { old_string: "two".into(), new_string: "three".into(), replace_all: false },
            ],
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "three\n");
    }

    #[test]
    #[cfg(unix)]
    fn batch_edit_rolls_back_files_written_before_a_later_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir();
        let first = dir.join("first.txt");
        let blocked = dir.join("blocked.txt");
        std::fs::write(&first, "old first\n").unwrap();
        std::fs::write(&blocked, "old blocked\n").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o444)).unwrap();

        let result = edit_files(
            &dir,
            &[
                (
                    "first.txt".into(),
                    vec![EditChunk { old_string: "old first".into(), new_string: "new first".into(), replace_all: false }],
                ),
                (
                    "blocked.txt".into(),
                    vec![EditChunk { old_string: "old blocked".into(), new_string: "new blocked".into(), replace_all: false }],
                ),
            ],
        );

        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "old first\n");
        assert_eq!(std::fs::read_to_string(&blocked).unwrap(), "old blocked\n");
    }

    #[test]
    #[cfg(unix)]
    fn batch_edit_treats_symlink_aliases_as_one_file() {
        let dir = tempdir();
        std::fs::write(dir.join("actual.txt"), "one\n").unwrap();
        std::os::unix::fs::symlink(dir.join("actual.txt"), dir.join("alias.txt")).unwrap();

        edit_files(
            &dir,
            &[
                (
                    "actual.txt".into(),
                    vec![EditChunk { old_string: "one".into(), new_string: "two".into(), replace_all: false }],
                ),
                (
                    "alias.txt".into(),
                    vec![EditChunk { old_string: "two".into(), new_string: "three".into(), replace_all: false }],
                ),
            ],
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(dir.join("actual.txt")).unwrap(), "three\n");
    }

    #[test]
    fn glob_and_grep_work() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/code.rs"), "fn main() {}\nhello world\n").unwrap();
        std::fs::write(dir.join("top.txt"), "needle here\n").unwrap();

        let globs = glob_files(&dir, "**/*.rs").unwrap();
        assert!(globs.contains("code.rs"));

        let hits = grep(&dir, "hello", None, false).unwrap();
        assert!(hits.contains("sub/code.rs:2"));

        let hits = grep(&dir, "NEEDLE", Some("*.txt"), true).unwrap();
        assert!(hits.contains("top.txt:1"));
    }

    #[test]
    #[cfg(unix)]
    fn searches_do_not_follow_a_directory_symlink_outside_the_project() {
        let dir = tempdir();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "outside-only-marker\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("linked-outside")).unwrap();

        assert_eq!(grep(&dir, "outside-only-marker", None, false).unwrap(), "no matches");
        assert!(!glob_files(&dir, "**/*").unwrap().contains("secret.txt"));
        assert!(glob_files(&dir, "linked-outside/*.txt").unwrap().contains("secret.txt"));
    }

    #[test]
    fn list_dir_marks_directories() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        let out = list_dir(&dir, ".").unwrap();
        assert!(out.contains("inner/"));
        assert!(out.lines().all(|l| l != "f.txt/"));
    }

    #[test]
    fn apply_edits_is_pure_and_validates() {
        let out = apply_edits("one\ntwo\n".into(), &[EditChunk {
            old_string: "two".into(),
            new_string: "dos".into(),
            replace_all: false,
        }])
        .unwrap();
        assert_eq!(out, "one\ndos\n");
        assert!(apply_edits("abc".into(), &[EditChunk {
            old_string: "zzz".into(),
            new_string: "y".into(),
            replace_all: false,
        }])
        .is_err());
        assert_eq!(apply_edits("a".repeat(10), &[]).unwrap(), "a".repeat(10));
    }

    #[test]
    fn a_window_of_a_file_is_read_without_reading_the_rest() {
        let dir = tempdir();
        let lines: String = (1..=5000).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.join("big.txt"), &lines).unwrap();

        let window = read_file(&dir, "big.txt", 10, 3).unwrap();
        assert_eq!(window, "    11\tline 11\n    12\tline 12\n    13\tline 13\n");
        // Numbering counts from the start of the file, not from the window.
        assert!(read_file(&dir, "big.txt", 4999, 5).unwrap().contains("  5000\tline 5000"));
        assert_eq!(read_file(&dir, "big.txt", 9000, 5).unwrap(), "(empty or past end of file)");
    }

    #[test]
    fn line_endings_and_a_last_line_without_one_read_the_same() {
        let dir = tempdir();
        std::fs::write(dir.join("crlf.txt"), "one\r\ntwo\r\nthree").unwrap();
        let text = read_file(&dir, "crlf.txt", 0, 10).unwrap();
        assert_eq!(text, "     1\tone\n     2\ttwo\n     3\tthree\n");
    }

    #[test]
    fn a_file_that_is_not_text_says_so_instead_of_talking_about_utf_8() {
        let dir = tempdir();
        std::fs::write(dir.join("picture.png"), [0x89, b'P', b'N', b'G', 0xff, 0xfe, 0x00, 0x01]).unwrap();
        let err = read_file(&dir, "picture.png", 0, 100).unwrap_err().to_string();
        assert!(err.contains("not a text file"), "{err}");
        assert!(err.contains("view_image"), "the model should be told what to do instead: {err}");
    }

    #[test]
    fn a_path_under_the_home_folder_is_read_from_there_not_from_the_project() {
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
            return;
        };
        let home = PathBuf::from(home);
        // `~/x` means the home folder, never `<project>/~/x`.
        assert_eq!(resolve(Path::new("/work/project"), "~/x/main.rs"), home.join("x/main.rs"));
    }

    #[test]
    fn write_preview_produces_diffs_for_write_and_edit() {
        use crate::{BuiltinTools, BuiltinToolsConfig};
        use flashagent_llm::ToolCall;

        let dir = tempdir();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: dir,
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: None,
            web_enabled: None,
            context_window: None,
            mcp_manager: None,
        })
        .unwrap();

        let d = tools
            .preview_for(&ToolCall {
                id: "t".into(),
                name: "write_file".into(),
                args_json: r#"{"path":"a.txt","content":"one\nTWO\n"}"#.into(),
            })
            .unwrap();
        assert!(d.contains("-two") && d.contains("+TWO"), "got: {d}");

        let d = tools
            .preview_for(&ToolCall {
                id: "t".into(),
                name: "edit_file".into(),
                args_json: r#"{"path":"a.txt","edits":[{"old_string":"one","new_string":"ONE"}]}"#.into(),
            })
            .unwrap();
        assert!(d.contains("-one") && d.contains("+ONE"), "got: {d}");

        let d = tools
            .preview_for(&ToolCall {
                id: "t".into(),
                name: "write_file".into(),
                args_json: r#"{"path":"new.txt","content":"x"}"#.into(),
            })
            .unwrap();
        assert!(d.contains("/dev/null"));

        assert!(tools
            .preview_for(&ToolCall { id: "t".into(), name: "grep".into(), args_json: "{}".into() })
            .is_none());
        // Broken args: approval falls back to no preview.
        assert!(tools
            .preview_for(&ToolCall {
                id: "t".into(),
                name: "write_file".into(),
                args_json: "garbage".into(),
            })
            .is_none());
    }

    #[test]
    fn resilient_edits_crlf_and_trailing_whitespace() {
        let text = "fn hello() {\r\n    println!(\"world\");  \r\n}\r\n";
        let edit = EditChunk {
            old_string: "fn hello() {\n    println!(\"world\");\n}".into(),
            new_string: "fn hello() {\n    println!(\"FlashAgent\");\n}".into(),
            replace_all: false,
        };
        let res = apply_edits(text.to_string(), &[edit]).unwrap();
        assert!(res.contains("FlashAgent"));
    }
}
