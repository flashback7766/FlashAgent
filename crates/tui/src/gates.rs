use super::*;

/// An approval gate answered from the terminal: the UI loop picks up
/// [`TuiGate::pending`] and calls [`TuiGate::respond`] with the user's key press.
#[derive(Default)]
pub struct TuiGate {
    pending: Mutex<Option<(ApprovalRequest, Option<Decision>)>>,
    changed: tokio::sync::Notify,
}

impl TuiGate {
    /// Create an empty gate.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Current pending request, if any (the UI renders a card for it).
    pub fn pending(&self) -> Option<ApprovalRequest> {
        self.pending.lock().as_ref().map(|(r, _)| r.clone())
    }

    /// Answer the pending request (no-op when nothing is pending).
    pub fn respond(&self, d: Decision) {
        let mut guard = self.pending.lock();
        if let Some(slot) = guard.as_mut() {
            slot.1 = Some(d);
        }
        drop(guard);
        self.changed.notify_waiters();
    }

    async fn wait_decision(&self) -> Decision {
        loop {
            // Register interest *before* checking: a respond() landing between
            // the check and the await would otherwise be a lost wakeup.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(d) = self.pending.lock().as_ref().and_then(|(_, d)| *d) {
                return d;
            }
            notified.await;
        }
    }
}

/// Clears a gate's pending card when the waiting call goes away — also when
/// the turn is cancelled mid-question, so the card never outlives its caller.
pub(crate) struct ClearOnDrop<'a, T>(&'a Mutex<Option<T>>, &'a tokio::sync::Notify);

impl<T> Drop for ClearOnDrop<'_, T> {
    fn drop(&mut self) {
        *self.0.lock() = None;
        self.1.notify_waiters();
    }
}

#[async_trait]
impl ApprovalGate for TuiGate {
    async fn approve(&self, req: &ApprovalRequest) -> Decision {
        *self.pending.lock() = Some((req.clone(), None));
        let _clear = ClearOnDrop(&self.pending, &self.changed);
        self.changed.notify_waiters();
        self.wait_decision().await
    }
}

/// Interactive question request from the model.
#[derive(Debug, Clone)]
pub struct QuestionRequest {
    pub question: String,
    pub options: Option<Vec<String>>,
    pub multi_select: bool,
    /// When an unanswered question stops waiting (during `/goal`).
    pub deadline: Option<std::time::Instant>,
}

pub(crate) type QuestionSlot = Option<(QuestionRequest, Option<(String, bool)>)>;

/// TUI question gate: presents questions from `ask_user` in the UI composer.
#[derive(Default)]
pub struct TuiQuestionGate {
    pending: parking_lot::Mutex<QuestionSlot>,
    changed: tokio::sync::Notify,
}

impl TuiQuestionGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn pending(&self) -> Option<QuestionRequest> {
        self.pending.lock().as_ref().map(|(r, _)| r.clone())
    }

    pub fn respond(&self, answer: String, is_write_in: bool) {
        let mut guard = self.pending.lock();
        if let Some(slot) = guard.as_mut() {
            slot.1 = Some((answer, is_write_in));
        }
        drop(guard);
        self.changed.notify_waiters();
    }

    pub fn cancel(&self) {
        self.respond("User cancelled the question".to_string(), true);
    }

    async fn wait_answer(&self) -> (String, bool) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(res) = self.pending.lock().as_ref().and_then(|(_, r)| r.clone()) {
                return res;
            }
            notified.await;
        }
    }
}

#[async_trait]
impl flashagent_tools::QuestionGate for TuiQuestionGate {
    async fn ask(
        &self,
        question: &str,
        options: Option<&[String]>,
        multi_select: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<(String, bool), String> {
        let req = QuestionRequest {
            question: question.to_string(),
            options: options.map(|opts| opts.to_vec()),
            multi_select,
            deadline,
        };
        *self.pending.lock() = Some((req, None));
        let _clear = ClearOnDrop(&self.pending, &self.changed);
        self.changed.notify_waiters();
        Ok(self.wait_answer().await)
    }
}
