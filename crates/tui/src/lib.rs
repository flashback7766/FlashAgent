//! flashagent-tui: terminal client. The testable core lives here —
//! [`ChatView`] (event stream → renderable lines), the approval card and
//! [`TuiGate`] (an [`ApprovalGate`] answered from the keyboard). The binary in
//! `main.rs` is a thin crossterm shell over this.

use std::sync::Arc;

use async_trait::async_trait;
use flashagent_core::{ApprovalGate, ApprovalRequest, Decision, DoneReason, LoopEvent};
use parking_lot::Mutex;
use unicode_width::UnicodeWidthChar;

pub mod autocomplete;
pub mod backend_error;
pub mod clipboard;
pub mod context_modal;
pub mod goal;
pub mod mcp_view;
pub mod prefill;
pub mod sampling;
pub mod select;
pub mod settings;
pub mod startup;
pub mod tips;
pub mod image_cost;
pub mod memory_view;
pub mod whatsnew;
pub mod wizard;
pub use autocomplete::{AutocompleteCategory, AutocompleteItem, AutocompletePopup};
pub use context_modal::ContextModal;
pub use mcp_view::{McpModal, McpModalAction, McpViewTab};
pub use prefill::{BucketStats, ContextBucket, ModelPrefillProfile, PrefillTracker};
pub use sampling::{SamplingAction, SamplingView};
pub use select::{ConfirmSelect, SelectItem, SelectMenu};
pub use settings::{SettingsAction, SettingsView};
pub use startup::{StartupAction, TrustScreen, TrustScreenMode};
pub use tips::{split_tip_at_word_boundary, TipAnimator};
pub use wizard::{run_wizard, run_wizard_channel, SetupWizard};
pub use ReasoningExpansion as VerboseMode;

use crossterm::event::{KeyCode, KeyModifiers, MouseEvent};
use flashagent_llm::{ChatMessage, ServerDiscovery};

/// Events dispatched to the TUI event loop.
#[derive(Debug)]
pub enum UiEvent {
    /// A loop event, tagged with the turn that produced it.
    Loop { turn_id: u64, event: LoopEvent },
    Key(KeyCode, KeyModifiers),
    Paste(String),
    Mouse(MouseEvent),
    Resize(u16, u16),
    /// A turn ended. `turn_id` lets the UI drop results of a turn it has
    /// already given up on (a hard-aborted cancel).
    Finished {
        turn_id: u64,
        /// On failure: the error text and the history up to the failure.
        result: Result<(Vec<ChatMessage>, DoneReason), (String, Vec<ChatMessage>)>,
    },
    ServerDiscovered(ServerDiscovery),
    /// Outcome of the Settings → "Run Tool Test" probe.
    ToolTestResult(String),
    /// What a picture costs this model, measured against the server.
    ImageCost { model: String, per_pixel: f32, fixed: f32 },
    BackgroundRecap {
        turn_id: u64,
        recap: String,
        suggestion: Option<String>,
    },
}

/// Kind of a rendered chat line — drives terminal colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// What the user typed.
    User,
    /// Assistant text.
    Assistant,
    /// Reasoning (rendered dim).
    Reasoning,
    /// Tool activity.
    Tool,
    /// Failed tool call.
    ToolError,
    /// Diff content.
    Diff,
    /// System/status info.
    System,
}

/// Category of collapsed tool execution for aggregation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolGroupKind {
    Command { count: usize, last_cmd: String, is_running: bool },
    Explore { files: usize, searches: usize, last_target: String, is_running: bool },
    Edit { path: String, added: usize, deleted: usize, is_running: bool },
    Subagent { count: usize, last_task: String, is_running: bool },
    Memory { scopes: Vec<String>, is_running: bool },
    Generic { name: String, summary: String, is_running: bool },
    Custom { run_text: String, done_text: String, fail_text: String, is_running: bool },
}

pub mod tool_views;

/// Single tool execution record tracked within a `ChatLine` (used for multi-tool group expansion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRecord {
    pub name: String,
    pub args_json: String,
    pub result: Option<String>,
    pub is_error: bool,
    pub is_running: bool,
}

/// One rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    /// Line kind (color).
    pub kind: LineKind,
    /// Text without prefix.
    pub text: String,
    /// Optional raw details (arguments JSON) for verbose expansion.
    pub details: Option<String>,
    /// Optional tool group tracking.
    pub tool_group: Option<ToolGroupKind>,
    /// Elapsed seconds for reasoning blocks.
    pub reasoning_secs: Option<u64>,
    /// Raw output/result length in characters for token accounting.
    pub tool_result_len: usize,
    /// Captured tool name (e.g. "run_shell", "read_file", "edit_file", etc.).
    pub tool_name: Option<String>,
    /// Captured tool output/result content.
    pub tool_result: Option<String>,
    /// Optional per-card expansion override.
    pub is_expanded: Option<bool>,
    /// Per-tool execution records for multi-tool group expansion.
    pub tool_calls: Vec<ToolCallRecord>,
}

impl ChatLine {
    pub fn new(kind: LineKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            details: None,
            tool_group: None,
            reasoning_secs: None,
            tool_result_len: 0,
            tool_name: None,
            tool_result: None,
            is_expanded: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn with_details(kind: LineKind, text: impl Into<String>, details: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            details: Some(details.into()),
            tool_group: None,
            reasoning_secs: None,
            tool_result_len: 0,
            tool_name: None,
            tool_result: None,
            is_expanded: None,
            tool_calls: Vec::new(),
        }
    }
}

#[derive(Default, Clone)]
struct SettledRenderCache {
    lines: Vec<RenderLine>,
    boundary: usize,
    width: usize,
    expansion: ReasoningExpansion,
}

/// Accumulates the conversation from [`LoopEvent`]s and renders it to lines.
#[derive(Default)]
pub struct ChatView {
    lines: Vec<ChatLine>,
    /// Index of the assistant content line being streamed, if any.
    streaming: Option<usize>,
    /// Index of the reasoning line being streamed, if any.
    streaming_reasoning: Option<usize>,
    /// Start time of the active reasoning block.
    reasoning_start: Option<std::time::Instant>,
    /// Index of a tool line started but not finished (renders with a spinner).
    open_tool: Option<usize>,
    /// Number of lines in the welcome card banner.
    card_len: usize,
    /// In-place update flag requesting full reprint of settled lines.
    needs_reprint: bool,
    /// Cached render of settled lines to avoid expensive markdown & wrap re-parsing every frame.
    settled_cache: Mutex<SettledRenderCache>,
    /// Language of the conversation, for the labels this view writes itself.
    language: String,
}

/// Wrap styled text to `width` visible characters, preserving active ANSI escape sequences
/// across line wraps and appending reset codes at line endings.
pub fn wrap_styled(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut out = Vec::new();

    for line in text.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        if visible_width(line) <= width {
            out.push(line.to_string());
            continue;
        }

        let mut cur = String::new();
        let mut cur_vis = 0;
        let mut active_style: Option<String> = None;

        for word in line.split(' ') {
            let w_vis = visible_width(word);
            let need_space = !cur.is_empty();
            let space_vis = if need_space { 1 } else { 0 };

            if !cur.is_empty() && cur_vis + space_vis + w_vis > width {
                if active_style.is_some() {
                    cur.push_str("\x1b[0m");
                }
                out.push(std::mem::take(&mut cur));
                cur_vis = 0;
                if let Some(ref style) = active_style {
                    cur.push_str(style);
                }
            }

            if !cur.is_empty() {
                cur.push(' ');
                cur_vis += 1;
            }
            cur.push_str(word);
            cur_vis += w_vis;

            // Track active ANSI style
            let b = word.as_bytes();
            let mut j = 0;
            while j < b.len() {
                if b[j] == 0x1b && j + 1 < b.len() && b[j + 1] == b'[' {
                    let start = j;
                    j += 2;
                    while j < b.len() && !('@'..='~').contains(&(b[j] as char)) {
                        j += 1;
                    }
                    if j < b.len() {
                        let seq = &word[start..=j];
                        if seq == "\x1b[0m" || seq == "\x1b[m" {
                            active_style = None;
                        } else {
                            active_style = Some(seq.to_string());
                        }
                        j += 1;
                    }
                } else {
                    j += 1;
                }
            }
        }

        if !cur.is_empty() {
            if active_style.is_some() {
                cur.push_str("\x1b[0m");
            }
            out.push(cur);
        }
    }

    out
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    wrap_styled(text, width)
}

/// "Add the null check" → "add the null check", so it can follow "Failed to".
/// Only the first character, and only when it is not part of an identifier
/// like `MEMORY.md` that would look wrong in lower case.
pub fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest: String = chars.collect();
    let second_is_upper = rest.chars().next().is_some_and(|c| c.is_uppercase());
    if second_is_upper {
        return text.to_string();
    }
    first.to_lowercase().collect::<String>() + &rest
}

pub fn format_cmd(cmd: &str) -> String {
    let clean = cmd.trim();
    let first_line = clean.lines().next().unwrap_or(clean).trim();
    // Models hand back absolute paths, and a line reading "Read
    // /tmp/claude-1000/-home-flashback/.../audit_proj/main.rs" is all
    // prefix and no information: everything up to the working directory
    // is where the user already is.
    let storage;
    let first_line = match std::env::current_dir() {
        Ok(cwd) => {
            let cwd = cwd.to_string_lossy().to_string();
            if !cwd.is_empty() && first_line.contains(&cwd) {
                storage = first_line.replace(&format!("{cwd}/"), "").replace(&cwd, ".");
                storage.as_str()
            } else {
                first_line
            }
        }
        Err(_) => first_line,
    };
    let max_len = 70;
    if first_line.chars().count() > max_len {
        let truncated: String = first_line.chars().take(max_len - 3).collect();
        format!("{truncated}...")
    } else {
        first_line.to_string()
    }
}

pub fn format_explore(is_running: bool, files: usize, searches: usize, last_target: &str) -> String {
    let verb = if is_running { "Exploring" } else { "Explored" };
    let chevron = "\x1b[38;2;120;125;140m›\x1b[0m";
    if files > 0 && searches > 0 {
        let f_str = if files == 1 { "1 file".to_string() } else { format!("{files} files") };
        let s_str = if searches == 1 { "1 search".to_string() } else { format!("{searches} searches") };
        format!("  \x1b[38;2;160;165;180m{verb} {f_str}, {s_str}\x1b[0m {chevron}")
    } else if files > 1 {
        format!("  \x1b[38;2;160;165;180m{verb} {files} files\x1b[0m {chevron}")
    } else if searches > 1 {
        format!("  \x1b[38;2;160;165;180m{verb} {searches} searches\x1b[0m {chevron}")
    } else if files == 1 {
        let v = if is_running { "Reading" } else { "Read" };
        format!("  \x1b[38;2;160;165;180m{v}\x1b[0m \x1b[38;2;225;230;240m{last_target}\x1b[0m {chevron}")
    } else if searches == 1 {
        let v = if is_running { "Searching" } else { "Searched" };
        format!("  \x1b[38;2;160;165;180m{v}\x1b[0m \x1b[38;2;225;230;240m{last_target}\x1b[0m {chevron}")
    } else {
        format!("  \x1b[38;2;160;165;180m{verb} {last_target}\x1b[0m {chevron}")
    }
}


/// What a call names, in a few words: the file and the size of the change for
/// a write, the target for a read, the command for a shell call.
///
/// Shown next to the model's own header, because the header says what it
/// meant to do and this says what it asked for.
pub fn call_facts(name: &str, parsed: &serde_json::Value) -> Option<String> {
    let path_of = |key: &str| -> Option<String> {
        let raw = parsed.get(key)?.as_str()?;
        let base = std::path::Path::new(raw)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(raw);
        Some(base.to_string())
    };
    match name {
        "edit_file" => {
            let (mut added, mut deleted) = (0usize, 0usize);
            for ed in parsed.get("edits")?.as_array()? {
                deleted += ed.get("old_string").and_then(|s| s.as_str()).unwrap_or("").lines().count();
                added += ed.get("new_string").and_then(|s| s.as_str()).unwrap_or("").lines().count();
            }
            Some(format!("{} +{added} -{deleted}", path_of("path")?))
        }
        "write_file" => {
            let lines = parsed.get("content").and_then(|s| s.as_str()).unwrap_or("").lines().count().max(1);
            Some(format!("{} +{lines}", path_of("path")?))
        }
        "patch_file" => {
            let patch = parsed.get("patch").and_then(|s| s.as_str()).unwrap_or("");
            let added = patch.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).count();
            let deleted = patch.lines().filter(|l| l.starts_with('-') && !l.starts_with("---")).count();
            Some(format!("{} +{added} -{deleted}", path_of("path")?))
        }
        "read_file" | "outline_file" => path_of("path"),
        "run_shell" => parsed.get("command").and_then(|c| c.as_str()).map(format_cmd),
        "grep" | "glob" => parsed
            .get("pattern")
            .or_else(|| parsed.get("query"))
            .and_then(|p| p.as_str())
            .map(|p| format!("\"{}\"", format_cmd(p))),
        "list_dir" => Some(path_of("path").unwrap_or_else(|| "the project".to_string())),
        _ => None,
    }
}

impl ChatView {
    /// Index of the first live (still-changing) line; `lines.len()` when
    /// everything is settled. Everything before it can be printed to the
    /// terminal scrollback once and never touched again (print-and-forget).
    pub fn settled_boundary(&self) -> usize {
        // min(): all live lines stay repaintable; taking the first Some() would
        // freeze an earlier line that still receives appends.
        self.streaming
            .into_iter()
            .chain(self.streaming_reasoning)
            .chain(self.open_tool)
            .min()
            .unwrap_or(self.lines.len())
    }

    /// Total chars across all lines (crude activity/token gauge).
    pub fn total_chars(&self) -> usize {
        self.lines.iter().map(|l| l.text.len()).sum()
    }

    /// Get raw character counts by category: (user, assistant, reasoning, tools).
    pub fn raw_content_chars(&self) -> (usize, usize, usize, usize) {
        let mut user = 0;
        let mut assistant = 0;
        let mut reasoning = 0;
        let mut tools = 0;
        for l in &self.lines {
            match l.kind {
                LineKind::User => user += l.text.len(),
                LineKind::Assistant => assistant += l.text.len(),
                LineKind::Reasoning => reasoning += l.text.len(),
                LineKind::Tool | LineKind::ToolError => {
                    if let Some(ref d) = l.details {
                        tools += d.len();
                    }
                    tools += l.tool_result_len;
                    tools += l.text.len();
                }
                _ => {}
            }
        }
        (user, assistant, reasoning, tools)
    }

    /// Clear all lines and streaming state.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.streaming = None;
        self.streaming_reasoning = None;
        self.reasoning_start = None;
        self.open_tool = None;
        self.card_len = 0;
        self.needs_reprint = false;
        *self.settled_cache.lock() = SettledRenderCache::default();
    }

    /// Returns the full text of the most recent assistant response, if any.
    pub fn last_assistant_text(&self) -> Option<String> {
        let mut lines = Vec::new();
        for l in self.lines.iter().rev() {
            if l.kind == LineKind::Assistant {
                lines.push(l.text.as_str());
            } else if !lines.is_empty() {
                break;
            }
        }
        if lines.is_empty() {
            None
        } else {
            lines.reverse();
            Some(lines.join("\n"))
        }
    }

    /// Take and reset the reprint flag.
    pub fn take_needs_reprint(&mut self) -> bool {
        std::mem::take(&mut self.needs_reprint)
    }

    /// Number of user lines (prompts and steering directives) so far.
    /// Tell the view which language the conversation is in, so the labels it
    /// writes itself do not arrive in English in a Russian chat.
    pub fn set_language(&mut self, language: &str) {
        if self.language != language {
            self.language = language.to_string();
            *self.settled_cache.lock() = SettledRenderCache::default();
            self.needs_reprint = true;
        }
    }

    pub fn user_turn_count(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == LineKind::User).count()
    }

    /// Check if any user message has been recorded.
    pub fn has_user_message(&self) -> bool {
        self.lines.iter().any(|l| l.kind == LineKind::User)
    }

    /// Truncate lines after the last user message, removing the last assistant response,
    /// reasoning blocks, tool executions, and recap from the chat view.
    /// Returns true if a user message was found and truncation occurred.
    pub fn truncate_to_last_user(&mut self) -> bool {
        if let Some(pos) = self.lines.iter().rposition(|l| l.kind == LineKind::User) {
            self.lines.truncate(pos + 1);
            self.streaming = None;
            self.streaming_reasoning = None;
            self.reasoning_start = None;
            self.open_tool = None;
            *self.settled_cache.lock() = SettledRenderCache::default();
            self.needs_reprint = true;
            true
        } else {
            false
        }
    }

    /// Update an existing system message matching `prefix`, or push a new one if not found.
    pub fn update_or_push_system(&mut self, prefix: &str, text: &str) {
        self.settled_cache.lock().boundary = 0;
        if let Some(pos) = self.lines.iter().rposition(|l| l.kind == LineKind::System && strip_ansi(&l.text).trim_start().starts_with(prefix)) {
            self.lines[pos].text = text.to_string();
        } else {
            self.streaming = None;
            self.streaming_reasoning = None;
            self.lines.push(ChatLine::new(LineKind::System, text.to_string()));
        }
    }

    /// Update an existing system message matching `prefix` in the current turn (after the latest User message),
    /// or append a new system message at the end of the chat.
    /// This prevents turn N's system annotations (like turn recaps) from overwriting earlier turns' recaps.
    pub fn update_or_push_turn_system(&mut self, prefix: &str, text: &str) {
        self.settled_cache.lock().boundary = 0;
        let last_user_idx = self.lines.iter().rposition(|l| l.kind == LineKind::User).unwrap_or(0);
        let turn_lines = &mut self.lines[last_user_idx..];
        if let Some(rel_pos) = turn_lines.iter().rposition(|l| l.kind == LineKind::System && strip_ansi(&l.text).trim_start().starts_with(prefix)) {
            turn_lines[rel_pos].text = text.to_string();
        } else {
            self.streaming = None;
            self.streaming_reasoning = None;
            self.lines.push(ChatLine::new(LineKind::System, text.to_string()));
        }
    }

    /// Attaches or updates a recap for a specific turn ID (1-indexed based on user turns).
    /// If subsequent turns already exist, the recap is placed at the end of that specific turn,
    /// right before the next user turn begins.
    pub fn attach_turn_recap(&mut self, turn_id: u64, recap_text: &str) {
        self.settled_cache.lock().boundary = 0;
        let user_indices: Vec<usize> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == LineKind::User)
            .map(|(i, _)| i)
            .collect();

        if user_indices.is_empty() {
            self.update_or_push_turn_system("recap:", recap_text);
            return;
        }

        let target_turn_idx = (turn_id as usize).saturating_sub(1);
        if target_turn_idx >= user_indices.len() {
            self.update_or_push_turn_system("recap:", recap_text);
            return;
        }

        let start_user = user_indices[target_turn_idx];
        let end_idx = if target_turn_idx + 1 < user_indices.len() {
            user_indices[target_turn_idx + 1]
        } else {
            self.lines.len()
        };

        let turn_slice = &mut self.lines[start_user..end_idx];
        if let Some(rel_pos) = turn_slice
            .iter()
            .rposition(|l| l.kind == LineKind::System && strip_ansi(&l.text).trim_start().starts_with("recap:"))
        {
            turn_slice[rel_pos].text = recap_text.to_string();
        } else {
            self.insert_line(end_idx, ChatLine::new(LineKind::System, recap_text.to_string()));
        }
    }

    /// Insert a line mid-conversation, keeping the live-line indices pointing
    /// at the same lines. A recap for an earlier turn can land while the
    /// current turn is still streaming or running a tool.
    fn insert_line(&mut self, at: usize, line: ChatLine) {
        self.lines.insert(at, line);
        for idx in [&mut self.streaming, &mut self.streaming_reasoning, &mut self.open_tool].into_iter().flatten() {
            if *idx >= at {
                *idx += 1;
            }
        }
    }

    /// Replaces the initial welcome card lines with fresh lines, keeping any subsequent notices.
    pub fn update_welcome_card(&mut self, new_card: Vec<RenderLine>) {
        if self.has_user_message() {
            return;
        }
        *self.settled_cache.lock() = SettledRenderCache::default();
        let end_idx = if self.card_len > 0 {
            self.card_len.min(self.lines.len())
        } else {
            self.lines
                .iter()
                .position(|l| l.text.trim_start().starts_with('['))
                .unwrap_or(self.lines.len())
        };
        let remaining_notices: Vec<ChatLine> = self.lines.drain(end_idx..).collect();
        self.lines.clear();
        self.card_len = new_card.len();
        for (kind, text) in new_card {
            self.lines.push(ChatLine::new(kind, text));
        }
        self.lines.extend(remaining_notices);
    }

    /// System/status line (memory loaded, notices).
    pub fn push_system(&mut self, text: &str) {
        self.streaming = None;
        self.streaming_reasoning = None;
        self.lines.push(ChatLine::new(LineKind::System, text.to_string()));
    }

    /// Replace the last system line with its outcome.
    ///
    /// Used for the work that announces itself before it starts —
    /// "Compacting context..." becomes "Context compacted · 12k saved" in
    /// place, rather than leaving both on the screen.
    pub fn replace_last_system(&mut self, text: &str) {
        match self.lines.iter().rposition(|l| l.kind == LineKind::System) {
            Some(i) => {
                self.lines[i].text = text.to_string();
                *self.settled_cache.lock() = SettledRenderCache::default();
                self.needs_reprint = true;
            }
            None => self.push_system(text),
        }
    }


    /// Push a line with explicit kind.
    pub fn push_line(&mut self, kind: LineKind, text: impl Into<String>) {
        self.streaming = None;
        self.streaming_reasoning = None;
        self.lines.push(ChatLine::new(kind, text.into()));
    }

    /// User typed a message. Stored unwrapped; wrapping happens in [`render`].
    pub fn push_user(&mut self, text: &str) {
        self.streaming = None;
        self.lines.push(ChatLine::new(LineKind::User, text.to_string()));
    }

    /// Assistant response message.
    pub fn push_assistant(&mut self, text: &str) {
        self.streaming = None;
        self.streaming_reasoning = None;
        self.lines.push(ChatLine::new(LineKind::Assistant, text.to_string()));
    }

    /// Feed one loop event.
    pub fn on_event(&mut self, ev: &LoopEvent) {
        match ev {
            LoopEvent::TurnDelta(d) => {
                self.open_tool = None;
                match self.streaming {
                    Some(i) => self.lines[i].text.push_str(d),
                    None => {
                        self.lines.push(ChatLine::new(LineKind::Assistant, d.clone()));
                        self.streaming = Some(self.lines.len() - 1);
                    }
                }
            }
            LoopEvent::ReasoningDelta(d) => {
                if self.reasoning_start.is_none() {
                    self.reasoning_start = Some(std::time::Instant::now());
                }
                match self.streaming_reasoning {
                    Some(i) => self.lines[i].text.push_str(d),
                    None => {
                        self.lines.push(ChatLine::new(LineKind::Reasoning, d.clone()));
                        self.streaming_reasoning = Some(self.lines.len() - 1);
                    }
                }
            }
            LoopEvent::ToolStarted { id: _, name, args_json } => {
                self.streaming = None;
                if let Some(i) = self.streaming_reasoning.take() {
                    let secs = self.reasoning_start.take().map(|t| t.elapsed().as_secs()).unwrap_or(1);
                    self.lines[i].reasoning_secs = Some(secs);
                }

                let parsed: serde_json::Value = serde_json::from_str(args_json.trim()).unwrap_or_default();
                let chevron = "\x1b[38;2;120;125;140m›\x1b[0m";

                // The model is asked to say what a call is for. When it did,
                // that sentence is the line — "Add the missing null check to
                // parser.rs" tells you more than "Editing parser.rs" ever
                // could. The expanded card underneath is unchanged, so the
                // facts are still one keypress away.
                let header = parsed
                    .get("header")
                    .and_then(|h| h.as_str())
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(format_cmd);

                if let Some(header) = header {
                    // The header is the model's intent; the suffix is what the
                    // call actually names. "Add the missing null check" reads
                    // well, but only "parser.rs +3 -1" says what happened to
                    // the tree.
                    let facts = call_facts(name, &parsed)
                        // "Read main.rs · main.rs" says it twice. When the
                        // header already names the file, only what it cannot
                        // say is worth adding — the size of the change.
                        .and_then(|f| {
                            let said = |part: &str| header.to_lowercase().contains(&part.to_lowercase());
                            match f.split_once(" +") {
                                Some((file, size)) if said(file) => Some(format!("+{size}")),
                                None if said(&f) => None,
                                _ => Some(f),
                            }
                        })
                        .map(|f| format!(" \x1b[38;2;120;125;140m·\x1b[0m \x1b[38;2;160;165;180m{f}\x1b[0m"))
                        .unwrap_or_default();
                    let run_text = format!("  \x1b[38;2;160;165;180m{header}\x1b[0m{facts} {chevron}");
                    let done_text = run_text.clone();
                    let fail_text = format!(
                        "  \x1b[38;2;230;120;120mFailed to {}\x1b[0m {chevron}",
                        lower_first(&header)
                    );
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "run_shell" {
                    let cmd = parsed.get("command").and_then(|s| s.as_str()).unwrap_or("command").to_string();
                    let mut consolidated = false;
                    if let Some(last_line) = self.lines.last_mut() {
                        if let Some(ToolGroupKind::Command { count, last_cmd, is_running }) = &mut last_line.tool_group {
                            *count += 1;
                            *last_cmd = cmd.clone();
                            *is_running = true;
                            last_line.text = format!("  \x1b[38;2;160;165;180mRunning {count} commands\x1b[0m {chevron}");
                            if let Some(ref mut d) = last_line.details {
                                d.push_str("\n\n---\n\n");
                                d.push_str(args_json);
                            }
                            consolidated = true;
                            self.open_tool = Some(self.lines.len() - 1);
                            self.needs_reprint = true;
                        }
                    }
                    if !consolidated {
                        let text = format!("  \x1b[38;2;160;165;180mRunning\x1b[0m \x1b[38;2;225;230;240m{}\x1b[0m {chevron}", format_cmd(&cmd));
                        let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.clone());
                        line.tool_name = Some(name.clone());
                        line.tool_group = Some(ToolGroupKind::Command { count: 1, last_cmd: cmd, is_running: true });
                        self.lines.push(line);
                        self.open_tool = Some(self.lines.len() - 1);
                    }
                } else if name == "read_file" || name == "list_dir" || name == "glob" || name == "grep" {
                    let is_file = name == "read_file";
                    let target = if is_file {
                        parsed.get("path").and_then(|s| s.as_str()).unwrap_or("file")
                    } else {
                        parsed.get("pattern")
                            .or_else(|| parsed.get("query"))
                            .or_else(|| parsed.get("path"))
                            .and_then(|s| s.as_str())
                            // A listing of the working directory carries no
                            // path and no pattern, and "Searched search" is
                            // not a sentence.
                            .unwrap_or("the project")
                    };
                    // Nor is "Searched .".
                    let target = if matches!(target, "." | "./" | "") { "the project" } else { target };

                    let mut consolidated = false;
                    if let Some(last_line) = self.lines.last_mut() {
                        if let Some(ToolGroupKind::Explore { files, searches, last_target, is_running }) = &mut last_line.tool_group {
                            if is_file { *files += 1; } else { *searches += 1; }
                            *last_target = target.to_string();
                            *is_running = true;
                            last_line.text = format_explore(true, *files, *searches, target);
                            if let Some(ref mut d) = last_line.details {
                                d.push_str("\n\n---\n\n");
                                d.push_str(args_json);
                            }
                            consolidated = true;
                            self.open_tool = Some(self.lines.len() - 1);
                            self.needs_reprint = true;
                        }
                    }
                    if !consolidated {
                        let files = if is_file { 1 } else { 0 };
                        let searches = if is_file { 0 } else { 1 };
                        let text = format_explore(true, files, searches, target);
                        let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.clone());
                        line.tool_name = Some(name.clone());
                        line.tool_group = Some(ToolGroupKind::Explore { files, searches, last_target: target.to_string(), is_running: true });
                        self.lines.push(line);
                        self.open_tool = Some(self.lines.len() - 1);
                    }
                } else if name == "edit_file" || name == "write_file" || name == "patch_file" {
                    let path = parsed.get("path").and_then(|s| s.as_str()).unwrap_or("file").to_string();
                    let basename = std::path::Path::new(&path).file_name().and_then(|s| s.to_str()).unwrap_or(&path);

                    let (added, deleted) = if name == "edit_file" {
                        let mut a = 0;
                        let mut d = 0;
                        if let Some(edits) = parsed.get("edits").and_then(|e| e.as_array()) {
                            for ed in edits {
                                let old_s = ed.get("old_string").and_then(|s| s.as_str()).unwrap_or("");
                                let new_s = ed.get("new_string").and_then(|s| s.as_str()).unwrap_or("");
                                d += old_s.lines().count();
                                a += new_s.lines().count();
                            }
                        }
                        (a, d)
                    } else if name == "write_file" {
                        let content = parsed.get("content").and_then(|s| s.as_str()).unwrap_or("");
                        (content.lines().count().max(1), 0)
                    } else {
                        let patch = parsed.get("patch").and_then(|s| s.as_str()).unwrap_or("");
                        let mut a = 0;
                        let mut d = 0;
                        for l in patch.lines() {
                            if l.starts_with('+') && !l.starts_with("+++") { a += 1; }
                            else if l.starts_with('-') && !l.starts_with("---") { d += 1; }
                        }
                        (a, d)
                    };

                    let text = format!("  \x1b[38;2;160;165;180mEditing\x1b[0m \x1b[1;38;2;225;230;240m{basename}\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Edit { path, added, deleted, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "spawn_agent" {
                    let task = parsed.get("task").and_then(|s| s.as_str()).unwrap_or("task").to_string();
                    let mut consolidated = false;
                    if let Some(last_line) = self.lines.last_mut() {
                        if let Some(ToolGroupKind::Subagent { count, last_task, is_running }) = &mut last_line.tool_group {
                            *count += 1;
                            *last_task = task.clone();
                            *is_running = true;
                            last_line.text = format!("  \x1b[38;2;160;165;180mExplored {count} tasks\x1b[0m {chevron}");
                            if let Some(ref mut d) = last_line.details {
                                d.push_str("\n\n---\n\n");
                                d.push_str(args_json);
                            }
                            consolidated = true;
                            self.open_tool = Some(self.lines.len() - 1);
                            self.needs_reprint = true;
                        }
                    }
                    if !consolidated {
                        let text = format!("  \x1b[38;2;160;165;180mRunning subagent: {}\x1b[0m {chevron}", format_cmd(&task));
                        let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.clone());
                        line.tool_name = Some(name.clone());
                        line.tool_group = Some(ToolGroupKind::Subagent { count: 1, last_task: task, is_running: true });
                        self.lines.push(line);
                        self.open_tool = Some(self.lines.len() - 1);
                    }
                } else if name == "memory_read" {
                    let scope = parsed.get("scope").and_then(|s| s.as_str()).unwrap_or("project");
                    let mut consolidated = false;
                    if let Some(last_line) = self.lines.last_mut() {
                        if let Some(ToolGroupKind::Memory { scopes, is_running }) = &mut last_line.tool_group {
                            if !scopes.contains(&scope.to_string()) {
                                scopes.push(scope.to_string());
                            }
                            *is_running = true;
                            let scopes_str = scopes.join(", ");
                            last_line.text = format!("  \x1b[38;2;160;165;180mReading memory\x1b[0m \x1b[38;2;225;230;240m[{scopes_str}]\x1b[0m {chevron}");
                            if let Some(ref mut d) = last_line.details {
                                d.push_str("\n\n---\n\n");
                                d.push_str(args_json);
                            }
                            consolidated = true;
                            self.open_tool = Some(self.lines.len() - 1);
                            self.needs_reprint = true;
                        }
                    }
                    if !consolidated {
                        let text = format!("  \x1b[38;2;160;165;180mReading memory\x1b[0m \x1b[38;2;225;230;240m[{scope}]\x1b[0m {chevron}");
                        let mut line = ChatLine::with_details(LineKind::Tool, text, args_json.clone());
                        line.tool_name = Some(name.clone());
                        line.tool_group = Some(ToolGroupKind::Memory { scopes: vec![scope.to_string()], is_running: true });
                        self.lines.push(line);
                        self.open_tool = Some(self.lines.len() - 1);
                    }
                } else if name == "memory_create" || name == "memory_update" || name == "memory_remove" {
                    let scope = parsed.get("scope").and_then(|s| s.as_str()).unwrap_or("project");
                    let (verb_run, verb_done, verb_fail) = match name.as_str() {
                        "memory_create" => ("Creating memory", "Created memory", "Failed to create memory"),
                        "memory_update" => ("Updating memory", "Updated memory", "Failed to update memory"),
                        "memory_remove" => ("Removing memory", "Removed memory", "Failed to remove memory"),
                        _ => ("Accessing memory", "Accessed memory", "Failed to access memory"),
                    };
                    let run_text = format!("  \x1b[38;2;160;165;180m{verb_run}\x1b[0m \x1b[38;2;225;230;240m[{scope}]\x1b[0m {chevron}");
                    let done_text = format!("  \x1b[38;2;160;165;180m{verb_done}\x1b[0m \x1b[38;2;225;230;240m[{scope}]\x1b[0m {chevron}");
                    let fail_text = format!("  \x1b[38;2;230;120;120m{verb_fail}\x1b[0m \x1b[38;2;225;230;240m[{scope}]\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "outline_file" {
                    let path = parsed.get("path").and_then(|s| s.as_str()).unwrap_or("file");
                    let p_short = format_cmd(path);
                    let run_text = format!("  \x1b[38;2;160;165;180mOutlining\x1b[0m \x1b[38;2;225;230;240m{p_short}\x1b[0m {chevron}");
                    let done_text = format!("  \x1b[38;2;160;165;180mOutlined\x1b[0m \x1b[38;2;225;230;240m{p_short}\x1b[0m {chevron}");
                    let fail_text = format!("  \x1b[38;2;230;120;120mFailed to outline\x1b[0m \x1b[38;2;225;230;240m{p_short}\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "web_search" {
                    let query = parsed.get("query").and_then(|s| s.as_str()).unwrap_or("web");
                    let q_short = format_cmd(query);
                    let run_text = format!("  \x1b[38;2;160;165;180mSearching web:\x1b[0m \x1b[38;2;225;230;240m\"{q_short}\"\x1b[0m {chevron}");
                    let done_text = format!("  \x1b[38;2;160;165;180mSearched web:\x1b[0m \x1b[38;2;225;230;240m\"{q_short}\"\x1b[0m {chevron}");
                    let fail_text = format!("  \x1b[38;2;230;120;120mWeb search failed:\x1b[0m \x1b[38;2;225;230;240m\"{q_short}\"\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "web_fetch" {
                    let url = parsed.get("url").and_then(|s| s.as_str()).unwrap_or("url");
                    let u_short = format_cmd(url);
                    let run_text = format!("  \x1b[38;2;160;165;180mFetching\x1b[0m \x1b[38;2;225;230;240m{u_short}\x1b[0m {chevron}");
                    let done_text = format!("  \x1b[38;2;160;165;180mFetched\x1b[0m \x1b[38;2;225;230;240m{u_short}\x1b[0m {chevron}");
                    let fail_text = format!("  \x1b[38;2;230;120;120mFailed to fetch\x1b[0m \x1b[38;2;225;230;240m{u_short}\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "git_status" {
                    let run_text = format!("  \x1b[38;2;160;165;180mChecking git status\x1b[0m {chevron}");
                    let done_text = format!("  \x1b[38;2;160;165;180mChecked git status\x1b[0m {chevron}");
                    let fail_text = format!("  \x1b[38;2;230;120;120mFailed to check git status\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "git_diff" {
                    let staged = parsed.get("staged").and_then(|v| v.as_bool()).unwrap_or(false);
                    let diff_type = if staged { "staged git diff" } else { "git diff" };
                    let run_text = format!("  \x1b[38;2;160;165;180mChecking {diff_type}\x1b[0m {chevron}");
                    let done_text = format!("  \x1b[38;2;160;165;180mChecked {diff_type}\x1b[0m {chevron}");
                    let fail_text = format!("  \x1b[38;2;230;120;120mFailed to check {diff_type}\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "env_info" {
                    let run_text = format!("  \x1b[38;2;160;165;180mInspecting environment\x1b[0m {chevron}");
                    let done_text = format!("  \x1b[38;2;160;165;180mInspected environment\x1b[0m {chevron}");
                    let fail_text = format!("  \x1b[38;2;230;120;120mFailed to inspect environment\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else if name == "ask_user" {
                    let q = parsed.get("question").or_else(|| parsed.get("prompt")).and_then(|s| s.as_str()).unwrap_or("confirmation");
                    let q_short = format_cmd(q);
                    let run_text = format!("  \x1b[38;2;160;165;180mAsking user:\x1b[0m \x1b[38;2;225;230;240m{q_short}\x1b[0m {chevron}");
                    let done_text = format!("  \x1b[38;2;160;165;180mAsked user:\x1b[0m \x1b[38;2;225;230;240m{q_short}\x1b[0m {chevron}");
                    let fail_text = format!("  \x1b[38;2;230;120;120mCancelled question\x1b[0m {chevron}");
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                } else {
                    let primary_arg = ["path", "file", "target", "pattern", "query", "command", "task", "scope", "url", "name", "key"]
                        .iter()
                        .find_map(|&k| parsed.get(k).and_then(|v| v.as_str()));
                    let (run_text, done_text, fail_text) = if let Some(arg) = primary_arg {
                        let short_arg = format_cmd(arg);
                        (
                            format!("  \x1b[38;2;160;165;180mRunning\x1b[0m \x1b[38;2;225;230;240m{name} [{short_arg}]\x1b[0m {chevron}"),
                            format!("  \x1b[38;2;160;165;180m{name} [{short_arg}] finished\x1b[0m {chevron}"),
                            format!("  \x1b[38;2;230;120;120m{name} [{short_arg}] failed\x1b[0m {chevron}"),
                        )
                    } else {
                        (
                            format!("  \x1b[38;2;160;165;180mRunning\x1b[0m \x1b[38;2;225;230;240m{name}\x1b[0m {chevron}"),
                            format!("  \x1b[38;2;160;165;180m{name} finished\x1b[0m {chevron}"),
                            format!("  \x1b[38;2;230;120;120m{name} failed\x1b[0m {chevron}"),
                        )
                    };
                    let mut line = ChatLine::with_details(LineKind::Tool, run_text.clone(), args_json.clone());
                    line.tool_name = Some(name.clone());
                    line.tool_group = Some(ToolGroupKind::Custom { run_text, done_text, fail_text, is_running: true });
                    self.lines.push(line);
                    self.open_tool = Some(self.lines.len() - 1);
                }
                if let Some(last_line) = self.lines.last_mut() {
                    last_line.tool_calls.push(ToolCallRecord {
                        name: name.clone(),
                        args_json: args_json.clone(),
                        result: None,
                        is_error: false,
                        is_running: true,
                    });
                }
            }
            LoopEvent::ToolFinished { is_error, result_len, result, .. } => {
                self.streaming = None;
                self.streaming_reasoning = None;
                if let Some(i) = self.open_tool.take() {
                    self.lines[i].tool_result_len += *result_len;
                    if let Some(res) = result {
                        match &mut self.lines[i].tool_result {
                            Some(existing) => {
                                existing.push_str("\n\n---\n\n");
                                existing.push_str(res);
                            }
                            None => {
                                self.lines[i].tool_result = Some(res.clone());
                            }
                        }
                    }
                    if let Some(last_call) = self.lines[i].tool_calls.last_mut() {
                        last_call.is_running = false;
                        last_call.is_error = *is_error;
                        last_call.result = result.clone();
                    }
                    let chevron = "\x1b[38;2;120;125;140m›\x1b[0m";
                    if let Some(mut group) = self.lines[i].tool_group.take() {
                        let (kind, text) = match &mut group {
                            ToolGroupKind::Command { count, last_cmd, is_running } => {
                                *is_running = false;
                                let k = if *is_error { LineKind::ToolError } else { LineKind::Tool };
                                let t = if *is_error {
                                    if *count == 1 {
                                        format!("  \x1b[38;2;230;120;120mFailed\x1b[0m \x1b[38;2;225;230;240m{}\x1b[0m {chevron}", format_cmd(last_cmd))
                                    } else {
                                        format!("  \x1b[38;2;230;120;120mFailed {count} commands\x1b[0m {chevron}")
                                    }
                                } else if *count == 1 {
                                    format!("  \x1b[38;2;160;165;180mRan\x1b[0m \x1b[38;2;225;230;240m{}\x1b[0m {chevron}", format_cmd(last_cmd))
                                } else {
                                    format!("  \x1b[38;2;160;165;180mRan {count} commands\x1b[0m {chevron}")
                                };
                                (k, t)
                            }
                            ToolGroupKind::Explore { files, searches, last_target, is_running } => {
                                *is_running = false;
                                let k = if *is_error { LineKind::ToolError } else { LineKind::Tool };
                                let t = format_explore(false, *files, *searches, last_target);
                                (k, t)
                            }
                            ToolGroupKind::Edit { path, added, deleted, is_running } => {
                                *is_running = false;
                                let k = if *is_error { LineKind::ToolError } else { LineKind::Tool };
                                let basename = std::path::Path::new(path).file_name().and_then(|s| s.to_str()).unwrap_or(path);
                                let t = format!(
                                    "  \x1b[38;2;160;165;180mEdited\x1b[0m \x1b[1;38;2;225;230;240m{basename}\x1b[0m \x1b[38;2;145;205;140m+{added}\x1b[0m \x1b[38;2;230;120;120m-{deleted}\x1b[0m"
                                );
                                (k, t)
                            }
                            ToolGroupKind::Subagent { count, last_task, is_running } => {
                                *is_running = false;
                                let k = if *is_error { LineKind::ToolError } else { LineKind::Tool };
                                let t = if *count == 1 {
                                    format!("  \x1b[38;2;160;165;180m{} finished\x1b[0m {chevron}", format_cmd(last_task))
                                } else {
                                    format!("  \x1b[38;2;160;165;180mExplored {count} tasks\x1b[0m {chevron}")
                                };
                                (k, t)
                            }
                            ToolGroupKind::Memory { scopes, is_running } => {
                                *is_running = false;
                                let k = if *is_error { LineKind::ToolError } else { LineKind::Tool };
                                let scopes_str = scopes.join(", ");
                                let t = if *is_error {
                                    format!("  \x1b[38;2;230;120;120mFailed to read memory\x1b[0m \x1b[38;2;225;230;240m[{scopes_str}]\x1b[0m {chevron}")
                                } else {
                                    format!("  \x1b[38;2;160;165;180mRead memory\x1b[0m \x1b[38;2;225;230;240m[{scopes_str}]\x1b[0m {chevron}")
                                };
                                (k, t)
                            }
                            ToolGroupKind::Generic { name, summary, is_running } => {
                                *is_running = false;
                                let k = if *is_error { LineKind::ToolError } else { LineKind::Tool };
                                let t = format!("  \x1b[38;2;160;165;180m{name} [{summary}] finished\x1b[0m {chevron}");
                                (k, t)
                            }
                            ToolGroupKind::Custom { done_text, fail_text, is_running, .. } => {
                                *is_running = false;
                                let k = if *is_error { LineKind::ToolError } else { LineKind::Tool };
                                let t = if *is_error { fail_text.clone() } else { done_text.clone() };
                                (k, t)
                            }
                        };
                        self.lines[i].kind = kind;
                        self.lines[i].text = text;
                        self.lines[i].tool_group = Some(group);
                    }
                }
            }
            LoopEvent::Usage(_) => {}
            LoopEvent::SteeringInjected(directive) => {
                self.streaming = None;
                if let Some(i) = self.streaming_reasoning.take() {
                    let secs = self.reasoning_start.take().map(|t| t.elapsed().as_secs()).unwrap_or(1);
                    self.lines[i].reasoning_secs = Some(secs);
                }
                self.lines.push(ChatLine::new(LineKind::User, directive));
                self.needs_reprint = true;
            }
            // Progress for the status line only: the transcript stays
            // readable instead of gaining a line per loop iteration.
            LoopEvent::StepStarted { .. } => {}
            LoopEvent::Done(reason) => {
                self.streaming = None;
                if let Some(i) = self.streaming_reasoning.take() {
                    let secs = self.reasoning_start.take().map(|t| t.elapsed().as_secs()).unwrap_or(1);
                    self.lines[i].reasoning_secs = Some(secs);
                }
                self.open_tool = None;
                if *reason != flashagent_core::DoneReason::Cancelled
                    && *reason != flashagent_core::DoneReason::Completed
                {
                    self.lines
                        .push(ChatLine::new(LineKind::System, format!("— {reason:?} —")));
                }
            }
        }
    }
}

/// Expansion mode for model reasoning blocks:
/// - `none()`: all collapsed
/// - `all()`: all expanded (Ctrl+Shift+O, permanent across turns)
/// - `last_only()`: only the latest reasoning block expanded (Ctrl+O, temporary per turn)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReasoningExpansion {
    /// Expand all reasoning blocks across all history (permanent Ctrl+Shift+O).
    pub all: bool,
    /// Expand only the latest reasoning block (temporary Ctrl+O).
    pub last: bool,
}

impl ReasoningExpansion {
    pub fn none() -> Self {
        Self { all: false, last: false }
    }

    pub fn all() -> Self {
        Self { all: true, last: false }
    }

    pub fn last_only() -> Self {
        Self { all: false, last: true }
    }
}

impl From<bool> for ReasoningExpansion {
    fn from(b: bool) -> Self {
        if b {
            Self::all()
        } else {
            Self::none()
        }
    }
}

/// Helper to render a single tool execution card.
fn render_single_tool_card(call: &ToolCallRecord, width: usize) -> Vec<RenderLine> {
    let tool_name = call.name.as_str();
    let details_str = call.args_json.as_str();
    let parsed: serde_json::Value = serde_json::from_str(details_str.trim()).unwrap_or_default();

    // The model's stated intent sits above the facts, never instead of them:
    // the card underneath still shows the command, the diff, the output.
    let card = |rows: Vec<RenderLine>| -> Vec<RenderLine> {
        let Some(header) = parsed
            .get("header")
            .and_then(|h| h.as_str())
            .map(str::trim)
            .filter(|h| !h.is_empty())
        else {
            return rows;
        };
        let mut out = vec![(
            LineKind::Tool,
            format!("  \x1b[38;2;160;165;180m{}\x1b[0m", format_cmd(header)),
        )];
        out.extend(rows);
        out
    };

    if tool_name == "run_shell" {
        let cmd = parsed.get("command").and_then(|s| s.as_str()).unwrap_or("command");
        let cwd = parsed.get("cwd").and_then(|s| s.as_str()).map(|s| s.to_string()).unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "~".to_string())
        });
        return card(tool_views::render_command_card(
            &cwd,
            cmd,
            call.result.as_deref(),
            call.is_error,
            call.is_running,
            width,
        ));
    }

    if tool_name == "read_file" || tool_name == "outline_file" {
        let path = parsed.get("path").and_then(|s| s.as_str()).unwrap_or("file");
        let offset = parsed.get("offset").and_then(|s| s.as_u64()).unwrap_or(0) as usize;
        let limit = parsed.get("limit").and_then(|s| s.as_u64()).unwrap_or(2000) as usize;
        return card(tool_views::render_read_card(
            path,
            call.result.as_deref(),
            offset,
            limit,
            width,
        ));
    }

    if tool_name == "list_dir" || tool_name == "glob" {
        let path = parsed.get("path")
            .or_else(|| parsed.get("pattern"))
            .and_then(|s| s.as_str())
            .unwrap_or(".");
        return card(tool_views::render_directory_card(
            path,
            call.result.as_deref(),
            call.is_running,
            width,
        ));
    }

    if tool_name == "grep" {
        let pattern = parsed.get("pattern")
            .or_else(|| parsed.get("query"))
            .and_then(|s| s.as_str())
            .unwrap_or("pattern");
        return card(tool_views::render_grep_card(
            pattern,
            call.result.as_deref(),
            width,
        ));
    }

    if tool_name == "edit_file" || tool_name == "write_file" || tool_name == "patch_file" {
        let path = parsed.get("path").and_then(|s| s.as_str()).unwrap_or("file");
        let is_write = tool_name == "write_file";
        let (added, deleted, diff_text) = if tool_name == "write_file" {
            let content = parsed.get("content").and_then(|s| s.as_str()).unwrap_or("");
            (content.lines().count().max(1), 0, Some(content.to_string()))
        } else if tool_name == "patch_file" {
            let patch = parsed.get("patch").and_then(|s| s.as_str()).unwrap_or("");
            let mut a = 0;
            let mut d = 0;
            for l in patch.lines() {
                if l.starts_with('+') && !l.starts_with("+++") { a += 1; }
                else if l.starts_with('-') && !l.starts_with("---") { d += 1; }
            }
            (a, d, Some(patch.to_string()))
        } else {
            let mut a = 0;
            let mut d = 0;
            let mut simulated = String::new();
            if let Some(edits) = parsed.get("edits").and_then(|e| e.as_array()) {
                for ed in edits {
                    let old_s = ed.get("old_string").and_then(|s| s.as_str()).unwrap_or("");
                    let new_s = ed.get("new_string").and_then(|s| s.as_str()).unwrap_or("");
                    for l in old_s.lines() {
                        simulated.push('-');
                        simulated.push_str(l);
                        simulated.push('\n');
                        d += 1;
                    }
                    for l in new_s.lines() {
                        simulated.push('+');
                        simulated.push_str(l);
                        simulated.push('\n');
                        a += 1;
                    }
                }
            }
            (a, d, Some(simulated))
        };

        return card(tool_views::render_edit_card(
            path,
            added,
            deleted,
            diff_text.as_deref(),
            is_write,
            width,
        ));
    }

    if tool_name == "spawn_agent" {
        let task = parsed.get("task").and_then(|s| s.as_str()).unwrap_or("task");
        return card(tool_views::render_subagent_card(
            task,
            call.result.as_deref(),
            call.is_running,
            width,
        ));
    }

    if tool_name.starts_with("memory_") {
        let scope = parsed.get("scope").and_then(|s| s.as_str()).unwrap_or("project");
        return card(tool_views::render_memory_card(
            tool_name,
            &[scope.to_string()],
            call.result.as_deref().or(Some(&call.args_json)),
            width,
        ));
    }

    if tool_name == "git_status" || tool_name == "git_diff" {
        let is_diff = tool_name == "git_diff";
        return card(tool_views::render_git_card(
            is_diff,
            call.result.as_deref(),
            width,
        ));
    }

    tool_views::render_generic_card(
        if !tool_name.is_empty() { tool_name } else { "tool" },
        &call.args_json,
        call.result.as_deref(),
        width,
    )
}

impl ChatView {
    /// Render all lines wrapped to `width`. `expansion` determines which reasoning blocks expand.
    pub fn render_split(
        &self,
        width: usize,
        expansion: impl Into<ReasoningExpansion>,
    ) -> (Vec<RenderLine>, Vec<RenderLine>) {
        let expansion = expansion.into();
        let boundary = self.settled_boundary();
        let mut settled = Vec::new();
        let mut live = Vec::new();
        let last_user_idx = self.lines.iter().rposition(|l| l.kind == LineKind::User).unwrap_or(0);

        let cached_settled_match = {
            let cache = self.settled_cache.lock();
            if cache.boundary == boundary && cache.width == width && cache.expansion == expansion && !cache.lines.is_empty() {
                Some(cache.lines.clone())
            } else {
                None
            }
        };

        let push_reasoning = |target: &mut Vec<RenderLine>, text: &str, secs: Option<u64>, is_streaming: bool, is_expanded: bool, stage: &str| {
            if !is_expanded {
                let chevron = "\x1b[38;2;120;125;140m›\x1b[0m";
                let text_styled = if is_streaming {
                    format!("  \x1b[1;38;2;194;231;255mThinking:\x1b[0m \x1b[1;38;2;240;235;225m{stage}\x1b[0m {chevron}")
                } else if let Some(s) = secs {
                    format!("  \x1b[1;38;2;194;231;255mThought:\x1b[0m \x1b[1;38;2;240;235;225m{stage}\x1b[0m \x1b[38;2;140;145;160m({s}s)\x1b[0m {chevron}")
                } else {
                    format!("  \x1b[1;38;2;194;231;255mThinking:\x1b[0m \x1b[1;38;2;240;235;225m{stage}\x1b[0m {chevron}")
                };
                let budget = width.saturating_sub(4);
                let clipped_styled = clip_ansi(&text_styled, budget);
                target.push((LineKind::Reasoning, clipped_styled));
            } else {
                let trimmed_text = text.trim();
                let title_vis = format!("Thinking: {stage}");
                let title_styled = format!("\x1b[1;38;2;194;231;255mThinking:\x1b[0m \x1b[1;38;2;240;235;225m{stage}\x1b[0m");
                let box_w = width.saturating_sub(4).clamp(30, 84);
                let border_color = "\x1b[38;2;75;99;130m";
                let reset = "\x1b[0m";

                let header_prefix = "╭─ ";
                let vis_header_len = 3 + visible_width(&title_vis) + 1;
                let dash_count = box_w.saturating_sub(vis_header_len + 1);
                target.push((
                    LineKind::Reasoning,
                    format!("{border_color}{header_prefix}{reset}{title_styled} {border_color}{}╮{reset}", "─".repeat(dash_count)),
                ));
                let inner_w = box_w.saturating_sub(6).max(15);
                for chunk in wrap(trimmed_text, inner_w) {
                    let styled_chunk = md(&chunk);
                    let vis_len = visible_width(&styled_chunk);
                    let pad = " ".repeat(inner_w.saturating_sub(vis_len));
                    if chunk.is_empty() {
                        let empty_pad = " ".repeat(inner_w);
                        target.push((LineKind::Reasoning, format!("{border_color}│{reset}  {empty_pad}  {border_color}│{reset}")));
                    } else {
                        target.push((LineKind::Reasoning, format!("{border_color}│{reset}  {}{pad}  {border_color}│{reset}", styled_chunk)));
                    }
                }
                let bot_dashes = box_w.saturating_sub(2);
                target.push((
                    LineKind::Reasoning,
                    format!("{border_color}╰{}╯{reset}", "─".repeat(bot_dashes)),
                ));
            }
        };

        let mut turn_reasoning_stages = Vec::<String>::new();
        let mut turn_tools_executed = 0usize;
        let mut last_tool_group: Option<ToolGroupKind> = None;

        let has_cached = cached_settled_match.is_some();
        if let Some(cached) = cached_settled_match {
            settled = cached;
        }

        for (i, line) in self.lines.iter().enumerate() {
            if i < boundary && has_cached {
                if line.kind == LineKind::User {
                    turn_reasoning_stages.clear();
                    turn_tools_executed = 0;
                    last_tool_group = None;
                } else if line.kind == LineKind::Tool || line.kind == LineKind::ToolError {
                    turn_tools_executed += 1;
                    if let Some(ref tg) = line.tool_group {
                        last_tool_group = Some(tg.clone());
                    }
                } else if line.kind == LineKind::Reasoning {
                    turn_reasoning_stages.push(resolve_reasoning_stage(
                        &line.text,
                        &turn_reasoning_stages,
                        last_tool_group.as_ref(),
                        turn_tools_executed,
                    ));
                } else if line.kind == LineKind::Assistant {
                    let (think_opt, _) = extract_thinking_from_text(&line.text);
                    if let Some(think_text) = think_opt {
                        let has_native_reasoning = self.lines[last_user_idx..i]
                            .iter()
                            .any(|l| l.kind == LineKind::Reasoning);
                        if !has_native_reasoning {
                            // The ledger keeps the English name: it is what
                            // the "do not repeat a stage" check compares
                            // against, and translating it would make every
                            // stage look new.
                            turn_reasoning_stages.push(resolve_reasoning_stage(
                                &think_text,
                                &turn_reasoning_stages,
                                last_tool_group.as_ref(),
                                turn_tools_executed,
                            ));
                        }
                    }
                }
                continue;
            }

            let target = if i < boundary { &mut settled } else { &mut live };
            let is_last_turn = i >= last_user_idx;
            let is_expanded = expansion.all || (expansion.last && is_last_turn);

            if line.kind == LineKind::User {
                turn_reasoning_stages.clear();
                turn_tools_executed = 0;
                last_tool_group = None;
            } else if line.kind == LineKind::Tool || line.kind == LineKind::ToolError {
                turn_tools_executed += 1;
                if let Some(ref tg) = line.tool_group {
                    last_tool_group = Some(tg.clone());
                }
            }

            if line.kind == LineKind::Reasoning {
                let stage = resolve_reasoning_stage(
                    &line.text,
                    &turn_reasoning_stages,
                    last_tool_group.as_ref(),
                    turn_tools_executed,
                );
                turn_reasoning_stages.push(stage.clone());
                let stage = localize_stage(&stage, &self.language);
                let is_streaming = self.streaming_reasoning == Some(i);
                push_reasoning(target, &line.text, line.reasoning_secs, is_streaming, is_expanded, &stage);
                continue;
            }

            if line.kind == LineKind::Assistant {
                let (think_opt, answer) = extract_thinking_from_text(&line.text);
                if let Some(think_text) = think_opt {
                    let has_native_reasoning = self.lines[last_user_idx..i]
                        .iter()
                        .any(|l| l.kind == LineKind::Reasoning);
                    if !has_native_reasoning {
                        let stage = resolve_reasoning_stage(
                            &think_text,
                            &turn_reasoning_stages,
                            last_tool_group.as_ref(),
                            turn_tools_executed,
                        );
                        turn_reasoning_stages.push(stage.clone());
                        let stage = localize_stage(&stage, &self.language);
                        let is_streaming = self.streaming == Some(i);
                        push_reasoning(target, &think_text, line.reasoning_secs, is_streaming, is_expanded, &stage);
                    }
                    if answer.is_empty() {
                        continue;
                    }
                    for chunk in render_markdown_text(&answer, width) {
                        target.push((LineKind::Assistant, chunk));
                    }
                    continue;
                } else {
                    for chunk in render_markdown_text(&line.text, width) {
                        target.push((LineKind::Assistant, chunk));
                    }
                    continue;
                }
            }

            if line.kind == LineKind::Tool || line.kind == LineKind::ToolError {
                let effective_expanded = line.is_expanded.unwrap_or(is_expanded);
                if effective_expanded {
                    if !line.tool_calls.is_empty() {
                        for call in &line.tool_calls {
                            let card_lines = render_single_tool_card(call, width);
                            for cl in card_lines {
                                target.push(cl);
                            }
                        }
                        continue;
                    } else if line.tool_name.is_some() || line.details.is_some() {
                        let synthetic_call = ToolCallRecord {
                            name: line.tool_name.clone().unwrap_or_default(),
                            args_json: line.details.clone().unwrap_or_default(),
                            result: line.tool_result.clone(),
                            is_error: line.kind == LineKind::ToolError,
                            is_running: false,
                        };
                        let card_lines = render_single_tool_card(&synthetic_call, width);
                        for cl in card_lines {
                            target.push(cl);
                        }
                        continue;
                    }
                }
                target.push((line.kind, line.text.clone()));
                continue;
            }

            let is_box_border = line.kind == LineKind::System
                && (line.text.contains('╭') || line.text.contains('╰') || line.text.contains('├') || line.text.contains('│'));
            if is_box_border {
                target.push((line.kind, clip_ansi(&line.text, width)));
                continue;
            }

            let wrapped = wrap(&line.text, if line.kind == LineKind::User { width - 2 } else { width });
            for (j, chunk) in wrapped.into_iter().enumerate() {
                let text = match line.kind {
                    LineKind::User => {
                        if j == 0 { format!("{GLYPH_PROMPT} {chunk}") } else { format!("  {chunk}") }
                    }
                    LineKind::System => chunk,
                    _ => md(&chunk),
                };
                target.push((line.kind, text));
            }
        }
        if boundary > 0 {
            let mut cache = self.settled_cache.lock();
            cache.lines = settled.clone();
            cache.boundary = boundary;
            cache.width = width;
            cache.expansion = expansion;
        }
        (settled, live)
    }

    /// Render everything wrapped to `width`, with `❯`-prefix on user lines.
    pub fn render(&self, width: usize) -> Vec<(LineKind, String)> {
        let (mut all, live) = self.render_split(width, true);
        all.extend(live);
        all
    }
}

use crossterm::style::Stylize;

/// One rendered line: kind + text.
pub type RenderLine = (LineKind, String);

/// Spinner frames — braille dots.
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Prompt chevron icon for user inputs (clean geometric glyph, no emojis).
pub const GLYPH_PROMPT: &str = "❯";

/// Status glyphs for tool lines (own set: diamonds, not circles/checks).
pub const GLYPH_RUN: &str = "◈"; // tool call in flight
pub const GLYPH_OK: &str = "◆"; // tool finished
pub const GLYPH_ERR: &str = "◇"; // tool failed (hollow = broken)
pub const GLYPH_SUB: &str = "↳"; // sub-result hanging under a call

/// Extract the current stage title from streamed reasoning. Models can indicate
/// their stage using markers like `[Stage: Name]`, `### Name`, or numbered steps
/// (`1. Analyze the Request:`, `1.  **Analyze the Request:**`, `5.  **Final Output Generation.**`).
/// The collapsed preview displays the LATEST stage.
pub fn reasoning_stage(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    for l in text.lines() {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }

        // 1. Explicit bracketed stage marker: `[Stage: Name]`, `[Phase: Name]`, `[Step: Name]`, `[Stage Name]`
        if let Some(rest) = t
            .strip_prefix("[Stage:")
            .or_else(|| t.strip_prefix("[stage:"))
            .or_else(|| t.strip_prefix("[Phase:"))
            .or_else(|| t.strip_prefix("[Step:"))
            .or_else(|| t.strip_prefix("[Stage "))
        {
            if let Some((name, _)) = rest.split_once(']') {
                let name = name.trim();
                if !name.is_empty() && name.chars().count() <= 60 {
                    last = Some(name.to_string());
                    continue;
                }
            }
        }

        // 2. Markdown heading: `### Name` or `## Name` or `# Name`
        if let Some(heading) = t.strip_prefix("### ").or_else(|| t.strip_prefix("## ")).or_else(|| t.strip_prefix("# ")) {
            let name = heading.trim().trim_matches('#').trim().trim_matches('*').trim();
            if !name.is_empty() && name.chars().count() <= 60 {
                last = Some(name.to_string());
                continue;
            }
        }

        // 3. Numbered or bulleted labeled steps: `N. Title:` or `* Title:` or `* **Title:**` or `N.  **Title**`
        let step = if let Some(body) = t.strip_prefix("* ").or_else(|| t.strip_prefix("- ")) {
            Some(body)
        } else {
            t.split_once('.').and_then(|(n, rest)| {
                let n_trim = n.trim();
                (!n_trim.is_empty() && n_trim.chars().all(|c| c.is_ascii_digit())).then_some(rest.trim_start())
            })
        };
        if let Some(body) = step {
            let body = body.trim();
            if let Some((title, _)) = body.split_once(':') {
                let title = title.trim().trim_matches('*').trim();
                if !title.is_empty() && title.chars().count() <= 60 {
                    last = Some(title.to_string());
                    continue;
                }
            }
            // Also handle `N. **Title**` (without trailing colon, e.g. `5.  **Final Output Generation.**`)
            if let Some(stripped) = body.strip_prefix("**") {
                if let Some(end) = stripped.find("**") {
                    let title = stripped[..end].trim().trim_end_matches('.').trim();
                    if !title.is_empty() && title.chars().count() <= 60 {
                        last = Some(title.to_string());
                        continue;
                    }
                }
            }
        }

        // 4. Standalone bold header or stage title: `**Stage Title**` or `**Step 1: ...**`
        if let Some(stripped) = t.strip_prefix("**") {
            if let Some(end) = stripped.find("**") {
                let inner = stripped[..end].trim().trim_end_matches(':').trim();
                if inner.chars().count() >= 3 && inner.chars().count() <= 60 && !inner.contains('\n') {
                    last = Some(inner.to_string());
                    continue;
                }
            }
        }

        // 5. Unbracketed Step or Phase: `Step 1: ...` or `Phase 1: ...`
        if let Some(rest) = t.strip_prefix("Step ").or_else(|| t.strip_prefix("Phase ")) {
            if let Some((_num, title)) = rest.split_once(':') {
                let clean = title.trim().trim_matches('*').trim();
                if clean.chars().count() >= 3 && clean.chars().count() <= 60 && !clean.contains('\n') {
                    last = Some(clean.to_string());
                    continue;
                }
            }
        }
    }
    if last.is_none() {
        let lower = text.to_lowercase();
        // 1. Specific verification / builds
        if lower.contains("cargo test") || lower.contains("cargo build") || lower.contains("cargo check") || lower.contains("cargo clippy") {
            return Some("Planning Verification".to_string());
        }
        if lower.contains("read memory") || lower.contains("read the memory") || lower.contains("reading memory") {
            return Some("Reading memory files".to_string());
        }
        if lower.contains("agents.md") || lower.contains("project rules") || lower.contains("philosophy.md") {
            return Some("Checking project rules".to_string());
        }

        // 2. High-priority tool actions: search, files, edits, shell
        if lower.contains("search result") || lower.contains("grep found") || lower.contains("glob found") || lower.contains("found definition") {
            return Some("Evaluating Search Results".to_string());
        }
        if lower.contains("read file") || lower.contains("file content") || lower.contains("in this file") || lower.contains("looking at the code") {
            return Some("Analyzing File Contents".to_string());
        }
        if lower.contains("edit file") || lower.contains("write file") || lower.contains("patch file") || lower.contains("applying edit") {
            return Some("Planning Code Changes".to_string());
        }
        if lower.contains("run shell") || lower.contains("running command") {
            return Some("Planning Command Execution".to_string());
        }
        if lower.contains("evaluat") || lower.contains("tool result") || lower.contains("tool output") {
            return Some("Evaluating Tool Results".to_string());
        }
        if lower.contains("plan") || lower.contains("next step") || lower.contains("next action") {
            return Some("Planning Next Action".to_string());
        }
        if lower.contains("final") || lower.contains("answer") || lower.contains("solution") || lower.contains("respond to") {
            return Some("Formulating Response".to_string());
        }
        if lower.contains("respond in english") || lower.contains("respond in text") {
            return Some("Formulating English response".to_string());
        }

        // 3. Initial request understanding (placed after specific tool/code actions)
        if lower.contains("user said") || lower.contains("user wants") || lower.contains("user asked") {
            return Some("Understanding user request".to_string());
        }
    }
    last
}

/// Helper to derive an intuitive stage name from the most recently executed tool.
pub fn derive_stage_from_tool(last_tool_group: Option<&ToolGroupKind>) -> String {
    match last_tool_group {
        Some(ToolGroupKind::Explore { files, searches, .. }) => {
            if *searches > 0 && *files == 0 {
                "Evaluating Search Results".to_string()
            } else if *files > 0 && *searches == 0 {
                "Analyzing File Contents".to_string()
            } else {
                "Evaluating Explored Context".to_string()
            }
        }
        Some(ToolGroupKind::Command { .. }) => "Evaluating Command Output".to_string(),
        Some(ToolGroupKind::Edit { .. }) => "Verifying Code Changes".to_string(),
        Some(ToolGroupKind::Subagent { .. }) => "Evaluating Subagent Output".to_string(),
        Some(ToolGroupKind::Memory { .. }) => "Reviewing Project Memory".to_string(),
        _ => "Evaluating Tool Results".to_string(),
    }
}

/// Say one of the stage labels this app writes itself in the language of the
/// conversation.
///
/// Only labels FlashAgent invents are translated. A stage lifted out of the
/// model's own reasoning is the model's wording and is left exactly as it
/// wrote it — putting Russian words in its mouth would be a lie about what it
/// said.
pub fn localize_stage(stage: &str, language: &str) -> String {
    if !language.eq_ignore_ascii_case("ru") {
        return stage.to_string();
    }
    const RU: &[(&str, &str)] = &[
        ("Analyzing Request", "Разбираю запрос"),
        ("Evaluating Search Results", "Оцениваю результаты поиска"),
        ("Deepening Code Search", "Углубляю поиск по коду"),
        ("Analyzing File Contents", "Разбираю содержимое файлов"),
        ("Evaluating Explored Context", "Оцениваю найденное"),
        ("Evaluating Command Output", "Оцениваю вывод команды"),
        ("Verifying Code Changes", "Проверяю правки"),
        ("Evaluating Subagent Output", "Оцениваю ответ субагента"),
        ("Reviewing Project Memory", "Просматриваю память проекта"),
        ("Evaluating Tool Results", "Оцениваю результаты инструментов"),
        ("Synthesizing Findings", "Свожу выводы"),
        ("Planning Implementation", "Планирую реализацию"),
        ("Refining Solution", "Уточняю решение"),
        ("Verifying Solution", "Проверяю решение"),
        ("Formulating Response", "Формулирую ответ"),
    ];
    // A repeated stage comes back as "Name (part 2)"; the suffix is ours too.
    let (base, part) = match stage.split_once(" (part ") {
        Some((base, rest)) => (base, rest.trim_end_matches(')').parse::<u32>().ok()),
        None => (stage, None),
    };
    let translated = RU
        .iter()
        .find(|(en, _)| en.eq_ignore_ascii_case(base))
        .map(|(_, ru)| (*ru).to_string());
    match (translated, part) {
        (Some(ru), Some(n)) => format!("{ru} (часть {n})"),
        (Some(ru), None) => ru,
        (None, _) => stage.to_string(),
    }
}

/// Contextual reasoning stage resolver.
/// Determines a non-repeating, progress-advancing stage label for a reasoning block
/// based on the text, what tools have already run in the current turn, and what stages
/// were previously assigned in the same turn.
pub fn resolve_reasoning_stage(
    text: &str,
    prior_stages: &[String],
    last_tool_group: Option<&ToolGroupKind>,
    tools_executed: usize,
) -> String {
    let raw_stage = reasoning_stage(text);

    let is_initial_like = |s: &str| {
        let lower = s.to_lowercase();
        lower.contains("understanding")
            || lower.contains("analyzing request")
            || lower.contains("analyze the request")
            || lower.contains("determine the goal")
            || lower.contains("identify the goal")
    };

    let stage = if let Some(parsed) = raw_stage {
        if tools_executed > 0 && is_initial_like(&parsed) {
            derive_stage_from_tool(last_tool_group)
        } else {
            parsed
        }
    } else if tools_executed == 0 {
        "Analyzing Request".to_string()
    } else {
        derive_stage_from_tool(last_tool_group)
    };

    if prior_stages.iter().any(|p| p.eq_ignore_ascii_case(&stage)) {
        let candidates = [
            "Evaluating Search Results",
            "Deepening Code Search",
            "Analyzing File Contents",
            "Synthesizing Findings",
            "Planning Implementation",
            "Refining Solution",
            "Verifying Solution",
            "Formulating Response",
        ];

        for candidate in candidates {
            if !prior_stages.iter().any(|p| p.eq_ignore_ascii_case(candidate)) {
                return candidate.to_string();
            }
        }

        let count = prior_stages.iter().filter(|p| p.to_lowercase().starts_with(&stage.to_lowercase())).count();
        format!("{stage} (part {})", count + 1)
    } else {
        stage
    }
}

/// Extracts embedded thinking/scratchpad from assistant text, returning
/// `(Some(thinking), remaining_answer)` or `(None, original_text)` if no thinking is found.
pub fn extract_thinking_from_text(text: &str) -> (Option<String>, String) {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("<think>") {
        if let Some(end) = rest.find("</think>") {
            let think = rest[..end].trim().to_string();
            let after = rest[end + "</think>".len()..].trim_start().to_string();
            return (Some(think), after);
        } else {
            let think = rest.trim().to_string();
            return (Some(think), String::new());
        }
    }

    if t.starts_with("Thinking Process:")
        || t.starts_with("Thinking:\n")
        || t.starts_with("[Stage:")
        || t.starts_with("### Stage:")
    {
        let mut thinking_lines = Vec::new();
        let mut answer_lines = Vec::new();
        let mut in_thinking = true;

        for line in text.lines() {
            let trimmed = line.trim();
            if in_thinking {
                if trimmed.is_empty()
                    || trimmed.starts_with("Thinking Process:")
                    || trimmed.starts_with("Thinking:")
                    || is_step_line(trimmed)
                {
                    thinking_lines.push(line);
                } else {
                    in_thinking = false;
                    answer_lines.push(line);
                }
            } else {
                answer_lines.push(line);
            }
        }

        let think_str = thinking_lines.join("\n").trim().to_string();
        let ans_str = answer_lines.join("\n").trim().to_string();
        if !think_str.is_empty() {
            return (Some(think_str), ans_str);
        }
    }

    (None, text.to_string())
}

fn is_step_line(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with("[Stage:") || t.starts_with("[stage:") || t.starts_with("[Phase:") || t.starts_with("[Step:") {
        return t.contains(']');
    }
    if t.starts_with("### ") || t.starts_with("## ") {
        return true;
    }
    if t.starts_with("**") && t.ends_with("**") && t.len() > 4 {
        return true;
    }
    if let Some(body) = t.strip_prefix("* ").or_else(|| t.strip_prefix("- ")) {
        body.contains(':')
    } else {
        t.split_once(". ").is_some_and(|(n, rest)| {
            !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && rest.contains(':')
        })
    }
}

/// Re-emit `prefix` after every color reset inside `text`: styled spans
/// (from `md()`) close with a full reset that would otherwise blank the
/// outer line color for the rest of the line (observed: rest of a reasoning
/// line turned plain white after an embedded `**bold**`).
pub fn restore_line_color(text: &str, prefix: &str) -> String {
    text.replace("\x1b[0m", &format!("\x1b[0m{prefix}"))
        .replace("\x1b[39m", &format!("\x1b[39m{prefix}"))
}

/// Clip `text` to `width` visible cells, keeping ANSI escape sequences whole
/// (they are zero-width). Cutting inside a CSI sequence would print its tail
/// literally (`[39m`) and leak style across lines.
pub fn clip_ansi(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cells = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Copy the whole sequence verbatim: ESC [ ... final-byte (@-~).
            out.push(c);
            if chars.peek() == Some(&'[') {
                if let Some(bracket) = chars.next() {
                    out.push(bracket);
                    while let Some(&n) = chars.peek() {
                        out.push(n);
                        chars.next();
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
            }
            continue;
        }
        let char_w = c.width().unwrap_or(0);
        if cells + char_w > width {
            continue;
        }
        cells += char_w;
        out.push(c);
    }
    out
}

/// Count visible cells in `text`, ignoring ANSI escape sequences.
pub fn visible_width(text: &str) -> usize {
    let mut cells = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        cells += c.width().unwrap_or(0);
    }
    cells
}

/// Strips all ANSI escape sequences from `text`.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Format a single row within an enclosed boxed container with vertical border delimiters `│`.
pub fn pad_box_row(content: &str, width: usize) -> String {
    let inner_w = width.saturating_sub(2);
    let border_color = "\x1b[38;2;95;90;85m";
    let reset = "\x1b[0m";
    let vis = visible_width(content);
    let clipped = if vis > inner_w {
        clip_ansi(content, inner_w)
    } else {
        content.to_string()
    };
    let clipped_vis = visible_width(&clipped);
    let pad = inner_w.saturating_sub(clipped_vis);
    format!("{border_color}│{reset}{clipped}{}{border_color}│{reset}", " ".repeat(pad))
}

/// Truncates string in the middle if it exceeds `max_len`, keeping prefix and suffix.
/// E.g. "qwen3.6-35b-a3b-uncensored-heretic-native-mtp-preserved-i1" -> "qwen3.6-35b...preserved-i1".
pub fn truncate_middle(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len || max_len < 5 {
        return s.chars().take(max_len).collect();
    }
    let keep = max_len.saturating_sub(3);
    let left = keep.div_ceil(2);
    let right = keep / 2;

    let prefix: String = s.chars().take(left).collect();
    let suffix: String = s.chars().skip(char_count - right).collect();
    format!("{prefix}...{suffix}")
}

/// Formats the startup welcome banner with runtime context:
/// model & context window, current directory, permission mode,
/// thinking effort/presets, loaded memory documents, and usage tips.
pub fn welcome_card(
    model: &str,
    context_window: Option<&str>,
    cwd: &str,
    mode: &str,
    memory_docs: usize,
    thinking: Option<&str>,
    width: usize,
) -> Vec<RenderLine> {
    welcome_card_with_thinking(model, cwd, mode, memory_docs, thinking, context_window, width)
}

fn pad_cell(s: &str, width: usize) -> String {
    let vis = visible_width(s);
    if vis >= width {
        clip_ansi(s, width)
    } else {
        let pad = width - vis;
        format!("{s}{}", " ".repeat(pad))
    }
}

fn center_cell(s: &str, width: usize) -> String {
    let vis = visible_width(s);
    if vis >= width {
        clip_ansi(s, width)
    } else {
        let left = (width - vis) / 2;
        let right = width - vis - left;
        format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
    }
}

const M3_PRI_B: &str = "\x1b[1;38;2;138;180;248m";
const M3_PRI: &str = "\x1b[38;2;138;180;248m";
const M3_LGT: &str = "\x1b[38;2;168;199;250m";
const M3_LGT_B: &str = "\x1b[1;38;2;168;199;250m";
const M3_ICE: &str = "\x1b[38;2;194;231;255m";
const M3_BRD: &str = "\x1b[38;2;75;99;130m";
const M3_MUT: &str = "\x1b[38;2;155;165;180m";
const M3_TXT: &str = "\x1b[38;2;235;240;250m";
const M3_TXT_B: &str = "\x1b[1;38;2;235;240;250m";
const RESET: &str = "\x1b[0m";

/// What the mascot's face is reacting to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MascotMood {
    /// Still waiting for the first answer from the model server.
    Checking,
    /// The server answered: it is reachable and listing models.
    Happy,
    /// The server did not answer — nothing will work until it does.
    Offline,
}

/// The mascot as a pixel grid: 16 wide, 12 tall, two pixels per terminal row.
///
/// A terminal cell is about twice as tall as it is wide, so a sprite drawn one
/// pixel per cell comes out stretched — that is what made the old mascot a
/// spiky kite. Drawing two pixels per cell with `▀` (foreground = upper pixel,
/// background = lower) gives square pixels and a shape that reads as intended.
///
/// `.` transparent · `#` body · `o` highlight · `w` eye · `k` mouth
fn mascot_grid(blink: bool, mood: MascotMood) -> [&'static str; 12] {
    // Blinking closes the upper half of each eye and leaves a lash line.
    // Offline keeps the eyes shut: nothing to look at until the server answers.
    let (eyes_top, eyes_bottom) = if blink || mood == MascotMood::Offline {
        (".##############.", ".###kk####kk###.")
    } else {
        (".###ww####ww###.", ".###ww####ww###.")
    };
    // The mouth carries the mood: corners up when the server answered, a flat
    // line when it did not, a small neutral dot while we are still asking.
    let (mouth_top, mouth_bottom) = match mood {
        MascotMood::Happy => ("..###k####k###..", "..####kkkk####.."),
        MascotMood::Checking => ("..############..", "..#####kk#####.."),
        MascotMood::Offline => ("..############..", "..####kkkk####.."),
    };
    [
        "......oooo......",
        "....oooooooo....",
        "..############..",
        ".##############.",
        eyes_top,
        eyes_bottom,
        mouth_top,
        mouth_bottom,
        "..############..",
        "...##########...",
        "....##....##....",
        "....##....##....",
    ]
}

/// Breathing: a slow triangle wave, 0 (dimmest) to 5 (brightest), one full
/// cycle per ~3 seconds at the 80 ms UI tick.
fn breath_level(tick_n: usize) -> u8 {
    const PERIOD: usize = 38;
    let half = PERIOD / 2;
    let phase = tick_n % PERIOD;
    let up = if phase < half { phase } else { PERIOD - phase - 1 };
    (up.min(half - 1) * 6 / half) as u8
}

/// Whether the mascot looks different at `tick_n` than it did one tick ago.
///
/// The welcome card is rebuilt only when this says so: breathing must not cost
/// a full card repaint 12 times a second.
pub fn mascot_needs_repaint(tick_n: usize) -> bool {
    let blink = |t: usize| (t % 50 == 46) || (t % 50 == 47);
    let prev = tick_n.wrapping_sub(1);
    blink(tick_n) != blink(prev) || breath_level(tick_n) != breath_level(prev)
}

/// Colour of one pixel, or `None` when it is transparent. `breath` (0..=5)
/// lifts the highlight; `offline` drains the body towards grey.
fn mascot_color(px: u8, breath: u8, offline: bool) -> Option<(u8, u8, u8)> {
    let lift = |lo: u8, hi: u8| -> u8 {
        let span = hi as i32 - lo as i32;
        (lo as i32 + span * breath as i32 / 5) as u8
    };
    let color = match px {
        b'#' => (138, 180, 248),
        b'o' => (lift(150, 194), lift(196, 231), lift(224, 255)),
        b'w' => (255, 255, 255),
        b'k' => (32, 42, 62),
        _ => return None,
    };
    if offline {
        // Halfway to grey: visibly not the healthy colour, still readable.
        let (r, g, b) = color;
        let grey = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        let mix = |c: u8| ((c as u16 + grey as u16) / 2) as u8;
        return Some((mix(r), mix(g), mix(b)));
    }
    Some(color)
}

/// The mascot's face for the status line, shown while the model works.
///
/// The welcome card (and the full sprite with it) is gone as soon as the
/// conversation starts, so this is the only place the mascot can react to a
/// running turn. Five columns wide in every frame — a status line that
/// changes width jitters.
///
/// `phase` is 80 ms of *wall clock*, not UI ticks: the screen repaints when
/// events arrive, which with a fast model is far more often than the tick and
/// with a slow one far less. A single-frame blink would be missed either way,
/// so each expression holds for a few hundred milliseconds.
pub fn thinking_face(phase: usize) -> &'static str {
    match phase % 24 {
        20..=23 => "(-_-)",
        16..=19 => "(^_-)",
        _ => "(•_•)",
    }
}

/// Returns the 6 lines of the 8-bit companion mascot «Swift» in Material 3 colors.
pub fn mascot_swift_lines() -> [String; 6] {
    mascot_swift_lines_animated(0)
}

/// Returns the 6 lines of the mascot for `tick_n`, with a neutral mood.
pub fn mascot_swift_lines_animated(tick_n: usize) -> [String; 6] {
    mascot_swift_lines_mood(tick_n, MascotMood::Checking)
}

/// Returns the 6 lines of the mascot: blinking every ~4 seconds, breathing
/// continuously, and wearing `mood` on its face.
///
/// Every line is exactly 16 columns wide, transparent pixels included, so the
/// rows stay aligned with each other when the card centres them.
pub fn mascot_swift_lines_mood(tick_n: usize, mood: MascotMood) -> [String; 6] {
    let blink = (tick_n % 50 == 46) || (tick_n % 50 == 47);
    let grid = mascot_grid(blink, mood);
    let breath = breath_level(tick_n);
    let offline = mood == MascotMood::Offline;
    let mut out: Vec<String> = Vec::with_capacity(6);
    for pair in grid.chunks(2) {
        let (top, bottom) = (pair[0].as_bytes(), pair[1].as_bytes());
        let mut line = String::new();
        for x in 0..16 {
            match (mascot_color(top[x], breath, offline), mascot_color(bottom[x], breath, offline)) {
                (None, None) => line.push(' '),
                (Some((r, g, b)), None) => {
                    line.push_str(&format!("\x1b[38;2;{r};{g};{b}m▀\x1b[0m"));
                }
                (None, Some((r, g, b))) => {
                    line.push_str(&format!("\x1b[38;2;{r};{g};{b}m▄\x1b[0m"));
                }
                (Some((tr, tg, tb)), Some((br, bg, bb))) => {
                    line.push_str(&format!(
                        "\x1b[38;2;{tr};{tg};{tb}m\x1b[48;2;{br};{bg};{bb}m▀\x1b[0m"
                    ));
                }
            }
        }
        out.push(line);
    }
    out.try_into().expect("12 pixel rows make exactly 6 terminal rows")
}

/// Formats the startup welcome banner with clean version header and quick instructions.
pub fn welcome_card_with_thinking(
    model: &str,
    cwd: &str,
    mode: &str,
    memory_docs: usize,
    thinking: Option<&str>,
    context_window: Option<&str>,
    width: usize,
) -> Vec<RenderLine> {
    welcome_card_with_thinking_animated(model, cwd, mode, memory_docs, thinking, context_window, width, 0)
}

/// Formats the startup welcome banner with animated mascot frame support.
#[allow(clippy::too_many_arguments)]
pub fn welcome_card_with_thinking_animated(
    model: &str,
    cwd: &str,
    mode: &str,
    memory_docs: usize,
    thinking: Option<&str>,
    context_window: Option<&str>,
    width: usize,
    tick_n: usize,
) -> Vec<RenderLine> {
    welcome_card_responsive(model, cwd, mode, memory_docs, thinking, context_window, width, 24, tick_n)
}

/// Fully responsive startup welcome banner adapting to both terminal width and height.
/// In standard (24-row) and compact terminals, lines are kept constrained so the card and pet
/// are 100% visible and never scroll off-screen.
#[allow(clippy::too_many_arguments)]
pub fn welcome_card_responsive(
    model: &str,
    cwd: &str,
    mode: &str,
    memory_docs: usize,
    thinking: Option<&str>,
    context_window: Option<&str>,
    width: usize,
    height: usize,
    tick_n: usize,
) -> Vec<RenderLine> {
    welcome_card_responsive_opts(
        model,
        cwd,
        mode,
        memory_docs,
        thinking,
        context_window,
        width,
        height,
        tick_n,
        true,
        MascotMood::Checking,
    )
}

/// Responsive welcome banner with customizable feature options.
#[allow(clippy::too_many_arguments)]
/// Join as many of `parts` as fit in `width`, in order, and drop the rest.
///
/// A status line built from four facts and then clipped loses the last one
/// mid-word and looks broken; dropping whole facts keeps it readable at any
/// terminal size. Each part is `(plain, styled)`: the plain form is what gets
/// measured, so colour codes do not count towards the width.
pub fn fit_parts(parts: &[(String, String)], separator: &str, width: usize) -> String {
    let sep_w = visible_width(separator);
    let mut out = String::new();
    let mut used = 0usize;
    for (plain, styled) in parts {
        let cost = plain.chars().count() + if out.is_empty() { 0 } else { sep_w };
        // Stop at the first one that does not fit rather than skipping it:
        // the parts are in priority order, and "64k" on its own, without the
        // model it belongs to, says nothing.
        if used + cost > width {
            break;
        }
        if !out.is_empty() {
            out.push_str(separator);
        }
        out.push_str(styled);
        used += cost;
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn welcome_card_responsive_opts(
    model: &str,
    cwd: &str,
    mode: &str,
    memory_docs: usize,
    thinking: Option<&str>,
    context_window: Option<&str>,
    width: usize,
    height: usize,
    tick_n: usize,
    show_mascot: bool,
    mood: MascotMood,
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "Developer".to_string());
    let username_clean = if username.chars().count() > 16 {
        truncate_middle(&username, 16)
    } else {
        username
    };

    let th_str = thinking.unwrap_or("High");
    let ctx_short = context_window.unwrap_or("128k");
    let mascot = if show_mascot {
        mascot_swift_lines_mood(tick_n, mood)
    } else {
        [
            "".to_string(),
            format!("{M3_PRI_B}FlashAgent Engine{RESET}"),
            format!("{M3_MUT}Local-first AI Pair Programmer{RESET}"),
            format!("{M3_MUT}Ultra-low latency inference{RESET}"),
            "".to_string(),
            "".to_string(),
        ]
    };

    let cwd_clean = if cwd.starts_with('~') && !cwd.starts_with("~/") && cwd.len() > 1 {
        format!("~/{}", &cwd[1..])
    } else {
        cwd.to_string()
    };

    let cur_ver = flashagent_svc::updater::current_version();
    let ver_disp = if cur_ver.starts_with('v') || cur_ver.starts_with('b') {
        cur_ver.to_string()
    } else {
        format!("v{cur_ver}")
    };

    if width >= 56 && height >= 18 {
        // Two-column modular layout in Material 3 Light Blue
        let card_w = if width >= 76 {
            width.clamp(76, 104)
        } else {
            width
        };
        let w1: usize = if card_w >= 76 {
            ((card_w * 44) / 100).clamp(36, 46)
        } else {
            ((card_w * 40) / 100).clamp(24, 32)
        };
        let w2: usize = card_w.saturating_sub(w1 + 3);

        let t_left_vis = format!(">_ FlashAgent {ver_disp}");
        let d1 = w1.saturating_sub(t_left_vis.chars().count() + 3);
        let t_left_colored = format!("{M3_PRI_B}>_ FlashAgent{RESET} {M3_LGT}{ver_disp}{RESET}");

        let t_r1_vis = "System & Context";
        let d2 = w2.saturating_sub(t_r1_vis.chars().count() + 3);
        let t_r1_colored = format!("{M3_LGT_B}{t_r1_vis}{RESET}");

        let t_r2_vis = "Quick Commands";
        let d_mid = w2.saturating_sub(t_r2_vis.chars().count() + 3);
        let t_r2_colored = format!("{M3_LGT_B}{t_r2_vis}{RESET}");

        let top = format!("{M3_BRD}╭─{RESET} {t_left_colored} {M3_BRD}{}┬─{RESET} {t_r1_colored} {M3_BRD}{}╮{RESET}", "─".repeat(d1), "─".repeat(d2));
        let mid_div = format!("{M3_BRD}├─{RESET} {t_r2_colored} {M3_BRD}{}┤{RESET}", "─".repeat(d_mid));
        let bot = format!("{M3_BRD}╰{}┴{}╯{RESET}", "─".repeat(w1), "─".repeat(w2));

        let th_short = if let Some((first, _)) = th_str.split_once(' ') {
            first
        } else {
            th_str
        };
        let ctx_clean = ctx_short.strip_suffix(" ctx").unwrap_or(ctx_short);
        let model_meta_len = w1.saturating_sub(18).clamp(14, 26);
        let model_meta = truncate_middle(model, model_meta_len);
        let left_meta = format!("{M3_PRI}{model_meta}{RESET} {M3_MUT}·{RESET} {M3_LGT}{th_short}{RESET} {M3_MUT}·{RESET} {M3_ICE}{ctx_clean}{RESET}");
        let cwd_meta = format!("{M3_MUT}{}{RESET}", truncate_middle(&cwd_clean, w1.saturating_sub(4)));

        let left_lines = [
            center_cell(&format!("{M3_TXT_B}Welcome back {M3_ICE}{username_clean}{M3_TXT_B}!{RESET}"), w1),
            "".to_string(),
            center_cell(&mascot[0], w1),
            center_cell(&mascot[1], w1),
            center_cell(&mascot[2], w1),
            center_cell(&mascot[3], w1),
            center_cell(&mascot[4], w1),
            center_cell(&mascot[5], w1),
            "".to_string(),
            center_cell(&left_meta, w1),
            center_cell(&cwd_meta, w1),
        ];

        let model_val = truncate_middle(model, w2.saturating_sub(13));
        let ctx_val = truncate_middle(context_window.unwrap_or("128k capacity (local)"), w2.saturating_sub(13));
        let mode_val = truncate_middle(mode, w2.saturating_sub(13));

        let right_top = [
            format!(" {M3_MUT}Model:   {RESET} {M3_TXT_B}{model_val}{RESET}"),
            format!(" {M3_MUT}Context: {RESET} {M3_ICE}{ctx_val}{RESET}"),
            format!(" {M3_MUT}Mode:    {RESET} {M3_LGT}{mode_val}{RESET}"),
            format!(" {M3_MUT}Memory:  {RESET} {M3_TXT}{memory_docs}{RESET} {M3_MUT}active document(s){RESET}"),
            format!(" {M3_MUT}Config:  {RESET} {M3_ICE}Tab{RESET} {M3_MUT}settings{RESET} {M3_MUT}·{RESET} {M3_ICE}F5{RESET} {M3_MUT}sampling{RESET}"),
        ];

        let right_bot = [
            format!(" {M3_PRI_B}/goal <task>{RESET} {M3_MUT}for autonomy{RESET}"),
            format!(" {M3_ICE}Tab{RESET} {M3_MUT}settings{RESET} {M3_MUT}·{RESET} {M3_ICE}Esc{RESET} {M3_MUT}quit{RESET}"),
            format!(" {M3_ICE}F1{RESET} {M3_MUT}context{RESET} {M3_MUT}·{RESET} {M3_ICE}F2{RESET} {M3_MUT}verbose{RESET} {M3_MUT}·{RESET} {M3_ICE}F3{RESET} {M3_MUT}model{RESET}"),
            format!(" {M3_ICE}F4{RESET} {M3_MUT}effort{RESET} {M3_MUT}·{RESET} {M3_ICE}F5{RESET} {M3_MUT}sampling{RESET} {M3_MUT}·{RESET} {M3_ICE}Ctrl+V{RESET} {M3_MUT}paste image{RESET}"),
            format!(" {M3_ICE}Ctrl+R{RESET} {M3_MUT}regen{RESET} {M3_MUT}·{RESET} {M3_ICE}/help{RESET} {M3_MUT}or{RESET} {M3_ICE}/skills{RESET} {M3_MUT}for more{RESET}"),
        ];

        lines.push((LineKind::System, top));
        for i in 0..5 {
            let l = pad_cell(&left_lines[i], w1);
            let r = pad_cell(&right_top[i], w2);
            lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{l}{M3_BRD}│{RESET}{r}{M3_BRD}│{RESET}")));
        }

        let l5 = pad_cell(&left_lines[5], w1);
        lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{l5}{mid_div}")));

        for i in 6..11 {
            let l = pad_cell(&left_lines[i], w1);
            let r = pad_cell(&right_bot[i - 6], w2);
            lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{l}{M3_BRD}│{RESET}{r}{M3_BRD}│{RESET}")));
        }
        lines.push((LineKind::System, bot));
    } else {
        // Compact single-column layout for compact or narrow screens (< 56 cols or < 18 rows)
        // Scaled to never overflow vertical height or horizontal bounds
        let inner_w = width.saturating_sub(2).min(74);
        let t_left_vis = format!(">_ FlashAgent {ver_disp}");
        let d1 = inner_w.saturating_sub(t_left_vis.chars().count() + 3);
        let t_left_colored = format!("{M3_PRI_B}>_ FlashAgent{RESET} {M3_LGT}{ver_disp}{RESET}");
        let top = format!("{M3_BRD}╭─{RESET} {t_left_colored} {M3_BRD}{}╮{RESET}", "─".repeat(d1));
        let bot = format!("{M3_BRD}╰{}╯{RESET}", "─".repeat(inner_w));

        // Model first, then effort, then context: whichever no longer fits is
        // dropped whole instead of being cut in half.
        let model_meta = truncate_middle(model, inner_w.saturating_sub(4).min(28));
        let left_meta = fit_parts(
            &[
                (model_meta.clone(), format!("{M3_PRI}{model_meta}{RESET}")),
                (th_str.to_string(), format!("{M3_LGT}{th_str}{RESET}")),
                (ctx_short.to_string(), format!("{M3_ICE}{ctx_short}{RESET}")),
            ],
            &format!(" {M3_MUT}\u{b7}{RESET} "),
            inner_w.saturating_sub(2),
        );
        let cwd_meta = format!("{M3_MUT}{}{RESET}", truncate_middle(&cwd_clean, inner_w.saturating_sub(4)));

        lines.push((LineKind::System, top));
        lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(&format!("{M3_TXT_B}Welcome back {M3_ICE}{username_clean}{M3_TXT_B}!{RESET}"), inner_w))));

        if show_mascot {
            if height >= 20 {
                for m in &mascot {
                    lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(m, inner_w))));
                }
            } else {
                // A short screen shows the top half of the same sprite rather
                // than a different creature: there used to be a hand-drawn
                // "mini" version here that still had the old round eyes, so
                // the mascot changed species when the window got short.
                for m in mascot.iter().take(3) {
                    lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(m, inner_w))));
                }
            }
        }

        lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(&left_meta, inner_w))));
        lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(&cwd_meta, inner_w))));

        if height >= 14 {
            let div_cmd = format!("{M3_BRD}├─{RESET} {M3_LGT_B}Quick Commands{RESET} {M3_BRD}{}┤{RESET}", "─".repeat(inner_w.saturating_sub(17)));
            lines.push((LineKind::System, div_cmd));
            lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", pad_cell(&format!(" {M3_PRI_B}/goal <task>{RESET} {M3_MUT}for autonomy{RESET}"), inner_w))));
            let hints = fit_parts(
                &[
                    ("Tab settings".into(), format!("{M3_ICE}Tab{RESET} {M3_MUT}settings{RESET}")),
                    ("Esc quit".into(), format!("{M3_ICE}Esc{RESET} {M3_MUT}quit{RESET}")),
                    ("F1..F5 hotkeys".into(), format!("{M3_ICE}F1..F5{RESET} {M3_MUT}hotkeys{RESET}")),
                ],
                &format!(" {M3_MUT}\u{b7}{RESET} "),
                inner_w.saturating_sub(2),
            );
            lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", pad_cell(&format!(" {hints}"), inner_w))));
        } else {
            lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", pad_cell(&format!(" {M3_PRI_B}/goal{RESET} {M3_MUT}·{RESET} {M3_ICE}Tab{RESET} {M3_MUT}settings{RESET} {M3_MUT}·{RESET} {M3_ICE}Esc{RESET} {M3_MUT}quit{RESET}"), inner_w))));
        }
        lines.push((LineKind::System, bot));
    }

    if height >= 14 {
        lines.push((LineKind::System, String::new()));
    }
    lines
}

/// Inline markdown for terminal: `**bold**` → bold, `` `code` `` → colored.
/// Line-level, no full parser — good enough for chat output.
pub fn md(line: &str) -> String {
    // Fast path: no markdown markers.
    if !line.contains("**") && !line.contains('`') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < line.len() {
        if line[i..].starts_with("**") {
            if let Some(end) = line[i + 2..].find("**") {
                let inner = &line[i + 2..i + 2 + end];
                if !inner.is_empty() {
                    out.push_str(&crossterm::style::style(inner.to_string()).bold().to_string());
                    i += 2 + end + 2;
                    continue;
                }
            }
            out.push_str("**");
            i += 2;
        } else if bytes[i] == b'`' {
            if let Some(end) = line[i + 1..].find('`') {
                let inner = &line[i + 1..i + 1 + end];
                if !inner.is_empty() {
                    out.push_str(
                        &crossterm::style::style(inner.to_string())
                            .with(crossterm::style::Color::Rgb { r: 215, g: 180, b: 110 })
                            .to_string(),
                    );
                    i += 1 + end + 1;
                    continue;
                }
            }
            out.push('`');
            i += 1;
        } else if let Some(ch) = line[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

enum ListMarkerKind {
    Bullet { glyph: &'static str },
    Number { num: String, is_paren: bool },
}

fn parse_list_marker(trimmed: &str, level: usize) -> Option<(ListMarkerKind, usize)> {
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        let glyph = match level {
            0 => "\x1b[38;2;140;145;160m•\x1b[0m",
            1 => "\x1b[38;2;140;145;160m◦\x1b[0m",
            _ => "\x1b[38;2;140;145;160m▪\x1b[0m",
        };
        return Some((ListMarkerKind::Bullet { glyph }, 2));
    }
    let num_digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if num_digits > 0 && num_digits <= 3 {
        let after = &trimmed[num_digits..];
        if after.starts_with(". ") {
            return Some((
                ListMarkerKind::Number {
                    num: trimmed[..num_digits].to_string(),
                    is_paren: false,
                },
                num_digits + 2,
            ));
        } else if after.starts_with(") ") {
            return Some((
                ListMarkerKind::Number {
                    num: trimmed[..num_digits].to_string(),
                    is_paren: true,
                },
                num_digits + 2,
            ));
        }
    }
    None
}

/// Formats an assistant's markdown response for the terminal:
/// - Headings (`#`, `##`, `###`) rendered as styled section titles without raw hashes
/// - Tables (`| col1 | col2 |` + delimiter) rendered with unicode box borders and aligned cells
/// - Lists (`- `, `* `, `1. `, `1) `, checkboxes) rendered with bullets and hanging indentation
/// - Inline bold (`**bold**`) and inline code (`` `code` ``) styled with ANSI colors
pub fn render_markdown_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(20);
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut in_code_block = false;

    let is_table_delimiter = |l: &str| -> bool {
        let t = l.trim();
        if !t.contains('-') || (!t.contains('|') && !t.contains('+')) {
            return false;
        }
        let inner = t.trim_matches('|').trim_matches('+').trim();
        !inner.is_empty() && inner.chars().all(|c| c == '-' || c == ':' || c == '|' || c == '+' || c.is_whitespace())
    };

    let is_table_row = |l: &str| -> bool {
        let t = l.trim();
        if t.starts_with("```") || t.starts_with('#') {
            return false;
        }
        (t.starts_with('|') && t.ends_with('|')) || (t.contains('|') && t.matches('|').count() >= 1)
    };

    let parse_cells = |l: &str| -> Vec<String> {
        let t = l.trim();
        let stripped = t.strip_prefix('|').unwrap_or(t);
        let stripped = stripped.strip_suffix('|').unwrap_or(stripped);
        stripped.split('|').map(|c| c.trim().to_string()).collect()
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // 1. Code blocks: ```
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                let lang = trimmed.strip_prefix("```").unwrap_or("").trim();
                let title = if lang.is_empty() { "code".to_string() } else { lang.to_string() };
                let border_color = "\x1b[38;2;75;99;130m";
                let reset = "\x1b[0m";
                let dash_count = width.saturating_sub(visible_width(&title) + 5).max(4);
                out.push(format!("{border_color}╭─ {reset}\x1b[38;2;194;231;255m{title}\x1b[0m {border_color}{}╮{reset}", "─".repeat(dash_count)));
            } else {
                let border_color = "\x1b[38;2;75;99;130m";
                let reset = "\x1b[0m";
                let dash_count = width.saturating_sub(2).max(4);
                out.push(format!("{border_color}╰{}╯{reset}", "─".repeat(dash_count)));
            }
            i += 1;
            continue;
        }

        if in_code_block {
            let border_color = "\x1b[38;2;75;99;130m";
            let reset = "\x1b[0m";
            let inner_w = width.saturating_sub(2);
            let inner_content_w = inner_w.saturating_sub(1);

            let chunks = if line.is_empty() {
                vec![String::new()]
            } else if visible_width(line) <= inner_content_w {
                vec![line.to_string()]
            } else {
                wrap(line, inner_content_w)
            };

            for chunk in chunks {
                let clipped = if visible_width(&chunk) > inner_content_w {
                    clip_ansi(&chunk, inner_content_w)
                } else {
                    chunk
                };
                let vis = visible_width(&clipped);
                let pad = inner_content_w.saturating_sub(vis);
                out.push(format!(
                    "{border_color}│{reset} \x1b[38;2;215;180;110m{clipped}\x1b[0m{}{border_color}│{reset}",
                    " ".repeat(pad)
                ));
            }
            i += 1;
            continue;
        }

        // 2. Table detection: header row followed by delimiter row
        if is_table_row(line) && i + 1 < lines.len() && is_table_delimiter(lines[i + 1]) {
            let header_row = parse_cells(line);
            let mut data_rows = Vec::new();
            i += 2; // skip header and delimiter

            while i < lines.len() && is_table_row(lines[i]) && !is_table_delimiter(lines[i]) {
                data_rows.push(parse_cells(lines[i]));
                i += 1;
            }

            let num_cols = header_row.len().max(data_rows.iter().map(|r| r.len()).max().unwrap_or(0));
            if num_cols > 0 {
                let mut col_widths = vec![3usize; num_cols];
                for (c, cell) in header_row.iter().enumerate() {
                    col_widths[c] = col_widths[c].max(visible_width(&md(cell)));
                }
                for row in &data_rows {
                    for (c, cell) in row.iter().enumerate() {
                        if c < num_cols {
                            col_widths[c] = col_widths[c].max(visible_width(&md(cell)));
                        }
                    }
                }

                // Constrain table to fit within terminal width if necessary
                let border_chars_count = 3 * num_cols + 1;
                let total_content_w: usize = col_widths.iter().sum();
                let total_w = total_content_w + border_chars_count;
                if total_w > width {
                    let available_content_w = width.saturating_sub(border_chars_count).max(num_cols * 3);
                    let excess = total_content_w.saturating_sub(available_content_w);
                    if excess > 0 {
                        for _ in 0..excess {
                            if let Some((max_idx, _)) = col_widths.iter().enumerate().max_by_key(|(_, &w)| w) {
                                if col_widths[max_idx] > 3 {
                                    col_widths[max_idx] -= 1;
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                }

                let border_color = "\x1b[38;2;75;99;130m";
                let reset = "\x1b[0m";

                if !out.is_empty() && !out.last().map(|s| s.is_empty()).unwrap_or(true) {
                    out.push(String::new());
                }

                // Top border: ╭─...─┬─...─╮
                let mut top_border = format!("{border_color}╭");
                for (c, &w) in col_widths.iter().enumerate() {
                    top_border.push_str(&"─".repeat(w + 2));
                    if c + 1 < num_cols {
                        top_border.push('┬');
                    } else {
                        top_border.push('╮');
                    }
                }
                top_border.push_str(reset);
                out.push(top_border);

                // Header row: │ col1 │ col2 │
                let mut header_str = format!("{border_color}│{reset}");
                for (c, &w) in col_widths.iter().enumerate() {
                    let raw_cell = header_row.get(c).map(|s| s.as_str()).unwrap_or("");
                    let styled = md(raw_cell);
                    let vis = visible_width(&styled);
                    let cell_content = if vis > w {
                        clip_ansi(&styled, w)
                    } else {
                        let pad = " ".repeat(w.saturating_sub(vis));
                        format!("{styled}{pad}")
                    };
                    header_str.push_str(&format!(" \x1b[1;38;2;240;235;225m{cell_content}\x1b[0m {border_color}│{reset}"));
                }
                out.push(header_str);

                // Middle separator: ├─...─┼─...─┤
                let mut mid_border = format!("{border_color}├");
                for (c, &w) in col_widths.iter().enumerate() {
                    mid_border.push_str(&"─".repeat(w + 2));
                    if c + 1 < num_cols {
                        mid_border.push('┼');
                    } else {
                        mid_border.push('┤');
                    }
                }
                mid_border.push_str(reset);
                out.push(mid_border);

                // Data rows
                for row in data_rows {
                    let mut row_str = format!("{border_color}│{reset}");
                    for (c, &w) in col_widths.iter().enumerate() {
                        let raw_cell = row.get(c).map(|s| s.as_str()).unwrap_or("");
                        let styled = md(raw_cell);
                        let vis = visible_width(&styled);
                        let cell_content = if vis > w {
                            clip_ansi(&styled, w)
                        } else {
                            let pad = " ".repeat(w.saturating_sub(vis));
                            format!("{styled}{pad}")
                        };
                        row_str.push_str(&format!(" {cell_content} {border_color}│{reset}"));
                    }
                    out.push(row_str);
                }

                // Bottom border: ╰─...─┴─...─╯
                let mut bot_border = format!("{border_color}╰");
                for (c, &w) in col_widths.iter().enumerate() {
                    bot_border.push_str(&"─".repeat(w + 2));
                    if c + 1 < num_cols {
                        bot_border.push('┴');
                    } else {
                        bot_border.push('╯');
                    }
                }
                bot_border.push_str(reset);
                out.push(bot_border);
            }
            continue;
        }

        // 3. Headings: `# `, `## `, `### `, `#### `
        if trimmed.starts_with('#') {
            let hashes = trimmed.chars().take_while(|&c| c == '#').count();
            if (1..=4).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
                let title = trimmed[hashes..].trim();
                if !out.is_empty() && !out.last().map(|s| s.is_empty()).unwrap_or(true) {
                    out.push(String::new());
                }
                let styled_heading = match hashes {
                    1 => format!("\x1b[1;38;2;255;255;255m◆ {}\x1b[0m", md(title)),
                    2 => format!("\x1b[1;38;2;194;231;255m◈ {}\x1b[0m", md(title)),
                    3 => format!("\x1b[1;38;2;225;230;240m▸ {}\x1b[0m", md(title)),
                    _ => format!("\x1b[38;2;180;185;200m▪ {}\x1b[0m", md(title)),
                };
                out.push(styled_heading);
                i += 1;
                continue;
            }
        }

        // 4. List items: bullet lists, task checkboxes, and numbered lists
        let indent_spaces: usize = line
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        let level = indent_spaces / 2;

        if let Some((marker, offset)) = parse_list_marker(trimmed, level) {
            let mut raw_content = trimmed[offset..].trim().to_string();

            // Collect subsequent continuation lines belonging to this item
            let mut next_i = i + 1;
            while next_i < lines.len() {
                let n_line = lines[next_i];
                let n_trimmed = n_line.trim();
                if n_trimmed.is_empty() {
                    break;
                }
                if n_trimmed.starts_with("```") || n_trimmed.starts_with('#') || is_table_row(n_line) {
                    break;
                }
                let n_spaces: usize = n_line
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .map(|c| if c == '\t' { 4 } else { 1 })
                    .sum();
                let n_level = n_spaces / 2;
                if parse_list_marker(n_trimmed, n_level).is_some() {
                    break;
                }
                if n_spaces >= indent_spaces + 2 || (indent_spaces == 0 && n_spaces >= 2) {
                    raw_content.push(' ');
                    raw_content.push_str(n_trimmed);
                    next_i += 1;
                } else {
                    break;
                }
            }
            i = next_i - 1; // will be incremented at end of loop

            // Task list checkboxes: `[ ] `, `[x] `, `[X] `
            let (check_str, content_clean, check_vis) = if let Some(stripped) = raw_content.strip_prefix("[ ] ") {
                ("\x1b[38;2;140;145;160m☐\x1b[0m ", stripped, 2)
            } else if let Some(stripped) = raw_content.strip_prefix("[x] ").or_else(|| raw_content.strip_prefix("[X] ")) {
                ("\x1b[38;2;145;205;140m☑\x1b[0m ", stripped, 2)
            } else {
                ("", raw_content.as_str(), 0)
            };

            let indent_pad = "  ".repeat(level);
            let (prefix, prefix_vis) = match marker {
                ListMarkerKind::Bullet { glyph } => {
                    let p = format!("{indent_pad}  {glyph} {check_str}");
                    let vis = 2 * level + 2 + 1 + 1 + check_vis;
                    (p, vis)
                }
                ListMarkerKind::Number { num, is_paren } => {
                    let delim = if is_paren { ")" } else { "." };
                    let num_glyph = format!("\x1b[38;2;140;145;160m{num}{delim}\x1b[0m");
                    let p = format!("{indent_pad}  {num_glyph} {check_str}");
                    let vis = 2 * level + 2 + num.len() + 1 + 1 + check_vis;
                    (p, vis)
                }
            };

            let hang_indent = " ".repeat(prefix_vis);
            let avail_w = width.saturating_sub(prefix_vis).max(10);
            let styled_content = md(content_clean);
            let chunks = wrap_styled(&styled_content, avail_w);
            if chunks.is_empty() {
                out.push(prefix);
            } else {
                for (idx, chunk) in chunks.into_iter().enumerate() {
                    if idx == 0 {
                        out.push(format!("{prefix}{chunk}"));
                    } else {
                        out.push(format!("{hang_indent}{chunk}"));
                    }
                }
            }
            i += 1;
            continue;
        }

        // 5. Regular paragraphs
        if trimmed.is_empty() {
            out.push(String::new());
        } else {
            let styled = md(line);
            for chunk in wrap_styled(&styled, width) {
                out.push(chunk);
            }
        }
        i += 1;
    }

    out
}

/// Render an approval card with default (Allow) selection.
pub fn approval_card(req: &ApprovalRequest) -> Vec<(LineKind, String)> {
    approval_card_with_selection(req, Decision::Allow)
}

/// Render an approval card with interactive Allow / Deny selection.
pub fn approval_card_with_selection(req: &ApprovalRequest, selected: Decision) -> Vec<(LineKind, String)> {
    let mut out = vec![
        (LineKind::System, "╭─ approval required ─────".to_string()),
        (LineKind::Tool, format!("│ tool: {}", req.tool)),
    ];
    let short: String = req.args_json.chars().take(120).collect();
    let more = if req.args_json.chars().count() > 120 { "…" } else { "" };
    out.push((LineKind::Tool, format!("│ args: {short}{more}")));
    if let Some(diff) = &req.diff {
        out.push((LineKind::Tool, "│ diff:".into()));
        for l in diff.lines().take(60) {
            let kind = if l.starts_with('+') {
                LineKind::Diff
            } else if l.starts_with('-') {
                LineKind::ToolError
            } else {
                LineKind::System
            };
            out.push((kind, format!("│ {l}")));
        }
    }
    let (allow_btn, deny_btn) = match selected {
        Decision::Allow => (
            "\x1b[1;38;2;145;205;140m❯ [Allow]\x1b[0m",
            "\x1b[38;2;140;135;130m  [Deny]\x1b[0m",
        ),
        Decision::Deny => (
            "\x1b[38;2;140;135;130m  [Allow]\x1b[0m",
            "\x1b[1;38;2;230;110;95m❯ [Deny]\x1b[0m",
        ),
    };
    out.push((
        LineKind::System,
        format!("╰─ {allow_btn}   {deny_btn}   \x1b[38;2;135;130;125m(←/→ select · enter confirm)\x1b[0m ───"),
    ));
    out
}

/// An approval gate answered from the terminal: the UI loop picks up
/// [`pending`] and calls [`TuiGate::respond`] with the user's key press.
#[derive(Default)]
pub struct TuiGate {
    pending: Mutex<Option<(ApprovalRequest, Option<Decision>)>>,
    changed: tokio::sync::Notify,
}

impl TuiGate {
    /// Create an empty gate.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Current pending request, if any (the UI renders a card for it).
    pub fn pending(&self) -> Option<ApprovalRequest> {
        self.pending.lock().as_ref().map(|(r, _)| r.clone())
    }

    /// Answer the pending request (no-op when nothing is pending).
    pub fn respond(&self, d: Decision) {
        let mut guard = self.pending.lock();
        if let Some(slot) = guard.as_mut() {
            slot.1 = Some(d);
        }
        drop(guard);
        self.changed.notify_waiters();
    }

    async fn wait_decision(&self) -> Decision {
        loop {
            // Register interest *before* checking: a respond() landing between
            // the check and the await would otherwise be a lost wakeup.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(d) = self.pending.lock().as_ref().and_then(|(_, d)| *d) {
                return d;
            }
            notified.await;
        }
    }
}

/// Clears a gate's pending card when the waiting call goes away — also when
/// the turn is cancelled mid-question, so the card never outlives its caller.
struct ClearOnDrop<'a, T>(&'a Mutex<Option<T>>, &'a tokio::sync::Notify);

impl<T> Drop for ClearOnDrop<'_, T> {
    fn drop(&mut self) {
        *self.0.lock() = None;
        self.1.notify_waiters();
    }
}

#[async_trait]
impl ApprovalGate for TuiGate {
    async fn approve(&self, req: &ApprovalRequest) -> Decision {
        *self.pending.lock() = Some((req.clone(), None));
        let _clear = ClearOnDrop(&self.pending, &self.changed);
        self.changed.notify_waiters();
        self.wait_decision().await
    }
}

/// Interactive question request from the model.
#[derive(Debug, Clone)]
pub struct QuestionRequest {
    pub question: String,
    pub options: Option<Vec<String>>,
    pub multi_select: bool,
}

type QuestionSlot = Option<(QuestionRequest, Option<(String, bool)>)>;

/// TUI question gate: presents questions from `ask_user` in the UI composer.
#[derive(Default)]
pub struct TuiQuestionGate {
    pending: parking_lot::Mutex<QuestionSlot>,
    changed: tokio::sync::Notify,
}

impl TuiQuestionGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn pending(&self) -> Option<QuestionRequest> {
        self.pending.lock().as_ref().map(|(r, _)| r.clone())
    }

    pub fn respond(&self, answer: String, is_write_in: bool) {
        let mut guard = self.pending.lock();
        if let Some(slot) = guard.as_mut() {
            slot.1 = Some((answer, is_write_in));
        }
        drop(guard);
        self.changed.notify_waiters();
    }

    pub fn cancel(&self) {
        self.respond("User cancelled the question".to_string(), true);
    }

    async fn wait_answer(&self) -> (String, bool) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(res) = self.pending.lock().as_ref().and_then(|(_, r)| r.clone()) {
                return res;
            }
            notified.await;
        }
    }
}

#[async_trait]
impl flashagent_tools::QuestionGate for TuiQuestionGate {
    async fn ask(&self, question: &str, options: Option<&[String]>, multi_select: bool) -> Result<(String, bool), String> {
        let req = QuestionRequest {
            question: question.to_string(),
            options: options.map(|opts| opts.to_vec()),
            multi_select,
        };
        *self.pending.lock() = Some((req, None));
        let _clear = ClearOnDrop(&self.pending, &self.changed);
        self.changed.notify_waiters();
        Ok(self.wait_answer().await)
    }
}

/// Renders a closed, beautiful session saved card for display upon application exit.
pub fn render_session_saved_card(session_id: &str, width: usize) -> Vec<String> {
    let box_w = width.saturating_sub(6).clamp(52, 90);
    let inner_text_w = box_w.saturating_sub(2);
    let border_color = "\x1b[38;2;225;175;95m";
    let reset = "\x1b[0m";

    let title_styled = " \x1b[1;38;2;225;175;95mSession Saved\x1b[0m ";
    let title_vis = visible_width(title_styled);
    let dashes = box_w.saturating_sub(title_vis + 1);
    let top = format!("  {border_color}╭─{title_styled}{}╮{reset}", "─".repeat(dashes));

    let resume_cmd = format!("flashagent --resume {session_id}");
    let msg = if inner_text_w >= 66 {
        format!("To resume next time: \x1b[1;38;2;240;235;225m{resume_cmd}\x1b[0m")
    } else {
        format!("Resume: \x1b[1;38;2;240;235;225m{resume_cmd}\x1b[0m")
    };
    let msg_clipped = if visible_width(&msg) > inner_text_w {
        clip_ansi(&msg, inner_text_w)
    } else {
        msg
    };
    let msg_vis = visible_width(&msg_clipped);
    let pad = " ".repeat(inner_text_w.saturating_sub(msg_vis));
    let body = format!("  {border_color}│{reset} {msg_clipped}{reset}{pad} {border_color}│{reset}");

    let bottom = format!("  {border_color}╰{}╯{reset}", "─".repeat(box_w));

    vec![top, body, bottom]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_recap_for_an_earlier_turn_does_not_derail_the_live_turn() {
        let mut chat = ChatView::default();
        chat.push_user("first");
        chat.on_event(&LoopEvent::TurnDelta("answer one".into()));
        chat.on_event(&LoopEvent::Done(DoneReason::Completed));
        chat.push_user("second");
        chat.on_event(&LoopEvent::ToolStarted { id: "c".into(), name: "run_shell".into(), args_json: r#"{"command":"ls"}"#.into() });
        // Turn 1's recap arrives while turn 2's tool is still running.
        chat.attach_turn_recap(1, "recap: one");
        chat.on_event(&LoopEvent::ToolFinished { id: "c".into(), is_error: false, result_len: 1, result: Some("x".into()) });
        chat.on_event(&LoopEvent::TurnDelta("answer two".into()));
        let (settled, live) = chat.render_split(100, ReasoningExpansion::default());
        let text: Vec<String> = settled.iter().chain(live.iter()).map(|(_, l)| strip_ansi(l)).collect();
        let joined = text.join("\n");
        assert!(joined.contains("Ran ls"), "{joined}");
        assert!(!joined.contains("Running ls"), "{joined}");
        let user2 = text.iter().find(|l| l.contains("second")).unwrap();
        assert!(!user2.contains("answer two"), "stream leaked into the user line: {user2}");
        assert!(joined.find("recap: one").unwrap() < joined.find("second").unwrap());
    }

    #[test]
    fn chat_view_accumulates_stream_and_tools() {
        let mut v = ChatView::default();
        v.push_user("hello");
        v.on_event(&LoopEvent::TurnDelta("Hel".into()));
        v.on_event(&LoopEvent::TurnDelta("lo!".into()));
        v.on_event(&LoopEvent::ToolStarted {
            id: "t1".into(),
            name: "grep".into(),
            args_json: r#"{"pattern":"x"}"#.into(),
        });
        v.on_event(&LoopEvent::ToolFinished { id: "t1".into(), is_error: false, result_len: 10, result: None });
        v.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));

        let lines = v.render(80);
        assert!(lines.iter().any(|(k, t)| *k == LineKind::User && t.contains(&format!("{GLYPH_PROMPT} hello"))));
        assert!(lines.iter().any(|(k, t)| *k == LineKind::Assistant && t.contains("Hello!")));
        assert!(lines.iter().any(|(k, t)| *k == LineKind::Tool && (t.contains("Explored") || t.contains("Searched") || t.contains("Grep") || t.contains('x'))));
        assert!(!lines.iter().any(|(k, t)| *k == LineKind::System && t.contains("Completed")));
    }

    #[test]
    fn welcome_card_and_visible_width() {
        let card = welcome_card("gpt-4", Some("32k ctx"), "/home/user/repo", "Manual", 2, Some("high"), 80);
        let cur_ver = flashagent_svc::updater::current_version();
        assert!(card.iter().any(|(_, t)| t.contains(">_ FlashAgent") && (t.contains(cur_ver) || t.contains("v0.1.0"))));

        let card80 = welcome_card("qwen3.6-35b-a3b-mtp", Some("128k ctx"), "/home/user/repo", "Manual", 2, Some("high"), 80);
        let card100 = welcome_card("qwen3.6-35b-a3b-mtp", Some("128k ctx"), "/home/user/repo", "Manual", 2, Some("high"), 100);
        let card120 = welcome_card("qwen3.6-35b-a3b-mtp", Some("128k ctx"), "/home/user/repo", "Manual", 2, Some("high"), 120);
        let w80 = visible_width(&card80[0].1);
        let w100 = visible_width(&card100[0].1);
        let w120 = visible_width(&card120[0].1);
        assert!(w120 > w80);
        assert_eq!(w80, 80);
        assert_eq!(w100, 100);
        assert_eq!(w120, 104);

        let raw = "\x1b[1;38;2;255;0;0mHello World\x1b[0m";
        assert_eq!(visible_width(raw), 11);
        let boxed = pad_box_row("test", 20);
        assert_eq!(visible_width(&boxed), 20);

        // Test ANSI-safe wrap
        let colored_line = "\x1b[38;2;95;90;85m│\x1b[0m \x1b[38;2;160;155;145mmodel:\x1b[0m \x1b[38;2;225;175;95mcustom-model-v1\x1b[0m";
        let wrapped = wrap(colored_line, 80);
        assert_eq!(wrapped.len(), 1, "Line with visible_width <= 80 must not be split by ANSI codes");
    }

    #[test]
    fn failed_tool_marks_with_cross() {
        let mut v = ChatView::default();
        v.on_event(&LoopEvent::ToolStarted {
            id: "t2".into(),
            name: "run_shell".into(),
            args_json: r#"{"command":"false"}"#.into(),
        });
        v.on_event(&LoopEvent::ToolFinished { id: "t2".into(), is_error: true, result_len: 0, result: None });
        let (settled, live) = v.render_split(80, false);
        let lines: Vec<_> = settled.into_iter().chain(live).collect();
        assert!(lines.iter().any(|(k, t)| *k == LineKind::ToolError && t.contains("Failed") && t.contains("false") && (t.contains('›') || t.contains('>'))));
    }

    #[test]
    fn test_collapsed_tools_formatting() {
        let mut v = ChatView::default();
        // 1. Single command
        v.on_event(&LoopEvent::ToolStarted {
            id: "c1".into(),
            name: "run_shell".into(),
            args_json: r#"{"command":"cargo test -p flashagent-llm"}"#.into(),
        });
        v.on_event(&LoopEvent::ToolFinished { id: "c1".into(), is_error: false, result_len: 100, result: None });
        let (settled, live) = v.render_split(100, false);
        let all: Vec<_> = settled.into_iter().chain(live).collect();
        assert!(all.iter().any(|(_, t)| t.contains("Ran") && t.contains("cargo test -p flashagent-llm") && (t.contains('›') || t.contains('>'))));

        // 2. Multiple commands consolidation
        let mut v2 = ChatView::default();
        v2.on_event(&LoopEvent::ToolStarted {
            id: "c1".into(),
            name: "run_shell".into(),
            args_json: r#"{"command":"cargo check"}"#.into(),
        });
        v2.on_event(&LoopEvent::ToolFinished { id: "c1".into(), is_error: false, result_len: 10, result: None });
        v2.on_event(&LoopEvent::ToolStarted {
            id: "c2".into(),
            name: "run_shell".into(),
            args_json: r#"{"command":"cargo test"}"#.into(),
        });
        v2.on_event(&LoopEvent::ToolFinished { id: "c2".into(), is_error: false, result_len: 10, result: None });
        v2.on_event(&LoopEvent::ToolStarted {
            id: "c3".into(),
            name: "run_shell".into(),
            args_json: r#"{"command":"cargo clippy"}"#.into(),
        });
        v2.on_event(&LoopEvent::ToolFinished { id: "c3".into(), is_error: false, result_len: 10, result: None });
        let (settled2, live2) = v2.render_split(100, false);
        let all2: Vec<_> = settled2.into_iter().chain(live2).collect();
        assert!(all2.iter().any(|(_, t)| t.contains("Ran 3 commands") && (t.contains('›') || t.contains('>'))));

        // 3. Edit file formatting with crab icon and diff stats
        let mut v3 = ChatView::default();
        v3.on_event(&LoopEvent::ToolStarted {
            id: "e1".into(),
            name: "edit_file".into(),
            args_json: r#"{"path":"crates/tui/src/lib.rs","edits":[{"old_string":"","new_string":"line1\nline2\n"}]}"#.into(),
        });
        v3.on_event(&LoopEvent::ToolFinished { id: "e1".into(), is_error: false, result_len: 50, result: None });
        let (settled3, live3) = v3.render_split(100, false);
        let all3: Vec<_> = settled3.into_iter().chain(live3).collect();
        assert!(all3.iter().any(|(_, t)| t.contains("Edited") && t.contains("lib.rs") && t.contains("+2") && t.contains("-0")));

        // 4. Consecutive exploration consolidation: 4 files, 1 search
        let mut v4 = ChatView::default();
        for i in 1..=4 {
            v4.on_event(&LoopEvent::ToolStarted {
                id: format!("f{i}"),
                name: "read_file".into(),
                args_json: format!(r#"{{"path":"file{i}.rs"}}"#),
            });
            v4.on_event(&LoopEvent::ToolFinished { id: format!("f{i}"), is_error: false, result_len: 10, result: None });
        }
        v4.on_event(&LoopEvent::ToolStarted {
            id: "s1".into(),
            name: "grep".into(),
            args_json: r#"{"pattern":"needle"}"#.into(),
        });
        v4.on_event(&LoopEvent::ToolFinished { id: "s1".into(), is_error: false, result_len: 10, result: None });
        let (settled4, live4) = v4.render_split(100, false);
        let all4: Vec<_> = settled4.into_iter().chain(live4).collect();
        assert!(all4.iter().any(|(_, t)| t.contains("Explored 4 files, 1 search") && (t.contains('›') || t.contains('>'))));

        // 5. Memory tool formatting
        let mut v5 = ChatView::default();
        v5.on_event(&LoopEvent::ToolStarted {
            id: "m1".into(),
            name: "memory_read".into(),
            args_json: r#"{"scope":"project"}"#.into(),
        });
        v5.on_event(&LoopEvent::ToolFinished { id: "m1".into(), is_error: false, result_len: 100, result: None });
        let (settled5, live5) = v5.render_split(100, false);
        let all5: Vec<_> = settled5.into_iter().chain(live5).collect();
        assert!(all5.iter().any(|(_, t)| t.contains("Read memory") && t.contains("[project]") && (t.contains('›') || t.contains('>'))));
        assert!(!all5.iter().any(|(_, t)| t.contains("{\"scope\":\"project\"}")));

        // 5b. Memory consolidation: consecutive reads merge scopes
        let mut v5b = ChatView::default();
        v5b.on_event(&LoopEvent::ToolStarted {
            id: "m1".into(),
            name: "memory_read".into(),
            args_json: r#"{"scope":"project"}"#.into(),
        });
        v5b.on_event(&LoopEvent::ToolFinished { id: "m1".into(), is_error: false, result_len: 50, result: None });
        v5b.on_event(&LoopEvent::ToolStarted {
            id: "m2".into(),
            name: "memory_read".into(),
            args_json: r#"{"scope":"global"}"#.into(),
        });
        v5b.on_event(&LoopEvent::ToolFinished { id: "m2".into(), is_error: false, result_len: 50, result: None });
        let (settled5b, live5b) = v5b.render_split(100, false);
        let all5b: Vec<_> = settled5b.into_iter().chain(live5b).collect();
        assert_eq!(all5b.len(), 1, "Consecutive memory_read calls must consolidate into a single line");
        assert!(all5b.iter().any(|(_, t)| t.contains("Read memory") && t.contains("[project, global]")));

        // 6. Env info and ask_user formatting
        let mut v6 = ChatView::default();
        v6.on_event(&LoopEvent::ToolStarted {
            id: "u1".into(),
            name: "ask_user".into(),
            args_json: r#"{"question":"Deploy now?"}"#.into(),
        });
        v6.on_event(&LoopEvent::ToolFinished { id: "u1".into(), is_error: false, result_len: 5, result: None });
        let (settled6, live6) = v6.render_split(100, false);
        let all6: Vec<_> = settled6.into_iter().chain(live6).collect();
        assert!(all6.iter().any(|(_, t)| t.contains("Asked user:") && t.contains("Deploy now?") && (t.contains('›') || t.contains('>'))));
    }

    #[test]
    fn settled_boundary_splits_live_from_printed() {
        let mut v = ChatView::default();
        v.push_user("q");
        v.on_event(&LoopEvent::TurnDelta("partial".into()));
        // streaming assistant line is live → boundary at its index.
        assert_eq!(v.settled_boundary(), 1);
        v.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));
        // Everything settled → boundary == len.
        assert_eq!(v.settled_boundary(), v.render_split(80, true).0.len() + v.render_split(80, true).1.len());
    }

    #[test]
    fn long_lines_wrap_within_width() {
        let mut v = ChatView::default();
        v.push_user(&"word ".repeat(40));
        let lines = v.render(40);
        assert!(lines.len() > 1);
        for (_, l) in &lines {
            assert!(l.chars().count() <= 40, "too long: {l}");
        }
        // Continuation lines keep the indent.
        assert!(lines.iter().skip(1).all(|(k, l)| *k == LineKind::User && l.starts_with("  ")));
    }

    #[test]
    fn approval_card_shows_diff_lines() {
        let req = ApprovalRequest {
            tool: "write_file".into(),
            args_json: r#"{"path":"a.txt"}"#.into(),
            category: flashagent_core::Category::Write,
            diff: Some("--- a/a.txt\n+++ b/a.txt\n-old\n+new\n".into()),
        };
        let card = approval_card(&req);
        assert!(card.iter().any(|(k, t)| *k == LineKind::Diff && t.contains("+new")));
        assert!(card.iter().any(|(k, t)| *k == LineKind::ToolError && t.contains("-old")));
        assert!(card.iter().any(|(_, t)| t.contains("[Allow]")));
        assert!(card.iter().any(|(_, t)| t.contains("[Deny]")));

        let deny_card = approval_card_with_selection(&req, Decision::Deny);
        assert!(deny_card.iter().any(|(_, t)| t.contains("❯ [Deny]")));
    }

    #[test]
    fn reasoning_deltas_merge_into_one_block() {
        let mut v = ChatView::default();
        for w in ["thinking", " about", " task"] {
            v.on_event(&LoopEvent::ReasoningDelta(w.into()));
        }
        v.on_event(&LoopEvent::TurnDelta("Response".into()));
        v.on_event(&LoopEvent::TurnDelta(" ready".into()));

        let lines = v.render(80);
        let reasoning: Vec<_> = lines.iter().filter(|(k, _)| *k == LineKind::Reasoning).collect();
        assert_eq!(reasoning.len(), 3, "reasoning rendered as boxed container: {reasoning:?}");
        assert!(reasoning[0].1.contains("Thinking"));
        assert!(reasoning[1].1.contains("thinking about task"));
        assert!(reasoning[2].1.contains('╰'));
        let assistant: Vec<_> = lines.iter().filter(|(k, _)| *k == LineKind::Assistant).collect();
        assert_eq!(assistant.len(), 1);
        assert!(assistant[0].1.contains("Response ready"));
    }

    #[test]
    fn interleaved_reasoning_keeps_assistant_line() {
        let mut v = ChatView::default();
        v.on_event(&LoopEvent::TurnDelta("Hel".into()));
        v.on_event(&LoopEvent::ReasoningDelta("thinking".into()));
        v.on_event(&LoopEvent::TurnDelta("lo!".into()));
        v.on_event(&LoopEvent::ReasoningDelta(" more".into()));
        v.on_event(&LoopEvent::TurnDelta(" All good".into()));

        let assistant: Vec<_> = v.lines.iter().filter(|l| l.kind == LineKind::Assistant).collect();
        assert_eq!(assistant.len(), 1, "no per-word fragmentation");
        assert_eq!(assistant[0].text, "Hello! All good");
        let reasoning: Vec<_> = v.lines.iter().filter(|l| l.kind == LineKind::Reasoning).collect();
        assert_eq!(reasoning.len(), 1, "reasoning deltas merge into one block");
        assert_eq!(reasoning[0].text, "thinking more");
        // The assistant line is still live (appends continue after reasoning).
        assert_eq!(v.streaming, Some(0));
        assert_eq!(v.settled_boundary(), 0);
    }

    #[test]
    fn a_path_inside_the_project_is_shown_relative_to_it() {
        // "Read /tmp/claude-1000/-home.../audit_proj/main.rs" was all prefix.
        let cwd = std::env::current_dir().unwrap().to_string_lossy().to_string();
        assert_eq!(format_cmd(&format!("Read {cwd}/src/main.rs")), "Read src/main.rs");
        assert_eq!(format_cmd(&format!("cat {cwd}")), "cat .");
        assert_eq!(format_cmd("Read /etc/hosts"), "Read /etc/hosts", "paths elsewhere are left alone");
    }

    #[test]
    fn a_headed_edit_card_still_says_what_it_did_to_the_tree() {
        // The header replaced the card, so "Edited main.rs +1 -1" was lost:
        // the model's intention survived and the fact did not.
        let mut chat = ChatView::default();
        chat.push_user("add a line");
        chat.on_event(&LoopEvent::ToolStarted {
            id: "e".into(),
            name: "edit_file".into(),
            args_json: r#"{"header":"Add a second println to main","path":"src/main.rs","edits":[{"old_string":"one","new_string":"one\ntwo"}]}"#.into(),
        });
        chat.on_event(&LoopEvent::ToolFinished { id: "e".into(), is_error: false, result_len: 1, result: Some("ok".into()) });
        let (settled, live) = chat.render_split(110, ReasoningExpansion::default());
        let dump: String = settled.iter().chain(live.iter()).map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Add a second println to main"), "{dump}");
        assert!(dump.contains("main.rs +2 -1"), "the facts are gone:\n{dump}");

        // A header that names the file gets only what it did not say.
        let mut chat = ChatView::default();
        chat.push_user("edit it");
        chat.on_event(&LoopEvent::ToolStarted {
            id: "e".into(),
            name: "edit_file".into(),
            args_json: r#"{"header":"Fix the loop in main.rs","path":"main.rs","edits":[{"old_string":"one","new_string":"two"}]}"#.into(),
        });
        let (settled, live) = chat.render_split(110, ReasoningExpansion::default());
        let dump: String = settled.iter().chain(live.iter()).map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Fix the loop in main.rs"), "{dump}");
        assert!(dump.contains("+1 -1"), "{dump}");
        assert!(!dump.contains("main.rs · main.rs"), "said twice:\n{dump}");
    }

    #[test]
    fn a_header_that_already_names_the_file_is_not_made_to_repeat_it() {
        let mut chat = ChatView::default();
        chat.push_user("read it");
        chat.on_event(&LoopEvent::ToolStarted {
            id: "r".into(),
            name: "read_file".into(),
            args_json: r#"{"header":"Read main.rs","path":"main.rs"}"#.into(),
        });
        let (settled, live) = chat.render_split(110, ReasoningExpansion::default());
        let dump: String = settled.iter().chain(live.iter()).map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Read main.rs"), "{dump}");
        assert!(!dump.contains("main.rs · main.rs"), "said twice:\n{dump}");
    }

    #[test]
    fn a_headed_shell_card_names_the_command() {
        let mut chat = ChatView::default();
        chat.push_user("list files");
        chat.on_event(&LoopEvent::ToolStarted {
            id: "s".into(),
            name: "run_shell".into(),
            args_json: r#"{"header":"List the files in the project","command":"ls -1"}"#.into(),
        });
        let (settled, live) = chat.render_split(110, ReasoningExpansion::default());
        let dump: String = settled.iter().chain(live.iter()).map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("List the files in the project"), "{dump}");
        assert!(dump.contains("ls -1"), "{dump}");
    }

    #[test]
    fn listing_the_working_directory_reads_as_a_sentence() {
        // It showed "Searched search": the fallback was the word "search",
        // and a listing of the working directory has neither path nor
        // pattern to name.
        let mut chat = ChatView::default();
        chat.push_user("look around");
        chat.on_event(&LoopEvent::ToolStarted { id: "a".into(), name: "list_dir".into(), args_json: "{}".into() });
        chat.on_event(&LoopEvent::ToolFinished { id: "a".into(), is_error: false, result_len: 1, result: Some("x".into()) });
        let (settled, live) = chat.render_split(100, ReasoningExpansion::default());
        let dump: String = settled.iter().chain(live.iter()).map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Searched the project"), "{dump}");
        assert!(!dump.contains("Searched search"), "{dump}");
    }

    #[test]
    fn the_labels_we_write_ourselves_follow_the_conversation() {
        // A Russian chat showed "Thought: Analyzing Request": the fallback
        // stage names were English constants with no way out.
        assert_eq!(localize_stage("Analyzing Request", "ru"), "Разбираю запрос");
        assert_eq!(localize_stage("Planning Implementation", "ru"), "Планирую реализацию");
        assert_eq!(localize_stage("Analyzing Request", "en"), "Analyzing Request");
        assert_eq!(
            localize_stage("Formulating Response (part 2)", "ru"),
            "Формулирую ответ (часть 2)",
            "the repeat suffix is ours too"
        );
    }

    #[test]
    fn the_models_own_words_are_left_alone() {
        // A stage lifted out of the model's reasoning is a quote. Translating
        // it would put words in its mouth.
        assert_eq!(
            localize_stage("Checking the apply lines in Close", "ru"),
            "Checking the apply lines in Close"
        );
    }

    #[test]
    fn a_russian_chat_shows_russian_stages() {
        let mut chat = ChatView::default();
        chat.set_language("ru");
        chat.push_user("Осмотри проект и расскажи что он делает");
        chat.on_event(&LoopEvent::ReasoningDelta("Сначала посмотрю структуру проекта.".into()));
        chat.on_event(&LoopEvent::TurnDelta("Готово.".into()));
        chat.on_event(&LoopEvent::Done(DoneReason::Completed));
        let (settled, live) = chat.render_split(100, ReasoningExpansion::default());
        let dump: String = settled.iter().chain(live.iter()).map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Разбираю запрос"), "{dump}");
        assert!(!dump.contains("Analyzing Request"), "{dump}");
    }

    #[test]
    fn reasoning_stage_tracks_labeled_steps() {
        // Numbered steps: latest wins.
        let t = "Thinking Process:\n1. Analyze the Request: the user said hi\n2. Determine the Goal: be polite";
        assert_eq!(reasoning_stage(t).as_deref(), Some("Determine the Goal"));
        // Bullet steps too.
        assert_eq!(
            reasoning_stage("* Examine Available Tools: read_file, grep").as_deref(),
            Some("Examine Available Tools")
        );
        // Bracketed stage markers: [Stage: Name]
        let t_stage = "Some freeform thinking...\n[Stage: Exploring Codebase]\nLooking at files...";
        assert_eq!(reasoning_stage(t_stage).as_deref(), Some("Exploring Codebase"));
        // Heading stage markers: ### Name
        let t_heading = "Thinking...\n### Planning Architecture\nDetails here";
        assert_eq!(reasoning_stage(t_heading).as_deref(), Some("Planning Architecture"));
        // Real LM Studio Gemma 4 output:
        let t_gemma = "Thinking Process:\n\n\
                       1.  **Analyze the Request:** The user asked to \"Say hello in 2 words.\"\n\
                       2.  **Identify the Goal:** Provide a greeting.\n\
                       5.  **Final Output Generation.**";
        assert_eq!(reasoning_stage(t_gemma).as_deref(), Some("Final Output Generation"));

        // User's bold section heading style:
        let t_user = "**Updating Welcome Card Functions**\n\
                      The existing welcome_card function needs modification...\n\n\
                      **Constructing Welcome Card Display**\n\
                      The function initializes a welcome card...\n\n\
                      **Displaying Runtime Context**\n\
                      The current working directory, along with the active execution mode...";
        assert_eq!(reasoning_stage(t_user).as_deref(), Some("Displaying Runtime Context"));

        // Fallback: free-form reasoning has no labels.
        assert_eq!(reasoning_stage("just thinking about task aloud"), None);
        // A bare `Note: ...` sentence is not a step (no number/bullet).
        assert_eq!(reasoning_stage("Note: this is just a long sentence with a colon"), None);
    }

    #[test]
    fn collapsed_preview_shows_current_stage() {
        let mut v = ChatView::default();
        v.on_event(&LoopEvent::ReasoningDelta(
            "Thinking Process:\n1. Analyze the Request: user said hi\n2. Determine the Goal: ack".into(),
        ));
        let (_, live) = v.render_split(200, false);
        assert!(live.iter().any(|(_, t)| t.contains("Determine the Goal") && (t.contains('›') || t.contains('>'))), "live: {live:?}");
        assert!(!live.iter().any(|(_, t)| t.contains("Analyze the Request")));

        // After task completion (Done event), it shows the finished thought summary.
        v.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));
        let (settled, _) = v.render_split(200, false);
        assert!(settled.iter().any(|(_, t)| t.contains("Determine the Goal") && (t.contains('›') || t.contains('>'))));

        // Fallback: free-form reasoning shows clean status without dumping sentences.
        let mut v2 = ChatView::default();
        v2.on_event(&LoopEvent::ReasoningDelta("freeform thoughts about task".into()));
        let (_, live2) = v2.render_split(100, false);
        assert!(live2.iter().any(|(_, t)| (t.contains("Thinking") || t.contains("Thought")) && (t.contains('›') || t.contains('>'))));
        v2.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));
        let (settled2, _) = v2.render_split(100, false);
        assert!(settled2.iter().any(|(_, t)| (t.contains("Thought") || t.contains("Thinking")) && (t.contains('›') || t.contains('>'))));
    }

    #[test]
    fn restore_line_color_reapplies_after_embedded_resets() {
        let prefix = "\x1b[38;2;120;120;120m";
        // bold span closes with a full reset — color must come back after it.
        let s = restore_line_color(&format!("{prefix}before\x1b[1mbold\x1b[0mafter"), prefix);
        assert!(s.contains("\x1b[0m\x1b[38;2;120;120;120mafter"));
        // foreground-only reset (from md code spans) re-applied too.
        let s2 = restore_line_color("a\x1b[39mb", prefix);
        assert!(s2.ends_with("\x1b[39m\x1b[38;2;120;120;120mb"));
    }

    #[test]
    fn clip_ansi_keeps_sequences_and_width() {
        // Sequence survives, visible width respected.
        let s = clip_ansi("ab\x1b[39mcd", 3);
        assert_eq!(s, "ab\x1b[39mc");
        // Cut inside styled text: sequence is NOT literalized.
        assert!(!clip_ansi("\x1b[1mbold text\x1b[0m here", 6).contains("[0m here"));
        assert_eq!(clip_ansi("plain", 10), "plain");
        assert!(clip_ansi("\x1b[1mx\x1b[0m", 0).contains('\x1b'));
    }

    #[test]
    fn reasoning_expansion_modes_last_and_all() {
        let mut v = ChatView::default();
        // Turn 1
        v.push_user("msg 1");
        v.on_event(&LoopEvent::ReasoningDelta("Thinking Process:\n1. Plan: step 1\ndetails of turn 1".into()));
        v.on_event(&LoopEvent::TurnDelta("answer 1".into()));
        v.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));

        // Turn 2
        v.push_user("msg 2");
        v.on_event(&LoopEvent::ReasoningDelta("Thinking Process:\n1. Plan: step 2\ndetails of turn 2".into()));
        v.on_event(&LoopEvent::TurnDelta("answer 2".into()));
        v.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));

        // 1. None: both collapsed (inner details hidden)
        let (all, _) = v.render_split(200, ReasoningExpansion::none());
        assert!(all.iter().all(|(_, t)| !t.contains("details of turn 1")));
        assert!(all.iter().all(|(_, t)| !t.contains("details of turn 2")));

        // 2. Last only: turn 1 collapsed, turn 2 expanded
        let (all_last, _) = v.render_split(200, ReasoningExpansion::last_only());
        assert!(all_last.iter().all(|(_, t)| !t.contains("details of turn 1")));
        assert!(all_last.iter().any(|(_, t)| t.contains("details of turn 2")));

        // 3. All: both expanded
        let (all_exp, _) = v.render_split(200, ReasoningExpansion::all());
        assert!(all_exp.iter().any(|(_, t)| t.contains("details of turn 1")));
        assert!(all_exp.iter().any(|(_, t)| t.contains("details of turn 2")));
    }

    #[tokio::test]
    async fn tui_gate_roundtrip() {
        let gate = TuiGate::new();
        let g2 = gate.clone();
        let req = ApprovalRequest {
            tool: "run_shell".into(),
            args_json: "{}".into(),
            category: flashagent_core::Category::Shell,
            diff: None,
        };
        let handle = tokio::spawn(async move { g2.approve(&req).await });
        // Wait until pending shows up, then answer.
        for _ in 0..100 {
            if gate.pending().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(gate.pending().is_some());
        gate.respond(Decision::Allow);
        assert_eq!(handle.await.unwrap(), Decision::Allow);
        assert!(gate.pending().is_none());
    }

    #[test]
    fn extract_thinking_from_text_parses_tags_and_steps() {
        // XML think tags
        let (think, ans) = extract_thinking_from_text("<think>internal plan</think>\nActual response");
        assert_eq!(think.as_deref(), Some("internal plan"));
        assert_eq!(ans, "Actual response");

        // Unclosed think tag
        let (think2, ans2) = extract_thinking_from_text("<think>streaming in progress");
        assert_eq!(think2.as_deref(), Some("streaming in progress"));
        assert_eq!(ans2, "");

        // Thinking Process with steps and answer
        let text_with_steps = "Thinking Process:\n\
                               1. Analyze the Request: foo\n\
                               2. Determine the Goal: bar\n\n\
                               Hello, user!";
        let (think3, ans3) = extract_thinking_from_text(text_with_steps);
        assert!(think3.is_some());
        assert!(think3.unwrap().contains("Determine the Goal"));
        assert_eq!(ans3, "Hello, user!");

        // Plain text
        let (think4, ans4) = extract_thinking_from_text("Just a normal answer");
        assert!(think4.is_none());
        assert_eq!(ans4, "Just a normal answer");
    }

    #[test]
    fn duplicate_reasoning_in_assistant_text_is_deduplicated() {
        let mut v = ChatView::default();
        v.push_user("do something");
        // Native reasoning from LLM backend
        v.on_event(&LoopEvent::ReasoningDelta("1. Analyze the Request: goal".into()));
        // Model also repeats Thinking Process in TurnDelta
        v.on_event(&LoopEvent::TurnDelta(
            "Thinking Process:\n1. Analyze the Request: duplicate\n2. Determine the Goal: duplicate\n\nReal Answer".into(),
        ));
        v.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));

        let (settled, _) = v.render_split(200, ReasoningExpansion::none());
        // Thinking block appears once (collapsed)
        let thinking_lines: Vec<_> = settled.iter().filter(|(k, _)| *k == LineKind::Reasoning).collect();
        assert_eq!(thinking_lines.len(), 1);
        // The duplicate text from TurnDelta was not printed as assistant text
        assert!(!settled.iter().any(|(_, t)| t.contains("duplicate")));
        // Real answer is rendered as Assistant
        assert!(settled.iter().any(|(k, t)| *k == LineKind::Assistant && t.contains("Real Answer")));
    }

    #[test]
    fn text_only_thinking_is_rendered_as_collapsible_reasoning() {
        let mut v = ChatView::default();
        v.push_user("question");
        // Model without native reasoning outputs Thinking Process directly in TurnDelta
        v.on_event(&LoopEvent::TurnDelta(
            "Thinking Process:\n1. Analyze the Request: text model\n\nFinal direct answer".into(),
        ));
        v.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));

        let (settled, _) = v.render_split(200, ReasoningExpansion::none());
        // Thinking process was converted to LineKind::Reasoning and collapsed
        assert!(settled.iter().any(|(k, t)| *k == LineKind::Reasoning && t.contains("Analyze the Request") && (t.contains('›') || t.contains('>'))));
        // Final direct answer is rendered as Assistant
        assert!(settled.iter().any(|(k, t)| *k == LineKind::Assistant && t.contains("Final direct answer")));
    }

    #[test]
    fn update_or_push_system_overwrites_matching_prefix() {
        let mut v = ChatView::default();
        v.update_or_push_system("[Reasoning view:", "[Reasoning view: expanded for LAST]");
        assert_eq!(v.lines.len(), 1);
        assert_eq!(v.lines[0].text, "[Reasoning view: expanded for LAST]");

        // Second update with same prefix overwrites in place
        v.update_or_push_system("[Reasoning view:", "[Reasoning view: expanded for ALL]");
        assert_eq!(v.lines.len(), 1);
        assert_eq!(v.lines[0].text, "[Reasoning view: expanded for ALL]");

        // Different prefix adds new line
        v.update_or_push_system("[Thinking effort set to:", "[Thinking effort set to: high]");
        assert_eq!(v.lines.len(), 2);
        assert_eq!(v.lines[1].text, "[Thinking effort set to: high]");

        // Updating first prefix again still updates line 0 without moving
        v.update_or_push_system("[Reasoning view:", "[Reasoning view: collapsed]");
        assert_eq!(v.lines.len(), 2);
        assert_eq!(v.lines[0].text, "[Reasoning view: collapsed]");
        assert_eq!(v.lines[1].text, "[Thinking effort set to: high]");
    }

    #[test]
    fn update_or_push_turn_system_retains_per_turn_recaps() {
        let mut v = ChatView::default();
        // Turn 1
        v.push_user("Turn 1 user query");
        v.push_line(LineKind::Assistant, "Turn 1 assistant answer");
        v.update_or_push_turn_system("recap:", "recap: Turn 1 recap");

        assert_eq!(v.lines.len(), 3);
        assert_eq!(v.lines[2].text, "recap: Turn 1 recap");

        // Turn 2
        v.push_user("Turn 2 user query");
        v.push_line(LineKind::Assistant, "Turn 2 assistant answer");
        v.update_or_push_turn_system("recap:", "recap: Turn 2 recap");

        // Must have 6 lines: Turn 1 recap is PRESERVED, Turn 2 recap is added at the end!
        assert_eq!(v.lines.len(), 6);
        assert_eq!(v.lines[2].text, "recap: Turn 1 recap");
        assert_eq!(v.lines[5].text, "recap: Turn 2 recap");

        // Updating Turn 2 recap in-place should only touch Turn 2 recap
        v.update_or_push_turn_system("recap:", "recap: Turn 2 updated recap");
        assert_eq!(v.lines.len(), 6);
        assert_eq!(v.lines[2].text, "recap: Turn 1 recap");
        assert_eq!(v.lines[5].text, "recap: Turn 2 updated recap");
    }

    #[test]
    fn update_welcome_card_replaces_card_and_preserves_notices() {
        let mut v = ChatView::default();
        v.push_line(LineKind::System, "banner-v1");
        v.update_or_push_system("[Verbose mode:", "[Verbose mode: last]");
        assert!(!v.has_user_message());

        // Update card to banner-v2
        let new_card = vec![(LineKind::System, "banner-v2".to_string())];
        v.update_welcome_card(new_card);

        // Verify banner-v2 is in the lines
        assert!(v.lines.iter().any(|l| l.text.contains("banner-v2")));
        assert!(!v.lines.iter().any(|l| l.text.contains("banner-v1")));
        // Verify subsequent notice was preserved at the end
        assert_eq!(v.lines.last().unwrap().text, "[Verbose mode: last]");

        // After user message is sent, update_welcome_card does nothing
        v.push_user("hello");
        assert!(v.has_user_message());
        let new_card_c = vec![(LineKind::System, "banner-v3".to_string())];
        v.update_welcome_card(new_card_c);
        assert!(!v.lines.iter().any(|l| l.text.contains("banner-v3")));
    }

    #[test]
    fn update_or_push_system_does_not_break_active_streaming_reasoning() {
        let mut v = ChatView::default();
        v.update_or_push_system("[Thinking effort set to:", "[Thinking effort set to: low]");
        v.on_event(&LoopEvent::ReasoningDelta("part 1".into()));
        assert_eq!(v.lines.len(), 2);
        assert_eq!(v.lines[1].text, "part 1");

        // In-place update to line 0 must not reset streaming_reasoning
        v.update_or_push_system("[Thinking effort set to:", "[Thinking effort set to: high]");
        v.on_event(&LoopEvent::ReasoningDelta(" part 2".into()));
        // Still 2 lines: part 2 merged into line 1, not split into line 2
        assert_eq!(v.lines.len(), 2);
        assert_eq!(v.lines[1].text, "part 1 part 2");
    }

    #[test]
    fn welcome_card_normalizes_cwd_with_slash() {
        let card = welcome_card("test-model", Some("128k"), "~FlashAgent", "Manual", 1, Some("high"), 80);
        let joined = card.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("~/FlashAgent"), "welcome card must contain '~/FlashAgent', got: {joined}");
    }

    #[test]
    fn expanded_reasoning_borders_have_matching_width() {
        let mut v = ChatView::default();
        v.lines.push(ChatLine::new(LineKind::Reasoning, "Analyzing the request and formulating a thorough step-by-step resolution."));
        let (settled, live) = v.render_split(80, ReasoningExpansion { all: true, last: true });
        let all: Vec<_> = settled.into_iter().chain(live).collect();
        assert!(all.len() >= 3);
        let top_line = &all[0].1;
        let bot_line = &all[all.len() - 1].1;
        let top_vis = visible_width(top_line);
        let bot_vis = visible_width(bot_line);
        assert_eq!(top_vis, bot_vis, "top and bottom borders must have identical visible width: top={top_vis}, bot={bot_vis}");
        for line in &all[1..all.len() - 1] {
            let vis = visible_width(&line.1);
            assert!(vis <= top_vis, "content line {vis} must not exceed box width {top_vis}");
        }
    }

    #[test]
    fn the_thinking_face_never_changes_width() {
        let mut seen = std::collections::HashSet::new();
        for tick in 0..200usize {
            let face = thinking_face(tick);
            assert_eq!(visible_width(face), 5, "status line would jitter: {face}");
            seen.insert(face);
        }
        assert!(seen.len() > 1, "the face must actually animate");
        // Each expression must hold long enough to be seen: repaints are
        // event-driven, so a one-frame blink is invisible in practice.
        let cycle: Vec<&str> = (0..24).map(thinking_face).collect();
        let shortest = cycle
            .chunk_by(|a, b| a == b)
            .map(|run| run.len())
            .min()
            .unwrap_or(0);
        assert!(shortest >= 4, "an expression lasting {shortest} frames would be missed");
    }

    #[test]
    fn a_tool_line_reads_as_the_model_explained_it() {
        let mut v = ChatView::default();
        let args = r#"{"header":"Add the missing null check to parser.rs","path":"src/parser.rs","edits":[]}"#;
        v.on_event(&LoopEvent::ToolStarted {
            id: "1".into(),
            name: "edit_file".into(),
            args_json: args.into(),
        });
        let collapsed = |v: &ChatView| -> String {
            v.render_split(120, ReasoningExpansion::default())
                .0
                .iter()
                .chain(v.render_split(120, ReasoningExpansion::default()).1.iter())
                .map(|(_, t)| strip_ansi(t))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let running = collapsed(&v);
        assert!(running.contains("Add the missing null check to parser.rs"), "{running}");
        assert!(!running.contains("Editing"), "the header replaces the generated line: {running}");

        // And when the call fails, the same sentence says so.
        v.on_event(&LoopEvent::ToolFinished {
            id: "1".into(),
            is_error: true,
            result_len: 5,
            result: Some("error: no such file".into()),
        });
        // The expanded diff card renders under the line, so look at the whole
        // frame rather than assuming a row index.
        let failed = collapsed(&v);
        assert!(
            failed.contains("Failed to add the missing null check to parser.rs"),
            "{failed}"
        );
    }

    #[test]
    fn a_call_without_a_header_keeps_the_built_in_line() {
        let mut v = ChatView::default();
        v.on_event(&LoopEvent::ToolStarted {
            id: "1".into(),
            name: "run_shell".into(),
            args_json: r#"{"command":"cargo test"}"#.into(),
        });
        let line = strip_ansi(&v.render(120)[0].1);
        assert!(line.contains("Running"), "{line}");
        assert!(line.contains("cargo test"), "{line}");
    }

    #[test]
    fn an_identifier_is_not_lower_cased_into_nonsense() {
        assert_eq!(lower_first("Add a null check"), "add a null check");
        assert_eq!(lower_first("MEMORY.md needs a line"), "MEMORY.md needs a line");
        assert_eq!(lower_first(""), "");
    }

    #[test]
    fn a_line_built_from_facts_drops_whole_facts_when_narrow() {
        let parts = vec![
            ("gemma-4-e2b".to_string(), "\x1b[31mgemma-4-e2b\x1b[0m".to_string()),
            ("auto".to_string(), "\x1b[32mauto\x1b[0m".to_string()),
            ("64k".to_string(), "\x1b[33m64k\x1b[0m".to_string()),
        ];
        let sep = " · ";

        // Everything fits.
        let wide = strip_ansi(&fit_parts(&parts, sep, 40));
        assert_eq!(wide, "gemma-4-e2b · auto · 64k");

        // Only the first two: the third is dropped whole, not cut in half.
        let narrow = strip_ansi(&fit_parts(&parts, sep, 20));
        assert_eq!(narrow, "gemma-4-e2b · auto");

        // And the colour codes are not counted as width.
        assert!(visible_width(&fit_parts(&parts, sep, 20)) <= 20);

        // Nothing fits: an empty line rather than a broken one.
        assert_eq!(fit_parts(&parts, sep, 3), "");
    }

    #[test]
    fn the_welcome_card_never_overflows_a_narrow_terminal() {
        for width in [30usize, 36, 46, 56, 80, 120] {
            let card = welcome_card_responsive_opts(
                "gemma-4-e2b-it-qat@q4_k_xl",
                "/home/someone/projects/flashagent",
                "Accept Edits",
                0,
                Some("auto [off, on]"),
                Some("64k ctx"),
                width,
                24,
                0,
                true,
                MascotMood::Happy,
            );
            for (_, line) in &card {
                assert!(
                    visible_width(line) <= width,
                    "{width} cols: row is {} wide: {:?}",
                    visible_width(line),
                    strip_ansi(line)
                );
            }
        }
    }

    #[test]
    fn mascot_lines_are_a_fixed_width_grid() {
        for mood in [MascotMood::Checking, MascotMood::Happy, MascotMood::Offline] {
            for tick in [0usize, 19, 46, 47, 60] {
                for line in &mascot_swift_lines_mood(tick, mood) {
                    assert_eq!(
                        visible_width(line),
                        16,
                        "every mascot row is 16 columns so the rows stay aligned: {line}"
                    );
                }
            }
        }
    }

    #[test]
    fn mascot_blinks_and_breathes() {
        let open = mascot_swift_lines_mood(0, MascotMood::Happy);
        let blink = mascot_swift_lines_mood(46, MascotMood::Happy);
        // Row 2 carries the eyes (pixel rows 4-5).
        assert!(open[2].contains("255;255;255"), "open eyes are white: {}", open[2]);
        assert!(!blink[2].contains("255;255;255"), "shut eyes show no white: {}", blink[2]);

        // Breathing moves the highlight colour on the top row without
        // touching the shape.
        let dim = mascot_swift_lines_mood(0, MascotMood::Happy);
        let bright = mascot_swift_lines_mood(18, MascotMood::Happy);
        assert_ne!(dim[0], bright[0], "the highlight must change over the breath cycle");
        assert_eq!(visible_width(&dim[0]), visible_width(&bright[0]));
    }

    #[test]
    fn mascot_face_reports_the_server_state() {
        let happy = mascot_swift_lines_mood(0, MascotMood::Happy);
        let offline = mascot_swift_lines_mood(0, MascotMood::Offline);
        let checking = mascot_swift_lines_mood(0, MascotMood::Checking);
        assert!(happy != offline && happy != checking && checking != offline);
        // Offline shuts the eyes and drains the colour; the body colour of a
        // reachable server must not appear.
        assert!(!offline.concat().contains("138;180;248"), "offline must not use the healthy blue");
        assert!(happy.concat().contains("138;180;248"));
    }

    #[test]
    fn mascot_repaints_only_when_it_changed() {
        // The card is rebuilt on these ticks; at 12.5 ticks a second, doing it
        // every tick would repaint the screen for nothing.
        let repaints = (0..100).filter(|t| mascot_needs_repaint(*t)).count();
        assert!(repaints > 4, "the mascot must actually animate: {repaints}");
        assert!(repaints < 40, "too many card rebuilds: {repaints}");
        assert!(mascot_needs_repaint(46), "the blink must trigger a repaint");
    }
    #[test]
    fn test_render_markdown_tables_headings_and_lists() {
        let md_input = "## Stack\n\n\
                        | Layer | Decision |\n\
                        |---|---|\n\
                        | Core | Rust |\n\
                        | UI | wgpu |\n\n\
                        ## Architecture\n\
                        - First item\n\
                        - Second item with **bold** text";

        let rendered = render_markdown_text(md_input, 80);
        let joined = rendered.join("\n");

        // Headings must NOT have raw ##
        assert!(!joined.contains("## Stack"));
        assert!(joined.contains("◈ Stack"));
        assert!(joined.contains("◈ Architecture"));

        // Tables must have unicode box drawing borders and header
        assert!(joined.contains('╭'));
        assert!(joined.contains('├'));
        assert!(joined.contains('╰'));
        assert!(joined.contains("Layer"));
        assert!(joined.contains("Decision"));
        assert!(joined.contains("Core"));
        assert!(joined.contains("Rust"));

        // Lists must have bullets
        assert!(joined.contains('•'));
        assert!(joined.contains("First item"));
        assert!(joined.contains("Second item"));
    }

    #[test]
    fn test_render_markdown_table_exact_border_alignment() {
        let md_input = "| Crate | Responsibility |\n\
                        |---|---|\n\
                        | core | Agent loop, modes, permissions, subagents, memory |\n\
                        | llm | Trait `LlmBackend`, adapters (OpenAI-compatible), multiparser |\n\
                        | tools | 9 builtin tools, MCP client, registry |\n\
                        | data | SQLite + FTS5, sessions, migrations |\n\
                        | app | Main binary |";

        let rendered = render_markdown_text(md_input, 100);
        assert!(!rendered.is_empty());

        // Every table border and content line must have the exact same visible width!
        let widths: Vec<usize> = rendered.iter().map(|l| visible_width(l)).collect();
        let expected_w = widths[0];
        for (idx, &w) in widths.iter().enumerate() {
            assert_eq!(
                w, expected_w,
                "Table line {idx} width ({w}) does not match expected width ({expected_w}):\n{}",
                rendered[idx]
            );
        }
    }

    #[test]
    fn test_render_markdown_code_block_exact_border_alignment() {
        let md_input = "```rust\nfn main() {\n    println!(\"Hello, world!\");\n}\n```";
        let width = 80;
        let rendered = render_markdown_text(md_input, width);
        assert_eq!(rendered.len(), 5); // top, 3 code lines, bottom

        for (idx, line) in rendered.iter().enumerate() {
            let vis_w = visible_width(line);
            assert_eq!(
                vis_w, width,
                "Code block line {idx} width ({vis_w}) does not match expected width ({width}):\n{line}"
            );
            let clean = strip_ansi(line);
            assert!(clean.ends_with('╮') || clean.ends_with('╯') || clean.ends_with('│'));
        }
    }

    #[test]
    fn test_render_markdown_advanced_lists_multiline_and_tasks() {
        let md_input = r#"- [x] Task 1 completed
- [ ] Task 2 in progress
1. **Core system**:
   Full implementation of agent loop and memory.
2. **LLM module**:
   Streaming and prefix caching support.
- Level 1
  - Level 2
1) Alternative numbering"#;

        let rendered = render_markdown_text(md_input, 80);
        let joined = rendered.join("\n");

        // Checkboxes rendered as unicode symbols
        assert!(joined.contains('☑'));
        assert!(joined.contains('☐'));

        // Nested lists have sub-bullet glyph
        assert!(joined.contains('•'));
        assert!(joined.contains('◦'));

        // Continuation lines properly grouped and aligned
        assert!(joined.contains("Full implementation of agent loop"));

        // Numbered list with paren supported
        assert!(joined.contains("1)"));
    }

    #[test]
    fn test_resolve_reasoning_stage_deduplication_and_progression() {
        let mut prior = Vec::new();

        // 1. First step before tools: analyzing request
        let s1 = resolve_reasoning_stage("The user wants to make a coding test", &prior, None, 0);
        assert_eq!(s1, "Understanding user request");
        prior.push(s1);

        // 2. Second step after search tool: even if text says "the user wants", it evaluates search results
        let explore_searches = ToolGroupKind::Explore { files: 0, searches: 2, last_target: "main.rs".into(), is_running: false };
        let s2 = resolve_reasoning_stage("The user wants me to check X. Found 2 matches.", &prior, Some(&explore_searches), 1);
        assert_eq!(s2, "Evaluating Search Results");
        prior.push(s2);

        // 3. Third step after file reads: analyzes file contents
        let explore_files = ToolGroupKind::Explore { files: 3, searches: 0, last_target: "lib.rs".into(), is_running: false };
        let s3 = resolve_reasoning_stage("The user wants X. Looking at lib.rs...", &prior, Some(&explore_files), 2);
        assert_eq!(s3, "Analyzing File Contents");
        prior.push(s3);

        // 4. Fourth step after more file reads: should NOT duplicate "Analyzing File Contents", must advance
        let s4 = resolve_reasoning_stage("The user wants X. Reading more files...", &prior, Some(&explore_files), 3);
        assert_ne!(s4, "Analyzing File Contents");
        assert_ne!(s4, "Understanding user request");
        assert_eq!(s4, "Deepening Code Search");
        prior.push(s4);

        // All 4 headers in this turn are completely unique!
        let unique_count = prior.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique_count, 4);
    }

    #[test]
    fn test_resolve_reasoning_stage_overrides_initial_marker_after_tools() {
        let prior = vec!["Analyzing Request".to_string()];
        let explore = ToolGroupKind::Explore { files: 0, searches: 2, last_target: "grep".into(), is_running: false };
        // Model explicitly outputs prompt example "**Understanding the Request**" after tools have run
        let stage = resolve_reasoning_stage("**Understanding the Request**\nI see the search results...", &prior, Some(&explore), 1);
        assert_eq!(stage, "Evaluating Search Results");
    }

    #[test]
    fn test_chat_view_last_assistant_text_multi_line() {
        let mut chat = ChatView::default();
        assert_eq!(chat.last_assistant_text(), None);

        chat.push_user("Hello");
        chat.push_line(LineKind::Assistant, "Line 1 of answer");
        chat.push_line(LineKind::Assistant, "Line 2 of answer");
        assert_eq!(chat.last_assistant_text(), Some("Line 1 of answer\nLine 2 of answer".into()));

        chat.push_user("Followup question");
        chat.push_line(LineKind::Assistant, "Followup answer");
        assert_eq!(chat.last_assistant_text(), Some("Followup answer".into()));
    }

    #[test]
    fn test_chat_view_truncate_to_last_user() {
        let mut chat = ChatView::default();
        assert!(!chat.truncate_to_last_user());

        chat.push_system("Welcome to FlashAgent");
        chat.push_user("Turn 1 Question");
        chat.push_line(LineKind::Assistant, "Turn 1 Answer");
        chat.push_system("  recap: Answered turn 1");

        chat.push_user("Turn 2 Question");
        chat.push_line(LineKind::Reasoning, "Thinking about turn 2...");
        chat.push_line(LineKind::Assistant, "Turn 2 Answer");
        chat.push_system("  recap: Answered turn 2");

        assert!(chat.truncate_to_last_user());
        // After truncation, last line must be the last user prompt!
        assert_eq!(chat.lines.last().map(|l| (l.kind, l.text.as_str())), Some((LineKind::User, "Turn 2 Question")));
        assert_eq!(chat.lines.len(), 5); // Welcome, Turn 1 Q, Turn 1 A, Recap 1, Turn 2 Q

        // Truncate again: still truncates to last user
        assert!(chat.truncate_to_last_user());
        assert_eq!(chat.lines.last().map(|l| (l.kind, l.text.as_str())), Some((LineKind::User, "Turn 2 Question")));
    }

    #[test]
    fn test_expanded_tools_render_split() {
        let mut chat = ChatView::default();
        chat.push_user("Run checks");
        // Command
        chat.on_event(&LoopEvent::ToolStarted {
            id: "t1".into(),
            name: "run_shell".into(),
            args_json: r#"{"command":"cargo check"}"#.into(),
        });
        chat.on_event(&LoopEvent::ToolFinished {
            id: "t1".into(),
            is_error: false,
            result_len: 20,
            result: Some("Finished dev profile".into()),
        });
        // Read file
        chat.on_event(&LoopEvent::ToolStarted {
            id: "t2".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"src/main.rs"}"#.into(),
        });
        chat.on_event(&LoopEvent::ToolFinished {
            id: "t2".into(),
            is_error: false,
            result_len: 25,
            result: Some("fn main() {\n    println!(\"hello\");\n}".into()),
        });
        // Directory analysis
        chat.on_event(&LoopEvent::ToolStarted {
            id: "t3".into(),
            name: "list_dir".into(),
            args_json: r#"{"path":".audit"}"#.into(),
        });
        chat.on_event(&LoopEvent::ToolFinished {
            id: "t3".into(),
            is_error: false,
            result_len: 30,
            result: Some("2026-09-07-a0-skeleton.md\n2026-09-08-a1-data.md".into()),
        });

        // 1. In collapsed mode: one line summaries
        let (settled_collapsed, live_collapsed) = chat.render_split(100, false);
        let all_collapsed: Vec<_> = settled_collapsed.into_iter().chain(live_collapsed).collect();
        let collapsed_text = all_collapsed.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        assert!(collapsed_text.contains("Ran") && collapsed_text.contains("cargo check"));
        assert!(collapsed_text.contains("Explored") || collapsed_text.contains("Read"));

        // 2. In expanded mode: rich visual cards
        let (settled_expanded, live_expanded) = chat.render_split(100, true);
        let all_expanded: Vec<_> = settled_expanded.into_iter().chain(live_expanded).collect();
        let expanded_text = all_expanded.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
        let plain_expanded = strip_ansi(&expanded_text);

        // Check command card prompt
        assert!(plain_expanded.contains("$ cargo check"));
        assert!(plain_expanded.contains("Finished dev profile"));

        // Check read file blue card
        assert!(plain_expanded.contains("Read") && plain_expanded.contains("src/main.rs"));
        assert!(expanded_text.contains("\x1b[48;2;22;38;60m"), "Read view must have soft blue background");

        // Check directory card
        assert!(plain_expanded.contains("Analyzed .audit"));
        assert!(plain_expanded.contains("2026-09-07-a0-skeleton.md"));
    }

    #[test]
    fn test_attach_turn_recap_preserves_turn_order() {
        let mut chat = ChatView::default();
        chat.push_user("First question");
        chat.on_event(&LoopEvent::TurnDelta("First answer".into()));
        chat.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));

        chat.push_user("Second question");
        chat.on_event(&LoopEvent::TurnDelta("Second answer".into()));

        // Late recap arriving for turn 1
        chat.attach_turn_recap(1, "recap: explained first question in detail");

        let (settled, live) = chat.render_split(80, false);
        let all: Vec<_> = settled.into_iter().chain(live).collect();
        let full_text = all.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");

        assert!(full_text.contains("recap: explained first question in detail"));
        let recap_pos = full_text.find("recap: explained first question in detail").unwrap();
        let second_user_pos = full_text.find("Second question").unwrap();
        assert!(recap_pos < second_user_pos, "Historical recap must attach before second user turn");
    }

    #[test]
    fn test_welcome_card_responsive_scales_to_terminal_dimensions() {
        // 1. Standard 80x24: 2 columns, height <= 14
        let card_80x24 = welcome_card_responsive(
            "test-model",
            "/home/user/project",
            "Accept Edits",
            3,
            Some("auto"),
            Some("128k"),
            80,
            24,
            0,
        );
        assert!(card_80x24.len() <= 14, "Standard card must be at most 14 lines, got {}", card_80x24.len());
        for line in &card_80x24 {
            let width = visible_width(&line.1);
            assert!(width <= 80, "Line width {} exceeds terminal width 80", width);
        }

        // 2. Compact 50x16: single column, height <= 14
        let card_50x16 = welcome_card_responsive(
            "test-model",
            "/home/user/project",
            "Accept Edits",
            3,
            Some("auto"),
            Some("128k"),
            50,
            16,
            0,
        );
        assert!(card_50x16.len() <= 14, "Compact card must be at most 14 lines, got {}", card_50x16.len());
        for line in &card_50x16 {
            let width = visible_width(&line.1);
            assert!(width <= 50, "Line width {} exceeds terminal width 50", width);
        }

        // 3. Ultra-compact 36x12: single column with minimal companion, height <= 12
        let card_36x12 = welcome_card_responsive(
            "test-model",
            "/home/user/project",
            "Accept Edits",
            3,
            Some("auto"),
            Some("128k"),
            36,
            12,
            0,
        );
        assert!(card_36x12.len() <= 12, "Ultra compact card must be at most 12 lines, got {}", card_36x12.len());
        for line in &card_36x12 {
            let width = visible_width(&line.1);
            assert!(width <= 36, "Line width {} exceeds terminal width 36", width);
        }
    }

    #[test]
    fn test_render_session_saved_card_closed_borders() {
        for w in [60, 80, 100, 120] {
            let card = render_session_saved_card("session_1789119858", w);
            assert_eq!(card.len(), 3);
            let expected_w = visible_width(&card[0]);
            assert!(expected_w <= w, "Card width {expected_w} exceeds terminal width {w}");
            for (idx, line) in card.iter().enumerate() {
                assert_eq!(
                    visible_width(line),
                    expected_w,
                    "Line {idx} width mismatch at terminal width {w}"
                );
            }
            assert!(card[0].ends_with("╮\x1b[0m"));
            assert!(card[1].ends_with("│\x1b[0m"));
            assert!(card[2].ends_with("╯\x1b[0m"));
            assert!(card[1].contains("session_1789119858"));
        }
    }
}



