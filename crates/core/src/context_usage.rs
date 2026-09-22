#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextUsage {
    pub system_tokens: usize,
    pub memory_tokens: usize,
    pub tools_tokens: usize,
    pub user_tokens: usize,
    pub assistant_tokens: usize,
    pub reasoning_tokens: usize,
    pub tool_output_tokens: usize,
    pub total_capacity: usize,
}

impl ContextUsage {
    pub fn new(total_capacity: usize) -> Self {
        Self {
            total_capacity: total_capacity.max(1024),
            ..Default::default()
        }
    }

    pub fn total_used(&self) -> usize {
        self.system_tokens
            + self.memory_tokens
            + self.tools_tokens
            + self.user_tokens
            + self.assistant_tokens
            + self.reasoning_tokens
            + self.tool_output_tokens
    }

    pub fn remaining(&self) -> usize {
        self.total_capacity.saturating_sub(self.total_used())
    }

    pub fn percentage(&self) -> f32 {
        if self.total_capacity == 0 {
            0.0
        } else {
            (self.total_used() as f32 / self.total_capacity as f32) * 100.0
        }
    }

    /// E.g. 5.2K instead of 5K.
    pub fn format_used_tokens(tokens: usize) -> String {
        if tokens >= 1_000_000 {
            format!("{:.1}M", tokens as f64 / 1_000_000.0)
        } else if tokens >= 1000 {
            format!("{:.1}K", tokens as f64 / 1_000.0)
        } else {
            tokens.to_string()
        }
    }

    /// E.g. 128K or 200K.
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

    /// E.g. 128K or 1.2M.
    pub fn format_tokens(tokens: usize) -> String {
        Self::format_capacity_tokens(tokens)
    }

    /// `❪▌─────────❫ 6% · 4.1K/64K`
    pub fn format_compact_gauge(&self, bar_width: usize) -> String {
        let pct = self.percentage().clamp(0.0, 100.0);
        let bar_width = bar_width.max(4);

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionInput {
    pub used: usize,
    pub capacity: usize,
    /// System prompt, memory index and tool schemas: summarising never shrinks them.
    pub fixed: usize,
    /// The next turn is assumed to need about as much.
    pub last_turn_growth: usize,
    /// Percentage of the window.
    pub threshold_pct: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionVerdict {
    No,
    NextTurnWouldNotFit,
    PastThreshold,
}

impl CompactionVerdict {
    pub fn should(self) -> bool {
        self != CompactionVerdict::No
    }
}

/// A flat percentage alone fires when the bulk is the system prompt and tool
/// schemas, which summarising cannot shrink, and waits even when the next turn
/// will clearly overflow.
pub fn should_compact(input: CompactionInput) -> CompactionVerdict {
    let CompactionInput { used, capacity, fixed, last_turn_growth, threshold_pct } = input;
    if capacity == 0 {
        return CompactionVerdict::No;
    }

    // What summarising could reclaim. Below this there is nothing to win.
    let compactable = used.saturating_sub(fixed);
    const MIN_WORTH_COMPACTING: usize = 2_000;
    if compactable < MIN_WORTH_COMPACTING {
        return CompactionVerdict::No;
    }

    // The next turn is assumed to be the size of the last, with a floor.
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
        // The next turn would run off the end even though 80% is below the threshold.
        let tight = CompactionInput { used: 80_000, last_turn_growth: 25_000, ..input() };
        assert_eq!(should_compact(tight), CompactionVerdict::NextTurnWouldNotFit);
    }

    #[test]
    fn nothing_is_compacted_when_the_bulk_cannot_be_compacted() {
        // Summarising frees nothing when the window is mostly fixed content.
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

/// Bigger windows compact later: 5% of a million tokens is plenty of room,
/// 5% of 32k is not one answer.
pub fn default_compact_threshold(capacity: usize) -> usize {
    match capacity {
        c if c >= 1_000_000 => 97,
        c if c >= 512_000 => 95,
        c if c >= 256_000 => 90,
        c if c >= 128_000 => 85,
        c if c >= 64_000 => 80,
        _ => 75,
    }
}

/// The user's value, or this window's default when it is 0.
pub fn resolved_compact_threshold(configured: usize, capacity: usize) -> usize {
    if configured == 0 {
        default_compact_threshold(capacity)
    } else {
        configured.clamp(10, 99)
    }
}

#[cfg(test)]
mod threshold_tests {
    use super::*;

    #[test]
    fn a_bigger_window_is_filled_further_before_it_is_summarised() {
        assert_eq!(default_compact_threshold(1_048_576), 97);
        assert_eq!(default_compact_threshold(524_288), 95);
        assert_eq!(default_compact_threshold(262_144), 90);
        assert_eq!(default_compact_threshold(131_072), 85);
        assert_eq!(default_compact_threshold(65_536), 80);
        assert_eq!(default_compact_threshold(32_768), 75);
        assert_eq!(default_compact_threshold(8_192), 75, "smaller still gets the tightest rule");
    }

    #[test]
    fn the_bands_are_read_from_the_window_actually_loaded() {
        // LM Studio reports 200k for a 256k-class model; the band follows what is loaded.
        assert_eq!(default_compact_threshold(204_800), 85);
        assert_eq!(default_compact_threshold(200_000), 85);
    }

    #[test]
    fn a_threshold_the_user_set_is_kept() {
        assert_eq!(resolved_compact_threshold(60, 131_072), 60);
        assert_eq!(resolved_compact_threshold(0, 131_072), 85, "zero means decide for me");
        assert_eq!(resolved_compact_threshold(150, 131_072), 99, "and nothing silly is obeyed");
        assert_eq!(resolved_compact_threshold(1, 131_072), 10);
    }
}
