use super::*;

/// A saved session is the evidence; the config exists as soon as setup finishes.
pub fn been_here_before() -> bool {
    let Some(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok() else {
        return false;
    };
    let sessions = std::path::PathBuf::from(home).join(".flashagent").join("sessions");
    std::fs::read_dir(sessions)
        .map(|mut d| d.any(|e| e.is_ok()))
        .unwrap_or(false)
}

/// One struct instead of eleven positional arguments.
#[derive(Debug, Clone)]
pub struct WelcomeCard<'a> {
    pub model: &'a str,
    /// Already shortened for display.
    pub cwd: &'a str,
    /// Loaded into the prompt.
    pub memory_docs: usize,
    pub thinking: Option<&'a str>,
    /// E.g. "64k ctx".
    pub context_window: Option<&'a str>,
    /// In cells.
    pub width: usize,
    pub height: usize,
    /// The mascot breathes and blinks by it.
    pub tick: usize,
    /// Settings -> UI.
    pub show_mascot: bool,
    /// Reflects the connection.
    pub mood: MascotMood,
}

impl Default for WelcomeCard<'_> {
    fn default() -> Self {
        Self {
            model: "",
            cwd: "",
            memory_docs: 0,
            thinking: None,
            context_window: None,
            width: 80,
            height: 24,
            tick: 0,
            show_mascot: true,
            mood: MascotMood::Checking,
        }
    }
}

pub(crate) fn pad_cell(s: &str, width: usize) -> String {
    let vis = visible_width(s);
    if vis >= width {
        clip_ansi(s, width)
    } else {
        let pad = width - vis;
        format!("{s}{}", " ".repeat(pad))
    }
}

pub(crate) fn center_cell(s: &str, width: usize) -> String {
    let vis = visible_width(s);
    if vis >= width {
        clip_ansi(s, width)
    } else {
        let left = (width - vis) / 2;
        let right = width - vis - left;
        format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
    }
}

pub(crate) const M3_PRI_B: &str = "\x1b[1;38;2;138;180;248m";
pub(crate) const M3_PRI: &str = "\x1b[38;2;138;180;248m";
pub(crate) const M3_LGT: &str = "\x1b[38;2;168;199;250m";
pub(crate) const M3_LGT_B: &str = "\x1b[1;38;2;168;199;250m";
pub(crate) const M3_ICE: &str = "\x1b[38;2;194;231;255m";
pub(crate) const M3_BRD: &str = "\x1b[38;2;75;99;130m";
pub(crate) const M3_MUT: &str = "\x1b[38;2;155;165;180m";
pub(crate) const M3_TXT: &str = "\x1b[38;2;235;240;250m";
pub(crate) const M3_TXT_B: &str = "\x1b[1;38;2;235;240;250m";
pub(crate) const RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MascotMood {
    /// Still waiting for the server's first answer.
    Checking,
    Happy,
    Offline,
}

/// 16×12 pixels, two per terminal row via `▀` (foreground upper, background
/// lower): a cell is twice as tall as wide, and one pixel per cell stretched
/// the old mascot into a spiky kite.
///
/// `.` transparent · `#` body · `o` highlight · `w` eye · `k` mouth
pub(crate) fn mascot_grid(blink: bool, mood: MascotMood) -> [&'static str; 12] {
    // Blinking closes the upper half of each eye. Offline keeps them shut.
    let (eyes_top, eyes_bottom) = if blink || mood == MascotMood::Offline {
        (".##############.", ".###kk####kk###.")
    } else {
        (".###ww####ww###.", ".###ww####ww###.")
    };
    // Smile when the server answered, a flat line when not, a dot while asking.
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

/// Triangle wave 0..=5, one cycle per ~3 s at the 80 ms tick.
pub(crate) fn breath_level(tick_n: usize) -> u8 {
    const PERIOD: usize = 38;
    let half = PERIOD / 2;
    let phase = tick_n % PERIOD;
    let up = if phase < half { phase } else { PERIOD - phase - 1 };
    (up.min(half - 1) * 6 / half) as u8
}

/// The card is rebuilt only when this says so, not 12 times a second.
pub fn mascot_needs_repaint(tick_n: usize) -> bool {
    if !crate::anim::enabled() {
        return false;
    }
    let blink = |t: usize| (t % 50 == 46) || (t % 50 == 47);
    let prev = tick_n.wrapping_sub(1);
    blink(tick_n) != blink(prev) || breath_level(tick_n) != breath_level(prev)
}

/// `None` when transparent. `breath` lifts the highlight; `offline` greys the body.
pub(crate) fn mascot_color(px: u8, breath: u8, offline: bool) -> Option<(u8, u8, u8)> {
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
        // Halfway to grey: clearly unhealthy, still readable.
        let (r, g, b) = color;
        let grey = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        let mix = |c: u8| ((c as u16 + grey as u16) / 2) as u8;
        return Some((mix(r), mix(g), mix(b)));
    }
    Some(color)
}


/// Blinks every ~4 s, breathes continuously. Every line is exactly 16 columns,
/// transparent pixels included, so centring keeps rows aligned.
pub fn mascot_swift_lines_mood(tick_n: usize, mood: MascotMood) -> [String; 6] {
    // Still, eyes open, with motion off.
    let tick_n = if crate::anim::enabled() { tick_n } else { 0 };
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

/// Drops whole parts instead of clipping mid-word. Parts are `(plain,
/// styled)`; the plain form is measured.
pub fn fit_parts(parts: &[(String, String)], separator: &str, width: usize) -> String {
    let sep_w = visible_width(separator);
    let mut out = String::new();
    let mut used = 0usize;
    for (plain, styled) in parts {
        let cost = plain.chars().count() + if out.is_empty() { 0 } else { sep_w };
        // Stops at the first that does not fit: parts are in priority order, and
        // "64k" without its model says nothing.
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

/// Drops from the middle: the last hint is how to leave, and a narrow card
/// that does not say how to close it is a trap.
pub fn fit_hints(parts: &[(String, String)], separator: &str, width: usize) -> String {
    let Some((last_plain, last_styled)) = parts.last() else { return String::new() };
    let sep_w = visible_width(separator);
    let joined = fit_parts(parts, separator, width);
    if visible_width(&joined) >= plain_width(parts, sep_w) {
        return joined;
    }
    // Keep the way out, then as many of the others from the front as fit.
    let mut kept: Vec<&(String, String)> = Vec::new();
    let mut used = last_plain.chars().count();
    for part in &parts[..parts.len() - 1] {
        let cost = part.0.chars().count() + sep_w;
        if used + cost > width {
            break;
        }
        used += cost;
        kept.push(part);
    }
    let mut out = String::new();
    for part in kept {
        out.push_str(&part.1);
        out.push_str(separator);
    }
    out.push_str(last_styled);
    out
}

fn plain_width(parts: &[(String, String)], sep_w: usize) -> usize {
    let text: usize = parts.iter().map(|(plain, _)| plain.chars().count()).sum();
    text + sep_w * parts.len().saturating_sub(1)
}

/// Composer (3), hint line, tip and status line. The card gets the rest.
const CHROME_ROWS: usize = 7;

/// Shapes are tried richest first; a short window gets a smaller card rather
/// than one whose top scrolled away.
#[derive(Debug, Clone, Copy)]
struct Shape {
    /// Needs width as well as height.
    two_column: bool,
    /// All six, the head, or none.
    mascot_rows: usize,
    /// Or a single line of hints.
    quick_commands: bool,
}

impl Shape {
    const ALL: [Shape; 5] = [
        Shape { two_column: true, mascot_rows: 6, quick_commands: true },
        Shape { two_column: false, mascot_rows: 6, quick_commands: true },
        Shape { two_column: false, mascot_rows: 3, quick_commands: true },
        Shape { two_column: false, mascot_rows: 0, quick_commands: true },
        Shape { two_column: false, mascot_rows: 0, quick_commands: false },
    ];
}

pub fn welcome_card(card: &WelcomeCard<'_>) -> Vec<RenderLine> {
    let budget = card.height.saturating_sub(CHROME_ROWS);
    for shape in Shape::ALL {
        if shape.two_column && card.width < 56 {
            continue;
        }
        let lines = build_card(card, shape);
        if lines.len() <= budget {
            return with_gap(lines, budget);
        }
    }
    // Nothing fits: one line saying what this is and where it runs.
    let single = format!(
        "{M3_PRI_B}>_ FlashAgent{RESET} {M3_MUT}{}{RESET}",
        truncate_middle(card.cwd, card.width.saturating_sub(16).max(8))
    );
    if budget == 0 {
        Vec::new()
    } else {
        vec![(LineKind::System, single)]
    }
}

/// When there is room for it.
fn with_gap(mut lines: Vec<RenderLine>, budget: usize) -> Vec<RenderLine> {
    if lines.len() < budget {
        lines.push((LineKind::System, String::new()));
    }
    lines
}

fn build_card(card: &WelcomeCard<'_>, shape: Shape) -> Vec<RenderLine> {
    let &WelcomeCard {
        model,
        cwd,
        memory_docs,
        thinking,
        context_window,
        width,
        height: _,
        tick: tick_n,
        show_mascot,
        mood,
    } = card;
    let mut lines = Vec::new();
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "Developer".to_string());
    let username_clean = if username.chars().count() > 16 {
        truncate_middle(&username, 16)
    } else {
        username
    };
    let greeting = if been_here_before() { "Welcome back" } else { "Welcome" };

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

    if shape.two_column {
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
        // The model and its settings are on the right; the left says where.
        let cwd_meta = format!("{M3_MUT}{}{RESET}", truncate_middle(&cwd_clean, w1.saturating_sub(4)));

        let left_lines = [
            center_cell(&format!("{M3_TXT_B}{greeting} {M3_ICE}{username_clean}{M3_TXT_B}!{RESET}"), w1),
            "".to_string(),
            center_cell(&mascot[0], w1),
            center_cell(&mascot[1], w1),
            center_cell(&mascot[2], w1),
            center_cell(&mascot[3], w1),
            center_cell(&mascot[4], w1),
            center_cell(&mascot[5], w1),
            "".to_string(),
            center_cell(&cwd_meta, w1),
            "".to_string(),
        ];

        let model_val = truncate_middle(model, w2.saturating_sub(13));
        let ctx_val = truncate_middle(context_window.unwrap_or("128k capacity (local)"), w2.saturating_sub(13));
        let memory_val = match memory_docs {
            0 => format!("{M3_MUT}no rule files yet{RESET}"),
            1 => format!("{M3_TXT}1{RESET} {M3_MUT}rule file{RESET}"),
            n => format!("{M3_TXT}{n}{RESET} {M3_MUT}rule files{RESET}"),
        };

        let right_top = [
            format!(" {M3_MUT}Model:   {RESET} {M3_TXT_B}{model_val}{RESET}"),
            format!(" {M3_MUT}Context: {RESET} {M3_ICE}{ctx_val}{RESET}"),
            // The mode is on the status line under the prompt.
            format!(" {M3_MUT}Effort:  {RESET} {M3_LGT}{th_short}{RESET} {M3_MUT}(F4){RESET}"),
            format!(" {M3_MUT}Memory:  {RESET} {memory_val}"),
            format!(" {M3_MUT}Config:  {RESET} {M3_ICE}Tab{RESET} {M3_MUT}settings{RESET}"),
        ];

        let right_bot = [
            format!(" {M3_PRI_B}/goal <task>{RESET} {M3_MUT}for autonomy{RESET}"),
            format!(" {M3_ICE}Ctrl+K{RESET} {M3_MUT}commands{RESET} {M3_MUT}·{RESET} {M3_ICE}Ctrl+D{RESET} {M3_MUT}quit{RESET}"),
            format!(" {M3_ICE}F1{RESET} {M3_MUT}context{RESET} {M3_MUT}·{RESET} {M3_ICE}F2{RESET} {M3_MUT}verbose{RESET} {M3_MUT}·{RESET} {M3_ICE}F3{RESET} {M3_MUT}model{RESET}"),
            format!(" {M3_ICE}F4{RESET} {M3_MUT}effort{RESET} {M3_MUT}·{RESET} {M3_ICE}Ctrl+V{RESET} {M3_MUT}paste image{RESET}"),
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
        // Narrow or short screens (< 56 cols or < 18 rows).
        let inner_w = width.saturating_sub(2).min(74);
        let t_left_vis = format!(">_ FlashAgent {ver_disp}");
        let d1 = inner_w.saturating_sub(t_left_vis.chars().count() + 3);
        let t_left_colored = format!("{M3_PRI_B}>_ FlashAgent{RESET} {M3_LGT}{ver_disp}{RESET}");
        let top = format!("{M3_BRD}╭─{RESET} {t_left_colored} {M3_BRD}{}╮{RESET}", "─".repeat(d1));
        let bot = format!("{M3_BRD}╰{}╯{RESET}", "─".repeat(inner_w));

        // Whatever no longer fits is dropped whole.
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
        lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(&format!("{M3_TXT_B}{greeting} {M3_ICE}{username_clean}{M3_TXT_B}!{RESET}"), inner_w))));

        // The top of the same sprite; an old hand-drawn "mini" version changed
        // species when the window got short.
        if show_mascot {
            for m in mascot.iter().take(shape.mascot_rows) {
                lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(m, inner_w))));
            }
        }

        lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(&left_meta, inner_w))));
        lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(&cwd_meta, inner_w))));

        if shape.quick_commands {
            let div_cmd = format!("{M3_BRD}├─{RESET} {M3_LGT_B}Quick Commands{RESET} {M3_BRD}{}┤{RESET}", "─".repeat(inner_w.saturating_sub(17)));
            lines.push((LineKind::System, div_cmd));
            lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", pad_cell(&format!(" {M3_PRI_B}/goal <task>{RESET} {M3_MUT}for autonomy{RESET}"), inner_w))));
            let hints = fit_parts(
                &[
                    ("Tab settings".into(), format!("{M3_ICE}Tab{RESET} {M3_MUT}settings{RESET}")),
                    ("Ctrl+K commands".into(), format!("{M3_ICE}Ctrl+K{RESET} {M3_MUT}commands{RESET}")),
                    ("Ctrl+D quit".into(), format!("{M3_ICE}Ctrl+D{RESET} {M3_MUT}quit{RESET}")),
                ],
                &format!(" {M3_MUT}\u{b7}{RESET} "),
                inner_w.saturating_sub(2),
            );
            lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", pad_cell(&format!(" {hints}"), inner_w))));
        } else {
            lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", pad_cell(&format!(" {M3_PRI_B}/goal{RESET} {M3_MUT}·{RESET} {M3_ICE}Ctrl+K{RESET} {M3_MUT}commands{RESET} {M3_MUT}·{RESET} {M3_ICE}Ctrl+D{RESET} {M3_MUT}quit{RESET}"), inner_w))));
        }
        lines.push((LineKind::System, bot));
    }

    lines
}

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
    let msg = if inner_text_w >= 86 {
        format!(
            "To resume: \x1b[1;38;2;240;235;225mflashagent --continue\x1b[0m here, or \x1b[1;38;2;240;235;225m{resume_cmd}\x1b[0m"
        )
    } else if inner_text_w >= 66 {
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
mod hint_tests {
    use super::*;

    fn parts(names: &[&str]) -> Vec<(String, String)> {
        names.iter().map(|n| (n.to_string(), format!("\x1b[2m{n}\x1b[0m"))).collect()
    }

    #[test]
    fn the_way_out_is_the_last_hint_to_be_dropped() {
        let hints = parts(&["↑/↓ — select", "s — summary", "d — forget", "esc — close"]);
        let wide = strip_ansi(&fit_hints(&hints, " · ", 80));
        assert_eq!(wide, "↑/↓ — select · s — summary · d — forget · esc — close");

        // Too narrow: the middle goes, the way out stays.
        let narrow = strip_ansi(&fit_hints(&hints, " · ", 30));
        assert!(narrow.ends_with("esc — close"), "{narrow:?}");
        assert!(narrow.chars().count() <= 30, "{narrow:?}");
        assert!(narrow.starts_with("↑/↓ — select"), "the first hint should survive too: {narrow:?}");

        let tiny = strip_ansi(&fit_hints(&hints, " · ", 12));
        assert_eq!(tiny, "esc — close");
    }
}
