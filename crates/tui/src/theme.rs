//! Colour themes. The app is written in one 24-bit palette; a theme transforms
//! the finished frame just before printing, so new colours are themed without
//! registration. `Dark` is the palette as written and costs nothing.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU8, Ordering};

pub use flashagent_core::ColorTheme;

/// Index in [`ColorTheme::all`].
static ACTIVE: AtomicU8 = AtomicU8::new(0);

pub fn set(theme: ColorTheme) {
    let theme = if theme == ColorTheme::Dark && terminal_is_light() { ColorTheme::Light } else { theme };
    let index = ColorTheme::all().iter().position(|t| *t == theme).unwrap_or(0);
    ACTIVE.store(index as u8, Ordering::Relaxed);
}

/// The dark palette is unreadable on a white background, so a terminal that
/// says it has one (`COLORFGBG`, set by rxvt, Konsole, iTerm2 and others) gets
/// the light theme unless another was chosen.
fn terminal_is_light() -> bool {
    static LIGHT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LIGHT.get_or_init(|| std::env::var("COLORFGBG").is_ok_and(|v| background_is_light(&v)))
}

/// `COLORFGBG` is `fg;bg` or `fg;default;bg`; 7 and 15 are the whites.
fn background_is_light(colorfgbg: &str) -> bool {
    matches!(colorfgbg.rsplit(';').next().and_then(|bg| bg.trim().parse::<u8>().ok()), Some(7 | 15))
}

pub fn active() -> ColorTheme {
    let all = ColorTheme::all();
    all[(ACTIVE.load(Ordering::Relaxed) as usize).min(all.len() - 1)]
}

/// Borrows the frame unchanged under `Dark`.
pub fn recolor(frame: &str) -> Cow<'_, str> {
    recolor_for(active(), frame)
}

/// Testable without touching the global choice.
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
                // Cursor motion, clears, an escape cut at a clip boundary: copied as is.
                out.push_str("\x1b[");
                rest = &rest[2..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Only a complete SGR sequence; an escape cut by clipping is left to the caller.
fn sgr_sequence(s: &str) -> Option<(&str, usize)> {
    let body = s.strip_prefix("\x1b[")?;
    let end = body.find(|c: char| !(c.is_ascii_digit() || c == ';'))?;
    if body.as_bytes()[end] != b'm' {
        return None;
    }
    Some((&body[..end], 2 + end + 1))
}

/// Attributes are kept: a colour is often written with bold, and losing the
/// bold changes the picture as much as losing the colour.
fn recolor_sgr(theme: ColorTheme, params: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut parts = params.split(';').peekable();
    while let Some(token) = parts.next() {
        let ground = match token {
            "38" => Some(Ground::Fore),
            "48" => Some(Ground::Back),
            _ => None,
        };
        // `5;N` is a terminal palette colour, already themed by the terminal.
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

fn painted(theme: ColorTheme, ground: Ground, rgb: (u8, u8, u8)) -> Vec<String> {
    match theme {
        ColorTheme::Ansi16 => {
            // The body text is near-white and panels near-black, drawn for a dark
            // screen. Mapped to bright white and black they vanished on a white one
            // (macOS Terminal's default); the terminal's own colours fit either.
            let (r, g, b) = rgb;
            let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
            match ground {
                Ground::Fore if lo >= 200 && hi - lo < 40 => return vec!["39".to_string()],
                Ground::Back if hi <= 70 => return vec!["49".to_string()],
                _ => {}
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ground {
    Fore,
    Back,
}

/// 0..=255, with the usual perceived-brightness weights.
fn luminance((r, g, b): (u8, u8, u8)) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000).min(255) as u8
}

fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn transform(theme: ColorTheme, (r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    match theme {
        ColorTheme::Dark | ColorTheme::Ansi16 => (r, g, b),
        // Warm tones keep their shape and brightness order but read as night.
        ColorTheme::Midnight => (
            clamp(r as i32 * 78 / 100),
            clamp(g as i32 * 88 / 100),
            clamp(b as i32 * 115 / 100 + 14),
        ),
        // Dim gets dimmer and bright brighter, so they do not blur on a washed-out screen.
        ColorTheme::HighContrast => {
            let stretch = |c: u8| clamp(128 + (c as i32 - 128) * 165 / 100);
            (stretch(r), stretch(g), stretch(b))
        }
        // The range is lifted off black first, or dim text would vanish.
        ColorTheme::Monochrome => {
            let grey = clamp(40 + luminance((r, g, b)) as i32 * 215 / 255);
            (grey, grey, grey)
        }
        ColorTheme::Light => light((r, g, b)),
    }
}

/// Greys and near-whites turn over (bright text goes dark, dark panels go
/// pale); coloured accents keep their hue and deepen, so gold and green stay
/// readable on white. Foreground and background alike: a picture drawn with
/// both, like the mascot, stays one colour.
fn light((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    let (h, s, l) = to_hsl((r, g, b));
    // By absolute spread, not HSL saturation, which calls any dark tint vivid.
    let chroma = r.max(g).max(b) - r.min(g).min(b);
    let l = if chroma < 50 || !(0.25..=0.9).contains(&l) { 0.08 + (1.0 - l) * 0.85 } else { (l * 0.72).min(0.55) };
    from_hsl(h, s, l)
}

fn to_hsl((r, g, b): (u8, u8, u8)) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if s == 0.0 {
        return (to_byte(l), to_byte(l), to_byte(l));
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (to_byte(channel(h + 1.0 / 3.0)), to_byte(channel(h)), to_byte(channel(h - 1.0 / 3.0)))
}

/// The eight are compared at their usual values; the terminal may paint them
/// differently, which is the point of this theme.
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

    #[test]
    fn the_light_theme_darkens_text_and_keeps_accents_coloured() {
        // Near-white answer text becomes near-black.
        let (r, g, b) = light((245, 240, 232));
        assert!(luminance((r, g, b)) < 60, "{r},{g},{b}");
        // A dark panel becomes pale.
        assert!(luminance(light((20, 30, 40))) > 200);
        // Gold stays gold, only deeper.
        let (r, g, b) = light((225, 175, 95));
        assert!(r > g && g > b && luminance((r, g, b)) < 150, "{r},{g},{b}");
    }

    #[test]
    fn a_white_background_is_recognised_from_colorfgbg() {
        assert!(background_is_light("0;15"));
        assert!(background_is_light("0;default;7"));
        assert!(!background_is_light("15;0"));
        assert!(!background_is_light("garbage"));
    }

    const GOLD: &str = "\x1b[38;2;225;175;95m";

    #[test]
    fn the_written_palette_is_printed_as_written() {
        let frame = format!("{GOLD}FlashAgent\x1b[0m");
        assert!(matches!(recolor_for(ColorTheme::Dark, &frame), Cow::Borrowed(_)), "Dark must not copy the frame");
        assert_eq!(recolor_for(ColorTheme::Dark, &frame), frame);
    }

    #[test]
    fn only_the_colours_are_touched() {
        // Cursor motion, reverse video and reset must survive a theme.
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
        let back = recolor_for(ColorTheme::Ansi16, "\x1b[48;2;10;10;180mx");
        assert_eq!(back, "\x1b[44mx");
        // The read card's near-black blue and the near-white body text take the
        // terminal's own background and text colour: black and bright white made
        // a black panel of invisible text on a white screen.
        let card = recolor_for(ColorTheme::Ansi16, "\x1b[48;2;22;38;60mx");
        assert_eq!(card, "\x1b[49mx");
        let body = recolor_for(ColorTheme::Ansi16, "\x1b[38;2;232;227;218mx");
        assert_eq!(body, "\x1b[39mx");
        assert!(!themed.contains("38;2"), "no 24-bit colour may survive this theme");
    }

    #[test]
    fn a_colour_written_together_with_bold_keeps_both() {
        // Headings are bold and gold in one sequence; an earlier version let them
        // through unthemed.
        let themed = recolor_for(ColorTheme::Monochrome, "\x1b[1;38;2;225;175;95mTitle\x1b[0m");
        assert!(themed.starts_with("\x1b[1;38;2;"), "the bold was lost: {themed:?}");
        assert!(!themed.contains("225;175;95"), "the colour was not themed: {themed:?}");
        let both = recolor_for(ColorTheme::Monochrome, "\x1b[38;2;200;100;50;48;2;20;30;40mx");
        assert!(!both.contains("200;100;50") && !both.contains("20;30;40"), "{both:?}");
    }

    #[test]
    fn a_colour_from_the_terminals_own_palette_is_left_to_the_terminal() {
        let themed = recolor_for(ColorTheme::Monochrome, "\x1b[38;5;208mx");
        assert_eq!(themed, "\x1b[38;5;208mx");
    }

    #[test]
    fn a_half_written_escape_is_left_alone() {
        // A clip can land inside an escape; it must not eat the rest of the line.
        for broken in ["\x1b[38;2;1;2", "\x1b[38;2;", "\x1b[", "\x1b[38;2"] {
            let themed = recolor_for(ColorTheme::Monochrome, broken);
            assert_eq!(themed, broken, "{broken:?} was rewritten");
        }
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
