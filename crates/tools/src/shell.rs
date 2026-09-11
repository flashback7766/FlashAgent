//! Shell execution: foreground with timeout, background with task ids and a
//! live output buffer. Every command runs in its own process group so a
//! timeout, a kill or a cancelled turn takes down the whole pipeline — not
//! just `sh` while its children keep running and keep our pipes open.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::process::Child;

use crate::ToolError;

/// Keep at most this many bytes of live output per background task.
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
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut buf = buffer.lock();
                buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
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

fn shell_command(cmd: &str) -> tokio::process::Command {
    #[cfg(windows)]
    let mut c = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
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

/// Kill the whole process group led by `pid` (the `sh` we spawned).
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: plain syscall on a pid we spawned; a stale pid only yields ESRCH.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

/// Kills the process tree if the foreground call is dropped before the
/// command finished — i.e. the user cancelled the turn.
struct TreeGuard(Option<Child>);

impl Drop for TreeGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            kill_tree(child);
        }
    }
}

/// Wait for the output pumps, but not forever: a backgrounded grandchild
/// (`server &`) inherits the pipes and would hold them open indefinitely.
async fn drain_pumps(p1: tokio::task::JoinHandle<()>, p2: tokio::task::JoinHandle<()>) -> bool {
    tokio::time::timeout(Duration::from_millis(500), async {
        let _ = tokio::join!(p1, p2);
    })
    .await
    .is_ok()
}

/// Run a command to completion (or timeout). Non-zero exit is an error whose
/// text still carries the captured output.
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

/// Registry of live background shell tasks.
#[derive(Default)]
pub struct ShellRegistry {
    next_id: AtomicU32,
    tasks: Mutex<HashMap<u32, ShellTask>>,
}

impl ShellRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a detached background task; returns a message with its id.
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

    /// Report state and recent output of a background task.
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

    /// Kill a background task and remove it from the registry.
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
    use super::*;

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
        // `sh` forks `sleep`; killing only `sh` would leave `sleep` holding
        // the pipe and the call would block for the full 30s.
        let started = std::time::Instant::now();
        let err = run_foreground("sleep 30; echo never", Duration::from_millis(200)).await.unwrap_err();
        assert!(err.to_string().contains("timeout"));
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
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
