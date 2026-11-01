use super::*;

/// One approval on its way to the person at the keyboard.
struct WaitingApproval {
    id: u64,
    request: ApprovalRequest,
    decision: Option<Decision>,
}

/// The UI loop reads [`TuiGate::pending`] and answers via [`TuiGate::respond`].
///
/// One slot was enough while a single turn asked one question at a time.
/// Subagents share their parent's gate and ask side by side; with one slot the
/// second request overwrote the first, one answer released both, and a finished
/// caller cleared a request it had never seen. Requests queue instead, and only
/// the head of the queue is on screen.
#[derive(Default)]
pub struct TuiGate {
    waiting: Mutex<Vec<WaitingApproval>>,
    next_id: std::sync::atomic::AtomicU64,
    changed: tokio::sync::Notify,
}

impl TuiGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The request being answered now: the head of the queue. `None` while the
    /// head has its answer and its caller is still waking up, so a second click
    /// cannot reach the request behind it.
    pub fn pending(&self) -> Option<ApprovalRequest> {
        let waiting = self.waiting.lock();
        waiting.first().filter(|w| w.decision.is_none()).map(|w| w.request.clone())
    }

    /// No-op when nothing is pending.
    pub fn respond(&self, d: Decision) {
        {
            let mut waiting = self.waiting.lock();
            if let Some(head) = waiting.first_mut().filter(|w| w.decision.is_none()) {
                head.decision = Some(d);
            }
        }
        self.changed.notify_waiters();
    }

    async fn wait_decision(&self, id: u64) -> Decision {
        loop {
            // Register before checking, or a respond() in between is a lost wakeup.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            // The entry cannot be gone: only this future's guard removes it.
            if let Some(d) = self.waiting.lock().iter().find(|w| w.id == id).and_then(|w| w.decision) {
                return d;
            }
            notified.await;
        }
    }
}

/// Removes a request whose caller went away, so a cancelled turn never leaves a
/// card that nobody will answer.
struct ClearOnDrop<'a, T: HasWaiting> {
    gate: &'a T,
    id: u64,
}

impl<T: HasWaiting> Drop for ClearOnDrop<'_, T> {
    fn drop(&mut self) {
        self.gate.remove(self.id);
    }
}

trait HasWaiting {
    fn remove(&self, id: u64);
}

impl HasWaiting for TuiGate {
    fn remove(&self, id: u64) {
        self.waiting.lock().retain(|w| w.id != id);
        self.changed.notify_waiters();
    }
}

#[async_trait]
impl ApprovalGate for TuiGate {
    async fn approve(&self, req: &ApprovalRequest) -> Decision {
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.waiting.lock().push(WaitingApproval { id, request: req.clone(), decision: None });
        let _clear = ClearOnDrop { gate: self, id };
        self.changed.notify_waiters();
        self.wait_decision(id).await
    }
}

#[derive(Debug, Clone)]
pub struct QuestionRequest {
    pub question: String,
    pub options: Option<Vec<String>>,
    pub multi_select: bool,
    /// During `/goal`.
    pub deadline: Option<std::time::Instant>,
}

/// An answer and whether it was written in, or why there is none.
type QuestionAnswer = Result<(String, bool), String>;

struct WaitingQuestion {
    id: u64,
    request: QuestionRequest,
    answer: Option<QuestionAnswer>,
}

/// The question side of the same queue, for the same reason.
#[derive(Default)]
pub struct TuiQuestionGate {
    waiting: Mutex<Vec<WaitingQuestion>>,
    next_id: std::sync::atomic::AtomicU64,
    changed: tokio::sync::Notify,
}

impl TuiQuestionGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The question being answered now: the head of the queue.
    pub fn pending(&self) -> Option<QuestionRequest> {
        let waiting = self.waiting.lock();
        waiting.first().filter(|w| w.answer.is_none()).map(|w| w.request.clone())
    }

    pub fn respond(&self, answer: String, is_write_in: bool) {
        self.settle(Ok((answer, is_write_in)));
    }

    /// Closed without an answer: the model is told so, not handed words the
    /// user never wrote.
    pub fn cancel(&self) {
        self.settle(Err(flashagent_core::permissions::QUESTION_CLOSED.to_string()));
    }

    fn settle(&self, reply: QuestionAnswer) {
        {
            let mut waiting = self.waiting.lock();
            if let Some(head) = waiting.first_mut().filter(|w| w.answer.is_none()) {
                head.answer = Some(reply);
            }
        }
        self.changed.notify_waiters();
    }

    async fn wait_answer(&self, id: u64) -> QuestionAnswer {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(answer) = self.waiting.lock().iter().find(|w| w.id == id).and_then(|w| w.answer.clone()) {
                return answer;
            }
            notified.await;
        }
    }
}

impl HasWaiting for TuiQuestionGate {
    fn remove(&self, id: u64) {
        self.waiting.lock().retain(|w| w.id != id);
        self.changed.notify_waiters();
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
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.waiting.lock().push(WaitingQuestion { id, request: req, answer: None });
        let _clear = ClearOnDrop { gate: self, id };
        self.changed.notify_waiters();
        self.wait_answer(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_core::permissions::Category;
    use flashagent_tools::QuestionGate;

    fn request(tool: &str) -> ApprovalRequest {
        ApprovalRequest { tool: tool.into(), args_json: "{}".into(), category: Category::Read, diff: None }
    }

    /// Two subagents share one gate. The second must not overwrite the card the
    /// person is looking at, and one answer must not release both.
    #[tokio::test]
    async fn two_calls_at_once_get_their_own_answers() {
        let gate = TuiGate::new();
        let first = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.approve(&request("write_file")).await })
        };
        for _ in 0..200 {
            if gate.pending().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let second = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.approve(&request("run_shell")).await })
        };
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.pending().map(|r| r.tool), Some("write_file".into()), "the queued call took the card");
        gate.respond(Decision::Allow);
        assert_eq!(first.await.unwrap(), Decision::Allow);
        for _ in 0..200 {
            if gate.pending().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.pending().map(|r| r.tool), Some("run_shell".into()), "the queued call was never shown");
        gate.respond(Decision::Deny);
        assert_eq!(second.await.unwrap(), Decision::Deny);
        assert!(gate.pending().is_none());
    }

    /// The same for questions: two children asking side by side get the answer
    /// meant for the one on screen, and the parent's question is not lost.
    #[tokio::test]
    async fn two_questions_at_once_are_answered_one_by_one() {
        let gate = TuiQuestionGate::new();
        let first = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.ask("first?", None, false, None).await })
        };
        for _ in 0..200 {
            if gate.pending().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let second = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.ask("second?", None, false, None).await })
        };
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.pending().map(|q| q.question), Some("first?".into()));
        gate.respond("one".into(), false);
        assert_eq!(first.await.unwrap(), Ok(("one".to_string(), false)));
        for _ in 0..200 {
            if gate.pending().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.pending().map(|q| q.question), Some("second?".into()));
        gate.cancel();
        assert!(second.await.unwrap().is_err());
    }
}
