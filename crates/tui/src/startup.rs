//! Startup directory trust confirmation screen.
//! Prompts user whether to trust the current working directory,
//! change working directory, or quit.

use std::path::PathBuf;
use crossterm::event::{KeyCode, KeyModifiers};

/// User choice from the startup trust screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    Continue,
    Quit,
}

/// Mode of the trust screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustScreenMode {
    /// Choosing between: 1. Yes, Continue. 2. Change working directory. 3. No, quit.
    Select,
    /// Typing a new directory path.
    ChangeDir {
        input: String,
        error: Option<String>,
    },
}

/// State of the startup trust screen.
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

    /// Move selection up (wraps around 0..=2).
    pub fn up(&mut self) {
        self.selected = (self.selected + 2) % 3;
    }

    /// Move selection down (wraps around 0..=2).
    pub fn down(&mut self) {
        self.selected = (self.selected + 1) % 3;
    }

    /// Handle key event. Returns `Some(action)` when an action completes the screen.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<StartupAction> {
        match &mut self.mode {
            TrustScreenMode::Select => match code {
                KeyCode::Esc => Some(StartupAction::Quit),
                KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('с') | KeyCode::Char('С')
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
                KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('с') | KeyCode::Char('С')
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

    /// Render screen lines matching the requested design and aesthetics.
    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let reset = "\x1b[0m";
        let prompt_style = "\x1b[1;38;2;225;175;95m"; // amber accent
        let text_dim = "\x1b[38;2;160;155;145m";
        let text_bright = "\x1b[1;38;2;240;235;225m";
        let text_footer = "\x1b[38;2;135;130;125m";
        let text_error = "\x1b[1;38;2;235;105;105m";

        match &self.mode {
            TrustScreenMode::Select => {
                lines.push(format!(
                    "{prompt_style}> {text_dim}You are in {text_bright}{}{reset}",
                    self.cwd.display()
                ));
                lines.push(String::new());

                let desc = "Do you trust the contents of this directory? Working with untrusted contents comes with higher risk of prompt injection. Trusting the directory allows project-local config, hooks, and exec policies to load.";
                let wrapped = wrap_words(desc, width.saturating_sub(4).max(20));
                for l in wrapped {
                    lines.push(format!("  {text_dim}{l}{reset}"));
                }
                lines.push(String::new());

                let options = [
                    "1. Yes, Continue.",
                    "2. Change working directory.",
                    "3. No, quit.",
                ];

                for (idx, opt) in options.iter().enumerate() {
                    if idx == self.selected {
                        lines.push(format!("{prompt_style}> {text_bright}{opt}{reset}"));
                    } else {
                        lines.push(format!("  {text_dim}{opt}{reset}"));
                    }
                }

                lines.push(String::new());
                lines.push(format!("  {text_footer}Press enter to continue{reset}"));
            }
            TrustScreenMode::ChangeDir { input, error } => {
                lines.push(format!(
                    "{prompt_style}> {text_bright}Change working directory{reset}"
                ));
                lines.push(String::new());
                lines.push(format!(
                    "  {text_dim}Enter directory path (relative or absolute, ~ for home):{reset}"
                ));
                lines.push(format!(
                    "  {prompt_style}❯ {text_bright}{input}\x1b[7m {reset}"
                ));
                if let Some(err) = error {
                    lines.push(format!("  {text_error}Error: {err}{reset}"));
                } else {
                    lines.push(String::new());
                }
                lines.push(String::new());
                lines.push(format!(
                    "  {text_footer}enter — confirm · esc — cancel and go back{reset}"
                ));
            }
        }

        lines
    }
}

/// Helper to expand `~` and parse paths.
pub fn resolve_path(input: &str) -> PathBuf {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
    if input == "~" {
        if let Ok(h) = home {
            return PathBuf::from(h);
        }
    } else if let Some(rest) = input.strip_prefix("~/") {
        if let Ok(h) = home {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(input)
}

/// Helper to wrap text without breaking words.
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

/// Run interactive trust screen in terminal.
pub async fn run_trust_screen(cwd: &mut PathBuf) -> anyhow::Result<StartupAction> {
    use crossterm::{
        cursor,
        event::{Event, KeyEventKind},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use std::io::Write;

    enable_raw_mode()?;
    #[cfg(windows)]
    let _ = crossterm::terminal::enable_virtual_terminal_processing();
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, cursor::Hide);

    let mut screen = TrustScreen::new(cwd.clone());

    let res = loop {
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        let lines = screen.render(term_w as usize);

        let mut buffer = String::from("\x1b[H\x1b[2J");
        for line in lines {
            buffer.push_str(&line);
            buffer.push_str("\r\n");
        }
        let _ = stdout.write_all(buffer.as_bytes());
        let _ = stdout.flush();

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
    #[cfg(windows)]
    let _ = crossterm::terminal::disable_virtual_terminal_processing();

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
        // Enter on 0 -> Continue
        let act = screen.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(act, Some(StartupAction::Continue));

        // Enter on 2 -> Quit
        screen.selected = 2;
        let act = screen.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(act, Some(StartupAction::Quit));

        // Esc -> Quit
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

        // Type invalid path
        for c in "nonexistent_dir_12345".chars() {
            screen.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        screen.handle_key(KeyCode::Enter, KeyModifiers::empty());
        if let TrustScreenMode::ChangeDir { ref error, .. } = screen.mode {
            assert!(error.is_some());
        } else {
            panic!("Expected ChangeDir mode");
        }

        // Cancel with Esc returns to Select mode
        screen.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(screen.mode, TrustScreenMode::Select);
    }

    #[test]
    fn test_startup_rendering_content() {
        let screen = TrustScreen::new(PathBuf::from("/test/path"));
        let rendered = screen.render(80);
        let joined = rendered.join("\n");
        assert!(joined.contains("You are in"));
        assert!(joined.contains("/test/path"));
        assert!(joined.contains("Do you trust the contents of this directory?"));
        assert!(joined.contains("1. Yes, Continue."));
        assert!(joined.contains("2. Change working directory."));
        assert!(joined.contains("3. No, quit."));
        assert!(joined.contains("Press enter to continue"));
    }
}
