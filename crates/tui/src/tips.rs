//! Tips shown in the footer, one at a time.

pub static TIPS_POOL: &[&str] = &[
    "Shift+Tab cycles the permission mode: Planning, Manual, Accept Edits, Accept All.",
    "Click a thought or a tool call to open or fold just that one; F2 does them all.",
    "Press Enter while the agent is working to steer it without stopping the turn.",
    "Esc interrupts a running turn and keeps everything it already did.",
    "Ctrl+Z takes back an attached image before it is sent.",
    "Drop an image file on the window to attach it to the next message.",
    "Ctrl+E writes the prompt in your external editor.",
    "Ctrl+U checks for an update, downloads it and shows the progress.",
    "/goal <task> runs on its own; its step, time and token limits are in Settings \u{2192} Goal.",
    "A /goal ends with a report of what changed, and says plainly when it is unfinished.",
    "/compact summarizes older turns to free context; /compact <focus> says what to keep.",
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
    "flashagent --setup reruns the setup; a new server is saved beside the others.",
    "/provider switches to another saved provider, local or cloud, and the conversation goes on.",
    "flashagent -y skips the directory trust question for trusted projects.",
    "flashagent --url <endpoint> talks to another server for one run and saves nothing about it.",
    "The line under the input shows what the agent is doing right now.",
    "Every tool call carries a one-line header from the model saying what it is for.",
    "A declined tool call is not retried; the model is told to ask you instead.",
    "Pictures cost tokens: the attachment label shows the measured cost for this model.",
];

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

/// How long a tip stays: 10 s at the 80 ms tick.
const HOLD_TICKS: usize = 125;
/// How long a new tip takes to fade in, with motion on.
const FADE_TICKS: usize = 5;
/// "  Tip: " before the text, and the columns the footer leaves free on the right.
const LABEL_COLS: usize = 7;
const RIGHT_MARGIN: usize = 2;
const TEXT_RGB: (u8, u8, u8) = (175, 170, 160);
/// Where the fade starts: close to the background, not black.
const FADED_RGB: (u8, u8, u8) = (70, 68, 64);

/// The tip under the prompt. Each is shown whole and replaced whole: a tip
/// typed out and erased letter by letter left half words and a second,
/// orange cursor on screen, and a bare "Tip:" between two tips.
#[derive(Debug, Clone)]
pub struct TipAnimator {
    pub tip_text: &'static str,
    /// Ticks since this tip appeared.
    age: usize,
    deck: Vec<usize>,
    deck_idx: usize,
    seed: u64,
    /// Width and rows the footer has for tips, once known: a tip that does
    /// not fit them is passed over rather than cut.
    room: Option<(usize, usize)>,
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
        Self { tip_text: TIPS_POOL[deck[0]], age: 0, deck, deck_idx: 0, seed, room: None }
    }

    /// One tick is 80 ms. Tips change with motion off too; only the fade goes.
    pub fn tick(&mut self) {
        self.age += 1;
        if self.age >= HOLD_TICKS {
            self.advance();
        }
    }

    /// The footer's room for tips. A tip that no longer fits gives way at once.
    pub fn fit_to(&mut self, width: usize, rows: usize) {
        if self.room == Some((width, rows)) {
            return;
        }
        self.room = Some((width, rows));
        if self.passes_over() && !fits(self.tip_text, width, rows) {
            self.advance();
        }
    }

    /// Whether tips that do not fit are skipped: only while at least two fit.
    /// One tip for good is no tip at all, so a narrower footer shows every
    /// tip, shortened at a word where it must be.
    fn passes_over(&self) -> bool {
        self.room.is_some_and(|(width, rows)| TIPS_POOL.iter().filter(|t| fits(t, width, rows)).nth(1).is_some())
    }

    /// The next tip, passing over those that do not fit (see `passes_over`).
    fn advance(&mut self) {
        let pass_over = self.passes_over();
        for _ in 0..self.deck.len() {
            self.deck_idx += 1;
            if self.deck_idx >= self.deck.len() {
                self.deck_idx = 0;
                shuffle_deck(&mut self.deck, &mut self.seed);
                if self.deck.len() > 1 && TIPS_POOL[self.deck[0]] == self.tip_text {
                    self.deck.swap(0, 1);
                }
            }
            if !pass_over || self.room.is_none_or(|(width, rows)| fits(TIPS_POOL[self.deck[self.deck_idx]], width, rows)) {
                break;
            }
        }
        self.tip_text = TIPS_POOL[self.deck[self.deck_idx]];
        self.age = 0;
    }

    fn text_color(&self) -> String {
        let (r, g, b) = if crate::anim::enabled() && self.age < FADE_TICKS {
            let k = (self.age + 1) as f32 / (FADE_TICKS + 1) as f32;
            let mix = |from: u8, to: u8| (from as f32 + (to as f32 - from as f32) * k).round() as u8;
            (mix(FADED_RGB.0, TEXT_RGB.0), mix(FADED_RGB.1, TEXT_RGB.1), mix(FADED_RGB.2, TEXT_RGB.2))
        } else {
            TEXT_RGB
        };
        format!("\x1b[38;2;{r};{g};{b}m")
    }

    /// At most `max_lines` rows, each at most `width - 2` columns, and as many
    /// rows for every tip at a given width: a footer that grew and shrank with
    /// the tip moved the whole conversation above it. Extra rows in a short
    /// window belong to the conversation.
    pub fn render_lines(&self, width: usize, max_lines: usize) -> Vec<String> {
        let room = text_room(width);
        let color = self.text_color();
        let label = "  \x1b[1;38;2;225;175;95mTip:\x1b[0m ";
        if !two_rows(width, max_lines) {
            return vec![format!("{label}{color}{}\x1b[0m", shorten(self.tip_text, room))];
        }
        let (line1, line2) = split_tip_at_word_boundary(self.tip_text, room);
        let second = if line2.is_empty() { String::new() } else { format!("{}{color}{}\x1b[0m", " ".repeat(LABEL_COLS), shorten(line2, room)) };
        vec![format!("{label}{color}{line1}\x1b[0m"), second]
    }
}

/// Characters of tip text a row holds.
fn text_room(width: usize) -> usize {
    width.saturating_sub(LABEL_COLS + RIGHT_MARGIN)
}

/// Two rows at this width if any tip needs them and the window has them.
fn two_rows(width: usize, max_lines: usize) -> bool {
    let longest = TIPS_POOL.iter().map(|t| t.chars().count()).max().unwrap_or(0);
    max_lines >= 2 && width >= 30 && longest > text_room(width)
}

fn fits(tip: &str, width: usize, rows: usize) -> bool {
    let room = text_room(width);
    if !two_rows(width, rows) {
        return tip.chars().count() <= room;
    }
    split_tip_at_word_boundary(tip, room).1.chars().count() <= room
}

/// Ended at a word and marked, never cut mid-word ("...to configur").
fn shorten(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    let head: String = text.chars().take(room.saturating_sub(1)).collect();
    let cut = match head.rfind(' ') {
        Some(i) if i >= room / 2 => head[..i].trim_end_matches([',', ';', ':']).to_string(),
        _ => head,
    };
    format!("{cut}\u{2026}")
}

/// Line 1 gets at most `budget1` visible chars.
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
        // A tip naming a command FlashAgent does not have (/quit) is worse than none.
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
    }


    fn visible(line: &str) -> String {
        crate::text::strip_ansi(line)
    }

    #[test]
    fn a_tip_is_shown_whole_and_replaced_whole() {
        let mut anim = TipAnimator::new();
        let first = anim.tip_text;
        for _ in 1..HOLD_TICKS {
            anim.tick();
            assert_eq!(anim.tip_text, first, "the tip changed before its time was up");
            let row = visible(&anim.render_lines(200, 2).concat());
            assert_eq!(row, format!("  Tip: {first}"), "a tip is never shown in part");
        }
        anim.tick();
        assert_ne!(anim.tip_text, first);
    }

    #[test]
    fn test_shuffled_deck_uniqueness() {
        let mut anim = TipAnimator::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(anim.tip_text);

        // A shuffled deck shows every tip once before any repeats.
        for _ in 1..TIPS_POOL.len() {
            anim.advance();
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
        assert!(!l2.starts_with(' '));
        let rejoined = format!("{l1} {l2}");
        assert_eq!(rejoined, long);

        let no_spaces = "Supercalifragilisticexpialidocious";
        let (l1, l2) = split_tip_at_word_boundary(no_spaces, 10);
        assert_eq!(l1, "Supercalif");
        assert_eq!(l2, "ragilisticexpialidocious");
    }

    #[test]
    fn a_long_tip_takes_a_second_row_indented_under_the_first() {
        let mut anim = TipAnimator::new();
        anim.tip_text = "Document file storage helpers the non-obvious design constraints and invariants in code comments.";
        let wide = anim.render_lines(200, 2);
        assert_eq!(wide.len(), 1);
        let rows = anim.render_lines(70, 2);
        assert_eq!(rows.len(), 2);
        assert!(visible(&rows[0]).starts_with("  Tip: Document"));
        assert!(visible(&rows[1]).starts_with("       "));
        assert_eq!(format!("{} {}", visible(&rows[0])["  Tip: ".len()..].trim_end(), visible(&rows[1]).trim()), anim.tip_text);
    }

    #[test]
    fn no_row_is_wider_than_the_footer_leaves_room_for() {
        let mut anim = TipAnimator::new();
        for width in [30, 44, 60, 70, 80, 100, 200] {
            for rows in [1, 2] {
                for tip in TIPS_POOL {
                    anim.tip_text = tip;
                    for line in anim.render_lines(width, rows) {
                        let shown = visible(&line);
                        assert!(shown.chars().count() <= width - RIGHT_MARGIN, "{width}x{rows}: {shown:?}");
                        assert!(!shown.contains('▌'), "a caret beside the real one");
                    }
                }
            }
        }
    }

    #[test]
    fn a_tip_too_long_for_the_room_is_passed_over_not_cut() {
        let mut anim = TipAnimator::new();
        anim.fit_to(80, 1);
        for _ in 0..TIPS_POOL.len() * 2 {
            let row = visible(&anim.render_lines(80, 1)[0]);
            assert!(!row.contains('\u{2026}'), "cut instead of passed over: {row:?}");
            assert!(row.ends_with(anim.tip_text), "{row:?}");
            anim.advance();
        }
        // Too narrow for more than one tip: every tip comes round, ended at a word and marked.
        anim.fit_to(60, 1);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..TIPS_POOL.len() {
            let row = visible(&anim.render_lines(60, 1)[0]);
            if row.ends_with('\u{2026}') {
                let before = row.trim_end_matches('\u{2026}');
                assert!(anim.tip_text.starts_with(before.trim_start_matches("  Tip: ")), "{row:?}");
                assert!(anim.tip_text[before.len() - "  Tip: ".len()..].starts_with([' ', ',', ';', ':']), "cut mid-word: {row:?}");
            }
            seen.insert(anim.tip_text);
            anim.advance();
        }
        assert!(seen.len() > 1, "one tip for good");
    }

    #[test]
    fn a_new_tip_fades_in_and_then_holds_its_colour() {
        let mut anim = TipAnimator::new();
        anim.advance();
        let fading = anim.render_lines(200, 1)[0].clone();
        for _ in 0..FADE_TICKS {
            anim.tick();
        }
        let settled = anim.render_lines(200, 1)[0].clone();
        assert_eq!(visible(&fading), visible(&settled), "only the colour changes");
        if crate::anim::enabled() {
            assert_ne!(fading, settled);
        }
        anim.tick();
        assert_eq!(anim.render_lines(200, 1)[0], settled);
    }

    #[test]
    fn every_tip_takes_the_same_rows_at_one_width() {
        let mut anim = TipAnimator::new();
        for width in [40, 70, 100, 200] {
            let rows: std::collections::HashSet<usize> = TIPS_POOL
                .iter()
                .map(|tip| {
                    anim.tip_text = tip;
                    anim.render_lines(width, 2).len()
                })
                .collect();
            assert_eq!(rows.len(), 1, "at {width} columns the footer changes height: {rows:?}");
        }
    }
}
