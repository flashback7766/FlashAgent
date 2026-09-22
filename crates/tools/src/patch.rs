//! `patch_file`: applies a unified diff.

use std::path::Path;
use crate::ToolError;

#[derive(Debug)]
struct Hunk {
    old_start: usize,
    old_count: usize,
    new_count: usize,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    /// `\ No newline at end of file` follows the last old-side line.
    old_no_eol: bool,
    /// `\ No newline at end of file` follows the last new-side line.
    new_no_eol: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Side { None, Old, New, Both }

fn parse_hunks(patch_text: &str) -> Result<Vec<Hunk>, ToolError> {
    let mut hunks = Vec::new();
    let mut current_hunk: Option<Hunk> = None;
    let mut last = Side::None;
    let misplaced_marker = || ToolError::Other("'\\ No newline at end of file' must follow the last line of a hunk side".into());

    for line in patch_text.lines() {
        if line.starts_with("@@") {
            if let Some(h) = current_hunk.take() {
                hunks.push(h);
            }
            last = Side::None;

            // @@ -10,5 +10,6 @@
            let bad_header = || ToolError::Other(format!("malformed hunk header: {line}"));
            let (header, _) = line.strip_prefix("@@ ").and_then(|s| s.split_once(" @@")).ok_or_else(bad_header)?;
            let mut sections = header.split_whitespace();
            let range = |spec: &str, sign: char| -> Option<(usize, usize)> {
                let mut parts = spec.strip_prefix(sign)?.split(',');
                let number = |s: &str| -> Option<usize> {
                    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) { return None; }
                    s.parse::<usize>().ok().filter(|n| *n <= isize::MAX as usize)
                };
                let start = number(parts.next()?)?;
                let count = match parts.next() { Some(s) => number(s)?, None => 1 };
                (parts.next().is_none() && (start > 0 || count == 0)).then_some((start, count))
            };
            let (start_line, old_count) = sections.next().and_then(|s| range(s, '-')).ok_or_else(bad_header)?;
            let (_, new_count) = sections.next().and_then(|s| range(s, '+')).ok_or_else(bad_header)?;
            if sections.next().is_some() { return Err(bad_header()); }

            current_hunk = Some(Hunk {
                old_start: start_line,
                old_count,
                new_count,
                old_lines: Vec::new(),
                new_lines: Vec::new(),
                old_no_eol: false,
                new_no_eol: false,
            });
            continue;
        }

        if line.starts_with('\\') {
            let hunk = current_hunk.as_mut().ok_or_else(misplaced_marker)?;
            match last {
                Side::None => return Err(misplaced_marker()),
                Side::Old => hunk.old_no_eol = true,
                Side::New => hunk.new_no_eol = true,
                Side::Both => { hunk.old_no_eol = true; hunk.new_no_eol = true; }
            }
            last = Side::None;
            continue;
        }

        if let Some(ref mut hunk) = current_hunk {
            let (text, side) = if let Some(rest) = line.strip_prefix('+') {
                (rest, Side::New)
            } else if let Some(rest) = line.strip_prefix('-') {
                (rest, Side::Old)
            } else if let Some(rest) = line.strip_prefix(' ') {
                (rest, Side::Both)
            } else if line.is_empty() {
                ("", Side::Both)
            } else {
                continue;
            };
            // A marked side is finished: nothing may follow its last line.
            if (hunk.old_no_eol && side != Side::New) || (hunk.new_no_eol && side != Side::Old) {
                return Err(misplaced_marker());
            }
            if side != Side::New { hunk.old_lines.push(text.to_string()); }
            if side != Side::Old { hunk.new_lines.push(text.to_string()); }
            last = side;
        }
    }

    if let Some(h) = current_hunk {
        hunks.push(h);
    }

    if hunks.is_empty() {
        return Err(ToolError::Other("no valid diff hunks (@@ ... @@) found in patch".into()));
    }

    for (idx, hunk) in hunks.iter().enumerate() {
        if hunk.old_lines.len() != hunk.old_count || hunk.new_lines.len() != hunk.new_count {
            return Err(ToolError::Other(format!("hunk #{} line counts do not match its header", idx + 1)));
        }
    }
    // Only the last hunk can touch the end of the file.
    if hunks[..hunks.len() - 1].iter().any(|h| h.old_no_eol || h.new_no_eol) {
        return Err(misplaced_marker());
    }

    Ok(hunks)
}

/// Split from its terminator (`"\n"`, `"\r\n"`, or `""`), so untouched bytes
/// survive exactly.
struct FileLine<'a> {
    text: &'a str,
    eol: &'a str,
}

fn split_file_lines(content: &str) -> Vec<FileLine<'_>> {
    content.split_inclusive('\n').map(|chunk| {
        let body = chunk.strip_suffix('\n').unwrap_or(chunk);
        match body.strip_suffix('\r') {
            Some(text) if chunk.ends_with('\n') => FileLine { text, eol: &chunk[text.len()..] },
            _ => FileLine { text: body, eol: &chunk[body.len()..] },
        }
    }).collect()
}

/// Locates each hunk by its context and removed lines, tolerating drift.
/// Shared by the tool and its preview. Kept lines keep their endings; new
/// lines use the file's first line ending. A missing final newline is kept
/// unless the patch says otherwise.
fn apply_hunks(orig: &str, hunks: &[Hunk], rel_path: &str) -> Result<String, ToolError> {
    let mut file_lines = split_file_lines(orig);
    let eol = file_lines.iter().map(|l| l.eol).find(|e| !e.is_empty()).unwrap_or("\n");
    let marker_error = |idx: usize| ToolError::Other(format!(
        "hunk #{} marks a missing final newline, but it does not end at the end of '{rel_path}'", idx + 1));
    let mut drift: isize = 0;
    for (idx, hunk) in hunks.iter().enumerate() {
        let expected_len = hunk.old_lines.len();
        let anchored = hunk.old_start.saturating_sub(1).checked_add_signed(drift)
            .ok_or_else(|| ToolError::Other(format!("hunk #{} has an invalid line position", idx + 1)))?;

        let pos = if expected_len == 0 {
            // Pure insertion (`@@ -N,0 +M,K @@`): the old start is the line it follows.
            hunk.old_start.checked_add_signed(drift).filter(|at| *at <= file_lines.len())
                .ok_or_else(|| ToolError::Other(format!("hunk #{} inserts outside '{rel_path}'", idx + 1)))?
        } else {
            let matches_at = |i: usize| i.checked_add(expected_len).and_then(|end| file_lines.get(i..end)).is_some_and(|lines| {
                lines.iter().zip(&hunk.old_lines).all(|(l, old)| l.text == old)
                    && (!hunk.old_no_eol || (i + expected_len == file_lines.len() && lines[expected_len - 1].eol.is_empty()))
            });
            if matches_at(anchored) {
                anchored
            } else {
                let mut matches = (0..=file_lines.len().saturating_sub(expected_len)).filter(|&i| matches_at(i));
                let pos = matches.next().ok_or_else(|| {
                    ToolError::Other(format!("hunk #{} failed to match target lines in '{rel_path}'", idx + 1))
                })?;
                if matches.next().is_some() {
                    return Err(ToolError::Other(format!("hunk #{} matches multiple locations in '{rel_path}'; add more context", idx + 1)));
                }
                pos
            }
        };

        let at_eof = pos + expected_len == file_lines.len();
        if hunk.new_no_eol && !at_eof {
            return Err(marker_error(idx));
        }
        // Explicit markers win; otherwise the file keeps its final ending.
        let final_eol = if hunk.new_no_eol {
            ""
        } else if hunk.old_no_eol {
            eol
        } else {
            file_lines.last().map_or(eol, |l| l.eol)
        };
        let replaced: Vec<FileLine<'_>> = hunk.new_lines.iter().map(|text| {
            let kept = file_lines[pos..pos + expected_len].iter().find(|l| l.text == text && !l.eol.is_empty());
            FileLine { text, eol: kept.map_or(eol, |l| l.eol) }
        }).collect();
        let inserted = replaced.len();
        file_lines.splice(pos..pos + expected_len, replaced);
        if at_eof {
            if let Some(last) = file_lines.last_mut() {
                last.eol = final_eol;
            }
        }
        // Only the file's last line may lack a terminator.
        let len = file_lines.len();
        for line in file_lines.iter_mut().take(len.saturating_sub(1)) {
            if line.eol.is_empty() { line.eol = eol; }
        }
        // The next hunk follows the location actually found, plus this hunk's size change.
        drift = (pos as i128 - hunk.old_start.saturating_sub(1) as i128
            + inserted as i128 - expected_len as i128)
            .try_into().map_err(|_| ToolError::Other("patch line offset overflow".into()))?;
    }

    Ok(file_lines.iter().flat_map(|l| [l.text, l.eol]).collect())
}

pub fn patch_file(cwd: &Path, rel_path: &str, patch_text: &str) -> Result<String, ToolError> {
    let full = flashagent_core::resolve_path(cwd, rel_path);
    if !full.exists() {
        return Err(ToolError::Other(format!("target file not found: {rel_path}")));
    }
    let orig_content = std::fs::read_to_string(&full)
        .map_err(|e| ToolError::Other(format!("failed to read file '{rel_path}': {e}")))?;
    let hunks = parse_hunks(patch_text)?;
    let new_content = apply_hunks(&orig_content, &hunks, rel_path)?;
    std::fs::write(&full, &new_content)
        .map_err(|e| ToolError::Other(format!("failed to write patched file '{rel_path}': {e}")))?;
    Ok(format!("Successfully applied {} hunk(s) to '{rel_path}'", hunks.len()))
}

/// New content for the write preview.
pub fn preview_patch(cwd: &Path, rel_path: &str, patch_text: &str) -> Option<String> {
    let orig_content = std::fs::read_to_string(flashagent_core::resolve_path(cwd, rel_path)).ok()?;
    let hunks = parse_hunks(patch_text).ok()?;
    let new_content = apply_hunks(&orig_content, &hunks, rel_path).ok()?;
    Some(flashagent_core::unified(Some(&orig_content), &new_content, rel_path, 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_patch_success() {
        let temp = crate::testing::tempdir();
        let file = temp.join("code.rs");
        std::fs::write(&file, "fn foo() -> i32 {\n    41\n}\n").unwrap();

        let patch = "--- a/code.rs\n+++ b/code.rs\n@@ -1,3 +1,3 @@\n fn foo() -> i32 {\n-    41\n+    42\n }\n";
        let res = patch_file(&temp, "code.rs", patch).unwrap();
        assert!(res.contains("applied 1 hunk(s)"));

        let updated = std::fs::read_to_string(&file).unwrap();
        assert_eq!(updated, "fn foo() -> i32 {\n    42\n}\n");
    }

    #[test]
    fn pure_insertion_hunks_are_applied_not_skipped() {
        let temp = crate::testing::tempdir();
        std::fs::write(temp.join("a.txt"), "one\ntwo\n").unwrap();
        let patch = "@@ -0,0 +1,1 @@\n+zero\n@@ -2,0 +4,1 @@\n+three\n";
        patch_file(&temp, "a.txt", patch).unwrap();
        assert_eq!(std::fs::read_to_string(temp.join("a.txt")).unwrap(), "zero\none\ntwo\nthree\n");
    }

    #[test]
    fn ambiguous_drift_is_refused_without_changing_the_file() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        let original = "same\nother\nsame\n";
        std::fs::write(&path, original).unwrap();
        let patch = "@@ -2,1 +2,1 @@\n-same\n+changed\n";
        assert!(patch_file(&temp, "a.txt", patch).is_err());
        assert!(preview_patch(&temp, "a.txt", patch).is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn an_insertion_past_the_file_is_refused() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let patch = "@@ -20,0 +21,1 @@\n+new\n";
        assert!(patch_file(&temp, "a.txt", patch).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\n");
    }

    #[test]
    fn exact_anchor_disambiguates_repeated_lines_and_unique_drift_still_works() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        std::fs::write(&path, "same\nother\nsame\n").unwrap();
        patch_file(&temp, "a.txt", "@@ -3,1 +3,1 @@\n-same\n+changed\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "same\nother\nchanged\n");
        patch_file(&temp, "a.txt", "@@ -1,1 +1,1 @@\n-other\n+unique\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "same\nunique\nchanged\n");
    }

    #[test]
    fn an_ambiguous_later_hunk_does_not_write_an_earlier_successful_hunk() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        let original = "first\nsame\nother\nsame\n";
        std::fs::write(&path, original).unwrap();
        let patch = "@@ -1,1 +1,1 @@\n-first\n+changed\n@@ -3,1 +3,1 @@\n-same\n+changed too\n";
        assert!(patch_file(&temp, "a.txt", patch).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn discovered_line_drift_moves_a_following_insertion() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        std::fs::write(&path, "prefix\nold\ntail\n").unwrap();
        let patch = "@@ -1,1 +1,1 @@\n-old\n+new\n@@ -1,0 +2,1 @@\n+inserted\n";
        patch_file(&temp, "a.txt", patch).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "prefix\nnew\ninserted\ntail\n");
    }

    #[test]
    fn malformed_and_truncated_hunks_never_modify_the_target() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        for patch in [
            "@@ -oops +1 @@\n-old\n+new\n",
            "@@ -1 +1\n-old\n+new\n",
            "@@ -1,2 +1,2 @@\n-old\n+new\n",
            "@@ -1,1 +1,2 @@\n-old\n+new\n",
            "@@ -0,1 +1,1 @@\n-old\n+new\n",
        ] {
            std::fs::write(&path, "old\n").unwrap();
            assert!(patch_file(&temp, "a.txt", patch).is_err(), "accepted: {patch}");
            assert!(preview_patch(&temp, "a.txt", patch).is_none());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
        }
    }

    #[test]
    fn crlf_line_endings_are_preserved_outside_and_inside_the_hunk() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        std::fs::write(&path, "one\r\ntwo\r\nthree\r\n").unwrap();
        patch_file(&temp, "a.txt", "@@ -2,1 +2,2 @@\n-two\n+TWO\n+extra\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one\r\nTWO\r\nextra\r\nthree\r\n");
    }

    #[test]
    fn missing_final_newline_is_kept_unless_the_patch_says_otherwise() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        std::fs::write(&path, "a\nb").unwrap();
        patch_file(&temp, "a.txt", "@@ -2,1 +2,1 @@\n-b\n+c\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nc");

        patch_file(&temp, "a.txt", "@@ -2,1 +2,1 @@\n-c\n\\ No newline at end of file\n+d\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nd\n");

        patch_file(&temp, "a.txt", "@@ -2,1 +2,1 @@\n-d\n+e\n\\ No newline at end of file\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\ne");

        patch_file(&temp, "a.txt", "@@ -2,0 +3,1 @@\n+f\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\ne\nf");
    }

    #[test]
    fn a_no_newline_marker_away_from_the_end_is_refused() {
        let temp = crate::testing::tempdir();
        let path = temp.join("a.txt");
        std::fs::write(&path, "a\nb\n").unwrap();
        for patch in [
            "@@ -1,1 +1,1 @@\n-a\n+x\n\\ No newline at end of file\n",
            "@@ -1,1 +1,1 @@\n-a\n\\ No newline at end of file\n+x\n",
            "\\ No newline at end of file\n@@ -1,1 +1,1 @@\n-a\n+x\n",
        ] {
            assert!(patch_file(&temp, "a.txt", patch).is_err(), "accepted: {patch}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");
        }
    }

    #[test]
    fn preview_matches_what_patch_writes() {
        let temp = crate::testing::tempdir();
        std::fs::write(temp.join("b.txt"), "a\nb\nc\n").unwrap();
        let patch = "@@ -2,1 +2,1 @@\n-b\n+B\n";
        let preview = preview_patch(&temp, "b.txt", patch).unwrap();
        assert!(preview.contains("-b") && preview.contains("+B"));
        patch_file(&temp, "b.txt", patch).unwrap();
        assert_eq!(std::fs::read_to_string(temp.join("b.txt")).unwrap(), "a\nB\nc\n");
    }
}
