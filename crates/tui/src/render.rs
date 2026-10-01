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
        // Between the tool colour and the reasoning grey: a child is work in
        // progress, closer to a tool call than to something said.
        LineKind::Subagent => crossterm::style::Color::Rgb { r: 150, g: 170, b: 195 },
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
    /// Screen row of each pinned subagent row, with the id to open. A click is
    /// matched here rather than against the transcript: these rows are not part
    /// of it, so F2 never reaches them.
    agent_screen: Vec<(usize, String)>,
    /// Where the first pinned subagent row sits in the tail, when the work box
    /// is up. A click is matched against this rather than counted back from the
    /// end of the composer: the composer block below the box has rows of its
    /// own that belong to no child.
    agent_first: Option<usize>,
    /// The pinned rows and the ones the mouse has opened, handed from `frame`
    /// to `paint_layout`, which is where they land on a screen row.
    agent_plan: Vec<(String, String)>,
    agent_open: Vec<String>,
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
    /// Laid out by `TipAnimator::render_lines`; `None` with tips off.
    pub(crate) tip_lines: Option<&'a [String]>,
    pub(crate) reasoning_expand: ReasoningExpansion,
    pub(crate) tick_n: usize,
    pub(crate) running: bool,
    /// Generation speed over the last 3 seconds; `None` when counters are off.
    pub(crate) tokens_per_sec: Option<f64>,
    /// `draft 81%`: how much of a draft model's guessing the model kept.
    pub(crate) draft_acceptance: Option<&'a str>,
    /// Tokens this turn has generated, so what a turn cost is on the line that
    /// is already reporting its speed, rather than only in `/goal`.
    pub(crate) turn_tokens: Option<String>,
    /// What the session has cost, only where the backend prices its turns. Absent
    /// otherwise: a local model is not free, it is unpriced, and "$0.00" would
    /// say the wrong thing confidently.
    pub(crate) cost_display: Option<&'a str>,
    pub(crate) confirm_selection: ConfirmChoice,
    pub(crate) question_state: Option<&'a QuestionUiState>,
    pub(crate) custom_placeholder: Option<&'a str>,
    pub(crate) suggested_prompt: Option<&'a str>,
    pub(crate) copy_toast: Option<&'a str>,
    pub(crate) prefill_status: Option<&'a str>,
    pub(crate) ttft_display: Option<&'a str>,
    /// The share of the prompt the server read from its cache.
    pub(crate) cache_display: Option<&'a str>,
    /// A model server on this machine or the local network, where a time to
    /// first token means something about the hardware. A hosted API's is a queue
    /// and a network, so the cache share is the honest thing to report there.
    pub(crate) nearby_server: bool,
    pub(crate) background: Option<&'a str>,
    pub(crate) background_style: NoticeStyle,
    pub(crate) turn_phase: Option<&'a TurnPhase>,
    pub(crate) attachments: &'a [String],
    pub(crate) channel_prompt: Option<&'a str>,
    /// Title of the card `channel_prompt` fills.
    pub(crate) prompt_title: &'a str,
    pub(crate) context_warn_threshold: usize,
    /// What the server said the window is. `None` before it answers: the
    /// capacity in use is then a guess, and a percentage off it is a guess
    /// wearing the clothes of a number.
    pub(crate) context_reported: bool,
    /// What `/compact` is doing, in the row under the prompt: a status belongs
    /// with the other running things, not in the conversation as a line.
    pub(crate) compact_status: Option<&'a str>,
    pub(crate) pending_steers: &'a [(String, Vec<Attachment>)],
    /// Every subagent row, pinned above the composer: id and the line itself.
    pub(crate) agent_rows: &'a [(String, String)],
    /// The rows the mouse has opened, by id.
    pub(crate) agent_expanded: &'a [&'a str],
    /// The prose a phase replaced, by id.
    pub(crate) agent_details: &'a std::collections::HashMap<String, String>,
    /// Background commands still running, one line each: they belong in the same
    /// box as the subagents, because both are work under way that the transcript
    /// does not show.
    pub(crate) background_rows: &'a [String],
    pub(crate) queued_commands: &'a [String],
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

/// The rows for the box that sits above the composer: the subagents under way,
/// the background commands under way, and the edges that say this is not the
/// conversation. Empty when nothing is running, so an idle screen has no box
/// standing there with nothing in it.
///
/// Pure, so the shape of the box can be checked without a terminal.
pub(crate) fn work_box(
    agent_rows: &[(String, String)],
    expanded: &[&str],
    details: &std::collections::HashMap<String, String>,
    background_rows: &[String],
    width: usize,
    t: u64,
) -> Vec<(LineKind, String)> {
    let mut body: Vec<(LineKind, String)> = Vec::new();
    for (id, line) in agent_rows {
        // `box_inner` is the room a body row may fill behind the box's own two
        // columns of indent, so a child's row is handed two columns more than
        // that: `AgentTree::line` counts its `width` as the whole row, mark
        // included, and the indent is added on the way in here.
        body.push((LineKind::Tool, live_row(&format!("  {line}"), t)));
        if expanded.contains(&id.as_str()) {
            if let Some(detail) = details.get(id) {
                body.push((LineKind::Tool, format!("\x1b[38;2;120;125;140m{detail}\x1b[0m")));
            }
        }
    }
    for row in background_rows {
        body.push((LineKind::Tool, live_row(&format!("  {row}"), t)));
    }
    if body.is_empty() {
        return Vec::new();
    }
    let edge = "\x1b[38;2;100;95;90m";
    // As wide as the composer block under it, not two columns narrower. Both
    // start at column 0 and both are closed on the right, so a narrower box
    // makes the right-hand wall step in exactly at the seam, and a seam that
    // steps looks like two frames rather than one.
    let box_w = width;
    // The title is cut to the room the top edge actually has. A title longer than
    // the box made the top edge wider than the window, which pushed the whole
    // layout one row over: the box has to close, so the words give way.
    //
    // Three fixed columns go to `╭`, the `─` that follows it, and `╮`; what is
    // left is the title and the run of dashes that closes the edge. Every branch
    // has to land on exactly that room, or the top edge and the bottom one stop
    // being the same width and the box no longer closes. A box too narrow for
    // even that one `─` drops it, rather than pushing the edge a column wider
    // than the bottom of its own frame.
    let lead = usize::from(box_w >= 3);
    let room = box_w.saturating_sub(2 + lead);
    let full = " subagents & background ";
    let title: String = if full.chars().count() <= room {
        full.to_string()
    } else if " work ".chars().count() <= room {
        " work ".to_string()
    } else if room == 0 {
        String::new()
    } else {
        // Narrower than the word: what is left says there was more.
        let mut cut_title: String = full.trim().chars().take(room - 1).collect();
        cut_title.push('\u{2026}');
        cut_title
    };
    let dashes = room.saturating_sub(visible_width(&title));
    let mut out = vec![(
        LineKind::Tool,
        format!(
            "{edge}\u{256d}{}{title}{edge}{}\u{256e}\x1b[0m",
            "\u{2500}".repeat(lead),
            "\u{2500}".repeat(dashes)
        ),
    )];
    // The sides run the height of the box, not only its edges: a body row
    // pushed raw left the frame open down both sides, so the rows read as
    // transcript rather than as work in progress.
    for (kind, row) in body {
        out.push((kind, pad_box_row(&row, box_w)));
    }
    // `├─┤`, not `╰─╯`: the composer draws its own bottom edge, and this is the
    // top of that half of the block, so the sides carry on unbroken.
    out.push((
        LineKind::Tool,
        format!("{edge}\u{251c}{}\u{2524}\x1b[0m", "\u{2500}".repeat(box_w.saturating_sub(2))),
    ));
    out
}

/// Every row the box holds is work that is under way: a child leaves the box when
/// it ends, and a command is listed only while it runs. So the whole box sweeps,
/// at the period and between the colours the call in flight uses — one movement
/// across the interface rather than a second one invented here. Animations off
/// leaves it still, which is the setting's promise and not a special case here.
fn live_row(plain: &str, t: u64) -> String {
    anim::shimmer(plain, t, 1800, anim::Rgb(150, 160, 175), anim::Rgb(240, 245, 255))
}

/// A background command as a row of the work box. `box_inner` is the room left
/// inside the box's own frame once its two columns of indent are spent, so the
/// mark and its trailing space and the ` · {elapsed}` that closes the row all
/// come out of the command instead of painting through the right edge.
///
/// The mark turns while the command runs. A `▸` says a command was started and
/// reads the same for a server that is still serving and for a build that died
/// four minutes ago, and this box only ever lists the running ones — so the mark
/// is the only place the difference between "just started" and "been at it a
/// while" can be told apart from.
pub(crate) fn background_row(command: &str, elapsed: &str, box_inner: usize, mark: &str) -> String {
    // The indent is already outside `box_inner`, so counting it again here
    // would throw two columns away rather than keep the row inside the box.
    let spent = visible_width(mark) + 1 + 3 + visible_width(elapsed);
    let cmd = flashagent_tui::truncate_middle(command, box_inner.saturating_sub(spent).max(4));
    format!("{mark} {cmd} \u{b7} {elapsed}")
}

/// The box's own top and bottom edges. A click has to skip them to land on the
/// row it belongs to, so the caller needs the count rather than guessing.
pub(crate) const WORK_BOX_EDGES: usize = 2;

#[cfg(test)]
mod work_box_tests {
    /// The colours a body row carries at one moment, for comparing two.
    fn row_colour(t: u64) -> String {
        work_box(&[("sub1".into(), "\u{25b8} researcher".into())], &[], &Default::default(), &[], 60, t)[1].1.clone()
    }

    /// The box sweeps because every row in it is work that is running: a child
    /// leaves when it ends, and a command is listed only while it runs. With
    /// animations off it must still be readable, which is that setting's promise
    /// rather than a special case made here.
    #[test]
    fn the_box_sweeps_while_its_work_runs_and_is_still_when_animations_are_off() {
        flashagent_tui::anim::set_enabled(true);
        assert_ne!(row_colour(0), row_colour(900), "the sweep has to differ from one moment to the next");
        flashagent_tui::anim::set_enabled(false);
        assert_eq!(row_colour(0), row_colour(900), "animations off is still: no sweep at all");
        flashagent_tui::anim::set_enabled(true);
    }
    use super::*;

    fn plain(rows: &[(LineKind, String)]) -> Vec<String> {
        rows.iter().map(|(_, t)| flashagent_tui::strip_ansi(t)).collect()
    }

    fn agent(id: &str, line: &str) -> (String, String) {
        (id.to_string(), line.to_string())
    }

    #[test]
    fn an_idle_screen_has_no_box_at_all() {
        assert!(work_box(&[], &[], &Default::default(), &[], 80, 0).is_empty(), "an empty box would stand there saying nothing");
    }

    #[test]
    fn subagents_and_background_commands_share_one_box() {
        let rows = work_box(
            &[agent("sub1", "  \u{25b8} researcher \u{b7} tooling grep")],
            &[],
            &Default::default(),
            &["  \u{25b8} cargo test \u{b7} 12s".to_string()],
            80,
            0,
        );
        let text = plain(&rows);
        assert_eq!(text.len(), 4, "two edges around two rows: {text:?}");
        assert!(text[0].contains('\u{256d}') && text[0].contains('\u{256e}'), "the top edge is closed: {text:?}");
        assert!(text[3].contains('\u{251c}') && text[3].contains('\u{2524}'), "the bottom edge is closed: {text:?}");
        assert!(text[1].contains("researcher"), "the child is in the box: {text:?}");
        assert!(text[2].contains("cargo test"), "and the background command beside it: {text:?}");
        assert_eq!(text[0].chars().count(), text[3].chars().count(), "the edges are the same width: {text:?}");
        assert_eq!(text[0].chars().count(), 80, "the whole window, the composer's own width: {text:?}");
    }

    #[test]
    fn an_opened_child_adds_its_detail_inside_the_same_box() {
        let details: std::collections::HashMap<String, String> =
            [("sub1".to_string(), "      grepping for the token tracker".to_string())].into_iter().collect();
        let rows = work_box(&[agent("sub1", "  \u{25b8} researcher")], &["sub1"], &details, &[], 80, 0);
        let text = plain(&rows);
        assert!(text[2].contains("grepping for the token tracker"), "{text:?}");
    }

    #[test]
    fn the_box_is_exactly_as_wide_as_the_composer_under_it() {
        for width in [20usize, 40, 80, 200] {
            let rows = work_box(&[agent("sub1", "  \u{25b8} coder")], &[], &Default::default(), &[], width, 0);
            let text = plain(&rows);
            let top = text[0].chars().count();
            let bottom = text[2].chars().count();
            assert_eq!(top, bottom, "the box must close at {width}: {text:?}");
            // The composer draws its block at the full width, and both start at
            // column 0, so a box two columns narrower makes the right-hand wall
            // step in exactly at the seam.
            assert_eq!(top, width, "the box must line up with the composer, not sit inside it: {width}: {text:?}");
        }
    }

    #[test]
    fn the_edges_are_equal_at_every_narrow_width_too() {
        // Below eleven columns the title is cut rather than shortened, and that
        // branch used to hand the top edge one more column than the bottom one.
        for width in 4usize..=32 {
            let rows = work_box(&[agent("sub1", "  \u{25b8} coder")], &[], &Default::default(), &[], width, 0);
            let text = plain(&rows);
            let top = text[0].chars().count();
            let bottom = text[2].chars().count();
            assert_eq!(top, bottom, "the edges must be the same width at {width}: {text:?}");
            assert_eq!(top, width, "the box must close at the window's width at {width}: {text:?}");
        }
    }

    #[test]
    fn the_box_is_framed_on_both_sides_for_its_whole_height() {
        let rows = work_box(
            &[agent("sub1", "  \u{25b8} researcher")],
            &[],
            &Default::default(),
            &["  \u{25b8} cargo test".to_string()],
            40,
            0,
        );
        for row in plain(&rows).iter().skip(1).take(2) {
            assert!(row.starts_with('\u{2502}') && row.ends_with('\u{2502}'), "a body row has no sides: {row:?}");
        }
    }

    /// The room a pinned row gets, spelled out: the box is the window wide,
    /// `pad_box_row` keeps `width - 2` inside its own frame, and the box spends
    /// two of those on its indent. A row built to that budget survives whole,
    /// while one cell more of it is cut -- which is what turned a phase label
    /// into "thinkin" with no ellipsis to say it had been.
    #[test]
    fn a_child_row_fills_the_room_inside_the_box_and_no_more() {
        let width = 60;
        let fits = "  \u{25b8} researcher \u{b7} thinking";
        let rows = work_box(&[agent("sub1", fits)], &[], &Default::default(), &[], width, 0);
        assert!(
            plain(&rows)[1].contains("thinking"),
            "a row that fits the room must not be cut: {:?}",
            plain(&rows)[1]
        );
        let over = format!("{fits} and a good deal more than the box has room for");
        let rows = work_box(&[agent("sub1", &over)], &[], &Default::default(), &[], width, 0);
        let body = plain(&rows)[1].clone();
        assert_eq!(body.chars().count(), width, "the row is still the width of the box: {body:?}");
        assert!(
            !body.trim_end().ends_with("room for"),
            "the row must stop at the wall: {body:?}"
        );
    }

    #[test]
    fn a_long_phase_stays_inside_its_own_box() {
        let long = format!("  \u{25b8} researcher \u{b7} {}", "grepping the token tracker ".repeat(6));
        for width in [30usize, 60, 120] {
            let rows = work_box(&[agent("sub1", &long)], &[], &Default::default(), &[], width, 0);
            for row in plain(&rows) {
                assert_eq!(row.chars().count(), width, "every row of the box is exactly the window wide: {width}: {row:?}");
            }
        }
    }

    #[test]
    fn a_background_row_fits_the_room_left_inside_the_box() {
        let cmd = "cargo test -p flashagent-core --lib --release -- --nocapture";
        for inner in [20usize, 40, 76] {
            let row = background_row(cmd, "1h02m", inner, "\u{25b8}");
            assert!(
                flashagent_tui::visible_width(&row) <= inner,
                "the row must fit {inner} columns with its indent: {row:?}"
            );
            assert!(row.contains("1h02m"), "the elapsed time is never the thing that is cut: {row:?}");
        }
        // Narrower than the marks themselves: the row is longer than the box,
        // and `pad_box_row` is what keeps it from painting through the side.
        let cramped = background_row(cmd, "1h02m", 10, "\u{25b8}");
        assert!(flashagent_tui::visible_width(&cramped) > 10, "the marks alone overrun a tiny box: {cramped:?}");
    }

    /// A child's row and a background command's row are two kinds of row in one
    /// box, so they have to be measured the same way: both fill the room behind
    /// the box's own two columns of indent, and both land inside `pad_box_row`
    /// without being cut. The phase is the part that shows the arithmetic -- it
    /// is at the far end of the row, where a two-column shortfall shows up as
    /// "thinkin" with no ellipsis.
    #[test]
    fn both_kinds_of_row_in_the_box_fill_the_same_room() {
        for width in [40usize, 80, 120] {
            let box_inner = width - 4;
            let mut tree = flashagent_tui::agents::AgentTree::default();
            tree.apply(&flashagent_core::SubagentEvent {
                id: "sub1".into(),
                role: "researcher".into(),
                event: flashagent_core::LoopEvent::ToolStarted { id: "c1".into(), name: "read_file".into(), args_json: "{}".into() },
            });
            // The row is handed the indent back, because `line` counts its
            // `width` as the whole row while the indent is added by the box.
            let rows = tree.pinned_rows(box_inner + 2, "\u{25b8}");
            let bg = background_row("cargo test -p flashagent-core --lib", "12s", box_inner, "\u{25b8}");
            let boxed = work_box(
                &rows,
                &[],
                &Default::default(),
                &[bg],
                width,
                0,
            );
            for row in plain(&boxed) {
                assert_eq!(row.chars().count(), width, "every row is the width of the box at {width}: {row:?}");
            }
            let body = plain(&boxed)[1].clone();
            assert!(body.contains("read_file"), "the phase has to survive whole at {width}: {body:?}");
        }
    }

    #[test]
    fn the_box_closes_onto_the_composer_rather_than_starting_a_second_one() {
        let rows = work_box(&[agent("sub1", "  \u{25b8} coder")], &[], &Default::default(), &[], 80, 0);
        let text = plain(&rows);
        let edges = text.iter().filter(|r| r.starts_with('\u{256d}') || r.starts_with('\u{251c}')).count();
        assert_eq!(edges, WORK_BOX_EDGES, "the box has two edges of its own: {text:?}");
        assert!(text.last().unwrap().starts_with('\u{251c}'), "the box opens into the composer: {text:?}");
    }
}

impl Renderer {
    pub(crate) fn new() -> Self {
        Self {
            scroll_offset: 0,
            max_scroll: 0,
            total_lines: 0,
            chat_view: (0, 0),
    agent_screen: Vec::new(),
            agent_first: None,
    agent_plan: Vec::new(),
    agent_open: Vec::new(),
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

    /// The subagent row on screen row `y`, if one is there.
    pub(crate) fn agent_row_at(&self, y: u16) -> Option<&str> {
        self.agent_screen.iter().find(|(row, _)| *row == y as usize).map(|(_, id)| id.as_str())
    }

    /// `dir` negative scrolls back. One line at a time; a throw is spent a few
    /// lines at a time by the caller.
    pub(crate) fn scroll_by(&mut self, dir: i32) {
        if dir > 0 {
            self.scroll_down(dir as usize);
        } else if dir < 0 {
            self.scroll_up(dir.unsigned_abs() as usize);
        }
    }

    /// What a mouse drag has covered, as plain text.
    pub(crate) fn text_in(&self, top: u16, bottom: u16, left: u16, right: u16) -> String {
        self.screen.text_in(top, bottom, left, right)
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

fn footer_hint(shown: FooterVisibility, st: &FrameState<'_>, width: usize, t: u64) -> String {
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
        if let Some(turn) = st.turn_tokens.as_deref() {
            live.push_str(&format!("{dot}{turn}"));
        }
        if let Some(draft) = st.draft_acceptance {
            live.push_str(&format!("{dot}{draft}"));
        }
        // A nearby server is a machine: its time to first token is worth showing
        // and its cache share beside it. A hosted one is a queue and a network, so
        // the number on the line was never about the model, and the cache share
        // says what the prefix actually cost.
        if st.nearby_server {
            if let Some(prefill) = st.ttft_display {
                live.push_str(&format!("{dot}{prefill}"));
            }
            if let Some(cache) = st.cache_display {
                live.push_str(&format!("{dot}{cache}"));
            }
        } else if let Some(cache) = st.cache_display {
            live.push_str(&format!("{dot}{cache}"));
        }
        // The counters show the model is alive and the notice must not be missed;
        // the key hints give way when both do not fit.
        let esc_does = if st.input.is_empty() { "interrupt" } else { "clear" };
        let esc = key_hints(&[("Esc", esc_does)], width);
        let steer = key_hints(&[("Enter", "steer or /command"), ("Esc", esc_does)], width);
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
    } else {
        // The one key that finds every other. The welcome card lists more, but
        // an empty row under the prompt read as a line that failed to draw.
        format!("  {}", key_hints(&[("Ctrl+K", "commands"), ("/help", ""), ("Ctrl+D", "quit")], width.saturating_sub(2)))
    };
    left_hint
}

fn append_tip_rows(tail: &mut Vec<RenderLine>, st: &FrameState<'_>, width: usize) {
    // A running compaction is the status of the moment: it takes the tip's row
    // rather than pushing the chat down, and it is clipped, never wrapped.
    if let Some(status) = st.compact_status {
        tail.push((LineKind::System, format!("  {}", clip_ansi(status, width.saturating_sub(2)))));
        return;
    }
    for line in st.tip_lines.unwrap_or_default() {
        tail.push((LineKind::System, clip_ansi(line, width.saturating_sub(2))));
    }
}

/// What the status line says about the context window, widest first. Until the
/// server names a window the capacity in use is a guess, and a percentage off
/// it would be a guess dressed as a fact: only the count is true.
fn context_gauges(context_usage: &ContextUsage, reported: bool, warn_threshold: usize) -> [String; 3] {
    let percent = context_usage.percentage().clamp(0.0, 100.0);
    if !reported {
        let used = format!(
            "\x1b[38;2;135;215;165m{}\x1b[0m used",
            ContextUsage::format_tokens(context_usage.total_used())
        );
        return [used.clone(), used.clone(), used];
    }
    // Nearly full, it says what to do about it instead of drawing the bar.
    if warn_threshold > 0 && percent >= warn_threshold as f32 {
        let amber = |s: String| format!("\x1b[1;38;2;245;160;80m{s}\x1b[0m");
        return [
            amber(format!("context {percent:.0}% full \u{b7} /compact frees it")),
            amber(format!("{percent:.0}% full \u{b7} /compact")),
            amber(format!("{percent:.0}%")),
        ];
    }
    [
        context_usage.format_compact_gauge(10),
        context_usage.format_compact_gauge(4),
        format!("\x1b[38;2;135;215;165m{percent:.0}%\x1b[0m"),
    ]
}

fn footer_status(width: usize, context_usage: &ContextUsage, st: &FrameState<'_>, awaiting_user: bool) -> String {
    let mode_room = 2 + visible_width(st.mode.label()) + 2 + 1;
    let gauges = context_gauges(context_usage, st.context_reported, st.context_warn_threshold);
    let gauge_str = gauges.into_iter().find(|g| mode_room + visible_width(g) + 3 <= width).unwrap_or_default();
    // What the session costs sits next to the context gauge, and goes before it
    // when the window is narrow: the gauge is what stops a turn from fitting,
    // the bill does not.
    let gauge_str = match st.cost_display {
        Some(cost) if mode_room + visible_width(cost) + 3 + visible_width(&gauge_str) <= width => {
            format!("\x1b[38;2;135;215;165m{cost}\x1b[0m · {gauge_str}")
        }
        _ => gauge_str,
    };
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

/// The change an approval card asks about, one styled row per diff line.
/// The card shows as many as fit; `v` puts them all in the conversation.
pub(crate) fn diff_rows(diff: &str, inner_w: usize) -> Vec<String> {
    diff.lines()
        .filter(|l| !l.starts_with("--- ") && !l.starts_with("+++ ") && !l.starts_with("@@"))
        .map(|line| {
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
            format!("  {color}{prefix} {line_clipped}\x1b[0m")
        })
        .collect()
}

/// Diff rows the card has room for: the rest of the screen, less the card's
/// own rows, the footer and a few lines of the conversation.
fn approval_preview_rows(height: usize) -> usize {
    height.saturating_sub(16).max(6)
}

/// `joined` is true when the work box above has already drawn the top edge of
/// this block: the card is then its bottom half, and a second `╭` under the
/// `├─┤` seam would open a box inside an unclosed one.
#[allow(clippy::too_many_arguments)]
fn append_approval_card(tail: &mut Vec<RenderLine>, gate: &TuiGate, st: &FrameState<'_>, width: usize, height: usize, inner_w: usize, border_color: &str, joined: bool) -> usize {
    // A card with nothing pending is nothing to draw. It used to be an `expect`,
    // and it fired: the row an approval card occupies moved once prompts started
    // taking a row of their own, and a stale match drew one over a screen that
    // had already answered the request. Rendering answers the screen; it does
    // not assert about it.
    let Some(req) = gate.pending() else { return 0 };
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
    if !joined {
        tail.push((
            LineKind::System,
            format!("{border_color}╭─{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
        ));
    }

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
        let rows = diff_rows(diff, inner_w);
        let room = approval_preview_rows(height);
        // A last row that would only say "+1 more line" shows that line instead.
        let shown = if rows.len() <= room + 1 { rows.len() } else { room };
        for row in &rows[..shown] {
            tail.push((LineKind::System, pad_box_row(row, width)));
        }
        if rows.len() > shown {
            let more = flashagent_tui::plural(rows.len() - shown, "more line", "more lines");
            tail.push((LineKind::System, pad_box_row(&format!("    \x1b[38;2;100;95;90m+{more} · \x1b[38;2;225;175;95mv\x1b[38;2;100;95;90m shows the whole change\x1b[0m"), width)));
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

/// `joined` is true when the work box above has already drawn the top edge of
/// this block: the card is then its bottom half, and a second `╭` under the
/// `├─┤` seam would open a box inside an unclosed one.
fn append_question_card(tail: &mut Vec<RenderLine>, question_gate: &TuiQuestionGate, st: &FrameState<'_>, width: usize, inner_w: usize, border_color: &str, joined: bool) -> usize {
    let req = question_gate.pending().expect("question card needs a pending request");
    let reset = "\x1b[0m";
    // The composer becomes the question card.
    let title = " Question from FlashAgent ";
    let vis_title_len = visible_width(title);
    let dash_w = inner_w.saturating_sub(vis_title_len + 1);

    if !joined {
        tail.push((
            LineKind::System,
            format!("{border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
        ));
    }

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
    let no_answer = flashagent_tui::Composer::new();
    let write_text = q_state.map_or(&no_answer, |s| &s.write_in_text);
    // Around the cursor, fitted to the row after its label: a long answer ran
    // past the border. Pasted control characters are shown, not sent.
    let answer_in = |label_cells: usize| write_text.field_view(inner_w.saturating_sub(label_cells + 4), card_safe);
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
                pad_box_row(&format!("  {write_ptr} \x1b[1;38;2;225;175;95m{write_num}. Your answer:\x1b[0m \x1b[1;38;2;240;235;225m{shown}\x1b[0m"), width),
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
            pad_box_row(&format!("  \x1b[1;38;2;225;175;95m›\x1b[0m \x1b[1;38;2;240;235;225m{shown}\x1b[0m"), width),
        ));
        let hints = flashagent_tui::key_hints(&[("Enter", "send"), ("Esc", "cancel")], inner_w.saturating_sub(4));
        tail.push((LineKind::System, pad_box_row(&format!("   {hints}"), width)));
    }

    // The answer being written shows its own block cursor.
    let input_line_idx = typing_line_idx.unwrap_or(tail.len().saturating_sub(1));

    tail.push((
        LineKind::System,
        format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
    ));
    input_line_idx
}

/// `joined` is true when the work box is already the top of this block: its
/// `├─┤` is the seam, so drawing a `╭───╮` here would start a second box where
/// the first one has not ended.
fn append_composer(
    tail: &mut Vec<RenderLine>,
    st: &FrameState<'_>,
    width: usize,
    height: u16,
    t: u64,
    border_color: &str,
    joined: bool,
) -> (usize, Option<u16>) {
    let inner_w = width.saturating_sub(2);
    let reset = "\x1b[0m";
    if !joined {
        tail.push((
            LineKind::System,
            format!("{border_color}╭{}╮{reset}", "─".repeat(inner_w)),
        ));
    }

    // Attachments go above the line they go with.
    if !st.attachments.is_empty() {
        let listed = st.attachments.join("  ");
        let row = format!(
            " \x1b[38;2;145;205;140mattached\x1b[0m \x1b[38;2;160;165;180m{listed}\x1b[0m \x1b[38;2;100;95;90m· ctrl+z removes\x1b[0m"
        );
        tail.push((LineKind::System, pad_box_row(&row, width)));
    }

    let mut input_line_idx = tail.len();
    let mut text_cursor = Some(4);
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
    (input_line_idx, text_cursor)
}

fn append_channel_card(tail: &mut Vec<RenderLine>, st: &FrameState<'_>, width: usize, inner_w: usize, t: u64, joined: bool) -> usize {
    let prompt = st.channel_prompt.expect("channel card needs a prompt");
    let title = format!(" {} ", st.prompt_title);
    let title = title.as_str();
    // Same weight as an approval card: this replaces the binary.
    let border_color = anim::pulse(t, 1800, Rgb(225, 175, 95), Rgb(250, 215, 150)).fg();
    let reset = "\x1b[0m";
    let dash_w = inner_w.saturating_sub(visible_width(title) + 1);
    if !joined {
        tail.push((
            LineKind::System,
            format!("{border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_w)),
        ));
    }
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
    let input_line_idx = tail.len();
    tail.push((
        LineKind::System,
        format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
    ));
    input_line_idx
}

struct LayoutInputs<'a> {
    width: usize,
    height: u16,
    card_key: CardKey,
    card_start: usize,
    paint_start: usize,
    input_line_idx: usize,
    text_cursor: Option<u16>,
    border_color: &'a str,
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
            for (steer, _) in st.pending_steers {
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

        for command in st.queued_commands {
            let suffix = " \x1b[38;2;135;130;125m· runs when the turn ends\x1b[0m";
            let shown = flashagent_tui::truncate_middle(command, width.saturating_sub(34).max(10));
            tail.push((LineKind::User, format!(" \x1b[1;38;2;225;175;95m›\x1b[0m \x1b[1;38;2;240;235;225m{shown}\x1b[0m{suffix}")));
        }

        // Every child, pinned above the composer and below the steering above
        // it. These are a status, not transcript: F2 does not reach them, and
        // the mouse opens the line a phase replaced.
        //
        // Subagents and background commands share one box, because both are work
        // under way that the conversation does not show. A box rather than bare
        // lines: without its edges these read as part of the chat, and the eye
        // has to work out that they are not what was said.
        let box_t = anim::now_ms();
        let box_rows = work_box(
            st.agent_rows,
            st.agent_expanded,
            st.agent_details,
            st.background_rows,
            width,
            box_t,
        );
        // The rows a click may land on start under the box's own top edge. Kept
        // as a tail index: the box goes into the pinned block, whose rows the
        // composer under it shares, and `card_start` below is what turns this
        // into an offset inside that block.
        self.agent_first = if box_rows.is_empty() {
            None
        } else {
            Some(tail.len() + WORK_BOX_EDGES - 1)
        };

        // A blank row between the conversation and the composer or card under it.
        //
        // Not when the work box is pinned on top of the composer. Its `├─┤` is
        // already the separator, and a blank row in between is a blank row with
        // no sides drawn: it cuts a hole through the left and right walls, and
        // the composer below stops reading as the bottom half of the same block
        // and starts reading as a box of its own floating under another one.
        let joined = self.agent_first.is_some();
        let last_row = tail.last().or_else(|| settled.last()).map(|(_, text)| text.as_str());
        if !joined && last_row.is_some_and(|text| !flashagent_tui::strip_ansi(text).trim().is_empty()) {
            tail.push((LineKind::System, String::new()));
        }

        // The pinned block -- work box, separator, card -- is the floor of the
        // frame, not part of the conversation. `card_start` is taken before the
        // box is appended so that scrolling back can never take it off the
        // screen: the box is what the user is working next to.
        let card_start = tail.len();
        tail.extend(box_rows);
        // Where the card itself starts, one row set below the box's own top edge.
        // Painting, unfolding and trimming act on the card and leave the box be.
        let paint_start = tail.len();

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
        let mut input_line_idx;
        // Where the user types, when they type into the composer.
        let mut text_cursor: Option<u16> = None;

        if gate.pending().is_some() {
            input_line_idx = append_approval_card(&mut tail, gate, &st, width, height as usize, inner_w, &border_color, joined);
        } else if st.channel_prompt.is_some() {
            input_line_idx = append_channel_card(&mut tail, &st, width, inner_w, t, joined);
        } else if question_gate.pending().is_some() {
            input_line_idx = append_question_card(&mut tail, question_gate, &st, width, inner_w, &border_color, joined);
        } else if let Some(overlay) = overlay {
            // The composer turns into the open menu or screen.
            tail.extend(overlay.render(width, height as usize));
            input_line_idx = tail.len().saturating_sub(1);
        } else {
            let composer = append_composer(&mut tail, &st, width, height, t, &border_color, joined);
            input_line_idx = composer.0;
            text_cursor = composer.1;
        }

        // The sides of a boxed card match its top and bottom.
        if border_rgb != BORDER && !matches!(card_key, CardKey::Overlay(_)) {
            for (_, row) in tail[paint_start..].iter_mut() {
                *row = row.replace(BORDER_ESC, &border_color);
            }
        }
        // A new card unfolds from its title down.
        if self.card.0 != card_key {
            self.card = (card_key, t);
        }
        let total = tail.len() - paint_start;
        if card_key != CardKey::Composer && total > 2 {
            let k = anim::progress(self.card.1, t, UNFOLD_MS);
            let shown = ((total as f32 * k).ceil() as usize).clamp(2, total);
            if shown < total {
                let bottom = tail.len() - 1;
                tail.drain(paint_start + shown - 1..bottom);
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
        tail.push((LineKind::System, footer_hint(shown, &st, width, t)));
        append_tip_rows(&mut tail, &st, width);
        let awaiting_user = gate.pending().is_some() || question_gate.pending().is_some();
        tail.push((LineKind::System, footer_status(width, context_usage, &st, awaiting_user)));


        let open = |id: &str| st.agent_expanded.contains(&id) && st.agent_details.contains_key(id);
        self.agent_plan = st.agent_rows.to_vec();
        self.agent_open = st.agent_rows.iter().map(|(id, _)| id.clone()).filter(|id| open(id)).collect();
        self.paint_layout(&settled, &mut tail, LayoutInputs {
            width, height, card_key, card_start, paint_start, input_line_idx, text_cursor, border_color: &border_color,
        });
    }
    fn paint_layout(&mut self, settled: &[RenderLine], tail: &mut Vec<RenderLine>, layout: LayoutInputs<'_>) {
        let LayoutInputs { width, height, card_key, card_start, paint_start, mut input_line_idx, text_cursor, border_color } = layout;
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
        let card_rows = tail.len() - paint_start;
        if card_rows > height && card_rows > HEAD + END {
            let from = paint_start + HEAD;
            let to = (from + card_rows - height + 1).min(tail.len() - END);
            if to > from + 1 {
                let hidden = to - from;
                let note = format!(
                    " \x1b[38;2;135;130;125m\u{2026} {} not shown \u{b7} a taller window shows them\x1b[0m",
                    flashagent_tui::plural(hidden, "row", "rows")
                );
                let marker = pad_box_row(&note, width).replace(BORDER_ESC, border_color);
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
    // A row of the room goes to the line that says the view is scrolled, so the
    // last line that can be read is one row up from the bottom. With no room for
    // that line there is nothing to read back into: the view is pinned, and an
    // accepted offset that changes nothing on screen is an offset the user cannot
    // undo by scrolling.
    self.max_scroll = scroll_max(chat_len, chat_room);
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
        // The last row of the conversation, and with the box pinned directly
        // under it that is also the row directly above the box -- the same
        // place, so it says there is more above the block the user works in,
        // not above some composer floating in the middle of the chat.
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
    let scrolled_marker = usize::from(self.scroll_offset > 0);
    self.chat_view = (first_chat_row + cut, chat_rows.saturating_sub(scrolled_marker).saturating_sub(cut));
    self.agent_screen.clear();
    // Where the pinned rows landed, for a click to find: inside the work box,
    // under its top edge, one row per child plus a detail row for one the mouse
    // has opened. `first` is where the first child sits in the tail, and the box
    // now lives in the bottom block, so its row is counted from where the block
    // starts on screen -- the rows under the box belong to no child.
    if let Some(first) = self.agent_first {
        let base = chat_rows + first - card_start;
        let mut opened = 0usize;
        for (i, (id, _)) in self.agent_plan.iter().enumerate() {
            let at = base + i + opened;
            if let Some(screen) = at.checked_sub(cut) {
                self.agent_screen.push((screen, id.clone()));
            }
            if self.agent_open.contains(id) {
                opened += 1;
            }
        }
    }
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
        self.chat.set_awaiting_user(cx.gate.pending().is_some() || cx.question_gate.pending().is_some());
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
        // A model server on this machine or the local network: a time to first
        // token is about its hardware there, and about a queue and a network on a
        // hosted API.
        let nearby_server = crate::warm::nearby_server(&cx.source.0.endpoint(), cx.source.discovery_kind());
        // An estimate of reading the prompt means nothing while no server answers.
        let live_prefill = self
            .token_tracker
            .live_prefill_status(nearby_server)
            .filter(|_| self.announced_mood != MascotMood::Offline);
        let ttft_display = self.token_tracker.ttft_display();
        let tg_speed = self.token_tracker.tg_3s();
        let draft = self.token_tracker.draft_display();
        let config = &self.config;
        let cache_display = self.token_tracker.cache_display();
        // Only when the backend priced something: a local server bills nothing
        // and says nothing, and the bar keeps quiet rather than showing $0.00.
        let cost_display = self.cost.summary();

        // Pinned above the composer, below the steering: the rows are built here
        // rather than in `draw` so a click can be matched against them.
        let pin_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100);
        // The room behind the box's own two columns of indent: `work_box` is
        // `pin_width` wide, `pad_box_row` keeps `pin_width - 2` inside the frame,
        // and the indent spends two of those, leaving `pin_width - 4`. Both
        // kinds of row in the box are measured against that same room -- a
        // child's row is handed the two columns of indent back because
        // `AgentTree::line` counts its `width` as the whole row, mark included,
        // while a background row is measured without them.
        let box_inner = pin_width.saturating_sub(4);
        // Work that is running says so from the clock, not from how often a frame
        // happened to be drawn: two agents listed at once must not tick in
        // lockstep just because they were reported in the same event.
        let mark = anim::spinner(anim::now_ms());
        let agent_rows = self.agents.pinned_rows(box_inner.saturating_add(2), mark);
        let agent_details = self.agents.details(box_inner.saturating_add(2));
        // Background commands join the subagents in one box, so each says what
        // it is and for how long, cut to fit like a child's row.
        let background_rows: Vec<String> = cx
            .tools_arc
            .shells()
            .tasks()
            .into_iter()
            .filter(|t| matches!(t.state, flashagent_tools::TaskState::Running))
            .map(|t| background_row(&t.command, &flashagent_tools::shell::format_elapsed(t.elapsed), box_inner, mark))
            .collect();
        let expanded: Vec<&str> = self.expanded_agents.iter().map(String::as_str).collect();
        self.renderer.frame(
            &self.chat,
            cx.gate,
            cx.question_gate,
            self.overlay.as_ref(),
            autocomplete,
            &self.context_usage,
            FrameState {
                cost_display: cost_display.as_deref(),
                input: &self.input,
                provider: (self.config.active_profile().name.as_str(), self.current_model.as_str()),
                history_search: self
                    .history_search
                    .as_ref()
                    .map(|h| (h.query.as_str(), h.found(&self.input_history))),
                mode: cx.perm.state().mode(),
                is_goal_active: self.goal_state.is_some(),
                goal_progress: goal_progress.as_deref(),
                tip_lines: config.show_tips.then_some(cx.tip_lines.as_slice()),
                reasoning_expand: ReasoningExpansion { all: self.all_expanded, last: self.last_expanded },
                tick_n: self.tick_n,
                running: self.running,
                tokens_per_sec: config.show_tokens.then_some(tg_speed),
                draft_acceptance: draft.as_deref().filter(|_| config.show_tokens),
                turn_tokens: config.show_tokens.then(|| self.turn_token_phrase()).flatten(),
                confirm_selection: self.confirm_select.choice(),
                question_state: Some(&self.question_ui_state),
                custom_placeholder: self.custom_placeholder.as_deref(),
                suggested_prompt: self.suggested_prompt.as_deref(),
                copy_toast: if config.show_toasts { self.copy_toast.as_ref().map(|(msg, _)| msg.as_str()) } else { None },
                prefill_status: if config.show_ttft { live_prefill.as_deref() } else { None },
                ttft_display: if config.show_ttft { ttft_display.as_deref() } else { None },
                cache_display: if config.show_ttft { cache_display.as_deref() } else { None },
                nearby_server,
                background: self.background.as_ref().map(|b| b.text.as_str()),
                channel_prompt: channel_prompt.as_deref(),
                prompt_title: if self.uninstall_confirm { UNINSTALL_TITLE } else { "Switch release channel" },
                turn_phase: self.running.then_some(&self.turn_phase),
                attachments: &attachment_labels,
                background_style: self.background.as_ref().map_or(NoticeStyle::FULL, BackgroundNotice::style),
                context_warn_threshold: config.context_warn_threshold,
                context_reported: self.current_context.is_some(),
                compact_status: self.compact_status.as_deref(),
                pending_steers: &self.pending_steers,
                queued_commands: &self.queued_commands,
                agent_rows: &agent_rows,
                agent_expanded: &expanded,
                agent_details: &agent_details,
                background_rows: &background_rows,
                background_tasks: cx.tools_arc.shells().running_count(),
                shell_running: self.running && cx.tools_arc.shells().foreground_running(),
                composer_flash,
            },
        );
    }

    /// What this turn has generated: the parent's own output and its children's,
    /// summed over every request the turn made. A turn with tools is many
    /// requests, and each one is added rather than replacing what came before.
    pub(crate) fn turn_token_phrase(&self) -> Option<String> {
        let out = self.token_tracker.turn_output();
        if out == 0 {
            return None;
        }
        // Through the shared formatter rather than a second one: this line had its
        // own `k`-only rule, so a turn that spent four million tokens read as
        // "4621.2k out" while `/goal` said "4.6M" for the same work.
        Some(format!("{} out", flashagent_tui::goal::human_count(out as i64)))
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

/// How far back the view may go: every row of the room but the one the scrolled
/// marker takes. With no room for that marker the view is pinned -- an offset
/// that changes nothing on screen, and that no marker explains, is an offset the
/// user can neither see nor scroll back from.
fn scroll_max(chat_len: usize, chat_room: usize) -> usize {
    if chat_room > 1 { chat_len.saturating_sub(chat_room - 1) } else { 0 }
}

#[cfg(test)]
mod footer_tests {
    use super::*;

    // These cover `context_gauges` and `scroll_max`, not the wiring: nothing here
    // builds a `FrameState`, so breaking `footer_status` into always reporting
    // the window would leave them green. The wiring was checked by running the
    // real binary; a test for it needs a `FrameState` that can be built cheaply.

    /// A room of one row cannot hold the marker, so nothing may be scrolled back.
    /// Taking an offset here would move no line on screen and say nothing about
    /// it, leaving the view scrolled with no way back.
    #[test]
    fn a_room_too_small_for_the_marker_cannot_be_scrolled_back() {
        assert_eq!(scroll_max(500, 0), 0);
        assert_eq!(scroll_max(500, 1), 0);
    }

    /// With room for the marker, every row above it is reachable.
    #[test]
    fn a_scrolled_view_stops_one_row_short_of_the_end() {
        assert_eq!(scroll_max(10, 2), 9);
        assert_eq!(scroll_max(2, 5), 0, "a short conversation has nothing to scroll");
    }

    /// 4.7K of a guessed 128K: a percentage here would be about nothing.
    #[test]
    fn an_unreported_window_shows_a_count_and_no_percentage() {
        let mut usage = ContextUsage::new(128_000);
        usage.system_tokens = 4_700;
        let gauges = context_gauges(&usage, false, 70);
        for g in &gauges {
            let plain = flashagent_tui::strip_ansi(g);
            assert!(plain.contains("used"), "{plain:?} names it as a count");
            assert!(!plain.contains('%'), "{plain:?} keeps a guess out of it");
            assert!(!plain.contains("128"), "{plain:?} does not print the guess");
        }
    }

    /// Once the server answers, the window is a fact and the bar is honest.
    #[test]
    fn a_reported_window_shows_the_bar_and_the_percentage() {
        let mut usage = ContextUsage::new(1_000_000);
        usage.system_tokens = 100_000;
        let gauges = context_gauges(&usage, true, 97);
        let plain = flashagent_tui::strip_ansi(&gauges[0]);
        assert!(plain.contains('%'), "{plain:?}");
        assert!(plain.contains("1M") || plain.contains("1.0M"), "{plain:?}");
        assert!(!plain.contains("used"), "{plain:?}");
    }

    /// The warning still wins over the plain gauge once there is a real number.
    #[test]
    fn a_reported_window_that_is_full_says_what_to_do() {
        let mut usage = ContextUsage::new(100_000);
        usage.system_tokens = 95_000;
        let gauges = context_gauges(&usage, true, 70);
        let plain = flashagent_tui::strip_ansi(&gauges[0]);
        assert!(plain.contains("/compact"), "{plain:?}");
        assert!(plain.contains("95%"), "{plain:?}");
    }

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
