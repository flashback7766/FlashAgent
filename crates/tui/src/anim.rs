//! Motion for the terminal UI, driven by one wall clock, never by event count:
//! a fast stream must not make a spinner race. Drawing functions are pure in
//! `t` (ms), so tests pin a moment. With motion off (Settings → UI →
//! Animations) spinners still turn but nothing sweeps, pulses or unfolds.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Since the first call.
pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn fg(self) -> String {
        format!("\x1b[38;2;{};{};{}m", self.0, self.1, self.2)
    }

    /// `k` in 0..=1.
    pub fn mix(self, other: Rgb, k: f32) -> Rgb {
        let k = k.clamp(0.0, 1.0);
        let ch = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * k).round() as u8;
        Rgb(ch(self.0, other.0), ch(self.1, other.1), ch(self.2, other.2))
    }
}

// Braille is missing from Consolas, the console font on many Windows setups.
pub const SPINNER: &[&str] = &["·", "∙", "•", "●", "•", "∙"];

/// One step every 80 ms.
pub fn spinner(t: u64) -> &'static str {
    SPINNER[(t / 80) as usize % SPINNER.len()]
}

/// 0 → 1 → 0 once every `period_ms`.
pub fn breathe(t: u64, period_ms: u64) -> f32 {
    let phase = (t % period_ms.max(1)) as f32 / period_ms.max(1) as f32;
    0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
}

/// Holds at `a` when motion is off.
pub fn pulse(t: u64, period_ms: u64, a: Rgb, b: Rgb) -> Rgb {
    if !enabled() {
        return a;
    }
    a.mix(b, breathe(t, period_ms))
}

pub fn ease_out(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    1.0 - (1.0 - k).powi(3)
}

/// 0..=1, eased. Always finished when motion is off.
pub fn progress(started: u64, t: u64, duration_ms: u64) -> f32 {
    if !enabled() || duration_ms == 0 {
        return 1.0;
    }
    ease_out(t.saturating_sub(started) as f32 / duration_ms as f32)
}

/// `text` must be plain. The band crosses once per `period_ms`, then rests off
/// the end so it reads as a gesture, not a strobe.
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


/// 530 ms on, 530 ms off. Always lit when motion is off.
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
    fn colours_mix_linearly_and_clamp() {
        assert_eq!(Rgb(0, 0, 0).mix(Rgb(200, 100, 50), 0.5), Rgb(100, 50, 25));
        assert_eq!(Rgb(0, 0, 0).mix(Rgb(200, 100, 50), 7.0), Rgb(200, 100, 50));
    }
}
