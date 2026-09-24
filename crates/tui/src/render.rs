use super::*;
use flashagent_tui::anim::{self, Rgb};
use flashagent_tui::theme;

pub(crate) fn color(kind: LineKind) -> crossterm::style::Color {
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

/// The screen is one list of rows: the end of the transcript, then the live tail
/// (streaming line, open tool, cards, input, footer). `flashagent_tui::screen`
/// writes only the rows that changed, in place, so nothing flickers however
/// often a frame comes.
pub(crate) struct Renderer {
    /// Lines scrolled back from the bottom; 0 is anchored.
    pub(crate) scroll_offset: usize,
    /// All lines, settled and tail, less what fits on screen.
    pub(crate) max_scroll: usize,
    /// Lines last frame, so a view scrolled back stays on what the user reads
    /// while new lines arrive under it.
    total_lines: usize,
    /// The conversation rows on screen: the first one's index (settled, then live)
    /// and how many, from the top of the screen. For clicks.
    chat_view: (usize, usize),
    screen: flashagent_tui::screen::Screen,
    /// The card on screen last frame and when it opened.
    card: (CardKey, u64),
}

pub(crate) struct FrameState<'a> {
    pub(crate) input: &'a flashagent_tui::Composer,
    /// The provider in use and its model, at the end of the status line: the
    /// first to go when it is narrow.
    pub(crate) provider: (&'a str, &'a str),
    /// Ctrl+F: the query and the prompt it found.
    pub(crate) history_search: Option<(&'a str, Option<&'a str>)>,
    pub(crate) mode: PermissionMode,
    pub(crate) is_goal_active: bool,
    pub(crate) goal_progress: Option<&'a str>,
    pub(crate) tip: Option<&'a str>,
    pub(crate) tip_animated: Option<&'a str>,
    pub(crate) tip_lines: Option<&'a [String]>,
    pub(crate) reasoning_expand: ReasoningExpansion,
    pub(crate) tick_n: usize,
    pub(crate) running: bool,
    /// Generation speed over the last 3 seconds; `None` when counters are off.
    pub(crate) tokens_per_sec: Option<f64>,
    /// `draft 81%`: how much of a draft model's guessing the model kept.
    pub(crate) draft_acceptance: Option<&'a str>,
    pub(crate) confirm_selection: ConfirmChoice,
    pub(crate) question_state: Option<&'a QuestionUiState>,
    pub(crate) custom_placeholder: Option<&'a str>,
    pub(crate) suggested_prompt: Option<&'a str>,
    pub(crate) copy_toast: Option<&'a str>,
    pub(crate) prefill_status: Option<&'a str>,
    pub(crate) ttft_display: Option<&'a str>,
    pub(crate) background: Option<&'a str>,
    pub(crate) background_style: NoticeStyle,
    pub(crate) turn_phase: Option<&'a TurnPhase>,
    pub(crate) attachments: &'a [String],
    pub(crate) channel_prompt: Option<&'a str>,
    /// Title of the card `channel_prompt` fills.
    pub(crate) prompt_title: &'a str,
    pub(crate) context_warn_threshold: usize,
    pub(crate) pending_steers: &'a [String],
    /// Background commands still running.
    pub(crate) background_tasks: usize,
    /// A foreground shell command Ctrl+B can move to the background.
    pub(crate) shell_running: bool,
    /// The colour of how the last turn ended, and how far (0..=1) it has faded.
    pub(crate) composer_flash: Option<(Rgb, f32)>,
}

/// To know when a new card opens and should unfold.
#[derive(Clone, Copy, PartialEq)]
enum CardKey {
    Composer,
    Approval,
    Channel,
    Question,
    Overlay(std::mem::Discriminant<Overlay>),
}

const UNFOLD_MS: u64 = 180;
const BORDER: Rgb = Rgb(95, 90, 85);
const BORDER_ESC: &str = "\x1b[38;2;95;90;85m";

impl Renderer {
    pub(crate) fn new() -> Self {
        Self {
            scroll_offset: 0,
            max_scroll: 0,
            total_lines: 0,
            chat_view: (0, 0),
            screen: flashagent_tui::screen::Screen::new(),
            card: (CardKey::Composer, 0),
        }
    }

    /// While true, frames come faster than the idle tick.
    pub(crate) fn animating(&self) -> bool {
        self.card.0 != CardKey::Composer
            && anim::enabled()
            && anim::now_ms().saturating_sub(self.card.1) < UNFOLD_MS + 40
    }

    /// Rows that did not change are skipped, so this is needed only when
    /// something else drew on the screen (an editor, a full-screen view) or
    /// cleared it: the next frame writes every row again.
    pub(crate) fn request_reprint(&mut self) {
        self.screen.invalidate();
    }

    pub(crate) fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = (self.scroll_offset + lines).min(self.max_scroll);
    }

    pub(crate) fn scroll_to_top(&mut self) {
        self.scroll_up(self.max_scroll);
    }

    pub(crate) fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// The conversation row drawn on screen row `y`, if one is.
    pub(crate) fn chat_row_at(&self, y: u16) -> Option<usize> {
        let (first, count) = self.chat_view;
        ((y as usize) < count).then_some(first + y as usize)
    }
}

/// On the status line while background commands run; a narrow window drops it whole.
fn tasks_status(running: usize) -> String {
    if running == 0 {
        return String::new();
    }
    format!(
        " \x1b[38;2;100;95;90m\u{b7}\x1b[0m \x1b[38;2;168;199;250m\u{25cf} {}\x1b[0m",
        flashagent_tui::plural(running, "task", "tasks")
    )
}

/// The mode, and what the turn waits on when that is not the model: the phase
/// itself is written in the composer, the speed on the line above.
pub(crate) fn format_status_left(
    running: bool,
    is_goal_active: bool,
    awaiting_user: bool,
    goal_progress: Option<&str>,
    mode_label: &str,
    expand_status: &str,
) -> String {
    let mode_str = if is_goal_active {
        "\x1b[1;38;2;225;175;95m[Goal: Autonomous]\x1b[0m".to_string()
    } else {
        format!("\x1b[38;2;145;205;140m[{mode_label}]\x1b[0m")
    };
    let activity = match (running, awaiting_user, goal_progress) {
        // Stopped at an approval card: waiting for the user, not generating.
        (true, true, _) => "\x1b[38;2;225;175;95mWaiting for your answer\x1b[0m".to_string(),
        // During a goal the budget burn-down.
        (true, false, Some(p)) => format!("\x1b[38;2;168;199;250m{p}\x1b[0m"),
        (true, false, None) => return format!("  {mode_str}{expand_status}"),
        (false, ..) => "\x1b[38;2;140;135;130mReady\x1b[0m".to_string(),
    };
    format!("  {mode_str} \x1b[38;2;100;95;90m·\x1b[0m {activity}{expand_status}")
}

/// " · Anthropic · claude-opus-5-5", in parts the status line drops from
/// the end when it is narrow: the model first, then the provider.
pub(crate) fn format_provider(provider: &str, model: &str) -> String {
    let dot = " \x1b[38;2;100;95;90m\u{b7}\x1b[0m ";
    let mut out = String::new();
    if !provider.is_empty() {
        out.push_str(&format!("{dot}\x1b[38;2;140;135;130m{provider}\x1b[0m"));
    }
    if !model.is_empty() {
        out.push_str(&format!("{dot}\x1b[38;2;175;170;160m{model}\x1b[0m"));
    }
    out
}

struct FooterVisibility {
    approval: bool,
    question: bool,
    overlay: bool,
    autocomplete: bool,
}

fn footer_hint(chat: &ChatView, shown: FooterVisibility, st: &FrameState<'_>, width: usize, t: u64) -> String {
    let left_hint = if let Some(toast) = st.copy_toast {
        format!("  \x1b[1;38;2;135;215;165m{toast}\x1b[0m")
    } else if st.history_search.is_some() {
        format!("  {}", key_hints(&[("Ctrl+F", "older"), ("Enter", "use"), ("Esc", "cancel")], width.saturating_sub(2)))
    } else if shown.approval {
        let hints = [("Enter", "confirm"), ("←/→", "choose"), ("a", "always"), ("Esc", "deny")];
        format!("  {}", flashagent_tui::key_hints(&hints, width.saturating_sub(2)))
    } else if shown.question {
        // The card lists its own keys, which differ for a choice and a written answer.
        st.background.map(|text| format!("  {}", st.background_style.paint(text))).unwrap_or_default()
    } else if st.channel_prompt.is_some() && st.prompt_title == UNINSTALL_TITLE {
        format!("  {}", key_hints(&[("y/Enter", "close and uninstall"), ("n/Esc", "keep FlashAgent")], width.saturating_sub(2)))
    } else if st.channel_prompt.is_some() {
        format!("  {}", key_hints(&[("y/Enter", "switch"), ("n/Esc", "keep the current channel")], width.saturating_sub(2)))
    } else if shown.overlay {
        // Panels draw their own keys inside their box; only an unprompted notice
        // still gets the line.
        st.background.map(|text| format!("  {}", st.background_style.paint(text))).unwrap_or_default()
    } else if shown.autocomplete {
        let hints = [("Tab", "complete"), ("↑/↓", "select"), ("Enter", "run"), ("Esc", "close")];
        format!("  {}", key_hints(&hints, width.saturating_sub(2)))
    } else if st.running {
        // The spinner says it is alive; then only how fast it writes and how fast it
        // read the prompt. A speed of 0 before the first token reads as a stall, so
        // it waits for one.
        let dot = " \x1b[38;2;75;99;130m·\x1b[0m ";
        let mut live = format!("  {}", anim::spinner(t));
        if let Some(speed) = st.tokens_per_sec.filter(|s| *s > 0.0) {
            live.push_str(&format!(" \x1b[38;2;194;231;255m{speed:.1} t/s\x1b[0m"));
        }
        if let Some(draft) = st.draft_acceptance {
            live.push_str(&format!("{dot}{draft}"));
        }
        if let Some(prefill) = st.ttft_display {
            live.push_str(&format!("{dot}{prefill}"));
        }
        // The counters show the model is alive and the notice must not be missed;
        // the key hints give way when both do not fit.
        let esc_does = if st.input.is_empty() { "interrupt" } else { "clear" };
        let esc = key_hints(&[("Esc", esc_does)], width);
        let steer = key_hints(&[("Enter", "steer"), ("Esc", esc_does)], width);
        let head = match st.background {
            Some(text) => format!("{live}{dot}{}", st.background_style.paint(text)),
            None => live.clone(),
        };
        let detach = key_hints(&[("Ctrl+B", "background"), ("Esc", esc_does)], width);
        let detach_steer = key_hints(&[("Ctrl+B", "background"), ("Enter", "steer"), ("Esc", esc_does)], width);
        let hints: Vec<&str> = match (st.shell_running, st.background.is_some()) {
            (true, true) => vec![&detach, &esc, ""],
            (true, false) => vec![&detach_steer, &detach, &esc, ""],
            (false, true) => vec![&esc, ""],
            (false, false) => vec![&steer, &esc, ""],
        };
        // The longest hint that fits, never cut mid-word.
        hints
            .into_iter()
            .map(|hint| if hint.is_empty() { head.clone() } else { format!("{head}{dot}{hint}") })
            .find(|line| visible_width(line) <= width)
            .unwrap_or(head)
    } else if let Some(text) = st.background {
        format!("  {}", st.background_style.paint(text))
    } else if !st.input.is_empty() {
        let hints = [("Enter", "send"), ("Alt+Enter", "new line"), ("Ctrl+W", "delete word"), ("Ctrl+F", "history"), ("Esc", "clear")];
        format!("  {}", key_hints(&hints, width.saturating_sub(2)))
    } else if chat.has_user_message() {
        // The welcome card lists the keys; once it has scrolled away, the one key
        // that finds every other.
        format!("  {}", key_hints(&[("Ctrl+K", "commands"), ("/help", ""), ("Ctrl+D", "quit")], width.saturating_sub(2)))
    } else {
        String::new()
    };
    left_hint
}

fn append_tip_rows(tail: &mut Vec<RenderLine>, st: &FrameState<'_>, width: usize, height: u16) {
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
        // A second tip row only in a tall window.
        let room_for_two = height >= 20;
        if room_for_two && width >= 30 && tip_content.chars().count() > avail1 {
            let (l1, l2) = flashagent_tui::tips::split_tip_at_word_boundary(tip_content, avail1);
            let row1 = format!("  \x1b[1;38;2;225;175;95mTip:\x1b[0m \x1b[38;2;175;170;160m{l1}\x1b[0m");
            // A tip needing three lines ends the second on a word.
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
}

fn footer_status(width: usize, context_usage: &ContextUsage, st: &FrameState<'_>, awaiting_user: bool) -> String {
    let mode_room = 2 + visible_width(st.mode.label()) + 2 + 1;
    let percent = context_usage.percentage().clamp(0.0, 100.0);
    // Nearly full, it says what to do about it instead of drawing the bar.
    let gauges = if st.context_warn_threshold > 0 && percent >= st.context_warn_threshold as f32 {
        let amber = |s: String| format!("\x1b[1;38;2;245;160;80m{s}\x1b[0m");
        [
            amber(format!("context {percent:.0}% full \u{b7} /compact frees it")),
            amber(format!("{percent:.0}% full \u{b7} /compact")),
            amber(format!("{percent:.0}%")),
        ]
    } else {
        [
            context_usage.format_compact_gauge(10),
            context_usage.format_compact_gauge(4),
            format!("\x1b[38;2;135;215;165m{percent:.0}%\x1b[0m"),
        ]
    };
    let gauge_str = gauges.into_iter().find(|g| mode_room + visible_width(g) + 3 <= width).unwrap_or_default();
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
        awaiting_user,
        st.goal_progress,
        st.mode.label(),
        &format!("{}{expand_status}", tasks_status(st.background_tasks)),
    );
    fit_status_row(width, &left_telemetry, &format_provider(st.provider.0, st.provider.1), &gauge_str)
}

fn fit_status_row(width: usize, left: &str, provider: &str, gauge_str: &str) -> String {
    let left = format!("{left}{provider}");
    let gauge_vis = visible_width(gauge_str);
    let left_vis = visible_width(&left);
    let status_row = if left_vis + gauge_vis + 3 <= width {
        let pad = " ".repeat(width.saturating_sub(left_vis + gauge_vis + 1));
        format!("{left}{pad}{gauge_str}")
    } else {
        // Whole parts go first, from the end; only a lone remaining part is clipped.
        let clip_budget = width.saturating_sub(gauge_vis + 3);
        let mut left = left.clone();
        while visible_width(&left) > clip_budget {
            match left.rfind(" \x1b[38;2;100;95;90m·") {
                Some(cut) => left = format!("{}\x1b[0m", &left[..cut]),
                None => break,
            }
        }
        let clipped_left = clip_ansi(&left, clip_budget);
        let pad = " ".repeat(width.saturating_sub(visible_width(&clipped_left) + gauge_vis + 1));
        format!("{clipped_left}{pad}{gauge_str}")
    };
    status_row
}

fn append_approval_card(tail: &mut Vec<RenderLine>, gate: &TuiGate, st: &FrameState<'_>, width: usize, inner_w: usize, border_color: &str) -> usize {
    let req = gate.pending().expect("approval card needs a pending request");
    let reset = "\x1b[0m";
    // The composer becomes the approval card.
    // Shows exactly what is approved, and never lets model-supplied escape codes
    // restyle or hide part of it.
    let args = flashagent_llm::effective_args(&req.args_json, &req.tool).unwrap_or_default();
    let several_files = args.get("files").and_then(|f| f.as_array()).is_some_and(|f| f.len() > 1);
    // A diff with nothing removed and nothing kept is a file that does not exist yet.
    let new_file = req.diff.as_deref().is_some_and(|d| {
        d.lines()
            .filter(|l| !l.starts_with("--- ") && !l.starts_with("+++ ") && !l.starts_with("@@"))
            .all(|l| l.starts_with('+'))
    });
    let question = match req.tool.as_str() {
        "run_shell" => "Run this command?".to_string(),
        "write_file" if new_file => "Create this file?".to_string(),
        "edit_file" | "patch_file" | "write_file" if several_files => "Change these files?".to_string(),
        "edit_file" | "patch_file" | "write_file" => "Change this file?".to_string(),
        tool => format!("Allow {tool}?"),
    };
    let title = format!(" \x1b[1;38;2;225;175;95m{question}\x1b[0m \x1b[38;2;135;130;125m{}\x1b[0m ", req.tool);
    let dash_w = inner_w.saturating_sub(visible_width(&title) + 1);
    tail.push((
        LineKind::System,
        format!("{border_color}╭─{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
    ));

    let field = |k: &str| args.get(k).and_then(|v| v.as_str()).map(card_safe);
    let label_row = |label: &str, value: &str| {
        pad_box_row(
            &format!(" \x1b[38;2;160;155;145m{label}\x1b[0m \x1b[1;38;2;240;235;225m{}\x1b[0m", clip_ansi(value, inner_w.saturating_sub(label.len() + 4))),
            width,
        )
    };
    // The same budget label_row clips to, for the widest label: rows of
    // any other width lost their ends.
    let value_w = inner_w.saturating_sub(12).max(1);
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
        // The file is named on the target row; the preview rows are for the change.
        let changed: Vec<&str> = diff
            .lines()
            .filter(|l| !l.starts_with("--- ") && !l.starts_with("+++ ") && !l.starts_with("@@"))
            .collect();
        for line in changed.iter().take(6) {
            let (color, prefix) = if line.starts_with('+') {
                ("\x1b[38;2;145;205;140m", "+")
            } else if line.starts_with('-') {
                ("\x1b[38;2;225;115;105m", "-")
            } else {
                ("\x1b[38;2;135;130;125m", " ")
            };
            // Only the diff's own marker goes: the indentation is part of the change.
            // A note above the diff ("outside the project: ...") has none.
            let body = match line.as_bytes().first() {
                Some(b'+' | b'-' | b' ') => &line[1..],
                _ => line,
            };
            // Shown, never executed: an escape in the new text must not hide part of it.
            let line_clean = card_safe(&body.replace('\t', "    "));
            let line_clipped = clip_ansi(&line_clean, inner_w.saturating_sub(6));
            tail.push((
                LineKind::System,
                pad_box_row(&format!("  {color}{prefix} {line_clipped}\x1b[0m"), width),
            ));
        }
        if changed.len() > 6 {
            let more = flashagent_tui::plural(changed.len() - 6, "more line", "more lines");
            tail.push((LineKind::System, pad_box_row(&format!("    \x1b[38;2;100;95;90m+{more}\x1b[0m"), width)));
        }
    }

    // The keys are on the hint line under the card.
    let button = |label: &str, choice: ConfirmChoice| {
        if st.confirm_selection == choice {
            let bg = if choice == ConfirmChoice::Deny { "225;115;105" } else { "225;175;95" };
            format!("\x1b[1;38;2;30;26;22;48;2;{bg}m {label} \x1b[0m")
        } else {
            format!("\x1b[38;2;190;185;175m {label} \x1b[0m")
        }
    };
    tail.push((LineKind::System, pad_box_row(" ", width)));
    tail.push((
        LineKind::System,
        pad_box_row(
            &format!(
                " {}  {}  {}",
                button("Allow", ConfirmChoice::Allow),
                button("Always allow", ConfirmChoice::Always),
                button("Deny", ConfirmChoice::Deny)
            ),
            width,
        ),
    ));

    let input_line_idx = tail.len();
    tail.push((
        LineKind::System,
        format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
    ));
    input_line_idx
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

        let (settled, live) = chat.render_split(width, st.reasoning_expand);
        let mut tail: Vec<RenderLine> = live;

        if !st.pending_steers.is_empty() {
            for steer in st.pending_steers {
                let suffix = " \x1b[38;2;135;130;125m· steer queued\x1b[0m";
                let avail = width.saturating_sub(18).max(10);
                let wrapped = wrap_plain(steer, avail);
                for (j, chunk) in wrapped.into_iter().enumerate() {
                    let text = if j == 0 {
                        format!(" \x1b[1;38;2;225;175;95m›\x1b[0m \x1b[1;38;2;240;235;225m{chunk}\x1b[0m{suffix}")
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
        // A card waiting on the user breathes amber; after a turn the border briefly
        // takes the colour of how it ended.
        let border_rgb = match card_key {
            CardKey::Approval | CardKey::Question => anim::pulse(t, 1800, BORDER, Rgb(225, 175, 95)),
            CardKey::Composer => st.composer_flash.map_or(BORDER, |(lit, faded)| lit.mix(BORDER, faded)),
            _ => BORDER,
        };
        let border_color = border_rgb.fg();
        let reset = "\x1b[0m";
        // A blank row between the conversation and the composer or card under it.
        let last_row = tail.last().or_else(|| settled.last()).map(|(_, text)| text.as_str());
        if last_row.is_some_and(|text| !flashagent_tui::strip_ansi(text).trim().is_empty()) {
            tail.push((LineKind::System, String::new()));
        }
        let card_start = tail.len();

        let mut input_line_idx;
        // Where the user types, when they type into the composer.
        let mut text_cursor: Option<u16> = None;

        if gate.pending().is_some() {
            input_line_idx = append_approval_card(&mut tail, gate, &st, width, inner_w, &border_color);
        } else if let Some(prompt) = st.channel_prompt {
            let title = format!(" {} ", st.prompt_title);
            let title = title.as_str();
            // Same weight as an approval card: this replaces the binary.
            let border_color = anim::pulse(t, 1800, Rgb(225, 175, 95), Rgb(250, 215, 150)).fg();
            let reset = "\x1b[0m";
            let dash_w = inner_w.saturating_sub(visible_width(title) + 1);
            tail.push((
                LineKind::System,
                format!("{border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
            ));
            // Word-wrapped: a sentence to read, not a command to inspect.
            for row in wrap_plain(prompt, inner_w.saturating_sub(3)) {
                tail.push((
                    LineKind::System,
                    pad_box_row(&format!(" \x1b[38;2;240;235;225m{row}\x1b[0m"), width),
                ));
            }
            tail.push((
                LineKind::System,
                pad_box_row(" \x1b[1;38;2;30;26;22;48;2;225;175;95m Yes \x1b[0m  \x1b[38;2;190;185;175m No \x1b[0m", width),
            ));
            input_line_idx = tail.len();
            tail.push((
                LineKind::System,
                format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
            ));
        } else if let Some(req) = question_gate.pending() {
            // The composer becomes the question card.
            let title = " Question from FlashAgent ";
            let vis_title_len = visible_width(title);
            let dash_w = inner_w.saturating_sub(vis_title_len + 1);

            tail.push((
                LineKind::System,
                format!("{border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
            ));

            // Wrapped: the question is what the user answers, so none of it is cut.
            // Line by line: a newline kept inside one row would misplace every row after it.
            for row in req.question.lines().flat_map(|line| wrap_plain(&card_safe(line), inner_w.saturating_sub(3))) {
                tail.push((LineKind::System, pad_box_row(&format!(" \x1b[1;38;2;240;235;225m{row}\x1b[0m"), width)));
            }
            if let Some(deadline) = req.deadline {
                // During /goal the question will not wait forever; say how long.
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
            // The end of the answer, where it is being typed, fitted to the row after
            // its label: a long answer ran past the border, cursor and all. Pasted
            // control characters are shown, not sent to the terminal.
            let answer_in = |label_cells: usize| {
                flashagent_tui::tail_window(&card_safe(write_text), inner_w.saturating_sub(label_cells + 4)).0
            };
            let mut typing_line_idx = None;

            if let Some(ref opts) = req.options {
                let total_choices = opts.len();
                for (i, opt) in opts.iter().enumerate() {
                    let num = i + 1;
                    let is_sel = !is_writing && sel_idx == i;
                    let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                    let colour = if is_sel { "\x1b[1;38;2;225;175;95m" } else { "\x1b[38;2;200;195;185m" };
                    let check_box = if !req.multi_select {
                        ""
                    } else if q_state.is_some_and(|s| s.selected_indices.contains(&i)) {
                        "\x1b[1;38;2;145;205;140m[x]\x1b[0m "
                    } else {
                        "\x1b[38;2;135;130;125m[ ]\x1b[0m "
                    };
                    // A long option wraps under its own text, not under its number.
                    let lead = format!("{num}. ");
                    let indent = 4 + if req.multi_select { 4 } else { 0 } + lead.len();
                    let mut rows: Vec<String> = opt
                        .lines()
                        .flat_map(|line| wrap_plain(&card_safe(line), inner_w.saturating_sub(indent + 1).max(10)))
                        .collect();
                    // An empty option still gets its numbered row.
                    if rows.is_empty() {
                        rows.push(String::new());
                    }
                    for (j, row) in rows.iter().enumerate() {
                        let line = if j == 0 {
                            format!("  {ptr} {check_box}{colour}{lead}{row}\x1b[0m")
                        } else {
                            format!("{}{colour}{row}\x1b[0m", " ".repeat(indent))
                        };
                        tail.push((LineKind::System, pad_box_row(&line, width)));
                    }
                }

                let write_num = total_choices + 1;
                let is_write_sel = is_writing || sel_idx == total_choices;
                let write_ptr = if is_write_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                if is_writing {
                    typing_line_idx = Some(tail.len());
                    let label = format!("{write_num}. Your answer: ");
                    let shown = answer_in(4 + label.len());
                    tail.push((
                        LineKind::User,
                        pad_box_row(&format!("  {write_ptr} \x1b[1;38;2;225;175;95m{write_num}. Your answer:\x1b[0m \x1b[1;38;2;240;235;225m{shown}█\x1b[0m"), width),
                    ));
                    let hints = flashagent_tui::key_hints(&[("Enter", "send"), ("Esc", "back to the choices")], inner_w.saturating_sub(4));
                    tail.push((LineKind::System, pad_box_row(&format!("   {hints}"), width)));
                } else {
                    let colour = if is_write_sel { "\x1b[1;38;2;225;175;95m" } else { "\x1b[38;2;160;155;145m" };
                    tail.push((
                        LineKind::System,
                        pad_box_row(&format!("  {write_ptr} {colour}{write_num}. Something else: just type it\x1b[0m"), width),
                    ));
                    let numbers = format!("1-{write_num}");
                    let hints: Vec<(&str, &str)> = if req.multi_select {
                        vec![("Space", "tick"), ("↑/↓", "move"), (numbers.as_str(), "pick"), ("Enter", "confirm"), ("Esc", "cancel")]
                    } else {
                        vec![("↑/↓", "move"), (numbers.as_str(), "pick"), ("Enter", "confirm"), ("Esc", "cancel")]
                    };
                    let hints = flashagent_tui::key_hints(&hints, inner_w.saturating_sub(4));
                    tail.push((LineKind::System, pad_box_row(&format!("   {hints}"), width)));
                }
            } else {
                typing_line_idx = Some(tail.len());
                let shown = answer_in(4);
                tail.push((
                    LineKind::User,
                    pad_box_row(&format!("  \x1b[1;38;2;225;175;95m›\x1b[0m \x1b[1;38;2;240;235;225m{shown}█\x1b[0m"), width),
                ));
                let hints = flashagent_tui::key_hints(&[("Enter", "send"), ("Esc", "cancel")], inner_w.saturating_sub(4));
                tail.push((LineKind::System, pad_box_row(&format!("   {hints}"), width)));
            }

            // The answer being written shows its own block cursor.
            input_line_idx = typing_line_idx.unwrap_or(tail.len().saturating_sub(1));

            tail.push((
                LineKind::System,
                format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
            ));
        } else if let Some(overlay) = overlay {
            // The composer turns into the open menu or screen.
            tail.extend(overlay.render(width));
            input_line_idx = tail.len().saturating_sub(1);
        } else {
            tail.push((
                LineKind::System,
                format!("{border_color}╭{}╮{reset}", "─".repeat(inner_w)),
            ));

            // Attachments go above the line they go with.
            if !st.attachments.is_empty() {
                let listed = st.attachments.join("  ");
                let row = format!(
                    " \x1b[38;2;145;205;140mattached\x1b[0m \x1b[38;2;160;165;180m{listed}\x1b[0m \x1b[38;2;100;95;90m· ctrl+z removes\x1b[0m"
                );
                tail.push((LineKind::System, pad_box_row(&row, width)));
            }

            input_line_idx = tail.len();
            text_cursor = Some(4);
            let cycle = if anim::enabled() { (st.tick_n % 28) as f32 / 28.0 } else { 0.25 };
            let phase = (cycle * std::f32::consts::PI * 2.0).sin() * 0.5 + 0.5;
            let r = (210.0 + phase * 45.0) as u8;
            let g = (150.0 + phase * 55.0) as u8;
            let b = (75.0 + phase * 40.0) as u8;
            let prompt_styled = format!("\x1b[1;38;2;{r};{g};{b}m›\x1b[0m");
            let input_rows: Vec<String> = if let Some((query, found)) = st.history_search {
                // The found prompt stands in the input, the search above it.
                let found_line = found.map_or_else(
                    || "\x1b[38;2;200;120;110mno match\x1b[0m".to_string(),
                    |f| format!("\x1b[38;2;155;160;175m{}\x1b[0m", f.lines().next().unwrap_or_default()),
                );
                let label = "\x1b[38;2;135;130;125mfind in history:\x1b[0m";
                text_cursor = Some((visible_width(query) + 21).min(width.saturating_sub(2)) as u16);
                vec![format!(" {prompt_styled} {label} \x1b[1;38;2;240;235;225m{query}\x1b[0m"), format!("   {found_line}")]
            } else if let (true, Some(custom)) = (st.input.is_empty() && st.prefill_status.is_none() && !st.running && st.suggested_prompt.is_none(), st.custom_placeholder) {
                // Up to three rows: a notice cut at the edge loses its advice.
                wrap_plain(custom, width.saturating_sub(8).max(10))
                    .into_iter()
                    .take(3)
                    .enumerate()
                    .map(|(i, row)| {
                        let lead = if i == 0 { format!(" {prompt_styled}  ") } else { "    ".to_string() };
                        format!("{lead}\x1b[38;2;135;140;155m{row}\x1b[0m")
                    })
                    .collect()
            } else if st.input.is_empty() {
                vec![if let Some(prefill) = st.prefill_status {
                    format!(" {prompt_styled}  {prefill}")
                } else if st.running {
                    let what = st.turn_phase.map_or_else(|| "Working on task".to_string(), TurnPhase::label);
                    let glow = if matches!(st.turn_phase, Some(TurnPhase::Stopping)) { Rgb(235, 150, 120) } else { Rgb(235, 225, 205) };
                    format!(" {prompt_styled} {}", anim::shimmer(&format!("{what}…"), t, 2000, Rgb(135, 130, 125), glow))
                } else if let Some(sug) = st.suggested_prompt {
                    format!(" {prompt_styled}  \x1b[38;2;155;160;175m{sug}\x1b[0m  {}", key_hints(&[("→", "use")], width))
                } else if !st.attachments.is_empty() {
                    let prompt_text = if width >= 60 {
                        "Press Enter to send the image, or type a message\u{2026}"
                    } else if width >= 40 {
                        "Enter sends the image\u{2026}"
                    } else {
                        "Enter to send\u{2026}"
                    };
                    format!(" {prompt_styled}  \x1b[38;2;135;130;125m{prompt_text}\x1b[0m")
                } else {
                    // The long form when it fits.
                    let prompt_text = if width >= 60 {
                        "Ask FlashAgent to do anything\u{2026}"
                    } else if width >= 40 {
                        "Ask FlashAgent\u{2026}"
                    } else {
                        "Ask\u{2026}"
                    };
                    format!(" {prompt_styled}  \x1b[38;2;135;130;125m{prompt_text}\x1b[0m")
                }]
            } else {
                // A long prompt grows the box to a third of the screen, then scrolls inside
                // it keeping the cursor row in view.
                let max_rows = (height as usize / 3).clamp(1, 10);
                let layout = st.input.layout(width.saturating_sub(6).max(1), max_rows);
                input_line_idx += layout.cursor_row;
                text_cursor = Some((layout.cursor_col + 4).min(width.saturating_sub(2)) as u16);
                let dim = |s: &str| format!("\x1b[38;2;100;95;90m{s}\x1b[0m");
                let last = layout.rows.len() - 1;
                layout
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(i, row)| {
                        let lead = match i {
                            0 if layout.hidden_above > 0 => format!(" {} ", dim("↑")),
                            0 => format!(" {prompt_styled} "),
                            _ if i == last && layout.hidden_below > 0 => format!(" {} ", dim("↓")),
                            _ => "   ".to_string(),
                        };
                        format!("{lead}{row}")
                    })
                    .collect()
            };
            for row in &input_rows {
                tail.push((LineKind::User, pad_box_row(row, width)));
            }

            tail.push((
                LineKind::System,
                format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
            ));
        }

        // The sides of a boxed card match its top and bottom.
        if border_rgb != BORDER && !matches!(card_key, CardKey::Overlay(_)) {
            for (_, row) in tail[card_start..].iter_mut() {
                *row = row.replace(BORDER_ESC, &border_color);
            }
        }
        // A new card unfolds from its title down.
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
            }
        }

        if let Some(ac) = autocomplete {
            tail.extend(ac.render(width));
        }

        // Three footer lines: hints, tip, and context status.
        let shown = FooterVisibility {
            approval: gate.pending().is_some(),
            question: question_gate.pending().is_some(),
            overlay: overlay.is_some(),
            autocomplete: autocomplete.is_some(),
        };
        tail.push((LineKind::System, footer_hint(chat, shown, &st, width, t)));
        append_tip_rows(&mut tail, &st, width, height);
        let awaiting_user = gate.pending().is_some() || question_gate.pending().is_some();
        tail.push((LineKind::System, footer_status(width, context_usage, &st, awaiting_user)));


        // The conversation scrolls; the composer and the footer under it stay where
        // they are, so a reply can be written while reading back.
        let height = height as usize;
        // A card taller than the screen keeps its head (what is asked, what is to
        // be approved) and its end (the choices, the keys); rows between go, and a
        // row says so. Cut from the top like the composer, it lost its title and
        // target first.
        if matches!(card_key, CardKey::Approval | CardKey::Question | CardKey::Channel) {
            const HEAD: usize = 2;
            const END: usize = 6;
            let card_rows = tail.len() - card_start;
            if card_rows > height && card_rows > HEAD + END {
                let from = card_start + HEAD;
                let to = (from + card_rows - height + 1).min(tail.len() - END);
                if to > from + 1 {
                    let hidden = to - from;
                    let note = format!(
                        " \x1b[38;2;135;130;125m\u{2026} {} not shown \u{b7} a taller window shows them\x1b[0m",
                        flashagent_tui::plural(hidden, "row", "rows")
                    );
                    let marker = pad_box_row(&note, width).replace(BORDER_ESC, &border_color);
                    tail.splice(from..to, [(LineKind::System, marker)]);
                    if input_line_idx >= to {
                        input_line_idx -= hidden - 1;
                    } else if input_line_idx >= from {
                        input_line_idx = from;
                    }
                }
            }
        }
        let chat_len = settled.len() + card_start;
        let bottom = &tail[card_start..];
        let chat_room = height.saturating_sub(bottom.len());
        if self.scroll_offset > 0 && chat_len > self.total_lines {
            // New lines arrived under a view scrolled back: it stays on what is read.
            self.scroll_offset += chat_len - self.total_lines;
        }
        self.total_lines = chat_len;
        // One row of the room goes to the line that says the view is scrolled.
        self.max_scroll = chat_len.saturating_sub(chat_room.saturating_sub(1).max(1));
        self.scroll_offset = self.scroll_offset.min(self.max_scroll);

        // Every row gets its kind's colour, restored after each reset inside it,
        // and is cut to the width: a row wider than the screen would wrap onto
        // the next and push everything under it down.
        let styled = |(kind, text): &RenderLine| {
            let prefix = crossterm::style::SetForegroundColor(color(*kind)).to_string();
            format!("{prefix}{}\x1b[39m", restore_line_color(&clip_ansi(text, width), &prefix))
        };
        let chat = || settled.iter().chain(tail[..card_start].iter());
        let mut first_chat_row = chat_len.saturating_sub(chat_room);
        let mut rows: Vec<String> = if self.scroll_offset > 0 && chat_room > 1 {
            let end = chat_len - self.scroll_offset;
            let start = end.saturating_sub(chat_room - 1);
            first_chat_row = start;
            let mut rows: Vec<String> = chat().skip(start).take(end - start).map(styled).collect();
            let marker = format!(
                " \x1b[38;2;100;95;90m── \x1b[38;2;225;175;95m↓ {} below\x1b[38;2;100;95;90m · End or Esc to return ──\x1b[0m",
                flashagent_tui::plural(self.scroll_offset, "more line", "more lines")
            );
            rows.push(marker);
            rows
        } else {
            // Anchored: the end of the conversation right above the composer, or, while
            // it is short, the composer right under it.
            chat().skip(chat_len.saturating_sub(chat_room)).map(styled).collect()
        };
        let chat_rows = rows.len();
        rows.extend(bottom.iter().map(styled));
        // A composer taller than the screen keeps its end, where the typing is.
        let cut = rows.len().saturating_sub(height);
        rows.drain(..cut);
        let scrolled_marker = usize::from(self.scroll_offset > 0 && chat_room > 1);
        self.chat_view = (first_chat_row + cut, chat_rows.saturating_sub(scrolled_marker).saturating_sub(cut));
        // The terminal's cursor is shown only where text is typed; cards and menus
        // mark their own choice.
        let cursor = text_cursor.and_then(|col| {
            let row = (chat_rows + input_line_idx.checked_sub(card_start)?).checked_sub(cut)?;
            Some((row as u16, col))
        });
        self.screen.paint(&rows, cursor);
    }
}

impl App {
    pub(crate) fn draw(&mut self, cx: &LoopCtx<'_>, autocomplete: Option<&AutocompletePopup>) {
        anim::set_enabled(self.config.animations);
        self.show_progress(cx);
        // Read every frame, so leaving settings without saving restores the theme;
        // while Settings is open, the theme picked there is the one shown.
        let shown_theme = match &self.overlay {
            Some(Overlay::Settings(view)) => view.config.color_theme,
            _ => self.config.color_theme,
        };
        theme::set(shown_theme);
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
        // An estimate of reading the prompt means nothing while no server answers.
        let live_prefill =
            self.token_tracker.live_prefill_status().filter(|_| self.announced_mood != MascotMood::Offline);
        let ttft_display = self.token_tracker.ttft_display();
        let tg_speed = self.token_tracker.tg_3s();
        let draft = self.token_tracker.draft_display();
        let config = &self.config;

        self.renderer.frame(
            &self.chat,
            cx.gate,
            cx.question_gate,
            self.overlay.as_ref(),
            autocomplete,
            &self.context_usage,
            FrameState {
                input: &self.input,
                provider: (self.config.active_profile().name.as_str(), self.current_model.as_str()),
                history_search: self
                    .history_search
                    .as_ref()
                    .map(|h| (h.query.as_str(), h.found(&self.input_history))),
                mode: cx.perm.state().mode(),
                is_goal_active: self.goal_state.is_some(),
                goal_progress: goal_progress.as_deref(),
                tip: config.show_tips.then_some(self.tip_animator.tip_text),
                tip_animated: None,
                tip_lines: config.show_tips.then_some(cx.tip_lines.as_slice()),
                reasoning_expand: ReasoningExpansion { all: self.all_expanded, last: self.last_expanded },
                tick_n: self.tick_n,
                running: self.running,
                tokens_per_sec: config.show_tokens.then_some(tg_speed),
                draft_acceptance: draft.as_deref().filter(|_| config.show_tokens),
                confirm_selection: self.confirm_select.choice(),
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
                background_tasks: cx.tools_arc.shells().running_count(),
                shell_running: self.running && cx.tools_arc.shells().foreground_running(),
                composer_flash,
            },
        );
    }

    pub(crate) fn animating(&self) -> bool {
        self.renderer.animating() || self.composer_flash().is_some()
    }

    /// The taskbar button says a turn is running, waiting on an answer, or how
    /// far an update has downloaded, so a long `/goal` can be left in the
    /// background.
    pub(crate) fn show_progress(&self, cx: &LoopCtx<'_>) {
        use flashagent_tui::screen::{set_progress, Progress};
        let downloading = self.update_progress.as_ref().and_then(|(_, stage)| match stage {
            flashagent_svc::updater::UpdateProgress::Downloading { received, total: Some(total) } if *total > 0 => {
                Some(((*received as f64 / *total as f64) * 100.0).round() as u8)
            }
            _ => None,
        });
        let progress = if self.running && (cx.gate.pending().is_some() || cx.question_gate.pending().is_some()) {
            Progress::Waiting
        } else if self.running {
            Progress::Busy
        } else if let Some(pct) = downloading {
            Progress::Percent(pct)
        } else {
            Progress::None
        };
        set_progress(progress);
    }

    /// While it lasts.
    fn composer_flash(&self) -> Option<(Rgb, f32)> {
        const FLASH_MS: u64 = 1400;
        let (colour, started) = self.turn_flash?;
        let faded = anim::progress(started, anim::now_ms(), FLASH_MS);
        (faded < 1.0).then_some((colour, faded))
    }

    pub(crate) fn flash_turn_end(&mut self, reason: Option<DoneReason>) {
        let colour = match reason {
            Some(DoneReason::Completed) => Rgb(120, 220, 140),
            None | Some(DoneReason::Failed) => Rgb(230, 110, 95),
            Some(_) => Rgb(225, 175, 95),
        };
        self.turn_flash = Some((colour, anim::now_ms()));
    }
}

/// Drops from the middle, keeping the last hint, which is the way out.
fn key_hints(pairs: &[(&str, &str)], width: usize) -> String {
    flashagent_tui::key_hints(pairs, width)
}

#[cfg(test)]
mod footer_tests {
    use super::*;

    #[test]
    fn the_status_row_never_exceeds_the_window() {
        let left = format_status_left(false, false, false, None, "Manual", "");
        let provider = format_provider("Anthropic", "claude-opus-5-5");
        for width in 1..120 {
            let row = fit_status_row(width, &left, &provider, "");
            assert!(visible_width(&row) <= width, "width {width}: {row:?}");
        }
    }

    #[test]
    fn the_status_row_drops_the_model_then_the_provider() {
        let left = format_status_left(false, false, false, None, "Manual", "");
        let provider_only = format_provider("Anthropic", "");
        let provider = format_provider("Anthropic", "claude-opus-5-5");
        let with_provider = fit_status_row(visible_width(&left) + visible_width(&provider_only) + 4, &left, &provider, "");
        let with_provider = flashagent_tui::strip_ansi(&with_provider);
        assert!(with_provider.contains("Anthropic"));
        assert!(!with_provider.contains("claude-opus-5-5"));

        let without_provider = fit_status_row(visible_width(&left) + 4, &left, &provider, "");
        let without_provider = flashagent_tui::strip_ansi(&without_provider);
        assert!(without_provider.contains("[Manual]"));
        assert!(!without_provider.contains("Anthropic"));
    }
}
