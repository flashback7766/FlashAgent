//! One visual shell for an opened tool call or read/search group. Bodies keep
//! their own semantics; only the shared shell cuts a preview, after every row
//! of the retained content has been generated.

use crate::{visible_width, LineKind, RenderLine};

const RESET: &str = "\x1b[0m";
const BORDER_DIM: &str = "\x1b[38;2;95;90;85m";
const TEXT_MUTED: &str = "\x1b[38;2;135;130;125m";
const TEXT_PROMPT: &str = "\x1b[38;2;120;140;160m";
const TEXT_BRIGHT: &str = "\x1b[38;2;225;230;240m";
const TEXT_YELLOW: &str = "\x1b[1;38;2;245;160;80m";
const TEXT_CYAN: &str = "\x1b[38;2;110;175;230m";
const TEXT_RED: &str = "\x1b[38;2;230;110;95m";
const TEXT_GREEN: &str = "\x1b[38;2;145;205;140m";
const BG_DEL: &str = "\x1b[48;2;65;20;25m";
const BG_ADD: &str = "\x1b[48;2;20;55;30m";
const BG_READ: &str = "\x1b[48;2;38;36;33m";
const TEXT_READ: &str = "\x1b[38;2;145;195;255m";

/// Clipped by cells and grapheme clusters, with an ellipsis only where text
/// was cut. All controls other than colours are made safe before measuring.
pub(crate) fn clip_ellipsis(text: &str, width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let text = crate::terminal_safe(text);
    if visible_width(&text) <= width {
        return text;
    }
    let room = width.saturating_sub(1);
    let mut out = String::with_capacity(text.len());
    let mut cells = 0usize;
    let mut cut = false;
    for (escape, piece) in crate::text::ansi_pieces(&text) {
        if escape {
            out.push_str(piece);
            continue;
        }
        for g in piece.graphemes(true) {
            if cut {
                continue;
            }
            let w = unicode_width::UnicodeWidthStr::width(g);
            if cells + w > room {
                if width > 0 {
                    out.push('…');
                }
                cut = true;
                continue;
            }
            cells += w;
            out.push_str(g);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Waiting,
    Running,
    Done,
    Failed,
    /// Declined, denied or stopped, rather than an execution failure.
    NotRun,
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

    pub(crate) fn marker(self, animated: bool) -> &'static str {
        match self {
            Self::Running if animated => crate::anim::spinner(crate::anim::now_ms()),
            Self::Running | Self::Waiting => "◦",
            Self::Done => "√",
            Self::Failed => "×",
            Self::NotRun => "○",
        }
    }

    pub(crate) fn colour(self) -> &'static str {
        match self {
            Self::Done => TEXT_GREEN,
            Self::Failed => TEXT_RED,
            Self::Running => TEXT_CYAN,
            Self::Waiting | Self::NotRun => TEXT_MUTED,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardControl {
    Close,
    Copy,
    Arguments,
}

/// Rows and the controls painted on them. Coordinates are visible cells,
/// with exclusive ends, relative to this card (not terminal screen rows).
pub(crate) struct ToolCard {
    pub lines: Vec<RenderLine>,
    pub zones: Vec<(usize, usize, usize, CardControl)>,
    pub cut: Option<usize>,
}

fn card_width(width: usize) -> usize {
    if width < 12 { width } else { crate::content_width(width) }
}

/// A carriage return in captured output replaces the previous progress line.
fn body_text(line: &str) -> String {
    crate::terminal_safe(line.rsplit('\r').find(|part| !part.is_empty()).unwrap_or(""))
}

/// Source whitespace and styles survive a wrap. Unlike clipping a body row,
/// this leaves all retained text available when the card is shown in full.
fn wrap_body_line(text: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    if width == 0 {
        return vec![String::new()];
    }
    let safe = body_text(text);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut style = String::new();
    let mut cells = 0usize;
    for (escape, piece) in crate::text::ansi_pieces(&safe) {
        if escape {
            row.push_str(piece);
            if matches!(piece, "\x1b[0m" | "\x1b[m") {
                style.clear();
            } else {
                style.push_str(piece);
            }
            continue;
        }
        for g in piece.graphemes(true) {
            let w = unicode_width::UnicodeWidthStr::width(g);
            if cells > 0 && cells + w > width {
                rows.push(format!("{row}{RESET}"));
                row = style.clone();
                cells = 0;
            }
            // A two-cell glyph cannot fit a one-cell terminal. Never draw
            // half of it, or let the terminal wrap outside the frame.
            if w > width {
                row.push('…');
                cells += 1;
            } else {
                row.push_str(g);
                cells += w;
            }
        }
    }
    rows.push(format!("{row}{RESET}"));
    rows
}

fn framed_row(text: &str, width: usize) -> RenderLine {
    let room = width.saturating_sub(4);
    let pad = " ".repeat(room.saturating_sub(visible_width(text)));
    (LineKind::Tool, format!("{BORDER_DIM}│{RESET} {text}{RESET}{pad} {BORDER_DIM}│{RESET}"))
}

/// Header, body, preview cut and footer are one card, even for a group.
/// The body renderers deliberately have no line limits of their own.
pub(crate) fn render_card(
    title: &str,
    body: Vec<RenderLine>,
    width: usize,
    full: bool,
    can_copy: bool,
    has_arguments: bool,
) -> ToolCard {
    let box_w = card_width(width);
    let mut card = ToolCard { lines: Vec::new(), zones: Vec::new(), cut: None };
    if box_w < 4 {
        card.lines.push((LineKind::Tool, clip_ellipsis(title, box_w)));
        for (_, row) in body {
            card.lines.extend(wrap_body_line(&row, box_w).into_iter().map(|row| (LineKind::Tool, row)));
        }
        return card;
    }

    let close_col = box_w - 3;
    let head = clip_ellipsis(title, box_w.saturating_sub(7));
    let prefix = if box_w < 7 { format!("{BORDER_DIM}╭{RESET}") }
        else { format!("{BORDER_DIM}╭─ {RESET}{head}{RESET}") };
    let pad = " ".repeat(close_col.saturating_sub(visible_width(&prefix)));
    card.lines.push((LineKind::Tool, format!("{prefix}{pad}{TEXT_MUTED}×{RESET} {BORDER_DIM}╮{RESET}")));
    card.zones.push((0, close_col, close_col + 1, CardControl::Close));

    let inner_w = box_w - 4;
    let mut rows = Vec::new();
    for (_, row) in body {
        rows.extend(wrap_body_line(&row, inner_w));
    }
    let hidden = if full { 0 } else { rows.len().saturating_sub(crate::BLOCK_PREVIEW_ROWS) };
    let shown = rows.len() - hidden;
    for row in rows.into_iter().take(shown) {
        card.lines.push(framed_row(&row, box_w));
    }
    if hidden > 0 {
        card.cut = Some(card.lines.len());
        let note = crate::i18n::tr_fill("… {n} more lines · click to show all", &[("n", &hidden.to_string())]);
        card.lines.push(framed_row(&format!("{TEXT_MUTED}{}{RESET}", clip_ellipsis(&note, inner_w)), box_w));
    }

    let mut controls = vec![(crate::i18n::tr("copy"), CardControl::Copy, can_copy)];
    if has_arguments {
        controls.push((crate::i18n::tr("arguments"), CardControl::Arguments, true));
    }
    let needed: usize = controls.iter().map(|(label, ..)| visible_width(label) + 2).sum::<usize>()
        + controls.len().saturating_sub(1) * 2;
    let short = needed > inner_w;
    let mut footer = String::new();
    for (label, control, enabled) in controls {
        let label = if short { if control == CardControl::Copy { "c" } else { "a" } } else { label };
        let button = format!("[{label}]");
        let gap = if footer.is_empty() { "" } else { "  " };
        let from = 2 + visible_width(&footer) + gap.len();
        let to = from + visible_width(&button);
        if to > box_w - 2 {
            continue;
        }
        footer.push_str(gap);
        let colour = if enabled { TEXT_PROMPT } else { TEXT_MUTED };
        footer.push_str(&format!("{colour}{button}{RESET}"));
        if enabled {
            card.zones.push((card.lines.len(), from, to, control));
        }
    }
    let pad = "─".repeat(inner_w.saturating_sub(visible_width(&footer)));
    card.lines.push((LineKind::Tool, format!("{BORDER_DIM}╰ {RESET}{footer}{BORDER_DIM}{pad} ╯{RESET}")));
    card
}

pub(crate) fn plain_body(text: &str, muted: bool) -> Vec<RenderLine> {
    let colour = if muted { TEXT_MUTED } else { TEXT_BRIGHT };
    text.lines().map(|line| (LineKind::Tool, format!("{colour}{line}{RESET}"))).collect()
}

pub(crate) fn section(text: &str) -> RenderLine {
    (LineKind::Tool, format!("{TEXT_MUTED}{text}{RESET}"))
}

pub(crate) fn command_body(cwd: Option<&str>, cmd: &str, output: Option<&str>, state: CallState) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    if let Some(cwd) = cwd.filter(|s| !s.is_empty()) {
        lines.push((LineKind::Tool, format!("{TEXT_PROMPT}{cwd}{RESET}")));
    }
    for (i, command) in cmd.lines().enumerate() {
        let at = command.find(char::is_whitespace).unwrap_or(command.len());
        let (bin, args) = command.split_at(at);
        let prompt = if i == 0 { "$" } else { " " };
        lines.push((LineKind::Tool, format!("{TEXT_PROMPT}{prompt}{RESET} {TEXT_YELLOW}{bin}{RESET}{TEXT_BRIGHT}{args}{RESET}")));
    }
    if let Some(output) = output {
        lines.push((LineKind::Tool, String::new()));
        lines.extend(plain_body(output, false));
    } else if state == CallState::Waiting {
        lines.push(section(crate::i18n::tr("Runs once you allow it")));
    } else if state == CallState::Running {
        lines.push(section(crate::i18n::tr("Executing command…")));
    }
    lines
}

pub(crate) fn is_file_separator(line: &str) -> bool {
    let line = line.trim();
    line.len() > 8 && line.starts_with("=== ") && line.ends_with(" ===") && !line.contains('\t')
}

fn numbered_read_line(line: &str) -> Option<(usize, &str)> {
    let (number, text) = line.split_once('\t')?;
    Some((number.trim().parse().ok()?, text))
}

fn result_note(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("error:") || line.starts_with("...[limit ")
        || line.starts_with("[truncated") || line.starts_with("…[")
        || matches!(line, "(empty or past end of file)" | "(empty)" | "no matches")
}

pub(crate) fn read_line_count(text: &str) -> usize {
    let batch = text.lines().any(is_file_separator);
    text.lines().filter(|line| {
        numbered_read_line(line).is_some()
            || (!is_file_separator(line) && !result_note(line) && (!batch || !line.trim().is_empty()))
    }).count()
}

pub(crate) fn read_body(content: &str, offset: usize) -> Vec<RenderLine> {
    let batch = content.lines().any(is_file_separator);
    let mut next = offset.saturating_add(1);
    let mut rows = Vec::new();
    for line in content.lines() {
        if is_file_separator(line) {
            rows.push(section(line));
            next = 1;
            continue;
        }
        if result_note(line) || (batch && line.trim().is_empty()) {
            rows.push(section(line));
            continue;
        }
        let (number, text) = numbered_read_line(line).unwrap_or((next, line));
        next = number.saturating_add(1);
        rows.push((LineKind::Tool, format!("{TEXT_CYAN}{number:>4}{RESET} {BG_READ}{TEXT_READ}{text}{RESET}")));
    }
    rows
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiffRow {
    Same { old_line: usize, new_line: usize, text: String },
    Del { old_line: usize, text: String },
    Add { new_line: usize, text: String },
    /// File/hunk headers and retained truncation notes are data too. A gap
    /// between hunks is not a hidden file block that this card can recover.
    Note(String),
}

pub(crate) fn parse_diff_to_rows(diff_text: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut old_pos = 1usize;
    let mut new_pos = 1usize;
    for line in diff_text.lines() {
        if line.starts_with("@@") {
            let start = |mark: char| line.find(mark).and_then(|at| {
                line[at + 1..].split(|c: char| !c.is_ascii_digit()).next()?.parse::<usize>().ok()
            });
            old_pos = start('-').unwrap_or(old_pos);
            new_pos = start('+').unwrap_or(new_pos);
            // The hunk header is machinery; the line numbers carry the same
            // information without a second notation.
        } else if line.starts_with("---") || line.starts_with("+++") {
            // File headers belong under [arguments], not in the change itself.
        } else if is_file_separator(line) {
            rows.push(DiffRow::Note(line.to_string()));
        } else if let Some(text) = line.strip_prefix(' ') {
            rows.push(DiffRow::Same { old_line: old_pos, new_line: new_pos, text: text.to_string() });
            old_pos = old_pos.saturating_add(1);
            new_pos = new_pos.saturating_add(1);
        } else if let Some(text) = line.strip_prefix('-') {
            rows.push(DiffRow::Del { old_line: old_pos, text: text.to_string() });
            old_pos = old_pos.saturating_add(1);
        } else if let Some(text) = line.strip_prefix('+') {
            rows.push(DiffRow::Add { new_line: new_pos, text: text.to_string() });
            new_pos = new_pos.saturating_add(1);
        } else {
            rows.push(DiffRow::Note(line.to_string()));
        }
    }
    rows
}

pub fn diff_body(text: &str) -> Vec<RenderLine> {
    let numbered = text.lines().any(|line| line.starts_with("@@"));
    let at = |n: usize, mark: &str| if numbered { n.to_string() } else { mark.to_string() };
    let safe = |text: &str| crate::text::shown_controls(&text.replace('\t', "    "));
    parse_diff_to_rows(text).into_iter().map(|row| {
        let styled = match row {
            DiffRow::Same { old_line, new_line, text } => format!("{TEXT_MUTED}{:>4} {:>4}{RESET} {TEXT_BRIGHT}{}{RESET}", at(old_line, ""), at(new_line, ""), safe(&text)),
            DiffRow::Del { old_line, text } => format!("{TEXT_RED}{:>4}     -{RESET} {BG_DEL}{TEXT_RED}{}{RESET}", at(old_line, "-"), safe(&text)),
            DiffRow::Add { new_line, text } => format!("     {TEXT_GREEN}{:>4} +{RESET} {BG_ADD}{TEXT_GREEN}{}{RESET}", at(new_line, "+"), safe(&text)),
            DiffRow::Note(text) => format!("{TEXT_MUTED}{}{RESET}", safe(&text)),
        };
        (LineKind::Tool, styled)
    }).collect()
}

pub(crate) fn written_body(content: &str) -> Vec<RenderLine> {
    content.lines().enumerate().map(|(i, line)| {
        (LineKind::Tool, format!("{TEXT_GREEN}{:>4} +{RESET} {BG_ADD}{TEXT_GREEN}{line}{RESET}", i + 1))
    }).collect()
}

/// `path:line:text`, including a Windows drive prefix in the path.
pub(crate) fn search_match(line: &str) -> Option<(&str, usize, &str)> {
    for (at, _) in line.char_indices().filter(|(_, c)| *c == ':') {
        let Some((number, text)) = line[at + 1..].split_once(':') else { continue };
        if let Ok(number) = number.parse::<usize>() {
            return Some((&line[..at], number, text));
        }
    }
    None
}

pub(crate) fn search_body(pattern: &str, output: &str) -> Vec<RenderLine> {
    let mut rows = Vec::new();
    let mut previous = None;
    for line in output.lines() {
        if let Some((path, number, text)) = search_match(line) {
            if previous != Some(path) {
                rows.push((LineKind::Tool, format!("{TEXT_BRIGHT}{path}{RESET}")));
                previous = Some(path);
            }
            let text = if pattern.is_empty() { text.to_string() } else {
                text.replace(pattern, &format!("{TEXT_YELLOW}{pattern}{RESET}"))
            };
            rows.push((LineKind::Tool, format!("{TEXT_CYAN}{number:>4}{RESET} {text}")));
        } else {
            rows.push(section(line));
        }
    }
    rows
}

pub(crate) fn listing_count(text: &str) -> usize {
    text.lines().filter(|line| !line.trim().is_empty() && !result_note(line)).count()
}

pub(crate) fn directory_body(listing: &str) -> Vec<RenderLine> {
    listing.lines().map(|line| {
        let colour = if result_note(line) { TEXT_MUTED } else if line.ends_with('/') { TEXT_CYAN } else { TEXT_BRIGHT };
        (LineKind::Tool, format!("{colour}{line}{RESET}"))
    }).collect()
}

pub(crate) fn git_status_body(output: &str) -> Vec<RenderLine> {
    output.lines().map(|line| {
        let trimmed = line.trim_start();
        let colour = if trimmed.starts_with('M') { TEXT_YELLOW }
            else if trimmed.starts_with('?') { TEXT_GREEN }
            else if trimmed.starts_with('D') { TEXT_RED }
            else { TEXT_BRIGHT };
        (LineKind::Tool, format!("{colour}{line}{RESET}"))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(card: &ToolCard) -> String {
        card.lines.iter().map(|(_, row)| crate::strip_ansi(row)).collect::<Vec<_>>().join("\n")
    }

    fn assert_square(card: &ToolCard, terminal_width: usize) {
        let width = visible_width(&card.lines[0].1);
        for (_, row) in &card.lines {
            assert_eq!(visible_width(row), width, "{row:?} in\n{}", plain(card));
            assert!(visible_width(row) <= terminal_width, "overflow at {terminal_width}: {row:?}");
        }
    }

    #[test]
    fn full_shell_content_is_generated_before_the_only_preview_cap() {
        let output = (1..=40).map(|i| format!("output line {i}")).collect::<Vec<_>>().join("\n");
        let body = || command_body(Some("/work"), "cargo test", Some(&output), CallState::Done);
        assert!(body().iter().any(|(_, row)| row.contains("output line 40")), "the old shell cap lost this line");
        let preview = render_card("✓ run_shell · cargo test", body(), 80, false, true, true);
        assert!(preview.cut.is_some());
        assert!(!plain(&preview).contains("output line 40"));
        let full = render_card("✓ run_shell · cargo test", body(), 80, true, true, true);
        assert!(full.cut.is_none());
        for i in 1..=40 {
            assert!(plain(&full).contains(&format!("output line {i}")), "line {i} was lost");
        }
        assert_square(&preview, 80);
        assert_square(&full, 80);
    }

    #[test]
    fn read_separators_are_not_file_lines_and_each_files_numbers_survive() {
        let content = "=== a.py ===\n     1\t\"Store.\"\n     2\timport os\n\n=== b.py ===\n     1\tx = 1";
        assert_eq!(read_line_count(content), 3);
        let rows = read_body(content, 0);
        assert_eq!(rows.iter().filter(|(_, row)| row.contains(BG_READ)).count(), 3);
        assert_eq!(rows.iter().filter(|(_, row)| crate::strip_ansi(row).starts_with("   1 ")).count(), 2);
        assert!(rows.iter().any(|(_, row)| crate::strip_ansi(row) == "=== a.py ==="));
    }

    #[test]
    fn read_and_search_bodies_have_no_internal_line_caps() {
        let read = (21..=70).map(|i| format!("{i:>6}\tretained {i}")).collect::<Vec<_>>().join("\n");
        let rows = read_body(&read, 20);
        assert_eq!(rows.len(), 50);
        assert!(rows.last().unwrap().1.contains("retained 70"));
        assert!(!rows.iter().any(|(_, row)| row.contains("more line")), "offsets are not recoverable preview data");
        let hits = (1..=30).map(|i| format!("src/lib.rs:{i}:let needle = {i};")).collect::<Vec<_>>().join("\n");
        let rows = search_body("needle", &hits);
        assert_eq!(rows.len(), 31, "one file label plus every match");
        assert!(rows.last().unwrap().1.contains("= 30;"));
        assert!(rows.iter().any(|(_, row)| row.contains(&format!("{TEXT_YELLOW}needle{RESET}"))));
        assert_eq!(search_match("C:\\src\\lib.rs:42:text"), Some(("C:\\src\\lib.rs", 42, "text")));
    }

    #[test]
    fn diffs_keep_file_headers_hunk_numbers_and_all_retained_rows() {
        let diff = format!("--- a/src/main.rs\n+++ b/src/main.rs\n@@ -10,1 +10,40 @@\n-old\n{}\n\\ No newline at end of file",
            (1..=40).map(|i| format!("+new {i}")).collect::<Vec<_>>().join("\n"));
        let rows = diff_body(&diff);
        // Headers are arguments, not the change; the numbers carry the hunk.
        assert!(!rows.iter().any(|(_, row)| row.contains("--- a/src/main.rs") || row.contains("+++ b/src/main.rs")));
        assert!(rows.iter().any(|(_, row)| row.contains("new 40")));
        assert!(rows.iter().any(|(_, row)| row.contains("No newline at end of file")));
        assert_eq!(rows.iter().filter(|(_, row)| row.contains(BG_ADD)).count(), 40);
        assert_eq!(rows.iter().filter(|(_, row)| row.contains(BG_DEL)).count(), 1);
        assert!(crate::strip_ansi(&rows[0].1).contains("  10"));
        let written = written_body("# Notes\n- item\n+ plus\n  indented");
        assert_eq!(written.iter().filter(|(_, row)| row.contains(BG_ADD)).count(), 4);
        assert!(!written.iter().any(|(_, row)| row.contains(BG_DEL)));
    }

    #[test]
    fn one_shell_keeps_borders_and_control_cells_aligned_at_narrow_widths() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        for width in [12, 18, 24, 40, 80, 120] {
            let body = plain_body(&format!("日本語 e\u{301} {family} {}\n\tindent\n10%\r100%", "long ".repeat(10)), false);
            let card = render_card("✓ read_file · 日本語/src/config.rs", body, width, true, true, true);
            assert_square(&card, width);
            for (row, from, to, control) in &card.zones {
                let text = crate::strip_ansi(&card.lines[*row].1);
                assert!(*from < *to && *to <= visible_width(&text));
                if *control == CardControl::Close {
                    assert_eq!(*to - *from, 1);
                    assert!(text.ends_with("× ╮"));
                }
            }
            assert!(plain(&card).contains("100%") && !plain(&card).contains("10%"));
            if width >= 18 {
                assert!(plain(&card).contains(family), "a joined grapheme was split: {}", plain(&card));
            }
        }
        for width in 0..12 {
            let card = render_card("✓ 文件", plain_body("日本語", false), width, true, true, true);
            assert!(card.lines.iter().all(|(_, row)| visible_width(row) <= width), "tiny width {width}: {}", plain(&card));
        }
    }

    #[test]
    fn clipping_keeps_joined_graphemes_and_the_prefix_at_zero_width() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let cut = clip_ellipsis(&format!("a{family}bc"), 4);
        assert!(cut.contains(family) && cut.ends_with('…'), "{cut:?}");
        assert!(visible_width(&cut) <= 4);
        assert_eq!(visible_width(&clip_ellipsis("日本語", 0)), 0);
    }

    #[test]
    fn a_waiting_command_is_not_presented_as_executing() {
        let waiting = command_body(None, "cargo test", None, CallState::Waiting);
        let text = waiting.iter().map(|(_, row)| crate::strip_ansi(row)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Runs once you allow it") && !text.contains("Executing"), "{text}");
        assert_eq!(CallState::of(true, false, true), CallState::Waiting);
        assert_eq!(CallState::of(true, false, false), CallState::Running);
    }
}
