//! Filesystem tools over a fixed working directory. Sandboxing is the
//! permission layer's job, not this module's.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::ToolError;

#[derive(Debug, Deserialize)]
#[derive(Default)]
pub struct EditChunk {
    /// The text to replace, exactly as in the file. Not needed when the edit
    /// names lines.
    #[serde(default)]
    pub old_string: String,
    /// The text that replaces it. With a line range it replaces those lines
    /// whole, with the indentation it should have.
    #[serde(default)]
    pub new_string: String,
    /// Every occurrence instead of exactly one.
    #[serde(default)]
    pub replace_all: bool,
    /// `old_string` is a regular expression, and `new_string` may use `$1` and
    /// `${name}` to put the matched parts back.
    #[serde(default)]
    pub regex: bool,
    /// The first line to replace, counted from 1.
    #[serde(default)]
    pub start_line: Option<usize>,
    /// The last line to replace, counted from 1 and included.
    #[serde(default)]
    pub end_line: Option<usize>,
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

/// How an io failure is put to the model.
///
/// A file that is not there is not a dead end, and a model handed "No such file
/// or directory (os error 2)" answers it the way a person does: with a sentence
/// about the file, or by asking which one. A model handed the near miss makes
/// the corrected call without being told to. In `docs/tool-calling.md` this one
/// scenario is what a small local model either recovers from or does not, and it
/// is the reason for this shape rather than the raw error.
fn io_error(action: &str, cwd: &Path, path: &str, e: &std::io::Error) -> ToolError {
    if e.kind() != std::io::ErrorKind::NotFound {
        return ToolError::Other(format!("{action} {path}: {e}"));
    }
    match nearest_sibling(cwd, path) {
        Some(near) => ToolError::Other(format!("{action} {path}: no such file (did you mean {near}?)")),
        None => ToolError::Other(format!("{action} {path}: no such file")),
    }
}

fn nearest_sibling(cwd: &Path, path: &str) -> Option<String> {
    nearest_sibling_of(&resolve(cwd, path), path)
}

/// The nearest name in the same directory, if one is close enough to be a
/// misspelling rather than a different file.
fn nearest_sibling_of(full: &Path, written: &str) -> Option<String> {
    let wanted = full.file_name()?.to_string_lossy().to_string();
    let mut best: Option<(usize, String)> = None;
    for entry in std::fs::read_dir(full.parent()?).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Half the word apart is a different name; a typo is closer than that.
        let distance = edit_distance(&wanted.to_lowercase(), &name.to_lowercase());
        if distance * 2 >= wanted.len().max(name.len()) + 2 {
            continue;
        }
        if best.as_ref().is_none_or(|(best, _)| distance < *best) {
            best = Some((distance, name));
        }
    }
    // Answered in the shape the model wrote the path, so the corrected call is
    // a paste rather than something to reformat. The separators follow the
    // written shape too: the platform's own join would turn the written
    // `src/config.rs` into `src\config.rs` on Windows.
    let name = best?.1;
    Some(match Path::new(written).parent() {
        Some(dir) if !dir.as_os_str().is_empty() => {
            join_in_written_shape(dir.to_string_lossy(), &name, written)
        }
        _ => name,
    })
}

/// The directory and the corrected name put back together with the separator
/// the written path used: a path with `/` keeps `/`, one with `\` keeps `\`.
fn join_in_written_shape(dir: std::borrow::Cow<'_, str>, name: &str, written: &str) -> String {
    let sep = if written.contains('/') {
        '/'
    } else if written.contains('\\') {
        '\\'
    } else {
        '/'
    };
    let dir = dir.trim_end_matches(['/', '\\']);
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}{sep}{name}")
    }
}

/// [`io_error`] for a caller that already resolved the path.
fn io_error_at(action: &str, full: &Path, written: &str, e: &std::io::Error) -> ToolError {
    if e.kind() != std::io::ErrorKind::NotFound {
        return ToolError::Other(format!("{action} {written}: {e}"));
    }
    match nearest_sibling_of(full, written) {
        Some(near) => ToolError::Other(format!("{action} {written}: no such file (did you mean {near}?)")),
        None => ToolError::Other(format!("{action} {written}: no such file")),
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut next = vec![i + 1];
        for (j, &cb) in b.iter().enumerate() {
            next.push((row[j] + usize::from(ca != cb)).min(row[j + 1] + 1).min(next[j] + 1));
        }
        row = next;
    }
    row.last().copied().unwrap_or(0)
}

/// Returns 1-based numbered lines, reading one line at a time so a small
/// window of a big log stays cheap. A non-text file says so plainly: "stream
/// did not contain valid UTF-8" does not tell the model to use another tool.
pub(crate) fn read_file(cwd: &Path, path: &str, offset: usize, limit: usize) -> Result<String, ToolError> {
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(resolve(cwd, path))
        .map_err(|e| io_error("read", cwd, path, &e))?;
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
    write_atomic(&file, content)?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}

/// Writes a file in one step: the new text goes into a temporary file beside
/// it and is renamed over the old one.
///
/// `std::fs::write` truncates the file first and writes afterwards, so a
/// failure in between (a full disk, a quota, a killed process) left a
/// truncated file behind and the old text was gone for good. Here the old
/// file is whole until the moment it is replaced, and it is replaced in one
/// step, so a reader never sees half of it.
pub(crate) fn write_atomic(file: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;

    // A symlink is written through, not replaced: the file it points at is
    // the one the caller means.
    let target = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let temp = dir.join(format!(".{name}.flashagent-{}-{}.tmp", std::process::id(), next_temp_id()));

    let result = (|| -> std::io::Result<()> {
        // The file's own permissions decide whether it may be written: a
        // rename would replace it whatever they say.
        if let Ok(meta) = std::fs::metadata(&target) {
            if meta.is_file() {
                std::fs::OpenOptions::new().write(true).open(&target)?;
            }
        }
        let mut handle = std::fs::File::create(&temp)?;
        handle.write_all(content.as_bytes())?;
        // The bytes are on the disk before the name points at them.
        handle.sync_all()?;
        drop(handle);
        // The permissions the file had are the ones it keeps.
        if let Ok(meta) = std::fs::metadata(&target) {
            let _ = std::fs::set_permissions(&temp, meta.permissions());
        }
        std::fs::rename(&temp, &target)
    })();
    if result.is_err() {
        // A temporary file left behind is litter in the user's project.
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// A number that tells two temporary files of the same process apart.
fn next_temp_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
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

/// Leading indentation is dropped. A model transcribes a block from what it
/// read and routinely loses or invents leading spaces; that is a mistake about
/// layout, not about the code being edited, and refusing the edit over it sends
/// the model round again to no purpose.
fn normalize_line_loose(s: &str) -> String {
    normalize_line(s).trim_start().to_string()
}

/// Tolerates CRLF/LF, curly quotes and trailing whitespace differences, and
/// the line numbers read_file puts in front of each line when a model copies
/// them into old_string.
fn find_actual_string(text: &str, needle: &str) -> Option<(String, Vec<String>)> {
    if text.contains(needle) {
        return Some((needle.to_string(), Vec::new()));
    }
    if let Some(found) = without_read_file_numbers(needle).and_then(|unnumbered| find_actual_string(text, &unnumbered)) {
        return Some(found);
    }
    match_lines(text, needle, false).or_else(|| match_lines(text, needle, true))
}

/// The one run of lines `needle` describes, or `None` if it describes none or
/// more than one. `loose` stops insisting on leading whitespace.
///
/// When it is set, the file's own indentation of the matched lines comes back
/// with the match. A model that under-indents its `old_string` would
/// otherwise under-indent the `new_string` too, and in Python or YAML that
/// silently changes what the code means — worse than refusing the edit.
fn match_lines(text: &str, needle: &str, loose: bool) -> Option<(String, Vec<String>)> {
    let line_of = |s: &str| if loose { normalize_line_loose(s) } else { normalize_line(s) };
    let indent_of = |s: &str| s.len() - s.trim_start().len();
    let indents_of = |s: &str| s[..indent_of(s)].to_string();

    let needle_lines: Vec<String> = needle.lines().map(line_of).collect();
    if needle_lines.is_empty() {
        return None;
    }

    let mut text_line_spans = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let len = line.len();
        text_line_spans.push((offset, offset + len, line_of(line), indents_of(line)));
        offset += len;
    }

    let k = needle_lines.len();
    if text_line_spans.len() < k {
        return None;
    }

    let mut matches: Vec<(String, Vec<String>)> = Vec::new();
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
            let indents = if loose {
                (0..k).map(|j| text_line_spans[i + j].3.clone()).collect()
            } else {
                Vec::new()
            };
            matches.push((target_str.to_string(), indents));
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

/// `new` re-indented to the file's own layout. Only lines the model wrote with
/// some indentation are touched; a blank line stays blank. This runs only when
/// the match needed the loose tier, so an exact match is never disturbed.
fn with_file_indent(new: &str, indents: &[String]) -> String {
    if indents.is_empty() {
        return new.to_string();
    }
    new.split_inclusive('\n')
        .enumerate()
        .map(|(i, line)| {
            let Some(indent) = indents.get(i) else { return line.to_string() };
            if line.trim().is_empty() {
                return line.to_string();
            }
            format!("{indent}{}", line.trim_start())
        })
        .collect()
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
    if let Some((_score, line_no, line)) = closest.filter(|c| c.0 >= 0.5) {
        message.push_str(&format!(
            ". The closest line is {line_no}: `{}`",
            line.trim_end()
        ));
        // A near miss is worth naming: the model wrote a line it believes is
        // in the file, and being told what it got wrong beats being told to
        // try again.
        if let Some(differs) = first_difference(first, line.trim()) {
            message.push_str(&format!(". You wrote {differs}"));
        }
        message.push_str(". Copy old_string exactly from the file, without line numbers");
    } else {
        message.push_str(". Read the file again and copy old_string exactly, without line numbers");
    }
    ToolError::Other(message)
}

/// The first word of `want` that is not in `have`, named so the model can see
/// what it got wrong; `None` when the lines are different lines rather than one
/// misremembered one. One added, dropped or swapped word is the case worth
/// naming — `pub` that the file does not have — and anything further off is
/// noise that would send the model guessing again.
fn first_difference(want: &str, have: &str) -> Option<String> {
    let w: Vec<&str> = want.split_whitespace().collect();
    let h: Vec<&str> = have.split_whitespace().collect();
    if w.is_empty() || h.is_empty() {
        return None;
    }
    if w.len() == h.len() {
        let differing: Vec<usize> = (0..w.len()).filter(|i| w[*i] != h[*i]).collect();
        let [i] = differing[..] else { return None };
        return Some(format!("`{}` where the file has `{}`", w[i], h[i]));
    }
    // A word added or dropped: the rest has to line up exactly, or these are
    // simply different lines.
    let (longer, shorter, mine_is_longer) = if w.len() > h.len() { (&w, &h, true) } else { (&h, &w, false) };
    if longer.len() != shorter.len() + 1 {
        return None;
    }
    let rest_matches_without = |skip: usize| {
        longer
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != skip)
            .map(|(_, word)| *word)
            .eq(shorter.iter().copied())
    };
    let skip = (0..=shorter.len()).find(|skip| rest_matches_without(*skip))?;
    let extra = longer[skip];
    if mine_is_longer {
        Some(format!("`{extra}` on a line the file has without it"))
    } else {
        Some(format!("the file has `{extra}` on this line"))
    }
}

/// The text with every edit applied in order, and how many replacements that
/// made; `path` only names the file in errors. Ambiguity is an error, not a
/// guess. Shared by the edit tools, the diff preview and the check made before
/// anyone is asked to approve an edit.
pub(crate) fn apply_edits(path: &str, mut text: String, edits: &[EditChunk]) -> Result<(String, usize), ToolError> {
    // Several cursors in one call: every edit is found in the text as it was,
    // and the edits are then applied from the last to the first. A line number
    // or a match found earlier keeps its place while the ones after it change,
    // and one edit that shifts the file cannot move the next.
    if let Some(planned) = plan_all_edits(path, &text, edits) {
        // Two edits that cover the same bytes would each overwrite the other,
        // and the second one to be applied would win while the first was lost
        // without a word. That is a silent loss, so it is refused instead.
        if let Some((a, b, at)) = overlapping_places(&planned) {
            return Err(ToolError::Other(format!(
                "edit: two edits in this call cover the same part of {path} \
                 (edit {} and edit {}, at line {}); apply them one at a time, \
                 or make them cover different text",
                a + 1,
                b + 1,
                line_of(&text, at),
            )));
        }
        let mut applied = 0usize;
        // The edits are applied from the end of the file towards its start,
        // whatever order the model listed them in. Every edit was planned
        // against the text as it was, so an edit that makes the file longer
        // must not move the ones before it — and the order in the list says
        // nothing about where in the file the edits are.
        let mut places: Vec<(usize, usize, String)> =
            planned.iter().flat_map(|edit| edit.places.iter().cloned()).collect();
        places.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        for (from, to, new_text) in places {
            text.replace_range(from..to, &new_text);
            applied += 1;
        }
        return Ok((text, applied));
    }
    // An edit that cannot be found in the text as it was depends on the one
    // before it -- the second edit renames what the first wrote. Those are
    // applied one after another, as they always were.
    let mut applied = 0usize;
    for edit in edits {
        let planned = plan_one_edit(path, &text, edit)?;
        for (from, to, new_text) in planned.places.into_iter().rev() {
            text.replace_range(from..to, &new_text);
            applied += 1;
        }
    }
    Ok((text, applied))
}

/// Every edit resolved against the text as it was, or `None` when one of them
/// cannot be found that way.
fn plan_all_edits(path: &str, text: &str, edits: &[EditChunk]) -> Option<Vec<PlannedEdit>> {
    let mut planned = Vec::with_capacity(edits.len());
    for edit in edits {
        planned.push(plan_one_edit(path, text, edit).ok()?);
    }
    Some(planned)
}

/// Two edits that would write over the same bytes, and where they meet.
///
/// The edits of one call are all planned against the text as it was, and then
/// applied from the last to the first. That is what makes several cursors
/// possible, and it is also what makes two edits over the same text lose one
/// of them: the one applied later wins and the other is gone without a word.
/// The file is left in a state nobody asked for, and the model is told the
/// edits were applied. So they are found here instead, and refused.
fn overlapping_places(planned: &[PlannedEdit]) -> Option<(usize, usize, usize)> {
    // Each place with the edit it belongs to, so a match inside one edit (the
    // several places `replace_all` makes) is not mistaken for a conflict.
    let mut all: Vec<(usize, usize, usize)> = Vec::new();
    for (n, edit) in planned.iter().enumerate() {
        for (from, to, _) in &edit.places {
            all.push((*from, *to, n));
        }
    }
    all.sort_unstable();
    for pair in all.windows(2) {
        let (_from_a, to_a, edit_a) = pair[0];
        let (from_b, _to_b, edit_b) = pair[1];
        // Sorted by start, so the second starts no earlier than the first.
        // The bytes they share are the overlap.
        if edit_a != edit_b && to_a > from_b {
            return Some((edit_a, edit_b, from_b));
        }
    }
    None
}

/// The line a byte offset falls on, counted from 1.
fn line_of(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].matches('\n').count() + 1
}

/// One edit, resolved against the text as it was before any edit.
struct PlannedEdit {
    /// Where the text goes and what goes there: the bytes it replaces and the
    /// text that takes their place.
    places: Vec<(usize, usize, String)>,
}

/// What one edit does to the file.
fn plan_one_edit(path: &str, text: &str, edit: &EditChunk) -> Result<PlannedEdit, ToolError> {
    // By line numbers: the lines themselves, counted from 1, the last one
    // included.
    if edit.start_line.is_some() || edit.end_line.is_some() {
        let start = edit.start_line.unwrap_or(1);
        let end = edit.end_line.unwrap_or(start);
        if start == 0 {
            return Err(ToolError::Other("edit: lines are counted from 1".into()));
        }
        if end < start {
            return Err(ToolError::Other(format!("edit: line {end} is before line {start}")));
        }
        let Some((from, to)) = line_span(text, start, end) else {
            return Err(ToolError::Other(format!("edit: lines {start}-{end} are not in {path}")));
        };
        // The text the model wrote carries the indentation it wants: the lines
        // it replaces are known by number, not by a match.
        let mut new_text = in_line_endings_of(text, &edit.new_string).into_owned();
        // The lines that went had a line ending after them; the text that
        // takes their place needs one too, or the next line joins it.
        let replaced = &text[from..to];
        if replaced.ends_with('\n') && !new_text.ends_with('\n') {
            let ending = if replaced.ends_with("\r\n") { "\r\n" } else { "\n" };
            new_text.push_str(ending);
        }
        return Ok(PlannedEdit { places: vec![(from, to, new_text)] });
    }
    if edit.old_string.is_empty() {
        return Err(ToolError::Other("edit: old_string must not be empty".into()));
    }
    if edit.regex {
        return plan_regex_edit(path, text, edit);
    }
    let Some((target, indents)) = find_actual_string(text, &edit.old_string) else {
        return Err(not_found(path, text, &edit.old_string));
    };
    let count = text.matches(target.as_str()).count();
    if count > 1 && !edit.replace_all {
        return Err(ToolError::Other(format!(
            "edit: old_string matches {count} times in {path}; add more context or set replace_all"
        )));
    }
    let reindented = with_file_indent(&edit.new_string, &indents);
    let new_text = in_line_endings_of(text, &reindented).into_owned();
    let places = if edit.replace_all {
        text.match_indices(target.as_str())
            .map(|(at, _)| (at, at + target.len(), new_text.clone()))
            .collect()
    } else {
        // The match was found a moment ago; if it is not there now, the text
        // said one thing and the model another, and that is an error to report
        // rather than a panic to hand the model.
        let at = text.find(target.as_str()).ok_or_else(|| {
            ToolError::Other(format!("edit: the text to replace is not in {path}"))
        })?;
        vec![(at, at + target.len(), new_text)]
    };
    Ok(PlannedEdit { places })
}

/// A regular expression: every match, or the first one, with the groups put
/// back where the replacement says.
fn plan_regex_edit(path: &str, text: &str, edit: &EditChunk) -> Result<PlannedEdit, ToolError> {
    let re = regex::Regex::new(&edit.old_string)
        .map_err(|e| ToolError::Other(format!("edit: the pattern is not a regular expression: {e}")))?;
    let mut places: Vec<(usize, usize, String)> = Vec::new();
    for m in re.find_iter(text) {
        let Some(caps) = re.captures_at(text, m.start()) else { continue };
        let mut buf = String::new();
        caps.expand(&edit.new_string, &mut buf);
        let expanded = in_line_endings_of(text, &buf).into_owned();
        places.push((m.start(), m.end(), expanded));
    }
    if places.is_empty() {
        return Err(not_found(path, text, &edit.old_string));
    }
    if places.len() > 1 && !edit.replace_all {
        return Err(ToolError::Other(format!(
            "edit: the pattern matches {} times in {path}; set replace_all",
            places.len()
        )));
    }
    Ok(PlannedEdit { places })
}

/// The bytes of the lines `start..=end`, counted from 1. The last line's own
/// line ending goes with it, so a replacement does not eat it.
fn line_span(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut begin = None;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let n = n + 1;
        if n == start {
            begin = Some(offset);
        }
        if n == end {
            return Some((begin?, offset + line.len()));
        }
        offset += line.len();
    }
    None
}

pub(crate) fn edit_file(cwd: &Path, path: &str, edits: &[EditChunk]) -> Result<String, ToolError> {
    let file = resolve(cwd, path);
    let text = std::fs::read_to_string(&file)
        .map_err(|e| io_error("read", cwd, path, &e))?;
    let (text, applied) = apply_edits(path, text, edits)?;
    write_atomic(&file, &text)?;
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

/// A write that fails puts back every file written before it, and the file
/// whose write failed is whole: a write is a rename, so the old text is still
/// there until the moment it is replaced.
fn write_planned(planned: &[PlannedFile]) -> Result<String, ToolError> {
    for (i, target) in planned.iter().enumerate() {
        if let Err(e) = write_atomic(&target.file, &target.new) {
            let mut rollback_failed: Vec<String> = Vec::new();
            // The file whose write failed is whole: a write is a rename, so
            // the old text is still there until the moment it is replaced.
            // It is put back anyway, for the case where something else
            // touched it in between.
            if target.file.is_file() {
                if let Err(err) = write_atomic(&target.file, &target.original) {
                    rollback_failed.push(format!("{}: {err}", target.path));
                }
            }
            for done in planned[..i].iter().rev() {
                if let Err(err) = write_atomic(&done.file, &done.original) {
                    rollback_failed.push(format!("{}: {err}", done.path));
                }
            }
            let done = if rollback_failed.is_empty() {
                format!("rolled back {i} earlier file(s)")
            } else {
                format!("rollback failed for {}", rollback_failed.join(", "))
            };
            return Err(match io_error_at("write", &target.file, &target.path, &e) {
                ToolError::Other(mut message) => {
                    let _ = write!(message, "; {done}");
                    ToolError::Other(message)
                }
                other => other,
            });
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
    let rd = std::fs::read_dir(&dir).map_err(|e| io_error("list", cwd, path, &e))?;
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
    // A relative pattern stays inside the project, whatever its first part
    // is. It used to be decided by the part before the first wildcard, so a
    // pattern that named a symlink pointing out of the project (or one that
    // climbed out of it with `..`) switched the filter off — while `grep`,
    // which never follows a symlink, kept looking inside. The two tools
    // answered the same question differently.
    let confine = !absolute;
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

const MAX_GREP_HITS: usize = 200;
const MAX_GREP_HITS_PER_FILE: usize = 50;
/// Searched in parallel, one chunk at a time, so a search with many hits
/// stops early.
const GREP_CHUNK: usize = 256;

/// Returns `path:line:text`, up to 200 matches, in path and line order: the
/// same search always gives the same answer.
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

    let mut files = Vec::new();
    walk(cwd, &mut files, 0);
    if let Some(m) = &matcher {
        files.retain(|file| {
            let rel = file.strip_prefix(cwd).unwrap_or(file);
            m.matches(&rel.to_string_lossy()) || file.file_name().is_some_and(|n| m.matches(&n.to_string_lossy()))
        });
    }
    files.sort();

    let mut hits: Vec<String> = Vec::new();
    'chunks: for chunk in files.chunks(GREP_CHUNK) {
        let found: Vec<Vec<String>> = chunk.par_iter().map(|file| grep_file(cwd, file, &re)).collect();
        for hit in found.into_iter().flatten() {
            hits.push(hit);
            if hits.len() >= MAX_GREP_HITS {
                break 'chunks;
            }
        }
    }
    Ok(if hits.is_empty() { "no matches".into() } else { hits.join("\n") })
}

/// Files over 10 MB and binary files are data, not something to search.
fn grep_file(cwd: &Path, file: &Path, re: &regex::Regex) -> Vec<String> {
    if std::fs::metadata(file).is_ok_and(|meta| meta.len() > 10 * 1024 * 1024) {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(file) else { return Vec::new() };
    if bytes[..bytes.len().min(1024)].contains(&0) {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else { return Vec::new() };
    let mut shown: Option<String> = None;
    text.lines()
        .enumerate()
        .filter(|(_, line)| re.is_match(line))
        .take(MAX_GREP_HITS_PER_FILE)
        .map(|(i, line)| {
            let path = shown.get_or_insert_with(|| file.strip_prefix(cwd).unwrap_or(file).to_string_lossy().replace('\\', "/"));
            format!("{path}:{}:{}", i + 1, line.trim())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tempdir;

    fn chunk(old: &str, new: &str, replace_all: bool) -> EditChunk {
        EditChunk { old_string: old.into(), new_string: new.into(), replace_all, ..Default::default() }
    }

    /// Lines are counted from 1 and the last one is included: the model reads
    /// the file with those numbers and names them back.
    #[test]
    fn an_edit_can_name_its_lines() {
        let text = "one\ntwo\nthree\nfour\n";
        let edit = EditChunk {
            start_line: Some(2),
            end_line: Some(3),
            new_string: "TWO\nTHREE".into(),
            ..Default::default()
        };
        let (out, applied) = apply_edits("f", text.into(), &[edit]).unwrap();
        assert_eq!(out, "one\nTWO\nTHREE\nfour\n");
        assert_eq!(applied, 1);
    }

    /// One line is one edit: the range that names it.
    #[test]
    fn a_single_line_can_be_replaced() {
        let text = "one\ntwo\nthree\n";
        let edit = EditChunk { start_line: Some(3), end_line: Some(3), new_string: "THREE".into(), ..Default::default() };
        let (out, _) = apply_edits("f", text.into(), &[edit]).unwrap();
        assert_eq!(out, "one\ntwo\nTHREE\n");
    }

    /// A line that is not in the file is refused, not silently guessed at.
    #[test]
    fn lines_that_are_not_in_the_file_are_refused() {
        let text = "one\ntwo\n";
        let edit = EditChunk { start_line: Some(5), end_line: Some(6), new_string: "x".into(), ..Default::default() };
        let err = apply_edits("f", text.into(), &[edit]).unwrap_err().to_string();
        assert!(err.contains("not in"), "{err}");
        let zero = EditChunk { start_line: Some(0), new_string: "x".into(), ..Default::default() };
        assert!(apply_edits("f", text.into(), &[zero]).is_err(), "lines are counted from 1");
    }

    /// A regular expression, with the matched part put back.
    #[test]
    fn a_regular_expression_can_replace_and_keep_the_groups() {
        let text = "let a = 1;\nlet b = 2;\n";
        let edit = EditChunk {
            old_string: r"let (\w) = (\d);".into(),
            new_string: "let $1 = $2 + 1;".into(),
            regex: true,
            replace_all: true,
            ..Default::default()
        };
        let (out, applied) = apply_edits("f", text.into(), &[edit]).unwrap();
        assert_eq!(out, "let a = 1 + 1;\nlet b = 2 + 1;\n");
        assert_eq!(applied, 2);
    }

    /// One match is replaced; the groups go back where the replacement says.
    #[test]
    fn a_regular_expression_replaces_one_match() {
        let text = "value = 1;\n";
        let edit = EditChunk {
            old_string: r"value = (\d);".into(),
            new_string: "value = $1 + 1;".into(),
            regex: true,
            ..Default::default()
        };
        let (out, applied) = apply_edits("f", text.into(), &[edit]).unwrap();
        assert_eq!(out, "value = 1 + 1;\n");
        assert_eq!(applied, 1);
    }

    /// A pattern that matches many times is refused unless it says so.
    #[test]
    fn a_pattern_that_matches_many_times_needs_replace_all() {
        let text = "x\nx\n";
        let edit = EditChunk { old_string: "x".into(), new_string: "y".into(), regex: true, ..Default::default() };
        let err = apply_edits("f", text.into(), &[edit]).unwrap_err().to_string();
        assert!(err.contains("replace_all"), "{err}");
        let all = EditChunk { old_string: "x".into(), new_string: "y".into(), regex: true, replace_all: true, ..Default::default() };
        let (out, applied) = apply_edits("f", text.into(), &[all]).unwrap();
        assert_eq!(out, "y\ny\n");
        assert_eq!(applied, 2);
    }

    /// Several cursors: the line numbers count the file as it was, so an edit
    /// that changes the lines before another does not move it.
    #[test]
    fn several_cursors_do_not_move_each_other() {
        let text = "one\ntwo\nthree\nfour\n";
        let edits = [
            EditChunk { start_line: Some(1), end_line: Some(1), new_string: "ONE\nONE-AND-A-HALF".into(), ..Default::default() },
            EditChunk { start_line: Some(4), end_line: Some(4), new_string: "FOUR".into(), ..Default::default() },
        ];
        let (out, applied) = apply_edits("f", text.into(), &edits).unwrap();
        assert_eq!(out, "ONE\nONE-AND-A-HALF\ntwo\nthree\nFOUR\n");
        assert_eq!(applied, 2);
    }

    /// Two edits over the same text would each overwrite the other, and the
    /// one applied later would win while the other was lost without a word.
    /// The file was left in a state nobody asked for, and the model was told
    /// both edits were applied. They are refused instead.
    #[test]
    fn two_edits_over_the_same_text_are_refused() {
        let text = "x = foo()\n";
        let edits = [
            EditChunk { old_string: "foo".into(), new_string: "bar".into(), ..Default::default() },
            EditChunk { old_string: "foo()".into(), new_string: "qux()".into(), ..Default::default() },
        ];
        let err = apply_edits("f", text.into(), &edits).unwrap_err().to_string();
        assert!(err.contains("cover the same part"), "{err}");
        assert!(err.contains("edit 1 and edit 2"), "the model must know which two: {err}");
        assert!(err.contains("line 1"), "the model must know where: {err}");
    }

    /// The same text twice is the same conflict, and it used to cut the file
    /// in half: "one" replaced by "A" and then "one" by "B" left "Awo".
    #[test]
    fn two_edits_over_the_same_words_are_refused() {
        let text = "one\ntwo\n";
        let edits = [
            EditChunk { old_string: "one".into(), new_string: "A".into(), ..Default::default() },
            EditChunk { old_string: "one".into(), new_string: "B".into(), ..Default::default() },
        ];
        let err = apply_edits("f", text.into(), &edits).unwrap_err().to_string();
        assert!(err.contains("cover the same part"), "{err}");
    }

    /// The same text inside one edit is not a conflict: `replace_all` means
    /// every occurrence, and that has to keep working.
    #[test]
    fn one_edit_over_many_places_is_not_a_conflict() {
        let text = "x\nx\n";
        let edit = EditChunk {
            old_string: "x".into(),
            new_string: "y".into(),
            replace_all: true,
            ..Default::default()
        };
        let (out, applied) = apply_edits("f", text.into(), &[edit]).unwrap();
        assert_eq!(out, "y\ny\n");
        assert_eq!(applied, 2);
    }

    /// A line and a word inside it are a conflict too, and the same one: the
    /// word's place is inside the line's.
    #[test]
    fn a_line_and_a_word_inside_it_are_refused() {
        let text = "one\ntwo\nthree\n";
        let edits = [
            EditChunk { start_line: Some(1), end_line: Some(2), new_string: "ONE TWO".into(), ..Default::default() },
            EditChunk { old_string: "two".into(), new_string: "TWO".into(), ..Default::default() },
        ];
        let err = apply_edits("f", text.into(), &edits).unwrap_err().to_string();
        assert!(err.contains("cover the same part"), "{err}");
    }

    /// Edits that do not touch each other still go through as several
    /// cursors: this is the case the check must not break.
    #[test]
    fn edits_that_do_not_touch_each_other_are_still_applied() {
        let text = "one\ntwo\nthree\n";
        let edits = [
            EditChunk { old_string: "one".into(), new_string: "ONE".into(), ..Default::default() },
            EditChunk { old_string: "three".into(), new_string: "THREE".into(), ..Default::default() },
        ];
        let (out, applied) = apply_edits("f", text.into(), &edits).unwrap();
        assert_eq!(out, "ONE\ntwo\nTHREE\n");
        assert_eq!(applied, 2);
    }

    /// The order the model lists the edits in says nothing about where in the
    /// file they are. A model that lists them bottom-up used to make the tool
    /// write over the wrong bytes: the offsets were right for the text as it
    /// was, but they were applied in the order of the list.
    #[test]
    fn edits_listed_bottom_up_are_applied_in_text_order() {
        let text = "one\ntwo\nthree\n";
        // Bottom-up, and the first edit makes the file longer: applied in the
        // order of the list, the second edit would land three bytes too far.
        let edits = [
            EditChunk { old_string: "three".into(), new_string: "3".into(), ..Default::default() },
            EditChunk { old_string: "one".into(), new_string: "ONE-TWO".into(), ..Default::default() },
        ];
        let (out, applied) = apply_edits("f", text.into(), &edits).unwrap();
        assert_eq!(out, "ONE-TWO\ntwo\n3\n");
        assert_eq!(applied, 2);
    }

    /// The same edits, the other way round: the answer must not depend on the
    /// order of the list.
    #[test]
    fn the_order_of_the_list_does_not_change_the_result() {
        let text = "one\ntwo\nthree\n";
        let bottom_up = [
            EditChunk { old_string: "three".into(), new_string: "3".into(), ..Default::default() },
            EditChunk { old_string: "one".into(), new_string: "ONE-TWO".into(), ..Default::default() },
        ];
        let top_down = [
            EditChunk { old_string: "one".into(), new_string: "ONE-TWO".into(), ..Default::default() },
            EditChunk { old_string: "three".into(), new_string: "3".into(), ..Default::default() },
        ];
        let (a, _) = apply_edits("f", text.into(), &bottom_up).unwrap();
        let (b, _) = apply_edits("f", text.into(), &top_down).unwrap();
        assert_eq!(a, b);
    }

    /// Edits by line number, listed bottom-up: the same rule applies.
    #[test]
    fn lines_listed_bottom_up_are_applied_in_text_order() {
        let text = "one\ntwo\nthree\nfour\n";
        let edits = [
            EditChunk { start_line: Some(4), end_line: Some(4), new_string: "FOUR".into(), ..Default::default() },
            EditChunk { start_line: Some(1), end_line: Some(1), new_string: "ONE\nONE-AND-A-HALF".into(), ..Default::default() },
        ];
        let (out, applied) = apply_edits("f", text.into(), &edits).unwrap();
        assert_eq!(out, "ONE\nONE-AND-A-HALF\ntwo\nthree\nFOUR\n");
        assert_eq!(applied, 2);
    }

    /// The file is replaced, not written over: a reader never sees half of the
    /// new text, and a failure cannot leave a truncated file behind.
    #[test]
    #[cfg(unix)]
    fn a_written_file_is_replaced_in_one_step() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempdir();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\n").unwrap();
        let before = std::fs::metadata(&file).unwrap().ino();
        write_file(&dir, "a.txt", "two\n").unwrap();
        let after = std::fs::metadata(&file).unwrap().ino();
        assert_ne!(before, after, "the file was written in place instead of being replaced");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\n");
    }

    /// A write that fails leaves nothing of its own behind.
    #[test]
    fn a_failed_write_leaves_no_temporary_file_behind() {
        let dir = tempdir();
        // A folder where the file should be: the rename cannot replace it.
        let file = dir.join("a.txt");
        std::fs::create_dir(&file).unwrap();
        let err = write_atomic(&file, "content").unwrap_err();
        assert!(err.raw_os_error().is_some(), "the failure must be the system's: {err}");
        assert!(file.is_dir(), "the folder was replaced by a file");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "a.txt")
            .collect();
        assert!(leftovers.is_empty(), "a temporary file was left behind: {leftovers:?}");
    }

    /// A file the owner made read-only is not replaced: the rename would
    /// replace it whatever its permissions say.
    #[test]
    #[cfg(unix)]
    fn a_read_only_file_is_not_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        let err = write_atomic(&file, "two\n").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\n");
        // The permissions are kept when the write does go through.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_atomic(&file, "two\n").unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the file lost its permissions");
    }

    /// Nothing of a failed batch is left changed: not the files written
    /// before the failure, and not the file whose write failed.
    #[test]
    fn a_batch_that_fails_leaves_every_file_as_it_was() {
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
        // A folder where the second file was read: no write gets through that.
        std::fs::remove_file(&blocked).unwrap();
        std::fs::create_dir(&blocked).unwrap();
        let err = write_planned(&planned).unwrap_err().to_string();

        assert!(err.contains("blocked.txt"), "{err}");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "old first\n", "the first file was not put back");
        assert!(blocked.is_dir(), "nothing was written in place of the folder");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "first.txt" && name != "blocked.txt")
            .collect();
        assert!(leftovers.is_empty(), "a temporary file was left behind: {leftovers:?}");
    }

    #[test]
    fn a_file_that_is_not_there_names_the_one_that_is() {
        // The scenario `docs/tool-calling.md` measures: handed a bare "no such
        // file", a small model explains it in prose and leaves the next step to
        // the user. Handed the near miss, it calls again.
        let dir = tempdir();
        write_file(&dir, "src/config.rs", "x").unwrap();
        let err = read_file(&dir, "src/confg.rs", 0, 10).unwrap_err().to_string();
        assert!(err.contains("no such file"), "{err}");
        assert!(err.contains("did you mean src/config.rs"), "{err}");
        assert!(!err.contains("os error"), "the raw io error says nothing the model can use: {err}");
    }

    #[test]
    fn a_file_with_nearby_name_but_no_near_match_is_not_a_typo() {
        let dir = tempdir();
        write_file(&dir, "src/config.rs", "x").unwrap();
        let err = read_file(&dir, "src/completely_different.rs", 0, 10).unwrap_err().to_string();
        assert!(err.contains("no such file"), "{err}");
        assert!(!err.contains("did you mean"), "a different file is not a misspelling: {err}");
    }

    #[test]
    fn the_suggestion_keeps_the_shape_the_model_wrote() {
        let dir = tempdir();
        write_file(&dir, "README.md", "x").unwrap();
        for written in ["READM.md", "./READM.md"] {
            let err = read_file(&dir, written, 0, 10).unwrap_err().to_string();
            assert!(err.contains("did you mean"), "{written}: {err}");
            let shape: &str = match written {
                "READM.md" => "README.md",
                "./READM.md" => "./README.md",
                _ => unreachable!(),
            };
            assert!(err.contains(shape), "{written}: {err}");
        }
    }

    #[test]
    fn a_directory_nobody_wrote_has_nothing_to_suggest() {
        let dir = tempdir();
        let err = read_file(&dir, "nothing-here.rs", 0, 10).unwrap_err().to_string();
        assert_eq!(err, "read nothing-here.rs: no such file");
    }

    #[test]
    fn an_edit_of_a_misspelled_file_says_which_one_it_meant() {
        let dir = tempdir();
        write_file(&dir, "lib.rs", "fn a() {}").unwrap();
        let err = edit_file(&dir, "lib.rss", &[chunk("fn a() {}", "fn b() {}", false)]).unwrap_err().to_string();
        assert!(err.contains("did you mean lib.rs"), "{err}");
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
            &[EditChunk { old_string: "two".into(), new_string: "dos".into(), replace_all: false, ..Default::default() }],
        )
        .unwrap();
        assert!(std::fs::read_to_string(dir.join("a.txt")).unwrap().contains("dos"));

        let err = edit_file(
            &dir,
            "a.txt",
            &[EditChunk { old_string: "zzz".into(), new_string: "y".into(), replace_all: false, ..Default::default() }],
        )
        .unwrap_err();
        assert!(err.to_string().contains("not found"));

        std::fs::write(dir.join("a.txt"), "x x x\n").unwrap();
        let err = edit_file(
            &dir,
            "a.txt",
            &[EditChunk { old_string: "x".into(), new_string: "y".into(), replace_all: false, ..Default::default() }],
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
                EditChunk { old_string: "one".into(), new_string: "two".into(), replace_all: false, ..Default::default() },
                EditChunk { old_string: "two".into(), new_string: "three".into(), replace_all: false, ..Default::default() },
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
                    vec![EditChunk { old_string: "one".into(), new_string: "two".into(), replace_all: false, ..Default::default() }],
                ),
                (
                    "alias.txt".into(),
                    vec![EditChunk { old_string: "two".into(), new_string: "three".into(), replace_all: false, ..Default::default() }],
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
    fn grep_answers_in_path_and_line_order_and_keeps_the_first_hits() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        let hits = |n: usize| (1..=n).map(|i| format!("hit {i}\nmiss\n")).collect::<String>();
        for (name, n) in [("c.txt", 2), ("a.txt", 2), ("b/z.txt", 60), ("b/a.txt", 60), ("d.txt", 60), ("e.txt", 60)] {
            std::fs::write(dir.join(name), hits(n)).unwrap();
        }
        let out = grep(&dir, "^hit", None, false).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), MAX_GREP_HITS);
        assert_eq!(&lines[..3], ["a.txt:1:hit 1", "a.txt:3:hit 2", "b/a.txt:1:hit 1"]);
        // Fifty per file at most, and the cap falls inside the last file.
        assert_eq!(lines.iter().filter(|l| l.starts_with("b/z.txt:")).count(), MAX_GREP_HITS_PER_FILE);
        assert_eq!(lines[MAX_GREP_HITS - 1], "e.txt:91:hit 46");
        assert_eq!(grep(&dir, "^hit", None, false).unwrap(), out, "the same search gave another answer");
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
        // The same rule as `grep`: a symlink out of the project is not
        // followed, so the file behind it is not part of the project.
        assert!(!glob_files(&dir, "linked-outside/*.txt").unwrap().contains("secret.txt"));
    }

    #[test]
    fn a_glob_pattern_that_climbs_out_of_the_project_finds_nothing() {
        let dir = tempdir();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        let pattern = format!("../{}/secret.txt", outside.path().file_name().unwrap().to_string_lossy());
        let out = glob_files(&dir, &pattern).unwrap();
        assert!(!out.contains("secret.txt"), "the project filter was switched off: {out}");
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
        assert_eq!(find_actual_string(text, copied).as_ref().map(|(t, _)| t.as_str()), Some("pub fn add(a: i32, b: i32) -> i32 {\n    a + b"));
    }

    #[test]
    fn a_missing_old_string_points_at_the_closest_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "use std::io;\n\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();
        let edits = [EditChunk { old_string: "pub fn add(a: u32, b: u32) -> u32 {".into(), new_string: "x".into(), replace_all: false, ..Default::default() }];
        let err = edit_file(dir.path(), "lib.rs", &edits).unwrap_err().to_string();
        assert!(err.contains("closest line is 3: `pub fn add(a: i32, b: i32) -> i32 {`"), "{err}");
    }

    #[test]
    fn an_edit_lands_even_when_the_model_lost_the_indentation() {
        // Measured: models routinely send `if media:` with 4 spaces for a line
        // the file indents by 8, or drop a method's indent entirely. Refusing
        // the edit over layout sends the model round again for nothing.
        let dir = tempfile::tempdir().unwrap();
        let body = "async def handle(self):\n        if media:\n            return await send(event)\n";
        std::fs::write(dir.path().join("a.py"), body).unwrap();

        let under_indented = "    if media:\n        return await send(event)";
        edit_file(dir.path(), "a.py", &[chunk(under_indented, "if media:\n            return None", false)]).unwrap();

        let no_indent = "async def handle(self):";
        edit_file(dir.path(), "a.py", &[chunk(no_indent, "async def handle(self, q):", false)]).unwrap();

        let out = std::fs::read_to_string(dir.path().join("a.py")).unwrap();
        assert_eq!(out, "async def handle(self, q):\n        if media:\n            return None\n");
    }

    #[test]
    fn losing_the_indentation_does_not_make_a_match_ambiguous() {
        // Tolerance must not turn into a guess: two lines differing only by
        // indentation are still two, and the count is what asks for context.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {\n    call();\n}\nfn b() {\n    call();\n}\n").unwrap();
        let err = edit_file(dir.path(), "a.rs", &[chunk("call();", "call(1);", false)]).unwrap_err().to_string();
        assert!(err.contains("matches 2 times"), "{err}");
    }

    #[test]
    fn an_invented_keyword_is_named_rather_than_guessed_at() {
        // Measured: `pub plan: Vec<..>` sent against a field the file declares
        // without `pub`. No tolerance can absorb that, so the error has to say
        // what was wrong instead of only "copy it exactly".
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("goal.rs"), "struct S {\n    plan: Vec<PlanStep>,\n}\n").unwrap();
        let err = edit_file(dir.path(), "goal.rs", &[chunk("    pub plan: Vec<PlanStep>,", "    pub plan: Vec<Step>,", false)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("closest line is 2: `    plan: Vec<PlanStep>,`"), "{err}");
        assert!(err.contains("`pub` on a line the file has without it"), "{err}");
    }

    #[test]
    fn a_different_line_is_not_reported_as_a_word_mistake() {
        // The word hint is only for near misses; on an unrelated line it would
        // be noise, and the closest-line hint already answers it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {\n    let x = compute(1, 2);\n}\n").unwrap();
        let err = edit_file(dir.path(), "a.rs", &[chunk("    let x = totally_different(9);", "y", false)]).unwrap_err().to_string();
        assert!(!err.contains("You wrote"), "{err}");
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
            ..Default::default()
        };
        let (res, _) = apply_edits("f", text.to_string(), &[edit]).unwrap();
        assert!(res.contains("FlashAgent"));
    }
}
