//! Colour themes.
//!
//! Every card, spinner and border in FlashAgent is written in one palette —
//! 24-bit colours spelled out where they are used. A theme is not a second
//! palette to keep in step with the first: it is a transformation applied to
//! the finished frame, one step before it is printed. A colour added
//! anywhere in the app is themed by that alone, with nothing to register.
//!
//! The cost is one pass over the frame's escape sequences, and only when a
//! theme other than [`ColorTheme::Dark`] is chosen — `Dark` is the palette as
//! written, so it is returned untouched.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU8, Ordering};

pub use flashagent_core::ColorTheme;

/// The active theme, as its index in [`ColorTheme::all`].
static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// Choose the theme every later frame is printed in.
pub fn set(theme: ColorTheme) {
    let index = ColorTheme::all().iter().position(|t| *t == theme).unwrap_or(0);
    ACTIVE.store(index as u8, Ordering::Relaxed);
}

/// The theme frames are being printed in.
pub fn active() -> ColorTheme {
    let all = ColorTheme::all();
    all[(ACTIVE.load(Ordering::Relaxed) as usize).min(all.len() - 1)]
}

/// Recolour a finished frame for the active theme.
///
/// Borrows the frame unchanged under `Dark`, which is the common case, so the
/// default theme costs a comparison and nothing else.
pub fn recolor(frame: &str) -> Cow<'_, str> {
    recolor_for(active(), frame)
}

/// As [`recolor`], for a named theme. Separate so it can be tested without
/// touching the global choice.
pub fn recolor_for(theme: ColorTheme, frame: &str) -> Cow<'_, str> {
    if theme == ColorTheme::Dark || frame.is_empty() {
        return Cow::Borrowed(frame);
    }
    let mut out = String::with_capacity(frame.len());
    let mut rest = frame;
    while let Some(at) = rest.find("\x1b[") {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        match sgr_sequence(rest) {
            Some((params, len)) => {
                out.push_str(&recolor_sgr(theme, params));
                rest = &rest[len..];
            }
            None => {
                // Cursor motion, a clear, a half-written escape at a clip
                // boundary: copied across as it stands.
                out.push_str("\x1b[");
                rest = &rest[2..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// The parameters of the `\x1b[...m` at the start of `s`, and its length.
///
/// Only a finished colour-and-style sequence is claimed: anything else,
/// including an escape cut in half by clipping the line to the terminal
/// width, is left for the caller to copy.
fn sgr_sequence(s: &str) -> Option<(&str, usize)> {
    let body = s.strip_prefix("\x1b[")?;
    let end = body.find(|c: char| !(c.is_ascii_digit() || c == ';'))?;
    if body.as_bytes()[end] != b'm' {
        return None;
    }
    Some((&body[..end], 2 + end + 1))
}

/// Rewrite one `\x1b[...m`, recolouring the colours in it and keeping
/// everything else — a colour is often written together with bold
/// (`\x1b[1;38;2;225;175;95m`), and losing the bold would change the picture
/// as much as losing the colour.
fn recolor_sgr(theme: ColorTheme, params: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut parts = params.split(';').peekable();
    while let Some(token) = parts.next() {
        let ground = match token {
            "38" => Some(Ground::Fore),
            "48" => Some(Ground::Back),
            _ => None,
        };
        // 38 and 48 also introduce the 256-colour form (`5;N`), which names a
        // colour in the terminal's own palette and is already themed by it.
        let is_truecolor = ground.is_some() && parts.peek() == Some(&"2");
        if !is_truecolor {
            kept.push(token.to_string());
            continue;
        }
        let ground = ground.expect("checked just above");
        parts.next();
        let mut channel = || parts.next().and_then(|c| c.parse::<u8>().ok());
        match (channel(), channel(), channel()) {
            (Some(r), Some(g), Some(b)) => kept.extend(painted(theme, ground, (r, g, b))),
            // Not a colour after all; put back what was read.
            _ => kept.push(format!("{}2", if ground == Ground::Fore { "38;" } else { "48;" })),
        }
    }
    format!("\x1b[{}m", kept.join(";"))
}

/// The parameters that paint `rgb` in `theme`.
fn painted(theme: ColorTheme, ground: Ground, rgb: (u8, u8, u8)) -> Vec<String> {
    match theme {
        ColorTheme::Ansi16 => {
            let (code, bright) = nearest_ansi(rgb);
            let base = match ground {
                Ground::Fore => 30,
                Ground::Back => 40,
            };
            // 90-97 and 100-107 are the bright halves of the same eight.
            vec![(base + code as u16 + if bright { 60 } else { 0 }).to_string()]
        }
        _ => {
            let (r, g, b) = transform(theme, rgb);
            let lead = match ground {
                Ground::Fore => "38",
                Ground::Back => "48",
            };
            vec![lead.to_string(), "2".to_string(), r.to_string(), g.to_string(), b.to_string()]
        }
    }
}

/// Whether a colour paints the text or what is behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ground {
    Fore,
    Back,
}

/// How bright a colour reads, 0..=255. The weights are the usual ones for
/// how much each channel contributes to perceived brightness.
fn luminance((r, g, b): (u8, u8, u8)) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000).min(255) as u8
}

fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Move one colour into `theme`.
fn transform(theme: ColorTheme, (r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    match theme {
        ColorTheme::Dark | ColorTheme::Ansi16 => (r, g, b),
        // Cooled towards blue: the warm greys and golds keep their shape and
        // their brightness order, but the picture reads as night.
        ColorTheme::Midnight => (
            clamp(r as i32 * 78 / 100),
            clamp(g as i32 * 88 / 100),
            clamp(b as i32 * 115 / 100 + 14),
        ),
        // Pushed away from mid-grey: dim text gets dimmer, bright text
        // brighter, so the two never blur together on a washed-out screen.
        ColorTheme::HighContrast => {
            let stretch = |c: u8| clamp(128 + (c as i32 - 128) * 165 / 100);
            (stretch(r), stretch(g), stretch(b))
        }
        // Brightness only. Dim text would vanish once its colour is gone, so
        // the range is lifted off black first.
        ColorTheme::Monochrome => {
            let grey = clamp(40 + luminance((r, g, b)) as i32 * 215 / 255);
            (grey, grey, grey)
        }
    }
}

/// The nearest of the terminal's own sixteen colours: which of the eight, and
/// whether it is the bright half.
///
/// The eight are compared at their usual values; a terminal is free to paint
/// them differently, which is the point of choosing this theme.
fn nearest_ansi(rgb: (u8, u8, u8)) -> (u8, bool) {
    const BASE: [(u8, u8, u8); 8] = [
        (0, 0, 0),       // black
        (170, 0, 0),     // red
        (0, 170, 0),     // green
        (170, 85, 0),    // yellow
        (0, 0, 170),     // blue
        (170, 0, 170),   // magenta
        (0, 170, 170),   // cyan
        (170, 170, 170), // white
    ];
    const BRIGHT: [(u8, u8, u8); 8] = [
        (85, 85, 85),
        (255, 85, 85),
        (85, 255, 85),
        (255, 255, 85),
        (85, 85, 255),
        (255, 85, 255),
        (85, 255, 255),
        (255, 255, 255),
    ];
    let distance = |a: (u8, u8, u8)| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, rgb.0) + d(a.1, rgb.1) + d(a.2, rgb.2)
    };
    let (mut best, mut best_bright, mut best_distance) = (0u8, false, i32::MAX);
    for (i, colour) in BASE.iter().enumerate() {
        let d = distance(*colour);
        if d < best_distance {
            (best, best_bright, best_distance) = (i as u8, false, d);
        }
    }
    for (i, colour) in BRIGHT.iter().enumerate() {
        let d = distance(*colour);
        if d < best_distance {
            (best, best_bright, best_distance) = (i as u8, true, d);
        }
    }
    (best, best_bright)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLD: &str = "\x1b[38;2;225;175;95m";

    #[test]
    fn the_written_palette_is_printed_as_written() {
        let frame = format!("{GOLD}FlashAgent\x1b[0m");
        assert!(matches!(recolor_for(ColorTheme::Dark, &frame), Cow::Borrowed(_)), "Dark must not copy the frame");
        assert_eq!(recolor_for(ColorTheme::Dark, &frame), frame);
    }

    #[test]
    fn only_the_colours_are_touched() {
        // Cursor motion, reverse video and reset must survive a theme, or the
        // frame lands in the wrong place on screen.
        let frame = format!("\x1b[2J\x1b[H{GOLD}hi\x1b[0m\x1b[7m!\x1b[39m\x1b[12A");
        let themed = recolor_for(ColorTheme::Monochrome, &frame);
        for untouched in ["\x1b[2J", "\x1b[H", "\x1b[0m", "\x1b[7m", "\x1b[39m", "\x1b[12A"] {
            assert!(themed.contains(untouched), "{untouched} was lost: {themed:?}");
        }
        assert!(themed.contains("hi") && themed.contains('!'));
        assert!(!themed.contains(GOLD), "the gold should have been recoloured");
    }

    #[test]
    fn monochrome_leaves_only_greys_and_keeps_them_apart() {
        let dim = transform(ColorTheme::Monochrome, (110, 105, 100));
        let bright = transform(ColorTheme::Monochrome, (240, 235, 225));
        assert_eq!(dim.0, dim.1, "a grey has three equal channels");
        assert_eq!(dim.1, dim.2);
        assert!(bright.0 > dim.0, "brighter text stays brighter");
        assert!(dim.0 >= 40, "dim text must not sink into the background");
    }

    #[test]
    fn high_contrast_spreads_the_palette_and_midnight_cools_it() {
        let (r, _, b) = transform(ColorTheme::Midnight, (200, 180, 150));
        assert!(b > 150 && r < 200, "midnight cools warm colours");
        let dim = transform(ColorTheme::HighContrast, (110, 105, 100));
        let bright = transform(ColorTheme::HighContrast, (240, 235, 225));
        assert!(dim.0 < 110 && bright.0 > 240, "the two ends move apart");
    }

    #[test]
    fn the_sixteen_colour_theme_speaks_the_terminals_own_palette() {
        let gold = format!("{GOLD}x");
        let themed = recolor_for(ColorTheme::Ansi16, &gold);
        // Gold is a bright yellow: SGR 93.
        assert_eq!(themed, "\x1b[93mx");
        // A background keeps being a background.
        let back = recolor_for(ColorTheme::Ansi16, "\x1b[48;2;10;10;180mx");
        assert_eq!(back, "\x1b[44mx");
        // The near-black blue the read card uses is nearer black than blue,
        // and is meant to land there: it is a background, not a colour.
        let card = recolor_for(ColorTheme::Ansi16, "\x1b[48;2;22;38;60mx");
        assert_eq!(card, "\x1b[40mx");
        assert!(!themed.contains("38;2"), "no 24-bit colour may survive this theme");
    }

    #[test]
    fn a_colour_written_together_with_bold_keeps_both() {
        // Most headings in the app are bold and gold in one sequence; an
        // earlier version of this module read only the bare colour form and
        // let every bold heading through unthemed.
        let themed = recolor_for(ColorTheme::Monochrome, "\x1b[1;38;2;225;175;95mTitle\x1b[0m");
        assert!(themed.starts_with("\x1b[1;38;2;"), "the bold was lost: {themed:?}");
        assert!(!themed.contains("225;175;95"), "the colour was not themed: {themed:?}");
        // Foreground and background in one sequence are both themed.
        let both = recolor_for(ColorTheme::Monochrome, "\x1b[38;2;200;100;50;48;2;20;30;40mx");
        assert!(!both.contains("200;100;50") && !both.contains("20;30;40"), "{both:?}");
    }

    #[test]
    fn a_colour_from_the_terminals_own_palette_is_left_to_the_terminal() {
        // `38;5;N` names a slot in the palette the terminal already owns.
        let themed = recolor_for(ColorTheme::Monochrome, "\x1b[38;5;208mx");
        assert_eq!(themed, "\x1b[38;5;208mx");
    }

    #[test]
    fn a_half_written_escape_is_left_alone() {
        // Frames are clipped to the terminal width, and a clip can land
        // inside an escape; it must not eat the rest of the line.
        for broken in ["\x1b[38;2;1;2", "\x1b[38;2;", "\x1b[", "\x1b[38;2"] {
            let themed = recolor_for(ColorTheme::Monochrome, broken);
            assert_eq!(themed, broken, "{broken:?} was rewritten");
        }
        // A colour followed by another attribute is finished, though, and is
        // themed with that attribute kept.
        let finished = recolor_for(ColorTheme::Monochrome, "\x1b[38;2;1;2;3;4m");
        assert!(finished.ends_with(";4m"), "the underline was lost: {finished:?}");
    }

    #[test]
    fn the_active_theme_is_what_was_set() {
        set(ColorTheme::Midnight);
        assert_eq!(active(), ColorTheme::Midnight);
        set(ColorTheme::Dark);
        assert_eq!(active(), ColorTheme::Dark);
    }
}
