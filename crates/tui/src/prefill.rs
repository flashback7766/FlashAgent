//! Prefill (Prompt Evaluation) speed tracking and exact Time To First Token (TTFT) prediction.
//!
//! Tracks evaluation speed (tokens/sec) per model across different context window lengths
//! (short <2k, medium 2k-8k, long 8k-32k, xl 32k+) with Exponential Moving Average (EMA).
//! Persists calibration data to `~/.flashagent/prefill_cache.json`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Categorization of context length for attention / KV scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContextBucket {
    /// 0 - 2,048 tokens
    Short,
    /// 2,049 - 8,192 tokens
    Medium,
    /// 8,193 - 32,768 tokens
    Long,
    /// 32,769+ tokens
    XLong,
}

impl ContextBucket {
    pub fn from_tokens(tokens: usize) -> Self {
        if tokens <= 2048 {
            ContextBucket::Short
        } else if tokens <= 8192 {
            ContextBucket::Medium
        } else if tokens <= 32768 {
            ContextBucket::Long
        } else {
            ContextBucket::XLong
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ContextBucket::Short => "<2k",
            ContextBucket::Medium => "2k-8k",
            ContextBucket::Long => "8k-32k",
            ContextBucket::XLong => "32k+",
        }
    }
}

/// Statistics within a context bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketStats {
    pub samples: usize,
    pub ema_speed_tok_s: f64,
    pub ema_overhead_ms: f64,
}

impl Default for BucketStats {
    fn default() -> Self {
        Self {
            samples: 0,
            ema_speed_tok_s: 1800.0,
            ema_overhead_ms: 120.0,
        }
    }
}

/// Historical prefill profile for a specific model ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPrefillProfile {
    pub model: String,
    pub buckets: HashMap<ContextBucket, BucketStats>,
    pub overall_speed_tok_s: f64,
    pub overall_overhead_ms: f64,
    pub total_samples: usize,
}

impl ModelPrefillProfile {
    pub fn new(model: String) -> Self {
        Self {
            model,
            buckets: HashMap::new(),
            overall_speed_tok_s: 1800.0,
            overall_overhead_ms: 120.0,
            total_samples: 0,
        }
    }

    pub fn record_sample(&mut self, prompt_tokens: usize, cached_tokens: usize, ttft: Duration) {
        let eval_tokens = prompt_tokens.saturating_sub(cached_tokens).max(1);
        let ttft_secs = ttft.as_secs_f64();
        if ttft_secs <= 0.005 {
            return;
        }

        let speed = eval_tokens as f64 / ttft_secs;
        let overhead_ms = (ttft_secs * 1000.0 * 0.15).clamp(15.0, 450.0);

        self.total_samples += 1;
        let alpha = (1.0 / (self.total_samples as f64).min(10.0)).max(0.20);

        if self.total_samples == 1 {
            self.overall_speed_tok_s = speed;
            self.overall_overhead_ms = overhead_ms;
        } else {
            self.overall_speed_tok_s = (1.0 - alpha) * self.overall_speed_tok_s + alpha * speed;
            self.overall_overhead_ms = (1.0 - alpha) * self.overall_overhead_ms + alpha * overhead_ms;
        }

        let bucket = ContextBucket::from_tokens(prompt_tokens);
        let b = self.buckets.entry(bucket).or_default();
        b.samples += 1;
        let b_alpha = (1.0 / (b.samples as f64).min(8.0)).max(0.25);
        if b.samples == 1 {
            b.ema_speed_tok_s = speed;
            b.ema_overhead_ms = overhead_ms;
        } else {
            b.ema_speed_tok_s = (1.0 - b_alpha) * b.ema_speed_tok_s + b_alpha * speed;
            b.ema_overhead_ms = (1.0 - b_alpha) * b.ema_overhead_ms + b_alpha * overhead_ms;
        }
    }

    pub fn predict_ttft(&self, prompt_tokens: usize, cached_tokens: usize) -> (Duration, f64) {
        let eval_tokens = prompt_tokens.saturating_sub(cached_tokens).max(1);
        let bucket = ContextBucket::from_tokens(prompt_tokens);

        let (speed, overhead_ms) = if let Some(b) = self.buckets.get(&bucket) {
            if b.samples > 0 {
                (b.ema_speed_tok_s, b.ema_overhead_ms)
            } else {
                (self.overall_speed_tok_s, self.overall_overhead_ms)
            }
        } else {
            (self.overall_speed_tok_s, self.overall_overhead_ms)
        };

        let est_secs = (overhead_ms / 1000.0) + (eval_tokens as f64 / speed.max(50.0));
        (Duration::from_secs_f64(est_secs.clamp(0.05, 300.0)), speed)
    }
}

/// Global tracker across all known models.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrefillTracker {
    pub profiles: HashMap<String, ModelPrefillProfile>,
}

impl PrefillTracker {
    pub fn cache_file_path() -> Option<PathBuf> {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .map(|h| PathBuf::from(h).join(".flashagent").join("prefill_cache.json"))
    }

    pub fn load_or_default() -> Self {
        if let Some(path) = Self::cache_file_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(tracker) = serde_json::from_str::<Self>(&content) {
                    return tracker;
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        if let Some(path) = Self::cache_file_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(&path, json);
            }
        }
    }

    pub fn record(&mut self, model: &str, prompt_tokens: usize, cached_tokens: usize, ttft: Duration) {
        let prof = self.profiles.entry(model.to_string()).or_insert_with(|| ModelPrefillProfile::new(model.to_string()));
        prof.record_sample(prompt_tokens, cached_tokens, ttft);
        self.save();
    }

    pub fn predict_ttft(&self, model: &str, prompt_tokens: usize, cached_tokens: usize) -> (Duration, f64) {
        if let Some(prof) = self.profiles.get(model) {
            prof.predict_ttft(prompt_tokens, cached_tokens)
        } else {
            let eval = prompt_tokens.saturating_sub(cached_tokens).max(1);
            let speed = 1800.0;
            let est_secs = 0.12 + (eval as f64 / speed);
            (Duration::from_secs_f64(est_secs.clamp(0.05, 300.0)), speed)
        }
    }

    pub fn format_live_prefill(
        &self,
        model: &str,
        prompt_tokens: usize,
        cached_tokens: usize,
        elapsed: Duration,
    ) -> String {
        let (pred_dur, speed) = self.predict_ttft(model, prompt_tokens, cached_tokens);
        let eval_tokens = prompt_tokens.saturating_sub(cached_tokens).max(1);
        let eval_str = if eval_tokens >= 1000 {
            format!("{:.1}k", eval_tokens as f64 / 1000.0)
        } else {
            format!("{eval_tokens}")
        };

        let speed_str = if speed >= 1000.0 {
            format!("{:.1}k t/s", speed / 1000.0)
        } else {
            format!("{:.0} t/s", speed)
        };

        let elapsed_secs = elapsed.as_secs_f64();
        let pred_secs = pred_dur.as_secs_f64();

        if elapsed_secs < pred_secs {
            let fraction = (elapsed_secs / pred_secs).clamp(0.0, 0.95);
            let total_bars = 6;
            let filled = ((fraction * total_bars as f64).round() as usize).min(total_bars);
            let bar: String = (0..total_bars)
                .map(|i| if i < filled { '•' } else { '·' })
                .collect();

            format!(
                "\x1b[38;2;120;220;140mPrefill ~{:.1}s\x1b[0m \x1b[38;2;140;150;170m({eval_str} @ {speed_str})\x1b[0m \x1b[38;2;100;140;180m[{bar}]\x1b[0m",
                pred_secs
            )
        } else {
            format!(
                "\x1b[38;2;225;175;95mPrefill {:.1}s\x1b[0m \x1b[38;2;140;150;170m({eval_str} @ {speed_str})...\x1b[0m",
                elapsed_secs
            )
        }
    }

    pub fn format_completed_prefill(
        &self,
        prompt_tokens: usize,
        cached_tokens: usize,
        ttft: Duration,
    ) -> String {
        let eval_tokens = prompt_tokens.saturating_sub(cached_tokens).max(1);
        let secs = ttft.as_secs_f64().max(0.001);
        let speed = eval_tokens as f64 / secs;

        let speed_str = if speed >= 1000.0 {
            format!("{:.1}k t/s", speed / 1000.0)
        } else {
            format!("{:.0} t/s", speed)
        };

        format!("\x1b[38;2;120;220;140mTTFT {:.2}s ({speed_str} prefill)\x1b[0m", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_buckets_classification() {
        assert_eq!(ContextBucket::from_tokens(500), ContextBucket::Short);
        assert_eq!(ContextBucket::from_tokens(2048), ContextBucket::Short);
        assert_eq!(ContextBucket::from_tokens(2049), ContextBucket::Medium);
        assert_eq!(ContextBucket::from_tokens(8192), ContextBucket::Medium);
        assert_eq!(ContextBucket::from_tokens(8193), ContextBucket::Long);
        assert_eq!(ContextBucket::from_tokens(32768), ContextBucket::Long);
        assert_eq!(ContextBucket::from_tokens(32769), ContextBucket::XLong);
    }

    #[test]
    fn test_model_prefill_profile_recording_and_prediction() {
        let mut prof = ModelPrefillProfile::new("qwen-35b".to_string());
        // Initial prediction on clean profile
        let (pred1, speed1) = prof.predict_ttft(2000, 0);
        assert!(pred1.as_secs_f64() > 0.5);
        assert_eq!(speed1, 1800.0);

        // Record a fast sample: 2000 tokens evaluated in 1.0s = 2000 tok/s
        prof.record_sample(2000, 0, Duration::from_secs_f64(1.0));
        assert_eq!(prof.total_samples, 1);
        assert!((prof.overall_speed_tok_s - 2000.0).abs() < 1.0);

        // Subsequent prediction should reflect the learned speed
        let (pred2, speed2) = prof.predict_ttft(2000, 0);
        assert!((speed2 - 2000.0).abs() < 1.0);
        assert!((pred2.as_secs_f64() - 1.15).abs() < 0.2);

        // With cached tokens (e.g. 1500 cached out of 2000, eval = 500)
        let (pred_cached, _) = prof.predict_ttft(2000, 1500);
        assert!(pred_cached.as_secs_f64() < pred2.as_secs_f64());
    }

    #[test]
    fn test_prefill_tracker_live_and_completed_formatting() {
        let mut tracker = PrefillTracker::default();
        tracker.record("test-model", 4000, 0, Duration::from_secs_f64(2.0));

        let live = tracker.format_live_prefill("test-model", 4000, 0, Duration::from_secs_f64(0.8));
        assert!(live.contains("Prefill"));
        assert!(live.contains("4.0k @"));

        let completed = tracker.format_completed_prefill(4000, 0, Duration::from_secs_f64(1.6));
        assert!(completed.contains("TTFT 1.60s"));
        assert!(completed.contains("prefill"));
    }
}
