//! The collector of untranslated phrases.
//!
//! The interface speaks two languages, and a phrase that is not in the table
//! shows in English in the middle of a Russian screen. There were a dozen of
//! them and nobody noticed, because a missing translation is invisible: the
//! text is there, it is just in the wrong language.
//!
//! So this walks the sources of the interface and finds the phrases that
//! reach the screen without a translation. A phrase is untranslated when it
//!
//! - is a literal in the sources of the interface,
//! - has no translation in the table,
//! - and looks like something a person reads: in Latin letters, without a
//!   hole in it, and either two words or more, or one word the table of
//!   translations knows.
//!
//! Whether the phrase stands next to a `tr` call does not matter: a call
//! without a translation in the table shows the English all the same, and
//! that was the first kind of gap this collector found — a phrase that went
//! through `tr` and still came out in English, because the table did not
//! hold it. The other way round is a gap as well: a phrase the code asks for
//! and the table does not hold. Both are looked for here, the second one by
//! reading the calls and the first one by reading the literals.
//!
//! The walk had blind spots of its own, and they are closed: a test module
//! in the middle of a file no longer hides the interface after it, and a
//! lifetime (`'a`) is no longer read as a character and swallowed with
//! everything up to the next quote.
//!
//! What is left is either a gap or a phrase that is English on purpose. The
//! second kind is listed below with the reason, so that a new one has to be
//! written down and not merely forgotten.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The phrases that are English on purpose.
///
/// Each one says why, so that the list is a decision and not a dump.
const ENGLISH_ON_PURPOSE: &[(&str, &str)] = &[
    (
        "This session is being continued after context compaction. The summary below covers the earlier conversation.\n",
        "the model reads it",
    ),
    (
        "This session was interrupted while FlashAgent was answering. The last answer below may be cut off mid-sentence; ask again if it ends strangely.",
        "the model reads it",
    ),
    ("Thinking Process:", "the model writes it, the interface reads it back"),
    ("Reply with OK.", "the tool test asks the model, not the person"),
    (
        "FlashAgent TUI\n\nUsage: flashagent [OPTIONS]\n\nOptions:\n  -v, --version        Print version\n  --update             Check and apply updates\n  --channel <name>     Switch release channel (stable, beta)\n  --model <name>       Model to use, saved for the provider in use\n  --url <endpoint>     Talk to this server for this run only (saved providers: /provider)\n  --setup              Run first-time setup wizard\n  --tool-test [--all-models]  Check whether the model can drive tools
      --scenario <key>  Check one scenario (repeatable)
      --json <file>      Write the full results as JSON to a file\n  -r, --resume [id]    Resume a saved session (without an id: pick one from this folder)\n  -c, --continue       Continue the latest session in this folder\n  -y, --yes            Skip directory trust confirmation\n  --uninstall [-y]     Remove FlashAgent; asks what data to delete (-y: take the defaults)\n  -h, --help           Show this help message",
        "the command line is for the person who runs it, in any language",
    ),
    (
        "In-app updater is disabled in development mode (running from source repository or cargo target build).",
        "the command line, for the developer who runs it",
    ),
    (
        "To update your dev build, pull latest git commits and run `cargo build --release`.",
        "the command line, for the developer who runs it",
    ),
    (
        "No crash report was written. Turn on \"Crash reports\" in Settings if you want one.",
        "written to stderr before the interface has chosen a language",
    ),
    ("Failed to", "the model writes this line, the interface reads it back"),
    ("Ran 3 commands", "the model writes this line, the interface reads it back"),
    ("Evaluating Command Output", "a stage the model writes, read back by the interface"),
    ("Evaluating Explored Context", "a stage the model writes, read back by the interface"),
    ("Evaluating Subagent Output", "a stage the model writes, read back by the interface"),
    ("Verifying Code Changes", "a stage the model writes, read back by the interface"),
    ("Reviewing Project Memory", "a stage the model writes, read back by the interface"),
    ("I should ", "the model says it, the interface strips it"),
    ("I will ", "the model says it, the interface strips it"),
    ("Let me ", "the model says it, the interface strips it"),
    ("Write in the language most of the memories are written in.", "a prompt to the model"),
    ("Ctrl+X Ctrl+E", "the names of the keys"),
    ("Alt+Enter, Shift+Enter, Ctrl+J, \\ Enter", "the names of the keys"),
    ("Ctrl+D, Ctrl+C twice", "the names of the keys"),
    ("Target goal:", "a marker in the memory file, not a phrase"),
    ("Call the report_status tool with code 42.", "a prompt to the model"),
    ("Report a numeric status code back to the test harness.", "a prompt to the model"),
    (
        "You are a tool-calling test harness. Respond only by calling the provided tool.",
        "a prompt to the model",
    ),
    ("FAIL: no answer within 120s", "the report of the tool test, for the developer who ran it"),
    ("\nThe tool-calling check is done.", "the report of the tool test, for the developer who ran it"),
    (
        "\nContinue the current task from this summary and the retained messages that follow. If an exact prior message or tool result is needed, read the archive in the session file named below, under its \"archive\" field, subject to normal file permissions. Verify changeable facts before acting.",
        "the model reads it",
    ),
    ("\n[Compaction Stats]: ", "a marker in the system message, the model reads it"),
    ("\n\n[Compacted Conversation History]:\n", "a marker in the system message, the model reads it"),
    ("\n\n[Full Earlier Transcript Archive]:\n", "a marker in the system message, the model reads it"),
    ("# Memory (automatically loaded", "a marker in the session file"),
    ("[AUTONOMOUS GOAL DIRECTIVE]", "a marker in the session file"),
    ("### Stage:", "the model writes it, the interface reads it back"),
    ("To increase", "the mark the server's own message is cut at"),
    ("Upstream request failed:", "the gateway's wrapper around a provider's message, stripped, not shown"),
    (". This is", "the mark an automatic notice is cut at"),
    ("## FlashAgent\n\n", "the name of the assistant in the exported document"),
    (
        "\u{2190}/\u{2192}, Home/End, Ctrl+A/E, Ctrl+W, Ctrl+U, Alt+K",
        "the names of the keys",
    ),
    (
        "\nA model that fails these will talk about doing the work instead of doing it.\nYou can re-run this any time with `flashagent --tool-test`, or compare models\nwith `flashagent --tool-test --all-models`.",
        "the report of the tool test, for the developer who ran it",
    ),
    // --- The prompts the app sends to the model ---
    (
        "Summarize the conversation above for the same assistant to continue the task.",
        "a prompt to the model",
    ),
    (
        "The summaries above are the parts of one conversation. Write one summary of the whole conversation for the same assistant to continue the task.",
        "a prompt to the model",
    ),
    (
        "These are the summaries of the parts of one conversation, in order:\n\n",
        "a prompt to the model",
    ),
    (
        "This is a summary the person asked for: the exact transcript stays in the archive, so keep this readable, but the current task and every instruction that still applies must survive.",
        "a prompt to the model",
    ),
    (
        "The context window is full and the work is not finished. This summary is the assistant's only memory of what has already happened: keep everything that could matter, with exact paths, commands, line numbers, errors verbatim and results. Length is not the problem here; losing a detail is.",
        "a prompt to the model",
    ),
    (
        "An earlier summary of what came before this point is already in the system message; fold it in rather than repeating it.\n",
        "a prompt to the model",
    ),
    (
        "Your previous answer was not accepted: it did not have the required headings. ",
        "a prompt to the model",
    ),
    (
        "Write the summary again, starting with '## Objective' and using exactly these headings, ",
        "a prompt to the model",
    ),
    (
        "Output only the summary: no preamble, no apology, nothing about what you were asked.",
        "a prompt to the model",
    ),
    // The headings the summary must have: a contract with the model, and the
    // model writes them in every summary.
    ("## Objective\n", "the heading the summary must have, a contract with the model"),
    ("## Requirements\n", "the heading the summary must have, a contract with the model"),
    ("## Decisions\n", "the heading the summary must have, a contract with the model"),
    ("## Work State\n", "the heading the summary must have, a contract with the model"),
    ("### Completed\n", "the heading the summary must have, a contract with the model"),
    ("### Active\n", "the heading the summary must have, a contract with the model"),
    ("### Blocked\n", "the heading the summary must have, a contract with the model"),
    ("## Next Move\n", "the heading the summary must have, a contract with the model"),
    ("## Relevant Files\n", "the heading the summary must have, a contract with the model"),
    ("## Important Context\n", "the heading the summary must have, a contract with the model"),
    (
        "Chinese or Japanese, matching the user",
        "the language named in a prompt to the model",
    ),
    (
        "  \\x1b[38;2;225;175;95mAuto-updater is disabled in dev mode\\x1b[0m (running from source repository / cargo build).\n  To update your dev build, pull latest git commits and run `cargo build --release`.",
        "the command line is for the person who runs it, in any language",
    ),
];

/// The sources of the interface, and the table of translations.
fn sources() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "rs"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// The table of translations: the English column of every pair.
///
/// The table is read with the same walk as the sources, so a phrase that is
/// written over several lines is the same phrase as one written on one.
fn table() -> BTreeSet<String> {
    pairs().into_iter().map(|(en, _)| en).collect()
}

/// The table as it is written: every pair, in the order it stands.
///
/// The pairs are read one after another — the English phrase, then the
/// Russian one — so a pair that lost its second half shows as a pair that
/// lost its first, and the guard below catches it.
fn pairs() -> Vec<(String, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("i18n.rs");
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Some(start) = text.find("&[(&str, &str)] = &[") else { return Vec::new() };
    let rest = &text[start..];
    let end = rest.find("\n];").unwrap_or(rest.len());
    let body = &rest[..end];
    let mut all = literals(body).into_iter();
    let mut out = Vec::new();
    while let (Some(en), Some(ru)) = (all.next(), all.next()) {
        out.push((en.text, ru.text));
    }
    out
}

/// One literal of a source.
struct Literal {
    /// What it says, with the escapes read.
    text: String,
    /// The code asked for a translation: the literal is an argument of `tr`
    /// or of `tr_fill`.
    ///
    /// The phrase is looked up in the source that holds it, so the answer is
    /// the same for every call of the same function.
    asked: bool,
}

/// The string literals of a source, with the place they stand at.
///
/// The walk knows the three things a source is made of: code, comments and
/// strings. A phrase in a comment is not shown to anybody, and a phrase in a
/// string is. The phrase is given as the text means it: the escapes read,
/// the line continuations gone.
fn literals(text: &str) -> Vec<Literal> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            // A line comment: nothing in it reaches the screen.
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            // A block comment: the same.
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            // A character literal: a quote, a character, a quote. A lifetime
            // is not one, and reading it as one hides the phrases in between.
            b'\'' => {
                i = char_end(text, i);
            }
            b'"' => {
                // A raw string is skipped whole: it holds code, not words.
                if i > 0 && bytes[i - 1] == b'r' {
                    let mut j = i + 1;
                    let mut hashes = 0;
                    while j < bytes.len() && bytes[j] == b'#' {
                        hashes += 1;
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b'"' {
                        let closing: String =
                            std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                        if let Some(end) = text[j + 1..].find(&closing) {
                            i = j + 1 + end + closing.len();
                            continue;
                        }
                    }
                }
                let start = i + 1;
                let mut phrase = String::new();
                let mut j = start;
                let mut closed = false;
                while j < bytes.len() {
                    match bytes[j] {
                        b'\\' => {
                            // A line continuation: the break and the indent
                            // after it are not part of the text.
                            if bytes.get(j + 1) == Some(&b'\n') {
                                j += 2;
                                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                                    j += 1;
                                }
                            } else {
                                if let Some(next) = text[j + 1..].chars().next() {
                                    phrase.push('\\');
                                    phrase.push(next);
                                }
                                j += 2;
                            }
                        }
                        b'"' => {
                            closed = true;
                            j += 1;
                            break;
                        }
                        _ => {
                            let rest = &text[j..];
                            let c = rest.chars().next().unwrap_or(' ');
                            phrase.push(c);
                            j += c.len_utf8();
                        }
                    }
                }
                if !closed {
                    break;
                }
                out.push(Literal { text: unescape(&phrase), asked: asks_for_a_translation(text, i) });
                i = j;
            }
            _ => i += 1,
        }
    }
    out
}

/// Whether the literal stands where a translation was asked for.
///
/// The call is named right before the phrase: `tr("...")`, `tr_fill("...")`,
/// `i18n::tr("...")`. A literal anywhere else is shown as it is. The name is
/// read whole: `push_str("...")` ends in `tr(` as well, and it translates
/// nothing.
fn asks_for_a_translation(text: &str, quote_at: usize) -> bool {
    matches!(call_name(text, quote_at).as_deref(), Some("tr") | Some("tr_fill"))
}

/// The name of the call a literal stands in.
///
/// The name is the word before the open bracket, read whole: `push_str("...")`
/// ends in `tr(` as well, and it translates nothing.
fn call_name(text: &str, quote_at: usize) -> Option<String> {
    let before = text[..quote_at].trim_end();
    let open = before.strip_suffix('(')?;
    let name: String = open
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    Some(name)
}

/// The source without its test modules: a phrase in a test never reaches the
/// screen.
///
/// The whole module is taken out, not everything after the first
/// `#[cfg(test)]`: a file may hold a test module in the middle and the
/// interface after it, and that part used to go unread.
fn without_tests(text: &str) -> String {
    const MARK: &str = "#[cfg(test)]";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARK) {
        out.push_str(&rest[..at]);
        let after = &rest[at + MARK.len()..];
        let Some(open) = after.find('{') else { break };
        let Some(len) = block_len(&after[open..]) else { break };
        rest = &after[open + len..];
    }
    out.push_str(rest);
    out
}

/// The end of the character literal or of the lifetime that starts at the
/// quote.
///
/// A lifetime (`'a`, `'static`) is not a character: treating it as one
/// swallows everything up to the next quote, and the phrases in between go
/// unread.
fn char_end(text: &str, quote_at: usize) -> usize {
    let bytes = text.as_bytes();
    if let Some(next) = bytes.get(quote_at + 1) {
        if next.is_ascii_alphabetic() || *next == b'_' {
            let mut j = quote_at + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if bytes.get(j) != Some(&b'\'') {
                return quote_at + 1;
            }
        }
    }
    let mut j = quote_at + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'\'' => return j + 1,
            _ => j += 1,
        }
    }
    bytes.len()
}

/// The length of the block that opens at the start of the text.
///
/// The braces are counted; the ones inside a string, a character or a comment
/// are not. The answer is where the block ends, so the code after it can be
/// read again.
fn block_len(text: &str) -> Option<usize> {
    bracket_len(text, 0, b'{', b'}')
}

/// The length of the bracketed run that opens at `open`: the place where the
/// bracket that closes it stands.
///
/// The same counting as the block above, with the brackets of the call: the
/// ones inside a string, a character or a comment are not counted, so a
/// bracket in a phrase does not end the call early.
fn bracket_len(text: &str, open: usize, open_ch: u8, close_ch: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'\'' => {
                i = char_end(text, i);
            }
            b'"' => {
                i = string_end(text, i);
            }
            _ if bytes[i] == open_ch => {
                depth += 1;
                i += 1;
            }
            _ if bytes[i] == close_ch => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// The end of the string literal that starts at the opening quote.
fn string_end(text: &str, quote_at: usize) -> usize {
    let bytes = text.as_bytes();
    let mut j = quote_at + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    bytes.len()
}

/// The source text of a literal, with the escapes read as what they mean, so
/// that a phrase in the table and a phrase in the source are the same text.
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                // A codepoint: \u{2190} is the arrow.
                let mut rest = String::new();
                let mut closed = false;
                for next in chars.by_ref() {
                    if next == '}' {
                        closed = true;
                        break;
                    }
                    rest.push(next);
                }
                match u32::from_str_radix(rest.trim_start_matches('{'), 16).ok().and_then(char::from_u32) {
                    Some(c) if closed => out.push(c),
                    _ => {
                        out.push('\\');
                        out.push('u');
                        out.push_str(&rest);
                        if closed {
                            out.push('}');
                        }
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The colour codes and other marks taken off, so that what is left is what
/// the person reads.
fn strip_codes(phrase: &str) -> String {
    let mut out = String::with_capacity(phrase.len());
    let mut chars = phrase.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // A colour code written out: it ends at the letter that names it.
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        // An escape in the source: \x1b, \u{1b}, \n.
        let mut mark = String::new();
        for next in chars.by_ref() {
            mark.push(next);
            if next.is_ascii_alphabetic() && mark.len() > 1 {
                break;
            }
        }
        if mark.starts_with('x') || mark.starts_with('u') {
            // The colour code that follows the mark: [ ... m
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else if mark.starts_with('u') {
                for next in chars.by_ref() {
                    if next == '}' {
                        break;
                    }
                }
            }
            continue;
        }
        out.push_str(&mark);
    }
    out
}

/// Whether the phrase looks like something a person reads.
///
/// Two words or more are read when the first letter is a capital one: that
/// was the old rule, and it kept the names of the code out. Two more kinds
/// used to slip past it:
///
/// - a label that ends with a colon (`find in history:`): the small letter
///   and the colon are what a label looks like;
/// - a word the table of translations knows (`used`, `Searched`, `Memory`,
///   `Editing`, `stopped`): the table holds the words of the interface, so a
///   word it knows is a word a person reads, and a word it does not know is
///   a name, a path or a command — `config`, `src/main.rs`, `uvx` — and those
///   are not something a person reads.
fn looks_like_text(phrase: &str, table: &BTreeSet<String>) -> bool {
    let plain = strip_codes(phrase);
    if !plain.is_ascii() {
        return false;
    }
    if plain.contains('{') || plain.contains('}') {
        return false;
    }
    let words = plain.split_whitespace().count();
    if words == 0 {
        return false;
    }
    if table.contains(&plain) {
        return true;
    }
    if words == 1 {
        return false;
    }
    // The first letter, not the first character: a phrase that starts with a
    // colour code starts with a backslash.
    let first = plain.chars().find(|c| c.is_alphabetic()).unwrap_or(' ');
    first.is_ascii_uppercase() || plain.ends_with(':')
}

/// The words of every call of `plural` in a source.
///
/// The call is read as the source spells it: the strings it names, in the
/// order they stand. The count is not a word, so only the strings count.
fn counted_calls(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for open in plural_calls(text) {
        let Some(len) = bracket_len(text, open, b'(', b')') else { continue };
        out.push(literals(&text[open..len]).into_iter().map(|l| l.text).collect());
    }
    out.extend(noun_pairs(text));
    out
}

/// The pairs of every counted noun in a source: the `one` and `many` of a
/// `Noun`, which the menu shows in the singular and the count shows in the
/// plural.
fn noun_pairs(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("one: \"") {
        let after = &rest[at + "one: \"".len()..];
        let end = string_end(after, 0);
        if end == 0 {
            break;
        }
        let one = after[..end - 1].to_string();
        let tail = &after[end..];
        let Some(m_at) = tail.find("many: \"") else {
            rest = tail;
            continue;
        };
        let m_after = &tail[m_at + "many: \"".len()..];
        let m_end = string_end(m_after, 0);
        if m_end == 0 {
            rest = tail;
            continue;
        }
        let many = m_after[..m_end - 1].to_string();
        out.push(vec![one, many]);
        rest = &m_after[m_end..];
    }
    out
}

/// Where every call of `plural` opens.
///
/// The name is read whole: `plural` is the call and `plural_word` is not, and
/// a name inside a string or a comment is not a call either.
fn plural_calls(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'\'' => {
                i = char_end(text, i);
            }
            b'"' => {
                i = string_end(text, i);
            }
            b'p' if text[i..].starts_with("plural") => {
                let after = i + "plural".len();
                let before = text[..i].chars().next_back();
                let whole = before.is_none_or(|c| !c.is_alphanumeric() && c != '_');
                // A definition is not a call: `fn plural` declares the helper.
                let declared = word_before(text, i) == "fn";
                if whole && !declared && text[after..].starts_with('(') {
                    out.push(after);
                }
                i = after;
            }
            _ => i += 1,
        }
    }
    out
}

/// The word that ends where `at` begins, the space before it skipped: the
/// name in front of it.
fn word_before(text: &str, at: usize) -> &str {
    let before = text[..at].trim_end();
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map_or(0, |(i, _)| i);
    &before[start..]
}

/// The counted words: the pair of English words the code counts, and the three
/// Russian forms the table holds for that pair.
///
/// The code says which words it counts; the table says how Russian writes
/// them. A pair the table does not hold shows English in the middle of a
/// Russian screen, and nothing says so: the word is there, it is just in the
/// wrong language.
///
/// So every pair the code asks for is looked up in the table, and the three
/// forms it must hold are counted. A call that carries the Russian with it
/// is caught too: the words of the interface live in the table, not in the
/// code.
#[test]
fn every_pair_the_code_counts_is_in_the_table_with_three_forms() {
    let table: BTreeMap<String, String> = pairs().into_iter().collect();
    let mut gaps: Vec<String> = Vec::new();
    for path in sources() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if name == "i18n.rs" || name == "untranslated.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // The tests are not the interface.
        let body = without_tests(&text);
        for words in counted_calls(&body) {
            if words.iter().any(|w| !w.is_ascii()) {
                gaps.push(format!("{name}: the call of plural carries the Russian with it: {words:?}"));
                continue;
            }
            if words.len() != 2 {
                // A call whose words are variables carries no Russian with it:
                // the words come from the table, and the pairs of those are
                // checked where they are written down.
                continue;
            }
            let key = format!("plural: {}/{}", words[0], words[1]);
            match table.get(&key) {
                Some(ru) => {
                    let forms: Vec<&str> = ru.split('|').collect();
                    if forms.len() != 3 || forms.iter().any(|f| f.trim().is_empty()) {
                        gaps.push(format!("{name}: {key:?} holds {} forms, not three", forms.len()));
                    }
                }
                None => gaps.push(format!("{name}: {key:?} is not in the table")),
            }
        }
    }
    gaps.sort();
    gaps.dedup();
    assert!(gaps.is_empty(), "these counted words have no Russian forms:\n{}", gaps.join("\n"));
}

#[test]
fn every_phrase_the_interface_shows_is_translated() {
    let table = table();
    let mut untranslated: Vec<String> = Vec::new();
    for path in sources() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if name == "i18n.rs" || name == "untranslated.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // The tests are not the interface.
        let body = without_tests(&text);
        for literal in literals(&body) {
            if !looks_like_text(&literal.text, &table) {
                continue;
            }
            if table.contains(&literal.text) {
                continue;
            }
            if ENGLISH_ON_PURPOSE.iter().any(|(p, _)| *p == literal.text) {
                continue;
            }
            untranslated.push(format!("{name}: {:?}", literal.text));
        }
    }
    untranslated.sort();
    untranslated.dedup();
    assert!(
        untranslated.is_empty(),
        "these phrases reach the screen in English only:\n{}",
        untranslated.join("\n")
    );
}

/// The other kind of gap: the code asked for a translation and the table did
/// not hold the phrase.
///
/// The phrase goes through `tr` and comes out in English all the same, in the
/// middle of a Russian screen. It is not found by looking the table up — the
/// table has no such entry — but by walking the sources.
#[test]
fn every_phrase_the_code_asks_for_is_in_the_table() {
    let table = table();
    let mut missing: Vec<String> = Vec::new();
    for path in sources() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if name == "i18n.rs" || name == "untranslated.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let body = without_tests(&text);
        for literal in literals(&body) {
            if !literal.asked {
                continue;
            }
            if table.contains(&literal.text) {
                continue;
            }
            if ENGLISH_ON_PURPOSE.iter().any(|(p, _)| *p == literal.text) {
                continue;
            }
            missing.push(format!("{name}: {:?}", literal.text));
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "these phrases are asked for and the table does not hold them:\n{}",
        missing.join("\n")
    );
}

/// The table of translations is read by the walk, and the walk takes the first
/// column of every pair as the English one.
///
/// Were the columns ever swapped, the collector would look for Russian in the
/// sources, find nothing, and the test above would pass in vain. So the
/// table is checked: it is not empty, its second column is Russian, and its
/// first column holds a phrase the sources use.
#[test]
fn the_table_is_read_as_it_is_written() {
    let pairs = pairs();
    assert!(pairs.len() > 100, "the table of translations was not read: {} pairs", pairs.len());
    assert!(
        pairs.iter().any(|(_, ru)| !ru.is_ascii()),
        "not one pair in the table has a Russian translation: the columns are swapped"
    );
    let in_sources: BTreeSet<String> = sources()
        .into_iter()
        .filter(|path| {
            !matches!(path.file_name().and_then(|n| n.to_str()), Some("i18n.rs") | Some("untranslated.rs"))
        })
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|text| literals(&text).into_iter().map(|l| l.text))
        .collect();
    assert!(
        pairs.iter().any(|(en, _)| in_sources.contains(en)),
        "not one phrase of the table is used in the sources: the columns are swapped"
    );
}

/// The collector has to see the phrases of the interface. Were the walk to
/// stop early, it would find nothing and the test above would pass on
/// emptiness.
#[test]
fn the_collector_sees_the_phrases_of_the_interface() {
    let table = table();
    let sample = "A phrase the collector has never seen";
    assert!(looks_like_text(sample, &table));
    assert!(!table.contains(sample));
    // The words the collector used to look past.
    for phrase in ["used", "Searched", "Memory", "Editing", "stopped"] {
        assert!(table.contains(phrase), "{phrase:?} is not in the table");
        assert!(looks_like_text(phrase, &table), "{phrase:?} is not read");
    }
    assert!(looks_like_text("find in history:", &table));
    assert!(looks_like_text("no match", &table));
    // The names of the code are not phrases.
    for phrase in ["config", "src/main.rs", "uvx", "run_shell"] {
        assert!(!looks_like_text(phrase, &table), "{phrase:?} is read as if it were a phrase");
    }
}

#[test]
fn the_allowlist_is_not_lying() {
    // A phrase on the list that is not in the sources any more, or that has a
    // translation after all, is a stale entry: it hides nothing, but it
    // makes the list look longer than the problem is.
    let table = table();
    let mut in_sources: BTreeSet<String> = BTreeSet::new();
    for path in sources() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name == "i18n.rs" || name == "untranslated.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for literal in literals(&text) {
            in_sources.insert(literal.text);
        }
    }
    let mut stale: Vec<&str> = Vec::new();
    for (phrase, why) in ENGLISH_ON_PURPOSE {
        assert!(!why.trim().is_empty(), "{phrase:?} is on the list without a reason");
        if !table.contains(*phrase) && !in_sources.contains(*phrase) {
            stale.push(phrase);
        }
    }
    assert!(stale.is_empty(), "these are on the list and are nowhere: {stale:#?}");
}


