use super::*;

/// Tracks tokens generated strictly by the model in the current turn and
/// calculates generation speeds (tg and tg_3s), prompt tokens, prefill speed / TTFT,
/// and speculative decoding stats (MTP).
pub(crate) struct TokenTracker {
    pub(crate) model: String,
    pub(crate) total_model_tokens: usize,
    pub(crate) window: std::collections::VecDeque<(std::time::Instant, usize)>,
    pub(crate) last_f_keep: Option<f64>,
    pub(crate) turn_start_time: Option<std::time::Instant>,
    pub(crate) first_token_time: Option<std::time::Instant>,
    pub(crate) last_token_time: Option<std::time::Instant>,
    pub(crate) active_duration: std::time::Duration,
    pub(crate) prompt_tokens: Option<usize>,
    pub(crate) last_tg: Option<f64>,
    pub(crate) last_mtp: Option<flashagent_llm::MtpStats>,
    pub(crate) last_ttft: Option<std::time::Duration>,
    pub(crate) last_prefill_speed: Option<f64>,
    pub(crate) prefill_tracker: PrefillTracker,
    pub(crate) is_running: bool,
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
            last_token_time: None,
            active_duration: std::time::Duration::ZERO,
            prompt_tokens: None,
            last_tg: None,
            last_mtp: None,
            last_ttft: None,
            last_prefill_speed: None,
            prefill_tracker: PrefillTracker::load_or_default(),
            is_running: false,
        }
    }

    pub(crate) fn on_turn_start(&mut self, model: String, estimated_prompt_tokens: usize) {
        self.model = model;
        self.total_model_tokens = 0;
        self.window.clear();
        self.turn_start_time = Some(std::time::Instant::now());
        self.first_token_time = None;
        self.last_token_time = None;
        self.active_duration = std::time::Duration::ZERO;
        self.prompt_tokens = if estimated_prompt_tokens > 0 {
            Some(estimated_prompt_tokens)
        } else {
            None
        };
        self.last_tg = None;
        self.last_mtp = None;
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
        if let Some(last) = self.last_token_time {
            let dt = now.duration_since(last);
            if dt < std::time::Duration::from_millis(1500) {
                self.active_duration += dt;
            }
        }
        self.last_token_time = Some(now);

        let count = flashagent_llm::count_tokens(&self.model, text).max(1);
        self.total_model_tokens += count;
        self.window.push_back((now, count));
    }

    pub(crate) fn on_usage(&mut self, usage: &flashagent_llm::Usage) {
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
        if let Some(mtp) = usage.mtp {
            self.last_mtp = Some(mtp);
        }
        if let Some(c) = usage.completion {
            if c > 0 {
                let comp = c as usize;
                if comp > self.total_model_tokens {
                    let diff = comp - self.total_model_tokens;
                    let now = std::time::Instant::now();
                    if self.first_token_time.is_none() {
                        self.first_token_time = Some(now);
                    }
                    if let Some(last) = self.last_token_time {
                        let dt = now.duration_since(last);
                        if dt < std::time::Duration::from_millis(1500) {
                            self.active_duration += dt;
                        }
                    }
                    self.last_token_time = Some(now);
                    self.window.push_back((now, diff));
                    self.total_model_tokens = comp;
                } else if comp < self.total_model_tokens {
                    self.total_model_tokens = comp;
                }
            }
        }
    }

    pub(crate) fn tg(&self) -> Option<f64> {
        if self.is_running {
            if self.total_model_tokens == 0 {
                return None;
            }
            let secs = self.active_duration.as_secs_f64();
            if secs >= 0.1 {
                Some(self.total_model_tokens as f64 / secs)
            } else if let Some(first) = self.first_token_time {
                let elapsed = std::time::Instant::now().duration_since(first).as_secs_f64();
                if elapsed >= 0.2 {
                    Some(self.total_model_tokens as f64 / elapsed)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            self.last_tg
        }
    }

    pub(crate) fn on_finished(&mut self) {
        self.is_running = false;
        if self.total_model_tokens > 0 {
            let secs = self.active_duration.as_secs_f64();
            if secs >= 0.05 {
                self.last_tg = Some(self.total_model_tokens as f64 / secs);
            } else if let (Some(first), Some(last)) = (self.first_token_time, self.last_token_time) {
                let dt = last.duration_since(first).as_secs_f64();
                if dt >= 0.05 {
                    self.last_tg = Some(self.total_model_tokens as f64 / dt);
                }
            }
        }
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

    pub(crate) fn format_stats_width(&self, budget: usize) -> Option<String> {
        if budget < 10 {
            return None;
        }

        let mut parts = Vec::new();

        if let Some(p) = self.prompt_tokens {
            if p > 0 && budget >= 45 {
                let p_str = flashagent_core::ContextUsage::format_used_tokens(p);
                if budget >= 60 {
                    parts.push(format!("\x1b[38;2;160;175;200m{p_str} prompt\x1b[0m"));
                } else {
                    parts.push(format!("\x1b[38;2;160;175;200m{p_str}\x1b[0m"));
                }
            }
        }

        if let Some(ttft) = self.last_ttft {
            if budget >= 60 {
                let spd_str = if let Some(spd) = self.last_prefill_speed {
                    if spd >= 1000.0 {
                        format!("{:.1}k t/s prefill", spd / 1000.0)
                    } else {
                        format!("{:.0} t/s prefill", spd)
                    }
                } else {
                    "prefill".to_string()
                };
                parts.push(format!("\x1b[38;2;120;220;140mTTFT {:.2}s ({spd_str})\x1b[0m", ttft.as_secs_f64()));
            } else if budget >= 30 {
                parts.push(format!("\x1b[38;2;120;220;140mTTFT {:.2}s\x1b[0m", ttft.as_secs_f64()));
            } else {
                parts.push(format!("\x1b[38;2;120;220;140m{:.2}s\x1b[0m", ttft.as_secs_f64()));
            }
        }

        let speed = if self.is_running {
            self.tg()
        } else {
            self.last_tg
        };

        if let Some(s) = speed {
            if s > 0.0 && budget >= 18 {
                parts.push(format!("\x1b[38;2;145;205;140m{:.1} tg\x1b[0m", s));
            }
        }

        if let Some(mtp) = &self.last_mtp {
            if mtp.total_draft_tokens > 0 && budget >= 35 {
                let rate = mtp.acceptance_rate();
                if budget >= 50 {
                    parts.push(format!("\x1b[38;2;225;175;95mmtp: {:.0}%\x1b[0m", rate));
                } else {
                    parts.push(format!("\x1b[38;2;225;175;95m{:.0}%\x1b[0m", rate));
                }
            }
        }

        if parts.is_empty() {
            None
        } else {
            let joined = parts.join(" \x1b[38;2;100;95;90m·\x1b[0m ");
            if visible_width(&joined) <= budget {
                Some(joined)
            } else {
                let mut fallback_parts = Vec::new();
                if let Some(ttft) = self.last_ttft {
                    fallback_parts.push(format!("\x1b[38;2;120;220;140m{:.2}s\x1b[0m", ttft.as_secs_f64()));
                }
                if let Some(s) = speed {
                    if s > 0.0 {
                        fallback_parts.push(format!("\x1b[38;2;145;205;140m{:.1} tg\x1b[0m", s));
                    }
                }
                let fallback = fallback_parts.join(" \x1b[38;2;100;95;90m·\x1b[0m ");
                if visible_width(&fallback) <= budget && !fallback.is_empty() {
                    Some(fallback)
                } else {
                    None
                }
            }
        }
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
