//! Visual renderers for MCP (Model Context Protocol) cards and diagnostics in FlashAgent TUI.

use std::path::{Path, PathBuf};
use flashagent_tools::mcp::{MarketplaceItem, McpTestReport, ServerConnectionState, ServerStatus};
use crate::{clip_ansi, visible_width};

const RESET: &str = "\x1b[0m";
const BORDER_GOLD: &str = "\x1b[38;2;225;175;95m";
const BORDER_DIM: &str = "\x1b[38;2;75;99;130m";
const TEXT_BRIGHT: &str = "\x1b[1;38;2;245;240;235m";
const TEXT_MUTED: &str = "\x1b[38;2;145;150;160m";
const TEXT_GREEN: &str = "\x1b[38;2;135;220;145m";
const TEXT_YELLOW: &str = "\x1b[38;2;240;210;115m";
const TEXT_CYAN: &str = "\x1b[38;2;110;175;230m";
const TEXT_RED: &str = "\x1b[38;2;245;120;120m";
const TEXT_MAGENTA: &str = "\x1b[38;2;215;145;230m";

/// Format a single boxed line with padding.
fn box_line(content: &str, inner_w: usize, border: &str) -> String {
    let vis_len = visible_width(content);
    let pad_len = inner_w.saturating_sub(vis_len);
    format!("{border}│{RESET} {content}{RESET}{} {border}│{RESET}", " ".repeat(pad_len))
}

/// Render the top rounded border of a card.
fn box_top(title: &str, inner_w: usize, border: &str) -> String {
    let t_vis = visible_width(title);
    let dashes = inner_w.saturating_sub(t_vis + 3);
    format!("{border}╭─ {TEXT_BRIGHT}{title}{RESET}{border} {}╮{RESET}", "─".repeat(dashes))
}

/// Render the bottom rounded border of a card.
fn box_bottom(inner_w: usize, border: &str) -> String {
    format!("{border}╰{}╯{RESET}", "─".repeat(inner_w + 2))
}

/// Render `/mcp` overview card.
pub fn render_mcp_overview(
    loaded_paths: &[PathBuf],
    servers: &[ServerStatus],
    width: usize,
) -> Vec<String> {
    let box_w = width.saturating_sub(4).clamp(44, 110);
    let inner_w = box_w.saturating_sub(4);
    let mut lines = Vec::new();

    lines.push(box_top("Model Context Protocol (MCP)", inner_w, BORDER_GOLD));
    lines.push(box_line(&format!("{TEXT_MUTED}Connect local tool providers and external servers via JSON-RPC 2.0"), inner_w, BORDER_GOLD));
    lines.push(box_line("", inner_w, BORDER_GOLD));

    // Stats
    let active_count = servers.iter().filter(|s| s.state == ServerConnectionState::Active).count();
    let total_tools: usize = servers.iter().map(|s| s.tool_count).sum();
    let stats_text = format!(
        "{TEXT_BRIGHT}Configured Servers:{RESET} {}  ·  {TEXT_GREEN}Connected:{RESET} {}  ·  {TEXT_CYAN}Discovered Tools:{RESET} {}",
        servers.len(),
        active_count,
        total_tools
    );
    lines.push(box_line(&stats_text, inner_w, BORDER_GOLD));

    // Config search paths
    let paths_display = if loaded_paths.is_empty() {
        format!("{TEXT_MUTED}None found (Checked .mcp.json, ~/.flashagent/mcp.json)")
    } else {
        loaded_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    lines.push(box_line(&format!("{TEXT_MUTED}Config files: {paths_display}"), inner_w, BORDER_GOLD));
    lines.push(box_line("", inner_w, BORDER_GOLD));

    // Commands overview
    lines.push(box_line(&format!("{TEXT_YELLOW}Available Commands:{RESET}"), inner_w, BORDER_GOLD));
    lines.push(box_line(&format!("  {TEXT_CYAN}/mcp list{RESET}       — List all configured servers, latency and tools"), inner_w, BORDER_GOLD));
    lines.push(box_line(&format!("  {TEXT_CYAN}/mcp market{RESET}     — Browse curated vetted extensions (SQLite, GitHub, etc.)"), inner_w, BORDER_GOLD));
    lines.push(box_line(&format!("  {TEXT_CYAN}/mcp test <name>{RESET} — Verify handshake, ping latency and inspect schema"), inner_w, BORDER_GOLD));
    lines.push(box_line(&format!("  {TEXT_CYAN}/mcp add <id>{RESET}    — Install and scaffold an extension to project .mcp.json"), inner_w, BORDER_GOLD));
    lines.push(box_line(&format!("  {TEXT_CYAN}/mcp reload{RESET}     — Reload configurations and reconnect running servers"), inner_w, BORDER_GOLD));

    lines.push(box_bottom(inner_w, BORDER_GOLD));
    lines
}

/// Render `/mcp list` card.
pub fn render_mcp_server_list(servers: &[ServerStatus], width: usize) -> Vec<String> {
    let box_w = width.saturating_sub(4).clamp(44, 110);
    let inner_w = box_w.saturating_sub(4);
    let mut lines = Vec::new();

    lines.push(box_top("Configured MCP Servers", inner_w, BORDER_GOLD));

    if servers.is_empty() {
        lines.push(box_line(&format!("{TEXT_MUTED}No MCP servers configured yet in .mcp.json or ~/.flashagent/mcp.json"), inner_w, BORDER_GOLD));
        lines.push(box_line("", inner_w, BORDER_GOLD));
        lines.push(box_line(&format!("Type {TEXT_CYAN}/mcp market{RESET} to browse curated extensions or {TEXT_CYAN}/mcp add sqlite{RESET} to install."), inner_w, BORDER_GOLD));
    } else {
        for (i, s) in servers.iter().enumerate() {
            if i > 0 {
                lines.push(box_line(&format!("{BORDER_DIM}{}{RESET}", "─".repeat(inner_w)), inner_w, BORDER_GOLD));
            }

            let (status_bullet, status_label) = match &s.state {
                ServerConnectionState::Active => (TEXT_GREEN, format!("Active ({} tools)", s.tool_count)),
                ServerConnectionState::Disabled => (TEXT_MUTED, "Disabled".to_string()),
                ServerConnectionState::Error(err) => (TEXT_RED, format!("Error: {err}")),
            };

            let policy = if s.read_only {
                format!("{TEXT_GREEN}[Read-only: Auto-approved]{RESET}")
            } else {
                format!("{TEXT_YELLOW}[Approval Required: Preview]{RESET}")
            };

            lines.push(box_line(
                &format!("{status_bullet}●{RESET} {TEXT_BRIGHT}{}{RESET}  ·  {status_bullet}{status_label}{RESET}  ·  {policy}", s.name),
                inner_w,
                BORDER_GOLD,
            ));

            lines.push(box_line(
                &format!("  {TEXT_MUTED}Command:{RESET} {}", s.command),
                inner_w,
                BORDER_GOLD,
            ));

            if !s.tool_names.is_empty() {
                let preview = s.tool_names.join(", ");
                let clipped = clip_ansi(&preview, inner_w.saturating_sub(12));
                lines.push(box_line(
                    &format!("  {TEXT_CYAN}Tools:{RESET} {TEXT_MUTED}{clipped}"),
                    inner_w,
                    BORDER_GOLD,
                ));
            }
        }
    }

    lines.push(box_bottom(inner_w, BORDER_GOLD));
    lines
}

/// Render `/mcp market` catalog.
pub fn render_mcp_marketplace(items: &[MarketplaceItem], width: usize) -> Vec<String> {
    let box_w = width.saturating_sub(4).clamp(44, 110);
    let inner_w = box_w.saturating_sub(4);
    let mut lines = Vec::new();

    lines.push(box_top("Curated MCP Marketplace", inner_w, BORDER_GOLD));
    lines.push(box_line(&format!("{TEXT_MUTED}Vetted extensions ready to scaffold into your project"), inner_w, BORDER_GOLD));
    lines.push(box_line("", inner_w, BORDER_GOLD));

    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            lines.push(box_line(&format!("{BORDER_DIM}{}{RESET}", "─".repeat(inner_w)), inner_w, BORDER_GOLD));
        }

        let cat_badge = format!("{TEXT_MAGENTA}[{}]{RESET}", item.category);
        let title_line = format!("{cat_badge} {TEXT_BRIGHT}{}{RESET} ({TEXT_CYAN}/mcp add {}{RESET})", item.name, item.id);
        lines.push(box_line(&title_line, inner_w, BORDER_GOLD));

        lines.push(box_line(&format!("  {TEXT_MUTED}{}", item.description), inner_w, BORDER_GOLD));

        let mut meta = format!("  {TEXT_MUTED}Runtime:{RESET} {}  ·  {TEXT_MUTED}Vendor:{RESET} {}", item.command, item.vendor);
        if !item.env_vars.is_empty() {
            meta.push_str(&format!("  ·  {TEXT_YELLOW}Auth:{RESET} {}", item.env_vars.join(", ")));
        }
        lines.push(box_line(&meta, inner_w, BORDER_GOLD));
    }

    lines.push(box_bottom(inner_w, BORDER_GOLD));
    lines
}

/// Render `/mcp test <server>` test report.
pub fn render_mcp_test_report(report: &McpTestReport, width: usize) -> Vec<String> {
    let box_w = width.saturating_sub(4).clamp(44, 110);
    let inner_w = box_w.saturating_sub(4);
    let mut lines = Vec::new();

    let title = format!("MCP Test Report: {}", report.name);
    lines.push(box_top(&title, inner_w, BORDER_GOLD));

    let ver = report.server_version.as_deref().unwrap_or("1.0.0");
    let lat_str = format!("{:.2}ms", report.latency.as_secs_f64() * 1000.0);
    lines.push(box_line(
        &format!("{TEXT_GREEN}● Handshake Successful{RESET}  ·  {TEXT_MUTED}Version:{RESET} {ver}  ·  {TEXT_CYAN}Latency:{RESET} {lat_str}"),
        inner_w,
        BORDER_GOLD,
    ));
    lines.push(box_line(&format!("{TEXT_MUTED}Command: {}{RESET}", report.command), inner_w, BORDER_GOLD));
    lines.push(box_line("", inner_w, BORDER_GOLD));

    lines.push(box_line(
        &format!("{TEXT_BRIGHT}Discovered Tools ({}):{RESET}", report.tools.len()),
        inner_w,
        BORDER_GOLD,
    ));

    for (name, desc, read_only) in &report.tools {
        let tag = if *read_only {
            format!("{TEXT_GREEN}[Read-only: Auto-approved]{RESET}")
        } else {
            format!("{TEXT_YELLOW}[Approval Required: Preview]{RESET}")
        };
        let tool_head = format!("  • {TEXT_CYAN}{name}{RESET}  {tag}");
        lines.push(box_line(&tool_head, inner_w, BORDER_GOLD));

        if let Some(d) = desc {
            let clipped = clip_ansi(d, inner_w.saturating_sub(6));
            lines.push(box_line(&format!("    {TEXT_MUTED}{clipped}{RESET}"), inner_w, BORDER_GOLD));
        }
    }

    lines.push(box_bottom(inner_w, BORDER_GOLD));
    lines
}

/// Render `/mcp add` success card.
pub fn render_mcp_add_success(item: &MarketplaceItem, target_path: &Path, width: usize) -> Vec<String> {
    let box_w = width.saturating_sub(4).clamp(44, 110);
    let inner_w = box_w.saturating_sub(4);
    let mut lines = Vec::new();

    let title = format!("Added MCP Extension: {}", item.name);
    lines.push(box_top(&title, inner_w, BORDER_GOLD));

    lines.push(box_line(
        &format!("{TEXT_GREEN}✔ Scaffolding saved to {}{RESET}", target_path.display()),
        inner_w,
        BORDER_GOLD,
    ));
    lines.push(box_line(&format!("{TEXT_MUTED}{}", item.description), inner_w, BORDER_GOLD));

    if !item.env_vars.is_empty() {
        lines.push(box_line("", inner_w, BORDER_GOLD));
        lines.push(box_line(
            &format!("{TEXT_YELLOW}Notice:{RESET} This server requires environment variables:"),
            inner_w,
            BORDER_GOLD,
        ));
        for v in item.env_vars {
            lines.push(box_line(&format!("  export {v}=\"your-key-here\""), inner_w, BORDER_GOLD));
        }
    }

    lines.push(box_line("", inner_w, BORDER_GOLD));
    lines.push(box_line(
        &format!("Run {TEXT_CYAN}/mcp test {}{RESET} to verify connection.", item.id),
        inner_w,
        BORDER_GOLD,
    ));

    lines.push(box_bottom(inner_w, BORDER_GOLD));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_render_mcp_overview_contains_sections() {
        let lines = render_mcp_overview(&[], &[], 80);
        assert!(!lines.is_empty());
        let text = lines.join("\n");
        assert!(text.contains("Model Context Protocol"));
        assert!(text.contains("/mcp list"));
        assert!(text.contains("/mcp market"));
    }

    #[test]
    fn test_render_mcp_test_report() {
        let report = McpTestReport {
            name: "sqlite".to_string(),
            command: "uvx mcp-server-sqlite".to_string(),
            latency: Duration::from_millis(4),
            tools: vec![("read_query".to_string(), Some("Execute select".to_string()), true)],
            server_version: Some("1.0.0".to_string()),
        };
        let lines = render_mcp_test_report(&report, 80);
        let text = lines.join("\n");
        assert!(text.contains("sqlite"));
        assert!(text.contains("read_query"));
        assert!(text.contains("Read-only: Auto-approved"));
    }
}
