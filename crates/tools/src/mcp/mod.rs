//! Native Model Context Protocol (MCP) subsystem for FlashAgent.
//! Supports stdio JSON-RPC 2.0 servers, project & global configuration,
//! curated marketplace, dynamic tool discovery, and security approval gates.

pub mod client;
pub mod config;
pub mod manager;
pub mod marketplace;
pub mod protocol;

pub use client::McpClient;
pub use config::{load_mcp_configs, save_server_to_project, McpConfigFile, McpServerConfig};
pub use manager::{McpManager, McpTestReport, ServerConnectionState, ServerStatus};
pub use marketplace::{find_marketplace_item, get_marketplace, scaffold_config, MarketplaceItem};
pub use protocol::{CallToolResult, McpTool, MCP_PROTOCOL_VERSION};
