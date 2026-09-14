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
    /// Prompt tokens over every model call of this turn (a turn with tool
    /// calls makes several), and how many of them the server said it served
    /// from its prompt cache.
    pub(crate) turn_prompt: i64,
    pub(crate) turn_cached: i64,
    pub(crate) turn_calls: u32,
    pub(crate) cache_reported: bool,
    pub(crate) total_turns: usize,
    /// The backend is LM Studio on this machine. It reports no cache figure,
    /// but its own server log says how many tokens each call really processed.
    pub(crate) lm_studio_local: bool,
    lm_studio_log: Option<(std::path::PathBuf, u64)>,
}

/// Where this turn's cache figure came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheSource {
    /// The API's own usage numbers.
    Reported,
    /// LM Studio's server log on this machine.
    ServerLog,
}

/// How much of a turn's prompt was served from the server's cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TurnCache {
    pub(crate) prompt: i64,
    pub(crate) cached: i64,
    pub(crate) source: CacheSource,
}

impl TurnCache {
    pub(crate) fn hit_percent(&self) -> f64 {
        if self.prompt <= 0 { 0.0 } else { (self.cached as f64 / self.prompt as f64 * 100.0).clamp(0.0, 100.0) }
    }
}

/// Tokens LM Studio (llama.cpp underneath) says it actually processed, summed
/// over the `prompt eval time = ... / N tokens` lines in `log`.
pub(crate) fn evaluated_tokens_in_log(log: &str) -> i64 {
    log.lines()
        .filter(|l| l.contains("prompt eval time ="))
        .filter_map(|l| {
            let after = l.split(" / ").nth(1)?;
            after.split_whitespace().next()?.parse::<i64>().ok()
        })
        .sum()
}

/// The newest LM Studio server log and how long it is right now.
fn lm_studio_log_mark() -> Option<(std::path::PathBuf, u64)> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let root = std::path::PathBuf::from(home).join(".lmstudio").join("server-logs");
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let mut consider = |path: std::path::PathBuf| {
        if let Ok(meta) = path.metadata() {
            if meta.is_file() {
                let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
                    newest = Some((modified, path));
                }
            }
        }
    };
    for entry in std::fs::read_dir(&root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            for inner in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                consider(inner.path());
            }
        } else {
            consider(path);
        }
    }
    let (_, path) = newest?;
    let len = path.metadata().ok()?.len();
    Some((path, len))
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
            turn_prompt: 0,
            turn_cached: 0,
            turn_calls: 0,
            cache_reported: false,
            total_turns: 0,
            lm_studio_local: false,
            lm_studio_log: None,
        }
    }

    /// This turn's cache figure, when the server gave one away: in its usage
    /// numbers, or (LM Studio on this machine) in its log. `None` rather than
    /// a guess when neither did.
    pub(crate) fn turn_cache(&self) -> Option<TurnCache> {
        if self.turn_prompt <= 0 {
            return None;
        }
        if self.cache_reported {
            return Some(TurnCache { prompt: self.turn_prompt, cached: self.turn_cached.min(self.turn_prompt), source: CacheSource::Reported });
        }
        let (path, offset) = self.lm_studio_log.as_ref()?;
        let bytes = std::fs::read(path).ok()?;
        let fresh = bytes.get(*offset as usize..)?;
        let evaluated = evaluated_tokens_in_log(&String::from_utf8_lossy(fresh));
        // Nothing logged means the log is not this server's: no figure, not 100%.
        // More processed than sent means someone else used the server too.
        if evaluated <= 0 || evaluated > self.turn_prompt {
            return None;
        }
        Some(TurnCache { prompt: self.turn_prompt, cached: self.turn_prompt - evaluated, source: CacheSource::ServerLog })
    }

    pub(crate) fn on_turn_start(&mut self, model: String, estimated_prompt_tokens: usize) {
        self.total_turns += 1;
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
        self.turn_prompt = 0;
        self.turn_cached = 0;
        self.turn_calls = 0;
        self.cache_reported = false;
        self.lm_studio_log = if self.lm_studio_local { lm_studio_log_mark() } else { None };
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
        if let Some(p) = usage.prompt.filter(|p| *p > 0) {
            self.turn_prompt += p;
            self.turn_calls += 1;
            if let Some(c) = usage.cached {
                self.turn_cached += c.max(0);
                self.cache_reported = true;
            }
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

        let prompt_val = if self.turn_prompt > 0 {
            self.turn_prompt as usize
        } else {
            self.prompt_tokens.unwrap_or(0)
        };

        if prompt_val > 0 && budget >= 40 {
            let p_str = flashagent_core::ContextUsage::format_used_tokens(prompt_val);
            if budget >= 55 {
                parts.push(format!("\x1b[38;2;160;175;200m{p_str} prompt\x1b[0m"));
            } else {
                parts.push(format!("\x1b[38;2;160;175;200m{p_str}\x1b[0m"));
            }
        }

        // Cache hit: On the very first request, if cache hit is 0%, do not show it at all.
        let cache_part = match self.turn_cache() {
            Some(cache) => {
                let pct = cache.hit_percent();
                let is_first_turn_zero = self.total_turns <= 1 && pct == 0.0;
                if is_first_turn_zero {
                    None
                } else {
                    let color = if pct >= 80.0 {
                        "120;220;140"
                    } else if pct >= 30.0 {
                        "225;175;95"
                    } else {
                        "230;110;95"
                    };
                    Some(format!("\x1b[38;2;{color}mcache hit {pct:.0}%\x1b[0m"))
                }
            }
            None => {
                if prompt_val > 0 {
                    Some("\x1b[38;2;160;155;145mcache hit n/a\x1b[0m".to_string())
                } else {
                    None
                }
            }
        };

        if let Some(cp) = cache_part {
            if budget >= 45 {
                parts.push(cp);
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

        if self.turn_calls > 1 && budget >= 75 {
            parts.push(format!("\x1b[38;2;160;155;145m{} calls\x1b[0m", self.turn_calls));
        }

        if parts.is_empty() {
            return None;
        }

        let joined = parts.join(" \x1b[38;2;100;95;90m·\x1b[0m ");
        if visible_width(&joined) <= budget {
            return Some(joined);
        }

        // If joined exceeds budget, try without verbose prefill speed:
        let mut trimmed_parts = Vec::new();
        if prompt_val > 0 && budget >= 35 {
            let p_str = flashagent_core::ContextUsage::format_used_tokens(prompt_val);
            trimmed_parts.push(format!("\x1b[38;2;160;175;200m{p_str} prompt\x1b[0m"));
        }
        if let Some(cache) = self.turn_cache() {
            let pct = cache.hit_percent();
            let is_first_turn_zero = self.total_turns <= 1 && pct == 0.0;
            if !is_first_turn_zero {
                let color = if pct >= 80.0 { "120;220;140" } else if pct >= 30.0 { "225;175;95" } else { "230;110;95" };
                trimmed_parts.push(format!("\x1b[38;2;{color}mcache hit {pct:.0}%\x1b[0m"));
            }
        } else if prompt_val > 0 && budget >= 50 {
            trimmed_parts.push("\x1b[38;2;160;155;145mcache hit n/a\x1b[0m".to_string());
        }
        if let Some(ttft) = self.last_ttft {
            trimmed_parts.push(format!("\x1b[38;2;120;220;140mTTFT {:.2}s\x1b[0m", ttft.as_secs_f64()));
        }
        if let Some(s) = speed {
            if s > 0.0 {
                trimmed_parts.push(format!("\x1b[38;2;145;205;140m{:.1} tg\x1b[0m", s));
            }
        }
        let trimmed_joined = trimmed_parts.join(" \x1b[38;2;100;95;90m·\x1b[0m ");
        if visible_width(&trimmed_joined) <= budget && !trimmed_joined.is_empty() {
            return Some(trimmed_joined);
        }

        // Minimal fallback for very narrow viewports
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

    #[cfg(test)]
    pub(crate) fn set_lm_studio_log_for_test(&mut self, path: std::path::PathBuf, offset: u64) {
        self.lm_studio_log = Some((path, offset));
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
mod turn_cache_tests {
    use super::*;

    fn usage(prompt: i64, cached: Option<i64>) -> flashagent_llm::Usage {
        flashagent_llm::Usage { prompt: Some(prompt), completion: Some(10), cached, mtp: None }
    }

    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for c in s.chars() {
            match (in_escape, c) {
                (false, '\x1b') => in_escape = true,
                (true, 'm') => in_escape = false,
                (false, _) => out.push(c),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn a_reported_cache_figure_is_summed_over_every_call_of_the_turn() {
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(6000, Some(5900)));
        t.on_usage(&usage(6200, Some(6100)));
        let cache = t.turn_cache().expect("the server reported it");
        assert_eq!((cache.prompt, cache.cached, cache.source), (12_200, 12_000, CacheSource::Reported));
        assert_eq!(cache.hit_percent().round(), 98.0);
        let stats = plain(&t.format_stats_width(80).unwrap());
        assert!(stats.contains("cache hit 98%") && stats.contains("2 calls"), "{stats}");
    }

    #[test]
    fn a_new_turn_starts_counting_from_zero() {
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(5000, Some(4000)));
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(5100, Some(5000)));
        assert_eq!(t.turn_cache().unwrap().cached, 5000);
        assert_eq!(t.turn_calls, 1);
    }

    #[test]
    fn a_server_that_reports_nothing_shows_no_number_rather_than_a_guess() {
        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(6000, None));
        assert!(t.turn_cache().is_none());
        let stats = plain(&t.format_stats_width(80).unwrap());
        assert!(stats.contains("cache hit n/a"), "{stats}");
    }

    #[test]
    fn lm_studio_s_log_says_what_was_really_processed() {
        let log = "[2026-09-14 12:02:53][DEBUG] 4.33.290.330 I slot print_timing: id  1 | task 78 | prompt eval time =   36815.78 ms /  6515 tokens (    5.65 ms per token,   176.96 tokens per second)\n\
                   [2026-09-14 12:02:53][DEBUG] 4.33.290.346 I slot print_timing: id  1 | task 78 |        eval time =    1496.10 ms /    42 tokens\n\
                   [2026-09-14 12:05:04][DEBUG] 6.44.502.379 I slot print_timing: id  1 | task 287 | prompt eval time =     774.85 ms /    15 tokens (   51.66 ms per token,    19.36 tokens per second)\n";
        assert_eq!(evaluated_tokens_in_log(log), 6530, "generation (eval time) lines are not prompt processing");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.log");
        std::fs::write(&path, "an earlier turn: prompt eval time = 1.0 ms / 9999 tokens\n").unwrap();
        let offset = std::fs::metadata(&path).unwrap().len();
        let mut this_turn = std::fs::read_to_string(&path).unwrap();
        this_turn.push_str("I slot print_timing: id  1 | task 5 | prompt eval time = 50.0 ms / 120 tokens (x)\n");
        std::fs::write(&path, this_turn).unwrap();

        let mut t = TokenTracker::new("m".into());
        t.on_turn_start("m".into(), 0);
        t.set_lm_studio_log_for_test(path.clone(), offset);
        t.on_usage(&usage(6000, None));
        let cache = t.turn_cache().expect("the log has this turn's figure");
        assert_eq!((cache.cached, cache.source), (5880, CacheSource::ServerLog), "lines from before the turn are not counted");

        // A log that did not grow is not this server's: no figure at all.
        let mut quiet = TokenTracker::new("m".into());
        quiet.on_turn_start("m".into(), 0);
        let len = std::fs::metadata(&path).unwrap().len();
        quiet.set_lm_studio_log_for_test(path, len);
        quiet.on_usage(&usage(6000, None));
        assert!(quiet.turn_cache().is_none());
    }

    #[test]
    fn format_stats_width_omits_cache_hit_on_first_turn_zero() {
        let mut t = TokenTracker::new("m".into());
        // Turn 1: cold request with 0% cache hit
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(6000, Some(0)));
        t.on_finished();
        assert_eq!(t.total_turns, 1);
        let stats = plain(&t.format_stats_width(80).unwrap());
        assert!(!stats.contains("cache hit"), "turn 1 with 0% cache hit should omit cache hit completely: {stats}");

        // Turn 2: cache hit is 0% -> must be shown on subsequent turns
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(6000, Some(0)));
        t.on_finished();
        assert_eq!(t.total_turns, 2);
        let stats2 = plain(&t.format_stats_width(80).unwrap());
        assert!(stats2.contains("cache hit 0%"), "turn 2 with 0% cache hit must show cache hit 0%: {stats2}");

        // Turn 3: cache hit is 95% -> must be shown
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(6000, Some(5700)));
        t.on_finished();
        let stats3 = plain(&t.format_stats_width(80).unwrap());
        assert!(stats3.contains("cache hit 95%"), "turn 3 with 95% cache hit must show: {stats3}");
    }

    #[test]
    fn format_stats_width_shows_cache_hit_on_first_turn_if_positive() {
        let mut t = TokenTracker::new("m".into());
        // Turn 1: system prompt hit gives 50%
        t.on_turn_start("m".into(), 0);
        t.on_usage(&usage(6000, Some(3000)));
        t.on_finished();
        let stats = plain(&t.format_stats_width(80).unwrap());
        assert!(stats.contains("cache hit 50%"), "turn 1 with positive cache hit should be shown: {stats}");
    }
}
