use super::*;

/// Wrap styled text to `width` visible characters, preserving active ANSI escape sequences
/// across line wraps and appending reset codes at line endings.
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
            // Decided by what is visible, not by what is in the buffer: a
            // continuation row starts with the style it carries over, and
            // treating that escape code as text put a space before the first
            // word of every wrapped styled line.
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

            // Track active ANSI style
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
                            active_style = None;
                        } else {
                            active_style = Some(seq.to_string());
                        }
                        j += 1;
                    }
                } else {
                    j += 1;
                }
            }
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

/// "Add the null check" → "add the null check", so it can follow "Failed to".
/// Only the first character, and only when it is not part of an identifier
/// like `MEMORY.md` that would look wrong in lower case.
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

/// `text` with the working directory taken out of any path in it. Models hand
/// back absolute paths, and everything up to the project is where the user
/// already is.
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
        let truncated: String = first_line.chars().take(max_len - 3).collect();
        format!("{truncated}...")
    } else {
        first_line.to_string()
    }
}

/// Re-emit `prefix` after every color reset inside `text`: styled spans
/// (from `md()`) close with a full reset that would otherwise blank the
/// outer line color for the rest of the line (observed: rest of a reasoning
/// line turned plain white after an embedded `**bold**`).
pub fn restore_line_color(text: &str, prefix: &str) -> String {
    text.replace("\x1b[0m", &format!("\x1b[0m{prefix}"))
        .replace("\x1b[39m", &format!("\x1b[39m{prefix}"))
}

/// Clip `text` to `width` visible cells, keeping ANSI escape sequences whole
/// (they are zero-width). Cutting inside a CSI sequence would print its tail
/// literally (`[39m`) and leak style across lines.
pub fn clip_ansi(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cells = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Copy the whole sequence verbatim: ESC [ ... final-byte (@-~).
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

/// Count visible cells in `text`, ignoring ANSI escape sequences.
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

/// Strips all ANSI escape sequences from `text`.
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

/// Format a single row within an enclosed boxed container with vertical border delimiters `│`.
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

/// Truncates string in the middle if it exceeds `max_len`, keeping prefix and suffix.
/// E.g. "qwen3.6-35b-a3b-uncensored-heretic-native-mtp-preserved-i1" -> "qwen3.6-35b...preserved-i1".
pub fn truncate_middle(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len || max_len < 5 {
        return s.chars().take(max_len).collect();
    }
    let keep = max_len.saturating_sub(3);
    let left = keep.div_ceil(2);
    let right = keep / 2;

    let prefix: String = s.chars().take(left).collect();
    let suffix: String = s.chars().skip(char_count - right).collect();
    format!("{prefix}...{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_styled_line_does_not_start_its_continuation_with_a_space() {
        // The carried-over style code made the row look non-empty, so every
        // continuation of a coloured line began one column too far right.
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
