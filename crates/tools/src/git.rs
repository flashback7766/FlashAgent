//! Native Git inspection tools (`git_status`, `git_diff`).
//!
//! Executes git commands directly via `std::process::Command` without shell interpretation,
//! enabling fast and safe inspection without prompting in AcceptEdits mode.

use std::path::Path;
use std::process::Command;
use crate::ToolError;

/// Returns current Git branch status, staged, unstaged, and untracked files.
pub fn git_status(cwd: &Path, path_filter: Option<&str>) -> Result<String, ToolError> {
    let mut cmd = Command::new("git");
    cmd.arg("status")
        .arg("--porcelain=v1")
        .arg("-b")
        .current_dir(cwd);

    if let Some(pf) = path_filter {
        cmd.arg("--").arg(pf);
    }

    let out = cmd.output().map_err(|e| ToolError::Other(format!("failed to execute git: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Ok(format!("git error: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return Ok("Clean working directory (no changes)".to_string());
    }

    let mut branch = "unknown";
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    for line in stdout.lines() {
        if let Some(b) = line.strip_prefix("## ") {
            branch = b;
        } else if let Some(u) = line.strip_prefix("?? ") {
            untracked.push(u);
        } else if line.len() >= 3 {
            let x = line.as_bytes()[0] as char;
            let y = line.as_bytes()[1] as char;
            let file = &line[3..];

            if x != ' ' && x != '?' {
                staged.push(format!("{x} {file}"));
            }
            if y != ' ' && y != '?' {
                unstaged.push(format!("{y} {file}"));
            }
        }
    }

    let mut report = Vec::new();
    report.push(format!("Branch: {branch}"));

    if !staged.is_empty() {
        report.push(format!("\nStaged changes ({}):", staged.len()));
        for s in staged {
            report.push(format!("  {s}"));
        }
    }

    if !unstaged.is_empty() {
        report.push(format!("\nUnstaged changes ({}):", unstaged.len()));
        for u in unstaged {
            report.push(format!("  {u}"));
        }
    }

    if !untracked.is_empty() {
        report.push(format!("\nUntracked files ({}):", untracked.len()));
        for f in untracked.iter().take(30) {
            report.push(format!("  ? {f}"));
        }
        if untracked.len() > 30 {
            report.push(format!("  ... and {} more files", untracked.len() - 30));
        }
    }

    Ok(report.join("\n"))
}

/// Returns unified diff of working tree or staged changes.
pub fn git_diff(cwd: &Path, staged: bool, path_filter: Option<&str>) -> Result<String, ToolError> {
    let mut cmd = Command::new("git");
    cmd.arg("diff").current_dir(cwd);

    if staged {
        cmd.arg("--cached");
    }

    if let Some(pf) = path_filter {
        cmd.arg("--").arg(pf);
    }

    let out = cmd.output().map_err(|e| ToolError::Other(format!("failed to execute git: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Ok(format!("git error: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        let target = if staged { "staged changes" } else { "unstaged changes" };
        return Ok(format!("No {target} detected."));
    }

    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_status_non_git_dir_returns_error_message() {
        let temp = crate::testing::tempdir();
        let res = git_status(&temp, None).unwrap();
        assert!(res.contains("not a git repository") || res.contains("git error"));
    }

    #[test]
    fn git_diff_non_git_dir_returns_error_message() {
        let temp = crate::testing::tempdir();
        let res = git_diff(&temp, false, None).unwrap();
        assert!(res.contains("not a git repository") || res.contains("git error"));
    }
}
