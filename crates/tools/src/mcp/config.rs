//! MCP config discovery for project and global servers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// All tools of this server run without approval.
    #[serde(default, rename = "read_only")]
    pub read_only: bool,
    #[serde(default, rename = "read_only_tools")]
    pub read_only_tools: Vec<String>,
    /// Not spawned automatically.
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub description: Option<String>,
}

impl McpServerConfig {
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

    pub fn is_tool_read_only(&self, tool_name: &str) -> bool {
        self.read_only || self.read_only_tools.iter().any(|t| t == tool_name)
    }

    /// `${VAR}` and `$VAR` replaced from the environment.
    pub fn expanded_args(&self) -> Vec<String> {
        self.args.iter().map(|arg| expand_env_vars(arg)).collect()
    }

    /// `${VAR}` and `$VAR` replaced from the environment.
    pub fn expanded_env(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (k, v) in &self.env {
            out.insert(k.clone(), expand_env_vars(v));
        }
        out
    }
}

fn expand_env_vars(raw: &str) -> String {
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

/// Server by server: one entry this client cannot run (an HTTP server has no
/// `command`) used to fail the whole file and drop every other server.
/// `None` only when the file is not a JSON object.
fn parse_servers(content: &str) -> Option<HashMap<String, McpServerConfig>> {
    let root: serde_json::Value = serde_json::from_str(content.trim_start_matches('\u{feff}')).ok()?;
    let root = root.as_object()?;
    let servers = root.get("mcpServers").or_else(|| root.get("servers")).and_then(|s| s.as_object());
    Some(
        servers
            .into_iter()
            .flatten()
            .filter_map(|(name, cfg)| Some((name.clone(), serde_json::from_value::<McpServerConfig>(cfg.clone()).ok()?)))
            .collect(),
    )
}

/// Project config overrides global.
pub fn load_mcp_configs(cwd: &Path) -> (HashMap<String, McpServerConfig>, Vec<PathBuf>) {
    let mut servers = HashMap::new();
    let mut loaded_paths = Vec::new();

    // ~/.flashagent/mcp.json
    if let Some(home) = dirs_next().or_else(|| std::env::var("HOME").ok().map(PathBuf::from)) {
        let global_path = home.join(".flashagent").join("mcp.json");
        if global_path.is_file() {
            if let Some(parsed) = std::fs::read_to_string(&global_path).ok().and_then(|c| parse_servers(&c)) {
                servers.extend(parsed);
                loaded_paths.push(global_path);
            }
        }
    }

    // .mcp.json or mcp.json
    let project_candidates = [cwd.join(".mcp.json"), cwd.join("mcp.json")];
    for p in &project_candidates {
        if p.is_file() {
            if let Some(parsed) = std::fs::read_to_string(p).ok().and_then(|c| parse_servers(&c)) {
                servers.extend(parsed);
                loaded_paths.push(p.clone());
                break;
            }
        }
    }

    (servers, loaded_paths)
}

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

/// Writes to the project `.mcp.json`, editing it in place: servers this
/// client cannot run and fields it does not know stay as they were, and a file
/// it cannot read is left alone instead of replaced.
pub fn save_server_to_project(cwd: &Path, name: &str, config: McpServerConfig) -> Result<PathBuf, String> {
    let target = cwd.join(".mcp.json");
    let mut root = if target.is_file() {
        let content = std::fs::read_to_string(&target)
            .map_err(|e| format!("Failed to read {}: {e}", target.display()))?;
        match serde_json::from_str::<serde_json::Value>(content.trim_start_matches('\u{feff}')) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => return Err(format!("{} is not a JSON object FlashAgent can edit; add the server by hand", target.display())),
        }
    } else {
        serde_json::Map::new()
    };
    let key = if root.contains_key("mcpServers") || !root.contains_key("servers") { "mcpServers" } else { "servers" };
    let servers = root.entry(key).or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(servers) = servers.as_object_mut() else {
        return Err(format!("\"{key}\" in {} is not an object; add the server by hand", target.display()));
    };
    let value = serde_json::to_value(&config).map_err(|e| format!("Failed to serialize config: {e}"))?;
    servers.insert(name.to_string(), value);

    let serialized = serde_json::to_string_pretty(&serde_json::Value::Object(root))
        .map_err(|e| format!("Failed to serialize config: {e}"))?;

    std::fs::write(&target, serialized)
        .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_this_client_cannot_run_neither_hides_nor_loses_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".mcp.json");
        std::fs::write(&file, r#"{"mcpServers":{"docs":{"type":"http","url":"https://x"},"db":{"command":"uvx","args":["db"]}},"extra":1}"#).unwrap();
        let (servers, _) = load_mcp_configs(dir.path());
        assert!(servers.contains_key("db"), "{servers:?}");
        save_server_to_project(dir.path(), "fetch", McpServerConfig::new("uvx", vec!["fetch".into()])).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        for name in ["docs", "db", "fetch"] {
            assert!(saved["mcpServers"].get(name).is_some(), "{name} lost: {saved}");
        }
        assert_eq!(saved["extra"], 1);
        // A file it cannot read is not replaced.
        std::fs::write(&file, "{ // a comment\n}").unwrap();
        assert!(save_server_to_project(dir.path(), "x", McpServerConfig::new("x", vec![])).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ // a comment\n}");
    }

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

        let parsed = parse_servers(raw).expect("parse config");
        assert!(parsed.contains_key("sqlite"));
        let cfg = &parsed["sqlite"];
        assert_eq!(cfg.command, "uvx");
        assert_eq!(cfg.args, vec!["mcp-server-sqlite", "--db-path", "test.db"]);
        assert!(cfg.is_tool_read_only("any_query"));
    }
}
