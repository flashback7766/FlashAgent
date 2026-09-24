use super::*;

/// Generation speed (tg_3s) while the model writes, and prefill speed / TTFT
/// once it has read the prompt.
pub(crate) struct TokenTracker {
    pub(crate) model: String,
    pub(crate) total_model_tokens: usize,
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
        // The server's count corrects the estimate made from the deltas.
        if let Some(c) = usage.completion {
            if c > 0 {
                let comp = c as usize;
                if comp > self.total_model_tokens {
                    let diff = comp - self.total_model_tokens;
                    let now = std::time::Instant::now();
                    if self.first_token_time.is_none() {
                        self.first_token_time = Some(now);
                    }
                    self.window.push_back((now, diff));
                    self.total_model_tokens = comp;
                } else if comp < self.total_model_tokens {
                    self.total_model_tokens = comp;
                }
            }
        }
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

        if self.window.is_empty() {
            return 0.0;
        }

        let sum: usize = self.window.iter().map(|(_, n)| *n).sum();
        let earliest = self.window.front().map(|(t, _)| *t).unwrap_or(now);
        let dt = now.duration_since(earliest).as_secs_f64().max(0.25);
        sum as f64 / dt
    }

    pub(crate) fn live_prefill_status(&self) -> Option<String> {
        if self.is_running && self.first_token_time.is_none() {
            let start = self.turn_start_time?;
            let elapsed = start.elapsed();
            let prompt = self.prompt_tokens.unwrap_or(500);
            let cached = (self.last_f_keep.unwrap_or(0.0) * prompt as f64) as usize;
            Some(self.prefill_tracker.format_live_prefill(&self.model, prompt, cached, elapsed))
        } else {
            None
        }
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
}
