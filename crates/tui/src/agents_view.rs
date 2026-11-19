//! The `/agents` panel and the detail a click on a pinned subagent opens.
//!
//! It is the child's own chat, not a log of counters: the same transcript the
//! main screen shows, with its thinking, tool calls and final report. A list of
//! the session's children sits at the top; the selected one fills the panel.

use std::cell::Cell;

use crate::agents::{AgentRow, AgentTree};
use crate::i18n::{tr, tr_fill};
use crate::{LineKind, RenderLine};

/// What a press on the panel reached: a row of the list picks that child, a
/// word on the hint line acts like its key does. Nothing else on the panel is
/// a button; the transcript is for reading and selecting text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsClick {
    None,
    /// The child at this index of `rows`.
    Select(usize),
    Message,
    Stop,
    Copy,
    Report,
}

pub struct AgentsView {
    pub rows: Vec<AgentRow>,
    pub selected: usize,
    /// Lines scrolled up from the newest end.
    pub scroll: usize,
    /// The message being typed for the selected child, when `m` opened it.
    pub message: Option<String>,
    transcript: Vec<RenderLine>,
    /// The list's first shown row and its length, and the hint line's index:
    /// what a click needs to mean what the screen shows. Set by `render`.
    list_start: Cell<usize>,
    list_len: Cell<usize>,
    hint_line: Cell<usize>,
}

impl AgentsView {
    pub fn new(tree: &AgentTree, width: usize) -> Self {
        let rows = tree.rows();
        let selected = rows.iter().position(|r| r.done.is_none()).unwrap_or(0);
        let mut view = Self {
            rows,
            selected,
            scroll: 0,
            message: None,
            transcript: Vec::new(),
            list_start: Cell::new(0),
            list_len: Cell::new(0),
            hint_line: Cell::new(0),
        };
        view.refresh(tree, width);
        view
    }

    pub fn select(&mut self, id: &str) {
        if let Some(at) = self.rows.iter().position(|r| r.id == id) {
            self.selected = at;
            self.scroll = 0;
            self.message = None;
        }
    }

    pub fn transcript_len(&self) -> usize {
        self.transcript.len()
    }

    pub fn current(&self) -> Option<&AgentRow> {
        self.rows.get(self.selected)
    }

    pub fn move_by(&mut self, delta: isize, tree: &AgentTree, width: usize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last) as usize;
        if next != self.selected {
            self.selected = next;
            self.scroll = 0;
            self.message = None;
            self.refresh(tree, width);
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let total = self.transcript.len();
        let next = (self.scroll as isize + delta).clamp(0, total as isize) as usize;
        self.scroll = next;
    }

    /// A press on the panel. Line 0 is the heading; the list follows it, and
    /// the hint line's verbs answer as their keys do. Typing a message stays
    /// with the keyboard: a click there has no cell to add.
    pub fn click(&mut self, line: usize, column: usize, tree: &AgentTree, width: usize) -> AgentsClick {
        if self.message.is_some() {
            return AgentsClick::None;
        }
        let list_len = self.list_len.get();
        if list_len > 0 && line >= 1 && line <= list_len {
            let at = self.list_start.get() + (line - 1);
            if at < self.rows.len() {
                if at == self.selected {
                    return AgentsClick::None;
                }
                self.selected = at;
                self.scroll = 0;
                self.message = None;
                self.refresh(tree, width);
                return AgentsClick::Select(at);
            }
            return AgentsClick::None;
        }
        if line == self.hint_line.get() {
            // The verbs sit where the hint line drew them: the same pairs, the
            // same fitting, so a dropped verb is no button either. The line
            // keeps one cell of margin, which the zones are shifted past.
            let pairs = [
                ("↑/↓", "agent"),
                ("PgUp/PgDn", "scroll"),
                ("m", "message"),
                ("x", "stop"),
                ("c", "copy"),
                ("o", "report"),
                ("Esc", "close"),
            ];
            let acts = [AgentsClick::None, AgentsClick::None, AgentsClick::Message, AgentsClick::Stop, AgentsClick::Copy, AgentsClick::Report, AgentsClick::None];
            let zones = crate::welcome::key_hint_zones(&pairs, width.saturating_sub(2).max(20));
            for (at, zone) in zones.iter().enumerate() {
                if let Some((from, to)) = zone {
                    if column > *from && column <= *to {
                        return acts[at];
                    }
                }
            }
        }
        AgentsClick::None
    }

    /// The live state moved: rows and the selected transcript are read again.
    /// The selected id survives a row that disappeared.
    pub fn refresh(&mut self, tree: &AgentTree, width: usize) {
        let selected_id = self.current().map(|r| r.id.clone());
        self.rows = tree.rows();
        if let Some(id) = selected_id {
            if let Some(at) = self.rows.iter().position(|r| r.id == id) {
                self.selected = at;
            }
        }
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        self.transcript = self
            .current()
            .and_then(|row| tree.chat(&row.id))
            .map(|chat| chat.render(width.saturating_sub(4).max(20)))
            .unwrap_or_default();
        let total = self.transcript.len();
        if self.scroll > total {
            self.scroll = total;
        }
    }

    pub fn render(&self, width: usize, height: usize) -> Vec<RenderLine> {
        let border = "\x1b[38;2;95;90;85m";
        let reset = "\x1b[0m";
        let dim = "\x1b[38;2;135;130;125m";
        let text = "\x1b[38;2;232;227;218m";
        let accent = "\x1b[1;38;2;225;175;95m";
        let live = "\x1b[1;38;2;138;180;248m";
        let inner = width.saturating_sub(2).max(20);
        let mut lines: Vec<RenderLine> = Vec::new();
        let running = self.rows.iter().filter(|r| r.done.is_none()).count();
        let title = format!(
            " {} · {} ",
            tr("Agents"),
            tr_fill("{running} running, {total} total", &[("running", &running.to_string()), ("total", &self.rows.len().to_string())])
        );
        let heading = format!("{accent}{title}{reset}");
        let rule = inner.saturating_sub(crate::visible_width(&heading));
        lines.push((LineKind::System, format!("{border}─{heading}{border}{}─{reset}", "─".repeat(rule))));

        if self.rows.is_empty() {
            // No list and no hint: the cells a click could reach must not
            // outlive the render that drew them.
            self.list_len.set(0);
            self.hint_line.set(0);
            lines.push((LineKind::System, format!(" {dim}{}{reset}", tr("No subagents in this session."))));
            return lines;
        }

        let list_room = (height / 3).clamp(3, 8).min(self.rows.len());
        let start = self.selected.saturating_sub(list_room.saturating_sub(1));
        self.list_start.set(start);
        self.list_len.set(list_room);
        for (at, row) in self.rows.iter().enumerate().skip(start).take(list_room) {
            let here = at == self.selected;
            let mark = if here { format!("{accent}›{reset}") } else { " ".to_string() };
            let state = if row.done.is_none() {
                format!("{live}{}{reset}", tr("running"))
            } else if row.review.is_some() {
                format!("{dim}{} · {}{reset}", tr("answered"), tr("reviewed"))
            } else {
                format!("{dim}{}{reset}", tr("answered"))
            };
            let name = if here { format!("{text}{} {} {reset}", row.role, row.id) } else { format!("{dim}{} {}{reset}", row.role, row.id) };
            let mut counts = Vec::new();
            if row.usage.steps > 0 {
                counts.push(crate::text::plural(row.usage.steps as usize, "step", "steps"));
            }
            if row.usage.total_tokens() > 0 {
                counts.push(crate::text::plural(row.usage.total_tokens() as usize, "token", "tokens"));
            }
            let counts = counts.join(" · ");
            lines.push((
                LineKind::System,
                format!(" {mark} {name} {state}{}{}", if counts.is_empty() { String::new() } else { format!(" {dim}· {counts}{reset}") }, ""),
            ));
        }

        lines.push((LineKind::System, format!("{border}{}{reset}", "─".repeat(inner))));
        let footer = self.message.as_ref().map_or(2, |_| 3);
        let room = height.saturating_sub(lines.len() + footer).max(1);
        let total = self.transcript.len();
        let end = total.saturating_sub(self.scroll);
        let begin = end.saturating_sub(room);
        if total == 0 {
            lines.push((LineKind::System, format!(" {dim}{}{reset}", tr("No steps yet."))));
        } else {
            for (_, row) in self.transcript.iter().skip(begin).take(end - begin) {
                lines.push((LineKind::System, format!(" {row}{reset}")));
            }
        }
        if begin > 0 {
            lines.push((LineKind::System, format!(" {dim}{}{reset}", tr_fill("↑ {n} earlier lines", &[("n", &begin.to_string())]))));
        }
        self.hint_line.set(lines.len());
        lines.push((
            LineKind::System,
            format!(
                " {dim}{}{reset}",
                crate::key_hints(
                    &[("↑/↓", "agent"), ("PgUp/PgDn", "scroll"), ("m", "message"), ("x", "stop"), ("c", "copy"), ("o", "report"), ("Esc", "close")],
                    inner
                )
            ),
        ));
        if let Some(message) = &self.message {
            lines.push((
                LineKind::System,
                format!(" {accent}›{reset} {text}{message}\x1b[7m \x1b[27m{reset}"),
            ));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_core::loop_::LoopEvent;
    use flashagent_core::{DoneReason, SubagentEvent};

    fn ev(id: &str, role: &str, event: LoopEvent) -> SubagentEvent {
        SubagentEvent { id: id.into(), role: role.into(), event }
    }

    fn tree_with_a_finished_child() -> AgentTree {
        let mut tree = AgentTree::default();
        tree.apply(&ev("sub1", "researcher", LoopEvent::StepStarted { step: 1, max_steps: Some(8) }));
        tree.apply(&ev("sub1", "researcher", LoopEvent::ToolStarted { id: "a".into(), name: "read".into(), args_json: "{\"path\":\"src/main.rs\"}".into() }));
        tree.apply(&ev("sub1", "researcher", LoopEvent::ToolFinished { id: "a".into(), is_error: false, result: Some("fn main() {}".into()), result_len: 12 }));
        tree.apply(&ev("sub1", "researcher", LoopEvent::TurnDelta("Found it in src/main.rs.".into())));
        tree.apply(&ev("sub1", "researcher", LoopEvent::Done(DoneReason::Completed)));
        tree.record_answer("sub1", "The bug is in src/main.rs:12.");
        tree
    }

    #[test]
    fn the_panel_shows_the_child_as_a_chat_not_only_counters() {
        let tree = tree_with_a_finished_child();
        let view = AgentsView::new(&tree, 100);
        let dump: String = view.render(100, 40).into_iter().map(|(_, line)| crate::strip_ansi(&line)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("researcher sub1"), "{dump}");
        assert!(dump.contains("Read") && dump.contains("src/main.rs"), "the tool call is missing:\n{dump}");
        assert!(dump.contains("The bug is in src/main.rs:12."), "the report is missing:\n{dump}");
        assert!(dump.contains("1 running, 1 total") || dump.contains("0 running, 1 total"), "{dump}");
    }

    #[test]
    fn a_finished_child_stays_in_the_panel_and_a_second_one_can_be_selected() {
        let mut tree = tree_with_a_finished_child();
        tree.apply(&ev("sub2", "coder", LoopEvent::TurnDelta("working".into())));
        let mut view = AgentsView::new(&tree, 100);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.current().map(|r| r.id.as_str()), Some("sub2"), "the running child starts selected");
        view.move_by(-1, &tree, 100);
        assert_eq!(view.current().map(|r| r.id.as_str()), Some("sub1"));
        assert!(view.current().and_then(|r| r.answer.as_deref()).is_some(), "a finished child keeps its report");
    }

    #[test]
    fn the_panel_does_not_grow_without_bound_while_a_child_works() {
        let mut tree = AgentTree::default();
        for i in 0..600 {
            tree.apply(&ev("sub1", "coder", LoopEvent::ToolStarted { id: format!("t{i}"), name: "read".into(), args_json: "{\"path\":\"a\"}".into() }));
            tree.apply(&ev("sub1", "coder", LoopEvent::ToolFinished { id: format!("t{i}"), is_error: false, result: Some("ok".into()), result_len: 2 }));
        }
        let view = AgentsView::new(&tree, 100);
        assert!(view.transcript_len() <= 400, "the child's transcript is unbounded: {}", view.transcript_len());
    }

    #[test]
    fn a_click_picks_a_row_and_a_hint_word_acts_like_its_key() {
        let mut tree = tree_with_a_finished_child();
        tree.apply(&ev("sub2", "coder", LoopEvent::TurnDelta("working".into())));
        let mut view = AgentsView::new(&tree, 100);
        assert_eq!(view.current().map(|r| r.id.as_str()), Some("sub2"));
        view.render(100, 40);
        // The list starts at line 1: a press on its first row picks sub1.
        assert_eq!(view.click(1, 0, &tree, 100), AgentsClick::Select(0));
        assert_eq!(view.current().map(|r| r.id.as_str()), Some("sub1"));
        // A hint word answers where the line drew it: find "stop" on the hint.
        view.render(100, 40);
        let hint_line = view.hint_line.get();
        let pairs = [("↑/↓", "agent"), ("PgUp/PgDn", "scroll"), ("m", "message"), ("x", "stop"), ("c", "copy"), ("o", "report"), ("Esc", "close")];
        let zones = crate::welcome::key_hint_zones(&pairs, 98);
        let (from, _) = zones[3].expect("stop is drawn on a wide panel");
        assert_eq!(view.click(hint_line, from + 1, &tree, 100), AgentsClick::Stop);
        assert_eq!(view.click(hint_line, from, &tree, 100), AgentsClick::None, "the margin cell is not the word");
        // A press on the transcript below the list is reading, not a button.
        assert_eq!(view.click(hint_line + 1, 3, &tree, 100), AgentsClick::None);
    }
}
