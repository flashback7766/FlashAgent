//! The list of what the app told the user, so it can be read again.
//!
//! A notice is a line under the composer that fades, and a message in the
//! placeholder that stays until the prompt is touched. Both are gone the
//! moment they are gone; this keeps them, so "what did it just say?" has an
//! answer.

use crossterm::event::{KeyCode, KeyModifiers};
use crate::tool_views::clip_ellipsis;
use crate::{visible_width, LineKind, RenderLine};

const RESET: &str = "\x1b[0m";
const BORDER_GOLD: &str = "\x1b[38;2;225;175;95m";
const TEXT_BRIGHT: &str = "\x1b[1;38;2;245;240;235m";
const TEXT_MUTED: &str = "\x1b[38;2;145;150;160m";
const TEXT_GREEN: &str = "\x1b[38;2;135;220;145m";
const TEXT_AMBER: &str = "\x1b[38;2;235;175;90m";

/// One thing the app said, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeEntry {
    /// How long ago, in words: "just now", "3 min ago".
    pub when: String,
    pub text: String,
    /// Something is wrong: amber, not green.
    pub warning: bool,
}

/// What the view asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeAction {
    None,
    Close,
}

/// The notices, newest first.
pub struct NoticeLog {
    pub entries: Vec<NoticeEntry>,
    pub selected: usize,
    /// The first entry on screen.
    pub scroll: usize,
    /// How many entries fit between the borders.
    pub height: usize,
}

impl NoticeLog {
    pub fn new(entries: Vec<NoticeEntry>) -> Self {
        Self { entries, selected: 0, scroll: 0, height: 12 }
    }

    /// Moves the cursor, and the window with it.
    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.keep_visible();
    }

    /// The keys the view answers to.
    pub fn handle_key(&mut self, code: KeyCode, _mods: KeyModifiers) -> NoticeAction {
        match code {
            KeyCode::Esc => NoticeAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_by(-1);
                NoticeAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_by(1);
                NoticeAction::None
            }
            KeyCode::PageUp => {
                self.move_by(-(self.height as isize));
                NoticeAction::None
            }
            KeyCode::PageDown => {
                self.move_by(self.height as isize);
                NoticeAction::None
            }
            KeyCode::Home => {
                self.selected = 0;
                self.keep_visible();
                NoticeAction::None
            }
            KeyCode::End => {
                self.selected = self.entries.len().saturating_sub(1);
                self.keep_visible();
                NoticeAction::None
            }
            _ => NoticeAction::None,
        }
    }

    /// The window follows the cursor.
    fn keep_visible(&mut self) {
        let height = self.height.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        let max_scroll = self.entries.len().saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
    }

    /// The wheel moves the selection with its window, stopping at the ends.
    pub fn wheel(&mut self, dir: i32) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let was = self.selected;
        self.move_by(dir as isize);
        self.selected != was
    }

    /// A click selects the notice it landed on. The title and the separator
    /// under it belong to the box; the rows begin at the third line.
    pub fn click_select(&mut self, line: usize) -> bool {
        const FIRST_ROW_LINE: usize = 2;
        let Some(in_window) = line.checked_sub(FIRST_ROW_LINE) else { return false };
        let idx = in_window + self.scroll;
        if idx >= self.entries.len() || idx == self.selected {
            return false;
        }
        self.selected = idx;
        self.keep_visible();
        true
    }

    /// The list, as it is drawn.
    pub fn render(&self, width: usize, hover: Option<(usize, u16)>, flash: Option<usize>) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let box_w = width.saturating_sub(6).min(100);
        let inner_w = box_w.saturating_sub(2);

        let pad_row = |content: &str| -> String {
            let clipped = clip_ellipsis(content, inner_w);
            let pad = " ".repeat(inner_w.saturating_sub(visible_width(&clipped)));
            format!("  {BORDER_GOLD}│{RESET} {clipped}{pad} {BORDER_GOLD}│{RESET}")
        };

        let title = format!(
            " \x1b[1;38;2;225;175;95m{}\x1b[0m \x1b[38;2;160;155;145m{}\x1b[0m ",
            crate::i18n::tr("Notices"),
            crate::i18n::tr_fill("{n} kept", &[("n", &self.entries.len().to_string())])
        );
        let dashes = box_w.saturating_sub(visible_width(&title) + 1);
        lines.push((LineKind::System, format!("  {BORDER_GOLD}╭─{title}{}╮{RESET}", "─".repeat(dashes))));
        lines.push((LineKind::System, format!("  {BORDER_GOLD}├{}┤{RESET}", "─".repeat(box_w))));

        let height = self.height.max(1);
        let end = (self.scroll + height).min(self.entries.len());
        for (idx, entry) in self.entries[self.scroll.min(self.entries.len())..end].iter().enumerate() {
            let card_line = 2 + idx;
            let is_sel = self.scroll + idx == self.selected;
            let is_hover = hover.map(|(l, _)| l) == Some(card_line) && !is_sel && flash != Some(card_line);
            let is_press = flash == Some(card_line);
            let pointer = if is_sel || is_press {
                format!("{TEXT_BRIGHT}▸{RESET}")
            } else if is_hover {
                format!("{TEXT_MUTED}·{RESET}")
            } else {
                " ".to_string()
            };
            let (when_styled, text_styled) = if is_press {
                (format!("{TEXT_AMBER}{}{RESET}", entry.when), format!("{TEXT_AMBER}{}{RESET}", entry.text))
            } else if entry.warning {
                (format!("{TEXT_AMBER}{}{RESET}", entry.when), format!("{TEXT_BRIGHT}{}{RESET}", entry.text))
            } else if is_hover {
                (format!("{TEXT_GREEN}{}{RESET}", entry.when), format!("{TEXT_BRIGHT}{}{RESET}", entry.text))
            } else {
                (format!("{TEXT_GREEN}{}{RESET}", entry.when), format!("{TEXT_MUTED}{}{RESET}", entry.text))
            };
            lines.push((LineKind::System, pad_row(&format!("{pointer} {when_styled}  {text_styled}"))));
        }
        if self.entries.is_empty() {
            lines.push((LineKind::System, pad_row(&format!("{TEXT_MUTED}{}{RESET}", crate::i18n::tr("Nothing has been said yet")))));
        }

        lines.push((LineKind::System, format!("  {BORDER_GOLD}├{}┤{RESET}", "─".repeat(box_w))));
        let hints: &[(&str, &str)] = &[("↑/↓", "move"), ("Esc", "close")];
        lines.push((LineKind::System, pad_row(&crate::key_hints(hints, inner_w.saturating_sub(1)))));
        lines.push((LineKind::System, format!("  {BORDER_GOLD}╰{}╯{RESET}", "─".repeat(box_w))));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<NoticeEntry> {
        vec![
            NoticeEntry { when: "just now".into(), text: "Exported to /tmp/chat.md".into(), warning: false },
            NoticeEntry { when: "2 min ago".into(), text: "The server did not answer".into(), warning: true },
        ]
    }

    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut v = NoticeLog::new(entries());
        v.move_by(-5);
        assert_eq!(v.selected, 0);
        v.move_by(50);
        assert_eq!(v.selected, 1);
        assert_eq!(v.handle_key(KeyCode::Esc, KeyModifiers::NONE), NoticeAction::Close);
    }

    #[test]
    fn the_list_is_drawn_with_what_was_said() {
        let v = NoticeLog::new(entries());
        let screen: Vec<String> = v.render(80, None, None).into_iter().map(|(_, l)| crate::strip_ansi(&l)).collect();
        assert!(screen.iter().any(|l| l.contains("Exported to")), "{screen:#?}");
        assert!(screen.iter().any(|l| l.contains("did not answer")), "{screen:#?}");
        assert!(screen.iter().any(|l| l.contains("2 kept")), "{screen:#?}");
    }

    #[test]
    fn an_empty_history_says_so() {
        let v = NoticeLog::new(Vec::new());
        let screen: Vec<String> = v.render(80, None, None).into_iter().map(|(_, l)| crate::strip_ansi(&l)).collect();
        assert!(screen.iter().any(|l| l.contains("Nothing has been said yet")), "{screen:#?}");
    }

    #[test]
    fn the_title_of_the_list_is_in_the_language_of_the_interface() {
        let v = NoticeLog::new(entries());
        let screen: Vec<String> = v.render(80, None, None).into_iter().map(|(_, l)| crate::strip_ansi(&l)).collect();
        assert!(screen[0].contains(crate::i18n::tr("Notices")), "{screen:#?}");
        assert!(screen[0].contains(&crate::i18n::tr_fill("{n} kept", &[("n", "2")])), "{screen:#?}");
    }
}

#[cfg(test)]
mod mouse_tests {
    use super::*;

    fn view() -> NoticeLog {
        NoticeLog::new(vec![
            NoticeEntry { when: "now".into(), text: "first".into(), warning: false },
            NoticeEntry { when: "then".into(), text: "second".into(), warning: true },
        ])
    }

    #[test]
    fn the_wheel_and_a_click_walk_the_entries() {
        let mut v = view();
        assert!(v.wheel(1));
        assert_eq!(v.selected, 1);
        assert!(!v.wheel(1), "the last row holds: no move, no reaction");
        assert_eq!(v.selected, 1, "no wrap");
        assert!(v.click_select(2), "line 2 is the first row");
        assert_eq!(v.selected, 0);
        assert!(!v.click_select(2), "already the selected one");
        assert!(!v.click_select(0), "the title is not a row");
    }
}
