//! Degenerate repetition in a streamed reply: the same line three times, two
//! lines taking turns, a phrase coming back three times, or a run of text
//! repeating itself without a newline. The loop asks after every delta, so
//! each byte is looked at once: lines and clauses are counted as they end, and
//! only the line being written is judged again.

use std::collections::{HashMap, HashSet};

/// Shorter text is never judged.
const MIN_TEXT: usize = 24;
/// Shorter lines (braces, markdown markers) repeat legitimately.
const MIN_LINE: usize = 6;
/// A phrase this long that comes back three times is an overthinking loop.
const MIN_CLAUSE: usize = 25;

/// Code repeats itself legitimately (closing tags, one decorator on two
/// functions, `if __name__` in two examples), so only prose outside code
/// fences is judged by lines and phrases.
#[derive(Default)]
pub(crate) struct RepetitionWatch {
    /// Everything before this was searched for line and clause ends.
    scanned: usize,
    line_start: usize,
    /// Inside the line being written.
    clause_start: usize,
    in_fence: bool,
    prose_lines: usize,
    /// The last six finished prose lines, trimmed, oldest first.
    recent: Vec<(usize, usize)>,
    /// Of the line being written, once a phrase in it is long enough to count.
    line_is_prose: Option<bool>,
    /// The phrase counted last in the line being written: the same one again
    /// right after it counts once.
    last_clause: Option<(usize, usize)>,
    clause_counts: HashMap<String, u32>,
    /// So an unfinished phrase is only hashed when some counted one is as long.
    clause_lengths: HashSet<usize>,
    clause_thrice: bool,
}

impl RepetitionWatch {
    /// `text` is the text of the last call with more appended.
    pub(crate) fn is_looping(&mut self, text: &str) -> bool {
        if text.len() < self.scanned {
            *self = Self::default();
        }
        self.advance(text);
        if text.len() < MIN_TEXT {
            return false;
        }

        let partial = text[self.line_start..].trim();
        let partial_is_prose = !self.in_fence && !partial.is_empty() && !partial.starts_with("```");
        let lines = self.prose_lines + usize::from(partial_is_prose);
        let mut last: Vec<&str> = self.recent.iter().map(|&(start, end)| &text[start..end]).collect();
        if partial_is_prose {
            last.push(partial);
        }
        // 1 is the last line.
        let back = |n: usize| last[last.len() - n];
        if lines >= 3 && back(1).len() >= MIN_LINE && back(2) == back(1) && back(3) == back(1) {
            return true;
        }
        // A B A B A B
        if lines >= 6
            && back(1) == back(3)
            && back(3) == back(5)
            && back(2) == back(4)
            && back(4) == back(6)
            && (back(1).len() >= MIN_LINE || back(2).len() >= MIN_LINE)
        {
            return true;
        }
        if tail_repeats(text.as_bytes()) {
            return true;
        }
        lines >= 4 && (self.clause_thrice || (partial_is_prose && self.unfinished_clause_is_third(text)))
    }

    fn advance(&mut self, text: &str) {
        let bytes = text.as_bytes();
        while let Some(found) = bytes[self.scanned..].iter().position(|b| matches!(b, b'\n' | b'.' | b'!' | b'?' | b';')) {
            let at = self.scanned + found;
            self.end_clause(text, at);
            if bytes[at] == b'\n' {
                self.end_line(text, at);
            }
            self.clause_start = at + 1;
            self.scanned = at + 1;
        }
        self.scanned = bytes.len();
    }

    fn end_clause(&mut self, text: &str, end: usize) {
        let (start, end) = trimmed(text, self.clause_start, end);
        if end - start < MIN_CLAUSE {
            return;
        }
        // A phrase that long means the line's first characters are in.
        let (in_fence, line_start) = (self.in_fence, self.line_start);
        let prose = *self.line_is_prose.get_or_insert_with(|| !in_fence && !text[line_start..].trim_start().starts_with("```"));
        let clause = &text[start..end];
        if !prose || self.last_clause.is_some_and(|(s, e)| &text[s..e] == clause) {
            return;
        }
        self.last_clause = Some((start, end));
        let count = match self.clause_counts.get_mut(clause) {
            Some(count) => {
                *count += 1;
                *count
            }
            None => {
                self.clause_counts.insert(clause.to_string(), 1);
                self.clause_lengths.insert(clause.len());
                1
            }
        };
        self.clause_thrice |= count >= 3;
    }

    fn end_line(&mut self, text: &str, end: usize) {
        let (start, end_trimmed) = trimmed(text, self.line_start, end);
        let line = &text[start..end_trimmed];
        if line.starts_with("```") {
            self.in_fence = !self.in_fence;
        } else if !self.in_fence && !line.is_empty() {
            self.prose_lines += 1;
            if self.recent.len() == 6 {
                self.recent.remove(0);
            }
            self.recent.push((start, end_trimmed));
        }
        self.line_start = end + 1;
        self.line_is_prose = None;
        self.last_clause = None;
    }

    fn unfinished_clause_is_third(&self, text: &str) -> bool {
        let clause = text[self.clause_start..].trim();
        clause.len() >= MIN_CLAUSE
            && self.clause_lengths.contains(&clause.len())
            && self.last_clause.is_none_or(|(s, e)| &text[s..e] != clause)
            && self.clause_counts.get(clause).is_some_and(|count| count + 1 >= 3)
    }
}

/// The byte range of `text[start..end]` without surrounding whitespace.
fn trimmed(text: &str, start: usize, end: usize) -> (usize, usize) {
    let slice = &text[start..end];
    let from = start + (slice.len() - slice.trim_start().len());
    (from, from + slice.trim().len())
}

/// Four times over at the very end, at least 48 bytes. A table rule
/// (`|---|---|`) or a line of `=` is decoration, not a loop.
fn tail_repeats(bytes: &[u8]) -> bool {
    let len = bytes.len();
    (8..=80usize).any(|w| {
        if len < w * 4 || w * 4 < 48 {
            return false;
        }
        let c1 = &bytes[len - w..];
        (2..=4).all(|k| &bytes[len - w * k..len - w * (k - 1)] == c1)
            && !c1.iter().all(|&b| b == c1[0])
            && !c1.iter().all(|b| b"|-:=*#_~. \n".contains(b))
    })
}

/// Keeps a single instance of the repeated pattern.
pub(crate) fn clean_repetition_loop(text: &mut String) {
    let trimmed = text.trim_end();
    if let Some(last_line) = trimmed.lines().rev().find(|l| !l.trim().is_empty()) {
        let pattern = last_line.trim();
        if pattern.len() >= MIN_LINE {
            let mut lines: Vec<&str> = text.lines().collect();
            // The trailing run of the pattern, blank lines between allowed.
            let mut repeat_count = 0;
            let mut first = lines.len();
            for (i, l) in lines.iter().enumerate().rev() {
                if l.trim() == pattern {
                    repeat_count += 1;
                    first = i;
                } else if !l.trim().is_empty() {
                    break;
                }
            }
            if repeat_count > 1 {
                lines.truncate(first + 1);
                *text = lines.join("\n");
                return;
            }
        }
    }

    let bytes = text.as_bytes();
    let len = bytes.len();
    for w in 8..=80 {
        if len >= w * 3 {
            let c1 = &bytes[len - w..];
            let c2 = &bytes[len - w * 2..len - w];
            let c3 = &bytes[len - w * 3..len - w * 2];
            if c1 == c2 && c2 == c3 && !c1.iter().all(|&b| b == c1[0]) {
                // A byte period can start inside a Cyrillic letter.
                let mut cut_pos = len - w * 2;
                while !text.is_char_boundary(cut_pos) {
                    cut_pos -= 1;
                }
                text.truncate(cut_pos);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looping(text: &str) -> bool {
        RepetitionWatch::default().is_looping(text)
    }

    /// The judgement as it was made before it was incremental: the whole text,
    /// every time. The watch must agree with it on every prefix.
    fn rescanned(text: &str) -> bool {
        let bytes = text.as_bytes();
        if bytes.len() < 24 {
            return false;
        }
        let mut in_fence = false;
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| {
                if l.starts_with("```") {
                    in_fence = !in_fence;
                    return false;
                }
                !in_fence && !l.is_empty()
            })
            .collect();
        if lines.len() >= 3 {
            let last = lines[lines.len() - 1];
            if last.len() >= 6 && lines[lines.len() - 2] == last && lines[lines.len() - 3] == last {
                return true;
            }
            if lines.len() >= 6 {
                let n = lines.len();
                if lines[n - 1] == lines[n - 3]
                    && lines[n - 3] == lines[n - 5]
                    && lines[n - 2] == lines[n - 4]
                    && lines[n - 4] == lines[n - 6]
                    && (lines[n - 1].len() >= 6 || lines[n - 2].len() >= 6)
                {
                    return true;
                }
            }
        }
        if tail_repeats(bytes) {
            return true;
        }
        if lines.len() >= 4 {
            let mut clause_counts = HashMap::new();
            for line in &lines {
                let mut clauses: Vec<&str> = line.split(&['.', '!', '?', ';'][..]).map(str::trim).filter(|s| s.len() >= 25).collect();
                clauses.dedup();
                for clause in clauses {
                    let count = clause_counts.entry(clause).or_insert(0);
                    *count += 1;
                    if *count >= 3 {
                        return true;
                    }
                }
            }
        }
        false
    }

    const SAMPLES: &[&str] = &[
        "*Wait, I'll call list_dir.*\n*Wait, I'll call list_dir.*\n*Wait, I'll call list_dir.*\n",
        "First line of answer.\nSecond line with different content.\nThird line.",
        "Two examples:\n```python\nif __name__ == \"__main__\":\n    main()\n```\nand\n```python\nif __name__ == \"__main__\":\n    run()\n```\nBoth work.",
        "```rust\n#[derive(Debug, Clone, PartialEq)]\nstruct A;\n#[derive(Debug, Clone, PartialEq)]\nstruct B;\n```",
        "| a | b | c | d | e | f |\n|---|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 | 6 |",
        "The configuration file is read at startup\nThe configuration file is read at startup, then cached.\nDone.\nOk.",
        "Response construction:\nTurn it over. Then the sealed top becomes the bottom, and the open bottom becomes the top.\nWait, if I am an AI assistant for coding, should I even answer riddles?\nFinal decision:\nTurn it over. Then the sealed top becomes the bottom, and the open bottom becomes the top.\nWait, I don't need to translate my thought process into Russian.\n\"Turn it over. Then the sealed top becomes the bottom, and the open bottom becomes the top.\"\nWait, I'll check if there are any other interpretations.",
        "Check the input first.\nThen run the tests.\nCheck the input first.\nThen run the tests.\nCheck the input first.\nThen run the tests.\n",
        "Ответ: повторяю снова повторяю снова повторяю снова повторяю снова повторяю снова",
        "the clause that is long enough to count here; another clause long enough to count too; the clause that is long enough to count here; another clause long enough to count too; the clause that is long enough to count here\nsecond\nthird\nfourth",
        "line one\r\nline two is here\r\nline two is here\r\nline two is here\r\n",
        "```\nsame line again and again\nsame line again and again\nsame line again and again\n```\nprose after the fence ends here.",
        "  indented and repeated line  \n\tindented and repeated line\nindented and repeated line   \n",
        "abcdefgh1234abcdefgh1234abcdefgh1234abcdefgh1234abcdefgh1234",
        "We should look at the parser module again. We should look at the parser module again.\nNo.\nYes.\nWe should look at the parser module again!",
        "``not a fence yet but a long enough phrase to count here. ``not a fence\nx\ny\nz",
    ];

    #[test]
    fn the_watch_agrees_with_a_full_rescan_after_every_delta() {
        for sample in SAMPLES {
            for step in [1, 2, 3, 5, 8, 13, 40, usize::MAX] {
                let mut watch = RepetitionWatch::default();
                let mut end = 0;
                while end < sample.len() {
                    end = end.saturating_add(step).min(sample.len());
                    while !sample.is_char_boundary(end) {
                        end += 1;
                    }
                    let prefix = &sample[..end];
                    assert_eq!(watch.is_looping(prefix), rescanned(prefix), "step {step}, prefix {prefix:?}");
                }
            }
        }
    }

    #[test]
    fn a_long_reply_is_watched_in_linear_time() {
        // Hundreds of thousands of deltas: a full rescan after each is quadratic
        // and took seconds.
        let line = |i: usize| format!("Step {i} checks another part of the program before moving on to the next one.\n");
        let mut text = String::new();
        let mut watch = RepetitionWatch::default();
        let started = std::time::Instant::now();
        for i in 0..5_000 {
            for piece in line(i).split_inclusive(' ') {
                text.push_str(piece);
                assert!(!watch.is_looping(&text), "a varied reply was taken for a loop at line {i}");
            }
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "took {:?}", started.elapsed());
    }

    #[test]
    fn every_kind_of_loop_is_caught_and_code_is_left_alone() {
        let caught = [
            "Check the input first.\nThen run the tests.\nCheck the input first.\nThen run the tests.\nCheck the input first.\nThen run the tests.\n",
            "line one\r\nline two is here\r\nline two is here\r\nline two is here\r\n",
            "  indented and repeated line  \n\tindented and repeated line\nindented and repeated line   \n",
            "abcdefgh1234abcdefgh1234abcdefgh1234abcdefgh1234abcdefgh1234",
            "the clause that is long enough to count here; another clause long enough to count too; the clause that is long enough to count here; another clause long enough to count too; the clause that is long enough to count here\nsecond\nthird\nfourth",
        ];
        for text in caught {
            assert!(looping(text), "{text:?}");
        }
        assert!(!looping("```\nsame line again and again\nsame line again and again\nsame line again and again\n```\nprose after the fence ends here."));
    }

    #[test]
    fn identical_lines_are_a_loop() {
        assert!(looping("*Wait, I'll call list_dir.*\n*Wait, I'll call list_dir.*\n*Wait, I'll call list_dir.*\n"));
        assert!(!looping("First line of answer.\nSecond line with different content.\nThird line."));
    }

    #[test]
    fn ordinary_answers_are_not_taken_for_a_loop() {
        for text in [
            "Two examples:\n```python\nif __name__ == \"__main__\":\n    main()\n```\nand\n```python\nif __name__ == \"__main__\":\n    run()\n```\nBoth work.",
            "```rust\n#[derive(Debug, Clone, PartialEq)]\nstruct A;\n#[derive(Debug, Clone, PartialEq)]\nstruct B;\n```",
            "```html\n<div><div><div>\n</div>\n</div>\n</div>\n```",
            "| a | b | c | d | e | f |\n|---|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 | 6 |",
            "The configuration file is read at startup\nThe configuration file is read at startup, then cached.\nDone.\nOk.",
        ] {
            assert!(!looping(text), "{text}");
        }
    }

    #[test]
    fn a_phrase_coming_back_on_separate_lines_is_a_loop() {
        let text = "Response construction:\n\
                    Turn it over. Then the sealed top becomes the bottom, and the open bottom becomes the top.\n\
                    Wait, if I am an AI assistant for coding, should I even answer riddles?\n\
                    Final decision:\n\
                    Turn it over. Then the sealed top becomes the bottom, and the open bottom becomes the top.\n\
                    Wait, I don't need to translate my thought process into Russian.\n\
                    \"Turn it over. Then the sealed top becomes the bottom, and the open bottom becomes the top.\"\n\
                    Wait, I'll check if there are any other interpretations.";
        assert!(looping(text));
    }

    #[test]
    fn a_loop_in_cyrillic_is_trimmed_without_a_panic() {
        let mut text = format!("Ответ: {}", "повторяю снова ".repeat(8));
        text.pop();
        text.push('ё');
        let mut cyr = format!("x{}", "ёжик ёлка ".repeat(10));
        clean_repetition_loop(&mut text);
        clean_repetition_loop(&mut cyr);
    }

    #[test]
    fn cleaning_keeps_one_copy_of_the_repeated_line() {
        let mut text = "Here is the plan:\n*Wait, I'll call list_dir.*\n*Wait, I'll call list_dir.*\n*Wait, I'll call list_dir.*".to_string();
        clean_repetition_loop(&mut text);
        assert_eq!(text, "Here is the plan:\n*Wait, I'll call list_dir.*");
        let mut spaced = "Plan:\nI'll check the file now.\n\nI'll check the file now.\nI'll check the file now.\n".to_string();
        clean_repetition_loop(&mut spaced);
        assert_eq!(spaced, "Plan:\nI'll check the file now.");
    }
}
