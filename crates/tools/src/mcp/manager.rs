//! MCP Manager: coordinates server lifecycles, tool discovery, and execution routing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use flashagent_llm::ToolSpec;
use parking_lot::RwLock;
use serde_json::Value;

use super::client::McpClient;
use super::config::{load_mcp_configs, McpServerConfig};
use crate::ToolError;

const INIT_TIMEOUT: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Server connection status for TUI and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerConnectionState {
    Active,
    /// Enabled in config but not running (not started yet or stopped).
    Stopped,
    Disabled,
    Error(String),
}

/// Status summary for one MCP server.
#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub name: String,
    pub command: String,
    pub state: ServerConnectionState,
    pub tool_count: usize,
    pub tool_names: Vec<String>,
    pub read_only: bool,
    pub description: Option<String>,
}

/// Detailed test report for `/mcp test <server>`.
#[derive(Debug, Clone)]
pub struct McpTestReport {
    pub name: String,
    pub command: String,
    pub latency: Duration,
    pub tools: Vec<(String, Option<String>, bool)>,
    pub server_version: Option<String>,
}

/// Central manager for all configured MCP servers.
pub struct McpManager {
    cwd: PathBuf,
    configs: RwLock<HashMap<String, McpServerConfig>>,
    clients: RwLock<HashMap<String, Arc<McpClient>>>,
    /// Why the last start attempt of a server failed, shown instead of a
    /// misleading "Disabled" for servers that are enabled but broken.
    start_errors: RwLock<HashMap<String, String>>,
    loaded_paths: RwLock<Vec<PathBuf>>,
}

impl McpManager {
    /// Create manager rooted at `cwd` and automatically load configs.
    pub fn new(cwd: PathBuf) -> Arc<Self> {
        let (configs, loaded_paths) = load_mcp_configs(&cwd);
        Arc::new(Self {
            cwd,
            configs: RwLock::new(configs),
            clients: RwLock::new(HashMap::new()),
            start_errors: RwLock::new(HashMap::new()),
            loaded_paths: RwLock::new(loaded_paths),
        })
    }

    /// Paths of loaded config files (.mcp.json, ~/.flashagent/mcp.json).
    pub fn loaded_paths(&self) -> Vec<PathBuf> {
        self.loaded_paths.read().clone()
    }

    /// Start all enabled servers in parallel.
    pub async fn start_enabled_servers(&self) -> Vec<(String, Result<usize, String>)> {
        let enabled: Vec<(String, McpServerConfig)> = {
            let configs = self.configs.read();
            configs
                .iter()
                .filter(|(_, cfg)| !cfg.disabled)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };

        let mut results = Vec::new();
        for (name, _) in enabled {
            let res = self.start_server(&name).await.map(|c| c.tools().len());
            results.push((name, res));
        }
        results
    }

    /// Start or obtain a running MCP server client.
    pub async fn start_server(&self, name: &str) -> Result<Arc<McpClient>, String> {
        let res = self.try_start_server(name).await;
        match &res {
            Ok(_) => {
                self.start_errors.write().remove(name);
            }
            Err(e) => {
                self.start_errors.write().insert(name.to_string(), e.clone());
            }
        }
        res
    }

    async fn try_start_server(&self, name: &str) -> Result<Arc<McpClient>, String> {
        // Check if already active and alive
        let existing = { self.clients.read().get(name).cloned() };
        if let Some(existing) = existing {
            if existing.is_alive().await {
                return Ok(existing);
            }
        }

        let cfg = {
            let configs = self.configs.read();
            configs
                .get(name)
                .cloned()
                .ok_or_else(|| format!("MCP server '{name}' is not configured in .mcp.json"))?
        };

        let expanded_args = cfg.expanded_args();
        let expanded_env = cfg.expanded_env();

        let client = McpClient::spawn(name, &cfg.command, &expanded_args, &expanded_env, &self.cwd)?;

        // Initialize handshake
        client.initialize(INIT_TIMEOUT).await.map_err(|e| {
            let stderr_dump = client.recent_stderr();
            if !stderr_dump.is_empty() {
                format!("{e}\nServer stderr:\n{}", stderr_dump.join("\n"))
            } else {
                e
            }
        })?;

        self.clients.write().insert(name.to_string(), client.clone());
        Ok(client)
    }

    /// Stop an active server.
    pub async fn stop_server(&self, name: &str) {
        let client = { self.clients.write().remove(name) };
        if let Some(client) = client {
            client.kill().await;
        }
    }

    /// Reload configurations from disk and restart enabled servers.
    pub async fn reload(&self) -> Result<(), String> {
        // 1. Kill all running servers
        let running: Vec<Arc<McpClient>> = { self.clients.write().drain().map(|(_, c)| c).collect() };
        for c in running {
            c.kill().await;
        }

        // 2. Re-read config files
        self.start_errors.write().clear();
        let (configs, paths) = load_mcp_configs(&self.cwd);
        *self.configs.write() = configs;
        *self.loaded_paths.write() = paths;

        // 3. Auto-start enabled servers
        self.start_enabled_servers().await;
        Ok(())
    }

    /// Generate LLM `ToolSpec` items for all tools discovered on active servers.
    pub fn get_all_tool_specs(&self) -> Vec<ToolSpec> {
        let clients = self.clients.read();
        let mut specs = Vec::new();

        for (server_name, client) in clients.iter() {
            for tool in client.tools() {
                let qualified_name = format!("mcp__{server_name}__{}", tool.name);
                let desc = match tool.description {
                    Some(ref d) => format!("[MCP: {server_name}] {d}"),
                    None => format!("[MCP: {server_name}] External tool {}", tool.name),
                };

                let schema_str = if tool.input_schema.is_object() {
                    tool.input_schema.to_string()
                } else {
                    r#"{"type":"object","properties":{}}"#.to_string()
                };

                specs.push(ToolSpec {
                    name: qualified_name,
                    description: desc,
                    parameters_json: schema_str,
                });
            }
        }

        specs
    }

    /// Check if a qualified tool name is classified as read-only.
    pub fn is_tool_read_only(&self, qualified_tool: &str) -> bool {
        let (server_name, tool_name) = match self.parse_tool_name(qualified_tool) {
            Some(pair) => pair,
            None => return false,
        };

        // 1. Check server configuration flags
        if let Some(cfg) = self.configs.read().get(&server_name) {
            if cfg.is_tool_read_only(&tool_name) {
                return true;
            }
        }

        // 2. Check MCP tool annotations from server
        if let Some(client) = self.clients.read().get(&server_name) {
            for t in client.tools() {
                if t.name == tool_name && t.is_read_only() {
                    return true;
                }
            }
        }

        false
    }

    /// True if the tool name belongs to an MCP server or matches `mcp__`.
    pub fn has_tool(&self, name: &str) -> bool {
        self.parse_tool_name(name).is_some()
    }

    /// Parse `mcp__<server>__<tool>` or find server for `<tool>`.
    pub fn parse_tool_name(&self, name: &str) -> Option<(String, String)> {
        if let Some(rest) = name.strip_prefix("mcp__") {
            let parts: Vec<&str> = rest.splitn(2, "__").collect();
            if parts.len() == 2 {
                return Some((parts[0].to_string(), parts[1].to_string()));
            }
        }

        // Fallback: check if name matches any tool on active clients
        let clients = self.clients.read();
        for (server_name, client) in clients.iter() {
            for t in client.tools() {
                if t.name == name {
                    return Some((server_name.clone(), t.name.clone()));
                }
            }
        }

        None
    }

    /// Dispatch execution of an MCP tool.
    pub async fn call_tool(&self, qualified_tool: &str, args_json: &str) -> Result<String, ToolError> {
        let (server_name, tool_name) = self
            .parse_tool_name(qualified_tool)
            .ok_or_else(|| ToolError::Other(format!("Unknown MCP tool: {qualified_tool}")))?;

        // Ensure server is running
        let client = match self.start_server(&server_name).await {
            Ok(c) => c,
            Err(e) => return Err(ToolError::Other(format!("Failed to connect to MCP server '{server_name}': {e}"))),
        };

        // Parse arguments JSON into Value. Unparseable arguments are an
        // error for the model to fix — silently calling the tool with no
        // arguments would run it with defaults the model never chose.
        // Same resolver as the approval card, so what was approved is sent.
        let args_val: Option<Value> = if args_json.trim().is_empty() {
            None
        } else {
            match flashagent_llm::effective_args(args_json, qualified_tool) {
                Some(v) => Some(v),
                None => return Err(ToolError::Other(format!("invalid JSON arguments for {qualified_tool}: {args_json}"))),
            }
        };

        let result = client
            .call_tool(&tool_name, args_val, CALL_TIMEOUT)
            .await
            .map_err(|e| ToolError::Other(format!("MCP execution failed on '{server_name}': {e}")))?;

        if result.is_error == Some(true) {
            Err(ToolError::Other(result.plain_text()))
        } else {
            Ok(result.plain_text())
        }
    }

    /// Get connection status list of all configured servers.
    pub async fn server_status_list(&self) -> Vec<ServerStatus> {
        let configs = self.configs.read().clone();
        let clients = self.clients.read().clone();
        let errors = self.start_errors.read().clone();
        let mut list = Vec::new();

        for (name, cfg) in configs {
            let (state, tool_count, tool_names) = if cfg.disabled {
                (ServerConnectionState::Disabled, 0, Vec::new())
            } else if let Some(client) = clients.get(&name) {
                if client.is_alive().await {
                    let tools = client.tools();
                    let names = tools.iter().map(|t| t.name.clone()).collect();
                    (ServerConnectionState::Active, tools.len(), names)
                } else {
                    (
                        ServerConnectionState::Error("Process exited unexpectedly".into()),
                        0,
                        Vec::new(),
                    )
                }
            } else if let Some(err) = errors.get(&name) {
                (ServerConnectionState::Error(err.clone()), 0, Vec::new())
            } else {
                (ServerConnectionState::Stopped, 0, Vec::new())
            };

            list.push(ServerStatus {
                name,
                command: cfg.command,
                state,
                tool_count,
                tool_names,
                read_only: cfg.read_only,
                description: cfg.description,
            });
        }

        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// Test server connection, measure latency, and list available tools.
    pub async fn test_server(&self, name: &str) -> Result<McpTestReport, String> {
        let client = self.start_server(name).await?;
        let latency = client.ping(Duration::from_secs(10)).await?;
        let tools = client.refresh_tools(Duration::from_secs(10)).await?;
        let server_version = client.server_info().and_then(|s| s.version);

        let tool_tuples = tools
            .into_iter()
            .map(|t| (t.name.clone(), t.description.clone(), self.is_tool_read_only(&format!("mcp__{name}__{}", t.name))))
            .collect();

        Ok(McpTestReport {
            name: name.to_string(),
            command: client.command().to_string(),
            latency,
            tools: tool_tuples,
            server_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_manager_tool_parsing_and_read_only() {
        let cwd = std::env::current_dir().unwrap();
        let mgr = McpManager::new(cwd);

        // Add in-memory config for testing
        {
            let mut cfg = McpServerConfig::new("echo", vec![]);
            cfg.read_only_tools = vec!["read_query".to_string()];
            mgr.configs.write().insert("mock_db".to_string(), cfg);
        }

        assert_eq!(
            mgr.parse_tool_name("mcp__mock_db__read_query"),
            Some(("mock_db".to_string(), "read_query".to_string()))
        );

        assert!(mgr.is_tool_read_only("mcp__mock_db__read_query"));
        assert!(!mgr.is_tool_read_only("mcp__mock_db__write_query"));
        assert!(!mgr.is_tool_read_only("unknown_tool"));
    }

    #[tokio::test]
    async fn test_mcp_manager_status_and_specs() {
        let cwd = std::env::current_dir().unwrap();
        let mgr = McpManager::new(cwd);

        {
            let mut cfg = McpServerConfig::new("echo", vec![]);
            cfg.description = Some("Mock database".to_string());
            mgr.configs.write().insert("db1".to_string(), cfg);

            let mut cfg2 = McpServerConfig::new("cat", vec![]);
            cfg2.disabled = true;
            mgr.configs.write().insert("db2".to_string(), cfg2);
        }

        let statuses = mgr.server_status_list().await;
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].name, "db1");
        assert_eq!(statuses[1].name, "db2");
        assert_eq!(statuses[1].state, ServerConnectionState::Disabled);

        // Unknown tool execution returns clean error
        let err = mgr.call_tool("mcp__nonexistent__tool", "{}").await;
        assert!(err.is_err());
    }
}

