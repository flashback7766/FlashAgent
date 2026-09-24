//! MCP support: stdio JSON-RPC 2.0 servers, project and global config, a
//! curated marketplace and tool discovery.

pub mod client;
pub mod config;
pub mod manager;
pub mod marketplace;
pub mod protocol;

pub use client::McpClient;
pub use config::{load_mcp_configs, save_server_to_project, McpServerConfig};
pub use manager::{McpManager, McpTestReport, ServerConnectionState, ServerStatus};
pub use marketplace::{find_marketplace_item, get_marketplace, scaffold_config, MarketplaceItem};
pub use protocol::{CallToolResult, McpTool, MCP_PROTOCOL_VERSION};
