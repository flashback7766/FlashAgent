//! Environment inspection tool (`env_info`).
//!
//! Provides system platform, architecture, working directory, and installed toolchain versions.

use std::path::Path;
use std::process::Command;
use crate::ToolError;

fn check_tool_version(cmd: &str, arg: &str) -> Option<String> {
    let out = Command::new(cmd).arg(arg).output().ok()?;
    if out.status.success() {
        let line = String::from_utf8_lossy(&out.stdout);
        let first_line = line.lines().next()?.trim();
        if !first_line.is_empty() {
            return Some(first_line.to_string());
        }
    }
    None
}

/// Gathers comprehensive environment and toolchain information.
pub fn env_info(cwd: &Path) -> Result<String, ToolError> {
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

    let mut found_tools = Vec::new();
    for (name, arg) in tools {
        if let Some(ver) = check_tool_version(name, arg) {
            found_tools.push(format!("  - {name}: {ver}"));
        }
    }

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

    #[test]
    fn test_env_info_reports_platform_and_cwd() {
        let temp = crate::testing::tempdir();
        let res = env_info(&temp).unwrap();
        assert!(res.contains("Platform:"));
        assert!(res.contains("Working Directory:"));
    }
}
