use super::*;

/// Whether this person has used FlashAgent before.
///
/// A saved session is the evidence: the config file exists from the moment
/// the setup wizard finishes, so it cannot answer this, but a session is only
/// written once a conversation has happened.
pub fn been_here_before() -> bool {
    let Some(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok() else {
        return false;
    };
    let sessions = std::path::PathBuf::from(home).join(".flashagent").join("sessions");
    std::fs::read_dir(sessions)
        .map(|mut d| d.any(|e| e.is_ok()))
        .unwrap_or(false)
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
pub(crate) fn mascot_grid(blink: bool, mood: MascotMood) -> [&'static str; 12] {
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
pub(crate) fn breath_level(tick_n: usize) -> u8 {
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
    // "Welcome back" to someone who has never been here is the kind of
    // detail that tells a person the whole thing was assembled carelessly.
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
            center_cell(&format!("{M3_TXT_B}{greeting} {M3_ICE}{username_clean}{M3_TXT_B}!{RESET}"), w1),
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
        lines.push((LineKind::System, format!("{M3_BRD}│{RESET}{}{M3_BRD}│{RESET}", center_cell(&format!("{M3_TXT_B}{greeting} {M3_ICE}{username_clean}{M3_TXT_B}!{RESET}"), inner_w))));

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
