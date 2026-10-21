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
//! - and looks like something a person reads: two words or more, in Latin
//!   letters, written with a capital, without a hole in it.
//!
//! Whether the phrase stands next to a `tr` call does not matter: a call
//! without a translation in the table shows the English all the same, and
//! that was the first kind of gap this collector found — a phrase that went
//! through `tr` and still came out in English, because the table did not
//! hold it.
//!
//! What is left is either a gap or a phrase that is English on purpose. The
//! second kind is listed below with the reason, so that a new one has to be
//! written down and not merely forgotten.

use std::collections::BTreeSet;
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
    (". To increase", "the mark the server's own message is cut at"),
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
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("i18n.rs");
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeSet::new() };
    let Some(start) = text.find("&[(&str, &str)] = &[") else { return BTreeSet::new() };
    let rest = &text[start..];
    let end = rest.find("\n];").unwrap_or(rest.len());
    let body = &rest[..end];
    let mut out = BTreeSet::new();
    let mut all = literals(body).into_iter();
    // The pairs come one after another: the English phrase, then the Russian.
    let mut take_english = true;
    for (_, phrase) in all.by_ref() {
        if take_english {
            out.insert(phrase);
        }
        take_english = !take_english;
    }
    out
}

/// The string literals of a source, with the place they stand at.
///
/// The walk knows the three things a source is made of: code, comments and
/// strings. A phrase in a comment is not shown to anybody, and a phrase in a
/// string is. The phrase is given as the text means it: the escapes read,
/// the line continuations gone.
fn literals(text: &str) -> Vec<(usize, String)> {
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
            // A character literal: a quote, a character, a quote.
            b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
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
                out.push((start, unescape(&phrase)));
                i = j;
            }
            _ => i += 1,
        }
    }
    out
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
fn looks_like_text(phrase: &str) -> bool {
    let plain = strip_codes(phrase);
    if plain.split_whitespace().count() < 2 {
        return false;
    }
    if !plain.is_ascii() {
        return false;
    }
    if plain.contains('{') || plain.contains('}') {
        return false;
    }
    // The first letter, not the first character: a phrase that starts with
    // a colour code starts with a backslash.
    let first = plain.chars().find(|c| c.is_alphabetic()).unwrap_or(' ');
    first.is_ascii_uppercase()
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
        let body = match text.find("#[cfg(test)]") {
            Some(at) => &text[..at],
            None => &text[..],
        };
        for (_, phrase) in literals(body) {
            if !looks_like_text(&phrase) {
                continue;
            }
            if table.contains(&phrase) {
                continue;
            }
            if ENGLISH_ON_PURPOSE.iter().any(|(p, _)| *p == phrase) {
                continue;
            }
            untranslated.push(format!("{name}: {phrase:?}"));
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
        for (_, phrase) in literals(&text) {
            in_sources.insert(phrase);
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
