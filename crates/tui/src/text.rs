use super::*;

/// So a wrap can reopen the colour on the next line and close it at the end.
fn track_style(word: &str, active_style: &mut Option<String>) {
    let b = word.as_bytes();
    let mut j = 0;
    while j < b.len() {
        if b[j] == 0x1b && j + 1 < b.len() && b[j + 1] == b'[' {
            let start = j;
            j += 2;
            while j < b.len() && !('@'..='~').contains(&(b[j] as char)) {
                j += 1;
            }
            if j < b.len() {
                let seq = &word[start..=j];
                if seq == "\x1b[0m" || seq == "\x1b[m" {
                    *active_style = None;
                } else {
                    *active_style = Some(seq.to_string());
                }
                j += 1;
            }
        } else {
            j += 1;
        }
    }
}

/// Measured in cells: a double-width character is never split, and escape
/// codes travel with their piece without counting towards its width.
fn break_wide_word(word: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_vis = 0usize;
    let mut chars = word.char_indices();
    while let Some((at, c)) = chars.next() {
        if c == '\x1b' {
            let rest = &word[at..];
            let end = rest
                .find(|c: char| ('@'..='~').contains(&c))
                .map(|p| p + 1)
                .unwrap_or(rest.len());
            cur.push_str(&rest[..end]);
            // Escape sequences are ASCII, so one byte is one character.
            for _ in 1..end {
                chars.next();
            }
            continue;
        }
        let cells = c.width().unwrap_or(0);
        if cur_vis > 0 && cur_vis + cells > width {
            out.push(std::mem::take(&mut cur));
            cur_vis = 0;
        }
        cur.push(c);
        cur_vis += cells;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Active ANSI styles carry across wraps; each line ends with a reset.
pub fn wrap_styled(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut out = Vec::new();

    for line in text.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        if visible_width(line) <= width {
            out.push(line.to_string());
            continue;
        }

        let mut cur = String::new();
        let mut cur_vis = 0;
        let mut active_style: Option<String> = None;

        for word in line.split(' ') {
            let w_vis = visible_width(word);
            // A word wider than the line (a URL, a path, Chinese text) would otherwise
            // be wrapped by the terminal, which this renderer cannot see, and it would
            // then erase the wrong rows.
            if w_vis > width {
                if cur_vis > 0 {
                    if active_style.is_some() {
                        cur.push_str("\x1b[0m");
                    }
                    out.push(std::mem::take(&mut cur));
                    if let Some(ref style) = active_style {
                        cur.push_str(style);
                    }
                }
                let mut pieces = break_wide_word(word, width);
                let last = pieces.pop().unwrap_or_default();
                for piece in pieces {
                    cur.push_str(&piece);
                    if active_style.is_some() {
                        cur.push_str("\x1b[0m");
                    }
                    out.push(std::mem::take(&mut cur));
                    if let Some(ref style) = active_style {
                        cur.push_str(style);
                    }
                }
                cur_vis = visible_width(&last);
                cur.push_str(&last);
                track_style(word, &mut active_style);
                continue;
            }
            // By visible width: counting the carried-over escape as text put a space
            // before the first word of every wrapped styled line.
            let need_space = cur_vis > 0;
            let space_vis = if need_space { 1 } else { 0 };

            if cur_vis > 0 && cur_vis + space_vis + w_vis > width {
                if active_style.is_some() {
                    cur.push_str("\x1b[0m");
                }
                out.push(std::mem::take(&mut cur));
                cur_vis = 0;
                if let Some(ref style) = active_style {
                    cur.push_str(style);
                }
            }

            if cur_vis > 0 {
                cur.push(' ');
                cur_vis += 1;
            }
            cur.push_str(word);
            cur_vis += w_vis;

            track_style(word, &mut active_style);
        }

        if !cur.is_empty() {
            if active_style.is_some() {
                cur.push_str("\x1b[0m");
            }
            out.push(cur);
        }
    }

    out
}

pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    wrap_styled(text, width)
}

/// So it can follow "Failed to". Identifiers like `MEMORY.md` are left alone.
pub fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest: String = chars.collect();
    let second_is_upper = rest.chars().next().is_some_and(|c| c.is_uppercase());
    if second_is_upper {
        return text.to_string();
    }
    first.to_lowercase().collect::<String>() + &rest
}

/// Models return absolute paths; the part up to the project is noise.
pub fn relative_to_cwd(text: &str) -> String {
    match std::env::current_dir() {
        Ok(cwd) => {
            let cwd = cwd.to_string_lossy().to_string();
            if cwd.is_empty() || !text.contains(&cwd) {
                return text.to_string();
            }
            text.replace(&format!("{cwd}/"), "").replace(&cwd, ".")
        }
        Err(_) => text.to_string(),
    }
}

pub fn format_cmd(cmd: &str) -> String {
    let clean = cmd.trim();
    let first_line = relative_to_cwd(clean.lines().next().unwrap_or(clean).trim());
    let first_line = first_line.as_str();
    let max_len = 70;
    if first_line.chars().count() > max_len {
        let truncated: String = first_line.chars().take(max_len - 1).collect();
        format!("{truncated}\u{2026}")
    } else {
        first_line.to_string()
    }
}

/// Styled spans end with a full reset that would blank the outer line colour
/// for the rest of the line (seen after `**bold**` in reasoning).
pub fn restore_line_color(text: &str, prefix: &str) -> String {
    text.replace("\x1b[0m", &format!("\x1b[0m{prefix}"))
        .replace("\x1b[39m", &format!("\x1b[39m{prefix}"))
}

/// Escapes are kept whole: cutting inside one prints its tail (`[39m`) and
/// leaks style across lines.
pub fn clip_ansi(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cells = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // ESC [ ... final byte (@-~), copied verbatim.
            out.push(c);
            if chars.peek() == Some(&'[') {
                if let Some(bracket) = chars.next() {
                    out.push(bracket);
                    while let Some(&n) = chars.peek() {
                        out.push(n);
                        chars.next();
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
            }
            continue;
        }
        let char_w = c.width().unwrap_or(0);
        if cells + char_w > width {
            continue;
        }
        cells += char_w;
        out.push(c);
    }
    out
}

pub fn visible_width(text: &str) -> usize {
    let mut cells = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        cells += c.width().unwrap_or(0);
    }
    cells
}

/// With a leading ellipsis when the start had to go. The composer shows the
/// end, where the user is typing.
pub fn tail_window(text: &str, width: usize) -> (String, usize) {
    let total: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= width {
        return (text.to_string(), total);
    }
    let room = width.saturating_sub(1);
    let mut kept = Vec::new();
    let mut cells = 0usize;
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if cells + w > room {
            break;
        }
        cells += w;
        kept.push(c);
    }
    let mut out = String::from("\u{2026}");
    out.extend(kept.into_iter().rev());
    (out, cells + 1)
}

pub fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub fn pad_box_row(content: &str, width: usize) -> String {
    let inner_w = width.saturating_sub(2);
    let border_color = "\x1b[38;2;95;90;85m";
    let reset = "\x1b[0m";
    let vis = visible_width(content);
    let clipped = if vis > inner_w {
        clip_ansi(content, inner_w)
    } else {
        content.to_string()
    };
    let clipped_vis = visible_width(&clipped);
    let pad = inner_w.saturating_sub(clipped_vis);
    format!("{border_color}│{reset}{clipped}{}{border_color}│{reset}", " ".repeat(pad))
}

/// E.g. "qwen3.6-35b-a3b-uncensored-heretic-native-mtp-preserved-i1" -> "qwen3.6-35b...preserved-i1".
pub fn truncate_middle(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len || max_len < 5 {
        return s.chars().take(max_len).collect();
    }
    let keep = max_len.saturating_sub(1);
    let left = keep.div_ceil(2);
    let right = keep / 2;

    let prefix: String = s.chars().take(left).collect();
    let suffix: String = s.chars().skip(char_count - right).collect();
    format!("{prefix}\u{2026}{suffix}")
}

/// `1 line`, `3 lines`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_window_keeps_the_end_and_counts_wide_characters() {
        assert_eq!(tail_window("short", 10), ("short".to_string(), 5));
        assert_eq!(tail_window("abcdefghij", 5), ("\u{2026}ghij".to_string(), 5));
        // Two cells each: only one fits next to the ellipsis in four cells.
        let (shown, cells) = tail_window("日本語テキスト", 4);
        assert_eq!(shown, "\u{2026}ト");
        assert!(cells <= 4);
    }

    #[test]
    fn a_wrapped_styled_line_does_not_start_its_continuation_with_a_space() {
        // The carried-over style made every continuation row start one column right.
        let styled = format!("\x1b[38;2;200;195;185m{}\x1b[0m", "alpha beta gamma delta epsilon zeta eta theta");
        let rows = wrap_styled(&styled, 12);
        assert!(rows.len() > 1, "{rows:?}");
        for row in &rows[1..] {
            assert!(!strip_ansi(row).starts_with(' '), "continuation starts with a space: {:?}", strip_ansi(row));
        }
        let plain: Vec<String> = rows.iter().map(|r| strip_ansi(r)).collect();
        assert_eq!(plain.join(" "), "alpha beta gamma delta epsilon zeta eta theta", "no word lost or doubled");
    }
}
