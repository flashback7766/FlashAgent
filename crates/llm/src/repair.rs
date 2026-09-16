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

/// The argument object a tool call will actually run with.
///
/// Every consumer — the permission check, the approval card, the diff preview
/// and the tool itself — must read arguments through this one function, so
/// the command that is judged and shown is exactly the command that runs.
/// Rules, in order: parse (repairing if needed); unwrap `{"<tool_name>": {..}}`;
/// unwrap `{"arguments"|"parameters"|"args": ..}` only when the object holds
/// nothing but such wrapper/metadata keys; otherwise the object itself. Last,
/// argument names other agents use are renamed to this tool's own (see
/// [`canonical_names`]) — here, so the renamed call is what gets judged too.
pub fn effective_args(args_json: &str, tool_name: &str) -> Option<serde_json::Value> {
    unwrapped_args(args_json, tool_name).map(|v| canonical_names(tool_name, v))
}

/// Other names for a built-in tool's arguments, as other agents taught models
/// to write them: Claude Code's `file_path`, OpenCode's `filePath` and
/// `oldString`, Gemini's `TargetFile` and `ReplacementContent`, `cmd` for
/// `command`. A model trained on one of those should not fail a call over a
/// name.
const PATH_ALIASES: &[&str] =
    &["file_path", "filePath", "filepath", "file", "filename", "TargetFile", "target_file", "AbsolutePath", "absolute_path"];
const DIR_ALIASES: &[&str] = &["directory", "dir", "dirPath", "dir_path", "DirectoryPath", "directory_path", "file_path", "filePath"];

fn aliases_for(tool: &str) -> &'static [(&'static str, &'static [&'static str])] {
    match tool {
        "read_file" | "outline_file" | "view_image" | "patch_file" | "edit_file" => &[("path", PATH_ALIASES)],
        "write_file" => &[
            ("path", PATH_ALIASES),
            ("content", &["contents", "text", "file_text", "fileContent", "file_content", "CodeContent", "code", "data"]),
        ],
        "list_dir" | "git_status" | "git_diff" => &[("path", DIR_ALIASES)],
        "run_shell" => &[("command", &["cmd", "CommandLine", "command_line", "shell_command", "script"])],
        "grep" => &[("pattern", &["query", "regex", "search", "SearchPattern"]), ("glob", &["include", "file_pattern", "Includes"])],
        "glob" => &[("pattern", &["glob", "file_pattern", "Pattern"])],
        "web_fetch" => &[("url", &["uri", "link", "href", "Url"])],
        "web_search" => &[("query", &["q", "search", "search_query", "Query"])],
        _ => &[],
    }
}

const OLD_ALIASES: &[&str] = &["oldString", "old_str", "old_text", "oldText", "TargetContent", "search", "find", "old"];
const NEW_ALIASES: &[&str] = &["newString", "new_str", "new_text", "newText", "ReplacementContent", "replace", "replacement", "new"];
const ALL_ALIASES: &[&str] = &["replaceAll", "AllowMultiple", "allow_multiple", "all"];

/// Rename `aliases` to `canonical` in `obj`, unless `canonical` is already
/// there: a call that names both is taken at its own word, never merged.
fn rename_first(obj: &mut serde_json::Map<String, serde_json::Value>, canonical: &str, aliases: &[&str]) {
    if obj.contains_key(canonical) {
        return;
    }
    if let Some(alias) = aliases.iter().find(|a| obj.contains_key(**a)) {
        if let Some(value) = obj.remove(*alias) {
            obj.insert(canonical.to_string(), value);
        }
    }
}

/// A built-in tool's arguments with other agents' names for them renamed to
/// its own. External tools keep theirs: their names are the server's.
fn canonical_names(tool: &str, mut value: serde_json::Value) -> serde_json::Value {
    let Some(obj) = value.as_object_mut() else {
        return value;
    };
    for (canonical, aliases) in aliases_for(tool) {
        rename_first(obj, canonical, aliases);
    }
    if tool == "edit_file" {
        normalize_edit_target(obj);
        if let Some(files) = obj.get_mut("files").and_then(|f| f.as_array_mut()) {
            for item in files.iter_mut() {
                if let Some(path) = item.as_str().map(str::to_string) {
                    *item = serde_json::json!({ "path": path });
                } else if let Some(entry) = item.as_object_mut() {
                    rename_first(entry, "path", PATH_ALIASES);
                    normalize_edit_target(entry);
                }
            }
        }
        if let Some(files) = obj.get("files").and_then(|f| f.as_array()).cloned() {
            if files.len() == 1 {
                let first = &files[0];
                let file_path = first.get("path").and_then(|p| p.as_str()).map(str::to_string);
                let file_edits = first.get("edits").cloned();
                if let Some(p) = file_path {
                    if !obj.contains_key("path") {
                        obj.insert("path".to_string(), serde_json::Value::String(p));
                    }
                }
                if let Some(e) = file_edits {
                    if !obj.contains_key("edits") {
                        obj.insert("edits".to_string(), e);
                    }
                }
                obj.remove("files");
            } else if !files.is_empty() && obj.contains_key("edits") {
                let top_edits = obj.remove("edits").unwrap();
                if let Some(files_mut) = obj.get_mut("files").and_then(|f| f.as_array_mut()) {
                    for f in files_mut.iter_mut().filter_map(|f| f.as_object_mut()) {
                        if !f.contains_key("edits") {
                            f.insert("edits".to_string(), top_edits.clone());
                        }
                    }
                }
            }
        }
    }
    if tool == "read_file" {
        // A list of paths is several files, each read whole.
        rename_first(obj, "files", &["paths", "file_paths", "filePaths"]);
        if let Some(files) = obj.get_mut("files").and_then(|f| f.as_array_mut()) {
            for item in files.iter_mut() {
                if let Some(path) = item.as_str().map(str::to_string) {
                    *item = serde_json::json!({ "path": path });
                } else if let Some(entry) = item.as_object_mut() {
                    rename_first(entry, "path", PATH_ALIASES);
                }
            }
        }
    }
    value
}

/// One file's edits in the tool's own words: a single edit given flat becomes
/// an edit list, and other agents' names inside the list are renamed.
fn normalize_edit_target(obj: &mut serde_json::Map<String, serde_json::Value>) {
    if !obj.contains_key("edits") {
        let mut single = serde_json::Map::new();
        for (canonical, aliases) in [("old_string", OLD_ALIASES), ("new_string", NEW_ALIASES), ("replace_all", ALL_ALIASES)] {
            rename_first(obj, canonical, aliases);
            if let Some(v) = obj.remove(canonical) {
                single.insert(canonical.to_string(), v);
            }
        }
        if single.contains_key("old_string") || single.contains_key("new_string") {
            obj.insert("edits".to_string(), serde_json::Value::Array(vec![serde_json::Value::Object(single)]));
        } else {
            // Nothing that looks like an edit: put back what was taken.
            obj.extend(single);
        }
    }
    if let Some(edits) = obj.get_mut("edits").and_then(|e| e.as_array_mut()) {
        for edit in edits.iter_mut().filter_map(|e| e.as_object_mut()) {
            rename_first(edit, "old_string", OLD_ALIASES);
            rename_first(edit, "new_string", NEW_ALIASES);
            rename_first(edit, "replace_all", ALL_ALIASES);
        }
    }
}

fn unwrapped_args(args_json: &str, tool_name: &str) -> Option<serde_json::Value> {
    let parse = |text: &str| -> Option<serde_json::Value> {
        let trimmed = text.trim();
        let candidate = if trimmed.is_empty() { "{}" } else { trimmed };
        serde_json::from_str(candidate)
            .ok()
            .or_else(|| serde_json::from_str(&repair_json(candidate)?).ok())
    };
    let value = parse(args_json)?;
    let obj = value.as_object()?;

    let unwrap_inner = |inner: &serde_json::Value| match inner {
        serde_json::Value::String(s) => parse(s).filter(|v| v.is_object()),
        v if v.is_object() => Some(v.clone()),
        _ => None,
    };
    if obj.len() == 1 {
        if let Some(inner) = obj.get(tool_name) {
            return unwrap_inner(inner);
        }
    }
    const WRAPPERS: [&str; 3] = ["arguments", "parameters", "args"];
    const METADATA: [&str; 5] = ["name", "tool", "id", "type", "function"];
    let only_wrapper_keys = obj.keys().all(|k| WRAPPERS.contains(&k.as_str()) || METADATA.contains(&k.as_str()));
    if only_wrapper_keys {
        if let Some(inner) = WRAPPERS.iter().find_map(|w| obj.get(*w)) {
            return unwrap_inner(inner);
        }
    }
    Some(value)
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

    #[test]
    fn effective_args_never_diverges_between_readers() {
        // Top-level fields win whenever the object has any: a stray wrapper
        // next to them is data, not the "real" arguments.
        let smuggled = r#"{"command":"echo SAFE","timeout_ms":"soon","arguments":{"command":"echo PWNED"}}"#;
        assert_eq!(effective_args(smuggled, "run_shell").unwrap()["command"], "echo SAFE");
        // Pure wrappers unwrap, including stringified inner JSON.
        assert_eq!(effective_args(r#"{"arguments":{"command":"ls"}}"#, "run_shell").unwrap()["command"], "ls");
        assert_eq!(effective_args(r#"{"name":"run_shell","arguments":"{\"command\":\"ls\"}"}"#, "run_shell").unwrap()["command"], "ls");
        assert_eq!(effective_args(r#"{"run_shell":{"command":"ls"}}"#, "run_shell").unwrap()["command"], "ls");
        // A single object-valued field of a different name is a real argument.
        assert!(effective_args(r#"{"filter":{"x":1}}"#, "mcp__db__query").unwrap().get("filter").is_some());
        assert_eq!(effective_args("", "list_dir").unwrap(), serde_json::json!({}));
        assert!(effective_args("[1,2]", "x").is_none());
    }

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

    fn args(tool: &str, json: &str) -> serde_json::Value {
        effective_args(json, tool).expect("arguments parsed")
    }

    #[test]
    fn other_agents_names_for_arguments_become_the_tools_own() {
        assert_eq!(args("read_file", r#"{"filePath":"src/a.rs"}"#)["path"], "src/a.rs");
        assert_eq!(
            args("write_file", r#"{"file_path":"a.txt","contents":"x"}"#),
            serde_json::json!({ "path": "a.txt", "content": "x" })
        );
        assert_eq!(args("run_shell", r#"{"cmd":"cargo test"}"#)["command"], "cargo test");
        assert_eq!(
            args("grep", r#"{"query":"TODO","include":"*.rs"}"#),
            serde_json::json!({ "pattern": "TODO", "glob": "*.rs" })
        );
        assert_eq!(args("list_dir", r#"{"directory":"src"}"#)["path"], "src");
        assert_eq!(args("web_fetch", r#"{"uri":"https://example.com"}"#)["url"], "https://example.com");
    }

    #[test]
    fn an_edit_in_other_agents_words_becomes_an_edit_list() {
        // Gemini's single-replace shape.
        assert_eq!(
            args("edit_file", r#"{"TargetFile":"a.rs","TargetContent":"old","ReplacementContent":"new"}"#),
            serde_json::json!({ "path": "a.rs", "edits": [ { "old_string": "old", "new_string": "new" } ] })
        );
        // OpenCode's names inside a list.
        assert_eq!(
            args("edit_file", r#"{"filePath":"a.rs","edits":[{"oldString":"a","newString":"b","replaceAll":true}]}"#),
            serde_json::json!({ "path": "a.rs", "edits": [ { "old_string": "a", "new_string": "b", "replace_all": true } ] })
        );
        // The tool's own names, flat.
        assert_eq!(
            args("edit_file", r#"{"path":"a.rs","old_string":"a","new_string":"b"}"#),
            serde_json::json!({ "path": "a.rs", "edits": [ { "old_string": "a", "new_string": "b" } ] })
        );
    }

    #[test]
    fn a_name_the_call_already_uses_correctly_is_never_overwritten() {
        // Which command runs must never depend on which of two names wins.
        let both = args("run_shell", r#"{"command":"ls","cmd":"rm -rf /"}"#);
        assert_eq!(both["command"], "ls");
        assert_eq!(both["cmd"], "rm -rf /", "the other name is left alone, not merged in");
    }

    #[test]
    fn external_tools_keep_their_own_argument_names() {
        assert_eq!(
            args("mcp__db__query", r#"{"cmd":"select 1","filePath":"x"}"#),
            serde_json::json!({ "cmd": "select 1", "filePath": "x" })
        );
        assert_eq!(args("edit_file", r#"{"path":"a.rs","note":"no edit here"}"#), serde_json::json!({ "path": "a.rs", "note": "no edit here" }));
    }

    #[test]
    fn several_files_to_read_are_given_as_files_however_they_were_named() {
        assert_eq!(
            args("read_file", r#"{"paths":["a.rs","b.rs"]}"#),
            serde_json::json!({ "files": [ { "path": "a.rs" }, { "path": "b.rs" } ] })
        );
        assert_eq!(
            args("read_file", r#"{"files":[{"file_path":"a.rs","limit":5},"b.rs"]}"#),
            serde_json::json!({ "files": [ { "path": "a.rs", "limit": 5 }, { "path": "b.rs" } ] })
        );
    }

    #[test]
    fn each_file_of_a_batch_edit_takes_other_agents_names_too() {
        assert_eq!(
            args("edit_file", r#"{"files":[{"filePath":"a.rs","oldString":"x","newString":"y"},{"path":"b.rs","edits":[{"TargetContent":"p","ReplacementContent":"q"}]}]}"#),
            serde_json::json!({ "files": [
                { "path": "a.rs", "edits": [ { "old_string": "x", "new_string": "y" } ] },
                { "path": "b.rs", "edits": [ { "old_string": "p", "new_string": "q" } ] }
            ] })
        );
    }

    #[test]
    fn single_file_in_files_array_with_root_edits_becomes_path_and_edits() {
        assert_eq!(
            args("edit_file", r#"{"files":[{"path":"src/parser.rs"}],"edits":[{"old_string":"a","new_string":"b"}]}"#),
            serde_json::json!({ "path": "src/parser.rs", "edits": [ { "old_string": "a", "new_string": "b" } ] })
        );
        assert_eq!(
            args("edit_file", r#"{"files":["src/parser.rs"],"edits":[{"old_string":"a","new_string":"b"}]}"#),
            serde_json::json!({ "path": "src/parser.rs", "edits": [ { "old_string": "a", "new_string": "b" } ] })
        );
        assert_eq!(
            args("edit_file", r#"{"files":[{"path":"src/parser.rs","edits":[{"old_string":"a","new_string":"b"}]}]}"#),
            serde_json::json!({ "path": "src/parser.rs", "edits": [ { "old_string": "a", "new_string": "b" } ] })
        );
    }

    #[test]
    fn wrapped_arguments_in_other_names_are_unwrapped_and_renamed() {
        assert_eq!(args("run_shell", r#"{"arguments":{"cmd":"ls"}}"#)["command"], "ls");
        assert_eq!(args("read_file", r#"{"read_file":{"file_path":"README.md"}}"#)["path"], "README.md");
    }
}
