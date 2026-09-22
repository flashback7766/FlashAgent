//! JSON-RPC 2.0 and MCP types, spec version 2024-11-05
//! (<https://spec.modelcontextprotocol.io>).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new_request(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(Value::from(id)),
            method: method.into(),
            params,
        }
    }

    pub fn new_notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpClientInfo {
    pub name: String,
    pub version: String,
}

impl Default for McpClientInfo {
    fn default() -> Self {
        Self {
            name: "FlashAgent".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct McpServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: Value,
    #[serde(rename = "clientInfo")]
    pub client_info: McpClientInfo,
}

impl Default for InitializeParams {
    fn default() -> Self {
        Self {
            protocol_version: MCP_PROTOCOL_VERSION.into(),
            // No client-side features (roots, sampling): a server told otherwise waits
            // on requests that are never answered.
            capabilities: serde_json::json!({}),
            client_info: McpClientInfo::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion", default)]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(rename = "serverInfo", default)]
    pub server_info: McpServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct McpToolAnnotations {
    /// The spec name is `readOnlyHint`; `readOnly` is the draft spelling.
    #[serde(rename = "readOnlyHint", alias = "readOnly", default)]
    pub read_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: Option<McpToolAnnotations>,
}

impl McpTool {
    /// The server's claim, informational only. Permissions never use it (see
    /// `McpManager::is_tool_read_only`).
    pub fn claims_read_only(&self) -> bool {
        self.annotations.as_ref().and_then(|a| a.read_only).unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsListResult {
    #[serde(default)]
    pub tools: Vec<McpTool>,
    #[serde(rename = "nextCursor", default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContentItem {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<ContentItem>,
    #[serde(rename = "isError", default)]
    pub is_error: Option<bool>,
}

impl CallToolResult {
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for item in &self.content {
            if let Some(ref text) = item.text {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            } else if let Some(ref data) = item.data {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("[Binary data: {} bytes, mime: {:?}]", data.len(), item.mime_type));
            }
        }
        if out.is_empty() {
            if self.is_error == Some(true) {
                "[Tool execution returned error with empty content]".into()
            } else {
                "[Tool executed successfully with no output]".into()
            }
        } else {
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jsonrpc_request_serialization() {
        let req = JsonRpcRequest::new_request(42, "ping", None);
        let s = serde_json::to_string(&req).expect("serialize");
        assert!(s.contains("\"id\":42"));
        assert!(s.contains("\"method\":\"ping\""));
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
    }

    #[test]
    fn test_tools_list_result_deserialization() {
        let raw = r#"{
            "tools": [
                {
                    "name": "read_query",
                    "description": "Execute select query",
                    "inputSchema": { "type": "object" },
                    "annotations": { "readOnly": true }
                }
            ]
        }"#;
        let res: ToolsListResult = serde_json::from_str(raw).expect("parse tools list");
        assert_eq!(res.tools.len(), 1);
        assert_eq!(res.tools[0].name, "read_query");
        assert!(res.tools[0].claims_read_only());
    }

    #[test]
    fn test_call_tool_result_plain_text() {
        let res = CallToolResult {
            content: vec![
                ContentItem {
                    content_type: "text".into(),
                    text: Some("First chunk".into()),
                    data: None,
                    mime_type: None,
                },
                ContentItem {
                    content_type: "text".into(),
                    text: Some("Second chunk".into()),
                    data: None,
                    mime_type: None,
                },
            ],
            is_error: Some(false),
        };
        assert_eq!(res.plain_text(), "First chunk\nSecond chunk");
    }
}
