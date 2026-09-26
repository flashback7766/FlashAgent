//! Expanded tool cards: diffs with two-column line numbers and folds, file
//! reads, boxed shell commands, directory listings, grep results grouped by
//! file, subagents, memory, git, web and MCP tools.

use unicode_width::UnicodeWidthChar;

use crate::{plural, visible_width, LineKind, RenderLine};

const RESET: &str = "\x1b[0m";
const BORDER_DIM: &str = "\x1b[38;2;75;99;130m";
const TEXT_MUTED: &str = "\x1b[38;2;120;125;140m";
const TEXT_PROMPT: &str = "\x1b[38;2;120;140;160m";
const TEXT_BRIGHT: &str = "\x1b[38;2;225;230;240m";
const TEXT_YELLOW: &str = "\x1b[1;38;2;240;210;115m";
const TEXT_CYAN: &str = "\x1b[38;2;110;175;230m";
const TEXT_LAVENDER: &str = "\x1b[38;2;185;160;240m";
const TEXT_RED: &str = "\x1b[38;2;245;120;120m";
const TEXT_GREEN: &str = "\x1b[38;2;135;220;145m";
const BG_DEL: &str = "\x1b[48;2;65;20;25m";
const BG_ADD: &str = "\x1b[48;2;20;55;30m";
const BG_READ: &str = "\x1b[48;2;22;38;60m";
const TEXT_READ: &str = "\x1b[38;2;145;195;255m";

/// Clipped to `width` cells, with `…` where it was cut. Styles are kept, as
/// `clip_ansi` keeps them; tabs and any other escape in a file or command
/// output are made safe first, so the width measured is the width drawn.
pub(crate) fn clip_ellipsis(text: &str, width: usize) -> String {
    let text = &crate::terminal_safe(text);
    if visible_width(text) <= width {
        return text.to_string();
    }
    let room = width.saturating_sub(1);
    let mut out = String::with_capacity(text.len());
    let mut cells = 0;
    let mut cut = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            if chars.peek() == Some(&'[') {
                out.extend(chars.next());
                for n in chars.by_ref() {
                    out.push(n);
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        if cut {
            continue;
        }
        let w = c.width().unwrap_or(0);
        if cells + w > room {
            if width > 0 {
                out.push('…');
            }
            cut = true;
            continue;
        }
        cells += w;
        out.push(c);
    }
    out
}

/// A captured line as the box can hold it: a carriage return redraws from
/// the start of the line, a tab is spaces, and other control characters
/// would move the cursor off the frame.
fn box_text(line: &str) -> String {
    let line = line.rsplit('\r').find(|part| !part.is_empty()).unwrap_or("");
    crate::terminal_safe(line)
}

/// `│  text  │`, padded or clipped to the box.
fn box_row(styled: &str, style: &str, inner_w: usize) -> RenderLine {
    let clipped = clip_ellipsis(styled, inner_w);
    let pad = " ".repeat(inner_w.saturating_sub(visible_width(&clipped)));
    (LineKind::Tool, format!("{BORDER_DIM}│{RESET}  {style}{clipped}{RESET}{pad}  {BORDER_DIM}│{RESET}"))
}

fn more_lines(n: usize) -> String {
    format!("+{}", plural(n, "more line", "more lines"))
}

/// `──────── +N more lines ────────`
pub fn render_fold_line(text: &str, width: usize) -> String {
    let vis_len = visible_width(text);
    let total_w = width.saturating_sub(4).max(20);
    let dashes = total_w.saturating_sub(vis_len + 2) / 2;
    let dash_str = "─".repeat(dashes.max(3));
    format!("  {BORDER_DIM}{dash_str}{RESET} {TEXT_MUTED}{text}{RESET} {BORDER_DIM}{dash_str}{RESET}")
}

/// Outer width of a boxed card.
fn box_width(width: usize) -> usize {
    width.saturating_sub(4).clamp(16, 100)
}

/// `╭─ ~/FlashAgent ──────────────────╮`; an empty title leaves the edge plain.
pub fn render_card_top(title: &str, width: usize) -> String {
    let box_w = box_width(width);
    if title.is_empty() {
        return format!("{BORDER_DIM}╭{}╮{RESET}", "─".repeat(box_w.saturating_sub(2)));
    }
    let clipped = clip_ellipsis(title, box_w.saturating_sub(8));
    let title_vis = visible_width(&clipped);
    let dash_count = box_w.saturating_sub(title_vis + 5);
    format!("{BORDER_DIM}╭─ {RESET}{clipped}{RESET} {BORDER_DIM}{}╮{RESET}", "─".repeat(dash_count))
}

pub fn render_card_bottom(width: usize) -> String {
    let box_w = box_width(width);
    let bot_dashes = box_w.saturating_sub(2);
    format!("{BORDER_DIM}╰{}╯{RESET}", "─".repeat(bot_dashes))
}

/// `cwd` with the home folder as `~`.
fn short_cwd(cwd: &str) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).and_then(|h| h.into_string().ok());
    match home.as_deref().filter(|h| !h.is_empty()).and_then(|h| cwd.strip_prefix(h)) {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("~{rest}"),
        _ => cwd.to_string(),
    }
}

/// `$ cmd` in a rounded box titled with the folder it ran in, output folded
/// after a limit.
pub fn render_command_card(
    cwd: &str,
    cmd: &str,
    output: Option<&str>,
    state: CallState,
    width: usize,
) -> Vec<RenderLine> {
    let (is_error, is_running) = (state == CallState::Failed, state == CallState::Running);
    let mut lines = Vec::new();
    let box_w = box_width(width);
    let inner_w = box_w.saturating_sub(6);

    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";
    let status_header = if state == CallState::Waiting {
        format!("  {TEXT_MUTED}Waiting to run{RESET} {TEXT_BRIGHT}{cmd}{RESET} {chevron}")
    } else if is_running {
        format!("  {TEXT_MUTED}Running{RESET} {TEXT_BRIGHT}{cmd}{RESET} {}", crate::anim::spinner(crate::anim::now_ms()))
    } else if is_error {
        format!("  {TEXT_RED}Failed{RESET} {TEXT_BRIGHT}{cmd}{RESET} {chevron}")
    } else {
        format!("  {TEXT_MUTED}Ran{RESET} {TEXT_BRIGHT}{cmd}{RESET} {chevron}")
    };
    lines.push((LineKind::Tool, status_header));

    // The end of a path says where; its start is what gets cut.
    let (folder, _) = crate::tail_window(&short_cwd(cwd), box_w.saturating_sub(8));
    let title = if folder.is_empty() { String::new() } else { format!("{TEXT_PROMPT}{folder}") };
    lines.push((LineKind::Tool, render_card_top(&title, width)));

    // Binary in yellow, flags in white.
    let mut parts = cmd.split_whitespace();
    let bin = parts.next().unwrap_or(cmd);
    let args_str = parts.collect::<Vec<&str>>().join(" ");
    let prompt_styled = if args_str.is_empty() {
        format!("{TEXT_PROMPT}${RESET} {TEXT_YELLOW}{bin}{RESET}")
    } else {
        format!("{TEXT_PROMPT}${RESET} {TEXT_YELLOW}{bin}{RESET} {TEXT_BRIGHT}{args_str}{RESET}")
    };
    lines.push(box_row(&box_text(&prompt_styled), "", inner_w));
    lines.push(box_row("", "", inner_w));

    if let Some(out) = output {
        let trimmed = out.trim_end();
        if !trimmed.is_empty() {
            let all_lines: Vec<&str> = trimmed.lines().collect();
            const MAX_LINES: usize = 15;
            let display_count = all_lines.len().min(MAX_LINES);

            for &raw_line in &all_lines[..display_count] {
                lines.push(box_row(&box_text(raw_line), TEXT_BRIGHT, inner_w));
            }
            if all_lines.len() > MAX_LINES {
                lines.push(box_row(&more_lines(all_lines.len() - display_count), TEXT_MUTED, inner_w));
            }
        }
    } else if state == CallState::Waiting {
        lines.push(box_row("Runs once you allow it", TEXT_MUTED, inner_w));
    } else if is_running {
        lines.push(box_row("Executing command…", TEXT_MUTED, inner_w));
    }

    lines.push((LineKind::Tool, render_card_bottom(width)));
    lines
}

/// Capped with `+N more entries`.
pub fn render_directory_card(
    path: &str,
    listing: Option<&str>,
    is_running: bool,
    _width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";

    let header = if is_running {
        format!("  {TEXT_MUTED}Analyzing{RESET} {TEXT_BRIGHT}{path}{RESET} {}", crate::anim::spinner(crate::anim::now_ms()))
    } else {
        format!("  {TEXT_MUTED}Analyzed{RESET} {TEXT_BRIGHT}{path}{RESET} {chevron}")
    };
    lines.push((LineKind::Tool, header));

    if let Some(list_text) = listing {
        let raw_entries: Vec<&str> = list_text.lines().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        const MAX_ENTRIES: usize = 15;
        let display_count = raw_entries.len().min(MAX_ENTRIES);

        for &entry in &raw_entries[..display_count] {
            lines.push((LineKind::Tool, format!("    {TEXT_BRIGHT}{entry}{RESET}")));
        }

        if raw_entries.len() > MAX_ENTRIES {
            let remaining = raw_entries.len() - display_count;
            lines.push((
                LineKind::Tool,
                format!("    {TEXT_MUTED}+{}{RESET}", plural(remaining, "more entry", "more entries")),
            ));
        }
    }

    lines
}

/// Unread blocks are folded.
pub fn render_read_card(
    path: &str,
    content: Option<&str>,
    offset: usize,
    _limit: usize,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";

    let raw_text = content.unwrap_or("");
    let file_lines: Vec<&str> = raw_text.lines().collect();
    let count = file_lines.len();

    let header = format!(
        "  {TEXT_MUTED}Read{RESET} {TEXT_BRIGHT}{path}{RESET} {TEXT_MUTED}({}){RESET} {chevron}",
        plural(count, "line", "lines")
    );
    lines.push((LineKind::Tool, header));

    if offset > 0 {
        lines.push((LineKind::Tool, render_fold_line(&more_lines(offset), width)));
    }

    const MAX_PREVIEW: usize = 20;
    let preview_count = count.min(MAX_PREVIEW);

    for (i, &line_content) in file_lines.iter().take(preview_count).enumerate() {
        // `read_file` output already carries "   1\t..." line numbers.
        let (line_no, text) = if let Some(tab_idx) = line_content.find('\t') {
            let (num_part, rest) = line_content.split_at(tab_idx);
            let parsed_no = num_part.trim().parse::<usize>().unwrap_or(offset + i + 1);
            (parsed_no, &rest[1..])
        } else {
            (offset + i + 1, line_content)
        };

        let budget = width.saturating_sub(14).max(10);
        let clipped = clip_ellipsis(text, budget);
        let styled = format!(
            "  {TEXT_CYAN}{:>4} {:>4}{RESET} {BG_READ}{TEXT_READ}{}{RESET}",
            line_no, line_no, clipped
        );
        lines.push((LineKind::Tool, styled));
    }

    if count > preview_count {
        lines.push((LineKind::Tool, render_fold_line(&more_lines(count - preview_count), width)));
    }

    lines
}

#[derive(Debug, Clone)]
pub enum DiffRow {
    Same { old_line: usize, new_line: usize, text: String },
    Del { old_line: usize, text: String },
    Add { new_line: usize, text: String },
    Fold { count: usize },
}

/// From a unified diff.
pub fn parse_diff_to_rows(diff_text: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut old_pos = 1usize;
    let mut new_pos = 1usize;
    let mut last_hunk_end_old = 0usize;

    for line in diff_text.lines() {
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        if line.starts_with("@@") {
            // @@ -old_start,old_cnt +new_start,new_cnt @@
            if let Some(plus_idx) = line.find('+') {
                let rest = &line[plus_idx + 1..];
                let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(ns) = num_str.parse::<usize>() {
                    new_pos = ns;
                }
            }
            if let Some(minus_idx) = line.find('-') {
                let rest = &line[minus_idx + 1..];
                let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(os) = num_str.parse::<usize>() {
                    if last_hunk_end_old > 0 && os > last_hunk_end_old + 1 {
                        let gap = os - last_hunk_end_old - 1;
                        rows.push(DiffRow::Fold { count: gap });
                    }
                    old_pos = os;
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix(' ') {
            rows.push(DiffRow::Same { old_line: old_pos, new_line: new_pos, text: rest.to_string() });
            last_hunk_end_old = old_pos;
            old_pos += 1;
            new_pos += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            rows.push(DiffRow::Del { old_line: old_pos, text: rest.to_string() });
            last_hunk_end_old = old_pos;
            old_pos += 1;
        } else if let Some(rest) = line.strip_prefix('+') {
            rows.push(DiffRow::Add { new_line: new_pos, text: rest.to_string() });
            new_pos += 1;
        }
    }
    rows
}

/// Where a call stands, as its verbose card tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// Started, and its approval or question card is up: nothing has run.
    Waiting,
    Running,
    Done,
    /// It failed, or was declined or stopped: nothing it proposed happened.
    Failed,
}

impl CallState {
    pub fn of(is_running: bool, is_error: bool, waiting: bool) -> Self {
        match (is_running, is_error) {
            (true, _) if waiting => Self::Waiting,
            (true, _) => Self::Running,
            (false, true) => Self::Failed,
            (false, false) => Self::Done,
        }
    }
}

/// `old new │ line`, red deletions, green additions, centered fold dividers.
/// A change that did not happen says so, with why, and shows no diff: in
/// green it read as made.
pub fn render_edit_card(
    path: &str,
    added: usize,
    deleted: usize,
    diff_or_content: Option<&str>,
    is_write: bool,
    state: CallState,
    reason: Option<&str>,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";

    let action_verb = match (state, is_write) {
        (CallState::Done, true) => "Wrote",
        (CallState::Done, false) => "Edited",
        (CallState::Waiting, true) => "Waiting to write",
        (CallState::Waiting, false) => "Waiting to edit",
        (CallState::Running, true) => "Writing",
        (CallState::Running, false) => "Editing",
        (CallState::Failed, true) => "Not written:",
        (CallState::Failed, false) => "Not edited:",
    };
    let verb_color = if state == CallState::Failed { TEXT_RED } else { TEXT_MUTED };
    let header = format!(
        "  {verb_color}{action_verb}{RESET} {TEXT_BRIGHT}{path}{RESET} {TEXT_GREEN}+{added}{RESET} {TEXT_RED}-{deleted}{RESET} {chevron}"
    );
    lines.push((LineKind::Tool, header));

    if state == CallState::Failed {
        if let Some(reason) = reason.and_then(|r| r.lines().find(|l| !l.trim().is_empty())) {
            lines.push((LineKind::Tool, format!("    {TEXT_MUTED}{}{RESET}", crate::tool_views::clip_ellipsis(reason.trim(), width.saturating_sub(6)))));
        }
        return lines;
    }

    if let Some(text) = diff_or_content {
        let budget = width.saturating_sub(14).max(10);
        // What is written is a file, not a diff: "- item" is a line of it.
        let rows = if is_write { Vec::new() } else { parse_diff_to_rows(text) };

        if rows.is_empty() {
            const MAX_WRITTEN: usize = 25;
            for (i, raw_l) in text.lines().take(MAX_WRITTEN).enumerate() {
                let clipped = clip_ellipsis(raw_l, budget);
                let styled = format!(
                    "       {TEXT_GREEN}{:>4}{RESET} {BG_ADD}{TEXT_GREEN}{}{RESET}",
                    i + 1, clipped
                );
                lines.push((LineKind::Tool, styled));
            }
            let total = text.lines().count();
            if total > MAX_WRITTEN {
                lines.push((LineKind::Tool, render_fold_line(&more_lines(total - MAX_WRITTEN), width)));
            }
        } else {
            const MAX_ROWS: usize = 35;
            let hidden = rows.iter().skip(MAX_ROWS).filter(|r| !matches!(r, DiffRow::Fold { .. })).count();
            // An edit_file preview has no hunk headers, so no real line numbers:
            // it is marked - and + rather than numbered from 1.
            let numbered = text.lines().any(|l| l.starts_with("@@"));
            let at = |n: usize, mark: &str| if numbered { n.to_string() } else { mark.to_string() };
            for row in rows.into_iter().take(MAX_ROWS) {
                match row {
                    DiffRow::Same { old_line, new_line, text } => {
                        let clipped = clip_ellipsis(&text, budget);
                        let styled = format!(
                            "  {TEXT_MUTED}{:>4} {:>4}{RESET} {TEXT_BRIGHT}{clipped}{RESET}",
                            at(old_line, ""),
                            at(new_line, "")
                        );
                        lines.push((LineKind::Tool, styled));
                    }
                    DiffRow::Del { old_line, text } => {
                        let clipped = clip_ellipsis(&text, budget);
                        let styled = format!(
                            "  {TEXT_RED}{:>4}{RESET}      {BG_DEL}{TEXT_RED}{clipped}{RESET}",
                            at(old_line, "-")
                        );
                        lines.push((LineKind::Tool, styled));
                    }
                    DiffRow::Add { new_line, text } => {
                        let clipped = clip_ellipsis(&text, budget);
                        let styled = format!(
                            "       {TEXT_GREEN}{:>4}{RESET} {BG_ADD}{TEXT_GREEN}{clipped}{RESET}",
                            at(new_line, "+")
                        );
                        lines.push((LineKind::Tool, styled));
                    }
                    DiffRow::Fold { count } => {
                        lines.push((LineKind::Tool, render_fold_line(&more_lines(count), width)));
                    }
                }
            }
            if hidden > 0 {
                lines.push((LineKind::Tool, render_fold_line(&more_lines(hidden), width)));
            }
        }
    }

    lines
}

/// Grouped by file, matches highlighted, capped.
pub fn render_grep_card(
    pattern: &str,
    output: Option<&str>,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";

    let header = format!("  {TEXT_MUTED}Searched{RESET} {TEXT_YELLOW}\"{pattern}\"{RESET} {chevron}");
    lines.push((LineKind::Tool, header));

    if let Some(text) = output {
        let raw_lines: Vec<&str> = text.lines().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        const MAX_MATCHES: usize = 12;
        let display_count = raw_lines.len().min(MAX_MATCHES);

        for &raw_l in &raw_lines[..display_count] {
            let budget = width.saturating_sub(6).max(15);
            let clipped = clip_ellipsis(raw_l, budget);

            let highlighted = if !pattern.is_empty() && clipped.contains(pattern) {
                clipped.replace(pattern, &format!("{TEXT_YELLOW}{pattern}{RESET}"))
            } else {
                clipped
            };

            lines.push((LineKind::Tool, format!("    {highlighted}")));
        }

        if raw_lines.len() > MAX_MATCHES {
            let remaining = raw_lines.len() - display_count;
            lines.push((
                LineKind::Tool,
                format!("    {TEXT_MUTED}+{}{RESET}", plural(remaining, "more match", "more matches")),
            ));
        }
    }

    lines
}

pub fn render_subagent_card(
    task: &str,
    output: Option<&str>,
    is_running: bool,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";

    let header = if is_running {
        format!("  {TEXT_LAVENDER}Subagent{RESET} {TEXT_MUTED}executing task…{RESET} {}", crate::anim::spinner(crate::anim::now_ms()))
    } else {
        format!("  {TEXT_LAVENDER}Subagent{RESET} {TEXT_MUTED}finished task{RESET} {chevron}")
    };
    lines.push((LineKind::Tool, header));

    lines.push((LineKind::Tool, render_card_top("Task", width)));
    let inner_w = box_width(width).saturating_sub(6);
    lines.push(box_row(&box_text(task.lines().next().unwrap_or_default()), TEXT_BRIGHT, inner_w));

    if let Some(out) = output {
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            const MAX_LINES: usize = 8;
            lines.push(box_row("", "", inner_w));
            for raw_l in trimmed.lines().take(MAX_LINES) {
                lines.push(box_row(&box_text(raw_l), TEXT_MUTED, inner_w));
            }
            let total = trimmed.lines().count();
            if total > MAX_LINES {
                lines.push(box_row(&more_lines(total - MAX_LINES), TEXT_MUTED, inner_w));
            }
        }
    }

    lines.push((LineKind::Tool, render_card_bottom(width)));
    lines
}

pub fn render_memory_card(
    action: &str,
    scopes: &[String],
    details: Option<&str>,
    _width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";
    let scope_tag = scopes.join(", ");

    let header = format!("  {TEXT_LAVENDER}Memory{RESET} {TEXT_MUTED}{action}{RESET} {TEXT_BRIGHT}[{scope_tag}]{RESET} {chevron}");
    lines.push((LineKind::Tool, header));

    if let Some(text) = details {
        const MAX_LINES: usize = 10;
        for l in text.lines().take(MAX_LINES) {
            lines.push((LineKind::Tool, format!("    {TEXT_MUTED}{l}{RESET}")));
        }
        let total = text.lines().count();
        if total > MAX_LINES {
            lines.push((LineKind::Tool, format!("    {TEXT_MUTED}{}{RESET}", more_lines(total - MAX_LINES))));
        }
    }

    lines
}

pub fn render_git_card(
    is_diff: bool,
    output: Option<&str>,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";

    if is_diff {
        let header = format!("  {TEXT_MUTED}Git diff{RESET} {chevron}");
        lines.push((LineKind::Tool, header));
        if let Some(text) = output {
            lines.extend(render_edit_card("git-diff", 0, 0, Some(text), false, CallState::Done, None, width));
        }
    } else {
        let header = format!("  {TEXT_MUTED}Git status{RESET} {chevron}");
        lines.push((LineKind::Tool, header));
        if let Some(text) = output {
            const MAX_LINES: usize = 15;
            for l in text.lines().take(MAX_LINES) {
                let trimmed = l.trim();
                let colored = if trimmed.starts_with('M') {
                    format!("{TEXT_YELLOW}{l}{RESET}")
                } else if trimmed.starts_with('?') {
                    format!("{TEXT_GREEN}{l}{RESET}")
                } else if trimmed.starts_with('D') {
                    format!("{TEXT_RED}{l}{RESET}")
                } else {
                    format!("{TEXT_BRIGHT}{l}{RESET}")
                };
                lines.push((LineKind::Tool, format!("    {colored}")));
            }
            let total = text.lines().count();
            if total > MAX_LINES {
                lines.push((LineKind::Tool, format!("    {TEXT_MUTED}{}{RESET}", more_lines(total - MAX_LINES))));
            }
        }
    }

    lines
}

pub fn render_generic_card(
    name: &str,
    args: &str,
    output: Option<&str>,
    state: CallState,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m▾\x1b[0m";

    let verb = match state {
        CallState::Waiting => "Waiting on",
        CallState::Running => "Running",
        CallState::Done => "Ran",
        CallState::Failed => "Failed",
    };
    let color = if state == CallState::Failed { TEXT_RED } else { TEXT_MUTED };
    let header = format!("  {color}{verb}{RESET} {TEXT_BRIGHT}{name}{RESET} {chevron}");
    lines.push((LineKind::Tool, header));

    lines.push((LineKind::Tool, render_card_top(name, width)));
    let box_w = box_width(width);
    let inner_w = box_w.saturating_sub(6);

    let formatted_args = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args.trim()) {
        serde_json::to_string_pretty(&val).unwrap_or_else(|_| args.to_string())
    } else {
        args.to_string()
    };

    const MAX_LINES: usize = 10;
    for arg_l in formatted_args.lines().take(MAX_LINES) {
        lines.push(box_row(&box_text(arg_l), TEXT_MUTED, inner_w));
    }
    let total = formatted_args.lines().count();
    if total > MAX_LINES {
        lines.push(box_row(&more_lines(total - MAX_LINES), TEXT_MUTED, inner_w));
    }

    if let Some(out) = output {
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            lines.push((
                LineKind::Tool,
                format!("{BORDER_DIM}├{}┤{RESET}", "─".repeat(box_w.saturating_sub(2))),
            ));
            for out_l in trimmed.lines().take(MAX_LINES) {
                lines.push(box_row(&box_text(out_l), TEXT_BRIGHT, inner_w));
            }
            let total = trimmed.lines().count();
            if total > MAX_LINES {
                lines.push(box_row(&more_lines(total - MAX_LINES), TEXT_MUTED, inner_w));
            }
        }
    }

    lines.push((LineKind::Tool, render_card_bottom(width)));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbose_card_tells_what_happened_to_the_call() {
        let plain = |lines: Vec<RenderLine>| lines.iter().map(|(_, l)| crate::strip_ansi(l)).collect::<Vec<_>>().join("\n");
        let diff = "-a\n+b\n";
        // Denied, or failed on bad arguments: it read "Edited" with a green diff.
        let failed = plain(render_edit_card("store.py", 1, 1, Some(diff), false, CallState::Failed, Some("error: bad arguments: missing field `path`"), 80));
        assert!(failed.contains("Not edited: store.py") && failed.contains("missing field `path`"), "{failed}");
        assert!(!failed.contains("+ b") && !failed.contains("Edited"), "{failed}");
        let waiting = plain(render_edit_card("store.py", 1, 1, Some(diff), false, CallState::Waiting, None, 80));
        assert!(waiting.contains("Waiting to edit store.py"), "{waiting}");
        assert!(plain(render_edit_card("store.py", 1, 1, Some(diff), false, CallState::Done, None, 80)).contains("Edited store.py"));
        // Nothing runs before Allow.
        let command = plain(render_command_card("/w", "cargo test", None, CallState::Waiting, 80));
        assert!(command.contains("Waiting to run cargo test") && command.contains("Runs once you allow it") && !command.contains("Executing"), "{command}");
        let asked = plain(render_generic_card("ask_user", "{}", None, CallState::Waiting, 80));
        assert!(asked.contains("Waiting on ask_user") && !asked.contains("Ran"), "{asked}");
        assert_eq!(CallState::of(true, false, true), CallState::Waiting);
        assert_eq!(CallState::of(true, false, false), CallState::Running);
        assert_eq!(CallState::of(false, true, true), CallState::Failed);
    }

    /// Emoji are two columns wide next to one-column glyphs, so listing columns
    /// never line up.
    #[test]
    fn tool_cards_carry_no_emoji() {
        let cards = [
            render_directory_card("src", Some("main.rs\nmod/\nnotes.md"), false, 100),
            render_read_card("src/main.rs", Some("fn main() {}"), 0, 50, 100),
            render_edit_card("src/lib.rs", 3, 1, None, false, CallState::Done, None, 100),
        ];
        for card in cards {
            for (_, line) in &card {
                for ch in line.chars() {
                    let c = ch as u32;
                    assert!(
                        !(0x1F000..=0x1FAFF).contains(&c) && c != 0x2699 && c != 0x26A1,
                        "emoji {ch:?} crept back into a tool card: {line}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_render_command_card_prompt_and_folding() {
        let output = (1..=25).map(|i| format!("Compiling package_{i} v0.1.0")).collect::<Vec<_>>().join("\n");
        let lines = render_command_card("/home/flashback/FlashAgent", "cargo build --release", Some(&output), CallState::Done, 80);
        let text_dump = lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        let plain = crate::strip_ansi(&text_dump);
        assert!(plain.contains("Ran cargo build --release"));
        assert!(plain.contains("$ cargo build --release"));
        assert!(plain.contains("Compiling package_1"));
        assert!(plain.contains("+10 more lines"), "{plain}");
    }

    fn plain_rows(lines: &[RenderLine]) -> Vec<String> {
        lines.iter().map(|(_, t)| crate::strip_ansi(t)).collect()
    }

    /// Every row of the box from its top edge on, as wide as that edge.
    fn assert_box_is_square(lines: &[RenderLine]) {
        let rows = plain_rows(lines);
        let top = rows.iter().position(|r| r.starts_with('╭')).expect("a top edge");
        let width = crate::visible_width(&rows[top]);
        for row in &rows[top..] {
            assert_eq!(crate::visible_width(row), width, "{row:?} in\n{}", rows.join("\n"));
        }
    }

    #[test]
    fn the_command_is_said_once_in_the_box_and_never_pushes_its_edge() {
        let cmd = format!("cargo test --workspace {}", "--features very-long-feature-name ".repeat(6));
        let output = "ok\n\tindented by a tab\nprogress 10%\rprogress 100%";
        for width in [40usize, 80, 120] {
            let lines = render_command_card("/work/FlashAgent", &cmd, Some(output), CallState::Done, width);
            assert_box_is_square(&lines);
            let rows = plain_rows(&lines);
            assert!(rows[1].starts_with("╭─ /work/FlashAgent "), "the folder is the title: {:?}", rows[1]);
            assert!(rows[2].contains("$ cargo test") && rows[2].contains('…'), "{:?}", rows[2]);
            assert!(rows.iter().any(|r| r.contains("progress 100%")) && !rows.iter().any(|r| r.contains("10%")));
        }
    }

    #[test]
    fn the_generic_card_divider_lines_up_with_its_edges() {
        let lines = render_generic_card("web_fetch", r#"{"url":"https://example.com"}"#, Some("fetched"), CallState::Done, 80);
        assert_box_is_square(&lines);
        assert!(plain_rows(&lines).iter().any(|r| r.starts_with('├') && r.ends_with('┤')));
    }

    #[test]
    fn a_written_file_is_all_additions_whatever_its_lines_start_with() {
        let content = "# Notes\n- item one\n  indented\n+ plus\nplain";
        let lines = render_edit_card("notes.md", 5, 0, Some(content), true, CallState::Done, None, 80);
        let rows = plain_rows(&lines);
        for text in ["# Notes", "- item one", "  indented", "+ plus", "plain"] {
            assert!(rows.iter().any(|r| r.ends_with(text)), "{text:?} missing from {rows:#?}");
        }
        assert!(!lines.iter().any(|(_, l)| l.contains(BG_DEL)), "a written line shown as a deletion");
        assert_eq!(lines.iter().filter(|(_, l)| l.contains(BG_ADD)).count(), 5);
    }

    #[test]
    fn counts_of_one_are_singular() {
        let rows = plain_rows(&render_read_card("a.txt", Some("only"), 0, 10, 80));
        assert!(rows[0].contains("(1 line)"), "{:?}", rows[0]);
        let rows = plain_rows(&render_read_card("a.txt", Some("x"), 1, 10, 80));
        assert!(rows[1].contains("+1 more line "), "{:?}", rows[1]);
    }

    #[test]
    fn test_render_directory_card() {
        let entries = (1..=20).map(|i| format!("2026-09-0{i}-audit-step.md")).collect::<Vec<_>>().join("\n");
        let lines = render_directory_card(".audit", Some(&entries), false, 80);
        let text_dump = lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        let plain = crate::strip_ansi(&text_dump);
        assert!(plain.contains("Analyzed .audit"));
        assert!(plain.contains("2026-09-01-audit-step.md"));
        assert!(plain.contains("+5 more entries"));
    }

    #[test]
    fn test_render_read_card_blue_highlighting() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let lines = render_read_card("src/main.rs", Some(content), 0, 100, 80);
        let text_dump = lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        assert!(text_dump.contains("Read") && text_dump.contains("src/main.rs"));
        assert!(text_dump.contains("\x1b[48;2;22;38;60m"), "Must have soft blue background");
        assert!(text_dump.contains("\x1b[38;2;145;195;255m"), "Must have ice blue text");
    }

    #[test]
    fn test_render_edit_card_diff_columns_and_folding() {
        let diff = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -10,2 +10,2 @@\n-old line 1\n-old line 2\n+new line 1\n+new line 2\n";
        let lines = render_edit_card("src/main.rs", 2, 2, Some(diff), false, CallState::Done, None, 80);
        let text_dump = lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        assert!(text_dump.contains("Edited") && text_dump.contains("src/main.rs"));
        assert!(text_dump.contains("+2") && text_dump.contains("-2"));
        assert!(text_dump.contains("\x1b[48;2;65;20;25m"), "Deletion must have dark red background");
        assert!(text_dump.contains("\x1b[48;2;20;55;30m"), "Addition must have dark green background");
    }

    #[test]
    fn test_render_grep_card_gold_highlight() {
        let output = "crates/tui/src/lib.rs:42: let val = 123;\ncrates/core/src/lib.rs:10: let val = 456;";
        let lines = render_grep_card("let val", Some(output), 80);
        let text_dump = lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        assert!(text_dump.contains("Searched") && text_dump.contains("\"let val\""));
        assert!(text_dump.contains("\x1b[1;38;2;240;210;115mlet val"), "Query match must be bold gold");
    }
}

