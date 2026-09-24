//! Server lifecycles, tool discovery and call routing for MCP.

use std::collections::{BTreeMap, HashMap};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerConnectionState {
    Active,
    /// Enabled in config but not running.
    Stopped,
    Disabled,
    Error(String),
}

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

/// For `/mcp test <server>`.
#[derive(Debug, Clone)]
pub struct McpTestReport {
    pub name: String,
    pub command: String,
    pub latency: Duration,
    pub tools: Vec<(String, Option<String>, bool)>,
    pub server_version: Option<String>,
}

pub struct McpManager {
    cwd: PathBuf,
    configs: RwLock<HashMap<String, McpServerConfig>>,
    /// By name, so their tools are offered in the same order on every run: the
    /// tool list is part of the prompt the server keeps cached.
    clients: RwLock<BTreeMap<String, Arc<McpClient>>>,
    /// Shown instead of a misleading "Disabled" for enabled but broken servers.
    start_errors: RwLock<HashMap<String, String>>,
    loaded_paths: RwLock<Vec<PathBuf>>,
}

impl McpManager {
    pub fn new(cwd: PathBuf) -> Arc<Self> {
        let (configs, loaded_paths) = load_mcp_configs(&cwd);
        Arc::new(Self {
            cwd,
            configs: RwLock::new(configs),
            clients: RwLock::new(BTreeMap::new()),
            start_errors: RwLock::new(HashMap::new()),
            loaded_paths: RwLock::new(loaded_paths),
        })
    }

    pub fn loaded_paths(&self) -> Vec<PathBuf> {
        self.loaded_paths.read().clone()
    }

    /// All at once: a server started through npx or uvx can take seconds to
    /// answer, and one slow server must not hold up the others.
    pub async fn start_enabled_servers(&self) -> Vec<(String, Result<usize, String>)> {
        let mut enabled: Vec<String> = self.configs.read().iter().filter(|(_, cfg)| !cfg.disabled).map(|(name, _)| name.clone()).collect();
        enabled.sort();
        let started = futures::future::join_all(enabled.iter().map(|name| self.start_server(name))).await;
        enabled.into_iter().zip(started).map(|(name, res)| (name, res.map(|c| c.tools().len()))).collect()
    }

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

    pub async fn reload(&self) -> Result<(), String> {
        let running = std::mem::take(&mut *self.clients.write());
        for c in running.into_values() {
            c.kill().await;
        }

        self.start_errors.write().clear();
        let (configs, paths) = load_mcp_configs(&self.cwd);
        *self.configs.write() = configs;
        *self.loaded_paths.write() = paths;

        self.start_enabled_servers().await;
        Ok(())
    }

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

    /// Only the user's config counts (`read_only`, `read_only_tools`). Server
    /// annotations are ignored: a server must not declare its own tools safe.
    pub fn is_tool_read_only(&self, qualified_tool: &str) -> bool {
        let Some((server_name, tool_name)) = self.parse_tool_name(qualified_tool) else {
            return false;
        };
        self.configs.read().get(&server_name).is_some_and(|cfg| cfg.is_tool_read_only(&tool_name))
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.parse_tool_name(name).is_some()
    }

    /// `mcp__<server>__<tool>`, or a bare tool name found on a running server.
    pub fn parse_tool_name(&self, name: &str) -> Option<(String, String)> {
        if let Some(rest) = name.strip_prefix("mcp__") {
            let parts: Vec<&str> = rest.splitn(2, "__").collect();
            if parts.len() == 2 {
                return Some((parts[0].to_string(), parts[1].to_string()));
            }
        }

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

    pub async fn call_tool(&self, qualified_tool: &str, args_json: &str) -> Result<String, ToolError> {
        let (server_name, tool_name) = self
            .parse_tool_name(qualified_tool)
            .ok_or_else(|| ToolError::Other(format!("Unknown MCP tool: {qualified_tool}")))?;

        let client = match self.start_server(&server_name).await {
            Ok(c) => c,
            Err(e) => return Err(ToolError::Other(format!("Failed to connect to MCP server '{server_name}': {e}"))),
        };

        // Unparseable arguments are an error for the model to fix; calling with no
        // arguments would run defaults it never chose. Same resolver as the approval
        // card, so what was approved is sent.
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

    #[cfg(unix)]
    #[tokio::test]
    async fn slow_servers_start_side_by_side() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = McpManager::new(dir.path().to_path_buf());
        let mut configs = HashMap::new();
        for name in ["one", "two", "three"] {
            let script = dir.path().join(format!("{name}.sh"));
            std::fs::write(
                &script,
                r#"while IFS= read -r line; do case "$line" in
                *'"initialize"'*) sleep 1; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"slow"}}}';;
                *tools/list*) echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}';;
                esac; done"#,
            )
            .unwrap();
            configs.insert(name.to_string(), McpServerConfig::new("bash", vec![script.to_string_lossy().to_string()]));
        }
        // Only these: the user's own global servers must not start in a test.
        *mgr.configs.write() = configs;
        let started = std::time::Instant::now();
        let results = mgr.start_enabled_servers().await;
        let took = started.elapsed();
        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert_eq!(results.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["one", "three", "two"]);
        assert!(took < Duration::from_millis(2500), "three one-second starts took {took:?}: one after another");
        let clients: Vec<Arc<McpClient>> = mgr.clients.read().values().cloned().collect();
        for client in clients {
            client.kill().await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_tools_of_several_servers_are_offered_in_the_same_order_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = McpManager::new(dir.path().to_path_buf());
        let names = ["zeta", "alpha", "mid"];
        for name in names {
            // In a file: arguments in the config have their `$VAR`s expanded.
            let script = dir.path().join(format!("{name}.sh"));
            std::fs::write(
                &script,
                format!(
                    r#"while IFS= read -r line; do case "$line" in
                    *initialize*) echo '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2024-11-05","serverInfo":{{"name":"{name}"}}}}}}';;
                    *tools/list*) echo '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"look","inputSchema":{{"type":"object"}}}}]}}}}';;
                    esac; done"#
                ),
            )
            .unwrap();
            let args = vec![script.to_string_lossy().to_string()];
            mgr.configs.write().insert(name.to_string(), McpServerConfig::new("bash", args));
            mgr.start_server(name).await.expect("mock server starts");
        }
        let offered: Vec<String> = mgr.get_all_tool_specs().into_iter().map(|s| s.name).collect();
        assert_eq!(offered, ["mcp__alpha__look", "mcp__mid__look", "mcp__zeta__look"]);
        let clients: Vec<Arc<McpClient>> = mgr.clients.read().values().cloned().collect();
        for client in clients {
            client.kill().await;
        }
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

        let err = mgr.call_tool("mcp__nonexistent__tool", "{}").await;
        assert!(err.is_err());
    }
}

