//! What this model actually needed, learned from how its turns went.
//!
//! Auto effort guesses the difficulty of a task from the request. The guess is
//! made before the model has said a word, so it cannot know that *this* model
//! thinks for two minutes about a one-line answer, or that it fumbles tool
//! calls unless it is given room. That only shows up afterwards, in how the
//! turn went — so the turns are watched, and the guess is nudged.
//!
//! The nudge is deliberately small: one preset step at most, only after a few
//! consistent observations, and it fades back to neutral as soon as the turns
//! stop complaining. A learned setting that cannot be walked back is worse
//! than no learning at all.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How much one turn moves the score.
const LEARNING_RATE: f32 = 0.3;
/// How far the score must lean before the effort actually moves.
const TRIGGER: f32 = 0.4;
/// Turns to watch before trusting the lean at all.
const MIN_SAMPLES: u32 = 3;
/// How strongly an unremarkable turn pulls the score back to neutral.
const DECAY: f32 = 0.75;

/// What happened during one turn, as far as effort is concerned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnOutcome {
    /// Characters of reasoning the model emitted.
    pub reasoning_chars: usize,
    /// Characters of actual answer.
    pub answer_chars: usize,
    /// Tool calls made.
    pub tool_calls: usize,
    /// Tool calls that came back as errors.
    pub failed_tools: usize,
    /// The user stopped it while it was still thinking.
    pub interrupted_while_thinking: bool,
    /// The user asked for the answer again.
    pub regenerated: bool,
}

impl TurnOutcome {
    /// Which way this turn leans: `+1` wants more thinking, `-1` less, `0`
    /// nothing to say.
    ///
    /// Only unambiguous turns vote. A turn that both failed a tool and ran
    /// long says two things at once, and the failure is the one that matters:
    /// a wrong answer costs more than a slow one.
    pub fn signal(&self) -> f32 {
        if self.failed_tools > 0 || self.regenerated {
            return 1.0;
        }
        if self.interrupted_while_thinking {
            return -1.0;
        }
        // Overthinking has a shape: a mountain of reasoning, a sentence of
        // answer, and nothing done about it.
        let long_reasoning = self.reasoning_chars > 2_000;
        let tiny_answer = self.answer_chars * 4 < self.reasoning_chars;
        if long_reasoning && tiny_answer && self.tool_calls == 0 {
            return -1.0;
        }
        0.0
    }
}

/// What has been learned about one model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelBias {
    /// Running lean, in `[-1, 1]`.
    #[serde(default)]
    pub score: f32,
    /// Turns observed.
    #[serde(default)]
    pub samples: u32,
}

impl ModelBias {
    /// Preset steps to shift auto effort by: `-1`, `0` or `+1`.
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

/// Per-model effort corrections, kept on disk between runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EffortMemory {
    #[serde(default)]
    models: BTreeMap<String, ModelBias>,
}

impl EffortMemory {
    /// Where the file lives.
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

    /// Read what was learned before. A missing or broken file simply means
    /// nothing has been learned yet — it never costs the user a session.
    pub fn load() -> Self {
        Self::default_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    }

    /// Write it back, best effort.
    pub fn save(&self) {
        let Some(path) = Self::default_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// The correction to apply to this model's auto effort right now.
    pub fn steps(&self, model: &str) -> i8 {
        self.models.get(model).map(ModelBias::steps).unwrap_or(0)
    }

    /// What is known about this model, for the user to read.
    pub fn bias(&self, model: &str) -> Option<&ModelBias> {
        self.models.get(model)
    }

    /// Fold one turn into what is known, and report the correction after it.
    pub fn observe(&mut self, model: &str, outcome: &TurnOutcome) -> i8 {
        if model.is_empty() {
            return 0;
        }
        let entry = self.models.entry(model.to_string()).or_default();
        let signal = outcome.signal();
        if signal == 0.0 {
            // Nothing to complain about: drift back towards neutral, so a
            // correction earned last week does not outlive its reason.
            entry.score *= DECAY;
        } else {
            entry.score += (signal - entry.score) * LEARNING_RATE;
        }
        entry.score = entry.score.clamp(-1.0, 1.0);
        entry.samples = entry.samples.saturating_add(1);
        entry.steps()
    }

    /// One line saying what was learned, or `None` when nothing has been.
    pub fn explain(&self, model: &str) -> Option<String> {
        let bias = self.models.get(model)?;
        if bias.samples == 0 {
            return None;
        }
        let turns = if bias.samples == 1 { "turn" } else { "turns" };
        // Said in a menu that is already about this model, so the name would
        // only push the useful half off the edge of the box.
        Some(match bias.steps() {
            1 => format!("one step up, after {} {turns}", bias.samples),
            -1 => format!("one step down, after {} {turns}", bias.samples),
            _ if bias.samples < MIN_SAMPLES => {
                format!("still watching ({} of {MIN_SAMPLES} {turns})", bias.samples)
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

    #[test]
    fn one_bad_turn_is_not_enough_to_move_anything() {
        // A single slow answer is noise; changing the model's behaviour on it
        // would make the app feel unpredictable.
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
        // Both are true of the same turn; the wrong answer is the one worth
        // spending time on.
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
