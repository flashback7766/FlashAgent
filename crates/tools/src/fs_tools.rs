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

/// The file a batch entry names, for telling `a.txt` and `./a.txt` apart
/// from two files.
pub(crate) fn same_file_key(cwd: &Path, path: &str) -> PathBuf {
    let file = resolve(cwd, path);
    file.canonicalize().unwrap_or(file)
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
    while number < offset.saturating_add(limit) {
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
    s.trim_start_matches('\u{feff}')
        .trim_end_matches(&['\r', '\n', ' ', '\t'][..])
        .replace(['‘', '’'], "'")
        .replace(['“', '”'], "\"")
}

/// Tolerates CRLF/LF, curly quotes and trailing whitespace differences, and
/// the line numbers read_file puts in front of each line when a model copies
/// them into old_string.
fn find_actual_string(text: &str, needle: &str) -> Option<String> {
    if text.contains(needle) {
        return Some(needle.to_string());
    }
    if let Some(found) = without_read_file_numbers(needle).and_then(|unnumbered| find_actual_string(text, &unnumbered)) {
        return Some(found);
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
            // The BOM stays where it is; it is not the model's to replace.
            let start = if i == 0 && text.starts_with('\u{feff}') { '\u{feff}'.len_utf8() } else { text_line_spans[i].0 };
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

    // The same block twice is one text: the caller counts it and asks for
    // replace_all, instead of saying it is not there.
    matches.sort();
    matches.dedup();
    if matches.len() == 1 {
        Some(matches.remove(0))
    } else {
        None
    }
}

/// new_string in the file's own line endings: models write `\n`, and a CRLF
/// file must not end up with both.
fn in_line_endings_of<'a>(text: &str, new: &'a str) -> std::borrow::Cow<'a, str> {
    let lines = text.matches('\n').count();
    let crlf = text.matches("\r\n").count();
    if lines > 0 && crlf * 2 > lines && new.contains('\n') && !new.contains('\r') {
        std::borrow::Cow::Owned(new.replace('\n', "\r\n"))
    } else {
        std::borrow::Cow::Borrowed(new)
    }
}

/// `     12\tcode` on every line, as read_file prints it; `None` otherwise.
fn without_read_file_numbers(needle: &str) -> Option<String> {
    let mut out = Vec::new();
    for line in needle.lines() {
        let (number, rest) = line.split_once('\t')?;
        if number.trim().is_empty() || !number.trim().chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        out.push(rest);
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// Said when old_string is not in the file: the line most like its first
/// line, so a model can copy the exact text instead of guessing again.
fn not_found(path: &str, text: &str, needle: &str) -> ToolError {
    let first = needle.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let bigrams = |s: &str| -> Vec<(char, char)> {
        let chars: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
        chars.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let want = bigrams(first);
    let closest = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let have = bigrams(l.trim());
            let shared = want.iter().filter(|b| have.contains(b)).count();
            let score = 2.0 * shared as f32 / (want.len() + have.len()).max(1) as f32;
            (score, i + 1, l)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0));
    let mut message = format!("edit: old_string not found in {path}");
    if let Some((_, line_no, line)) = closest.filter(|c| c.0 >= 0.5) {
        message.push_str(&format!(
            ". The closest line is {line_no}: `{}`. Copy old_string exactly from the file, without line numbers",
            line.trim_end()
        ));
    } else {
        message.push_str(". Read the file again and copy old_string exactly, without line numbers");
    }
    ToolError::Other(message)
}

/// The text with every edit applied in order, and how many replacements that
/// made; `path` only names the file in errors. Ambiguity is an error, not a
/// guess. Shared by the edit tools, the diff preview and the check made before
/// anyone is asked to approve an edit.
pub(crate) fn apply_edits(path: &str, mut text: String, edits: &[EditChunk]) -> Result<(String, usize), ToolError> {
    let mut applied = 0usize;
    for edit in edits {
        if edit.old_string.is_empty() {
            return Err(ToolError::Other("edit: old_string must not be empty".into()));
        }
        let Some(target) = find_actual_string(&text, &edit.old_string) else {
            return Err(not_found(path, &text, &edit.old_string));
        };
        let count = text.matches(target.as_str()).count();
        if count > 1 && !edit.replace_all {
            return Err(ToolError::Other(format!(
                "edit: old_string matches {count} times in {path}; add more context or set replace_all"
            )));
        }
        applied += if edit.replace_all { count } else { 1 };
        let new_string = in_line_endings_of(&text, &edit.new_string);
        text = if edit.replace_all {
            text.replace(target.as_str(), &new_string)
        } else {
            text.replacen(target.as_str(), &new_string, 1)
        };
    }
    Ok((text, applied))
}

pub(crate) fn edit_file(cwd: &Path, path: &str, edits: &[EditChunk]) -> Result<String, ToolError> {
    let file = resolve(cwd, path);
    let text = std::fs::read_to_string(&file)
        .map_err(|e| ToolError::Other(format!("read {path}: {e}")))?;
    let (text, applied) = apply_edits(path, text, edits)?;
    std::fs::write(&file, &text)?;
    Ok(format!("applied {applied} edit(s) to {path}"))
}

/// A batch is for a handful of related files, not the whole project.
const MAX_BATCH_FILES: usize = 20;

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
    write_planned(&plan_edits(cwd, files)?)
}

/// One file of a batch edit, changed in memory and not yet written.
struct PlannedFile {
    /// As the call named it.
    path: String,
    file: PathBuf,
    original: String,
    new: String,
    edits: usize,
}

fn plan_edits(cwd: &Path, files: &[(String, Vec<EditChunk>)]) -> Result<Vec<PlannedFile>, ToolError> {
    if files.is_empty() {
        return Err(ToolError::Other("edit_file: files is empty".into()));
    }
    if files.len() > MAX_BATCH_FILES {
        return Err(ToolError::Other(format!("edit_file: at most {MAX_BATCH_FILES} files per call, got {}", files.len())));
    }
    let mut planned: Vec<PlannedFile> = Vec::new();
    for (path, edits) in files {
        if edits.is_empty() {
            return Err(ToolError::Other(format!("nothing was changed: {path} has no edits")));
        }
        let file = resolve(cwd, path)
            .canonicalize()
            .map_err(|e| ToolError::Other(format!("nothing was changed: read {path}: {e}")))?;
        let nothing_changed = |e: ToolError| ToolError::Other(format!("nothing was changed: {path}: {e}"));
        // The same file twice: the second edits apply to the first's result.
        match planned.iter_mut().find(|p| p.file == file) {
            Some(earlier) => {
                let text = std::mem::take(&mut earlier.new);
                earlier.new = apply_edits(path, text, edits).map_err(nothing_changed)?.0;
                earlier.edits += edits.len();
            }
            None => {
                let original = std::fs::read_to_string(&file)
                    .map_err(|e| ToolError::Other(format!("nothing was changed: read {path}: {e}")))?;
                let new = apply_edits(path, original.clone(), edits).map_err(nothing_changed)?.0;
                planned.push(PlannedFile { path: path.clone(), file, original, new, edits: edits.len() });
            }
        }
    }
    Ok(planned)
}

/// A write that fails puts back every file written before it.
fn write_planned(planned: &[PlannedFile]) -> Result<String, ToolError> {
    for (i, target) in planned.iter().enumerate() {
        if let Err(e) = std::fs::write(&target.file, &target.new) {
            let rollback_failed: Vec<String> = planned[..i]
                .iter()
                .rev()
                .filter_map(|done| std::fs::write(&done.file, &done.original).err().map(|err| format!("{}: {err}", done.path)))
                .collect();
            let done = if rollback_failed.is_empty() {
                format!("rolled back {i} earlier file(s)")
            } else {
                format!("rollback failed for {}", rollback_failed.join(", "))
            };
            return Err(ToolError::Other(format!("write {}: {e}; {done}", target.path)));
        }
    }
    let total: usize = planned.iter().map(|p| p.edits).sum();
    let list = planned.iter().map(|p| format!("{} ({})", p.path, p.edits)).collect::<Vec<_>>().join(", ");
    Ok(format!("applied {total} edit(s) to {} file(s): {list}", planned.len()))
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

    fn chunk(old: &str, new: &str, replace_all: bool) -> EditChunk {
        EditChunk { old_string: old.into(), new_string: new.into(), replace_all }
    }

    #[test]
    fn an_edit_keeps_a_crlf_file_in_crlf() {
        let dir = tempdir();
        write_file(&dir, "w.txt", "one\r\ntwo\r\nthree\r\n").unwrap();
        edit_file(&dir, "w.txt", &[chunk("one\ntwo", "ONE\nTWO\nextra", false)]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("w.txt")).unwrap(), "ONE\r\nTWO\r\nextra\r\nthree\r\n");
        // An LF file stays LF.
        write_file(&dir, "u.txt", "a\nb\n").unwrap();
        edit_file(&dir, "u.txt", &[chunk("a", "a\nx", false)]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("u.txt")).unwrap(), "a\nx\nb\n");
    }

    #[test]
    fn a_block_found_twice_by_the_tolerant_match_is_counted_not_lost() {
        let dir = tempdir();
        write_file(&dir, "d.txt", "x = 1\r\ny = 2\r\nz\r\nx = 1\r\ny = 2\r\n").unwrap();
        let err = edit_file(&dir, "d.txt", &[chunk("x = 1\ny = 2", "q", false)]).unwrap_err().to_string();
        assert!(err.contains("2 times"), "{err}");
        edit_file(&dir, "d.txt", &[chunk("x = 1\ny = 2", "q", true)]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("d.txt")).unwrap(), "q\r\nz\r\nq\r\n");
    }

    #[test]
    fn a_byte_order_mark_neither_hides_line_one_nor_is_lost() {
        let dir = tempdir();
        write_file(&dir, "p.cs", "\u{feff}using A;\r\nusing B;\r\nclass C {}\r\n").unwrap();
        edit_file(&dir, "p.cs", &[chunk("using A;\nusing B;", "using A;\nusing B;\nusing D;", false)]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("p.cs")).unwrap(), "\u{feff}using A;\r\nusing B;\r\nusing D;\r\nclass C {}\r\n");
    }

    #[test]
    fn a_huge_limit_reads_to_the_end_instead_of_overflowing() {
        let dir = tempdir();
        write_file(&dir, "l.txt", "one\ntwo\n").unwrap();
        assert!(read_file(&dir, "l.txt", 1, usize::MAX).unwrap().contains("two"));
    }

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
    fn batch_edit_rolls_back_files_written_before_a_later_write_fails() {
        let dir = tempdir();
        let first = dir.join("first.txt");
        let blocked = dir.join("blocked.txt");
        std::fs::write(&first, "old first\n").unwrap();
        std::fs::write(&blocked, "old blocked\n").unwrap();

        let planned = plan_edits(
            &dir,
            &[
                ("first.txt".into(), vec![chunk("old first", "new first", false)]),
                ("blocked.txt".into(), vec![chunk("old blocked", "new blocked", false)]),
            ],
        )
        .unwrap();
        // A folder where the second file was read: no write gets through that, not
        // even as root, which a read-only file does not stop.
        std::fs::remove_file(&blocked).unwrap();
        std::fs::create_dir(&blocked).unwrap();
        let err = write_planned(&planned).unwrap_err().to_string();

        assert!(err.contains("blocked.txt") && err.contains("rolled back 1 earlier file(s)"), "{err}");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "old first\n");
        assert!(blocked.is_dir(), "nothing was written in place of the folder");
    }

    #[test]
    fn a_batch_edit_that_does_not_fit_says_where_the_text_is_closest() {
        let dir = tempdir();
        write_file(&dir, "a.rs", "fn one() {}\n").unwrap();
        write_file(&dir, "b.rs", "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();
        let err = edit_files(
            &dir,
            &[
                ("a.rs".into(), vec![chunk("fn one() {}", "fn uno() {}", false)]),
                ("b.rs".into(), vec![chunk("pub fn add(a: u32, b: u32) -> u32 {", "x", false)]),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(err.starts_with("nothing was changed: b.rs"), "{err}");
        assert!(err.contains("closest line is 1"), "{err}");
        assert_eq!(std::fs::read_to_string(dir.join("a.rs")).unwrap(), "fn one() {}\n");
    }

    #[test]
    fn replacements_are_counted_as_made() {
        let (text, applied) = apply_edits("f", "a a b".into(), &[chunk("a", "x", true), chunk("b", "y", false)]).unwrap();
        assert_eq!((text.as_str(), applied), ("x x y", 3));
        let err = apply_edits("f.txt", "a a".into(), &[chunk("a", "x", false)]).unwrap_err().to_string();
        assert!(err.contains("2 times in f.txt"), "{err}");
        assert!(apply_edits("f", "a".into(), &[chunk("", "x", false)]).is_err());
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
    fn line_numbers_copied_from_read_file_still_match() {
        let text = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let copied = "     1\tpub fn add(a: i32, b: i32) -> i32 {\n     2\t    a + b";
        assert_eq!(find_actual_string(text, copied).as_deref(), Some("pub fn add(a: i32, b: i32) -> i32 {\n    a + b"));
    }

    #[test]
    fn a_missing_old_string_points_at_the_closest_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "use std::io;\n\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();
        let edits = [EditChunk { old_string: "pub fn add(a: u32, b: u32) -> u32 {".into(), new_string: "x".into(), replace_all: false }];
        let err = edit_file(dir.path(), "lib.rs", &edits).unwrap_err().to_string();
        assert!(err.contains("closest line is 3: `pub fn add(a: i32, b: i32) -> i32 {`"), "{err}");
    }

    #[test]
    fn apply_edits_is_pure_and_validates() {
        let (out, _) = apply_edits("f", "one\ntwo\n".into(), &[chunk("two", "dos", false)]).unwrap();
        assert_eq!(out, "one\ndos\n");
        assert!(apply_edits("f", "abc".into(), &[chunk("zzz", "y", false)]).is_err());
        assert_eq!(apply_edits("f", "a".repeat(10), &[]).unwrap(), ("a".repeat(10), 0));
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
            ..Default::default()
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
        let (res, _) = apply_edits("f", text.to_string(), &[edit]).unwrap();
        assert!(res.contains("FlashAgent"));
    }
}
