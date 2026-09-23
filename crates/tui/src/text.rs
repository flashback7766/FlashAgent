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

/// Text and escape sequences in order, `(true, seq)` for an escape. A CSI
/// sequence ends at its final byte, searched for after `ESC [`: `[` is itself
/// in the final-byte range, and stopping there printed `38;2;220;215;205m`.
fn ansi_pieces(text: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut plain_from = 0;
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            i += 1;
            continue;
        }
        if plain_from < i {
            out.push((false, &text[plain_from..i]));
        }
        let end = if bytes.get(i + 1) == Some(&b'[') {
            bytes[i + 2..].iter().position(|b| (0x40..=0x7e).contains(b)).map_or(bytes.len(), |p| i + 2 + p + 1)
        } else if bytes.get(i + 1) == Some(&b']') {
            // An OSC string runs to BEL or ESC \.
            let body = &bytes[i + 2..];
            body.iter()
                .position(|b| *b == 0x07)
                .map(|p| i + 2 + p + 1)
                .or_else(|| body.windows(2).position(|w| w == b"\x1b\\").map(|p| i + 2 + p + 2))
                .unwrap_or(bytes.len())
        } else {
            (i + 2).min(bytes.len())
        };
        // Never inside a character: an escape's bytes are ASCII up to its end.
        let end = (end..=bytes.len()).find(|&e| text.is_char_boundary(e)).unwrap_or(bytes.len());
        out.push((true, &text[i..end]));
        i = end;
        plain_from = end;
    }
    if plain_from < bytes.len() {
        out.push((false, &text[plain_from..]));
    }
    out
}

fn cells(grapheme: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(grapheme)
}

/// Measured in cells, grapheme by grapheme: a double-width character or an
/// emoji joined into one symbol is never split, and escape codes travel with
/// their piece without counting towards its width.
fn break_wide_word(word: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_vis = 0usize;
    for (escape, piece) in ansi_pieces(word) {
        if escape {
            cur.push_str(piece);
            continue;
        }
        for g in piece.graphemes(true) {
            let w = cells(g);
            if cur_vis > 0 && cur_vis + w > width {
                out.push(std::mem::take(&mut cur));
                cur_vis = 0;
            }
            cur.push_str(g);
            cur_vis += w;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Text from a tool or the model, as a screen row can hold it: a tab becomes
/// spaces to the next stop of four, colour codes stay, and any other escape
/// or control character is dropped. Printed as they are, `ESC[2J` wiped the
/// screen, a cursor move drew over other rows and a tab jumped over cells
/// nothing painted, leaving old text showing through.
pub fn terminal_safe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut col = 0usize;
    for (escape, piece) in ansi_pieces(text) {
        if escape {
            if piece.starts_with("\x1b[") && piece.ends_with('m') {
                out.push_str(piece);
            }
            continue;
        }
        for c in piece.chars() {
            match c {
                '\t' => {
                    let n = 4 - col % 4;
                    out.extend(std::iter::repeat_n(' ', n));
                    col += n;
                }
                '\n' | '\r' => {
                    out.push(' ');
                    col += 1;
                }
                c if c.is_control() => {}
                c => {
                    out.push(c);
                    col += c.width().unwrap_or(0);
                }
            }
        }
    }
    out
}

/// The leading spaces of a styled line, and the rest with every escape found
/// among those spaces moved in front of it.
fn split_indent(line: &str) -> (String, String) {
    let mut lead = String::new();
    let mut escapes = String::new();
    let mut rest_at = line.len();
    for (escape, piece) in ansi_pieces(line) {
        if escape {
            escapes.push_str(piece);
            continue;
        }
        let spaces = piece.len() - piece.trim_start_matches(' ').len();
        lead.push_str(&piece[..spaces]);
        if spaces < piece.len() {
            rest_at = piece.as_ptr() as usize - line.as_ptr() as usize + spaces;
            break;
        }
    }
    (lead, format!("{escapes}{}", &line[rest_at.min(line.len())..]))
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

        // An indented line wraps under its own indent, not at the left edge. The
        // indent is counted past any colour codes in front of it: highlighted code
        // opens with one, and its wrapped lines lost their indentation.
        let (lead, body) = split_indent(line);
        let indent = visible_width(&lead);
        if indent > 0 && indent + 10 <= width {
            let pad = " ".repeat(indent);
            out.extend(wrap_styled(&body, width - indent).into_iter().map(|row| format!("{pad}{row}")));
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
                // Piece by piece: a colour opened inside the word must close at
                // the end of its row and open again on the next.
                for piece in pieces {
                    cur.push_str(&piece);
                    track_style(&piece, &mut active_style);
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
                track_style(&last, &mut active_style);
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
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = String::with_capacity(text.len());
    let mut used = 0;
    // Once something does not fit, nothing after it is text: a narrower
    // character later would make the result something other than a prefix.
    let mut full = false;
    for (escape, piece) in ansi_pieces(text) {
        if escape {
            out.push_str(piece);
            continue;
        }
        for g in piece.graphemes(true) {
            let w = cells(g);
            if full || used + w > width {
                full = true;
                continue;
            }
            used += w;
            out.push_str(g);
        }
    }
    out
}

/// Grapheme by grapheme, as the composer measures, so a joined emoji takes
/// the same cells in both.
pub fn visible_width(text: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    // Most rows: plain ASCII, one cell a byte.
    if text.is_ascii() && !text.contains('\x1b') {
        return text.bytes().filter(|b| !b.is_ascii_control()).count();
    }
    ansi_pieces(text).into_iter().filter(|(escape, _)| !escape).flat_map(|(_, piece)| piece.graphemes(true)).map(cells).sum()
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
    fn a_long_coloured_token_wraps_without_printing_its_escapes() {
        let code = "let v = self.config.backend.settings.values.get(key).unwrap_or_default().to_string();";
        let mut carry = crate::highlight::Carry::default();
        let coloured = crate::highlight::highlight_line("rust", code, &mut carry);
        let rows = wrap_styled(&coloured, 40);
        for row in &rows {
            assert!(visible_width(row) <= 40, "{row:?}");
            assert!(!strip_ansi(row).contains("[0m") && !strip_ansi(row).contains(";2;"), "escape printed as text: {row:?}");
        }
        assert_eq!(rows.iter().map(|r| strip_ansi(r)).collect::<String>().replace(' ', ""), code.replace(' ', ""));
    }

    #[test]
    fn highlighted_code_keeps_its_indent_when_wrapped() {
        let code = "        result = compute_the_value(first_argument, second_argument, third)";
        let mut carry = crate::highlight::Carry::default();
        let coloured = crate::highlight::highlight_line("python", code, &mut carry);
        for row in wrap_styled(&coloured, 60) {
            assert!(strip_ansi(&row).starts_with("        "), "{:?}", strip_ansi(&row));
        }
    }

    #[test]
    fn a_clip_is_a_prefix_of_what_it_clips() {
        assert_eq!(clip_ansi("abcd日本語xyz", 5), "abcd");
        assert_eq!(strip_ansi(&clip_ansi("\x1b[1mab\x1b[0m日x", 3)), "ab");
    }

    #[test]
    fn only_text_and_colour_reach_a_row() {
        assert_eq!(terminal_safe("\tfmt"), "    fmt");
        assert_eq!(terminal_safe("ab\tc"), "ab  c");
        assert_eq!(terminal_safe("\x1b[2J\x1b[Hdone\x1b[31m!\x1b[0m"), "done\x1b[31m!\x1b[0m");
        assert_eq!(terminal_safe("title\x1b]0;evil\x07 ok"), "title ok");
        assert_eq!(terminal_safe("a\nb"), "a b");
    }

    #[test]
    fn a_joined_emoji_is_as_wide_as_the_composer_measures_it() {
        let family = "hi \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} x";
        assert_eq!(visible_width(family), unicode_width::UnicodeWidthStr::width(family));
    }

    #[test]
    fn an_indented_line_wraps_under_its_indent() {
        let rows = wrap_styled("  \x1b[2mstart the server or point it somewhere else with the url\x1b[0m", 24);
        assert!(rows.len() > 2, "{rows:?}");
        for row in &rows {
            assert!(row.starts_with("  ") && !row.starts_with("   "), "{row:?}");
            assert!(visible_width(row) <= 24, "{row:?}");
        }
    }

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
