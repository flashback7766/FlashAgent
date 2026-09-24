//! Syntax colour for code blocks in answers. A tokenizer, not a parser: it
//! knows comments, strings, numbers, keywords, types, calls and macros, which
//! is what makes code readable at a glance. An unknown language gets the same
//! treatment minus the keywords.

const TEXT: &str = "\x1b[38;2;220;215;205m";
const KEYWORD: &str = "\x1b[38;2;198;160;246m";
const STRING: &str = "\x1b[38;2;166;218;149m";
const COMMENT: &str = "\x1b[38;2;125;130;140m";
const NUMBER: &str = "\x1b[38;2;245;169;127m";
const TYPE: &str = "\x1b[38;2;238;212;159m";
const CALL: &str = "\x1b[38;2;138;180;248m";
const MACRO: &str = "\x1b[38;2;125;196;228m";
const ADDED: &str = "\x1b[38;2;145;205;140m";
const REMOVED: &str = "\x1b[38;2;225;115;105m";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    Js,
    Go,
    CLike,
    Shell,
    PowerShell,
    Data,
    Sql,
    Lua,
    Ruby,
    Diff,
    Other,
}

fn lang_of(name: &str) -> Lang {
    match name.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => Lang::Rust,
        "python" | "py" | "python3" => Lang::Python,
        "javascript" | "js" | "jsx" | "typescript" | "ts" | "tsx" | "mjs" | "cjs" => Lang::Js,
        "go" | "golang" => Lang::Go,
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "cxx" | "java" | "kotlin" | "kt" | "cs" | "csharp" | "c#"
        | "swift" | "scala" | "dart" | "zig" => Lang::CLike,
        "bash" | "sh" | "shell" | "zsh" | "console" | "shellsession" | "fish" => Lang::Shell,
        "powershell" | "ps1" | "pwsh" | "ps" => Lang::PowerShell,
        "json" | "jsonc" | "toml" | "yaml" | "yml" | "ini" => Lang::Data,
        "sql" | "postgres" | "postgresql" | "mysql" | "sqlite" => Lang::Sql,
        "lua" => Lang::Lua,
        "ruby" | "rb" => Lang::Ruby,
        "diff" | "patch" => Lang::Diff,
        _ => Lang::Other,
    }
}

fn keywords(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false",
            "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
            "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
        ],
        Lang::Python => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else",
            "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None",
            "nonlocal", "not", "or", "pass", "raise", "return", "self", "True", "try", "while", "with", "yield",
        ],
        Lang::Js => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "do",
            "else", "export", "extends", "false", "finally", "for", "from", "function", "if", "import", "in",
            "instanceof", "interface", "let", "new", "null", "of", "return", "static", "super", "switch", "this",
            "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield",
        ],
        Lang::Go => &[
            "break", "case", "chan", "const", "continue", "default", "defer", "else", "false", "for", "func", "go",
            "if", "import", "interface", "map", "nil", "package", "range", "return", "select", "struct", "switch",
            "true", "type", "var",
        ],
        Lang::CLike => &[
            "auto", "bool", "break", "case", "catch", "char", "class", "const", "continue", "default", "delete",
            "do", "double", "else", "enum", "extends", "false", "final", "float", "for", "fun", "if", "implements",
            "import", "include", "int", "interface", "long", "namespace", "new", "null", "nullptr", "override",
            "package", "private", "protected", "public", "return", "short", "sizeof", "static", "struct", "switch",
            "template", "this", "throw", "true", "try", "typedef", "typename", "using", "val", "var", "virtual",
            "void", "volatile", "while",
        ],
        Lang::Shell => &[
            "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local",
            "return", "then", "until", "while",
        ],
        Lang::PowerShell => &[
            "begin", "break", "catch", "continue", "else", "elseif", "end", "finally", "for", "foreach", "function",
            "if", "in", "param", "process", "return", "switch", "throw", "try", "while",
        ],
        Lang::Data => &["true", "false", "null"],
        Lang::Sql => &[
            "and", "as", "by", "create", "delete", "desc", "distinct", "drop", "from", "group", "having", "in",
            "insert", "into", "is", "join", "key", "left", "limit", "not", "null", "on", "or", "order", "primary",
            "select", "set", "table", "update", "values", "where",
        ],
        Lang::Lua => &[
            "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local", "nil",
            "not", "or", "repeat", "return", "then", "true", "until", "while",
        ],
        Lang::Ruby => &[
            "begin", "class", "def", "do", "else", "elsif", "end", "ensure", "false", "if", "module", "nil",
            "raise", "require", "rescue", "return", "self", "then", "true", "unless", "until", "when", "while",
            "yield",
        ],
        Lang::Diff | Lang::Other => &[],
    }
}

fn line_comment(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust | Lang::Js | Lang::Go | Lang::CLike => &["//"],
        Lang::Python | Lang::Shell | Lang::PowerShell | Lang::Ruby => &["#"],
        Lang::Data => &["#", "//"],
        Lang::Sql | Lang::Lua => &["--"],
        Lang::Diff | Lang::Other => &["//", "#"],
    }
}

fn has_block_comments(lang: Lang) -> bool {
    matches!(lang, Lang::Rust | Lang::Js | Lang::Go | Lang::CLike | Lang::Other)
}

/// State that carries from one line of a block to the next.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Carry {
    in_block_comment: bool,
}

/// One line of a code block in `lang` (the fence's info string), coloured.
/// Every piece carries its own colour, so the line needs no outer one.
pub(crate) fn highlight_line(lang_name: &str, line: &str, carry: &mut Carry) -> String {
    let lang = lang_of(lang_name);
    if lang == Lang::Diff {
        let colour = match line.as_bytes().first() {
            Some(b'+') if !line.starts_with("+++") => ADDED,
            Some(b'-') if !line.starts_with("---") => REMOVED,
            Some(b'@') => CALL,
            _ => TEXT,
        };
        return format!("{colour}{line}\x1b[0m");
    }

    let chars: Vec<char> = line.chars().collect();
    let keywords = keywords(lang);
    let comments = line_comment(lang);
    let mut out = String::with_capacity(line.len() * 2);
    let mut push = |colour: &str, text: &str| {
        out.push_str(colour);
        out.push_str(text);
    };
    let starts_with = |at: usize, s: &str| s.chars().enumerate().all(|(k, c)| chars.get(at + k) == Some(&c));
    let mut i = 0;
    while i < chars.len() {
        if carry.in_block_comment {
            let start = i;
            while i < chars.len() && !starts_with(i, "*/") {
                i += 1;
            }
            if i < chars.len() {
                i += 2;
                carry.in_block_comment = false;
            }
            push(COMMENT, &chars[start..i].iter().collect::<String>());
            continue;
        }
        let c = chars[i];
        if has_block_comments(lang) && starts_with(i, "/*") {
            carry.in_block_comment = true;
            continue;
        }
        // A `#` inside a word (`C#`, `a#b`) or after `$` in a shell is not a comment.
        if let Some(marker) = comments.iter().find(|m| starts_with(i, m)) {
            let word_before = i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '$');
            if !(marker.starts_with('#') && word_before) {
                push(COMMENT, &chars[i..].iter().collect::<String>());
                break;
            }
        }
        let is_lifetime = lang == Lang::Rust
            && c == '\''
            && chars.get(i + 1).is_some_and(|n| n.is_alphabetic() || *n == '_')
            && chars.get(i + 2) != Some(&'\'')
            && chars.get(i + 1) != Some(&'\\');
        if (c == '"' || c == '\'' || c == '`') && !is_lifetime {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(chars.len());
            push(STRING, &chars[start..i].iter().collect::<String>());
            continue;
        }
        if c.is_ascii_digit() && !(i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            push(NUMBER, &chars[start..i].iter().collect::<String>());
            continue;
        }
        if c.is_alphabetic() || c == '_' || (c == '$' && matches!(lang, Lang::Shell | Lang::PowerShell)) {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let next = chars[i..].iter().find(|c| !c.is_whitespace()).copied();
            let colour = if word.starts_with('$') {
                MACRO
            } else if keywords.iter().any(|k| {
                if lang == Lang::Sql || lang == Lang::PowerShell { k.eq_ignore_ascii_case(&word) } else { *k == word }
            }) {
                KEYWORD
            } else if lang == Lang::Rust && chars.get(i) == Some(&'!') {
                MACRO
            } else if next == Some('(') {
                CALL
            } else if word.chars().next().is_some_and(char::is_uppercase)
                && word.chars().any(char::is_lowercase)
                && !matches!(lang, Lang::Shell | Lang::Data | Lang::Sql)
            {
                TYPE
            } else {
                TEXT
            };
            push(colour, &word);
            continue;
        }
        if lang == Lang::Rust && c == '#' && chars.get(i + 1).is_some_and(|n| *n == '[' || *n == '!') {
            let start = i;
            while i < chars.len() && chars[i] != ']' {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            push(MACRO, &chars[start..i].iter().collect::<String>());
            continue;
        }
        push(TEXT, &c.to_string());
        i += 1;
    }
    out.push_str("\x1b[0m");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        crate::strip_ansi(s)
    }

    #[test]
    fn colour_never_changes_the_text() {
        let mut carry = Carry::default();
        for (lang, line) in [
            ("rust", r#"let s: &'static str = "a \"b\""; // done"#),
            ("python", "def f(x=1.5):  # comment"),
            ("bash", r#"echo "$HOME" | grep -c x # count"#),
            ("", "anything goes 42"),
            ("diff", "+added line"),
        ] {
            assert_eq!(plain(&highlight_line(lang, line, &mut carry)), line);
        }
    }

    #[test]
    fn the_parts_of_a_line_get_their_colours() {
        let mut carry = Carry::default();
        let line = highlight_line("rust", r#"fn main() { println!("hi {}", 42); } // end"#, &mut carry);
        assert!(line.contains(&format!("{KEYWORD}fn")), "{line:?}");
        assert!(line.contains(&format!("{CALL}main")), "{line:?}");
        assert!(line.contains(&format!("{MACRO}println")), "{line:?}");
        assert!(line.contains(&format!("{STRING}\"hi {{}}\"")), "{line:?}");
        assert!(line.contains(&format!("{NUMBER}42")), "{line:?}");
        assert!(line.contains(&format!("{COMMENT}// end")), "{line:?}");
    }

    #[test]
    fn a_block_comment_carries_to_the_next_line() {
        let mut carry = Carry::default();
        let first = highlight_line("c", "int x; /* starts", &mut carry);
        assert!(first.contains(&format!("{COMMENT}/* starts")));
        let second = highlight_line("c", "still */ int y;", &mut carry);
        assert!(second.starts_with(&format!("{COMMENT}still */")), "{second:?}");
        assert!(second.contains(&format!("{KEYWORD}int")));
    }

    #[test]
    fn a_rust_lifetime_is_not_a_string() {
        let mut carry = Carry::default();
        let line = highlight_line("rust", "fn f<'a>(x: &'a str) -> char { 'z' }", &mut carry);
        assert!(!line.contains(&format!("{STRING}'a")), "{line:?}");
        assert!(line.contains(&format!("{STRING}'z'")), "{line:?}");
    }
}
