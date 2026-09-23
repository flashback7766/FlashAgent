//! Windows consoles draw with Consolas or Cascadia Mono and, outside Windows
//! Terminal, fall back to no other font: a symbol missing from them shows as a
//! box. Every symbol the UI draws must be in both.

use std::path::Path;

/// Checked against the character maps of consola.ttf and CascadiaMono.ttf.
const IN_BOTH_FONTS: &str = "«»¶·×—•…›←↑→↓∙√─│┌┐└┘├┤┬┴┼╭╮╯╰▀▄█▌■□▪▲▸►▼▾◊○●◦♦";

fn drawn_symbols(path: &Path, found: &mut Vec<(char, String)>) {
    let text = std::fs::read_to_string(path).unwrap();
    // Tests may use anything; they are not drawn.
    let code = text.split("#[cfg(test)]").next().unwrap_or_default();
    for (i, line) in code.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let line = line.split(" // ").next().unwrap_or_default();
        for c in line.chars().filter(|c| !c.is_ascii() && !c.is_alphabetic()) {
            if !IN_BOTH_FONTS.contains(c) {
                found.push((c, format!("{}:{}", path.display(), i + 1)));
            }
        }
    }
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
