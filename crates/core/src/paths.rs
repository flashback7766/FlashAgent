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
}
