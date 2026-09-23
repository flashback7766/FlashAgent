//! Startup trust screen: trust the working directory, change it, or quit.

use std::path::PathBuf;
use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    Continue,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustScreenMode {
    /// 1. Yes, continue. 2. Change folder. 3. No, quit.
    Select,
    ChangeDir {
        input: String,
        error: Option<String>,
    },
}

pub struct TrustScreen {
    pub cwd: PathBuf,
    pub selected: usize,
    pub mode: TrustScreenMode,
}

impl TrustScreen {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            selected: 0,
            mode: TrustScreenMode::Select,
        }
    }

    /// Wraps around.
    pub fn up(&mut self) {
        self.selected = (self.selected + 2) % 3;
    }

    /// Wraps around.
    pub fn down(&mut self) {
        self.selected = (self.selected + 1) % 3;
    }

    /// `Some(action)` when the screen is done.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<StartupAction> {
        match &mut self.mode {
            TrustScreenMode::Select => match code {
                KeyCode::Esc => Some(StartupAction::Quit),
                KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                    if mods.contains(KeyModifiers::CONTROL) =>
                {
                    Some(StartupAction::Quit)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.up();
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.down();
                    None
                }
                KeyCode::Char('1') => {
                    self.selected = 0;
                    None
                }
                KeyCode::Char('2') => {
                    self.selected = 1;
                    None
                }
                KeyCode::Char('3') => {
                    self.selected = 2;
                    None
                }
                KeyCode::Enter => match self.selected {
                    0 => Some(StartupAction::Continue),
                    1 => {
                        self.mode = TrustScreenMode::ChangeDir {
                            input: String::new(),
                            error: None,
                        };
                        None
                    }
                    2 => Some(StartupAction::Quit),
                    _ => None,
                },
                _ => None,
            },
            TrustScreenMode::ChangeDir { input, error } => match code {
                KeyCode::Esc => {
                    self.mode = TrustScreenMode::Select;
                    None
                }
                KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                    if mods.contains(KeyModifiers::CONTROL) =>
                {
                    Some(StartupAction::Quit)
                }
                KeyCode::Backspace => {
                    input.pop();
                    *error = None;
                    None
                }
                KeyCode::Enter => {
                    let trimmed = input.trim();
                    if trimmed.is_empty() {
                        *error = Some("Path cannot be empty".to_string());
                        return None;
                    }
                    let resolved = resolve_path(trimmed);
                    if resolved.is_dir() {
                        let canon = std::fs::canonicalize(&resolved).unwrap_or(resolved);
                        if let Err(e) = std::env::set_current_dir(&canon) {
                            *error = Some(format!("Failed to enter directory: {e}"));
                        } else {
                            self.cwd = canon;
                            self.selected = 0;
                            self.mode = TrustScreenMode::Select;
                        }
                    } else if resolved.exists() {
                        *error = Some(format!("'{}' is not a directory", trimmed));
                    } else {
                        *error = Some(format!("Directory '{}' does not exist", trimmed));
                    }
                    None
                }
                KeyCode::Char(c)
                    if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) =>
                {
                    input.push(c);
                    *error = None;
                    None
                }
                _ => None,
            },
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let reset = "\x1b[0m";
        let prompt_style = "\x1b[1;38;2;225;175;95m"; // amber accent
        let text_dim = "\x1b[38;2;160;155;145m";
        let text_bright = "\x1b[1;38;2;240;235;225m";
        let text_error = "\x1b[1;38;2;235;105;105m";

        match &self.mode {
            TrustScreenMode::Select => {
                lines.push(format!("{prompt_style}FlashAgent{reset} {text_dim}\u{b7} trust this folder?{reset}"));
                lines.push(String::new());
                // The folder's own name is at the end of the path: in a narrow window
                // the start goes, never the name of what is being trusted.
                let path = self.cwd.display().to_string();
                let (shown, _) = crate::tail_window(&path, width.saturating_sub(4).max(10));
                lines.push(format!("  {text_bright}{shown}{reset}"));
                lines.push(String::new());

                let desc = "FlashAgent will read, edit and run commands here. Continue only if you trust \
                            its files: they can carry instructions aimed at the model.";
                for l in wrap_words(desc, width.saturating_sub(4).max(20)) {
                    lines.push(format!("  {text_dim}{l}{reset}"));
                }
                lines.push(String::new());

                let options = ["1. Yes, continue", "2. Change folder", "3. No, quit"];
                for (idx, opt) in options.iter().enumerate() {
                    if idx == self.selected {
                        lines.push(format!("{prompt_style}\u{25b8} {text_bright}{opt}{reset}"));
                    } else {
                        lines.push(format!("  {text_dim}{opt}{reset}"));
                    }
                }

                lines.push(String::new());
                let hints = [("\u{2191}/\u{2193}", "select"), ("Enter", "confirm"), ("Esc", "quit")];
                lines.push(format!("  {}", crate::key_hints(&hints, width.saturating_sub(2))));
            }
            TrustScreenMode::ChangeDir { input, error } => {
                lines.push(format!("{prompt_style}FlashAgent{reset} {text_dim}\u{b7} change folder{reset}"));
                lines.push(String::new());
                lines.push(format!(
                    "  {text_dim}Folder to work in (relative, absolute, or starting with ~):{reset}"
                ));
                lines.push(format!(
                    "  {prompt_style}› {text_bright}{input}\x1b[7m {reset}"
                ));
                if let Some(err) = error {
                    lines.push(format!("  {text_error}Error: {err}{reset}"));
                } else {
                    lines.push(String::new());
                }
                lines.push(String::new());
                let hints = [("Enter", "open"), ("Esc", "back")];
                lines.push(format!("  {}", crate::key_hints(&hints, width.saturating_sub(2))));
            }
        }

        lines
    }
}

/// `~` is expanded in one place for the whole app, so the folder picker and
/// the tools agree.
pub fn resolve_path(input: &str) -> PathBuf {
    PathBuf::from(flashagent_core::expand_home(input).as_ref())
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            out.push(cur);
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub async fn run_trust_screen(cwd: &mut PathBuf) -> anyhow::Result<StartupAction> {
    use crossterm::{
        cursor,
        event::{Event, KeyEventKind},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, cursor::Hide);

    let mut screen = TrustScreen::new(cwd.clone());
    let mut painter = crate::screen::Screen::new();

    let res = loop {
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        crate::screen::paint_page(&mut painter, &screen.render(term_w as usize), false);

        if crossterm::event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind != KeyEventKind::Release {
                    if let Some(action) = screen.handle_key(key.code, key.modifiers) {
                        break action;
                    }
                }
            }
        }
    };

    if res == StartupAction::Continue {
        *cwd = screen.cwd;
    }

    let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
    disable_raw_mode()?;

    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_startup_initial_selection() {
        let screen = TrustScreen::new(PathBuf::from("/tmp"));
        assert_eq!(screen.selected, 0);
        assert_eq!(screen.mode, TrustScreenMode::Select);
    }

    #[test]
    fn test_startup_navigation_and_numbers() {
        let mut screen = TrustScreen::new(PathBuf::from("/tmp"));
        screen.down();
        assert_eq!(screen.selected, 1);
        screen.down();
        assert_eq!(screen.selected, 2);
        screen.down();
        assert_eq!(screen.selected, 0);
        screen.up();
        assert_eq!(screen.selected, 2);

        screen.handle_key(KeyCode::Char('1'), KeyModifiers::empty());
        assert_eq!(screen.selected, 0);
        screen.handle_key(KeyCode::Char('3'), KeyModifiers::empty());
        assert_eq!(screen.selected, 2);
        screen.handle_key(KeyCode::Char('2'), KeyModifiers::empty());
        assert_eq!(screen.selected, 1);
    }

    #[test]
    fn test_startup_actions() {
        let mut screen = TrustScreen::new(PathBuf::from("/tmp"));
        let act = screen.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(act, Some(StartupAction::Continue));

        screen.selected = 2;
        let act = screen.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(act, Some(StartupAction::Quit));

        let act = screen.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(act, Some(StartupAction::Quit));
    }

    #[test]
    fn test_startup_change_directory_flow() {
        let mut screen = TrustScreen::new(PathBuf::from("/tmp"));
        screen.selected = 1;
        let act = screen.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(act, None);
        assert!(matches!(screen.mode, TrustScreenMode::ChangeDir { .. }));

        for c in "nonexistent_dir_12345".chars() {
            screen.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        screen.handle_key(KeyCode::Enter, KeyModifiers::empty());
        if let TrustScreenMode::ChangeDir { ref error, .. } = screen.mode {
            assert!(error.is_some());
        } else {
            panic!("Expected ChangeDir mode");
        }

        // Esc returns to Select mode.
        screen.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(screen.mode, TrustScreenMode::Select);
    }

    #[test]
    fn test_startup_rendering_content() {
        let screen = TrustScreen::new(PathBuf::from("/test/path"));
        let rendered = screen.render(80);
        let joined = rendered.join("\n");
        assert!(joined.contains("trust this folder?"));
        assert!(joined.contains("/test/path"));
        assert!(joined.contains("read, edit and run commands"));
        assert!(joined.contains("1. Yes, continue"));
        assert!(joined.contains("2. Change folder"));
        assert!(joined.contains("3. No, quit"));
        assert!(joined.contains("Esc"));
    }
}
