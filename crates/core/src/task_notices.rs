//! Notices that a background command ended, on their way to the model. The
//! model learns it without polling: into the turn that is running, through
//! its steering channel, or as one follow-up turn when the app is idle. Each
//! notice reaches the history exactly once.

/// How a notice opens; with [`TASK_NOTICE_NOTE`] it is told apart from
/// something the user said.
pub const TASK_NOTICE_OPENING: &str = "[Background task ";
pub const TASK_NOTICE_NOTE: &str = "This is an automatic notice from run_shell, not a message from the user.]";

/// A user-role message that carries a notice, not a prompt.
pub fn is_task_notice(text: &str) -> bool {
    text.starts_with(TASK_NOTICE_OPENING) && text.contains(TASK_NOTICE_NOTE)
}

#[derive(Debug, Default)]
pub struct NoticeInbox {
    queued: Vec<String>,
    /// Down a running turn's steering channel, not yet injected by the loop.
    sent: Vec<String>,
    /// Injected into the running turn: a turn thrown away takes them with it.
    injected: Vec<String>,
    /// The user stopped the last turn: notices wait for them to send something.
    held: bool,
}

impl NoticeInbox {
    pub fn push(&mut self, notice: String) {
        self.queued.push(notice);
    }

    /// Waiting for a turn, or sent into one and not injected yet.
    pub fn pending(&self) -> usize {
        self.queued.len() + self.sent.len()
    }

    /// For the running turn's steering channel. Each stays marked as sent
    /// until the loop reports it injected.
    pub fn send_into_turn(&mut self) -> Vec<String> {
        let out = std::mem::take(&mut self.queued);
        self.sent.extend(out.iter().cloned());
        out
    }

    /// The loop injected `text`. False when it is not one of ours (the user
    /// steering).
    pub fn injected(&mut self, text: &str) -> bool {
        match self.sent.iter().position(|s| s == text) {
            Some(pos) => {
                self.injected.push(self.sent.remove(pos));
                true
            }
            None => false,
        }
    }

    /// What the turn never injected goes back to the front of the queue. A
    /// turn the user stopped holds the queue until they send something: they
    /// asked for quiet.
    pub fn turn_ended(&mut self, stopped_by_user: bool) {
        self.injected.clear();
        let mut back = std::mem::take(&mut self.sent);
        back.append(&mut self.queued);
        self.queued = back;
        if stopped_by_user {
            self.held = true;
        }
    }

    /// The turn and its history were thrown away (it would not stop when
    /// asked): what it injected never reached the kept history either.
    pub fn turn_aborted(&mut self) {
        let mut back = std::mem::take(&mut self.injected);
        back.append(&mut self.sent);
        back.append(&mut self.queued);
        self.queued = back;
        self.held = true;
    }

    /// Any turn starting ends a hold; what is queued rides along with it.
    pub fn turn_started(&mut self) {
        self.held = false;
        self.injected.clear();
    }

    /// With the app idle: every queued notice as the message of one follow-up
    /// turn. `None` while held, while `busy` (the user is typing or answering
    /// something), or with nothing queued.
    pub fn wake(&mut self, busy: bool) -> Option<String> {
        if self.held || busy || self.queued.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.queued).join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(id: u32) -> String {
        format!("{TASK_NOTICE_OPENING}{id} exited with code 0 after 1s. {TASK_NOTICE_NOTE}\ncommand: true")
    }

    #[test]
    fn a_notice_is_told_apart_from_what_the_user_says() {
        assert!(is_task_notice(&notice(3)));
        assert!(!is_task_notice("[Background task 3 please look]"));
        assert!(!is_task_notice("hello"));
    }

    #[test]
    fn an_idle_notice_wakes_exactly_one_follow_up() {
        let mut inbox = NoticeInbox::default();
        inbox.push(notice(1));
        inbox.push(notice(2));
        let wake = inbox.wake(false).expect("a follow-up turn");
        assert!(wake.contains("task 1") && wake.contains("task 2"), "{wake}");
        inbox.turn_started();
        assert_eq!(inbox.wake(false), None, "a second follow-up for the same notices");
        assert_eq!(inbox.pending(), 0);
    }

    #[test]
    fn a_notice_waits_while_the_user_is_busy() {
        let mut inbox = NoticeInbox::default();
        inbox.push(notice(1));
        assert_eq!(inbox.wake(true), None);
        assert!(inbox.wake(false).is_some());
    }

    #[test]
    fn a_notice_sent_into_a_turn_is_delivered_once_or_comes_back() {
        let mut inbox = NoticeInbox::default();
        inbox.push(notice(1));
        inbox.push(notice(2));
        let sent = inbox.send_into_turn();
        assert_eq!(sent.len(), 2);
        assert!(inbox.send_into_turn().is_empty(), "sent twice");
        assert!(inbox.injected(&sent[0]));
        assert!(!inbox.injected(&sent[0]), "counted twice");
        assert!(!inbox.injected("use postgres"), "the user's steer is not a notice");
        // The turn ended before the second reached the model.
        inbox.turn_ended(false);
        assert_eq!(inbox.wake(false), Some(sent[1].clone()));
    }

    #[test]
    fn after_the_user_stops_a_turn_notices_wait_for_their_next_message() {
        let mut inbox = NoticeInbox::default();
        inbox.push(notice(1));
        inbox.send_into_turn();
        inbox.turn_ended(true);
        assert_eq!(inbox.wake(false), None, "woke up after Esc");
        inbox.turn_started();
        assert_eq!(inbox.send_into_turn(), vec![notice(1)], "it rides along with the next prompt");
    }

    #[test]
    fn a_turn_thrown_away_gives_back_what_it_was_told() {
        let mut inbox = NoticeInbox::default();
        inbox.push(notice(1));
        inbox.push(notice(2));
        let sent = inbox.send_into_turn();
        assert!(inbox.injected(&sent[0]));
        inbox.turn_aborted();
        assert_eq!(inbox.wake(false), None, "held after the user stopped it");
        inbox.turn_started();
        assert_eq!(inbox.send_into_turn(), sent);
    }
}
