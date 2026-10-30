//! Tool-call paths: `~` is expanded here because a tool call has no shell.
//! Both the tools and the permission layer resolve paths through this module,
//! so a path cannot be checked in one place and opened in another. `~user` is
//! left as a literal name.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn is_separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// `~user/...` and a bare `~` without a home are returned unchanged.
pub fn expand_home(raw: &str) -> Cow<'_, str> {
    match home() {
        Some(home) => expand_home_in(raw, &home),
        None => Cow::Borrowed(raw),
    }
}

fn expand_home_in<'a>(raw: &'a str, home: &Path) -> Cow<'a, str> {
    let Some(rest) = raw.strip_prefix('~') else { return Cow::Borrowed(raw) };
    let home = home.to_string_lossy();
    let home = home.trim_end_matches(is_separator);
    match rest.chars().next() {
        None => Cow::Owned(home.to_string()),
        Some(c) if is_separator(c) => Cow::Owned(format!("{home}{rest}")),
        Some(_) => Cow::Borrowed(raw),
    }
}

pub fn resolve_path(root: &Path, raw: &str) -> PathBuf {
    let raw = expand_home(raw);
    let path = Path::new(raw.as_ref());
    if path.is_absolute() { path.to_path_buf() } else { root.join(path) }
}

/// Writes a file with its mode set at creation, never widened afterwards: the
/// config and sessions hold API keys, and a file that is briefly world-readable
/// has already leaked to every other account on the machine.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    let mut options = {
        use std::os::unix::fs::OpenOptionsExt;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true).mode(0o600);
        o
    };
    #[cfg(not(unix))]
    let mut options = std::fs::OpenOptions::new();
    let mut f = options.write(true).create(true).truncate(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Writes through a temporary beside the final path and renames it into
/// place, so a crash or a full disk leaves the previous file instead of half
/// of the new one. An existing file keeps its mode.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let wrote = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|()| {
            // A rename makes a new file; without this an executable bit or a
            // deliberately private note would be lost on every save.
            if let Ok(meta) = std::fs::metadata(path) {
                let _ = std::fs::set_permissions(&tmp, meta.permissions());
            }
            std::fs::rename(&tmp, path)
        });
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(parent);
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_tilde_becomes_the_home_folder() {
        let home = Path::new("/home/ada");
        assert_eq!(expand_home_in("~", home), "/home/ada");
        assert_eq!(expand_home_in("~/notes.txt", home), "/home/ada/notes.txt");
        assert_eq!(expand_home_in("~/FlashAgent/src/main.rs", home), "/home/ada/FlashAgent/src/main.rs");
    }

    #[test]
    fn a_trailing_separator_on_the_home_folder_is_not_doubled() {
        assert_eq!(expand_home_in("~/notes.txt", Path::new("/home/ada/")), "/home/ada/notes.txt");
    }

    #[test]
    fn everything_else_is_left_exactly_as_written() {
        let home = Path::new("/home/ada");
        for raw in ["notes.txt", "/etc/hosts", "./~", "~other/notes.txt", "~snapshot.txt", ""] {
            assert_eq!(expand_home_in(raw, home), raw, "{raw} was rewritten");
        }
    }

    #[test]
    fn a_resolved_path_is_relative_to_the_project_unless_it_says_otherwise() {
        let root = Path::new("/work/project");
        assert_eq!(resolve_path(root, "src/main.rs"), Path::new("/work/project/src/main.rs"));
        // Windows only calls a path absolute once it names a drive.
        let absolute = if cfg!(windows) { r"C:\Windows\hosts" } else { "/etc/hosts" };
        assert_eq!(resolve_path(root, absolute), Path::new(absolute));
        if let Some(home) = home() {
            assert_eq!(resolve_path(root, "~/notes.txt"), home.join("notes.txt"));
        }
    }

    #[test]
    fn an_atomic_write_keeps_the_previous_file_when_it_cannot_finish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        std::fs::write(&path, "the text that was already there").unwrap();
        // A directory where the temporary file would go makes the write fail,
        // as a full disk or a read-only folder would.
        let blocker = dir.path().join(format!(".MEMORY.md.{}.tmp", std::process::id()));
        std::fs::create_dir(&blocker).unwrap();

        assert!(write_atomic(&path, b"the new text").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "the text that was already there");
    }

    #[cfg(unix)]
    #[test]
    fn an_atomic_write_keeps_the_mode_of_the_file_it_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn a_private_write_is_readable_only_by_its_owner() {
        let dir = std::env::temp_dir().join(format!("fa-write-private-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        write_private(&path, b"{\"api_key\":\"secret\"}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"api_key\":\"secret\"}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the file was readable by others");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
