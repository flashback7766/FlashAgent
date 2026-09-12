use super::*;

/// Inline markdown for terminal: `**bold**` → bold, `` `code` `` → colored.
/// Line-level, no full parser — good enough for chat output.
pub fn md(line: &str) -> String {
    // Fast path: no markdown markers.
    if !line.contains("**") && !line.contains('`') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < line.len() {
        if line[i..].starts_with("**") {
            if let Some(end) = line[i + 2..].find("**") {
                let inner = &line[i + 2..i + 2 + end];
                if !inner.is_empty() {
                    out.push_str(&crossterm::style::style(inner.to_string()).bold().to_string());
                    i += 2 + end + 2;
                    continue;
                }
            }
            out.push_str("**");
            i += 2;
        } else if bytes[i] == b'`' {
            if let Some(end) = line[i + 1..].find('`') {
                let inner = &line[i + 1..i + 1 + end];
                if !inner.is_empty() {
                    out.push_str(
                        &crossterm::style::style(inner.to_string())
                            .with(crossterm::style::Color::Rgb { r: 215, g: 180, b: 110 })
                            .to_string(),
                    );
                    i += 1 + end + 1;
                    continue;
                }
            }
            out.push('`');
            i += 1;
        } else if let Some(ch) = line[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

pub(crate) enum ListMarkerKind {
    Bullet { glyph: &'static str },
    Number { num: String, is_paren: bool },
}

pub(crate) fn parse_list_marker(trimmed: &str, level: usize) -> Option<(ListMarkerKind, usize)> {
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        let glyph = match level {
            0 => "\x1b[38;2;140;145;160m•\x1b[0m",
            1 => "\x1b[38;2;140;145;160m◦\x1b[0m",
            _ => "\x1b[38;2;140;145;160m▪\x1b[0m",
        };
        return Some((ListMarkerKind::Bullet { glyph }, 2));
    }
    let num_digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if num_digits > 0 && num_digits <= 3 {
        let after = &trimmed[num_digits..];
        if after.starts_with(". ") {
            return Some((
                ListMarkerKind::Number {
                    num: trimmed[..num_digits].to_string(),
                    is_paren: false,
                },
                num_digits + 2,
            ));
        } else if after.starts_with(") ") {
            return Some((
                ListMarkerKind::Number {
                    num: trimmed[..num_digits].to_string(),
                    is_paren: true,
                },
                num_digits + 2,
            ));
        }
    }
    None
}

/// Formats an assistant's markdown response for the terminal:
/// - Headings (`#`, `##`, `###`) rendered as styled section titles without raw hashes
/// - Tables (`| col1 | col2 |` + delimiter) rendered with unicode box borders and aligned cells
/// - Lists (`- `, `* `, `1. `, `1) `, checkboxes) rendered with bullets and hanging indentation
/// - Inline bold (`**bold**`) and inline code (`` `code` ``) styled with ANSI colors
pub fn render_markdown_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(20);
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut in_code_block = false;

    let is_table_delimiter = |l: &str| -> bool {
        let t = l.trim();
        if !t.contains('-') || (!t.contains('|') && !t.contains('+')) {
            return false;
        }
        let inner = t.trim_matches('|').trim_matches('+').trim();
        !inner.is_empty() && inner.chars().all(|c| c == '-' || c == ':' || c == '|' || c == '+' || c.is_whitespace())
    };

    let is_table_row = |l: &str| -> bool {
        let t = l.trim();
        if t.starts_with("```") || t.starts_with('#') {
            return false;
        }
        (t.starts_with('|') && t.ends_with('|')) || (t.contains('|') && t.matches('|').count() >= 1)
    };

    let parse_cells = |l: &str| -> Vec<String> {
        let t = l.trim();
        let stripped = t.strip_prefix('|').unwrap_or(t);
        let stripped = stripped.strip_suffix('|').unwrap_or(stripped);
        stripped.split('|').map(|c| c.trim().to_string()).collect()
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // 1. Code blocks: ```
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                let lang = trimmed.strip_prefix("```").unwrap_or("").trim();
                let title = if lang.is_empty() { "code".to_string() } else { lang.to_string() };
                let border_color = "\x1b[38;2;75;99;130m";
                let reset = "\x1b[0m";
                let dash_count = width.saturating_sub(visible_width(&title) + 5).max(4);
                out.push(format!("{border_color}╭─ {reset}\x1b[38;2;194;231;255m{title}\x1b[0m {border_color}{}╮{reset}", "─".repeat(dash_count)));
            } else {
                let border_color = "\x1b[38;2;75;99;130m";
                let reset = "\x1b[0m";
                let dash_count = width.saturating_sub(2).max(4);
                out.push(format!("{border_color}╰{}╯{reset}", "─".repeat(dash_count)));
            }
            i += 1;
            continue;
        }

        if in_code_block {
            let border_color = "\x1b[38;2;75;99;130m";
            let reset = "\x1b[0m";
            let inner_w = width.saturating_sub(2);
            let inner_content_w = inner_w.saturating_sub(1);

            let chunks = if line.is_empty() {
                vec![String::new()]
            } else if visible_width(line) <= inner_content_w {
                vec![line.to_string()]
            } else {
                wrap(line, inner_content_w)
            };

            for chunk in chunks {
                let clipped = if visible_width(&chunk) > inner_content_w {
                    clip_ansi(&chunk, inner_content_w)
                } else {
                    chunk
                };
                let vis = visible_width(&clipped);
                let pad = inner_content_w.saturating_sub(vis);
                out.push(format!(
                    "{border_color}│{reset} \x1b[38;2;215;180;110m{clipped}\x1b[0m{}{border_color}│{reset}",
                    " ".repeat(pad)
                ));
            }
            i += 1;
            continue;
        }

        // 2. Table detection: header row followed by delimiter row
        if is_table_row(line) && i + 1 < lines.len() && is_table_delimiter(lines[i + 1]) {
            let header_row = parse_cells(line);
            let mut data_rows = Vec::new();
            i += 2; // skip header and delimiter

            while i < lines.len() && is_table_row(lines[i]) && !is_table_delimiter(lines[i]) {
                data_rows.push(parse_cells(lines[i]));
                i += 1;
            }

            let num_cols = header_row.len().max(data_rows.iter().map(|r| r.len()).max().unwrap_or(0));
            if num_cols > 0 {
                let mut col_widths = vec![3usize; num_cols];
                for (c, cell) in header_row.iter().enumerate() {
                    col_widths[c] = col_widths[c].max(visible_width(&md(cell)));
                }
                for row in &data_rows {
                    for (c, cell) in row.iter().enumerate() {
                        if c < num_cols {
                            col_widths[c] = col_widths[c].max(visible_width(&md(cell)));
                        }
                    }
                }

                // Constrain table to fit within terminal width if necessary
                let border_chars_count = 3 * num_cols + 1;
                let total_content_w: usize = col_widths.iter().sum();
                let total_w = total_content_w + border_chars_count;
                if total_w > width {
                    let available_content_w = width.saturating_sub(border_chars_count).max(num_cols * 3);
                    let excess = total_content_w.saturating_sub(available_content_w);
                    if excess > 0 {
                        for _ in 0..excess {
                            if let Some((max_idx, _)) = col_widths.iter().enumerate().max_by_key(|(_, &w)| w) {
                                if col_widths[max_idx] > 3 {
                                    col_widths[max_idx] -= 1;
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                }

                let border_color = "\x1b[38;2;75;99;130m";
                let reset = "\x1b[0m";

                if !out.is_empty() && !out.last().map(|s| s.is_empty()).unwrap_or(true) {
                    out.push(String::new());
                }

                // Top border: ╭─...─┬─...─╮
                let mut top_border = format!("{border_color}╭");
                for (c, &w) in col_widths.iter().enumerate() {
                    top_border.push_str(&"─".repeat(w + 2));
                    if c + 1 < num_cols {
                        top_border.push('┬');
                    } else {
                        top_border.push('╮');
                    }
                }
                top_border.push_str(reset);
                out.push(top_border);

                // Header row: │ col1 │ col2 │
                let mut header_str = format!("{border_color}│{reset}");
                for (c, &w) in col_widths.iter().enumerate() {
                    let raw_cell = header_row.get(c).map(|s| s.as_str()).unwrap_or("");
                    let styled = md(raw_cell);
                    let vis = visible_width(&styled);
                    let cell_content = if vis > w {
                        clip_ansi(&styled, w)
                    } else {
                        let pad = " ".repeat(w.saturating_sub(vis));
                        format!("{styled}{pad}")
                    };
                    header_str.push_str(&format!(" \x1b[1;38;2;240;235;225m{cell_content}\x1b[0m {border_color}│{reset}"));
                }
                out.push(header_str);

                // Middle separator: ├─...─┼─...─┤
                let mut mid_border = format!("{border_color}├");
                for (c, &w) in col_widths.iter().enumerate() {
                    mid_border.push_str(&"─".repeat(w + 2));
                    if c + 1 < num_cols {
                        mid_border.push('┼');
                    } else {
                        mid_border.push('┤');
                    }
                }
                mid_border.push_str(reset);
                out.push(mid_border);

                // Data rows
                for row in data_rows {
                    let mut row_str = format!("{border_color}│{reset}");
                    for (c, &w) in col_widths.iter().enumerate() {
                        let raw_cell = row.get(c).map(|s| s.as_str()).unwrap_or("");
                        let styled = md(raw_cell);
                        let vis = visible_width(&styled);
                        let cell_content = if vis > w {
                            clip_ansi(&styled, w)
                        } else {
                            let pad = " ".repeat(w.saturating_sub(vis));
                            format!("{styled}{pad}")
                        };
                        row_str.push_str(&format!(" {cell_content} {border_color}│{reset}"));
                    }
                    out.push(row_str);
                }

                // Bottom border: ╰─...─┴─...─╯
                let mut bot_border = format!("{border_color}╰");
                for (c, &w) in col_widths.iter().enumerate() {
                    bot_border.push_str(&"─".repeat(w + 2));
                    if c + 1 < num_cols {
                        bot_border.push('┴');
                    } else {
                        bot_border.push('╯');
                    }
                }
                bot_border.push_str(reset);
                out.push(bot_border);
            }
            continue;
        }

        // 3. Headings: `# `, `## `, `### `, `#### `
        if trimmed.starts_with('#') {
            let hashes = trimmed.chars().take_while(|&c| c == '#').count();
            if (1..=4).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
                let title = trimmed[hashes..].trim();
                if !out.is_empty() && !out.last().map(|s| s.is_empty()).unwrap_or(true) {
                    out.push(String::new());
                }
                let styled_heading = match hashes {
                    1 => format!("\x1b[1;38;2;255;255;255m◆ {}\x1b[0m", md(title)),
                    2 => format!("\x1b[1;38;2;194;231;255m◈ {}\x1b[0m", md(title)),
                    3 => format!("\x1b[1;38;2;225;230;240m▸ {}\x1b[0m", md(title)),
                    _ => format!("\x1b[38;2;180;185;200m▪ {}\x1b[0m", md(title)),
                };
                out.push(styled_heading);
                i += 1;
                continue;
            }
        }

        // 4. List items: bullet lists, task checkboxes, and numbered lists
        let indent_spaces: usize = line
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        let level = indent_spaces / 2;

        if let Some((marker, offset)) = parse_list_marker(trimmed, level) {
            let mut raw_content = trimmed[offset..].trim().to_string();

            // Collect subsequent continuation lines belonging to this item
            let mut next_i = i + 1;
            while next_i < lines.len() {
                let n_line = lines[next_i];
                let n_trimmed = n_line.trim();
                if n_trimmed.is_empty() {
                    break;
                }
                if n_trimmed.starts_with("```") || n_trimmed.starts_with('#') || is_table_row(n_line) {
                    break;
                }
                let n_spaces: usize = n_line
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .map(|c| if c == '\t' { 4 } else { 1 })
                    .sum();
                let n_level = n_spaces / 2;
                if parse_list_marker(n_trimmed, n_level).is_some() {
                    break;
                }
                if n_spaces >= indent_spaces + 2 || (indent_spaces == 0 && n_spaces >= 2) {
                    raw_content.push(' ');
                    raw_content.push_str(n_trimmed);
                    next_i += 1;
                } else {
                    break;
                }
            }
            i = next_i - 1; // will be incremented at end of loop

            // Task list checkboxes: `[ ] `, `[x] `, `[X] `
            let (check_str, content_clean, check_vis) = if let Some(stripped) = raw_content.strip_prefix("[ ] ") {
                ("\x1b[38;2;140;145;160m☐\x1b[0m ", stripped, 2)
            } else if let Some(stripped) = raw_content.strip_prefix("[x] ").or_else(|| raw_content.strip_prefix("[X] ")) {
                ("\x1b[38;2;145;205;140m☑\x1b[0m ", stripped, 2)
            } else {
                ("", raw_content.as_str(), 0)
            };

            let indent_pad = "  ".repeat(level);
            let (prefix, prefix_vis) = match marker {
                ListMarkerKind::Bullet { glyph } => {
                    let p = format!("{indent_pad}  {glyph} {check_str}");
                    let vis = 2 * level + 2 + 1 + 1 + check_vis;
                    (p, vis)
                }
                ListMarkerKind::Number { num, is_paren } => {
                    let delim = if is_paren { ")" } else { "." };
                    let num_glyph = format!("\x1b[38;2;140;145;160m{num}{delim}\x1b[0m");
                    let p = format!("{indent_pad}  {num_glyph} {check_str}");
                    let vis = 2 * level + 2 + num.len() + 1 + 1 + check_vis;
                    (p, vis)
                }
            };

            let hang_indent = " ".repeat(prefix_vis);
            let avail_w = width.saturating_sub(prefix_vis).max(10);
            let styled_content = md(content_clean);
            let chunks = wrap_styled(&styled_content, avail_w);
            if chunks.is_empty() {
                out.push(prefix);
            } else {
                for (idx, chunk) in chunks.into_iter().enumerate() {
                    if idx == 0 {
                        out.push(format!("{prefix}{chunk}"));
                    } else {
                        out.push(format!("{hang_indent}{chunk}"));
                    }
                }
            }
            i += 1;
            continue;
        }

        // 5. Regular paragraphs
        if trimmed.is_empty() {
            out.push(String::new());
        } else {
            let styled = md(line);
            for chunk in wrap_styled(&styled, width) {
                out.push(chunk);
            }
        }
        i += 1;
    }

    out
}
