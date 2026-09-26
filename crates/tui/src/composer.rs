//! The prompt text and cursor. The cursor moves by grapheme: a combining
//! accent, a flag or a ZWJ emoji family is one step and one Backspace.
//! Positions are byte offsets on grapheme boundaries. [`Composer::layout`]
//! folds the text, newlines included, into rows for a given width.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composer {
    text: String,
    /// Always on a grapheme boundary.
    cursor: usize,
}

impl std::ops::Deref for Composer {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for Composer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl From<String> for Composer {
    fn from(text: String) -> Self {
        let cursor = text.len();
        Self { text, cursor }
    }
}

impl From<&str> for Composer {
    fn from(text: &str) -> Self {
        Self::from(text.to_string())
    }
}

impl PartialEq<str> for Composer {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for Composer {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerLayout {
    /// Unstyled, top to bottom.
    pub rows: Vec<String>,
    pub cursor_row: usize,
    /// In cells.
    pub cursor_col: usize,
    /// Rows above the first one shown, and below the last.
    pub hidden_above: usize,
    pub hidden_below: usize,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The cursor goes to the end.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    /// Leaves the composer empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.snap();
    }

    /// Line endings become `\n`; a tab becomes four spaces so rows line up.
    pub fn insert_str(&mut self, s: &str) {
        let clean = normalise(s);
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        self.snap();
    }

    /// False when there was nothing to delete.
    pub fn backspace(&mut self) -> bool {
        let Some(start) = self.prev_boundary(self.cursor) else { return false };
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }

    pub fn delete(&mut self) -> bool {
        let Some(end) = self.next_boundary(self.cursor) else { return false };
        self.text.replace_range(self.cursor..end, "");
        true
    }

    pub fn left(&mut self) -> bool {
        self.prev_boundary(self.cursor).map(|p| self.cursor = p).is_some()
    }

    pub fn right(&mut self) -> bool {
        self.next_boundary(self.cursor).map(|p| self.cursor = p).is_some()
    }

    /// At a line's start already: to the start of the text.
    pub fn home(&mut self) {
        let line_start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        self.cursor = if self.cursor == line_start { 0 } else { line_start };
    }

    /// At a line's end already: to the end of the text.
    pub fn end(&mut self) {
        let line_end = self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i);
        self.cursor = if self.cursor == line_end { self.text.len() } else { line_end };
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start_before(self.cursor);
    }

    pub fn word_right(&mut self) {
        let rest = &self.text[self.cursor..];
        let mut seen_word = false;
        let mut end = rest.len();
        for (i, g) in rest.grapheme_indices(true) {
            let is_word = g.chars().any(char::is_alphanumeric) || g == "_";
            if seen_word && !is_word {
                end = i;
                break;
            }
            seen_word |= is_word;
        }
        self.cursor += end;
    }

    /// Ctrl+W: the word before the cursor and the spaces after it.
    pub fn delete_word_before(&mut self) -> bool {
        let start = self.word_start_before(self.cursor);
        if start == self.cursor {
            return false;
        }
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }

    /// Ctrl+U: back to the start of the line; at the start, the newline before it.
    pub fn delete_to_line_start(&mut self) -> bool {
        let start = match self.text[..self.cursor].rfind('\n') {
            Some(i) if i + 1 == self.cursor => i,
            Some(i) => i + 1,
            None => 0,
        };
        if start == self.cursor {
            return false;
        }
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }

    /// Ctrl+K: to the end of the line; at the end, the newline itself.
    pub fn delete_to_line_end(&mut self) -> bool {
        let end = match self.text[self.cursor..].find('\n') {
            Some(0) => self.cursor + 1,
            Some(i) => self.cursor + i,
            None => self.text.len(),
        };
        if end == self.cursor {
            return false;
        }
        self.text.replace_range(self.cursor..end, "");
        true
    }

    /// False on the first line: that press goes to the prompt history.
    pub fn up(&mut self) -> bool {
        let line_start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        if line_start == 0 {
            return false;
        }
        let col = self.text[line_start..self.cursor].width();
        let prev_start = self.text[..line_start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.cursor = self.offset_at_col(prev_start, line_start - 1, col);
        true
    }

    /// False on the last line.
    pub fn down(&mut self) -> bool {
        let Some(nl) = self.text[self.cursor..].find('\n').map(|i| self.cursor + i) else { return false };
        let line_start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        let col = self.text[line_start..self.cursor].width();
        let next_start = nl + 1;
        let next_end = self.text[next_start..].find('\n').map_or(self.text.len(), |i| next_start + i);
        self.cursor = self.offset_at_col(next_start, next_end, col);
        true
    }

    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    /// Shows at most `max_rows` rows around the cursor. Breaks between words, and
    /// inside a word only when it is wider than the box.
    pub fn layout(&self, width: usize, max_rows: usize) -> ComposerLayout {
        let width = width.max(1);
        let max_rows = max_rows.max(1);
        let mut rows: Vec<String> = Vec::new();
        let mut cursor_at = (0usize, 0usize);
        let mut line_start = 0usize;
        for line in self.text.split('\n') {
            let line_end = line_start + line.len();
            let first_row = rows.len();
            for (row_start, row) in wrap_line(line, width) {
                let abs = line_start + row_start;
                if self.cursor >= abs && self.cursor <= abs + row.len() {
                    cursor_at = (rows.len(), self.text[abs..self.cursor].width());
                }
                rows.push(row.to_string());
            }
            if rows.len() == first_row {
                rows.push(String::new());
                if self.cursor == line_start {
                    cursor_at = (first_row, 0);
                }
            }
            line_start = line_end + 1;
        }
        // A cursor at the end of a full row wraps onto a row of its own. Only the
        // last row of a line gets here (mid-line, the next row already holds the
        // cursor), so the row below belongs to the next line: drawing the cursor
        // there put it before text it would not type in front of.
        if cursor_at.1 >= width {
            cursor_at = (cursor_at.0 + 1, 0);
            rows.insert(cursor_at.0, String::new());
        }
        let total = rows.len();
        let first = if total <= max_rows { 0 } else { cursor_at.0.saturating_sub(max_rows - 1).min(total - max_rows) };
        let last = (first + max_rows).min(total);
        ComposerLayout {
            rows: rows[first..last].to_vec(),
            cursor_row: cursor_at.0 - first,
            cursor_col: cursor_at.1,
            hidden_above: first,
            hidden_below: total - last,
        }
    }

    fn prev_boundary(&self, at: usize) -> Option<usize> {
        self.text[..at].grapheme_indices(true).next_back().map(|(i, _)| i)
    }

    fn next_boundary(&self, at: usize) -> Option<usize> {
        self.text[at..].graphemes(true).next().map(|g| at + g.len())
    }

    fn word_start_before(&self, at: usize) -> usize {
        let mut start = at;
        let mut seen_word = false;
        for (i, g) in self.text[..at].grapheme_indices(true).rev() {
            let is_word = g.chars().any(char::is_alphanumeric) || g == "_";
            if seen_word && !is_word {
                break;
            }
            // Ctrl+W at a line's start takes only the newline.
            if g == "\n" && !seen_word {
                if start == at {
                    start = i;
                }
                break;
            }
            seen_word |= is_word;
            start = i;
        }
        start
    }

    fn offset_at_col(&self, start: usize, end: usize, col: usize) -> usize {
        let mut cells = 0;
        for (i, g) in self.text[start..end].grapheme_indices(true) {
            let w = g.width();
            if cells + w > col {
                return start + i;
            }
            cells += w;
        }
        end
    }

    /// A combining accent typed after a letter joins it; the cursor must not sit between.
    fn snap(&mut self) {
        if self.cursor >= self.text.len() {
            self.cursor = self.text.len();
            return;
        }
        let mut at = 0;
        for g in self.text.graphemes(true) {
            if at + g.len() > self.cursor {
                self.cursor = if at == self.cursor { at } else { at + g.len() };
                return;
            }
            at += g.len();
        }
    }
}

fn normalise(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ")
}

/// (byte offset in the line, row). Breaks after a space when possible.
fn wrap_line(line: &str, width: usize) -> Vec<(usize, &str)> {
    let mut rows = Vec::new();
    let mut row_start = 0;
    let mut cells = 0;
    let mut last_space: Option<usize> = None;
    for (i, g) in line.grapheme_indices(true) {
        let w = g.width();
        if cells + w > width && i > row_start {
            let cut = last_space.filter(|&s| s > row_start).unwrap_or(i);
            rows.push((row_start, &line[row_start..cut]));
            row_start = cut;
            cells = line[row_start..i].width();
            last_space = None;
        }
        cells += w;
        if g == " " {
            last_space = Some(i + 1);
        }
    }
    if row_start < line.len() {
        rows.push((row_start, &line[row_start..]));
    }
    rows
}

/// Case-insensitive, skipping the first `skip` matches from the newest.
pub fn search_history(history: &[String], query: &str, skip: usize) -> Option<usize> {
    let query = query.to_lowercase();
    history
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, h)| h.to_lowercase().contains(&query))
        .nth(skip)
        .map(|(i, _)| i)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistorySearch {
    pub query: String,
    /// Newer matches to pass over.
    pub skip: usize,
    /// Restored on Esc.
    pub draft: String,
}

impl HistorySearch {
    pub fn start(draft: String) -> Self {
        Self { query: String::new(), skip: 0, draft }
    }

    pub fn found<'a>(&self, history: &'a [String]) -> Option<&'a str> {
        search_history(history, &self.query, self.skip).map(|i| history[i].as_str())
    }

    pub fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.skip = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.skip = 0;
    }

    pub fn older(&mut self, history: &[String]) {
        if search_history(history, &self.query, self.skip + 1).is_some() {
            self.skip += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, cursor: usize) -> Composer {
        Composer { text: text.to_string(), cursor }
    }

    fn shown(c: &Composer) -> String {
        format!("{}|{}", &c.text[..c.cursor], &c.text[c.cursor..])
    }

    #[test]
    fn a_cursor_after_a_full_line_is_not_drawn_on_the_next_line() {
        let layout = at("abcde\nxyz", 5).layout(5, 10);
        assert_eq!(layout.rows, vec!["abcde", "", "xyz"]);
        assert_eq!((layout.cursor_row, layout.cursor_col), (1, 0));
        // At the very end the same, with nothing after it.
        let layout = at("abcde", 5).layout(5, 10);
        assert_eq!((layout.cursor_row, layout.cursor_col), (1, 0));
    }

    #[test]
    fn ctrl_u_deletes_back_to_the_start_of_the_line_as_in_a_shell() {
        let mut c = at("first line\nsecond half", 18);
        assert!(c.delete_to_line_start());
        assert_eq!(shown(&c), "first line\n|half");
        // At the start of a line: the newline before it, as Ctrl+K takes the one after.
        assert!(c.delete_to_line_start());
        assert_eq!(shown(&c), "first line|half");
        c.home();
        c.home();
        assert!(!c.delete_to_line_start(), "nothing before the start");
    }

    #[test]
    fn typing_goes_where_the_cursor_is() {
        let mut c = Composer::from("helo");
        c.left();
        c.insert_char('l');
        assert_eq!(shown(&c), "hell|o");
        c.home();
        c.insert_str("oh, ");
        assert_eq!(shown(&c), "oh, |hello");
    }

    #[test]
    fn a_step_is_one_character_as_a_person_counts_them() {
        // e + combining acute, a flag (two code points), a family (seven).
        let family = "👨\u{200d}👩\u{200d}👧";
        let text = format!("e\u{301}🇷🇺{family}ж");
        let mut c = Composer::from(text.as_str());
        let mut steps = 0;
        while c.left() {
            steps += 1;
        }
        assert_eq!(steps, 4);
        c.end();
        assert!(c.backspace());
        assert!(c.backspace());
        assert_eq!(c.text(), "e\u{301}🇷🇺", "Backspace took the family apart");
    }

    #[test]
    fn delete_takes_the_character_under_the_cursor() {
        let mut c = at("abc", 1);
        assert!(c.delete());
        assert_eq!(shown(&c), "a|c");
        c.end();
        assert!(!c.delete());
    }

    #[test]
    fn words_are_jumped_and_deleted_whole() {
        let mut c = Composer::from("git commit --amend");
        c.word_left();
        assert_eq!(shown(&c), "git commit --|amend");
        c.word_left();
        assert_eq!(shown(&c), "git |commit --amend");
        c.word_right();
        assert_eq!(shown(&c), "git commit| --amend");
        c.end();
        assert!(c.delete_word_before());
        assert_eq!(shown(&c), "git commit --|");
        assert!(c.delete_word_before());
        assert_eq!(shown(&c), "git |", "punctuation and the word before it go together");
    }

    #[test]
    fn ctrl_w_at_a_line_start_takes_only_the_newline() {
        let mut c = at("first line\nsecond", 11);
        assert!(c.delete_word_before());
        assert_eq!(shown(&c), "first line|second");
    }

    #[test]
    fn ctrl_k_deletes_to_the_end_of_the_line_then_joins_lines() {
        let mut c = at("one two\nthree", 3);
        assert!(c.delete_to_line_end());
        assert_eq!(shown(&c), "one|\nthree");
        assert!(c.delete_to_line_end());
        assert_eq!(shown(&c), "one|three");
    }

    #[test]
    fn home_and_end_go_to_the_line_first_and_the_text_second() {
        let mut c = at("ab\ncd\nef", 4);
        c.home();
        assert_eq!(c.cursor(), 3);
        c.home();
        assert_eq!(c.cursor(), 0);
        c.set("ab\ncd\nef");
        c.cursor = 3;
        c.end();
        assert_eq!(c.cursor(), 5);
        c.end();
        assert_eq!(c.cursor(), 8);
    }

    #[test]
    fn up_and_down_move_between_lines_keeping_the_column() {
        let mut c = at("long first line\nab\nthird line", 5);
        assert!(!c.up(), "the first line leaves Up to the history");
        assert!(c.down());
        assert_eq!(shown(&c), "long first line\nab|\nthird line", "a short line takes the cursor to its end");
        assert!(c.down());
        assert_eq!(shown(&c), "long first line\nab\nth|ird line");
        assert!(!c.down());
        assert!(c.up());
        assert!(c.up());
        assert_eq!(shown(&c), "lo|ng first line\nab\nthird line");
    }

    #[test]
    fn pasted_line_endings_and_tabs_are_made_even() {
        let mut c = Composer::new();
        c.insert_str("a\r\nb\rc\td");
        assert_eq!(c.text(), "a\nb\nc    d");
        assert_eq!(c.line_count(), 3);
    }

    #[test]
    fn a_long_line_wraps_between_words_and_the_cursor_follows() {
        let c = Composer::from("the quick brown fox");
        let l = c.layout(10, 10);
        assert_eq!(l.rows, ["the quick ", "brown fox"]);
        assert_eq!((l.cursor_row, l.cursor_col), (1, 9));
    }

    #[test]
    fn a_word_wider_than_the_box_is_cut() {
        let l = Composer::from("abcdefghijkl").layout(5, 10);
        assert_eq!(l.rows, ["abcde", "fghij", "kl"]);
    }

    #[test]
    fn a_cursor_after_a_full_row_waits_on_the_next_one() {
        let l = Composer::from("abcde").layout(5, 10);
        assert_eq!(l.rows, ["abcde", ""]);
        assert_eq!((l.cursor_row, l.cursor_col), (1, 0));
    }

    #[test]
    fn wide_characters_are_counted_in_cells() {
        let l = Composer::from("日本語").layout(4, 10);
        assert_eq!(l.rows, ["日本", "語"]);
        assert_eq!((l.cursor_row, l.cursor_col), (1, 2));
    }

    #[test]
    fn empty_lines_are_rows_too() {
        let c = at("a\n\nb", 2);
        let l = c.layout(10, 10);
        assert_eq!(l.rows, ["a", "", "b"]);
        assert_eq!((l.cursor_row, l.cursor_col), (1, 0));
    }

    #[test]
    fn only_the_rows_around_the_cursor_are_shown_when_there_are_too_many() {
        let text = (1..=10).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let mut c = Composer::from(text.as_str());
        let l = c.layout(10, 3);
        assert_eq!(l.rows, ["8", "9", "10"]);
        assert_eq!((l.hidden_above, l.hidden_below, l.cursor_row), (7, 0, 2));
        c.cursor = 0;
        let l = c.layout(10, 3);
        assert_eq!(l.rows, ["1", "2", "3"]);
        assert_eq!((l.hidden_above, l.hidden_below, l.cursor_row), (0, 7, 0));
    }

    #[test]
    fn searching_again_goes_further_back_and_stops_at_the_oldest() {
        let h: Vec<String> = ["cargo test", "git push", "cargo build"].iter().map(|s| s.to_string()).collect();
        let mut search = HistorySearch::start("draft".into());
        search.type_char('c');
        search.type_char('a');
        assert_eq!(search.found(&h), Some("cargo build"));
        search.older(&h);
        assert_eq!(search.found(&h), Some("cargo test"));
        search.older(&h);
        assert_eq!(search.found(&h), Some("cargo test"), "past the oldest match it stays put");
        search.type_char('r');
        assert_eq!(search.skip, 0, "a changed query starts from the newest again");
    }

    #[test]
    fn history_is_searched_newest_first_without_case() {
        let h: Vec<String> = ["Fix the build", "add tests", "fix typo"].iter().map(|s| s.to_string()).collect();
        assert_eq!(search_history(&h, "FIX", 0), Some(2));
        assert_eq!(search_history(&h, "fix", 1), Some(0));
        assert_eq!(search_history(&h, "fix", 2), None);
        assert_eq!(search_history(&h, "nothing", 0), None);
    }
}
