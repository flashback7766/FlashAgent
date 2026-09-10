//! JSON repair for model-emitted tool arguments.
//!
//! Local models routinely produce: single-quoted Python dictionaries,
//! Python booleans/None (`True`, `False`, `None`), markdown codeblock fences,
//! unescaped newlines/tabs inside strings, truncated output, and trailing commas.
//! Repair is a best-effort normalization pipeline — if it cannot be fixed,
//! the caller sees `None` and the call is reported as a parse error, never silently dropped.

/// Attempt to repair a JSON fragment into parseable JSON.
pub fn repair_json(input: &str) -> Option<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return None;
    }

    // 0. Fast-path: already valid JSON
    if serde_json::from_str::<serde_json::Value>(raw).is_ok() {
        return Some(raw.to_string());
    }

    // 1. Strip markdown fences or tool tags
    let unfenced = strip_fences_and_tags(raw);
    if serde_json::from_str::<serde_json::Value>(unfenced).is_ok() {
        return Some(unfenced.to_string());
    }

    // 2. Normalization pipeline: quotes -> Python dict/literals -> trailing commas -> closing structures
    let mut s = normalize_smart_quotes(unfenced);
    s = normalize_python_dict_and_literals(&s);
    s = strip_trailing_commas(&s);
    if let Some(closed) = close_structures(&s) {
        if serde_json::from_str::<serde_json::Value>(&closed).is_ok() {
            return Some(closed);
        }
    }

    // 3. Fallback: extract the outermost JSON object or array if embedded in text
    if let Some(extracted) = extract_embedded_json(&s) {
        let mut sub = normalize_smart_quotes(&extracted);
        sub = normalize_python_dict_and_literals(&sub);
        sub = strip_trailing_commas(&sub);
        if let Some(closed) = close_structures(&sub) {
            if serde_json::from_str::<serde_json::Value>(&closed).is_ok() {
                return Some(closed);
            }
        }
    }

    None
}

/// Strip ``` fences (like ```json or ```tool_calls) and <tool_call> tags.
fn strip_fences_and_tags(s: &str) -> &str {
    let mut trimmed = s.trim();

    // Strip XML-style <tool_call> ... </tool_call>
    if let (Some(start), Some(end)) = (trimmed.find("<tool_call>"), trimmed.rfind("</tool_call>")) {
        if start < end {
            trimmed = trimmed[start + "<tool_call>".len()..end].trim();
        }
    }

    // Strip ``` fences
    if trimmed.starts_with("```") {
        if let Some(first_nl) = trimmed.find('\n') {
            trimmed = &trimmed[first_nl + 1..];
        }
        if let Some(last_fence) = trimmed.rfind("```") {
            trimmed = &trimmed[..last_fence];
        }
    }

    trimmed.trim()
}

/// Convert typographical smart quotes to standard ASCII quotes.
fn normalize_smart_quotes(s: &str) -> String {
    s.replace(['\u{201C}', '\u{201D}'], "\"")
        .replace(['\u{2018}', '\u{2019}'], "'")
}

/// Normalize Python-style dicts (`'key': 'val'`), Python literals (`True`, `False`, `None`),
/// and escape unescaped control characters within strings.
pub fn normalize_python_dict_and_literals(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    #[derive(Copy, Clone, PartialEq, Eq)]
    enum QuoteStyle {
        None,
        Single,
        Double,
    }

    let mut in_quote = QuoteStyle::None;
    let mut escape = false;

    while i < len {
        let c = chars[i];

        if escape {
            escape = false;
            if in_quote == QuoteStyle::Single {
                // Inside single-quoted string: \' was an escaped single quote in Python
                if c == '\'' {
                    out.push('\'');
                } else if c == '"' {
                    out.push_str("\\\"");
                } else {
                    out.push('\\');
                    out.push(c);
                }
            } else {
                out.push('\\');
                out.push(c);
            }
            i += 1;
            continue;
        }

        if c == '\\' {
            escape = true;
            i += 1;
            continue;
        }

        match in_quote {
            QuoteStyle::None => {
                match c {
                    '\'' => {
                        // Begin single-quoted string converted to double-quoted JSON string
                        in_quote = QuoteStyle::Single;
                        out.push('"');
                        i += 1;
                        continue;
                    }
                    '"' => {
                        in_quote = QuoteStyle::Double;
                        out.push('"');
                        i += 1;
                        continue;
                    }
                    'T' if matches_word(&chars, i, "True") => {
                        out.push_str("true");
                        i += 4;
                        continue;
                    }
                    'F' if matches_word(&chars, i, "False") => {
                        out.push_str("false");
                        i += 5;
                        continue;
                    }
                    'N' if matches_word(&chars, i, "None") => {
                        out.push_str("null");
                        i += 4;
                        continue;
                    }
                    _ => {
                        out.push(c);
                        i += 1;
                        continue;
                    }
                }
            }
            QuoteStyle::Single => {
                match c {
                    '\'' => {
                        // End single-quoted string
                        in_quote = QuoteStyle::None;
                        out.push('"');
                        i += 1;
                        continue;
                    }
                    '"' => {
                        // Literal double quote inside single-quoted string -> escape for JSON
                        out.push_str("\\\"");
                        i += 1;
                        continue;
                    }
                    '\n' => {
                        out.push_str("\\n");
                        i += 1;
                        continue;
                    }
                    '\r' => {
                        out.push_str("\\r");
                        i += 1;
                        continue;
                    }
                    '\t' => {
                        out.push_str("\\t");
                        i += 1;
                        continue;
                    }
                    _ => {
                        out.push(c);
                        i += 1;
                        continue;
                    }
                }
            }
            QuoteStyle::Double => {
                match c {
                    '"' => {
                        in_quote = QuoteStyle::None;
                        out.push('"');
                        i += 1;
                        continue;
                    }
                    '\n' => {
                        out.push_str("\\n");
                        i += 1;
                        continue;
                    }
                    '\r' => {
                        out.push_str("\\r");
                        i += 1;
                        continue;
                    }
                    '\t' => {
                        out.push_str("\\t");
                        i += 1;
                        continue;
                    }
                    _ => {
                        out.push(c);
                        i += 1;
                        continue;
                    }
                }
            }
        }
    }

    out
}

fn matches_word(chars: &[char], start: usize, word: &str) -> bool {
    let word_chars: Vec<char> = word.chars().collect();
    if start + word_chars.len() > chars.len() {
        return false;
    }
    for (offset, &wc) in word_chars.iter().enumerate() {
        if chars[start + offset] != wc {
            return false;
        }
    }
    // Check boundary before
    if start > 0 {
        let prev = chars[start - 1];
        if prev.is_alphanumeric() || prev == '_' {
            return false;
        }
    }
    // Check boundary after
    let after_idx = start + word_chars.len();
    if after_idx < chars.len() {
        let next = chars[after_idx];
        if next.is_alphanumeric() || next == '_' {
            return false;
        }
    }
    true
}

fn strip_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escape = false;
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '"' if !escape => in_string = !in_string,
            '\\' if in_string => escape = !escape,
            _ => escape = false,
        }
        if c == ',' && !in_string {
            let next = chars[i + 1..].iter().find(|&&n| !n.is_whitespace());
            if next == Some(&'}') || next == Some(&']') {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Close open strings, objects and arrays left from truncation.
fn close_structures(s: &str) -> Option<String> {
    let mut in_string = false;
    let mut escape = false;
    let mut stack: Vec<char> = Vec::new();
    for c in s.chars() {
        match c {
            '"' if !escape => in_string = !in_string,
            '\\' if in_string => escape = !escape,
            _ => escape = false,
        }
        if !in_string {
            match c {
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }
    let mut out = String::from(s);
    if in_string {
        // Unterminated string: close it
        if escape {
            while out.ends_with('\\') {
                out.pop();
            }
        }
        out.push('"');
    }
    // Complete truncated literals (tru, fals, nul) left by an interrupted stream
    for (frag, full) in [("tru", "true"), ("fals", "false"), ("nul", "null")] {
        if out.ends_with(frag) {
            out.push_str(&full[frag.len()..]);
            break;
        }
    }
    for closer in stack.into_iter().rev() {
        out.push(closer);
    }
    Some(out)
}

/// Attempts to extract the outermost balanced `{ ... }` or `[ ... ]` substring.
fn extract_embedded_json(s: &str) -> Option<String> {
    let first_obj = s.find('{');
    let first_arr = s.find('[');

    let (start_idx, open_char, close_char) = match (first_obj, first_arr) {
        (Some(o), Some(a)) => {
            if o < a {
                (o, '{', '}')
            } else {
                (a, '[', ']')
            }
        }
        (Some(o), None) => (o, '{', '}'),
        (None, Some(a)) => (a, '[', ']'),
        (None, None) => return None,
    };

    let slice = &s[start_idx..];
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;

    for (i, c) in slice.char_indices() {
        match c {
            '"' if !escape => in_str = !in_str,
            '\\' if in_str => escape = !escape,
            _ => escape = false,
        }
        if in_str {
            continue;
        }
        if c == open_char {
            depth += 1;
        } else if c == close_char {
            depth -= 1;
            if depth == 0 {
                return Some(slice[..=i].to_string());
            }
        }
    }

    // If unclosed, return from start_idx onwards
    Some(slice.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(s: &str) -> serde_json::Value {
        serde_json::from_str(&repair_json(s).unwrap()).unwrap()
    }

    #[test]
    fn valid_json_passes_through() {
        assert_eq!(repair_json(r#"{"a":1}"#).unwrap(), r#"{"a":1}"#);
    }

    #[test]
    fn trailing_commas_removed() {
        let v = parsed(r#"{"a": 1, "b": [1, 2,],}"#);
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn truncated_object_closed() {
        let v = parsed(r#"{"cmd": "cargo test", "flags": {"fast": tru"#);
        assert_eq!(v["cmd"], "cargo test");
        assert!(v["flags"]["fast"].is_boolean() || v["flags"]["fast"].is_null() || v["flags"]["fast"] == "");
    }

    #[test]
    fn unterminated_string_closed() {
        let v = parsed(r#"{"path": "/home/user/proj"#);
        assert_eq!(v["path"], "/home/user/proj");
    }

    #[test]
    fn smart_quotes_normalized() {
        let v = parsed("{\"cmd\": \u{201C}ls -la\u{201D}}");
        assert_eq!(v["cmd"], "ls -la");
    }

    #[test]
    fn unicode_values_survive() {
        let v = parsed(r#"{"task": "build project", "note": "unicode check: ✨ / café"}"#);
        assert_eq!(v["task"], "build project");
        assert_eq!(v["note"], "unicode check: ✨ / café");
    }

    #[test]
    fn python_dict_and_literals_repaired() {
        let raw = "{'question': 'What language?', 'options': ['Python', 'Rust', 'Go'], 'multi_select': False, 'empty': None, 'active': True}";
        let v = parsed(raw);
        assert_eq!(v["question"], "What language?");
        assert_eq!(v["options"][0], "Python");
        assert_eq!(v["options"][1], "Rust");
        assert_eq!(v["options"][2], "Go");
        assert_eq!(v["multi_select"], false);
        assert_eq!(v["empty"], serde_json::Value::Null);
        assert_eq!(v["active"], true);
    }

    #[test]
    fn markdown_fences_stripped() {
        let fenced = "```json\n{\"name\": \"ask_user\", \"arguments\": {\"question\": \"Ready?\"}}\n```";
        let v = parsed(fenced);
        assert_eq!(v["name"], "ask_user");
        assert_eq!(v["arguments"]["question"], "Ready?");
    }

    #[test]
    fn embedded_json_in_text_extracted() {
        let text = "Sure! Here is the tool call: {\"name\": \"ask_user\", \"arguments\": {\"question\": \"Topic?\"}} Let me know!";
        let v = parsed(text);
        assert_eq!(v["name"], "ask_user");
        assert_eq!(v["arguments"]["question"], "Topic?");
    }

    #[test]
    fn garbage_returns_none() {
        assert!(repair_json("not json at all without any structures").is_none());
    }
}
