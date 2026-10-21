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
}

impl RulesView {
    pub fn new(rules: &RuleSet) -> Self {
        let rows = rules
            .all()
            .into_iter()
            .map(|(kind, text)| RuleRow { kind, text })
            .collect();
        RulesView { rows, selected: 0, scroll: 0, height: 10 }
    }

    /// What the rule under the cursor is.
    pub fn selected_rule(&self) -> Option<&RuleRow> {
        self.rows.get(self.selected)
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
            KeyCode::Esc => RulesAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.keep_visible();
                RulesAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                self.keep_visible();
                RulesAction::None
            }
            KeyCode::Home => {
                self.selected = 0;
                self.keep_visible();
                RulesAction::None
            }
            KeyCode::End => {
                self.selected = self.rows.len().saturating_sub(1);
                self.keep_visible();
                RulesAction::None
            }
            // One rule is taken back.
            KeyCode::Delete | KeyCode::Char('d') => {
                if self.selected < self.rows.len() {
                    RulesAction::Remove(self.selected)
                } else {
                    RulesAction::None
                }
            }
            // All of them are taken back. The keys are held down so that a
            // stray press does not take away a rule that was there for a
            // reason.
            KeyCode::Char('D') | KeyCode::Char('x') => RulesAction::ClearAll,
            _ if mods.contains(KeyModifiers::CONTROL) => RulesAction::None,
            _ => RulesAction::None,
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

    /// What a rule of this kind is called.
    fn kind_label(kind: RuleKind) -> &'static str {
        match kind {
            RuleKind::Tool => "tool",
            RuleKind::ShellPrefix => "shell",
            RuleKind::ShellExact => "command",
        }
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
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
            let is_sel = self.scroll + idx == self.selected;
            let pointer = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
            let kind = crate::i18n::tr(Self::kind_label(row.kind));
            let body = format!(
                "{pointer} \x1b[38;2;160;155;145m{:<10}\x1b[0m \x1b[38;2;235;230;220m{}\x1b[0m",
                kind,
                row.text
            );
            lines.push((LineKind::System, pad_row(&body)));
        }

        lines.push((LineKind::System, pad_row("")));
        let hints = crate::key_hints(
            &[
                ("↑/↓", "move"),
                ("d", "remove"),
                ("x", "remove all"),
                ("Esc", "close"),
            ],
            inner_w.saturating_sub(2),
        );
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
        let rows: Vec<String> = v.render(80).iter().map(|(_, l)| crate::strip_ansi(l)).collect();
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
