//! Filesystem tools over a fixed working directory. Paths may be relative
//! (resolved against the cwd) or absolute; sandboxing by permission rules is
//! the A5 layer's job, not this module's.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ToolError;

/// One surgical replacement inside [`edit_file`].
#[derive(Debug, Deserialize)]
pub struct EditChunk {
    /// Exact text to find.
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace every occurrence instead of exactly one.
    #[serde(default)]
    pub replace_all: bool,
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}

/// Read a text file, returning 1-based numbered lines.
pub(crate) fn read_file(cwd: &Path, path: &str, offset: usize, limit: usize) -> Result<String, ToolError> {
    let text = std::fs::read_to_string(resolve(cwd, path))
        .map_err(|e| ToolError::Other(format!("read {path}: {e}")))?;
    let mut out = String::new();
    for (i, line) in text.lines().skip(offset).take(limit).enumerate() {
        let _ = writeln!(out, "{:>6}\t{}", offset + i + 1, line);
    }
    Ok(if out.is_empty() { "(empty or past end of file)".into() } else { out })
}

/// Create or overwrite a file, creating parent directories as needed.
pub(crate) fn write_file(cwd: &Path, path: &str, content: &str) -> Result<String, ToolError> {
    let file = resolve(cwd, path);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&file, content)?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}

/// Read a file as raw text (no line numbers); used for diff previews.
pub(crate) fn read_raw(cwd: &Path, path: &str) -> Option<String> {
    std::fs::read_to_string(resolve(cwd, path)).ok()
}

fn normalize_line(s: &str) -> String {
    s.trim_end_matches(&['\r', '\n', ' ', '\t'][..])
        .replace(['‘', '’'], "'")
        .replace(['“', '”'], "\"")
}

/// Finds the exact substring in `text` that corresponds to `needle`, with resilient
/// fallback for CRLF/LF line endings, curly quotes, and trailing whitespace differences.
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

/// Apply string replacements to text in memory. Ambiguity is an error, not a
/// guess. Shared by [`edit_file`] and the permission diff preview.
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

/// Apply string replacements sequentially; ambiguity is an error, not a guess.
pub(crate) fn edit_file(cwd: &Path, path: &str, edits: &[EditChunk]) -> Result<String, ToolError> {
    let file = resolve(cwd, path);
    let text = std::fs::read_to_string(&file)
        .map_err(|e| ToolError::Other(format!("read {path}: {e}")))?;
    let mut applied = 0usize;
    for edit in edits {
        if edit.old_string.is_empty() {
            return Err(ToolError::Other("edit: old_string must not be empty".into()));
        }
        let target_string = if text.contains(&edit.old_string) {
            edit.old_string.clone()
        } else if let Some(actual) = find_actual_string(&text, &edit.old_string) {
            actual
        } else {
            return Err(ToolError::Other(format!("edit: old_string not found in {path}")));
        };
        let count = text.matches(&target_string).count();
        if count == 0 {
            return Err(ToolError::Other(format!("edit: old_string not found in {path}")));
        }
        if count > 1 && !edit.replace_all {
            return Err(ToolError::Other(format!(
                "edit: old_string matches {count} times in {path}; add more context or set replace_all"
            )));
        }
        applied += if edit.replace_all { count } else { 1 };
    }
    let text = apply_edits(text, edits)?;
    std::fs::write(&file, &text)?;
    Ok(format!("applied {applied} edit(s) to {path}"))
}

/// List a directory; directories are suffixed with `/`.
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

/// Glob file search relative to the cwd, capped at 500 results.
pub(crate) fn glob_files(cwd: &Path, pattern: &str) -> Result<String, ToolError> {
    let full = if Path::new(pattern).is_absolute() {
        pattern.to_string()
    } else {
        format!("{}/{pattern}", cwd.display())
    };
    let paths = glob::glob(&full).map_err(|e| ToolError::Other(format!("bad pattern: {e}")))?;
    let mut out: Vec<String> = Vec::new();
    for p in paths.flatten() {
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
        if path.is_dir() {
            if matches!(
                name,
                ".git" | "target" | "node_modules" | ".venv" | "__pycache__" | ".cache" | "dist" | "build"
            ) {
                continue;
            }
            walk(&path, out, depth + 1);
        } else {
            out.push(path);
        }
    }
}

use rayon::prelude::*;

/// Regex search over text files; returns `path:line:text` up to 200 matches.
/// Parallelized across all CPU cores with Rayon for lightning-fast multi-core throughput.
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

    // Pre-filter files by glob matcher and file size (< 10MB)
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

    // Multi-core parallel file processing across all available CPU threads
    let hits: Vec<String> = candidate_files
        .par_iter()
        .flat_map(|file| {
            let Ok(bytes) = std::fs::read(file) else { return Vec::new() };
            // Fast binary check on first 1024 bytes
            let check_len = bytes.len().min(1024);
            if bytes[..check_len].contains(&0) {
                return Vec::new();
            }
            let Ok(text) = std::str::from_utf8(&bytes) else { return Vec::new() };
            let mut file_hits = Vec::new();
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    let rel = file.strip_prefix(cwd).unwrap_or(file).display();
                    file_hits.push(format!("{rel}:{}:{}", i + 1, line.trim()));
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
            &[EditChunk { old_string: "two".into(), new_string: "два".into(), replace_all: false }],
        )
        .unwrap();
        assert!(std::fs::read_to_string(dir.join("a.txt")).unwrap().contains("два"));

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
    fn glob_and_grep_work() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/код.rs"), "fn главная() {}\nпривет мир\n").unwrap();
        std::fs::write(dir.join("top.txt"), "needle here\n").unwrap();

        let globs = glob_files(&dir, "**/*.rs").unwrap();
        assert!(globs.contains("код.rs"));

        let hits = grep(&dir, "привет", None, false).unwrap();
        assert!(hits.contains("sub/код.rs:2"));

        let hits = grep(&dir, "NEEDLE", Some("*.txt"), true).unwrap();
        assert!(hits.contains("top.txt:1"));
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
            new_string: "два".into(),
            replace_all: false,
        }])
        .unwrap();
        assert_eq!(out, "one\nдва\n");
        assert!(apply_edits("abc".into(), &[EditChunk {
            old_string: "zzz".into(),
            new_string: "y".into(),
            replace_all: false,
        }])
        .is_err());
        // Original text untouched on error (purity).
        assert_eq!(apply_edits("a".repeat(10).into(), &[]).unwrap(), "a".repeat(10));
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

        // New file → /dev/null header.
        let d = tools
            .preview_for(&ToolCall {
                id: "t".into(),
                name: "write_file".into(),
                args_json: r#"{"path":"new.txt","content":"x"}"#.into(),
            })
            .unwrap();
        assert!(d.contains("/dev/null"));

        // Non-write tools → None.
        assert!(tools
            .preview_for(&ToolCall { id: "t".into(), name: "grep".into(), args_json: "{}".into() })
            .is_none());
        // Broken args → None (falls back to approval without preview).
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
        // Needle has unix LF and lacks trailing whitespace
        let edit = EditChunk {
            old_string: "fn hello() {\n    println!(\"world\");\n}".into(),
            new_string: "fn hello() {\n    println!(\"FlashAgent\");\n}".into(),
            replace_all: false,
        };
        let res = apply_edits(text.to_string(), &[edit]).unwrap();
        assert!(res.contains("FlashAgent"));
    }
}
