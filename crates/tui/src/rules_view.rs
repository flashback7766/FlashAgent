//! `/rules`: the standing permissions the owner gave, and a way to take them
//! back.
//!
//! The rules used to live in memory until the app closed: a rule given with
//! "Always" was forgotten at the next launch, and there was nowhere to look
//! to see what was standing. So they are kept in the config, they survive the
//! restart, and this screen lists them.

use crossterm::event::{KeyCode, KeyModifiers};

use flashagent_core::permissions::{RuleKind, RuleSet};

use crate::{LineKind, RenderLine};

/// What the screen asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesAction {
    None,
    Close,
    /// The rule under the cursor is taken back.
    Remove(usize),
    /// Every rule is taken back.
    ClearAll,
}

/// One line of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRow {
    pub kind: RuleKind,
    pub text: String,
}

pub struct RulesView {
    pub rows: Vec<RuleRow>,
    pub selected: usize,
    /// The scroll of a long list.
    pub scroll: usize,
    /// How many rows the window shows.
    pub height: usize,
    /// The owner asked for every rule to be taken back and has not confirmed
    /// it yet. One press arms it, the next one does it: a stray `x` used to
    /// take away every standing permission the owner had given.
    pub confirm_clear: bool,
}

impl RulesView {
    pub fn new(rules: &RuleSet) -> Self {
        let rows = rules
            .all()
            .into_iter()
            .map(|(kind, text)| RuleRow { kind, text })
            .collect();
        RulesView { rows, selected: 0, scroll: 0, height: 10, confirm_clear: false }
    }

    /// The list as it stands now, after a rule was taken back. The cursor
    /// stays where it was: the list only ever gets shorter, and the rule
    /// that took its place is the one that was under it.
    pub fn refresh(&mut self, rules: &RuleSet) {
        let rows: Vec<RuleRow> = rules
            .all()
            .into_iter()
            .map(|(kind, text)| RuleRow { kind, text })
            .collect();
        self.rows = rows;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        self.keep_visible();
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> RulesAction {
        match code {
            KeyCode::Esc => {
                self.confirm_clear = false;
                RulesAction::Close
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.confirm_clear = false;
                self.selected = self.selected.saturating_sub(1);
                self.keep_visible();
                RulesAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.confirm_clear = false;
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                self.keep_visible();
                RulesAction::None
            }
            KeyCode::Home => {
                self.confirm_clear = false;
                self.selected = 0;
                self.keep_visible();
                RulesAction::None
            }
            KeyCode::End => {
                self.confirm_clear = false;
                self.selected = self.rows.len().saturating_sub(1);
                self.keep_visible();
                RulesAction::None
            }
            // One rule is taken back.
            KeyCode::Delete | KeyCode::Char('d') => {
                self.confirm_clear = false;
                if self.selected < self.rows.len() {
                    RulesAction::Remove(self.selected)
                } else {
                    RulesAction::None
                }
            }
            // All of them are taken back. The first press only arms it and
            // says so: a stray `x` in this panel used to take away every
            // standing permission the owner had given, with nothing to undo it.
            KeyCode::Char('D') | KeyCode::Char('x') => {
                if self.confirm_clear {
                    self.confirm_clear = false;
                    RulesAction::ClearAll
                } else {
                    self.confirm_clear = true;
                    RulesAction::None
                }
            }
            _ if mods.contains(KeyModifiers::CONTROL) => RulesAction::None,
            _ => {
                self.confirm_clear = false;
                RulesAction::None
            }
        }
    }

    fn keep_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + self.height {
            self.scroll = self.selected + 1 - self.height;
        }
    }

    /// The wheel moves the selection with its window, stopping at the ends.
    /// A wheel over the confirm state clears it, as any other move does.
    pub fn wheel(&mut self, dir: i32) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let was = self.selected;
        self.confirm_clear = false;
        let last = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + dir as isize).clamp(0, last) as usize;
        self.keep_visible();
        self.selected != was
    }

    /// A click selects the row it landed on. Taking a rule back stays with
    /// the keys: a click is a poor place for a destructive surprise.
    pub fn click_select(&mut self, line: usize) -> bool {
        // Title, what the list is, a blank — then the rows.
        const FIRST_ROW_LINE: usize = 3;
        let Some(in_window) = line.checked_sub(FIRST_ROW_LINE) else { return false };
        let idx = in_window + self.scroll;
        if idx >= self.rows.len() || idx == self.selected {
            return false;
        }
        self.confirm_clear = false;
        self.selected = idx;
        self.keep_visible();
        true
    }

    /// What a rule of this kind is called.
    fn kind_label(kind: RuleKind) -> &'static str {
        match kind {
            RuleKind::Tool => "tool",
            RuleKind::ShellPrefix => "shell",
            RuleKind::ShellExact => "command",
        }
    }

    pub fn render(&self, width: usize, hover: Option<(usize, u16)>, flash: Option<usize>) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let box_w = width.saturating_sub(6).min(100);
        let inner_w = box_w.saturating_sub(2);
        let border_color = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";

        let pad_row = |content: &str| -> String {
            let clipped = crate::tool_views::clip_ellipsis(content, inner_w);
            let pad = " ".repeat(inner_w.saturating_sub(crate::visible_width(&clipped)));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        let title = format!(" {} ", crate::i18n::tr("Standing permissions"));
        let dashes = inner_w.saturating_sub(crate::visible_width(&title) + 1);
        lines.push((
            LineKind::System,
            format!("  {border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dashes)),
        ));

        lines.push((
            LineKind::System,
            pad_row(&format!("\x1b[38;2;135;130;125m{}\x1b[0m", crate::i18n::tr("What the model may do without asking"))),
        ));
        lines.push((LineKind::System, pad_row("")));

        if self.rows.is_empty() {
            lines.push((
                LineKind::System,
                pad_row(&format!("\x1b[38;2;160;155;145m{}\x1b[0m", crate::i18n::tr("Nothing is standing: every call is asked"))),
            ));
        }
        let height = self.height.max(1);
        let end = (self.scroll + height).min(self.rows.len());
        for (idx, row) in self.rows[self.scroll.min(self.rows.len())..end].iter().enumerate() {
            let card_line = 3 + idx;
            let is_sel = self.scroll + idx == self.selected;
            let is_hover = hover.map(|(l, _)| l) == Some(card_line) && !is_sel && flash != Some(card_line);
            let is_press = flash == Some(card_line);
            let pointer = if is_sel || is_press {
                "\x1b[1;38;2;225;175;95m▸\x1b[0m"
            } else if is_hover {
                "\x1b[38;2;135;130;125m·\x1b[0m"
            } else {
                " "
            };
            let kind = crate::i18n::tr(Self::kind_label(row.kind));
            let text_color = if is_press { "\x1b[38;2;225;175;95m" } else { "\x1b[38;2;235;230;220m" };
            let body = format!("{pointer} \x1b[38;2;160;155;145m{:<10}\x1b[0m {text_color}{}\x1b[0m", kind, row.text);
            lines.push((LineKind::System, pad_row(&body)));
        }

        lines.push((LineKind::System, pad_row("")));
        if self.confirm_clear {
            lines.push((
                LineKind::System,
                pad_row(&format!("\x1b[38;2;225;175;95m{}\x1b[0m", crate::i18n::tr("Press x again to remove every rule"))),
            ));
        }
        let hints = if self.confirm_clear {
            crate::key_hints(&[("x", "press again"), ("Esc", "close")], inner_w.saturating_sub(2))
        } else {
            crate::key_hints(
                &[
                    ("↑/↓", "move"),
                    ("d", "remove"),
                    ("x", "remove all"),
                    ("Esc", "close"),
                ],
                inner_w.saturating_sub(2),
            )
        };
        lines.push((LineKind::System, pad_row(&hints)));
        lines.push((LineKind::System, format!("  {border_color}╰{}╯{reset}", "─".repeat(inner_w))));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> RulesView {
        let mut rules = RuleSet::default();
        rules.always_allow_tools.push("read_file".into());
        rules.shell_prefixes.push("npm test".into());
        rules.shell_exact.push("git status".into());
        RulesView::new(&rules)
    }

    #[test]
    fn the_list_shows_every_rule_the_owner_gave() {
        let v = view();
        assert_eq!(v.rows.len(), 3);
        assert_eq!(v.rows[0].kind, RuleKind::Tool);
        assert_eq!(v.rows[0].text, "read_file");
        assert_eq!(v.rows[1].kind, RuleKind::ShellPrefix);
        assert_eq!(v.rows[2].kind, RuleKind::ShellExact);
    }

    #[test]
    fn a_rule_under_the_cursor_is_removed() {
        let mut v = view();
        v.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(
            v.handle_key(KeyCode::Char('d'), KeyModifiers::empty()),
            RulesAction::Remove(1)
        );
    }

    #[test]
    fn all_of_them_are_removed_at_once() {
        let mut v = view();
        // The first press only arms it: a stray `x` in this panel used to take
        // away every standing permission the owner had given.
        assert_eq!(v.handle_key(KeyCode::Char('x'), KeyModifiers::empty()), RulesAction::None);
        assert_eq!(v.handle_key(KeyCode::Char('x'), KeyModifiers::empty()), RulesAction::ClearAll);
    }

    #[test]
    fn another_key_cancels_the_removal_of_every_rule() {
        let mut v = view();
        v.handle_key(KeyCode::Char('x'), KeyModifiers::empty());
        v.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(v.handle_key(KeyCode::Char('x'), KeyModifiers::empty()), RulesAction::None);
        assert_eq!(v.handle_key(KeyCode::Char('x'), KeyModifiers::empty()), RulesAction::ClearAll);
    }

    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut v = view();
        v.handle_key(KeyCode::Up, KeyModifiers::empty());
        assert_eq!(v.selected, 0, "it did not go past the first rule");
        for _ in 0..10 {
            v.handle_key(KeyCode::Down, KeyModifiers::empty());
        }
        assert_eq!(v.selected, 2, "it did not go past the last rule");
    }

    #[test]
    fn the_panel_says_what_to_do() {
        let v = view();
        let rows: Vec<String> = v.render(80, None, None).iter().map(|(_, l)| crate::strip_ansi(l)).collect();
        let text = rows.join("\n");
        assert!(text.contains("Standing permissions"), "{text}");
        assert!(text.contains("d"), "the keys are not named: {text}");
    }

    /// Every phrase the panel shows has a Russian version. The table is read
    /// with the language in hand: the language of the interface is a global,
    /// and a test that changes it changes it for the other tests too.
    #[test]
    fn every_phrase_of_the_panel_is_translated() {
        for phrase in [
            "Standing permissions",
            "What the model may do without asking",
            "Nothing is standing: every call is asked",
            "tool",
            "shell",
            "command",
            "move",
            "remove",
            "remove all",
            "press again",
            "Press x again to remove every rule",
            "close",
        ] {
            assert_ne!(
                crate::i18n::translate(crate::i18n::Lang::Russian, phrase),
                phrase,
                "{phrase:?} has no Russian version"
            );
        }
    }
}

#[cfg(test)]
mod mouse_tests {
    use super::*;
    use flashagent_core::permissions::RuleKind;

    fn view() -> RulesView {
        RulesView {
            rows: vec![
                RuleRow { kind: RuleKind::Tool, text: "read_file".into() },
                RuleRow { kind: RuleKind::ShellPrefix, text: "git ".into() },
            ],
            selected: 0,
            scroll: 0,
            height: 10,
            confirm_clear: false,
        }
    }

    #[test]
    fn a_click_selects_its_row_and_takes_no_rule_back() {
        let mut v = view();
        assert!(v.click_select(4), "line 4 is the second row");
        assert_eq!(v.selected, 1);
        assert!(!v.click_select(4), "the row under the cursor is already the selected one");
        assert!(v.click_select(3));
        assert_eq!(v.selected, 0);
        assert!(!v.click_select(1), "the title is not a row");
        assert!(!v.confirm_clear, "a click never arms the clear-everything state");
    }

    #[test]
    fn the_wheel_moves_the_selection_and_stops_at_the_ends() {
        let mut v = view();
        assert!(v.wheel(1));
        assert_eq!(v.selected, 1);
        assert!(!v.wheel(1), "the last row holds: no move, no reaction");
        assert_eq!(v.selected, 1, "no wrap");
        assert!(v.wheel(-1));
        assert_eq!(v.selected, 0);
        assert!(!v.wheel(-1), "the first row holds: no move, no reaction");
        assert_eq!(v.selected, 0);
    }
}
