//! Curated and verifiable registry of popular, vetted MCP server extensions.

use super::config::McpServerConfig;
use std::collections::HashMap;

/// Curated MCP Marketplace item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketplaceItem {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub vendor: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub env_vars: &'static [&'static str],
    pub read_only_by_default: bool,
    pub category: &'static str,
}

pub const MARKETPLACE_ITEMS: &[MarketplaceItem] = &[
    MarketplaceItem {
        id: "sqlite",
        name: "SQLite Database",
        description: "Inspect schemas, read tables, and run queries on local SQLite databases",
        vendor: "Model Context Protocol Official",
        command: "uvx",
        args: &["mcp-server-sqlite", "--db-path", "data.db"],
        env_vars: &[],
        read_only_by_default: false,
        category: "Database",
    },
    MarketplaceItem {
        id: "github",
        name: "GitHub Integration",
        description: "Search repositories, read code, view issues, inspect pull requests, and manage releases",
        vendor: "Model Context Protocol Official",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-github"],
        env_vars: &["GITHUB_PERSONAL_ACCESS_TOKEN"],
        read_only_by_default: false,
        category: "Developer Tools",
    },
    MarketplaceItem {
        id: "postgres",
        name: "PostgreSQL Database",
        description: "Query PostgreSQL schemas, inspect table structures, and run analytical queries",
        vendor: "Model Context Protocol Official",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/mydb"],
        env_vars: &[],
        read_only_by_default: false,
        category: "Database",
    },
    MarketplaceItem {
        id: "filesystem",
        name: "Local Filesystem",
        description: "Secure local filesystem access with strict allowed directory boundaries",
        vendor: "Model Context Protocol Official",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-filesystem", "."],
        env_vars: &[],
        read_only_by_default: false,
        category: "Filesystem",
    },
    MarketplaceItem {
        id: "fetch",
        name: "Web Content Fetcher",
        description: "Extract clean web content and HTML as markdown via readability parser",
        vendor: "Model Context Protocol Official",
        command: "uvx",
        args: &["mcp-server-fetch"],
        env_vars: &[],
        read_only_by_default: true,
        category: "Web & Network",
    },
    MarketplaceItem {
        id: "brave-search",
        name: "Brave Web Search",
        description: "Web search engine queries with snippets, results, and web ranking",
        vendor: "Model Context Protocol Official",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-brave-search"],
        env_vars: &["BRAVE_API_KEY"],
        read_only_by_default: true,
        category: "Web & Network",
    },
    MarketplaceItem {
        id: "docker",
        name: "Docker Containers",
        description: "Manage containers, inspect images, view compose logs, and monitor container metrics",
        vendor: "Model Context Protocol Official",
        command: "uvx",
        args: &["mcp-server-docker"],
        env_vars: &[],
        read_only_by_default: false,
        category: "DevOps",
    },
    MarketplaceItem {
        id: "puppeteer",
        name: "Puppeteer Browser",
        description: "Automate browser interactions, capture screenshots, and scrape dynamic web pages",
        vendor: "Model Context Protocol Official",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-puppeteer"],
        env_vars: &[],
        read_only_by_default: false,
        category: "Automation",
    },
    MarketplaceItem {
        id: "git",
        name: "Git Repository",
        description: "Advanced git operations, branch visualization, commit histories, and worktrees",
        vendor: "Model Context Protocol Official",
        command: "uvx",
        args: &["mcp-server-git"],
        env_vars: &[],
        read_only_by_default: false,
        category: "Developer Tools",
    },
    MarketplaceItem {
        id: "memory",
        name: "Knowledge Graph Memory",
        description: "Persistent knowledge-graph memory server with entity and relation tracking",
        vendor: "Model Context Protocol Official",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-memory"],
        env_vars: &[],
        read_only_by_default: false,
        category: "Memory & Context",
    },
];

/// Return all marketplace items.
pub fn get_marketplace() -> &'static [MarketplaceItem] {
    MARKETPLACE_ITEMS
}

/// Find a marketplace item by id or case-insensitive query.
pub fn find_marketplace_item(query: &str) -> Option<&'static MarketplaceItem> {
    let lower = query.trim().to_lowercase();
    MARKETPLACE_ITEMS
        .iter()
        .find(|item| item.id == lower || item.name.to_lowercase().contains(&lower))
}

/// Scaffold a default `McpServerConfig` from a marketplace item.
pub fn scaffold_config(item: &MarketplaceItem) -> McpServerConfig {
    let mut env = HashMap::new();
    for v in item.env_vars {
        env.insert(v.to_string(), format!("${{{v}}}"));
    }

    McpServerConfig {
        command: item.command.to_string(),
        args: item.args.iter().map(|s| s.to_string()).collect(),
        env,
        read_only: item.read_only_by_default,
        read_only_tools: Vec::new(),
        disabled: false,
        description: Some(item.description.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marketplace_find_item() {
        let item = find_marketplace_item("sqlite").expect("sqlite exists");
        assert_eq!(item.id, "sqlite");
        assert_eq!(item.category, "Database");

        let gh = find_marketplace_item("github").expect("github exists");
        assert_eq!(gh.command, "npx");
    }

    #[test]
    fn test_scaffold_config() {
        let item = find_marketplace_item("fetch").expect("fetch exists");
        let cfg = scaffold_config(item);
        assert_eq!(cfg.command, "uvx");
        assert!(cfg.read_only);
    }
}
