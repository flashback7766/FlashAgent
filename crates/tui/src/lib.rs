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
mod text;
mod stages;
mod welcome;
mod markdown;
mod gates;
pub use text::*;
pub use stages::*;
pub use welcome::*;
pub use markdown::*;
pub use gates::*;
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
                    // Thinking lifted out of the answer text: no duration was
                    // ever measured, but it is plainly over — the answer is
                    // underneath it.
                    format!("  \x1b[1;38;2;194;231;255mThought:\x1b[0m \x1b[1;38;2;240;235;225m{stage}\x1b[0m {chevron}")
                };
                let budget = width.saturating_sub(4);
                let clipped_styled = clip_ansi(&text_styled, budget);
                target.push((LineKind::Reasoning, clipped_styled));
            } else {
                let trimmed_text = text.trim();
                // The expanded box carried "Thinking:" for the whole session,
                // including under a finished answer.
                let verb = if is_streaming { "Thinking" } else { "Thought" };
                let elapsed = match (is_streaming, secs) {
                    (false, Some(s)) => format!(" ({s}s)"),
                    _ => String::new(),
                };
                let title_vis = format!("{verb}: {stage}{elapsed}");
                let title_styled = format!(
                    "\x1b[1;38;2;194;231;255m{verb}:\x1b[0m \x1b[1;38;2;240;235;225m{stage}\x1b[0m\x1b[38;2;140;145;160m{elapsed}\x1b[0m"
                );
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
    fn a_finished_thought_is_not_still_called_thinking() {
        // Seen in a screenshot: the answer and its recap were on the screen
        // and the block above them still read "Thinking: Разбираю запрос".
        let mut v = ChatView::default();
        v.push_user("Hi!");
        v.on_event(&LoopEvent::ReasoningDelta("The user sent a greeting.".into()));
        v.on_event(&LoopEvent::TurnDelta("Hello!".into()));
        v.on_event(&LoopEvent::Done(DoneReason::Completed));
        let dump: String = v.render(100).iter().map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Thought:"), "{dump}");
        assert!(!dump.contains("Thinking:"), "{dump}");
    }

    #[test]
    fn thinking_that_came_out_of_the_answer_is_also_finished() {
        // A model that writes <think> into its content leaves no duration
        // behind, which is not a reason to claim it is still going.
        let mut v = ChatView::default();
        v.push_user("hi");
        v.on_event(&LoopEvent::TurnDelta("<think>weighing it up</think>Hello!".into()));
        v.on_event(&LoopEvent::Done(DoneReason::Completed));
        let dump: String = v.render(100).iter().map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Thought:"), "{dump}");
        assert!(!dump.contains("Thinking:"), "{dump}");
    }

    #[test]
    fn while_it_is_still_thinking_it_says_so() {
        let mut v = ChatView::default();
        v.push_user("hi");
        v.on_event(&LoopEvent::ReasoningDelta("still going".into()));
        let dump: String = v.render(100).iter().map(|(_, t)| strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(dump.contains("Thinking:"), "{dump}");
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

        // 6 lines: turn 1's recap is kept and turn 2's is appended.
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

        // Every border and content row has the same visible width.
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

        // All four headers in this turn are distinct.
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
        // After truncation the last line is the last user prompt.
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



