//! Patch tool (`patch_file`).
//!
//! Applies standard unified diff patches directly to target files.

use std::path::Path;
use crate::ToolError;

#[derive(Debug)]
struct Hunk {
    old_start: usize,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

fn parse_hunks(patch_text: &str) -> Result<Vec<Hunk>, ToolError> {
    let mut hunks = Vec::new();
    let mut current_hunk: Option<Hunk> = None;

    for line in patch_text.lines() {
        if line.starts_with("@@ ") {
            if let Some(h) = current_hunk.take() {
                hunks.push(h);
            }

            // Example: @@ -10,5 +10,6 @@
            let parts: Vec<&str> = line.split("@@").collect();
            if parts.len() < 2 {
                return Err(ToolError::Other(format!("malformed hunk header: {line}")));
            }
            let header = parts[1].trim();
            let mut start_line = 1;

            for section in header.split_whitespace() {
                if let Some(old_spec) = section.strip_prefix('-') {
                    let num_str = old_spec.split(',').next().unwrap_or("1");
                    start_line = num_str.parse::<usize>().unwrap_or(1);
                }
            }

            current_hunk = Some(Hunk {
                old_start: start_line,
                old_lines: Vec::new(),
                new_lines: Vec::new(),
            });
            continue;
        }

        if let Some(ref mut hunk) = current_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                hunk.new_lines.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.old_lines.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.old_lines.push(rest.to_string());
                hunk.new_lines.push(rest.to_string());
            } else if line.is_empty() {
                hunk.old_lines.push(String::new());
                hunk.new_lines.push(String::new());
            }
        }
    }

    if let Some(h) = current_hunk {
        hunks.push(h);
    }

    if hunks.is_empty() {
        return Err(ToolError::Other("no valid diff hunks (@@ ... @@) found in patch".into()));
    }

    Ok(hunks)
}

/// Apply parsed hunks to `orig`, locating each hunk by its context/removed
/// lines (tolerating line drift). Pure: shared by the tool and its preview.
fn apply_hunks(orig: &str, hunks: &[Hunk], rel_path: &str) -> Result<String, ToolError> {
    let mut file_lines: Vec<String> = orig.lines().map(str::to_string).collect();
    // Hunks are applied top to bottom; each one shifts the lines below it.
    let mut drift: isize = 0;
    for (idx, hunk) in hunks.iter().enumerate() {
        let expected_len = hunk.old_lines.len();
        let anchored = (hunk.old_start as isize - 1 + drift).max(0) as usize;

        if expected_len == 0 {
            // Pure insertion (`@@ -N,0 +M,K @@`): no lines to locate; the
            // old start names the line the insertion follows.
            let at = (hunk.old_start as isize + drift).clamp(0, file_lines.len() as isize) as usize;
            file_lines.splice(at..at, hunk.new_lines.iter().cloned());
            drift += hunk.new_lines.len() as isize;
            continue;
        }

        let matches_at = |i: usize| i + expected_len <= file_lines.len() && file_lines[i..i + expected_len] == hunk.old_lines[..];
        let pos = if matches_at(anchored) {
            anchored
        } else {
            (0..=file_lines.len().saturating_sub(expected_len)).find(|&i| matches_at(i)).ok_or_else(|| {
                ToolError::Other(format!("hunk #{} failed to match target lines in '{rel_path}'", idx + 1))
            })?
        };
        file_lines.splice(pos..pos + expected_len, hunk.new_lines.iter().cloned());
        drift += hunk.new_lines.len() as isize - expected_len as isize;
    }

    let mut new_content = file_lines.join("\n");
    if orig.ends_with('\n') && !new_content.is_empty() {
        new_content.push('\n');
    }
    Ok(new_content)
}

/// Applies a unified diff patch to a target file.
pub fn patch_file(cwd: &Path, rel_path: &str, patch_text: &str) -> Result<String, ToolError> {
    let full = cwd.join(rel_path);
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

/// Computes the prospective new content for write preview.
pub fn preview_patch(cwd: &Path, rel_path: &str, patch_text: &str) -> Option<String> {
    let orig_content = std::fs::read_to_string(cwd.join(rel_path)).ok()?;
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
        // Insert at the top (-0,0) and after line 2 (-2,0).
        let patch = "@@ -0,0 +1,1 @@\n+zero\n@@ -2,0 +4,1 @@\n+three\n";
        patch_file(&temp, "a.txt", patch).unwrap();
        assert_eq!(std::fs::read_to_string(temp.join("a.txt")).unwrap(), "zero\none\ntwo\nthree\n");
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
