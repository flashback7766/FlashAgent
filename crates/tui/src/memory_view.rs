//! The `/memory` screen. Memory written unasked must be visible and
//! reversible: every fact is listed, forgettable on a key, and a note about
//! one ("this is wrong") goes straight to the model.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use flashagent_core::memory_store::{Entry, Scope, Store, INDEX};

use crate::{LineKind, RenderLine};

#[derive(Debug, Clone, PartialEq)]
pub enum MemoryAction {
    None,
    Close,
    Forget { name: String, scope: Scope },
    /// The user's note about a memory, sent as a turn.
    Tell { message: String },
    /// `force` rewrites one that is still current.
    Summarize { force: bool },
    /// Open a memory file, or the index, in the owner's editor.
    Edit { path: PathBuf },
}

/// Written by the model.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemorySummary {
    /// Overview first.
    pub sections: Vec<(String, String)>,
    /// Sendable with one key.
    pub dive_deeper: Vec<String>,
    /// Seconds since the epoch.
    pub updated: u64,
    /// A changed memory marks the summary stale.
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SummaryState {
    None,
    Writing,
    Ready(MemorySummary),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTab {
    List,
    Summary,
}

pub fn fingerprint(rows: &[Row]) -> String {
    let mut parts: Vec<String> = rows.iter().map(|r| {
        format!("{:?}", (r.scope.label(), &r.entry.name, &r.entry.recorded, &r.entry.description, &r.entry.body))
    }).collect();
    parts.sort();
    format!("v2:{:016x}", flashagent_core::snapshots::prompt_hash(&parts.join("|")))
}

/// `language` names the language to write in, when known.
pub fn summary_request(rows: &[Row], language: Option<&str>) -> (String, String) {
    let lang = match language {
        Some(l) => format!("Write every string in {l}."),
        None => "Write in the language most of the memories are written in.".to_string(),
    };
    let system = format!(
        "You summarize what a coding assistant remembers about its user and their projects. \
         Return strictly JSON: {{\"overview\": \"2-3 sentences addressed to the user as 'you', naming the main themes\", \
         \"sections\": [{{\"title\": \"a short topic name\", \"text\": \"one paragraph addressed to the user\"}}], \
         \"dive_deeper\": [\"a question or request the user could send next about these memories\", \"...\"]}}. \
         Group related memories into 2-4 sections. Give exactly 2 dive_deeper items, each under 90 characters. \
         Only state what the memories say; never invent facts. {lang} \
         Do not think out loud. Start with {{ and return only the JSON."
    );
    let mut user = String::from("Memories:\n");
    for r in rows {
        user.push_str(&format!(
            "- [{}] {} ({}, {}): {}\n",
            r.scope.label(),
            r.entry.name,
            r.entry.description,
            r.entry.recorded,
            r.entry.body.trim()
        ));
    }
    (system, user)
}

pub type ParsedSummary = (Vec<(String, String)>, Vec<String>);

pub fn parse_summary(raw: &str) -> Option<ParsedSummary> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    let v: serde_json::Value = serde_json::from_str(raw.get(start..=end)?).ok()?;
    let text = |v: &serde_json::Value| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let mut sections = Vec::new();
    if let Some(overview) = v.get("overview").and_then(text) {
        sections.push(("Overview".to_string(), overview));
    }
    for sec in v.get("sections").and_then(|s| s.as_array()).into_iter().flatten() {
        if let (Some(title), Some(body)) = (sec.get("title").and_then(text), sec.get("text").and_then(text)) {
            sections.push((title, body));
        }
    }
    let dive = v
        .get("dive_deeper")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().filter_map(text).take(3).collect())
        .unwrap_or_default();
    (!sections.is_empty()).then_some((sections, dive))
}

pub fn ago(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    let (n, unit) = match secs {
        0..=59 => return crate::i18n::tr("just now").to_string(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86_399 => (secs / 3600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    // The word follows the number, and Russian has three forms of it. The
    // forms live in the translation table, like every other word.
    let word = match unit {
        "minute" => crate::text::plural(n as usize, "minute", "minutes"),
        "hour" => crate::text::plural(n as usize, "hour", "hours"),
        _ => crate::text::plural(n as usize, "day", "days"),
    };
    format!("{} {}", word, crate::i18n::tr("ago"))
}

#[derive(Debug, Clone)]
pub struct Row {
    pub entry: Entry,
    pub scope: Scope,
}

pub struct MemoryModal {
    pub rows: Vec<Row>,
    /// Index into the rows the current search leaves visible.
    pub selected: usize,
    pub note: Option<String>,
    pub tab: MemoryTab,
    pub summary: SummaryState,
    /// The "Ask or update" field.
    pub ask: String,
    pub dive_selected: usize,
    /// The project whose stores are shown; an editor needs a real path.
    cwd: PathBuf,
    /// `Some("")` while the search field is open; `Some(text)` filters the
    /// list. `None` shows everything.
    pub filter: Option<String>,
    /// `d` asks once, `d` again forgets. Holds the row the question is about.
    pub confirm_delete: Option<String>,
    /// First line of the body shown under the list.
    pub detail_scroll: usize,
    /// Entries still in the old layout. They are named on an empty screen so
    /// `/migrate-memory` is discovered instead of the memory looking lost.
    pub legacy: usize,
    /// What a click may reach, as the last render drew it: the list's first
    /// line, its length, the first row it showed, and the hint line's index.
    /// A click means what the screen showed, so the layout is stored, not
    /// guessed.
    layout: Cell<ClickLayout>,
    /// The hint words that answer a press, each as the key that acts for it.
    zones: RefCell<Vec<(usize, usize, char)>>,
}

/// The list's place on the card at the last render.
#[derive(Clone, Copy, Default)]
struct ClickLayout {
    list_at: usize,
    list_len: usize,
    list_from: usize,
    hint_line: usize,
}

impl MemoryModal {
    pub fn new(cwd: &std::path::Path) -> Self {
        let mut rows = Vec::new();
        for scope in [Scope::Project, Scope::Global] {
            if let Some(store) = Store::for_scope(scope, cwd) {
                // Opening the screen is also how the index becomes editable:
                // a store that has topics but no index gets one now.
                let _ = store.ensure();
                rows.extend(store.list().into_iter().map(|entry| Row { entry, scope }));
            }
        }
        let legacy = flashagent_core::memory_store::home_dir()
            .map(|home| flashagent_core::memory_store::legacy_inventory(cwd, &home).count())
            .unwrap_or(0);
        Self {
            rows,
            selected: 0,
            note: None,
            tab: MemoryTab::List,
            summary: SummaryState::None,
            ask: String::new(),
            dive_selected: 0,
            cwd: cwd.to_path_buf(),
            filter: None,
            confirm_delete: None,
            detail_scroll: 0,
            legacy,
            layout: Cell::new(ClickLayout::default()),
            zones: RefCell::new(Vec::new()),
        }
    }

    /// The rows the search leaves, in order. An empty search shows all.
    pub fn visible_rows(&self) -> Vec<&Row> {
        let needle = self.filter.as_deref().unwrap_or("").trim().to_lowercase();
        self.rows
            .iter()
            .filter(|row| {
                needle.is_empty()
                    || row.entry.name.to_lowercase().contains(&needle)
                    || row.entry.description.to_lowercase().contains(&needle)
                    || row.entry.body.to_lowercase().contains(&needle)
            })
            .collect()
    }

    pub fn current(&self) -> Option<&Row> {
        self.visible_rows().get(self.selected).copied()
    }

    fn current_path(&self, row: &Row) -> Option<PathBuf> {
        Store::for_scope(row.scope, &self.cwd)
            .map(|store| store.dir().join(row.entry.path()))
            .filter(|path| path.is_file())
    }

    /// The index of the store the cursor is in, or the project's when the list
    /// is empty. `i` opens it.
    fn current_index(&self) -> PathBuf {
        let scope = self.current().map_or(Scope::Project, |row| row.scope);
        Store::for_scope(scope, &self.cwd)
            .map(|store| store.index_path())
            .unwrap_or_else(|| self.cwd.join(INDEX))
    }

    fn move_to(&mut self, next: usize) {
        self.selected = next;
        self.detail_scroll = 0;
        self.confirm_delete = None;
    }

    fn move_by(&mut self, delta: isize) {
        let count = self.visible_rows().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let last = count as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last) as usize;
        self.move_to(next);
    }

    pub fn summary_is_stale(&self) -> bool {
        match &self.summary {
            SummaryState::Ready(s) => s.fingerprint != fingerprint(&self.rows),
            SummaryState::Writing => false,
            SummaryState::None | SummaryState::Failed(_) => true,
        }
    }

    fn summary_key(&mut self, code: KeyCode, mods: KeyModifiers) -> MemoryAction {
        let dives = match &self.summary {
            SummaryState::Ready(s) => s.dive_deeper.clone(),
            _ => Vec::new(),
        };
        match code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.tab = MemoryTab::List;
                MemoryAction::None
            }
            KeyCode::Esc if !self.ask.is_empty() => {
                self.ask.clear();
                MemoryAction::None
            }
            KeyCode::Esc => MemoryAction::Close,
            KeyCode::Up => {
                self.dive_selected = self.dive_selected.saturating_sub(1);
                MemoryAction::None
            }
            KeyCode::Down => {
                if self.dive_selected + 1 < dives.len() {
                    self.dive_selected += 1;
                }
                MemoryAction::None
            }
            KeyCode::Enter => {
                let message = if self.ask.trim().is_empty() {
                    match dives.get(self.dive_selected) {
                        Some(d) => d.clone(),
                        None => return MemoryAction::None,
                    }
                } else {
                    format!("About what you remember of me: {}", std::mem::take(&mut self.ask).trim())
                };
                MemoryAction::Tell { message }
            }
            // Ctrl+R: rewrite even though nothing changed.
            KeyCode::Char('r') if mods.contains(KeyModifiers::CONTROL) => {
                self.summary = SummaryState::Writing;
                MemoryAction::Summarize { force: true }
            }
            KeyCode::Backspace => {
                self.ask.pop();
                MemoryAction::None
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                self.ask.push(c);
                MemoryAction::None
            }
            _ => MemoryAction::None,
        }
    }

    /// The wheel moves the list's selection, stopping at the ends. Only
    /// while the list is what is on the screen: a word being typed or a
    /// summary being written takes no wheel.
    pub fn wheel(&mut self, dir: i32) -> bool {
        if self.tab != MemoryTab::List || self.note.is_some() || self.filter.is_some() {
            return false;
        }
        let count = self.visible_rows().len();
        if count == 0 {
            return false;
        }
        let was = self.selected;
        self.selected = (self.selected as isize + dir as isize).clamp(0, count as isize - 1) as usize;
        self.detail_scroll = 0;
        self.confirm_delete = None;
        self.selected != was
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> MemoryAction {
        if self.tab == MemoryTab::Summary && self.note.is_none() {
            return self.summary_key(code, mods);
        }
        // A search field takes the letters, as a note does.
        if let Some(filter) = self.filter.as_mut() {
            return match code {
                KeyCode::Esc => {
                    self.filter = None;
                    self.selected = 0;
                    MemoryAction::None
                }
                KeyCode::Enter => {
                    self.filter = Some(filter.clone());
                    MemoryAction::None
                }
                KeyCode::Backspace => {
                    filter.pop();
                    self.selected = 0;
                    MemoryAction::None
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                    filter.push(c);
                    self.selected = 0;
                    MemoryAction::None
                }
                _ => MemoryAction::None,
            };
        }
        // While a note is typed, keys belong to it.
        if let Some(note) = self.note.as_mut() {
            return match code {
                KeyCode::Esc => {
                    self.note = None;
                    MemoryAction::None
                }
                KeyCode::Enter => {
                    let text = note.trim().to_string();
                    self.note = None;
                    if text.is_empty() {
                        return MemoryAction::None;
                    }
                    let about = self
                        .current()
                        .map(|r| {
                            format!(
                                "About the {} memory \"{}\" ({}): {text}",
                                r.scope.label(),
                                r.entry.name,
                                r.entry.description
                            )
                        })
                        .unwrap_or(text);
                    MemoryAction::Tell { message: about }
                }
                KeyCode::Backspace => {
                    note.pop();
                    MemoryAction::None
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                    note.push(c);
                    MemoryAction::None
                }
                _ => MemoryAction::None,
            };
        }

        // Ctrl+D and the other shortcuts belong to the app, not to a list
        // where the letter itself is a command.
        if mods.contains(KeyModifiers::CONTROL) {
            return MemoryAction::None;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.filter.is_some() {
                    self.filter = None;
                    self.selected = 0;
                    return MemoryAction::None;
                }
                MemoryAction::Close
            }
            KeyCode::Char('/') => {
                self.filter = Some(String::new());
                self.selected = 0;
                MemoryAction::None
            }
            KeyCode::Tab | KeyCode::Char('s') => {
                if self.rows.is_empty() {
                    return MemoryAction::None;
                }
                self.tab = MemoryTab::Summary;
                if self.summary_is_stale() {
                    self.summary = SummaryState::Writing;
                    return MemoryAction::Summarize { force: false };
                }
                MemoryAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_by(-1);
                MemoryAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_by(1);
                MemoryAction::None
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10);
                MemoryAction::None
            }
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(10);
                MemoryAction::None
            }
            KeyCode::Char('i') => {
                MemoryAction::Edit { path: self.current_index() }
            }
            KeyCode::Char('e') | KeyCode::Char('E') => match self.current() {
                Some(row) => match self.current_path(row) {
                    Some(path) => MemoryAction::Edit { path },
                    None => MemoryAction::None,
                },
                None => MemoryAction::Edit { path: self.current_index() },
            },
            // A note to the model: the old `e`. The editor is `e` now; the
            // correction still goes through the conversation.
            KeyCode::Char('c') | KeyCode::Enter => {
                if self.current().is_none() {
                    return MemoryAction::None;
                }
                self.note = Some(String::new());
                MemoryAction::None
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                let Some(row) = self.current() else { return MemoryAction::None };
                let name = row.entry.name.clone();
                let scope = row.scope;
                let key = format!("{}:{name}", scope.label());
                if self.confirm_delete.as_deref() == Some(key.as_str()) {
                    self.confirm_delete = None;
                    MemoryAction::Forget { name, scope }
                } else {
                    self.confirm_delete = Some(key);
                    MemoryAction::None
                }
            }
            _ => MemoryAction::None,
        }
    }

    /// A press on the list screen: a row selects, the row already selected
    /// opens in the editor, and a hint word acts as its key. "Forget" stays
    /// with the keyboard — the second press that deletes must be a deliberate
    /// one. Typing (a note, a search) and the summary keep the keys.
    pub fn click(&mut self, line: usize, column: usize) -> MemoryAction {
        if self.note.is_some() || self.filter.is_some() || self.tab == MemoryTab::Summary {
            return MemoryAction::None;
        }
        let zones = self.zones.borrow().clone();
        for (from, to, key) in zones.iter() {
            if column >= *from && column < *to {
                return match *key {
                    '\x1b' => self.handle_key(KeyCode::Esc, KeyModifiers::NONE),
                    other => self.handle_key(KeyCode::Char(other), KeyModifiers::NONE),
                };
            }
        }
        let layout = self.layout.get();
        if layout.list_len > 0 && line >= layout.list_at && line < layout.list_at + layout.list_len {
            let at = layout.list_from + (line - layout.list_at);
            if self.visible_rows().get(at).is_some() {
                if at == self.selected {
                    return self.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
                }
                self.move_to(at);
            }
        }
        MemoryAction::None
    }

    pub fn render(&self, width: usize, height: usize) -> Vec<RenderLine> {
        if self.tab == MemoryTab::Summary {
            self.layout.set(ClickLayout::default());
            self.zones.borrow_mut().clear();
            return self.render_summary(width);
        }
        let border = "\x1b[38;2;95;90;85m";
        let reset = "\x1b[0m";
        let dim = "\x1b[38;2;135;130;125m";
        let text = "\x1b[38;2;232;227;218m";
        let bright = "\x1b[1;38;2;232;227;218m";
        let accent = "\x1b[1;38;2;225;175;95m";
        let warn = "\x1b[38;2;245;160;80m";
        let inner_w = width.saturating_sub(6).min(96);

        let mut lines: Vec<RenderLine> = Vec::new();
        // The heading shrinks first; a sheared box is worse than a shorter name.
        let titles = [
            format!(" {} ", crate::i18n::tr("What FlashAgent remembers")),
            format!(" {} ", crate::i18n::tr("What is remembered")),
            format!(" {} ", crate::i18n::tr("Memory")),
        ];
        let title = titles
            .iter()
            .find(|t| crate::visible_width(t) < inner_w)
            .cloned()
            .unwrap_or_else(|| format!(" {} ", crate::i18n::tr("Memory")));
        let dashes = inner_w.saturating_sub(crate::visible_width(&title) + 1);
        lines.push((
            LineKind::System,
            format!("  {border}╭─{accent}{title}{border}{}╮{reset}", "─".repeat(dashes)),
        ));

        let pad = |content: &str| -> String {
            let max_w = inner_w.saturating_sub(2);
            let clipped = crate::tool_views::clip_ellipsis(content, max_w);
            let fill = " ".repeat(max_w.saturating_sub(crate::visible_width(&clipped)));
            format!("  {border}│{reset} {clipped}{fill} {border}│{reset}")
        };

        let visible = self.visible_rows();
        let height = height.max(8);
        let list_budget = height.saturating_sub(9).max(1);
        let list_rows = visible.len().min(list_budget).min((height / 2).max(2)).max(1);
        let selected = self.selected.min(visible.len().saturating_sub(1));
        let start = if selected >= list_rows { selected + 1 - list_rows } else { 0 };

        lines.push((LineKind::System, pad("")));
        if visible.is_empty() {
            // An empty list has no rows to press; the hints keep the keys.
            self.layout.set(ClickLayout::default());
            self.zones.borrow_mut().clear();
            let said = if self.rows.is_empty() {
                crate::i18n::tr("Nothing remembered yet. Facts worth keeping are written as they come up.")
            } else {
                crate::i18n::tr("Nothing matches the search.")
            };
            lines.push((LineKind::System, pad(&format!("{dim}{said}{reset}"))));
            if self.rows.is_empty() && self.legacy > 0 {
                lines.push((
                    LineKind::System,
                    pad(&format!(
                        "{warn}{}{reset}",
                        crate::i18n::tr_fill(
                            "Old memory: {count} entries. /migrate-memory converts them.",
                            &[("count", &self.legacy.to_string())]
                        )
                    )),
                ));
            }
        } else {
            // The list begins two rows in (title, blank); where the hints sit
            // is stored once the render reaches them.
            self.layout.set(ClickLayout { list_at: 2, list_len: list_rows, list_from: start, hint_line: 0 });
            for (offset, row) in visible.iter().enumerate().skip(start).take(list_rows) {
                let is_selected = offset == selected;
                let marker = if is_selected { format!("{accent}›{reset}") } else { " ".to_string() };
                let scope_tag = match row.scope {
                    Scope::Project => crate::i18n::tr("project"),
                    Scope::Global => crate::i18n::tr("global "),
                };
                let name = if is_selected {
                    format!("{bright}{}{reset}", row.entry.name)
                } else {
                    format!("{text}{}{reset}", row.entry.name)
                };
                let type_tag = format!("{dim}{}{reset}", row.entry.kind.label());
                lines.push((
                    LineKind::System,
                    pad(&format!(
                        "{marker} {dim}{scope_tag}{reset}  {name}  {type_tag}  {dim}{}{reset}",
                        row.entry.description
                    )),
                ));
            }
            if visible.len() > list_rows {
                lines.push((
                    LineKind::System,
                    pad(&format!(
                        "{dim}… {} more{reset}",
                        visible.len().saturating_sub(start + list_rows)
                    )),
                ));
            }
            if self.legacy > 0 {
                lines.push((
                    LineKind::System,
                    pad(&format!(
                        "{warn}{}{reset}",
                        crate::i18n::tr_fill(
                            "Old memory: {count} entries. /migrate-memory converts them.",
                            &[("count", &self.legacy.to_string())]
                        )
                    )),
                ));
            }

            if let Some(row) = visible.get(selected) {
                lines.push((LineKind::System, pad("")));
                let path = self
                    .current_path(row)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| format!("{} memory", row.scope.label()));
                lines.push((
                    LineKind::System,
                    pad(&format!(
                        "{dim}{} · recorded {} · {path}{reset}",
                        row.entry.kind.label(),
                        row.entry.recorded
                    )),
                ));
                let detail_budget = height.saturating_sub(list_rows + 9).max(1);
                let wrapped = crate::wrap_styled(
                    &format!("{text}{}{reset}", row.entry.body),
                    inner_w.saturating_sub(4),
                );
                let first = self.detail_scroll.min(wrapped.len().saturating_sub(1));
                for body_line in wrapped.iter().skip(first).take(detail_budget) {
                    lines.push((LineKind::System, pad(body_line)));
                }
                let hidden = wrapped.len().saturating_sub(first + detail_budget);
                if hidden > 0 {
                    lines.push((
                        LineKind::System,
                        pad(&format!("{dim}… {hidden} more line(s) · PgDn · e opens the file{reset}")),
                    ));
                }
            }
        }

        lines.push((LineKind::System, pad("")));
        match &self.note {
            Some(note) => {
                lines.push((LineKind::System, pad(&format!("{accent}Tell the model what to change:{reset}"))));
                lines.push((LineKind::System, pad(&format!("{text}{note}\x1b[7m \x1b[27m{reset}"))));
                lines.push((LineKind::System, pad(&crate::key_hints(&[("Enter", "send"), ("Esc", "cancel")], inner_w.saturating_sub(2)))));
            }
            None if self.filter.is_some() => {
                let query = self.filter.as_deref().unwrap_or("");
                lines.push((LineKind::System, pad(&format!("{accent}/{reset} {text}{query}\x1b[7m \x1b[27m{reset}"))));
                lines.push((LineKind::System, pad(&crate::key_hints(&[("Enter", "keep filter"), ("Esc", "clear")], inner_w.saturating_sub(2)))));
            }
            None => {
                if let Some(row) = self.current() {
                    let key = format!("{}:{}", row.scope.label(), row.entry.name);
                    if self.confirm_delete.as_deref() == Some(key.as_str()) {
                        lines.push((
                            LineKind::System,
                            pad(&format!(
                                "{warn}{} \"{}\"? d again to forget it{reset}",
                                crate::i18n::tr("Forget"),
                                row.entry.name
                            )),
                        ));
                    }
                }
                let hints = crate::key_hints(
                    &[
                        ("↑/↓", "select"),
                        ("e", "edit"),
                        ("s", "summary"),
                        ("i", "index"),
                        ("d", "forget"),
                        ("c", "correct it"),
                        ("/", "search"),
                        ("Esc", "close"),
                    ],
                    inner_w.saturating_sub(2),
                );
                // The hint words are buttons, except the ones a click must not
                // be: "forget" deletes on the second press, and that stays a
                // key's job. The content starts four cells in, past the border.
                let pairs = [("↑/↓", "select"), ("e", "edit"), ("s", "summary"), ("i", "index"), ("d", "forget"), ("c", "correct it"), ("/", "search"), ("Esc", "close")];
                let keys: [Option<char>; 8] = [None, Some('e'), Some('s'), Some('i'), None, Some('c'), Some('/'), Some('\x1b')];
                let mut layout = self.layout.get();
                layout.hint_line = lines.len();
                self.layout.set(layout);
                *self.zones.borrow_mut() = crate::welcome::key_hint_zones(&pairs, inner_w.saturating_sub(2))
                    .iter()
                    .enumerate()
                    .filter_map(|(i, zone)| zone.zip(keys[i]).map(|((from, to), key)| (from + 4, to + 4, key)))
                    .collect();
                lines.push((LineKind::System, pad(&hints)));
            }
        }
        lines.push((LineKind::System, format!("  {border}╰{}╯{reset}", "─".repeat(inner_w))));
        lines
    }

    fn render_summary(&self, width: usize) -> Vec<RenderLine> {
        let border = "\x1b[38;2;95;90;85m";
        let reset = "\x1b[0m";
        let dim = "\x1b[38;2;135;130;125m";
        let text = "\x1b[38;2;232;227;218m";
        let heading = "\x1b[1;38;2;232;227;218m";
        let accent = "\x1b[1;38;2;225;175;95m";
        let link = "\x1b[4;38;2;138;180;248m";
        let inner_w = width.saturating_sub(6).min(96);
        let t = crate::anim::now_ms();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);

        let updated = match &self.summary {
            SummaryState::Ready(s) => format!(" {dim}updated {}{reset} ", ago(s.updated, now)),
            _ => String::new(),
        };
        let title = format!(" {accent}Memory summary{reset}{updated}");
        let dashes = inner_w.saturating_sub(crate::visible_width(&title) + 1);
        let mut lines: Vec<RenderLine> =
            vec![(LineKind::System, format!("  {border}╭─{title}{border}{}╮{reset}", "─".repeat(dashes)))];
        let pad = |content: &str| -> String {
            let max_w = inner_w.saturating_sub(2);
            let clipped = crate::tool_views::clip_ellipsis(content, max_w);
            let fill = " ".repeat(max_w.saturating_sub(crate::visible_width(&clipped)));
            format!("  {border}│{reset} {clipped}{fill} {border}│{reset}")
        };
        let body_w = inner_w.saturating_sub(4);
        lines.push((LineKind::System, pad("")));
        match &self.summary {
            SummaryState::None | SummaryState::Writing => {
                let words = crate::anim::shimmer(
                    crate::i18n::tr("Reading what FlashAgent remembers and writing it up…"),
                    t,
                    2000,
                    crate::anim::Rgb(135, 130, 125),
                    crate::anim::Rgb(240, 235, 225),
                );
                lines.push((LineKind::System, pad(&format!("\x1b[38;2;138;180;248m{}{reset} {words}", crate::anim::spinner(t)))));
            }
            SummaryState::Failed(why) => {
                lines.push((LineKind::System, pad(&format!("\x1b[38;2;230;110;95mCould not write the summary:{reset} {text}{why}{reset}"))));
                lines.push((LineKind::System, pad(&crate::key_hints(&[("Ctrl+R", "try again")], body_w))));
            }
            SummaryState::Ready(summary) => {
                if summary.fingerprint != fingerprint(&self.rows) {
                    lines.push((LineKind::System, pad(&format!("{dim}Memories changed since this was written; Ctrl+R rewrites it{reset}"))));
                    lines.push((LineKind::System, pad("")));
                }
                for (i, (name, body)) in summary.sections.iter().enumerate() {
                    if i > 0 {
                        lines.push((LineKind::System, pad("")));
                    }
                    lines.push((LineKind::System, pad(&format!("{heading}{name}{reset}"))));
                    for row in crate::wrap_styled(&format!("{text}{body}{reset}"), body_w) {
                        lines.push((LineKind::System, pad(&row)));
                    }
                }
                if !summary.dive_deeper.is_empty() {
                    lines.push((LineKind::System, pad("")));
                    lines.push((LineKind::System, pad(&format!("{heading}Dive deeper{reset}"))));
                    for (i, q) in summary.dive_deeper.iter().enumerate() {
                        let row = if i == self.dive_selected && self.ask.is_empty() {
                            format!("{accent}└{reset} {link}{q}{reset}")
                        } else {
                            format!("{dim}└ {q}{reset}")
                        };
                        lines.push((LineKind::System, pad(&row)));
                    }
                }
            }
        }
        lines.push((LineKind::System, pad("")));
        let ask = if self.ask.is_empty() {
            format!("{accent}›{reset} {dim}Ask or update…{reset}")
        } else {
            let (shown, _) = crate::tail_window(&self.ask, body_w.saturating_sub(2));
            let caret = if crate::anim::blink_on(t) { "\x1b[7m \x1b[27m" } else { " " };
            format!("{accent}›{reset} {text}{shown}{caret}{reset}")
        };
        lines.push((LineKind::System, pad(&ask)));
        lines.push((
            LineKind::System,
            pad(&crate::key_hints(
                &[
                    ("type", "to ask"),
                    ("↑/↓", "pick a question"),
                    ("Enter", "send"),
                    ("Ctrl+R", "rewrite"),
                    ("Tab", "list"),
                    ("Esc", "close"),
                ],
                inner_w.saturating_sub(2),
            )),
        ));
        lines.push((LineKind::System, format!("  {border}╰{}╯{reset}", "─".repeat(inner_w))));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_core::memory_store::{today, Kind};

    fn modal_with(rows: Vec<(&str, &str, Scope)>) -> MemoryModal {
        MemoryModal {
            rows: rows
                .into_iter()
                .map(|(name, description, scope)| Row {
                    entry: Entry {
                        name: name.into(),
                        description: description.into(),
                        kind: Kind::Project,
                        recorded: today(),
                        body: "The fact itself, at length.".into(),
                    },
                    scope,
                })
                .collect(),
            selected: 0,
            note: None,
            tab: MemoryTab::List,
            summary: SummaryState::None,
            ask: String::new(),
            dive_selected: 0,
            cwd: PathBuf::new(),
            filter: None,
            confirm_delete: None,
            detail_scroll: 0,
            legacy: 0,
            layout: Cell::new(ClickLayout::default()),
            zones: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn forgetting_names_the_memory_under_the_cursor() {
        let mut m = modal_with(vec![
            ("build-uses-just", "the build is driven by just", Scope::Project),
            ("prefers-short-answers", "the user wants short answers", Scope::Global),
        ]);
        m.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(m.handle_key(KeyCode::Char('d'), KeyModifiers::NONE), MemoryAction::None, "the first d asks");
        assert_eq!(
            m.handle_key(KeyCode::Char('d'), KeyModifiers::NONE),
            MemoryAction::Forget { name: "prefers-short-answers".into(), scope: Scope::Global },
            "the second d forgets"
        );
    }

    #[test]
    fn a_click_picks_a_row_and_acts_like_the_keys() {
        let mut m = modal_with(vec![
            ("build-uses-just", "the build is driven by just", Scope::Project),
            ("prefers-short-answers", "the user wants short answers", Scope::Global),
        ]);
        m.render(80, 60);
        // The list starts two rows in; the first row selects.
        assert_eq!(m.click(2, 3), MemoryAction::None);
        assert_eq!(m.current().map(|r| r.entry.name.as_str()), Some("build-uses-just"));
        // The second row selects too.
        assert_eq!(m.click(3, 3), MemoryAction::None);
        assert_eq!(m.current().map(|r| r.entry.name.as_str()), Some("prefers-short-answers"));
        // The row already selected acts as `e` does — the click is the key's
        // path, whatever that path returns for a row with no file behind it.
        assert_eq!(m.click(3, 3), m.handle_key(KeyCode::Char('e'), KeyModifiers::NONE));
        // A hint word answers where the line drew it; "forget" is not among
        // the words a click may reach.
        m.render(80, 60);
        let pairs = [("↑/↓", "select"), ("e", "edit"), ("s", "summary"), ("i", "index"), ("d", "forget"), ("c", "correct it"), ("/", "search"), ("Esc", "close")];
        let zones = crate::welcome::key_hint_zones(&pairs, 78);
        let hint_line = m.layout.get().hint_line;
        let (e_from, _) = zones[1].expect("edit is drawn");
        assert_eq!(m.click(hint_line, e_from + 4), MemoryAction::None, "`e` on a row with no file says nothing, and so does the word");
        // The summary word opens the summary, as `s` does.
        assert_eq!(m.click(hint_line, zones[2].unwrap().0 + 4), MemoryAction::Summarize { force: false }, "the summary word opens the summary, as `s` does");
        m.tab = MemoryTab::List;
        m.summary = SummaryState::None;
        // Where "d forget" is drawn, a click does nothing.
        let (d_from, _) = zones[4].expect("forget is drawn on a wide screen");
        assert_eq!(m.click(hint_line, d_from + 4), MemoryAction::None, "forget stays with the keyboard");
    }

    #[test]
    fn a_note_reaches_the_model_with_the_memory_it_is_about() {
        // The user should not have to retype the memory's name.
        let mut m = modal_with(vec![("build-uses-just", "the build is driven by just", Scope::Project)]);
        m.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        for c in "это уже не так, теперь make".chars() {
            m.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let action = m.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let MemoryAction::Tell { message } = action else { panic!("{action:?}") };
        assert!(message.contains("build-uses-just"), "{message}");
        assert!(message.contains("это уже не так, теперь make"), "{message}");
        assert!(m.note.is_none(), "the note field closes after sending");
    }

    #[test]
    fn typing_a_note_does_not_navigate_or_delete() {
        // 'd' and 'j' are list commands and letters in a note.
        let mut m = modal_with(vec![
            ("a-fact", "one", Scope::Project),
            ("b-fact", "two", Scope::Project),
        ]);
        m.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        for c in "jd".chars() {
            assert_eq!(m.handle_key(KeyCode::Char(c), KeyModifiers::NONE), MemoryAction::None);
        }
        assert_eq!(m.selected, 0, "the list did not move");
        assert_eq!(m.note.as_deref(), Some("jd"));
    }

    #[test]
    fn an_abandoned_note_leaves_the_list_working() {
        let mut m = modal_with(vec![("a-fact", "one", Scope::Project)]);
        m.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(m.handle_key(KeyCode::Esc, KeyModifiers::NONE), MemoryAction::None, "esc cancels the note");
        assert_eq!(m.handle_key(KeyCode::Esc, KeyModifiers::NONE), MemoryAction::Close, "and then closes the screen");
    }

    #[test]
    fn an_empty_memory_screen_still_renders_and_closes() {
        let mut m = modal_with(vec![]);
        let lines = m.render(80, 40);
        let dump: String = lines.iter().map(|(_, t)| crate::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Nothing remembered yet"), "{dump}");
        assert!(dump.contains("s summary") && dump.contains("Esc close") && dump.contains('╰'), "{dump}");
        assert_eq!(m.handle_key(KeyCode::Char('d'), KeyModifiers::NONE), MemoryAction::None);
        assert_eq!(m.handle_key(KeyCode::Esc, KeyModifiers::NONE), MemoryAction::Close);
    }

    #[test]
    fn every_row_fits_the_terminal() {
        let m = modal_with(vec![
            ("a-very-long-memory-name-that-keeps-going", "a description long enough to run past the edge of a narrow window", Scope::Project),
        ]);
        for width in [40usize, 60, 80, 120] {
            for (_, row) in m.render(width, 60) {
                assert!(crate::visible_width(&row) <= width, "{width}: {:?}", crate::strip_ansi(&row));
            }
        }
    }

    #[test]
    fn the_summary_is_asked_for_once_and_shown_when_it_arrives() {
        let mut m = modal_with(vec![("uses-byd", "drives a BYD Sealion 07", Scope::Global)]);
        assert_eq!(m.handle_key(KeyCode::Char('s'), KeyModifiers::NONE), MemoryAction::Summarize { force: false });
        let dump = |m: &MemoryModal| m.render(90, 60).iter().map(|(_, t)| crate::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump(&m).contains("writing it up"), "{}", dump(&m));

        let (sections, dive) = parse_summary(
            r#"```json
{"overview": "Вы интересуетесь автомобилем.", "sections": [{"title": "Автомобиль", "text": "BYD Sealion 07."}], "dive_deeper": ["Сравните версии DiLink"]}
```"#,
        )
        .unwrap();
        m.summary = SummaryState::Ready(MemorySummary { sections, dive_deeper: dive, updated: 0, fingerprint: fingerprint(&m.rows) });
        let shown = dump(&m);
        assert!(shown.contains("Overview") && shown.contains("Автомобиль") && shown.contains("└ Сравните версии DiLink"), "{shown}");
        assert!(shown.lines().all(|l| crate::visible_width(l) <= 90), "{shown}");

        // Going back to the list and in again does not ask a second time.
        m.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.handle_key(KeyCode::Tab, KeyModifiers::NONE), MemoryAction::None);
        assert_eq!(
            m.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            MemoryAction::Tell { message: "Сравните версии DiLink".into() }
        );
        // Typing asks instead; forgetting a memory marks the summary stale.
        for c in "а что ещё?".chars() {
            m.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let MemoryAction::Tell { message } = m.handle_key(KeyCode::Enter, KeyModifiers::NONE) else { panic!() };
        assert!(message.contains("а что ещё?"));
        m.rows.clear();
        assert!(m.summary_is_stale());
    }

    #[test]
    fn changing_memory_text_or_description_invalidates_the_summary() {
        let mut m = modal_with(vec![("editor", "preferred editor", Scope::Global)]);
        m.rows[0].entry.body = "Use vim".into();
        m.summary = SummaryState::Ready(MemorySummary {
            sections: vec![("Overview".into(), "You use vim.".into())],
            dive_deeper: Vec::new(),
            updated: 0,
            fingerprint: fingerprint(&m.rows),
        });
        m.rows[0].entry.body = "Use zed".into();
        assert!(m.summary_is_stale(), "equal-length edits must invalidate the summary");
        m.rows[0].entry.body = "Use vim".into();
        assert!(!m.summary_is_stale());
        m.rows[0].entry.description = "former editor".into();
        assert!(m.summary_is_stale(), "description edits must invalidate the summary");
    }

    #[test]
    fn a_search_narrows_the_list_and_esc_brings_it_back() {
        let mut m = modal_with(vec![
            ("build-uses-just", "the build is driven by just", Scope::Project),
            ("prefers-short-answers", "the user wants short answers", Scope::Global),
        ]);
        m.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        for c in "just".chars() {
            m.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(m.visible_rows().len(), 1);
        assert_eq!(m.visible_rows()[0].entry.name, "build-uses-just");
        assert_eq!(m.handle_key(KeyCode::Enter, KeyModifiers::NONE), MemoryAction::None);
        assert_eq!(m.filter.as_deref(), Some("just"), "Enter keeps the filter");
        assert_eq!(m.handle_key(KeyCode::Esc, KeyModifiers::NONE), MemoryAction::None);
        assert!(m.filter.is_none(), "Esc clears it before closing the screen");
        assert_eq!(m.visible_rows().len(), 2);
    }

    #[test]
    fn a_search_that_matches_nothing_says_so() {
        let mut m = modal_with(vec![("a-fact", "one", Scope::Project)]);
        m.filter = Some("no such fact".into());
        let dump: String = m.render(80, 40).iter().map(|(_, t)| crate::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Nothing matches the search."), "{dump}");
    }

    #[test]
    fn the_editor_opens_the_file_under_the_cursor_or_the_index() {
        let mut m = modal_with(vec![("a-fact", "one", Scope::Project)]);
        assert!(matches!(m.handle_key(KeyCode::Char('e'), KeyModifiers::NONE), MemoryAction::None), "there is no file on disk in this fixture");
        let MemoryAction::Edit { path } = m.handle_key(KeyCode::Char('i'), KeyModifiers::NONE) else {
            panic!("i opens the index");
        };
        assert!(path.ends_with("MEMORY.md"), "{}", path.display());
    }

    #[test]
    fn page_down_scrolls_a_long_body_instead_of_overflowing() {
        let mut m = modal_with(vec![("long", "a long fact", Scope::Project)]);
        m.rows[0].entry.body = (1..=40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(m.detail_scroll, 0);
        m.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(m.detail_scroll, 10);
        let dump: String = m.render(80, 24).iter().map(|(_, t)| crate::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("line 11"), "the window did not move:\n{dump}");
    }

    #[test]
    fn time_since_reads_naturally() {
        assert_eq!(ago(100, 110), "just now");
        assert_eq!(ago(0, 60), "1 minute ago");
        assert_eq!(ago(0, 3 * 3600 + 5), "3 hours ago");
        assert_eq!(ago(0, 2 * 86_400), "2 days ago");
    }
}
