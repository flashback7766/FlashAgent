//! What changed since the build you were running.
//!
//! An update that lands silently is an update nobody uses: the features are
//! there, and the person keeps working the old way. After the binary moves
//! forward by more than a hair, this shows what arrived — read straight out
//! of `CHANGELOG.md`, so the screen cannot drift from what actually shipped.

/// The changelog as it stood when this binary was built.
pub const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// One released version worth showing.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    /// Version tag, e.g. `b238`.
    pub version: String,
    /// Short title after the em dash, when the entry has one.
    pub title: String,
    /// Entries, in the order they were written.
    pub items: Vec<String>,
}

/// Sortable rank of a version: `b238` → (238, 0, 0), `v1.2.3` → (1, 2, 3).
fn rank(version: &str) -> Option<(u64, u64, u64)> {
    if let Some(n) = version.strip_prefix('b') {
        return n.parse().ok().map(|b| (b, 0, 0));
    }
    let mut parts = version.strip_prefix('v')?.split(['.', '-']).map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

/// Collapse a wrapped changelog bullet back into one paragraph.
fn flatten(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Releases in `changelog` that are newer than `from` and no newer than `to`,
/// newest first.
///
/// `Unreleased` is skipped: it describes a build nobody has. An unparseable
/// version is skipped rather than guessed at.
pub fn releases_between(changelog: &str, from: &str, to: &str) -> Vec<Release> {
    let (Some(from_rank), Some(to_rank)) = (rank(from), rank(to)) else {
        return Vec::new();
    };
    if from_rank >= to_rank {
        return Vec::new();
    }

    let mut out: Vec<Release> = Vec::new();
    let mut current: Option<Release> = None;
    let mut item: Vec<String> = Vec::new();

    let finish_item = |item: &mut Vec<String>, current: &mut Option<Release>| {
        if !item.is_empty() {
            if let Some(rel) = current.as_mut() {
                rel.items.push(flatten(item));
            }
            item.clear();
        }
    };

    for line in changelog.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            finish_item(&mut item, &mut current);
            if let Some(rel) = current.take() {
                out.push(rel);
            }
            let (version, title) = match heading.split_once(" — ") {
                Some((v, t)) => (v.trim(), t.trim()),
                None => (heading.trim(), ""),
            };
            current = match rank(version) {
                Some(r) if r > from_rank && r <= to_rank => Some(Release {
                    version: version.to_string(),
                    title: title.to_string(),
                    items: Vec::new(),
                }),
                _ => None,
            };
            continue;
        }
        if current.is_none() {
            continue;
        }
        if let Some(sub) = line.strip_prefix("### ") {
            // Sub-headings become their own entry, so a long release still
            // reads as a list rather than one wall.
            finish_item(&mut item, &mut current);
            if let Some(rel) = current.as_mut() {
                rel.items.push(format!("[{}]", sub.trim()));
            }
        } else if let Some(bullet) = line.strip_prefix("- ") {
            finish_item(&mut item, &mut current);
            item.push(bullet.to_string());
        } else if line.starts_with("  ") && !item.is_empty() {
            item.push(line.to_string());
        } else if line.trim().is_empty() {
            finish_item(&mut item, &mut current);
        }
    }
    finish_item(&mut item, &mut current);
    if let Some(rel) = current.take() {
        out.push(rel);
    }
    out
}

/// Whether there is anything to show, and what.
pub fn since(from: Option<&str>, to: &str) -> Vec<Release> {
    match from {
        // A first run has nothing to compare against, and the setup wizard
        // has just walked the user through the app anyway.
        None => Vec::new(),
        Some(from) => releases_between(CHANGELOG, from, to),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Changelog\n\
        \n\
        Some preamble.\n\
        \n\
        ## Unreleased\n\
        \n\
        - not shipped yet\n\
        \n\
        ## b238 — no emoji in the tool cards\n\
        \n\
        - Tool cards drop their emoji icons. `Edited notes.md`, and the\n  \
          extension already says what a file is.\n\
        - Directories still end in `/`.\n\
        \n\
        ## b236 — updates you can watch\n\
        \n\
        - Ctrl+U shows the download.\n\
        \n\
        ## b233 — end-to-end audit\n\
        \n\
        ### Tool calling\n\
        - Text tool calls are executed.\n";

    #[test]
    fn only_releases_between_the_two_builds_are_shown() {
        let rels = releases_between(SAMPLE, "b233", "b238");
        assert_eq!(
            rels.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(),
            vec!["b238", "b236"],
            "the build you were on is not news, and Unreleased does not exist yet"
        );
        assert_eq!(rels[0].title, "no emoji in the tool cards");
    }

    #[test]
    fn a_wrapped_entry_comes_back_as_one_paragraph() {
        let rels = releases_between(SAMPLE, "b236", "b238");
        assert_eq!(rels.len(), 1);
        assert_eq!(
            rels[0].items[0],
            "Tool cards drop their emoji icons. `Edited notes.md`, and the extension already says what a file is."
        );
        assert_eq!(rels[0].items[1], "Directories still end in `/`.");
    }

    #[test]
    fn sub_headings_survive_as_their_own_entry() {
        let rels = releases_between(SAMPLE, "b218", "b233");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].items, vec!["[Tool calling]", "Text tool calls are executed."]);
    }

    #[test]
    fn nothing_is_shown_when_the_build_did_not_move_forward() {
        assert!(releases_between(SAMPLE, "b238", "b238").is_empty());
        assert!(releases_between(SAMPLE, "b238", "b236").is_empty(), "a downgrade is not news");
        assert!(releases_between(SAMPLE, "nonsense", "b238").is_empty());
        assert!(since(None, "b238").is_empty(), "a first run has nothing to compare against");
    }

    #[test]
    fn the_shipped_changelog_parses() {
        // The screen is only as good as this file; a format change here must
        // not quietly empty it.
        let rels = releases_between(CHANGELOG, "b233", "b238");
        assert!(!rels.is_empty(), "the real changelog produced nothing");
        assert!(rels.iter().all(|r| !r.items.is_empty()), "{rels:?}");
        assert!(rels.iter().all(|r| !r.version.contains("Unreleased")));
    }
}
