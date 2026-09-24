//! `env_info`: platform, architecture, working directory, toolchain versions.

use std::path::Path;
use std::process::Command;
use crate::ToolError;

fn check_tool_version(cmd: &str, arg: &str) -> Option<String> {
    let out = Command::new(crate::shell::find_program(cmd)).arg(arg).output().ok()?;
    if out.status.success() {
        let line = String::from_utf8_lossy(&out.stdout);
        let first_line = line.lines().next()?.trim();
        if !first_line.is_empty() {
            return Some(first_line.to_string());
        }
    }
    None
}

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

    // All at once: each asks a separate program, and `npm --version` alone can
    // take a second.
    let found_tools: Vec<String> = std::thread::scope(|scope| {
        let asked: Vec<_> = tools
            .iter()
            .map(|&(name, arg)| (name, scope.spawn(move || check_tool_version(name, arg))))
            .collect();
        asked
            .into_iter()
            .filter_map(|(name, answer)| answer.join().ok().flatten().map(|ver| format!("  - {name}: {ver}")))
            .collect()
    });

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
