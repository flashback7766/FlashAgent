//! Self-updater: checks GitHub releases on the chosen channel, downloads the
//! platform binary, verifies its SHA-256 against the release's `SHA256SUMS`
//! manifest and replaces the executable in place. Releases carry no
//! signatures: the checksum is the only integrity check there is.

use std::path::{Path, PathBuf};
use serde::Deserialize;
pub use flashagent_core::config::UpdateChannel;

#[derive(Debug, Clone, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    prerelease: bool,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

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
        /// `SHA256SUMS` of the same release. `None` means the download cannot
        /// be verified, so applying it is refused.
        checksums_url: Option<String>,
    },
}

const CHECKSUMS_ASSET: &str = "SHA256SUMS";

pub const DEFAULT_RELEASES_API: &str = "https://api.github.com/repos/flashback7766/FlashAgent/releases";

pub const BACKGROUND_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(240);

/// E.g. "b200" or "v0.1.0".
pub fn current_version() -> &'static str {
    option_env!("FLASHAGENT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Recognised by `crates/tui/Cargo.toml` naming the crate, so another Rust
/// project the user keeps a binary in is not mistaken for this one.
fn in_source_tree(path: &Path) -> bool {
    let mut dir = path.to_path_buf();
    if dir.is_file() {
        dir.pop();
    }
    loop {
        let tui_manifest = dir.join("crates").join("tui").join("Cargo.toml");
        if std::fs::read_to_string(&tui_manifest).is_ok_and(|m| m.contains("name = \"flashagent-tui\"")) {
            return true;
        }
        if !dir.pop() {
            return false;
        }
    }
}

/// Running from a source checkout or a cargo build: auto-update is off so it
/// does not overwrite dev binaries.
pub fn is_dev_mode() -> bool {
    if std::env::var("FLASHAGENT_DEV").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")) {
        return true;
    }
    if std::env::var_os("CARGO").is_some() || std::env::var_os("CARGO_MANIFEST_DIR").is_some() {
        return true;
    }
    if let Ok(exe) = std::env::current_exe() {
        // Running out of a cargo target directory.
        let path = exe.to_string_lossy();
        if path.contains("/target/") || path.contains("\\target\\") {
            return true;
        }
        // Decided by where the executable is, not the cwd: an installed
        // `flashagent` must keep updating while the user works in the repository.
        if in_source_tree(&exe) {
            return true;
        }
    }
    cfg!(debug_assertions)
}

/// Stable: stable releases only (`vX.Y.Z`, or the rolling `stable`/`release`).
/// Beta: everything, so the newest wins even when it is a stable.
fn on_channel(rel: &GitHubRelease, channel: UpdateChannel) -> bool {
    match channel {
        UpdateChannel::Beta => true,
        UpdateChannel::Stable => {
            if matches!(rel.tag_name.as_str(), "stable" | "release") {
                return true;
            }
            if rel.prerelease || rel.tag_name == "beta" {
                return false;
            }
            crate::Version::parse(&extract_release_version(rel)).is_some_and(|v| !v.is_beta())
        }
    }
}

/// By version, not list position: GitHub lists by creation date, and a
/// rolling release edited in place keeps its old date.
fn find_target_release(releases: &[GitHubRelease], channel: UpdateChannel) -> Option<&GitHubRelease> {
    let candidates: Vec<&GitHubRelease> = releases.iter().filter(|r| on_channel(r, channel)).collect();
    candidates
        .iter()
        .copied()
        .filter_map(|r| crate::Version::parse(&extract_release_version(r)).map(|version| (version, r)))
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, r)| r)
        .or_else(|| candidates.first().copied())
}

/// Rolling releases are tagged by channel, so the version is looked for in the
/// title, tag, asset names and notes, in that order. None found gives the
/// running version, so it never reads as an update.
fn extract_release_version(rel: &GitHubRelease) -> String {
    use crate::Version;
    rel.name
        .as_deref()
        .and_then(Version::find_in)
        .or_else(|| Version::parse(&rel.tag_name))
        .or_else(|| rel.assets.iter().find_map(|a| Version::find_in(&a.name)))
        .or_else(|| rel.body.as_deref().and_then(Version::find_in))
        .map(|v| v.to_string())
        .unwrap_or_else(|| current_version().to_string())
}

/// For the current OS and architecture.
fn find_platform_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        if let Some(a) = assets.iter().find(|a| a.name.contains("flashagent") && a.name.contains("linux-x86_64") && !a.name.ends_with(".tar.gz") && !a.name.ends_with(".zst") && !a.name.ends_with(".deb")) {
            return Some(a);
        }
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

    // No fallback to "any archive": it would install a binary for another platform.
    let _ = assets;
    None
}

/// By [`crate::version`] ordering. Unparseable versions are never a downgrade.
fn is_downgrade(current: &str, target: &str) -> bool {
    match (crate::Version::parse(current), crate::Version::parse(target)) {
        (Some(c), Some(t)) => t < c,
        _ => false,
    }
}

/// `None` when the channel is empty. [`check_for_updates`] reports `UpToDate`
/// for both "newest" and "empty", which matters when offering a channel switch.
pub async fn newest_on_channel(
    channel: UpdateChannel,
    api_url: &str,
) -> anyhow::Result<Option<String>> {
    let releases = fetch_releases(api_url).await?;
    Ok(find_target_release(&releases, channel).map(extract_release_version))
}

async fn fetch_releases(api_url: &str) -> anyhow::Result<Vec<GitHubRelease>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(format!("FlashAgent-Updater/{}", current_version()))
        .build()?;
    let resp = client.get(api_url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub API returned HTTP {}", resp.status());
    }
    Ok(resp.json().await?)
}

pub async fn check_for_updates(channel: UpdateChannel, api_url: &str) -> anyhow::Result<UpdateStatus> {
    let releases = fetch_releases(api_url).await?;
    let cur = current_version();

    if let Some(target_rel) = find_target_release(&releases, channel) {
        let version = extract_release_version(target_rel);
        // Anything but the running version: newer, or older on a channel the user
        // moved to (flagged as a downgrade).
        let differs = match (crate::Version::parse(&version), crate::Version::parse(cur)) {
            (Some(target), Some(running)) => target != running,
            _ => version != cur,
        };
        if differs {
            if let Some(asset) = find_platform_asset(&target_rel.assets) {
                let checksums_url = target_rel
                    .assets
                    .iter()
                    .find(|a| a.name == CHECKSUMS_ASSET)
                    .map(|a| a.browser_download_url.clone());
                return Ok(UpdateStatus::UpdateAvailable {
                    target: version.clone(),
                    channel,
                    is_downgrade: is_downgrade(cur, &version),
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

/// Handles tar.gz or a bare binary.
fn extract_binary_bytes(asset_name: &str, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    if asset_name.ends_with(".tar.gz") {
        use flate2::read::GzDecoder;
        use tar::Archive;

        let gz = GzDecoder::new(payload);
        let mut archive = Archive::new(gz);

        for entry in archive.entries()? {
            let mut entry = entry?;
            // Regular files only: a symlink entry reads as zero bytes and would install
            // an empty binary.
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

    // An archive must never be written in place of the executable (a .zip saved
    // as flashagent.exe bricks the install).
    const PACKAGES: [&str; 5] = [".zip", ".zst", ".deb", ".AppImage", ".tar.xz"];
    if PACKAGES.iter().any(|ext| asset_name.ends_with(ext)) {
        anyhow::bail!("{asset_name} is a package, not a binary; update with your package manager or the installer");
    }

    Ok(payload.to_vec())
}

/// In `sha256sum` format. The manifest is written before upload and GitHub
/// renames characters such as the `+` of a stable build (`v1.0.0+b290`), so
/// names are compared with those characters made alike.
fn expected_checksum(manifest: &str, asset_name: &str) -> Option<String> {
    let alike = |name: &str| -> String {
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '.' }).collect()
    };
    let wanted = alike(asset_name);
    manifest.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == asset_name || alike(name) == wanted).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The asset's checksum from the release's manifest. A download from the
/// network is never installed unchecked, so a release that publishes no
/// `SHA256SUMS` is refused rather than accepted silently; a manifest that
/// lacks the asset is a hard failure too.
async fn published_checksum(client: &reqwest::Client, checksums_url: Option<&str>, asset_name: &str) -> anyhow::Result<String> {
    let Some(url) = checksums_url else {
        anyhow::bail!(
            "{asset_name} cannot be verified: this release has no {CHECKSUMS_ASSET} asset, and installing an unchecked download is refused"
        );
    };
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("could not fetch {CHECKSUMS_ASSET}: HTTP {}", resp.status());
    }
    let manifest = resp.text().await?;
    expected_checksum(&manifest, asset_name)
        .ok_or_else(|| anyhow::anyhow!("{asset_name} is not listed in {CHECKSUMS_ASSET}"))
}

fn check_checksum(asset_name: &str, payload: &[u8], expected: &str) -> anyhow::Result<()> {
    let actual = sha256_hex(payload);
    if actual != expected {
        anyhow::bail!("checksum mismatch for {asset_name}: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Hashing, unpacking and writing a release take a few hundred milliseconds
/// of CPU and disk: done on the async workers, they held up whatever else
/// ran there, such as the stream of an answer.
async fn off_runtime<T: Send + 'static>(work: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(work).await?
}

/// Checks the folder, since replacing is a rename into it. A running
/// executable cannot be opened for writing on Linux ("text file busy"), which
/// used to send every update to `~/.local/bin`.
fn is_writable(path: &Path) -> bool {
    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let probe = parent.join(format!(".flashagent-write-test.{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Where the install lives, when the running executable is its own parked
/// copy. A process that has updated once runs from the backup name —
/// `/proc/self/exe` follows the rename that parked it — and installing to
/// that name writes over the copy the rollback exists for, or fails outright
/// once the cleanup has taken it. The binary the owner starts is the one the
/// last update placed beside it.
fn installed_beside_parked(exe: &Path) -> Option<PathBuf> {
    let name = exe.file_name()?.to_string_lossy();
    if !(name.starts_with(".flashagent-old.") && name.ends_with(".bak")) {
        return None;
    }
    let sibling = if cfg!(windows) { "flashagent.exe" } else { "flashagent" };
    exe.parent().map(|dir| dir.join(sibling))
}

/// The running binary when it is replaceable; otherwise the per-user
/// install location: `~/.local/bin/flashagent`, or on Windows where
/// install.ps1 puts it. Never a bare name, which would land in whatever
/// folder FlashAgent was started from.
fn resolve_install_target() -> anyhow::Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let real_path = std::fs::canonicalize(&exe).unwrap_or(exe);
        let real_path = installed_beside_parked(&real_path).unwrap_or(real_path);
        if is_writable(&real_path) {
            return Ok(real_path);
        }
    }

    #[cfg(windows)]
    let fallback = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("FlashAgent").join("flashagent.exe"));
    #[cfg(not(windows))]
    let fallback = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("bin").join("flashagent"));
    let Some(target) = fallback else {
        anyhow::bail!("the folder of the running FlashAgent cannot be written to, and there is no per-user folder to install into");
    };
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(target)
}

/// A replaced binary is parked beside the new one while it still runs (on
/// Windows) and kept as a rollback copy (on unix); nothing removed it, so
/// every update left a full copy behind and uninstall found the folder not
/// empty. Removed at the next start.
pub fn remove_stale_backups(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".flashagent-old.") && name.ends_with(".bak") {
            // The one this process just parked is still running; it fails and stays.
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub fn remove_stale_backups_beside_exe() {
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| std::fs::canonicalize(e).ok()).and_then(|e| e.parent().map(Path::to_path_buf)) {
        remove_stale_backups(&dir);
    }
}

fn atomic_replace_executable(target: &Path, new_binary_bytes: &[u8]) -> anyhow::Result<()> {
    replace_executable(target, new_binary_bytes, place_binary)
}

/// Writes the download next to its destination and only then puts it in place.
/// The current binary is parked first on every platform, so a failed install
/// can put it back; a temp file never survives the attempt.
fn replace_executable(
    target: &Path,
    new_binary_bytes: &[u8],
    place: impl FnOnce(&Path, &Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let temp_file = parent.join(format!(".flashagent-update.{}.tmp", std::process::id()));
    let old_backup = parent.join(format!(".flashagent-old.{}.bak", std::process::id()));

    let result = install_executable(target, &temp_file, &old_backup, new_binary_bytes, place);
    if result.is_err() {
        // A half-written update must not stay behind for the next start to
        // mistake for something worth keeping.
        let _ = std::fs::remove_file(&temp_file);
    }
    result
}

/// Moves the running binary to the backup name. Windows cannot rename over
/// a name another copy holds and needs the removal first; unix replaces
/// atomically, and a `NotFound` means the target was parked away by another
/// update between the check and this rename — the place step still puts our
/// build where the owner runs it from.
#[cfg(windows)]
fn park_running_binary(target: &Path, old_backup: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(old_backup);
    std::fs::rename(target, old_backup)
        .map_err(|e| anyhow::anyhow!("could not move the current binary {} aside: {e}", target.display()))
}

#[cfg(not(windows))]
fn park_running_binary(target: &Path, old_backup: &Path) -> anyhow::Result<()> {
    match std::fs::rename(target, old_backup) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::anyhow!("could not move the current binary {} aside: {e}", target.display())),
    }
}

fn install_executable(
    target: &Path,
    temp_file: &Path,
    old_backup: &Path,
    new_binary_bytes: &[u8],
    place: impl FnOnce(&Path, &Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    write_synced(temp_file, new_binary_bytes)?;

    // A running .exe cannot be overwritten, but can be renamed; on unix the
    // same move keeps the current build available for a rollback.
    let parked = target.is_file();
    if parked {
        park_running_binary(target, old_backup)?;
    }

    match place(temp_file, target) {
        Ok(()) => Ok(()),
        Err(e) => {
            if parked {
                // A failed copy can leave part of the new build at the path.
                let _ = std::fs::remove_file(target);
                if let Err(restore) = std::fs::rename(old_backup, target) {
                    anyhow::bail!(
                        "{e}; the old binary at {} could not be put back: {restore}",
                        target.display()
                    );
                }
            } else {
                let _ = std::fs::remove_file(target);
            }
            Err(e)
        }
    }
}

fn place_binary(temp_file: &Path, target: &Path) -> anyhow::Result<()> {
    if let Err(rename_err) = std::fs::rename(temp_file, target) {
        // A rename across devices fails where a copy does not.
        std::fs::copy(temp_file, target).map_err(|copy_err| {
            anyhow::anyhow!("could not replace {}: {rename_err}; copying failed too: {copy_err}", target.display())
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))?;
        }
        let _ = std::fs::remove_file(temp_file);
    }
    Ok(())
}

/// `sync_all` before the rename: otherwise a power cut can install a file
/// whose contents never left the page cache, and there would be no old binary
/// left to fall back to.
fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    // The mode is set before the sync, so a power cut cannot leave the new
    // build behind without the right to run it.
    file.sync_all()
}

/// Shown for a background update only when the user asks to watch it
/// (Ctrl+U): an update nobody asked about must not take over the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateProgress {
    /// `total` when the server declared one.
    Downloading { received: u64, total: Option<u64> },
    Verifying,
    Installing,
}

/// A 7 MB download arrives in hundreds of chunks; a repaint per chunk costs
/// more than the download.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(120);

/// Downloads, verifies against the release's mandatory SHA-256 manifest, and
/// installs.
pub async fn download_and_apply(download_url: &str, asset_name: &str, checksums_url: Option<&str>) -> anyhow::Result<PathBuf> {
    download_and_apply_with_progress(download_url, asset_name, checksums_url, |_| {}).await
}

/// The build this process has already put on the disk, if it did. A process
/// that has updated once runs from its parked copy — `/proc/self/exe`
/// follows the rename — and a second install from it aims at the parked
/// name, where the last thing it should do is write. One install per
/// process; the restart applies what landed.
static INSTALLED_THIS_RUN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub async fn download_and_apply_with_progress(
    download_url: &str,
    asset_name: &str,
    checksums_url: Option<&str>,
    mut on_progress: impl FnMut(UpdateProgress),
) -> anyhow::Result<PathBuf> {
    if is_dev_mode() {
        anyhow::bail!("In-app updater is disabled in development mode (running from source repository or cargo target build).");
    }
    if let Some(version) = INSTALLED_THIS_RUN.lock().ok().and_then(|g| g.clone()) {
        anyhow::bail!("an update to {version} is already on the disk; restart FlashAgent to run it");
    }

    // Idle timeout, not total: a slow download may take minutes but must not stall.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(60))
        .user_agent(format!("FlashAgent-Updater/{}", current_version()))
        .build()?;

    let payload = download(&client, download_url, asset_name, &mut on_progress).await?;

    on_progress(UpdateProgress::Verifying);
    let expected = published_checksum(&client, checksums_url, asset_name).await?;
    let asset = asset_name.to_string();
    let payload = off_runtime(move || check_checksum(&asset, &payload, &expected).map(|()| payload)).await?;

    on_progress(UpdateProgress::Installing);
    let asset = asset_name.to_string();
    let target_path = off_runtime(move || {
        let binary_bytes = extract_binary_bytes(&asset, &payload)?;
        let target_path = resolve_install_target()?;
        atomic_replace_executable(&target_path, &binary_bytes)?;
        Ok(target_path)
    })
    .await?;
    if let Ok(mut installed) = INSTALLED_THIS_RUN.lock() {
        *installed = Some(current_version().to_string());
    }
    Ok(target_path)
}

/// The size a server declares reserves memory only up to this: a wrong
/// Content-Length must not ask for more than the machine has.
const MAX_RESERVED_DOWNLOAD: u64 = 64 * 1024 * 1024;

/// Far above any release (they are about 10 MB): a server that streams without
/// end, or claims to, is refused rather than read into memory.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// Streamed, so the caller can show progress.
async fn download(
    client: &reqwest::Client,
    url: &str,
    asset_name: &str,
    on_progress: &mut impl FnMut(UpdateProgress),
) -> anyhow::Result<Vec<u8>> {
    download_capped(client, url, asset_name, MAX_DOWNLOAD, on_progress).await
}

async fn download_capped(
    client: &reqwest::Client,
    url: &str,
    asset_name: &str,
    limit: u64,
    on_progress: &mut impl FnMut(UpdateProgress),
) -> anyhow::Result<Vec<u8>> {
    let too_big = || anyhow::anyhow!("Refused {asset_name}: larger than {} MB, which no release is", limit / (1024 * 1024));
    let mut resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Failed downloading asset {}: HTTP {}", asset_name, resp.status());
    }
    let total = resp.content_length();
    if total.is_some_and(|t| t > limit) {
        return Err(too_big());
    }
    let mut payload: Vec<u8> = Vec::with_capacity(total.unwrap_or(0).min(MAX_RESERVED_DOWNLOAD) as usize);
    let mut last_report = std::time::Instant::now();
    on_progress(UpdateProgress::Downloading { received: 0, total });
    while let Some(chunk) = resp.chunk().await? {
        if (payload.len() + chunk.len()) as u64 > limit {
            return Err(too_big());
        }
        payload.extend_from_slice(&chunk);
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            last_report = std::time::Instant::now();
            on_progress(UpdateProgress::Downloading { received: payload.len() as u64, total });
        }
    }
    on_progress(UpdateProgress::Downloading { received: payload.len() as u64, total });
    Ok(payload)
}

/// Returns the installed version on success, and reports it with each stage,
/// so a download already under way can be watched.
pub async fn check_and_apply_background_with_progress(
    channel: UpdateChannel,
    mut on_progress: impl FnMut(&str, UpdateProgress),
) -> anyhow::Result<Option<String>> {
    if is_dev_mode() {
        return Ok(None);
    }

    let status = check_for_updates(channel, DEFAULT_RELEASES_API).await?;
    match status {
        // Never silently roll back a newer beta (a local b233 while the feed says
        // b218). Explicit switches to Stable still apply.
        UpdateStatus::UpdateAvailable { is_downgrade: true, channel: UpdateChannel::Beta, .. } => Ok(None),
        UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. } => {
            download_and_apply_with_progress(&download_url, &asset_name, checksums_url.as_deref(), |stage| {
                on_progress(&target, stage)
            })
            .await?;
            Ok(Some(target))
        }
        UpdateStatus::UpToDate { .. } => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_checkout(root: &Path) {
        std::fs::create_dir_all(root.join("crates").join("tui")).unwrap();
        std::fs::write(
            root.join("crates").join("tui").join("Cargo.toml"),
            "[package]\nname = \"flashagent-tui\"\n",
        )
        .unwrap();
    }

    #[test]
    fn dev_mode_follows_the_binary_not_the_working_directory() {
        let tmp = std::env::temp_dir().join(format!("fa-devmode-{}", std::process::id()));
        let repo = tmp.join("FlashAgent");
        let elsewhere = tmp.join("opt").join("bin");
        std::fs::create_dir_all(&elsewhere).unwrap();
        fake_checkout(&repo);

        assert!(in_source_tree(&repo.join("target").join("release").join("flashagent")));
        assert!(in_source_tree(&repo.join("crates").join("tui")));
        // An installed one is not, even when the user stands in the repository.
        assert!(!in_source_tree(&elsewhere.join("flashagent")));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_parked_process_installs_beside_its_parked_copy() {
        // A process that has already updated once runs from its parked name:
        // /proc/self/exe follows the rename. Installing to that name would
        // write over the rollback — or fail, once the cleanup takes it. The
        // install lives where the last update placed it: beside the copy.
        let parked = std::path::Path::new("/opt/bin/.flashagent-old.123.bak");
        assert_eq!(installed_beside_parked(parked).as_deref(), Some(std::path::Path::new("/opt/bin/flashagent")));
        // A binary under its own name is no parked copy.
        assert_eq!(installed_beside_parked(std::path::Path::new("/opt/bin/flashagent")), None);
        // Nor is another file that merely starts the same way.
        assert_eq!(installed_beside_parked(std::path::Path::new("/opt/bin/.flashagent-old-README")), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn nothing_to_park_is_not_an_error() {
        // Two updates at once: the other one parked the binary away between
        // our check and our rename. The place step still puts our build
        // where the owner runs it from; the failed move is not a failure.
        let dir = std::env::temp_dir().join(format!("fa-park-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("flashagent");
        let backup = dir.join(".flashagent-old.1.bak");
        park_running_binary(&target, &backup).expect("a target that is already gone parks nothing");
        assert!(!backup.exists(), "nothing was written for a target that is not there");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn the_park_replaces_what_stands_at_the_backup_name() {
        // The second update of a running process reuses the same {pid} backup
        // name; the park must replace the name, not die on it — the running
        // binary it parked before is this process's own inode.
        let dir = std::env::temp_dir().join(format!("fa-park2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("flashagent");
        let backup = dir.join(".flashagent-old.1.bak");
        std::fs::write(&target, b"the running build").unwrap();
        std::fs::write(&backup, b"a stale backup").unwrap();
        park_running_binary(&target, &backup).expect("the park replaces the backup name");
        assert!(!target.exists(), "the running binary moved aside");
        assert_eq!(std::fs::read(&backup).unwrap(), b"the running build", "the backup holds what was parked");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_running_binary_in_a_folder_we_can_write_to_is_replaced_in_place() {
        // A running binary in a folder the build owns: the rename swaps the file
        // while the process keeps the old one.
        let exe = std::env::current_exe().unwrap();
        assert!(is_writable(&exe), "{} is in a writable folder but was judged read-only", exe.display());
    }

    // Unix only: on Windows the updater moves whatever is at the path aside
    // first, so the replacement genuinely succeeds.
    #[cfg(unix)]
    #[test]
    fn a_replacement_that_did_not_happen_is_not_reported_as_done() {
        // A directory at the path: rename and copy both fail, something still exists.
        let tmp = std::env::temp_dir().join(format!("fa-replace-fails-{}", std::process::id()));
        let target = tmp.join("flashagent");
        std::fs::create_dir_all(&target).unwrap();
        let result = atomic_replace_executable(&target, b"new build");
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(result.is_err(), "nothing was replaced, yet the update reported success");
    }

    #[test]
    fn an_unrelated_rust_project_is_not_this_source_tree() {
        let tmp = std::env::temp_dir().join(format!("fa-otherproj-{}", std::process::id()));
        let other = tmp.join("someones-project");
        std::fs::create_dir_all(other.join("crates").join("tui")).unwrap();
        std::fs::write(
            other.join("crates").join("tui").join("Cargo.toml"),
            "[package]\nname = \"their-tui\"\n",
        )
        .unwrap();
        assert!(!in_source_tree(&other.join("flashagent")));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_find_target_release_beta_and_stable() {
        let releases = vec![
            GitHubRelease {
                tag_name: "b190".into(),
                name: Some("FlashAgent Beta b190".into()),
                prerelease: true,
                body: None,
                assets: vec![ReleaseAsset {
                    name: "flashagent-linux-x86_64".into(),
                    browser_download_url: "https://example.com/b190".into(),
                }],
            },
            GitHubRelease {
                tag_name: "v0.1.0".into(),
                name: Some("FlashAgent v0.1.0+b180".into()),
                prerelease: false,
                body: None,
                assets: vec![ReleaseAsset {
                    name: "flashagent-linux-x86_64".into(),
                    browser_download_url: "https://example.com/v0.1.0".into(),
                }],
            },
        ];

        // The stable was cut from b180: on beta, the newer b190 wins.
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
                body: None,
                assets: vec![],
            },
            GitHubRelease {
                tag_name: "release".into(),
                name: Some("FlashAgent v1.0.0".into()),
                prerelease: false,
                body: None,
                assets: vec![],
            },
        ];

        let beta = find_target_release(&releases, UpdateChannel::Beta).unwrap();
        assert_eq!(beta.tag_name, "release");
        let rolling_beta = releases.iter().find(|r| r.tag_name == "beta").unwrap();
        assert_eq!(extract_release_version(rolling_beta), "b200");

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
            body: None,
            assets: vec![],
        };
        // GitHub lists newest-created first; the rolling `beta` release was created
        // long ago and later edited to b233.
        let releases = vec![rel("b218", "FlashAgent b218", true), rel("b215", "FlashAgent b215", true), rel("beta", "FlashAgent b233", true)];
        assert_eq!(find_target_release(&releases, UpdateChannel::Beta).unwrap().tag_name, "beta");
        let stable = vec![rel("v1.0.0", "FlashAgent v1.0.0", false), rel("stable", "FlashAgent v1.2.0", false), rel("b300", "FlashAgent b300", true)];
        assert_eq!(find_target_release(&stable, UpdateChannel::Stable).unwrap().tag_name, "stable");
    }

    #[test]
    fn stable_updates_follow_build_numbers_even_when_semver_is_lower() {
        let releases: Vec<GitHubRelease> = ["v2.0.0+b290", "v1.0.0+b291"]
            .into_iter()
            .map(|tag| GitHubRelease {
                tag_name: tag.into(), name: None, prerelease: false,
                body: None, assets: vec![],
            })
            .collect();
        for channel in [UpdateChannel::Stable, UpdateChannel::Beta] {
            assert_eq!(find_target_release(&releases, channel).unwrap().tag_name, "v1.0.0+b291");
        }
        assert!(!is_downgrade("v2.0.0+b290", "v1.0.0+b291"));
        assert!(is_downgrade("v1.0.0+b291", "v2.0.0+b290"));
    }

    #[test]
    fn a_downgrade_is_decided_by_the_version_ordering() {
        assert!(is_downgrade("b191", "b190"));
        assert!(!is_downgrade("b190", "b191"));
        assert!(!is_downgrade("b287", "v1.0.0"));
        // A stable cut from an older build than the running beta is a downgrade.
        assert!(is_downgrade("b300", "v1.0.0+b290"));
        assert!(!is_downgrade("dev", "b1"), "an unknown version is never called a downgrade");
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
        let stable = "beef  flashagent-v1.0.0+b400-linux-x86_64.tar.gz\n";
        assert_eq!(expected_checksum(stable, "flashagent-v1.0.0.b400-linux-x86_64.tar.gz").as_deref(), Some("beef"));
        assert_eq!(expected_checksum(stable, "flashagent-v1.0.0+b400-linux-x86_64.tar.gz").as_deref(), Some("beef"));
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(check_checksum("a", b"abc", abc).is_ok());
        assert!(check_checksum("a", b"abd", abc).is_err(), "a changed download passed");
    }

    #[tokio::test]
    async fn a_release_without_a_checksum_manifest_is_refused() {
        let client = reqwest::Client::new();
        let err = published_checksum(&client, None, "flashagent")
            .await
            .expect_err("a download with no checksum must not be installed")
            .to_string();
        assert!(err.contains(CHECKSUMS_ASSET), "{err}");
        assert!(err.contains("refused"), "{err}");
    }

    #[tokio::test]
    async fn a_manifest_that_does_not_list_the_asset_is_refused() {
        let url = serve_once("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n", b"deadbeef  another-file\n").await;
        let err = published_checksum(&reqwest::Client::new(), Some(&url), "flashagent")
            .await
            .expect_err("an unlisted asset has no checksum")
            .to_string();
        assert!(err.contains("not listed"), "{err}");
    }

    #[test]
    fn a_failed_replacement_puts_the_old_binary_back_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("flashagent");
        std::fs::write(&target, b"old build").unwrap();

        let result = replace_executable(&target, b"new build", |_, _| anyhow::bail!("injected install failure"));

        assert!(result.is_err(), "a failed install must not be reported as done");
        assert_eq!(std::fs::read(&target).unwrap(), b"old build", "the rollback must restore the previous binary");
        let temp = dir.path().join(format!(".flashagent-update.{}.tmp", std::process::id()));
        assert!(!temp.exists(), "the temp file must not survive a failed install");
    }

    #[test]
    fn an_untagged_build_does_not_pin_a_past_release() {
        // A hardcoded fallback made every build without git report that release
        // (b286 once); the "what's new" screen then named a build never run.
        for line in include_str!("../build.rs").lines() {
            if let Some((_, value)) = line.split_once("FLASHAGENT_VERSION=") {
                assert!(value.starts_with('{'), "build.rs pins a version instead of leaving it to CARGO_PKG_VERSION: {line}");
            }
        }
    }

    #[test]
    fn a_parked_old_binary_is_removed_later() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".flashagent-old.4242.bak"), "old exe").unwrap();
        std::fs::write(dir.path().join("flashagent.exe"), "new exe").unwrap();
        remove_stale_backups(dir.path());
        assert!(!dir.path().join(".flashagent-old.4242.bak").exists());
        assert!(dir.path().join("flashagent.exe").exists());
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

        let old_backup = dir.join(format!(".flashagent-old.{}.bak", std::process::id()));
        assert_eq!(std::fs::read(&old_backup).unwrap(), b"version-old", "the previous build is kept for a rollback");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Answers one request with `head`, then `body`, then hangs up.
    async fn serve_once(head: &'static str, body: &'static [u8]) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/asset", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = sock.read(&mut request).await;
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(body).await;
        });
        url
    }

    #[tokio::test]
    async fn a_download_declaring_a_terabyte_reserves_no_terabyte() {
        // Reserving what the header claimed took down the whole app.
        let url = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 1099511627776\r\n\r\n", b"abc").await;
        let mut reported = false;
        let result = download(&reqwest::Client::new(), &url, "flashagent", &mut |_| reported = true).await;
        let err = result.expect_err("a terabyte is no release").to_string();
        assert!(err.contains("larger than 512 MB"), "{err}");
        assert!(!reported, "refused before reading anything");
    }

    #[tokio::test]
    async fn a_download_that_does_not_stop_is_cut_off_at_the_limit() {
        // No length given: only counting what arrives can stop it.
        let url = serve_once("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n", &[b'x'; 4096]).await;
        let result = download_capped(&reqwest::Client::new(), &url, "flashagent", 1024, &mut |_| {}).await;
        assert!(result.expect_err("more than the limit arrived").to_string().contains("flashagent"));
        let url = serve_once("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n", &[b'x'; 1000]).await;
        assert_eq!(download_capped(&reqwest::Client::new(), &url, "flashagent", 1024, &mut |_| {}).await.unwrap().len(), 1000);
    }

    #[tokio::test]
    async fn a_download_reports_what_arrived() {
        let url = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n", b"hello").await;
        let mut last = None;
        let payload = download(&reqwest::Client::new(), &url, "flashagent", &mut |p| last = Some(p)).await.unwrap();
        assert_eq!(payload, b"hello");
        assert_eq!(last, Some(UpdateProgress::Downloading { received: 5, total: Some(5) }));
    }

    #[test]
    fn test_is_dev_mode_detects_cargo_env() {
        assert!(is_dev_mode(), "is_dev_mode must be true during cargo test / dev checkout");
    }
}
