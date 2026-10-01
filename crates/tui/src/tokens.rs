use super::*;

/// Generation speed (tg_3s) while the model writes, and prefill speed / TTFT
/// once it has read the prompt.
pub(crate) struct TokenTracker {
    pub(crate) model: String,
    pub(crate) total_model_tokens: usize,
    /// What the current request has produced so far. The server counts per
    /// request, not per turn, and a turn with tools is many requests: writing
    /// the server's figure straight into the total made every request erase
    /// the ones before it, so a long turn reported only its last step.
    pub(crate) request_tokens: usize,
    /// What the children of this turn have produced, in the same number as the
    /// parent's own output. Their work is this turn's work: a turn that spent
    /// three agents and reports only its own text hides most of what it cost.
    pub(crate) subagent_tokens: usize,
    /// The children's total across the whole session, so a new turn counts only
    /// what its children produced after it started.
    pub(crate) subagent_seen: usize,
    pub(crate) window: std::collections::VecDeque<(std::time::Instant, usize)>,
    pub(crate) last_f_keep: Option<f64>,
    pub(crate) turn_start_time: Option<std::time::Instant>,
    pub(crate) first_token_time: Option<std::time::Instant>,
    pub(crate) prompt_tokens: Option<usize>,
    pub(crate) last_ttft: Option<std::time::Duration>,
    pub(crate) last_prefill_speed: Option<f64>,
    pub(crate) prefill_tracker: PrefillTracker,
    pub(crate) is_running: bool,
    /// Share of draft tokens the model kept, from a server that decodes with a
    /// draft model or MTP heads (llama.cpp): what the MTP presets are tuned by.
    pub(crate) draft_acceptance: Option<f64>,
}

impl TokenTracker {
    pub(crate) fn new(model: String) -> Self {
        Self {
            model,
            total_model_tokens: 0,
            request_tokens: 0,
            subagent_tokens: 0,
            subagent_seen: 0,
            window: std::collections::VecDeque::new(),
            last_f_keep: None,
            turn_start_time: None,
            first_token_time: None,
            prompt_tokens: None,
            last_ttft: None,
            last_prefill_speed: None,
            prefill_tracker: PrefillTracker::load_or_default(),
            is_running: false,
            draft_acceptance: None,
        }
    }

    pub(crate) fn on_turn_start(&mut self, model: String, estimated_prompt_tokens: usize) {
        self.model = model;
        self.total_model_tokens = 0;
        self.request_tokens = 0;
        // The children's session total carries over on purpose: a child still
        // running when this turn started keeps its earlier tokens to its earlier
        // turn, and only what it produces from here belongs to this one.
        self.subagent_tokens = 0;
        self.window.clear();
        self.turn_start_time = Some(std::time::Instant::now());
        self.draft_acceptance = None;
        self.first_token_time = None;
        self.prompt_tokens = if estimated_prompt_tokens > 0 {
            Some(estimated_prompt_tokens)
        } else {
            None
        };
        self.last_ttft = None;
        self.last_prefill_speed = None;
        self.is_running = true;
    }

    pub(crate) fn on_delta(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        if self.first_token_time.is_none() {
            self.first_token_time = Some(now);
            if let Some(start) = self.turn_start_time {
                let ttft = now.duration_since(start);
                self.last_ttft = Some(ttft);
                let prompt = self.prompt_tokens.unwrap_or(0);
                let cached = (self.last_f_keep.unwrap_or(0.0) * prompt as f64) as usize;
                let eval = prompt.saturating_sub(cached).max(1);
                let spd = eval as f64 / ttft.as_secs_f64().max(0.001);
                self.last_prefill_speed = Some(spd);
                self.prefill_tracker.record(&self.model, prompt, cached, ttft);
            }
        }
        let count = flashagent_llm::count_tokens(&self.model, text).max(1);
        self.request_tokens += count;
        self.total_model_tokens += count;
        self.window.push_back((now, count));
    }

    pub(crate) fn on_usage(&mut self, usage: &flashagent_llm::Usage) {
        if let Some(mtp) = usage.mtp.filter(|m| m.total_draft_tokens > 0) {
            self.draft_acceptance = Some(mtp.acceptance_rate());
        }
        if let (Some(c), Some(p)) = (usage.cached, usage.prompt) {
            if p > 0 {
                self.last_f_keep = Some((c as f64 / p as f64).clamp(0.0, 1.0));
                if let Some(ttft) = self.last_ttft {
                    self.prefill_tracker.record(&self.model, p as usize, c as usize, ttft);
                    let eval = (p as usize).saturating_sub(c as usize).max(1);
                    self.last_prefill_speed = Some(eval as f64 / ttft.as_secs_f64().max(0.001));
                }
            }
        }
        if let Some(p) = usage.prompt {
            if p > 0 {
                self.prompt_tokens = Some(p as usize);
            }
        }
        // The server's count corrects what the deltas estimated, for this
        // request. It is a difference and not a total: the turn's total already
        // holds every earlier request, and taking the server's figure as the
        // turn's total is what made a long turn report only its last step.
        if let Some(c) = usage.completion {
            if c > 0 {
                let comp = c as usize;
                if comp > self.request_tokens {
                    let diff = comp - self.request_tokens;
                    let now = std::time::Instant::now();
                    if self.first_token_time.is_none() {
                        self.first_token_time = Some(now);
                    }
                    self.window.push_back((now, diff));
                    self.request_tokens = comp;
                    self.total_model_tokens += diff;
                } else {
                    // The server counted less than the deltas did, which happens
                    // when a draft model was rejected. The total follows it down
                    // by the same amount, so it stays the same figure the server
                    // would report for the request.
                    self.total_model_tokens -= self.request_tokens - comp;
                    self.request_tokens = comp;
                }
            }
        }
    }

    /// A new request starts. The server's count is per request, so the running
    /// estimate has to start again: without this, the second request's figure is
    /// read as a correction of the first and the turn comes out smaller than the
    /// work it did.
    pub(crate) fn on_request_start(&mut self) {
        self.request_tokens = 0;
    }

    /// What every child has produced this session, so the turn can count the
    /// difference since it started and nothing twice.
    pub(crate) fn on_subagent_usage(&mut self, session_total: usize) {
        if session_total > self.subagent_seen {
            self.subagent_tokens += session_total - self.subagent_seen;
        }
        self.subagent_seen = session_total;
    }

    /// What this turn has generated: the parent's own output and its children's.
    pub(crate) fn turn_output(&self) -> usize {
        self.total_model_tokens + self.subagent_tokens
    }

    pub(crate) fn on_finished(&mut self) {
        self.is_running = false;
    }

    pub(crate) fn tg_3s(&mut self) -> f64 {
        let now = std::time::Instant::now();
        let cutoff = now.checked_sub(std::time::Duration::from_secs(3)).unwrap_or(now);

        while let Some(&(t, _)) = self.window.front() {
            if t < cutoff {
                self.window.pop_front();
            } else {
                break;
            }
        }

        // Nothing came for a while (a tool runs, the model reads): the rate is not
        // "falling", there is no rate. It would decay to 0.3 t/s on screen.
        if self.window.back().is_none_or(|(t, _)| now.duration_since(*t) > std::time::Duration::from_millis(1500)) {
            return 0.0;
        }

        let sum: usize = self.window.iter().map(|(_, n)| *n).sum();
        let earliest = self.window.front().map(|(t, _)| *t).unwrap_or(now);
        let dt = now.duration_since(earliest).as_secs_f64().max(0.25);
        sum as f64 / dt
    }

    pub(crate) fn live_prefill_status(&self, nearby: bool) -> Option<String> {
        if self.is_running && self.first_token_time.is_none() {
            let start = self.turn_start_time?;
            let elapsed = start.elapsed();
            let prompt = self.prompt_tokens.unwrap_or(500);
            let cached = (self.last_f_keep.unwrap_or(0.0) * prompt as f64) as usize;
            Some(self.prefill_tracker.format_live_prefill(&self.model, prompt, cached, elapsed, nearby))
        } else {
            None
        }
    }

    /// The share of the prompt the server read from its cache, which it reports
    /// only once the turn is over. Nothing is said on a turn with no hit: on the
    /// first message there is nothing to have cached, and a 0% would read as a
    /// fault rather than as the first line of a conversation.
    pub(crate) fn cache_display(&self) -> Option<String> {
        let share = self.last_f_keep.filter(|s| *s > 0.0)?;
        Some(format!("\x1b[38;2;175;170;225mcache {:.0}%\x1b[0m", share * 100.0))
    }

    /// E.g. `draft 81%`, while a turn runs on a server that reports it.
    pub(crate) fn draft_display(&self) -> Option<String> {
        let rate = self.draft_acceptance.filter(|_| self.is_running)?;
        Some(format!("\x1b[38;2;175;170;225mdraft {rate:.0}%\x1b[0m"))
    }

    pub(crate) fn ttft_display(&self) -> Option<String> {
        let ttft = self.last_ttft?;
        let spd = self.last_prefill_speed?;
        let speed_str = if spd >= 1000.0 {
            format!("{:.1}k t/s", spd / 1000.0)
        } else {
            format!("{:.0} t/s", spd)
        };
        Some(format!("\x1b[38;2;120;220;140mTTFT {:.2}s ({speed_str})\x1b[0m", ttft.as_secs_f64()))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_model_s_hit_rate_shows_while_the_turn_runs() {
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        assert_eq!(t.draft_display(), None, "a server without a draft model says nothing");
        let mtp = flashagent_llm::MtpStats { total_draft_tokens: 16, accepted_draft_tokens: 13, rejected_draft_tokens: 3 };
        t.on_usage(&flashagent_llm::Usage { mtp: Some(mtp), ..Default::default() });
        assert!(t.draft_display().is_some_and(|d| d.contains("draft 81%")), "{:?}", t.draft_display());
        t.on_finished();
        assert_eq!(t.draft_display(), None);
        t.on_turn_start("m".into(), 0);
        assert_eq!(t.draft_display(), None, "each turn reports its own");
    }

    fn usage(completion: usize) -> flashagent_llm::Usage {
        flashagent_llm::Usage { completion: Some(completion as i64), ..Default::default() }
    }

    #[test]
    fn a_turn_with_several_requests_reports_all_of_them_and_not_only_the_last() {
        // The server counts a request, not a turn. A turn that calls tools makes
        // several, and taking each figure as the turn's total made every request
        // erase the ones before it, so a long turn reported only its last step.
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(300));
        t.on_request_start();
        t.on_usage(&usage(250));
        t.on_request_start();
        t.on_usage(&usage(400));
        assert_eq!(t.turn_output(), 950, "300 + 250 + 400, not 400");
    }

    #[test]
    fn a_turn_counts_what_its_children_wrote_without_counting_it_twice() {
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(200));
        t.on_subagent_usage(1_000);
        assert_eq!(t.turn_output(), 1_200, "the parent's 200 and the children's 1000");
        // The same total arriving again is the same turn, not more output.
        t.on_subagent_usage(1_000);
        assert_eq!(t.turn_output(), 1_200);
        t.on_subagent_usage(1_400);
        assert_eq!(t.turn_output(), 1_600, "only what came after the last reading");
    }

    #[test]
    fn a_new_turn_does_not_inherit_what_the_last_one_generated() {
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(500));
        t.on_subagent_usage(800);
        assert_eq!(t.turn_output(), 1_300);
        t.on_turn_start("m".into(), 0);
        assert_eq!(t.turn_output(), 0, "a new turn starts its own count");
        // A child still running keeps counting into the turn that is live now.
        t.on_subagent_usage(950);
        assert_eq!(t.turn_output(), 150, "what it wrote after this turn started");
    }

    #[test]
    fn reasoning_is_generation_and_is_counted_as_such() {
        // Reasoning is tokens the model spent, so it belongs in what a turn
        // generated. It arrives as its own delta and goes through the same path
        // as the answer, which is the point: a thought costs what a sentence
        // costs.
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_delta("the user asked for a plan, so the plan needs");
        t.on_delta(" what it is for, and that is the first clause");
        assert!(t.turn_output() > 0, "a thought is output too");
    }

    #[test]
    fn a_server_count_below_the_deltas_corrects_the_total_down() {
        // A rejected draft makes the server count less than the deltas did. The
        // total follows it rather than being replaced by it, so a later request
        // still adds to what this one really cost.
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_delta(&"word ".repeat(100));
        let estimated = t.turn_output();
        t.on_usage(&usage(20));
        assert_eq!(t.turn_output(), 20, "the server's figure for this request");
        t.on_request_start();
        t.on_usage(&usage(30));
        assert_eq!(t.turn_output(), 50, "20 for the first request plus 30 for the next");
        assert!(estimated > 20, "the deltas had over-counted, which is what made this worth correcting");
    }

    #[test]
    fn a_deltas_estimate_is_never_double_counted_when_the_server_agrees() {
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_delta("some words here");
        let before = t.turn_output();
        // The server says what the deltas already counted, as it would for a
        // request whose estimate happened to be right.
        t.on_usage(&usage(before));
        assert_eq!(t.turn_output(), before, "the same figure twice is not twice the output");
        t.on_request_start();
        t.on_usage(&usage(50));
        assert_eq!(t.turn_output(), before + 50, "the next request is added, not folded in");
    }
}
