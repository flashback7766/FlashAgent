//! FlashAgent version numbers: what they look like and how they compare.
//!
//! There are two kinds, one per release channel (see `VERSIONING.md`):
//!
//! - **Beta builds**: `b<N>`, e.g. `b287`. `N` is a build counter that only
//!   grows, by a few steps per release depending on its size.
//! - **Stable releases**: `v<MAJOR>.<MINOR>.<PATCH>`, e.g. `v1.0.0`, compared
//!   as in SemVer. The leading `v` is optional when parsing. A stable release
//!   may say which beta build it was cut from as SemVer build metadata:
//!   `v1.0.0+b290`.
//!
//! Ordering, the one rule every part of the app uses:
//!
//! 1. Two betas compare by build number; two stables by MAJOR.MINOR.PATCH.
//! 2. A beta and a stable that names its build compare by build number; on a
//!    tie the stable is newer (it is that build, released).
//! 3. A stable that names no build is newer than every beta. This is what
//!    makes the first `v1.0.0` an upgrade from any `b<N>`.

use std::cmp::Ordering;
use std::fmt;

/// A parsed FlashAgent version.
#[derive(Debug, Clone, Copy)]
pub enum Version {
    /// `b<build>`.
    Beta { build: u64 },
    /// `v<major>.<minor>.<patch>`, optionally `+b<build>`.
    Stable { major: u64, minor: u64, patch: u64, build: Option<u64> },
}

impl Version {
    /// Parse `b287`, `v1.2.3`, `1.2.3`, `v1.2` (patch 0) or `v1.2.3+b290`.
    /// Surrounding whitespace is ignored; anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(n) = text.strip_prefix('b').or_else(|| text.strip_prefix('B')) {
            return if all_digits(n) { Some(Version::Beta { build: n.parse().ok()? }) } else { None };
        }
        let text = text.strip_prefix('v').or_else(|| text.strip_prefix('V')).unwrap_or(text);
        let (core, meta) = match text.split_once('+') {
            Some((core, meta)) => (core, Some(meta)),
            None => (text, None),
        };
        let build = match meta {
            None => None,
            Some(m) => match m.strip_prefix('b') {
                Some(n) if all_digits(n) => Some(n.parse().ok()?),
                _ => return None,
            },
        };
        let mut parts = core.split('.');
        let mut number = || -> Option<Option<u64>> {
            match parts.next() {
                None => Some(None),
                Some(p) if all_digits(p) => Some(Some(p.parse().ok()?)),
                Some(_) => None,
            }
        };
        let major = number()??;
        let minor = number()?.unwrap_or(0);
        let patch = number()?.unwrap_or(0);
        if parts.next().is_some() {
            return None;
        }
        Some(Version::Stable { major, minor, patch, build })
    }

    /// Find the first version-looking word in free text, such as a release
    /// title ("FlashAgent b287 — ...") or an asset name
    /// ("flashagent-b287-linux-x86_64"). Bare numbers ("2", "64") are not
    /// taken for versions: a stable needs its `v` here.
    pub fn find_in(text: &str) -> Option<Self> {
        text.split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '(' | ')' | ',' | ':' | '/'))
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .filter(|w| w.starts_with(['b', 'B', 'v', 'V']))
            .find_map(Version::parse)
    }

    pub fn is_beta(&self) -> bool {
        matches!(self, Version::Beta { .. })
    }
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        use Version::*;
        match (*self, *other) {
            (Beta { build: a }, Beta { build: b }) => a.cmp(&b),
            (Stable { major, minor, patch, .. }, Stable { major: m2, minor: n2, patch: p2, .. }) => {
                (major, minor, patch).cmp(&(m2, n2, p2))
            }
            (Stable { build: Some(s), .. }, Beta { build: b }) => s.cmp(&b).then(Ordering::Greater),
            (Stable { build: None, .. }, Beta { .. }) => Ordering::Greater,
            (Beta { .. }, Stable { .. }) => other.cmp(self).reverse(),
        }
    }
}

/// Equal when neither is newer: `v1.0.0+b290` and `v1.0.0` are one release.
impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    /// The canonical spelling: `b287`, `v1.2.3`, `v1.2.3+b290`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Version::Beta { build } => write!(f, "b{build}"),
            Version::Stable { major, minor, patch, build } => {
                write!(f, "v{major}.{minor}.{patch}")?;
                match build {
                    Some(b) => write!(f, "+b{b}"),
                    None => Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} did not parse"))
    }

    #[test]
    fn both_kinds_parse_and_print_canonically() {
        assert_eq!(v("b287"), Version::Beta { build: 287 });
        assert_eq!(v("v1.2.3").to_string(), "v1.2.3");
        assert_eq!(v("1.2.3").to_string(), "v1.2.3");
        assert_eq!(v("v1.2").to_string(), "v1.2.0");
        assert_eq!(v(" v1.0.0+b290 ").to_string(), "v1.0.0+b290");
        for bad in ["", "b", "b2x", "beta", "stable", "v", "v1.x", "v1.2.3.4", "v1.0.0+rc1", "1.0.0-rc.1"] {
            assert_eq!(Version::parse(bad), None, "{bad} parsed");
        }
    }

    #[test]
    fn the_ordering_rules_hold() {
        assert!(v("b238") < v("b287"));
        assert!(v("v1.2.10") > v("v1.2.9"));
        assert!(v("v2.0.0") > v("v1.99.99"));
        // A stable with no build is newer than every beta: b287 → v1.0.0 is
        // an upgrade.
        assert!(v("v1.0.0") > v("b287"));
        assert!(v("b9999") < v("v0.1.0"));
        // A stable that names its build sits right after that build.
        assert!(v("v1.0.0+b290") > v("b290"));
        assert!(v("v1.0.0+b290") < v("b291"));
        assert!(v("v1.0.0+b290") > v("b287"));
        // Build metadata does not change how two stables compare.
        assert_eq!(v("v1.0.0+b290").cmp(&v("v1.0.0")), Ordering::Equal);
    }

    #[test]
    fn versions_are_found_in_titles_and_asset_names() {
        assert_eq!(Version::find_in("FlashAgent b287 — fast startup"), Some(v("b287")));
        assert_eq!(Version::find_in("flashagent-b287-linux-x86_64.tar.gz"), Some(v("b287")));
        assert_eq!(Version::find_in("FlashAgent v1.0.0"), Some(v("v1.0.0")));
        assert_eq!(Version::find_in("built with 64 bit tools"), None);
        assert_eq!(Version::find_in("beta"), None);
    }
}
