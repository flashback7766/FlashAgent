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
                old_start: start_line.max(1),
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

/// Applies a unified diff patch to a target file.
pub fn patch_file(cwd: &Path, rel_path: &str, patch_text: &str) -> Result<String, ToolError> {
    let full = cwd.join(rel_path);
    if !full.exists() {
        return Err(ToolError::Other(format!("target file not found: {rel_path}")));
    }

    let orig_content = std::fs::read_to_string(&full)
        .map_err(|e| ToolError::Other(format!("failed to read file '{rel_path}': {e}")))?;

    let hunks = parse_hunks(patch_text)?;
    let mut file_lines: Vec<String> = orig_content.lines().map(str::to_string).collect();

    // Apply hunks in reverse or with offset adjustment
    // To handle line drift reliably, we apply each hunk by locating its old_lines in file_lines
    for (idx, hunk) in hunks.iter().enumerate() {
        let expected_len = hunk.old_lines.len();
        if expected_len == 0 {
            continue;
        }

        // Search starting near hunk.old_start - 1
        let preferred_pos = hunk.old_start.saturating_sub(1);
        let mut match_pos = None;

        // Check exact position first
        if preferred_pos + expected_len <= file_lines.len()
            && file_lines[preferred_pos..preferred_pos + expected_len] == hunk.old_lines[..]
        {
            match_pos = Some(preferred_pos);
        } else {
            // Search nearby within file
            for i in 0..=file_lines.len().saturating_sub(expected_len) {
                if file_lines[i..i + expected_len] == hunk.old_lines[..] {
                    match_pos = Some(i);
                    break;
                }
            }
        }

        let pos = match_pos.ok_or_else(|| {
            ToolError::Other(format!(
                "hunk #{} failed to match target lines in '{rel_path}'",
                idx + 1
            ))
        })?;

        // Replace matched lines with hunk.new_lines
        file_lines.splice(pos..pos + expected_len, hunk.new_lines.clone());
    }

    let mut new_content = file_lines.join("\n");
    if orig_content.ends_with('\n') {
        new_content.push('\n');
    }

    std::fs::write(&full, &new_content)
        .map_err(|e| ToolError::Other(format!("failed to write patched file '{rel_path}': {e}")))?;

    Ok(format!(
        "Successfully applied {} hunk(s) to '{rel_path}'",
        hunks.len()
    ))
}

/// Computes the prospective new content for write preview.
pub fn preview_patch(cwd: &Path, rel_path: &str, patch_text: &str) -> Option<String> {
    let full = cwd.join(rel_path);
    let orig_content = std::fs::read_to_string(&full).ok()?;
    let hunks = parse_hunks(patch_text).ok()?;
    let mut file_lines: Vec<String> = orig_content.lines().map(str::to_string).collect();

    for hunk in &hunks {
        let expected_len = hunk.old_lines.len();
        if expected_len == 0 {
            continue;
        }
        let preferred_pos = hunk.old_start.saturating_sub(1);
        let mut match_pos = None;

        if preferred_pos + expected_len <= file_lines.len()
            && file_lines[preferred_pos..preferred_pos + expected_len] == hunk.old_lines[..]
        {
            match_pos = Some(preferred_pos);
        } else {
            for i in 0..=file_lines.len().saturating_sub(expected_len) {
                if file_lines[i..i + expected_len] == hunk.old_lines[..] {
                    match_pos = Some(i);
                    break;
                }
            }
        }

        let pos = match_pos?;
        file_lines.splice(pos..pos + expected_len, hunk.new_lines.clone());
    }

    let mut new_content = file_lines.join("\n");
    if orig_content.ends_with('\n') {
        new_content.push('\n');
    }

    Some(flashagent_core::unified(
        Some(&orig_content),
        &new_content,
        rel_path,
        3,
    ))
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
}
