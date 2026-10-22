//! `env_info`: platform, architecture, working directory, toolchain versions.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::ToolError;

/// How long a toolchain may take to answer `--version`.
///
/// A package manager that waits for a lock, or a tool that asks something on
/// its standard input, never answers at all. The tool call must not wait for
/// it: the turn would never end.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

async fn check_tool_version(cmd: &str, arg: &str, timeout: Duration) -> Option<String> {
    let mut command = tokio::process::Command::new(crate::shell::find_program(cmd));
    command
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // The program dies with the future that waits for it.
        .kill_on_drop(true);
    let child = command.spawn().ok()?;
    let out = tokio::time::timeout(timeout, child.wait_with_output()).await.ok()?.ok()?;
    if out.status.success() {
        let line = String::from_utf8_lossy(&out.stdout);
        let first_line = line.lines().next()?.trim();
        if !first_line.is_empty() {
            return Some(first_line.to_string());
        }
    }
    None
}

pub async fn env_info(cwd: &Path) -> Result<String, ToolError> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let family = std::env::consts::FAMILY;

    let mut report = Vec::new();
    report.push(format!("Platform: {os} ({family}) - {arch}"));
    report.push(format!("Working Directory: {}", cwd.display()));

    let tools = [
        ("git", "--version"),
        ("cargo", "--version"),
        ("rustc", "--version"),
        ("node", "--version"),
        ("npm", "--version"),
        ("python3", "--version"),
        ("go", "version"),
        ("gcc", "--version"),
        ("clang", "--version"),
    ];

    // All at once: each asks a separate program, and `npm --version` alone can
    // take a second.
    let found_tools: Vec<String> = futures::future::join_all(
        tools.iter().map(|&(name, arg)| check_tool_version(name, arg, VERSION_TIMEOUT)),
    )
    .await
    .into_iter()
    .zip(tools.iter())
    .filter_map(|(answer, &(name, _))| answer.map(|ver| format!("  - {name}: {ver}")))
    .collect();

    if !found_tools.is_empty() {
        report.push("\nInstalled Toolchains:".to_string());
        report.extend(found_tools);
    } else {
        report.push("\nNo common developer toolchains detected on PATH.".to_string());
    }

    Ok(report.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_env_info_reports_platform_and_cwd() {
        let temp = crate::testing::tempdir();
        let res = env_info(&temp).await.unwrap();
        assert!(res.contains("Platform:"));
        assert!(res.contains("Working Directory:"));
    }

    /// A toolchain that never answers is given up on: the call must end.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_toolchain_that_never_answers_is_given_up_on() {
        let started = std::time::Instant::now();
        let answer = check_tool_version("sleep", "30", Duration::from_millis(200)).await;
        assert!(answer.is_none());
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }
}
