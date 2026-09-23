//! Flicker-free output for every full-screen view. A frame is the list of rows
//! that should be on the screen; only rows that differ from the last frame are
//! written, each over the old one in place, and nothing is cleared first. A
//! terminal that shows a frame half-written (Windows' console, which also gets
//! the frame in pieces from the standard library) then shows old rows next to
//! new ones, never a blank screen between two frames.

use std::io::Write as _;

use crate::{clip_ansi, terminal_safe, visible_width};

/// A terminal that knows the mode shows the frame only once complete; others
/// ignore it.
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";

/// What the terminal shows, as far as this process knows.
#[derive(Default)]
pub struct Screen {
    rows: Vec<String>,
    size: (u16, u16),
    cursor: Option<(u16, u16)>,
    /// Rows are compared before the theme recolours them, so a new theme has to
    /// rewrite them all.
    theme: Option<crate::theme::ColorTheme>,
    /// False until the first frame, and after anything else drew on the screen.
    valid: bool,
}

impl Screen {
    pub fn new() -> Self {
        Self::default()
    }

    /// The next frame rewrites every row: the screen was cleared or drawn on by
    /// someone else (an editor, a switch of screen buffers).
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Draws `rows` from the top of a `width` x `height` screen and parks the
    /// cursor at `cursor` (row, column), or hides it. Writes nothing when the
    /// frame is the one already shown.
    pub fn paint(&mut self, rows: &[String], cursor: Option<(u16, u16)>) {
        let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
        let theme = crate::theme::active();
        if self.theme != Some(theme) {
            self.theme = Some(theme);
            self.valid = false;
        }
        if let Some(frame) = self.compose(rows, cursor, width, height) {
            let _ = write_frame(&crate::theme::recolor(&frame));
        }
    }

    /// The bytes that turn the last frame into this one; `None` when there is
    /// nothing to change. Pure apart from `self`, for tests.
    pub fn compose(&mut self, rows: &[String], cursor: Option<(u16, u16)>, width: u16, height: u16) -> Option<String> {
        if self.size != (width, height) {
            self.valid = false;
            self.size = (width, height);
        }
        let (w, h) = (width as usize, height as usize);
        let fitted: Vec<String> = (0..h)
            .map(|i| match rows.get(i) {
                // The bottom row stays a column short: a character in the last cell
                // of the screen makes some consoles scroll the whole screen up.
                // Last line of defence: whatever reached a row, only text and colour
                // go to the terminal.
                Some(row) => clip_ansi(&terminal_safe(row), if i + 1 == h { w.saturating_sub(1) } else { w }),
                None => String::new(),
            })
            .collect();
        let cursor = cursor.filter(|(r, c)| (*r as usize) < h && (*c as usize) < w);

        let mut out = String::new();
        for (i, row) in fitted.iter().enumerate() {
            if self.valid && self.rows.get(i) == Some(row) {
                continue;
            }
            out.push_str(&format!("\x1b[{};1H\x1b[0m{row}\x1b[0m", i + 1));
            // A row that fills the width leaves the cursor past its last cell, where
            // an erase would take that cell too. A new line ends it instead: Windows'
            // console otherwise marks the row as running on into the next, and joins
            // the two when text is copied or the window is resized. The bottom row is
            // never full, so this never scrolls.
            if visible_width(row) < w {
                out.push_str("\x1b[K");
            } else {
                out.push_str("\r\n");
            }
        }
        if out.is_empty() && self.valid && cursor == self.cursor {
            return None;
        }
        self.rows = fitted;
        self.cursor = cursor;
        self.valid = true;

        let mut frame = String::with_capacity(out.len() + 48);
        frame.push_str(SYNC_BEGIN);
        frame.push_str(HIDE_CURSOR);
        frame.push_str(&out);
        if let Some((r, c)) = cursor {
            frame.push_str(&format!("\x1b[{};{}H{SHOW_CURSOR}", r + 1, c + 1));
        }
        frame.push_str(SYNC_END);
        Some(frame)
    }
}

/// In one piece where the platform allows. Rust's standard output hands a
/// Windows console at most a few kilobytes per call and the console draws
/// between calls, so a frame written through it is seen being drawn.
pub fn write_frame(frame: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    // Whatever was queued before goes out first.
    out.flush()?;
    #[cfg(windows)]
    if win::write_console(frame)? {
        return Ok(());
    }
    out.write_all(frame.as_bytes())?;
    out.flush()
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    /// `(DWORD)-11`.
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    /// Old consoles failed on very large writes; a frame is far smaller.
    const MAX_UNITS: usize = 32 * 1024;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn WriteConsoleW(handle: *mut c_void, buffer: *const c_void, units: u32, written: *mut u32, reserved: *mut c_void) -> i32;
    }

    /// `Ok(false)` when standard output is not a console, for the caller to
    /// write the usual way.
    pub(super) fn write_console(text: &str) -> std::io::Result<bool> {
        // SAFETY: plain Win32 calls; every pointer is to a live local or to `wide`.
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0u32;
            if handle.is_null() || GetConsoleMode(handle, &mut mode) == 0 {
                return Ok(false);
            }
            let wide: Vec<u16> = text.encode_utf16().collect();
            let mut rest: &[u16] = &wide;
            while !rest.is_empty() {
                let mut end = rest.len().min(MAX_UNITS);
                // Never between the two halves of a surrogate pair.
                if end < rest.len() && (0xD800..0xDC00).contains(&rest[end - 1]) {
                    end -= 1;
                }
                let mut written = 0u32;
                if WriteConsoleW(handle, rest.as_ptr().cast(), end as u32, &mut written, std::ptr::null_mut()) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if written == 0 {
                    break;
                }
                rest = &rest[(written as usize).min(rest.len())..];
            }
        }
        Ok(true)
    }
}

/// What the Windows taskbar button (and the Windows Terminal tab) shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    None,
    /// Working, amount unknown.
    Busy,
    /// Stopped until the user answers.
    Waiting,
    /// 0..=100.
    Percent(u8),
}

impl Progress {
    /// `OSC 9;4`, which Windows Terminal and ConEmu show on the taskbar.
    fn sequence(self) -> String {
        let (state, value) = match self {
            Progress::None => (0, 0),
            Progress::Busy => (3, 0),
            Progress::Waiting => (4, 100),
            Progress::Percent(p) => (1, p.min(100)),
        };
        format!("\x1b]9;4;{state};{value}\x07")
    }
}

/// Sent only when it changes. Only where it is understood: iTerm2 reads
/// `OSC 9` as a notification and would post "4;3" on every turn.
pub fn set_progress(progress: Progress) {
    static LAST: std::sync::Mutex<Option<Progress>> = std::sync::Mutex::new(None);
    if !(cfg!(windows) || std::env::var_os("WT_SESSION").is_some() || std::env::var_os("ConEmuPID").is_some()) {
        return;
    }
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if *last == Some(progress) {
        return;
    }
    *last = Some(progress);
    let _ = write_frame(&progress.sequence());
}

/// For the full-screen views outside the chat (setup, release notes, the trust
/// question): `lines`, under a blank row if asked, with no cursor.
pub fn paint_page(screen: &mut Screen, lines: &[String], blank_top: bool) {
    let mut rows = Vec::with_capacity(lines.len() + 1);
    if blank_top {
        rows.push(String::new());
    }
    rows.extend(lines.iter().cloned());
    screen.paint(&rows, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_frame_never_clears_the_screen() {
        let mut s = Screen::new();
        let first = s.compose(&rows(&["one", "two"]), None, 20, 5).unwrap();
        let second = s.compose(&rows(&["one", "TWO"]), None, 20, 5).unwrap();
        for frame in [&first, &second] {
            assert!(!frame.contains("\x1b[2J") && !frame.contains("\x1b[J"), "{frame:?}");
        }
    }

    #[test]
    fn only_rows_that_changed_are_written() {
        let mut s = Screen::new();
        s.compose(&rows(&["alpha", "beta", "gamma"]), Some((0, 2)), 20, 5);
        let frame = s.compose(&rows(&["alpha", "BETA", "gamma"]), Some((0, 2)), 20, 5).unwrap();
        assert!(frame.contains("BETA"));
        assert!(!frame.contains("alpha") && !frame.contains("gamma"), "{frame:?}");
        assert!(frame.contains("\x1b[2;1H"), "row 2 is addressed directly: {frame:?}");
    }

    #[test]
    fn the_same_frame_twice_writes_nothing() {
        let mut s = Screen::new();
        s.compose(&rows(&["x"]), Some((0, 1)), 20, 5);
        assert!(s.compose(&rows(&["x"]), Some((0, 1)), 20, 5).is_none());
        // The cursor moving alone is still a change.
        assert!(s.compose(&rows(&["x"]), Some((0, 0)), 20, 5).is_some());
    }

    #[test]
    fn rows_that_went_away_are_erased() {
        let mut s = Screen::new();
        s.compose(&rows(&["a", "b", "c"]), None, 20, 5);
        let frame = s.compose(&rows(&["a"]), None, 20, 5).unwrap();
        assert!(frame.contains("\x1b[2;1H\x1b[0m\x1b[0m\x1b[K"), "{frame:?}");
        assert!(frame.contains("\x1b[3;1H\x1b[0m\x1b[0m\x1b[K"), "{frame:?}");
    }

    #[test]
    fn invalidating_or_resizing_rewrites_everything() {
        let mut s = Screen::new();
        s.compose(&rows(&["a", "b"]), None, 20, 5);
        s.invalidate();
        let frame = s.compose(&rows(&["a", "b"]), None, 20, 5).unwrap();
        assert!(frame.contains('a') && frame.contains('b'));
        let frame = s.compose(&rows(&["a", "b"]), None, 30, 5).unwrap();
        assert!(frame.contains('a') && frame.contains('b'));
    }

    #[test]
    fn a_full_row_ends_in_a_new_line_not_an_erase_and_the_bottom_row_is_one_short() {
        let mut s = Screen::new();
        let full = "x".repeat(10);
        let frame = s.compose(&rows(&[&full, "", &full]), None, 10, 3).unwrap();
        assert!(frame.contains(&format!("\x1b[1;1H\x1b[0m{full}\x1b[0m\r\n\x1b[2;1H")), "{frame:?}");
        assert!(frame.contains(&format!("\x1b[3;1H\x1b[0m{}\x1b[0m\x1b[K", "x".repeat(9))), "{frame:?}");
    }

    #[test]
    fn the_cursor_is_hidden_while_drawing_and_shown_only_where_asked() {
        let mut s = Screen::new();
        let typing = s.compose(&rows(&["> hi"]), Some((0, 4)), 20, 5).unwrap();
        assert!(typing.starts_with(&format!("{SYNC_BEGIN}{HIDE_CURSOR}")));
        assert!(typing.ends_with(&format!("\x1b[1;5H{SHOW_CURSOR}{SYNC_END}")), "{typing:?}");
        let menu = s.compose(&rows(&["> hi", "menu"]), None, 20, 5).unwrap();
        assert!(!menu.contains(SHOW_CURSOR), "{menu:?}");
    }

    #[test]
    fn taskbar_progress_speaks_windows_terminal() {
        assert_eq!(Progress::Busy.sequence(), "\x1b]9;4;3;0\x07");
        assert_eq!(Progress::Waiting.sequence(), "\x1b]9;4;4;100\x07");
        assert_eq!(Progress::Percent(250).sequence(), "\x1b]9;4;1;100\x07");
        assert_eq!(Progress::None.sequence(), "\x1b]9;4;0;0\x07");
    }

    #[test]
    fn rows_below_the_screen_are_dropped() {
        let mut s = Screen::new();
        let frame = s.compose(&rows(&["1", "2", "3", "4"]), None, 20, 2).unwrap();
        assert!(!frame.contains("\x1b[3;1H"), "{frame:?}");
    }
}
