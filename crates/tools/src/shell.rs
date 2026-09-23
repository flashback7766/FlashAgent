//! Shell execution: foreground with timeout, background with task ids and a
//! live output buffer. Each command gets its own process group, so a timeout
//! or cancel kills the whole pipeline, not just `sh` while its children keep
//! our pipes open.
//!
//! On Windows commands go to Git Bash when it is installed: models write Unix
//! commands, and cmd.exe runs none of them. Without it, cmd.exe runs them and
//! the system prompt says so.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::process::Child;

use crate::ToolError;

/// Per background task.
const BUFFER_CAP: usize = 200_000;

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
    let keep = rest.to_vec();
    *bytes = keep;
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

/// The platform line of the system prompt: the shell decides which commands
/// the model can write.
pub fn platform() -> String {
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

/// cmd.exe does not read the quoting `arg` would add; /S keeps the command's
/// own quotes as written.
#[cfg(windows)]
fn cmd_exe(cmd: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("cmd");
    c.raw_arg(format!("/D /S /C \"{cmd}\""));
    c
}

fn shell_command(cmd: &str) -> tokio::process::Command {
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

fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: plain syscall on a pid we spawned; a stale pid only yields ESRCH.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
    // Before the shell dies: taskkill finds the children through their parent.
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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
    let mut child =
        shell_command(cmd).spawn().map_err(|e| ToolError::Other(format!("spawn: {e}")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let buffer = Arc::new(Mutex::new(String::new()));
    let p1 = tokio::spawn(pump(stdout, buffer.clone()));
    let p2 = tokio::spawn(pump(stderr, buffer.clone()));
    let mut guard = TreeGuard(Some(child));

    let waited = match guard.0.as_mut() {
        Some(child) => tokio::time::timeout(timeout, child.wait()).await,
        None => return Err(ToolError::Other("spawn: lost child handle".into())),
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
            let out = tail_str(&out, 4000);
            return Err(ToolError::Other(format!(
                "timeout after {}s, process killed. partial output:\n{}",
                timeout.as_secs(),
                if out.trim().is_empty() { "(no output)" } else { out }
            )));
        }
    };

    let complete = drain_pumps(p1, p2).await;
    let out = buffer.lock();
    let out = tail_str(&out, 16_000);
    let out = if out.trim().is_empty() { "(no output)" } else { out };
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

struct ShellTask {
    child: Child,
    buffer: Arc<Mutex<String>>,
}

#[derive(Default)]
pub struct ShellRegistry {
    next_id: AtomicU32,
    tasks: Mutex<HashMap<u32, ShellTask>>,
}

impl ShellRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn_background(&self, cmd: &str) -> Result<String, ToolError> {
        let mut child =
            shell_command(cmd).spawn().map_err(|e| ToolError::Other(format!("spawn: {e}")))?;
        let buffer = Arc::new(Mutex::new(String::new()));
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        tokio::spawn(pump(stdout, buffer.clone()));
        tokio::spawn(pump(stderr, buffer.clone()));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.tasks.lock().insert(id, ShellTask { child, buffer });
        Ok(format!("started background task {id}; poll with {{\"task_id\":{id}}}"))
    }

    pub fn status(&self, id: u32) -> Result<String, ToolError> {
        let mut tasks = self.tasks.lock();
        let task = tasks
            .get_mut(&id)
            .ok_or_else(|| ToolError::Other(format!("no such background task: {id}")))?;
        let state = match task.child.try_wait() {
            Ok(Some(status)) if status.success() => "finished, exit code 0".to_string(),
            Ok(Some(status)) => format!("finished, exit code {}", status.code().unwrap_or(-1)),
            Ok(None) => "running".to_string(),
            Err(e) => format!("wait error: {e}"),
        };
        let guard = task.buffer.lock();
        let output = tail_str(&guard, 4000);
        Ok(format!(
            "task {id}: {state}\noutput:\n{}",
            if output.trim().is_empty() { "(no output yet)" } else { output }
        ))
    }

    pub fn kill(&self, id: u32) -> Result<String, ToolError> {
        let mut tasks = self.tasks.lock();
        let mut task = tasks
            .remove(&id)
            .ok_or_else(|| ToolError::Other(format!("no such background task: {id}")))?;
        kill_tree(&mut task.child);
        let _ = task.child.try_wait();
        let guard = task.buffer.lock();
        let output = tail_str(&guard, 4000);
        Ok(format!(
            "task {id} killed. output:\n{}",
            if output.trim().is_empty() { "(no output)" } else { output }
        ))
    }
}

#[cfg(test)]
mod tests {
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

        let killed = registry.kill(1).unwrap();
        assert!(killed.contains("task 1 killed"));
        assert!(registry.status(1).is_err());
    }
}
