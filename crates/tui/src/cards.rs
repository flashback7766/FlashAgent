use super::*;
use flashagent_tui::WelcomeCard;

impl App {
    /// Only while the conversation has not started.
    pub(crate) fn refresh_welcome(&mut self, source: &BackendSource, mood: MascotMood) {
        self.animate_welcome(source, mood, None, None);
    }

    /// The model, the provider or the level changed after the conversation
    /// started: the card at the top says what runs now. Before the first
    /// message it is the same card, drawn as usual.
    pub(crate) fn refresh_welcome_state(&mut self, source: &BackendSource, mood: MascotMood) {
        if !self.chat.has_user_message() {
            self.animate_welcome(source, mood, None, None);
            return;
        }
        let card = self.welcome_lines(source, mood, None);
        self.chat.update_welcome_card_state(card);
    }

    /// The card as it stands now: model, provider, level and the rest.
    fn welcome_lines(&self, source: &BackendSource, mood: MascotMood, width: Option<usize>) -> flashagent_tui::WelcomeRender {
        let (w, h) = crossterm::terminal::size().unwrap_or((100, 24));
        let th_sum = thinking_summary_str(source, &self.current_effort);
        welcome_card(&WelcomeCard {
            model: &self.current_model,
            provider: Some(&self.config.active_profile().name),
            cwd: &self.cwd_display,
            memory_docs: self.memory_docs,
            thinking: Some(&th_sum),
            context_window: self.current_context.as_deref(),
            width: width.unwrap_or(w as usize),
            height: h as usize,
            tick: self.tick_n,
            show_mascot: self.config.show_mascot,
            mood,
        })
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
        let card = self.welcome_lines(source, mood, width);
        let lines = match reveal_rows {
            Some(rows) => revealed(card.lines, rows),
            None => card.lines,
        };
        // Only the rows that changed are drawn; nothing to ask for.
        self.chat.update_welcome_card(flashagent_tui::WelcomeRender { lines, zones: card.zones });
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
                shown.push_str(&format!(" \u{b7}x{spaces} "));
            } else {
                shown.push_str(&" ".repeat(spaces));
            }
            spaces = 0;
            if c != '\0' {
                shown.push(c);
            }
        }
    }
    // By cells, as the row is clipped: split by characters, a wide character
    // made rows twice as wide as the card and the rest of each row was cut off,
    // hiding part of the command being approved.
    let mut rows = split_cells(&shown, width.max(1));
    if rows.len() > max_rows {
        let hidden: usize = rows[max_rows - 1..].iter().map(|r| r.chars().count()).sum();
        rows.truncate(max_rows - 1);
        rows.push(format!(
            "\u{2026} {} {}",
            flashagent_tui::text::plural(hidden, "more character", "more characters"),
            flashagent_tui::i18n::tr("(not shown; deny if unsure)")
        ));
    }
    rows
}

/// Control characters, ANSI escapes included, are shown, not executed: in
/// caret notation as `cat -v` writes them, so ESC reads `^[`.
pub(crate) fn card_safe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if !c.is_control() || c == '\n' {
            out.push(c);
            continue;
        }
        let code = c as u32;
        if code >= 0x80 {
            out.push_str("M-");
        }
        out.push('^');
        out.push(match code & 0x7f {
            0x7f => '?',
            low => char::from_u32(low + 0x40).unwrap_or('?'),
        });
    }
    out
}

/// From facts the loop reported, so a run stopped by a budget cannot read as
/// finished whatever the model's summary claims.
pub(crate) fn push_goal_report(chat: &mut ChatView, ledger: &GoalLedger, reason: DoneReason) {
    let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
    let card_w = (w as usize).saturating_sub(4).min(110);
    let inner_w = card_w.saturating_sub(2);
    let border = if matches!(reason, DoneReason::Completed) {
        "\x1b[38;2;145;205;140m"
    } else {
        "\x1b[38;2;225;175;95m"
    };
    let reset = "\x1b[0m";

    let title = format!(
        " {} ",
        flashagent_tui::i18n::tr_fill("Goal report: {task}", &[("task", &flashagent_tui::truncate_middle(&ledger.task, inner_w.saturating_sub(18)))])
    );
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

fn cells(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// Rows of at most `width` cells, never splitting a character.
fn split_cells(text: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    for g in text.graphemes(true) {
        let w = cells(g);
        if used + w > width && !row.is_empty() {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push_str(g);
        used += w;
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Keeps a leading indent on every row, and the spaces inside a line.
/// Measured in cells: counted in characters, a Japanese question ran past the
/// card and its end was cut off. The indent comes out of the width, so a row
/// is never `indent + text` cells wide.
pub(crate) fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    if cells(text) <= width {
        return vec![text.to_string()];
    }
    let leading: String = text.chars().take_while(|c| *c == ' ').collect();
    // A indent wider than the widest row leaves no room for text at all.
    let indent: String = leading.chars().take(width.saturating_sub(1)).collect();
    let body = &text[leading.len()..];
    let indent_w = cells(&indent);
    let room = width.saturating_sub(indent_w).max(1);

    // Runs of whitespace and runs of everything else, in order: a word is not
    // re-spaced, so `a    b` stays four spaces apart while it fits.
    let mut tokens: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut in_space: Option<bool> = None;
    for (i, c) in body.char_indices() {
        let space = c.is_whitespace();
        match in_space {
            None => in_space = Some(space),
            Some(was) if was != space => {
                tokens.push(&body[start..i]);
                start = i;
                in_space = Some(space);
            }
            Some(_) => {}
        }
    }
    if start < body.len() {
        tokens.push(&body[start..]);
    }

    let mut rows = Vec::new();
    let mut row = indent.clone();
    let mut used = indent_w;
    for token in tokens {
        let whitespace = token.starts_with(char::is_whitespace);
        let token_w = cells(token);
        if ws_token_is_wide(whitespace, token) {
            // A line break inside the text: the next row starts afresh.
            if used > indent_w {
                rows.push(std::mem::take(&mut row));
            }
            row.push_str(&indent);
            used = indent_w;
            continue;
        }
        if whitespace {
            if used + token_w > width {
                if used > indent_w {
                    rows.push(std::mem::take(&mut row));
                    row.push_str(&indent);
                    used = indent_w;
                }
                // Keep the part that still fits; a wrapped row does not start
                // with the spaces that ended the one above.
                let keep = width.saturating_sub(used);
                let kept: String = token.chars().take(keep).collect();
                used += cells(&kept);
                row.push_str(&kept);
            } else {
                row.push_str(token);
                used += token_w;
            }
            continue;
        }
        if used > indent_w && used + token_w > width {
            rows.push(std::mem::take(&mut row));
            row.push_str(&indent);
            used = indent_w;
        }
        if token_w <= width.saturating_sub(used) {
            row.push_str(token);
            used += token_w;
            continue;
        }
        // A single word longer than the row: hard-split it.
        for piece in split_cells(token, room) {
            let piece_w = cells(&piece);
            if used > indent_w && used + piece_w > width {
                rows.push(std::mem::take(&mut row));
                row.push_str(&indent);
                used = indent_w;
            }
            row.push_str(&piece);
            used += piece_w;
        }
    }
    if used > indent_w || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// A whitespace run with a line break in it ends the row rather than joining
/// two lines with spaces.
fn ws_token_is_wide(whitespace: bool, token: &str) -> bool {
    whitespace && token.chars().any(|c| c == '\n' || c == '\r')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_shown_in_caret_notation() {
        assert_eq!(card_safe("echo \u{1b}[8mhidden\r\nok\u{7f}"), "echo ^[[8mhidden^M\nok^?");
        assert_eq!(card_safe("\u{9b}"), "M-^[");
    }

    #[test]
    fn wide_text_is_split_by_the_cells_it_takes() {
        let command = format!("echo {} ; rm -rf ~/projects ; echo {}", "你".repeat(33), "好".repeat(10));
        let rows = card_rows(&command, 40, 12);
        assert!(rows.iter().all(|r| cells(r) <= 40), "{rows:?}");
        assert!(rows.concat().contains("rm -rf ~/projects"), "{rows:?}");
        let question = "このリポジトリにはテストが二つあります。どちらを先に実行しますか？結果によって次の手順が変わります。";
        let wrapped = wrap_plain(question, 40);
        assert!(wrapped.iter().all(|r| cells(r) <= 40), "{wrapped:?}");
        assert_eq!(wrapped.concat(), question);
    }

    #[test]
    fn a_long_run_of_spaces_is_counted_in_drawable_characters() {
        let rows = card_rows(&format!("a{}b", " ".repeat(12)), 40, 6);
        assert_eq!(rows, ["a \u{b7}x12 b"]);
    }

    #[test]
    fn a_wrapped_line_keeps_its_indent_on_every_row_and_stays_inside_the_card() {
        let text = format!("  {}", "word ".repeat(20).trim_end());
        for width in [20usize, 24, 40] {
            let rows = wrap_plain(&text, width);
            assert!(rows.len() > 1, "{width}: {rows:?}");
            for row in &rows {
                assert!(row.starts_with("  "), "a row lost the indent: {row:?}");
                assert!(cells(row) <= width, "{width}: a row ran past the card: {row:?}");
            }
        }
        // The indent is spent from the width, not added on top of it.
        let rows = wrap_plain("      several words that need two rows", 20);
        assert!(rows.iter().all(|r| cells(r) <= 20), "{rows:?}");
    }

    #[test]
    fn spaces_inside_a_wrapped_line_are_not_collapsed() {
        let text = format!("a{}b{}c", " ".repeat(4), " ".repeat(5));
        assert_eq!(wrap_plain(&text, 40), std::slice::from_ref(&text), "a line that fits is left exactly as written");
        let long = format!("one{}two{}three{}four{}five", " ".repeat(4), " ".repeat(3), " ".repeat(5), " ".repeat(2));
        let rows = wrap_plain(&long, 24);
        assert!(rows.iter().any(|r| r.contains("one    two")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("two   three")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("four  five")), "{rows:?}");
        assert!(rows.iter().all(|r| cells(r) <= 24), "{rows:?}");
    }
}
