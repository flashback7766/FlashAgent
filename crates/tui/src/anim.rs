//! Motion for the terminal UI.
//!
//! Everything here runs on one wall clock, never on how many events arrived:
//! a model streaming fast must not make a spinner race. The drawing functions
//! are pure in `t` (milliseconds on that clock), so tests pin a moment and
//! check the frame; the app reads [`now_ms`].
//!
//! Motion can be switched off (Settings → UI → Animations). A spinner still
//! turns then — it says work is happening — but nothing sweeps, pulses or
//! unfolds.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Turn decorative motion on or off.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether decorative motion is on.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Milliseconds since the first call: the clock every animation reads.
pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// An RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// The foreground escape for this colour.
    pub fn fg(self) -> String {
        format!("\x1b[38;2;{};{};{}m", self.0, self.1, self.2)
    }

    /// `self` moved `k` (0..=1) of the way to `other`.
    pub fn mix(self, other: Rgb, k: f32) -> Rgb {
        let k = k.clamp(0.0, 1.0);
        let ch = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * k).round() as u8;
        Rgb(ch(self.0, other.0), ch(self.1, other.1), ch(self.2, other.2))
    }
}

/// Braille spinner frames.
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The spinner frame at `t`: one step every 80 ms.
pub fn spinner(t: u64) -> &'static str {
    SPINNER[(t / 80) as usize % SPINNER.len()]
}

/// 0 → 1 → 0 once every `period_ms`, eased like breathing.
pub fn breathe(t: u64, period_ms: u64) -> f32 {
    let phase = (t % period_ms.max(1)) as f32 / period_ms.max(1) as f32;
    0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
}

/// `a` and `b` alternating smoothly every `period_ms`. Holds at `a` when
/// motion is off.
pub fn pulse(t: u64, period_ms: u64, a: Rgb, b: Rgb) -> Rgb {
    if !enabled() {
        return a;
    }
    a.mix(b, breathe(t, period_ms))
}

/// Ease-out cubic: fast start, gentle landing.
pub fn ease_out(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    1.0 - (1.0 - k).powi(3)
}

/// How far (0..=1) an animation `duration_ms` long that began at `started`
/// has got by `t`, eased. Always finished when motion is off.
pub fn progress(started: u64, t: u64, duration_ms: u64) -> f32 {
    if !enabled() || duration_ms == 0 {
        return 1.0;
    }
    ease_out(t.saturating_sub(started) as f32 / duration_ms as f32)
}

/// `text` (plain, no escapes) with a band of light sweeping across it: each
/// character is coloured between `base` and `glow` by how close the band is.
/// The band crosses once every `period_ms`, then rests off the end so the
/// sweep reads as a gesture, not a strobe.
pub fn shimmer(text: &str, t: u64, period_ms: u64, base: Rgb, glow: Rgb) -> String {
    let chars: Vec<char> = text.chars().collect();
    if !enabled() || chars.is_empty() {
        return format!("{}{text}\x1b[0m", base.fg());
    }
    const BAND: f32 = 4.0;
    let span = chars.len() as f32 + BAND * 4.0;
    let phase = (t % period_ms.max(1)) as f32 / period_ms.max(1) as f32;
    let centre = phase * span - BAND * 2.0;
    let mut out = String::new();
    let mut last: Option<Rgb> = None;
    for (i, c) in chars.iter().enumerate() {
        let d = (i as f32 - centre).abs();
        let k = (1.0 - d / BAND).max(0.0);
        let colour = base.mix(glow, k * k * (3.0 - 2.0 * k));
        if last != Some(colour) {
            out.push_str(&colour.fg());
            last = Some(colour);
        }
        out.push(*c);
    }
    out.push_str("\x1b[0m");
    out
}

/// A one-row chart of `values` (oldest first) in block heights, scaled to
/// the largest of them.
pub fn sparkline(values: &[f64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().fold(0.0_f64, f64::max);
    values
        .iter()
        .map(|v| {
            if max <= 0.0 {
                BARS[0]
            } else {
                BARS[((v / max) * (BARS.len() - 1) as f64).round().clamp(0.0, 7.0) as usize]
            }
        })
        .collect()
}

/// Whether a blinking cursor is lit at `t` (on 530 ms, off 530 ms). Always lit
/// when motion is off.
pub fn blink_on(t: u64) -> bool {
    !enabled() || (t / 530).is_multiple_of(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        crate::strip_ansi(s)
    }

    #[test]
    fn the_spinner_moves_with_the_clock_not_with_calls() {
        assert_eq!(spinner(0), spinner(79));
        assert_ne!(spinner(0), spinner(80));
        assert_eq!(spinner(0), spinner(80 * SPINNER.len() as u64));
    }

    #[test]
    fn shimmer_keeps_the_text_and_lights_only_part_of_it() {
        let base = Rgb(100, 100, 100);
        let glow = Rgb(255, 255, 255);
        let text = "Thinking about the answer";
        // Some moment mid-sweep.
        let frame = shimmer(text, 700, 2000, base, glow);
        assert_eq!(plain(&frame), text);
        assert!(frame.contains(&glow.fg()) || frame.contains("38;2;2"), "nothing is lit: {frame:?}");
        assert!(frame.contains(&base.fg()), "everything is lit: {frame:?}");
    }

    #[test]
    fn breathing_goes_from_rest_to_full_and_back() {
        assert!(breathe(0, 1000) < 0.01);
        assert!(breathe(500, 1000) > 0.99);
        assert!(breathe(1000, 1000) < 0.01);
    }

    #[test]
    fn progress_is_eased_and_finishes() {
        assert_eq!(progress(100, 100, 200), 0.0);
        assert!(progress(100, 200, 200) > 0.5, "ease-out is past halfway at half time");
        assert_eq!(progress(100, 400, 200), 1.0);
    }

    #[test]
    fn sparkline_scales_to_the_tallest_value() {
        assert_eq!(sparkline(&[0.0, 5.0, 10.0]), "▁▅█");
        assert_eq!(sparkline(&[0.0, 0.0]), "▁▁");
        assert_eq!(sparkline(&[]), "");
    }

    #[test]
    fn colours_mix_linearly_and_clamp() {
        assert_eq!(Rgb(0, 0, 0).mix(Rgb(200, 100, 50), 0.5), Rgb(100, 50, 25));
        assert_eq!(Rgb(0, 0, 0).mix(Rgb(200, 100, 50), 7.0), Rgb(200, 100, 50));
    }
}
