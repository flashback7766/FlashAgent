//! Text shaping layer built on cosmic-text. B0 prototype quality bar:
//! prove that Cyrillic, metrics, wrapping and bidi all behave headlessly,
//! before any GPU code is written.

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping};

/// Laid-out single-line measurement, in physical pixels at given font size.
#[derive(Debug, Clone, PartialEq)]
pub struct LineMetrics {
    /// Advance width of the whole line.
    pub width: f32,
    /// Line height (ascent + descent).
    pub height: f32,
    /// Glyph/cluster count across runs.
    pub glyph_count: usize,
}

/// Errors of the text layer.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    /// Font system failed to load any usable font.
    #[error("no fonts available in system")]
    NoFonts,
    /// Text produced no layout runs.
    #[error("text produced no layout runs")]
    NoLayout,
}

fn shape_single(text: &str, font_size: f32, max_width: Option<f32>) -> Buffer {
    let mut font_system = FontSystem::new();
    let metrics = Metrics::new(font_size, font_size * 1.4);
    let mut buffer = Buffer::new(&mut font_system, metrics);
    buffer.set_size(&mut font_system, max_width, None);
    buffer.set_text(&mut font_system, text, &Attrs::new(), Shaping::Advanced);
    buffer.shape_until_scroll(&mut font_system, false);
    buffer
}

/// Shape a single line and measure it. Uses the system font database.
/// Works headless: no window, no GPU.
pub fn measure_line(text: &str, font_size: f32) -> Result<LineMetrics, TextError> {
    let buffer = shape_single(text, font_size, None);
    let run = buffer
        .layout_runs()
        .next()
        .ok_or(TextError::NoLayout)?;
    Ok(LineMetrics {
        width: run.line_w,
        height: run.line_height,
        glyph_count: run.glyphs.len(),
    })
}

/// True if the first laid-out run is right-to-left (bidi proof).
pub fn first_run_is_rtl(text: &str, font_size: f32) -> Result<bool, TextError> {
    let buffer = shape_single(text, font_size, None);
    let run = buffer.layout_runs().next().ok_or(TextError::NoLayout)?;
    Ok(run.rtl)
}

/// Word-wrap a paragraph at a fixed width; returns number of resulting lines.
pub fn wrap_lines(text: &str, font_size: f32, max_width: f32) -> Result<usize, TextError> {
    let buffer = shape_single(text, font_size, Some(max_width));
    Ok(buffer.layout_runs().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_line_measures_positive() {
        let m = measure_line("Hello FlashAgent", 16.0).expect("fonts present in image");
        assert!(m.width > 0.0);
        assert!(m.height > 16.0);
        assert!(m.glyph_count >= 15);
    }

    #[test]
    fn cyrillic_shapes_and_measures() {
        let m = measure_line("Привет, ФлэшАгент", 16.0).expect("fonts present");
        assert!(m.width > 0.0);
        assert!(m.glyph_count >= 16);
    }

    #[test]
    fn wider_text_is_wider() {
        let short = measure_line("abc", 16.0).unwrap();
        let long = measure_line("abcdefg hijklmn", 16.0).unwrap();
        assert!(long.width > short.width);
    }

    #[test]
    fn arabic_run_is_rtl() {
        assert!(first_run_is_rtl("مرحبا hello", 16.0).unwrap());
        assert!(!first_run_is_rtl("hello مرحبا", 16.0).unwrap());
    }

    #[test]
    fn wrapping_limits_lines_width() {
        let text = "the quick brown fox jumps over the lazy dog repeatedly";
        let lines = wrap_lines(text, 16.0, 120.0).unwrap();
        assert!(lines >= 3, "expected wrapping, got {lines} lines");
    }
}
