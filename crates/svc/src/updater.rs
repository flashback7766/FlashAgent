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
        /// `SHA256SUMS` asset of the same release, when published.
        checksums_url: Option<String>,
    },
}

/// Name of the checksum manifest attached to every release.
pub const CHECKSUMS_ASSET: &str = "SHA256SUMS";

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

    // 4. Working inside the FlashAgent source tree itself. Only this repo:
    // any git-tracked Rust project also has a Cargo.toml, and an installed
    // binary must keep updating when the user works in one.
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let tui_manifest = dir.join("crates").join("tui").join("Cargo.toml");
            if std::fs::read_to_string(&tui_manifest).is_ok_and(|m| m.contains("name = \"flashagent-tui\"")) {
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

fn on_channel(rel: &GitHubRelease, channel: UpdateChannel) -> bool {
    match channel {
        // Stable: the rolling `stable`/`release` tag, or any non-prerelease
        // that is not a beta build.
        UpdateChannel::Stable => {
            rel.tag_name == "stable"
                || rel.tag_name == "release"
                || (!rel.prerelease && rel.tag_name != "beta" && (rel.tag_name.starts_with('v') || !rel.tag_name.starts_with('b')))
        }
        // Beta: the rolling `beta` tag, a pre-release, or a `b<N>` tag.
        UpdateChannel::Beta => rel.tag_name == "beta" || rel.prerelease || rel.tag_name.starts_with('b'),
    }
}

/// Sortable rank of a version string: `b233` → (233,0,0), `v1.2.3` → (1,2,3).
fn version_rank(version: &str) -> Option<(u64, u64, u64)> {
    if let Some(n) = version.strip_prefix('b') {
        return n.parse().ok().map(|b| (b, 0, 0));
    }
    let mut parts = version.strip_prefix('v')?.split(['.', '-']).map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

/// The newest release on `channel`. Chosen by version, not by list position:
/// GitHub lists by creation date, and a rolling `beta`/`stable` release that
/// is edited in place keeps its old creation date.
pub fn find_target_release(releases: &[GitHubRelease], channel: UpdateChannel) -> Option<&GitHubRelease> {
    let candidates: Vec<&GitHubRelease> = releases.iter().filter(|r| on_channel(r, channel)).collect();
    candidates
        .iter()
        .copied()
        .filter_map(|r| version_rank(&extract_release_version(r)).map(|rank| (rank, r)))
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, r)| r)
        .or_else(|| candidates.first().copied())
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
                let checksums_url = target_rel
                    .assets
                    .iter()
                    .find(|a| a.name == CHECKSUMS_ASSET)
                    .map(|a| a.browser_download_url.clone());
                return Ok(UpdateStatus::UpdateAvailable {
                    target: version.clone(),
                    channel,
                    is_downgrade: is_downgrade(cur, &version, channel),
                    asset_name: asset.name.clone(),
                    download_url: asset.browser_download_url.clone(),
                    checksums_url,
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
            // Only a regular file with the exact binary name: a symlink entry
            // reads as zero bytes and would install an empty "binary".
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path()?;
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if matches!(name.as_str(), "flashagent-tui" | "flashagent") {
                let mut binary_bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut binary_bytes)?;
                if binary_bytes.is_empty() {
                    anyhow::bail!("binary {name} in {asset_name} is empty");
                }
                return Ok(binary_bytes);
            }
        }
        anyhow::bail!("No executable binary found in archive {asset_name}");
    }

    // Anything else that is an archive or a package must never be written in
    // place of the executable (a .zip saved as flashagent.exe bricks the install).
    const PACKAGES: [&str; 5] = [".zip", ".zst", ".deb", ".AppImage", ".tar.xz"];
    if PACKAGES.iter().any(|ext| asset_name.ends_with(ext)) {
        anyhow::bail!("{asset_name} is a package, not a binary; update with your package manager or the installer");
    }

    // Direct binary payload
    Ok(payload.to_vec())
}

/// Hex SHA-256 recorded for `asset_name` in a `sha256sum`-format manifest.
pub fn expected_checksum(manifest: &str, asset_name: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == asset_name).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Verify `payload` against the release's checksum manifest. Releases that
/// predate the manifest have none (`checksums_url` is `None`) and pass; a
/// manifest that exists but lacks or contradicts the asset is a hard failure.
pub async fn verify_checksum(
    client: &reqwest::Client,
    checksums_url: Option<&str>,
    asset_name: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let Some(url) = checksums_url else {
        return Ok(());
    };
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("could not fetch {CHECKSUMS_ASSET}: HTTP {}", resp.status());
    }
    let manifest = resp.text().await?;
    let expected = expected_checksum(&manifest, asset_name)
        .ok_or_else(|| anyhow::anyhow!("{asset_name} is not listed in {CHECKSUMS_ASSET}"))?;
    let actual = sha256_hex(payload);
    if actual != expected {
        anyhow::bail!("checksum mismatch for {asset_name}: expected {expected}, got {actual}");
    }
    Ok(())
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

/// Download asset payload, verify it against the release checksums, and
/// apply the in-place update.
pub async fn download_and_apply(download_url: &str, asset_name: &str, checksums_url: Option<&str>) -> anyhow::Result<PathBuf> {
    if is_dev_mode() {
        anyhow::bail!("In-app updater is disabled in development mode (running from source repository or cargo target build).");
    }

    // Idle timeout rather than a total one: a 15 MB download on a slow link
    // may take minutes but should never stall silently.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(60))
        .user_agent(format!("FlashAgent-Updater/{}", current_version()))
        .build()?;

    let resp = client.get(download_url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Failed downloading asset {}: HTTP {}", asset_name, resp.status());
    }

    let payload = resp.bytes().await?;
    verify_checksum(&client, checksums_url, asset_name, &payload).await?;
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
        // Never silently roll a newer beta back to an older published one
        // (e.g. a locally built b233 while the release feed still says b218).
        // Channel switches to Stable are explicit and still apply.
        UpdateStatus::UpdateAvailable { is_downgrade: true, channel: UpdateChannel::Beta, .. } => Ok(None),
        UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. } => {
            download_and_apply(&download_url, &asset_name, checksums_url.as_deref()).await?;
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
    fn newest_version_wins_over_list_order() {
        let rel = |tag: &str, name: &str, pre: bool| GitHubRelease {
            tag_name: tag.into(),
            name: Some(name.into()),
            prerelease: pre,
            published_at: None,
            body: None,
            assets: vec![],
        };
        // GitHub order: newest *created* first. The rolling `beta` release was
        // created long ago and later edited to b233.
        let releases = vec![rel("b218", "FlashAgent b218", true), rel("b215", "FlashAgent b215", true), rel("beta", "FlashAgent b233", true)];
        assert_eq!(find_target_release(&releases, UpdateChannel::Beta).unwrap().tag_name, "beta");
        let stable = vec![rel("v1.0.0", "FlashAgent v1.0.0", false), rel("stable", "FlashAgent v1.2.0", false), rel("b300", "FlashAgent b300", true)];
        assert_eq!(find_target_release(&stable, UpdateChannel::Stable).unwrap().tag_name, "stable");
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
    fn packages_are_never_installed_as_the_binary() {
        for name in ["flashagent-windows-b218-x86_64.zip", "flashagent_b218-1_amd64.deb", "FlashAgent-b218-x86_64.AppImage"] {
            assert!(extract_binary_bytes(name, b"PK\x03\x04 not an exe").is_err(), "{name}");
        }
        assert_eq!(extract_binary_bytes("flashagent-linux-x86_64", b"\x7fELF").unwrap(), b"\x7fELF");
    }

    #[test]
    fn checksum_manifest_lookup_and_digest() {
        let manifest = "0f1e  flashagent-b233-linux-x86_64.tar.gz\nABCD *flashagent-windows-x86_64.exe\n";
        assert_eq!(expected_checksum(manifest, "flashagent-b233-linux-x86_64.tar.gz").as_deref(), Some("0f1e"));
        assert_eq!(expected_checksum(manifest, "flashagent-windows-x86_64.exe").as_deref(), Some("abcd"));
        assert_eq!(expected_checksum(manifest, "missing"), None);
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
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
