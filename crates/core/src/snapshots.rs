//! Per-turn file snapshots, so a turn can be taken back: files it wrote are
//! restored and the conversation returns to before it. Only the file tools
//! are recorded; changes made through the shell are not, and rewind says so.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Larger files are not copied; rewind reports them.
pub const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;

const WRITING_TOOLS: &[&str] = &["write_file", "edit_file", "patch_file"];
/// Project memory is files in the project like any other; global memory in
/// `~/.flashagent` is the user's own and is left as it is.
const MEMORY_TOOLS: &[&str] = &["memory_create", "memory_update", "memory_remove"];

/// State before the turn first touched the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Before {
    /// Rewind removes it.
    Missing,
    /// Blob file name under the store's directory.
    Blob(String),
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBefore {
    pub path: PathBuf,
    pub before: Before,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSnapshot {
    pub seq: u64,
    /// Hash of the user message that started the turn: it finds the turn again
    /// after compaction or steering has changed the history.
    pub prompt_hash: u64,
    pub files: Vec<FileBefore>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewindable {
    /// Index among the history's user messages.
    pub user_index: usize,
    pub turn: usize,
    pub files: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RewindReport {
    pub restored: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Worked out without touching the file, to show before the user confirms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePreview {
    pub path: PathBuf,
    pub added: usize,
    pub removed: usize,
    /// The turn being taken back created this file.
    pub will_delete: bool,
    /// No copy was kept; the file would stay as it is.
    pub too_large: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    turns: Vec<TurnSnapshot>,
    next_blob: u64,
}

struct State {
    manifest: Manifest,
    /// `None` until a turn begins.
    current: Option<usize>,
}

/// Kept in `dir` so they survive `--resume`.
pub struct SnapshotStore {
    dir: PathBuf,
    cwd: PathBuf,
    state: Mutex<State>,
}

/// FNV-1a: stable across runs and builds, so a resumed session finds its turns.
pub fn prompt_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl SnapshotStore {
    pub fn open(dir: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let manifest = std::fs::read_to_string(dir.join("manifest.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        // Canonical, like the recorded paths: the same folder is `/var` and
        // `/private/var` on macOS, and short and long forms on Windows.
        let cwd = cwd.into();
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        Self { dir, cwd, state: Mutex::new(State { manifest, current: None }) }
    }

    pub fn begin_turn(&self, prompt: &str) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let seq = st.manifest.turns.last().map_or(1, |t| t.seq + 1);
        st.manifest.turns.push(TurnSnapshot { seq, prompt_hash: prompt_hash(prompt), files: Vec::new() });
        st.current = Some(st.manifest.turns.len() - 1);
        self.save(&st.manifest);
    }

    /// Regenerate keeps recording against the same turn, so rewinding reaches
    /// before the first attempt.
    pub fn continue_turn(&self, prompt: &str) {
        let hash = prompt_hash(prompt);
        let found = {
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let found = st.manifest.turns.iter().rposition(|t| t.prompt_hash == hash);
            if found.is_some() {
                st.current = found;
            }
            found
        };
        if found.is_none() {
            self.begin_turn(prompt);
        }
    }

    /// Called before a tool runs: keeps a file this turn has not touched yet.
    pub fn before_write(&self, tool: &str, args_json: &str) {
        if MEMORY_TOOLS.contains(&tool) {
            let args: Option<serde_json::Value> = serde_json::from_str(args_json).ok();
            if let Some(title) = args.as_ref().and_then(|a| a.get("title")).and_then(|t| t.as_str()) {
                let store = crate::memory_store::Store::new(&self.cwd);
                self.keep_before(store.dir().join(format!("{}.md", crate::memory_store::slugify(title))));
                self.keep_before(store.index_path());
            }
            return;
        }
        if !WRITING_TOOLS.contains(&tool) {
            return;
        }
        let Some(args) = flashagent_llm::effective_args(args_json, tool) else {
            return;
        };
        let single = args.get("path").and_then(|p| p.as_str());
        let batch = args.get("files").and_then(|f| f.as_array()).into_iter().flatten().filter_map(|f| f.get("path").and_then(|p| p.as_str()));
        let raws: Vec<String> = single.into_iter().chain(batch).filter(|p| !p.trim().is_empty()).map(str::to_string).collect();
        for raw in raws {
            self.keep_before(self.resolve(&raw));
        }
    }

    fn keep_before(&self, path: PathBuf) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(current) = st.current else {
            return;
        };
        if st.manifest.turns[current].files.iter().any(|f| f.path == path) {
            return;
        }
        let before = match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Before::Missing,
            // Not recorded: marking it missing would delete it on rewind.
            Err(_) => return,
            Ok(meta) if !meta.is_file() => return,
            Ok(meta) if meta.len() > MAX_SNAPSHOT_BYTES => Before::TooLarge,
            Ok(_) => {
                let Ok(bytes) = std::fs::read(&path) else {
                    return;
                };
                let name = format!("{}.bin", st.manifest.next_blob);
                let blobs = self.dir.join("blobs");
                if std::fs::create_dir_all(&blobs).is_err() || std::fs::write(blobs.join(&name), bytes).is_err() {
                    return;
                }
                st.manifest.next_blob += 1;
                Before::Blob(name)
            }
        };
        st.manifest.turns[current].files.push(FileBefore { path, before });
        self.save(&st.manifest);
    }

    /// Oldest first. Messages without a turn (steering) are skipped, and so are
    /// turns whose message was compacted away.
    pub fn rewindable(&self, user_messages: &[&str]) -> Vec<Rewindable> {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let turns = &st.manifest.turns;
        let mut found = Vec::new();
        let mut limit = turns.len();
        for (index, message) in user_messages.iter().enumerate().rev() {
            let hash = prompt_hash(message);
            if let Some(turn) = turns[..limit].iter().rposition(|t| t.prompt_hash == hash) {
                found.push(Rewindable { user_index: index, turn, files: turns[turn].files.len() });
                limit = turn;
            }
        }
        found.reverse();
        found
    }

    /// Also takes back every later turn: each file returns to how it was before
    /// the first of them touched it.
    pub fn rewind(&self, turn: usize) -> RewindReport {
        let mut report = RewindReport::default();
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if turn >= st.manifest.turns.len() {
            return report;
        }
        let mut taken_back: Vec<TurnSnapshot> = st.manifest.turns.drain(turn..).collect();
        let mut seen = HashSet::new();
        // Oldest first, so the first record of a path is the original state.
        for file in taken_back.iter().flat_map(|t| &t.files) {
            if !seen.insert(file.path.clone()) {
                continue;
            }
            match &file.before {
                Before::Missing => match std::fs::remove_file(&file.path) {
                    Ok(()) => report.removed.push(file.path.clone()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => report.failed.push((file.path.clone(), e.to_string())),
                },
                Before::Blob(name) => match std::fs::read(self.dir.join("blobs").join(name)) {
                    Ok(bytes) => {
                        if let Some(parent) = file.path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match std::fs::write(&file.path, bytes) {
                            Ok(()) => report.restored.push(file.path.clone()),
                            Err(e) => report.failed.push((file.path.clone(), e.to_string())),
                        }
                    }
                    Err(e) => report.failed.push((file.path.clone(), format!("its saved copy is gone: {e}"))),
                },
                Before::TooLarge => report.failed.push((
                    file.path.clone(),
                    format!("larger than {} MB, so no copy was kept", MAX_SNAPSHOT_BYTES / (1024 * 1024)),
                )),
            }
        }
        let failed: HashSet<&PathBuf> = report.failed.iter().map(|(path, _)| path).collect();
        let mut pending = Vec::new();
        let mut retained = HashSet::new();
        for file in taken_back.iter().flat_map(|t| &t.files) {
            // Keep the earliest copy of each failed path: the state being restored.
            if failed.contains(&file.path) && retained.insert(file.path.clone()) {
                pending.push(file.clone());
                continue;
            }
            if let Before::Blob(name) = &file.before {
                let _ = std::fs::remove_file(self.dir.join("blobs").join(name));
            }
        }
        if !pending.is_empty() {
            let mut retry = taken_back.remove(0);
            retry.files = pending;
            st.manifest.turns.push(retry);
        }
        st.current = None;
        self.save(&st.manifest);
        report
    }

    /// Same "first record wins" rule as `rewind`.
    pub fn preview_rewind(&self, turn: usize) -> Vec<FilePreview> {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if turn >= st.manifest.turns.len() {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for file in st.manifest.turns[turn..].iter().flat_map(|t| &t.files) {
            if !seen.insert(file.path.clone()) {
                continue;
            }
            match &file.before {
                // Created and gone again, or never written (a global memory).
                Before::Missing if !file.path.exists() => {}
                Before::Missing => {
                    let current = std::fs::read_to_string(&file.path).unwrap_or_default();
                    out.push(FilePreview {
                        path: file.path.clone(),
                        added: 0,
                        removed: current.lines().count(),
                        will_delete: true,
                        too_large: false,
                    });
                }
                Before::TooLarge => {
                    out.push(FilePreview { path: file.path.clone(), added: 0, removed: 0, will_delete: false, too_large: true });
                }
                Before::Blob(name) => {
                    let current = std::fs::read_to_string(&file.path).unwrap_or_default();
                    let before = std::fs::read(self.dir.join("blobs").join(name))
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default();
                    // From the current file to `before`: `+` is brought back, `-` taken away.
                    let diff = crate::diff::unified(Some(&current), &before, "f", 0);
                    // Past the two header lines: `-- sql` and `++i;` are content.
                    let body = || diff.lines().skip(2);
                    let added = body().filter(|l| l.starts_with('+')).count();
                    let removed = body().filter(|l| l.starts_with('-')).count();
                    out.push(FilePreview { path: file.path.clone(), added, removed, will_delete: false, too_large: false });
                }
            }
        }
        out
    }

    /// Relative to the project when inside it.
    pub fn display_path(&self, path: &Path) -> String {
        let shown = path.strip_prefix(&self.cwd).unwrap_or(path);
        // Never shows Windows' `\\?\` prefix that `canonicalize` adds.
        let text = shown.display().to_string();
        match text.strip_prefix(r"\\?\UNC\") {
            Some(rest) => format!(r"\\{rest}"),
            None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
        }
    }

    fn resolve(&self, raw: &str) -> PathBuf {
        let joined = crate::paths::resolve_path(&self.cwd, raw);
        // Resolve existing links before `..`; keep missing components so a new file
        // can be recorded before the write.
        let mut out = PathBuf::new();
        for part in joined.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => {
                    out.push(other.as_os_str());
                    if let Ok(resolved) = out.canonicalize() {
                        out = resolved;
                    }
                }
            }
        }
        out
    }

    fn save(&self, manifest: &Manifest) {
        if std::fs::create_dir_all(&self.dir).is_ok() {
            if let Ok(text) = serde_json::to_string(manifest) {
                let _ = std::fs::write(self.dir.join("manifest.json"), text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, tempfile::TempDir, SnapshotStore) {
        let project = tempfile::tempdir().unwrap();
        let snaps = tempfile::tempdir().unwrap();
        let s = SnapshotStore::open(snaps.path(), project.path());
        (project, snaps, s)
    }

    /// Canonical, so a folder reached through a symlink compares equal; a deleted
    /// file is named from its folder.
    fn canon(path: &Path) -> PathBuf {
        if let Ok(resolved) = path.canonicalize() {
            return resolved;
        }
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => canon(parent).join(name),
            _ => path.to_path_buf(),
        }
    }

    fn args(path: &str) -> String {
        serde_json::json!({ "path": path }).to_string()
    }

    #[test]
    fn taking_turns_back_restores_edited_files_and_removes_created_ones() {
        let (project, _snaps, s) = store();
        let a = project.path().join("a.txt");
        std::fs::write(&a, "original").unwrap();

        s.begin_turn("first prompt");
        s.before_write("edit_file", &args("a.txt"));
        std::fs::write(&a, "first").unwrap();

        s.begin_turn("second prompt");
        s.before_write("edit_file", &args("./a.txt"));
        std::fs::write(&a, "second").unwrap();
        s.before_write("write_file", &args("sub/new.txt"));
        std::fs::create_dir_all(project.path().join("sub")).unwrap();
        std::fs::write(project.path().join("sub/new.txt"), "new").unwrap();

        let report = s.rewind(1);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "first");
        assert!(!project.path().join("sub/new.txt").exists(), "a file the turn created is gone");
        assert_eq!(report.restored, vec![canon(&a)]);
        assert_eq!(report.removed, vec![canon(&project.path().join("sub").join("new.txt"))]);
        assert!(report.failed.is_empty());

        s.rewind(0);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "original");
    }

    #[test]
    fn taking_back_several_turns_at_once_reaches_before_the_first_of_them() {
        let (project, _snaps, s) = store();
        let a = project.path().join("a.txt");
        std::fs::write(&a, "original").unwrap();
        for (prompt, text) in [("one", "1"), ("two", "2"), ("three", "3")] {
            s.begin_turn(prompt);
            s.before_write("write_file", &args("a.txt"));
            std::fs::write(&a, text).unwrap();
        }
        s.rewind(0);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "original");
    }

    #[test]
    fn only_how_a_file_was_before_the_turn_is_kept() {
        let (project, _snaps, s) = store();
        let a = project.path().join("a.txt");
        std::fs::write(&a, "original").unwrap();
        s.begin_turn("p");
        s.before_write("edit_file", &args("a.txt"));
        std::fs::write(&a, "halfway").unwrap();
        s.before_write("edit_file", &args("a.txt"));
        std::fs::write(&a, "done").unwrap();
        s.rewind(0);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "original", "not the state between two edits");
    }

    #[test]
    fn a_regenerated_answer_is_taken_back_to_before_the_first_attempt() {
        let (project, snaps, s) = store();
        let a = project.path().join("a.txt");
        std::fs::write(&a, "original").unwrap();
        s.begin_turn("fix it");
        s.before_write("write_file", &args("a.txt"));
        std::fs::write(&a, "attempt 1").unwrap();
        let resumed = SnapshotStore::open(snaps.path(), project.path());
        resumed.continue_turn("fix it");
        resumed.before_write("write_file", &args("a.txt"));
        std::fs::write(&a, "attempt 2").unwrap();
        let turns = resumed.rewindable(&["fix it"]);
        assert_eq!(turns.len(), 1);
        resumed.rewind(turns[0].turn);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "original");
    }

    #[test]
    fn turns_are_found_again_after_compaction_and_steering() {
        let (_project, _snaps, s) = store();
        s.begin_turn("p1");
        s.begin_turn("p2");
        s.begin_turn("p3");
        // p1 was compacted away; "also check the tests" was steering.
        let found = s.rewindable(&["p2", "also check the tests", "p3"]);
        assert_eq!(
            found,
            vec![
                Rewindable { user_index: 0, turn: 1, files: 0 },
                Rewindable { user_index: 2, turn: 2, files: 0 },
            ]
        );
    }

    #[test]
    fn a_session_resumed_from_disk_can_still_take_turns_back() {
        let (project, snaps, s) = store();
        let a = project.path().join("a.txt");
        std::fs::write(&a, "original").unwrap();
        s.begin_turn("p");
        s.before_write("patch_file", &args("a.txt"));
        std::fs::write(&a, "patched").unwrap();
        drop(s);

        let reopened = SnapshotStore::open(snaps.path(), project.path());
        let turns = reopened.rewindable(&["p"]);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].files, 1);
        reopened.rewind(turns[0].turn);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "original");
    }

    #[test]
    fn nothing_is_recorded_outside_a_turn_or_for_tools_that_do_not_write_files() {
        let (project, _snaps, s) = store();
        std::fs::write(project.path().join("a.txt"), "x").unwrap();
        s.before_write("write_file", &args("a.txt"));
        s.begin_turn("p");
        s.before_write("run_shell", r#"{"command":"rm a.txt"}"#);
        s.before_write("memory_create", &args("a.txt"));
        s.before_write("read_file", &args("a.txt"));
        assert_eq!(s.rewindable(&["p"])[0].files, 0);
    }

    #[test]
    fn a_project_memory_written_in_the_turn_is_taken_back_too() {
        let (project, _snaps, s) = store();
        let index = project.path().join("MEMORY.md");
        std::fs::write(&index, "# Memory\n").unwrap();
        s.begin_turn("p");
        s.before_write("memory_create", r#"{"title":"Tests use nextest","content":"x"}"#);
        std::fs::create_dir_all(project.path().join("memory")).unwrap();
        let note = project.path().join("memory").join("tests-use-nextest.md");
        std::fs::write(&note, "x").unwrap();
        std::fs::write(&index, "# Memory\n- tests-use-nextest\n").unwrap();
        let preview = s.preview_rewind(0);
        assert_eq!(preview.len(), 2, "{preview:?}");
        s.rewind(0);
        assert!(!note.exists());
        assert_eq!(std::fs::read_to_string(&index).unwrap(), "# Memory\n");
    }

    #[test]
    fn a_line_that_starts_like_a_diff_header_is_still_counted() {
        let (project, _snaps, s) = store();
        let a = project.path().join("q.sql");
        std::fs::write(&a, "-- sql comment\nselect 1;\n").unwrap();
        s.begin_turn("p");
        s.before_write("edit_file", &args("q.sql"));
        std::fs::write(&a, "++counter;\nselect 1;\n").unwrap();
        let preview = s.preview_rewind(0);
        assert_eq!((preview[0].added, preview[0].removed), (1, 1), "{preview:?}");
    }

    #[test]
    fn a_file_too_big_to_copy_is_reported_instead_of_silently_skipped() {
        let (project, _snaps, s) = store();
        let big = project.path().join("big.bin");
        std::fs::write(&big, vec![0u8; MAX_SNAPSHOT_BYTES as usize + 1]).unwrap();
        s.begin_turn("p");
        s.before_write("write_file", &args("big.bin"));
        std::fs::write(&big, "small now").unwrap();
        let report = s.rewind(0);
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].1.contains("no copy was kept"), "{:?}", report.failed);
    }

    #[test]
    fn every_file_of_a_batch_edit_is_kept() {
        let (project, _snaps, s) = store();
        std::fs::write(project.path().join("a.txt"), "a").unwrap();
        std::fs::write(project.path().join("b.txt"), "b").unwrap();
        s.begin_turn("p");
        s.before_write("edit_file", r#"{"files":[{"path":"a.txt","edits":[]},{"file_path":"b.txt","edits":[]}]}"#);
        std::fs::write(project.path().join("a.txt"), "A").unwrap();
        std::fs::write(project.path().join("b.txt"), "B").unwrap();
        let report = s.rewind(0);
        assert_eq!(std::fs::read_to_string(project.path().join("a.txt")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(project.path().join("b.txt")).unwrap(), "b");
        assert_eq!(report.restored.len(), 2);
    }

    #[test]
    fn a_preview_reports_the_change_without_making_it() {
        let (project, _snaps, s) = store();
        let a = project.path().join("a.txt");
        std::fs::write(&a, "one\ntwo\nthree\n").unwrap();
        s.begin_turn("p");
        s.before_write("edit_file", &args("a.txt"));
        std::fs::write(&a, "one\nTWO\nthree\nfour\n").unwrap();

        let preview = s.preview_rewind(0);
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].path, canon(&a));
        assert_eq!(preview[0].removed, 2, "{preview:?}");
        assert_eq!(preview[0].added, 1, "{preview:?}");
        assert!(!preview[0].will_delete);
        assert!(!preview[0].too_large);

        // Preview must not touch the file or the store.
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "one\nTWO\nthree\nfour\n");
        assert_eq!(s.rewindable(&["p"]).len(), 1);
    }

    #[test]
    fn previewing_a_file_the_turn_created_shows_it_will_be_removed() {
        let (project, _snaps, s) = store();
        s.begin_turn("p");
        s.before_write("write_file", &args("new.txt"));
        std::fs::write(project.path().join("new.txt"), "one\ntwo\n").unwrap();

        let preview = s.preview_rewind(0);
        assert_eq!(preview.len(), 1);
        assert!(preview[0].will_delete);
        assert_eq!(preview[0].removed, 2);
        assert_eq!(preview[0].added, 0);
    }

    #[test]
    fn previewing_several_turns_reaches_before_the_first_of_them() {
        let (project, _snaps, s) = store();
        let a = project.path().join("a.txt");
        std::fs::write(&a, "original\n").unwrap();
        for (prompt, text) in [("one", "1\n"), ("two", "2\n"), ("three", "3\n")] {
            s.begin_turn(prompt);
            s.before_write("write_file", &args("a.txt"));
            std::fs::write(&a, text).unwrap();
        }
        // One entry, diffed against the first "before", as rewind(0) restores.
        let preview = s.preview_rewind(0);
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].added, 1, "brings back \"original\"");
        assert_eq!(preview[0].removed, 1, "drops \"3\"");

        s.rewind(0);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "original\n");
    }

    #[test]
    fn previewing_a_file_too_big_to_copy_says_so_without_a_diff() {
        let (project, _snaps, s) = store();
        let big = project.path().join("big.bin");
        std::fs::write(&big, vec![0u8; MAX_SNAPSHOT_BYTES as usize + 1]).unwrap();
        s.begin_turn("p");
        s.before_write("write_file", &args("big.bin"));
        std::fs::write(&big, "small now").unwrap();

        let preview = s.preview_rewind(0);
        assert_eq!(preview.len(), 1);
        assert!(preview[0].too_large);
        assert_eq!(preview[0].added, 0);
        assert_eq!(preview[0].removed, 0);
    }

    #[test]
    #[cfg(unix)]
    fn a_recorded_file_is_still_named_relative_to_a_project_reached_another_way() {
        let outer = tempfile::tempdir().unwrap();
        let snaps = tempfile::tempdir().unwrap();
        let real = outer.path().join("project");
        std::fs::create_dir(&real).unwrap();
        let through_link = outer.path().join("link");
        std::os::unix::fs::symlink(&real, &through_link).unwrap();
        std::fs::write(real.join("a.txt"), "original").unwrap();

        let store = SnapshotStore::open(snaps.path(), &through_link);
        store.begin_turn("p");
        store.before_write("write_file", &args("a.txt"));
        std::fs::write(real.join("a.txt"), "changed").unwrap();
        let restored = store.rewind(0).restored;
        assert_eq!(restored.len(), 1);
        assert_eq!(store.display_path(&restored[0]), "a.txt", "{:?}", restored[0]);
        assert_eq!(std::fs::read_to_string(real.join("a.txt")).unwrap(), "original");
    }

    #[test]
    fn a_windows_extended_length_prefix_is_not_shown_to_the_user() {
        let (project, _snaps, store) = store();
        let outside = PathBuf::from(r"\\?\C:\Users\someone\notes.txt");
        assert_eq!(store.display_path(&outside), r"C:\Users\someone\notes.txt");
        let _ = project;
    }

    #[test]
    #[cfg(unix)]
    fn rewind_restores_the_file_addressed_through_a_symlink_parent() {
        let (project, _snaps, store) = store();
        std::fs::create_dir_all(project.path().join("nested/child")).unwrap();
        std::os::unix::fs::symlink(project.path().join("nested/child"), project.path().join("link")).unwrap();
        let actual = project.path().join("nested/file.txt");
        let other = project.path().join("file.txt");
        std::fs::write(&actual, "original").unwrap();
        std::fs::write(&other, "unrelated").unwrap();
        store.begin_turn("edit linked file");
        store.before_write("write_file", &args("link/../file.txt"));
        std::fs::write(project.path().join("link/../file.txt"), "changed").unwrap();
        let report = store.rewind(0);
        assert!(report.failed.is_empty());
        assert_eq!(std::fs::read_to_string(actual).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(other).unwrap(), "unrelated");
    }

    #[test]
    fn a_partial_rewind_retains_only_failed_files_for_retry_after_restart() {
        let (project, snaps, store) = store();
        let a = project.path().join("a.txt");
        let b = project.path().join("b.txt");
        std::fs::write(&a, "old a").unwrap();
        std::fs::write(&b, "old b").unwrap();
        store.begin_turn("change both");
        store.before_write("write_file", &args("a.txt"));
        store.before_write("write_file", &args("b.txt"));
        std::fs::write(&a, "new a").unwrap();
        std::fs::remove_file(&b).unwrap();
        std::fs::create_dir(&b).unwrap();
        let report = store.rewind(0);
        assert_eq!(report.restored, vec![canon(&a)]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(store.rewindable(&["change both"])[0].files, 1);
        std::fs::write(&a, "user change after partial rewind").unwrap();
        std::fs::remove_dir(&b).unwrap();
        let reopened = SnapshotStore::open(snaps.path(), project.path());
        let report = reopened.rewind(0);
        assert!(report.failed.is_empty());
        assert_eq!(report.restored, vec![canon(&b)]);
        assert_eq!(std::fs::read_to_string(a).unwrap(), "user change after partial rewind");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "old b");
        assert!(reopened.rewindable(&["change both"]).is_empty());
    }

    #[test]
    fn the_fingerprint_does_not_change_between_builds() {
        assert_eq!(prompt_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(prompt_hash("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
