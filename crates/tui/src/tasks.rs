//! Background commands from the app's side: their notices reach the model
//! (into the running turn, or as a follow-up turn when idle), Ctrl+B moves a
//! running command to the background, `/tasks` lists them, and quitting says
//! how many it will stop.

use super::*;

use flashagent_tools::{TaskNotice, TaskState};
use flashagent_tui::{TasksAction, TasksModal};

/// The transcript's line for a task that ended.
pub(crate) fn task_line(notice: &TaskNotice) -> String {
    let colour = match notice.state {
        TaskState::Exited(Some(0)) => "145;205;140",
        TaskState::Killed => "135;130;125",
        _ => "225;115;105",
    };
    let command = flashagent_tui::truncate_middle(&notice.command.replace(['\n', '\r'], " "), 60);
    format!(
        "  \x1b[38;2;{colour}m\u{25cf}\x1b[0m \x1b[38;2;200;195;185mBackground task {} {} after {}\x1b[0m \x1b[38;2;135;130;125m\u{b7} {}\x1b[0m",
        notice.id,
        notice.state.describe(),
        flashagent_tools::shell::format_elapsed(notice.elapsed),
        flashagent_tui::terminal_safe(&command)
    )
}

/// The same line for each notice in a saved message, from the text the model read.
pub(crate) fn notice_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.strip_prefix(flashagent_core::TASK_NOTICE_OPENING))
        .filter_map(|rest| rest.split_once(". This is").map(|(said, _)| said.to_string()))
        .map(|said| format!("  \x1b[38;2;135;130;125m\u{25cf}\x1b[0m \x1b[38;2;200;195;185mBackground task {}\x1b[0m", flashagent_tui::terminal_safe(&said)))
        .collect()
}

impl App {
    pub(crate) fn on_task_ended(&mut self, cx: &LoopCtx<'_>, notice: TaskNotice) {
        let line = task_line(&notice);
        if self.running {
            // A line now would split the answer being written; it goes in when the
            // loop takes the notice, or when the turn ends.
            self.task_lines.push(line);
            if !notice.killed() {
                self.background = Some(BackgroundNotice::fading(
                    format!("Background task {} {} \u{b7} the agent will be told", notice.id, notice.state.describe()),
                    6,
                ));
            }
        } else {
            self.chat.push_system(&line);
        }
        // Stopped on purpose: nothing for the model to react to.
        if !notice.killed() {
            self.task_inbox.push(notice.message());
        }
        self.refresh_tasks_overlay(cx);
        self.deliver_task_notices(cx);
        self.renderer.request_reprint();
    }

    pub(crate) fn flush_task_lines(&mut self) {
        for line in std::mem::take(&mut self.task_lines) {
            self.chat.push_system(&line);
        }
    }

    /// Into the running turn through its steering channel, or, with the app
    /// idle and nobody in the middle of something, as one follow-up turn.
    pub(crate) fn deliver_task_notices(&mut self, cx: &LoopCtx<'_>) {
        if self.running {
            // None while the turn is being stopped: the notices wait.
            if let Some(tx) = self.active_steer_tx.clone() {
                for notice in self.task_inbox.send_into_turn() {
                    let _ = tx.send(notice);
                }
            }
            return;
        }
        // A draft waits to be sent and carries the notices with it; an emptied
        // prompt lets them go. Only a view that only shows things stays open
        // under a turn.
        let busy = cx.gate.pending().is_some()
            || cx.question_gate.pending().is_some()
            || !self.input.trim().is_empty()
            || !self.attachments.is_empty()
            || self.history_search.is_some()
            || self.provider_switch.is_some()
            || self.pending_resume.is_some()
            || self.uninstall_confirm
            || self.channel_switch.is_some()
            || self.overlay.as_ref().is_some_and(|o| !matches!(o, Overlay::Tasks(_) | Overlay::Context(_)));
        let Some(message) = self.task_inbox.wake(busy) else { return };
        self.flush_task_lines();
        self.custom_placeholder = None;
        self.suggested_prompt = None;
        self.history.push(ChatMessage::user(message));
        self.renderer.scroll_to_bottom();
        self.start_turn(cx, GoalBudgets::steps_only(self.max_steps));
    }

    /// Ctrl+B.
    pub(crate) fn detach_shell(&mut self, cx: &LoopCtx<'_>) {
        self.background = Some(if cx.tools_arc.shells().detach_foreground() {
            BackgroundNotice::fading("Moved to the background \u{b7} /tasks shows it; the agent is told when it ends", 6)
        } else {
            BackgroundNotice::fading("Ctrl+B moves a running shell command to the background; none is running", 4)
        });
        self.renderer.request_reprint();
    }

    pub(crate) fn open_tasks(&mut self, cx: &LoopCtx<'_>) {
        self.open_overlay(Overlay::Tasks(TasksModal::new(cx.tools_arc.shells().tasks())));
    }

    pub(crate) fn tasks_key(&mut self, cx: &LoopCtx<'_>, mut modal: TasksModal, code: KeyCode, mods: KeyModifiers) {
        let shells = cx.tools_arc.shells();
        match modal.handle_key(code, mods) {
            TasksAction::Close => return,
            TasksAction::Open(id) => modal.detail = Some((id, shells.output(id).unwrap_or_default())),
            TasksAction::Kill(id) => {
                if shells.stop(id) {
                    self.background = Some(BackgroundNotice::fading(format!("Stopping background task {id}"), 4));
                }
            }
            TasksAction::None => {}
        }
        let output = modal.detail.as_ref().and_then(|(id, _)| shells.output(*id));
        modal.refresh(shells.tasks(), output);
        self.overlay = Some(Overlay::Tasks(modal));
    }

    pub(crate) fn refresh_tasks_overlay(&mut self, cx: &LoopCtx<'_>) {
        if let Some(Overlay::Tasks(modal)) = self.overlay.as_mut() {
            let shells = cx.tools_arc.shells();
            let output = modal.detail.as_ref().and_then(|(id, _)| shells.output(*id));
            modal.refresh(shells.tasks(), output);
        }
    }

    /// Notices held back (a draft, a question) go once nothing holds them, and
    /// the task list keeps its clocks moving.
    pub(crate) fn tasks_tick(&mut self, cx: &LoopCtx<'_>) {
        if self.task_inbox.pending() > 0 {
            self.deliver_task_notices(cx);
        }
        if self.tasks_refreshed.elapsed() >= std::time::Duration::from_millis(500) {
            self.tasks_refreshed = std::time::Instant::now();
            self.refresh_tasks_overlay(cx);
        }
    }

    /// Quitting stops every background task. With some running, the first try
    /// says how many, and another within a few seconds quits.
    pub(crate) fn quit_confirmed(&mut self, cx: &LoopCtx<'_>) -> bool {
        let running = cx.tools_arc.shells().running_count();
        let armed = self.quit_armed.take().is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(4));
        if running == 0 || armed || UNINSTALL_AFTER_EXIT.load(Ordering::SeqCst) {
            return true;
        }
        self.quit_armed = Some(std::time::Instant::now());
        self.background = Some(
            BackgroundNotice::fading(
                format!(
                    "{} running \u{b7} quitting stops {}; do it again to quit",
                    flashagent_tui::plural(running, "background task", "background tasks"),
                    if running == 1 { "it" } else { "them" }
                ),
                4,
            )
            .warning(),
        );
        self.renderer.request_reprint();
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_notice_is_shown_as_its_own_line() {
        let notice = TaskNotice {
            id: 4,
            command: "cargo build".into(),
            state: TaskState::Exited(Some(0)),
            elapsed: std::time::Duration::from_secs(3),
            tail: "Finished".into(),
        };
        let lines = notice_lines(&format!("{}\n\n{}", notice.message(), TaskNotice { id: 5, ..notice.clone() }.message()));
        assert_eq!(lines.len(), 2, "{lines:?}");
        let first = flashagent_tui::strip_ansi(&lines[0]);
        assert!(first.contains("Background task 4 exited with code 0 after 3s"), "{first}");
        let live = flashagent_tui::strip_ansi(&task_line(&notice));
        assert!(live.contains("Background task 4 exited with code 0 after 3s") && live.contains("cargo build"), "{live}");
    }
}
