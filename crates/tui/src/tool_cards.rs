//! Compact transcript rows name the action, its exact target and the known
//! outcome. Only adjacent successful filesystem reads/searches share a row;
//! a command, change, failure or refusal always keeps its own card.

use super::*;
use crate::i18n::tr;
use crate::tool_views::CallState;

const MUTED: &str = "\x1b[38;2;160;165;180m";
const STRONG: &str = "\x1b[38;2;225;230;240m";
const FAILED: &str = "\x1b[38;2;230;120;120m";
const FAINT: &str = "\x1b[38;2;120;125;140m";
const OFF: &str = "\x1b[0m";
const ADDED_COLOUR: &str = "\x1b[38;2;135;220;145m";
const REMOVED_COLOUR: &str = "\x1b[38;2;245;120;120m";

/// Counts keep the same meaning and colours as the diff below them.
pub(crate) fn colored_size(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let is_size = (c == '+' || c == '-')
            && (i == 0 || chars[i - 1] == ' ')
            && chars.get(i + 1).is_some_and(char::is_ascii_digit);
        if !is_size {
            out.push(c);
            i += 1;
            continue;
        }
        let colour = if c == '+' { ADDED_COLOUR } else { REMOVED_COLOUR };
        let start = i;
        i += 1;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        out.push_str(colour);
        out.extend(chars[start..i].iter());
        out.push_str(OFF);
    }
    out
}

/// The margin mark used by non-call tool notices, such as a child's answer.
pub(crate) const TOOL_MARK: &str = "▸";

fn one_line(text: &str) -> String {
    crate::terminal_safe(&strip_ansi(text).replace('\n', " \\n "))
}

pub(crate) fn arguments(call: &ToolCallRecord) -> serde_json::Value {
    flashagent_llm::effective_args(&call.args_json, &call.name).unwrap_or_default()
}

/// Nothing here shortens a path to its basename or rewrites a shell command.
/// Width clipping belongs to the row, not to the retained facts.
pub(crate) fn target(name: &str, parsed: &serde_json::Value) -> String {
    let text = |key: &str| parsed.get(key).and_then(|value| value.as_str());
    match name {
        "read_file" | "outline_file" | "edit_file" | "write_file" | "patch_file" => {
            let paths = call_paths(parsed);
            if paths.is_empty() { tr("file").to_string() } else { paths.join(", ") }
        }
        "run_shell" => text("command").unwrap_or(tr("command")).to_string(),
        "grep" => {
            let pattern = text("pattern").or_else(|| text("query")).unwrap_or("");
            match text("glob") {
                Some(filter) => format!("\"{pattern}\" [{filter}]"),
                None => format!("\"{pattern}\""),
            }
        }
        "glob" => format!("\"{}\"", text("pattern").unwrap_or("")),
        "list_dir" | "git_status" | "git_diff" => text("path").unwrap_or(".").to_string(),
        "web_search" => format!("\"{}\"", text("query").unwrap_or("")),
        "web_fetch" => text("url").unwrap_or("").to_string(),
        "spawn_agent" => text("task").unwrap_or(tr("task")).to_string(),
        "ask_user" => {
            let questions = parsed.get("questions").and_then(|q| q.as_array());
            let first = questions.and_then(|q| q.first()).and_then(|q| q.get("question")).and_then(|q| q.as_str());
            let mut question = first.or_else(|| text("question")).or_else(|| text("prompt")).unwrap_or(tr("a question")).to_string();
            let more = questions.map_or(0, |q| q.len().saturating_sub(1));
            if more > 0 {
                question.push_str(&tr(" (+{more} more)").replace("{more}", &more.to_string()));
            }
            question
        }
        name if name.starts_with("memory_") => {
            let scope = text("scope").unwrap_or("project");
            match text("name").or_else(|| text("key")) {
                Some(name) => format!("[{scope}] {name}"),
                None => format!("[{scope}]"),
            }
        }
        _ => ["path", "file", "target", "pattern", "query", "command", "task", "scope", "url", "name", "key"]
            .iter().find_map(|key| text(key)).unwrap_or("").to_string(),
    }
}

fn action(name: &str, parsed: &serde_json::Value, state: CallState) -> String {
    if state == CallState::NotRun {
        return tr("Not run:").to_string();
    }
    if state == CallState::Waiting {
        return match name {
            "run_shell" => tr("Waiting to run").to_string(),
            "edit_file" | "patch_file" => tr("Waiting to edit").to_string(),
            "write_file" => tr("Waiting to write").to_string(),
            "ask_user" => tr("Asking user:").to_string(),
            _ => format!("{} {name}", tr("Waiting on")),
        };
    }
    let verbs = match name {
        "run_shell" => (tr("Running"), tr("Ran"), tr("Failed")),
        "read_file" => (tr("Reading"), tr("Read"), tr("Failed to read")),
        "outline_file" => (tr("Outlining"), tr("Outlined"), tr("Failed to outline")),
        "list_dir" => (tr("Analyzing"), tr("Analyzed"), tr("Failed to explore")),
        "grep" | "glob" => (tr("Searching"), tr("Searched"), tr("Failed to explore")),
        "edit_file" | "patch_file" => (tr("Editing"), tr("Edited"), tr("Failed to edit")),
        "write_file" => (tr("Writing"), tr("Wrote"), tr("Not written:")),
        "spawn_agent" => (tr("Running subagent:"), tr("Subagent finished:"), tr("Subagent failed:")),
        "memory_read" => (tr("Reading memory"), tr("Read memory"), tr("Failed to read memory")),
        "memory_create" => (tr("Creating memory"), tr("Created memory"), tr("Failed to create memory")),
        "memory_update" => (tr("Updating memory"), tr("Updated memory"), tr("Failed to update memory")),
        "memory_remove" => (tr("Removing memory"), tr("Removed memory"), tr("Failed to remove memory")),
        "web_search" => (tr("Searching web:"), tr("Searched web:"), tr("Web search failed:")),
        "web_fetch" => (tr("Fetching"), tr("Fetched"), tr("Failed to fetch")),
        "git_status" => (tr("Checking git status"), tr("Checked git status"), tr("Failed to check git status")),
        "env_info" => (tr("Inspecting environment"), tr("Inspected environment"), tr("Failed to inspect environment")),
        "ask_user" => (tr("Asking user:"), tr("Asked user:"), tr("Cancelled question")),
        "git_diff" => {
            let what = if parsed.get("staged").and_then(|s| s.as_bool()).unwrap_or(false) { tr("staged git diff") } else { "git diff" };
            let verb = match state {
                CallState::Done => tr("Checked {what}"),
                CallState::Failed => tr("Failed to check {what}"),
                _ => tr("Checking {what}"),
            };
            return verb.replace("{what}", what);
        }
        "subagent_answer" => return tr("A subagent answered").to_string(),
        _ => {
            let verb = match state { CallState::Done => tr("Ran"), CallState::Failed => tr("Failed"), _ => tr("Running") };
            return format!("{verb} {name}");
        }
    };
    match state {
        CallState::Done => verbs.1,
        CallState::Failed => verbs.2,
        CallState::Waiting | CallState::NotRun => unreachable!(),
        CallState::Running => verbs.0,
    }.to_string()
}

pub(crate) fn not_run_note(result: &str) -> Option<&'static str> {
    if result == flashagent_core::permissions::DECLINED {
        Some(tr("declined"))
    } else if result.starts_with(flashagent_core::permissions::DENIED) {
        Some(tr("not allowed"))
    } else if flashagent_core::stopped_by_user(result) {
        Some(tr("stopped"))
    } else if result.contains(flashagent_core::permissions::QUESTION_CLOSED) {
        Some(tr("not answered"))
    } else {
        None
    }
}

fn failure_reason(result: &str) -> Option<String> {
    let mut line = result.lines().map(str::trim).find(|line| !line.is_empty())?;
    while let Some(rest) = ["error:", "Error:", "edit:", "write:", "run_shell:"]
        .iter().find_map(|prefix| line.strip_prefix(prefix))
    {
        line = rest.trim_start();
    }
    Some(crate::tool_views::clip_ellipsis(line, 72))
}

fn change_size(added: usize, deleted: usize) -> Option<String> {
    let mut parts = Vec::new();
    if added > 0 { parts.push(format!("+{added}")); }
    if deleted > 0 { parts.push(format!("-{deleted}")); }
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub(crate) fn requested_change(name: &str, parsed: &serde_json::Value) -> (usize, usize, Option<String>) {
    match name {
        "write_file" => {
            let content = parsed.get("content").and_then(|s| s.as_str());
            (content.map_or(0, |s| s.lines().count()), 0, content.map(str::to_string))
        }
        "patch_file" => {
            let patch = parsed.get("patch").and_then(|s| s.as_str());
            let added = patch.map_or(0, |s| s.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).count());
            let deleted = patch.map_or(0, |s| s.lines().filter(|l| l.starts_with('-') && !l.starts_with("---")).count());
            (added, deleted, patch.map(str::to_string))
        }
        _ => {
            let mut body = String::new();
            let mut added = 0;
            let mut deleted = 0;
            for edit in call_edits(parsed) {
                for (key, mark) in [("old_string", '-'), ("new_string", '+')] {
                    for line in edit.get(key).and_then(|s| s.as_str()).unwrap_or("").lines() {
                        body.push(mark);
                        body.push_str(line);
                        body.push('\n');
                        if mark == '+' { added += 1; } else { deleted += 1; }
                    }
                }
            }
            (added, deleted, (!body.is_empty()).then_some(body))
        }
    }
}

fn outcome(call: &ToolCallRecord, parsed: &serde_json::Value) -> Option<String> {
    if call.is_running {
        return None;
    }
    if let Some(note) = call.result.as_deref().and_then(not_run_note) {
        return Some(note.to_string());
    }
    let result = call.result.as_deref();
    if call.name == "run_shell" {
        let code = result.and_then(|text| text.lines().find_map(|line| {
            line.trim().strip_prefix("exit code:")?.trim().parse::<i32>().ok()
        }));
        if let Some(code) = code {
            return Some(format!("{} {code}", tr("exit")));
        }
    }
    if call.is_error {
        return result.and_then(failure_reason);
    }
    if call.name == "run_shell" {
        // Shell output is not a list of lines to count; without an exit code
        // line there is nothing truthful to add to the row.
        return None;
    }
    match call.name.as_str() {
        "read_file" => result.map(|text| {
            if text.trim() == "(empty or past end of file)" { text.trim().to_string() }
            else { crate::text::plural(crate::tool_views::read_line_count(text), "line", "lines") }
        }),
        "grep" => result.map(|text| crate::text::plural(text.lines().filter(|line| crate::tool_views::search_match(line).is_some()).count(), "match", "matches")),
        "glob" => result.map(|text| crate::text::plural(crate::tool_views::listing_count(text), "file", "files")),
        "list_dir" => result.map(|text| crate::text::plural(crate::tool_views::listing_count(text), "entry", "entries")),
        "edit_file" | "write_file" | "patch_file" => {
            if call.diff.is_some() {
                change_size(call.added, call.deleted).or_else(|| result.and_then(failure_reason))
            } else {
                // Saved sessions keep the arguments, not the original files.
                // Their requested strings cannot stand in for a real diff.
                let (added, deleted, _) = requested_change(&call.name, parsed);
                change_size(added, deleted).map(|size| format!("{} {size}", tr("requested")))
                    .or_else(|| result.and_then(failure_reason))
            }
        }
        _ => result.and_then(|text| {
            let count = text.lines().count();
            if count > 1 { Some(crate::text::plural(count, "line", "lines")) } else { failure_reason(text) }
        }),
    }
}

struct Summary {
    state: CallState,
    action: String,
    target: String,
    outcome: Option<String>,
    elapsed_ms: Option<u64>,
}

impl Summary {
    fn row(&self, width: usize, animated: bool) -> String {
        let mark = self.state.marker(animated);
        let colour = self.state.colour();
        let action = one_line(&self.action);
        let mut suffix = String::new();
        if let Some(result) = &self.outcome {
            let result_colour = if self.state == CallState::Failed { FAILED } else { MUTED };
            suffix.push_str(&format!(" {FAINT}·{OFF} {result_colour}{}{OFF}", colored_size(&one_line(result))));
        }
        if let Some(ms) = self.elapsed_ms {
            let elapsed = crate::goal::human_duration(std::time::Duration::from_millis(ms));
            suffix.push_str(&format!(" {FAINT}·{OFF} {FAINT}{elapsed}{OFF}"));
        }
        let prefix = format!("  {FAINT}›{OFF} {colour}{mark}{OFF} {MUTED}{action}{OFF}");
        let mut target = one_line(&self.target);
        let available = width.saturating_sub(visible_width(&prefix));
        if !target.is_empty() {
            if visible_width(&suffix) + 8 > available { suffix = crate::tool_views::clip_ellipsis(&suffix, available / 2); }
            let room = available.saturating_sub(visible_width(&suffix) + 1);
            target = format!(" {STRONG}{}{OFF}", crate::tool_views::clip_ellipsis(&target, room));
        }
        crate::tool_views::clip_ellipsis(&format!("{prefix}{target}{suffix}"), width)
    }
}

fn call_summary(call: &ToolCallRecord, waiting: bool) -> Summary {
    let parsed = arguments(call);
    Summary {
        state: call.state(waiting),
        action: action(&call.name, &parsed, call.state(waiting)),
        target: target(&call.name, &parsed),
        outcome: outcome(call, &parsed),
        elapsed_ms: call.elapsed_ms,
    }
}

pub(crate) fn call_title(call: &ToolCallRecord, waiting: bool) -> String {
    let summary = call_summary(call, waiting);
    let state = summary.state;
    let action = one_line(&summary.action);
    let tool = one_line(&call.name);
    let label = if action.ends_with(&tool) { action } else { format!("{action} {FAINT}·{OFF} {MUTED}{tool}") };
    let mut title = format!("{}{}{OFF} {MUTED}{label}{OFF}", state.colour(), state.marker(true));
    if !summary.target.is_empty() {
        title.push_str(&format!(" {FAINT}·{OFF} {STRONG}{}{OFF}", one_line(&summary.target)));
    }
    if let Some(result) = summary.outcome {
        title.push_str(&format!(" {FAINT}·{OFF} {MUTED}{}{OFF}", colored_size(&one_line(&result))));
    }
    if let Some(ms) = summary.elapsed_ms {
        title.push_str(&format!(" {FAINT}· {}{OFF}", crate::goal::human_duration(std::time::Duration::from_millis(ms))));
    }
    title
}

fn is_explore(name: &str) -> bool {
    matches!(name, "read_file" | "outline_file" | "list_dir" | "glob" | "grep")
}

fn has_problem(call: &ToolCallRecord) -> bool {
    call.is_error || call.result.as_deref().is_some_and(|result| {
        not_run_note(result).is_some() || (result.lines().any(crate::tool_views::is_file_separator)
            && result.lines().any(|line| line.trim_start().starts_with("error:")))
    })
}

fn exploration_counts(calls: &[ToolCallRecord]) -> (usize, usize) {
    let mut files = 0;
    let mut searches = 0;
    for call in calls {
        if matches!(call.name.as_str(), "read_file" | "outline_file") {
            files += call_paths(&arguments(call)).len().max(1);
        } else {
            searches += 1;
        }
    }
    (files, searches)
}

fn line_summary(line: &ChatLine, waiting: bool) -> Option<Summary> {
    let last = line.tool_calls.last()?;
    if line.tool_calls.len() == 1 {
        return Some(call_summary(last, waiting));
    }
    let (files, searches) = exploration_counts(&line.tool_calls);
    let state = if line.tool_calls.iter().any(|call| call.is_running) { last.state(waiting) }
        else if line.tool_calls.iter().any(has_problem) { CallState::Failed }
        else { CallState::Done };
    let action = if state == CallState::Running || state == CallState::Waiting { tr("Exploring") }
        else if state == CallState::Failed { tr("Failed to explore") }
        else { tr("Explored") };
    let mut counts = Vec::new();
    if files > 0 { counts.push(crate::text::plural(files, "file", "files")); }
    if searches > 0 { counts.push(crate::text::plural(searches, "search", "searches")); }
    let targets = line.tool_calls.iter().map(|call| target(&call.name, &arguments(call))).collect::<Vec<_>>().join(", ");
    let results = line.tool_calls.iter().filter(|call| !call.is_running)
        .filter_map(|call| outcome(call, &arguments(call))).collect::<Vec<_>>();
    // Every exact target is retained in the row's source; the cell budget
    // clips it visibly, and the shared card lists the calls individually.
    let outcome = if state == CallState::Failed {
        Some(tr("{failed} failed").replace("{failed}", &line.tool_calls.iter().filter(|call| has_problem(call)).count().to_string()))
    } else if results.len() == 1 { results.into_iter().next() } else { None };
    let elapsed_ms = line.tool_calls.iter().try_fold(0u64, |sum, call| sum.checked_add(call.elapsed_ms?));
    Some(Summary { state, action: format!("{action} {}", counts.join(", ")), target: targets, outcome, elapsed_ms })
}

pub(crate) fn compact_row(line: &ChatLine, waiting: bool, width: usize) -> String {
    line_summary(line, waiting).map_or_else(|| crate::tool_views::clip_ellipsis(&line.text, width), |summary| summary.row(width, true))
}

pub(crate) fn group_title(line: &ChatLine, waiting: bool) -> String {
    let Some(summary) = line_summary(line, waiting) else { return one_line(&line.text) };
    let mut title = format!("{}{}{OFF} {MUTED}{}{OFF}", summary.state.colour(), summary.state.marker(true), summary.action);
    if let Some(ms) = summary.elapsed_ms {
        title.push_str(&format!(" {FAINT}· {}{OFF}", crate::goal::human_duration(std::time::Duration::from_millis(ms))));
    }
    title
}

fn sync_line(line: &mut ChatLine) {
    let Some(last) = line.tool_calls.last() else { return };
    line.tool_name = Some(last.name.clone());
    line.details = Some(line.tool_calls.iter().map(|call| call.args_json.as_str()).collect::<Vec<_>>().join("\n\n---\n\n"));
    let results = line.tool_calls.iter().filter_map(|call| call.result.as_deref()).collect::<Vec<_>>();
    line.tool_result = (!results.is_empty()).then(|| results.join("\n\n---\n\n"));
    let running = line.tool_calls.iter().any(|call| call.is_running);
    let parsed = arguments(last);
    let what = target(&last.name, &parsed);
    line.tool_group = Some(if is_explore(&last.name) {
        let (files, searches) = exploration_counts(&line.tool_calls);
        ToolGroupKind::Explore { files, searches, last_target: what, is_running: running }
    } else {
        match last.name.as_str() {
            "run_shell" => ToolGroupKind::Command { count: 1, last_cmd: what, is_running: running },
            "edit_file" | "write_file" | "patch_file" => {
                let (added, deleted, _) = requested_change(&last.name, &parsed);
                ToolGroupKind::Edit { path: what, added: last.diff.as_ref().map_or(added, |_| last.added), deleted: last.diff.as_ref().map_or(deleted, |_| last.deleted), is_running: running }
            }
            "spawn_agent" => ToolGroupKind::Subagent { count: 1, last_task: what, is_running: running },
            "memory_read" => ToolGroupKind::Memory { scopes: vec![parsed.get("scope").and_then(|s| s.as_str()).unwrap_or("project").to_string()], is_running: running },
            _ => ToolGroupKind::Generic { name: last.name.clone(), summary: what, is_running: running },
        }
    });
    line.kind = if line.tool_calls.iter().any(|call| call.state(false) == CallState::Failed) { LineKind::ToolError } else { LineKind::Tool };
    if let Some(summary) = line_summary(line, false) {
        line.text = summary.row(usize::MAX, false);
    }
}

impl ChatView {
    pub fn tool_started(&mut self, name: &str, args_json: &str, diff: Option<String>) {
        self.streaming = None;
        let started = self.reasoning_start.take();
        if let Some(i) = self.streaming_reasoning.take() {
            self.lines[i].reasoning_secs.get_or_insert_with(|| crate::thought_secs(started));
        }
        let (added, deleted, diff) = diff.map_or((0, 0, None), |text| {
            let (added, deleted, kept) = crate::stored_change(&text);
            (added, deleted, Some(kept))
        });
        let call = ToolCallRecord {
            name: name.to_string(), args_json: args_json.to_string(), result: None,
            is_error: false, is_running: true, added, deleted, diff, elapsed_ms: None,
        };
        let fold = is_explore(name) && self.lines.last().is_some_and(|line| {
            line.kind == LineKind::Tool && matches!(&line.tool_group, Some(ToolGroupKind::Explore { .. }))
                && !line.tool_calls.is_empty()
                && line.tool_calls.iter().all(|call| is_explore(&call.name) && !call.is_running && !has_problem(call))
        });
        if fold {
            let i = self.lines.len() - 1;
            self.lines[i].tool_calls.push(call);
            sync_line(&mut self.lines[i]);
            self.invalidate_tool_cache(i);
            self.needs_reprint = true;
        } else {
            let mut line = ChatLine::new(LineKind::Tool, "");
            line.tool_calls.push(call);
            sync_line(&mut line);
            self.lines.push(line);
        }
        self.open_tool = Some(self.lines.len() - 1);
    }

    pub(crate) fn tool_finished(&mut self, is_error: bool, result: Option<&str>) {
        self.streaming = None;
        self.streaming_reasoning = None;
        let Some(i) = self.open_tool.take() else { return };
        let Some(call) = self.lines[i].tool_calls.last_mut() else { return };
        call.is_running = false;
        call.is_error = is_error;
        call.result = result.map(str::to_string);
        // Its state is not known when a read starts. If it fails or is
        // refused, detach it from the successful reads immediately above it.
        if has_problem(call) && self.lines[i].tool_calls.len() > 1 {
            let call = self.lines[i].tool_calls.pop().expect("the finishing call");
            let mut separate = ChatLine::new(LineKind::Tool, "");
            separate.is_expanded = self.lines[i].is_expanded;
            separate.tool_calls.push(call);
            sync_line(&mut separate);
            self.insert_line(i + 1, separate);
            self.needs_reprint = true;
        }
        sync_line(&mut self.lines[i]);
        self.invalidate_tool_cache(i);
    }

    /// A caller can attach a measured execution time without starting a
    /// clock at ToolStarted, which may include an approval wait.
    pub fn set_last_tool_elapsed_ms(&mut self, elapsed_ms: u64) -> bool {
        let Some(i) = self.lines.iter().rposition(|line| !line.tool_calls.is_empty()) else { return false };
        let Some(call) = self.lines[i].tool_calls.last_mut() else { return false };
        call.elapsed_ms = Some(elapsed_ms);
        sync_line(&mut self.lines[i]);
        self.invalidate_tool_cache(i);
        true
    }

    fn invalidate_tool_cache(&self, i: usize) {
        let mut cache = self.settled_cache.lock();
        if cache.boundary > i {
            *cache = SettledRenderCache::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(name: &str, args: &str) -> ChatView {
        let mut chat = ChatView::default();
        chat.tool_started(name, args, None);
        chat
    }

    fn text_of(chat: &ChatView) -> String {
        strip_ansi(&chat.lines.last().expect("a line was opened").text)
    }

    #[test]
    fn sizes_use_diff_colours_without_colouring_a_paths_hyphens() {
        let out = colored_size("my-file.rs +3 -1");
        assert!(out.contains(&format!("{ADDED_COLOUR}+3{OFF}")));
        assert!(out.contains(&format!("{REMOVED_COLOUR}-1{OFF}")));
        assert_eq!(strip_ansi(&out), "my-file.rs +3 -1");
        assert_eq!(colored_size("c++ -std=c++20"), "c++ -std=c++20");
    }

    #[test]
    fn a_compact_row_states_the_action_exact_target_and_known_count_or_exit() {
        let mut read = started("read_file", r#"{"header":"Inspect settings","file_path":"src/config.rs"}"#);
        assert_eq!(text_of(&read), "  › ◦ Reading src/config.rs");
        let content = (1..=120).map(|i| format!("{i:>6}\tline")).collect::<Vec<_>>().join("\n");
        read.tool_finished(false, Some(&content));
        assert_eq!(text_of(&read), "  › √ Read src/config.rs · 120 lines");
        let mut shell = started("run_shell", r#"{"cmd":"cargo test --features exact"}"#);
        shell.tool_finished(false, Some("exit code: 0\noutput:\nall passed"));
        assert_eq!(text_of(&shell), "  › √ Ran cargo test --features exact · exit 0");
        assert!(!text_of(&shell).ends_with('s'), "no elapsed time was measured");
        let mut edit = ChatView::default();
        edit.tool_started("edit_file", r#"{"path":"src/config.rs","edits":[]}"#, Some("--- a\n+++ b\n-old\n+new\n+extra".into()));
        edit.tool_finished(false, Some("applied"));
        assert_eq!(text_of(&edit), "  › √ Edited src/config.rs · +2 -1");
    }

    #[test]
    fn a_failure_and_a_refusal_have_different_states_and_do_not_claim_success() {
        let mut failed = started("read_file", r#"{"path":"a.rs"}"#);
        failed.tool_finished(true, Some("error: read: no such file"));
        assert!(text_of(&failed).starts_with("  › × Failed to read a.rs · read: no such file"));
        assert_eq!(failed.lines[0].kind, LineKind::ToolError);
        for result in [flashagent_core::permissions::DECLINED, flashagent_core::STOPPED_RESULT] {
            let mut denied = started("run_shell", r#"{"command":"cargo test"}"#);
            denied.tool_finished(true, Some(result));
            let text = text_of(&denied);
            assert!(text.starts_with("  › ○ Not run: cargo test · "), "{text}");
            assert!(!text.contains("Failed") && !text.contains("Ran"));
            assert_eq!(denied.lines[0].kind, LineKind::Tool);
        }
    }

    #[test]
    fn only_adjacent_reads_and_searches_group_and_failed_members_are_detached() {
        let mut chat = started("read_file", r#"{"path":"a.rs"}"#);
        chat.tool_finished(false, Some("     1\tfirst"));
        chat.tool_started("grep", r#"{"pattern":"needle","glob":"src/*.rs"}"#, None);
        chat.tool_finished(false, Some("src/a.rs:1:needle"));
        assert_eq!(chat.lines.len(), 1);
        assert_eq!(chat.lines[0].tool_calls.len(), 2);
        assert!(text_of(&chat).contains("Explored 1 file, 1 search"));
        assert!(text_of(&chat).contains("a.rs") && text_of(&chat).contains("\"needle\" [src/*.rs]"));
        chat.tool_started("read_file", r#"{"path":"missing.rs"}"#, None);
        chat.tool_finished(true, Some("error: no such file"));
        assert_eq!(chat.lines.len(), 2, "a failure is never buried in a successful group");
        assert_eq!(chat.lines[0].tool_calls.len(), 2);
        assert_eq!(chat.lines[1].kind, LineKind::ToolError);
        chat.tool_started("read_file", r#"{"path":"b.rs"}"#, None);
        chat.tool_finished(false, Some("second"));
        assert_eq!(chat.lines.len(), 3, "a new read does not group across an error");
        chat.push_line(LineKind::Assistant, "an intervening answer");
        chat.tool_started("grep", r#"{"pattern":"next"}"#, None);
        chat.tool_finished(false, Some("no matches"));
        assert_eq!(chat.lines.len(), 5, "only adjacent reads/searches group");
    }

    #[test]
    fn commands_changes_subagents_and_memory_calls_each_keep_their_own_rows() {
        for (name, args) in [
            ("run_shell", r#"{"command":"pwd"}"#),
            ("edit_file", r#"{"path":"src/a.rs","edits":[]}"#),
            ("spawn_agent", r#"{"task":"inspect"}"#),
            ("memory_read", r#"{"scope":"project"}"#),
        ] {
            let mut chat = started(name, args);
            chat.tool_finished(false, Some("ok"));
            chat.tool_started(name, args, None);
            chat.tool_finished(false, Some("ok"));
            assert_eq!(chat.lines.len(), 2, "{name} must stay separate");
            assert!(chat.lines.iter().all(|line| line.tool_calls.len() == 1));
        }
    }

    #[test]
    fn a_declined_read_and_a_partly_failed_batch_are_not_grouped() {
        let mut chat = started("read_file", r#"{"path":"a.rs"}"#);
        chat.tool_finished(false, Some("a"));
        chat.tool_started("read_file", r#"{"path":"b.rs"}"#, None);
        chat.tool_finished(true, Some(flashagent_core::permissions::DECLINED));
        assert_eq!(chat.lines.len(), 2);
        chat.tool_started("read_file", r#"{"paths":["c.rs","missing.rs"]}"#, None);
        chat.tool_finished(false, Some("=== c.rs ===\n     1\tc\n=== missing.rs ===\nerror: no such file"));
        chat.tool_started("read_file", r#"{"path":"d.rs"}"#, None);
        chat.tool_finished(false, Some("d"));
        assert_eq!(chat.lines.len(), 4, "partial batch errors stop grouping too");
    }

    #[test]
    fn an_unknown_tool_is_readable_and_model_headers_never_hide_the_actual_target() {
        let chat = started("mcp__sqlite__query", r#"{"header":"Look up a value","query":"select 1"}"#);
        assert_eq!(text_of(&chat), "  › ◦ Running mcp__sqlite__query select 1");
        let chat = started("edit_file", r#"{"header":"Improve things","path":"nested/src/config.rs","edits":[]}"#);
        assert!(text_of(&chat).contains("Editing nested/src/config.rs"));
        assert!(!text_of(&chat).contains("Improve things"), "intent is in the expanded body, not instead of facts");
    }

    #[test]
    fn a_compact_error_stays_one_row_and_never_splits_unicode_or_overflows() {
        let mut chat = started("read_file", r#"{"path":"日本語/非常に長い設定ファイル.rs"}"#);
        chat.tool_finished(true, Some(&format!("error: {}", "日".repeat(80))));
        for width in [0, 1, 8, 12, 24, 40, 80] {
            let row = compact_row(&chat.lines[0], false, width);
            assert!(visible_width(&row) <= width, "{width}: {row:?}");
            assert!(!row.contains('\n'));
        }
        let long = failure_reason(&"日".repeat(80)).unwrap();
        assert!(visible_width(&long) <= 72 && long.ends_with('…'));
    }
}
