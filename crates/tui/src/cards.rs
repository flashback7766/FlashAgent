use super::*;
use flashagent_tui::WelcomeCard;

impl App {
    /// Only while the conversation has not started.
    pub(crate) fn refresh_welcome(&mut self, source: &BackendSource, mood: MascotMood) {
        self.animate_welcome(source, mood, None, None);
    }

    /// `width` overrides the terminal's; `reveal_rows` draws only that many rows
    /// for the start-up reveal.
    pub(crate) fn animate_welcome(
        &mut self,
        source: &BackendSource,
        mood: MascotMood,
        width: Option<usize>,
        reveal_rows: Option<usize>,
    ) {
        if self.chat.has_user_message() {
            return;
        }
        let (w, h) = crossterm::terminal::size().unwrap_or((100, 24));
        let th_sum = thinking_summary_str(source, &self.current_effort);
        let card = welcome_card(&WelcomeCard {
            model: &self.current_model,
            cwd: &self.cwd_display,
            memory_docs: self.memory_docs,
            thinking: Some(&th_sum),
            context_window: self.current_context.as_deref(),
            width: width.unwrap_or(w as usize),
            height: h as usize,
            tick: self.tick_n,
            show_mascot: self.config.show_mascot,
            mood,
        });
        let card = match reveal_rows {
            Some(rows) => revealed(card, rows),
            None => card,
        };
        // Only the rows that changed are drawn; nothing to ask for.
        self.chat.update_welcome_card(card);
    }
}

/// Animated, it starts as its top border and the tick loop draws the rest;
/// otherwise it goes up whole.
pub(crate) fn opening_card(card: Vec<RenderLine>, animate: bool) -> Vec<RenderLine> {
    if animate {
        revealed(card, 1)
    } else {
        card
    }
}

/// The first `rows` rows drawn, the rest held blank: the card is at its full
/// height from the first frame, so the composer under it does not step down
/// the screen while it appears.
pub(crate) fn revealed(card: Vec<RenderLine>, rows: usize) -> Vec<RenderLine> {
    card.into_iter()
        .enumerate()
        // A space, not nothing: an empty line wraps to no row at all.
        .map(|(i, (kind, text))| (kind, if i < rows.max(1) { text } else { " ".to_string() }))
        .collect()
}

/// `None` once the card is whole.
pub(crate) fn welcome_reveal_rows(started_at: std::time::Instant) -> Option<usize> {
    if !flashagent_tui::anim::enabled() {
        return None;
    }
    const ROW_MS: u128 = 35;
    const ROWS: u128 = 15;
    let elapsed = started_at.elapsed().as_millis();
    (elapsed < ROW_MS * ROWS).then(|| (elapsed / ROW_MS) as usize + 1)
}

/// Wrapped, never clipped; long whitespace runs are made visible so padding
/// cannot push `... | sh` out of sight; running out of rows is noted.
pub(crate) fn card_rows(value: &str, width: usize, max_rows: usize) -> Vec<String> {
    let mut shown = String::new();
    for line in value.lines() {
        if !shown.is_empty() {
            shown.push('¶'); // ¶ marks a real newline
        }
        let mut spaces = 0usize;
        for c in line.chars().chain(std::iter::once('\0')) {
            if c == ' ' {
                spaces += 1;
                continue;
            }
            if spaces > 3 {
                shown.push_str(&format!(" \u{2423}x{spaces} "));
            } else {
                shown.push_str(&" ".repeat(spaces));
            }
            spaces = 0;
            if c != '\0' {
                shown.push(c);
            }
        }
    }
    let chars: Vec<char> = shown.chars().collect();
    let mut rows: Vec<String> = chars.chunks(width.max(1)).map(|c| c.iter().collect()).collect();
    if rows.len() > max_rows {
        let hidden: usize = rows[max_rows - 1..].iter().map(|r| r.chars().count()).sum();
        rows.truncate(max_rows - 1);
        rows.push(format!("... {hidden} more characters (not shown; deny if unsure)"));
    }
    rows
}

/// Control characters, ANSI escapes included, are shown, not executed.
pub(crate) fn card_safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\n' { '\u{fffd}' } else { c })
        .collect()
}

/// From facts the loop reported, so a run stopped by a budget cannot read as
/// finished whatever the model's summary claims.
pub(crate) fn push_goal_report(chat: &mut ChatView, ledger: &GoalLedger, reason: DoneReason) {
    let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
    let card_w = (w as usize).saturating_sub(4).clamp(44, 110);
    let inner_w = card_w.saturating_sub(2);
    let border = if matches!(reason, DoneReason::Completed) {
        "\x1b[38;2;145;205;140m"
    } else {
        "\x1b[38;2;225;175;95m"
    };
    let reset = "\x1b[0m";

    let title = format!(" Goal report: {} ", flashagent_tui::truncate_middle(&ledger.task, inner_w.saturating_sub(18)));
    let dashes = inner_w.saturating_sub(flashagent_tui::visible_width(&title) + 1);
    chat.push_line(
        LineKind::System,
        format!("{border}╭─\x1b[1;38;2;245;240;232m{title}{border}{}╮{reset}", "─".repeat(dashes)),
    );
    for line in ledger.report(reason) {
        for row in wrap_plain(&card_safe(&line), inner_w.saturating_sub(2)) {
            let pad = inner_w.saturating_sub(flashagent_tui::visible_width(&row) + 1);
            chat.push_line(
                LineKind::System,
                format!("{border}│{reset} \x1b[38;2;200;196;188m{row}\x1b[0m{}{border}│{reset}", " ".repeat(pad)),
            );
        }
    }
    chat.push_line(LineKind::System, format!("{border}╰{}╯{reset}", "─".repeat(inner_w)));
}

/// Keeps a leading indent.
pub(crate) fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    if text.chars().count() <= width {
        return vec![text.to_string()];
    }
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let candidate = if row.is_empty() { word.chars().count() } else { row.chars().count() + 1 + word.chars().count() };
        if !row.is_empty() && candidate > width {
            rows.push(std::mem::take(&mut row));
            row.push_str(&indent);
            row.push_str("  ");
        }
        if !row.is_empty() && !row.ends_with(' ') {
            row.push(' ');
        }
        row.push_str(word);
        // A single word longer than the card: hard-split it.
        while row.chars().count() > width {
            let head: String = row.chars().take(width).collect();
            let tail: String = row.chars().skip(width).collect();
            rows.push(head);
            row = tail;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}
