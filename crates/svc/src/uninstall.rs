//! Uninstall: every binary copy the installers put down, their PATH lines,
//! and the parts of `~/.flashagent` the user picks. A package-managed install
//! is removed through its package manager. Only [`run_interactive`] touches
//! the real home and executable; the rest works on given paths and is tested
//! on temp folders.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const PATH_MARKER: &str = "# FlashAgent";

/// Relative to the home directory.
const RC_FILES: &[&str] = &[".bashrc", ".bash_profile", ".zshrc", ".zprofile", ".profile", ".config/fish/config.fish"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Pacman,
    Dpkg,
    Xbps,
}

impl PackageManager {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pacman => "pacman",
            Self::Dpkg => "dpkg",
            Self::Xbps => "xbps",
        }
    }

    /// Without `sudo`. The package manager's own confirmation is left to the user.
    pub fn remove_command(self, package: &str) -> Vec<String> {
        let parts: &[&str] = match self {
            Self::Pacman => &["pacman", "-R"],
            Self::Dpkg => &["dpkg", "--remove"],
            Self::Xbps => &["xbps-remove"],
        };
        parts.iter().map(|s| s.to_string()).chain(std::iter::once(package.to_string())).collect()
    }
}

/// Prints the package name alone.
pub fn parse_pacman_owner(stdout: &str) -> Option<String> {
    stdout.lines().next().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Prints `package: /path` (or `pkg1, pkg2: /path`).
pub fn parse_dpkg_owner(stdout: &str) -> Option<String> {
    let line = stdout.lines().next()?;
    // An error message has the same colon, but no path.
    let (owners, path) = line.split_once(": ")?;
    if !path.starts_with('/') {
        return None;
    }
    owners.split(',').next().map(str::trim).filter(|s| !s.is_empty() && !s.contains(' ')).map(str::to_string)
}

/// Prints `pkgname-version_revision: /path (regular file)`.
pub fn parse_xbps_owner(stdout: &str) -> Option<String> {
    let line = stdout.lines().next()?;
    let (pkgver, _) = line.split_once(": ")?;
    let (name, version) = pkgver.trim().rsplit_once('-')?;
    (!name.is_empty() && version.chars().next().is_some_and(|c| c.is_ascii_digit())).then(|| name.to_string())
}

pub fn package_owner(binary: &Path) -> Option<(PackageManager, String)> {
    let path = binary.to_string_lossy().to_string();
    let ask = |program: &str, args: &[&str]| -> Option<String> {
        let out = Command::new(program).args(args).arg(&path).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
    };
    if let Some(name) = ask("pacman", &["-Qqo"]).as_deref().and_then(parse_pacman_owner) {
        return Some((PackageManager::Pacman, name));
    }
    if let Some(name) = ask("dpkg", &["-S"]).as_deref().and_then(parse_dpkg_owner) {
        return Some((PackageManager::Dpkg, name));
    }
    if let Some(name) = ask("xbps-query", &["-o"]).as_deref().and_then(parse_xbps_owner) {
        return Some((PackageManager::Xbps, name));
    }
    None
}

/// One part of `~/.flashagent`, deletable on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPart {
    pub label: &'static str,
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    /// Rebuildable things (caches, `/rewind` copies) are ticked; what the user
    /// wrote or chose is not.
    pub selected_by_default: bool,
}

/// Parts with nothing on disk are left out; unknown files are "Other files".
pub fn data_parts(data: &Path) -> Vec<DataPart> {
    const KNOWN: &[(&str, &[&str], bool)] = &[
        ("Settings", &["config.json"], false),
        ("Saved sessions", &["sessions"], false),
        ("Memory", &["MEMORY.md", "memory"], false),
        ("MCP servers", &["mcp.json"], false),
        ("Skills and rules", &["skills", "rules"], false),
        ("Rewind snapshots", &["snapshots"], true),
        ("Caches", &["effort.json", "image-costs.json", "prefill_cache.json"], true),
    ];
    let mut parts = Vec::new();
    for (label, names, selected) in KNOWN {
        let paths: Vec<PathBuf> = names.iter().map(|n| data.join(n)).filter(|p| p.symlink_metadata().is_ok()).collect();
        if !paths.is_empty() {
            let bytes = paths.iter().map(|p| size_of(p)).sum();
            parts.push(DataPart { label, paths, bytes, selected_by_default: *selected });
        }
    }
    let known: Vec<&str> = KNOWN.iter().flat_map(|(_, names, _)| names.iter().copied()).collect();
    let mut other: Vec<PathBuf> = std::fs::read_dir(data)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !known.contains(&n)))
                .collect()
        })
        .unwrap_or_default();
    other.sort();
    if !other.is_empty() {
        let bytes = other.iter().map(|p| size_of(p)).sum();
        parts.push(DataPart { label: "Other files", paths: other, bytes, selected_by_default: false });
    }
    parts
}

/// Symlinks counted as themselves, never followed.
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = path.symlink_metadata() else { return 0 };
    if meta.is_dir() {
        std::fs::read_dir(path).map(|es| es.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0)
    } else {
        meta.len()
    }
}

/// `1536` → `1.5 KB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// Removes the marker, the PATH line after it for `dir`, and the blank line
/// before the marker. `None` when nothing is ours; a line the user wrote is
/// never touched, even one naming the same folder.
pub fn strip_path_lines(text: &str, dir: &str) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut keep: Vec<&str> = Vec::with_capacity(lines.len());
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        let is_ours = lines[i].trim() == PATH_MARKER
            && lines.get(i + 1).is_some_and(|next| {
                let next = next.trim();
                next.contains(dir) && (next.starts_with("fish_add_path") || next.starts_with("export PATH="))
            });
        if is_ours {
            if keep.last().is_some_and(|l| l.trim().is_empty()) {
                keep.pop();
            }
            changed = true;
            i += 2;
            continue;
        }
        keep.push(lines[i]);
        i += 1;
    }
    changed.then(|| keep.join("\n"))
}

/// `None` when `dir` is absent. Compared as Windows does: case-insensitive,
/// trailing backslash ignored.
pub fn remove_from_path_list(path_var: &str, dir: &str) -> Option<String> {
    let norm = |s: &str| s.trim().trim_end_matches(['\\', '/']).to_lowercase();
    let target = norm(dir);
    let entries: Vec<&str> = path_var.split(';').filter(|e| !e.trim().is_empty()).collect();
    let kept: Vec<&str> = entries.iter().copied().filter(|e| norm(e) != target).collect();
    (kept.len() != entries.len()).then(|| kept.join(";"))
}

/// The running binary and every place the installers and updater write to.
/// Existing paths only, each once.
pub fn binary_locations(exe: &Path, home: &Path, local_app_data: Option<&Path>) -> Vec<PathBuf> {
    let exe_name = if cfg!(windows) { "flashagent.exe" } else { "flashagent" };
    let mut candidates = vec![exe.to_path_buf(), home.join(".local").join("bin").join(exe_name)];
    // Leftovers of older installs next to either of those.
    for dir in [exe.parent().map(Path::to_path_buf), Some(home.join(".local").join("bin"))].into_iter().flatten() {
        candidates.push(dir.join(if cfg!(windows) { "flashagent-tui.exe" } else { "flashagent-tui" }));
        candidates.push(dir.join(format!("{exe_name}.old")));
    }
    if let Some(local) = local_app_data {
        candidates.push(local.join("Programs").join("FlashAgent").join(exe_name));
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    for c in candidates {
        let key = std::fs::canonicalize(&c).unwrap_or_else(|_| c.clone());
        if c.symlink_metadata().is_ok() && !seen.iter().any(|s| std::fs::canonicalize(s).unwrap_or_else(|_| s.clone()) == key) {
            seen.push(c);
        }
    }
    seen
}

/// Worked out before anything is touched.
#[derive(Debug, Clone)]
pub struct Plan {
    /// When set, the binaries are the package's to remove, not ours.
    pub package: Option<(PackageManager, String)>,
    pub binaries: Vec<PathBuf>,
    /// (file, folder it adds to PATH)
    pub rc_files: Vec<(PathBuf, PathBuf)>,
    pub data_dir: PathBuf,
    pub data: Vec<DataPart>,
}

pub fn plan(exe: &Path, home: &Path, local_app_data: Option<&Path>, package: Option<(PackageManager, String)>) -> Plan {
    let binaries = if package.is_some() { vec![exe.to_path_buf()] } else { binary_locations(exe, home, local_app_data) };
    let mut dirs: Vec<PathBuf> = binaries.iter().filter_map(|b| b.parent().map(Path::to_path_buf)).collect();
    dirs.push(home.join(".local").join("bin"));
    dirs.sort();
    dirs.dedup();
    let mut rc_files = Vec::new();
    for rc in RC_FILES {
        let path = home.join(rc);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for dir in &dirs {
            if strip_path_lines(&text, &dir.to_string_lossy()).is_some() {
                rc_files.push((path.clone(), dir.clone()));
            }
        }
    }
    let data_dir = home.join(".flashagent");
    let data = data_parts(&data_dir);
    Plan { package, binaries, rc_files, data_dir, data }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub removed: Vec<String>,
    pub kept: Vec<String>,
    pub failed: Vec<String>,
}

/// Everything except the package removal, which needs a terminal and `sudo`
/// and runs in [`run_interactive`]. `delete` selects data parts.
pub fn execute(plan: &Plan, delete: &[bool]) -> Report {
    let mut report = Report::default();
    if plan.package.is_none() {
        for binary in &plan.binaries {
            match remove_binary(binary) {
                Ok(()) => report.removed.push(binary.display().to_string()),
                Err(e) => report.failed.push(format!("{}: {e}", binary.display())),
            }
        }
    }
    for (rc, dir) in &plan.rc_files {
        // Only a folder FlashAgent was alone in loses its PATH line.
        if dir_has_files(dir) {
            report.kept.push(format!("PATH line in {} ({} still holds other files)", rc.display(), dir.display()));
            continue;
        }
        match strip_rc_file(rc, dir) {
            Ok(true) => report.removed.push(format!("PATH line in {} (backup: {})", rc.display(), backup_path(rc).display())),
            Ok(false) => {}
            Err(e) => report.failed.push(format!("{}: {e}", rc.display())),
        }
    }
    for (part, wanted) in plan.data.iter().zip(delete.iter().copied().chain(std::iter::repeat(false))) {
        if !wanted {
            report.kept.push(format!("{} ({})", part.label, human_size(part.bytes)));
            continue;
        }
        let mut ok = true;
        for path in &part.paths {
            let result = match path.symlink_metadata() {
                Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
                Ok(_) => std::fs::remove_file(path),
                Err(_) => Ok(()),
            };
            if let Err(e) = result {
                ok = false;
                report.failed.push(format!("{}: {e}", path.display()));
            }
        }
        if ok {
            report.removed.push(format!("{} ({})", part.label, human_size(part.bytes)));
        }
    }
    // The folder itself goes only once it is empty.
    if std::fs::read_dir(&plan.data_dir).is_ok_and(|mut d| d.next().is_none()) && std::fs::remove_dir(&plan.data_dir).is_ok() {
        report.removed.push(plan.data_dir.display().to_string());
    }
    report
}

fn dir_has_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

fn backup_path(rc: &Path) -> PathBuf {
    let mut name = rc.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".flashagent-uninstall.bak");
    rc.with_file_name(name)
}

/// Keeps a backup copy first.
fn strip_rc_file(rc: &Path, dir: &Path) -> std::io::Result<bool> {
    let text = std::fs::read_to_string(rc)?;
    let Some(stripped) = strip_path_lines(&text, &dir.to_string_lossy()) else {
        return Ok(false);
    };
    std::fs::copy(rc, backup_path(rc))?;
    std::fs::write(rc, stripped)?;
    Ok(true)
}

fn remove_binary(binary: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // A running .exe cannot be deleted but can be moved; a detached shell
        // deletes it after exit.
        if std::env::current_exe().ok().and_then(|e| std::fs::canonicalize(e).ok()) == std::fs::canonicalize(binary).ok() {
            let parked = std::env::temp_dir().join(format!("flashagent-uninstall-{}.exe", std::process::id()));
            std::fs::rename(binary, &parked)?;
            let _ = Command::new("cmd")
                .args(["/C", "ping", "127.0.0.1", "-n", "3", ">", "nul", "&", "del", "/f", "/q"])
                .arg(&parked)
                .spawn();
            if let Some(parent) = binary.parent() {
                crate::updater::remove_stale_backups(parent);
                let _ = std::fs::remove_dir(parent);
            }
            return Ok(());
        }
    }
    std::fs::remove_file(binary)?;
    #[cfg(windows)]
    if let Some(parent) = binary.parent().filter(|p| p.ends_with("Programs\\FlashAgent") || p.ends_with("FlashAgent")) {
        crate::updater::remove_stale_backups(parent);
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

/// Empty answer takes `default`; anything else asks again, at most three times.
pub fn ask_yes_no(input: &mut impl BufRead, out: &mut impl Write, question: &str, default: bool) -> bool {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    for _ in 0..3 {
        let _ = write!(out, "{question} {hint} ");
        let _ = out.flush();
        let mut line = String::new();
        if input.read_line(&mut line).unwrap_or(0) == 0 {
            return default;
        }
        match line.trim().to_lowercase().as_str() {
            "" => return default,
            "y" | "yes" => return true,
            "n" | "no" => return false,
            _ => {
                let _ = writeln!(out, "Please answer y or n.");
            }
        }
    }
    default
}

/// `flashagent --uninstall`. `assume_yes` takes every default and confirms,
/// for scripts.
pub fn run_interactive(assume_yes: bool) -> anyhow::Result<()> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("no home directory to uninstall from"))?;
    let exe = std::env::current_exe()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if crate::updater::is_dev_mode() {
        anyhow::bail!(
            "this is a build from source ({}); remove it by deleting the checkout, not with --uninstall",
            exe.display()
        );
    }
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let package = if cfg!(unix) { package_owner(&exe) } else { None };
    let plan = plan(&exe, &home, local_app_data.as_deref(), package);

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();

    println!("Uninstall FlashAgent\n");
    match &plan.package {
        Some((manager, name)) => {
            println!("  Installed by {}: package {name}", manager.label());
            println!("  It will be removed with: sudo {}", manager.remove_command(name).join(" "));
        }
        None => {
            println!("  Program:");
            for b in &plan.binaries {
                println!("    {}", b.display());
            }
        }
    }
    if !plan.rc_files.is_empty() {
        println!("  PATH lines added by the installer (a backup of each file is kept):");
        for (rc, dir) in &plan.rc_files {
            println!("    {}  ({})", rc.display(), dir.display());
        }
    }
    #[cfg(windows)]
    let windows_path_dirs: Vec<PathBuf> = plan.binaries.iter().filter_map(|b| b.parent().map(Path::to_path_buf)).collect();

    let mut delete = Vec::new();
    if plan.data.is_empty() {
        println!("  No data in {}", plan.data_dir.display());
    } else {
        println!("\n  Data in {} — choose what to delete:", plan.data_dir.display());
        for part in &plan.data {
            let question = format!("    Delete {} ({})?", part.label, human_size(part.bytes));
            let yes = if assume_yes {
                println!("{question} {}", if part.selected_by_default { "yes" } else { "no" });
                part.selected_by_default
            } else {
                ask_yes_no(&mut input, &mut out, &question, part.selected_by_default)
            };
            delete.push(yes);
        }
    }
    println!();
    if !assume_yes && !ask_yes_no(&mut input, &mut out, "Uninstall FlashAgent now?", false) {
        println!("Nothing was changed.");
        return Ok(());
    }

    if let Some((manager, name)) = &plan.package {
        let mut command = manager.remove_command(name);
        let is_root = Command::new("id").arg("-u").output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0");
        if !is_root {
            command.insert(0, "sudo".to_string());
        }
        let status = Command::new(&command[0]).args(&command[1..]).status()?;
        if !status.success() {
            anyhow::bail!("{} did not remove {name}; nothing else was changed", command.join(" "));
        }
    }

    let report = execute(&plan, &delete);

    #[cfg(unix)]
    for dir in plan.binaries.iter().filter_map(|b| b.parent()) {
        // `fish_add_path -U` keeps the folder in a universal variable, not a file.
        if !dir_has_files(dir) {
            let script = format!(
                "if set -l i (contains -i -- '{}' $fish_user_paths); set -Ue fish_user_paths[$i]; end",
                dir.display()
            );
            let _ = Command::new("fish").args(["-c", &script]).output();
        }
    }
    #[cfg(windows)]
    for dir in windows_path_dirs.iter().filter(|d| !dir_has_files(d)) {
        let read = Command::new("powershell")
            .args(["-NoProfile", "-Command", "[Environment]::GetEnvironmentVariable('Path','User')"])
            .output();
        if let Ok(read) = read {
            let current = String::from_utf8_lossy(&read.stdout).trim().to_string();
            if let Some(updated) = remove_from_path_list(&current, &dir.to_string_lossy()) {
                let set = format!("[Environment]::SetEnvironmentVariable('Path', '{}', 'User')", updated.replace('\'', "''"));
                let _ = Command::new("powershell").args(["-NoProfile", "-Command", &set]).status();
            }
        }
    }

    println!("\nFlashAgent is uninstalled.");
    for line in &report.removed {
        println!("  removed  {line}");
    }
    for line in &report.kept {
        println!("  kept     {line}");
    }
    for line in &report.failed {
        println!("  FAILED   {line}");
    }
    if !report.failed.is_empty() {
        anyhow::bail!("some things could not be removed; see above");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_owners_are_read_from_each_managers_output() {
        assert_eq!(parse_pacman_owner("flashagent-bin\n").as_deref(), Some("flashagent-bin"));
        assert_eq!(parse_pacman_owner(""), None);
        assert_eq!(parse_dpkg_owner("flashagent: /usr/bin/flashagent\n").as_deref(), Some("flashagent"));
        assert_eq!(parse_dpkg_owner("dpkg-query: no path found matching pattern /x"), None);
        assert_eq!(parse_xbps_owner("flashagent-0.1.0_1: /usr/bin/flashagent (regular file)").as_deref(), Some("flashagent"));
        assert_eq!(parse_xbps_owner("garbage"), None);
        assert_eq!(PackageManager::Pacman.remove_command("flashagent-bin"), ["pacman", "-R", "flashagent-bin"]);
        assert_eq!(PackageManager::Dpkg.remove_command("flashagent"), ["dpkg", "--remove", "flashagent"]);
    }

    #[test]
    fn only_the_installers_own_path_lines_are_removed() {
        let dir = "/home/me/.local/bin";
        let bashrc = format!("alias ll='ls -l'\n\n{PATH_MARKER}\nexport PATH=\"{dir}:$PATH\"\n# mine\nexport PATH=\"{dir}:$PATH\"\n");
        let stripped = strip_path_lines(&bashrc, dir).expect("the installer's lines are there");
        assert_eq!(stripped, format!("alias ll='ls -l'\n# mine\nexport PATH=\"{dir}:$PATH\"\n"), "the user's own line for the same folder stays");

        let fish = format!("set -g x 1\n\n{PATH_MARKER}\nfish_add_path {dir}\n");
        assert_eq!(strip_path_lines(&fish, dir).unwrap(), "set -g x 1\n");

        assert_eq!(strip_path_lines("export PATH=\"/opt/bin:$PATH\"\n", dir), None, "nothing of ours: file untouched");
        let other_dir = format!("{PATH_MARKER}\nexport PATH=\"/usr/local/bin:$PATH\"\n");
        assert_eq!(strip_path_lines(&other_dir, dir), None, "a line for another folder is not this folder's");
    }

    #[test]
    fn a_windows_path_entry_is_removed_the_way_windows_compares_it() {
        let path = r"C:\Windows;C:\Users\me\AppData\Local\Programs\FlashAgent\;C:\tools";
        assert_eq!(
            remove_from_path_list(path, r"c:\users\me\appdata\local\programs\flashagent").as_deref(),
            Some(r"C:\Windows;C:\tools")
        );
        assert_eq!(remove_from_path_list(path, r"C:\nowhere"), None);
    }

    fn home_with_everything() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let data = h.join(".flashagent");
        std::fs::create_dir_all(data.join("sessions")).unwrap();
        std::fs::create_dir_all(data.join("snapshots/s1")).unwrap();
        std::fs::write(data.join("config.json"), "{}").unwrap();
        std::fs::write(data.join("sessions/s.json"), "x".repeat(2048)).unwrap();
        std::fs::write(data.join("snapshots/s1/blob"), "y".repeat(100)).unwrap();
        std::fs::write(data.join("effort.json"), "{}").unwrap();
        std::fs::write(data.join("something-new.txt"), "z").unwrap();
        std::fs::create_dir_all(h.join(".local/bin")).unwrap();
        std::fs::write(h.join(".local/bin/flashagent"), "binary").unwrap();
        let dir = h.join(".local/bin").to_string_lossy().to_string();
        std::fs::write(h.join(".bashrc"), format!("alias x=y\n\n{PATH_MARKER}\nexport PATH=\"{dir}:$PATH\"\n")).unwrap();
        home
    }

    #[test]
    fn data_is_offered_part_by_part_with_only_rebuildable_parts_ticked() {
        let home = home_with_everything();
        let parts = data_parts(&home.path().join(".flashagent"));
        let summary: Vec<(&str, bool)> = parts.iter().map(|p| (p.label, p.selected_by_default)).collect();
        assert_eq!(
            summary,
            [("Settings", false), ("Saved sessions", false), ("Rewind snapshots", true), ("Caches", true), ("Other files", false)]
        );
        let sessions = parts.iter().find(|p| p.label == "Saved sessions").unwrap();
        assert_eq!(sessions.bytes, 2048);
        assert_eq!(human_size(sessions.bytes), "2.0 KB");
        assert!(data_parts(&home.path().join("missing")).is_empty());
    }

    #[test]
    fn an_uninstall_removes_the_program_its_path_line_and_only_the_chosen_data() {
        let home = home_with_everything();
        let h = home.path();
        let exe = h.join(".local/bin/flashagent");
        let plan = plan(&exe, h, None, None);
        assert_eq!(plan.binaries, std::slice::from_ref(&exe));
        assert_eq!(plan.rc_files.len(), 1);

        let delete: Vec<bool> = plan.data.iter().map(|p| p.selected_by_default || p.label == "Settings").collect();
        let report = execute(&plan, &delete);
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        assert!(!exe.exists(), "the binary is gone");
        let bashrc = std::fs::read_to_string(h.join(".bashrc")).unwrap();
        assert_eq!(bashrc, "alias x=y\n", "the PATH line is gone and the rest kept");
        assert!(h.join(".bashrc.flashagent-uninstall.bak").exists(), "a backup was kept");
        let data = h.join(".flashagent");
        assert!(!data.join("config.json").exists() && !data.join("snapshots").exists() && !data.join("effort.json").exists());
        assert!(data.join("sessions/s.json").exists(), "saved sessions were not chosen and must stay");
        assert!(data.join("something-new.txt").exists());
    }

    #[test]
    fn a_path_line_stays_while_its_folder_still_holds_other_programs() {
        let home = home_with_everything();
        let h = home.path();
        std::fs::write(h.join(".local/bin/some-other-tool"), "tool").unwrap();
        let plan = plan(&h.join(".local/bin/flashagent"), h, None, None);
        let report = execute(&plan, &vec![false; plan.data.len()]);
        assert!(std::fs::read_to_string(h.join(".bashrc")).unwrap().contains(PATH_MARKER));
        assert!(report.kept.iter().any(|k| k.contains("still holds other files")), "{:?}", report.kept);
    }

    #[test]
    fn deleting_every_part_removes_the_data_folder_itself() {
        let home = home_with_everything();
        let h = home.path();
        let plan = plan(&h.join(".local/bin/flashagent"), h, None, None);
        let report = execute(&plan, &vec![true; plan.data.len()]);
        assert!(!h.join(".flashagent").exists(), "{report:?}");
    }

    #[test]
    fn a_package_install_leaves_its_files_to_the_package_manager() {
        let home = home_with_everything();
        let h = home.path();
        let exe = h.join(".local/bin/flashagent");
        let plan = plan(&exe, h, None, Some((PackageManager::Pacman, "flashagent-bin".into())));
        execute(&plan, &vec![false; plan.data.len()]);
        assert!(exe.exists(), "files a package owns are removed by the package manager, not by us");
    }

    #[test]
    fn questions_take_their_default_on_enter_and_ask_again_on_nonsense() {
        let mut out = Vec::new();
        assert!(ask_yes_no(&mut "\n".as_bytes(), &mut out, "q?", true));
        assert!(!ask_yes_no(&mut "\n".as_bytes(), &mut out, "q?", false));
        assert!(ask_yes_no(&mut "maybe\nyes\n".as_bytes(), &mut out, "q?", false));
        assert!(!ask_yes_no(&mut "N\n".as_bytes(), &mut out, "q?", true));
        assert!(!ask_yes_no(&mut "".as_bytes(), &mut out, "q?", false), "a closed input is the default, never a yes");
        assert!(String::from_utf8(out).unwrap().contains("Please answer y or n."));
    }
}
