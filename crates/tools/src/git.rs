//! `git_status` and `git_diff`, run without a shell so they need no approval
//! in AcceptEdits mode.
//!
//! Both are waited for, but not for ever: a repository with a lock file or a
//! huge index can keep `git` busy for a long time, and a turn that waits is a
//! turn that cannot steer, interrupt or answer.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::ToolError;

/// How long a `git` command may hold the turn.
///
/// A repository with a lock file, an index that is being rebuilt, or a hook
/// that waits for something can keep `git status` busy for a long time. The
/// command used to be waited for without a limit, so the turn never ended.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs a program and waits for it, but not forever.
///
/// The child is killed when the wait runs out, and its pipes are read while it
/// runs, so a command that writes a lot does not block on a full pipe.
async fn run_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Result<std::process::Output, ToolError> {
    use tokio::io::AsyncReadExt;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Git spawns what the repository configures (textconv, external diff): a
    // group, so a timeout takes the whole tree instead of leaving the driver
    // running as an orphan while the error says everything was stopped.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| ToolError::Other(format!("failed to execute {program}: {e}")))?;
    let pid = child.id();
    // The pipes are read while it runs, so a command that writes a lot does
    // not block on a full pipe.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let read_stdout = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    });
    let read_stderr = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    });
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => {
            let status = status.map_err(|e| ToolError::Other(format!("{program} failed: {e}")))?;
            let stdout = read_stdout.await.unwrap_or_default();
            let stderr = read_stderr.await.unwrap_or_default();
            Ok(std::process::Output { status, stdout, stderr })
        }
        Err(_) => {
            // While git still lives: on Windows the children are found through
            // their parent.
            if let Some(pid) = pid {
                crate::shell::kill_pid_tree(pid);
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = read_stdout.await;
            let _ = read_stderr.await;
            Err(ToolError::Other(format!(
                "{program} did not finish within {} seconds and was stopped; the repository may be locked or unusually large",
                timeout.as_secs().max(1)
            )))
        }
    }
}

/// The branch name out of the porcelain header.
///
/// `## main...origin/main [ahead 1]` is the branch `main`, and
/// `## No commits yet on main` is `main` too: the tracking branch and the
/// counts are not part of the name.
fn branch_name(header: &str) -> String {
    let rest = header.strip_prefix("## ").unwrap_or(header).trim();
    let rest = rest.strip_prefix("No commits yet on ").unwrap_or(rest);
    let name = rest.split("...").next().unwrap_or(rest);
    let name = name.split(" [").next().unwrap_or(name);
    let name = name.trim();
    if name.is_empty() { "unknown".to_string() } else { name.to_string() }
}

pub async fn git_status(cwd: &Path, path_filter: Option<&str>) -> Result<String, ToolError> {
    let mut args: Vec<&str> = vec!["status", "--porcelain=v1", "-b"];
    if let Some(pf) = path_filter {
        args.push("--");
        args.push(pf);
    }
    let out = run_with_timeout("git", &args, cwd, GIT_TIMEOUT).await?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Ok(format!("git error: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return Ok("Clean working directory (no changes)".to_string());
    }

    let mut lines = stdout.lines();
    // The branch header is the first line and the only one: a file whose name
    // begins with `## ` is quoted by git and cannot be mistaken for it.
    let branch = lines
        .next()
        .filter(|line| line.starts_with("## "))
        .map(branch_name)
        .unwrap_or_else(|| "unknown".to_string());
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    for line in lines {
        if let Some(u) = line.strip_prefix("?? ") {
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

pub async fn git_diff(cwd: &Path, staged: bool, path_filter: Option<&str>) -> Result<String, ToolError> {
    let mut args: Vec<&str> = vec!["diff"];
    if staged {
        args.push("--cached");
    }
    if let Some(pf) = path_filter {
        args.push("--");
        args.push(pf);
    }
    let out = run_with_timeout("git", &args, cwd, GIT_TIMEOUT).await?;

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

    #[tokio::test]
    async fn git_status_non_git_dir_returns_error_message() {
        let temp = crate::testing::tempdir();
        let res = git_status(&temp, None).await.unwrap();
        assert!(res.contains("not a git repository") || res.contains("git error"));
    }

    #[tokio::test]
    async fn git_diff_non_git_dir_returns_error_message() {
        let temp = crate::testing::tempdir();
        let res = git_diff(&temp, false, None).await.unwrap();
        assert!(res.contains("not a git repository") || res.contains("git error"));
    }

    /// A repository with one commit on a branch of its own.
    fn repo_with_branch() -> std::path::PathBuf {
        let dir = crate::testing::tempdir();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "."]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-qm", "one"]);
        run(&["checkout", "-q", "-b", "feature"]);
        dir
    }

    /// The branch is the branch: not the tracking branch and not the counts
    /// that follow it.
    #[tokio::test]
    async fn the_branch_is_the_branch_and_not_the_whole_header() {
        let dir = repo_with_branch();
        let out = git_status(&dir, None).await.unwrap();
        assert!(out.contains("Branch: feature"), "{out}");
    }

    /// With an upstream, the header names two branches and how far apart they
    /// are. The branch is the first of them.
    #[tokio::test]
    async fn a_branch_with_an_upstream_is_only_its_own_name() {
        let dir = repo_with_branch();
        let head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .output()
            .expect("git runs");
        let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
        for args in [
            vec!["update-ref", "refs/remotes/origin/feature", sha.as_str()],
            vec!["config", "branch.feature.remote", "origin"],
            vec!["config", "branch.feature.merge", "refs/heads/feature"],
        ] {
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(&dir)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?} failed");
        }
        let out = git_status(&dir, None).await.unwrap();
        assert!(out.starts_with("Branch: feature"), "{out}");
        assert!(!out.contains("origin/feature"), "the tracking branch is not the branch: {out}");
    }

    /// A file whose name begins with `## ` is a file, not a branch.
    #[tokio::test]
    async fn a_file_named_like_a_branch_header_is_not_the_branch() {
        let dir = repo_with_branch();
        std::fs::write(dir.join("## not-a-branch"), "x\n").unwrap();
        let out = git_status(&dir, None).await.unwrap();
        assert!(out.contains("Branch: feature"), "{out}");
        assert!(out.contains("not-a-branch"), "{out}");
    }

    /// A command that never ends is stopped: the turn must not wait for it
    /// forever.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_git_command_that_does_not_finish_is_stopped() {
        let dir = crate::testing::tempdir();
        let started = std::time::Instant::now();
        let err = run_with_timeout("sh", &["-c", "sleep 30"], &dir, Duration::from_millis(200))
            .await
            .expect_err("a command that never ends must not be waited for");
        assert!(err.to_string().contains("did not finish"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }
}
