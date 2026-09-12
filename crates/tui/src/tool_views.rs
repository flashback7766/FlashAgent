//! Rich visual tool card renderers for FlashAgent TUI.
//!
//! Provides expanded views for:
//! - File Edits/Creation/Patches: Two-column line numbers (`old_line new_line`),
//!   red deletions, green additions, centered folding pills (`+N more lines`).
//! - File Reads/Inspection: Two-column line numbers with soft blue highlighting
//!   (`\x1b[48;2;22;38;60m` / `\x1b[38;2;145;195;255m`) and folding dividers.
//! - Shell Commands: Boxed card with `~/FlashAgent $ <cmd>`, yellow command,
//!   white flags, output lines with `- And N More lines...` folding.
//! - Directory Analysis: `Analyzed 📁 <path> ⌵`, indented `📄 <file>` / `📁 <dir>`,
//!   capped with `- And 56 More...`.
//! - Grep / Search: Grouped by file, cyan line numbers, bold yellow query match,
//!   `- And N More matches...`.
//! - Subagent: Task quote, role badge, report preview.
//! - Memory, Git, Web, MCP tools.

use crate::{clip_ansi, visible_width, LineKind, RenderLine};
use std::path::Path;

// ANSI Colors matching FlashAgent Material 3 Expressive palette
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

/// File icon helper based on extension.
pub fn tool_file_icon(path: &str) -> &'static str {
    let ext = Path::new(path).extension().and_then(|s| s.to_str()).unwrap_or("");
    match ext {
        "rs" => "🦀",
        "toml" | "yaml" | "yml" | "json" => "⚙",
        "md" | "txt" => "📄",
        "sh" | "bash" => "$",
        _ => "📄",
    }
}

/// Helper to render centered fold line: `──────── +N more lines ────────`
pub fn render_fold_line(text: &str, width: usize) -> String {
    let vis_len = visible_width(text);
    let total_w = width.saturating_sub(4).max(20);
    let dashes = total_w.saturating_sub(vis_len + 2) / 2;
    let dash_str = "─".repeat(dashes.max(3));
    format!("  {BORDER_DIM}{dash_str}{RESET} {TEXT_MUTED}{text}{RESET} {BORDER_DIM}{dash_str}{RESET}")
}

/// Helper to render a rounded container header:
/// `╭─ Ran cargo build ────────────────╮`
pub fn render_card_top(title: &str, width: usize) -> String {
    let box_w = width.saturating_sub(4).clamp(30, 100);
    let max_title_w = box_w.saturating_sub(8);
    let clipped = clip_ansi(title, max_title_w);
    let title_vis = visible_width(&clipped);
    let dash_count = box_w.saturating_sub(title_vis + 5);
    format!("{BORDER_DIM}╭─ {RESET}{clipped}{RESET} {BORDER_DIM}{}╮{RESET}", "─".repeat(dash_count))
}

/// Helper to render a rounded container bottom: `╰────────────────────────────────────╯`
pub fn render_card_bottom(width: usize) -> String {
    let box_w = width.saturating_sub(4).clamp(30, 100);
    let bot_dashes = box_w.saturating_sub(2);
    format!("{BORDER_DIM}╰{}╯{RESET}", "─".repeat(bot_dashes))
}

/// Render an interactive shell command execution card (Screenshot 2).
///
/// Features:
/// - Header: `Ran cargo build --release ⌵`
/// - Rounded box container
/// - Prompt: `~/FlashAgent $ cargo build --release`
/// - Monospace stdout/stderr with `- And N More lines...` folding
pub fn render_command_card(
    cwd: &str,
    cmd: &str,
    output: Option<&str>,
    is_error: bool,
    is_running: bool,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let box_w = width.saturating_sub(4).clamp(30, 100);
    let inner_w = box_w.saturating_sub(6).max(20);

    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";
    let status_header = if is_running {
        format!("  {TEXT_MUTED}Running{RESET} {TEXT_BRIGHT}{cmd}{RESET} ⠋")
    } else if is_error {
        format!("  {TEXT_RED}Failed{RESET} {TEXT_BRIGHT}{cmd}{RESET} {chevron}")
    } else {
        format!("  {TEXT_MUTED}Ran{RESET} {TEXT_BRIGHT}{cmd}{RESET} {chevron}")
    };
    lines.push((LineKind::Tool, status_header));

    // Top border
    lines.push((LineKind::Tool, render_card_top(cmd, width)));

    // Prompt line inside box: `~/cwd $ cmd args`
    let short_cwd = if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()) {
        if cwd.starts_with(&home) {
            format!("~{}", &cwd[home.len()..])
        } else {
            cwd.to_string()
        }
    } else {
        cwd.to_string()
    };

    // Syntax-highlight prompt: binary in yellow, flags in white
    let mut parts = cmd.split_whitespace();
    let bin = parts.next().unwrap_or(cmd);
    let args: Vec<&str> = parts.collect();
    let args_str = args.join(" ");

    let prompt_styled = if args_str.is_empty() {
        format!("{TEXT_PROMPT}{short_cwd} ${RESET} {TEXT_YELLOW}{bin}{RESET}")
    } else {
        format!("{TEXT_PROMPT}{short_cwd} ${RESET} {TEXT_YELLOW}{bin}{RESET} {TEXT_BRIGHT}{args_str}{RESET}")
    };

    let vis_prompt = visible_width(&prompt_styled);
    let pad = " ".repeat(inner_w.saturating_sub(vis_prompt));
    lines.push((
        LineKind::Tool,
        format!("{BORDER_DIM}│{RESET}  {prompt_styled}{pad}  {BORDER_DIM}│{RESET}"),
    ));

    // Separator line under prompt
    lines.push((
        LineKind::Tool,
        format!("{BORDER_DIM}│{RESET}  {}{BORDER_DIM}│{RESET}", " ".repeat(inner_w + 2)),
    ));

    // Output lines
    if let Some(out) = output {
        let trimmed = out.trim_end();
        if !trimmed.is_empty() {
            let all_lines: Vec<&str> = trimmed.lines().collect();
            const MAX_LINES: usize = 15;
            let display_count = all_lines.len().min(MAX_LINES);

            for &raw_line in &all_lines[..display_count] {
                let clipped = clip_ansi(raw_line, inner_w);
                let vis = visible_width(&clipped);
                let pad = " ".repeat(inner_w.saturating_sub(vis));
                lines.push((
                    LineKind::Tool,
                    format!("{BORDER_DIM}│{RESET}  {TEXT_BRIGHT}{clipped}{RESET}{pad}  {BORDER_DIM}│{RESET}"),
                ));
            }

            if all_lines.len() > MAX_LINES {
                let remaining = all_lines.len() - display_count;
                let fold_text = format!("- And {remaining} More lines...");
                let vis = visible_width(&fold_text);
                let pad = " ".repeat(inner_w.saturating_sub(vis));
                lines.push((
                    LineKind::Tool,
                    format!("{BORDER_DIM}│{RESET}  {TEXT_MUTED}{fold_text}{RESET}{pad}  {BORDER_DIM}│{RESET}"),
                ));
            }
        }
    } else if is_running {
        let running_text = "Executing command...";
        let pad = " ".repeat(inner_w.saturating_sub(running_text.len()));
        lines.push((
            LineKind::Tool,
            format!("{BORDER_DIM}│{RESET}  {TEXT_MUTED}{running_text}{RESET}{pad}  {BORDER_DIM}│{RESET}"),
        ));
    }

    // Bottom border
    lines.push((LineKind::Tool, render_card_bottom(width)));
    lines
}

/// Render directory exploration card (Screenshot 3).
///
/// Features:
/// - Header: `Analyzed 📁 .audit ⌵`
/// - Indented entries: `  📄 filename` / `  📁 dirname/`
/// - Capped with: `  - And 56 More...`
pub fn render_directory_card(
    path: &str,
    listing: Option<&str>,
    is_running: bool,
    _width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";
    let icon = "📁";

    let header = if is_running {
        format!("  {TEXT_MUTED}Analyzing{RESET} {icon} {TEXT_BRIGHT}{path}{RESET} ⠋")
    } else {
        format!("  {TEXT_MUTED}Analyzed{RESET} {icon} {TEXT_BRIGHT}{path}{RESET} {chevron}")
    };
    lines.push((LineKind::Tool, header));

    if let Some(list_text) = listing {
        let raw_entries: Vec<&str> = list_text.lines().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        const MAX_ENTRIES: usize = 15;
        let display_count = raw_entries.len().min(MAX_ENTRIES);

        for &entry in &raw_entries[..display_count] {
            let is_dir = entry.ends_with('/');
            let entry_icon = if is_dir { "📁" } else { tool_file_icon(entry) };
            lines.push((
                LineKind::Tool,
                format!("    {entry_icon} {TEXT_BRIGHT}{entry}{RESET}"),
            ));
        }

        if raw_entries.len() > MAX_ENTRIES {
            let remaining = raw_entries.len() - display_count;
            lines.push((
                LineKind::Tool,
                format!("    {TEXT_MUTED}- And {remaining} More...{RESET}"),
            ));
        }
    }

    lines
}

/// Render file reading / code inspection card (Screenshot 1 styling in soft blue).
///
/// Features:
/// - Header: `Read 📄 src/main.rs (120 lines) ⌵`
/// - Two-column line numbers with soft blue background (`\x1b[48;2;22;38;60m`)
///   and ice-cyan text (`\x1b[38;2;145;195;255m`).
/// - Folded unread blocks with `──────── +N more lines ────────`.
pub fn render_read_card(
    path: &str,
    content: Option<&str>,
    offset: usize,
    _limit: usize,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let icon = tool_file_icon(path);
    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";

    let raw_text = content.unwrap_or("");
    let file_lines: Vec<&str> = raw_text.lines().collect();
    let count = file_lines.len();

    let header = format!("  {TEXT_MUTED}Read{RESET} {icon} {TEXT_BRIGHT}{path}{RESET} {TEXT_MUTED}({count} lines){RESET} {chevron}");
    lines.push((LineKind::Tool, header));

    if offset > 0 {
        lines.push((LineKind::Tool, render_fold_line(&format!("+{offset} more lines"), width)));
    }

    const MAX_PREVIEW: usize = 20;
    let preview_count = count.min(MAX_PREVIEW);

    for (i, &line_content) in file_lines.iter().take(preview_count).enumerate() {
        // Strip out leading line number if fs_tools::read_file already injected "   1\t..."
        let (line_no, text) = if let Some(tab_idx) = line_content.find('\t') {
            let (num_part, rest) = line_content.split_at(tab_idx);
            let parsed_no = num_part.trim().parse::<usize>().unwrap_or(offset + i + 1);
            (parsed_no, &rest[1..])
        } else {
            (offset + i + 1, line_content)
        };

        let budget = width.saturating_sub(14).max(10);
        let clipped = clip_ansi(text, budget);
        let styled = format!(
            "  {TEXT_CYAN}{:>4} {:>4}{RESET} {BG_READ}{TEXT_READ}{}{RESET}",
            line_no, line_no, clipped
        );
        lines.push((LineKind::Tool, styled));
    }

    if count > preview_count {
        let remaining = count - preview_count;
        lines.push((LineKind::Tool, render_fold_line(&format!("+{remaining} more lines"), width)));
    }

    lines
}

/// Diff line entry for two-column diff rendering.
#[derive(Debug, Clone)]
pub enum DiffRow {
    Same { old_line: usize, new_line: usize, text: String },
    Del { old_line: usize, text: String },
    Add { new_line: usize, text: String },
    Fold { count: usize },
}

/// Parse unified diff text or EditChunk into DiffRows.
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
            // Parse hunk header: @@ -old_start,old_cnt +new_start,new_cnt @@
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

/// Render file edits & creation card (Screenshot 1).
///
/// Features:
/// - Header: `Edited 📄 src/lib.rs (+12 -4) ⌵`
/// - Two-column line numbers: `old_num new_num │ line`
/// - Deletions: dark red background (`\x1b[48;2;65;20;25m`) + red text (`\x1b[38;2;245;120;120m`).
/// - Additions: dark green background (`\x1b[48;2;20;55;30m`) + green text (`\x1b[38;2;135;220;145m`).
/// - Centered fold dividers: `──────── +3045 more lines ────────`.
pub fn render_edit_card(
    path: &str,
    added: usize,
    deleted: usize,
    diff_or_content: Option<&str>,
    is_write: bool,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let icon = tool_file_icon(path);
    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";

    let action_verb = if is_write { "Wrote" } else { "Edited" };
    let header = format!(
        "  {TEXT_MUTED}{action_verb}{RESET} {icon} {TEXT_BRIGHT}{path}{RESET} {TEXT_GREEN}+{added}{RESET} {TEXT_RED}-{deleted}{RESET} {chevron}"
    );
    lines.push((LineKind::Tool, header));

    if let Some(text) = diff_or_content {
        let budget = width.saturating_sub(14).max(10);
        let rows = parse_diff_to_rows(text);

        if rows.is_empty() {
            // Fallback for write_file or raw content without @@ hunks: treat as additions
            for (i, raw_l) in text.lines().take(25).enumerate() {
                let clipped = clip_ansi(raw_l, budget);
                let styled = format!(
                    "       {TEXT_MUTED}{:>4}{RESET} {BG_ADD}{TEXT_GREEN}{}{RESET}",
                    i + 1, clipped
                );
                lines.push((LineKind::Tool, styled));
            }
            let total = text.lines().count();
            if total > 25 {
                lines.push((LineKind::Tool, render_fold_line(&format!("+{} more lines", total - 25), width)));
            }
        } else {
            for row in rows.into_iter().take(35) {
                match row {
                    DiffRow::Same { old_line, new_line, text } => {
                        let clipped = clip_ansi(&text, budget);
                        let styled = format!(
                            "  {TEXT_MUTED}{:>4} {:>4}{RESET} {TEXT_BRIGHT}{clipped}{RESET}",
                            old_line, new_line
                        );
                        lines.push((LineKind::Tool, styled));
                    }
                    DiffRow::Del { old_line, text } => {
                        let clipped = clip_ansi(&text, budget);
                        let styled = format!(
                            "  {TEXT_RED}{:>4}{RESET}      {BG_DEL}{TEXT_RED}{clipped}{RESET}",
                            old_line
                        );
                        lines.push((LineKind::Tool, styled));
                    }
                    DiffRow::Add { new_line, text } => {
                        let clipped = clip_ansi(&text, budget);
                        let styled = format!(
                            "       {TEXT_GREEN}{:>4}{RESET} {BG_ADD}{TEXT_GREEN}{clipped}{RESET}",
                            new_line
                        );
                        lines.push((LineKind::Tool, styled));
                    }
                    DiffRow::Fold { count } => {
                        lines.push((LineKind::Tool, render_fold_line(&format!("+{count} more lines"), width)));
                    }
                }
            }
        }
    }

    lines
}

/// Render pattern search card (`grep`).
///
/// Features:
/// - Grouped by file
/// - Line numbers in cyan
/// - Query occurrences highlighted in bold gold
/// - Capped with `- And N More matches...`
pub fn render_grep_card(
    pattern: &str,
    output: Option<&str>,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";

    let header = format!("  {TEXT_MUTED}Searched{RESET} {TEXT_YELLOW}\"{pattern}\"{RESET} {chevron}");
    lines.push((LineKind::Tool, header));

    if let Some(text) = output {
        let raw_lines: Vec<&str> = text.lines().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        const MAX_MATCHES: usize = 12;
        let display_count = raw_lines.len().min(MAX_MATCHES);

        for &raw_l in &raw_lines[..display_count] {
            let budget = width.saturating_sub(6).max(15);
            let clipped = clip_ansi(raw_l, budget);

            // Highlight matches of pattern with gold
            let highlighted = if !pattern.is_empty() && clipped.contains(pattern) {
                clipped.replace(pattern, &format!("{TEXT_YELLOW}{pattern}{RESET}"))
            } else {
                clipped
            };

            lines.push((LineKind::Tool, format!("    {highlighted}")));
        }

        if raw_lines.len() > MAX_MATCHES {
            let remaining = raw_lines.len() - display_count;
            lines.push((LineKind::Tool, format!("    {TEXT_MUTED}- And {remaining} More matches...{RESET}")));
        }
    }

    lines
}

/// Render Subagent task card (`spawn_agent`).
pub fn render_subagent_card(
    task: &str,
    output: Option<&str>,
    is_running: bool,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";

    let header = if is_running {
        format!("  {TEXT_LAVENDER}Subagent{RESET} {TEXT_MUTED}executing task...{RESET} ⠋")
    } else {
        format!("  {TEXT_LAVENDER}Subagent{RESET} {TEXT_MUTED}finished task{RESET} {chevron}")
    };
    lines.push((LineKind::Tool, header));

    lines.push((LineKind::Tool, render_card_top("Subagent Execution", width)));
    let box_w = width.saturating_sub(4).clamp(30, 100);
    let inner_w = box_w.saturating_sub(6).max(20);

    let task_vis = format!("Task: \"{task}\"");
    let clipped_task = clip_ansi(&task_vis, inner_w);
    let pad = " ".repeat(inner_w.saturating_sub(visible_width(&clipped_task)));
    lines.push((
        LineKind::Tool,
        format!("{BORDER_DIM}│{RESET}  {TEXT_BRIGHT}{clipped_task}{RESET}{pad}  {BORDER_DIM}│{RESET}"),
    ));

    if let Some(out) = output {
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            lines.push((
                LineKind::Tool,
                format!("{BORDER_DIM}│{RESET}  {}{BORDER_DIM}│{RESET}", " ".repeat(inner_w + 2)),
            ));
            for raw_l in trimmed.lines().take(8) {
                let clipped = clip_ansi(raw_l, inner_w);
                let pad = " ".repeat(inner_w.saturating_sub(visible_width(&clipped)));
                lines.push((
                    LineKind::Tool,
                    format!("{BORDER_DIM}│{RESET}  {TEXT_MUTED}{clipped}{RESET}{pad}  {BORDER_DIM}│{RESET}"),
                ));
            }
        }
    }

    lines.push((LineKind::Tool, render_card_bottom(width)));
    lines
}

/// Render memory tool card (`memory_*`).
pub fn render_memory_card(
    action: &str,
    scopes: &[String],
    details: Option<&str>,
    _width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";
    let scope_tag = scopes.join(", ");

    let header = format!("  {TEXT_LAVENDER}Memory{RESET} {TEXT_MUTED}{action}{RESET} {TEXT_BRIGHT}[{scope_tag}]{RESET} {chevron}");
    lines.push((LineKind::Tool, header));

    if let Some(text) = details {
        for l in text.lines().take(10) {
            lines.push((LineKind::Tool, format!("    {TEXT_MUTED}{l}{RESET}")));
        }
    }

    lines
}

/// Render git status / diff card.
pub fn render_git_card(
    is_diff: bool,
    output: Option<&str>,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";

    if is_diff {
        let header = format!("  {TEXT_MUTED}Git Diff{RESET} {chevron}");
        lines.push((LineKind::Tool, header));
        if let Some(text) = output {
            lines.extend(render_edit_card("git-diff", 0, 0, Some(text), false, width));
        }
    } else {
        let header = format!("  {TEXT_MUTED}Git Status{RESET} {chevron}");
        lines.push((LineKind::Tool, header));
        if let Some(text) = output {
            for l in text.lines().take(15) {
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
        }
    }

    lines
}

/// Render generic / custom / MCP tool card.
pub fn render_generic_card(
    name: &str,
    args: &str,
    output: Option<&str>,
    width: usize,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let chevron = "\x1b[38;2;120;125;140m⌵\x1b[0m";

    let header = format!("  {TEXT_MUTED}Ran{RESET} {TEXT_BRIGHT}{name}{RESET} {chevron}");
    lines.push((LineKind::Tool, header));

    lines.push((LineKind::Tool, render_card_top(name, width)));
    let box_w = width.saturating_sub(4).clamp(30, 100);
    let inner_w = box_w.saturating_sub(6).max(20);

    // Render formatted args
    let formatted_args = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args.trim()) {
        serde_json::to_string_pretty(&val).unwrap_or_else(|_| args.to_string())
    } else {
        args.to_string()
    };

    for arg_l in formatted_args.lines().take(10) {
        let clipped = clip_ansi(arg_l, inner_w);
        let pad = " ".repeat(inner_w.saturating_sub(visible_width(&clipped)));
        lines.push((
            LineKind::Tool,
            format!("{BORDER_DIM}│{RESET}  {TEXT_MUTED}{clipped}{RESET}{pad}  {BORDER_DIM}│{RESET}"),
        ));
    }

    // Render output preview
    if let Some(out) = output {
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            lines.push((
                LineKind::Tool,
                format!("{BORDER_DIM}│{RESET}  {}{BORDER_DIM}│{RESET}", "─".repeat(inner_w)),
            ));
            for out_l in trimmed.lines().take(10) {
                let clipped = clip_ansi(out_l, inner_w);
                let pad = " ".repeat(inner_w.saturating_sub(visible_width(&clipped)));
                lines.push((
                    LineKind::Tool,
                    format!("{BORDER_DIM}│{RESET}  {TEXT_BRIGHT}{clipped}{RESET}{pad}  {BORDER_DIM}│{RESET}"),
                ));
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
    fn test_render_command_card_prompt_and_folding() {
        let output = (1..=25).map(|i| format!("Compiling package_{i} v0.1.0")).collect::<Vec<_>>().join("\n");
        let lines = render_command_card("/home/flashback/FlashAgent", "cargo build --release", Some(&output), false, false, 80);
        let text_dump = lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        assert!(text_dump.contains("Ran") && text_dump.contains("cargo build --release"));
        assert!(text_dump.contains("$") && text_dump.contains("cargo") && text_dump.contains("build --release"));
        assert!(text_dump.contains("Compiling package_1"));
        assert!(text_dump.contains("- And 10 More lines..."));
    }

    #[test]
    fn test_render_directory_card_screenshot3() {
        let entries = (1..=20).map(|i| format!("2026-09-0{i}-audit-step.md")).collect::<Vec<_>>().join("\n");
        let lines = render_directory_card(".audit", Some(&entries), false, 80);
        let text_dump = lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        let plain = crate::strip_ansi(&text_dump);
        assert!(plain.contains("Analyzed 📁 .audit"));
        assert!(plain.contains("📄 2026-09-01-audit-step.md"));
        assert!(plain.contains("- And 5 More..."));
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
        let lines = render_edit_card("src/main.rs", 2, 2, Some(diff), false, 80);
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

