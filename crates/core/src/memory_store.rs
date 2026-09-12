//! Memories the model writes for itself, one fact per file.
//!
//! The old shape was a single `MEMORY.md` that every write appended to and
//! every turn injected whole. That has two failures built in: the file grows
//! until it crowds out the conversation it was supposed to help, and nothing
//! in it says when it was true, so a note from June quietly outlives the
//! decision it recorded.
//!
//! So a memory is a file: a name, a one-line description, what kind of fact it
//! is, the date it was recorded, and the fact itself. `MEMORY.md` becomes an
//! index of one line each — small enough to carry in every prompt — and the
//! bodies are read only when they turn out to matter.

use std::path::{Path, PathBuf};

/// Markers around the generated part of the index. Anything outside them is
/// the user's own writing and is never touched.
const BEGIN: &str = "<!-- flashagent:memory -->";
const END: &str = "<!-- /flashagent:memory -->";

/// Where a memory lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// This project only: decisions, conventions, how it is built and tested.
    Project,
    /// Everywhere: who the user is and how they like to work.
    Global,
}

impl Scope {
    /// The word used in tool arguments and in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::Global => "global",
        }
    }

    /// Parse a scope from a tool argument, defaulting to the project.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or("project").to_ascii_lowercase().as_str() {
            "global" | "user" | "everywhere" => Scope::Global,
            _ => Scope::Project,
        }
    }
}

/// What kind of fact a memory holds. The kind decides how much a stale entry
/// costs: a preference is safe to keep, a decision about code has to be
/// checked against the code before it is acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Who the user is and how they like to work.
    Preference,
    /// A choice made about this project and the reason for it.
    Decision,
    /// A pointer outwards: a URL, a dashboard, a ticket.
    Reference,
    /// Ongoing work, goals or constraints not visible in the code.
    Work,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Preference => "preference",
            Kind::Decision => "decision",
            Kind::Reference => "reference",
            Kind::Work => "work",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "preference" | "user" | "style" => Kind::Preference,
            "reference" | "link" | "url" => Kind::Reference,
            "work" | "project" | "goal" | "task" => Kind::Work,
            _ => Kind::Decision,
        }
    }
}

/// One remembered fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// File-name slug, and the handle the model uses to read or replace it.
    pub name: String,
    /// One line saying what this is about; this is what goes in the index and
    /// therefore into every prompt.
    pub description: String,
    /// What kind of fact it is.
    pub kind: Kind,
    /// The date it was written, `YYYY-MM-DD`.
    pub recorded: String,
    /// The fact itself.
    pub body: String,
}

impl Entry {
    /// The line this entry contributes to the index.
    pub fn index_line(&self) -> String {
        format!("- [{}](memory/{}.md) — {}", self.name, self.name, self.description)
    }

    /// The file as it is written to disk.
    pub fn to_markdown(&self) -> String {
        format!(
            "---\nname: {}\ndescription: {}\ntype: {}\nrecorded: {}\n---\n\n{}\n",
            self.name,
            self.description,
            self.kind.label(),
            self.recorded,
            self.body.trim()
        )
    }

    /// Read one back. A file without frontmatter is still readable — it is
    /// treated as a body with the file name for a title, because a memory the
    /// user edited by hand must not disappear.
    pub fn from_markdown(name: &str, text: &str) -> Self {
        let mut description = String::new();
        let mut kind = Kind::Decision;
        let mut recorded = String::new();
        let mut body = text.trim().to_string();

        if let Some(rest) = text.trim_start().strip_prefix("---") {
            if let Some(end) = rest.find("\n---") {
                for line in rest[..end].lines() {
                    let Some((key, value)) = line.split_once(':') else { continue };
                    let value = value.trim();
                    match key.trim() {
                        "description" => description = value.to_string(),
                        "type" => kind = Kind::parse(value),
                        "recorded" => recorded = value.to_string(),
                        _ => {}
                    }
                }
                body = rest[end + "\n---".len()..].trim().to_string();
            }
        }

        Entry { name: name.to_string(), description, kind, recorded, body }
    }
}

/// Turn a title into a file name: lowercase, words joined by dashes.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in title.trim().chars() {
        if c.is_alphanumeric() {
            for lower in c.to_lowercase() {
                out.push(lower);
            }
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    // A name has to survive being a file name and being typed back.
    let slug: String = slug.chars().take(60).collect();
    if slug.is_empty() {
        "memory".to_string()
    } else {
        slug.trim_matches('-').to_string()
    }
}

/// Today, as the entries record it. Dates are written out in full because a
/// memory saying "last week" means nothing when it is read in a year.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs / 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The memories of one scope, on disk.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `root` is the project directory, or the user's `~/.flashagent`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store for a scope, or `None` when there is no home directory to
    /// put a global store in.
    pub fn for_scope(scope: Scope, cwd: &Path) -> Option<Self> {
        match scope {
            Scope::Project => Some(Self::new(cwd)),
            Scope::Global => std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .ok()
                .map(|h| Self::new(PathBuf::from(h).join(".flashagent"))),
        }
    }

    /// Directory holding one file per memory.
    pub fn dir(&self) -> PathBuf {
        self.root.join("memory")
    }

    /// The index every prompt carries.
    pub fn index_path(&self) -> PathBuf {
        self.root.join("MEMORY.md")
    }

    /// Every memory, oldest file name first.
    pub fn list(&self) -> Vec<Entry> {
        let Ok(dir) = std::fs::read_dir(self.dir()) else { return Vec::new() };
        let mut entries: Vec<Entry> = dir
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .filter_map(|e| {
                let name = e.path().file_stem()?.to_string_lossy().to_string();
                let text = std::fs::read_to_string(e.path()).ok()?;
                Some(Entry::from_markdown(&name, &text))
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    /// One memory by name.
    pub fn get(&self, name: &str) -> Option<Entry> {
        let path = self.dir().join(format!("{}.md", slugify(name)));
        let text = std::fs::read_to_string(&path).ok()?;
        Some(Entry::from_markdown(&slugify(name), &text))
    }

    /// Write a memory and refresh the index. Writing over an existing name is
    /// how a fact is corrected — that is the point, not an accident.
    pub fn save(&self, entry: &Entry) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(self.dir())?;
        let path = self.dir().join(format!("{}.md", entry.name));
        std::fs::write(&path, entry.to_markdown())?;
        self.rewrite_index()?;
        Ok(path)
    }

    /// Forget one. `false` when there was nothing by that name.
    pub fn remove(&self, name: &str) -> std::io::Result<bool> {
        let path = self.dir().join(format!("{}.md", slugify(name)));
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(path)?;
        self.rewrite_index()?;
        Ok(true)
    }

    /// A memory already covering this ground, if there is one.
    ///
    /// Only a strong overlap counts. Guessing that two facts are the same and
    /// overwriting one of them loses information that was written on purpose.
    pub fn find_similar(&self, description: &str) -> Option<Entry> {
        let wanted = keywords(description);
        if wanted.len() < 2 {
            return None;
        }
        self.list().into_iter().find(|e| {
            let have = keywords(&e.description);
            let shared = wanted.iter().filter(|w| have.contains(*w)).count();
            // More than half the shorter description's content words match.
            shared * 2 > wanted.len().min(have.len()) && shared >= 2
        })
    }

    /// Rewrite the generated block of the index, leaving everything the user
    /// wrote around it exactly as it was.
    pub fn rewrite_index(&self) -> std::io::Result<()> {
        let entries = self.list();
        let mut block = String::from(BEGIN);
        block.push('\n');
        if entries.is_empty() {
            block.push_str("_Nothing remembered yet._\n");
        } else {
            for e in &entries {
                block.push_str(&e.index_line());
                block.push('\n');
            }
        }
        block.push_str(END);

        let path = self.index_path();
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let updated = match (existing.find(BEGIN), existing.find(END)) {
            (Some(start), Some(end)) if end > start => {
                format!("{}{}{}", &existing[..start], block, &existing[end + END.len()..])
            }
            _ => {
                let mut out = existing.trim_end().to_string();
                if out.is_empty() {
                    out.push_str("# Memory\n\nWhat FlashAgent remembers here. Each line links to one fact.\n");
                }
                out.push_str("\n\n");
                out.push_str(&block);
                out.push('\n');
                out
            }
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, updated)
    }
}

/// Content words of a description, lowercased; short words carry no meaning
/// for matching.
fn keywords(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 3)
        .map(|w| w.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Store::new(dir.path()), dir)
    }

    fn entry(name: &str, description: &str) -> Entry {
        Entry {
            name: name.to_string(),
            description: description.to_string(),
            kind: Kind::Decision,
            recorded: today(),
            body: "The fact itself.".to_string(),
        }
    }

    #[test]
    fn a_memory_survives_a_round_trip() {
        let (store, _d) = temp_store();
        let e = entry("tests-run-with-nextest", "the suite is run with cargo nextest, not cargo test");
        store.save(&e).unwrap();
        assert_eq!(store.get("tests-run-with-nextest").as_ref(), Some(&e));
    }

    #[test]
    fn the_index_lists_every_memory_in_one_line_each() {
        let (store, _d) = temp_store();
        store.save(&entry("build-uses-just", "the build is driven by just, not make")).unwrap();
        store.save(&entry("deploy-is-manual", "deploys are run by hand from the release branch")).unwrap();
        let index = std::fs::read_to_string(store.index_path()).unwrap();
        assert!(index.contains("- [build-uses-just](memory/build-uses-just.md) — the build is driven by just, not make"));
        assert!(index.contains("deploy-is-manual"));
        assert_eq!(index.matches("- [").count(), 2, "one line per memory:\n{index}");
    }

    #[test]
    fn what_the_user_wrote_around_the_index_is_left_alone() {
        // The project file is checked in and read by people; the generated
        // part must not eat the part they wrote.
        let (store, _d) = temp_store();
        std::fs::write(store.index_path(), "# Notes\n\nHand-written intro.\n\nAnd a closing word.\n").unwrap();
        store.save(&entry("a-fact", "something worth keeping")).unwrap();
        let index = std::fs::read_to_string(store.index_path()).unwrap();
        assert!(index.contains("Hand-written intro."), "{index}");
        assert!(index.contains("And a closing word."), "{index}");
        assert!(index.contains("a-fact"), "{index}");

        store.remove("a-fact").unwrap();
        let index = std::fs::read_to_string(store.index_path()).unwrap();
        assert!(index.contains("Hand-written intro."), "{index}");
        assert!(!index.contains("a-fact"), "a forgotten memory leaves the index:\n{index}");
    }

    #[test]
    fn forgetting_something_that_was_never_there_is_not_an_error() {
        let (store, _d) = temp_store();
        assert!(!store.remove("never-existed").unwrap());
    }

    #[test]
    fn a_second_memory_about_the_same_thing_is_recognised() {
        let (store, _d) = temp_store();
        store.save(&entry("deploys-need-approval", "production deploys need approval from the release owner")).unwrap();
        let found = store.find_similar("production deploys need approval before they run");
        assert_eq!(found.map(|e| e.name).as_deref(), Some("deploys-need-approval"));
        assert!(
            store.find_similar("the mascot animates at four frames a second").is_none(),
            "unrelated facts must not be mistaken for each other"
        );
    }

    #[test]
    fn a_hand_edited_file_without_frontmatter_still_reads() {
        let (store, _d) = temp_store();
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(store.dir().join("scribbled.md"), "just a note someone typed").unwrap();
        let e = store.get("scribbled").expect("still readable");
        assert_eq!(e.body, "just a note someone typed");
        assert_eq!(e.description, "", "nothing is invented for it");
    }

    #[test]
    fn names_come_out_usable_as_file_names() {
        assert_eq!(slugify("Tests run with `cargo nextest`!"), "tests-run-with-cargo-nextest");
        assert_eq!(slugify("  Привет, мир  "), "привет-мир");
        assert_eq!(slugify("???"), "memory");
    }

    #[test]
    fn the_date_is_written_out_in_full() {
        let d = today();
        assert_eq!(d.len(), 10, "{d}");
        assert!(d.starts_with("20"), "{d}");
        let parts: Vec<&str> = d.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!((1..=12).contains(&parts[1].parse::<u32>().unwrap()), "{d}");
        assert!((1..=31).contains(&parts[2].parse::<u32>().unwrap()), "{d}");
    }
}
