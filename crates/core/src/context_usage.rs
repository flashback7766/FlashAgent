//! Context window usage accounting and visual gauge formatting.

/// Component-by-component token breakdown of the context window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextUsage {
    /// System prompt token count.
    pub system_tokens: usize,
    /// Injected memory and project context docs tokens.
    pub memory_tokens: usize,
    /// Tool definitions and JSON schemas tokens.
    pub tools_tokens: usize,
    /// User conversation prompts tokens.
    pub user_tokens: usize,
    /// Assistant response text tokens.
    pub assistant_tokens: usize,
    /// Internal reasoning / thinking tokens.
    pub reasoning_tokens: usize,
    /// Tool call outputs and results tokens.
    pub tool_output_tokens: usize,
    /// Total context window capacity of active model (e.g. 131072 for 128k).
    pub total_capacity: usize,
}

impl ContextUsage {
    pub fn new(total_capacity: usize) -> Self {
        Self {
            total_capacity: total_capacity.max(1024),
            ..Default::default()
        }
    }

    /// Sum of all tokens currently used in context.
    pub fn total_used(&self) -> usize {
        self.system_tokens
            + self.memory_tokens
            + self.tools_tokens
            + self.user_tokens
            + self.assistant_tokens
            + self.reasoning_tokens
            + self.tool_output_tokens
    }

    /// Remaining available token capacity.
    pub fn remaining(&self) -> usize {
        self.total_capacity.saturating_sub(self.total_used())
    }

    /// Used context percentage (0.0 to 100.0).
    pub fn percentage(&self) -> f32 {
        if self.total_capacity == 0 {
            0.0
        } else {
            (self.total_used() as f32 / self.total_capacity as f32) * 100.0
        }
    }

    /// Format used tokens with one-tenth-of-a-thousand precision (e.g. 5.2K instead of 5K).
    pub fn format_used_tokens(tokens: usize) -> String {
        if tokens >= 1_000_000 {
            format!("{:.1}M", tokens as f64 / 1_000_000.0)
        } else if tokens >= 1000 {
            format!("{:.1}K", tokens as f64 / 1_000.0)
        } else {
            tokens.to_string()
        }
    }

    /// Format capacity tokens with standard binary/round bounds (e.g. 128K or 200K).
    pub fn format_capacity_tokens(tokens: usize) -> String {
        if tokens >= 1_048_576 {
            let m = (tokens as f64) / 1_048_576.0;
            if (m.fract() * 10.0).round() == 0.0 {
                format!("{:.0}M", m)
            } else {
                format!("{:.1}M", m)
            }
        } else if tokens >= 1_000_000 {
            format!("{:.0}M", tokens as f64 / 1_000_000.0)
        } else if tokens >= 1024 {
            if tokens == 131_072 || tokens == 128_000 {
                "128K".to_string()
            } else if tokens == 204_800 || tokens == 200_000 {
                "200K".to_string()
            } else if tokens == 65_536 || tokens == 64_000 {
                "64K".to_string()
            } else if tokens == 32_768 || tokens == 32_000 {
                "32K".to_string()
            } else if tokens == 16_384 || tokens == 16_000 {
                "16K".to_string()
            } else if tokens == 8_192 || tokens == 8_000 {
                "8K".to_string()
            } else if tokens == 4_096 || tokens == 4_000 {
                "4K".to_string()
            } else if tokens == 2_048 || tokens == 2_000 {
                "2K".to_string()
            } else if tokens.is_multiple_of(1000) {
                format!("{}K", tokens / 1_000)
            } else if tokens.is_multiple_of(1024) {
                format!("{}K", tokens / 1024)
            } else {
                format!("{}K", (tokens + 500) / 1_000)
            }
        } else {
            tokens.to_string()
        }
    }

    /// Format tokens in human readable format (e.g. 128K or 1.2M).
    pub fn format_tokens(tokens: usize) -> String {
        Self::format_capacity_tokens(tokens)
    }

    /// Format sleek, high-precision gauge string with sub-block resolution:
    /// `❪▌─────────❫ 6% · 4.1K/64K`
    pub fn format_compact_gauge(&self, bar_width: usize) -> String {
        let pct = self.percentage().clamp(0.0, 100.0);
        let bar_width = bar_width.max(4);

        // Color coding for percentage: mint green -> warm amber -> coral warning
        let fill_color = if pct >= 90.0 {
            "\x1b[1;38;2;240;110;110m" // coral red
        } else if pct >= 75.0 {
            "\x1b[1;38;2;235;185;105m" // warm amber
        } else {
            "\x1b[38;2;135;215;165m" // mint green
        };

        let rail_color = "\x1b[38;2;60;65;78m"; // recessed dark rail
        let bracket_color = "\x1b[38;2;100;105;120m"; // subtle pill bracket
        let reset = "\x1b[0m";

        // Sub-block characters (1/8ths to 8/8ths)
        const SUB_BLOCKS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

        let total_eighths = ((pct / 100.0) * (bar_width as f32) * 8.0).round() as usize;
        let full_blocks = (total_eighths / 8).min(bar_width);
        let rem_eighths = total_eighths % 8;
        let sub_block = if full_blocks < bar_width && rem_eighths > 0 {
            SUB_BLOCKS[rem_eighths]
        } else {
            ""
        };

        let filled_slots = full_blocks + if !sub_block.is_empty() { 1 } else { 0 };
        let empty_slots = bar_width.saturating_sub(filled_slots);

        let full_str = "█".repeat(full_blocks);
        let empty_str = "─".repeat(empty_slots);

        let used_str = Self::format_used_tokens(self.total_used());
        let cap_str = Self::format_capacity_tokens(self.total_capacity);

        format!(
            "{bracket_color}❪{reset}{fill_color}{full_str}{sub_block}{reset}{rail_color}{empty_str}{reset}{bracket_color}❫{reset} \x1b[1;38;2;245;240;235m{pct:.0}%\x1b[0m \x1b[38;2;120;125;140m·\x1b[0m \x1b[38;2;160;165;180m{used_str}\x1b[38;2;110;115;130m/\x1b[38;2;160;165;180m{cap_str}\x1b[0m"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_usage_calculations() {
        let mut usage = ContextUsage::new(128_000);
        usage.system_tokens = 1_000;
        usage.memory_tokens = 2_000;
        usage.user_tokens = 5_000;
        usage.assistant_tokens = 12_000;
        usage.reasoning_tokens = 10_000;

        assert_eq!(usage.total_used(), 30_000);
        assert_eq!(usage.remaining(), 98_000);
        let pct = usage.percentage();
        assert!((pct - 23.4375).abs() < 0.1);

        let gauge = usage.format_compact_gauge(10);
        assert!(gauge.contains("23%"));
        assert!(gauge.contains("30.0K"));
        assert!(gauge.contains("128K"));
    }

    #[test]
    fn test_format_tokens_binary_and_decimal() {
        assert_eq!(ContextUsage::format_capacity_tokens(131_072), "128K");
        assert_eq!(ContextUsage::format_capacity_tokens(65_536), "64K");
        assert_eq!(ContextUsage::format_capacity_tokens(32_768), "32K");
        assert_eq!(ContextUsage::format_capacity_tokens(128_000), "128K");
        assert_eq!(ContextUsage::format_capacity_tokens(200_000), "200K");
        assert_eq!(ContextUsage::format_capacity_tokens(3_250), "3K");
        assert_eq!(ContextUsage::format_capacity_tokens(500), "500");

        assert_eq!(ContextUsage::format_used_tokens(5_230), "5.2K");
        assert_eq!(ContextUsage::format_used_tokens(5_000), "5.0K");
        assert_eq!(ContextUsage::format_used_tokens(8_400), "8.4K");
        assert_eq!(ContextUsage::format_used_tokens(500), "500");
    }
}

/// What is known when deciding whether to compact before the next turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionInput {
    /// Tokens in use right now.
    pub used: usize,
    /// Window capacity.
    pub capacity: usize,
    /// Tokens that cannot be compacted away: the system prompt, the injected
    /// memory index and the tool schemas. Summarising never touches them.
    pub fixed: usize,
    /// How much the last turn added. The next one is assumed to want about
    /// as much again.
    pub last_turn_growth: usize,
    /// The user's threshold, as a percentage of the window.
    pub threshold_pct: usize,
}

/// Whether to compact now, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionVerdict {
    /// Leave it alone.
    No,
    /// The next turn would not fit.
    NextTurnWouldNotFit,
    /// Past the threshold the user set.
    PastThreshold,
}

impl CompactionVerdict {
    pub fn should(self) -> bool {
        self != CompactionVerdict::No
    }
}

/// Decide whether the conversation should be summarised before the next turn.
///
/// A flat percentage is a poor rule on its own. It fires on a conversation
/// whose bulk is the system prompt and tool schemas — which summarising cannot
/// shrink, so the cost is the recent context and the gain is nothing — and it
/// waits for the line to be crossed even when the next turn is plainly going
/// to run past the end of the window.
pub fn should_compact(input: CompactionInput) -> CompactionVerdict {
    let CompactionInput { used, capacity, fixed, last_turn_growth, threshold_pct } = input;
    if capacity == 0 {
        return CompactionVerdict::No;
    }

    // What summarising could actually reclaim. Below this there is nothing to
    // win and a recent turn to lose.
    let compactable = used.saturating_sub(fixed);
    const MIN_WORTH_COMPACTING: usize = 2_000;
    if compactable < MIN_WORTH_COMPACTING {
        return CompactionVerdict::No;
    }

    // A turn needs room for the prompt and for the answer. Assume the next one
    // is the size of the last, with a floor for the first few turns.
    let expected = last_turn_growth.max(1_500);
    let headroom = capacity.saturating_sub(used);
    if headroom < expected {
        return CompactionVerdict::NextTurnWouldNotFit;
    }

    let pct = (used as f32 / capacity as f32) * 100.0;
    if pct >= threshold_pct as f32 {
        return CompactionVerdict::PastThreshold;
    }
    CompactionVerdict::No
}

#[cfg(test)]
mod compaction_tests {
    use super::*;

    fn input() -> CompactionInput {
        CompactionInput {
            used: 10_000,
            capacity: 100_000,
            fixed: 6_000,
            last_turn_growth: 2_000,
            threshold_pct: 90,
        }
    }

    #[test]
    fn an_early_conversation_is_left_alone() {
        assert_eq!(should_compact(input()), CompactionVerdict::No);
    }

    #[test]
    fn the_threshold_still_applies() {
        let late = CompactionInput { used: 92_000, ..input() };
        assert_eq!(should_compact(late), CompactionVerdict::PastThreshold);
    }

    #[test]
    fn a_turn_that_would_not_fit_is_not_made_to_wait_for_the_threshold() {
        // 80% used, but the last turn took 25k and there are 20k left: the
        // next one runs off the end, and finding that out mid-turn costs the
        // turn.
        let tight = CompactionInput { used: 80_000, last_turn_growth: 25_000, ..input() };
        assert_eq!(should_compact(tight), CompactionVerdict::NextTurnWouldNotFit);
    }

    #[test]
    fn nothing_is_compacted_when_the_bulk_cannot_be_compacted() {
        // A small window filled by the system prompt, memory and tool
        // schemas: summarising the three remaining messages frees nothing and
        // throws away what the model was just told.
        let mostly_fixed = CompactionInput {
            used: 15_000,
            capacity: 16_000,
            fixed: 14_000,
            last_turn_growth: 3_000,
            threshold_pct: 90,
        };
        assert_eq!(should_compact(mostly_fixed), CompactionVerdict::No);
    }

    #[test]
    fn a_conversation_with_real_history_in_a_tight_window_is_compacted() {
        let worth_it = CompactionInput {
            used: 15_000,
            capacity: 16_000,
            fixed: 6_000,
            last_turn_growth: 3_000,
            threshold_pct: 90,
        };
        assert_eq!(should_compact(worth_it), CompactionVerdict::NextTurnWouldNotFit);
    }

    #[test]
    fn a_missing_capacity_never_triggers_anything() {
        let zero = CompactionInput { capacity: 0, ..input() };
        assert_eq!(should_compact(zero), CompactionVerdict::No);
        assert!(!CompactionVerdict::No.should());
        assert!(CompactionVerdict::PastThreshold.should());
    }
}
