//! The `/memory` screen: what FlashAgent remembers, and what to do about it.
//!
//! Memory written without being asked has to be visible and reversible, or it
//! is something happening to the user rather than for them. This lists every
//! fact, shows the one under the cursor in full, forgets one on a keypress,
//! and — because "this is wrong, fix it" is easier to say than to edit —
//! hands a note about a memory straight to the model.

use crossterm::event::{KeyCode, KeyModifiers};
use flashagent_core::memory_store::{Entry, Scope, Store};

use crate::{LineKind, RenderLine};

/// What the screen wants the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryAction {
    /// Nothing; keep the screen open.
    None,
    /// Close the screen.
    Close,
    /// Forget this memory, in this scope.
    Forget { name: String, scope: Scope },
    /// Send this to the model as a turn: the user's note about a memory.
    Tell { message: String },
}

/// One row: a memory and where it lives.
#[derive(Debug, Clone)]
pub struct Row {
    pub entry: Entry,
    pub scope: Scope,
}

/// The `/memory` screen.
pub struct MemoryModal {
    pub rows: Vec<Row>,
    pub selected: usize,
    /// Typed note, when the user is writing one.
    pub note: Option<String>,
}

impl MemoryModal {
    /// Read both stores and open the screen.
    pub fn new(cwd: &std::path::Path) -> Self {
        let mut rows = Vec::new();
        for scope in [Scope::Project, Scope::Global] {
            if let Some(store) = Store::for_scope(scope, cwd) {
                rows.extend(store.list().into_iter().map(|entry| Row { entry, scope }));
            }
        }
        Self { rows, selected: 0, note: None }
    }

    /// The memory under the cursor.
    pub fn current(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> MemoryAction {
        // While a note is being typed the keys belong to the note.
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
        let border = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";
        let dim = "\x1b[38;2;135;130;125m";
        let text = "\x1b[38;2;200;195;185m";
        let bright = "\x1b[1;38;2;240;235;225m";
        let accent = "\x1b[1;38;2;225;175;95m";
        let inner_w = width.saturating_sub(6).clamp(24, 96);

        let mut lines: Vec<RenderLine> = Vec::new();
        // The heading is the first thing that stops fitting; a sheared box is
        // worse than a shorter name.
        let title = [" What FlashAgent remembers (/memory) ", " What is remembered ", " Memory "]
            .into_iter()
            .find(|t| crate::visible_width(t) < inner_w)
            .unwrap_or(" Memory ");
        let dashes = inner_w.saturating_sub(crate::visible_width(title) + 1);
        lines.push((
            LineKind::System,
            format!("  {border}┌─{accent}{title}{border}{}┐{reset}", "─".repeat(dashes)),
        ));

        let pad = |content: &str| -> String {
            let max_w = inner_w.saturating_sub(2);
            let vis = crate::visible_width(content);
            let clipped = if vis > max_w { crate::clip_ansi(content, max_w) } else { content.to_string() };
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
                lines.push((LineKind::System, pad(&format!("{dim}enter — send · esc — cancel{reset}"))));
            }
            None => {
                lines.push((
                    LineKind::System,
                    pad(&format!("{dim}↑/↓ — select · e — tell the model what to change · d — forget · esc — close{reset}")),
                ));
            }
        }
        lines.push((LineKind::System, format!("  {border}└{}┘{reset}", "─".repeat(inner_w))));
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
        // "This one is wrong" only means something next to the memory it
        // refers to, and the user should not have to retype its name.
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
        // 'd' and 'j' are commands on the list and letters in a note.
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
}
