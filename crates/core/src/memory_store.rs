//! Memory, in the shape Claude Code uses: one directory per scope, a plain
//! `MEMORY.md` index that is read at the start of every turn, and one topic
//! file per fact. The model reads a topic on demand instead of carrying every
//! body in its prompt.
//!
//! The project directory is `~/.flashagent/projects/<slug>/memory/`, global
//! is `~/.flashagent/memory/`. Both are private (0700/0600): what the user
//! tells the model about themselves is not for every account on the machine.
//!
//! The index is a record, not an instruction. Its wording comes from the model
//! and sometimes from something it read, so the prompt fences it apart from
//! the user's own rules and says to open a fact before acting on it.
//!
//! The previous format (`<project>/memory/*.md` with a generated block in
//! `<project>/MEMORY.md`) is still readable here: [`legacy_inventory`] lists
//! it for `/migrate-memory`, and [`import_legacy`] copies one entry into the
//! new store without retyping the body.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::paths::write_atomic_private;

pub const INDEX: &str = "MEMORY.md";
/// A topic larger than this is not memory; it is a document that belongs in
/// the project. It is still readable, just not written through the tool.
const MAX_TOPIC_BYTES: usize = 128 * 1024;
/// The line cap an index is read with in the prompt. Long enough for several
/// dozen facts, short enough that the index never becomes the turn.
pub const INDEX_LINES: usize = 200;
const INDEX_DESCRIPTION_CHARS: usize = 160;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type MemoryResult<T> = Result<T, MemoryError>;

fn invalid(what: impl Into<String>) -> MemoryError {
    MemoryError::Invalid(what.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Decisions, conventions, how it is built and tested.
    Project,
    /// Who the user is, how they like to work, durable feedback.
    Global,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::Global => "global",
        }
    }

    /// Defaults to the project.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or("project").to_ascii_lowercase().as_str() {
            "global" | "user" | "everywhere" => Scope::Global,
            _ => Scope::Project,
        }
    }
}

/// The four kinds Claude Code uses. The old names (`preference`, `decision`,
/// `work`) are accepted when reading a file from the previous format, so a
/// migration never has to guess what a fact was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Who the user is, their role, their preferences.
    User,
    /// Guidance the user gave about how to work, with the reason.
    Feedback,
    /// Ongoing work, decisions and constraints of this codebase.
    Project,
    /// A URL, a dashboard, a ticket, an external pointer.
    Reference,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Feedback => "feedback",
            Kind::Project => "project",
            Kind::Reference => "reference",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "user" | "preference" | "prefer" | "personal" => Kind::User,
            "feedback" | "correction" | "guidance" => Kind::Feedback,
            "reference" | "link" | "url" | "external" => Kind::Reference,
            "project" | "decision" | "work" | "goal" | "task" | "convention" => Kind::Project,
            _ => Kind::Project,
        }
    }

    /// A user preference and a correction about how to work belong to the
    /// person, not to one repository.
    pub fn is_personal(self) -> bool {
        matches!(self, Kind::User | Kind::Feedback)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// File stem; the handle the model uses.
    pub name: String,
    /// The one line that goes into the index, and therefore into every prompt.
    pub description: String,
    pub kind: Kind,
    /// `YYYY-MM-DD`
    pub recorded: String,
    pub body: String,
}

impl Entry {
    pub fn path(&self) -> String {
        format!("{}.md", self.name)
    }

    pub fn to_markdown(&self) -> String {
        format!(
            "---\nname: {}\ndescription: {}\ntype: {}\nrecorded: {}\n---\n\n{}\n",
            one_line(&self.name),
            one_line(&self.description),
            self.kind.label(),
            one_line(&self.recorded),
            self.body.trim()
        )
    }

    /// A file without frontmatter is read as a body titled by its file name,
    /// so a hand-edited memory does not disappear. The old type names are
    /// mapped onto the four new ones.
    pub fn from_markdown(name: &str, text: &str) -> Self {
        let text = text.replace("\r\n", "\n");
        let text = text.trim_start_matches('\u{feff}');
        let (mut description, mut kind, mut recorded) = (String::new(), Kind::Project, String::new());
        let mut body = text.trim().to_string();

        if let Some(rest) = text.trim_start().strip_prefix("---") {
            if let Some(end) = rest.find("\n---") {
                for line in rest[..end].lines() {
                    let Some((key, value)) = line.split_once(':') else { continue };
                    let value = value.trim();
                    match key.trim() {
                        "description" => description = value.to_string(),
                        "type" | "kind" => kind = Kind::parse(value),
                        "recorded" | "date" => recorded = value.to_string(),
                        _ => {}
                    }
                }
                body = rest[end + "\n---".len()..].trim().to_string();
            }
        }
        if description.trim().is_empty() {
            description = first_line(&body);
        }
        if recorded.trim().is_empty() {
            recorded = today();
        }

        Entry { name: name.to_string(), description, kind, recorded, body }
    }

    pub fn line_count(&self) -> usize {
        self.body.lines().count()
    }

    pub fn byte_len(&self) -> usize {
        self.body.len()
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn first_line(body: &str) -> String {
    let flat = one_line(body);
    let flat = flat.trim_start_matches('#').trim();
    let cut = flat.find(". ").map(|i| i + 1).unwrap_or(flat.len());
    let sentence = flat[..cut].trim_end_matches('.').to_string();
    truncate_chars(&sentence, INDEX_DESCRIPTION_CHARS)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// A file name the tool may touch. Flat, lowercase or not, but never a path:
/// a memory name that can walk out of the directory is not a name.
pub fn file_name(raw: &str) -> MemoryResult<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(invalid("a file name is required"));
    }
    let raw = raw.strip_prefix("memory/").unwrap_or(raw);
    let raw = raw.strip_prefix("./").unwrap_or(raw);
    if raw == INDEX {
        return Ok(raw.to_string());
    }
    if raw.contains(['/', '\\', ':', '\0']) || raw == "." || raw == ".." {
        return Err(invalid("a memory file is a name, not a path"));
    }
    let name = if raw.ends_with(".md") { raw.to_string() } else { format!("{raw}.md") };
    let stem = name.strip_suffix(".md").unwrap_or(&name);
    if stem.is_empty() || stem.starts_with('.') {
        return Err(invalid("a memory file needs a visible name"));
    }
    if stem.chars().count() > 80 {
        return Err(invalid("a memory file name is too long"));
    }
    if stem.chars().any(|c| c.is_control() || matches!(c, '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(invalid("a memory file name has a character a file system may refuse"));
    }
    Ok(name)
}

fn stem(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

/// `YYYY-MM-DD`, from the system clock.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    date_from_days(secs / 86_400)
}

fn date_from_days(days: i64) -> String {
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

/// `~` for the prompts; `None` when there is no home.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Claude Code names a project directory by its path with the separators
/// flattened (`-home-user-project`). The same spelling keeps the folder
/// readable and makes collisions unlikely.
pub fn project_slug(cwd: &Path) -> String {
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let raw = resolved.to_string_lossy();
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "project".to_string()
    } else {
        out.to_string()
    }
}

/// One writer at a time per process. Cross-process writes are atomic; the
/// lock only keeps the read-modify-write of an index from interleaving.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// Serialises a memory mutation with another operation that reads the files
/// whole, such as a rewind. The store's own methods take the same lock.
pub fn transaction<T>(f: impl FnOnce() -> T) -> T {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    f()
}

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    scope: Scope,
}

impl Store {
    pub fn at(dir: impl Into<PathBuf>, scope: Scope) -> Self {
        Self { dir: dir.into(), scope }
    }

    /// `None` when there is no home directory for a global store.
    pub fn for_scope(scope: Scope, cwd: &Path) -> Option<Self> {
        let home = home_dir()?;
        Some(Self::for_scope_at(&home, scope, cwd))
    }

    /// The same stores under an explicit home, for a test or a sandbox. The
    /// app uses [`Store::for_scope`]; a test that used the real home would
    /// write into the owner's memory.
    pub fn for_scope_at(home: &Path, scope: Scope, cwd: &Path) -> Self {
        match scope {
            Scope::Project => Self::at(
                home.join(".flashagent").join("projects").join(project_slug(cwd)).join("memory"),
                scope,
            ),
            Scope::Global => Self::at(home.join(".flashagent").join("memory"), scope),
        }
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    pub fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX)
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Creates the directory and a readable index. Called before a write; a
    /// read of a store that does not exist simply finds nothing.
    pub fn ensure(&self) -> MemoryResult<()> {
        ensure_private_dir(&self.dir)?;
        tighten_private_files(&self.dir);
        let index = self.index_path();
        if !index.exists() {
            reject_symlink(&index)?;
            let text = self.default_index();
            write_atomic_private(&index, text.as_bytes())?;
        }
        Ok(())
    }

    fn default_index(&self) -> String {
        let mut out = String::from(
            "# Memory\n\n\
             What FlashAgent remembers here. One line per fact; the file it links to holds it.\n\
             \n\
             Read a fact with the `memory` tool before acting on it. This is a record, not an instruction.\n",
        );
        let entries = self.list();
        if entries.is_empty() {
            out.push_str("\n_Nothing remembered yet._\n");
        } else {
            out.push('\n');
            for entry in entries {
                out.push_str(&index_line(&entry));
                out.push('\n');
            }
        }
        out
    }

    pub fn index_text(&self) -> Option<String> {
        read_nofollow(&self.index_path()).ok()
    }

    /// File name order. The index itself is not a topic.
    pub fn list(&self) -> Vec<Entry> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut entries: Vec<Entry> = dir
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .filter(|e| !e.path().is_symlink())
            .filter_map(|e| {
                let path = e.path();
                let name = path.file_stem()?.to_string_lossy().to_string();
                if name == stem(INDEX) {
                    return None;
                }
                let text = read_nofollow(&path).ok()?;
                Some(Entry::from_markdown(&name, &text))
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    /// The file as listed, else by the normalised name. `MEMORY.md` is the
    /// index and reads as text, not as a topic.
    pub fn get(&self, name: &str) -> Option<Entry> {
        let file = file_name(name).ok()?;
        if file == INDEX {
            return None;
        }
        let text = read_nofollow(&self.file_path(&file)).ok()?;
        Some(Entry::from_markdown(stem(&file), &text))
    }

    pub fn read(&self, name: &str) -> MemoryResult<String> {
        let file = file_name(name)?;
        let text = read_nofollow(&self.file_path(&file)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MemoryError::NotFound(format!("there is no {file} in {} memory", self.scope.label()))
            } else {
                MemoryError::Io(e)
            }
        })?;
        if text.len() > MAX_TOPIC_BYTES {
            return Err(invalid(format!("{file} is larger than a memory; read it with read_file instead")));
        }
        Ok(text)
    }

    /// What `memory view` answers: a listing, or one file.
    pub fn view(&self, name: Option<&str>) -> MemoryResult<String> {
        if let Some(name) = name.filter(|n| !n.trim().is_empty()) {
            let file = file_name(name)?;
            if file == INDEX {
                return match self.index_text() {
                    Some(text) => Ok(format!(
                        "=== {} memory: {file} (the index) ===\n{}",
                        self.scope.label(),
                        text.trim_end()
                    )),
                    None => Ok(format!("No {file} in {} memory yet.", self.scope.label())),
                };
            }
            let text = self.read(&file)?;
            return Ok(format!("=== {} memory: {file} ===\n{}", self.scope.label(), text.trim_end()));
        }
        let entries = self.list();
        let index = self.index_text();
        let mut out = format!(
            "=== {} memory ===\nDirectory: {}\nIndex: {} ({})\n",
            self.scope.label(),
            self.dir.display(),
            INDEX,
            index
                .as_ref()
                .map_or("missing".to_string(), |t| format!("{} lines", t.lines().count()))
        );
        if entries.is_empty() {
            out.push_str("\nNothing remembered yet.\n");
            return Ok(out);
        }
        out.push_str("\nTopics:\n");
        for entry in &entries {
            out.push_str(&format!(
                "- {} ({}, recorded {}) — {} ({} lines)\n",
                entry.name,
                entry.kind.label(),
                entry.recorded,
                entry.description,
                entry.line_count()
            ));
        }
        let indexed = index.as_deref().map(index_files).unwrap_or_default();
        let missing: Vec<&str> = entries
            .iter()
            .filter(|e| !indexed.contains(&e.path()))
            .map(|e| e.name.as_str())
            .collect();
        if !missing.is_empty() {
            out.push_str(&format!("\nNot linked from the index (still readable): {}\n", missing.join(", ")));
        }
        out.push_str("\nRead one with the `memory` tool, command \"view\" and its path.\n");
        Ok(out)
    }

    /// Writes a new topic and links it from the index. The body is normalised
    /// into the frontmatter format; the model does not have to write it.
    pub fn create(
        &self,
        name: &str,
        text: &str,
        description: Option<&str>,
        kind: Option<Kind>,
    ) -> MemoryResult<CreateOutcome> {
        if text.len() > MAX_TOPIC_BYTES {
            return Err(invalid("the memory is too large"));
        }
        let file = file_name(name)?;
        if file == INDEX {
            return Err(invalid("MEMORY.md is the index; edit it with str_replace, do not create it"));
        }
        if self.file_path(&file).exists() {
            return Err(invalid(format!(
                "{file} already exists; use str_replace to correct it, or rename it first"
            )));
        }
        let parsed = Entry::from_markdown(stem(&file), text);
        let entry = Entry {
            name: stem(&file).to_string(),
            description: description
                .map(one_line)
                .filter(|d| !d.trim().is_empty())
                .unwrap_or(parsed.description),
            kind: kind.unwrap_or(parsed.kind),
            recorded: if parsed.recorded.is_empty() { today() } else { parsed.recorded },
            body: parsed.body,
        };
        let similar = self.find_similar(&entry.description).filter(|e| e.name != entry.name);
        self.write_topic(&entry)?;
        Ok(CreateOutcome { entry, similar })
    }

    /// Copies an entry that already has a date and a body (migration), rather
    /// than calling it a new fact.
    pub fn put(&self, entry: &Entry) -> MemoryResult<()> {
        let file = file_name(&entry.path())?;
        if file == INDEX {
            return Err(invalid("MEMORY.md is the index; edit it with str_replace, do not create it"));
        }
        if self.file_path(&file).exists() {
            return Err(invalid(format!("{file} already exists in {} memory", self.scope.label())));
        }
        self.write_topic(entry)
    }

    fn write_topic(&self, entry: &Entry) -> MemoryResult<()> {
        self.ensure()?;
        let _guard = SAVE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let path = self.file_path(&entry.path());
        reject_symlink(&path)?;
        write_atomic_private(&path, entry.to_markdown().as_bytes())?;
        self.index_upsert(entry)?;
        Ok(())
    }

    /// Replaces exactly one occurrence. More than one is ambiguous; none means
    /// the text is not there any more, and guessing would overwrite the wrong
    /// line.
    pub fn replace(&self, name: &str, old: &str, new: &str) -> MemoryResult<usize> {
        if old.is_empty() {
            return Err(invalid("old_str is empty; say what text to replace"));
        }
        let file = file_name(name)?;
        let text = self.read(&file)?;
        let count = text.matches(old).count();
        if count == 0 {
            return Err(MemoryError::NotFound(format!("old_str does not appear in {file}")));
        }
        if count > 1 {
            return Err(invalid(format!(
                "old_str appears {count} times in {file}; give enough of the line to be unambiguous"
            )));
        }
        let updated = text.replacen(old, new, 1);
        self.write_existing(&file, &updated)?;
        Ok(1)
    }

    /// Inserts after a 1-based line. `0` puts the text at the top.
    pub fn insert(&self, name: &str, after_line: usize, text_to_insert: &str) -> MemoryResult<()> {
        let file = file_name(name)?;
        let text = self.read(&file)?;
        let mut lines: Vec<&str> = text.lines().collect();
        let at = after_line.min(lines.len());
        for (offset, line) in text_to_insert.lines().enumerate() {
            lines.insert(at + offset, line);
        }
        let mut updated = lines.join("\n");
        updated.push('\n');
        self.write_existing(&file, &updated)
    }

    fn write_existing(&self, file: &str, text: &str) -> MemoryResult<()> {
        if text.len() > MAX_TOPIC_BYTES {
            return Err(invalid("the memory is too large"));
        }
        let _guard = SAVE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let path = self.file_path(file);
        reject_symlink(&path)?;
        write_atomic_private(&path, text.as_bytes())?;
        if file != INDEX {
            let entry = Entry::from_markdown(stem(file), text);
            self.index_upsert(&entry)?;
        }
        Ok(())
    }

    /// Removes a topic and its index line. The index itself is refused: it may
    /// hold the user's own notes, and `str_replace` is how those are edited.
    pub fn delete(&self, name: &str) -> MemoryResult<bool> {
        let file = file_name(name)?;
        if file == INDEX {
            return Err(invalid("MEMORY.md is the index; edit it with str_replace instead of deleting it"));
        }
        if !self.file_path(&file).exists() {
            return Ok(false);
        }
        let _guard = SAVE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::remove_file(self.file_path(&file))?;
        self.index_remove(&file)?;
        Ok(true)
    }

    /// Renames a topic and moves its index link with it.
    pub fn rename(&self, old: &str, new: &str) -> MemoryResult<()> {
        let old = file_name(old)?;
        let new = file_name(new)?;
        if old == INDEX || new == INDEX {
            return Err(invalid("MEMORY.md is the index; it cannot be renamed"));
        }
        if old == new {
            return Ok(());
        }
        if !self.file_path(&old).exists() {
            return Err(MemoryError::NotFound(format!("there is no {old} in {} memory", self.scope.label())));
        }
        if self.file_path(&new).exists() {
            return Err(invalid(format!("{new} already exists; choose another name")));
        }
        let _guard = SAVE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reject_symlink(&self.file_path(&old))?;
        reject_symlink(&self.file_path(&new))?;
        let text = read_nofollow(&self.file_path(&old))?;
        let mut entry = Entry::from_markdown(stem(&old), &text);
        entry.name = stem(&new).to_string();
        write_atomic_private(&self.file_path(&new), entry.to_markdown().as_bytes())?;
        std::fs::remove_file(self.file_path(&old))?;
        self.index_remove(&old)?;
        self.index_upsert(&entry)?;
        Ok(())
    }

    /// Only a strong overlap counts: wrongly merging two facts loses
    /// information. A hit is returned as a warning, never as a refusal.
    pub fn find_similar(&self, description: &str) -> Option<Entry> {
        let wanted = keywords(description);
        if wanted.len() < 2 {
            return None;
        }
        self.list().into_iter().find(|e| {
            let have = keywords(&e.description);
            let shared = wanted.iter().filter(|w| have.contains(*w)).count();
            shared * 2 > wanted.len().min(have.len()) && shared >= 2
        })
    }

    fn index_upsert(&self, entry: &Entry) -> MemoryResult<()> {
        self.ensure()?;
        let path = self.index_path();
        let existing = match read_nofollow(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.default_index(),
            Err(e) => return Err(MemoryError::Io(e)),
        };
        let line = index_line(entry);
        let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
        let needle = format!("]({})", entry.path());
        if let Some(at) = lines.iter().position(|l| l.contains(&needle)) {
            lines[at] = line;
        } else {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(line);
        }
        let mut updated = lines.join("\n");
        updated.push('\n');
        reject_symlink(&path)?;
        write_atomic_private(&path, updated.as_bytes())?;
        Ok(())
    }

    fn index_remove(&self, file: &str) -> MemoryResult<()> {
        let Some(existing) = self.index_text() else { return Ok(()) };
        let needle = format!("]({file})");
        let lines: Vec<&str> = existing.lines().filter(|l| !l.contains(&needle)).collect();
        let mut updated = lines.join("\n");
        updated.push('\n');
        reject_symlink(&self.index_path())?;
        write_atomic_private(&self.index_path(), updated.as_bytes())?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutcome {
    pub entry: Entry,
    /// A fact that already says much the same. The caller may warn; it does
    /// not refuse, because two close facts can still be two facts.
    pub similar: Option<Entry>,
}

fn index_line(entry: &Entry) -> String {
    // The link is never cut: a long description is shortened instead, because
    // a line without its closing `)` is not an index line any more.
    let description = truncate_chars(&one_line(&entry.description), INDEX_DESCRIPTION_CHARS);
    format!("- [{}]({}) — {}", one_line(&entry.name), entry.path(), description)
}

fn index_files(index: &str) -> Vec<String> {
    index
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("- [")?;
            let open = rest.find("](")?;
            let after = &rest[open + 2..];
            let close = after.find(')')?;
            Some(after[..close].trim_start_matches("memory/").to_string())
        })
        .collect()
}

fn keywords(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 3)
        .map(|w| w.to_lowercase())
        .collect()
}

fn ensure_private_dir(dir: &Path) -> MemoryResult<()> {
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(invalid(format!("{} is not a memory directory", dir.display())));
        }
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// The old store wrote 0644 files. They are still memory, so opening one
/// tightens it: no other account on the machine needs what the user told the
/// model about themselves. Symlinks are skipped, not followed.
#[cfg(unix)]
fn tighten_private_files(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.extension().is_some_and(|x| x == "md") || path.is_symlink() {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.permissions().mode() & 0o077 != 0 {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}

#[cfg(not(unix))]
fn tighten_private_files(_dir: &Path) {}

fn reject_symlink(path: &Path) -> MemoryResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(invalid(format!(
            "{} is a symbolic link; memory files are not written through links",
            path.display()
        ))),
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn read_nofollow(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

#[cfg(not(unix))]
fn read_nofollow(path: &Path) -> std::io::Result<String> {
    if std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "symbolic link"));
    }
    std::fs::read_to_string(path)
}

// ---------------------------------------------------------------------------
// The old format: its generated index markers, its listing, and one-way import.
// ---------------------------------------------------------------------------

pub(crate) const BEGIN: &str = "<!-- flashagent:memory -->";
pub(crate) const END: &str = "<!-- /flashagent:memory -->";

/// The range of the generated block in an old memory file. Anything outside
/// it is the user's own text and is never touched.
pub(crate) fn generated_range(text: &str) -> Option<std::ops::Range<usize>> {
    let start = text.find(BEGIN)?;
    let after_begin = start + BEGIN.len();
    let end = text[after_begin..]
        .match_indices(END)
        .find_map(|(offset, _)| {
            let at = after_begin + offset;
            let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let after_end = at + END.len();
            let line_end = text[after_end..].find('\n').map_or(text.len(), |i| after_end + i);
            (text[line_start..at].trim().is_empty() && text[after_end..line_end].trim().is_empty()).then_some(after_end)
        })
        .unwrap_or(text.len());
    Some(start..end)
}

/// Splits an old memory file into the user's own text and the generated
/// index. The two are not the same kind of thing: the first is an
/// instruction, the second is a record.
pub fn split_generated(content: &str) -> (String, Option<String>) {
    let mut own = String::new();
    let mut generated = Vec::new();
    let mut rest = content;
    while let Some(range) = generated_range(rest) {
        own.push_str(&rest[..range.start]);
        own.push('\n');
        generated.push(rest[range.clone()].trim().to_string());
        rest = &rest[range.end..];
    }
    if generated.is_empty() {
        return (content.to_string(), None);
    }
    own.push_str(rest);
    (own.trim().to_string(), Some(generated.join("\n")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyEntry {
    pub scope: Scope,
    /// File stem.
    pub name: String,
    pub path: PathBuf,
    pub description: String,
    pub kind: Kind,
    pub recorded: String,
    pub body: String,
    pub has_file: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LegacyInventory {
    pub entries: Vec<LegacyEntry>,
    pub project_dir: Option<PathBuf>,
    pub project_index: Option<PathBuf>,
    pub global_dir: Option<PathBuf>,
    pub global_index: Option<PathBuf>,
}

impl LegacyInventory {
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn by_scope(&self, scope: Scope) -> Vec<&LegacyEntry> {
        self.entries.iter().filter(|e| e.scope == scope).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// For the instruction the agent receives: one line per entry, no bodies.
    pub fn listing(&self) -> String {
        let mut out = String::new();
        for scope in [Scope::Project, Scope::Global] {
            let entries = self.by_scope(scope);
            if entries.is_empty() {
                continue;
            }
            out.push_str(&format!("{} ({}):\n", scope.label(), entries.len()));
            for entry in entries {
                out.push_str(&format!(
                    "- {} [{}] recorded {} — {}{}\n",
                    entry.name,
                    entry.kind.label(),
                    entry.recorded,
                    entry.description,
                    if entry.has_file { "" } else { " (index only; the file is gone)" }
                ));
            }
        }
        out
    }

    pub fn find(&self, scope: Scope, name: &str) -> Option<&LegacyEntry> {
        let wanted = stem(&file_name(name).ok()?).to_lowercase();
        self.entries.iter().find(|e| {
            e.scope == scope
                && (e.name.to_lowercase() == wanted
                    || e.path
                        .file_stem()
                        .is_some_and(|s| s.to_string_lossy().to_lowercase() == wanted))
        })
    }
}

/// A directory that is really a directory, not a link to somewhere else. The
/// old store is read only through paths the user can see; a link planted in a
/// project must not pull an unrelated file into the prompt.
fn real_dir(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

fn real_file(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
}

fn legacy_file_names(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter(|e| !e.path().is_symlink())
        .filter_map(|e| e.path().file_stem().map(|n| n.to_string_lossy().to_string()))
        .filter(|name| name != stem(INDEX))
        .collect();
    names.sort();
    names
}

fn read_legacy_dir(scope: Scope, dir: &Path, skip: &[String]) -> Vec<LegacyEntry> {
    if !real_dir(dir) {
        return Vec::new();
    }
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut entries: Vec<LegacyEntry> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter(|e| !e.path().is_symlink())
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_stem()?.to_string_lossy().to_string();
            if name == stem(INDEX) || skip.contains(&name) {
                return None;
            }
            let text = read_nofollow(&path).ok()?;
            let entry = Entry::from_markdown(&name, &text);
            Some(LegacyEntry {
                scope,
                name,
                path,
                description: entry.description,
                kind: entry.kind,
                recorded: entry.recorded,
                body: entry.body,
                has_file: true,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Reads the old generated index for entries whose file is already gone, so a
/// migration report can name them instead of silently losing them.
fn legacy_index_entries(scope: Scope, index: &Path) -> Vec<LegacyEntry> {
    let Ok(text) = read_nofollow(index) else { return Vec::new() };
    let Some(range) = generated_range(&text) else { return Vec::new() };
    let block = &text[range];
    let mut out = Vec::new();
    for line in block.lines() {
        let Some((file, description)) = parse_index_line(line) else { continue };
        out.push(LegacyEntry {
            scope,
            path: PathBuf::new(),
            name: stem(&file).to_string(),
            description,
            kind: Kind::Project,
            recorded: today(),
            body: String::new(),
            has_file: false,
        });
    }
    out
}

fn parse_index_line(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix("- [")?;
    let open = rest.find("](")?;
    let after = &rest[open + 2..];
    let close = after.find(')')?;
    let file = after[..close].trim().trim_start_matches("memory/").to_string();
    if !file.ends_with(".md") {
        return None;
    }
    let tail = after[close + 1..].trim();
    let description = tail
        .strip_prefix('—')
        .or_else(|| tail.strip_prefix("--"))
        .or_else(|| tail.strip_prefix('-'))
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    Some((file, description))
}

pub fn legacy_inventory(cwd: &Path, home: &Path) -> LegacyInventory {
    let project_dir = cwd.join("memory");
    let project_index = cwd.join(INDEX);
    let global_dir = home.join(".flashagent").join("memory");
    let global_index = home.join(".flashagent").join(INDEX);
    let mut inventory = LegacyInventory {
        project_dir: real_dir(&project_dir).then(|| project_dir.clone()),
        project_index: real_file(&project_index).then(|| project_index.clone()),
        global_dir: real_dir(&global_dir).then(|| global_dir.clone()),
        global_index: real_file(&global_index).then(|| global_index.clone()),
        entries: Vec::new(),
    };
    for (scope, dir, index) in [
        (Scope::Project, &project_dir, &project_index),
        (Scope::Global, &global_dir, &global_index),
    ] {
        let all_names = legacy_file_names(dir);
        // The global directory is also the new store. A topic already linked
        // from the new index has been migrated; its body is not even read.
        let linked = if scope == Scope::Global {
            read_nofollow(&dir.join(INDEX))
                .map(|text| index_files(&text).into_iter().filter_map(|f| f.strip_suffix(".md").map(str::to_string)).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut entries = read_legacy_dir(scope, dir, &linked);
        let have: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        entries.extend(
            legacy_index_entries(scope, index)
                .into_iter()
                .filter(|e| !have.contains(&e.name) && !all_names.contains(&e.name)),
        );
        inventory.entries.extend(entries);
    }
    // The old global directory is the new global directory. A `MEMORY.md`
    // inside it is the new index, not an old topic; `read_legacy_dir` already
    // skips it. Entries here stay in place during migration.
    inventory
}

/// One old entry, and what the agent decided it should become.
#[derive(Debug, Clone, Copy)]
pub struct LegacyImport<'a> {
    pub scope: Scope,
    pub name: &'a str,
    pub to: Option<&'a str>,
    pub kind: Option<Kind>,
    pub description: Option<&'a str>,
}

/// Copies an old entry into the new store. The body and first date are kept;
/// the agent chooses the new scope, name, type and description.
pub fn import_legacy(
    cwd: &Path,
    home: &Path,
    target_store: &Store,
    import: LegacyImport<'_>,
) -> MemoryResult<Entry> {
    let LegacyImport { scope: from, name, to, kind, description } = import;
    let inventory = legacy_inventory(cwd, home);
    let source = inventory
        .find(from, name)
        .ok_or_else(|| MemoryError::NotFound(format!("no old {} memory named '{name}'", from.label())))?;
    if !source.has_file {
        return Err(invalid(format!(
            "'{}' was in the old index but its file is gone; there is no body to import",
            source.name
        )));
    }
    let target = target_store.scope();
    let new_name = to.map(str::to_string).unwrap_or_else(|| source.name.clone());
    let new_file = file_name(&new_name)?;
    if new_file == INDEX {
        return Err(invalid("MEMORY.md is the index, not a topic name"));
    }
    let entry = Entry {
        name: stem(&new_file).to_string(),
        description: description
            .map(one_line)
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| source.description.clone()),
        kind: kind.unwrap_or(source.kind),
        recorded: source.recorded.clone(),
        body: source.body.clone(),
    };
    // Global topics already live in the new global directory. Normalise the
    // frontmatter and link them from the index instead of copying the file
    // over itself.
    if target == Scope::Global && from == Scope::Global && source.path == target_store.file_path(&new_file) {
        target_store.write_topic(&entry)?;
        return Ok(entry);
    }
    target_store.put(&entry)?;
    Ok(entry)
}

/// A copy of everything the old structure held, under
/// `~/.flashagent/memory-backup/<stamp>/`. Nothing is deleted here.
pub fn backup_legacy(cwd: &Path, home: &Path, stamp: &str) -> std::io::Result<PathBuf> {
    let root = home.join(".flashagent").join("memory-backup").join(stamp);
    ensure_private_dir(&root).map_err(std::io::Error::other)?;
    fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
        if !from.exists() || from.is_symlink() {
            return Ok(());
        }
        if from.is_file() {
            if let Some(parent) = to.parent() {
                ensure_private_dir(parent).map_err(std::io::Error::other)?;
            }
            let bytes = std::fs::read(from)?;
            write_atomic_private(to, &bytes)?;
            return Ok(());
        }
        ensure_private_dir(to).map_err(std::io::Error::other)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_symlink() {
                continue;
            }
            copy_tree(&path, &to.join(entry.file_name()))?;
        }
        Ok(())
    }
    copy_tree(&cwd.join("memory"), &root.join("project").join("memory"))?;
    copy_tree(&cwd.join(INDEX), &root.join("project").join(INDEX))?;
    copy_tree(&home.join(".flashagent").join("memory"), &root.join("global").join("memory"))?;
    copy_tree(&home.join(".flashagent").join(INDEX), &root.join("global").join(INDEX))?;
    Ok(root)
}

/// Removes the generated block from an old index, keeping every word the user
/// wrote. Returns `true` when nothing but whitespace remained and the file was
/// removed.
pub fn strip_legacy_index(path: &Path) -> std::io::Result<bool> {
    let text = match read_nofollow(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let (own, _) = split_generated(&text);
    if own.trim().is_empty() {
        std::fs::remove_file(path)?;
        return Ok(true);
    }
    write_atomic_private(path, format!("{}\n", own.trim_end()).as_bytes())?;
    Ok(false)
}

/// Deletes the old project topic directory after a confirmed migration. The
/// caller is responsible for the backup; this function does not guess.
pub fn remove_legacy_project_dir(cwd: &Path) -> std::io::Result<bool> {
    let dir = cwd.join("memory");
    if !real_dir(&dir) {
        return Ok(false);
    }
    std::fs::remove_dir_all(dir)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("memory"), Scope::Project);
        (store, dir)
    }

    #[test]
    fn a_memory_survives_a_round_trip_and_is_linked_from_the_index() {
        let (store, _d) = temp_store();
        let made = store
            .create("tests-run-with-nextest", "The suite is run with cargo nextest.", None, None)
            .unwrap();
        assert_eq!(made.entry.name, "tests-run-with-nextest");
        assert_eq!(store.get("tests-run-with-nextest").as_ref(), Some(&made.entry));
        let index = store.index_text().unwrap();
        assert!(
            index.contains("- [tests-run-with-nextest](tests-run-with-nextest.md) —"),
            "{index}"
        );
    }

    #[test]
    fn the_frontmatter_carries_the_four_kinds_and_old_names_map_onto_them() {
        assert_eq!(Kind::parse("preference"), Kind::User);
        assert_eq!(Kind::parse("decision"), Kind::Project);
        assert_eq!(Kind::parse("work"), Kind::Project);
        assert_eq!(Kind::parse("feedback"), Kind::Feedback);
        let entry = Entry::from_markdown(
            "old",
            "---\nname: old\ndescription: a fact\ntype: preference\nrecorded: 2025-01-02\n---\n\nbody\n",
        );
        assert_eq!(entry.kind, Kind::User);
        assert_eq!(entry.recorded, "2025-01-02");
        assert_eq!(entry.description, "a fact");
    }

    #[test]
    fn a_name_cannot_walk_out_of_the_memory_directory() {
        for bad in ["../outside", "a/b", "a\\b", "..", ".hidden", "/etc/passwd", "x:y"] {
            assert!(file_name(bad).is_err(), "{bad} was accepted");
        }
        assert_eq!(file_name("a-fact").unwrap(), "a-fact.md");
        assert_eq!(file_name("MEMORY.md").unwrap(), "MEMORY.md");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_not_written_through() {
        let (store, dir) = temp_store();
        store.ensure().unwrap();
        let target = dir.path().join("outside.md");
        fs::write(&target, "outside").unwrap();
        std::os::unix::fs::symlink(&target, store.dir().join("linked.md")).unwrap();
        let err = store.replace("linked", "outside", "changed").unwrap_err().to_string();
        assert!(!err.is_empty(), "a symlink is refused with a reason");
        assert_eq!(fs::read_to_string(&target).unwrap(), "outside");
        assert!(!store.list().iter().any(|e| e.name == "linked"));
    }

    #[cfg(unix)]
    #[test]
    fn memory_files_and_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (store, _d) = temp_store();
        store.create("a", "body", None, None).unwrap();
        assert_eq!(fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(store.index_path()).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(store.dir().join("a.md")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn replacing_exactly_one_occurrence_is_the_rule() {
        let (store, _d) = temp_store();
        store.create("a", "alpha beta gamma", Some("a fact"), None).unwrap();
        assert!(store.replace("a", "beta", "BETA").is_ok());
        assert!(store.replace("a", "not there", "x").is_err());
        fs::write(store.dir().join("b.md"), "same same").unwrap();
        assert!(store.replace("b", "same", "once").is_err(), "ambiguous replacements are refused");
    }

    #[test]
    fn delete_and_rename_move_the_index_with_them() {
        let (store, _d) = temp_store();
        store.create("one", "first", None, None).unwrap();
        store.rename("one", "two").unwrap();
        let index = store.index_text().unwrap();
        assert!(!index.contains("one.md") && index.contains("two.md"), "{index}");
        assert!(store.delete("two").unwrap());
        assert!(!store.index_text().unwrap().contains("two.md"));
    }

    #[test]
    fn a_large_store_keeps_every_fact() {
        let (store, _d) = temp_store();
        for i in 0..250 {
            store.create(&format!("fact-{i:03}"), &format!("fact number {i}"), None, None).unwrap();
        }
        assert!(store.index_text().unwrap().lines().count() > INDEX_LINES);
        assert_eq!(store.list().len(), 250);
    }

    #[test]
    fn the_old_index_splits_the_users_text_from_the_generated_block() {
        let text = "# Owner\nuse tabs\n\n<!-- flashagent:memory -->\n- [a](memory/a.md) — recorded\n<!-- /flashagent:memory -->\n\nowner tail\n";
        let (own, generated) = split_generated(text);
        assert!(own.contains("use tabs") && own.contains("owner tail"));
        assert!(!own.contains("recorded"));
        assert!(generated.unwrap().contains("recorded"));
    }

    #[test]
    fn a_legacy_project_entry_can_be_imported_without_retyping_it() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        fs::create_dir_all(cwd.path().join("memory")).unwrap();
        fs::write(
            cwd.path().join("memory").join("old-fact.md"),
            "---\nname: old-fact\ndescription: old wording\ntype: preference\nrecorded: 2025-03-04\n---\n\nThe body to keep.\n",
        )
        .unwrap();
        let store = Store::at(home.path().join(".flashagent").join("memory"), Scope::Global);
        let imported = import_legacy(
            cwd.path(),
            home.path(),
            &store,
            LegacyImport {
                scope: Scope::Project,
                name: "old-fact",
                to: Some("new-fact"),
                kind: Some(Kind::User),
                description: Some("new wording"),
            },
        )
        .unwrap();
        assert_eq!(imported.name, "new-fact");
        assert_eq!(imported.kind, Kind::User);
        assert_eq!(imported.recorded, "2025-03-04");
        assert_eq!(imported.body, "The body to keep.");
        assert!(store.index_text().unwrap().contains("new-fact.md"));
    }

    #[test]
    fn backing_up_copies_the_old_tree_without_removing_it() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        fs::create_dir_all(cwd.path().join("memory")).unwrap();
        fs::write(cwd.path().join("memory").join("a.md"), "a body").unwrap();
        fs::write(
            cwd.path().join("MEMORY.md"),
            "<!-- flashagent:memory -->\n- [a](memory/a.md) — x\n<!-- /flashagent:memory -->\n",
        )
        .unwrap();
        let backup = backup_legacy(cwd.path(), home.path(), "test-stamp").unwrap();
        assert!(backup.join("project/memory/a.md").is_file());
        assert!(backup.join("project/MEMORY.md").is_file());
        assert!(cwd.path().join("memory/a.md").is_file(), "backup is a copy, not a move");
    }

    #[test]
    fn a_long_description_never_cuts_the_index_link() {
        let (store, _d) = temp_store();
        let long = "word ".repeat(200);
        store.create("a-fact", "body", Some(&long), None).unwrap();
        let index = store.index_text().unwrap();
        let line = index.lines().find(|l| l.contains("a-fact")).unwrap_or("");
        assert!(line.contains("](a-fact.md) — "), "{index}");
        assert!(line.ends_with('…'), "the description was shortened, not the link: {line}");
    }

    #[test]
    fn the_index_cannot_be_created_or_deleted_as_a_topic() {
        let (store, _d) = temp_store();
        store.ensure().unwrap();
        assert!(store.create("MEMORY.md", "x", None, None).is_err());
        assert!(store.delete("MEMORY.md").is_err(), "the index may hold the owner's own notes");
    }

    #[test]
    fn a_global_topic_linked_from_the_new_index_is_not_offered_as_old_work() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let dir = home.path().join(".flashagent").join("memory");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("already.md"), "---\ndescription: moved\ntype: user\n---\nbody\n").unwrap();
        fs::write(dir.join("MEMORY.md"), "# Memory\n\n- [already](already.md) — moved\n").unwrap();
        fs::write(dir.join("old.md"), "---\ndescription: still old\ntype: preference\n---\nbody\n").unwrap();
        let inventory = legacy_inventory(cwd.path(), home.path());
        let names: Vec<&str> = inventory.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(!names.contains(&"already"), "{names:?}");
        assert!(names.contains(&"old"), "{names:?}");
    }

    #[test]
    fn cleanup_takes_the_generated_block_and_keeps_the_owner_text() {
        let cwd = tempfile::tempdir().unwrap();
        let index = cwd.path().join("MEMORY.md");
        fs::write(
            &index,
            "# My rules\nuse tabs\n<!-- flashagent:memory -->\n- [a](memory/a.md) — x\n<!-- /flashagent:memory -->\n",
        )
        .unwrap();
        assert!(!strip_legacy_index(&index).unwrap());
        let text = fs::read_to_string(&index).unwrap();
        assert!(text.contains("use tabs") && !text.contains("flashagent:memory"));
    }
}
