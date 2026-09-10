//! In-app release updater and channel manager.
//!
//! Checks GitHub releases, filters by channel (Stable/Beta), downloads platform
//! binaries, and performs seamless atomic in-place updates.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
pub use flashagent_core::config::UpdateChannel;

/// Information about a GitHub release asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// GitHub release payload subset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub body: Option<String>,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

/// Result of checking for updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate {
        current: String,
        channel: UpdateChannel,
    },
    UpdateAvailable {
        target: String,
        channel: UpdateChannel,
        is_downgrade: bool,
        asset_name: String,
        download_url: String,
    },
}

/// The official GitHub repository releases API endpoint.
pub const DEFAULT_RELEASES_API: &str = "https://api.github.com/repos/flashback7766/FlashAgent/releases";

/// Default interval for background update checks (~3-5 minutes).
pub const BACKGROUND_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(240);

/// Get current application version string (e.g. "b200" or "0.1.0").
pub fn current_version() -> &'static str {
    option_env!("FLASHAGENT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Check if the application is running in development mode (from source repository or cargo target build).
///
/// In development mode, auto-updating is disabled to prevent overwriting binaries or dropping
/// unexpected files into ~/.local/bin/flashagent.
pub fn is_dev_mode() -> bool {
    // 1. Explicit dev environment override
    if std::env::var("FLASHAGENT_DEV")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return true;
    }

    // 2. Cargo execution environment
    if std::env::var_os("CARGO").is_some() || std::env::var_os("CARGO_MANIFEST_DIR").is_some() {
        return true;
    }

    // 3. Binary path check: running out of a cargo target directory
    if let Ok(exe) = std::env::current_exe() {
        let path_str = exe.to_string_lossy();
        if path_str.contains("/target/debug/")
            || path_str.contains("/target/release/")
            || path_str.contains("\\target\\debug\\")
            || path_str.contains("\\target\\release\\")
            || path_str.contains("/target/")
            || path_str.contains("\\target\\")
        {
            return true;
        }
    }

    // 4. Source tree detection: Cargo.toml + crates/ directory in current working dir or its parents
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            if dir.join("Cargo.toml").is_file() && (dir.join("crates").is_dir() || dir.join(".git").is_dir()) {
                return true;
            }
            if !dir.pop() {
                break;
            }
        }
    }

    // 5. Debug compilation profile
    #[cfg(debug_assertions)]
    {
        true
    }

    #[cfg(not(debug_assertions))]
    false
}

/// Filter releases for the requested channel.
pub fn find_target_release(releases: &[GitHubRelease], channel: UpdateChannel) -> Option<&GitHubRelease> {
    for rel in releases {
        match channel {
            UpdateChannel::Stable => {
                // Stable: official release tag "release", or official release not marked as prerelease and non-beta
                if rel.tag_name == "release"
                    || (!rel.prerelease
                        && rel.tag_name != "beta"
                        && (rel.tag_name.starts_with('v') || !rel.tag_name.starts_with('b')))
                {
                    return Some(rel);
                }
            }
            UpdateChannel::Beta => {
                // Beta: rolling tag "beta", pre-release, or tag starts with 'b'
                if rel.tag_name == "beta" || rel.prerelease || rel.tag_name.starts_with('b') {
                    return Some(rel);
                }
            }
        }
    }
    None
}

/// Extract effective version string from release (from title like "FlashAgent b200", tag_name, or assets).
pub fn extract_release_version(rel: &GitHubRelease) -> String {
    // 1. First search title (e.g. "FlashAgent b200" or "FlashAgent v1.0.0")
    if let Some(ref name) = rel.name {
        for word in name.split_whitespace() {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.');
            if (clean.starts_with('b') && clean.len() > 1 && clean[1..].chars().all(|c| c.is_ascii_digit()))
                || (clean.starts_with('v') && clean.len() > 1 && clean[1..].chars().next().is_some_and(|c| c.is_ascii_digit()))
            {
                return clean.to_string();
            }
        }
    }

    // 2. If tag_name is an actual version tag (not a channel tag like "beta" or "release")
    let lower_tag = rel.tag_name.to_lowercase();
    if lower_tag != "beta" && lower_tag != "release" && lower_tag != "stable" && lower_tag != "latest" {
        return rel.tag_name.clone();
    }

    // 3. Search assets for version pattern like -b200- or _b200
    for asset in &rel.assets {
        for part in asset.name.split(['-', '_', '.']) {
            if part.starts_with('b') && part.len() > 1 && part[1..].chars().all(|c| c.is_ascii_digit()) {
                return part.to_string();
            }
            if part.starts_with('v') && part.len() > 1 && part[1..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return part.to_string();
            }
        }
    }

    // 4. Search release body
    if let Some(ref body) = rel.body {
        for word in body.split_whitespace() {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.');
            if clean.starts_with('b') && clean.len() > 1 && clean[1..].chars().all(|c| c.is_ascii_digit()) {
                return clean.to_string();
            }
        }
    }

    // Fallback: never return rolling channel name as a version
    current_version().to_string()
}

/// Detect best matching asset for the current OS and architecture.
pub fn find_platform_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        // 1. Direct raw binary if available
        if let Some(a) = assets.iter().find(|a| a.name.contains("flashagent") && a.name.contains("linux-x86_64") && !a.name.ends_with(".tar.gz") && !a.name.ends_with(".zst") && !a.name.ends_with(".deb")) {
            return Some(a);
        }
        // 2. Generic linux tarball containing binary
        if let Some(a) = assets.iter().find(|a| a.name.contains("linux") && a.name.contains("x86_64") && a.name.ends_with(".tar.gz") && !a.name.contains("void")) {
            return Some(a);
        }
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        if let Some(a) = assets.iter().find(|a| a.name.ends_with(".exe") && a.name.contains("windows")) {
            return Some(a);
        }
        if let Some(a) = assets.iter().find(|a| a.name.contains("windows") && (a.name.contains("x86_64") || a.name.contains("amd64")) && a.name.ends_with(".zip")) {
            return Some(a);
        }
        if let Some(a) = assets.iter().find(|a| a.name.contains("windows") && a.name.ends_with(".zip")) {
            return Some(a);
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        if let Some(a) = assets.iter().find(|a| a.name.contains("macos") && (a.name.contains("aarch64") || a.name.contains("arm64")) && a.name.ends_with(".tar.gz")) {
            return Some(a);
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        if let Some(a) = assets.iter().find(|a| a.name.contains("macos") && a.name.contains("x86_64") && a.name.ends_with(".tar.gz")) {
            return Some(a);
        }
    }

    // Fallback: check any asset with linux tarball or matching name
    assets.iter().find(|a| a.name.contains("tar.gz") || a.name.contains("zip"))
}

/// Determine whether `target` represents a downgrade from `current`.
pub fn is_downgrade(current: &str, target: &str, channel: UpdateChannel) -> bool {
    if channel == UpdateChannel::Stable && current.starts_with('b') && target.starts_with('v') {
        // Switching from Beta to Stable is a downgrade to official release
        return true;
    }
    // Simple tag / build number comparison
    if let (Some(cur_b), Some(tgt_b)) = (current.strip_prefix('b'), target.strip_prefix('b')) {
        if let (Ok(c), Ok(t)) = (cur_b.parse::<u64>(), tgt_b.parse::<u64>()) {
            return t < c;
        }
    }
    false
}

/// Checks GitHub releases API and returns update status.
pub async fn check_for_updates(channel: UpdateChannel, api_url: &str) -> anyhow::Result<UpdateStatus> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(format!("FlashAgent-Updater/{}", current_version()))
        .build()?;

    let resp = client.get(api_url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub API returned HTTP {}", resp.status());
    }

    let releases: Vec<GitHubRelease> = resp.json().await?;
    let cur = current_version();

    if let Some(target_rel) = find_target_release(&releases, channel) {
        let version = extract_release_version(target_rel);
        let tag = target_rel.tag_name.clone();
        // Check if version differs from current running version
        let needs_update = (version != cur && version != format!("v{cur}")) || (tag != cur && tag != "beta" && tag != "release");
        if needs_update && version != cur {
            if let Some(asset) = find_platform_asset(&target_rel.assets) {
                return Ok(UpdateStatus::UpdateAvailable {
                    target: version.clone(),
                    channel,
                    is_downgrade: is_downgrade(cur, &version, channel),
                    asset_name: asset.name.clone(),
                    download_url: asset.browser_download_url.clone(),
                });
            }
        }
    }

    Ok(UpdateStatus::UpToDate {
        current: cur.to_string(),
        channel,
    })
}

/// Unpack binary bytes from downloaded asset payload (handles tar.gz or direct binary).
pub fn extract_binary_bytes(asset_name: &str, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    if asset_name.ends_with(".tar.gz") {
        use flate2::read::GzDecoder;
        use tar::Archive;

        let gz = GzDecoder::new(payload);
        let mut archive = Archive::new(gz);

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "flashagent" || name == "flashagent-tui" || name.starts_with("flashagent") {
                let mut binary_bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut binary_bytes)?;
                return Ok(binary_bytes);
            }
        }
        anyhow::bail!("No executable binary found in archive {asset_name}");
    }

    // Direct binary payload
    Ok(payload.to_vec())
}

/// Test if the destination path can be written to by the current process.
pub fn is_writable(path: &Path) -> bool {
    if path.exists() {
        std::fs::OpenOptions::new().write(true).open(path).is_ok()
    } else if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).is_ok()
            && std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(parent.join(".write_test"))
                .map(|_| {
                    let _ = std::fs::remove_file(parent.join(".write_test"));
                    true
                })
                .unwrap_or(false)
    } else {
        false
    }
}

/// Resolve the best target path for installing the updated binary.
///
/// If `current_exe()` is writable (e.g. `/usr/bin/flashagent` with write rights,
/// or standalone portable/AppImage), replaces it directly for all users.
///
/// If `current_exe()` is read-only (e.g. `/usr/bin/flashagent` under standard non-root user),
/// installs to `~/.local/bin/flashagent` so the current user and session can use it seamlessly.
pub fn resolve_install_target() -> anyhow::Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        // Resolve symlinks to target real binary file
        let real_path = std::fs::canonicalize(&exe).unwrap_or(exe);
        if is_writable(&real_path) {
            return Ok(real_path);
        }
    }

    // Fallback to ~/.local/bin/flashagent (standard XDG path in Linux/macOS $PATH)
    if let Ok(home) = std::env::var("HOME") {
        let local_bin = PathBuf::from(home).join(".local").join("bin");
        std::fs::create_dir_all(&local_bin)?;
        return Ok(local_bin.join("flashagent"));
    }

    // Last resort: current working directory
    Ok(PathBuf::from("flashagent"))
}

/// Atomically replace the target executable file on disk.
pub fn atomic_replace_executable(target: &Path, new_binary_bytes: &[u8]) -> anyhow::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let temp_file = parent.join(format!(".flashagent-update.{}.tmp", std::process::id()));

    // Write new binary
    std::fs::write(&temp_file, new_binary_bytes)?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_file, std::fs::Permissions::from_mode(0o755))?;
    }

    // On Windows, running binaries cannot be overwritten in place, but can be renamed
    #[cfg(windows)]
    {
        if target.exists() {
            let old_backup = parent.join(format!(".flashagent-old.{}.bak", std::process::id()));
            let _ = std::fs::remove_file(&old_backup);
            let _ = std::fs::rename(target, &old_backup);
        }
    }

    // Atomic rename replaces the target inode on Unix
    if let Err(e) = std::fs::rename(&temp_file, target) {
        // If cross-device link error, copy and remove
        let _ = std::fs::copy(&temp_file, target);
        let _ = std::fs::remove_file(&temp_file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755));
        }
        if !target.exists() {
            return Err(e.into());
        }
    }

    Ok(())
}

/// Download asset payload and apply in-place update.
pub async fn download_and_apply(download_url: &str, asset_name: &str) -> anyhow::Result<PathBuf> {
    if is_dev_mode() {
        anyhow::bail!("In-app updater is disabled in development mode (running from source repository or cargo target build).");
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent(format!("FlashAgent-Updater/{}", current_version()))
        .build()?;

    let resp = client.get(download_url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Failed downloading asset {}: HTTP {}", asset_name, resp.status());
    }

    let payload = resp.bytes().await?;
    let binary_bytes = extract_binary_bytes(asset_name, &payload)?;

    let target_path = resolve_install_target()?;
    atomic_replace_executable(&target_path, &binary_bytes)?;

    Ok(target_path)
}

/// Silent background worker: checks for updates, downloads, replaces executable,
/// and returns the installed version string (e.g. "b190" or "v0.1.0") upon success.
pub async fn check_and_apply_background(channel: UpdateChannel) -> anyhow::Result<Option<String>> {
    if is_dev_mode() {
        return Ok(None);
    }

    let status = check_for_updates(channel, DEFAULT_RELEASES_API).await?;
    match status {
        UpdateStatus::UpdateAvailable { target, asset_name, download_url, .. } => {
            download_and_apply(&download_url, &asset_name).await?;
            Ok(Some(target))
        }
        UpdateStatus::UpToDate { .. } => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_target_release_beta_and_stable() {
        let releases = vec![
            GitHubRelease {
                tag_name: "b190".into(),
                name: Some("FlashAgent Beta b190".into()),
                prerelease: true,
                published_at: None,
                body: None,
                assets: vec![ReleaseAsset {
                    name: "flashagent-linux-x86_64".into(),
                    browser_download_url: "https://example.com/b190".into(),
                    size: 1000,
                }],
            },
            GitHubRelease {
                tag_name: "v0.1.0".into(),
                name: Some("FlashAgent v0.1.0".into()),
                prerelease: false,
                published_at: None,
                body: None,
                assets: vec![ReleaseAsset {
                    name: "flashagent-linux-x86_64".into(),
                    browser_download_url: "https://example.com/v0.1.0".into(),
                    size: 1000,
                }],
            },
        ];

        let beta = find_target_release(&releases, UpdateChannel::Beta).unwrap();
        assert_eq!(beta.tag_name, "b190");

        let stable = find_target_release(&releases, UpdateChannel::Stable).unwrap();
        assert_eq!(stable.tag_name, "v0.1.0");
    }

    #[test]
    fn test_rolling_tags_and_version_extraction() {
        let releases = vec![
            GitHubRelease {
                tag_name: "beta".into(),
                name: Some("FlashAgent b200".into()),
                prerelease: true,
                published_at: None,
                body: None,
                assets: vec![],
            },
            GitHubRelease {
                tag_name: "release".into(),
                name: Some("FlashAgent v1.0.0".into()),
                prerelease: false,
                published_at: None,
                body: None,
                assets: vec![],
            },
        ];

        let beta = find_target_release(&releases, UpdateChannel::Beta).unwrap();
        assert_eq!(beta.tag_name, "beta");
        assert_eq!(extract_release_version(beta), "b200");

        let stable = find_target_release(&releases, UpdateChannel::Stable).unwrap();
        assert_eq!(stable.tag_name, "release");
        assert_eq!(extract_release_version(stable), "v1.0.0");
    }

    #[test]
    fn test_is_downgrade_logic() {
        // Beta to Stable is always considered a downgrade to official release
        assert!(is_downgrade("b190", "v0.1.0", UpdateChannel::Stable));
        // Lower beta number is downgrade
        assert!(is_downgrade("b191", "b190", UpdateChannel::Beta));
        // Higher beta number is NOT downgrade
        assert!(!is_downgrade("b190", "b191", UpdateChannel::Beta));
    }

    #[test]
    fn test_extract_binary_from_tar_gz() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::Builder;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut tar = Builder::new(&mut encoder);
            let dummy_content = b"fake-elf-binary-content";
            let mut header = tar::Header::new_gnu();
            header.set_path("flashagent-tui").unwrap();
            header.set_size(dummy_content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append(&header, &dummy_content[..]).unwrap();
            tar.finish().unwrap();
        }
        let tar_gz_bytes = encoder.finish().unwrap();

        let extracted = extract_binary_bytes("flashagent-v0.1.0-linux-x86_64.tar.gz", &tar_gz_bytes).unwrap();
        assert_eq!(extracted, b"fake-elf-binary-content");
    }

    #[test]
    fn test_atomic_replace_file() {
        let dir = std::env::temp_dir().join(format!("flashagent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("flashagent");

        std::fs::write(&target, b"version-old").unwrap();
        atomic_replace_executable(&target, b"version-new").unwrap();

        let read_back = std::fs::read(&target).unwrap();
        assert_eq!(read_back, b"version-new");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_dev_mode_detects_cargo_env() {
        // In `cargo test`, CARGO / CARGO_MANIFEST_DIR or debug profile is active
        assert!(is_dev_mode(), "is_dev_mode must be true during cargo test / dev checkout");
    }
}
