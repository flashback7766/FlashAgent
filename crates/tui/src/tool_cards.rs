//! How a tool call looks in the transcript.
//!
//! One tool call becomes one line, which is rewritten when the call
//! finishes. Some tools fold into the line above them instead: ten reads in
//! a row are one "Explored 10 files" rather than ten lines, and those lines
//! carry a [`ToolGroupKind`] holding the running count.
//!
//! Most tools need none of that. Their line says the same three things every
//! time — running, done, failed — and differs only in the verb and in which
//! argument it names, so they are a table ([`fixed_labels`]) rather than a
//! branch each.

use super::*;

/// The colours a tool line is written in.
const MUTED: &str = "\x1b[38;2;160;165;180m";
const STRONG: &str = "\x1b[38;2;225;230;240m";
const FAILED: &str = "\x1b[38;2;230;120;120m";
const FAINT: &str = "\x1b[38;2;120;125;140m";
const OFF: &str = "\x1b[0m";

/// The mark at the end of a tool line that says the card can be opened.
fn chevron() -> String {
    format!("{FAINT}›{OFF}")
}

/// A tool line: a verb, optionally what it acted on, and the chevron.
fn card_line(verb: &str, subject: Option<&str>) -> String {
    match subject {
        Some(subject) => format!("  {MUTED}{verb}{OFF} {STRONG}{subject}{OFF} {}", chevron()),
        None => format!("  {MUTED}{verb}{OFF} {}", chevron()),
    }
}

/// The same line, in the colour of something that did not work.
fn failed_line(verb: &str, subject: Option<&str>) -> String {
    match subject {
        Some(subject) => format!("  {FAILED}{verb}{OFF} {STRONG}{subject}{OFF} {}", chevron()),
        None => format!("  {FAILED}{verb}{OFF} {}", chevron()),
    }
}

/// What a tool's line says in each of its three states.
struct Labels {
    run: String,
    done: String,
    fail: String,
}

impl Labels {
    /// The usual shape: three verbs over one subject, with the failure in
    /// the colour of a failure.
    fn new(verbs: (&str, &str, &str), subject: Option<&str>) -> Self {
        Self {
            run: card_line(verbs.0, subject),
            done: card_line(verbs.1, subject),
            fail: failed_line(verbs.2, subject),
        }
    }
}

/// The wording for every tool whose line is the same each time it runs.
///
/// Adding a tool is adding an arm here; the card, the grouping and the
/// repaint are the same for all of them.
fn fixed_labels(name: &str, parsed: &serde_json::Value) -> Labels {
    let text = |key: &str| parsed.get(key).and_then(|v| v.as_str());
    match name {
        "memory_create" | "memory_update" | "memory_remove" => {
            let scope = format!("[{}]", text("scope").unwrap_or("project"));
            let verbs = match name {
                "memory_create" => ("Creating memory", "Created memory", "Failed to create memory"),
                "memory_update" => ("Updating memory", "Updated memory", "Failed to update memory"),
                _ => ("Removing memory", "Removed memory", "Failed to remove memory"),
            };
            Labels::new(verbs, Some(&scope))
        }
        "outline_file" => {
            let path = format_cmd(text("path").unwrap_or("file"));
            Labels::new(("Outlining", "Outlined", "Failed to outline"), Some(&path))
        }
        "web_search" => {
            let query = format!("\"{}\"", format_cmd(text("query").unwrap_or("web")));
            Labels::new(("Searching web:", "Searched web:", "Web search failed:"), Some(&query))
        }
        "web_fetch" => {
            let url = format_cmd(text("url").unwrap_or("url"));
            Labels::new(("Fetching", "Fetched", "Failed to fetch"), Some(&url))
        }
        "git_status" => Labels::new(
            ("Checking git status", "Checked git status", "Failed to check git status"),
            None,
        ),
        "git_diff" => {
            let staged = parsed.get("staged").and_then(|v| v.as_bool()).unwrap_or(false);
            let what = if staged { "staged git diff" } else { "git diff" };
            Labels {
                run: card_line(&format!("Checking {what}"), None),
                done: card_line(&format!("Checked {what}"), None),
                fail: failed_line(&format!("Failed to check {what}"), None),
            }
        }
        "env_info" => Labels::new(
            ("Inspecting environment", "Inspected environment", "Failed to inspect environment"),
            None,
        ),
        "ask_user" => {
            let question =
                format_cmd(text("question").or_else(|| text("prompt")).unwrap_or("confirmation"));
            Labels {
                run: card_line("Asking user:", Some(&question)),
                done: card_line("Asked user:", Some(&question)),
                // The question is gone from the screen by then, and repeating
                // it would read as though it had been answered.
                fail: failed_line("Cancelled question", None),
            }
        }
        // Anything else, including an MCP server's tools: named, with its
        // main argument if one of the usual names carries it.
        _ => {
            let argument = ["path", "file", "target", "pattern", "query", "command", "task", "scope", "url", "name", "key"]
                .iter()
                .find_map(|key| text(key));
            let subject = match argument {
                Some(argument) => format!("{name} [{}]", format_cmd(argument)),
                None => name.to_string(),
            };
            Labels {
                run: card_line("Running", Some(&subject)),
                // "read_notes [notes.txt] finished" is one statement, so it
                // is one colour rather than a verb and a subject.
                done: card_line(&format!("{subject} finished"), None),
                fail: failed_line(&format!("{subject} failed"), None),
            }
        }
    }
}

impl ChatView {
    /// A tool call has started: open the line that stands for it.
    pub(crate) fn tool_started(&mut self, name: &str, args_json: &str) {
        self.streaming = None;
        if let Some(i) = self.streaming_reasoning.take() {
            let secs = self.reasoning_start.take().map(|t| t.elapsed().as_secs()).unwrap_or(1);
            self.lines[i].reasoning_secs = Some(secs);
        }

        // Read the way the tool reads it, so other agents' argument names and
        // batches show what will actually happen.
        let parsed: serde_json::Value =
            flashagent_llm::effective_args(args_json, name).unwrap_or_default();

        // The model is asked to say what a call is for. When it did, that
        // sentence is the line: "Add the missing null check to parser.rs"
        // tells you more than "Editing parser.rs" could. The expanded card
        // underneath is unchanged, so the facts stay one keypress away.
        let header = parsed
            .get("header")
            .and_then(|h| h.as_str())
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(format_cmd);

        match header {
            Some(header) => self.header_card(name, args_json, &parsed, &header),
            None => match name {
                "run_shell" => self.shell_card(args_json, &parsed),
                "read_file" | "list_dir" | "glob" | "grep" => self.explore_card(name, args_json, &parsed),
                "edit_file" | "write_file" | "patch_file" => self.edit_card(name, args_json, &parsed),
                "spawn_agent" => self.subagent_card(args_json, &parsed),
                "memory_read" => self.memory_read_card(args_json, &parsed),
                _ => {
                    let labels = fixed_labels(name, &parsed);
                    self.push_card(name, args_json, labels);
                }
            },
        }

        if let Some(last_line) = self.lines.last_mut() {
            last_line.tool_calls.push(ToolCallRecord {
                name: name.to_string(),
                args_json: args_json.to_string(),
                result: None,
                is_error: false,
                is_running: true,
            });
        }
    }

    /// Open a line whose wording is fixed, and make it the open card.
    fn push_card(&mut self, name: &str, args_json: &str, labels: Labels) {
        let Labels { run, done, fail } = labels;
        let mut line = ChatLine::with_details(LineKind::Tool, run.clone(), args_json.to_string());
        line.tool_name = Some(name.to_string());
        line.tool_group = Some(ToolGroupKind::Custom {
            run_text: run,
            done_text: done,
            fail_text: fail,
            is_running: true,
        });
        self.lines.push(line);
        self.open_tool = Some(self.lines.len() - 1);
    }

    /// Fold this call into the line above when that line is a group of the
    /// same kind, letting `fold` update the running count and the text.
    ///
    /// Returns whether it folded; when it did not, the caller opens a new
    /// line instead.
    fn fold_into_previous(
        &mut self,
        args_json: &str,
        fold: impl FnOnce(&mut ChatLine) -> bool,
    ) -> bool {
        let Some(last_line) = self.lines.last_mut() else { return false };
        if !fold(last_line) {
            return false;
        }
        // The open card shows every call it stands for, one after another.
        if let Some(ref mut details) = last_line.details {
            details.push_str("\n\n---\n\n");
            details.push_str(args_json);
        }
        self.open_tool = Some(self.lines.len() - 1);
        self.needs_reprint = true;
        true
    }

    /// The model said what the call is for, so the line is that sentence.
    fn header_card(&mut self, name: &str, args_json: &str, parsed: &serde_json::Value, header: &str) {
        // The header is the model's intent; the suffix is what the call
        // actually names. "Add the missing null check" reads well, but only
        // "parser.rs +3 -1" says what happened to the tree.
        let facts = call_facts(name, parsed)
            // "Read main.rs · main.rs" says it twice. When the header already
            // names the file, only what it cannot say is worth adding, which
            // is the size of the change.
            .and_then(|f| {
                let said = |part: &str| header.to_lowercase().contains(&part.to_lowercase());
                match f.split_once(" +") {
                    Some((file, size)) if said(file) => Some(format!("+{size}")),
                    None if said(&f) => None,
                    _ => Some(f),
                }
            })
            .map(|f| format!(" {FAINT}·{OFF} {MUTED}{f}{OFF}"))
            .unwrap_or_default();
        let run = format!("  {MUTED}{header}{OFF}{facts} {}", chevron());
        let labels = Labels {
            done: run.clone(),
            fail: failed_line(&format!("Failed to {}", lower_first(header)), None),
            run,
        };
        self.push_card(name, args_json, labels);
    }

    /// A shell command. Commands in a row become "Ran 3 commands".
    fn shell_card(&mut self, args_json: &str, parsed: &serde_json::Value) {
        let cmd = parsed.get("command").and_then(|s| s.as_str()).unwrap_or("command").to_string();
        let folded = {
            let cmd = cmd.clone();
            self.fold_into_previous(args_json, |line| {
                let Some(ToolGroupKind::Command { count, last_cmd, is_running }) = &mut line.tool_group
                else {
                    return false;
                };
                *count += 1;
                *last_cmd = cmd;
                *is_running = true;
                line.text = card_line(&format!("Running {count} commands"), None);
                true
            })
        };
        if !folded {
            let text = card_line("Running", Some(&format_cmd(&cmd)));
            let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.to_string());
            line.tool_name = Some("run_shell".to_string());
            line.tool_group = Some(ToolGroupKind::Command { count: 1, last_cmd: cmd, is_running: true });
            self.lines.push(line);
            self.open_tool = Some(self.lines.len() - 1);
        }
    }

    /// Reading and searching: both are looking around, and both fold into
    /// one "Explored 10 files · 3 searches" line.
    fn explore_card(&mut self, name: &str, args_json: &str, parsed: &serde_json::Value) {
        let is_file = name == "read_file";
        let read_paths = if is_file { call_paths(parsed) } else { Vec::new() };
        // A batch read counts every file it reads.
        let file_count = if is_file { read_paths.len().max(1) } else { 0 };
        let batch_label: String;
        let target = if is_file {
            match read_paths.len() {
                0 => "file",
                1 => read_paths[0].as_str(),
                _ => {
                    batch_label = paths_label(&read_paths, false);
                    batch_label.as_str()
                }
            }
        } else {
            parsed
                .get("pattern")
                .or_else(|| parsed.get("query"))
                .or_else(|| parsed.get("path"))
                .and_then(|s| s.as_str())
                // A listing of the working directory carries no path and no
                // pattern, and "Searched search" is not a sentence.
                .unwrap_or("the project")
        };
        // Nor is "Searched .".
        let target = if matches!(target, "." | "./" | "") { "the project" } else { target };

        let folded = self.fold_into_previous(args_json, |line| {
            let Some(ToolGroupKind::Explore { files, searches, last_target, is_running }) =
                &mut line.tool_group
            else {
                return false;
            };
            if is_file {
                *files += file_count;
            } else {
                *searches += 1;
            }
            *last_target = target.to_string();
            *is_running = true;
            line.text = format_explore(true, *files, *searches, target);
            true
        });
        if !folded {
            let searches = if is_file { 0 } else { 1 };
            let text = format_explore(true, file_count, searches, target);
            let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.to_string());
            line.tool_name = Some(name.to_string());
            line.tool_group = Some(ToolGroupKind::Explore {
                files: file_count,
                searches,
                last_target: target.to_string(),
                is_running: true,
            });
            self.lines.push(line);
            self.open_tool = Some(self.lines.len() - 1);
        }
    }

    /// A change to a file, counted in lines so the finished line can say
    /// "+12 -3".
    fn edit_card(&mut self, name: &str, args_json: &str, parsed: &serde_json::Value) {
        let path = paths_label(&call_paths(parsed), false);
        let basename = std::path::Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&path)
            .to_string();

        let (added, deleted) = match name {
            "edit_file" => {
                let (mut a, mut d) = (0, 0);
                for edit in call_edits(parsed) {
                    let old_s = edit.get("old_string").and_then(|s| s.as_str()).unwrap_or("");
                    let new_s = edit.get("new_string").and_then(|s| s.as_str()).unwrap_or("");
                    d += old_s.lines().count();
                    a += new_s.lines().count();
                }
                (a, d)
            }
            "write_file" => {
                let content = parsed.get("content").and_then(|s| s.as_str()).unwrap_or("");
                (content.lines().count().max(1), 0)
            }
            _ => {
                let patch = parsed.get("patch").and_then(|s| s.as_str()).unwrap_or("");
                let (mut a, mut d) = (0, 0);
                for l in patch.lines() {
                    if l.starts_with('+') && !l.starts_with("+++") {
                        a += 1;
                    } else if l.starts_with('-') && !l.starts_with("---") {
                        d += 1;
                    }
                }
                (a, d)
            }
        };

        let text = format!("  {MUTED}Editing{OFF} \x1b[1;38;2;225;230;240m{basename}{OFF} {}", chevron());
        let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.to_string());
        line.tool_name = Some(name.to_string());
        line.tool_group = Some(ToolGroupKind::Edit { path, added, deleted, is_running: true });
        self.lines.push(line);
        self.open_tool = Some(self.lines.len() - 1);
    }

    /// A subagent sent off with a task of its own.
    fn subagent_card(&mut self, args_json: &str, parsed: &serde_json::Value) {
        let task = parsed.get("task").and_then(|s| s.as_str()).unwrap_or("task").to_string();
        let folded = {
            let task = task.clone();
            self.fold_into_previous(args_json, |line| {
                let Some(ToolGroupKind::Subagent { count, last_task, is_running }) = &mut line.tool_group
                else {
                    return false;
                };
                *count += 1;
                *last_task = task;
                *is_running = true;
                line.text = card_line(&format!("Explored {count} tasks"), None);
                true
            })
        };
        if !folded {
            let text = card_line(&format!("Running subagent: {}", format_cmd(&task)), None);
            let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.to_string());
            line.tool_name = Some("spawn_agent".to_string());
            line.tool_group =
                Some(ToolGroupKind::Subagent { count: 1, last_task: task, is_running: true });
            self.lines.push(line);
            self.open_tool = Some(self.lines.len() - 1);
        }
    }

    /// Reading memory, which names the scopes it read rather than a count.
    fn memory_read_card(&mut self, args_json: &str, parsed: &serde_json::Value) {
        let scope = parsed.get("scope").and_then(|s| s.as_str()).unwrap_or("project").to_string();
        let folded = {
            let scope = scope.clone();
            self.fold_into_previous(args_json, |line| {
                let Some(ToolGroupKind::Memory { scopes, is_running }) = &mut line.tool_group else {
                    return false;
                };
                if !scopes.contains(&scope) {
                    scopes.push(scope);
                }
                *is_running = true;
                line.text = card_line("Reading memory", Some(&format!("[{}]", scopes.join(", "))));
                true
            })
        };
        if !folded {
            let text = card_line("Reading memory", Some(&format!("[{scope}]")));
            let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.to_string());
            line.tool_name = Some("memory_read".to_string());
            line.tool_group = Some(ToolGroupKind::Memory { scopes: vec![scope], is_running: true });
            self.lines.push(line);
            self.open_tool = Some(self.lines.len() - 1);
        }
    }

    /// A tool call has finished: rewrite its line as what happened.
    pub(crate) fn tool_finished(&mut self, is_error: bool, result_len: usize, result: Option<&str>) {
        self.streaming = None;
        self.streaming_reasoning = None;
        let Some(i) = self.open_tool.take() else { return };

        self.lines[i].tool_result_len += result_len;
        if let Some(res) = result {
            match &mut self.lines[i].tool_result {
                // A folded group keeps every result, in the order they came.
                Some(existing) => {
                    existing.push_str("\n\n---\n\n");
                    existing.push_str(res);
                }
                None => self.lines[i].tool_result = Some(res.to_string()),
            }
        }
        if let Some(last_call) = self.lines[i].tool_calls.last_mut() {
            last_call.is_running = false;
            last_call.is_error = is_error;
            last_call.result = result.map(str::to_string);
        }

        let Some(mut group) = self.lines[i].tool_group.take() else { return };
        let text = finished_text(&mut group, is_error);
        self.lines[i].kind = if is_error { LineKind::ToolError } else { LineKind::Tool };
        self.lines[i].text = text;
        self.lines[i].tool_group = Some(group);
    }
}

/// What a finished line says, and mark the group as no longer running.
fn finished_text(group: &mut ToolGroupKind, is_error: bool) -> String {
    match group {
        ToolGroupKind::Command { count, last_cmd, is_running } => {
            *is_running = false;
            match (is_error, *count) {
                (true, 1) => failed_line("Failed", Some(&format_cmd(last_cmd))),
                (true, n) => failed_line(&format!("Failed {n} commands"), None),
                (false, 1) => card_line("Ran", Some(&format_cmd(last_cmd))),
                (false, n) => card_line(&format!("Ran {n} commands"), None),
            }
        }
        ToolGroupKind::Explore { files, searches, last_target, is_running } => {
            *is_running = false;
            format_explore(false, *files, *searches, last_target)
        }
        ToolGroupKind::Edit { path, added, deleted, is_running } => {
            *is_running = false;
            let basename =
                std::path::Path::new(path).file_name().and_then(|s| s.to_str()).unwrap_or(path);
            format!(
                "  {MUTED}Edited{OFF} \x1b[1;38;2;225;230;240m{basename}{OFF} \x1b[38;2;145;205;140m+{added}{OFF} {FAILED}-{deleted}{OFF}"
            )
        }
        ToolGroupKind::Subagent { count, last_task, is_running } => {
            *is_running = false;
            match *count {
                1 => card_line(&format!("{} finished", format_cmd(last_task)), None),
                n => card_line(&format!("Explored {n} tasks"), None),
            }
        }
        ToolGroupKind::Memory { scopes, is_running } => {
            *is_running = false;
            let scopes = format!("[{}]", scopes.join(", "));
            if is_error {
                failed_line("Failed to read memory", Some(&scopes))
            } else {
                card_line("Read memory", Some(&scopes))
            }
        }
        ToolGroupKind::Generic { name, summary, is_running } => {
            *is_running = false;
            card_line(&format!("{name} [{summary}] finished"), None)
        }
        ToolGroupKind::Custom { done_text, fail_text, is_running, .. } => {
            *is_running = false;
            if is_error {
                fail_text.clone()
            } else {
                done_text.clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(name: &str, args: &str) -> ChatView {
        let mut chat = ChatView::default();
        chat.tool_started(name, args);
        chat
    }

    fn text_of(chat: &ChatView) -> String {
        strip_ansi(&chat.lines.last().expect("a line was opened").text)
    }

    #[test]
    fn a_tool_line_names_the_tool_and_what_it_acts_on() {
        let chat = started("web_search", r#"{"query":"rust ownership"}"#);
        assert_eq!(text_of(&chat), "  Searching web: \"rust ownership\" ›");
        let chat = started("outline_file", r#"{"path":"src/main.rs"}"#);
        assert_eq!(text_of(&chat), "  Outlining src/main.rs ›");
        let chat = started("git_diff", r#"{"staged":true}"#);
        assert_eq!(text_of(&chat), "  Checking staged git diff ›");
    }

    #[test]
    fn a_tool_nobody_wrote_a_card_for_still_gets_a_readable_line() {
        // An MCP server's tools arrive with names this build has never seen.
        let chat = started("mcp__sqlite__query", r#"{"query":"select 1"}"#);
        assert_eq!(text_of(&chat), "  Running mcp__sqlite__query [select 1] ›");
        let chat = started("mcp__clock__now", "{}");
        assert_eq!(text_of(&chat), "  Running mcp__clock__now ›");
    }

    #[test]
    fn what_the_model_said_the_call_is_for_wins_over_the_tool_name() {
        let chat = started("edit_file", r#"{"header":"Add the missing null check","path":"src/parse.rs","edits":[]}"#);
        assert!(text_of(&chat).starts_with("  Add the missing null check"), "{}", text_of(&chat));
    }

    #[test]
    fn a_finished_call_says_what_happened_and_a_failed_one_says_it_failed() {
        let mut chat = started("web_fetch", r#"{"url":"https://example.com"}"#);
        chat.tool_finished(false, 10, Some("page"));
        assert_eq!(text_of(&chat), "  Fetched https://example.com ›");

        let mut chat = started("web_fetch", r#"{"url":"https://example.com"}"#);
        chat.tool_finished(true, 0, Some("404"));
        assert_eq!(text_of(&chat), "  Failed to fetch https://example.com ›");
        assert_eq!(chat.lines.last().unwrap().kind, LineKind::ToolError);
    }

    #[test]
    fn calls_of_the_same_kind_in_a_row_become_one_line() {
        let mut chat = started("run_shell", r#"{"command":"ls"}"#);
        chat.tool_finished(false, 1, Some("a.txt"));
        chat.tool_started("run_shell", r#"{"command":"pwd"}"#);
        chat.tool_finished(false, 1, Some("/tmp"));
        assert_eq!(chat.lines.len(), 1, "two commands should share one line");
        assert_eq!(text_of(&chat), "  Ran 2 commands ›");

        let mut chat = started("read_file", r#"{"path":"a.rs"}"#);
        chat.tool_finished(false, 1, Some("..."));
        chat.tool_started("grep", r#"{"pattern":"fn main"}"#);
        chat.tool_finished(false, 1, Some("..."));
        assert_eq!(chat.lines.len(), 1);
        let line = text_of(&chat);
        assert!(line.contains('1') && line.to_lowercase().contains("search"), "{line}");
    }

    #[test]
    fn every_call_of_a_folded_line_is_kept_under_it() {
        let mut chat = started("run_shell", r#"{"command":"ls"}"#);
        chat.tool_finished(false, 1, Some("a.txt"));
        chat.tool_started("run_shell", r#"{"command":"pwd"}"#);
        chat.tool_finished(false, 1, Some("/tmp"));
        let line = chat.lines.last().unwrap();
        assert_eq!(line.tool_calls.len(), 2, "both calls are kept for the open card");
        let details = line.details.clone().unwrap_or_default();
        assert!(details.contains("ls") && details.contains("pwd"), "{details}");
        let result = line.tool_result.clone().unwrap_or_default();
        assert!(result.contains("a.txt") && result.contains("/tmp"), "{result}");
    }
}
