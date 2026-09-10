//! MCP Configuration parser and discovery for project and global server configs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// Per-server configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// When true, all tools from this server are treated as read-only (no approval required).
    #[serde(default, rename = "read_only")]
    pub read_only: bool,
    /// List of specific tools from this server that are explicitly read-only.
    #[serde(default, rename = "read_only_tools")]
    pub read_only_tools: Vec<String>,
    /// When true, FlashAgent does not spawn this server automatically.
    #[serde(default)]
    pub disabled: bool,
    /// Human-friendly description or vendor note.
    #[serde(default)]
    pub description: Option<String>,
}

impl McpServerConfig {
    /// Create a new server config.
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            env: HashMap::new(),
            read_only: false,
            read_only_tools: Vec::new(),
            disabled: false,
            description: None,
        }
    }

    /// Check if a tool from this server is considered read-only.
    pub fn is_tool_read_only(&self, tool_name: &str) -> bool {
        self.read_only || self.read_only_tools.iter().any(|t| t == tool_name)
    }

    /// Return expanded command arguments with `${VAR}` and `$VAR` replaced from environment.
    pub fn expanded_args(&self) -> Vec<String> {
        self.args.iter().map(|arg| expand_env_vars(arg)).collect()
    }

    /// Return expanded environment variables with `${VAR}` and `$VAR` replaced from environment.
    pub fn expanded_env(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (k, v) in &self.env {
            out.insert(k.clone(), expand_env_vars(v));
        }
        out
    }
}

/// Root MCP configuration file schema (`.mcp.json`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct McpConfigFile {
    #[serde(default, rename = "mcpServers", alias = "servers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

/// Helper to expand `$VAR` and `${VAR}` using host environment.
pub fn expand_env_vars(raw: &str) -> String {
    let mut result = String::new();
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' {
            if chars.peek() == Some(&'{') {
                chars.next(); // consume '{'
                let mut var_name = String::new();
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    var_name.push(c);
                }
                if let Ok(val) = std::env::var(&var_name) {
                    result.push_str(&val);
                }
            } else {
                let mut var_name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' {
                        var_name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !var_name.is_empty() {
                    if let Ok(val) = std::env::var(&var_name) {
                        result.push_str(&val);
                    }
                } else {
                    result.push('$');
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Discover and merge MCP configurations from project and global paths.
/// Project configurations take precedence over global configurations.
pub fn load_mcp_configs(cwd: &Path) -> (HashMap<String, McpServerConfig>, Vec<PathBuf>) {
    let mut servers = HashMap::new();
    let mut loaded_paths = Vec::new();

    // 1. Global config: ~/.flashagent/mcp.json
    if let Some(home) = dirs_next().or_else(|| std::env::var("HOME").ok().map(PathBuf::from)) {
        let global_path = home.join(".flashagent").join("mcp.json");
        if global_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&global_path) {
                if let Ok(parsed) = serde_json::from_str::<McpConfigFile>(&content) {
                    for (name, cfg) in parsed.mcp_servers {
                        servers.insert(name, cfg);
                    }
                    loaded_paths.push(global_path);
                }
            }
        }
    }

    // 2. Project config: .mcp.json or mcp.json
    let project_candidates = [cwd.join(".mcp.json"), cwd.join("mcp.json")];
    for p in &project_candidates {
        if p.is_file() {
            if let Ok(content) = std::fs::read_to_string(p) {
                if let Ok(parsed) = serde_json::from_str::<McpConfigFile>(&content) {
                    for (name, cfg) in parsed.mcp_servers {
                        // Project overrides global
                        servers.insert(name, cfg);
                    }
                    loaded_paths.push(p.clone());
                    break;
                }
            }
        }
    }

    (servers, loaded_paths)
}

/// Helper to get user home directory without external dependencies.
fn dirs_next() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

/// Save or update a server config inside project `.mcp.json`.
pub fn save_server_to_project(cwd: &Path, name: &str, config: McpServerConfig) -> Result<PathBuf, String> {
    let target = cwd.join(".mcp.json");
    let mut file_cfg = if target.is_file() {
        let content = std::fs::read_to_string(&target)
            .map_err(|e| format!("Failed to read {}: {e}", target.display()))?;
        serde_json::from_str::<McpConfigFile>(&content).unwrap_or_default()
    } else {
        McpConfigFile::default()
    };

    file_cfg.mcp_servers.insert(name.to_string(), config);

    let serialized = serde_json::to_string_pretty(&file_cfg)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;

    std::fs::write(&target, serialized)
        .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_var_expansion() {
        std::env::set_var("TEST_MCP_FOO", "hello_world");
        assert_eq!(expand_env_vars("value_${TEST_MCP_FOO}_ok"), "value_hello_world_ok");
        assert_eq!(expand_env_vars("value_$TEST_MCP_FOO/path"), "value_hello_world/path");
    }

    #[test]
    fn test_mcp_config_deserialization() {
        let raw = r#"{
            "mcpServers": {
                "sqlite": {
                    "command": "uvx",
                    "args": ["mcp-server-sqlite", "--db-path", "test.db"],
                    "read_only": true
                }
            }
        }"#;

        let parsed: McpConfigFile = serde_json::from_str(raw).expect("parse config");
        assert!(parsed.mcp_servers.contains_key("sqlite"));
        let cfg = &parsed.mcp_servers["sqlite"];
        assert_eq!(cfg.command, "uvx");
        assert_eq!(cfg.args, vec!["mcp-server-sqlite", "--db-path", "test.db"]);
        assert!(cfg.is_tool_read_only("any_query"));
    }
}
