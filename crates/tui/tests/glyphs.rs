//! Windows consoles draw with Consolas or Cascadia Mono and, outside Windows
//! Terminal, fall back to no other font: a symbol missing from them shows as a
//! box. Every symbol the UI draws must be in both.

use std::path::Path;

/// Checked against the character maps of consola.ttf and CascadiaMono.ttf.
const IN_BOTH_FONTS: &str = "«»¶·×—•…›←↑→↓∙√─│┌┐└┘├┤┬┴┼╭╮╯╰▀▄█▌■□▪▲▸►▼▾◊○●◦♦";

/// `\u{2423}` in a string is drawn as the character it names.
fn unescape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(escaped) = after.strip_prefix('\\') {
            // `\\u{…}` is a backslash followed by text.
            out.push_str("\\\\");
            rest = escaped;
            continue;
        }
        let decoded = after
            .strip_prefix("u{")
            .and_then(|hex| hex.split_once('}'))
            .and_then(|(hex, tail)| Some((char::from_u32(u32::from_str_radix(hex, 16).ok()?)?, tail)));
        match decoded {
            Some((c, tail)) => {
                out.push(c);
                rest = tail;
            }
            None => {
                out.push('\\');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn drawn_symbols(path: &Path, found: &mut Vec<(char, String)>) {
    let text = std::fs::read_to_string(path).unwrap();
    // Tests may use anything; they are not drawn.
    let code = text.split("#[cfg(test)]").next().unwrap_or_default();
    for (i, line) in code.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let line = unescape(line.split(" // ").next().unwrap_or_default());
        for c in line.chars().filter(|c| !c.is_ascii() && !c.is_alphabetic()) {
            if !IN_BOTH_FONTS.contains(c) {
                found.push((c, format!("{}:{}", path.display(), i + 1)));
            }
        }
    }
}

#[test]
fn escapes_are_read_as_the_characters_they_name() {
    assert_eq!(unescape(r#"format!(" \u{2423}x{n} ")"#), r#"format!(" ␣x{n} ")"#);
    assert_eq!(unescape(r#"'\u{fffd}' \n \x1b"#), r#"'�' \n \x1b"#);
    assert_eq!(unescape(r#""\\u{2423}""#), r#""\\u{2423}""#);
}

#[test]
fn every_symbol_the_ui_draws_is_in_the_windows_console_fonts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root.join("src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            drawn_symbols(&path, &mut found);
        }
    }
    drawn_symbols(&root.join("../core/src/context_usage.rs"), &mut found);
    assert!(found.is_empty(), "shown as a box in Consolas or Cascadia Mono: {found:?}");
}
