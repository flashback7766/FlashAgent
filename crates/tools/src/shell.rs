//! Shell execution: foreground with timeout, background with task ids and a
//! live output buffer. Each command gets its own process group, so a timeout
//! or cancel kills the whole pipeline, not just `sh` while its children keep
//! our pipes open.
//!
//! On Windows commands go to Git Bash when it is installed: models write Unix
//! commands, and cmd.exe runs none of them. Without it, cmd.exe runs them and
//! the system prompt says so.

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

use crate::ToolError;

/// Per background task.
const BUFFER_CAP: usize = 200_000;

/// The tail, with a line saying the start was dropped: 3,000 lines that begin
/// at line 1401 must not look like the whole output.
fn tail_noted(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    let tail = tail_str(s, max);
    if tail.len() == s.len() {
        std::borrow::Cow::Borrowed(tail)
    } else {
        std::borrow::Cow::Owned(format!("[earlier output omitted; the last {} bytes follow]\n{tail}", tail.len()))
    }
}

fn tail_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

async fn pump<R>(stream: Option<R>, buffer: Arc<Mutex<String>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut stream = match stream {
        Some(s) => s,
        None => return,
    };
    let mut chunk = [0u8; 8192];
    // A character split across two reads is held until its last bytes arrive;
    // decoding each read alone produced two U+FFFD.
    let mut carry: Vec<u8> = Vec::new();
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => {
                if !carry.is_empty() {
                    buffer.lock().push_str(&String::from_utf8_lossy(&carry));
                }
                break;
            }
            Ok(n) => {
                carry.extend_from_slice(&chunk[..n]);
                let text = decode(&mut carry);
                let mut buf = buffer.lock();
                buf.push_str(&text);
                if buf.len() > BUFFER_CAP {
                    let mut cut = buf.len() - BUFFER_CAP / 2;
                    while !buf.is_char_boundary(cut) {
                        cut += 1;
                    }
                    buf.drain(..cut);
                }
            }
        }
    }
}

/// UTF-8, or on Windows the legacy code page for a read that is not UTF-8:
/// cmd.exe and most Windows tools write that to a pipe, so an error from cmd
/// on a Russian system arrived as mojibake. Decided per read, so one native
/// tool's line does not turn the rest of a Git Bash session into mojibake.
fn decode(bytes: &mut Vec<u8>) -> String {
    if cfg!(windows) && std::str::from_utf8(bytes).is_err_and(|e| e.error_len().is_some()) {
        return decode_legacy(&std::mem::take(bytes));
    }
    decode_complete(bytes)
}

#[cfg(not(windows))]
fn decode_legacy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The console's OEM code page, the one cmd.exe writes in.
#[cfg(windows)]
fn decode_legacy(bytes: &[u8]) -> String {
    #[link(name = "kernel32")]
    extern "system" {
        fn MultiByteToWideChar(code_page: u32, flags: u32, src: *const u8, src_len: i32, dst: *mut u16, dst_len: i32)
            -> i32;
    }
    const CP_OEMCP: u32 = 1;
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    if len == 0 {
        return String::new();
    }
    // SAFETY: the first call only measures; the second writes at most `size`
    // units into a buffer of exactly that many.
    let size = unsafe { MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if size <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; size as usize];
    let written = unsafe { MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), size) };
    String::from_utf16_lossy(&wide[..written.max(0) as usize])
}

/// An incomplete trailing character stays in `bytes`. Invalid sequences decode
/// as U+FFFD.
fn decode_complete(bytes: &mut Vec<u8>) -> String {
    let mut out = String::new();
    let mut rest: &[u8] = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.push_str(text);
                rest = &[];
                break;
            }
            Err(e) => {
                let (valid, after) = rest.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                match e.error_len() {
                    Some(bad) => {
                        out.push('\u{FFFD}');
                        rest = &after[bad..];
                    }
                    None => {
                        rest = after;
                        break;
                    }
                }
            }
        }
    }
    let consumed = bytes.len() - rest.len();
    bytes.drain(..consumed);
    out
}

/// Git for Windows' `bin\bash.exe`, which puts its Unix tools on PATH. Not
/// `System32\bash.exe`: that is WSL, a different machine with its own files.
#[cfg(windows)]
fn git_bash() -> Option<&'static std::path::Path> {
    static FOUND: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    FOUND.get_or_init(find_git_bash).as_deref()
}

#[cfg(windows)]
fn find_git_bash() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    // git.exe sits in <root>\cmd, <root>\bin or <root>\mingw64\bin.
    // Absolute entries only: a relative one resolves inside whatever project
    // is open, which could bring its own "git.exe" and "bin\bash.exe".
    let beside_git = path_dirs
        .into_iter()
        .filter(|dir| dir.is_absolute() && dir.join("git.exe").is_file())
        .flat_map(|dir| dir.ancestors().skip(1).take(2).map(PathBuf::from).collect::<Vec<_>>());
    let usual = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|p| PathBuf::from(p).join("Git"))
        .chain(std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join(r"Programs\Git")))
        .chain(std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(r"scoop\apps\git\current")));
    beside_git.chain(usual).map(|root| root.join(r"bin\bash.exe")).find(|bash| bash.is_file())
}

/// A bare program name as a shell would find it. Windows process creation
/// only tries `.exe`, so `npx` failed while npm installs `npx.cmd`; Rust runs a
/// `.cmd` found this way through cmd.exe with its own quoting. Elsewhere, and
/// for a name with an extension or a path, the name is used as given.
pub(crate) fn find_program(command: &str) -> std::path::PathBuf {
    let given = std::path::PathBuf::from(command);
    if !cfg!(windows) || given.extension().is_some() || given.components().count() > 1 {
        return given;
    }
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    // Absolute entries only, as for Git Bash: a relative one is the open project.
    dirs.iter()
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| exts.split(';').filter(|e| !e.is_empty()).map(move |ext| dir.join(format!("{command}{}", ext.to_ascii_lowercase()))))
        .find(|candidate| candidate.is_file())
        .unwrap_or(given)
}

/// The platform line of the system prompt: the shell decides which commands
/// the model can write.
pub fn platform() -> String {
    announce_dialect();
    #[cfg(windows)]
    {
        if git_bash().is_some() {
            "windows; run_shell uses Git Bash: POSIX commands, forward slashes in paths".to_string()
        } else {
            "windows; run_shell uses cmd.exe: Windows commands (dir, type, findstr), not Unix ones".to_string()
        }
    }
    #[cfg(not(windows))]
    std::env::consts::OS.to_string()
}

/// The permission rules split a command the way this shell will.
fn announce_dialect() {
    #[cfg(windows)]
    let dialect = if git_bash().is_some() { flashagent_core::ShellDialect::Posix } else { flashagent_core::ShellDialect::Cmd };
    #[cfg(not(windows))]
    let dialect = flashagent_core::ShellDialect::Posix;
    flashagent_core::set_shell_dialect(dialect);
}

/// cmd.exe does not read the quoting `arg` would add; /S keeps the command's
/// own quotes as written.
#[cfg(windows)]
fn cmd_exe(cmd: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("cmd");
    c.raw_arg(format!("/D /S /C \"{cmd}\""));
    c
}

fn shell_command(cmd: &str) -> tokio::process::Command {
    announce_dialect();
    #[cfg(windows)]
    let mut c = match git_bash() {
        Some(bash) => {
            // Git Bash reads its command line by its own rules: inside quotes
            // `\\` becomes `\`, so the MSVC quoting `arg` does changed commands.
            // Quoted always, with `\` and `"` escaped, it arrives as written.
            let mut c = tokio::process::Command::new(bash);
            c.arg("-c");
            c.raw_arg(format!("\"{}\"", cmd.replace('\\', r"\\").replace('"', "\\\"")));
            c
        }
        None => cmd_exe(cmd),
    };
    #[cfg(not(windows))]
    let mut c = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    c.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    #[cfg(unix)]
    c.process_group(0);
    c
}

/// A process group, or on Windows the process and its children.
fn kill_pid_tree(pid: u32) {
    #[cfg(unix)]
    // SAFETY: plain syscall on a pid we spawned; a stale pid only yields ESRCH.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
    // Before the shell dies: taskkill finds the children through their parent.
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn kill_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        kill_pid_tree(pid);
    }
    let _ = child.start_kill();
}

/// Kills the tree if the call is dropped before the command finished (the
/// user cancelled the turn).
struct TreeGuard(Option<Child>);

impl Drop for TreeGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            kill_tree(child);
        }
    }
}

/// Bounded: a backgrounded grandchild (`server &`) inherits the pipes and can
/// hold them open forever.
async fn drain_pumps(p1: tokio::task::JoinHandle<()>, p2: tokio::task::JoinHandle<()>) -> bool {
    tokio::time::timeout(Duration::from_millis(500), async {
        let _ = tokio::join!(p1, p2);
    })
    .await
    .is_ok()
}

/// A non-zero exit is an error whose text still carries the output.
pub async fn run_foreground(cmd: &str, timeout: Duration) -> Result<String, ToolError> {
    run(cmd, timeout, None).await
}

/// Resolves when the user asks to move the command to the background; never
/// for a command that cannot be moved.
async fn detach_requested(slot: Option<&mut ForegroundSlot>) {
    if let Some(slot) = slot {
        if (&mut slot.rx).await.is_ok() {
            return;
        }
    }
    std::future::pending::<()>().await
}

async fn run(cmd: &str, timeout: Duration, registry: Option<&ShellRegistry>) -> Result<String, ToolError> {
    let started = Instant::now();
    let mut child =
        shell_command(cmd).spawn().map_err(|e| ToolError::Other(format!("spawn: {e}")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let buffer = Arc::new(Mutex::new(String::new()));
    let p1 = tokio::spawn(pump(stdout, buffer.clone()));
    let p2 = tokio::spawn(pump(stderr, buffer.clone()));
    let mut guard = TreeGuard(Some(child));
    let mut slot = registry.map(ShellRegistry::foreground_slot);

    // Biased to the exit: a command that ended as the key was pressed is
    // reported as ended. Dropping the wait is safe; the child keeps its status.
    let waited = match guard.0.as_mut() {
        Some(child) => tokio::select! {
            biased;
            res = tokio::time::timeout(timeout, child.wait()) => Some(res),
            _ = detach_requested(slot.as_mut()) => None,
        },
        None => return Err(ToolError::Other("spawn: lost child handle".into())),
    };
    drop(slot);
    let waited = match (waited, registry) {
        (Some(waited), _) => waited,
        (None, Some(registry)) => {
            let Some(child) = guard.0.take() else {
                return Err(ToolError::Other("spawn: lost child handle".into()));
            };
            let so_far = tail_noted(&buffer.lock(), 4000).into_owned();
            let id = registry.adopt(child, cmd, buffer, [p1, p2], started, true);
            return Ok(format!(
                "The user moved this command to the background; it keeps running as background task {id}. \
                 A notice will arrive when it exits, so there is no need to poll it; {{\"task_id\":{id},\"kill\":true}} stops it.\n\
                 output so far:\n{}",
                if so_far.trim().is_empty() { "(no output yet)" } else { &so_far }
            ));
        }
        (None, None) => return Err(ToolError::Other("spawn: lost child handle".into())),
    };
    let status = match waited {
        Ok(res) => {
            // Finished on its own: whatever it backgrounded may keep running.
            guard.0 = None;
            res.map_err(|e| ToolError::Other(format!("wait: {e}")))?
        }
        Err(_) => {
            if let Some(mut child) = guard.0.take() {
                kill_tree(&mut child);
                let _ = child.wait().await;
            }
            drain_pumps(p1, p2).await;
            let out = buffer.lock();
            let out = tail_noted(&out, 4000);
            return Err(ToolError::Other(format!(
                "timeout after {}s, process killed. partial output:\n{}",
                timeout.as_secs(),
                if out.trim().is_empty() { "(no output)" } else { &out }
            )));
        }
    };

    let complete = drain_pumps(p1, p2).await;
    let out = buffer.lock();
    let out = tail_noted(&out, 16_000);
    let out = if out.trim().is_empty() { "(no output)" } else { &out };
    let note = if complete { "" } else { "\n[a background process still holds the output pipe; later output is not captured]" };
    if status.success() {
        Ok(format!("exit code: 0\noutput:\n{out}{note}"))
    } else {
        Err(ToolError::Other(format!(
            "exit code: {}\noutput:\n{out}{note}",
            status.code().unwrap_or(-1)
        )))
    }
}

/// How a background task ended, or that it has not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskState {
    Running,
    /// `None`: ended by a signal.
    Exited(Option<i32>),
    /// Stopped by the model, the user, or FlashAgent quitting.
    Killed,
}

impl TaskState {
    pub fn describe(&self) -> String {
        match self {
            TaskState::Running => "running".into(),
            TaskState::Exited(Some(code)) => format!("exited with code {code}"),
            TaskState::Exited(None) => "was ended by a signal".into(),
            TaskState::Killed => "was stopped".into(),
        }
    }
}

/// `4s`, `2m 05s`, `1h 03m`.
pub fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// A background task, for the task list.
#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: u32,
    pub command: String,
    pub state: TaskState,
    /// Running so far, or how long it ran.
    pub elapsed: Duration,
    pub last_line: String,
    /// Moved to the background by the user, not started there by the model.
    pub detached: bool,
}

/// Sent once for each background task, when it ends.
#[derive(Clone, Debug)]
pub struct TaskNotice {
    pub id: u32,
    pub command: String,
    pub state: TaskState,
    pub elapsed: Duration,
    /// The end of its output, a few lines.
    pub tail: String,
}

const NOTICE_LINES: usize = 20;
const NOTICE_BYTES: usize = 1500;

impl TaskNotice {
    /// Stopped on purpose: nobody needs waking about it.
    pub fn killed(&self) -> bool {
        self.state == TaskState::Killed
    }

    /// What the model reads: data from a tool, worded as such.
    pub fn message(&self) -> String {
        use flashagent_core::{TASK_NOTICE_NOTE, TASK_NOTICE_OPENING};
        let command: String = self.command.chars().take(300).collect();
        let output = if self.tail.trim().is_empty() { "(no output)".to_string() } else { format!("last output:\n{}", self.tail) };
        format!(
            "{TASK_NOTICE_OPENING}{} {} after {}. {TASK_NOTICE_NOTE}\ncommand: {command}\n{output}",
            self.id,
            self.state.describe(),
            format_elapsed(self.elapsed)
        )
    }
}

/// The last `lines` lines, at most `bytes` long.
fn last_lines(text: &str, lines: usize, bytes: usize) -> String {
    let tail = tail_str(text.trim_end(), bytes);
    let kept: Vec<&str> = tail.lines().collect();
    kept[kept.len().saturating_sub(lines)..].join("\n")
}

/// What a finished task keeps of its output: enough to read its end.
const FINISHED_KEEP: usize = 16_000;

struct ShellTask {
    command: String,
    started: Instant,
    ended: Option<Instant>,
    state: TaskState,
    buffer: Arc<Mutex<String>>,
    /// While it runs, to stop it without its handle (quitting).
    pid: Option<u32>,
    stop: Option<oneshot::Sender<()>>,
    /// An exit racing the kill still counts as stopped, with no wake-up.
    kill_requested: bool,
    detached: bool,
}

#[derive(Default)]
struct Shared {
    next_id: AtomicU32,
    tasks: Mutex<BTreeMap<u32, ShellTask>>,
    notices: Mutex<Option<mpsc::UnboundedSender<TaskNotice>>>,
    next_foreground: AtomicU64,
    /// Foreground commands that can be moved to the background.
    foreground: Mutex<HashMap<u64, oneshot::Sender<()>>>,
}

/// A foreground command's claim on the detach key, given up when it ends.
struct ForegroundSlot {
    key: u64,
    rx: oneshot::Receiver<()>,
    shared: Arc<Shared>,
}

impl Drop for ForegroundSlot {
    fn drop(&mut self) {
        self.shared.foreground.lock().remove(&self.key);
    }
}

#[derive(Default)]
pub struct ShellRegistry {
    shared: Arc<Shared>,
}

/// A dev server started in the background must not outlive FlashAgent and
/// keep holding its port.
impl Drop for ShellRegistry {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// Owns the child until it exits, then records how and sends the one notice.
async fn watch(shared: Arc<Shared>, id: u32, child: Child, stop: oneshot::Receiver<()>, pumps: [tokio::task::JoinHandle<()>; 2]) {
    // Dropped with the runtime, it still takes the process tree down.
    let mut guard = TreeGuard(Some(child));
    let code = match guard.0.as_mut() {
        Some(child) => tokio::select! {
            biased;
            status = child.wait() => status.ok().and_then(|s| s.code()),
            Ok(()) = stop => {
                kill_tree(child);
                child.wait().await.ok().and_then(|s| s.code())
            }
        },
        None => return,
    };
    guard.0 = None;
    let [p1, p2] = pumps;
    drain_pumps(p1, p2).await;
    let notice = {
        let mut tasks = shared.tasks.lock();
        let Some(task) = tasks.get_mut(&id) else { return };
        task.state = if task.kill_requested { TaskState::Killed } else { TaskState::Exited(code) };
        let ended = Instant::now();
        task.ended = Some(ended);
        task.pid = None;
        task.stop = None;
        let mut buf = task.buffer.lock();
        if buf.len() > FINISHED_KEEP {
            let cut = buf.len() - tail_str(&buf, FINISHED_KEEP).len();
            buf.drain(..cut);
        }
        TaskNotice {
            id,
            command: task.command.clone(),
            state: task.state.clone(),
            elapsed: ended.duration_since(task.started),
            tail: last_lines(&buf, NOTICE_LINES, NOTICE_BYTES),
        }
    };
    if let Some(tx) = shared.notices.lock().as_ref() {
        let _ = tx.send(notice);
    }
}

impl ShellRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Each background task's end, once. A second call takes the notices
    /// from the first.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<TaskNotice> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.shared.notices.lock() = Some(tx);
        rx
    }

    fn foreground_slot(&self) -> ForegroundSlot {
        let key = self.shared.next_foreground.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.foreground.lock().insert(key, tx);
        ForegroundSlot { key, rx, shared: self.shared.clone() }
    }

    /// Runs `cmd` until it exits or times out, or until
    /// [`Self::detach_foreground`] makes it a background task.
    pub async fn run_foreground(&self, cmd: &str, timeout: Duration) -> Result<String, ToolError> {
        run(cmd, timeout, Some(self)).await
    }

    /// While true, [`Self::detach_foreground`] has something to move.
    pub fn foreground_running(&self) -> bool {
        !self.shared.foreground.lock().is_empty()
    }

    /// Moves every running foreground command to the background. False when
    /// none was running.
    pub fn detach_foreground(&self) -> bool {
        let waiting: Vec<oneshot::Sender<()>> = self.shared.foreground.lock().drain().map(|(_, tx)| tx).collect();
        let mut any = false;
        for tx in waiting {
            any |= tx.send(()).is_ok();
        }
        any
    }

    fn adopt(
        &self,
        child: Child,
        cmd: &str,
        buffer: Arc<Mutex<String>>,
        pumps: [tokio::task::JoinHandle<()>; 2],
        started: Instant,
        detached: bool,
    ) -> u32 {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = ShellTask {
            command: cmd.to_string(),
            started,
            ended: None,
            state: TaskState::Running,
            buffer,
            pid: child.id(),
            stop: Some(stop_tx),
            kill_requested: false,
            detached,
        };
        self.shared.tasks.lock().insert(id, task);
        tokio::spawn(watch(self.shared.clone(), id, child, stop_rx, pumps));
        id
    }

    pub fn spawn_background(&self, cmd: &str) -> Result<String, ToolError> {
        let mut child =
            shell_command(cmd).spawn().map_err(|e| ToolError::Other(format!("spawn: {e}")))?;
        let buffer = Arc::new(Mutex::new(String::new()));
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let p1 = tokio::spawn(pump(stdout, buffer.clone()));
        let p2 = tokio::spawn(pump(stderr, buffer.clone()));
        let id = self.adopt(child, cmd, buffer, [p1, p2], Instant::now(), false);
        Ok(format!(
            "started background task {id}. A notice arrives when it exits; {{\"task_id\":{id}}} shows its output so far, adding \"kill\":true stops it."
        ))
    }

    pub fn status(&self, id: u32) -> Result<String, ToolError> {
        let tasks = self.shared.tasks.lock();
        let task = tasks.get(&id).ok_or_else(|| ToolError::Other(format!("no such background task: {id}")))?;
        let state = match task.state {
            TaskState::Running => format!("running for {}", format_elapsed(task.started.elapsed())),
            ref ended => ended.describe(),
        };
        let guard = task.buffer.lock();
        let output = tail_noted(&guard, 4000);
        Ok(format!(
            "task {id}: {state}\noutput:\n{}",
            if output.trim().is_empty() { "(no output yet)" } else { &output }
        ))
    }

    /// Waits a moment for the process to be gone, so what is reported is final.
    pub async fn kill(&self, id: u32) -> Result<String, ToolError> {
        {
            let mut tasks = self.shared.tasks.lock();
            let task = tasks.get_mut(&id).ok_or_else(|| ToolError::Other(format!("no such background task: {id}")))?;
            if task.state != TaskState::Running {
                return Ok(format!("task {id} had already ended: it {}", task.state.describe()));
            }
            task.kill_requested = true;
            if let Some(stop) = task.stop.take() {
                let _ = stop.send(());
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.state(id) == Some(TaskState::Running) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let output = self.output(id).unwrap_or_default();
        Ok(format!(
            "task {id} killed. output:\n{}",
            if output.trim().is_empty() { "(no output)" } else { &output }
        ))
    }

    /// Kills every running task at once, without waiting. How many there were.
    pub fn stop_all(&self) -> usize {
        let mut tasks = self.shared.tasks.lock();
        let mut stopped = 0;
        for task in tasks.values_mut().filter(|t| t.state == TaskState::Running && !t.kill_requested) {
            task.kill_requested = true;
            if let Some(pid) = task.pid {
                kill_pid_tree(pid);
            }
            if let Some(stop) = task.stop.take() {
                let _ = stop.send(());
            }
            stopped += 1;
        }
        stopped
    }

    pub fn state(&self, id: u32) -> Option<TaskState> {
        self.shared.tasks.lock().get(&id).map(|t| t.state.clone())
    }

    pub fn running_count(&self) -> usize {
        self.shared.tasks.lock().values().filter(|t| t.state == TaskState::Running).count()
    }

    /// Oldest first.
    pub fn tasks(&self) -> Vec<TaskInfo> {
        self.shared
            .tasks
            .lock()
            .iter()
            .map(|(id, t)| TaskInfo {
                id: *id,
                command: t.command.clone(),
                state: t.state.clone(),
                elapsed: t.ended.unwrap_or_else(Instant::now).duration_since(t.started),
                last_line: t.buffer.lock().lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string(),
                detached: t.detached,
            })
            .collect()
    }

    /// The end of a task's output, marked when the start was cut.
    pub fn output(&self, id: u32) -> Option<String> {
        let tasks = self.shared.tasks.lock();
        let task = tasks.get(&id)?;
        let out = tail_noted(&task.buffer.lock(), 4000).into_owned();
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn a_bare_program_name_is_found_with_its_windows_extension() {
        let found = super::find_program("cmd");
        assert!(found.to_string_lossy().to_lowercase().ends_with("cmd.exe"), "{found:?}");
        assert_eq!(super::find_program("tool.exe"), std::path::PathBuf::from("tool.exe"));
    }

    #[test]
    fn a_cut_output_says_it_was_cut() {
        let long: String = (1..=3000).map(|i| format!("line-{i}
")).collect();
        let shown = super::tail_noted(&long, 16_000);
        assert!(shown.starts_with("[earlier output omitted"), "{}", &shown[..80]);
        assert!(shown.ends_with("line-3000
"));
        assert_eq!(super::tail_noted("short", 100), "short");
    }

    #[test]
    fn a_character_split_across_reads_decodes_whole() {
        let word = "привет".as_bytes();
        let mut carry = word[..3].to_vec();
        let first = super::decode_complete(&mut carry);
        assert_eq!(first, "п");
        assert_eq!(carry.len(), 1);
        carry.extend_from_slice(&word[3..]);
        assert_eq!(first + &super::decode_complete(&mut carry), "привет");
        assert!(carry.is_empty());

        let mut bad = vec![b'a', 0xff, b'b'];
        assert_eq!(super::decode_complete(&mut bad), "a\u{FFFD}b");
    }

    use super::*;

    #[test]
    fn output_that_is_not_utf8_is_read_in_the_legacy_code_page() {
        let mut plain = "ok \u{2713}".as_bytes().to_vec();
        assert_eq!(decode(&mut plain), "ok \u{2713}");
        // "Привет" in CP866; whatever the OEM page, no byte may become U+FFFD.
        let mut oem = vec![b'>', 0x8F, 0xE0, 0xA8, 0xA2, 0xA5, 0xE2];
        let text = decode(&mut oem);
        if cfg!(windows) {
            assert_eq!(text.chars().count(), 7, "{text:?}");
            assert!(!text.contains('\u{FFFD}'), "{text:?}");
        }
        // One such read does not decide for the next.
        let mut after = "done \u{2713}".as_bytes().to_vec();
        assert_eq!(decode(&mut after), "done \u{2713}");
    }

    #[tokio::test]
    async fn quoted_arguments_reach_the_command_as_written() {
        let out = run_foreground(r#"echo "a  b" c"#, Duration::from_secs(10)).await.unwrap();
        assert!(out.contains("a  b"), "{out}");
    }

    #[tokio::test]
    async fn backslashes_reach_a_posix_shell_as_written() {
        if platform().contains("cmd.exe") {
            return;
        }
        let out = run_foreground(r#"printf '%s|%s' 'a\\b' "q\"x""#, Duration::from_secs(10)).await.unwrap();
        assert!(out.contains(r#"a\\b|q"x"#), "{out}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_exe_keeps_the_commands_own_quotes() {
        let out = cmd_exe(r#"echo "a  b" & ver"#).stdout(Stdio::piped()).output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("\"a  b\""), "{text}");
        assert!(text.contains("Windows"), "{text}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unix_commands_run_on_windows_when_git_bash_is_installed() {
        if git_bash().is_none() {
            return;
        }
        let out = run_foreground("ls -a | grep -c .", Duration::from_secs(10)).await.unwrap();
        assert!(out.contains("exit code: 0"), "{out}");
    }

    #[tokio::test]
    async fn foreground_echo_captures_output() {
        let out = run_foreground("echo hello-tools", Duration::from_secs(10)).await.unwrap();
        assert!(out.contains("exit code: 0"));
        assert!(out.contains("hello-tools"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_with_output() {
        #[cfg(windows)]
        let cmd = "echo bad 1>&2 & exit 3";
        #[cfg(not(windows))]
        let cmd = "echo bad >&2; exit 3";
        let err = run_foreground(cmd, Duration::from_secs(10)).await.unwrap_err();
        assert!(err.to_string().contains("exit code: 3"));
        assert!(err.to_string().contains("bad"));
    }

    #[tokio::test]
    async fn timeout_kills_process() {
        let err = run_foreground("sleep 5", Duration::from_millis(150)).await.unwrap_err();
        assert!(err.to_string().contains("timeout after 0s"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_whole_pipeline_promptly() {
        // Killing only `sh` would leave `sleep` holding the pipe for the full 30s.
        let started = std::time::Instant::now();
        let err = run_foreground("sleep 30; echo never", Duration::from_millis(200)).await.unwrap_err();
        assert!(err.to_string().contains("timeout"));
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_leaves_no_descendant_running() {
        // Checks the process-group kill: the pipeline's next step must never run.
        let marker = std::env::temp_dir().join(format!("fa-shell-timeout-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let cmd = format!("sleep 1; touch {}", marker.display());
        let err = run_foreground(&cmd, Duration::from_millis(150)).await.unwrap_err();
        assert!(err.to_string().contains("timeout"));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!marker.exists(), "a descendant of the timed-out command kept running");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backgrounded_grandchild_does_not_hang_the_call() {
        let started = std::time::Instant::now();
        let out = run_foreground("echo ready; sleep 30 &", Duration::from_secs(20)).await.unwrap();
        assert!(out.contains("ready"));
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_call_kills_the_process_tree() {
        let marker = std::env::temp_dir().join(format!("fa-shell-cancel-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let cmd = format!("sleep 1; touch {}", marker.display());
        // The loop drops the tool future when the user cancels.
        let _ = tokio::time::timeout(Duration::from_millis(100), run_foreground(&cmd, Duration::from_secs(30))).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!marker.exists(), "cancelled command kept running");
    }

    #[tokio::test]
    async fn background_spawn_status_kill() {
        let registry = ShellRegistry::new();
        let msg = registry.spawn_background("echo bg-marker-123; sleep 30").unwrap();
        assert!(msg.contains("background task 1"), "got: {msg}");

        let mut saw_output = false;
        for _ in 0..100 {
            let status = registry.status(1).unwrap();
            if status.contains("bg-marker-123") {
                saw_output = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(saw_output, "background output never appeared");

        let killed = registry.kill(1).await.unwrap();
        assert!(killed.contains("task 1 killed"), "{killed}");
        assert!(registry.status(1).unwrap().contains("was stopped"));
        assert!(registry.status(2).is_err());
    }

    /// The next notice, or `None` after `wait`.
    async fn next_notice(rx: &mut mpsc::UnboundedReceiver<TaskNotice>, wait: Duration) -> Option<TaskNotice> {
        tokio::time::timeout(wait, rx.recv()).await.ok().flatten()
    }

    #[tokio::test]
    async fn a_finished_background_task_is_announced_once() {
        let registry = ShellRegistry::new();
        let mut notices = registry.subscribe();
        #[cfg(windows)]
        let cmd = "echo done-marker & exit 3";
        #[cfg(not(windows))]
        let cmd = "echo done-marker; exit 3";
        registry.spawn_background(cmd).unwrap();
        let notice = next_notice(&mut notices, Duration::from_secs(10)).await.expect("no notice of the exit");
        assert_eq!(notice.id, 1);
        assert_eq!(notice.state, TaskState::Exited(Some(3)));
        assert!(!notice.killed());
        assert!(notice.tail.contains("done-marker"), "{notice:?}");
        assert!(flashagent_core::is_task_notice(&notice.message()), "{}", notice.message());
        assert!(next_notice(&mut notices, Duration::from_millis(300)).await.is_none(), "announced twice");
        assert_eq!(registry.running_count(), 0);
        assert!(registry.status(1).unwrap().contains("exited with code 3"));
    }

    #[tokio::test]
    async fn a_killed_task_is_announced_as_stopped() {
        let registry = ShellRegistry::new();
        let mut notices = registry.subscribe();
        registry.spawn_background("sleep 30").unwrap();
        assert_eq!(registry.running_count(), 1);
        registry.kill(1).await.unwrap();
        let notice = next_notice(&mut notices, Duration::from_secs(5)).await.expect("no notice of the kill");
        assert!(notice.killed(), "{notice:?}");
        assert!(next_notice(&mut notices, Duration::from_millis(300)).await.is_none(), "announced twice");
        assert!(registry.kill(1).await.unwrap().contains("already ended"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_moved_to_the_background_keeps_running_and_its_output() {
        let registry = Arc::new(ShellRegistry::new());
        let mut notices = registry.subscribe();
        assert!(!registry.detach_foreground(), "nothing was running");
        let reg = registry.clone();
        // A timeout shorter than the command: it no longer applies once detached.
        let call = tokio::spawn(async move { reg.run_foreground("echo before; sleep 1; echo after", Duration::from_millis(600)).await });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !registry.foreground_running() {
            assert!(Instant::now() < deadline, "the command never started");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(registry.detach_foreground());
        let result = tokio::time::timeout(Duration::from_secs(2), call).await.expect("the call did not return").unwrap().unwrap();
        assert!(result.contains("moved this command to the background"), "{result}");
        assert!(result.contains("background task 1"), "{result}");
        assert!(result.contains("before"), "{result}");
        assert!(!registry.foreground_running());
        assert_eq!(registry.state(1), Some(TaskState::Running));
        assert!(registry.tasks()[0].detached);

        let notice = next_notice(&mut notices, Duration::from_secs(10)).await.expect("no notice of the exit");
        assert_eq!(notice.state, TaskState::Exited(Some(0)), "{notice:?}");
        assert!(notice.tail.contains("before") && notice.tail.contains("after"), "output was lost: {notice:?}");
        assert!(next_notice(&mut notices, Duration::from_millis(300)).await.is_none(), "announced twice");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_tasks_die_with_the_registry() {
        let marker = std::env::temp_dir().join(format!("fa-shell-quit-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let registry = ShellRegistry::new();
        registry.spawn_background(&format!("sleep 1; touch {}", marker.display())).unwrap();
        registry.spawn_background("sleep 30").unwrap();
        assert_eq!(registry.stop_all(), 2);
        assert_eq!(registry.stop_all(), 0, "a stopped task counted again");
        drop(registry);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!marker.exists(), "a background task outlived FlashAgent");
    }

    #[tokio::test]
    async fn a_command_that_ends_is_not_moved() {
        let registry = ShellRegistry::new();
        let out = registry.run_foreground("echo quick", Duration::from_secs(10)).await.unwrap();
        assert!(out.contains("exit code: 0"), "{out}");
        assert!(!registry.foreground_running());
        assert!(!registry.detach_foreground());
        assert!(registry.tasks().is_empty());
    }

    #[tokio::test]
    async fn the_output_kept_for_a_task_is_capped() {
        use tokio::io::AsyncReadExt as _;
        let buffer = Arc::new(Mutex::new(String::new()));
        pump(Some(tokio::io::repeat(b'x').take(1_000_000)), buffer.clone()).await;
        let kept = buffer.lock().len();
        assert!(kept <= BUFFER_CAP && kept >= BUFFER_CAP / 2, "{kept}");
    }

    #[test]
    fn a_notice_carries_the_command_how_it_ended_and_the_end_of_its_output() {
        let output: String = (1..=50).map(|i| format!("line-{i}\n")).collect();
        let notice = TaskNotice {
            id: 7,
            command: "npm run build".into(),
            state: TaskState::Exited(Some(1)),
            elapsed: Duration::from_secs(125),
            tail: last_lines(&output, NOTICE_LINES, NOTICE_BYTES),
        };
        let text = notice.message();
        assert!(text.starts_with("[Background task 7 exited with code 1 after 2m 05s."), "{text}");
        assert!(text.contains("command: npm run build"), "{text}");
        assert!(text.contains("line-50") && text.contains("line-31") && !text.contains("line-30\n"), "{text}");
        let quiet = TaskNotice { tail: String::new(), ..notice };
        assert!(quiet.message().ends_with("(no output)"));
        assert!(last_lines(&"y".repeat(10_000), 20, NOTICE_BYTES).len() <= NOTICE_BYTES);
    }
}
