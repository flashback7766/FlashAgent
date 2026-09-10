//! MCP Client implementation over stdio JSON-RPC 2.0 transport.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

use super::protocol::{
    CallToolParams, CallToolResult, InitializeParams, InitializeResult, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, McpServerInfo, McpTool, ToolsListResult,
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const STDERR_RING_BUFFER_SIZE: usize = 100;

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>>>;

/// Native Stdio MCP Client.
pub struct McpClient {
    name: String,
    command: String,
    server_info: RwLock<Option<McpServerInfo>>,
    tools: RwLock<Vec<McpTool>>,
    next_id: AtomicU64,
    pending: PendingMap,
    stdin_tx: tokio::sync::mpsc::UnboundedSender<String>,
    stderr_lines: Arc<Mutex<VecDeque<String>>>,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
}

impl McpClient {
    /// Spawn an MCP server process over stdio.
    pub fn spawn(
        name: impl Into<String>,
        command: impl Into<String>,
        args: &[String],
        env: &HashMap<String, String>,
        cwd: &Path,
    ) -> Result<Arc<Self>, String> {
        let name_str = name.into();
        let cmd_str = command.into();

        let mut cmd = Command::new(&cmd_str);
        cmd.args(args);
        cmd.envs(env);
        cmd.current_dir(cwd);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Spawn process
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn MCP server '{}' ({cmd_str}): {e}", name_str))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("Failed to capture stdin for MCP server '{}'", name_str))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("Failed to capture stdout for MCP server '{}'", name_str))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("Failed to capture stderr for MCP server '{}'", name_str))?;

        let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let stderr_lines = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_RING_BUFFER_SIZE)));

        // 1. Background task for writing to stdin
        tokio::spawn(async move {
            let mut writer = tokio::io::BufWriter::new(stdin);
            while let Some(line) = stdin_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
        });

        // 2. Background task for reading from stdout
        let pending_clone = pending.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) {
                    if let Some(id_val) = resp.id {
                        let id_opt = id_val.as_u64().or_else(|| id_val.as_i64().map(|i| i as u64));
                        if let Some(id) = id_opt {
                            if let Some(tx) = pending_clone.lock().remove(&id) {
                                if let Some(err) = resp.error {
                                    let _ = tx.send(Err(err));
                                } else {
                                    let res = resp.result.unwrap_or(Value::Null);
                                    let _ = tx.send(Ok(res));
                                }
                            }
                        }
                    }
                }
            }
        });

        // 3. Background task for collecting stderr
        let stderr_ring = stderr_lines.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut ring = stderr_ring.lock();
                if ring.len() >= STDERR_RING_BUFFER_SIZE {
                    ring.pop_front();
                }
                ring.push_back(line);
            }
        });

        Ok(Arc::new(Self {
            name: name_str,
            command: cmd_str,
            server_info: RwLock::new(None),
            tools: RwLock::new(Vec::new()),
            next_id: AtomicU64::new(1),
            pending,
            stdin_tx,
            stderr_lines,
            child: Arc::new(tokio::sync::Mutex::new(Some(child))),
        }))
    }

    /// Server name identifier.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Server launch command.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Cached list of discovered tools.
    pub fn tools(&self) -> Vec<McpTool> {
        self.tools.read().clone()
    }

    /// Server metadata returned during initialization.
    pub fn server_info(&self) -> Option<McpServerInfo> {
        self.server_info.read().clone()
    }

    /// Get recent stderr lines for diagnostics.
    pub fn recent_stderr(&self) -> Vec<String> {
        self.stderr_lines.lock().iter().cloned().collect()
    }

    /// Send a JSON-RPC request and await response with timeout.
    async fn request(&self, method: &str, params: Option<Value>, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest::new_request(id, method, params);
        let json_str = serde_json::to_string(&req).map_err(|e| format!("Serialization error: {e}"))?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);

        self.stdin_tx
            .send(json_str)
            .map_err(|_| format!("MCP server '{}' stdin channel closed", self.name))?;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(val))) => Ok(val),
            Ok(Ok(Err(err))) => Err(format!("MCP error (code {}): {}", err.code, err.message)),
            Ok(Err(_)) => Err(format!("MCP server '{}' closed response channel", self.name)),
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(format!("MCP request '{method}' to '{}' timed out after {:?}", self.name, timeout))
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    fn notify(&self, method: &str, params: Option<Value>) -> Result<(), String> {
        let req = JsonRpcRequest::new_notification(method, params);
        let json_str = serde_json::to_string(&req).map_err(|e| format!("Serialization error: {e}"))?;
        self.stdin_tx
            .send(json_str)
            .map_err(|_| format!("MCP server '{}' stdin channel closed", self.name))
    }

    /// Complete MCP handshake: `initialize` + `notifications/initialized` + `tools/list`.
    pub async fn initialize(&self, timeout: Duration) -> Result<InitializeResult, String> {
        let params = serde_json::to_value(InitializeParams::default())
            .map_err(|e| format!("Invalid init params: {e}"))?;

        let res_val = self.request("initialize", Some(params), timeout).await?;
        let init_result: InitializeResult = serde_json::from_value(res_val)
            .map_err(|e| format!("Invalid initialize response: {e}"))?;

        *self.server_info.write() = Some(init_result.server_info.clone());

        // Send required initialized notification
        let _ = self.notify("notifications/initialized", None);

        // Populate tools list immediately
        let _ = self.refresh_tools(timeout).await;

        Ok(init_result)
    }

    /// Query and update the server's tools list.
    pub async fn refresh_tools(&self, timeout: Duration) -> Result<Vec<McpTool>, String> {
        let res_val = self.request("tools/list", Some(serde_json::json!({})), timeout).await?;
        let list_res: ToolsListResult = serde_json::from_value(res_val)
            .map_err(|e| format!("Invalid tools/list response: {e}"))?;

        *self.tools.write() = list_res.tools.clone();
        Ok(list_res.tools)
    }

    /// Call an MCP tool on this server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
        timeout: Duration,
    ) -> Result<CallToolResult, String> {
        let params = CallToolParams {
            name: tool_name.to_string(),
            arguments,
        };
        let p_val = serde_json::to_value(params).map_err(|e| format!("Failed to serialize tool params: {e}"))?;

        let res_val = self.request("tools/call", Some(p_val), timeout).await?;
        let call_res: CallToolResult = serde_json::from_value(res_val)
            .map_err(|e| format!("Invalid tools/call response: {e}"))?;

        Ok(call_res)
    }

    /// Send a ping request to verify server responsiveness and roundtrip latency.
    pub async fn ping(&self, timeout: Duration) -> Result<Duration, String> {
        let start = std::time::Instant::now();
        self.request("ping", None, timeout).await?;
        Ok(start.elapsed())
    }

    /// Check if the child process is still running.
    pub async fn is_alive(&self) -> bool {
        let mut guard = self.child.lock().await;
        if let Some(child) = guard.as_mut() {
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
    }

    /// Kill the child process.
    pub async fn kill(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Drop channels; child process will see EOF on stdin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_client_spawn_and_communication() {
        // Spawn a simple bash script that acts as an MCP server responding to initialize and tools/list
        let mock_script = r#"
while IFS= read -r line; do
    if [[ "$line" =~ "initialize" ]]; then
        echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"mock-server","version":"1.0.0"}}}'
    elif [[ "$line" =~ "tools/list" ]]; then
        echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"test_tool","description":"A test tool","inputSchema":{"type":"object"}}]}}'
    elif [[ "$line" =~ "tools/call" ]]; then
        echo '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hello from mock"}],"isError":false}}'
    fi
done
"#;

        let client = McpClient::spawn(
            "mock",
            "bash",
            &["-c".to_string(), mock_script.to_string()],
            &HashMap::new(),
            Path::new("."),
        )
        .expect("spawn mock");

        let init = client.initialize(DEFAULT_TIMEOUT).await.expect("initialize");
        assert_eq!(init.server_info.name, "mock-server");

        let tools = client.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "test_tool");

        let result = client.call_tool("test_tool", None, DEFAULT_TIMEOUT).await.expect("call_tool");
        assert_eq!(result.plain_text(), "hello from mock");

        client.kill().await;
    }
}
