use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh_welcome_card_if_before_user_msg(
    chat: &mut ChatView,
    renderer: &mut Renderer,
    model: &str,
    cwd_display: &str,
    mode_str: &str,
    memory_docs: usize,
    source: &BackendSource,
    current_effort: &str,
    context_display: Option<&str>,
    show_mascot: bool,
    mood: MascotMood,
) {
    refresh_welcome_card_animated(
        chat, renderer, model, cwd_display, mode_str, memory_docs, source, current_effort,
        context_display, 0, None, show_mascot, mood, None,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh_welcome_card_animated(
    chat: &mut ChatView,
    renderer: &mut Renderer,
    model: &str,
    cwd_display: &str,
    mode_str: &str,
    memory_docs: usize,
    source: &BackendSource,
    current_effort: &str,
    context_display: Option<&str>,
    tick_n: usize,
    width_override: Option<usize>,
    show_mascot: bool,
    mood: MascotMood,
    // `reveal_rows`: draw only this many rows — the start-up reveal, where the
    // card appears to draw itself from the top down. `None` draws all of it.
    reveal_rows: Option<usize>,
) {
    if !chat.has_user_message() {
        let (term_w, term_h) = {
            let (w, h) = crossterm::terminal::size().unwrap_or((100, 24));
            (width_override.unwrap_or(w as usize), h as usize)
        };
        let th_sum = thinking_summary_str(source, current_effort);
        let card = welcome_card_responsive_opts(
            model,
            cwd_display,
            mode_str,
            memory_docs,
            Some(&th_sum),
            context_display,
            term_w,
            term_h,
            tick_n,
            show_mascot,
            mood,
        );
        let card = match reveal_rows {
            Some(rows) => card.into_iter().take(rows.max(1)).collect(),
            None => card,
        };
        chat.update_welcome_card(card);
        renderer.request_reprint();
    }
}

/// The welcome card as first shown. Animated, it starts as its top border and
/// the tick loop draws in the rest; otherwise it goes up whole, because
/// nothing will come back to finish it.
pub(crate) fn opening_card(card: Vec<RenderLine>, animate: bool) -> Vec<RenderLine> {
    if animate {
        card.into_iter().take(1).collect()
    } else {
        card
    }
}

/// How many rows of the welcome card to draw, so it appears to draw itself
/// from the top down over the first half second. `None` once it is whole.
pub(crate) fn welcome_reveal_rows(started_at: std::time::Instant) -> Option<usize> {
    const ROW_MS: u128 = 35;
    const ROWS: u128 = 15;
    let elapsed = started_at.elapsed().as_millis();
    (elapsed < ROW_MS * ROWS).then(|| (elapsed / ROW_MS) as usize + 1)
}

/// Rows of an approval-card value: wrapped (never silently clipped), with
/// long whitespace runs made visible — padding must not push the tail of a
/// command (`... | sh`) out of sight — and an explicit note when rows run out.
pub(crate) fn card_rows(value: &str, width: usize, max_rows: usize) -> Vec<String> {
    let mut shown = String::new();
    for line in value.lines() {
        if !shown.is_empty() {
            shown.push('\u{21b5}'); // ↵ marks a real newline
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

/// Model-supplied text made safe to print: control characters (ANSI
/// escapes included) are shown, not executed.
pub(crate) fn card_safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\n' { '\u{fffd}' } else { c })
        .collect()
}

/// Draw the `/goal` ledger as a card. Facts the loop reported, so a run that
/// stopped at a budget cannot read as a run that finished — whatever the
/// model's own closing summary claims.
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

/// Wrap plain text at `width` on word boundaries, keeping a leading indent.
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
