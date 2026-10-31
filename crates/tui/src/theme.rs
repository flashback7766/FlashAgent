//! Colour themes. The app is written in one 24-bit palette; a theme transforms
//! the finished frame just before printing. Custom roles recognise only that
//! palette's anchors and nearby shades, not semantic markers in arbitrary RGB.

use std::borrow::Cow;
use std::sync::{LazyLock, RwLock};

use flashagent_core::{AppConfig, RgbColor, ThemeColors, ThemePalette};
pub use flashagent_core::ColorTheme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTheme {
    pub base: ColorTheme,
    pub colors: ThemeColors,
}

impl ResolvedTheme {
    fn builtin(base: ColorTheme) -> Self {
        Self { base, colors: ThemeColors::default() }
    }
}

static ACTIVE: LazyLock<RwLock<ResolvedTheme>> = LazyLock::new(|| RwLock::new(ResolvedTheme::builtin(ColorTheme::Dark)));

pub fn set(theme: ColorTheme) {
    let theme = resolve_choice(theme, None, terminal_is_light());
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = theme;
}

pub fn set_config(config: &AppConfig) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = resolved_for(config);
}

pub fn current() -> ResolvedTheme {
    *ACTIVE.read().unwrap_or_else(|e| e.into_inner())
}

/// Explicit custom bases are never replaced by terminal auto-detection.
pub fn resolved_for(config: &AppConfig) -> ResolvedTheme {
    resolve_choice(config.color_theme, config.active_palette(), terminal_is_light())
}

fn resolve_choice(base: ColorTheme, palette: Option<&ThemePalette>, light: bool) -> ResolvedTheme {
    match palette {
        Some(palette) => ResolvedTheme { base: palette.base, colors: palette.colors },
        None => ResolvedTheme::builtin(if base == ColorTheme::Dark && light { ColorTheme::Light } else { base }),
    }
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
    current().base
}

/// Borrows the frame unchanged under the built-in `Dark` theme.
pub fn recolor(frame: &str) -> Cow<'_, str> {
    recolor_with(current(), frame)
}

/// Testable without touching the global choice or terminal detection.
pub fn recolor_for(theme: ColorTheme, frame: &str) -> Cow<'_, str> {
    recolor_with(ResolvedTheme::builtin(theme), frame)
}

pub fn recolor_with(theme: ResolvedTheme, frame: &str) -> Cow<'_, str> {
    if theme == ResolvedTheme::builtin(ColorTheme::Dark) || frame.is_empty() {
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
fn recolor_sgr(theme: ResolvedTheme, params: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let parts: Vec<&str> = params.split(';').collect();
    let mut i = 0;
    while i < parts.len() {
        let ground = match parts[i] {
            "38" => Some(Ground::Fore),
            "48" => Some(Ground::Back),
            _ => None,
        };
        match (ground, parts.get(i + 1).copied()) {
            (Some(ground), Some("2")) => {
                let channel = |at: usize| parts.get(at).and_then(|c| c.parse::<u8>().ok());
                let (Some(r), Some(g), Some(b)) = (channel(i + 2), channel(i + 3), channel(i + 4)) else {
                    // Channels may also look like attributes or colour leads. Do not
                    // reinterpret a malformed colour, even after an earlier valid one.
                    return format!("\x1b[{params}m");
                };
                kept.extend(painted(theme, ground, (r, g, b)));
                i += 5;
            }
            (Some(_), Some("5")) => {
                // A palette index of 38 or 48 is not another colour lead.
                let end = (i + 3).min(parts.len());
                kept.extend(parts[i..end].iter().map(|s| s.to_string()));
                i = end;
            }
            _ => {
                kept.push(parts[i].to_string());
                i += 1;
            }
        }
    }
    format!("\x1b[{}m", kept.join(";"))
}

fn painted(theme: ResolvedTheme, ground: Ground, rgb: (u8, u8, u8)) -> Vec<String> {
    let custom = role_override(theme.colors, ground, rgb);
    match theme.base {
        ColorTheme::Ansi16 => {
            // The body text is near-white and panels near-black, drawn for a dark
            // screen. Mapped to bright white and black they vanished on a white one
            // (macOS Terminal's default); the terminal's own colours fit either.
            let (r, g, b) = rgb;
            let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
            if custom.is_none() {
                match ground {
                    Ground::Fore if lo >= 200 && hi - lo < 40 => return vec!["39".to_string()],
                    Ground::Back if hi <= 70 => return vec!["49".to_string()],
                    _ => {}
                }
            }
            let (code, bright) = nearest_ansi(custom.unwrap_or(rgb));
            let base = match ground {
                Ground::Fore => 30,
                Ground::Back => 40,
            };
            // 90-97 and 100-107 are the bright halves of the same eight.
            vec![(base + code as u16 + if bright { 60 } else { 0 }).to_string()]
        }
        _ => {
            // A user RGB is final; applying Light/Monochrome again would undo it.
            let (r, g, b) = custom.unwrap_or_else(|| transform(theme.base, rgb));
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

#[derive(Clone, Copy)]
enum Role {
    Text, Muted, Border, Surface, Accent, Info, Success, Warning, Error,
}

impl Role {
    fn color(self, colors: ThemeColors) -> Option<RgbColor> {
        match self {
            Self::Text => colors.text,
            Self::Muted => colors.muted,
            Self::Border => colors.border,
            Self::Surface => colors.surface,
            Self::Accent => colors.accent,
            Self::Info => colors.info,
            Self::Success => colors.success,
            Self::Warning => colors.warning,
            Self::Error => colors.error,
        }
    }
}

const TEXT: (u8, u8, u8) = (232, 227, 218);
const MUTED: (u8, u8, u8) = (140, 135, 130);
const BORDER: (u8, u8, u8) = (95, 90, 85);
const SURFACE: (u8, u8, u8) = (22, 38, 60);
const GOLD: (u8, u8, u8) = (225, 175, 95);
const BLUE: (u8, u8, u8) = (138, 180, 248);
const CHROMATIC: [(Role, (u8, u8, u8)); 6] = [
    (Role::Accent, GOLD),
    (Role::Accent, BLUE),
    (Role::Info, (110, 175, 230)),
    (Role::Success, (135, 220, 145)),
    (Role::Warning, (240, 210, 115)),
    (Role::Error, (245, 120, 120)),
];

fn role_override(colors: ThemeColors, ground: Ground, rgb: (u8, u8, u8)) -> Option<(u8, u8, u8)> {
    if colors == ThemeColors::default() {
        return None;
    }
    let (role, anchor) = matching_role(ground, rgb)?;
    let RgbColor(r, g, b) = role.color(colors)?;
    // Preserve the brightness steps in pulses and shading; the canonical RGB
    // has zero delta and therefore becomes exactly the configured colour.
    let delta = luminance(rgb) as i32 - luminance(anchor) as i32;
    Some((clamp(r as i32 + delta), clamp(g as i32 + delta), clamp(b as i32 + delta)))
}

fn matching_role(ground: Ground, rgb: (u8, u8, u8)) -> Option<(Role, (u8, u8, u8))> {
    for (role, anchor) in [(Role::Text, TEXT), (Role::Muted, MUTED), (Role::Border, BORDER), (Role::Surface, SURFACE)]
        .into_iter().chain(CHROMATIC)
    {
        if rgb == anchor {
            return Some((role, anchor));
        }
    }

    let (r, g, b) = rgb;
    // Panels and selection backgrounds have a blue tint. Dark red/green diff
    // fills are not surfaces, nor is every near-black foreground a panel.
    let surface_radius = if ground == Ground::Back { 24 } else { 12 };
    if r <= g && g.saturating_add(8) <= b && max_distance(rgb, SURFACE) <= surface_radius {
        return Some((Role::Surface, SURFACE));
    }

    // These two narrow paths are the existing gold pulse and welcome highlight.
    // A plain nearest-anchor match would turn the bright gold pulse into Warning.
    if on_shade_path(rgb, GOLD, (250, 215, 150)) {
        return Some((Role::Accent, GOLD));
    }
    if on_shade_path(rgb, BLUE, (194, 231, 255)) {
        return Some((Role::Accent, BLUE));
    }

    let (role, anchor) = CHROMATIC.into_iter().min_by_key(|(_, anchor)| color_distance(rgb, *anchor))?;
    if color_distance(rgb, anchor) <= 36 * 36 {
        return Some((role, anchor));
    }

    // The cool border is the only non-neutral border in the written palette.
    if ground == Ground::Fore && max_distance(rgb, (75, 99, 130)) <= 12 {
        return Some((Role::Border, BORDER));
    }
    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
    if hi - lo <= 30 && (ground == Ground::Fore || lo >= 200) {
        let brightness = luminance(rgb);
        return match brightness {
            185..=255 => Some((Role::Text, TEXT)),
            112..=184 => Some((Role::Muted, MUTED)),
            70..=111 => Some((Role::Border, BORDER)),
            _ => None,
        };
    }
    None
}

fn max_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u8 {
    a.0.abs_diff(b.0).max(a.1.abs_diff(b.1)).max(a.2.abs_diff(b.2))
}

fn color_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> i32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

fn on_shade_path(rgb: (u8, u8, u8), from: (u8, u8, u8), to: (u8, u8, u8)) -> bool {
    let offset = [rgb.0 as f32 - from.0 as f32, rgb.1 as f32 - from.1 as f32, rgb.2 as f32 - from.2 as f32];
    let span = [to.0 as f32 - from.0 as f32, to.1 as f32 - from.1 as f32, to.2 as f32 - from.2 as f32];
    let dot: f32 = offset.iter().zip(span).map(|(a, b)| a * b).sum();
    let length: f32 = span.iter().map(|v| v * v).sum();
    let t = (dot / length).clamp(0.0, 1.0);
    offset.iter().zip(span).all(|(a, b)| (a - b * t).abs() <= 8.0)
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

    fn custom(base: ColorTheme) -> ResolvedTheme {
        ResolvedTheme {
            base,
            colors: ThemeColors {
                text: Some(RgbColor(14, 25, 36)),
                muted: Some(RgbColor(47, 58, 69)),
                border: Some(RgbColor(80, 91, 102)),
                surface: Some(RgbColor(113, 124, 135)),
                accent: Some(RgbColor(146, 157, 168)),
                info: Some(RgbColor(179, 190, 201)),
                success: Some(RgbColor(212, 223, 234)),
                warning: Some(RgbColor(135, 57, 246)),
                error: Some(RgbColor(12, 200, 123)),
            },
        }
    }

    fn rgb_sgr(ground: Ground, (r, g, b): (u8, u8, u8)) -> String {
        let lead = if ground == Ground::Fore { 38 } else { 48 };
        format!("\x1b[{lead};2;{r};{g};{b}m")
    }

    #[test]
    fn every_canonical_role_becomes_exactly_the_override_not_a_second_transform() {
        let anchors = [
            ((232, 227, 218), (14, 25, 36)),
            ((140, 135, 130), (47, 58, 69)),
            ((95, 90, 85), (80, 91, 102)),
            ((22, 38, 60), (113, 124, 135)),
            ((225, 175, 95), (146, 157, 168)),
            ((138, 180, 248), (146, 157, 168)),
            ((110, 175, 230), (179, 190, 201)),
            ((135, 220, 145), (212, 223, 234)),
            ((240, 210, 115), (135, 57, 246)),
            ((245, 120, 120), (12, 200, 123)),
        ];
        for base in [ColorTheme::Dark, ColorTheme::Midnight, ColorTheme::HighContrast, ColorTheme::Monochrome, ColorTheme::Light] {
            for ground in [Ground::Fore, Ground::Back] {
                for (source, target) in anchors {
                    let frame = format!("{}x\x1b[0m", rgb_sgr(ground, source));
                    let expected = format!("{}x\x1b[0m", rgb_sgr(ground, target));
                    assert_eq!(recolor_with(custom(base), &frame), expected, "{base:?} {ground:?} {source:?}");
                }
            }
        }
    }

    #[test]
    fn nearby_text_and_status_shades_keep_their_brightness_steps() {
        let colors = custom(ColorTheme::Dark).colors;
        for (source, expected) in [
            ((242, 237, 228), (24, 35, 46)),
            ((160, 155, 150), (67, 78, 89)),
            ((100, 95, 90), (85, 96, 107)),
            ((215, 165, 85), (136, 147, 158)),
            ((100, 165, 220), (169, 180, 191)),
            ((125, 210, 135), (202, 213, 224)),
            ((230, 200, 105), (125, 47, 236)),
            ((235, 110, 110), (2, 190, 113)),
        ] {
            assert_eq!(role_override(colors, Ground::Fore, source), Some(expected), "{source:?}");
            let frame = rgb_sgr(Ground::Fore, source);
            assert_eq!(recolor_with(custom(ColorTheme::Dark), &frame), rgb_sgr(Ground::Fore, expected));
        }
    }

    #[test]
    fn dynamic_gold_and_blue_accents_stay_accents_and_keep_shading() {
        let theme = custom(ColorTheme::Dark);
        for (anchor, shades) in [
            ((225, 175, 95), [(225, 175, 95), (231, 185, 109), (238, 195, 123), (244, 205, 136), (250, 215, 150)]),
            ((138, 180, 248), [(138, 180, 248), (152, 193, 250), (166, 206, 252), (180, 218, 253), (194, 231, 255)]),
        ] {
            let mut previous = 0;
            for source in shades {
                let delta = luminance(source) as i32 - luminance(anchor) as i32;
                let expected = (clamp(146 + delta), clamp(157 + delta), clamp(168 + delta));
                for ground in [Ground::Fore, Ground::Back] {
                    let frame = rgb_sgr(ground, source);
                    assert_eq!(recolor_with(theme, &frame), rgb_sgr(ground, expected), "{source:?}");
                }
                assert!(luminance(expected) > previous, "the pulse lost a brightness step");
                previous = luminance(expected);
            }
        }
    }

    #[test]
    fn missing_roles_and_unrelated_rgb_inherit_the_builtin_base() {
        let untouched = [
            (232, 227, 218), (140, 135, 130), (95, 90, 85), (22, 38, 60),
            (110, 175, 230), (135, 220, 145), (240, 210, 115), (245, 120, 120),
            (255, 0, 255), (40, 100, 180), (0, 255, 255), (255, 0, 0),
        ];
        for base in ColorTheme::all() {
            let theme = ResolvedTheme {
                base: *base,
                colors: ThemeColors { accent: Some(RgbColor(10, 20, 30)), ..ThemeColors::default() },
            };
            for ground in [Ground::Fore, Ground::Back] {
                for source in untouched {
                    let frame = rgb_sgr(ground, source);
                    assert_eq!(recolor_with(theme, &frame), recolor_for(*base, &frame), "{base:?} {source:?}");
                }
            }
        }
    }

    #[test]
    fn surfaces_do_not_capture_diff_backgrounds_or_arbitrary_dark_colors() {
        for base in ColorTheme::all() {
            let theme = custom(*base);
            for source in [(65, 20, 25), (20, 55, 30), (60, 60, 60), (10, 10, 180)] {
                let frame = rgb_sgr(Ground::Back, source);
                assert_eq!(recolor_with(theme, &frame), recolor_for(*base, &frame), "{source:?}");
            }
        }
        assert_eq!(role_override(custom(ColorTheme::Dark).colors, Ground::Back, (32, 48, 70)), Some((123, 134, 145)));
        assert_eq!(role_override(custom(ColorTheme::Dark).colors, Ground::Fore, (30, 26, 22)), None);
    }

    #[test]
    fn custom_fg_bg_and_multiple_sgr_preserve_attributes_resets_and_cursor_motion() {
        let theme = custom(ColorTheme::Light);
        let frame = "\x1b[2J\x1b[H\x1b[1;4;38;2;232;227;218;48;2;22;38;60;7m日本🦀\x1b[0m\x1b[39;49;22m\x1b[12A\x1b[38;2;225;175;95;48;5;48;2m!\x1b[m";
        let expected = "\x1b[2J\x1b[H\x1b[1;4;38;2;14;25;36;48;2;113;124;135;7m日本🦀\x1b[0m\x1b[39;49;22m\x1b[12A\x1b[38;2;146;157;168;48;5;48;2m!\x1b[m";
        assert_eq!(recolor_with(theme, frame), expected);
        for frame in ["\x1b[38;5;38;2m", "\x1b[48;5;48;2m", "\x1b[38;5;208;48;5;234;1m"] {
            assert_eq!(recolor_with(theme, frame), frame);
        }
    }

    #[test]
    fn ansi16_quantizes_custom_rgb_instead_of_printing_truecolor() {
        let theme = ResolvedTheme {
            base: ColorTheme::Ansi16,
            colors: ThemeColors {
                text: Some(RgbColor(255, 255, 255)),
                surface: Some(RgbColor(0, 0, 170)),
                accent: Some(RgbColor(170, 0, 170)),
                ..ThemeColors::default()
            },
        };
        let frame = "\x1b[1;38;2;232;227;218;48;2;22;38;60mtext\x1b[0m\x1b[38;2;225;175;95maccent\x1b[0m";
        assert_eq!(recolor_with(theme, frame), "\x1b[1;97;44mtext\x1b[0m\x1b[35maccent\x1b[0m");
        let all_anchors: String = [TEXT, MUTED, BORDER, SURFACE].into_iter()
            .chain(CHROMATIC.into_iter().map(|(_, anchor)| anchor))
            .flat_map(|source| [rgb_sgr(Ground::Fore, source), rgb_sgr(Ground::Back, source)])
            .collect();
        let themed = recolor_with(custom(ColorTheme::Ansi16), &all_anchors);
        assert!(!themed.contains("38;2") && !themed.contains("48;2"), "{themed:?}");
    }

    #[test]
    fn malformed_complete_rgb_and_utf8_are_preserved_byte_for_byte() {
        let broken = [
            "\x1b[38;2;300;2;3m", "\x1b[38;2;1;2m", "\x1b[38;2;;2;3m",
            "\x1b[1;38;2;1;999;3;4m", "\x1b[48;2;1;2;999;0m",
            "\x1b[38;2;225;175;95;48;2;1;;3;4m",
            "\x1b[38;2;-1;2;3m", "\x1b[38;2;1;2;Жm", "\x1b[38:2:1:2:3m",
            "\x1b[38;2;1;2", "\x1b[38;2;", "\x1b[", "\x1b[38;2",
        ];
        for escape in broken {
            let frame = format!("日本🦀{escape}café");
            for theme in [custom(ColorTheme::Dark), custom(ColorTheme::Ansi16), ResolvedTheme::builtin(ColorTheme::Monochrome)] {
                assert_eq!(recolor_with(theme, &frame).as_bytes(), frame.as_bytes(), "{frame:?}");
            }
        }
    }

    #[test]
    fn custom_dark_is_explicit_but_builtin_dark_keeps_terminal_auto_light() {
        let palette = ThemePalette { base: ColorTheme::Dark, colors: custom(ColorTheme::Dark).colors };
        let mut config = AppConfig { color_theme: ColorTheme::Monochrome, ..AppConfig::default() };
        config.theme_palettes.insert("custom".into(), palette);
        config.active_theme_palette = Some("custom".into());
        let expected = custom(ColorTheme::Dark);
        assert_eq!(resolved_for(&config), expected);
        for light in [false, true] {
            assert_eq!(resolve_choice(config.color_theme, config.active_palette(), light), expected);
        }
        config.active_theme_palette = None;
        assert_eq!(resolved_for(&config), ResolvedTheme::builtin(ColorTheme::Monochrome));
        config.color_theme = ColorTheme::Dark;
        assert_eq!(resolve_choice(config.color_theme, config.active_palette(), true), ResolvedTheme::builtin(ColorTheme::Light));
        assert_eq!(resolve_choice(config.color_theme, config.active_palette(), false), ResolvedTheme::builtin(ColorTheme::Dark));
        let builtin = if terminal_is_light() { ColorTheme::Light } else { ColorTheme::Dark };
        assert_eq!(resolved_for(&config), ResolvedTheme::builtin(builtin));
    }
}
