//! Quick tips pool for FlashAgent TUI.
//! Contains 2,000 concise, practical, non-repeating developer tips randomly displayed in the UI.

/// Static pool of 2,000 developer tips across shortcuts, workflows, tools, git, Rust, Linux, and architecture.
pub static TIPS_POOL: &[&str] = &[
    "Tab on an empty prompt opens Settings; Shift+Tab cycles the permission mode.",
    "F1 shows where the context window is going, token by token.",
    "F2 cycles verbose mode: off, the last turn, or every thought and tool call.",
    "F3 switches model; F4 picks the thinking effort; F5 opens sampling.",
    "Ctrl+R asks for the last answer again from scratch.",
    "Press Enter while the agent is working to steer it without stopping the turn.",
    "Esc interrupts a running turn and keeps everything it already did.",
    "Ctrl+V pastes a screenshot for a vision model; Ctrl+Z takes it back.",
    "Drop an image file on the window to attach it to the next message.",
    "Ctrl+E writes the prompt in your external editor.",
    "Ctrl+U checks for an update, downloads it and shows the progress.",
    "/goal runs a task autonomously under a budget: /goal --steps 50 --time 20m <task>.",
    "A /goal ends with a report of what changed, and says plainly when it is unfinished.",
    "/compact summarises older turns to free context; /compact <focus> says what to keep.",
    "Auto-compaction fills a bigger window further: 85% of 128k, 97% of a million.",
    "/memory lists what FlashAgent remembers; d forgets a fact, e asks the model to fix one.",
    "Say \"remember that ...\" and the fact follows you into every project and model.",
    "Facts about this codebase stay in its own MEMORY.md; facts about you are global.",
    "/whatsnew shows what changed in the latest releases.",
    "/effort explains what auto effort has learned about the current model.",
    "Auto effort nudges itself by one step when a model keeps overthinking or fumbling tools.",
    "/diff shows changed tracked files; /commit <message> commits what is staged.",
    "/export md, /export html or /export jsonl writes the conversation to a file.",
    "/mcp manages Model Context Protocol servers and their tools.",
    "Put a skill in .agents/skills/<name>.md and run it with /skill:<name>.",
    "/mode planning keeps the agent read-only until you approve a plan.",
    "In Manual mode every edit and command waits for your approval card.",
    "On an approval card, a allows this kind of call for the rest of the session.",
    "/clear empties the scrollback; the conversation itself is kept.",
    "/channel stable or /channel beta picks the release channel, with a warning first.",
    "Sessions are saved on exit and come back with flashagent --resume <id>.",
    "flashagent --tool-test scores how reliably the current model drives tools.",
    "flashagent --tool-test --all-models compares every chat model the server lists.",
    "flashagent --setup reruns the first-start wizard.",
    "flashagent -y skips the directory trust question for trusted projects.",
    "flashagent --url <endpoint> --model <name> connects without touching the config.",
    "The line under the input shows what the agent is doing right now.",
    "Every tool call carries a one-line header from the model saying what it is for.",
    "A declined tool call is not retried; the model is told to ask you instead.",
    "Pictures cost tokens: the attachment label shows the measured cost for this model.",
];

/// Pick a tip from the pool given a pseudo-random seed or tick counter.
pub fn get_tip(seed: usize) -> &'static str {
    TIPS_POOL[seed % TIPS_POOL.len()]
}

/// Pick a random tip using system time with a splitmix64 scramble.
pub fn random_tip() -> &'static str {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut z = nanos.wrapping_add(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    let hash = z ^ (z >> 31);
    get_tip(hash as usize)
}

fn next_rand(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *seed
}

fn shuffle_deck(deck: &mut [usize], seed: &mut u64) {
    for i in (1..deck.len()).rev() {
        let j = (next_rand(seed) as usize) % (i + 1);
        deck.swap(i, j);
    }
}

/// Lifecycle phases for the animated tip typewriter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipPhase {
    /// Progressively revealing characters with active cursor.
    Typing,
    /// Holding full tip text with blinking cursor for 10 seconds.
    Holding,
    /// Progressively deleting characters with active cursor.
    Erasing,
    /// Brief pause before next tip starts typing.
    Pause,
}

/// Typewriter animator for rotating developer tips in the footer.
#[derive(Debug, Clone)]
pub struct TipAnimator {
    pub tip_text: &'static str,
    pub char_count: usize,
    pub total_chars: usize,
    pub phase: TipPhase,
    pub hold_ticks: usize,
    pub pause_ticks: usize,
    pub deck: Vec<usize>,
    pub deck_idx: usize,
    pub seed: u64,
}

impl Default for TipAnimator {
    fn default() -> Self {
        Self::new()
    }
}

impl TipAnimator {
    pub fn new() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e3779b97f4a7c15);
        let mut seed = nanos ^ 0xbf58476d1ce4e5b9;

        let mut deck: Vec<usize> = (0..TIPS_POOL.len()).collect();
        shuffle_deck(&mut deck, &mut seed);

        let initial_idx = deck[0];
        let tip_text = TIPS_POOL[initial_idx];
        let total_chars = tip_text.chars().count();

        Self {
            tip_text,
            char_count: 0,
            total_chars,
            phase: TipPhase::Typing,
            hold_ticks: 0,
            pause_ticks: 0,
            deck,
            deck_idx: 0,
            seed,
        }
    }

    /// Advance typewriter animation state by one tick (80ms).
    pub fn tick(&mut self) {
        match self.phase {
            TipPhase::Typing => {
                // Type ultra-fast (4x faster): 8 characters per 80ms tick (~100 chars/sec)
                self.char_count = (self.char_count + 8).min(self.total_chars);
                if self.char_count >= self.total_chars {
                    self.phase = TipPhase::Holding;
                    self.hold_ticks = 125; // 10s hold
                }
            }
            TipPhase::Holding => {
                if self.hold_ticks > 0 {
                    self.hold_ticks -= 1;
                } else {
                    self.phase = TipPhase::Erasing;
                }
            }
            TipPhase::Erasing => {
                // Erase ultra-fast (4x faster): 12 characters per 80ms tick (~150 chars/sec)
                self.char_count = self.char_count.saturating_sub(12);
                if self.char_count == 0 {
                    self.phase = TipPhase::Pause;
                    self.pause_ticks = 4;
                }
            }
            TipPhase::Pause => {
                if self.pause_ticks > 0 {
                    self.pause_ticks -= 1;
                } else {
                    self.deck_idx += 1;
                    if self.deck_idx >= self.deck.len() {
                        self.deck_idx = 0;
                        shuffle_deck(&mut self.deck, &mut self.seed);
                        if self.deck.len() > 1 && TIPS_POOL[self.deck[0]] == self.tip_text {
                            self.deck.swap(0, 1);
                        }
                    }
                    let next = TIPS_POOL[self.deck[self.deck_idx]];
                    self.tip_text = next;
                    self.total_chars = next.chars().count();
                    self.char_count = 0;
                    self.phase = TipPhase::Typing;
                }
            }
        }
    }

    /// Returns currently visible substring and cursor visibility.
    pub fn render_state(&self, tick_n: usize) -> (String, bool) {
        let visible: String = self.tip_text.chars().take(self.char_count).collect();
        let caret = match self.phase {
            TipPhase::Typing | TipPhase::Erasing => true,
            TipPhase::Holding | TipPhase::Pause => (tick_n / 6).is_multiple_of(2),
        };
        (visible, caret)
    }

    /// Renders the animated tip text with blinking cursor for terminal display.
    pub fn render_line(&self, tick_n: usize, max_w: usize) -> String {
        let (visible, caret_on) = self.render_state(tick_n);
        let clipped = if max_w > 1 {
            let limit = max_w.saturating_sub(1);
            if visible.chars().count() > limit {
                visible.chars().take(limit).collect::<String>()
            } else {
                visible
            }
        } else {
            visible
        };
        let caret = if caret_on {
            "\x1b[1;38;2;225;175;95m▌\x1b[0m"
        } else {
            " "
        };
        format!("\x1b[38;2;175;170;160m{clipped}\x1b[0m{caret}")
    }

    /// Renders the animated tip lines with typewriter effect.
    /// In compact terminals where the tip exceeds a single line, wraps onto a second line.
    pub fn render_lines(&self, tick_n: usize, width: usize) -> Vec<String> {
        let avail1 = width.saturating_sub(8); // "  Tip: " is 7 chars + 1 char margin
        let avail2 = width.saturating_sub(8); // "       " is 7 chars indent + 1 char margin

        if width < 30 || self.total_chars <= avail1 {
            let single = self.render_line(tick_n, avail1);
            return vec![format!("  \x1b[1;38;2;225;175;95mTip:\x1b[0m {single}")];
        }

        let (line1_full, line2_full) = split_tip_at_word_boundary(self.tip_text, avail1);
        let l1_char_count = line1_full.chars().count();
        let (visible_total, caret_on) = self.render_state(tick_n);
        let caret = if caret_on {
            "\x1b[1;38;2;225;175;95m▌\x1b[0m"
        } else {
            " "
        };

        if self.char_count <= l1_char_count {
            let typed1 = visible_total;
            vec![format!("  \x1b[1;38;2;225;175;95mTip:\x1b[0m \x1b[38;2;175;170;160m{typed1}\x1b[0m{caret}")]
        } else {
            let l2_start_char = self.tip_text.chars().count().saturating_sub(line2_full.chars().count());
            let l2_typed_chars = self.char_count.saturating_sub(l2_start_char);
            let typed2: String = line2_full.chars().take(l2_typed_chars).collect();
            // A tip longer than two lines used to lose the rest mid-word
            // ("...to configur"). Cut at a word instead, and say so.
            let clipped2 = if typed2.chars().count() > avail2 {
                let room = avail2.saturating_sub(1);
                let head: String = typed2.chars().take(room).collect();
                let cut = match head.rfind(' ') {
                    Some(i) if i >= room / 2 => head[..i].to_string(),
                    _ => head,
                };
                format!("{cut}\u{2026}")
            } else {
                typed2
            };
            vec![
                format!("  \x1b[1;38;2;225;175;95mTip:\x1b[0m \x1b[38;2;175;170;160m{line1_full}\x1b[0m"),
                format!("       \x1b[38;2;175;170;160m{clipped2}\x1b[0m{caret}"),
            ]
        }
    }
}

/// Split tip text at a word boundary so that line 1 has at most `budget1` visible chars.
pub fn split_tip_at_word_boundary(text: &str, budget1: usize) -> (&str, &str) {
    if text.chars().count() <= budget1 {
        return (text, "");
    }
    let mut last_space_byte = None;
    for (current_char_count, (byte_idx, ch)) in text.char_indices().enumerate() {
        if current_char_count > budget1 {
            break;
        }
        if ch.is_whitespace() {
            last_space_byte = Some(byte_idx);
        }
    }

    if let Some(space_idx) = last_space_byte {
        let l1 = &text[..space_idx];
        let l2 = text[space_idx..].trim_start();
        (l1, l2)
    } else {
        let split_byte = text.char_indices().nth(budget1).map(|(b, _)| b).unwrap_or(text.len());
        let l1 = &text[..split_byte];
        let l2 = text[split_byte..].trim_start();
        (l1, l2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tip_is_about_something_that_exists() {
        // The old pool held two thousand tips, most of them general
        // programming trivia, and dozens named commands FlashAgent does not
        // have. A tip that sends someone looking for /quit is worse than no tip.
        let commands: std::collections::HashSet<String> = crate::autocomplete::builtin_commands()
            .into_iter()
            .map(|c| c.trigger.split_whitespace().next().unwrap_or("").to_string())
            .collect();
        let mut seen = std::collections::HashSet::new();
        for tip in TIPS_POOL {
            assert!(!tip.is_empty());
            assert!(seen.insert(*tip), "duplicate tip: {tip}");
            for word in tip.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == ':') {
                let word = word.trim_matches(|c: char| c == '"' || c == '.' || c == '(' || c == ')');
                if let Some(cmd) = word.strip_prefix('/') {
                    if cmd.is_empty() || cmd.starts_with("skill") || !cmd.chars().all(|c| c.is_ascii_lowercase()) {
                        continue;
                    }
                    assert!(commands.contains(word), "tip names a command that does not exist: {word} in {tip:?}");
                }
            }
        }
        assert!(!get_tip(0).is_empty());
        assert!(!random_tip().is_empty());
    }


    #[test]
    fn test_tip_animator_cycle() {
        let mut anim = TipAnimator::new();
        assert_eq!(anim.phase, TipPhase::Typing);
        assert_eq!(anim.char_count, 0);

        // Advance typing phase
        for _ in 0..anim.total_chars {
            if anim.phase != TipPhase::Typing {
                break;
            }
            anim.tick();
        }
        assert_eq!(anim.phase, TipPhase::Holding);
        assert_eq!(anim.char_count, anim.total_chars);

        // Advance holding phase (125 ticks)
        for _ in 0..126 {
            anim.tick();
        }
        assert_eq!(anim.phase, TipPhase::Erasing);

        // Advance erasing phase
        for _ in 0..anim.total_chars {
            if anim.phase != TipPhase::Erasing {
                break;
            }
            anim.tick();
        }
        assert_eq!(anim.phase, TipPhase::Pause);

        // Advance pause
        for _ in 0..5 {
            anim.tick();
        }
        assert_eq!(anim.phase, TipPhase::Typing);

        let rendered = anim.render_line(0, 80);
        assert!(!rendered.is_empty());
    }

    #[test]
    fn test_shuffled_deck_uniqueness() {
        let mut anim = TipAnimator::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(anim.tip_text);

        // A shuffled deck shows every tip once before any repeats; that is
        // the property, whatever the size of the pool.
        for _ in 1..TIPS_POOL.len() {
            anim.phase = TipPhase::Pause;
            anim.pause_ticks = 0;
            anim.tick();
            assert!(seen.insert(anim.tip_text), "tip repeated before the deck ran out: {}", anim.tip_text);
        }
        assert_eq!(seen.len(), TIPS_POOL.len());
    }

    #[test]
    fn test_split_tip_at_word_boundary() {
        let short = "Short tip";
        let (l1, l2) = split_tip_at_word_boundary(short, 20);
        assert_eq!(l1, "Short tip");
        assert_eq!(l2, "");

        let exact = "Exact length test";
        let (l1, l2) = split_tip_at_word_boundary(exact, exact.chars().count());
        assert_eq!(l1, exact);
        assert_eq!(l2, "");

        let long = "Document file storage helpers the non-obvious design constraints and invariants in code comments.";
        let (l1, l2) = split_tip_at_word_boundary(long, 50);
        assert!(l1.chars().count() <= 50);
        assert!(!l1.is_empty());
        assert!(!l2.is_empty());
        // Verify no space at start of line 2
        assert!(!l2.starts_with(' '));
        // Verify joined content matches original
        let rejoined = format!("{l1} {l2}");
        assert_eq!(rejoined, long);

        // Test without any spaces
        let no_spaces = "Supercalifragilisticexpialidocious";
        let (l1, l2) = split_tip_at_word_boundary(no_spaces, 10);
        assert_eq!(l1, "Supercalif");
        assert_eq!(l2, "ragilisticexpialidocious");
    }

    #[test]
    fn test_tip_animator_render_lines_compact_multiline() {
        let mut anim = TipAnimator::new();
        anim.tip_text = "Document file storage helpers the non-obvious design constraints and invariants in code comments.";
        anim.total_chars = anim.tip_text.chars().count();
        anim.phase = TipPhase::Typing;

        // Wide terminal: fits on 1 line
        let lines_wide = anim.render_lines(0, 160);
        assert_eq!(lines_wide.len(), 1);
        assert!(lines_wide[0].contains("Tip:"));

        // Compact terminal (e.g. 70 columns): exceeds 1 line
        // When typing character 0: line 1 only
        anim.char_count = 0;
        let lines_c0 = anim.render_lines(0, 70);
        assert_eq!(lines_c0.len(), 1);

        // When typing line 1: still 1 line
        anim.char_count = 20;
        let lines_c20 = anim.render_lines(0, 70);
        assert_eq!(lines_c20.len(), 1);

        // When all characters are typed (holding phase): spans 2 lines
        anim.char_count = anim.total_chars;
        anim.phase = TipPhase::Holding;
        let lines_holding = anim.render_lines(0, 70);
        assert_eq!(lines_holding.len(), 2);
        assert!(lines_holding[0].contains("Tip:"));
        assert!(lines_holding[1].starts_with("       ")); // 7 space indent

        // When erasing: line 2 erases first
        anim.phase = TipPhase::Erasing;
        anim.char_count = 20; // erased down into line 1
        let lines_erasing = anim.render_lines(0, 70);
        assert_eq!(lines_erasing.len(), 1); // the second line has been erased
    }
}
