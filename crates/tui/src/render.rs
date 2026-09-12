use super::*;

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
    pub(crate) quit_prompt: Option<&'a str>,
    pub(crate) context_warn_threshold: usize,
}

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
        }
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

    pub(crate) fn scroll_up(&mut self, lines: usize, max_scroll: usize) {
        self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);
        self.needs_reprint = true;
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
        effort_menu: Option<&SelectMenu<String>>,
        model_menu: Option<&SelectMenu<String>>,
        settings_view: Option<&SettingsView>,
        sampling_view: Option<&SamplingView>,
        context_modal: Option<&ContextModal>,
        mcp_modal: Option<&McpModal>,
        memory_modal: Option<&flashagent_tui::memory_view::MemoryModal>,
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
                ("target:", flashagent_tui::relative_to_cwd(&path))
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
        } else if let Some(prompt) = st.channel_prompt.or(st.quit_prompt) {
            let title = if st.quit_prompt.is_some() { " Quit FlashAgent " } else { " Switch release channel " };
            // Same treatment as an approval card: this replaces the binary
            // under the user, so it is asked in the same place and with the
            // same weight as anything else that cannot be undone by typing.
            let border_color = "\x1b[38;2;225;175;95m";
            let reset = "\x1b[0m";
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
        } else if let Some(modal) = memory_modal {
            // Morph composer into the memory screen
            tail.extend(modal.render(width));
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
                    let dots = match (st.tick_n / 4) % 3 {
                        0 => ".  ",
                        1 => ".. ",
                        _ => "...",
                    };
                    let what = st.turn_phase.map_or_else(|| "Working on task".to_string(), TurnPhase::label);
                    format!(" {prompt_styled} \x1b[38;2;135;130;125m{what}{dots}\x1b[0m")
                } else if let Some(sug) = st.suggested_prompt {
                    format!(" {prompt_styled}  \x1b[38;2;155;160;175m{sug}\x1b[0m \x1b[38;2;100;105;120m(→ to use)\x1b[0m")
                } else if let Some(custom) = st.custom_placeholder {
                    format!(" {prompt_styled}  \x1b[38;2;135;140;155m{custom}\x1b[0m")
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
        } else if st.quit_prompt.is_some() {
            // The question under the cursor owns the keys; the usual hints
            // would be answering a different question.
            "  \x1b[38;2;135;130;125my / enter — quit · n / esc — stay\x1b[0m".to_string()
        } else if st.channel_prompt.is_some() {
            "  \x1b[38;2;135;130;125my / enter — switch · n / esc — keep the current channel\x1b[0m".to_string()
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
        } else if memory_modal.is_some() {
            "  \x1b[38;2;135;130;125m↑/↓ — select · e — tell the model · d — forget · esc — close\x1b[0m".to_string()
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
                // Below ~68 columns the list is assembled from what fits,
                // so it ends on a word rather than on half a separator.
                let hints = flashagent_tui::fit_parts(
                    &[
                        ("enter — send".into(), "enter — send".into()),
                        ("tab — menu".into(), "tab — menu".into()),
                        ("esc — quit".into(), "esc — quit".into()),
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
        } else {
            let default_tip = "Press Tab or type /settings to configure FlashAgent";
            let tip_content = st.tip.unwrap_or(default_tip);
            let avail1 = width.saturating_sub(8);
            if width >= 30 && tip_content.chars().count() > avail1 {
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
