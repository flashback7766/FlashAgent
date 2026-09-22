//! Sent prompts, kept between launches so ↑ and Ctrl+F reach older ones. One
//! JSON string per line in `~/.flashagent/prompt_history.jsonl`; appends keep
//! concurrent writers from mixing bytes. Saved only when sessions are saved.

use super::*;

/// Older ones are dropped when the file is read.
const KEEP: usize = 1000;

fn history_path() -> Option<std::path::PathBuf> {
    flashagent_home_dir().map(|home| home.join("prompt_history.jsonl"))
}

/// Oldest first.
pub(crate) fn load() -> Vec<String> {
    history_path().map(|p| load_from(&p)).unwrap_or_default()
}

fn load_from(path: &std::path::Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else { return Vec::new() };
    // A line cut short by a crash is skipped.
    let mut entries: Vec<String> = content.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    if entries.len() > KEEP {
        entries.drain(..entries.len() - KEEP);
        // Written back shorter, so the file does not grow forever.
        let body: String = entries.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
        let tmp = path.with_extension("jsonl.tmp");
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
    entries
}

fn append_to(path: &std::path::Path, prompt: &str) {
    use std::io::Write;
    let Ok(line) = serde_json::to_string(prompt) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

impl App {
    /// Appended once, and saved when sessions are kept.
    pub(crate) fn remember_prompt(&mut self, text: &str) {
        if text.trim().is_empty() || self.input_history.last().is_some_and(|last| last == text) {
            return;
        }
        self.input_history.push(text.to_string());
        if self.config.auto_save_sessions {
            if let Some(path) = history_path() {
                append_to(&path, text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_come_back_whole_and_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        append_to(&path, "first");
        append_to(&path, "two\nlines");
        std::fs::OpenOptions::new().append(true).open(&path).and_then(|mut f| {
            use std::io::Write;
            f.write_all(b"\"cut sho")
        }).unwrap();
        assert_eq!(load_from(&path), ["first", "two\nlines"]);
    }

    #[test]
    fn only_the_newest_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        for n in 0..KEEP + 5 {
            append_to(&path, &n.to_string());
        }
        let loaded = load_from(&path);
        assert_eq!(loaded.len(), KEEP);
        assert_eq!(loaded[0], "5");
        assert_eq!(load_from(&path).len(), KEEP, "the file was not written back shorter");
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), KEEP);
    }
}
