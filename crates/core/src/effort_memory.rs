//! Per-model correction of auto effort, learned from how turns went. Auto
//! guesses difficulty before the model has answered, so it cannot know that a
//! given model overthinks one-liners or fumbles tools without room. The nudge
//! is at most one preset step, needs a few consistent turns, and decays back
//! to neutral when turns stop complaining.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const LEARNING_RATE: f32 = 0.3;
const TRIGGER: f32 = 0.4;
const MIN_SAMPLES: u32 = 3;
/// How strongly an unremarkable turn pulls the score back to neutral.
const DECAY: f32 = 0.75;
/// A turn that took this much longer than this model's own usual, and still
/// came out right, is the model thinking more than the job needed. The first
/// turns of any model have no baseline yet, so nothing is claimed from them.
const SLOW_FACTOR: f32 = 2.0;
/// Below this a turn is too short to time meaningfully (a local model can
/// answer a one-liner in a fraction of a second).
const MIN_BASELINE_SECS: f32 = 0.5;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnOutcome {
    pub reasoning_chars: usize,
    pub answer_chars: usize,
    pub tool_calls: usize,
    pub failed_tools: usize,
    pub interrupted_while_thinking: bool,
    /// The user asked for the answer again.
    pub regenerated: bool,
    /// Wall-clock seconds the turn took, once the model had a token to measure
    /// against. `None` until the first real turn is finished.
    pub secs: Option<f32>,
    /// What the model generated, as the server counted it.
    pub completion_tokens: usize,
}

impl TurnOutcome {
    /// `+1` wants more thinking, `-1` less, `0` says nothing on its own. A turn
    /// that both failed a tool and ran long counts as a failure: a wrong answer
    /// costs more than a slow one.
    pub fn signal(&self) -> f32 {
        if self.failed_tools > 0 || self.regenerated {
            return 1.0;
        }
        if self.interrupted_while_thinking {
            return -1.0;
        }
        // Overthinking: a lot of reasoning, a short answer, no tool calls.
        let long_reasoning = self.reasoning_chars > 2_000;
        let tiny_answer = self.answer_chars * 4 < self.reasoning_chars;
        if long_reasoning && tiny_answer && self.tool_calls == 0 {
            return -1.0;
        }
        0.0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelBias {
    /// In `[-1, 1]`.
    #[serde(default)]
    pub score: f32,
    #[serde(default)]
    pub samples: u32,
    /// What this model usually spends on a turn, learned from its own past
    /// turns. A turn is only "slow" against this, never against a constant:
    /// a 2 minute turn is normal for one model and a bug in another.
    #[serde(default)]
    pub avg_secs: Option<f64>,
    /// The same, for what it generates. Recorded but not judged on yet: token
    /// counts swing with the size of the answer, and time is the steadier
    /// signal. Kept so the question can be asked of real numbers later.
    #[serde(default)]
    pub avg_tokens: Option<f64>,
}

impl ModelBias {
    /// `-1`, `0` or `+1`.
    pub fn steps(&self) -> i8 {
        if self.samples < MIN_SAMPLES {
            return 0;
        }
        if self.score >= TRIGGER {
            1
        } else if self.score <= -TRIGGER {
            -1
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EffortMemory {
    #[serde(default)]
    models: BTreeMap<String, ModelBias>,
}

impl EffortMemory {
    pub fn default_path() -> Option<PathBuf> {
        // Tests must never touch the real file.
        if let Ok(exe) = std::env::current_exe() {
            let exe = exe.to_string_lossy().to_string();
            if exe.contains("/deps/") || exe.contains("\\deps\\") {
                return Some(std::env::temp_dir().join("flashagent_test_effort.json"));
            }
        }
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .map(|h| PathBuf::from(h).join(".flashagent").join("effort.json"))
    }

    /// A missing or broken file means nothing has been learned yet.
    pub fn load() -> Self {
        Self::default_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    }

    /// Best effort.
    pub fn save(&self) {
        let Some(path) = Self::default_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn steps(&self, model: &str) -> i8 {
        self.models.get(model).map(ModelBias::steps).unwrap_or(0)
    }

    pub fn bias(&self, model: &str) -> Option<&ModelBias> {
        self.models.get(model)
    }

    /// What this turn says about the level that was used, judged against the
    /// model's own history rather than a constant: a turn that took twice this
    /// model's usual and still came out right did not need the thinking.
    fn slow_but_right(baseline: Option<f64>, outcome: &TurnOutcome, signal: f32) -> f32 {
        if signal != 0.0 {
            return signal;
        }
        let (Some(secs), Some(avg)) = (outcome.secs, baseline) else {
            return 0.0;
        };
        if secs > MIN_BASELINE_SECS && avg >= MIN_BASELINE_SECS as f64 && secs as f64 > avg * SLOW_FACTOR as f64 {
            return -1.0;
        }
        0.0
    }

    /// Folds one turn into what is known about the model. Returns the nudge in
    /// preset steps to apply from the next turn on.
    pub fn observe(&mut self, model: &str, outcome: &TurnOutcome) -> i8 {
        if model.is_empty() {
            return 0;
        }
        // Read the baseline before borrowing the map mutably.
        let baseline = self.models.get(model).map(|b| b.avg_secs).unwrap_or(None);
        let entry = self.models.entry(model.to_string()).or_default();
        let signal = Self::slow_but_right(baseline, outcome, outcome.signal());
        if signal == 0.0 {
            // Nothing to complain about: drift back towards neutral.
            entry.score *= DECAY;
        } else {
            entry.score += (signal - entry.score) * LEARNING_RATE;
        }
        entry.score = entry.score.clamp(-1.0, 1.0);
        entry.samples = entry.samples.saturating_add(1);
        // The baseline learns only from turns that raised no complaint. If
        // every slow turn pulled the average up towards itself, the model would
        // be excused from the very slowness being measured, and "slow" would
        // mean nothing after a few turns.
        if signal == 0.0 {
            if let Some(secs) = outcome.secs.filter(|s| *s > 0.0) {
                entry.avg_secs = Some(match entry.avg_secs {
                    Some(avg) => avg + (secs as f64 - avg) * LEARNING_RATE as f64,
                    None => secs as f64,
                });
            }
        }
        if outcome.completion_tokens > 0 {
            let t = outcome.completion_tokens as f64;
            entry.avg_tokens = Some(match entry.avg_tokens {
                Some(avg) => avg + (t - avg) * LEARNING_RATE as f64,
                None => t,
            });
        }
        entry.steps()
    }

    pub fn explain(&self, model: &str) -> Option<String> {
        let bias = self.models.get(model)?;
        if bias.samples == 0 {
            return None;
        }
        let turns = if bias.samples == 1 { "turn" } else { "turns" };
        // The count is the threshold, not the sample ("1 of 3 turn" was wrong).
        // The menu is already about this model, so the name is omitted.
        Some(match bias.steps() {
            1 => format!("one step up, after {} {turns}", bias.samples),
            -1 => format!("one step down, after {} {turns}", bias.samples),
            _ if bias.samples < MIN_SAMPLES => {
                format!("still watching ({} of {MIN_SAMPLES} turns)", bias.samples)
            }
            _ => format!("nothing to correct, after {} {turns}", bias.samples),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_turn() -> TurnOutcome {
        TurnOutcome { tool_calls: 1, failed_tools: 1, answer_chars: 200, ..Default::default() }
    }

    fn overthought_turn() -> TurnOutcome {
        TurnOutcome { reasoning_chars: 6_000, answer_chars: 40, ..Default::default() }
    }

    fn ordinary_turn() -> TurnOutcome {
        TurnOutcome { reasoning_chars: 400, answer_chars: 900, tool_calls: 2, ..Default::default() }
    }

    /// A turn that went fine and took `secs`. Nothing about it is wrong: the
    /// only thing to notice is that it was slow.
    fn slow_but_right(secs: f32) -> TurnOutcome {
        TurnOutcome {
            reasoning_chars: 400,
            answer_chars: 900,
            tool_calls: 1,
            secs: Some(secs),
            completion_tokens: 900,
            ..Default::default()
        }
    }

    #[test]
    fn a_turn_slower_than_this_models_own_usual_counts_as_overthinking() {
        let mut mem = EffortMemory::default();
        // Teach it what "normal" means for this model: four quick turns.
        for _ in 0..4 {
            mem.observe("m", &slow_but_right(2.0));
        }
        assert_eq!(mem.steps("m"), 0, "nothing to complain about yet");
        // Now the same model takes four times as long and is still right.
        for _ in 0..4 {
            mem.observe("m", &slow_but_right(9.0));
        }
        assert_eq!(mem.steps("m"), -1, "right answer, four times the time: less thinking was needed");
    }

    #[test]
    fn a_model_with_no_baseline_is_not_judged_on_time_yet() {
        let mut mem = EffortMemory::default();
        // The first turn of anything is slow because everything is cold: it
        // must not immediately teach the model to think less.
        for _ in 0..6 {
            mem.observe("m", &slow_but_right(120.0));
        }
        assert_eq!(mem.steps("m"), 0, "one sample cannot be a baseline, and 120s may be the whole task");
    }

    #[test]
    fn being_slow_for_yourself_depends_on_the_model_not_on_a_constant() {
        // The same 20s turn is a triumph for a model whose usual is 1s and
        // nothing at all for one whose usual is 30s.
        let mut quick = EffortMemory::default();
        for _ in 0..4 {
            quick.observe("fast", &slow_but_right(1.0));
        }
        for _ in 0..4 {
            quick.observe("fast", &slow_but_right(20.0));
        }
        assert_eq!(quick.steps("fast"), -1);

        let mut slow = EffortMemory::default();
        for _ in 0..4 {
            slow.observe("slow", &slow_but_right(30.0));
        }
        for _ in 0..4 {
            slow.observe("slow", &slow_but_right(20.0));
        }
        assert_eq!(slow.steps("slow"), 0, "faster than usual is not a complaint");
    }

    #[test]
    fn a_failed_turn_outranks_a_slow_one() {
        let mut mem = EffortMemory::default();
        for _ in 0..4 {
            mem.observe("m", &slow_but_right(2.0));
        }
        for _ in 0..4 {
            let mut bad = slow_but_right(30.0);
            bad.failed_tools = 1;
            mem.observe("m", &bad);
        }
        assert_eq!(mem.steps("m"), 1, "a wrong answer costs more than a slow one");
    }

    #[test]
    fn one_slow_outlier_does_not_become_the_baseline() {
        let mut mem = EffortMemory::default();
        for _ in 0..4 {
            mem.observe("m", &slow_but_right(2.0));
        }
        // A single stall teaches the average, but not enough to call the next
        // normal turn slow.
        mem.observe("m", &slow_but_right(60.0));
        for _ in 0..3 {
            mem.observe("m", &slow_but_right(2.0));
        }
        assert_eq!(mem.steps("m"), 0, "the stall itself is what was learned from, not the turns after it");
    }

    #[test]
    fn one_bad_turn_is_not_enough_to_move_anything() {
        // A single slow answer is noise.
        let mut mem = EffortMemory::default();
        assert_eq!(mem.observe("m", &overthought_turn()), 0);
        assert_eq!(mem.observe("m", &overthought_turn()), 0);
        assert_eq!(mem.steps("m"), 0);
    }

    #[test]
    fn a_model_that_keeps_overthinking_is_turned_down_one_step() {
        let mut mem = EffortMemory::default();
        for _ in 0..6 {
            mem.observe("m", &overthought_turn());
        }
        assert_eq!(mem.steps("m"), -1);
    }

    #[test]
    fn a_model_that_keeps_failing_tools_is_given_more_room() {
        let mut mem = EffortMemory::default();
        for _ in 0..6 {
            mem.observe("m", &failed_turn());
        }
        assert_eq!(mem.steps("m"), 1);
    }

    #[test]
    fn the_correction_fades_once_the_turns_stop_complaining() {
        let mut mem = EffortMemory::default();
        for _ in 0..6 {
            mem.observe("m", &overthought_turn());
        }
        assert_eq!(mem.steps("m"), -1);
        for _ in 0..6 {
            mem.observe("m", &ordinary_turn());
        }
        assert_eq!(mem.steps("m"), 0, "a correction has to be able to expire");
    }

    #[test]
    fn the_correction_never_goes_past_one_step() {
        let mut mem = EffortMemory::default();
        for _ in 0..200 {
            mem.observe("m", &failed_turn());
        }
        assert_eq!(mem.steps("m"), 1);
        assert!(mem.bias("m").unwrap().score <= 1.0);
    }

    #[test]
    fn models_are_learned_about_separately() {
        let mut mem = EffortMemory::default();
        for _ in 0..6 {
            mem.observe("slow", &overthought_turn());
            mem.observe("shaky", &failed_turn());
        }
        assert_eq!(mem.steps("slow"), -1);
        assert_eq!(mem.steps("shaky"), 1);
        assert_eq!(mem.steps("never-seen"), 0);
    }

    #[test]
    fn a_failed_tool_outweighs_a_long_think() {
        // The wrong answer is the signal that wins.
        let mixed = TurnOutcome {
            reasoning_chars: 9_000,
            answer_chars: 10,
            tool_calls: 1,
            failed_tools: 1,
            ..Default::default()
        };
        assert_eq!(mixed.signal(), 1.0);
    }

    #[test]
    fn a_long_think_that_did_work_is_not_overthinking() {
        let worked = TurnOutcome {
            reasoning_chars: 9_000,
            answer_chars: 20,
            tool_calls: 4,
            ..Default::default()
        };
        assert_eq!(worked.signal(), 0.0, "it thought hard and then did something");
    }

    #[test]
    fn what_was_learned_survives_a_restart() {
        let mut mem = EffortMemory::default();
        for _ in 0..6 {
            mem.observe("m", &failed_turn());
        }
        let json = serde_json::to_string(&mem).unwrap();
        let back: EffortMemory = serde_json::from_str(&json).unwrap();
        assert_eq!(back.steps("m"), 1);
        assert_eq!(back, mem);
    }

    #[test]
    fn a_broken_file_costs_nothing() {
        assert!(serde_json::from_str::<EffortMemory>("{ not json").is_err());
        let empty: EffortMemory = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.steps("m"), 0);
    }

    #[test]
    fn the_explanation_says_what_is_going_on() {
        let mut mem = EffortMemory::default();
        assert_eq!(mem.explain("m"), None, "nothing learned, nothing claimed");
        mem.observe("m", &failed_turn());
        assert!(mem.explain("m").unwrap().contains("still watching"));
        for _ in 0..6 {
            mem.observe("m", &failed_turn());
        }
        assert!(mem.explain("m").unwrap().contains("one step up"));
    }
}
