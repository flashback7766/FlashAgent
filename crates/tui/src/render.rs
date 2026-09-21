use super::*;
use flashagent_tui::anim::{self, Rgb};
use flashagent_tui::theme;

pub(crate) fn color(kind: LineKind) -> crossterm::style::Color {
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

/// Synchronized output: a terminal that knows the mode shows the frame only
/// once all of it has arrived, so a full repaint never flickers through a
/// cleared screen; one that does not ignores it.
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// Print-and-forget renderer: settled lines are
/// printed to the terminal scrollback once and never redrawn; only the live
/// tail (streaming line, open tool, approval card, input) is repainted each
/// frame by cursoring up over it. Native scrollback works for free.
pub(crate) struct Renderer {
    /// Lines of the live tail currently on screen (to erase next frame).
    pub(crate) tail_height: u16,
    pub(crate) prev_width: u16,
    /// How many settled lines have already been printed to the scrollback.
    pub(crate) printed_settled: usize,
    /// Last rendered thinking mode — a flip needs a full repaint (settled
    /// reasoning was printed in the other form and must not linger).
    pub(crate) prev_expansion: Option<ReasoningExpansion>,
    /// Explicit request to clear and repaint all settled lines (e.g. in-place notice update).
    pub(crate) needs_reprint: bool,
    /// Distance in lines from the top of the tail down to the input prompt line.
    pub(crate) prev_cursor_tail_offset: u16,
    /// Vertical scroll offset (lines scrolled back from the bottom). 0 = anchored to bottom.
    pub(crate) scroll_offset: usize,
    /// How far back the last frame could be scrolled: all its lines, settled
    /// and tail, less what fits on screen.
    pub(crate) max_scroll: usize,
    /// What the last frame wrote, so an idle tick that would write the same
    /// bytes again writes nothing.
    last_frame: String,
    /// The card on screen last frame and when it opened.
    card: (CardKey, u64),
}

pub(crate) struct FrameState<'a> {
    pub(crate) input: &'a str,
    pub(crate) mode: PermissionMode,
    pub(crate) is_goal_active: bool,
    /// Live `/goal` progress against its budgets, when one is running.
    pub(crate) goal_progress: Option<&'a str>,
    pub(crate) tip: Option<&'a str>,
    pub(crate) tip_animated: Option<&'a str>,
    pub(crate) tip_lines: Option<&'a [String]>,
    /// Token tracker to dynamically compute turn stats fitting into available space without truncating tips.
    pub(crate) token_tracker: Option<&'a TokenTracker>,
    pub(crate) reasoning_expand: ReasoningExpansion,
    pub(crate) tick_n: usize,
    pub(crate) running: bool,
    /// Seconds since turn start (for the working status line).
    pub(crate) elapsed_secs: u64,
    /// 80 ms steps of wall clock since the turn began — the mascot's face
    /// animates on this rather than on repaints, which are event-driven.
    pub(crate) face_phase: usize,
    /// Number of tokens generated strictly by the model in this turn.
    pub(crate) model_tokens: usize,
    /// Speed of token generation over a rolling 3-second window (tg_3s).
    pub(crate) tokens_per_sec: f64,
    /// Prompt cache reuse ratio (f_keep) from latest turn if reported.
    pub(crate) f_keep: Option<f64>,
    pub(crate) confirm_selection: Decision,
    pub(crate) question_state: Option<&'a QuestionUiState>,
    pub(crate) custom_placeholder: Option<&'a str>,
    pub(crate) suggested_prompt: Option<&'a str>,
    pub(crate) copy_toast: Option<&'a str>,
    pub(crate) prefill_status: Option<&'a str>,
    pub(crate) ttft_display: Option<&'a str>,
    pub(crate) background: Option<&'a str>,
    pub(crate) background_style: NoticeStyle,
    /// What the running turn is doing, for the composer line.
    pub(crate) turn_phase: Option<&'a TurnPhase>,
    pub(crate) attachments: &'a [String],
    /// A release-channel switch waiting for a yes or no.
    pub(crate) channel_prompt: Option<&'a str>,
    /// Title of the yes-or-no card `channel_prompt` fills.
    pub(crate) prompt_title: &'a str,
    pub(crate) context_warn_threshold: usize,
    pub(crate) pending_steers: &'a [String],
    /// Recent generation speeds, oldest first, for the footer sparkline.
    pub(crate) speed_history: &'a [f64],
    /// The composer border lit up by how the last turn ended: the colour,
    /// and how far (0..=1) it has faded back.
    pub(crate) composer_flash: Option<(Rgb, f32)>,
}

/// Which card stands in for the composer, to know when a new one opens and
/// should unfold.
#[derive(Clone, Copy, PartialEq)]
enum CardKey {
    Composer,
    Approval,
    Channel,
    Question,
    Overlay(std::mem::Discriminant<Overlay>),
}

/// How long a card takes to unfold.
const UNFOLD_MS: u64 = 180;
/// The resting colour of box borders.
const BORDER: Rgb = Rgb(95, 90, 85);
const BORDER_ESC: &str = "\x1b[38;2;95;90;85m";

impl Renderer {
    pub(crate) fn new() -> Self {
        Self {
            tail_height: 0,
            prev_width: 0,
            printed_settled: 0,
            prev_expansion: None,
            needs_reprint: false,
            prev_cursor_tail_offset: 0,
            scroll_offset: 0,
            max_scroll: 0,
            last_frame: String::new(),
            card: (CardKey::Composer, 0),
        }
    }

    /// Whether a card is still unfolding, so frames should come faster than
    /// the idle tick.
    pub(crate) fn animating(&self) -> bool {
        self.card.0 != CardKey::Composer
            && anim::enabled()
            && anim::now_ms().saturating_sub(self.card.1) < UNFOLD_MS + 40
    }

    pub(crate) fn request_reprint(&mut self) {
        self.needs_reprint = true;
    }

    pub(crate) fn clear_tail(&mut self) {
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

    pub(crate) fn scroll_up(&mut self, lines: usize) {
        let old = self.scroll_offset;
        self.scroll_offset = (self.scroll_offset + lines).min(self.max_scroll);
        if old != self.scroll_offset {
            self.needs_reprint = true;
        }
    }

    /// Scroll back to the first line.
    pub(crate) fn scroll_to_top(&mut self) {
        self.scroll_up(self.max_scroll);
    }

    pub(crate) fn scroll_down(&mut self, lines: usize) {
        let old = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if old != self.scroll_offset {
            self.needs_reprint = true;
        }
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset = 0;
            self.needs_reprint = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn format_status_left(
    running: bool,
    is_goal_active: bool,
    awaiting_user: bool,
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
        let activity = match (awaiting_user, goal_progress) {
            // A turn stopped at an approval card is not generating anything;
            // it is waiting for the person who has to answer it.
            (true, _) => "\x1b[38;2;225;175;95mWaiting for your answer\x1b[0m".to_string(),
            (false, Some(p)) => format!("\x1b[38;2;168;199;250m{p}\x1b[0m"),
            (false, None) => "\x1b[38;2;168;199;250mGenerating response...\x1b[0m".to_string(),
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
    pub(crate) fn frame(
        &mut self,
        chat: &ChatView,
        gate: &TuiGate,
        question_gate: &TuiQuestionGate,
        overlay: Option<&Overlay>,
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

        if !st.pending_steers.is_empty() {
            for steer in st.pending_steers {
                let suffix = " \x1b[38;2;135;130;125m· steer queued\x1b[0m";
                let avail = width.saturating_sub(18).max(10);
                let wrapped = wrap_plain(steer, avail);
                for (j, chunk) in wrapped.into_iter().enumerate() {
                    let text = if j == 0 {
                        format!(" \x1b[1;38;2;225;175;95m❯\x1b[0m \x1b[1;38;2;240;235;225m{chunk}\x1b[0m{suffix}")
                    } else {
                        format!("   \x1b[1;38;2;240;235;225m{chunk}\x1b[0m")
                    };
                    tail.push((LineKind::User, text));
                }
            }
        }

        let inner_w = width.saturating_sub(2);
        let t = anim::now_ms();
        let card_key = if gate.pending().is_some() {
            CardKey::Approval
        } else if st.channel_prompt.is_some() {
            CardKey::Channel
        } else if question_gate.pending().is_some() {
            CardKey::Question
        } else if let Some(overlay) = overlay {
            CardKey::Overlay(std::mem::discriminant(overlay))
        } else {
            CardKey::Composer
        };
        // A card waiting on the user breathes amber; the composer lights up
        // for a moment in the colour of how the last turn ended.
        let border_rgb = match card_key {
            CardKey::Approval | CardKey::Question => anim::pulse(t, 1800, BORDER, Rgb(225, 175, 95)),
            CardKey::Composer => st.composer_flash.map_or(BORDER, |(lit, faded)| lit.mix(BORDER, faded)),
            _ => BORDER,
        };
        let border_color = border_rgb.fg();
        let reset = "\x1b[0m";
        let card_start = tail.len();

        let mut input_line_idx;
        let mut custom_cursor_col: Option<u16> = None;

        if let Some(req) = gate.pending() {
            // The composer becomes the approval card.
            let tool_styled = format!("\x1b[1;38;2;225;175;95m{}\x1b[0m", req.tool);
            let title = format!(" Confirm: {tool_styled} ");
            let vis_title_len = visible_width(&title);
            let dash_w = inner_w.saturating_sub(vis_title_len + 1);

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
            let (label, value): (&str, String) = if let Some(cmd) = field("command") {
                ("command:", cmd)
            } else if let Some(path) = field("path") {
                ("target:", flashagent_tui::relative_to_cwd(&path))
            } else if let Some(files) = args.get("files").and_then(|f| f.as_array()) {
                if let Some(first_path) = files.first().and_then(|f| f.get("path").or_else(|| f.get("filePath"))).and_then(|p| p.as_str()) {
                    let extra = if files.len() > 1 { format!(" (+{} more)", files.len() - 1) } else { String::new() };
                    ("target:", format!("{}{extra}", flashagent_tui::relative_to_cwd(first_path)))
                } else if args.as_object().is_some_and(|o| !o.is_empty()) {
                    ("args:", card_safe(&args.to_string()))
                } else {
                    ("", String::new())
                }
            } else if args.as_object().is_some_and(|o| !o.is_empty()) {
                ("args:", card_safe(&args.to_string()))
            } else {
                ("", String::new())
            };
            for (i, row) in card_rows(&value, value_w, 6).iter().enumerate() {
                tail.push((LineKind::System, label_row(if i == 0 { label } else { "        " }, row)));
            }

            if let Some(ref diff) = req.diff {
                // The file is already named on the target row; the six rows
                // of preview are for the change itself.
                for line in diff.lines().filter(|l| !l.starts_with("--- ") && !l.starts_with("+++ ")).take(6) {
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
            let title = format!(" {} ", st.prompt_title);
            let title = title.as_str();
            // Same treatment as an approval card: this replaces the binary
            // under the user, so it is asked in the same place and with the
            // same weight as anything else that cannot be undone by typing.
            let border_color = anim::pulse(t, 1800, Rgb(225, 175, 95), Rgb(250, 215, 150)).fg();
            let reset = "\x1b[0m";
            let dash_w = inner_w.saturating_sub(visible_width(title) + 1);
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
            // The composer becomes the question card.
            let title = " Question from FlashAgent ";
            let vis_title_len = visible_width(title);
            let dash_w = inner_w.saturating_sub(vis_title_len + 1);

            tail.push((
                LineKind::System,
                format!("{border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
            ));

            let q_clipped = clip_ansi(&req.question, inner_w.saturating_sub(4));
            tail.push((
                LineKind::System,
                pad_box_row(&format!(" \x1b[1;38;2;240;235;225m{q_clipped}\x1b[0m"), width),
            ));
            if let Some(deadline) = req.deadline {
                // During /goal the run will not wait forever; say how long.
                let left = deadline.saturating_duration_since(std::time::Instant::now()).as_secs();
                tail.push((
                    LineKind::System,
                    pad_box_row(
                        &format!(
                            " \x1b[38;2;225;175;95mNo answer in {}:{:02} and the run carries on without one\x1b[0m",
                            left / 60,
                            left % 60
                        ),
                        width,
                    ),
                ));
            }

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
        } else if let Some(overlay) = overlay {
            // The composer turns into the open menu or screen.
            tail.extend(overlay.render(width));
            input_line_idx = tail.len().saturating_sub(1);
            custom_cursor_col = Some(0);
        } else {
            // Standard input box
            tail.push((
                LineKind::System,
                format!("{border_color}╭{}╮{reset}", "─".repeat(inner_w)),
            ));

            // What is going with the message, above the line it goes with.
            if !st.attachments.is_empty() {
                let listed = st.attachments.join("  ");
                let row = format!(
                    " \x1b[38;2;145;205;140mattached\x1b[0m \x1b[38;2;160;165;180m{listed}\x1b[0m \x1b[38;2;100;95;90m· ctrl+z removes\x1b[0m"
                );
                tail.push((LineKind::System, pad_box_row(&row, width)));
            }

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
                    let what = st.turn_phase.map_or_else(|| "Working on task".to_string(), TurnPhase::label);
                    let glow = if matches!(st.turn_phase, Some(TurnPhase::Stopping)) { Rgb(235, 150, 120) } else { Rgb(235, 225, 205) };
                    format!(" {prompt_styled} {}", anim::shimmer(&format!("{what}…"), t, 2000, Rgb(135, 130, 125), glow))
                } else if let Some(sug) = st.suggested_prompt {
                    format!(" {prompt_styled}  \x1b[38;2;155;160;175m{sug}\x1b[0m \x1b[38;2;100;105;120m(→ to use)\x1b[0m")
                } else if let Some(custom) = st.custom_placeholder {
                    format!(" {prompt_styled}  \x1b[38;2;135;140;155m{custom}\x1b[0m")
                } else if !st.attachments.is_empty() {
                    let prompt_text = if width >= 60 {
                        "Press Enter to send image, or type a message..."
                    } else if width >= 40 {
                        "Enter to send image..."
                    } else {
                        "Enter to send..."
                    };
                    format!(" {prompt_styled}  \x1b[38;2;135;130;125m{prompt_text}\x1b[0m")
                } else {
                    // The long form is friendlier; the short one is what fits.
                    let prompt_text = if width >= 60 {
                        "Ask FlashAgent to do anything..."
                    } else if width >= 40 {
                        "Ask FlashAgent..."
                    } else {
                        "Ask..."
                    };
                    format!(" {prompt_styled}  \x1b[38;2;135;130;125m{prompt_text}\x1b[0m")
                }
            } else {
                // Borders, " ❯ " and a cell for the cursor after the text.
                let (shown, cells) = flashagent_tui::tail_window(st.input, width.saturating_sub(6));
                custom_cursor_col = Some((cells + 4).min(width.saturating_sub(2)) as u16);
                format!(" {prompt_styled} {shown}")
            };
            tail.push((LineKind::User, pad_box_row(&input_row_content, width)));

            // Bottom border of input box
            tail.push((
                LineKind::System,
                format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
            ));
        }

        // The sides of a boxed card take the same colour as its top and bottom.
        if border_rgb != BORDER && !matches!(card_key, CardKey::Overlay(_)) {
            for (_, row) in tail[card_start..].iter_mut() {
                *row = row.replace(BORDER_ESC, &border_color);
            }
        }
        // A card that just opened unfolds from its title down: its first rows,
        // then its bottom edge, until all of it is there.
        if self.card.0 != card_key {
            self.card = (card_key, t);
        }
        let total = tail.len() - card_start;
        if card_key != CardKey::Composer && total > 2 {
            let k = anim::progress(self.card.1, t, UNFOLD_MS);
            let shown = ((total as f32 * k).ceil() as usize).clamp(2, total);
            if shown < total {
                let bottom = tail.len() - 1;
                tail.drain(card_start + shown - 1..bottom);
                input_line_idx = input_line_idx.min(tail.len() - 1);
                custom_cursor_col = Some(0);
            }
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
        } else if question_gate.pending().is_some() {
            // The card lists its own keys, and they differ between a choice
            // and a written answer.
            st.background.map(|text| format!("  {}", st.background_style.paint(text))).unwrap_or_default()
        } else if st.channel_prompt.is_some() && st.prompt_title == UNINSTALL_TITLE {
            "  \x1b[38;2;135;130;125my / enter — close and uninstall · n / esc — keep FlashAgent\x1b[0m".to_string()
        } else if st.channel_prompt.is_some() {
            "  \x1b[38;2;135;130;125my / enter — switch · n / esc — keep the current channel\x1b[0m".to_string()
        } else if overlay.is_some_and(Overlay::has_own_hints) {
            // These draw their own keys inside the box; saying them twice
            // only pushes the box up. Something that turned up on its own
            // still gets the line.
            st.background.map(|text| format!("  {}", st.background_style.paint(text))).unwrap_or_default()
        } else if overlay.is_some() {
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
            // How the speed has moved over the last few seconds.
            let spark = if st.speed_history.len() >= 2 && st.speed_history.iter().any(|v| *v > 0.0) {
                format!(" \x1b[38;2;138;180;248m{}\x1b[0m", anim::sparkline(st.speed_history))
            } else {
                String::new()
            };
            let live = format!(
                "  {} \x1b[38;2;168;199;250mTokens - \x1b[1;38;2;235;240;250m{}\x1b[0m \x1b[38;2;194;231;255m({:.1}/s)\x1b[0m{spark}{cache_str}{ttft_str} \x1b[38;2;75;99;130m·\x1b[0m \x1b[38;2;155;165;180m{}s\x1b[0m",
                anim::spinner(t),
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
                // The longest hint that fits, never one cut mid-word.
                None => [tail_hint, "\x1b[38;2;135;130;125m· esc to interrupt\x1b[0m", ""]
                    .into_iter()
                    .map(|hint| if hint.is_empty() { live.clone() } else { format!("{live} {hint}") })
                    .find(|line| visible_width(line) <= width)
                    .unwrap_or_else(|| live.clone()),
            }
        } else if let Some(text) = st.background {
            format!("  {}", st.background_style.paint(text))
        } else {
            // The longest list of shortcuts that fits: measured, since fixed
            // column thresholds let the longer lists clip mid-word.
            const FOOTERS: [&str; 4] = [
                "enter — send · tab — settings · f1 — context · f2 — verbose · f3 — model · f4 — effort · f5 — sampling · ctrl+r — regen · ctrl+c/v · esc esc — quit",
                "enter — send · tab — settings · f1 — context · f3 — model · f4 — effort · f5 — sampling · ctrl+r — regen · esc esc — quit",
                "enter — send · tab — settings · f3 — model · f4 — effort · f5 — sampling · esc esc — quit",
                "enter — send · tab — settings · f3 — model · esc esc — quit",
            ];
            if let Some(fits) = FOOTERS.iter().find(|f| visible_width(f) + 2 <= width) {
                format!("  \x1b[38;2;135;130;125m{fits}\x1b[0m")
            } else {
                // Below ~68 columns the list is assembled from what fits,
                // so it ends on a word rather than on half a separator.
                let hints = flashagent_tui::fit_parts(
                    &[
                        ("enter — send".into(), "enter — send".into()),
                        ("tab — menu".into(), "tab — menu".into()),
                        ("esc esc — quit".into(), "esc esc — quit".into()),
                    ],
                    " · ",
                    width.saturating_sub(2),
                );
                format!("  \x1b[38;2;135;130;125m{hints}\x1b[0m")
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
        } else if let Some(tip_content) = st.tip {
            let avail1 = width.saturating_sub(8);
            // A second row for the tip is a luxury of a tall window. In a
            // short one those rows belong to the conversation.
            let room_for_two = height >= 20;
            if room_for_two && width >= 30 && tip_content.chars().count() > avail1 {
                let (l1, l2) = flashagent_tui::tips::split_tip_at_word_boundary(tip_content, avail1);
                let row1 = format!("  \x1b[1;38;2;225;175;95mTip:\x1b[0m \x1b[38;2;175;170;160m{l1}\x1b[0m");
                // The split gives two lines; a tip that needs three loses the
                // rest, so end the second one on a word rather than inside it.
                let room = width.saturating_sub(9);
                let l2 = if l2.chars().count() > room {
                    let head: String = l2.chars().take(room.saturating_sub(1)).collect();
                    let cut = match head.rfind(' ') {
                        Some(i) if i >= room / 3 => head[..i].to_string(),
                        _ => head,
                    };
                    format!("{cut}\u{2026}")
                } else {
                    l2.to_string()
                };
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

        let expand_status = if st.reasoning_expand.all {
            " \x1b[38;2;100;95;90m·\x1b[0m \x1b[38;2;175;170;225m[verbose: all]\x1b[0m"
        } else if st.reasoning_expand.last {
            " \x1b[38;2;100;95;90m·\x1b[0m \x1b[38;2;175;170;225m[verbose: last]\x1b[0m"
        } else {
            ""
        };
        // The turn stats give way to the verbose tag rather than clipping it.
        let budget = width.saturating_sub(gauge_vis + 4 + visible_width(expand_status));

        let left_telemetry = format_status_left(
            st.running,
            st.is_goal_active,
            gate.pending().is_some() || question_gate.pending().is_some(),
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

        let view_h = (height as usize).saturating_sub(2).max(5);
        self.max_scroll = (settled.len() + tail.len()).saturating_sub(view_h);
        self.scroll_offset = self.scroll_offset.min(self.max_scroll);

        // When scroll_offset > 0, render shifted history view
        if self.scroll_offset > 0 {
            let mut all_lines: Vec<RenderLine> = settled.clone();
            all_lines.extend(tail.clone());
            let total_len = all_lines.len();
            let end = total_len.saturating_sub(self.scroll_offset);
            let start = end.saturating_sub(view_h);
            let visible_slice = &all_lines[start..end];

            let mut out = String::from(SYNC_BEGIN);
            out.push_str("\x1b[H\x1b[2J");
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
            out.push_str(SYNC_END);
            if !reprint_all && out == self.last_frame {
                return;
            }
            self.last_frame.clone_from(&out);
            crossterm::queue!(
                std::io::stdout(),
                crossterm::cursor::MoveToColumn(0),
                crossterm::style::Print(theme::recolor(&out)),
            )
            .ok();
            std::io::stdout().flush().ok();
            return;
        }

        let mut out = String::from(SYNC_BEGIN);
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
        let prints_settled = reprint_all || settled.len() > self.printed_settled;
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
        // Park the cursor on the input line; the frame ends there.
        if lift_up > 0 {
            out.push_str(&format!("\x1b[{lift_up}A"));
        }
        out.push_str(&format!("\x1b[{}G", cursor_col + 1));
        out.push_str(SYNC_END);
        if !prints_settled && out == self.last_frame {
            return;
        }
        self.last_frame.clone_from(&out);
        crossterm::queue!(
            std::io::stdout(),
            crossterm::cursor::MoveToColumn(0),
            crossterm::style::Print(theme::recolor(&out)),
        )
        .ok();
        std::io::stdout().flush().ok();
    }
}

impl App {
    /// Paint one frame of the app as it stands.
    pub(crate) fn draw(&mut self, cx: &LoopCtx<'_>, autocomplete: Option<&AutocompletePopup>) {
        anim::set_enabled(self.config.animations);
        // Taken from the config on every frame, so that leaving the settings
        // screen without saving puts the old theme back by itself.
        theme::set(self.config.color_theme);
        let composer_flash = self.composer_flash();
        let channel_prompt: Option<String> = if self.uninstall_confirm {
            Some(
                "Close FlashAgent and remove it? The uninstaller then shows what it will remove and asks, \
                 part by part, which of your data to delete. Your session is saved first."
                    .to_string(),
            )
        } else {
            self.channel_switch.as_ref().map(|sw| {
                channel_switch_warning(sw.to, flashagent_svc::updater::current_version(), &sw.target)
            })
        };
        let cost_now = self.image_costs.get(&self.current_model);
        let attachment_labels: Vec<String> = self.attachments.iter().map(|a| a.labelled(cost_now)).collect();
        let goal_progress: Option<String> = self.goal_ledger.as_ref().filter(|_| self.running).map(|l| l.progress());
        let live_prefill = self.token_tracker.live_prefill_status();
        let ttft_display = self.token_tracker.ttft_display();
        let tg_speed = self.token_tracker.tg_3s();
        let config = &self.config;
        let turn_clock = |ms_per_step: u128| self.turn_started.map(|t| (t.elapsed().as_millis() / ms_per_step) as usize).unwrap_or(0);

        self.renderer.frame(
            &self.chat,
            cx.gate,
            cx.question_gate,
            self.overlay.as_ref(),
            autocomplete,
            &self.context_usage,
            FrameState {
                input: &self.input,
                mode: cx.perm.state().mode(),
                is_goal_active: self.goal_state.is_some(),
                goal_progress: goal_progress.as_deref(),
                tip: config.show_tips.then_some(self.tip_animator.tip_text),
                tip_animated: None,
                tip_lines: config.show_tips.then_some(cx.tip_lines.as_slice()),
                token_tracker: config.show_tokens.then_some(&self.token_tracker),
                reasoning_expand: ReasoningExpansion { all: self.all_expanded, last: self.last_expanded },
                tick_n: self.tick_n,
                running: self.running,
                elapsed_secs: turn_clock(1000) as u64,
                face_phase: turn_clock(80),
                model_tokens: if config.show_tokens { self.token_tracker.total_model_tokens } else { 0 },
                tokens_per_sec: if config.show_tokens { tg_speed } else { 0.0 },
                f_keep: if config.show_tokens { self.token_tracker.last_f_keep } else { None },
                confirm_selection: self.confirm_select.decision(),
                question_state: Some(&self.question_ui_state),
                custom_placeholder: self.custom_placeholder.as_deref(),
                suggested_prompt: self.suggested_prompt.as_deref(),
                copy_toast: if config.show_toasts { self.copy_toast.as_ref().map(|(msg, _)| msg.as_str()) } else { None },
                prefill_status: if config.show_ttft { live_prefill.as_deref() } else { None },
                ttft_display: if config.show_ttft { ttft_display.as_deref() } else { None },
                background: self.background.as_ref().map(|b| b.text.as_str()),
                channel_prompt: channel_prompt.as_deref(),
                prompt_title: if self.uninstall_confirm { UNINSTALL_TITLE } else { "Switch release channel" },
                turn_phase: self.running.then_some(&self.turn_phase),
                attachments: &attachment_labels,
                background_style: self.background.as_ref().map_or(NoticeStyle::FULL, BackgroundNotice::style),
                context_warn_threshold: config.context_warn_threshold,
                pending_steers: &self.pending_steers,
                speed_history: &self.speed_history,
                composer_flash,
            },
        );
    }

    /// Whether frames should come faster than the idle tick right now.
    pub(crate) fn animating(&self) -> bool {
        self.renderer.animating() || self.composer_flash().is_some()
    }

    /// The composer border's flash for the turn that just ended, while it
    /// lasts.
    fn composer_flash(&self) -> Option<(Rgb, f32)> {
        const FLASH_MS: u64 = 1400;
        let (colour, started) = self.turn_flash?;
        let faded = anim::progress(started, anim::now_ms(), FLASH_MS);
        (faded < 1.0).then_some((colour, faded))
    }

    /// Light the composer border in the colour of how a turn ended.
    pub(crate) fn flash_turn_end(&mut self, reason: Option<DoneReason>) {
        let colour = match reason {
            Some(DoneReason::Completed) => Rgb(120, 220, 140),
            None | Some(DoneReason::Failed) => Rgb(230, 110, 95),
            Some(_) => Rgb(225, 175, 95),
        };
        self.turn_flash = Some((colour, anim::now_ms()));
    }

    /// Take a speed sample for the footer sparkline, a few times a second.
    pub(crate) fn sample_speed(&mut self) {
        const EVERY_MS: u64 = 250;
        const KEEP: usize = 24;
        let t = anim::now_ms();
        if !self.running || t.saturating_sub(self.last_speed_sample) < EVERY_MS {
            return;
        }
        self.last_speed_sample = t;
        self.speed_history.push(self.token_tracker.tg_3s());
        if self.speed_history.len() > KEEP {
            self.speed_history.remove(0);
        }
    }
}
