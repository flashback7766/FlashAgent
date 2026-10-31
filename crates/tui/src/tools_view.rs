//! The list of tools and whether each one is on.
//!
//! A tool that is off is not in the schemas the model reads, and a call to it
//! is refused. The list is the whole registry: what is on, what is off, and
//! what each one is for.

use crossterm::event::{KeyCode, KeyModifiers};
use crate::tool_views::clip_ellipsis;
use crate::{visible_width, LineKind, RenderLine};

const RESET: &str = "\x1b[0m";
const BORDER_GOLD: &str = "\x1b[38;2;225;175;95m";
const TEXT_BRIGHT: &str = "\x1b[1;38;2;245;240;235m";
const TEXT_MUTED: &str = "\x1b[38;2;145;150;160m";
const TEXT_GREEN: &str = "\x1b[38;2;135;220;145m";
const TEXT_OFF: &str = "\x1b[38;2;130;135;145m";
const TEXT_AMBER: &str = "\x1b[38;2;225;175;95m";

/// The tools a safe session does not have: the shell, the web and the
/// subagents. Reading and editing inside the project stay, so the agent can
/// still do the work; it just cannot leave the project or run anything.
pub const SAFE_MODE_TOOLS: [&str; 6] = ["run_shell", "web_search", "web_fetch", "spawn_agent", "send_message", "review_agent"];

/// One tool in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    pub name: String,
    /// What the tool is for, in the model's words.
    pub description: String,
    /// Off means the model is not offered it and a call to it is refused.
    pub on: bool,
    /// Not offered in this session anyway: the web tools without the web, the
    /// picture tool for a model that cannot see.
    pub available: bool,
}

/// What the view asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsAction {
    None,
    Close,
    /// The tool under the cursor was turned on or off.
    Toggle(String, bool),
}

/// The list of tools, with the cursor on one of them.
pub struct ToolsView {
    pub rows: Vec<ToolRow>,
    pub selected: usize,
    /// The first row on screen.
    pub scroll: usize,
    /// How many rows fit between the borders.
    pub height: usize,
}

impl ToolsView {
    pub fn new(rows: Vec<ToolRow>) -> Self {
        Self { rows, selected: 0, scroll: 0, height: 12 }
    }

    /// The names of the tools that are off.
    pub fn disabled(&self) -> Vec<String> {
        self.rows.iter().filter(|r| !r.on).map(|r| r.name.clone()).collect()
    }

    /// How many tools are on.
    pub fn on_count(&self) -> usize {
        self.rows.iter().filter(|r| r.on).count()
    }

    /// Moves the cursor, and the window with it.
    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last) as usize;
        self.selected = next;
        self.keep_visible();
    }

    /// The wheel moves the cursor with its window, a notch at a time. True
    /// when there was a row to move to.
    pub fn wheel(&mut self, dir: i32) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let was = self.selected;
        self.move_by(dir as isize);
        self.selected != was
    }

    /// A click on the card. `line` counts from the title row. A row selects
    /// its tool; the tool already under the cursor turns on or off, as Space
    /// would — a click is a focus first, an action second.
    pub fn click(&mut self, line: usize) -> ToolsAction {
        // The title and the separator under it belong to the box.
        const FIRST_ROW_LINE: usize = 2;
        let Some(in_window) = line.checked_sub(FIRST_ROW_LINE) else { return ToolsAction::None };
        let idx = in_window + self.scroll;
        if idx >= self.rows.len() {
            return ToolsAction::None;
        }
        if idx != self.selected {
            self.selected = idx;
            self.keep_visible();
            ToolsAction::None
        } else {
            self.toggle()
        }
    }

    /// The row the cursor is on, if there is one.
    pub fn current(&self) -> Option<&ToolRow> {
        self.rows.get(self.selected)
    }

    /// Turns the tool under the cursor on or off. A tool that is not offered
    /// in this session cannot be turned on: there is nothing to offer.
    pub fn toggle(&mut self) -> ToolsAction {
        let Some(row) = self.rows.get_mut(self.selected) else { return ToolsAction::None };
        if !row.available && !row.on {
            return ToolsAction::None;
        }
        row.on = !row.on;
        ToolsAction::Toggle(row.name.clone(), row.on)
    }

    /// The keys the view answers to.
    pub fn handle_key(&mut self, code: KeyCode, _mods: KeyModifiers) -> ToolsAction {
        match code {
            KeyCode::Esc => ToolsAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_by(-1);
                ToolsAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_by(1);
                ToolsAction::None
            }
            KeyCode::PageUp => {
                self.move_by(-(self.height as isize));
                ToolsAction::None
            }
            KeyCode::PageDown => {
                self.move_by(self.height as isize);
                ToolsAction::None
            }
            KeyCode::Home => {
                self.selected = 0;
                self.keep_visible();
                ToolsAction::None
            }
            KeyCode::End => {
                self.selected = self.rows.len().saturating_sub(1);
                self.keep_visible();
                ToolsAction::None
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle(),
            _ => ToolsAction::None,
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
        let max_scroll = self.rows.len().saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
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

        let on = self.on_count();
        let title = format!(
            " \x1b[1;38;2;225;175;95m{}\x1b[0m \x1b[38;2;160;155;145m{}\x1b[0m ",
            crate::i18n::tr("Tools"),
            crate::i18n::tr("{on} of {total} on")
                .replace("{on}", &on.to_string())
                .replace("{total}", &self.rows.len().to_string())
        );
        let dashes = box_w.saturating_sub(visible_width(&title) + 1);
        lines.push((LineKind::System, format!("  {BORDER_GOLD}╭─{title}{BORDER_GOLD}{}╮{RESET}", "─".repeat(dashes))));
        lines.push((LineKind::System, format!("  {BORDER_GOLD}├{}┤{RESET}", "─".repeat(box_w))));

        let height = self.height.max(1);
        let end = (self.scroll + height).min(self.rows.len());
        for (idx, row) in self.rows[self.scroll.min(self.rows.len())..end].iter().enumerate() {
            let card_line = 2 + idx;
            let is_sel = self.scroll + idx == self.selected;
            let is_hover = hover.map(|(l, _)| l) == Some(card_line) && !is_sel && flash != Some(card_line);
            let is_press = flash == Some(card_line);
            // The same mark the plan steps use: a glyph both Windows console
            // fonts have, or it shows as a box on half the terminals.
            let mark = if row.on { "√" } else { "·" };
            let (mark_styled, name_styled) = if is_press {
                (format!("{TEXT_AMBER}{mark}{RESET}"), format!("{TEXT_AMBER}{}{RESET}", row.name))
            } else if row.on {
                (format!("{TEXT_GREEN}{mark}{RESET}"), format!("{TEXT_BRIGHT}{}{RESET}", row.name))
            } else if is_hover {
                (format!("{TEXT_OFF}{mark}{RESET}"), format!("{TEXT_MUTED}{}{RESET}", row.name))
            } else {
                (format!("{TEXT_OFF}{mark}{RESET}"), format!("{TEXT_OFF}{}{RESET}", row.name))
            };
            let pointer = if is_sel || is_press {
                format!("{TEXT_BRIGHT}▸{RESET}")
            } else if is_hover {
                format!("{TEXT_MUTED}·{RESET}")
            } else {
                " ".to_string()
            };
            // The description the model gets stays English; the person reads
            // it in their own language.
            let description = crate::i18n::tr(&row.description);
            let what = if row.available {
                description.to_string()
            } else {
                format!("{} ({})", description, crate::i18n::tr("not offered here"))
            };
            let what_styled = if row.available {
                format!("{TEXT_MUTED}{what}{RESET}")
            } else {
                format!("{TEXT_OFF}{what}{RESET}")
            };
            lines.push((LineKind::System, pad_row(&format!("{pointer} {mark_styled} {name_styled}  {what_styled}"))));
        }
        if self.rows.is_empty() {
            lines.push((LineKind::System, pad_row(&format!("{TEXT_MUTED}{}{RESET}", crate::i18n::tr("No tools are registered")))));
        }

        lines.push((LineKind::System, format!("  {BORDER_GOLD}├{}┤{RESET}", "─".repeat(box_w))));
        let hints: &[(&str, &str)] = &[("↑/↓", "move"), ("Space", "turn one on or off"), ("Esc", "close")];
        lines.push((LineKind::System, pad_row(&crate::key_hints(hints, inner_w.saturating_sub(1)))));
        lines.push((LineKind::System, format!("  {BORDER_GOLD}╰{}╯{RESET}", "─".repeat(box_w))));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<ToolRow> {
        vec![
            ToolRow { name: "read_file".into(), description: "Read a file".into(), on: true, available: true },
            ToolRow { name: "run_shell".into(), description: "Run a command".into(), on: true, available: true },
            ToolRow { name: "web_search".into(), description: "Search the web".into(), on: false, available: false },
        ]
    }

    #[test]
    fn space_turns_a_tool_on_or_off() {
        let mut v = ToolsView::new(rows());
        assert_eq!(v.handle_key(KeyCode::Char(' '), KeyModifiers::NONE), ToolsAction::Toggle("read_file".into(), false));
        // The web tool was off from the start.
        assert_eq!(v.disabled(), ["read_file", "web_search"]);
        v.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(v.handle_key(KeyCode::Char(' '), KeyModifiers::NONE), ToolsAction::Toggle("run_shell".into(), false));
        assert_eq!(v.disabled(), ["read_file", "run_shell", "web_search"]);
    }

    #[test]
    fn a_tool_that_is_not_offered_here_cannot_be_turned_on() {
        let mut v = ToolsView::new(rows());
        v.move_by(2);
        assert_eq!(v.current().map(|r| r.name.as_str()), Some("web_search"));
        assert_eq!(v.toggle(), ToolsAction::None, "there is nothing to offer");
        assert!(!v.rows[2].on);
    }

    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut v = ToolsView::new(rows());
        v.move_by(-5);
        assert_eq!(v.selected, 0);
        v.move_by(50);
        assert_eq!(v.selected, 2);
        assert_eq!(v.on_count(), 2);
    }

    #[test]
    fn the_list_is_drawn_with_what_is_on() {
        let v = ToolsView::new(rows());
        let screen: Vec<String> = v.render(80, None, None).into_iter().map(|(_, l)| crate::strip_ansi(&l)).collect();
        assert!(screen.iter().any(|l| l.contains("read_file")), "{screen:#?}");
        assert!(screen.iter().any(|l| l.contains("2 of 3 on")), "{screen:#?}");
        assert!(screen.iter().any(|l| l.contains("not offered here")), "{screen:#?}");
    }
}

#[cfg(test)]
mod mouse_tests {
    use super::*;

    fn view() -> ToolsView {
        ToolsView::new(vec![
            ToolRow { name: "read_file".into(), description: "reads".into(), on: true, available: true },
            ToolRow { name: "run_shell".into(), description: "runs".into(), on: false, available: true },
        ])
    }

    #[test]
    fn a_click_selects_a_tool_and_the_selected_one_toggles() {
        let mut v = view();
        assert_eq!(v.click(3), ToolsAction::None, "line 3 is the second row: the first click only focuses");
        assert_eq!(v.selected, 1);
        assert_eq!(v.click(3), ToolsAction::Toggle("run_shell".into(), true), "the focused row toggles, as Space would");
        assert!(v.rows[1].on);
        assert_eq!(v.click(0), ToolsAction::None, "the title is not a row");
    }

    #[test]
    fn the_wheel_walks_the_rows_and_stops_at_the_ends() {
        let mut v = view();
        assert!(v.wheel(1));
        assert_eq!(v.selected, 1);
        assert!(!v.wheel(1), "already at the last row: no move, no reaction");
        assert_eq!(v.selected, 1, "no wrap");
        assert!(v.wheel(-1));
        assert_eq!(v.selected, 0);
        assert!(!v.wheel(-1), "the first row holds: no move, no reaction");
        assert_eq!(v.selected, 0);
    }
}
