//! The `/memory` screen. Memory written unasked must be visible and
//! reversible: every fact is listed, forgettable on a key, and a note about
//! one ("this is wrong") goes straight to the model.

use crossterm::event::{KeyCode, KeyModifiers};
use flashagent_core::memory_store::{Entry, Scope, Store};

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
        0..=59 => return "just now".to_string(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86_399 => (secs / 3600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[derive(Debug, Clone)]
pub struct Row {
    pub entry: Entry,
    pub scope: Scope,
}

pub struct MemoryModal {
    pub rows: Vec<Row>,
    pub selected: usize,
    pub note: Option<String>,
    pub tab: MemoryTab,
    pub summary: SummaryState,
    /// The "Ask or update" field.
    pub ask: String,
    pub dive_selected: usize,
}

impl MemoryModal {
    pub fn new(cwd: &std::path::Path) -> Self {
        let mut rows = Vec::new();
        for scope in [Scope::Project, Scope::Global] {
            if let Some(store) = Store::for_scope(scope, cwd) {
                rows.extend(store.list().into_iter().map(|entry| Row { entry, scope }));
            }
        }
        Self { rows, selected: 0, note: None, tab: MemoryTab::List, summary: SummaryState::None, ask: String::new(), dive_selected: 0 }
    }

    pub fn current(&self) -> Option<&Row> {
        self.rows.get(self.selected)
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

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> MemoryAction {
        if self.tab == MemoryTab::Summary && self.note.is_none() {
            return self.summary_key(code, mods);
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

        match code {
            KeyCode::Esc | KeyCode::Char('q') => MemoryAction::Close,
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
                self.selected = self.selected.saturating_sub(1);
                MemoryAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                MemoryAction::None
            }
            KeyCode::Char('d') | KeyCode::Delete => match self.current() {
                Some(row) => MemoryAction::Forget { name: row.entry.name.clone(), scope: row.scope },
                None => MemoryAction::None,
            },
            KeyCode::Char('e') | KeyCode::Enter => {
                if self.rows.is_empty() {
                    return MemoryAction::None;
                }
                self.note = Some(String::new());
                MemoryAction::None
            }
            _ => MemoryAction::None,
        }
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        if self.tab == MemoryTab::Summary {
            return self.render_summary(width);
        }
        let border = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";
        let dim = "\x1b[38;2;135;130;125m";
        let text = "\x1b[38;2;200;195;185m";
        let bright = "\x1b[1;38;2;240;235;225m";
        let accent = "\x1b[1;38;2;225;175;95m";
        let inner_w = width.saturating_sub(6).clamp(24, 96);

        let mut lines: Vec<RenderLine> = Vec::new();
        // The heading shrinks first; a sheared box is worse than a shorter name.
        let title = [" What FlashAgent remembers ", " What is remembered ", " Memory "]
            .into_iter()
            .find(|t| crate::visible_width(t) < inner_w)
            .unwrap_or(" Memory ");
        let dashes = inner_w.saturating_sub(crate::visible_width(title) + 1);
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

        if self.rows.is_empty() {
            lines.push((LineKind::System, pad("")));
            lines.push((LineKind::System, pad(&format!("{dim}Nothing remembered yet. Facts worth keeping are written as they come up.{reset}"))));
        } else {
            lines.push((LineKind::System, pad("")));
            for (i, row) in self.rows.iter().enumerate() {
                let selected = i == self.selected;
                let marker = if selected { format!("{accent}▸{reset}") } else { " ".to_string() };
                let scope_tag = match row.scope {
                    Scope::Project => "project",
                    Scope::Global => "global ",
                };
                let name = if selected {
                    format!("{bright}{}{reset}", row.entry.name)
                } else {
                    format!("{text}{}{reset}", row.entry.name)
                };
                lines.push((
                    LineKind::System,
                    pad(&format!("{marker} {dim}{scope_tag}{reset}  {name}  {dim}{}{reset}", row.entry.description)),
                ));
            }

            if let Some(row) = self.current() {
                lines.push((LineKind::System, pad("")));
                lines.push((
                    LineKind::System,
                    pad(&format!("{dim}{} · recorded {}{reset}", row.entry.kind.label(), row.entry.recorded)),
                ));
                for body_line in crate::wrap_styled(&format!("{text}{}{reset}", row.entry.body), inner_w.saturating_sub(4)) {
                    lines.push((LineKind::System, pad(&body_line)));
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
            None => {
                let hints = crate::key_hints(
                    &[("↑/↓", "select"), ("d", "forget"), ("e", "correct it"), ("s", "summary"), ("Esc", "close")],
                    inner_w.saturating_sub(2),
                );
                lines.push((LineKind::System, pad(&hints)));
            }
        }
        lines.push((LineKind::System, format!("  {border}╰{}╯{reset}", "─".repeat(inner_w))));
        lines
    }

    fn render_summary(&self, width: usize) -> Vec<RenderLine> {
        let border = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";
        let dim = "\x1b[38;2;135;130;125m";
        let text = "\x1b[38;2;200;195;185m";
        let heading = "\x1b[1;38;2;240;235;225m";
        let accent = "\x1b[1;38;2;225;175;95m";
        let link = "\x1b[4;38;2;168;199;250m";
        let inner_w = width.saturating_sub(6).clamp(24, 96);
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
                    "Reading what FlashAgent remembers and writing it up…",
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
                        kind: Kind::Decision,
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
        }
    }

    #[test]
    fn forgetting_names_the_memory_under_the_cursor() {
        let mut m = modal_with(vec![
            ("build-uses-just", "the build is driven by just", Scope::Project),
            ("prefers-short-answers", "the user wants short answers", Scope::Global),
        ]);
        m.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            m.handle_key(KeyCode::Char('d'), KeyModifiers::NONE),
            MemoryAction::Forget { name: "prefers-short-answers".into(), scope: Scope::Global }
        );
    }

    #[test]
    fn a_note_reaches_the_model_with_the_memory_it_is_about() {
        // The user should not have to retype the memory's name.
        let mut m = modal_with(vec![("build-uses-just", "the build is driven by just", Scope::Project)]);
        m.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
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
        m.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        for c in "jd".chars() {
            assert_eq!(m.handle_key(KeyCode::Char(c), KeyModifiers::NONE), MemoryAction::None);
        }
        assert_eq!(m.selected, 0, "the list did not move");
        assert_eq!(m.note.as_deref(), Some("jd"));
    }

    #[test]
    fn an_abandoned_note_leaves_the_list_working() {
        let mut m = modal_with(vec![("a-fact", "one", Scope::Project)]);
        m.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(m.handle_key(KeyCode::Esc, KeyModifiers::NONE), MemoryAction::None, "esc cancels the note");
        assert_eq!(m.handle_key(KeyCode::Esc, KeyModifiers::NONE), MemoryAction::Close, "and then closes the screen");
    }

    #[test]
    fn an_empty_memory_screen_still_renders_and_closes() {
        let mut m = modal_with(vec![]);
        let lines = m.render(80);
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
            for (_, row) in m.render(width) {
                assert!(crate::visible_width(&row) <= width, "{width}: {:?}", crate::strip_ansi(&row));
            }
        }
    }

    #[test]
    fn the_summary_is_asked_for_once_and_shown_when_it_arrives() {
        let mut m = modal_with(vec![("uses-byd", "drives a BYD Sealion 07", Scope::Global)]);
        assert_eq!(m.handle_key(KeyCode::Char('s'), KeyModifiers::NONE), MemoryAction::Summarize { force: false });
        let dump = |m: &MemoryModal| m.render(90).iter().map(|(_, t)| crate::strip_ansi(t)).collect::<Vec<_>>().join("\n");
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
    fn time_since_reads_naturally() {
        assert_eq!(ago(100, 110), "just now");
        assert_eq!(ago(0, 60), "1 minute ago");
        assert_eq!(ago(0, 3 * 3600 + 5), "3 hours ago");
        assert_eq!(ago(0, 2 * 86_400), "2 days ago");
    }
}
