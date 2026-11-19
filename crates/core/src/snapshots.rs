//! Per-turn file snapshots, so a turn can be taken back: files it wrote are
//! restored and the conversation returns to before it. Only the file tools
//! are recorded; changes made through the shell are not, and rewind says so.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::memory_store::{self, Kind, Mutation, Scope, Store};

/// Larger files are not copied; rewind reports them.
const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;

const WRITING_TOOLS: &[&str] = &["write_file", "edit_file", "patch_file"];
/// Project memory is files in the project like any other; global memory in
/// `~/.flashagent` is the user's own and is left as it is.
const MEMORY_TOOLS: &[&str] = &["memory_create", "memory_update", "memory_remove"];

/// State before the turn first touched the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Before {
    /// Rewind removes it.
    Missing,
    /// Blob file name under the store's directory.
    Blob(String),
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileBefore {
    path: PathBuf,
    before: Before,
    /// Old manifests have no mode: keep the current file's, or use a private
    /// default when it has disappeared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TurnSnapshot {
    seq: u64,
    /// Hash of the user message that started the turn: it finds the turn again
    /// after compaction or steering has changed the history.
    prompt_hash: u64,
    files: Vec<FileBefore>,
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
        let dir = resolved_path(&dir.into());
        let manifest = read_checked(&dir.join("manifest.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
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
        let _ = self.save(&st.manifest);
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
            self.before_memory(tool, args_json, |scope| Store::for_scope(scope, &self.cwd));
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

    fn before_memory(&self, tool: &str, args_json: &str, stores: impl Fn(Scope) -> Option<Store>) {
        let Some(args) = flashagent_llm::effective_args(args_json, tool) else { return };
        let Some(title) = args.get("title").and_then(|t| t.as_str()) else { return };
        let scope = args.get("scope").and_then(|s| s.as_str());
        let kind = args.get("type").and_then(|k| k.as_str()).map(Kind::parse);
        let mutation = match tool {
            "memory_create" => Mutation::Create,
            "memory_update" => Mutation::Update,
            "memory_remove" => Mutation::Remove,
            _ => return,
        };
        memory_store::transaction(|_| {
            let Some(target) = memory_store::resolve_in(scope, kind, title, mutation, stores) else { return };
            // Global memory is personal; rewind only owns the project's files.
            // A rejected create never arms an unrelated same-name file either.
            if target.scope != Scope::Project || mutation == Mutation::Create && target.existing.is_some() {
                return;
            }
            self.keep_before(self.resolve(&target.store.index_path().to_string_lossy()));
            self.keep_before(self.resolve(&target.path().to_string_lossy()));
        });
    }

    fn keep_before(&self, path: PathBuf) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(current) = st.current else {
            return;
        };
        if st.manifest.turns[current].files.iter().any(|f| f.path == path) {
            return;
        }
        let mut mode = None;
        let before = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Before::Missing,
            // Not recorded: marking it missing would delete it on rewind.
            Err(_) => return,
            Ok(meta) if !meta.is_file() || meta.is_symlink() => return,
            Ok(meta) if meta.len() > MAX_SNAPSHOT_BYTES => Before::TooLarge,
            Ok(meta) => {
                mode = Some(file_mode(&meta));
                let Ok(bytes) = read_checked(&path) else {
                    return;
                };
                let name = format!("{}.bin", st.manifest.next_blob);
                let blobs = self.dir.join("blobs");
                if private_dir(&blobs).is_err() || restore_checked(&blobs.join(&name), &bytes, Some(private_mode())).is_err() {
                    return;
                }
                st.manifest.next_blob += 1;
                Before::Blob(name)
            }
        };
        st.manifest.turns[current].files.push(FileBefore { path, before, mode });
        let _ = self.save(&st.manifest);
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
        memory_store::transaction(|_| self.rewind_locked(turn))
    }

    fn rewind_locked(&self, turn: usize) -> RewindReport {
        let mut report = RewindReport::default();
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if turn >= st.manifest.turns.len() {
            return report;
        }
        let taken_back = &st.manifest.turns[turn..];
        let mut seen = HashSet::new();
        // Oldest first, so the first record of a path is the original state.
        for file in taken_back.iter().flat_map(|t| &t.files) {
            if !seen.insert(file.path.clone()) {
                continue;
            }
            match &file.before {
                Before::Missing => match remove_created(&file.path, &self.dir) {
                    Ok(true) => report.removed.push(file.path.clone()),
                    Ok(false) => {}
                    Err(e) => report.failed.push((file.path.clone(), e.to_string())),
                },
                Before::Blob(name) => match read_checked(&self.dir.join("blobs").join(name)) {
                    Ok(bytes) => match restore_checked(&file.path, &bytes, file.mode) {
                        Ok(()) => report.restored.push(file.path.clone()),
                        Err(e) => report.failed.push((file.path.clone(), e.to_string())),
                    },
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
        let mut doomed = Vec::new();
        for file in taken_back.iter().flat_map(|t| &t.files) {
            if failed.contains(&file.path) && retained.insert(file.path.clone()) {
                pending.push(file.clone());
            } else if let Before::Blob(name) = &file.before {
                doomed.push(name.clone());
            }
        }
        let mut updated = Manifest { turns: st.manifest.turns[..turn].to_vec(), next_blob: st.manifest.next_blob };
        if !pending.is_empty() {
            let mut retry = taken_back[0].clone();
            retry.files = pending;
            updated.turns.push(retry);
        }
        st.current = None;
        // A restore is synced before its record is forgotten, and the new
        // manifest before its blobs. Failure keeps the in-memory record too.
        match self.save(&updated) {
            Ok(()) => {
                st.manifest = updated;
                for name in doomed {
                    let _ = remove_checked(&self.dir.join("blobs").join(name));
                }
                let _ = sync_dir(&self.dir.join("blobs"));
                #[cfg(windows)]
                if let Ok(entries) = std::fs::read_dir(self.dir.join("removed")) {
                    for entry in entries.flatten() { let _ = remove_checked(&entry.path()); }
                }
            }
            Err(e) => report.failed.push((self.dir.join("manifest.json"), format!("could not save rewind for retry: {e}"))),
        }
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
                    let current = read_checked(&file.path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
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
                    let current = read_checked(&file.path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                    let before = read_checked(&self.dir.join("blobs").join(name))
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
        resolved_path(&crate::paths::resolve_path(&self.cwd, raw))
    }

    fn save(&self, manifest: &Manifest) -> std::io::Result<()> {
        private_dir(&self.dir)?;
        let text = serde_json::to_string(manifest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = self.dir.join("manifest.json");
        let tmp = format!("manifest.json.{}.tmp", std::process::id());
        SafeParent::open(&path, false)?.replace(text.as_bytes(), Some(private_mode()), &tmp)
    }
}

fn resolved_path(path: &Path) -> PathBuf {
    let joined = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    // Resolve the owner's existing links before `..`, retaining missing tails.
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            other => {
                out.push(other.as_os_str());
                if let Ok(resolved) = out.canonicalize() { out = resolved; }
            }
        }
    }
    out
}

fn private_mode() -> u32 {
    if cfg!(unix) { 0o600 } else { 0 }
}

fn file_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    { u32::from(meta.permissions().readonly()) }
}

#[cfg(unix)]
fn open_directory(path: &Path, create: bool) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let mut dir = std::fs::File::open("/")?;
    for part in path.components() {
        let name = match part {
            Component::RootDir => continue,
            Component::Normal(name) => std::ffi::CString::new(name.as_bytes())?,
            _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "snapshot path is not absolute and normalized")),
        };
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // Every component is opened relative to the directory already held;
        // replacing a parent with a symlink cannot redirect the next open. The
        // paths come from `resolved_path`, which is where the owner's own
        // links (`/var` on macOS, a linked home) are resolved once, so this
        // refusing a symlink here refuses a swap, not how the machine is
        // spelled.
        let mut fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && create && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            if unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::AlreadyExists { return Err(e); }
            }
            fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 { return Err(std::io::Error::last_os_error()); }
            let next = unsafe { std::fs::File::from_raw_fd(fd) };
            dir.sync_all()?;
            dir = next;
            continue;
        }
        if fd < 0 { return Err(std::io::Error::last_os_error()); }
        dir = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(dir)
}

#[cfg(unix)]
struct SafeParent {
    dir: std::fs::File,
    path: PathBuf,
    name: std::ffi::CString,
}

#[cfg(unix)]
impl SafeParent {
    fn open(path: &Path, create: bool) -> std::io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::path::absolute(path)?;
        let parent = path.parent().ok_or_else(|| std::io::Error::other("snapshot file has no parent"))?;
        let name = path.file_name().ok_or_else(|| std::io::Error::other("snapshot file has no name"))?;
        Ok(Self { dir: open_directory(parent, create)?, path: parent.to_path_buf(), name: std::ffi::CString::new(name.as_bytes())? })
    }

    fn stat(&self, name: &std::ffi::CStr) -> std::io::Result<Option<libc::stat>> {
        use std::os::fd::AsRawFd;
        let mut meta = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstatat(self.dir.as_raw_fd(), name.as_ptr(), meta.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            let e = std::io::Error::last_os_error();
            return if e.kind() == std::io::ErrorKind::NotFound { Ok(None) } else { Err(e) };
        }
        let meta = unsafe { meta.assume_init() };
        if meta.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(std::io::Error::other("snapshot path is now a symlink or not a regular file"));
        }
        Ok(Some(meta))
    }

    fn verify(&self) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let now = open_directory(&self.path, false)?.metadata()?;
        let held = self.dir.metadata()?;
        if now.dev() != held.dev() || now.ino() != held.ino() {
            return Err(std::io::Error::other("snapshot parent changed during rewind"));
        }
        Ok(())
    }

    fn read(&self) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        use std::os::fd::{AsRawFd, FromRawFd};
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        let fd = unsafe { libc::openat(self.dir.as_raw_fd(), self.name.as_ptr(), flags) };
        if fd < 0 { return Err(std::io::Error::last_os_error()); }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        if !file.metadata()?.is_file() { return Err(std::io::Error::other("snapshot path is not a regular file")); }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        self.verify()?;
        Ok(bytes)
    }

    fn restore(&self, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
        self.replace(bytes, mode, &temporary_name(&self.name.to_string_lossy()))
    }

    #[allow(clippy::unnecessary_cast)] // libc field widths differ across Unix targets.
    fn replace(&self, bytes: &[u8], mode: Option<u32>, temp: &str) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let current = self.stat(&self.name)?;
        // Atomic replacement must not bypass a read-only file's permissions.
        if current.is_some() {
            let fd = unsafe { libc::openat(self.dir.as_raw_fd(), self.name.as_ptr(), libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK) };
            if fd < 0 { return Err(std::io::Error::last_os_error()); }
            let probe = unsafe { std::fs::File::from_raw_fd(fd) };
            if !probe.metadata()?.is_file() { return Err(std::io::Error::other("snapshot path is not a regular file")); }
        }
        let mode = mode.or_else(|| current.map(|m| m.st_mode as u32 & 0o7777)).unwrap_or(0o600);
        let temp = std::ffi::CString::new(temp)?;
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = unsafe { libc::openat(self.dir.as_raw_fd(), temp.as_ptr(), flags, 0o600) };
        if fd < 0 { return Err(std::io::Error::last_os_error()); }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(bytes)?;
            file.set_permissions(std::fs::Permissions::from_mode(mode & 0o7777))?;
            file.sync_all()?;
            self.verify()?;
            self.stat(&self.name)?;
            let temp_meta = self.stat(&temp)?.ok_or_else(|| std::io::Error::other("rewind temporary file disappeared"))?;
            let held = file.metadata()?;
            if temp_meta.st_dev as u64 != held.dev() || temp_meta.st_ino as u64 != held.ino() {
                return Err(std::io::Error::other("rewind temporary file changed"));
            }
            if unsafe { libc::renameat(self.dir.as_raw_fd(), temp.as_ptr(), self.dir.as_raw_fd(), self.name.as_ptr()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            self.dir.sync_all()?;
            self.verify()?;
            let restored = self.stat(&self.name)?.ok_or_else(|| std::io::Error::other("restored file disappeared"))?;
            if restored.st_dev as u64 != held.dev() || restored.st_ino as u64 != held.ino() {
                return Err(std::io::Error::other("restored file changed during rewind"));
            }
            Ok(())
        })();
        if result.is_err() {
            // unlinkat removes the temporary name itself, never a link target.
            unsafe { libc::unlinkat(self.dir.as_raw_fd(), temp.as_ptr(), 0); }
        }
        result
    }

    fn remove(&self) -> std::io::Result<bool> {
        use std::os::fd::AsRawFd;
        if self.stat(&self.name)?.is_none() { return Ok(false); }
        self.verify()?;
        if unsafe { libc::unlinkat(self.dir.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
            let e = std::io::Error::last_os_error();
            return if e.kind() == std::io::ErrorKind::NotFound { Ok(false) } else { Err(e) };
        }
        self.dir.sync_all()?;
        self.verify()?;
        Ok(true)
    }
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let wide = |path: &Path| path.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>();
    let from = wide(from);
    let to = wide(to);
    // REPLACE_EXISTING | WRITE_THROUGH: don't forget the blob before the
    // replacement has reached disk, even when the target did not exist.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(not(unix))]
struct SafeParent {
    path: PathBuf,
    // Windows denies replacement of every held parent while rewind is using it.
    #[cfg(windows)]
    _parents: Vec<std::fs::File>,
}

#[cfg(not(unix))]
impl SafeParent {
    fn open(path: &Path, create: bool) -> std::io::Result<Self> {
        let path = std::path::absolute(path)?;
        let parent = path.parent().ok_or_else(|| std::io::Error::other("snapshot file has no parent"))?;
        let mut at = PathBuf::new();
        #[cfg(windows)]
        let mut parents = Vec::new();
        for part in parent.components() {
            if matches!(part, Component::CurDir | Component::ParentDir) {
                return Err(std::io::Error::other("snapshot path is not normalized"));
            }
            at.push(part.as_os_str());
            if matches!(part, Component::Prefix(_)) { continue; }
            let meta = match std::fs::symlink_metadata(&at) {
                Ok(meta) => meta,
                Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => {
                    match std::fs::create_dir(&at) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(e),
                    }
                    std::fs::symlink_metadata(&at)?
                }
                Err(e) => return Err(e),
            };
            if meta.is_symlink() || !meta.is_dir() {
                return Err(std::io::Error::other("snapshot parent is now a symlink or not a directory"));
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
                // OPEN_REPARSE_POINT | BACKUP_SEMANTICS; share reads/writes but
                // not deletion, which includes renaming or replacing a parent.
                let held = std::fs::OpenOptions::new().read(true).share_mode(3).custom_flags(0x0020_0000 | 0x0200_0000).open(&at)?;
                let meta = held.metadata()?;
                if meta.file_attributes() & 0x400 != 0 || !meta.is_dir() {
                    return Err(std::io::Error::other("snapshot parent is a reparse point"));
                }
                parents.push(held);
            }
        }
        Ok(Self { path, #[cfg(windows)] _parents: parents })
    }

    fn current(&self) -> std::io::Result<Option<std::fs::Metadata>> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.is_symlink() || !meta.is_file() => Err(std::io::Error::other("snapshot path is now a symlink or not a regular file")),
            Ok(meta) => Ok(Some(meta)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read(&self) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        self.current()?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000).share_mode(3);
        }
        let mut file = options.open(&self.path)?;
        let meta = file.metadata()?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 { return Err(std::io::Error::other("snapshot path is a reparse point")); }
        }
        if !meta.is_file() { return Err(std::io::Error::other("snapshot path is not a regular file")); }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn restore(&self, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
        self.replace(bytes, mode, &temporary_name(&self.path.file_name().unwrap().to_string_lossy()))
    }

    fn replace(&self, bytes: &[u8], mode: Option<u32>, temp: &str) -> std::io::Result<()> {
        use std::io::Write;
        let current = self.current()?;
        let mode = mode.or_else(|| current.as_ref().map(file_mode)).unwrap_or(0);
        let temp = self.path.with_file_name(temp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000 | 0x8000_0000);
        }
        let mut file = options.open(&temp)?;
        let result = (|| {
            file.write_all(bytes)?;
            let mut permissions = file.metadata()?.permissions();
            permissions.set_readonly(mode != 0);
            file.set_permissions(permissions)?;
            file.sync_all()?;
            self.current()?;
            replace_file(&temp, &self.path)?;
            sync_dir(self.path.parent().unwrap())
        })();
        if result.is_err() { let _ = std::fs::remove_file(&temp); }
        result
    }

    fn remove(&self) -> std::io::Result<bool> {
        if self.current()?.is_none() { return Ok(false); }
        match std::fs::remove_file(&self.path) {
            Ok(()) => { sync_dir(self.path.parent().unwrap())?; Ok(true) }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

fn temporary_name(name: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut cut = name.len().min(96);
    while !name.is_char_boundary(cut) { cut -= 1; }
    format!(".{}.flashagent-{}-{id}.tmp", &name[..cut], std::process::id())
}

fn read_checked(path: &Path) -> std::io::Result<Vec<u8>> {
    SafeParent::open(path, false)?.read()
}

fn restore_checked(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    SafeParent::open(path, true)?.restore(bytes, mode)
}

fn remove_checked(path: &Path) -> std::io::Result<bool> {
    match SafeParent::open(path, false) {
        Ok(parent) => parent.remove(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn remove_created(path: &Path, snapshots: &Path) -> std::io::Result<bool> {
    #[cfg(windows)]
    {
        // Windows cannot sync a directory through std. Move the created file
        // durably into the private snapshot folder; only delete it after the
        // manifest has forgotten it. A failed move leaves the retry intact.
        let parent = match SafeParent::open(path, false) {
            Ok(parent) => parent,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if parent.current()?.is_none() { return Ok(false); }
        let trash = snapshots.join("removed");
        private_dir(&trash)?;
        let destination = trash.join(temporary_name("rewound"));
        let _held = SafeParent::open(&destination, false)?;
        parent.current()?;
        replace_file(path, &destination)?;
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let _ = snapshots;
        remove_checked(path)
    }
}

/// Snapshots hold copies of project files, which can hold secrets.
#[cfg(unix)]
fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = open_directory(path, true)?;
    dir.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    dir.sync_all()
}

#[cfg(not(unix))]
fn private_dir(path: &Path) -> std::io::Result<()> {
    SafeParent::open(&path.join(".directory-check"), true).map(|_| ())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    open_directory(dir, false)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> { Ok(()) }

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
        s.before_write("memory_create", r#"{"title":"Tests use nextest","content":"x","scope":"project"}"#);
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
    fn a_global_memory_write_does_not_arm_a_rewind_of_the_project_index() {
        let (project, _snaps, s) = store();
        let global = tempfile::tempdir().unwrap();
        s.begin_turn("p");
        s.before_memory("memory_create", r#"{"title":"favourite editor","type":"preference","kind":"decision"}"#, |scope| {
            Some(Store::new(match scope { Scope::Project => project.path(), Scope::Global => global.path() }))
        });
        let index = project.path().join("MEMORY.md");
        std::fs::write(&index, "# Memory\n\nhand written\n").unwrap();
        assert_eq!(s.rewindable(&["p"])[0].files, 0, "a global preference armed project files");
        assert!(s.preview_rewind(0).is_empty());
        let report = s.rewind(0);
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(std::fs::read_to_string(index).unwrap(), "# Memory\n\nhand written\n");
    }

    #[test]
    fn an_explicit_memory_scope_never_records_the_other_same_name() {
        for (tool, scope, kept) in [
            ("memory_create", "global", 0),
            ("memory_update", "global", 0),
            ("memory_remove", "global", 0),
            ("memory_update", "project", 2),
            ("memory_remove", "project", 2),
        ] {
            let (project, _snaps, s) = store();
            let global = tempfile::tempdir().unwrap();
            let project_store = Store::new(project.path());
            let global_store = Store::new(global.path());
            for (store, text) in [(&project_store, "project note"), (&global_store, "personal note")] {
                std::fs::create_dir_all(store.dir()).unwrap();
                std::fs::write(store.dir().join("Build Notes.md"), text).unwrap();
                std::fs::write(store.dir().join("build-notes.md"), "separate slug note").unwrap();
                std::fs::write(store.index_path(), text).unwrap();
            }
            s.begin_turn("p");
            let args = serde_json::json!({"title":"Build Notes", "scope":scope}).to_string();
            s.before_memory(tool, &args, |scope| Some(match scope {
                Scope::Project => project_store.clone(), Scope::Global => global_store.clone(),
            }));
            let st = s.state.lock().unwrap();
            let files = &st.manifest.turns[0].files;
            assert_eq!(files.len(), kept, "{tool} in {scope}: {files:?}");
            assert!(files.iter().all(|f| f.path.starts_with(&s.cwd)), "a personal path was recorded: {files:?}");
            assert!(files.iter().all(|f| f.path.file_name().unwrap() != "build-notes.md"), "the unrelated slug was recorded: {files:?}");
            drop(st);
            s.rewind(0);
            assert_eq!(std::fs::read_to_string(global_store.dir().join("Build Notes.md")).unwrap(), "personal note");
        }
    }

    #[test]
    fn project_scope_creation_does_not_snapshot_a_global_same_name() {
        let (project, _snaps, s) = store();
        let global = tempfile::tempdir().unwrap();
        let global_store = Store::new(global.path());
        std::fs::create_dir_all(global_store.dir()).unwrap();
        std::fs::write(global_store.dir().join("new-fact.md"), "personal note").unwrap();
        s.begin_turn("p");
        s.before_memory("memory_create", r#"{"title":"New fact","scope":"project","type":"preference"}"#, |scope| {
            Some(Store::new(match scope { Scope::Project => project.path(), Scope::Global => global.path() }))
        });
        std::fs::create_dir_all(project.path().join("memory")).unwrap();
        std::fs::write(project.path().join("memory/new-fact.md"), "created").unwrap();
        std::fs::write(project.path().join("MEMORY.md"), "new index").unwrap();
        assert_eq!(s.rewindable(&["p"])[0].files, 2);
        let report = s.rewind(0);
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(!project.path().join("MEMORY.md").exists());
        assert!(!project.path().join("memory/new-fact.md").exists());
        assert_eq!(std::fs::read_to_string(global_store.dir().join("new-fact.md")).unwrap(), "personal note");
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
    #[cfg(unix)]
    fn a_new_file_symlink_is_rejected_and_the_snapshot_can_be_retried() {
        let (project, snaps, store) = store();
        let outside = tempfile::tempdir().unwrap();
        let file = project.path().join("a.txt");
        let personal = outside.path().join("personal.txt");
        std::fs::write(&file, "original").unwrap();
        std::fs::write(&personal, "untouched").unwrap();
        store.begin_turn("p");
        store.before_write("write_file", &args("a.txt"));
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&personal, &file).unwrap();
        drop(store);
        let store = SnapshotStore::open(snaps.path(), project.path());
        let preview = store.preview_rewind(0);
        assert_eq!(preview[0].removed, 0, "preview read through the new link: {preview:?}");
        let report = store.rewind(0);
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(std::fs::symlink_metadata(&file).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&personal).unwrap(), "untouched");
        std::fs::remove_file(&file).unwrap();
        let reopened = SnapshotStore::open(snaps.path(), project.path());
        let report = reopened.rewind(0);
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "original");
        assert_eq!(std::fs::read_dir(snaps.path().join("blobs")).unwrap().count(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn a_new_parent_symlink_cannot_redirect_restore_or_deletion() {
        for existed in [true, false] {
            let (project, snaps, store) = store();
            let outside = tempfile::tempdir().unwrap();
            let parent = project.path().join("sub");
            let file = parent.join("a.txt");
            let personal = outside.path().join("a.txt");
            std::fs::create_dir(&parent).unwrap();
            if existed { std::fs::write(&file, "original").unwrap(); }
            std::fs::write(&personal, "untouched").unwrap();
            store.begin_turn("p");
            store.before_write("write_file", &args("sub/a.txt"));
            if existed { std::fs::remove_file(&file).unwrap(); }
            std::fs::remove_dir(&parent).unwrap();
            std::os::unix::fs::symlink(outside.path(), &parent).unwrap();
            let report = store.rewind(0);
            assert_eq!(report.failed.len(), 1, "{report:?}");
            assert_eq!(std::fs::read_to_string(&personal).unwrap(), "untouched");
            assert!(std::fs::symlink_metadata(&parent).unwrap().is_symlink());
            std::fs::remove_file(&parent).unwrap();
            let reopened = SnapshotStore::open(snaps.path(), project.path());
            let report = reopened.rewind(0);
            assert!(report.failed.is_empty(), "{report:?}");
            if existed { assert_eq!(std::fs::read_to_string(file).unwrap(), "original"); }
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_created_file_replaced_by_a_symlink_is_not_removed() {
        let (project, _snaps, store) = store();
        let outside = tempfile::tempdir().unwrap();
        let personal = outside.path().join("personal.txt");
        let file = project.path().join("created.txt");
        std::fs::write(&personal, "untouched").unwrap();
        store.begin_turn("p");
        store.before_write("write_file", &args("created.txt"));
        std::os::unix::fs::symlink(&personal, &file).unwrap();
        let report = store.rewind(0);
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(std::fs::symlink_metadata(&file).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(personal).unwrap(), "untouched");
    }

    #[test]
    #[cfg(unix)]
    fn a_parent_swapped_after_open_cannot_redirect_descriptor_operations() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let parent_path = project.path().join("sub");
        std::fs::create_dir(&parent_path).unwrap();
        let file = parent_path.join("a.txt");
        std::fs::write(&file, "original").unwrap();
        std::fs::write(outside.path().join("a.txt"), "untouched").unwrap();
        // As the store does: paths are resolved before they are kept, so
        // the machine's own links are not what this refuses.
        let held = SafeParent::open(&resolved_path(&file), false).unwrap();
        std::fs::rename(&parent_path, project.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &parent_path).unwrap();
        assert!(held.restore(b"restored", Some(0o600)).is_err());
        assert!(held.remove().is_err());
        assert_eq!(std::fs::read_to_string(outside.path().join("a.txt")).unwrap(), "untouched");
        assert_eq!(std::fs::read_to_string(project.path().join("moved/a.txt")).unwrap(), "original");
    }

    #[test]
    #[cfg(unix)]
    fn atomic_restore_failure_keeps_bytes_mode_and_retry_copy() {
        use std::os::unix::fs::PermissionsExt;
        let (project, snaps, store) = store();
        let file = project.path().join("run.sh");
        std::fs::write(&file, "original").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o751)).unwrap();
        store.begin_turn("p");
        store.before_write("write_file", &args("run.sh"));
        std::fs::write(&file, "changed").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let parent = SafeParent::open(&resolved_path(&file), false).unwrap();
        let blocker = project.path().join(".restore-blocker");
        std::fs::create_dir(&blocker).unwrap();
        assert!(parent.replace(b"original", Some(0o751), ".restore-blocker").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "changed");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let reopened = SnapshotStore::open(snaps.path(), project.path());
        let report = reopened.rewind(0);
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o751);
        assert_eq!(std::fs::read_dir(snaps.path().join("blobs")).unwrap().count(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn a_deleted_executable_recovers_its_original_mode_after_restart() {
        use std::os::unix::fs::PermissionsExt;
        let (project, snaps, store) = store();
        let file = project.path().join("run.sh");
        std::fs::write(&file, "original").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o750)).unwrap();
        store.begin_turn("p");
        store.before_write("write_file", &args("run.sh"));
        std::fs::remove_file(&file).unwrap();
        let reopened = SnapshotStore::open(snaps.path(), project.path());
        assert!(reopened.rewind(0).failed.is_empty());
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o750);
    }

    #[test]
    fn an_old_manifest_without_modes_remains_rewindable() {
        let (project, snaps, store) = store();
        let file = project.path().join("a.txt");
        std::fs::write(&file, "original").unwrap();
        store.begin_turn("p");
        store.before_write("write_file", &args("a.txt"));
        let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(snaps.path().join("manifest.json")).unwrap()).unwrap();
        manifest["turns"][0]["files"][0].as_object_mut().unwrap().remove("mode");
        std::fs::write(snaps.path().join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(&file, "changed").unwrap();
        let reopened = SnapshotStore::open(snaps.path(), project.path());
        assert!(reopened.rewind(0).failed.is_empty());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "original");
    }

    #[test]
    fn a_partial_rewind_retains_only_failed_files_for_retry_after_restart() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        let (project, snaps, store) = store();
        let a = project.path().join("a.txt");
        let b = project.path().join("b.txt");
        std::fs::write(&a, "old a").unwrap();
        std::fs::write(&b, "old b").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o751)).unwrap();
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
        #[cfg(unix)]
        assert_eq!(std::fs::metadata(&b).unwrap().permissions().mode() & 0o777, 0o751, "the failed file's mode was not retained for retry");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "old b");
        assert!(reopened.rewindable(&["change both"]).is_empty());
    }

    #[test]
    fn a_save_that_cannot_finish_leaves_the_previous_manifest_whole() {
        let (project, snaps, store) = store();
        store.begin_turn("first");
        // A directory where the temporary file would go makes the next save
        // fail, as a full disk or a read-only folder would.
        let blocker = snaps.path().join(format!("manifest.json.{}.tmp", std::process::id()));
        std::fs::create_dir(&blocker).unwrap();
        store.begin_turn("second");

        let written = std::fs::read_to_string(snaps.path().join("manifest.json")).unwrap();
        serde_json::from_str::<Manifest>(&written).expect("half a manifest");
        let reopened = SnapshotStore::open(snaps.path(), project.path());
        assert_eq!(reopened.rewindable(&["first"]).len(), 1, "the last good manifest was lost");
        assert!(reopened.rewindable(&["second"]).is_empty(), "a save that failed was taken for saved");
    }

    #[test]
    fn a_rewind_whose_record_cannot_be_saved_keeps_the_copies_needed_to_retry() {
        let (project, snaps, store) = store();
        let a = project.path().join("a.txt");
        std::fs::write(&a, "original").unwrap();
        store.begin_turn("p");
        store.before_write("write_file", &args("a.txt"));
        std::fs::write(&a, "changed").unwrap();
        let blocker = snaps.path().join(format!("manifest.json.{}.tmp", std::process::id()));
        std::fs::create_dir(&blocker).unwrap();

        let report = store.rewind(0);
        assert_eq!(report.failed.len(), 1, "the manifest save failure must keep history rewindable: {report:?}");
        // The store keeps paths in the machine's own spelling (`/private/var`
        // on macOS, the long form on Windows), so the expectation is resolved
        // the same way rather than compared with the tempdir's short name.
        assert_eq!(report.failed[0].0, resolved_path(&snaps.path().join("manifest.json")));
        assert_eq!(store.rewindable(&["p"])[0].files, 1, "the in-memory retry was lost");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "original");
        // The record on disk still names the copy. Deleting the blob before the
        // record is saved would leave it pointing at nothing on the next start.
        std::fs::remove_dir(&blocker).unwrap();
        assert_eq!(store.rewindable(&["p"])[0].files, 1, "same-process retry disappeared");
        let reopened = SnapshotStore::open(snaps.path(), project.path());
        let report = reopened.rewind(0);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(reopened.rewindable(&["p"]).is_empty());
        assert_eq!(std::fs::read_dir(snaps.path().join("blobs")).unwrap().count(), 0, "a blob nobody names was left behind");
    }

    #[test]
    #[cfg(unix)]
    fn the_snapshot_copies_are_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let project = tempfile::tempdir().unwrap();
        let snaps = tempfile::tempdir().unwrap();
        let dir = snaps.path().join("session");
        let store = SnapshotStore::open(&dir, project.path());
        std::fs::write(project.path().join("a.txt"), "secret").unwrap();
        store.begin_turn("p");
        store.before_write("write_file", &args("a.txt"));

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("blobs")), 0o700);
        assert_eq!(mode(&dir.join("manifest.json")), 0o600);
        assert_eq!(mode(&dir.join("blobs").join("0.bin")), 0o600);
    }

    #[test]
    fn the_fingerprint_does_not_change_between_builds() {
        assert_eq!(prompt_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(prompt_hash("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
