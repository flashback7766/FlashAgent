//! `/tasks`: the background commands of this session, what they print, and
//! a key to stop one.

use crossterm::event::{KeyCode, KeyModifiers};
use flashagent_tools::shell::format_elapsed;
use flashagent_tools::{TaskInfo, TaskState};

use crate::{visible_width, LineKind, RenderLine};

pub struct TasksModal {
    pub tasks: Vec<TaskInfo>,
    selected: usize,
    /// Open on one task: its id and the end of its output.
    pub detail: Option<(u32, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TasksAction {
    None,
    Close,
    /// Wants the output of this task in `detail`.
    Open(u32),
    Kill(u32),
}

const OUTPUT_ROWS: usize = 14;

/// A state in a few letters, for the list.
fn short_state(state: &TaskState) -> String {
    match state {
        TaskState::Running => "running".into(),
        TaskState::Exited(Some(code)) => format!("exit {code}"),
        TaskState::Exited(None) => "signal".into(),
        TaskState::Killed => "stopped".into(),
    }
}

/// Output and commands come from programs: no escape of theirs reaches the screen.
fn plain(text: &str) -> String {
    crate::terminal_safe(&crate::strip_ansi(text))
}

impl TasksModal {
    /// On the newest task still running, else the newest.
    pub fn new(tasks: Vec<TaskInfo>) -> Self {
        let selected = tasks.iter().rposition(|t| t.state == TaskState::Running).unwrap_or(tasks.len().saturating_sub(1));
        Self { tasks, selected, detail: None }
    }

    /// Keeps the selection on the same task.
    pub fn refresh(&mut self, tasks: Vec<TaskInfo>, output: Option<String>) {
        let id = self.selected_id();
        self.tasks = tasks;
        if let Some(pos) = id.and_then(|id| self.tasks.iter().position(|t| t.id == id)) {
            self.selected = pos;
        }
        self.selected = self.selected.min(self.tasks.len().saturating_sub(1));
        if let (Some((_, shown)), Some(output)) = (self.detail.as_mut(), output) {
            *shown = output;
        }
    }

    pub fn selected_id(&self) -> Option<u32> {
        self.tasks.get(self.selected).map(|t| t.id)
    }

    fn selected_running(&self) -> Option<u32> {
        self.tasks.get(self.selected).filter(|t| t.state == TaskState::Running).map(|t| t.id)
    }

    pub fn handle_key(&mut self, code: KeyCode, _mods: KeyModifiers) -> TasksAction {
        let kill = matches!(code, KeyCode::Char('k' | 'K' | '\u{043b}' | '\u{041b}') | KeyCode::Delete);
        if self.detail.is_some() {
            return match code {
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Enter => {
                    self.detail = None;
                    TasksAction::None
                }
                _ if kill => self.selected_running().map_or(TasksAction::None, TasksAction::Kill),
                KeyCode::Char('q' | 'Q') => TasksAction::Close,
                _ => TasksAction::None,
            };
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => TasksAction::Close,
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                TasksAction::None
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.tasks.len().saturating_sub(1));
                TasksAction::None
            }
            KeyCode::Enter | KeyCode::Right => self.selected_id().map_or(TasksAction::None, TasksAction::Open),
            _ if kill => self.selected_running().map_or(TasksAction::None, TasksAction::Kill),
            _ => TasksAction::None,
        }
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let border = "\x1b[38;2;100;95;90m";
        let dim = "\x1b[38;2;135;130;125m";
        let reset = "\x1b[0m";
        let inner_w = width.saturating_sub(6).clamp(20, 110);
        let row = |content: &str| -> RenderLine {
            let clipped = crate::tool_views::clip_ellipsis(content, inner_w.saturating_sub(2));
            let pad = " ".repeat(inner_w.saturating_sub(visible_width(&clipped) + 2));
            (LineKind::System, format!("  {border}\u{2502}{reset} {clipped}{pad} {border}\u{2502}{reset}"))
        };
        let title = " Background tasks ";
        let mut lines = vec![(
            LineKind::System,
            format!(
                "  {border}\u{256d}\u{2500}\x1b[1;38;2;225;175;95m{title}{border}{}\u{256e}{reset}",
                "\u{2500}".repeat(inner_w.saturating_sub(title.len() + 1))
            ),
        )];
        let state_colour = |state: &TaskState| match state {
            TaskState::Running => "\x1b[38;2;145;205;140m",
            TaskState::Exited(Some(0)) => "\x1b[38;2;160;155;145m",
            _ => "\x1b[38;2;225;115;105m",
        };
        let hints: Vec<(&str, &str)> = match self.detail.as_ref().and_then(|(id, out)| Some((self.tasks.iter().find(|t| t.id == *id)?, out))) {
            Some((task, output)) => {
                lines.push(row(&format!(
                    "\x1b[1;38;2;240;235;225m#{}\x1b[0m  {}{}{reset}  {dim}{}{reset}  {}",
                    task.id,
                    state_colour(&task.state),
                    task.state.describe(),
                    format_elapsed(task.elapsed),
                    plain(&task.command)
                )));
                lines.push(row(""));
                let out: Vec<&str> = output.lines().collect();
                if out.iter().all(|l| l.trim().is_empty()) {
                    lines.push(row(&format!("{dim}(no output yet){reset}")));
                }
                for line in &out[out.len().saturating_sub(OUTPUT_ROWS)..] {
                    lines.push(row(&plain(line)));
                }
                if task.state == TaskState::Running { vec![("k", "stop it"), ("Esc", "back")] } else { vec![("Esc", "back")] }
            }
            None if self.tasks.is_empty() => {
                lines.push(row("No background tasks yet."));
                lines.push(row(&format!("{dim}The agent starts them for servers and long builds; Ctrl+B moves a running command here.{reset}")));
                vec![("Esc", "close")]
            }
            None => {
                for (i, task) in self.tasks.iter().enumerate() {
                    let marker = if i == self.selected { "\x1b[1;38;2;225;175;95m\u{203a}\x1b[0m" } else { " " };
                    let last = if task.last_line.is_empty() { String::new() } else { format!("  {dim}\u{b7} {}{reset}", plain(&task.last_line)) };
                    lines.push(row(&format!(
                        "{marker} #{:<3} {}{:<8}{reset} {dim}{:>7}{reset}  {}{last}",
                        task.id,
                        state_colour(&task.state),
                        short_state(&task.state),
                        format_elapsed(task.elapsed),
                        plain(&task.command)
                    )));
                }
                if self.selected_running().is_some() {
                    vec![("\u{2191}/\u{2193}", "choose"), ("Enter", "output"), ("k", "stop it"), ("Esc", "close")]
                } else {
                    vec![("\u{2191}/\u{2193}", "choose"), ("Enter", "output"), ("Esc", "close")]
                }
            }
        };
        lines.push(row(""));
        lines.push(row(&crate::key_hints(&hints, inner_w.saturating_sub(2))));
        lines.push((LineKind::System, format!("  {border}\u{2570}{}\u{256f}{reset}", "\u{2500}".repeat(inner_w))));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn task(id: u32, state: TaskState, command: &str, last: &str) -> TaskInfo {
        TaskInfo { id, command: command.into(), state, elapsed: Duration::from_secs(65), last_line: last.into(), detached: false }
    }

    fn text(lines: &[RenderLine]) -> String {
        crate::strip_ansi(&lines.iter().map(|l| l.1.clone()).collect::<Vec<_>>().join("\n"))
    }

    #[test]
    fn the_list_shows_each_task_and_stops_only_a_running_one() {
        let mut modal = TasksModal::new(vec![
            task(1, TaskState::Exited(Some(0)), "cargo build", "Finished"),
            task(2, TaskState::Running, "npm run dev", "listening on :3000"),
        ]);
        let shown = text(&modal.render(100));
        assert!(shown.contains("#1") && shown.contains("exit 0") && shown.contains("cargo build"), "{shown}");
        assert!(shown.contains("running") && shown.contains("1m 05s") && shown.contains("listening on :3000"), "{shown}");
        assert_eq!(modal.selected_id(), Some(2), "opens on the running task");
        assert_eq!(modal.handle_key(KeyCode::Char('k'), KeyModifiers::NONE), TasksAction::Kill(2));
        modal.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(modal.handle_key(KeyCode::Char('k'), KeyModifiers::NONE), TasksAction::None, "stopped a task that ended");
        assert_eq!(modal.handle_key(KeyCode::Enter, KeyModifiers::NONE), TasksAction::Open(1));
        assert_eq!(modal.handle_key(KeyCode::Esc, KeyModifiers::NONE), TasksAction::Close);
    }

    #[test]
    fn a_task_opens_on_its_output_and_esc_goes_back() {
        let mut modal = TasksModal::new(vec![task(3, TaskState::Running, "tail -f log", "")]);
        modal.detail = Some((3, "one\n\x1b[2Jtwo\n".into()));
        let shown = text(&modal.render(80));
        assert!(shown.contains("one") && shown.contains("two") && shown.contains("running"), "{shown}");
        assert!(!modal.render(80).iter().any(|l| l.1.contains("\x1b[2J")), "an escape from the output reached the screen");
        assert_eq!(modal.handle_key(KeyCode::Esc, KeyModifiers::NONE), TasksAction::None);
        assert!(modal.detail.is_none());
    }

    #[test]
    fn no_row_is_wider_than_the_window() {
        let long = "x".repeat(300);
        let mut modal = TasksModal::new(vec![task(1, TaskState::Running, &long, &long), task(2, TaskState::Killed, "sleep 9", "")]);
        for width in [30usize, 60, 80, 140] {
            for (_, row) in modal.render(width) {
                assert!(visible_width(&row) <= width, "{width}: {row}");
            }
            modal.detail = Some((1, format!("{long}\n{long}")));
            for (_, row) in modal.render(width) {
                assert!(visible_width(&row) <= width, "{width}: {row}");
            }
            modal.detail = None;
        }
        assert!(text(&TasksModal::new(Vec::new()).render(80)).contains("No background tasks"));
    }
}
