//! Terminal client binary: wires backend → loop → tools → permissions and
//! renders through `flashagent_tui`. Runs the stack in-process (svc IPC is a
//! later milestone).

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use flashagent_core::{
    build_system_prompt, AgentLoop, AppConfig, ContextUsage, Decision, DoneReason, LlmSource,
    LoopConfig, LoopEvent, PermissionMode, PermissionState, PermissionedTools, SystemPromptConfig,
};
use flashagent_llm::{ChatMessage, LlmBackend};
use flashagent_tools::{BuiltinTools, BuiltinToolsConfig};
use flashagent_tui::goal::{GoalBudgets, GoalLedger};
use flashagent_tui::MascotMood;
use flashagent_tui::{
    clip_ansi, pad_box_row, render_session_saved_card, restore_line_color,
    visible_width, welcome_card_responsive_opts, AutocompletePopup, ChatView, ConfirmSelect,
    ContextModal, LineKind, McpModal, McpModalAction, McpViewTab, PrefillTracker, ReasoningExpansion,
    RenderLine, SamplingAction, SamplingView, SelectItem, SelectMenu, SettingsAction, SettingsView,
    TuiGate, TuiQuestionGate, UiEvent, SPINNER,
};

#[derive(Default, Clone)]
struct QuestionUiState {
    selected_index: usize,
    write_in_text: String,
    is_writing: bool,
    selected_indices: std::collections::BTreeSet<usize>,
}

/// [`LlmSource`] over the OpenAI-compatible backend.
struct BackendSource(flashagent_llm::OpenAiCompat);

impl BackendSource {
    fn profile(&self) -> Option<flashagent_llm::thinking::ThinkingProfile> {
        self.0.profile()
    }

    fn discovery(&self) -> Option<flashagent_llm::ServerDiscovery> {
        self.0.discovery()
    }

    #[allow(dead_code)]
    fn model(&self) -> String {
        self.0.model()
    }

    fn set_model(&self, model: impl Into<String>) {
        self.0.set_model(model);
    }

    async fn discover_server(&self) -> Option<flashagent_llm::ServerDiscovery> {
        self.0.discover_server().await
    }
}

#[async_trait::async_trait]
impl LlmSource for BackendSource {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[flashagent_llm::ToolSpec],
    ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError>
    {
        self.0.stream(messages, tools).await
    }

    async fn turn_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[flashagent_llm::ToolSpec],
        options: &flashagent_llm::TurnOptions,
    ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError>
    {
        self.0.stream_with_options(messages, tools, options).await
    }
}

fn color(kind: LineKind) -> crossterm::style::Color {
    // Truecolor dark warm aesthetic (M3 Expressive / Charcoal & Amber / Sand).
    match kind {
        LineKind::User => crossterm::style::Color::Rgb { r: 245, g: 240, b: 232 },
        LineKind::Assistant => crossterm::style::Color::Rgb { r: 232, g: 227, b: 218 },
        LineKind::Reasoning => crossterm::style::Color::Rgb { r: 140, g: 135, b: 130 },
        LineKind::Tool => crossterm::style::Color::Rgb { r: 135, g: 185, b: 205 },
        LineKind::ToolError => crossterm::style::Color::Rgb { r: 230, g: 110, b: 95 },
        LineKind::Diff => crossterm::style::Color::Rgb { r: 145, g: 205, b: 140 },
        LineKind::System => crossterm::style::Color::Rgb { r: 225, g: 175, b: 95 },
    }
}

/// Print-and-forget renderer: settled lines are
/// printed to the terminal scrollback once and never redrawn; only the live
/// tail (streaming line, open tool, approval card, input) is repainted each
/// frame by cursoring up over it. Native scrollback works for free.
struct Renderer {
    /// Lines of the live tail currently on screen (to erase next frame).
    tail_height: u16,
    prev_width: u16,
    /// How many settled lines have already been printed to the scrollback.
    printed_settled: usize,
    /// Last rendered thinking mode — a flip needs a full repaint (settled
    /// reasoning was printed in the other form and must not linger).
    prev_expansion: Option<ReasoningExpansion>,
    /// Explicit request to clear and repaint all settled lines (e.g. in-place notice update).
    needs_reprint: bool,
    /// Distance in lines from the top of the tail down to the input prompt line.
    prev_cursor_tail_offset: u16,
    /// Vertical scroll offset (lines scrolled back from the bottom). 0 = anchored to bottom.
    scroll_offset: usize,
}

#[allow(dead_code)]
struct FrameState<'a> {
    input: &'a str,
    model: &'a str,
    context_window: Option<&'a str>,
    cwd: &'a str,
    mode: PermissionMode,
    is_goal_active: bool,
    /// Live `/goal` progress against its budgets, when one is running.
    goal_progress: Option<&'a str>,
    tip: Option<&'a str>,
    tip_animated: Option<&'a str>,
    tip_lines: Option<&'a [String]>,
    /// Token tracker to dynamically compute turn stats fitting into available space without truncating tips.
    token_tracker: Option<&'a TokenTracker>,
    thinking_effort: &'a str,
    reasoning_expand: ReasoningExpansion,
    tick_n: usize,
    running: bool,
    /// Seconds since turn start (for the working status line).
    elapsed_secs: u64,
    /// 80 ms steps of wall clock since the turn began — the mascot's face
    /// animates on this rather than on repaints, which are event-driven.
    face_phase: usize,
    /// Number of tokens generated strictly by the model in this turn.
    model_tokens: usize,
    /// Speed of token generation over a rolling 3-second window (tg_3s).
    tokens_per_sec: f64,
    /// Prompt cache reuse ratio (f_keep) from latest turn if reported.
    f_keep: Option<f64>,
    confirm_selection: Decision,
    question_state: Option<&'a QuestionUiState>,
    custom_placeholder: Option<&'a str>,
    suggested_prompt: Option<&'a str>,
    copy_toast: Option<&'a str>,
    prefill_status: Option<&'a str>,
    ttft_display: Option<&'a str>,
    background: Option<&'a str>,
    background_style: NoticeStyle,
    /// A release-channel switch waiting for a yes or no.
    channel_prompt: Option<&'a str>,
    context_warn_threshold: usize,
}

/// Tracks tokens generated strictly by the model in the current turn and
/// calculates generation speeds (tg and tg_3s), prompt tokens, prefill speed / TTFT,
/// and speculative decoding stats (MTP).
struct TokenTracker {
    model: String,
    total_model_tokens: usize,
    window: std::collections::VecDeque<(std::time::Instant, usize)>,
    last_f_keep: Option<f64>,
    turn_start_time: Option<std::time::Instant>,
    first_token_time: Option<std::time::Instant>,
    last_token_time: Option<std::time::Instant>,
    active_duration: std::time::Duration,
    prompt_tokens: Option<usize>,
    last_tg: Option<f64>,
    last_mtp: Option<flashagent_llm::MtpStats>,
    last_ttft: Option<std::time::Duration>,
    last_prefill_speed: Option<f64>,
    prefill_tracker: PrefillTracker,
    is_running: bool,
}

impl TokenTracker {
    fn new(model: String) -> Self {
        Self {
            model,
            total_model_tokens: 0,
            window: std::collections::VecDeque::new(),
            last_f_keep: None,
            turn_start_time: None,
            first_token_time: None,
            last_token_time: None,
            active_duration: std::time::Duration::ZERO,
            prompt_tokens: None,
            last_tg: None,
            last_mtp: None,
            last_ttft: None,
            last_prefill_speed: None,
            prefill_tracker: PrefillTracker::load_or_default(),
            is_running: false,
        }
    }

    fn on_turn_start(&mut self, model: String, estimated_prompt_tokens: usize) {
        self.model = model;
        self.total_model_tokens = 0;
        self.window.clear();
        self.turn_start_time = Some(std::time::Instant::now());
        self.first_token_time = None;
        self.last_token_time = None;
        self.active_duration = std::time::Duration::ZERO;
        self.prompt_tokens = if estimated_prompt_tokens > 0 {
            Some(estimated_prompt_tokens)
        } else {
            None
        };
        self.last_tg = None;
        self.last_mtp = None;
        self.last_ttft = None;
        self.last_prefill_speed = None;
        self.is_running = true;
    }

    fn on_delta(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        if self.first_token_time.is_none() {
            self.first_token_time = Some(now);
            if let Some(start) = self.turn_start_time {
                let ttft = now.duration_since(start);
                self.last_ttft = Some(ttft);
                let prompt = self.prompt_tokens.unwrap_or(0);
                let cached = (self.last_f_keep.unwrap_or(0.0) * prompt as f64) as usize;
                let eval = prompt.saturating_sub(cached).max(1);
                let spd = eval as f64 / ttft.as_secs_f64().max(0.001);
                self.last_prefill_speed = Some(spd);
                self.prefill_tracker.record(&self.model, prompt, cached, ttft);
            }
        }
        if let Some(last) = self.last_token_time {
            let dt = now.duration_since(last);
            if dt < std::time::Duration::from_millis(1500) {
                self.active_duration += dt;
            }
        }
        self.last_token_time = Some(now);

        let count = flashagent_llm::count_tokens(&self.model, text).max(1);
        self.total_model_tokens += count;
        self.window.push_back((now, count));
    }

    fn on_usage(&mut self, usage: &flashagent_llm::Usage) {
        if let (Some(c), Some(p)) = (usage.cached, usage.prompt) {
            if p > 0 {
                self.last_f_keep = Some((c as f64 / p as f64).clamp(0.0, 1.0));
                if let Some(ttft) = self.last_ttft {
                    self.prefill_tracker.record(&self.model, p as usize, c as usize, ttft);
                    let eval = (p as usize).saturating_sub(c as usize).max(1);
                    self.last_prefill_speed = Some(eval as f64 / ttft.as_secs_f64().max(0.001));
                }
            }
        }
        if let Some(p) = usage.prompt {
            if p > 0 {
                self.prompt_tokens = Some(p as usize);
            }
        }
        if let Some(mtp) = usage.mtp {
            self.last_mtp = Some(mtp);
        }
        if let Some(c) = usage.completion {
            if c > 0 {
                let comp = c as usize;
                if comp > self.total_model_tokens {
                    let diff = comp - self.total_model_tokens;
                    let now = std::time::Instant::now();
                    if self.first_token_time.is_none() {
                        self.first_token_time = Some(now);
                    }
                    if let Some(last) = self.last_token_time {
                        let dt = now.duration_since(last);
                        if dt < std::time::Duration::from_millis(1500) {
                            self.active_duration += dt;
                        }
                    }
                    self.last_token_time = Some(now);
                    self.window.push_back((now, diff));
                    self.total_model_tokens = comp;
                } else if comp < self.total_model_tokens {
                    self.total_model_tokens = comp;
                }
            }
        }
    }

    fn tg(&self) -> Option<f64> {
        if self.is_running {
            if self.total_model_tokens == 0 {
                return None;
            }
            let secs = self.active_duration.as_secs_f64();
            if secs >= 0.1 {
                Some(self.total_model_tokens as f64 / secs)
            } else if let Some(first) = self.first_token_time {
                let elapsed = std::time::Instant::now().duration_since(first).as_secs_f64();
                if elapsed >= 0.2 {
                    Some(self.total_model_tokens as f64 / elapsed)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            self.last_tg
        }
    }

    fn on_finished(&mut self) {
        self.is_running = false;
        if self.total_model_tokens > 0 {
            let secs = self.active_duration.as_secs_f64();
            if secs >= 0.05 {
                self.last_tg = Some(self.total_model_tokens as f64 / secs);
            } else if let (Some(first), Some(last)) = (self.first_token_time, self.last_token_time) {
                let dt = last.duration_since(first).as_secs_f64();
                if dt >= 0.05 {
                    self.last_tg = Some(self.total_model_tokens as f64 / dt);
                }
            }
        }
    }

    fn tg_3s(&mut self) -> f64 {
        let now = std::time::Instant::now();
        let cutoff = now.checked_sub(std::time::Duration::from_secs(3)).unwrap_or(now);

        while let Some(&(t, _)) = self.window.front() {
            if t < cutoff {
                self.window.pop_front();
            } else {
                break;
            }
        }

        if self.window.is_empty() {
            return 0.0;
        }

        let sum: usize = self.window.iter().map(|(_, n)| *n).sum();
        let earliest = self.window.front().map(|(t, _)| *t).unwrap_or(now);
        let dt = now.duration_since(earliest).as_secs_f64().max(0.25);
        sum as f64 / dt
    }

    #[allow(dead_code)]
    fn format_stats(&self) -> Option<String> {
        self.format_stats_width(120)
    }

    fn format_stats_width(&self, budget: usize) -> Option<String> {
        if budget < 10 {
            return None;
        }

        let mut parts = Vec::new();

        if let Some(p) = self.prompt_tokens {
            if p > 0 && budget >= 45 {
                let p_str = flashagent_core::ContextUsage::format_used_tokens(p);
                if budget >= 60 {
                    parts.push(format!("\x1b[38;2;160;175;200m{p_str} prompt\x1b[0m"));
                } else {
                    parts.push(format!("\x1b[38;2;160;175;200m{p_str}\x1b[0m"));
                }
            }
        }

        if let Some(ttft) = self.last_ttft {
            if budget >= 60 {
                let spd_str = if let Some(spd) = self.last_prefill_speed {
                    if spd >= 1000.0 {
                        format!("{:.1}k t/s prefill", spd / 1000.0)
                    } else {
                        format!("{:.0} t/s prefill", spd)
                    }
                } else {
                    "prefill".to_string()
                };
                parts.push(format!("\x1b[38;2;120;220;140mTTFT {:.2}s ({spd_str})\x1b[0m", ttft.as_secs_f64()));
            } else if budget >= 30 {
                parts.push(format!("\x1b[38;2;120;220;140mTTFT {:.2}s\x1b[0m", ttft.as_secs_f64()));
            } else {
                parts.push(format!("\x1b[38;2;120;220;140m{:.2}s\x1b[0m", ttft.as_secs_f64()));
            }
        }

        let speed = if self.is_running {
            self.tg()
        } else {
            self.last_tg
        };

        if let Some(s) = speed {
            if s > 0.0 && budget >= 18 {
                parts.push(format!("\x1b[38;2;145;205;140m{:.1} tg\x1b[0m", s));
            }
        }

        if let Some(mtp) = &self.last_mtp {
            if mtp.total_draft_tokens > 0 && budget >= 35 {
                let rate = mtp.acceptance_rate();
                if budget >= 50 {
                    parts.push(format!("\x1b[38;2;225;175;95mmtp: {:.0}%\x1b[0m", rate));
                } else {
                    parts.push(format!("\x1b[38;2;225;175;95m{:.0}%\x1b[0m", rate));
                }
            }
        }

        if parts.is_empty() {
            None
        } else {
            let joined = parts.join(" \x1b[38;2;100;95;90m·\x1b[0m ");
            if visible_width(&joined) <= budget {
                Some(joined)
            } else {
                let mut fallback_parts = Vec::new();
                if let Some(ttft) = self.last_ttft {
                    fallback_parts.push(format!("\x1b[38;2;120;220;140m{:.2}s\x1b[0m", ttft.as_secs_f64()));
                }
                if let Some(s) = speed {
                    if s > 0.0 {
                        fallback_parts.push(format!("\x1b[38;2;145;205;140m{:.1} tg\x1b[0m", s));
                    }
                }
                let fallback = fallback_parts.join(" \x1b[38;2;100;95;90m·\x1b[0m ");
                if visible_width(&fallback) <= budget && !fallback.is_empty() {
                    Some(fallback)
                } else {
                    None
                }
            }
        }
    }

    fn live_prefill_status(&self) -> Option<String> {
        if self.is_running && self.first_token_time.is_none() {
            let start = self.turn_start_time?;
            let elapsed = start.elapsed();
            let prompt = self.prompt_tokens.unwrap_or(500);
            let cached = (self.last_f_keep.unwrap_or(0.0) * prompt as f64) as usize;
            Some(self.prefill_tracker.format_live_prefill(&self.model, prompt, cached, elapsed))
        } else {
            None
        }
    }

    fn ttft_display(&self) -> Option<String> {
        let ttft = self.last_ttft?;
        let spd = self.last_prefill_speed?;
        let speed_str = if spd >= 1000.0 {
            format!("{:.1}k t/s", spd / 1000.0)
        } else {
            format!("{:.0} t/s", spd)
        };
        Some(format!("\x1b[38;2;120;220;140mTTFT {:.2}s ({speed_str})\x1b[0m", ttft.as_secs_f64()))
    }
}

impl Renderer {
    fn new() -> Self {
        Self {
            tail_height: 0,
            prev_width: 0,
            printed_settled: 0,
            prev_expansion: None,
            needs_reprint: false,
            prev_cursor_tail_offset: 0,
            scroll_offset: 0,
        }
    }

    fn request_reprint(&mut self) {
        self.needs_reprint = true;
    }

    fn clear_tail(&mut self) {
        if self.tail_height > 0 {
            let mut out = String::new();
            if self.prev_cursor_tail_offset > 0 {
                out.push_str(&format!("\x1b[{}F", self.prev_cursor_tail_offset));
            } else {
                out.push('\r');
            }
            out.push_str("\x1b[J");
            let _ = crossterm::queue!(
                std::io::stdout(),
                crossterm::cursor::MoveToColumn(0),
                crossterm::style::Print(out),
            );
            let _ = std::io::stdout().flush();
            self.tail_height = 0;
            self.prev_cursor_tail_offset = 0;
        }
    }

    fn scroll_up(&mut self, lines: usize, max_scroll: usize) {
        self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);
        self.needs_reprint = true;
    }

    fn scroll_down(&mut self, lines: usize) {
        let old = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if old != self.scroll_offset {
            self.needs_reprint = true;
        }
    }

    fn scroll_to_bottom(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset = 0;
            self.needs_reprint = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn format_status_left(
    running: bool,
    is_goal_active: bool,
    goal_progress: Option<&str>,
    mode_label: &str,
    token_tracker: Option<&TokenTracker>,
    budget: usize,
    expand_status: &str,
    face_phase: usize,
) -> String {
    if running {
        // The welcome card is long gone by now, so this is where the mascot
        // keeps the user company while the model works.
        let face = format!(
            "\x1b[38;2;138;180;248m{}\x1b[0m ",
            flashagent_tui::thinking_face(face_phase)
        );
        let mode_str = if is_goal_active {
            "\x1b[1;38;2;225;175;95m[Goal: Autonomous]\x1b[0m".to_string()
        } else {
            format!("\x1b[38;2;145;205;140m[{mode_label}]\x1b[0m")
        };
        // During a goal the budget burn-down is the useful thing to watch;
        // "Generating response..." says nothing a spinner does not.
        let activity = match goal_progress {
            Some(p) => format!("\x1b[38;2;168;199;250m{p}\x1b[0m"),
            None => "\x1b[38;2;168;199;250mGenerating response...\x1b[0m".to_string(),
        };
        format!("  {face}{mode_str} \x1b[38;2;100;95;90m·\x1b[0m {activity}{expand_status}")
    } else if let Some(stats) = token_tracker.and_then(|tt| tt.format_stats_width(budget)) {
        if is_goal_active {
            format!("  \x1b[1;38;2;225;175;95m[Goal: Autonomous]\x1b[0m \x1b[38;2;100;95;90m·\x1b[0m {stats}{expand_status}")
        } else {
            format!("  {stats}{expand_status}")
        }
    } else {
        let mode_str = if is_goal_active {
            "\x1b[1;38;2;225;175;95m[Goal: Autonomous]\x1b[0m".to_string()
        } else {
            format!("\x1b[38;2;145;205;140m[{mode_label}]\x1b[0m")
        };
        format!("  {mode_str} \x1b[38;2;100;95;90m·\x1b[0m \x1b[38;2;140;135;130mReady\x1b[0m{expand_status}")
    }
}

impl Renderer {
    #[allow(clippy::too_many_arguments)]
    fn frame(
        &mut self,
        chat: &ChatView,
        gate: &TuiGate,
        question_gate: &TuiQuestionGate,
        effort_menu: Option<&SelectMenu<String>>,
        model_menu: Option<&SelectMenu<String>>,
        settings_view: Option<&SettingsView>,
        sampling_view: Option<&SamplingView>,
        context_modal: Option<&ContextModal>,
        mcp_modal: Option<&McpModal>,
        autocomplete: Option<&AutocompletePopup>,
        context_usage: &ContextUsage,
        st: FrameState<'_>,
    ) {
        let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
        let width = width as usize;
        let resized = self.prev_width != width as u16;
        self.prev_width = width as u16;
        let toggled = self.prev_expansion != Some(st.reasoning_expand);
        self.prev_expansion = Some(st.reasoning_expand);

        // On resize the scrollback already contains old-width lines; when the
        // thinking mode flips or an existing notice is updated in place, settled
        // reasoning was printed in the other form. Both need everything printed afresh.
        let reprint_all = resized || toggled || self.needs_reprint;
        self.needs_reprint = false;

        let (settled, live) = chat.render_split(width, st.reasoning_expand);
        let mut tail: Vec<RenderLine> = live;

        let inner_w = width.saturating_sub(2);
        let border_color = "\x1b[38;2;95;90;85m";
        let reset = "\x1b[0m";

        let input_line_idx;
        let mut custom_cursor_col: Option<u16> = None;

        if let Some(req) = gate.pending() {
            // Morph the composer into the approval card!
            let tool_styled = format!("\x1b[1;38;2;225;175;95m{}\x1b[0m", req.tool);
            let title = format!(" Confirm: {tool_styled} ");
            let vis_title_len = visible_width(&title);
            let dash_w = inner_w.saturating_sub(vis_title_len + 2);

            tail.push((
                LineKind::System,
                format!("{border_color}╭─{title}{}╮{reset}", "─".repeat(dash_w)),
            ));

            // The user is approving exactly this: show it whatever the JSON
            // formatting, and never let model-supplied escape codes restyle
            // or hide part of it.
            let args = flashagent_llm::effective_args(&req.args_json, &req.tool).unwrap_or_default();
            let field = |k: &str| args.get(k).and_then(|v| v.as_str()).map(card_safe);
            let label_row = |label: &str, value: &str| {
                pad_box_row(
                    &format!(" \x1b[38;2;160;155;145m{label}\x1b[0m \x1b[1;38;2;240;235;225m{}\x1b[0m", clip_ansi(value, inner_w.saturating_sub(label.len() + 4))),
                    width,
                )
            };
            let value_w = inner_w.saturating_sub(14).max(20);
            let (label, value) = if let Some(cmd) = field("command") {
                ("command:", cmd)
            } else if let Some(path) = field("path") {
                ("target:", path)
            } else if args.as_object().is_some_and(|o| !o.is_empty()) {
                ("args:", card_safe(&args.to_string()))
            } else {
                ("", String::new())
            };
            for (i, row) in card_rows(&value, value_w, 6).iter().enumerate() {
                tail.push((LineKind::System, label_row(if i == 0 { label } else { "        " }, row)));
            }

            if let Some(ref diff) = req.diff {
                for line in diff.lines().take(6) {
                    let (color, prefix) = if line.starts_with('+') {
                        ("\x1b[38;2;145;205;140m", "+")
                    } else if line.starts_with('-') {
                        ("\x1b[38;2;225;115;105m", "-")
                    } else {
                        ("\x1b[38;2;135;130;125m", " ")
                    };
                    let line_clean = line.trim_start_matches('+').trim_start_matches('-').trim_start_matches(' ');
                    let line_clipped = clip_ansi(line_clean, inner_w.saturating_sub(6));
                    tail.push((
                        LineKind::System,
                        pad_box_row(&format!("  {color}{prefix} {line_clipped}\x1b[0m"), width),
                    ));
                }
            }

            let allow_btn = if st.confirm_selection == Decision::Allow {
                "\x1b[1;38;2;225;175;95m[► Allow (Enter)]\x1b[0m"
            } else {
                "\x1b[38;2;160;155;145m[ Allow (Enter) ]\x1b[0m"
            };
            let always_btn = "\x1b[38;2;160;155;145m[ Always (a) ]\x1b[0m";
            let deny_btn = if st.confirm_selection == Decision::Deny {
                "\x1b[1;38;2;225;115;105m[► Deny (d / Esc)]\x1b[0m"
            } else {
                "\x1b[38;2;160;155;145m[ Deny (d / Esc) ]\x1b[0m"
            };
            tail.push((
                LineKind::System,
                pad_box_row(&format!(" {allow_btn}   {always_btn}   {deny_btn}"), width),
            ));

            input_line_idx = tail.len();
            tail.push((
                LineKind::System,
                format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
            ));
            custom_cursor_col = Some(0);
        } else if let Some(prompt) = st.channel_prompt {
            // Same treatment as an approval card: this replaces the binary
            // under the user, so it is asked in the same place and with the
            // same weight as anything else that cannot be undone by typing.
            let border_color = "\x1b[38;2;225;175;95m";
            let reset = "\x1b[0m";
            let title = " Switch release channel ";
            let dash_w = inner_w.saturating_sub(visible_width(title) + 2);
            tail.push((
                LineKind::System,
                format!("{border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
            ));
            // Word-aware wrapping: this is a sentence to read and decide on,
            // not a command to inspect character by character.
            for row in wrap_plain(prompt, inner_w.saturating_sub(3)) {
                tail.push((
                    LineKind::System,
                    pad_box_row(&format!(" \x1b[38;2;240;235;225m{row}\x1b[0m"), width),
                ));
            }
            tail.push((
                LineKind::System,
                pad_box_row(
                    " \x1b[1;38;2;225;175;95m[► Yes (y)]\x1b[0m   \x1b[38;2;160;155;145m[ No (n / Esc) ]\x1b[0m",
                    width,
                ),
            ));
            input_line_idx = tail.len();
            tail.push((
                LineKind::System,
                format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
            ));
            custom_cursor_col = Some(0);
        } else if let Some(req) = question_gate.pending() {
            // Morph the composer into the question card!
            let title = " Question from FlashAgent ";
            let vis_title_len = visible_width(title);
            let dash_w = inner_w.saturating_sub(vis_title_len + 2);

            tail.push((
                LineKind::System,
                format!("{border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
            ));

            let q_clipped = clip_ansi(&req.question, inner_w.saturating_sub(4));
            tail.push((
                LineKind::System,
                pad_box_row(&format!(" \x1b[1;38;2;240;235;225m{q_clipped}\x1b[0m"), width),
            ));

            let q_state = st.question_state;
            let sel_idx = q_state.map(|s| s.selected_index).unwrap_or(0);
            let is_writing = q_state.map(|s| s.is_writing).unwrap_or(false);
            let write_text = q_state.map(|s| s.write_in_text.as_str()).unwrap_or("");
            let mut typing_line_idx = None;

            if let Some(ref opts) = req.options {
                let total_choices = opts.len();
                for (i, opt) in opts.iter().enumerate() {
                    let num = i + 1;
                    let is_sel = !is_writing && sel_idx == i;
                    let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                    let opt_clipped = clip_ansi(opt, inner_w.saturating_sub(14));
                    let opt_styled = if req.multi_select {
                        let checked = q_state.map(|s| s.selected_indices.contains(&i)).unwrap_or(false);
                        let check_box = if checked {
                            "\x1b[1;38;2;145;205;140m[x]\x1b[0m"
                        } else {
                            "\x1b[38;2;135;130;125m[ ]\x1b[0m"
                        };
                        if is_sel {
                            format!("{check_box} \x1b[1;38;2;225;175;95m{num}. {opt_clipped}\x1b[0m")
                        } else {
                            format!("{check_box} \x1b[38;2;200;195;185m{num}. {opt_clipped}\x1b[0m")
                        }
                    } else {
                        if is_sel {
                            format!("\x1b[1;38;2;225;175;95m{num}. {opt_clipped}\x1b[0m")
                        } else {
                            format!("\x1b[38;2;200;195;185m{num}. {opt_clipped}\x1b[0m")
                        }
                    };
                    tail.push((LineKind::System, pad_box_row(&format!("  {ptr} {opt_styled}"), width)));
                }

                let write_num = total_choices + 1;
                let is_write_sel = is_writing || sel_idx == total_choices;
                let write_ptr = if is_write_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                if is_writing {
                    typing_line_idx = Some(tail.len());
                    tail.push((
                        LineKind::User,
                        pad_box_row(&format!("  {write_ptr} \x1b[1;38;2;225;175;95m{write_num}. Custom (write-in):\x1b[0m \x1b[1;38;2;240;235;225m{write_text}█\x1b[0m"), width),
                    ));
                    tail.push((
                        LineKind::System,
                        pad_box_row("   \x1b[38;2;135;130;125m[Enter] Submit write-in · [Esc] Back to choices\x1b[0m", width),
                    ));
                } else {
                    let write_styled = if is_write_sel {
                        format!("\x1b[1;38;2;225;175;95m{write_num}. Other (Type custom answer)\x1b[0m")
                    } else {
                        format!("\x1b[38;2;160;155;145m{write_num}. Other (Type custom answer)\x1b[0m")
                    };
                    tail.push((LineKind::System, pad_box_row(&format!("  {write_ptr} {write_styled}"), width)));
                    if req.multi_select {
                        tail.push((
                            LineKind::System,
                            pad_box_row("   \x1b[38;2;135;130;125m[Space] Toggle [x] · [1-N / ↑↓] Select · [Enter] Confirm · [Esc] Cancel\x1b[0m", width),
                        ));
                    } else {
                        tail.push((
                            LineKind::System,
                            pad_box_row("   \x1b[38;2;135;130;125m[1-N / ↑↓] Select · [Enter] Confirm · [Esc] Cancel\x1b[0m", width),
                        ));
                    }
                }
            } else {
                typing_line_idx = Some(tail.len());
                tail.push((
                    LineKind::User,
                    pad_box_row(&format!("  \x1b[1;38;2;225;175;95m❯\x1b[0m \x1b[1;38;2;240;235;225m{write_text}█\x1b[0m"), width),
                ));
                tail.push((
                    LineKind::System,
                    pad_box_row("   \x1b[38;2;135;130;125m[Enter] Submit answer · [Esc] Cancel\x1b[0m", width),
                ));
            }

            input_line_idx = typing_line_idx.unwrap_or(tail.len().saturating_sub(1));
            if typing_line_idx.is_some() {
                custom_cursor_col = Some(((write_text.chars().count() + 6) as u16).min(width.saturating_sub(2) as u16));
            } else {
                custom_cursor_col = Some(4);
            }

            tail.push((
                LineKind::System,
                format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
            ));
        } else if let Some(sm) = sampling_view {
            // Morph composer into Sampling Parameters menu
            tail.extend(sm.render(width));
            input_line_idx = tail.len().saturating_sub(1);
            custom_cursor_col = Some(0);
        } else if let Some(settings) = settings_view {
            // Morph composer into Settings tab
            tail.extend(settings.render(width));
            input_line_idx = tail.len().saturating_sub(1);
            custom_cursor_col = Some(0);
        } else if let Some(menu) = model_menu {
            // Morph composer into Model selection menu
            tail.extend(menu.render(width));
            input_line_idx = tail.len().saturating_sub(1);
            custom_cursor_col = Some(0);
        } else if let Some(menu) = effort_menu {
            // Morph composer into Thinking effort menu
            tail.extend(menu.render(width));
            input_line_idx = tail.len().saturating_sub(1);
            custom_cursor_col = Some(0);
        } else if let Some(modal) = context_modal {
            // Morph composer into Context breakdown modal
            tail.extend(modal.render(width));
            input_line_idx = tail.len().saturating_sub(1);
            custom_cursor_col = Some(0);
        } else if let Some(modal) = mcp_modal {
            // Morph composer into MCP modal
            tail.extend(modal.render(width));
            input_line_idx = tail.len().saturating_sub(1);
            custom_cursor_col = Some(0);
        } else {
            // Standard input box
            tail.push((
                LineKind::System,
                format!("{border_color}╭{}╮{reset}", "─".repeat(inner_w)),
            ));

            // Line 1: Input line with breathing prompt icon `❯` and placeholder if empty
            input_line_idx = tail.len();
            let cycle = (st.tick_n % 28) as f32 / 28.0;
            let phase = (cycle * std::f32::consts::PI * 2.0).sin() * 0.5 + 0.5;
            let r = (210.0 + phase * 45.0) as u8;
            let g = (150.0 + phase * 55.0) as u8;
            let b = (75.0 + phase * 40.0) as u8;
            let prompt_styled = format!("\x1b[1;38;2;{r};{g};{b}m❯\x1b[0m");
            let input_row_content = if st.input.is_empty() {
                if let Some(prefill) = st.prefill_status {
                    format!(" {prompt_styled}  {prefill}")
                } else if st.running {
                    let dots = match (st.tick_n / 4) % 3 {
                        0 => ".  ",
                        1 => ".. ",
                        _ => "...",
                    };
                    format!(" {prompt_styled} \x1b[38;2;135;130;125mWorking on task{dots}\x1b[0m")
                } else if let Some(sug) = st.suggested_prompt {
                    format!(" {prompt_styled}  \x1b[38;2;155;160;175m{sug}\x1b[0m \x1b[38;2;100;105;120m(→ to use)\x1b[0m")
                } else if let Some(custom) = st.custom_placeholder {
                    format!(" {prompt_styled}  \x1b[38;2;135;140;155m{custom}\x1b[0m")
                } else {
                    format!(" {prompt_styled}  \x1b[38;2;135;130;125mAsk FlashAgent to do anything...\x1b[0m")
                }
            } else {
                format!(" {prompt_styled} {}", st.input)
            };
            tail.push((LineKind::User, pad_box_row(&input_row_content, width)));

            // Bottom border of input box
            tail.push((
                LineKind::System,
                format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
            ));
        }

        // Autocomplete popup under composer
        if let Some(ac) = autocomplete {
            tail.extend(ac.render(width));
        }

        // 3-Line persistent footer under composer
        // Line 1: Action hint / hotkeys
        let left_hint = if let Some(toast) = st.copy_toast {
            format!("  \x1b[1;38;2;135;215;165m{toast}\x1b[0m")
        } else if gate.pending().is_some() {
            "  \x1b[38;2;135;130;125menter — allow · a — always · d / esc — deny\x1b[0m".to_string()
        } else if let Some(req) = question_gate.pending() {
            if req.multi_select {
                "  \x1b[38;2;135;130;125mSpace — toggle [x] · 1-N / ↑↓ — select · enter — confirm · esc — cancel\x1b[0m".to_string()
            } else {
                "  \x1b[38;2;135;130;125m↑/↓ / 1-N — select · enter — confirm · esc — cancel\x1b[0m".to_string()
            }
        } else if sampling_view.is_some() {
            "  \x1b[38;2;135;130;125mtype digits/./- · ←/→ adjust · ↑/↓ navigate · enter apply · esc back\x1b[0m".to_string()
        } else if settings_view.is_some() {
            "  \x1b[38;2;135;130;125m↑/↓ — item · enter — open menu/change · esc — close\x1b[0m".to_string()
        } else if model_menu.is_some() {
            "  \x1b[38;2;135;130;125m↑/↓ — select model · enter — switch · esc — cancel\x1b[0m".to_string()
        } else if effort_menu.is_some() {
            "  \x1b[38;2;135;130;125m↑/↓ — select effort · enter — apply · esc — cancel\x1b[0m".to_string()
        } else if context_modal.is_some() {
            "  \x1b[38;2;135;130;125mf1 / enter / esc — close context breakdown\x1b[0m".to_string()
        } else if mcp_modal.is_some() {
            "  \x1b[38;2;135;130;125mtab/1-3 — switch tab · ↑/↓ — navigate · enter — select · esc — close\x1b[0m".to_string()
        } else if autocomplete.is_some() {
            "  \x1b[38;2;135;130;125mtab — complete · ↑/↓ — select · enter — send · esc — dismiss\x1b[0m".to_string()
        } else if st.running {
            let cache_str = if let Some(fk) = st.f_keep {
                format!(" \x1b[38;2;75;99;130m·\x1b[0m \x1b[38;2;120;220;140mcache {:.0}%\x1b[0m", fk * 100.0)
            } else {
                String::new()
            };
            let ttft_str = if let Some(spd_str) = st.ttft_display {
                format!(" \x1b[38;2;75;99;130m·\x1b[0m {spd_str}")
            } else {
                String::new()
            };
            let live = format!(
                "  {} \x1b[38;2;168;199;250mTokens - \x1b[1;38;2;235;240;250m{}\x1b[0m \x1b[38;2;194;231;255m({:.1}/s)\x1b[0m{cache_str}{ttft_str} \x1b[38;2;75;99;130m·\x1b[0m \x1b[38;2;155;165;180m{}s\x1b[0m",
                SPINNER[st.tick_n % SPINNER.len()],
                st.model_tokens,
                st.tokens_per_sec,
                st.elapsed_secs,
            );
            // A turn is running, and something turned up on its own. Both
            // belong on this line: the counters prove the model is alive, the
            // notice is the thing the user did not ask for and must not miss.
            // The keybinding tail is what gives way when they do not both fit.
            let tail_hint = "\x1b[38;2;135;130;125m· tab/f1-f5 menus · enter to steer · esc to interrupt\x1b[0m";
            match st.background {
                Some(text) => {
                    let notice = format!(" \x1b[38;2;75;99;130m·\x1b[0m {}", st.background_style.paint(text));
                    let with_hint = format!("{live}{notice} \x1b[38;2;135;130;125m· esc to interrupt\x1b[0m");
                    if visible_width(&with_hint) <= width {
                        with_hint
                    } else {
                        format!("{live}{notice}")
                    }
                }
                None => format!("{live} {tail_hint}"),
            }
        } else if let Some(text) = st.background {
            format!("  {}", st.background_style.paint(text))
        } else {
            // Responsive footer shortcuts adapting cleanly to any terminal width:
            if width >= 140 {
                "  \x1b[38;2;135;130;125menter — send · tab — settings · f1 — context · f2 — verbose · f3 — model · f4 — effort · f5 — sampling · ctrl+r — regen · ctrl+c/v · esc — quit\x1b[0m".to_string()
            } else if width >= 115 {
                "  \x1b[38;2;135;130;125menter — send · tab — settings · f1 — context · f3 — model · f4 — effort · f5 — sampling · ctrl+r — regen · esc — quit\x1b[0m".to_string()
            } else if width >= 92 {
                "  \x1b[38;2;135;130;125menter — send · tab — settings · f3 — model · f4 — effort · f5 — sampling · esc — quit\x1b[0m".to_string()
            } else if width >= 68 {
                "  \x1b[38;2;135;130;125menter — send · tab — settings · f3 — model · esc — quit\x1b[0m".to_string()
            } else {
                "  \x1b[38;2;135;130;125menter — send · tab — menu · esc — quit\x1b[0m".to_string()
            }
        };
        tail.push((LineKind::System, left_hint));

        // Line 2: Dedicated Developer Tip across full terminal width (wraps to 2 lines in compact terminals)
        if let Some(lines) = st.tip_lines {
            for line in lines {
                let tip_row = clip_ansi(line, width.saturating_sub(2));
                tail.push((LineKind::System, tip_row));
            }
        } else if let Some(anim) = st.tip_animated {
            let raw_tip = format!("  \x1b[1;38;2;225;175;95mTip:\x1b[0m {anim}");
            let tip_row = clip_ansi(&raw_tip, width.saturating_sub(2));
            tail.push((LineKind::System, tip_row));
        } else {
            let default_tip = "Press Tab or type /settings to configure FlashAgent";
            let tip_content = st.tip.unwrap_or(default_tip);
            let avail1 = width.saturating_sub(8);
            if width >= 30 && tip_content.chars().count() > avail1 {
                let (l1, l2) = flashagent_tui::tips::split_tip_at_word_boundary(tip_content, avail1);
                let row1 = format!("  \x1b[1;38;2;225;175;95mTip:\x1b[0m \x1b[38;2;175;170;160m{l1}\x1b[0m");
                let row2 = format!("       \x1b[38;2;175;170;160m{l2}\x1b[0m");
                tail.push((LineKind::System, clip_ansi(&row1, width.saturating_sub(2))));
                tail.push((LineKind::System, clip_ansi(&row2, width.saturating_sub(2))));
            } else {
                let raw_tip = format!("  \x1b[1;38;2;225;175;95mTip:\x1b[0m \x1b[38;2;175;170;160m{tip_content}\x1b[0m");
                let tip_row = clip_ansi(&raw_tip, width.saturating_sub(2));
                tail.push((LineKind::System, tip_row));
            }
        }

        // Line 3: Turn telemetry & performance metrics on the left, context gauge on the right
        let gauge_base = context_usage.format_compact_gauge(10);
        let gauge_str = if st.context_warn_threshold > 0 && context_usage.percentage() >= st.context_warn_threshold as f32 {
            format!("\x1b[1;38;2;245;140;80m⚠ High Ctx ({:.0}%)\x1b[0m {gauge_base}", context_usage.percentage())
        } else {
            gauge_base
        };
        let gauge_vis = visible_width(&gauge_str);
        let budget = width.saturating_sub(gauge_vis + 4);

        let expand_status = if st.reasoning_expand.all {
            " \x1b[38;2;100;95;90m·\x1b[0m \x1b[38;2;175;170;225m[verbose: all]\x1b[0m"
        } else if st.reasoning_expand.last {
            " \x1b[38;2;100;95;90m·\x1b[0m \x1b[38;2;175;170;225m[verbose: last]\x1b[0m"
        } else {
            ""
        };

        let left_telemetry = format_status_left(
            st.running,
            st.is_goal_active,
            st.goal_progress,
            st.mode.label(),
            st.token_tracker,
            budget,
            expand_status,
            st.face_phase,
        );

        let left_vis = visible_width(&left_telemetry);
        let status_row = if left_vis + gauge_vis + 3 <= width {
            let pad = " ".repeat(width.saturating_sub(left_vis + gauge_vis + 1));
            format!("{left_telemetry}{pad}{gauge_str}")
        } else {
            let clip_budget = width.saturating_sub(gauge_vis + 3);
            let clipped_left = clip_ansi(&left_telemetry, clip_budget);
            let pad = " ".repeat(width.saturating_sub(visible_width(&clipped_left) + gauge_vis + 1));
            format!("{clipped_left}{pad}{gauge_str}")
        };
        tail.push((LineKind::System, status_row));

        // When scroll_offset > 0, render shifted history view
        if self.scroll_offset > 0 {
            let mut all_lines: Vec<RenderLine> = settled.clone();
            all_lines.extend(tail.clone());
            let total_len = all_lines.len();
            let view_h = (height as usize).saturating_sub(2).max(5);
            let end = total_len.saturating_sub(self.scroll_offset);
            let start = end.saturating_sub(view_h);
            let visible_slice = &all_lines[start..end];

            let mut out = String::from("\x1b[H\x1b[2J");
            let mut first = true;
            for (kind, text) in visible_slice {
                if !first {
                    out.push_str("\r\n");
                }
                first = false;
                let clipped = clip_ansi(text, width);
                let prefix = crossterm::style::SetForegroundColor(color(*kind)).to_string();
                out.push_str(&prefix);
                out.push_str(&restore_line_color(&clipped, &prefix));
                out.push_str("\x1b[39m");
            }
            out.push_str("\r\n");
            let scroll_status = format!(
                " \x1b[7m ↑ Scrolled up {} lines (↓ / Esc / PageDown / type to return) \x1b[0m",
                self.scroll_offset
            );
            out.push_str(&clip_ansi(&scroll_status, width));
            crossterm::queue!(
                std::io::stdout(),
                crossterm::cursor::MoveToColumn(0),
                crossterm::style::Print(out),
            )
            .ok();
            std::io::stdout().flush().ok();
            return;
        }

        let mut out = String::new();
        if reprint_all {
            // Full repaint: home + clear, then reprint including settled.
            // When on the initial welcome screen, also purge scrollback (\x1b[3J) so
            // terminal emulator reflow lines from window resize are wiped completely.
            if !chat.has_user_message() {
                out.push_str("\x1b[3J\x1b[H\x1b[2J");
            } else {
                out.push_str("\x1b[H\x1b[2J");
            }
        } else {
            // Move cursor up into the top of the previous tail, erase down.
            if self.prev_cursor_tail_offset > 0 {
                out.push_str(&format!("\x1b[{}F", self.prev_cursor_tail_offset));
            } else {
                out.push('\r');
            }
            out.push_str("\x1b[J"); // clear from cursor down
        }

        let mut first = true;
        let mut print_line = |out: &mut String, kind: &LineKind, text: &str| {
            if !first {
                out.push_str("\r\n");
            }
            first = false;
            // HARD CAP: any tail line longer than the terminal width would
            // wrap physically and desync tail_height (the duplication bug).
            let clipped = clip_ansi(text, width);
            let prefix = crossterm::style::SetForegroundColor(color(*kind)).to_string();
            out.push_str(&prefix);
            out.push_str(&restore_line_color(&clipped, &prefix));
            out.push_str("\x1b[39m");
        };
        if reprint_all {
            for (k, t) in &settled {
                print_line(&mut out, k, t);
            }
            self.printed_settled = settled.len();
        } else if settled.len() > self.printed_settled {
            // Newly frozen lines print once; they never repaint.
            for (k, t) in &settled[self.printed_settled..] {
                print_line(&mut out, k, t);
            }
            self.printed_settled = settled.len();
        }
        for (k, t) in &tail {
            print_line(&mut out, k, t);
        }
        self.tail_height = tail.len() as u16;
        self.prev_cursor_tail_offset = input_line_idx as u16;

        let lift_up = (tail.len() - 1 - input_line_idx) as u16;
        let cursor_col = custom_cursor_col.unwrap_or_else(|| ((st.input.chars().count() + 4) as u16).min(width.saturating_sub(2) as u16));
        crossterm::queue!(
            std::io::stdout(),
            crossterm::cursor::MoveToColumn(0),
            crossterm::style::Print(out),
            crossterm::cursor::MoveUp(lift_up),
            crossterm::cursor::MoveToColumn(cursor_col),
        )
        .ok();
        std::io::stdout().flush().ok();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = AppConfig::load();
    let mut force_setup = false;
    let mut skip_trust = false;
    let mut resume_session_id = None;
    let mut tool_test: Option<bool> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--url" => {
                if let Some(val) = args.next() {
                    config.backend_url = val;
                }
            }
            "--model" => {
                if let Some(val) = args.next() {
                    config.model = val;
                }
            }
            "-v" | "--version" => {
                println!("FlashAgent {}", flashagent_svc::updater::current_version());
                return Ok(());
            }
            "--resume" => {
                if let Some(val) = args.next() {
                    resume_session_id = Some(val);
                } else {
                    anyhow::bail!("--resume requires a session ID");
                }
            }
            "--update" => {
                if flashagent_svc::updater::is_dev_mode() {
                    println!("In-app updater is disabled in development mode (running from source repository or cargo target build).");
                    println!("To update your dev build, pull latest git commits and run `cargo build --release`.");
                    return Ok(());
                }
                println!("Checking for updates on {} channel...", config.update_channel.label());
                match flashagent_svc::updater::check_for_updates(config.update_channel, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                    Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, is_downgrade, checksums_url, .. }) => {
                        let op = if is_downgrade { "Downgrading" } else { "Updating" };
                        println!("{op} FlashAgent to {target} (downloading {asset_name})...");
                        let path = flashagent_svc::updater::download_and_apply(&download_url, &asset_name, checksums_url.as_deref()).await?;
                        println!("Update successfully installed to {}", path.display());
                    }
                    Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, channel }) => {
                        println!("FlashAgent {current} is already up to date on {} channel.", channel.label());
                    }
                    Err(e) => {
                        eprintln!("Update check failed: {e}");
                    }
                }
                return Ok(());
            }
            "--channel" => {
                if let Some(val) = args.next() {
                    match val.to_lowercase().as_str() {
                        "stable" | "v" => config.update_channel = flashagent_core::config::UpdateChannel::Stable,
                        "beta" | "b" => config.update_channel = flashagent_core::config::UpdateChannel::Beta,
                        other => anyhow::bail!("invalid channel: {other} (use stable or beta)"),
                    }
                    let _ = config.save();
                    println!("Release channel set to {}", config.update_channel.label());
                    return Ok(());
                } else {
                    println!("Current channel: {}", config.update_channel.label());
                    return Ok(());
                }
            }
            "--tool-test" => tool_test = Some(false),
            "--all-models" => tool_test = Some(true),
            "--setup" => force_setup = true,
            "-y" | "--yes" => skip_trust = true,
            "-h" | "--help" => {
                println!("FlashAgent TUI\n\nUsage: flashagent [OPTIONS]\n\nOptions:\n  -v, --version        Print version\n  --update             Check and apply updates\n  --channel <name>     Switch release channel (stable, beta)\n  --model <name>       Specify LLM model name\n  --url <endpoint>     API endpoint (default: http://localhost:1234/v1)\n  --setup              Run first-time setup wizard\n  --tool-test [--all-models]  Check whether the model can drive tools\n  --resume <id>        Resume a previously saved chat session\n  -y, --yes            Skip directory trust confirmation\n  -h, --help           Show this help message");
                return Ok(());
            }
            other => anyhow::bail!("usage: flashagent [-v] [--update] [--channel <stable|beta>] [--model <name>] [--url http://host/v1] [--tool-test [--all-models]] [--setup] [--resume <id>] [-y|--yes] (got {other})"),
        }
    }

    if let Some(all_models) = tool_test {
        let code = run_tool_check_cli(&config, all_models).await;
        std::process::exit(code);
    }

    use std::io::IsTerminal;
    if (!config.setup_completed || force_setup) && std::io::stdout().is_terminal() {
        let completed = flashagent_tui::run_wizard(&mut config).await.unwrap_or(false);
        if !completed {
            return Ok(());
        }
        // Whether the chosen model can actually drive tools decides whether
        // anything here works, and finding out by watching it narrate its
        // intentions for ten minutes is a bad first hour. Ask it now.
        first_run_tool_check(&config).await;
    }

    let api_key = config.api_key.clone().or_else(|| std::env::var("FLASHAGENT_API_KEY").ok());
    let url = config.backend_url.clone();
    let mut model = config.model.clone();

    if std::env::var("FLASHAGENT_TRUST_DIR").is_ok() {
        skip_trust = true;
    }

    // Spawn server discovery in background immediately so it collects
    // models, context window, and thinking presets concurrently while the user interacts with the startup screen.
    let backend_initial = flashagent_llm::OpenAiCompat::new(&url, &model, api_key.clone());
    let discovery_task = tokio::spawn(async move {
        backend_initial.discover_server().await
    });

    let mut cwd = std::env::current_dir()?;

    if config.is_directory_trusted(&cwd) {
        skip_trust = true;
    }

    // Prompt user for directory trust / change working directory / quit
    if !skip_trust && std::io::stdout().is_terminal() {
        let action = flashagent_tui::startup::run_trust_screen(&mut cwd).await?;
        if action == flashagent_tui::StartupAction::Quit {
            discovery_task.abort();
            return Ok(());
        }
        config.trust_directory(&cwd);
        let _ = config.save();
    }

    // Process-lifetime objects: leaked once, so the spawned loop task can hold
    // &'static references (the process is the session).
    let backend = flashagent_llm::OpenAiCompat::new(&url, &model, api_key);
    backend.set_max_retries(config.network_retries);
    let discovery = discovery_task.await.unwrap_or(None);

    // If model was not specified, auto-detect loaded model or first available model
    if model.is_empty() {
        if let Some(ref disc) = discovery {
            if let Some(ref active) = disc.active_model {
                model = active.id.clone();
            } else if let Some(first) = disc.models.first() {
                model = first.id.clone();
            }
        }
    } else if let Some(ref disc) = discovery {
        // If user specified substring/fuzzy model name, align with full ID
        if let Some(matched) = disc.models.iter().find(|m| m.id == model || m.id.contains(&model) || model.contains(&m.id)) {
            model = matched.id.clone();
        }
    }
    backend.set_model(&model);
    config.model = model.clone();

    if model.is_empty() {
        anyhow::bail!(
            "No model specified and could not connect to LLM server at {url} to auto-detect a loaded model.\n\
             Please start your server (e.g. LM Studio on port 1234) or run with --model <name> or --setup."
        );
    }

    let active_model_info = discovery.as_ref().and_then(|d| {
        d.models.iter().find(|m| m.id == model).cloned()
    });

    let context_display = active_model_info.as_ref().and_then(|m| m.context_display());
    let context_capacity = active_model_info
        .as_ref()
        .and_then(|m| m.context_length.or(m.max_context_length))
        .unwrap_or(131_072);

    let available_models: Vec<String> = discovery
        .as_ref()
        .map(|d| d.models.iter().map(|m| m.id.clone()).collect())
        .unwrap_or_default();

    let profile = backend.profile().or_else(|| active_model_info.as_ref().map(|m| m.thinking.clone()));

    let initial_effort = if !config.thinking_effort.is_empty() && config.thinking_effort != "default" {
        config.thinking_effort.clone()
    } else if let Some(ref p) = profile {
        if !p.supported {
            "off".to_string()
        } else {
            "auto".to_string()
        }
    } else {
        "auto".to_string()
    };
    let source: Arc<BackendSource> = Arc::new(BackendSource(backend));
    let home_str = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    let cwd_display = if !home_str.is_empty() {
        cwd.strip_prefix(&home_str)
            .map(|rel| {
                let s = rel.to_string_lossy();
                if s.is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", s.trim_start_matches('/'))
                }
            })
            .unwrap_or_else(|_| cwd.display().to_string())
    } else {
        cwd.display().to_string()
    };

    let gate = TuiGate::new();
    let question_gate = TuiQuestionGate::new();
    let mcp_manager = flashagent_tools::mcp::McpManager::new(cwd.clone());
    let mcp_bg = mcp_manager.clone();
    tokio::spawn(async move {
        mcp_bg.start_enabled_servers().await;
    });

    let state = Arc::new(PermissionState::new(config.permission_mode, gate.clone()));
    // MCP config `read_only`/`read_only_tools` and server readOnlyHint
    // annotations decide what counts as a read for external tools.
    let hint_mgr = mcp_manager.clone();
    state.set_read_only_hint(Arc::new(move |tool: &str| hint_mgr.is_tool_read_only(tool)));
    let tools_arc: Arc<BuiltinTools> = Arc::new(BuiltinTools::new(BuiltinToolsConfig {
        cwd: cwd.clone(),
        brave_api_key: None,
        question_gate: Some(question_gate.clone()),
        is_goal_mode: None,
        toolset_profile: Some(config.toolset_profile),
        web_enabled: Some(config.free_search),
        context_window: Some(context_capacity),
        mcp_manager: Some(mcp_manager.clone()),
    })?);
    // The parent sees the built-in toolset plus `spawn_agent` (subagents).
    let composite = flashagent_tools::agent_tools(tools_arc.clone(), source.clone(), state.clone());
    let perm: &'static PermissionedTools =
        Box::leak(Box::new(PermissionedTools::new(Arc::new(composite), Some(tools_arc.clone()), state.clone())));

    // Memory injection: project + global docs into the first user message.
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
    let docs = flashagent_core::collect(&cwd, &home.join(".flashagent"));
    let memory_block = flashagent_core::injection_block(&docs, config.token_budget);
    let memory_docs = docs.len();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::cursor::Show,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
        );
        let _ = crossterm::terminal::disable_raw_mode();
        let log_content = format!(
            "FlashAgent Crash Report\nVersion: {}\nTime: {:?}\nPanic: {}\nBacktrace:\n{:?}\n\nPlease report this issue at: https://github.com/flashback7766/FlashAgent/issues\n",
            flashagent_svc::updater::current_version(),
            std::time::SystemTime::now(),
            info,
            std::backtrace::Backtrace::capture()
        );
        // Never into the user's project directory.
        let log_path = flashagent_home_dir()
            .map(|d| {
                let _ = std::fs::create_dir_all(&d);
                d.join("crash.log")
            })
            .unwrap_or_else(|| std::env::temp_dir().join("flashagent-crash.log"));
        let _ = std::fs::write(&log_path, log_content);
        eprintln!("\x1b[1;38;2;245;120;120mFlashAgent encountered an unexpected crash.\x1b[0m");
        eprintln!("Crash report written to {}. Please submit an issue at: https://github.com/flashback7766/FlashAgent/issues", log_path.display());
        default_hook(info);
    }));

    enable_raw_mode()?;
    let _ = crossterm::execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
    );
    let result = run_app(AppContext {
        config,
        source,
        perm,
        gate,
        question_gate,
        tools_arc,
        memory_block,
        memory_docs,
        model,
        context_display,
        context_capacity,
        cwd_display,
        initial_effort,
        available_models,
        resume_session_id,
    })
    .await;
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::MoveToColumn(0),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    disable_raw_mode()?;
    if let Ok(Some(ref saved_id)) = result {
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        let card = render_session_saved_card(saved_id, term_w as usize);
        println!();
        for line in card {
            println!("{line}");
        }
        println!();
    }
    result.map(|_| ())
}

struct AppContext {
    config: AppConfig,
    source: Arc<BackendSource>,
    perm: &'static PermissionedTools,
    gate: Arc<TuiGate>,
    question_gate: Arc<TuiQuestionGate>,
    tools_arc: Arc<BuiltinTools>,
    memory_block: String,
    memory_docs: usize,
    model: String,
    context_display: Option<String>,
    context_capacity: usize,
    cwd_display: String,
    initial_effort: String,
    available_models: Vec<String>,
    resume_session_id: Option<String>,
}

fn build_model_menu(source: &BackendSource) -> Option<SelectMenu<String>> {
    let disc = source.discovery()?;
    if disc.models.is_empty() {
        return None;
    }
    let url = disc.base_url.to_lowercase();
    let is_lm_studio = url.contains("1234") || url.contains("lmstudio");
    let has_loaded = disc.models.iter().any(|m| m.is_loaded);
    let models: Vec<_> = if is_lm_studio && has_loaded {
        disc.models.iter().filter(|m| m.is_loaded).collect()
    } else {
        disc.models.iter().collect()
    };

    let mut items = Vec::new();
    for m in models {
        let summary = m.capabilities_summary();
        let load_tag = if m.is_loaded { "● loaded" } else { "○ available" };
        let desc = format!("{load_tag} · {summary}");
        items.push(SelectItem::with_description(m.id.clone(), desc, m.id.clone()));
    }
    Some(SelectMenu::new("Select Model", items).with_noun("models"))
}

fn build_effort_menu(source: &BackendSource) -> SelectMenu<String> {
    let mut items = Vec::new();
    items.push(SelectItem::with_description(
        "auto",
        "Auto (dynamically adjusts thinking per turn)",
        "auto".to_string(),
    ));
    if let Some(prof) = source.profile() {
        if prof.supported && !prof.presets.is_empty() {
            for p in &prof.presets {
                let desc = match p.to_lowercase().as_str() {
                    "off" | "disabled" | "none" | "false" | "0" => "Disable internal reasoning",
                    "low" | "minimal" | "min" | "fast" => "Fast response, low thinking budget",
                    "medium" | "standard" => "Balanced reasoning depth and speed",
                    "high" | "deep" => "Full reasoning exploration",
                    "xhigh" | "extra-high" => "Maximum thinking budget for difficult tasks",
                    "on" | "enabled" | "true" | "1" => "Enable internal reasoning",
                    _ => "API model preset",
                };
                items.push(SelectItem::with_description(p.clone(), desc, p.clone()));
            }
        } else if !prof.supported {
            items.push(SelectItem::with_description("off", "Reasoning unsupported by this endpoint", "off".to_string()));
        }
    }
    if items.len() == 1 {
        items.push(SelectItem::with_description("off", "Disable reasoning", "off".to_string()));
        items.push(SelectItem::with_description("low", "Minimal reasoning budget", "low".to_string()));
        items.push(SelectItem::with_description("medium", "Balanced reasoning depth", "medium".to_string()));
        items.push(SelectItem::with_description("high", "Deep reasoning exploration", "high".to_string()));
    }
    SelectMenu::new("Select Thinking Effort", items).with_noun("options")
}

fn thinking_summary_str(source: &BackendSource, current_effort: &str) -> String {
    if let Some(ref p) = source.profile() {
        if p.supported && !p.presets.is_empty() {
            format!("{} [{}]", current_effort, p.presets.join(", "))
        } else if !p.supported {
            "disabled (unsupported)".to_string()
        } else {
            current_effort.to_string()
        }
    } else {
        current_effort.to_string()
    }
}

#[derive(Debug, Clone)]
/// A release-channel change waiting to be confirmed.
///
/// Switching channel is not a preference like a colour: it replaces the
/// binary with a different line of builds, which can take features away and
/// leave settings behind that the older build does not understand. It is
/// asked about, once, in those words.
struct ChannelSwitch {
    from: flashagent_core::config::UpdateChannel,
    to: flashagent_core::config::UpdateChannel,
    /// The version that channel would put you on, once the check answers.
    target: ChannelTarget,
}

#[derive(Debug, Clone, PartialEq)]
enum ChannelTarget {
    /// The release feed has not answered yet.
    Checking,
    /// The newest build on that channel.
    Version(String),
    /// The channel exists but has nothing published on it.
    Empty,
    /// The feed could not be reached; the switch is still the user's to make.
    Unknown,
}

/// What the user is about to do, in a sentence they can decide on.
fn channel_switch_warning(
    to: flashagent_core::config::UpdateChannel,
    current: &str,
    target: &ChannelTarget,
) -> String {
    use flashagent_core::config::UpdateChannel;
    let channel = to.label().to_lowercase();
    let destination = match target {
        ChannelTarget::Version(v) => v.clone(),
        ChannelTarget::Empty => {
            return format!(
                "Nothing is published on the {channel} channel yet, so you would stay on \
                 {current} until something is. Switch anyway?"
            )
        }
        // Still checking, or the feed could not be reached: name the channel
        // rather than invent a version.
        ChannelTarget::Checking | ChannelTarget::Unknown => format!("the newest {channel} release"),
    };
    match to {
        UpdateChannel::Stable => format!(
            "You will be moved from {current} down to {destination}. Features added since may \
             disappear or behave differently, and settings they introduced can be reset. Continue?"
        ),
        UpdateChannel::Beta => format!(
            "You will be moved from {current} to {destination}. Beta builds land often and can \
             regress; that is the point of them. Continue?"
        ),
    }
}

/// A message that appeared without the user doing anything. It lives on the
/// line under the input, and most of them fade: a notice that is no longer
/// actionable should not take that line for the rest of the session.
struct BackgroundNotice {
    text: String,
    expires: Option<std::time::Instant>,
}

/// How brightly a background notice is drawn. A notice that is about to
/// expire dims over its last second, so it leaves the line instead of
/// blinking out of it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NoticeStyle {
    fade: f32,
}

impl NoticeStyle {
    const FULL: Self = Self { fade: 1.0 };

    fn paint(self, text: &str) -> String {
        let lerp = |from: f32, to: f32| (to + (from - to) * self.fade).round() as u8;
        // From the healthy green towards the muted grey the hints use.
        let (r, g, b) = (lerp(120.0, 100.0), lerp(220.0, 95.0), lerp(140.0, 90.0));
        let bold = if self.fade > 0.6 { "1;" } else { "" };
        format!("\x1b[{bold}38;2;{r};{g};{b}m{text}\x1b[0m")
    }
}

impl BackgroundNotice {
    /// Full brightness until the last second of its life, then a ramp down.
    fn style(&self) -> NoticeStyle {
        match self.expires {
            None => NoticeStyle::FULL,
            Some(at) => {
                let left = at.saturating_duration_since(std::time::Instant::now()).as_secs_f32();
                NoticeStyle { fade: left.clamp(0.0, 1.0) }
            }
        }
    }

    /// Stays until something replaces it — use for anything the user still
    /// has to act on ("restart to run the new build").
    fn sticky(text: impl Into<String>) -> Self {
        Self { text: text.into(), expires: None }
    }

    /// Disappears after `secs`.
    fn fading(text: impl Into<String>, secs: u64) -> Self {
        Self {
            text: text.into(),
            expires: Some(std::time::Instant::now() + std::time::Duration::from_secs(secs)),
        }
    }

    fn expired(&self) -> bool {
        self.expires.is_some_and(|t| std::time::Instant::now() >= t)
    }
}

enum UpdateNotice {
    Available { version: String, asset_name: String, download_url: String, checksums_url: Option<String> },
    /// Only the manual update (Ctrl+U) sends these; a background update stays
    /// silent so it never takes over a screen the user is reading.
    Progress { version: String, stage: flashagent_svc::updater::UpdateProgress },
    Ready { version: String },
    UpToDate { version: String },
    Failed { error: String },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedToolCall {
    id: String,
    name: String,
    args_json: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedMessage {
    role: String,
    content: String,
    reasoning: Option<String>,
    tool_call_id: Option<String>,
    tool_calls: Vec<SavedToolCall>,
}

impl From<&ChatMessage> for SavedMessage {
    fn from(m: &ChatMessage) -> Self {
        Self {
            role: m.role.as_str().to_string(),
            content: m.content.clone(),
            reasoning: m.reasoning.clone(),
            tool_call_id: m.tool_call_id.clone(),
            tool_calls: m
                .tool_calls
                .iter()
                .map(|tc| SavedToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    args_json: tc.args_json.clone(),
                })
                .collect(),
        }
    }
}

impl From<SavedMessage> for ChatMessage {
    fn from(m: SavedMessage) -> Self {
        let role = match m.role.as_str() {
            "system" => flashagent_llm::Role::System,
            "assistant" => flashagent_llm::Role::Assistant,
            "tool" => flashagent_llm::Role::Tool,
            _ => flashagent_llm::Role::User,
        };
        Self {
            role,
            content: m.content,
            reasoning: m.reasoning,
            tool_call_id: m.tool_call_id,
            tool_calls: m
                .tool_calls
                .into_iter()
                .map(|tc| flashagent_llm::ToolCall {
                    id: tc.id,
                    name: tc.name,
                    args_json: tc.args_json,
                })
                .collect(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedSession {
    id: String,
    timestamp: u64,
    model: String,
    cwd: String,
    messages: Vec<SavedMessage>,
}

fn flashagent_home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".flashagent"))
}

fn save_session_file(session_id: &str, model: &str, cwd: &str, history: &[ChatMessage]) -> Option<std::path::PathBuf> {
    let base_dir = flashagent_home_dir()?.join("sessions");
    let _ = std::fs::create_dir_all(&base_dir);
    let path = base_dir.join(format!("{session_id}.json"));
    let saved = SavedSession {
        id: session_id.to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        model: model.to_string(),
        cwd: cwd.to_string(),
        messages: history.iter().map(SavedMessage::from).collect(),
    };
    if let Ok(data) = serde_json::to_string_pretty(&saved) {
        if std::fs::write(&path, data).is_ok() {
            return Some(path);
        }
    }
    None
}

fn open_in_external_editor(initial_text: &str, preferred_editor: &str) -> std::io::Result<String> {
    let editor = if !preferred_editor.is_empty() {
        preferred_editor.to_string()
    } else {
        std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "nano".to_string())
    };

    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join(format!("flashagent_prompt_{}.md", std::process::id()));
    std::fs::write(&temp_file, initial_text)?;

    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Show,
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    );

    let status = std::process::Command::new(&editor)
        .arg(&temp_file)
        .status();

    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Hide,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    );

    let result = match status {
        Ok(s) if s.success() => std::fs::read_to_string(&temp_file).unwrap_or_else(|_| initial_text.to_string()),
        _ => initial_text.to_string(),
    };

    let _ = std::fs::remove_file(&temp_file);
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn refresh_welcome_card_if_before_user_msg(
    chat: &mut ChatView,
    renderer: &mut Renderer,
    model: &str,
    cwd_display: &str,
    mode_str: &str,
    memory_docs: usize,
    source: &BackendSource,
    current_effort: &str,
    context_display: Option<&str>,
    show_mascot: bool,
    mood: MascotMood,
) {
    refresh_welcome_card_animated(
        chat, renderer, model, cwd_display, mode_str, memory_docs, source, current_effort,
        context_display, 0, None, show_mascot, mood, None,
    );
}

#[allow(clippy::too_many_arguments)]
fn refresh_welcome_card_animated(
    chat: &mut ChatView,
    renderer: &mut Renderer,
    model: &str,
    cwd_display: &str,
    mode_str: &str,
    memory_docs: usize,
    source: &BackendSource,
    current_effort: &str,
    context_display: Option<&str>,
    tick_n: usize,
    width_override: Option<usize>,
    show_mascot: bool,
    mood: MascotMood,
    // `reveal_rows`: draw only this many rows — the start-up reveal, where the
    // card appears to draw itself from the top down. `None` draws all of it.
    reveal_rows: Option<usize>,
) {
    if !chat.has_user_message() {
        let (term_w, term_h) = {
            let (w, h) = crossterm::terminal::size().unwrap_or((100, 24));
            (width_override.unwrap_or(w as usize), h as usize)
        };
        let th_sum = thinking_summary_str(source, current_effort);
        let card = welcome_card_responsive_opts(
            model,
            cwd_display,
            mode_str,
            memory_docs,
            Some(&th_sum),
            context_display,
            term_w,
            term_h,
            tick_n,
            show_mascot,
            mood,
        );
        let card = match reveal_rows {
            Some(rows) => card.into_iter().take(rows.max(1)).collect(),
            None => card,
        };
        chat.update_welcome_card(card);
        renderer.request_reprint();
    }
}

async fn run_app(ctx: AppContext) -> Result<Option<String>> {
    let AppContext {
        config: mut app_config,
        source,
        perm,
        gate,
        question_gate,
        tools_arc,
        memory_block,
        memory_docs,
        model,
        context_display,
        context_capacity,
        cwd_display,
        initial_effort,
        mut available_models,
        resume_session_id,
    } = ctx;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut question_ui_state = QuestionUiState::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();

    // Keyboard and mouse reader thread: crossterm is blocking, tokio is async.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                match crossterm::event::read() {
                    Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                        if tx.send(UiEvent::Key(k.code, k.modifiers)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Paste(s)) => {
                        if tx.send(UiEvent::Paste(s)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        if tx.send(UiEvent::Mouse(m)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Resize(w, h)) => {
                        if tx.send(UiEvent::Resize(w, h)).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
    }

    let system_prompt_config = SystemPromptConfig::new()
        .with_cwd(&cwd_display)
        .with_platform(std::env::consts::OS)
        .with_model(&model)
        .with_effort(&initial_effort);
    let system_prompt_text = build_system_prompt(&system_prompt_config);
    let mut history: Vec<ChatMessage> = vec![ChatMessage::system(system_prompt_text)];
    let mut input = String::new();
    let mut custom_placeholder: Option<String> = None;
    let mut suggested_prompt: Option<String> = None;


    let mut latest_suggestion: Option<String> = None;
    let mut tip_animator = flashagent_tui::tips::TipAnimator::new();
    let mut input_history: Vec<String> = Vec::new();
    let mut history_index: Option<usize> = None;
    let mut current_draft = String::new();
    let mut confirm_select = ConfirmSelect::new();
    let mut chat = ChatView::default();
    let mut running = false;
    let mut active_turn_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut active_steer_tx: Option<tokio::sync::mpsc::UnboundedSender<String>> = None;
    // Set when the user interrupts: the loop is asked to stop cooperatively so
    // it can hand back a consistent history; a hard abort is the fallback.
    let mut cancel_requested: Option<std::time::Instant> = None;
    let mut aborted_turn: Option<u64> = None;
    let mut turn_counter: u64 = 0;
    let mut all_expanded = false;
    let mut last_expanded = false;
    let mut current_model = model;
    let mut current_context = context_display;
    let mut current_effort = initial_effort;
    let mut effort_menu: Option<SelectMenu<String>> = None;
    let mut model_menu: Option<SelectMenu<String>> = None;
    let mut settings_view: Option<SettingsView> = None;
    let mut sampling_view: Option<SamplingView> = None;
    let mut context_modal: Option<ContextModal> = None;
    let mut mcp_modal: Option<McpModal> = None;
    let mut context_usage = ContextUsage::new(context_capacity);
    update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
    let mut autocomplete_idx = 0usize;
    let mut tick_n = 0usize;
    let mut renderer = Renderer::new();

    /// A one-line system notice. These go under the cursor rather than into
    /// the transcript: they are addressed to the person at the keyboard, not
    /// to the conversation, and a chat full of "[No models discovered]" is a
    /// chat you stop reading. Anything with structure — a listing, a diff, a
    /// report — still belongs in the transcript.
    macro_rules! notice {
        ($text:expr) => {{
            custom_placeholder = Some(($text).to_string());
            // A pending suggestion is drawn in the same spot and would hide
            // the notice; take() rather than assign, so a site that sets it
            // again right after is not flagged as a dead store.
            suggested_prompt.take();
            renderer.request_reprint();
        }};
    }
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(80));
    let mut check_interval = tokio::time::interval(std::time::Duration::from_secs(3));
    check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let is_discovering = Arc::new(AtomicBool::new(false));
    let session_id = resume_session_id.clone().unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("session_{ts}")
    });

    // Things that turn up on their own — an update installing, the context
    // being compacted — go on the line under the input. The composer is where
    // the user's own actions are answered; a message they did not ask for
    // must not take that spot.
    let mut background: Option<BackgroundNotice> = None;
    let mut channel_switch: Option<ChannelSwitch> = None;
    let (channel_probe_tx, mut channel_probe_rx) =
        tokio::sync::mpsc::unbounded_channel::<ChannelTarget>();
    let mut pending_update: Option<(String, String, String, Option<String>)> = None;
    let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel::<UpdateNotice>();
    let (channel_watch_tx, mut channel_watch_rx) = tokio::sync::watch::channel(app_config.update_channel);
    if app_config.auto_check_updates && !flashagent_svc::updater::is_dev_mode() {
        let update_tx_clone = update_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(flashagent_svc::updater::BACKGROUND_UPDATE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let ch = *channel_watch_rx.borrow();
                        if let Ok(Some(target_ver)) = flashagent_svc::updater::check_and_apply_background(ch).await {
                            let _ = update_tx_clone.send(UpdateNotice::Ready { version: target_ver });
                            break;
                        }
                    }
                    changed = channel_watch_rx.changed() => {
                        if changed.is_ok() {
                            let ch = *channel_watch_rx.borrow();
                            interval.reset();
                            if let Ok(Some(target_ver)) = flashagent_svc::updater::check_and_apply_background(ch).await {
                                let _ = update_tx_clone.send(UpdateNotice::Ready { version: target_ver });
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
        });
    } else if app_config.silent_update_check && !flashagent_svc::updater::is_dev_mode() {
        let silent_tx_clone = update_tx.clone();
        let ch = app_config.update_channel;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            if let Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) =
                flashagent_svc::updater::check_for_updates(ch, flashagent_svc::updater::DEFAULT_RELEASES_API).await
            {
                let _ = silent_tx_clone.send(UpdateNotice::Available { version: target, asset_name, download_url, checksums_url });
            }
        });
    }

    let (term_w, term_h) = crossterm::terminal::size().unwrap_or((100, 24));
    let mut last_term_size = (term_w, term_h);
    let thinking_summary = if let Some(ref p) = source.profile() {
        if p.supported && !p.presets.is_empty() {
            format!("{} [{}]", current_effort, p.presets.join(", "))
        } else if !p.supported {
            "disabled (unsupported)".to_string()
        } else {
            current_effort.clone()
        }
    } else {
        current_effort.clone()
    };
    let initial_card = welcome_card_responsive_opts(
        &current_model,
        &cwd_display,
        perm.state().mode().label(),
        memory_docs,
        Some(&thinking_summary),
        current_context.as_deref(),
        term_w as usize,
        term_h as usize,
        0,
        app_config.show_mascot,
        MascotMood::Checking,
    );
    // The reveal is driven by the tick loop, which stops touching the card as
    // soon as the transcript has a user message in it. A resumed session puts
    // messages up immediately, so its card would stay stuck at whatever row
    // the reveal had reached — draw it whole instead.
    chat.update_welcome_card(opening_card(initial_card, resume_session_id.is_none()));

    if let Some(ref resume_id) = resume_session_id {
        if let Some(home) = flashagent_home_dir() {
            let session_path = home.join("sessions").join(format!("{resume_id}.json"));
            if let Ok(content) = std::fs::read_to_string(&session_path) {
                if let Ok(saved) = serde_json::from_str::<SavedSession>(&content) {
                    for saved_msg in saved.messages {
                        let msg: ChatMessage = saved_msg.into();
                        match msg.role {
                            flashagent_llm::Role::User => {
                                chat.push_user(extract_user_prompt(&msg.content));
                                history.push(msg);
                            }
                            flashagent_llm::Role::Assistant => {
                                if !msg.content.trim().is_empty() {
                                    chat.push_assistant(&msg.content);
                                }
                                history.push(msg);
                            }
                            // The fresh system prompt (current cwd/model) wins;
                            // only a compaction summary carries over. A second
                            // system message mid-history breaks strict chat
                            // templates (Gemma, Qwen).
                            flashagent_llm::Role::System => {
                                // Current sessions keep the summary inside the
                                // system prompt; older ones stored it as its own
                                // system message. Both carry the marker title.
                                let title = COMPACTED_MARK.trim_start();
                                if let Some(pos) = msg.content.find(title) {
                                    history[0].content.push_str(COMPACTED_MARK);
                                    history[0].content.push_str(msg.content[pos + title.len()..].trim_start());
                                }
                            }
                            flashagent_llm::Role::Tool => history.push(msg),
                        }
                    }
                    notice!(&format!("Resumed session '{resume_id}' ({} messages loaded).", history.len()));
                } else {
                    chat.push_line(LineKind::ToolError, format!("Session file {} is unreadable; starting fresh.", session_path.display()));
                }
            } else {
                chat.push_line(LineKind::ToolError, format!("No saved session '{resume_id}' in ~/.flashagent/sessions; starting fresh."));
            }
        }
    }

    let mut turn_started: Option<std::time::Instant> = None;
    let mut token_tracker = TokenTracker::new(current_model.clone());
    let mut max_steps: Option<u32> = app_config.max_steps;
    #[derive(Clone)]
    struct SavedGoalState {
        mode: PermissionMode,
        effort: String,
        max_steps: Option<u32>,
        task: String,
    }
    let mut goal_state: Option<SavedGoalState> = None;
    // Facts about the running /goal, accumulated from loop events for the
    // live progress line and the final report.
    let mut goal_ledger: Option<GoalLedger> = None;
    let mut copy_toast: Option<(String, std::time::Instant)> = None;
    let mut last_ctrl_c: Option<std::time::Instant> = None;
    let started_at = std::time::Instant::now();
    let mut last_mascot_mood = MascotMood::Checking;

    macro_rules! finish {
        () => {
            if app_config.auto_save_sessions && worth_saving(&history) {
                save_session_file(&session_id, &current_model, &cwd_display, &history)
                    .map(|_| session_id.clone())
            } else {
                None
            }
        };
    }

    'main_loop: loop {
        // The face reports the one thing that decides whether anything works:
        // did the model server answer. Discovery reruns every few seconds, so
        // starting the server later turns the face around on its own.
        let mascot_mood = if source.discovery().is_some() {
            MascotMood::Happy
        } else if started_at.elapsed() < std::time::Duration::from_secs(5) {
            MascotMood::Checking
        } else {
            MascotMood::Offline
        };
        let autocomplete = if !running
            && input.starts_with('/')
            && effort_menu.is_none()
            && model_menu.is_none()
            && settings_view.is_none()
            && sampling_view.is_none()
            && context_modal.is_none()
            && mcp_modal.is_none()
            && gate.pending().is_none()
            && question_gate.pending().is_none()
        {
            AutocompletePopup::for_input(&input, std::path::Path::new("."), autocomplete_idx)
        } else {
            None
        };

        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((100, 24));
        if (term_w, term_h) != last_term_size {
            last_term_size = (term_w, term_h);
            if !chat.has_user_message() {
                refresh_welcome_card_animated(
                    &mut chat,
                    &mut renderer,
                    &current_model,
                    &cwd_display,
                    perm.state().mode().label(),
                    memory_docs,
                    &source,
                    &current_effort,
                    current_context.as_deref(),
                    tick_n,
                    Some(term_w as usize),
                    app_config.show_mascot,
                    mascot_mood,
                    None,
                );
            }
        }
        let tip_lines = tip_animator.render_lines(tick_n, term_w as usize);

        if let Some((_, instant)) = copy_toast {
            if instant.elapsed().as_secs_f32() >= 2.5 {
                copy_toast = None;
            }
        }
        let active_toast = copy_toast.as_ref().map(|(msg, _)| msg.as_str());
        // Recomputed every frame: the elapsed part of it moves on its own.
        let channel_prompt: Option<String> = channel_switch.as_ref().map(|sw| {
            channel_switch_warning(sw.to, flashagent_svc::updater::current_version(), &sw.target)
        });
        let goal_progress: Option<String> =
            goal_ledger.as_ref().filter(|_| running).map(|l| l.progress());
        let live_prefill = token_tracker.live_prefill_status();
        let ttft_display = token_tracker.ttft_display();
        let tg_speed = token_tracker.tg_3s();

        renderer.frame(
            &chat,
            &gate,
            &question_gate,
            effort_menu.as_ref(),
            model_menu.as_ref(),
            settings_view.as_ref(),
            sampling_view.as_ref(),
            context_modal.as_ref(),
            mcp_modal.as_ref(),
            autocomplete.as_ref(),
            &context_usage,
            FrameState {
                input: &input,
                mode: perm.state().mode(),
                is_goal_active: goal_state.is_some(),
                goal_progress: goal_progress.as_deref(),
                tip: if app_config.show_tips { Some(tip_animator.tip_text) } else { None },
                tip_animated: None,
                tip_lines: if app_config.show_tips { Some(&tip_lines) } else { None },
                token_tracker: if app_config.show_tokens { Some(&token_tracker) } else { None },
                thinking_effort: &current_effort,
                reasoning_expand: ReasoningExpansion {
                    all: all_expanded,
                    last: last_expanded,
                },
                tick_n,
                running,
                elapsed_secs: turn_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                face_phase: turn_started.map(|t| (t.elapsed().as_millis() / 80) as usize).unwrap_or(0),
                model_tokens: if app_config.show_tokens { token_tracker.total_model_tokens } else { 0 },
                tokens_per_sec: if app_config.show_tokens { tg_speed } else { 0.0 },
                f_keep: if app_config.show_tokens { token_tracker.last_f_keep } else { None },
                model: &current_model,
                context_window: current_context.as_deref(),
                cwd: &cwd_display,
                confirm_selection: confirm_select.decision(),
                question_state: Some(&question_ui_state),
                custom_placeholder: custom_placeholder.as_deref(),
                suggested_prompt: suggested_prompt.as_deref(),
                copy_toast: if app_config.show_toasts { active_toast } else { None },
                prefill_status: if app_config.show_ttft { live_prefill.as_deref() } else { None },
                ttft_display: if app_config.show_ttft { ttft_display.as_deref() } else { None },
                background: background.as_ref().map(|b| b.text.as_str()),
                channel_prompt: channel_prompt.as_deref(),
                background_style: background.as_ref().map_or(NoticeStyle::FULL, BackgroundNotice::style),
                context_warn_threshold: app_config.context_warn_threshold,
            },
        );

        let ev = tokio::select! {
            Some(target) = channel_probe_rx.recv() => {
                if let Some(sw) = channel_switch.as_mut() {
                    sw.target = target;
                    renderer.request_reprint();
                }
                continue;
            }
            Some(notice) = update_rx.recv() => {
                match notice {
                    UpdateNotice::Available { version, asset_name, download_url, checksums_url } => {
                        pending_update = Some((version.clone(), asset_name, download_url, checksums_url));
                        background = Some(BackgroundNotice::sticky(format!(
                            "Update available: {version} · press Ctrl+U to install"
                        )));
                        if let Some(ref mut s) = settings_view {
                            s.update_check_status = Some(format!("Available: {version} (Press Ctrl+U)"));
                        }
                    }
                    UpdateNotice::Progress { version, stage } => {
                        background = Some(BackgroundNotice::sticky(update_progress_line(&version, stage)));
                        renderer.request_reprint();
                    }
                    UpdateNotice::Ready { version } => {
                        pending_update = None;
                        background = Some(BackgroundNotice::sticky(format!(
                            "Update ready: {version} · restart FlashAgent to run it"
                        )));
                        if let Some(ref mut s) = settings_view {
                            s.update_check_status = Some(format!("Ready: {version} (restart to apply)"));
                        }
                    }
                    UpdateNotice::UpToDate { version } => {
                        if let Some(ref mut s) = settings_view {
                            s.update_check_status = Some(format!("Up to date ({version})"));
                        }
                        background = Some(BackgroundNotice::fading(
                            format!("FlashAgent {version} is up to date"),
                            6,
                        ));
                    }
                    UpdateNotice::Failed { error } => {
                        if let Some(ref mut s) = settings_view {
                            s.update_check_status = Some(format!("Error: {error}"));
                        }
                        background = Some(BackgroundNotice::fading(
                            format!("Update failed: {}", flashagent_tui::truncate_middle(&error, 90)),
                            10,
                        ));
                    }
                }
                renderer.request_reprint();
                continue;
            }
            Some(ev) = rx.recv() => {
                tick_n += 1;
                ev
            }
            _ = tick.tick() => {
                tick_n += 1;
                tip_animator.tick();
                if background.as_ref().is_some_and(BackgroundNotice::expired) {
                    background = None;
                    renderer.request_reprint();
                }
                // The loop did not wind down in time (a tool ignoring
                // cancellation): abort it. History keeps the prompt but not
                // the partial turn, and the user is told so.
                if running && cancel_requested.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(3)) {
                    if let Some(handle) = active_turn_handle.take() {
                        handle.abort();
                    }
                    running = false;
                    turn_started = None;
                    cancel_requested = None;
                    aborted_turn = Some(turn_counter);
                    close_dangling_user(&mut history, "[turn aborted by the user]");
                    token_tracker.on_finished();
                    if let Some(saved) = goal_state.take() {
                        tools_arc.set_goal_mode(false);
                        perm.state().set_mode(saved.mode);
                        current_effort = saved.effort.clone();
                        max_steps = saved.max_steps;
                        if let Some(ledger) = goal_ledger.take() {
                            push_goal_report(&mut chat, &ledger, DoneReason::Cancelled);
                        }
                    }
                    chat.on_event(&flashagent_core::LoopEvent::Done(flashagent_core::DoneReason::Cancelled));
                    custom_placeholder = Some("Turn aborted; its partial output was not kept in the model context".to_string());
                    renderer.request_reprint();
                }
                let current_term_size = crossterm::terminal::size().unwrap_or((100, 24));
                let term_resized = current_term_size != last_term_size;
                if term_resized {
                    last_term_size = current_term_size;
                }
                // The mascot breathes and blinks, and the card draws itself
                // in on start-up; rebuild it only on the ticks where it
                // actually looks different.
                let reveal_rows = welcome_reveal_rows(started_at);
                let mood_changed = mascot_mood != last_mascot_mood;
                last_mascot_mood = mascot_mood;
                if !running
                    && !chat.has_user_message()
                    && (term_resized
                        || mood_changed
                        || reveal_rows.is_some()
                        || flashagent_tui::mascot_needs_repaint(tick_n))
                {
                    refresh_welcome_card_animated(
                        &mut chat,
                        &mut renderer,
                        &current_model,
                        &cwd_display,
                        perm.state().mode().label(),
                        memory_docs,
                        &source,
                        &current_effort,
                        current_context.as_deref(),
                        tick_n,
                        Some(current_term_size.0 as usize),
                        app_config.show_mascot,
                        mascot_mood,
                        reveal_rows,
                    );
                }
                if term_resized {
                    renderer.request_reprint();
                }
                continue;
            }
            _ = check_interval.tick() => {
                if !running && !is_discovering.load(Ordering::Relaxed) {
                    is_discovering.store(true, Ordering::Relaxed);
                    let source_bg = source.clone();
                    let tx_bg = tx.clone();
                    let flag = is_discovering.clone();
                    tokio::spawn(async move {
                        if let Some(disc) = source_bg.discover_server().await {
                            let _ = tx_bg.send(UiEvent::ServerDiscovered(disc));
                        }
                        flag.store(false, Ordering::Relaxed);
                    });
                }
                continue;
            }
        };

        match ev {
            UiEvent::BackgroundRecap { turn_id, recap, suggestion } => {
                let formatted = format!("  \x1b[38;2;155;165;180mrecap:\x1b[0m \x1b[38;2;225;230;240m{recap}\x1b[0m");
                // `turn_id` is the ordinal of the user message the recap is
                // about; regenerate and steering make turn_counter drift.
                let current_turn = chat.user_turn_count() as u64;
                if turn_id == current_turn {
                    chat.update_or_push_turn_system("recap:", &formatted);
                    latest_suggestion = suggestion.clone();
                    custom_placeholder = None;
                    if input.is_empty() && active_turn_handle.is_none() {
                        suggested_prompt = suggestion;
                    }
                    renderer.request_reprint();
                } else if turn_id < current_turn {
                    chat.attach_turn_recap(turn_id, &formatted);
                    renderer.request_reprint();
                }
            }
            UiEvent::ToolTestResult(verdict) => {
                match settings_view.as_mut() {
                    Some(s) => s.tool_test_status = Some(verdict),
                    None => notice!(&format!("Tool test: {verdict}")),
                }
                renderer.request_reprint();
            }
            UiEvent::ServerDiscovered(disc) => {
                let url = disc.base_url.to_lowercase();
                let is_lm_studio = url.contains("1234") || url.contains("lmstudio");
                let has_loaded = disc.models.iter().any(|m| m.is_loaded);
                available_models = if is_lm_studio && has_loaded {
                    disc.models.iter().filter(|m| m.is_loaded).map(|m| m.id.clone()).collect()
                } else {
                    disc.models.iter().map(|m| m.id.clone()).collect()
                };
                if let Some(active) = disc.active_model {
                    let new_ctx_len = active.context_length.or(active.max_context_length).unwrap_or(131_072);
                    let new_ctx_disp = active.context_display();
                    let model_changed = active.id != current_model;
                    let ctx_changed = context_usage.total_capacity != new_ctx_len || current_context != new_ctx_disp;

                    if model_changed || ctx_changed {
                        let old_m = current_model.clone();
                        let old_ctx_len = context_usage.total_capacity;
                        current_model = active.id.clone();
                        current_context = new_ctx_disp;
                        context_usage.total_capacity = new_ctx_len.max(1024);
                        tools_arc.set_context_window(Some(new_ctx_len));
                        source.set_model(&current_model);
                        update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);

                        if model_changed {
                            app_config.model = current_model.clone();
                            let _ = app_config.save();
                            if !active.thinking.supported {
                                current_effort = "off".to_string();
                            } else if current_effort == "off" || current_effort.is_empty() {
                                current_effort = "auto".to_string();
                            }
                        }

                        let ctx_tag = current_context.as_deref().unwrap_or("");
                        refresh_welcome_card_if_before_user_msg(
                            &mut chat,
                            &mut renderer,
                            &current_model,
                            &cwd_display,
                            perm.state().mode().label(),
                            memory_docs,
                            &source,
                            &current_effort,
                            current_context.as_deref(),
                            app_config.show_mascot,
                            mascot_mood,
                        );

                        if model_changed {
                            let msg = format!(
                                "Server active model switched: {old_m} -> {current_model}"
                            );
                            custom_placeholder = Some(msg);
                            suggested_prompt = None;
                        } else if ctx_changed {
                            let old_formatted = ContextUsage::format_tokens(old_ctx_len);
                            let new_formatted = ContextUsage::format_tokens(new_ctx_len);
                            let msg = format!(
                                "Model context capacity: {old_formatted} -> {new_formatted} ({ctx_tag})"
                            );
                            custom_placeholder = Some(msg);
                            suggested_prompt = None;
                        }
                        renderer.request_reprint();
                    }
                }
            }
            UiEvent::Loop { turn_id, event: e } => {
                // Late events of a turn that was aborted or superseded.
                if turn_id != turn_counter || aborted_turn == Some(turn_id) {
                    continue;
                }
                match &e {
                    LoopEvent::TurnDelta(text) => {
                        token_tracker.on_delta(text);
                    }
                    LoopEvent::ReasoningDelta(text) => {
                        token_tracker.on_delta(text);
                    }
                    LoopEvent::ToolStarted { name, args_json, .. } => {
                        token_tracker.on_delta(name);
                        token_tracker.on_delta(args_json);
                    }
                    LoopEvent::Usage(u) => {
                        token_tracker.on_usage(u);
                    }
                    _ => {}
                }
                if let Some(ledger) = goal_ledger.as_mut() {
                    ledger.on_event(&e);
                    if matches!(e, LoopEvent::StepStarted { .. }) {
                        renderer.request_reprint();
                    }
                }
                chat.on_event(&e);
                if chat.take_needs_reprint() {
                    renderer.request_reprint();
                }
                update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
            }
            UiEvent::Finished { turn_id, result: res } => {
                if turn_id != turn_counter || aborted_turn == Some(turn_id) {
                    // A turn that was hard-aborted (or superseded) reporting late.
                    continue;
                }
                running = false;
                turn_started = None;
                active_turn_handle = None;
                active_steer_tx = None;
                cancel_requested = None;
                cancel.store(false, Ordering::Relaxed);
                token_tracker.on_finished();

                // Roll back any temporary goal state
                if let Some(saved) = goal_state.take() {
                    tools_arc.set_goal_mode(false);
                    perm.state().set_mode(saved.mode);
                    current_effort = saved.effort.clone();
                    max_steps = saved.max_steps;
                    if let Some(ledger) = goal_ledger.take() {
                        let reason = match &res {
                            Ok((_, r)) => *r,
                            Err(_) => DoneReason::Failed,
                        };
                        push_goal_report(&mut chat, &ledger, reason);
                    }
                    notice!(&format!(
                        "[Goal \"{}\" finished · Restored mode to {} and thinking to {}]",
                        flashagent_tui::truncate_middle(&saved.task, 40),
                        saved.mode.label(),
                        saved.effort
                    ));
                }

                match res {
                    Ok((h, reason)) => {
                        history = h;
                        update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);

                        if reason == DoneReason::Cancelled {
                            close_dangling_user(&mut history, "[interrupted by the user before replying]");
                            suggested_prompt = None;
                            let interrupt_msg = "Request interrupted by user";
                            custom_placeholder = Some(interrupt_msg.to_string());
                            renderer.request_reprint();
                        } else {
                            // Auto-compact context when reaching threshold capacity
                            if app_config.auto_compact_context
                                && context_usage.percentage() >= app_config.context_compact_threshold as f32
                                && history.len() > 3
                            {
                                let source_compact = source.clone();
                                if let Some(freed) = compact_context(&source_compact, &mut history, None).await {
                                    update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                                    background = Some(BackgroundNotice::fading(
                                        format!("Context auto-compacted (~{} freed)", ContextUsage::format_tokens(freed)),
                                        8,
                                    ));
                                }
                            }

                            if app_config.auto_save_sessions {
                                save_session_file(&session_id, &current_model, &cwd_display, &history);
                            }

                            // Clear any existing ghost suggestion.
                            // No hardcoded or heuristic fallback strings for recap or write-in suggestions:
                            // they are ONLY shown if and when dynamically generated by the background LLM task.
                            suggested_prompt = None;
                            renderer.request_reprint();

                            // Asynchronously ask LLM in background for refined recap & contextual suggestion
                            let source_bg = source.clone();
                            let tx_bg = tx.clone();
                            let history_bg = history.clone();
                            let turn_id = chat.user_turn_count() as u64;
                            tokio::spawn(async move {
                                if let Some((llm_recap, llm_suggestion)) = generate_llm_recap_and_suggestion(&source_bg, &history_bg).await {
                                    let _ = tx_bg.send(UiEvent::BackgroundRecap {
                                        turn_id,
                                        recap: llm_recap,
                                        suggestion: llm_suggestion,
                                    });
                                }
                            });
                        }
                    }
                    Err((e, h)) => {
                        // Keep the steps that already ran (and changed files).
                        history = h;
                        close_dangling_user(&mut history, "[no reply: the model backend failed]");
                        update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                        chat.on_event(&LoopEvent::Done(DoneReason::Failed));
                        chat.push_line(LineKind::ToolError, format!("Error: {e}"));
                        notice!("The model backend failed — press Ctrl+R to retry the last prompt.");
                    }
                }
            }
            UiEvent::Resize(cols, rows) => {
                let term_resized = (cols, rows) != last_term_size;
                last_term_size = (cols, rows);
                if !chat.has_user_message() && term_resized {
                    refresh_welcome_card_animated(
                        &mut chat,
                        &mut renderer,
                        &current_model,
                        &cwd_display,
                        perm.state().mode().label(),
                        memory_docs,
                        &source,
                        &current_effort,
                        current_context.as_deref(),
                        tick_n,
                        Some(cols as usize),
                        app_config.show_mascot,
                        mascot_mood,
                        None,
                    );
                }
                renderer.request_reprint();
            }
            UiEvent::Mouse(m) => {
                let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
                let total_chat_lines = chat.render_split(width as usize, ReasoningExpansion { all: all_expanded, last: last_expanded }).0.len() + 15;
                let max_scroll = total_chat_lines.saturating_sub(height as usize);
                if let Some(ref mut menu) = model_menu {
                    match m.kind {
                        MouseEventKind::ScrollUp => menu.up(),
                        MouseEventKind::ScrollDown => menu.down(),
                        _ => {}
                    }
                    renderer.request_reprint();
                } else if let Some(ref mut menu) = effort_menu {
                    match m.kind {
                        MouseEventKind::ScrollUp => menu.up(),
                        MouseEventKind::ScrollDown => menu.down(),
                        _ => {}
                    }
                    renderer.request_reprint();
                } else {
                    match m.kind {
                        MouseEventKind::ScrollUp => {
                            renderer.scroll_up(3, max_scroll);
                        }
                        MouseEventKind::ScrollDown => {
                            renderer.scroll_down(3);
                        }
                        _ => {}
                    }
                }
            }
            UiEvent::Paste(pasted) => {
                let sanitized = pasted.replace("\r\n", " ").replace(['\n', '\r'], " ");
                if !sanitized.is_empty() {
                    if question_gate.pending().is_some() {
                        question_ui_state.write_in_text.push_str(&sanitized);
                    } else if let Some(ref mut sm) = sampling_view {
                        for ch in sanitized.chars() {
                            sm.handle_key(KeyCode::Char(ch), KeyModifiers::NONE);
                        }
                    } else if effort_menu.is_none() && model_menu.is_none() && settings_view.is_none() && context_modal.is_none() && mcp_modal.is_none() {
                        input.push_str(&sanitized);
                        history_index = None;
                        autocomplete_idx = 0;
                    }
                    renderer.request_reprint();
                }
            }
            UiEvent::Key(code, mods) => {
                // The channel card owns the keyboard while it is up: it is a
                // yes-or-no about replacing the binary, and typing past it
                // would leave the answer ambiguous.
                if let Some(sw) = channel_switch.take() {
                    let yes = matches!(
                        code,
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('\u{043d}')
                            | KeyCode::Char('\u{041d}') | KeyCode::Enter
                    );
                    if yes {
                        app_config.update_channel = sw.to;
                        let _ = app_config.save();
                        if channel_watch_tx.receiver_count() > 0 {
                            let _ = channel_watch_tx.send(sw.to);
                        }
                        if let Some(ref mut view) = settings_view {
                            view.config.update_channel = sw.to;
                        }
                        background = Some(BackgroundNotice::sticky(format!(
                            "Release channel is now {} \u{b7} press Ctrl+U to move to it",
                            sw.to.label()
                        )));
                    } else {
                        // Everything else the user changed stayed applied; only
                        // this one is put back.
                        if let Some(ref mut view) = settings_view {
                            view.config.update_channel = sw.from;
                        }
                        background = Some(BackgroundNotice::fading(
                            format!("Still on the {} channel", sw.from.label()),
                            5,
                        ));
                    }
                    renderer.request_reprint();
                    continue;
                }
                // If settings view is open, it captures all keyboard input
                if let Some(ref mut settings) = settings_view {
                    // What the view was opened with (live session values).
                    let shown_mode = goal_state.as_ref().map_or(perm.state().mode(), |g| g.mode);
                    let shown_effort = goal_state.as_ref().map_or(current_effort.clone(), |g| g.effort.clone());
                    let action = settings.handle_key(code, mods);
                    match action {
                        SettingsAction::Close => {
                            let old_model = current_model.clone();
                            let old_effort = shown_effort.clone();
                            let old_mode = shown_mode;
                            let chosen_mode = settings.config.permission_mode;
                            let chosen_effort = settings.config.thinking_effort.clone();
                            let mut changes = Vec::new();
                            if settings.config.permission_mode != old_mode {
                                changes.push(format!("mode ({})", settings.config.permission_mode.label()));
                            }
                            if settings.config.model != old_model {
                                changes.push(format!("model ({})", settings.config.model));
                            }
                            if settings.config.thinking_effort != old_effort {
                                changes.push(format!("thinking ({})", settings.config.thinking_effort));
                            }

                            if changes.len() == 1 && settings.config.permission_mode != old_mode {
                                custom_placeholder = Some(format!("Permission mode set to: {}", settings.config.permission_mode.label()));
                            } else if !changes.is_empty() {
                                custom_placeholder = Some(format!("Settings updated: {}", changes.join(", ")));
                            }
                            suggested_prompt = None;

                            // Every other setting is applied now; the release
                            // channel waits for an answer, so declining costs
                            // the user nothing else they just changed.
                            let wanted_channel = settings.config.update_channel;
                            let mut applied = persisted_from_view(&settings.config, &app_config, shown_mode, &shown_effort);
                            if wanted_channel != app_config.update_channel {
                                applied.update_channel = app_config.update_channel;
                                custom_placeholder = None;
                                channel_switch = Some(ChannelSwitch {
                                    from: app_config.update_channel,
                                    to: wanted_channel,
                                    target: ChannelTarget::Checking,
                                });
                                let tx_ch = channel_probe_tx.clone();
                                tokio::spawn(async move {
                                    let target = match flashagent_svc::updater::newest_on_channel(
                                        wanted_channel,
                                        flashagent_svc::updater::DEFAULT_RELEASES_API,
                                    )
                                    .await
                                    {
                                        Ok(Some(version)) => ChannelTarget::Version(version),
                                        Ok(None) => ChannelTarget::Empty,
                                        Err(_) => ChannelTarget::Unknown,
                                    };
                                    let _ = tx_ch.send(target);
                                });
                            }
                            app_config = applied;
                            let _ = app_config.save();
                            tools_arc.set_toolset_profile(app_config.toolset_profile);
                            tools_arc.set_web_enabled(app_config.free_search);
                            source.0.set_max_retries(app_config.network_retries);
                            if app_config.model != current_model {
                                current_model = app_config.model.clone();
                                source.set_model(&current_model);
                            }
                            // During /goal the live mode/effort are the goal's;
                            // edits apply to what the goal restores afterwards.
                            match goal_state.as_mut() {
                                Some(g) => {
                                    g.mode = chosen_mode;
                                    g.effort = chosen_effort;
                                }
                                None => {
                                    perm.state().set_mode(chosen_mode);
                                    current_effort = chosen_effort;
                                }
                            }
                            settings_view = None;
                            refresh_welcome_card_if_before_user_msg(
                                &mut chat,
                                &mut renderer,
                                &current_model,
                                &cwd_display,
                                perm.state().mode().label(),
                                memory_docs,
                                &source,
                                &current_effort,
                                current_context.as_deref(),
                                app_config.show_mascot,
                                mascot_mood,
                            );
                            renderer.request_reprint();
                        }
                        SettingsAction::DiscoverModels => {
                            app_config = persisted_from_view(&settings.config, &app_config, shown_mode, &shown_effort);
                            let _ = app_config.save();
                            // Probe the URL the user just typed, not the one
                            // this session is connected to.
                            let key = settings.config.api_key.clone().or_else(|| std::env::var("FLASHAGENT_API_KEY").ok());
                            let probe = flashagent_llm::OpenAiCompat::new(&settings.config.backend_url, "", key);
                            settings.available_models = match probe.discover_server().await {
                                Some(disc) => disc.models.into_iter().map(|m| m.id).collect(),
                                None => Vec::new(),
                            };
                            if settings.config.backend_url.trim_end_matches('/') != source.0.base_url() {
                                custom_placeholder = Some("Backend URL saved; restart FlashAgent to connect to it".to_string());
                            }
                            renderer.request_reprint();
                        }
                        SettingsAction::RunToolTest => {
                            settings.tool_test_status = Some(format!("Probing {current_model}..."));
                            let source_bg = source.clone();
                            let tx_bg = tx.clone();
                            tokio::spawn(async move {
                                let verdict = run_tool_call_probe(&source_bg).await;
                                let _ = tx_bg.send(UiEvent::ToolTestResult(verdict));
                            });
                            renderer.request_reprint();
                        }
                        SettingsAction::OpenModelMenu => {
                            app_config = persisted_from_view(&settings.config, &app_config, shown_mode, &shown_effort);
                            let _ = app_config.save();
                            settings_view = None;
                            if let Some(mut menu) = build_model_menu(&source) {
                                menu.select_by_value(&current_model);
                                model_menu = Some(menu);
                            } else {
                                notice!("[No models discovered from server]");
                            }
                            renderer.request_reprint();
                        }
                        SettingsAction::OpenEffortMenu => {
                            app_config = persisted_from_view(&settings.config, &app_config, shown_mode, &shown_effort);
                            let _ = app_config.save();
                            settings_view = None;
                            let mut menu = build_effort_menu(&source);
                            menu.select_by_value(&current_effort);
                            effort_menu = Some(menu);
                            renderer.request_reprint();
                        }
                        SettingsAction::OpenWizard => {
                            app_config = persisted_from_view(&settings.config, &app_config, shown_mode, &shown_effort);
                            let _ = app_config.save();
                            settings_view = None;
                            let completed = flashagent_tui::run_wizard_channel(&mut app_config, &mut rx).await.unwrap_or(false);
                            if completed {
                                current_model = app_config.model.clone();
                                source.set_model(&current_model);
                                current_effort = app_config.thinking_effort.clone();
                                perm.state().set_mode(app_config.permission_mode);
                                refresh_welcome_card_if_before_user_msg(
                                    &mut chat,
                                    &mut renderer,
                                    &current_model,
                                    &cwd_display,
                                    perm.state().mode().label(),
                                    memory_docs,
                                    &source,
                                    &current_effort,
                                    current_context.as_deref(),
                                    app_config.show_mascot,
                                    mascot_mood,
                                );
                            }
                            renderer.request_reprint();
                        }
                        SettingsAction::OpenSamplingMenu => {
                            app_config = persisted_from_view(&settings.config, &app_config, shown_mode, &shown_effort);
                            let _ = app_config.save();
                            settings_view = None;
                            sampling_view = Some(SamplingView::new(&app_config));
                            renderer.request_reprint();
                        }
                        SettingsAction::CheckUpdatesNow => {
                            if flashagent_svc::updater::is_dev_mode() {
                                settings.update_check_status = Some("Dev mode: updates disabled (source build)".into());
                            } else {
                                settings.update_check_status = Some("Checking GitHub releases...".into());
                                let ch = settings.config.update_channel;
                                let update_tx_clone = update_tx.clone();
                                tokio::spawn(async move {
                                    match flashagent_svc::updater::check_for_updates(ch, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                                        Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                            let _ = update_tx_clone.send(UpdateNotice::Available { version: target, asset_name, download_url, checksums_url });
                                        }
                                        Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, .. }) => {
                                            let _ = update_tx_clone.send(UpdateNotice::UpToDate { version: current });
                                        }
                                        Err(e) => {
                                            let _ = update_tx_clone.send(UpdateNotice::Failed { error: e.to_string() });
                                        }
                                    }
                                });
                            }
                            renderer.request_reprint();
                        }
                        SettingsAction::OpenMcpMenu => {
                            app_config = persisted_from_view(&settings.config, &app_config, shown_mode, &shown_effort);
                            let _ = app_config.save();
                            settings_view = None;
                            let mgr = tools_arc.mcp_manager();
                            let paths = mgr.loaded_paths();
                            let statuses = mgr.server_status_list().await;
                            mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Overview));
                            renderer.request_reprint();
                        }
                        SettingsAction::None => {
                            renderer.request_reprint();
                        }
                    }
                    continue;
                }

                // If sampling parameters view is open, it captures all keyboard input
                if let Some(ref mut sm) = sampling_view {
                    let action = sm.handle_key(code, mods);
                    match action {
                        SamplingAction::Close => {
                            sampling_view = None;
                            renderer.request_reprint();
                        }
                        SamplingAction::SaveAndClose => {
                            sm.apply_to_config(&mut app_config);
                            let _ = app_config.save();
                            sampling_view = None;
                            custom_placeholder = Some("Sampling parameters updated".to_string());
                            suggested_prompt = None;
                            renderer.request_reprint();
                        }
                        SamplingAction::None => {
                            renderer.request_reprint();
                        }
                    }
                    continue;
                }

                // If context modal is open, F1, Enter, Esc or 'q' closes it
                if context_modal.is_some() {
                    if matches!(code, KeyCode::F(1) | KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q')) {
                        context_modal = None;
                        renderer.request_reprint();
                    }
                    continue;
                }

                // If MCP modal is open, it captures navigation and actions
                if let Some(ref mut modal) = mcp_modal {
                    match modal.handle_key(code, mods) {
                        McpModalAction::Close => {
                            mcp_modal = None;
                            renderer.request_reprint();
                        }
                        McpModalAction::Reload => {
                            let mgr = tools_arc.mcp_manager();
                            let _ = mgr.reload().await;
                            modal.servers = mgr.server_status_list().await;
                            modal.status_message = Some("Reloaded MCP configurations".to_string());
                            renderer.request_reprint();
                        }
                        McpModalAction::TestServer(name) => {
                            modal.status_message = Some(format!("Testing {name}..."));
                            renderer.request_reprint();
                            let mgr = tools_arc.mcp_manager();
                            match mgr.test_server(&name).await {
                                Ok(report) => {
                                    let ver = report.server_version.as_deref().unwrap_or("1.0.0");
                                    let lat = format!("{:.1}ms", report.latency.as_secs_f64() * 1000.0);
                                    modal.status_message = Some(format!("✔ {name} connected (v{ver}, {lat}, {} tools)", report.tools.len()));
                                }
                                Err(e) => {
                                    modal.status_message = Some(format!("✕ {name} failed: {e}"));
                                }
                            }
                            modal.servers = mgr.server_status_list().await;
                            renderer.request_reprint();
                        }
                        McpModalAction::InstallMarketplace(id) => {
                            if let Some(item) = flashagent_tools::mcp::find_marketplace_item(&id) {
                                let cfg = flashagent_tools::mcp::scaffold_config(item);
                                match flashagent_tools::mcp::save_server_to_project(std::path::Path::new("."), item.id, cfg) {
                                    Ok(path) => {
                                        modal.status_message = Some(format!("✔ Added {} to {}", item.name, path.display()));
                                        let mgr = tools_arc.mcp_manager();
                                        let server_id = item.id.to_string();
                                        let mgr_clone = mgr.clone();
                                        tokio::spawn(async move {
                                            let _ = mgr_clone.reload().await;
                                            let _ = mgr_clone.start_server(&server_id).await;
                                        });
                                    }
                                    Err(e) => {
                                        modal.status_message = Some(format!("✕ Failed to install {id}: {e}"));
                                    }
                                }
                            }
                            renderer.request_reprint();
                        }
                        McpModalAction::None => {
                            renderer.request_reprint();
                        }
                    }
                    continue;
                }

                // If model selection menu is open, it captures navigation
                if let Some(ref mut menu) = model_menu {
                    match code {
                        KeyCode::Up => menu.up(),
                        KeyCode::Down => menu.down(),
                        KeyCode::PageUp => menu.page_up(),
                        KeyCode::PageDown => menu.page_down(),
                        KeyCode::Backspace => menu.pop_filter_char(),
                        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                            menu.push_filter_char(c);
                        }
                        KeyCode::Enter => {
                            if let Some(val) = menu.selected_value() {
                                current_model = val.clone();
                                app_config.model = current_model.clone();
                                let _ = app_config.save();
                                source.set_model(&current_model);
                                if let Some(disc) = source.discovery() {
                                    if let Some(m) = disc.models.iter().find(|m| m.id == current_model) {
                                        current_context = m.context_display();
                                        if !m.thinking.supported {
                                            current_effort = "off".to_string();
                                        } else if current_effort == "off" || current_effort.is_empty() {
                                            current_effort = "auto".to_string();
                                        }
                                    }
                                }
                                refresh_welcome_card_if_before_user_msg(
                                    &mut chat,
                                    &mut renderer,
                                    &current_model,
                                    &cwd_display,
                                    perm.state().mode().label(),
                                    memory_docs,
                                    &source,
                                    &current_effort,
                                    current_context.as_deref(),
                                    app_config.show_mascot,
                                    mascot_mood,
                                );
                                custom_placeholder = Some(format!("Switched active model to: {current_model}"));
                                suggested_prompt = None;
                                renderer.request_reprint();
                            }
                            model_menu = None;
                        }
                        KeyCode::Esc => {
                            model_menu = None;
                        }
                        _ => {}
                    }
                    continue;
                }

                // If effort selection menu is open, it captures navigation
                if let Some(ref mut menu) = effort_menu {
                    match code {
                        KeyCode::Up => menu.up(),
                        KeyCode::Down => menu.down(),
                        KeyCode::Enter => {
                            if let Some(val) = menu.selected_value() {
                                current_effort = val.clone();
                                refresh_welcome_card_if_before_user_msg(
                                    &mut chat,
                                    &mut renderer,
                                    &current_model,
                                    &cwd_display,
                                    perm.state().mode().label(),
                                    memory_docs,
                                    &source,
                                    &current_effort,
                                    current_context.as_deref(),
                                    app_config.show_mascot,
                                    mascot_mood,
                                );
                                custom_placeholder = Some(format!("Thinking effort set to: {current_effort}"));
                                suggested_prompt = None;
                                renderer.request_reprint();
                            }
                            effort_menu = None;
                        }
                        KeyCode::Esc => {
                            effort_menu = None;
                        }
                        _ => {}
                    }
                    continue;
                }

                if let Some(req) = question_gate.pending() {
                    match code {
                        // Ctrl+C interrupts the turn, as everywhere else while it runs.
                        KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                            if mods.contains(KeyModifiers::CONTROL) =>
                        {
                            question_ui_state = QuestionUiState::default();
                            question_gate.cancel();
                            if running && cancel_requested.is_none() {
                                cancel.store(true, Ordering::Relaxed);
                                cancel_requested = Some(std::time::Instant::now());
                                active_steer_tx = None;
                                custom_placeholder = Some("Interrupting...".to_string());
                            }
                        }
                        KeyCode::Esc => {
                            if req.options.is_some() && question_ui_state.is_writing {
                                question_ui_state.is_writing = false;
                            } else {
                                question_ui_state = QuestionUiState::default();
                                question_gate.cancel();
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(ref opts) = req.options {
                                let total_choices = opts.len();
                                if question_ui_state.is_writing {
                                    let answer = std::mem::take(&mut question_ui_state.write_in_text);
                                    let trimmed = answer.trim().to_string();
                                    if !trimmed.is_empty() {
                                        if req.multi_select && !question_ui_state.selected_indices.is_empty() {
                                            let mut chosen: Vec<String> = question_ui_state.selected_indices.iter().filter_map(|&i| opts.get(i).cloned()).collect();
                                            chosen.push(trimmed);
                                            question_ui_state = QuestionUiState::default();
                                            question_gate.respond(chosen.join(", "), true);
                                        } else {
                                            question_ui_state = QuestionUiState::default();
                                            question_gate.respond(trimmed, true);
                                        }
                                    }
                                } else if question_ui_state.selected_index == total_choices {
                                    question_ui_state.is_writing = true;
                                    question_ui_state.write_in_text.clear();
                                } else if req.multi_select {
                                    let mut chosen: Vec<String> = question_ui_state.selected_indices.iter().filter_map(|&i| opts.get(i).cloned()).collect();
                                    if chosen.is_empty() && question_ui_state.selected_index < total_choices {
                                        chosen.push(opts[question_ui_state.selected_index].clone());
                                    }
                                    question_ui_state = QuestionUiState::default();
                                    question_gate.respond(chosen.join(", "), false);
                                } else if question_ui_state.selected_index < total_choices {
                                    let chosen = opts[question_ui_state.selected_index].clone();
                                    question_ui_state = QuestionUiState::default();
                                    question_gate.respond(chosen, false);
                                }
                            } else {
                                let answer = std::mem::take(&mut question_ui_state.write_in_text);
                                let trimmed = answer.trim().to_string();
                                question_ui_state = QuestionUiState::default();
                                question_gate.respond(trimmed, true);
                            }
                        }
                        KeyCode::Up => {
                            if !question_ui_state.is_writing {
                                if let Some(ref opts) = req.options {
                                    let total = opts.len() + 1;
                                    if question_ui_state.selected_index == 0 {
                                        question_ui_state.selected_index = total.saturating_sub(1);
                                    } else {
                                        question_ui_state.selected_index -= 1;
                                    }
                                }
                            }
                        }
                        KeyCode::Down => {
                            if !question_ui_state.is_writing {
                                if let Some(ref opts) = req.options {
                                    let total = opts.len() + 1;
                                    question_ui_state.selected_index = (question_ui_state.selected_index + 1) % total;
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            if question_ui_state.is_writing || req.options.is_none() {
                                question_ui_state.write_in_text.pop();
                            }
                        }
                        KeyCode::Char(' ') if req.multi_select && !question_ui_state.is_writing => {
                            if let Some(ref opts) = req.options {
                                let total_choices = opts.len();
                                if question_ui_state.selected_index < total_choices {
                                    let idx = question_ui_state.selected_index;
                                    if question_ui_state.selected_indices.contains(&idx) {
                                        question_ui_state.selected_indices.remove(&idx);
                                    } else {
                                        question_ui_state.selected_indices.insert(idx);
                                    }
                                } else {
                                    question_ui_state.is_writing = true;
                                    question_ui_state.write_in_text.clear();
                                }
                            }
                        }
                        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                            if question_ui_state.is_writing || req.options.is_none() {
                                question_ui_state.write_in_text.push(c);
                            } else if let Some(ref opts) = req.options {
                                let total = opts.len() + 1;
                                if let Some(d) = c.to_digit(10) {
                                    let idx = (d as usize).saturating_sub(1);
                                    if idx < opts.len() {
                                        if req.multi_select {
                                            if question_ui_state.selected_indices.contains(&idx) {
                                                question_ui_state.selected_indices.remove(&idx);
                                            } else {
                                                question_ui_state.selected_indices.insert(idx);
                                            }
                                        }
                                        question_ui_state.selected_index = idx;
                                    } else if idx == total - 1 {
                                        question_ui_state.selected_index = idx;
                                        question_ui_state.is_writing = true;
                                        question_ui_state.write_in_text.clear();
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    renderer.request_reprint();
                    continue;
                }

                if gate.pending().is_some() {
                    match code {
                        // Ctrl+C refuses the call and interrupts the turn.
                        KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                            if mods.contains(KeyModifiers::CONTROL) =>
                        {
                            gate.respond(Decision::Deny);
                            confirm_select = ConfirmSelect::new();
                            if running && cancel_requested.is_none() {
                                cancel.store(true, Ordering::Relaxed);
                                cancel_requested = Some(std::time::Instant::now());
                                active_steer_tx = None;
                                custom_placeholder = Some("Interrupting...".to_string());
                            }
                        }
                        KeyCode::Esc
                        | KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('\u{0432}') | KeyCode::Char('\u{0412}') => {
                            gate.respond(Decision::Deny);
                            confirm_select = ConfirmSelect::new();
                        }
                        KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('\u{0444}') | KeyCode::Char('\u{0424}') => {
                            if let Some(req) = gate.pending() {
                                // Narrow rules for shell (canon: `npm test` never
                                // covers `npm publish`); tool-wide otherwise.
                                let rules = perm.state().allow_always(&req);
                                if rules.is_empty() {
                                    notice!("[Allowed once: this command cannot be saved as a narrow rule]");
                                } else {
                                    notice!(&format!("[Always allowed this session: {}]", rules.join(", ")));
                                }
                            }
                            gate.respond(Decision::Allow);
                            confirm_select = ConfirmSelect::new();
                        }
                        KeyCode::Enter => {
                            gate.respond(confirm_select.decision());
                            confirm_select = ConfirmSelect::new();
                        }
                        KeyCode::Left => {
                            confirm_select.left();
                        }
                        KeyCode::Right => {
                            confirm_select.right();
                        }
                        KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
                            confirm_select.toggle();
                        }
                        _ => {}
                    }
                    renderer.request_reprint();
                    continue;
                }

                // --- Chat History Scrolling & Navigation ---
                if matches!(code, KeyCode::PageUp) {
                    let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
                    let total_chat_lines = chat.render_split(width as usize, ReasoningExpansion { all: all_expanded, last: last_expanded }).0.len() + 15;
                    let max_scroll = total_chat_lines.saturating_sub(height as usize);
                    renderer.scroll_up((height as usize / 2).max(5), max_scroll);
                    continue;
                }
                if matches!(code, KeyCode::PageDown) {
                    let (_, height) = crossterm::terminal::size().unwrap_or((100, 24));
                    renderer.scroll_down((height as usize / 2).max(5));
                    continue;
                }
                if matches!(code, KeyCode::Home) && renderer.scroll_offset > 0 {
                    let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
                    let total_chat_lines = chat.render_split(width as usize, ReasoningExpansion { all: all_expanded, last: last_expanded }).0.len() + 15;
                    let max_scroll = total_chat_lines.saturating_sub(height as usize);
                    renderer.scroll_up(max_scroll, max_scroll);
                    continue;
                }
                if matches!(code, KeyCode::End) && renderer.scroll_offset > 0 {
                    renderer.scroll_to_bottom();
                    continue;
                }
                if matches!(code, KeyCode::Esc) && renderer.scroll_offset > 0 {
                    renderer.scroll_to_bottom();
                    continue;
                }
                // Shift+Up, Ctrl+Up, Alt+Up -> scroll chat up.
                // If already scrolled up (scroll_offset > 0), plain Up also scrolls chat up!
                if (matches!(code, KeyCode::Up) && (mods.contains(KeyModifiers::SHIFT) || mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT)))
                    || (renderer.scroll_offset > 0 && matches!(code, KeyCode::Up))
                {
                    let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
                    let total_chat_lines = chat.render_split(width as usize, ReasoningExpansion { all: all_expanded, last: last_expanded }).0.len() + 15;
                    let max_scroll = total_chat_lines.saturating_sub(height as usize);
                    renderer.scroll_up(2, max_scroll);
                    continue;
                }
                // Shift+Down, Ctrl+Down, Alt+Down -> scroll chat down.
                // If already scrolled up (scroll_offset > 0), plain Down also scrolls chat down!
                if (matches!(code, KeyCode::Down) && (mods.contains(KeyModifiers::SHIFT) || mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT)))
                    || (renderer.scroll_offset > 0 && matches!(code, KeyCode::Down))
                {
                    renderer.scroll_down(2);
                    continue;
                }

                if renderer.scroll_offset > 0 && matches!(code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter) {
                    renderer.scroll_to_bottom();
                }

                match code {
                    KeyCode::Esc => {
                        if running {
                            // Cooperative: the loop answers pending tool calls,
                            // keeps partial text and returns its history via
                            // Finished, so model memory matches the screen.
                            if cancel_requested.is_none() {
                                cancel.store(true, Ordering::Relaxed);
                                cancel_requested = Some(std::time::Instant::now());
                                active_steer_tx = None;
                                suggested_prompt = None;
                                custom_placeholder = Some("Interrupting...".to_string());
                            }
                            renderer.request_reprint();
                        } else if !input.is_empty() {
                            input.clear();
                            autocomplete_idx = 0;
                            history_index = None;
                            if latest_suggestion.is_some() {
                                suggested_prompt = latest_suggestion.clone();
                            }
                            renderer.request_reprint();
                        } else if mcp_modal.is_some() {
                            mcp_modal = None;
                            renderer.request_reprint();
                        } else if suggested_prompt.is_some() || custom_placeholder.is_some() {
                            suggested_prompt = None;
                            latest_suggestion = None;
                            custom_placeholder = None;
                            renderer.request_reprint();
                        } else {
                            break 'main_loop;
                        }
                    }
                    // F1: toggle context modal
                    KeyCode::F(1) => {
                        if context_modal.is_some() {
                            context_modal = None;
                        } else {
                            effort_menu = None;
                            model_menu = None;
                            settings_view = None;
                            sampling_view = None;
                            mcp_modal = None;
                            context_modal = Some(ContextModal::new(context_usage.clone()));
                        }
                        renderer.request_reprint();
                    }

                    // Ctrl+C / Ctrl+Shift+C (handles both Latin and alternate physical keycodes)
                    KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        if running {
                            // Cooperative: the loop answers pending tool calls,
                            // keeps partial text and returns its history via
                            // Finished, so model memory matches the screen.
                            if cancel_requested.is_none() {
                                cancel.store(true, Ordering::Relaxed);
                                cancel_requested = Some(std::time::Instant::now());
                                active_steer_tx = None;
                                suggested_prompt = None;
                                custom_placeholder = Some("Interrupting...".to_string());
                            }
                            renderer.request_reprint();
                        } else {
                            let now = std::time::Instant::now();
                            let is_double_tap = last_ctrl_c.map(|t| now.duration_since(t).as_millis() < 1200).unwrap_or(false);
                            last_ctrl_c = Some(now);

                            if !input.is_empty() {
                                flashagent_tui::clipboard::set_clipboard_text(&input);
                                copy_toast = Some(("Copied input to clipboard".to_string(), now));
                                renderer.request_reprint();
                            } else if let Some(text) = chat.last_assistant_text() {
                                if is_double_tap {
                                    break 'main_loop;
                                }
                                flashagent_tui::clipboard::set_clipboard_text(&text);
                                copy_toast = Some(("Copied assistant response (press Ctrl+C again to exit)".to_string(), now));
                                renderer.request_reprint();
                            } else {
                                break 'main_loop;
                            }
                        }
                    }

                    // Ctrl+V / Ctrl+Shift+V: paste from clipboard (supports alternative keyboard layouts)
                    KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('\u{043c}') | KeyCode::Char('\u{041c}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        if let Some(text) = flashagent_tui::clipboard::get_clipboard_text() {
                            let sanitized = text.replace("\r\n", " ").replace(['\n', '\r'], " ");
                            if !sanitized.is_empty() {
                                input.push_str(&sanitized);
                                history_index = None;
                                autocomplete_idx = 0;
                                renderer.request_reprint();
                            }
                        }
                    }

                    // Ctrl+D: exit on empty input when idle
                    KeyCode::Char('d') | KeyCode::Char('D')
                        if mods.contains(KeyModifiers::CONTROL) && input.is_empty() && !running =>
                    {
                        break 'main_loop;
                    }

                    // Ctrl+R / Ctrl+Shift+R: regenerate last response from scratch (supports alternative keyboard layouts)
                    KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Char('\u{043a}') | KeyCode::Char('\u{041a}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        if !running
                            && gate.pending().is_none()
                            && question_gate.pending().is_none()
                            && effort_menu.is_none()
                            && model_menu.is_none()
                            && settings_view.is_none()
                            && sampling_view.is_none()
                            && context_modal.is_none()
                        {
                            if let Some(user_idx) = history.iter().rposition(|m| m.role == flashagent_llm::Role::User) {
                                history.truncate(user_idx + 1);
                                chat.truncate_to_last_user();
                                renderer.scroll_to_bottom();
                                renderer.printed_settled = 0;
                                renderer.prev_expansion = None;
                                renderer.request_reprint();
                                update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                                cancel.store(false, Ordering::Relaxed);
                                suggested_prompt = None;
                                custom_placeholder = None;
                                last_expanded = false;
                                running = true;
                                turn_started = Some(std::time::Instant::now());
                                token_tracker.on_turn_start(current_model.clone(), context_usage.total_used());
                                source.set_model(&current_model);
                                let turn_opts = build_turn_options(&app_config, &current_effort);
                                turn_counter += 1;
                                let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                                active_steer_tx = Some(steer_tx);
                                active_turn_handle = Some(spawn_turn(
                                    cancel.clone(),
                                    source.clone(),
                                    perm,
                                    history.clone(),
                                    GoalBudgets::steps_only(max_steps),
                                    turn_opts,
                                    tx.clone(),
                                    steer_rx,
                                    turn_counter,
                                ));
                            } else {
                                let now = std::time::Instant::now();
                                copy_toast = Some(("No previous turn to regenerate".to_string(), now));
                                renderer.request_reprint();
                            }
                        }
                    }

                    // F2: cycle reasoning expansion mode (none -> last -> all -> none)
                    KeyCode::F(2) => {
                        if !last_expanded && !all_expanded {
                            last_expanded = true;
                            all_expanded = false;
                        } else if last_expanded && !all_expanded {
                            last_expanded = false;
                            all_expanded = true;
                        } else {
                            last_expanded = false;
                            all_expanded = false;
                        }
                        renderer.request_reprint();
                    }

                    // ALT + O: expand / collapse ALL thinking blocks permanently (supports alternative keyboard layouts)
                    KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Char('\u{0449}') | KeyCode::Char('\u{0429}')
                        if mods.contains(KeyModifiers::ALT) =>
                    {
                        all_expanded = !all_expanded;
                        if all_expanded {
                            last_expanded = false;
                        }
                        renderer.request_reprint();
                    }

                    // CTRL + O: expand / collapse LAST thinking block temporarily (supports alternative keyboard layouts)
                    KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Char('\u{0449}') | KeyCode::Char('\u{0429}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        last_expanded = !last_expanded;
                        if last_expanded {
                            all_expanded = false;
                        }
                        renderer.request_reprint();
                    }

                    // CTRL + E: launch external editor on current input buffer
                    KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::Char('\u{0443}') | KeyCode::Char('\u{0423}')
                        if mods.contains(KeyModifiers::CONTROL) && !running =>
                    {
                        match open_in_external_editor(&input, &app_config.external_editor) {
                            Ok(edited) => {
                                input = edited;
                                autocomplete_idx = 0;
                            }
                            Err(err) => {
                                notice!(&format!("Failed to launch external editor: {err}"));
                            }
                        }
                        renderer.request_reprint();
                    }

                    // CTRL + U: download & apply pending update or check for updates
                    KeyCode::Char('u') | KeyCode::Char('U') | KeyCode::Char('\u{0433}') | KeyCode::Char('\u{0413}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        if let Some((target_ver, asset_name, download_url, checksums_url)) = pending_update.clone() {
                            background = Some(BackgroundNotice::sticky(format!(
                                "{UPDATE_LINE_PREFIX}{target_ver} \u{b7} starting download..."
                            )));
                            renderer.request_reprint();
                            let update_tx_clone = update_tx.clone();
                            tokio::spawn(async move {
                                let progress_tx = update_tx_clone.clone();
                                let ver_for_progress = target_ver.clone();
                                let result = flashagent_svc::updater::download_and_apply_with_progress(
                                    &download_url,
                                    &asset_name,
                                    checksums_url.as_deref(),
                                    move |stage| {
                                        let _ = progress_tx.send(UpdateNotice::Progress {
                                            version: ver_for_progress.clone(),
                                            stage,
                                        });
                                    },
                                )
                                .await;
                                let notice = match result {
                                    Ok(_) => UpdateNotice::Ready { version: target_ver },
                                    Err(e) => UpdateNotice::Failed { error: e.to_string() },
                                };
                                let _ = update_tx_clone.send(notice);
                            });
                        } else if !flashagent_svc::updater::is_dev_mode() {
                            background = Some(BackgroundNotice::fading(
                                format!(
                                    "{UPDATE_LINE_PREFIX}\u{b7} checking the {} channel...",
                                    app_config.update_channel.label()
                                ),
                                30,
                            ));
                            renderer.request_reprint();
                            let update_tx_clone = update_tx.clone();
                            let ch = app_config.update_channel;
                            tokio::spawn(async move {
                                match flashagent_svc::updater::check_for_updates(ch, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                                    // Asked for by hand: go straight on to the
                                    // download instead of making the user press
                                    // Ctrl+U a second time.
                                    Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                        let progress_tx = update_tx_clone.clone();
                                        let ver_for_progress = target.clone();
                                        let result = flashagent_svc::updater::download_and_apply_with_progress(
                                            &download_url,
                                            &asset_name,
                                            checksums_url.as_deref(),
                                            move |stage| {
                                                let _ = progress_tx.send(UpdateNotice::Progress {
                                                    version: ver_for_progress.clone(),
                                                    stage,
                                                });
                                            },
                                        )
                                        .await;
                                        let _ = update_tx_clone.send(match result {
                                            Ok(_) => UpdateNotice::Ready { version: target },
                                            Err(e) => UpdateNotice::Failed { error: e.to_string() },
                                        });
                                    }
                                    Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, .. }) => {
                                        let _ = update_tx_clone.send(UpdateNotice::UpToDate {
                                            version: current,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = update_tx_clone.send(UpdateNotice::Failed { error: e.to_string() });
                                    }
                                }
                            });
                        } else {
                            background = Some(BackgroundNotice::fading("Auto-updater is disabled in dev mode", 6));
                            renderer.request_reprint();
                        }
                    }

                    // F4 or CTRL + T or ALT + T: open Thinking Effort menu (supports alternative keyboard layouts)
                    KeyCode::F(4)
                    | KeyCode::Char('t') | KeyCode::Char('T') | KeyCode::Char('\u{0435}') | KeyCode::Char('\u{0415}')
                        if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) || matches!(code, KeyCode::F(4)) =>
                    {
                        let mut menu = build_effort_menu(&source);
                        menu.select_by_value(&current_effort);
                        effort_menu = Some(menu);
                    }

                    // F3 or CTRL + M or ALT + M: open Model menu (supports alternative keyboard layouts)
                    KeyCode::F(3)
                    | KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Char('\u{044c}') | KeyCode::Char('\u{042c}')
                        if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) || matches!(code, KeyCode::F(3)) =>
                    {
                        if let Some(mut menu) = build_model_menu(&source) {
                            menu.select_by_value(&current_model);
                            model_menu = Some(menu);
                        } else {
                            notice!("[No models discovered from server]");
                        }
                    }

                    // F5: open Sampling Parameters menu
                    KeyCode::F(5) => {
                        if sampling_view.is_some() {
                            sampling_view = None;
                        } else {
                            effort_menu = None;
                            model_menu = None;
                            settings_view = None;
                            context_modal = None;
                            mcp_modal = None;
                            sampling_view = Some(SamplingView::new(&app_config));
                        }
                        renderer.request_reprint();
                    }

                    // Mode cycling with Shift+Tab (both KeyCode::BackTab and Tab+Shift)
                    KeyCode::BackTab | KeyCode::Tab if matches!(code, KeyCode::BackTab) || mods.contains(KeyModifiers::SHIFT) => {
                        let next_mode = perm.state().mode().next();
                        perm.state().set_mode(next_mode);
                        refresh_welcome_card_if_before_user_msg(
                            &mut chat,
                            &mut renderer,
                            &current_model,
                            &cwd_display,
                            next_mode.label(),
                            memory_docs,
                            &source,
                            &current_effort,
                            current_context.as_deref(),
                            app_config.show_mascot,
                            mascot_mood,
                        );
                        custom_placeholder = Some(format!("Permission mode set to: {}", next_mode.label()));
                        suggested_prompt = None;
                        renderer.request_reprint();
                    }
                    // Tab on empty input: toggle settings tab
                    KeyCode::Tab if input.is_empty() && gate.pending().is_none() && effort_menu.is_none() && model_menu.is_none() && sampling_view.is_none() && mcp_modal.is_none() => {
                        if settings_view.is_some() {
                            settings_view = None;
                        } else {
                            mcp_modal = None;
                            let runtime_mode = goal_state.as_ref().map_or(perm.state().mode(), |g| g.mode);
                            let effort = goal_state.as_ref().map_or(current_effort.as_str(), |g| g.effort.as_str());
                            settings_view = Some(settings_for_runtime(&app_config, runtime_mode, effort, &current_model, &available_models));
                        }
                        renderer.request_reprint();
                    }
                    // Tab: complete autocomplete suggestion if input starts with `/`, or toggle approval choice when pending
                    KeyCode::Tab if gate.pending().is_some() => {
                        confirm_select.toggle();
                    }
                    KeyCode::Tab if input.starts_with('/') => {
                        if let Some(ac) = AutocompletePopup::for_input(&input, std::path::Path::new("."), autocomplete_idx) {
                            input = ac.complete_input(&input);
                            autocomplete_idx = 0;
                        }
                    }
                    // Arrow navigation for approval card, autocomplete popup, and prompt history
                    KeyCode::Left => {
                        if gate.pending().is_some() {
                            confirm_select.left();
                        }
                    }
                    KeyCode::Right => {
                        if gate.pending().is_some() {
                            confirm_select.right();
                        } else if !running && input.is_empty() {
                            if let Some(sug) = suggested_prompt.take() {
                                latest_suggestion = None;
                                input = sug;
                                renderer.request_reprint();
                            }
                        }
                    }
                    KeyCode::Up => {
                        if gate.pending().is_some() {
                            confirm_select.toggle();
                        } else if !running && input.starts_with('/') {
                            if let Some(ac) = AutocompletePopup::for_input(&input, std::path::Path::new("."), autocomplete_idx) {
                                if autocomplete_idx == 0 {
                                     autocomplete_idx = ac.items.len().saturating_sub(1);
                                } else {
                                     autocomplete_idx -= 1;
                                }
                            }
                        } else if !running && !input_history.is_empty() {
                            match history_index {
                                None => {
                                    current_draft = input.clone();
                                    let idx = input_history.len() - 1;
                                    history_index = Some(idx);
                                    input = input_history[idx].clone();
                                }
                                Some(idx) if idx > 0 => {
                                    let new_idx = idx - 1;
                                    history_index = Some(new_idx);
                                    input = input_history[new_idx].clone();
                                }
                                _ => {}
                            }
                        }
                    }
                    KeyCode::Down => {
                        if gate.pending().is_some() {
                            confirm_select.toggle();
                        } else if !running && input.starts_with('/') {
                            if let Some(ac) = AutocompletePopup::for_input(&input, std::path::Path::new("."), autocomplete_idx) {
                                autocomplete_idx = (autocomplete_idx + 1) % ac.items.len();
                            }
                        } else if !running {
                            if let Some(idx) = history_index {
                                if idx + 1 < input_history.len() {
                                    let new_idx = idx + 1;
                                    history_index = Some(new_idx);
                                    input = input_history[new_idx].clone();
                                } else {
                                    history_index = None;
                                    input = std::mem::take(&mut current_draft);
                                }
                            }
                        }
                    }
                    KeyCode::Backspace if gate.pending().is_none() => {
                        input.pop();
                        history_index = None;
                        autocomplete_idx = 0;
                        if input.is_empty() && latest_suggestion.is_some() {
                            suggested_prompt = latest_suggestion.clone();
                        }
                    }
                    KeyCode::Enter => {
                        custom_placeholder = None;
                        suggested_prompt = None;
                        latest_suggestion = None;
                        if gate.pending().is_some() {
                            gate.respond(confirm_select.decision());
                            confirm_select = ConfirmSelect::new();
                        } else if !input.is_empty() && running {
                            if let Some(ref steer_tx) = active_steer_tx {
                                let text = std::mem::take(&mut input);
                                if input_history.last() != Some(&text) {
                                    input_history.push(text.clone());
                                }
                                history_index = None;
                                current_draft.clear();
                                let _ = steer_tx.send(text);
                                renderer.request_reprint();
                            }
                        } else if !input.is_empty() && !running {
                            let trimmed = input.trim();
                            if trimmed == "/help" || trimmed == "/?" {
                                input.clear();
                                autocomplete_idx = 0;
                                chat.push_system(
                                    "Commands & Skills (Tab to autocomplete):\n\
                                     • /goal [--steps N] [--time 30m] [--tokens 200k] <task> — autonomous run under a budget\n\
                                     • /settings (or Tab)     — open settings configuration tab\n\
                                     • /context               — show detailed context window breakdown\n\
                                     • /verbose [all|last|off] — toggle verbose mode (or press F2 / Alt+O / Ctrl+O)\n\
                                     • /effort (or /t)        — choose thinking effort preset (or press F4 / Ctrl+T)\n\
                                     • /model  (or /m)        — choose and switch model (or press F3)\n\
                                     • /mode [plan|man|edits|all] — switch permission mode (or press Shift+Tab)\n\
                                     • /mcp [list|market|test|add|reload] — manage Model Context Protocol servers\n\
                                     • /sampling              — sampling parameters (or press F5)\n\
                                     • /compact [focus]       — summarize older turns to free context\n\
                                     • /clear                 — clear chat scrollback\n\
                                     • /regenerate (or Ctrl+R) — regenerate last model response from scratch\n\
                                     • /diff · /commit <msg>  — git diff --stat / commit staged changes\n\
                                     • /export [md|html|jsonl] — write the conversation to a file\n\
                                     • /editor (or Ctrl+E)    — compose the prompt in an external editor\n\
                                     • /update (or Ctrl+U) · /channel <stable|beta> — check, download and install an update, with progress\n\
                                     • /skill:<name>          — invoke a skill from .agents/skills/\n\
                                     • /exit                  — save the session and quit\n\
                                     • Tab                    — autocomplete popup or settings tab\n\
                                     • Esc                    — dismiss suggestions / interrupt; quits on an empty prompt"
                                );
                                continue;
                            }

                            if trimmed == "/goal" {
                                input.clear();
                                autocomplete_idx = 0;
                                chat.push_system(&format!(
                                    "Autonomous Goal Mode:\n\
                                     Usage: /goal [--steps N] [--time 30m] [--tokens 200k] <task description>\n\
                                     Example: /goal --time 20m Refactor error handling in core crate and run test suite\n\
                                     Budgets default to {} and stop the run when reached; --tokens counts generated tokens only.\n\
                                     In goal mode, FlashAgent lifts all permission gates, maximizes reasoning, and executes autonomously without human interruption until a budget or the task ends it.",
                                    GoalBudgets::default().summary()
                                ));
                                continue;
                            } else if let Some(task) = trimmed.strip_prefix("/goal ") {
                                let task = task.to_string();
                                input.clear();
                                autocomplete_idx = 0;
                                let (goal_budgets, task_desc) =
                                    match flashagent_tui::goal::parse_goal_command(&task) {
                                        Ok(parsed) => parsed,
                                        Err(msg) => {
                                            notice!(&msg);
                                            renderer.request_reprint();
                                            continue;
                                        }
                                    };

                                // Save previous state to roll back upon goal completion
                                goal_state = Some(SavedGoalState {
                                    mode: perm.state().mode(),
                                    effort: current_effort.clone(),
                                    max_steps,
                                    task: task_desc.clone(),
                                });
                                goal_ledger =
                                    Some(GoalLedger::new(task_desc.clone(), goal_budgets.clone()));

                                // Lift restrictions for autonomous execution
                                perm.state().set_mode(PermissionMode::Bypass);
                                current_effort = "high".to_string();
                                tools_arc.set_goal_mode(true);

                                let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                                let card_w = (w as usize).saturating_sub(4).clamp(44, 110);
                                let inner_w = card_w.saturating_sub(2);
                                let border_color = "\x1b[38;2;225;175;95m";
                                let reset = "\x1b[0m";

                                let title = format!(" Goal: {} ", flashagent_tui::truncate_middle(&task_desc, inner_w.saturating_sub(10)));
                                let dash_count = inner_w.saturating_sub(title.chars().count() + 1);
                                chat.push_line(LineKind::System, format!("{border_color}╭─\x1b[1;38;2;245;240;232m{title}{border_color}{}╮{reset}", "─".repeat(dash_count)));
                                let meta_line = "Mode: Autonomous · Permissions: Auto-Approved · Thinking: Max · Esc to stop";
                                let pad_len = inner_w.saturating_sub(meta_line.chars().count() + 1);
                                chat.push_line(LineKind::System, format!("{border_color}│{reset} \x1b[38;2;160;155;145m{meta_line}\x1b[0m{border_color}{}│{reset}", " ".repeat(pad_len)));
                                let budget_line = format!("Budget: {}", goal_budgets.summary());
                                let budget_line = flashagent_tui::truncate_middle(&budget_line, inner_w.saturating_sub(2));
                                let pad_len = inner_w.saturating_sub(budget_line.chars().count() + 1);
                                chat.push_line(LineKind::System, format!("{border_color}│{reset} \x1b[38;2;160;155;145m{budget_line}\x1b[0m{border_color}{}│{reset}", " ".repeat(pad_len)));
                                chat.push_line(LineKind::System, format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)));

                                chat.push_user(&format!("/goal {task_desc}"));

                                let autonomous_directive = format!(
                                    "[AUTONOMOUS GOAL DIRECTIVE]\n\
                                     You are operating in fully autonomous /goal mode.\n\
                                     Target goal: {}\n\n\
                                     Autonomous Rules:\n\
                                     1. Do NOT ask clarifying questions or seek user confirmation. All tool actions are pre-approved.\n\
                                     2. Plan, research, edit, execute, and verify completely on your own.\n\
                                     3. Thoroughly test and verify your changes before finishing.\n\
                                     4. Conclude with a clear structured summary of what was accomplished.\n\n\
                                     Budget for this run: {}. When it runs out the run is stopped wherever it \
                                     is, so do the load-bearing work first and say plainly what is left \
                                     unfinished or unverified rather than claiming success.",
                                    task_desc,
                                    goal_budgets.summary()
                                );

                                let first = !history.iter().any(|m| m.role == flashagent_llm::Role::User);
                                let content = if first && !memory_block.is_empty() {
                                    format!("{memory_block}\n\n---\n\n{autonomous_directive}")
                                } else {
                                    autonomous_directive
                                };
                                history.push(ChatMessage::user(content));
                                update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                                cancel.store(false, Ordering::Relaxed);
                                suggested_prompt = None;
                                custom_placeholder = None;
                                running = true;
                                turn_started = Some(std::time::Instant::now());
                                token_tracker.on_turn_start(current_model.clone(), context_usage.total_used());
                                source.set_model(&current_model);
                                let turn_opts = build_turn_options(&app_config, &current_effort);
                                turn_counter += 1;
                                let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                                active_steer_tx = Some(steer_tx);
                                active_turn_handle = Some(spawn_turn(
                                    cancel.clone(),
                                    source.clone(),
                                    perm,
                                    history.clone(),
                                    goal_budgets,
                                    turn_opts,
                                    tx.clone(),
                                    steer_rx,
                                    turn_counter,
                                ));
                                continue;
                            }

                            if trimmed == "/skills" {
                                input.clear();
                                autocomplete_idx = 0;
                                let skills = flashagent_tui::autocomplete::load_skills(std::path::Path::new("."));
                                let listed: Vec<String> = skills
                                    .iter()
                                    .filter(|s| s.trigger.starts_with("/skill:"))
                                    .map(|s| format!("  • {} — {}", s.trigger, s.description))
                                    .collect();
                                if listed.is_empty() {
                                    notice!("[No skills found. Add .agents/skills/<name>.md (project) or ~/.flashagent/skills/<name>.md (global).]");
                                } else {
                                    chat.push_system(&format!("Skills:\n{}", listed.join("\n")));
                                }
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/settings" || trimmed == "/config" {
                                input.clear();
                                autocomplete_idx = 0;
                                let runtime_mode = goal_state.as_ref().map_or(perm.state().mode(), |g| g.mode);
                                let effort = goal_state.as_ref().map_or(current_effort.as_str(), |g| g.effort.as_str());
                                settings_view = Some(settings_for_runtime(&app_config, runtime_mode, effort, &current_model, &available_models));
                                continue;
                            }

                            if trimmed == "/context" {
                                input.clear();
                                autocomplete_idx = 0;
                                update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                                context_modal = Some(ContextModal::new(context_usage.clone()));
                                continue;
                            }

                            if trimmed == "/clear" {
                                input.clear();
                                autocomplete_idx = 0;
                                chat.clear();
                                renderer.printed_settled = 0;
                                renderer.prev_expansion = None;
                                continue;
                            }

                            if trimmed == "/regenerate" || trimmed == "/retry" {
                                input.clear();
                                autocomplete_idx = 0;
                                if let Some(user_idx) = history.iter().rposition(|m| m.role == flashagent_llm::Role::User) {
                                    history.truncate(user_idx + 1);
                                    chat.truncate_to_last_user();
                                    renderer.scroll_to_bottom();
                                    renderer.printed_settled = 0;
                                    renderer.prev_expansion = None;
                                    renderer.request_reprint();
                                    update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                                    cancel.store(false, Ordering::Relaxed);
                                    suggested_prompt = None;
                                    custom_placeholder = None;
                                    last_expanded = false;
                                    running = true;
                                    turn_started = Some(std::time::Instant::now());
                                    token_tracker.on_turn_start(current_model.clone(), context_usage.total_used());
                                    source.set_model(&current_model);
                                    let turn_opts = build_turn_options(&app_config, &current_effort);
                                    turn_counter += 1;
                                    let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                                    active_steer_tx = Some(steer_tx);
                                    active_turn_handle = Some(spawn_turn(
                                        cancel.clone(),
                                        source.clone(),
                                        perm,
                                        history.clone(),
                                        GoalBudgets::steps_only(max_steps),
                                        turn_opts,
                                        tx.clone(),
                                        steer_rx,
                                        turn_counter,
                                    ));
                                } else {
                                    notice!("[No previous turn to regenerate]");
                                    renderer.request_reprint();
                                }
                                continue;
                            }

                            if trimmed == "/effort" || trimmed == "/thinking" || trimmed == "/t" {
                                input.clear();
                                autocomplete_idx = 0;
                                let mut menu = build_effort_menu(&source);
                                menu.select_by_value(&current_effort);
                                effort_menu = Some(menu);
                                continue;
                            } else if let Some(arg) = trimmed.strip_prefix("/effort ") {
                                let arg_val = arg.trim().to_lowercase();
                                input.clear();
                                autocomplete_idx = 0;
                                let mut known: Vec<String> = ["auto", "default", "off", "low", "medium", "high"].iter().map(|s| s.to_string()).collect();
                                if let Some(p) = source.profile() {
                                    known.extend(p.presets.iter().cloned());
                                }
                                if !known.contains(&arg_val) {
                                    known.dedup();
                                    notice!(&format!("[Unknown effort '{arg_val}'. Available: {}]", known.join(", ")));
                                    renderer.request_reprint();
                                    continue;
                                }
                                current_effort = arg_val.clone();
                                refresh_welcome_card_if_before_user_msg(
                                    &mut chat,
                                    &mut renderer,
                                    &current_model,
                                    &cwd_display,
                                    perm.state().mode().label(),
                                    memory_docs,
                                    &source,
                                    &current_effort,
                                    current_context.as_deref(),
                                    app_config.show_mascot,
                                    mascot_mood,
                                );
                                custom_placeholder = Some(format!("Thinking effort set to: {arg_val}"));
                                suggested_prompt = None;
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/model" || trimmed == "/models" || trimmed == "/m" {
                                input.clear();
                                autocomplete_idx = 0;
                                if let Some(mut menu) = build_model_menu(&source) {
                                    menu.select_by_value(&current_model);
                                    model_menu = Some(menu);
                                } else {
                                    notice!("[No models discovered from server]");
                                }
                                continue;
                            }

                            if trimmed == "/sampling" || trimmed == "/params" {
                                input.clear();
                                autocomplete_idx = 0;
                                sampling_view = Some(SamplingView::new(&app_config));
                                continue;
                            }

                            if trimmed == "/mode" {
                                input.clear();
                                autocomplete_idx = 0;
                                let next_mode = perm.state().mode().next();
                                perm.state().set_mode(next_mode);
                                refresh_welcome_card_if_before_user_msg(
                                    &mut chat,
                                    &mut renderer,
                                    &current_model,
                                    &cwd_display,
                                    next_mode.label(),
                                    memory_docs,
                                    &source,
                                    &current_effort,
                                    current_context.as_deref(),
                                    app_config.show_mascot,
                                    mascot_mood,
                                );
                                custom_placeholder = Some(format!("Permission mode set to: {}", next_mode.label()));
                                suggested_prompt = None;
                                renderer.request_reprint();
                                continue;
                            } else if let Some(arg) = trimmed.strip_prefix("/mode ") {
                                let arg_val = arg.trim().to_lowercase();
                                input.clear();
                                autocomplete_idx = 0;
                                let m = match arg_val.as_str() {
                                    "planning" | "plan" => Some(PermissionMode::Planning),
                                    "manual" | "man" => Some(PermissionMode::Manual),
                                    "acceptedits" | "accept_edits" | "edits" | "auto" | "default" => Some(PermissionMode::AcceptEdits),
                                    "bypass" | "accept_all" | "all" => Some(PermissionMode::Bypass),
                                    "autonomic" => {
                                        notice!("[Autonomic mode is temporary and activated exclusively during `/goal <task>` execution]");
                                        None
                                    }
                                    _ => {
                                        notice!("[Usage: /mode planning | /mode manual | /mode edits | /mode all]");
                                        None
                                    }
                                };
                                if let Some(mode) = m {
                                    perm.state().set_mode(mode);
                                    refresh_welcome_card_if_before_user_msg(
                                        &mut chat,
                                        &mut renderer,
                                        &current_model,
                                        &cwd_display,
                                        mode.label(),
                                        memory_docs,
                                        &source,
                                        &current_effort,
                                        current_context.as_deref(),
                                        app_config.show_mascot,
                                        mascot_mood,
                                    );
                                    custom_placeholder = Some(format!("Permission mode set to: {}", mode.label()));
                                    suggested_prompt = None;
                                }
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/update" {
                                input.clear();
                                autocomplete_idx = 0;
                                if flashagent_svc::updater::is_dev_mode() {
                                    chat.push_system("  \x1b[38;2;225;175;95mAuto-updater is disabled in dev mode\x1b[0m (running from source repository / cargo build).\n  To update your dev build, pull latest git commits and run `cargo build --release`.");
                                    continue;
                                }
                                let channel = app_config.update_channel;
                                notice!(&format!("Checking for updates on {} channel...", channel.label()));
                                let update_tx_clone = update_tx.clone();
                                tokio::spawn(async move {
                                    match flashagent_svc::updater::check_for_updates(channel, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                                        Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                            let _ = update_tx_clone.send(UpdateNotice::Available {
                                                version: target,
                                                asset_name,
                                                download_url,
                                                checksums_url,
                                            });
                                        }
                                        Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, .. }) => {
                                            let _ = update_tx_clone.send(UpdateNotice::UpToDate {
                                                version: current,
                                            });
                                        }
                                        Err(e) => {
                                            let _ = update_tx_clone.send(UpdateNotice::Failed { error: e.to_string() });
                                        }
                                    }
                                });
                                continue;
                            }

                            if trimmed == "/channel" {
                                input.clear();
                                autocomplete_idx = 0;
                                chat.push_system(&format!(
                                    "Current release channel: \x1b[1m{}\x1b[0m\nUsage: /channel <stable|beta>\n• /channel stable — Official stable releases (v*)\n• /channel beta   — Latest beta pre-releases (b*)",
                                    app_config.update_channel.label()
                                ));
                                continue;
                            } else if let Some(arg) = trimmed.strip_prefix("/channel ") {
                                let choice = arg.trim().to_lowercase();
                                input.clear();
                                autocomplete_idx = 0;
                                let target_ch = match choice.as_str() {
                                    "stable" | "v" => Some(flashagent_core::config::UpdateChannel::Stable),
                                    "beta" | "b" => Some(flashagent_core::config::UpdateChannel::Beta),
                                    _ => None,
                                };
                                if let Some(ch) = target_ch {
                                    app_config.update_channel = ch;
                                    let _ = app_config.save();
                                    notice!(&format!(
                                        "Switched to \x1b[1m{}\x1b[0m channel. Checking for releases in background...",
                                        ch.label()
                                    ));
                                    if !flashagent_svc::updater::is_dev_mode() {
                                        if channel_watch_tx.receiver_count() > 0 {
                                            let _ = channel_watch_tx.send(ch);
                                        } else {
                                            let update_tx_clone = update_tx.clone();
                                            tokio::spawn(async move {
                                                if let Ok(Some(target_ver)) = flashagent_svc::updater::check_and_apply_background(ch).await {
                                                    let _ = update_tx_clone.send(UpdateNotice::Ready { version: target_ver });
                                                }
                                            });
                                        }
                                    }
                                } else {
                                    notice!("Invalid channel. Choose either: /channel stable or /channel beta");
                                }
                                continue;
                            }

                            if trimmed == "/mcp" || trimmed == "/mcp help" {
                                input.clear();
                                autocomplete_idx = 0;
                                let mgr = tools_arc.mcp_manager();
                                let paths = mgr.loaded_paths();
                                let statuses = mgr.server_status_list().await;
                                effort_menu = None;
                                model_menu = None;
                                settings_view = None;
                                sampling_view = None;
                                context_modal = None;
                                mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Overview));
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/mcp list" || trimmed == "/mcp ls" {
                                input.clear();
                                autocomplete_idx = 0;
                                let mgr = tools_arc.mcp_manager();
                                let paths = mgr.loaded_paths();
                                let statuses = mgr.server_status_list().await;
                                effort_menu = None;
                                model_menu = None;
                                settings_view = None;
                                sampling_view = None;
                                context_modal = None;
                                mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Servers));
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/mcp market" || trimmed == "/mcp marketplace" {
                                input.clear();
                                autocomplete_idx = 0;
                                let mgr = tools_arc.mcp_manager();
                                let paths = mgr.loaded_paths();
                                let statuses = mgr.server_status_list().await;
                                effort_menu = None;
                                model_menu = None;
                                settings_view = None;
                                sampling_view = None;
                                context_modal = None;
                                mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Marketplace));
                                renderer.request_reprint();
                                continue;
                            }

                            if let Some(target) = trimmed.strip_prefix("/mcp test ") {
                                let server_name = target.trim().to_string();
                                input.clear();
                                autocomplete_idx = 0;
                                notice!(&format!("Testing MCP server '{}'...", server_name));
                                let mgr = tools_arc.mcp_manager();
                                match mgr.test_server(&server_name).await {
                                    Ok(report) => {
                                        let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                                        for line in flashagent_tui::mcp_view::render_mcp_test_report(&report, w as usize) {
                                            chat.push_line(LineKind::System, line);
                                        }
                                    }
                                    Err(e) => {
                                        chat.push_system(&format!("\x1b[38;2;245;120;120mMCP server '{server_name}' test failed:\x1b[0m\n{e}"));
                                    }
                                }
                                renderer.request_reprint();
                                continue;
                            } else if trimmed == "/mcp test" {
                                input.clear();
                                autocomplete_idx = 0;
                                chat.push_system("Usage: /mcp test <server_name>\nExample: /mcp test sqlite");
                                continue;
                            }

                            if let Some(target) = trimmed.strip_prefix("/mcp add ") {
                                let id = target.trim().to_string();
                                input.clear();
                                autocomplete_idx = 0;
                                if let Some(item) = flashagent_tools::mcp::find_marketplace_item(&id) {
                                    let cfg = flashagent_tools::mcp::scaffold_config(item);
                                    match flashagent_tools::mcp::save_server_to_project(std::path::Path::new("."), item.id, cfg) {
                                        Ok(path) => {
                                            let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                                            for line in flashagent_tui::mcp_view::render_mcp_add_success(item, &path, w as usize) {
                                                chat.push_line(LineKind::System, line);
                                            }
                                            // Start in background
                                            let mgr = tools_arc.mcp_manager();
                                            let server_id = item.id.to_string();
                                            tokio::spawn(async move {
                                                let _ = mgr.reload().await;
                                                let _ = mgr.start_server(&server_id).await;
                                            });
                                        }
                                        Err(e) => {
                                            notice!(&format!("\x1b[38;2;245;120;120mFailed to save MCP configuration:\x1b[0m {e}"));
                                        }
                                    }
                                } else {
                                    notice!(&format!("Unknown marketplace extension: '{id}'. Type /mcp market to see available items."));
                                }
                                renderer.request_reprint();
                                continue;
                            } else if trimmed == "/mcp add" {
                                input.clear();
                                autocomplete_idx = 0;
                                chat.push_system("Usage: /mcp add <marketplace_id>\nExample: /mcp add sqlite\nType /mcp market to browse extensions.");
                                continue;
                            }

                            if trimmed == "/mcp reload" {
                                input.clear();
                                autocomplete_idx = 0;
                                let mgr = tools_arc.mcp_manager();
                                match mgr.reload().await {
                                    Ok(()) => {
                                        let statuses = mgr.server_status_list().await;
                                        let active = statuses.iter().filter(|s| s.state == flashagent_tools::mcp::ServerConnectionState::Active).count();
                                        let tools: usize = statuses.iter().map(|s| s.tool_count).sum();
                                        notice!(&format!(
                                            "\x1b[38;2;135;220;145m✔ MCP reload complete:\x1b[0m {} active server(s), {} discovered tool(s).",
                                            active, tools
                                        ));
                                    }
                                    Err(e) => {
                                        notice!(&format!("\x1b[38;2;245;120;120mMCP reload failed:\x1b[0m {e}"));
                                    }
                                }
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/compact" || trimmed.starts_with("/compact ") {
                                let focus = trimmed.strip_prefix("/compact").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                                input.clear();
                                autocomplete_idx = 0;
                                custom_placeholder = Some("Compacting conversation context...".to_string());
                                suggested_prompt = None;
                                let tg_speed = token_tracker.tg_3s();
                                renderer.frame(
                                    &chat,
                                    &gate,
                                    &question_gate,
                                    effort_menu.as_ref(),
                                    model_menu.as_ref(),
                                    settings_view.as_ref(),
                                    sampling_view.as_ref(),
                                    context_modal.as_ref(),
                                    mcp_modal.as_ref(),
                                    autocomplete.as_ref(),
                                    &context_usage,
                                    FrameState {
                                        input: &input,
                                        mode: perm.state().mode(),
                                        is_goal_active: goal_state.is_some(),
                                        goal_progress: None,
                                        tip: Some(tip_animator.tip_text),
                                        tip_animated: None,
                                        tip_lines: Some(&tip_lines),
                                        token_tracker: Some(&token_tracker),
                                        thinking_effort: &current_effort,
                                        reasoning_expand: ReasoningExpansion {
                                            all: all_expanded,
                                            last: last_expanded,
                                        },
                                        tick_n,
                                        running,
                                        elapsed_secs: turn_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                                        face_phase: turn_started.map(|t| (t.elapsed().as_millis() / 80) as usize).unwrap_or(0),
                                        model_tokens: token_tracker.total_model_tokens,
                                        tokens_per_sec: tg_speed,
                                        f_keep: token_tracker.last_f_keep,
                                        model: &current_model,
                                        context_window: current_context.as_deref(),
                                        cwd: &cwd_display,
                                        confirm_selection: confirm_select.decision(),
                                        question_state: Some(&question_ui_state),
                                        custom_placeholder: custom_placeholder.as_deref(),
                                        suggested_prompt: suggested_prompt.as_deref(),
                                        copy_toast: None,
                                        prefill_status: None,
                                        ttft_display: None,
                                        background: background.as_ref().map(|b| b.text.as_str()),
                                        channel_prompt: None,
                background_style: background.as_ref().map_or(NoticeStyle::FULL, BackgroundNotice::style),
                                        context_warn_threshold: app_config.context_warn_threshold,
                                    },
                                );
                                if let Some(freed) = compact_context(&source, &mut history, focus.as_deref()).await {
                                    update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                                    custom_placeholder = Some(format!("Context compacted (~{} freed)", ContextUsage::format_tokens(freed)));
                                } else {
                                    custom_placeholder = Some("Context is already compact".to_string());
                                }
                                suggested_prompt = None;
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/verbose" || trimmed == "/expand" || trimmed == "/think" || trimmed == "/o" {
                                input.clear();
                                autocomplete_idx = 0;
                                if !last_expanded && !all_expanded {
                                    last_expanded = true;
                                    all_expanded = false;
                                } else if last_expanded && !all_expanded {
                                    last_expanded = false;
                                    all_expanded = true;
                                } else {
                                    last_expanded = false;
                                    all_expanded = false;
                                }
                                renderer.request_reprint();
                                continue;
                            } else if let Some(arg) = trimmed.strip_prefix("/verbose ")
                                .or_else(|| trimmed.strip_prefix("/expand "))
                                .or_else(|| trimmed.strip_prefix("/think "))
                                .or_else(|| trimmed.strip_prefix("/o "))
                            {
                                let arg_val = arg.trim().to_string();
                                input.clear();
                                autocomplete_idx = 0;
                                match arg_val.as_str() {
                                    "all" => {
                                        all_expanded = true;
                                        last_expanded = false;
                                    }
                                    "last" => {
                                        all_expanded = false;
                                        last_expanded = true;
                                    }
                                    "off" | "none" | "collapse" => {
                                        all_expanded = false;
                                        last_expanded = false;
                                    }
                                    _ => {
                                        notice!("[Usage: /verbose all | /verbose last | /verbose off]");
                                    }
                                };
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/exit" || trimmed == "/quit" || trimmed == "/q" {
                                break 'main_loop;
                            }

                            if trimmed == "/editor" {
                                input.clear();
                                autocomplete_idx = 0;
                                match open_in_external_editor("", &app_config.external_editor) {
                                    Ok(edited) => {
                                        input = edited;
                                    }
                                    Err(err) => {
                                        notice!(&format!("Failed to launch external editor: {err}"));
                                    }
                                }
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/diff" {
                                input.clear();
                                autocomplete_idx = 0;
                                match std::process::Command::new("git").args(["diff", "--stat"]).output() {
                                    Ok(out) => {
                                        let s = String::from_utf8_lossy(&out.stdout);
                                        if s.trim().is_empty() {
                                            notice!("[Git working tree clean — no unstaged changes]");
                                        } else {
                                            chat.push_system(&format!("Git diff summary:\n{}", s.trim_end()));
                                        }
                                    }
                                    Err(e) => {
                                        notice!(&format!("Failed to run git diff: {e}"));
                                    }
                                }
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/commit" || trimmed.starts_with("/commit ") {
                                let msg_arg = trimmed.strip_prefix("/commit ").map(|s| s.trim().to_string()).unwrap_or_default();
                                input.clear();
                                autocomplete_idx = 0;
                                let commit_msg = if !msg_arg.is_empty() {
                                    msg_arg.to_string()
                                } else {
                                    let ts = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs())
                                        .unwrap_or(0);
                                    format!("chore: checkpoint update ({ts})")
                                };
                                match std::process::Command::new("git").args(["commit", "-m", &commit_msg]).output() {
                                    Ok(out) if out.status.success() => {
                                        let s = String::from_utf8_lossy(&out.stdout);
                                        chat.push_system(&format!("Git commit succeeded:\n{}", s.trim_end()));
                                    }
                                    Ok(out) => {
                                        let err = String::from_utf8_lossy(&out.stderr);
                                        let s = String::from_utf8_lossy(&out.stdout);
                                        chat.push_system(&format!("Git commit status:\n{}{}", s, err));
                                    }
                                    Err(e) => {
                                        notice!(&format!("Failed to run git commit: {e}"));
                                    }
                                }
                                renderer.request_reprint();
                                continue;
                            }

                            if trimmed == "/export" || trimmed.starts_with("/export ") {
                                let format_arg = trimmed.strip_prefix("/export ").map(|s| s.trim().to_lowercase()).unwrap_or_else(|| "md".to_string());
                                input.clear();
                                autocomplete_idx = 0;
                                let filename = match format_arg.as_str() {
                                    "html" => format!("session_{session_id}.html"),
                                    "jsonl" | "json" => format!("session_{session_id}.jsonl"),
                                    _ => format!("session_{session_id}.md"),
                                };
                                let content = match format_arg.as_str() {
                                    "html" => {
                                        let mut html = String::from("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>FlashAgent Session</title><style>body{font-family:sans-serif;max-width:800px;margin:2rem auto;line-height:1.6;background:#1e1e2e;color:#cdd6f4;}pre{background:#181825;padding:1rem;border-radius:6px;overflow-x:auto;}h3{color:#89b4fa;}</style></head><body>");
                                        for m in &history {
                                            let role = m.role.as_str();
                                            let escaped = m.content.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                                            html.push_str(&format!("<h3>Role: {role}</h3><pre>{escaped}</pre>"));
                                        }
                                        html.push_str("</body></html>");
                                        html
                                    }
                                    "jsonl" | "json" => {
                                        let mut buf = String::new();
                                        for m in &history {
                                            let saved = SavedMessage::from(m);
                                            if let Ok(line) = serde_json::to_string(&saved) {
                                                buf.push_str(&line);
                                                buf.push('\n');
                                            }
                                        }
                                        buf
                                    }
                                    _ => {
                                        let mut md = format!("# FlashAgent Session Export\n- **Session ID**: `{session_id}`\n- **Model**: `{current_model}`\n\n---\n\n");
                                        for m in &history {
                                            md.push_str(&format!("### {}\n\n{}\n\n", m.role.as_str().to_uppercase(), m.content));
                                        }
                                        md
                                    }
                                };
                                match std::fs::write(&filename, content) {
                                    Ok(_) => notice!(&format!("Exported conversation to: \x1b[1m{filename}\x1b[0m")),
                                    Err(e) => notice!(&format!("Failed to export conversation: {e}")),
                                }
                                renderer.request_reprint();
                                continue;
                            }

                            // Check if the command invokes a skill (/skill:<name> or /<name>)
                            let explicit_skill = trimmed.strip_prefix("/skill:").map(|rest| rest.trim().to_string());
                            let skill_name = explicit_skill.clone().or_else(|| {
                                let cand = trimmed.strip_prefix('/').filter(|c| !c.contains(' '))?;
                                find_skill_file(cand).map(|_| cand.to_string())
                            });

                            if let Some(sname) = skill_name {
                                let Some(skill_content) = find_skill_file(&sname).and_then(|p| std::fs::read_to_string(p).ok()) else {
                                    input.clear();
                                    notice!(&format!("[Unknown skill '{sname}'. Type /skills to list available skills.]"));
                                    renderer.request_reprint();
                                    continue;
                                };
                                input.clear();
                                autocomplete_idx = 0;
                                chat.push_user(&format!("/skill:{sname}"));
                                let prompt = format!("Execute skill: {sname}\n\nSkill Instructions:\n{skill_content}");
                                history.push(ChatMessage::user(prompt));
                                update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                                cancel.store(false, Ordering::Relaxed);
                                suggested_prompt = None;
                                custom_placeholder = None;
                                running = true;
                                turn_started = Some(std::time::Instant::now());
                                token_tracker.on_turn_start(current_model.clone(), context_usage.total_used());
                                source.set_model(&current_model);
                                let turn_opts = build_turn_options(&app_config, &current_effort);
                                turn_counter += 1;
                                let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                                active_steer_tx = Some(steer_tx);
                                active_turn_handle = Some(spawn_turn(
                                    cancel.clone(),
                                    source.clone(),
                                    perm,
                                    history.clone(),
                                    GoalBudgets::steps_only(max_steps),
                                    turn_opts,
                                    tx.clone(),
                                    steer_rx,
                                    turn_counter,
                                ));
                                continue;
                            }

                            let text = std::mem::take(&mut input);
                            if input_history.last() != Some(&text) {
                                input_history.push(text.clone());
                            }
                            history_index = None;
                            current_draft.clear();
                            // Sending new prompt closes temporary last thinking block (Rule 4)
                            last_expanded = false;

                            chat.push_user(&text);
                            let first = !history.iter().any(|m| m.role == flashagent_llm::Role::User);
                            let content = if first && !memory_block.is_empty() {
                                format!("{memory_block}\n\n---\n\n{text}")
                            } else {
                                text
                            };
                            history.push(ChatMessage::user(content));
                            update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
                            cancel.store(false, Ordering::Relaxed);
                            suggested_prompt = None;
                            custom_placeholder = None;
                            running = true;
                            turn_started = Some(std::time::Instant::now());
                            token_tracker.on_turn_start(current_model.clone(), context_usage.total_used());
                            source.set_model(&current_model);
                            let turn_opts = build_turn_options(&app_config, &current_effort);
                            turn_counter += 1;
                            let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                            active_steer_tx = Some(steer_tx);
                            active_turn_handle = Some(spawn_turn(
                                cancel.clone(),
                                source.clone(),
                                perm,
                                history.clone(),
                                GoalBudgets::steps_only(max_steps),
                                turn_opts,
                                tx.clone(),
                                steer_rx,
                                turn_counter,
                            ));
                        }
                    }
                    KeyCode::Char(c)
                        if gate.pending().is_none()
                            && !mods.contains(KeyModifiers::CONTROL)
                            && !mods.contains(KeyModifiers::ALT) =>
                    {
                        input.push(c);
                        history_index = None;
                        autocomplete_idx = 0;
                    }
                    _ => {}
                }
            }
        }
    }

    renderer.clear_tail();
    Ok(finish!())
}

fn update_context_usage(
    usage: &mut ContextUsage,
    history: &[ChatMessage],
    memory_block: &str,
    chat: &ChatView,
    tools: &dyn flashagent_core::ToolExec,
) {
    let system_chars: usize = history
        .iter()
        .filter(|m| m.role == flashagent_llm::Role::System)
        .map(|m| m.content.len())
        .sum();
    usage.system_tokens = (system_chars / 4).max(150);
    usage.memory_tokens = memory_block.len() / 4;
    // What the model is actually sent: every advertised schema (MCP included).
    usage.tools_tokens = tools
        .specs()
        .iter()
        .map(|t| (t.name.len() + t.description.len() + t.parameters_json.len()) / 4 + 8)
        .sum();

    let (chat_user, chat_assistant, chat_reasoning, chat_tools) = chat.raw_content_chars();

    let (mut user, mut assistant, mut reasoning, mut tool_out) = (0usize, 0usize, 0usize, 0usize);
    for m in history {
        match m.role {
            flashagent_llm::Role::User => user += m.content.len(),
            flashagent_llm::Role::Assistant => {
                assistant += m.content.len() + m.tool_calls.iter().map(|c| c.args_json.len()).sum::<usize>();
                reasoning += m.reasoning.as_ref().map_or(0, |r| r.len());
            }
            flashagent_llm::Role::Tool => tool_out += m.content.len(),
            flashagent_llm::Role::System => {}
        }
    }
    // The memory block rides inside the first user message; it is already
    // counted once as memory.
    if !memory_block.is_empty()
        && history.iter().find(|m| m.role == flashagent_llm::Role::User).is_some_and(|m| m.content.starts_with(memory_block))
    {
        user = user.saturating_sub(memory_block.len());
    }

    usage.user_tokens = user.max(chat_user) / 4;
    usage.assistant_tokens = assistant.max(chat_assistant) / 4;
    usage.reasoning_tokens = reasoning.max(chat_reasoning) / 4;
    usage.tool_output_tokens = tool_out.max(chat_tools) / 4;
}

fn build_turn_options(
    app_config: &flashagent_core::AppConfig,
    effort: &str,
) -> flashagent_llm::TurnOptions {
    let mut opts = flashagent_llm::TurnOptions {
        temperature: Some(app_config.temperature),
        top_p: app_config.top_p,
        top_k: app_config.top_k,
        repeat_penalty: app_config.repeat_penalty,
        presence_penalty: app_config.presence_penalty,
        min_p: app_config.min_p,
        ..Default::default()
    };
    if effort == "auto" || effort.is_empty() {
        opts.thinking = flashagent_llm::ThinkingEffort::Auto;
        opts.custom_effort = None;
    } else if effort == "default" {
        // The server's own advertised default preset.
        opts.thinking = flashagent_llm::ThinkingEffort::Default;
        opts.custom_effort = None;
    } else {
        opts.custom_effort = Some(effort.to_string());
        match effort.to_lowercase().as_str() {
            "off" => opts.thinking = flashagent_llm::ThinkingEffort::Off,
            "low" => opts.thinking = flashagent_llm::ThinkingEffort::Low,
            "medium" => opts.thinking = flashagent_llm::ThinkingEffort::Medium,
            "high" => opts.thinking = flashagent_llm::ThinkingEffort::High,
            _ => {}
        }
    }
    opts
}

#[allow(clippy::too_many_arguments)]
fn spawn_turn(
    cancel: Arc<std::sync::atomic::AtomicBool>,
    source: Arc<BackendSource>,
    perm: &'static PermissionedTools,
    history: Vec<ChatMessage>,
    budgets: GoalBudgets,
    turn_opts: flashagent_llm::TurnOptions,
    tx: tokio::sync::mpsc::UnboundedSender<UiEvent>,
    steer_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    turn_id: u64,
) -> tokio::task::JoinHandle<()> {
    cancel.store(false, Ordering::Relaxed);
    tokio::spawn(async move {
        let config = LoopConfig {
            max_steps: budgets.steps,
            max_output_tokens: budgets.output_tokens,
            time_budget: budgets.time,
            base_turn_options: turn_opts,
            ..Default::default()
        };
        let loop_ = AgentLoop::with_steering(config, cancel, steer_rx);
        let res = loop_
            .run(source.as_ref(), perm, history, |event| {
                let _ = tx.send(UiEvent::Loop { turn_id, event });
            })
            .await;
        let result = res.map_err(|e| (e.to_string(), e.into_history()));
        let _ = tx.send(UiEvent::Finished { turn_id, result });
    })
}

/// Whether this run produced a conversation worth writing to disk.
///
/// `history` is never empty — it opens with the system prompt — so anything
/// that only checks emptiness saves a session for a start-and-quit, and hands
/// the user a `--resume` id that restores nothing.
fn worth_saving(history: &[ChatMessage]) -> bool {
    history.iter().any(|m| m.role == flashagent_llm::Role::User)
}

/// One line of manual-update progress, e.g.
/// `Update b235 · [████████░░░░░░░░] 52% · 3.5/6.7 MB`.
fn update_progress_line(version: &str, stage: flashagent_svc::updater::UpdateProgress) -> String {
    use flashagent_svc::updater::UpdateProgress;
    const MB: f64 = 1024.0 * 1024.0;
    let detail = match stage {
        UpdateProgress::Downloading { received, total: Some(total) } if total > 0 => {
            let done = (received.min(total) as f64 / total as f64).clamp(0.0, 1.0);
            let filled = (done * 16.0).round() as usize;
            format!(
                "[{}{}] {:>3}% · {:.1}/{:.1} MB",
                "\u{2588}".repeat(filled),
                "\u{2591}".repeat(16 - filled),
                (done * 100.0).round() as u32,
                received as f64 / MB,
                total as f64 / MB
            )
        }
        // Some mirrors send no content-length; show the bytes, not a fake bar.
        UpdateProgress::Downloading { received, .. } => {
            format!("{:.1} MB downloaded", received as f64 / MB)
        }
        UpdateProgress::Verifying => "verifying checksum...".to_string(),
        UpdateProgress::Installing => "installing...".to_string(),
    };
    format!("{UPDATE_LINE_PREFIX}{version} \u{b7} {detail}")
}

/// Marks the single line that manual-update progress rewrites in place.
const UPDATE_LINE_PREFIX: &str = "Update ";

/// The welcome card as first shown. Animated, it starts as its top border and
/// the tick loop draws in the rest; otherwise it goes up whole, because
/// nothing will come back to finish it.
fn opening_card(card: Vec<RenderLine>, animate: bool) -> Vec<RenderLine> {
    if animate {
        card.into_iter().take(1).collect()
    } else {
        card
    }
}

/// How many rows of the welcome card to draw, so it appears to draw itself
/// from the top down over the first half second. `None` once it is whole.
fn welcome_reveal_rows(started_at: std::time::Instant) -> Option<usize> {
    const ROW_MS: u128 = 35;
    const ROWS: u128 = 15;
    let elapsed = started_at.elapsed().as_millis();
    (elapsed < ROW_MS * ROWS).then(|| (elapsed / ROW_MS) as usize + 1)
}

fn extract_user_prompt(content: &str) -> &str {
    if let Some((_mem, user_part)) = content.rsplit_once("\n\n---\n\n") {
        user_part.trim()
    } else {
        content.trim()
    }
}



/// The language a turn is written in, by script, when it is clearly not
/// English. Script is a crude signal but a reliable one for the case that
/// matters: a model answering a Russian conversation in English.
fn script_language(text: &str) -> Option<&'static str> {
    let mut cyrillic = 0usize;
    let mut latin = 0usize;
    let mut cjk = 0usize;
    let mut other = 0usize;
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        match c as u32 {
            0x0400..=0x04FF => cyrillic += 1,
            0x0041..=0x005A | 0x0061..=0x007A => latin += 1,
            0x3040..=0x30FF | 0x4E00..=0x9FFF => cjk += 1,
            _ => other += 1,
        }
    }
    let total = cyrillic + latin + cjk + other;
    if total < 12 {
        return None;
    }
    let share = |n: usize| n as f32 / total as f32;
    // Code and identifiers are Latin even in a Russian conversation, so a
    // minority of Cyrillic is still a Russian conversation.
    if share(cyrillic) > 0.25 {
        Some("Russian")
    } else if share(cjk) > 0.25 {
        Some("Chinese or Japanese, matching the user")
    } else {
        None
    }
}

async fn generate_llm_recap_and_suggestion(
    source: &BackendSource,
    history: &[ChatMessage],
) -> Option<(String, Option<String>)> {
    let last_user_idx = history.iter().rposition(|m| m.role == flashagent_llm::Role::User)?;
    let user_msg = history.get(last_user_idx)?;
    let user_prompt = extract_user_prompt(&user_msg.content);

    let turn_messages = &history[last_user_idx..];
    let assistant_msg = turn_messages.iter().rev().find(|m| m.role == flashagent_llm::Role::Assistant);
    let asst_text = assistant_msg.map(|m| m.content.as_str()).unwrap_or("");
    if asst_text.trim().is_empty() {
        return None;
    }

    let user_snip = flashagent_tui::truncate_middle(user_prompt, 200);
    let asst_snip = flashagent_tui::truncate_middle(asst_text, 600);

    let system_prompt =
        "You are a conversation analyzer. Return strictly JSON with the following fields:\n\
        {\n  \
          \"suggestion\": \"the next message the USER will send to the assistant, in imperative mood, about the work just done (e.g. 'Show Swift code examples', 'Explain how safety works', 'Run the tests and fix what fails'). It must be something the assistant can answer on its own. NEVER ask the user for information about themselves, their project or what they need: 'Tell me about your project', 'Tell me what help you need' are WRONG - those are the assistant talking to the user. Write it in the language of the conversation. Do not repeat the existing query!\",\n  \
          \"recap\": \"one concise past-tense sentence of what was explained or done\"\n\
        }\n\
        Do NOT generate any internal thinking or explanations. Start immediately with { and return only valid JSON.";

    // "the language of the conversation" is not an instruction a small model
    // reliably follows; naming the language is.
    let language_rule = match script_language(&format!("{user_snip} {asst_snip}")) {
        Some(lang) => format!(
            "\nThe conversation is in {lang}. Write BOTH fields in {lang}, not in English."
        ),
        None => String::new(),
    };
    let user_turn_info =
        format!("User prompt: {user_snip}\nAssistant response: {asst_snip}{language_rule}");
    let query_messages = vec![
        ChatMessage::system(system_prompt),
        ChatMessage::user(user_turn_info),
    ];

    let turn_opts = flashagent_llm::TurnOptions {
        thinking: flashagent_llm::ThinkingEffort::Off,
        custom_effort: Some("none".to_string()),
        temperature: Some(0.2),
        max_tokens: Some(512),
        ..Default::default()
    };

    let overall_task = async {
        let mut stream = source.turn_with_options(&query_messages, &[], &turn_opts).await.ok()?;
        use futures::StreamExt;
        let mut collected = String::new();
        let mut reasoning_collected = String::new();
        while let Some(ev_res) = stream.next().await {
            if let Ok(ev) = ev_res {
                match ev {
                    flashagent_llm::LlmEvent::TextDelta(delta) => {
                        collected.push_str(&delta);
                    }
                    flashagent_llm::LlmEvent::ReasoningDelta(delta) => {
                        reasoning_collected.push_str(&delta);
                    }
                    flashagent_llm::LlmEvent::Done(_) => break,
                    _ => {}
                }
            }
        }
        if let Some(parsed) = parse_recap_and_suggestion_json(&collected) {
            Some(parsed)
        } else {
            parse_recap_and_suggestion_json(&reasoning_collected)
        }
    };

    // 90s timeout accommodates larger local models (e.g. 26B Gemma TTFT ~8s + generation ~5s, slow CPU inference, or heavy load)
    tokio::time::timeout(std::time::Duration::from_secs(90), overall_task).await.ok()?
}

fn parse_recap_and_suggestion_json(raw: &str) -> Option<(String, Option<String>)> {
    let mut storage;
    let mut text = raw.trim();

    // Strip <thought>...</thought> or <think>...</think> if model emitted reasoning in text stream
    if let Some(start_think) = text.find("<thought>") {
        if let Some(end_think) = text.find("</thought>") {
            let after = &text[end_think + "</thought>".len()..];
            let before = &text[..start_think];
            storage = format!("{before} {after}");
            text = storage.trim();
        }
    }
    if let Some(start_think) = text.find("<think>") {
        if let Some(end_think) = text.find("</think>") {
            let after = &text[end_think + "</think>".len()..];
            let before = &text[..start_think];
            storage = format!("{before} {after}");
            text = storage.trim();
        }
    }

    // Extract inside ```json ... ``` or ``` ... ``` if present
    let candidate = if let Some(code_start) = text.find("```") {
        let after_fence = &text[code_start + 3..];
        let content_start = if after_fence.to_lowercase().starts_with("json") {
            &after_fence[4..]
        } else {
            after_fence
        };
        if let Some(code_end) = content_start.find("```") {
            &content_start[..code_end]
        } else {
            content_start
        }
    } else {
        text
    };

    // Extract between outermost '{' and '}'
    let json_str = if let (Some(first_brace), Some(last_brace)) = (candidate.find('{'), candidate.rfind('}')) {
        if first_brace <= last_brace {
            &candidate[first_brace..=last_brace]
        } else {
            candidate.trim()
        }
    } else {
        candidate.trim()
    };

    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .ok()
        .or_else(|| flashagent_llm::repair_json(json_str).and_then(|r| serde_json::from_str(&r).ok()))?;

    let recap_raw = parsed.get("recap").and_then(|s| s.as_str())?;
    let recap = recap_raw.trim().trim_matches('"').to_string();
    if recap.is_empty() || is_generic_recap(&recap) {
        return None;
    }

    let suggestion = parsed.get("suggestion")
        .and_then(|s| s.as_str())
        .and_then(sanitize_user_suggestion);

    Some((recap, suggestion))
}

fn sanitize_user_suggestion(s: &str) -> Option<String> {
    let mut trimmed = s.trim().trim_matches('"').trim_matches('\'').trim();
    if trimmed.is_empty() || is_generic_suggestion(trimmed) || is_addressed_to_the_user(trimmed) {
        return None;
    }

    // Strip Russian and ASCII quotes
    trimmed = trimmed
        .trim_matches('«')
        .trim_matches('»')
        .trim_matches('"')
        .trim_matches('\'')
        .trim();

    let mut text = trimmed.to_string();
    let lower = text.to_lowercase();

    // Convert assistant question or infinitive formulations into direct user imperatives
    if lower.starts_with("would you like to see examples of ") {
        text = format!("Show code examples of {}", &trimmed["would you like to see examples of ".len()..]);
    } else if lower.starts_with("would you like to see ") {
        text = format!("Show {}", &trimmed["would you like to see ".len()..]);
    } else if lower.starts_with("would you like to know more about ") {
        text = format!("Explain more about {}", &trimmed["would you like to know more about ".len()..]);
    } else if lower.starts_with("would you like to know ") {
        text = format!("Explain {}", &trimmed["would you like to know ".len()..]);
    } else if lower.starts_with("do you want to know more about ") {
        text = format!("Explain more about {}", &trimmed["do you want to know more about ".len()..]);
    } else if lower.starts_with("do you want to know ") {
        text = format!("Explain {}", &trimmed["do you want to know ".len()..]);
    } else if lower.starts_with("tell me about ") {
        text = format!("Tell me about {}", &trimmed["tell me about ".len()..]);
    } else if lower.starts_with("show me ") {
        text = format!("Show me {}", &trimmed["show me ".len()..]);
    } else if lower.starts_with("show code examples of ") {
        text = format!("Show code examples of {}", &trimmed["show code examples of ".len()..]);
    } else if lower.starts_with("show examples ") {
        text = format!("Show examples {}", &trimmed["show examples ".len()..]);
    } else if lower.starts_with("explain to me ") {
        text = format!("Explain to me {}", &trimmed["explain to me ".len()..]);
    } else if lower.starts_with("explain ") {
        text = format!("Explain {}", &trimmed["explain ".len()..]);
    } else if lower.starts_with("give examples of ") {
        text = format!("Give examples of {}", &trimmed["give examples of ".len()..]);
    } else if lower.starts_with("learn more about ") {
        text = format!("Explain more about {}", &trimmed["learn more about ".len()..]);
    } else if is_assistant_phrased(&text) {
        return None;
    }

    // Capitalize first character
    let mut chars = text.chars();
    let first = chars.next()?;
    let capitalized: String = first.to_uppercase().chain(chars).collect();

    // Clean trailing punctuation (?, ., !) so it is an imperative prompt ready to send
    let clean = capitalized
        .trim_end_matches(['?', '.', '!', ' ', '"', '\'', '»', '«'])
        .to_string();

    if clean.is_empty() || is_generic_suggestion(&clean) {
        None
    } else {
        Some(clean)
    }
}

/// A suggestion is a message the user is about to send TO the model. Models
/// often return the opposite: the assistant asking the *user* for information
/// ("Расскажи о своём проекте", "Tell me about your setup"). Pressing → on one
/// of those sends the user their own question back, so they are dropped.
///
/// The giveaway is the object, not the verb: "расскажи о архитектуре" is a
/// fine prompt, "расскажи о своём проекте" is the assistant talking to you.
fn is_addressed_to_the_user(text: &str) -> bool {
    let lower = text.to_lowercase();
    // Russian: reflexive/second-person possessives always point at whoever is
    // being addressed, and here that is the user.
    const RU_POSSESSIVE: &[&str] = &[
        "свой", "своё", "свое", "своего", "своей", "своем", "своём", "своих", "свою", "своими",
        "твой", "твоё", "твое", "твоей", "твоем", "твоём", "твои", "твоих",
        "ваш ", "ваша", "ваше", "вашей", "вашем", "ваши", "вашу",
    ];
    // Phrases that only make sense coming from the assistant.
    const HANDOFF: &[&str] = &[
        "нужна помощь", "нужна ли помощь", "чем помочь", "чем могу помочь", "чем я могу помочь",
        "что тебя интересует", "что вас интересует", "дай знать", "дайте знать", "если хочешь",
        "если хотите", "уточни, что", "уточните, что",
        "your project", "your code", "your task", "your goal", "your setup", "your repo",
        "your codebase", "your use case", "your requirements", "what help", "how can i help",
        "let me know", "feel free to", "if you'd like", "if you would like",
    ];
    RU_POSSESSIVE.iter().chain(HANDOFF).any(|needle| lower.contains(needle))
}

fn is_assistant_phrased(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("would you ")
        || lower.starts_with("do you want ")
        || lower.starts_with("should i ")
        || lower.starts_with("what examples ")
        || lower.starts_with("which ")
        || lower.starts_with("what would ")
        || lower.ends_with("show?")
}

fn is_generic_recap(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("response generated")
        || lower.contains("answer provided")
        || lower.contains("what's next")
}

fn is_generic_suggestion(text: &str) -> bool {
    let lower = text.to_lowercase();
    let trimmed = lower.trim_matches(|c: char| c.is_whitespace() || c == '?' || c == '.' || c == '!' || c == '"' || c == '\'');
    trimmed == "what's next"
        || trimmed == "what next"
        || trimmed == "continue"
        || trimmed == "next"
}

/// Marker that introduces the rolling summary inside the system message.
const COMPACTED_MARK: &str = "\n\n[Compacted Conversation History]:\n";

async fn compact_context(
    source: &BackendSource,
    history: &mut Vec<ChatMessage>,
    focus_prompt: Option<&str>,
) -> Option<usize> {
    use futures::StreamExt;

    // Keep the current turn intact, starting at its user message: cutting
    // anywhere else can orphan tool results from their tool calls, or leave an
    // assistant message first — both rejected by strict servers/templates.
    let split_idx = history.iter().rposition(|m| m.role == flashagent_llm::Role::User)?;
    if split_idx <= 1 {
        return None;
    }

    let before_tokens: usize = history.iter().map(|m| m.content.len() / 4 + 4).sum();

    // A previous summary lives in the system message; fold it in so repeated
    // compaction never forgets older turns.
    let (base_system, prior_summary) = match history[0].content.find(COMPACTED_MARK) {
        Some(pos) => (history[0].content[..pos].to_string(), Some(history[0].content[pos + COMPACTED_MARK.len()..].to_string())),
        None => (history[0].content.clone(), None),
    };

    let mut to_compact = String::new();
    if let Some(prior) = &prior_summary {
        to_compact.push_str(&format!("Earlier summary:\n{prior}\n\n"));
    }
    for msg in &history[1..split_idx] {
        let role_str = match msg.role {
            flashagent_llm::Role::User => "User",
            flashagent_llm::Role::Assistant => "Assistant",
            flashagent_llm::Role::System => "System",
            flashagent_llm::Role::Tool => "Tool",
        };
        let clean_content = if msg.role == flashagent_llm::Role::User {
            extract_user_prompt(&msg.content)
        } else {
            msg.content.as_str()
        };
        to_compact.push_str(&format!("{role_str}: {}\n\n", clean_content));
    }

    let focus_text = if let Some(focus) = focus_prompt {
        format!("\nSpecial user focus/instructions: preserve details regarding: {focus}\n")
    } else {
        String::new()
    };

    let summary_prompt = format!(
        "You are an expert technical context compaction engine.\n\
         Summarize the following conversation history concisely into clean structured notes.\n\
         Preserve key technical decisions, architectural context, modified files, errors encountered, and user intents.\n\
         Discard pleasantries, repetitive steps, and verbose tool dumps.\n\
         {focus_text}\n\
         Conversation to compact:\n\n\
         {to_compact}\n\n\
         Provide ONLY the structured technical summary."
    );

    let msgs = vec![
        ChatMessage::system("You are a technical context compaction engine."),
        ChatMessage::user(summary_prompt),
    ];
    let opts = flashagent_llm::TurnOptions {
        thinking: flashagent_llm::ThinkingEffort::Off,
        temperature: Some(0.2),
        ..Default::default()
    };

    let summary = if let Ok(Ok(mut stream)) = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        source.turn_with_options(&msgs, &[], &opts),
    ).await {
        let mut text = String::new();
        while let Ok(Some(Ok(ev))) = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await {
            if let flashagent_llm::LlmEvent::TextDelta(d) = ev {
                text.push_str(&d);
            }
        }
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            trimmed.to_string()
        } else {
            fallback_summary(&history[1..split_idx], prior_summary.as_deref())
        }
    } else {
        fallback_summary(&history[1..split_idx], prior_summary.as_deref())
    };

    let mut new_history = Vec::with_capacity(history.len() - split_idx + 1);
    new_history.push(ChatMessage::system(format!("{base_system}{COMPACTED_MARK}{summary}")));
    new_history.extend_from_slice(&history[split_idx..]);

    let after_tokens: usize = new_history.iter().map(|m| m.content.len() / 4 + 4).sum();
    *history = new_history;

    let freed = before_tokens.saturating_sub(after_tokens);
    Some(freed.max(1))
}

fn fallback_summary(messages: &[ChatMessage], prior: Option<&str>) -> String {
    let mut out = String::from("Previous conversation summary (auto-compacted):\n");
    if let Some(prior) = prior {
        out.push_str(prior.trim_end());
        out.push('\n');
    }
    for (i, msg) in messages.iter().enumerate() {
        let role = match msg.role {
            flashagent_llm::Role::User => "User",
            flashagent_llm::Role::Assistant => "Assistant",
            flashagent_llm::Role::System => "System",
            flashagent_llm::Role::Tool => "Tool",
        };
        let clean = if msg.role == flashagent_llm::Role::User {
            extract_user_prompt(&msg.content)
        } else {
            msg.content.as_str()
        };
        let snippet = flashagent_tui::truncate_middle(clean, 120);
        out.push_str(&format!("{}. {role}: {snippet}\n", i + 1));
    }
    out
}

/// Strict chat templates (Gemma, Mistral) demand alternating user/assistant
/// turns; a turn that ended before any reply would leave two user messages
/// in a row once the next prompt is sent.
fn close_dangling_user(history: &mut Vec<ChatMessage>, note: &str) {
    if history.last().is_some_and(|m| m.role == flashagent_llm::Role::User) {
        history.push(ChatMessage::assistant(note));
    }
}

/// Skill file for `name`: the project's `.agents/skills` first, then the
/// user's `~/.flashagent/skills` (the same places autocomplete lists).
fn find_skill_file(name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    let file = format!("{name}.md");
    let project = std::path::Path::new(".agents").join("skills").join(&file);
    let global = flashagent_home_dir().map(|h| h.join("skills").join(&file));
    std::iter::once(project).chain(global).find(|p| p.is_file())
}

/// A settings view that shows the live session state (mode, effort, model),
/// not just the startup defaults stored in the config file.
fn settings_for_runtime(
    config: &AppConfig,
    mode: PermissionMode,
    effort: &str,
    model: &str,
    models: &[String],
) -> SettingsView {
    let mut cfg = config.clone();
    cfg.permission_mode = mode;
    cfg.thinking_effort = effort.to_string();
    cfg.model = model.to_string();
    SettingsView::new(cfg, models.to_vec())
}

/// The config to persist from a settings view. The view shows the live mode
/// and effort; they become startup defaults only when the user changed them
/// there — merely opening Settings mid-session (or during /goal's Accept All)
/// must not silently make that the default for the next launch.
fn persisted_from_view(view: &AppConfig, persisted: &AppConfig, shown_mode: PermissionMode, shown_effort: &str) -> AppConfig {
    let mut cfg = view.clone();
    if cfg.permission_mode == shown_mode {
        cfg.permission_mode = persisted.permission_mode;
    }
    if cfg.thinking_effort == shown_effort {
        cfg.thinking_effort = persisted.thinking_effort.clone();
    }
    cfg
}

/// Rows of an approval-card value: wrapped (never silently clipped), with
/// long whitespace runs made visible — padding must not push the tail of a
/// command (`... | sh`) out of sight — and an explicit note when rows run out.
fn card_rows(value: &str, width: usize, max_rows: usize) -> Vec<String> {
    let mut shown = String::new();
    for line in value.lines() {
        if !shown.is_empty() {
            shown.push('\u{21b5}'); // ↵ marks a real newline
        }
        let mut spaces = 0usize;
        for c in line.chars().chain(std::iter::once('\0')) {
            if c == ' ' {
                spaces += 1;
                continue;
            }
            if spaces > 3 {
                shown.push_str(&format!(" \u{2423}x{spaces} "));
            } else {
                shown.push_str(&" ".repeat(spaces));
            }
            spaces = 0;
            if c != '\0' {
                shown.push(c);
            }
        }
    }
    let chars: Vec<char> = shown.chars().collect();
    let mut rows: Vec<String> = chars.chunks(width.max(1)).map(|c| c.iter().collect()).collect();
    if rows.len() > max_rows {
        let hidden: usize = rows[max_rows - 1..].iter().map(|r| r.chars().count()).sum();
        rows.truncate(max_rows - 1);
        rows.push(format!("... {hidden} more characters (not shown; deny if unsure)"));
    }
    rows
}

/// Model-supplied text made safe to print: control characters (ANSI
/// escapes included) are shown, not executed.
fn card_safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\n' { '\u{fffd}' } else { c })
        .collect()
}

/// Draw the `/goal` ledger as a card. Facts the loop reported, so a run that
/// stopped at a budget cannot read as a run that finished — whatever the
/// model's own closing summary claims.
fn push_goal_report(chat: &mut ChatView, ledger: &GoalLedger, reason: DoneReason) {
    let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
    let card_w = (w as usize).saturating_sub(4).clamp(44, 110);
    let inner_w = card_w.saturating_sub(2);
    let border = if matches!(reason, DoneReason::Completed) {
        "\x1b[38;2;145;205;140m"
    } else {
        "\x1b[38;2;225;175;95m"
    };
    let reset = "\x1b[0m";

    let title = format!(" Goal report: {} ", flashagent_tui::truncate_middle(&ledger.task, inner_w.saturating_sub(18)));
    let dashes = inner_w.saturating_sub(flashagent_tui::visible_width(&title) + 1);
    chat.push_line(
        LineKind::System,
        format!("{border}╭─\x1b[1;38;2;245;240;232m{title}{border}{}╮{reset}", "─".repeat(dashes)),
    );
    for line in ledger.report(reason) {
        for row in wrap_plain(&card_safe(&line), inner_w.saturating_sub(2)) {
            let pad = inner_w.saturating_sub(flashagent_tui::visible_width(&row) + 1);
            chat.push_line(
                LineKind::System,
                format!("{border}│{reset} \x1b[38;2;200;196;188m{row}\x1b[0m{}{border}│{reset}", " ".repeat(pad)),
            );
        }
    }
    chat.push_line(LineKind::System, format!("{border}╰{}╯{reset}", "─".repeat(inner_w)));
}

/// Wrap plain text at `width` on word boundaries, keeping a leading indent.
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    if text.chars().count() <= width {
        return vec![text.to_string()];
    }
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let candidate = if row.is_empty() { word.chars().count() } else { row.chars().count() + 1 + word.chars().count() };
        if !row.is_empty() && candidate > width {
            rows.push(std::mem::take(&mut row));
            row.push_str(&indent);
            row.push_str("  ");
        }
        if !row.is_empty() && !row.ends_with(' ') {
            row.push(' ');
        }
        row.push_str(word);
        // A single word longer than the card: hard-split it.
        while row.chars().count() > width {
            let head: String = row.chars().take(width).collect();
            let tail: String = row.chars().skip(width).collect();
            rows.push(head);
            row = tail;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Run the tool-calling check once, right after setup, and say what it means.
async fn first_run_tool_check(config: &AppConfig) {
    println!("\nChecking whether {} can drive tools...", config.model);
    let source = BackendSource(flashagent_llm::OpenAiCompat::new(
        &config.backend_url,
        &config.model,
        config.api_key.clone(),
    ));
    let report = flashagent_core::toolcheck::check_model(
        &source,
        &config.model,
        std::time::Duration::from_secs(60),
    )
    .await;
    for line in report.lines() {
        println!("{line}");
    }
    let (passed, total) = report.score();
    if passed < total {
        println!(
            "\nA model that fails these will talk about doing the work instead of doing it.\n\
             You can re-run this any time with `flashagent --tool-test`, or compare models\n\
             with `flashagent --tool-test --all-models`."
        );
    }
    println!();
}

/// `--tool-test`: run the tool-calling scenarios against the configured model,
/// or against every model the server lists. Exits non-zero when a model cannot
/// drive tools, so it can be used as a check rather than only read.
async fn run_tool_check_cli(config: &AppConfig, all_models: bool) -> i32 {
    let timeout = std::time::Duration::from_secs(120);
    let mut models = vec![config.model.clone()];
    if all_models {
        let probe = BackendSource(flashagent_llm::OpenAiCompat::new(
            &config.backend_url,
            &config.model,
            config.api_key.clone(),
        ));
        match probe.discover_server().await {
            Some(disc) if !disc.models.is_empty() => {
                models = disc
                    .models
                    .iter()
                    .map(|m| m.id.clone())
                    .filter(|id| flashagent_core::toolcheck::is_chat_model(id))
                    .collect();
            }
            _ => {
                eprintln!("Could not list models at {}; checking the configured one only.", config.backend_url);
            }
        }
    }

    println!("Tool-calling check against {}", config.backend_url);
    let mut reports = Vec::new();
    for model in &models {
        println!("\n{model}");
        let source = BackendSource(flashagent_llm::OpenAiCompat::new(
            &config.backend_url,
            model,
            config.api_key.clone(),
        ));
        let report = flashagent_core::toolcheck::check_model(&source, model, timeout).await;
        for line in report.lines() {
            println!("{line}");
        }
        reports.push(report);
    }

    if reports.len() > 1 {
        println!("\n{}", flashagent_core::toolcheck::MARKDOWN_HEADER);
        for r in &reports {
            println!("{}", r.markdown_row());
        }
    }
    let json: Vec<_> = reports.iter().map(|r| r.to_json()).collect();
    if let Ok(path) = std::env::var("FLASHAGENT_TOOL_TEST_JSON") {
        // Raw results, so a published table can be checked rather than believed.
        if let Ok(text) = serde_json::to_string_pretty(&json) {
            let _ = std::fs::write(&path, text);
            println!("\nRaw results written to {path}");
        }
    }

    // A model that fails everything is a failure of the check, not of the run.
    i32::from(reports.iter().any(|r| r.score().0 == 0))
}

/// Settings → "Run Tool Test": ask the active model to call a probe tool and
/// report whether a well-formed call came back — natively or as text markup
/// that FlashAgent's scanner recovers.
async fn run_tool_call_probe(source: &BackendSource) -> String {
    use futures::StreamExt;
    let probe = flashagent_llm::ToolSpec {
        name: "report_status".into(),
        description: "Report a numeric status code back to the test harness.".into(),
        parameters_json: r#"{"type":"object","properties":{"code":{"type":"integer"}},"required":["code"]}"#.into(),
    };
    let messages = vec![
        ChatMessage::system("You are a tool-calling test harness. Respond only by calling the provided tool."),
        ChatMessage::user("Call the report_status tool with code 42."),
    ];
    let opts = flashagent_llm::TurnOptions {
        thinking: flashagent_llm::ThinkingEffort::Off,
        temperature: Some(0.0),
        max_tokens: Some(1024),
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let run = async {
        let mut stream = source.turn_with_options(&messages, std::slice::from_ref(&probe), &opts).await.map_err(|e| e.to_string())?;
        let (mut name, mut args, mut text) = (String::new(), String::new(), String::new());
        while let Some(ev) = stream.next().await {
            match ev.map_err(|e| e.to_string())? {
                flashagent_llm::LlmEvent::ToolCallDelta { name: Some(n), args_delta, .. } => {
                    name = n;
                    args.push_str(&args_delta);
                }
                flashagent_llm::LlmEvent::ToolCallDelta { args_delta, .. } => args.push_str(&args_delta),
                flashagent_llm::LlmEvent::TextDelta(t) => text.push_str(&t),
                _ => {}
            }
        }
        Ok::<_, String>((name, args, text))
    };
    let (name, args, text) = match tokio::time::timeout(std::time::Duration::from_secs(120), run).await {
        Err(_) => return "FAIL: no answer within 120s".into(),
        Ok(Err(e)) => return format!("FAIL: {e}"),
        Ok(Ok(parts)) => parts,
    };
    let secs = started.elapsed().as_secs_f32();
    let (name, args, via) = if !name.is_empty() {
        (name, args, "native")
    } else {
        let mut scanner = flashagent_llm::TextToolScanner::default();
        let mut events = scanner.feed(&text);
        events.extend(scanner.finish());
        match events.into_iter().find_map(|e| match e {
            flashagent_llm::ScannerEvent::ToolCall { name, args_json, .. } => Some((name, args_json)),
            flashagent_llm::ScannerEvent::Text(_) => None,
        }) {
            Some((n, a)) => (n, a, "text markup"),
            None => {
                let snippet: String = text.trim().chars().take(60).collect();
                return format!("FAIL: model replied with text, no tool call ({snippet:?})");
            }
        }
    };
    let code = serde_json::from_str::<serde_json::Value>(&args)
        .ok()
        .or_else(|| flashagent_llm::repair_json(&args).and_then(|r| serde_json::from_str(&r).ok()))
        .and_then(|v| v.get("code").and_then(|c| c.as_i64()));
    match (name == "report_status", code) {
        (true, Some(42)) => format!("PASS: {via} tool call in {secs:.1}s"),
        (true, _) => format!("PARTIAL: {via} call, wrong arguments {args}"),
        (false, _) => format!("FAIL: called unknown tool '{name}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offline_source() -> BackendSource {
        // Nothing listens on port 9: every request fails fast, so compaction
        // takes its offline fallback path.
        BackendSource(flashagent_llm::OpenAiCompat::new("http://127.0.0.1:9/v1", "m", None))
    }

    #[tokio::test]
    async fn compaction_never_orphans_tool_results_or_stacks_system_messages() {
        let call = flashagent_llm::ToolCall { id: "c1".into(), name: "read_file".into(), args_json: "{}".into() };
        let mut asst_call = ChatMessage::assistant("");
        asst_call.tool_calls = vec![call];
        let mut history = vec![
            ChatMessage::system("SYSTEM PROMPT"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("read a file"),
            asst_call,
            ChatMessage::tool_result("c1", "file body"),
            ChatMessage::assistant("here it is"),
        ];
        let freed = compact_context(&offline_source(), &mut history, None).await;
        assert!(freed.is_some());
        assert_eq!(history.iter().filter(|m| m.role == flashagent_llm::Role::System).count(), 1);
        assert!(history[0].content.starts_with("SYSTEM PROMPT"));
        assert!(history[0].content.contains("first question"));
        // The kept region is the whole current turn, starting at its prompt.
        assert_eq!(history[1].role, flashagent_llm::Role::User);
        assert_eq!(history[1].content, "read a file");
        assert_eq!(history[2].tool_calls.len(), 1);
        assert_eq!(history[3].tool_call_id.as_deref(), Some("c1"));

        // A second compaction keeps the earlier summary instead of dropping it.
        history.push(ChatMessage::user("next"));
        history.push(ChatMessage::assistant("ok"));
        compact_context(&offline_source(), &mut history, None).await;
        assert!(history[0].content.contains("first question"));
        assert_eq!(history[0].content.matches(COMPACTED_MARK.trim()).count(), 1);
    }

    #[tokio::test]
    async fn single_turn_history_is_already_compact() {
        let mut history = vec![ChatMessage::system("s"), ChatMessage::user("u"), ChatMessage::assistant("a")];
        assert_eq!(compact_context(&offline_source(), &mut history, None).await, None);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn approval_card_shows_commands_whatever_the_json_formatting() {
        let spaced = flashagent_llm::effective_args("{ \"command\" : \"cargo test\" , \"timeout_ms\": 5 }", "run_shell").unwrap();
        assert_eq!(spaced.get("command").and_then(|v| v.as_str()), Some("cargo test"));
        let pythonish = flashagent_llm::effective_args("{'command': 'ls -la'}", "run_shell").unwrap();
        assert_eq!(pythonish.get("command").and_then(|v| v.as_str()), Some("ls -la"));
        // An escape sequence in a command is displayed, never executed.
        assert!(!card_safe("echo \u{1b}[8mhidden").contains('\u{1b}'));
    }

    #[test]
    fn approval_card_never_hides_the_tail_of_a_command() {
        let padded = format!("cargo test{}| sh", " ".repeat(200));
        let rows = card_rows(&padded, 40, 6);
        let joined = rows.join("");
        assert!(joined.contains("| sh"), "{rows:?}");
        assert!(joined.contains("x200"));
        let long = "x".repeat(1000);
        let rows = card_rows(&long, 40, 6);
        assert_eq!(rows.len(), 6);
        assert!(rows[5].contains("more characters"));
    }

    #[test]
    fn default_effort_means_the_server_default_preset() {
        let cfg = AppConfig::default();
        assert_eq!(build_turn_options(&cfg, "default").thinking, flashagent_llm::ThinkingEffort::Default);
        assert_eq!(build_turn_options(&cfg, "auto").thinking, flashagent_llm::ThinkingEffort::Auto);
        let high = build_turn_options(&cfg, "high");
        assert_eq!(high.thinking, flashagent_llm::ThinkingEffort::High);
        assert_eq!(high.custom_effort.as_deref(), Some("high"));
    }

    #[test]
    fn opening_settings_never_persists_the_live_mode_as_default() {
        let persisted = AppConfig { permission_mode: PermissionMode::AcceptEdits, thinking_effort: "auto".into(), ..AppConfig::default() };
        let view = settings_for_runtime(&persisted, PermissionMode::Bypass, "high", "m", &[]);
        // Untouched: defaults stay as they were on disk.
        let saved = persisted_from_view(&view.config, &persisted, PermissionMode::Bypass, "high");
        assert_eq!(saved.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(saved.thinking_effort, "auto");
        // Changed in the view: that choice becomes the default.
        let mut edited = view.config.clone();
        edited.permission_mode = PermissionMode::Manual;
        let saved = persisted_from_view(&edited, &persisted, PermissionMode::Bypass, "high");
        assert_eq!(saved.permission_mode, PermissionMode::Manual);
    }

    #[test]
    fn settings_open_on_live_session_state() {
        let cfg = AppConfig { permission_mode: PermissionMode::AcceptEdits, thinking_effort: "auto".into(), ..AppConfig::default() };
        let view = settings_for_runtime(&cfg, PermissionMode::Bypass, "high", "gemma", &[]);
        assert_eq!(view.config.permission_mode, PermissionMode::Bypass);
        assert_eq!(view.config.thinking_effort, "high");
        assert_eq!(view.config.model, "gemma");
    }

    #[test]
    fn test_parse_recap_and_suggestion_json() {
        let raw = "```json\n{\"recap\": \"Explained Swift concepts\", \"suggestion\": \"Show mascot code\"}\n```";
        let parsed = parse_recap_and_suggestion_json(raw);
        assert_eq!(
            parsed,
            Some(("Explained Swift concepts".to_string(), Some("Show mascot code".to_string())))
        );

        // Filters out generic filler
        let generic = "{\"recap\": \"Response generated\", \"suggestion\": \"What's next?\"}";
        assert_eq!(parse_recap_and_suggestion_json(generic), None);
    }

    #[test]
    fn test_sanitize_user_suggestion_conversions() {
        assert_eq!(
            sanitize_user_suggestion("«Tell me about Swift features»"),
            Some("Tell me about Swift features".to_string())
        );
        assert_eq!(
            sanitize_user_suggestion("show code examples of Swift"),
            Some("Show code examples of Swift".to_string())
        );
        assert_eq!(
            sanitize_user_suggestion("tell me about Swift features."),
            Some("Tell me about Swift features".to_string())
        );
        assert_eq!(
            sanitize_user_suggestion("Would you like to know more about memory safety?"),
            Some("Explain more about memory safety".to_string())
        );
        // Generic fillers are rejected
        assert_eq!(sanitize_user_suggestion("What's next?"), None);
        assert_eq!(sanitize_user_suggestion("Next"), None);
        assert_eq!(sanitize_user_suggestion("Continue"), None);
    }

    #[test]
    fn suggestions_the_assistant_aimed_at_the_user_are_dropped() {
        // Pressing → sends the suggestion to the model, so a question the
        // assistant is asking *the user* is worse than no suggestion at all.
        for bad in [
            "Расскажи о своём проекте и опиши, в чём нужна помощь",
            "Опиши свою задачу подробнее",
            "Напиши, чем могу помочь",
            "Дай знать, если хочешь примеры",
            "Tell me about your project",
            "Let me know what you need",
            "Describe your setup and your goal",
            "Feel free to ask about anything else",
        ] {
            assert_eq!(sanitize_user_suggestion(bad), None, "should have been dropped: {bad}");
        }
    }

    #[test]
    fn real_follow_up_prompts_still_pass() {
        // The verb is not the giveaway — the object is. These are things a
        // user genuinely sends next, and they must survive the filter.
        for (input, expected) in [
            ("Расскажи об архитектуре агентного цикла", "Расскажи об архитектуре агентного цикла"),
            ("Объясни, почему тест падает", "Объясни, почему тест падает"),
            ("Запусти тесты и почини то, что упало", "Запусти тесты и почини то, что упало"),
            ("Explain how the permission modes differ", "Explain how the permission modes differ"),
            ("Show the diff before applying it", "Show the diff before applying it"),
        ] {
            assert_eq!(
                sanitize_user_suggestion(input).as_deref(),
                Some(expected),
                "should have been kept: {input}"
            );
        }
    }


    #[test]
    fn test_token_tracker_initial_state() {
        let tracker = TokenTracker::new("default".to_string());
        assert_eq!(tracker.format_stats(), None);
    }

    #[test]
    fn test_token_tracker_turn_start_and_usage() {
        let mut tracker = TokenTracker::new("default".to_string());
        tracker.on_turn_start("default".to_string(), 17);

        let stats = tracker.format_stats().expect("should have stats");
        assert!(stats.contains("17 prompt"));

        // Simulate usage with completion and MTP speculative decoding stats
        let usage = flashagent_llm::Usage {
            prompt: Some(17),
            completion: Some(25),
            cached: None,
            mtp: Some(flashagent_llm::MtpStats {
                total_draft_tokens: 16,
                accepted_draft_tokens: 13,
                rejected_draft_tokens: 3,
            }),
        };
        tracker.on_usage(&usage);
        tracker.last_tg = Some(24.3);
        tracker.on_finished();

        let formatted = tracker.format_stats().expect("should format stats");
        assert!(formatted.contains("17 prompt"));
        assert!(formatted.contains("24.3 tg"));
        assert!(formatted.contains("mtp: 81%"));
    }

    #[test]
    fn test_token_tracker_mtp_omitted_when_not_applicable() {
        let mut tracker = TokenTracker::new("default".to_string());
        tracker.on_turn_start("default".to_string(), 4920);

        let usage = flashagent_llm::Usage {
            prompt: Some(4920),
            completion: Some(100),
            cached: None,
            mtp: None,
        };
        tracker.on_usage(&usage);
        tracker.last_tg = Some(15.2);
        tracker.on_finished();

        let formatted = tracker.format_stats().expect("should format stats");
        assert!(formatted.contains("4.9K prompt"));
        assert!(formatted.contains("15.2 tg"));
        assert!(!formatted.contains("mtp:"));
    }

    #[test]
    fn test_format_status_left_running_does_not_duplicate_metrics() {
        let mut tracker = TokenTracker::new("default".to_string());
        tracker.on_turn_start("default".to_string(), 4920);
        let usage = flashagent_llm::Usage {
            prompt: Some(4920),
            completion: Some(100),
            cached: None,
            mtp: None,
        };
        tracker.on_usage(&usage);
        tracker.last_tg = Some(44.4);
        tracker.last_ttft = Some(std::time::Duration::from_millis(1770));

        // When running is true, Line 3 must show "Generating response..." and NOT duplicate prompt tokens or TTFT
        let status = format_status_left(true, false, None, "Normal", Some(&tracker), 80, "", 0);
        assert!(status.contains("Generating response..."));
        assert!(status.contains("[Normal]"));
        assert!(!status.contains("4.9K"));
        assert!(!status.contains("TTFT"));
        assert!(!status.contains("44.4 tg"));

        // When running is false, Line 3 displays the completed turn telemetry
        tracker.on_finished();
        let status_done = format_status_left(false, false, None, "Normal", Some(&tracker), 80, "", 0);
        assert!(status_done.contains("4.9K prompt"));
        assert!(status_done.contains("TTFT 1.77s"));
        assert!(status_done.contains("44.4 tg"));
        assert!(!status_done.contains("Generating response..."));
    }

    #[test]
    fn test_format_status_left_goal_active_modes() {
        let tracker = TokenTracker::new("default".to_string());
        let status_running = format_status_left(true, true, None, "Autonomous", Some(&tracker), 80, "", 0);
        assert!(status_running.contains("[Goal: Autonomous]"));
        assert!(status_running.contains("Generating response..."));

        let status_ready = format_status_left(false, true, None, "Autonomous", None, 80, "", 0);
        assert!(status_ready.contains("[Goal: Autonomous]"));
        assert!(status_ready.contains("Ready"));
    }

    #[test]
    fn test_goal_progress_replaces_the_generic_running_text() {
        let tracker = TokenTracker::new("default".to_string());
        let status = format_status_left(
            true,
            true,
            Some("step 12/250 · 4.2k tok · 3m05s/1h0m"),
            "Autonomous",
            Some(&tracker),
            80,
            "",
            0,
        );
        assert!(status.contains("step 12/250"), "{status}");
        assert!(status.contains("3m05s/1h0m"), "{status}");
        assert!(!status.contains("Generating response..."), "{status}");
    }

    #[test]
    fn opening_and_closing_saves_nothing() {
        let system_only = vec![ChatMessage::system("you are a helpful agent")];
        assert!(!worth_saving(&system_only), "a start-and-quit must not leave a session file");

        let mut talked = system_only.clone();
        talked.push(ChatMessage::user("привет"));
        assert!(worth_saving(&talked));
    }

    #[test]
    fn switching_channel_says_what_it_will_do() {
        use flashagent_core::config::UpdateChannel;

        // Going back to stable is a downgrade, and the user is told so in
        // those words before anything is replaced.
        let down = channel_switch_warning(
            UpdateChannel::Stable,
            "b238",
            &ChannelTarget::Version("v1.0.0".into()),
        );
        assert!(down.contains("from b238 down to v1.0.0"), "{down}");
        assert!(down.contains("disappear"), "{down}");
        assert!(down.ends_with("Continue?"), "{down}");

        let up = channel_switch_warning(
            UpdateChannel::Beta,
            "v1.0.0",
            &ChannelTarget::Version("b238".into()),
        );
        assert!(up.contains("from v1.0.0 to b238"), "{up}");
        assert!(up.contains("regress"), "{up}");

        // The feed has not answered yet, or could not be reached: name the
        // channel rather than invent a version.
        for unknown in [ChannelTarget::Checking, ChannelTarget::Unknown] {
            let text = channel_switch_warning(UpdateChannel::Stable, "b238", &unknown);
            assert!(text.contains("the newest stable release"), "{text}");
        }

        // Nothing published there yet — the honest answer is that you would
        // stay where you are.
        let empty = channel_switch_warning(UpdateChannel::Stable, "b238", &ChannelTarget::Empty);
        assert!(empty.contains("Nothing is published"), "{empty}");
        assert!(empty.contains("stay on b238"), "{empty}");
    }

    #[test]
    fn the_conversation_language_is_named_not_guessed() {
        // A Russian conversation that quotes Python still reads as Russian:
        // the code is Latin either way.
        let ru = "Покажи пример кода для функции сортировки. Вот пример функции \
                  сортировки пузырьком: def bubble_sort(arr): return sorted(arr)";
        assert_eq!(script_language(ru), Some("Russian"));

        assert_eq!(script_language("Show me a bubble sort in Python, with comments"), None);
        // Too little to judge: better to say nothing than to guess.
        assert_eq!(script_language("ок"), None);
        assert_eq!(script_language(""), None);
    }

    #[test]
    fn manual_update_progress_shows_what_it_is_doing() {
        use flashagent_svc::updater::UpdateProgress;
        let half = update_progress_line(
            "b235",
            UpdateProgress::Downloading { received: 3_500_000, total: Some(7_000_000) },
        );
        assert!(half.starts_with(UPDATE_LINE_PREFIX), "{half}");
        assert!(half.contains("50%"), "{half}");
        assert!(half.contains("3.3/6.7 MB"), "{half}");
        assert!(half.contains('█') && half.contains('░'), "{half}");

        // A mirror that sends no content-length must not get a fabricated bar.
        let unknown =
            update_progress_line("b235", UpdateProgress::Downloading { received: 1_048_576, total: None });
        assert!(unknown.contains("1.0 MB downloaded"), "{unknown}");
        assert!(!unknown.contains('%'), "{unknown}");

        // The stages after the download are named, not silent.
        assert!(update_progress_line("b235", UpdateProgress::Verifying).contains("verifying"));
        assert!(update_progress_line("b235", UpdateProgress::Installing).contains("installing"));

        // Every stage rewrites one line rather than stacking up.
        let mut chat = ChatView::default();
        for stage in [
            UpdateProgress::Downloading { received: 1, total: Some(10) },
            UpdateProgress::Downloading { received: 9, total: Some(10) },
            UpdateProgress::Verifying,
            UpdateProgress::Installing,
        ] {
            chat.update_or_push_system(UPDATE_LINE_PREFIX, &update_progress_line("b235", stage));
        }
        let lines = chat.render(120);
        let update_lines = lines
            .iter()
            .filter(|(_, t)| flashagent_tui::strip_ansi(t).trim_start().starts_with(UPDATE_LINE_PREFIX))
            .count();
        assert_eq!(update_lines, 1, "progress must rewrite its line, not stack: {lines:?}");
    }

    #[test]
    fn the_face_keeps_the_user_company_only_while_working() {
        let tracker = TokenTracker::new("default".to_string());
        let working = format_status_left(true, false, None, "Normal", Some(&tracker), 80, "", 0);
        assert!(working.contains("(•_•)"), "{working}");
        let blinking = format_status_left(true, false, None, "Normal", Some(&tracker), 80, "", 46);
        assert!(blinking.contains("(-_-)"), "{blinking}");
        // Idle the mascot lives on the welcome card; two of them would be one
        // too many.
        let idle = format_status_left(false, false, None, "Normal", None, 80, "", 0);
        assert!(!idle.contains("(•_•)"), "{idle}");
    }

    #[test]
    fn a_resumed_session_gets_the_whole_card_not_a_stuck_reveal() {
        // The tick loop stops refreshing the card once the transcript has a
        // user message, so a resumed session must never start truncated.
        let card = flashagent_tui::welcome_card_responsive_opts(
            "m", "/tmp", "Manual", 0, None, None, 100, 30, 0, true,
            flashagent_tui::MascotMood::Checking,
        );
        assert!(card.len() > 1);
        assert_eq!(opening_card(card.clone(), false).len(), card.len(), "resume draws it whole");
        assert_eq!(opening_card(card, true).len(), 1, "a fresh start animates it in");
    }

    #[test]
    fn test_goal_report_card_states_the_budget_stop() {
        let mut chat = ChatView::default();
        let ledger = GoalLedger::new(
            "refactor the parser".into(),
            GoalBudgets { steps: Some(40), time: None, output_tokens: None },
        );
        push_goal_report(&mut chat, &ledger, DoneReason::StepLimit);
        let rendered = chat.render(100);
        let text: String = rendered
            .iter()
            .map(|(_, t)| flashagent_tui::strip_ansi(t))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Goal report"), "{text}");
        assert!(text.contains("INCOMPLETE"), "{text}");
        assert!(text.contains("step budget reached (40 steps)"), "{text}");

        let widths: Vec<usize> = rendered
            .iter()
            .map(|(_, t)| flashagent_tui::visible_width(&flashagent_tui::strip_ansi(t)))
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "card rows must line up with the border: {widths:?}"
        );
    }
}

