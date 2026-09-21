//! The real `flashagent` binary in a pseudo-terminal, and the screen it drew.
//!
//! What a scenario checks is what a person would see: text on the screen,
//! not state inside the app.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

pub const ENTER: &str = "\r";
pub const ESC: &str = "\x1b";

/// A home directory of its own, so a scenario never reads or writes the
/// real `~/.flashagent`, and a config written into it.
pub struct Home {
    pub dir: tempfile::TempDir,
}

impl Home {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp home");
        std::fs::create_dir_all(dir.path().join(".flashagent")).unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        Home { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn config_path(&self) -> PathBuf {
        self.path().join(".flashagent").join("config.json")
    }

    /// The project directory the app is started in.
    pub fn work(&self) -> PathBuf {
        self.path().join("work")
    }

    pub fn write_config(&self, config: serde_json::Value) {
        std::fs::write(self.config_path(), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    }

    /// A config for someone who has set up already, pointed at `url`, who
    /// has read the notes of this version and is not checking for updates.
    pub fn set_up(&self, url: &str) {
        self.set_up_with(url, serde_json::json!({}));
    }

    /// As `set_up`, with `extra`'s fields merged in on top — overriding
    /// `model`, say, or adding `thinking_effort` — for a scenario that needs
    /// more than the baseline config.
    pub fn set_up_with(&self, url: &str, extra: serde_json::Value) {
        let mut config = serde_json::json!({
            "setup_completed": true,
            "backend_url": url,
            "model": super::mock_server::MODEL,
            "last_seen_version": version(),
            "auto_check_updates": false,
        });
        if let (Some(base), Some(extra)) = (config.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        self.write_config(config);
    }

    pub fn sessions(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.path().join(".flashagent").join("sessions"))
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }
}

/// The version the binary reports, as `last_seen_version` wants it.
pub fn version() -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_flashagent")).arg("--version").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().trim_start_matches("FlashAgent ").to_string()
}

pub struct Term {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
}

impl Term {
    /// Start `flashagent` with `args` in `home`, on a terminal of this size.
    pub fn start(home: &Home, args: &[&str], cols: u16, rows: u16) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .expect("open a pseudo-terminal");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_flashagent"));
        cmd.args(args);
        cmd.cwd(home.work());
        cmd.env("HOME", home.path());
        cmd.env("USERPROFILE", home.path());
        cmd.env("FLASHAGENT_CONFIG_PATH", home.config_path());
        cmd.env("TERM", "xterm-256color");
        cmd.env_remove("FLASHAGENT_API_KEY");
        cmd.env_remove("FLASHAGENT_TRUST_DIR");
        let child = pty.slave.spawn_command(cmd).expect("start flashagent");
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let writer = Arc::new(Mutex::new(pty.master.take_writer().expect("pty writer")));
        let mut reader = pty.master.try_clone_reader().expect("pty reader");
        let (p2, w2) = (parser.clone(), writer.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let bytes = &buf[..n];
                let mut parser = p2.lock().unwrap();
                parser.process(bytes);
                // A real terminal answers "where is the cursor?"; this one
                // must too, or the app waits for an answer that never comes.
                if contains(bytes, b"\x1b[6n") {
                    let (row, col) = parser.screen().cursor_position();
                    let _ = w2.lock().unwrap().write_all(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
                }
            }
        });
        Term { parser, writer, child, _master: pty.master }
    }

    /// The screen as a person sees it, one line per row.
    /// The screen with its colours, as the escape sequences a terminal would
    /// need to redraw it. Used to check what colour something came out.
    #[allow(dead_code)]
    pub fn screen_colours(&self) -> String {
        String::from_utf8_lossy(&self.parser.lock().unwrap().screen().contents_formatted()).into_owned()
    }

    pub fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    /// Type keys: text, or the constants above.
    pub fn send(&self, keys: &str) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(keys.as_bytes()).unwrap();
        w.flush().unwrap();
    }

    /// Type text a character at a time, the way it arrives from a keyboard
    /// rather than as one paste.
    pub fn type_text(&self, text: &str) {
        for c in text.chars() {
            self.send(&c.to_string());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait until `needle` is on the screen. On a timeout the test fails with
    /// the screen it was looking at.
    pub fn wait_for(&self, needle: &str, timeout: Duration) {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if screen.contains(needle) {
                return;
            }
            if start.elapsed() > timeout {
                panic!("waited {timeout:?} for {needle:?}; the screen was:\n{}", framed(&screen));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    /// Wait until `needle` has left the screen.
    pub fn wait_gone(&self, needle: &str, timeout: Duration) {
        let start = Instant::now();
        while self.screen().contains(needle) {
            if start.elapsed() > timeout {
                panic!("waited {timeout:?} for {needle:?} to go; the screen was:\n{}", framed(&self.screen()));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    /// Wait for the app to exit; `None` if it is still running at the timeout.
    pub fn wait_exit(&mut self, timeout: Duration) -> Option<portable_pty::ExitStatus> {
        let start = Instant::now();
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if start.elapsed() > timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn framed(screen: &str) -> String {
    screen.lines().map(|l| format!("  |{l}")).collect::<Vec<_>>().join("\n")
}
