//! Visual renderers for MCP (Model Context Protocol) cards and diagnostics in FlashAgent TUI.

use std::path::{Path, PathBuf};
use crossterm::event::{KeyCode, KeyModifiers};
use flashagent_tools::mcp::{MarketplaceItem, McpTestReport, ServerConnectionState, ServerStatus};
use crate::{clip_ansi, visible_width, LineKind, RenderLine};

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

/// Tabs for the interactive MCP Modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpViewTab {
    #[default]
    Overview,
    Servers,
    Marketplace,
}

impl McpViewTab {
    pub fn all() -> &'static [McpViewTab] {
        &[McpViewTab::Overview, McpViewTab::Servers, McpViewTab::Marketplace]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "1. Overview",
            Self::Servers => "2. Servers",
            Self::Marketplace => "3. Marketplace",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Overview => Self::Servers,
            Self::Servers => Self::Marketplace,
            Self::Marketplace => Self::Overview,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Overview => Self::Marketplace,
            Self::Servers => Self::Overview,
            Self::Marketplace => Self::Servers,
        }
    }
}

/// Action returned by McpModal on key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpModalAction {
    None,
    Close,
    Reload,
    TestServer(String),
    InstallMarketplace(String),
}

/// Interactive modal for MCP overview, servers list, and curated marketplace in the TUI composer.
pub struct McpModal {
    pub loaded_paths: Vec<PathBuf>,
    pub servers: Vec<ServerStatus>,
    pub active_tab: McpViewTab,
    pub selected_index: usize,
    pub status_message: Option<String>,
}

impl McpModal {
    pub fn new(loaded_paths: Vec<PathBuf>, servers: Vec<ServerStatus>, initial_tab: McpViewTab) -> Self {
        Self {
            loaded_paths,
            servers,
            active_tab: initial_tab,
            selected_index: 0,
            status_message: None,
        }
    }

    pub fn total_items(&self) -> usize {
        match self.active_tab {
            McpViewTab::Overview => 3,
            McpViewTab::Servers => self.servers.len().max(1),
            McpViewTab::Marketplace => flashagent_tools::mcp::get_marketplace().len(),
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, _modifiers: KeyModifiers) -> McpModalAction {
        match code {
            KeyCode::Esc => McpModalAction::Close,
            KeyCode::Tab => {
                self.active_tab = self.active_tab.next();
                self.selected_index = 0;
                McpModalAction::None
            }
            KeyCode::BackTab => {
                self.active_tab = self.active_tab.prev();
                self.selected_index = 0;
                McpModalAction::None
            }
            KeyCode::Char('1') => {
                self.active_tab = McpViewTab::Overview;
                self.selected_index = 0;
                McpModalAction::None
            }
            KeyCode::Char('2') => {
                self.active_tab = McpViewTab::Servers;
                self.selected_index = 0;
                McpModalAction::None
            }
            KeyCode::Char('3') => {
                self.active_tab = McpViewTab::Marketplace;
                self.selected_index = 0;
                McpModalAction::None
            }
            KeyCode::Up => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                } else {
                    self.selected_index = self.total_items().saturating_sub(1);
                }
                McpModalAction::None
            }
            KeyCode::Down => {
                let max = self.total_items().saturating_sub(1);
                if self.selected_index < max {
                    self.selected_index += 1;
                } else {
                    self.selected_index = 0;
                }
                McpModalAction::None
            }
            KeyCode::Enter => {
                match self.active_tab {
                    McpViewTab::Overview => match self.selected_index {
                        0 => {
                            self.active_tab = McpViewTab::Servers;
                            self.selected_index = 0;
                            McpModalAction::None
                        }
                        1 => {
                            self.active_tab = McpViewTab::Marketplace;
                            self.selected_index = 0;
                            McpModalAction::None
                        }
                        2 => McpModalAction::Reload,
                        _ => McpModalAction::None,
                    },
                    McpViewTab::Servers => {
                        if let Some(s) = self.servers.get(self.selected_index) {
                            McpModalAction::TestServer(s.name.clone())
                        } else {
                            McpModalAction::None
                        }
                    }
                    McpViewTab::Marketplace => {
                        let items = flashagent_tools::mcp::get_marketplace();
                        if let Some(item) = items.get(self.selected_index) {
                            McpModalAction::InstallMarketplace(item.id.to_string())
                        } else {
                            McpModalAction::None
                        }
                    }
                }
            }
            _ => McpModalAction::None,
        }
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let box_w = width.saturating_sub(6).clamp(52, 100);
        let inner_text_w = box_w.saturating_sub(2);
        let border_color = BORDER_GOLD;
        let reset = RESET;

        let pad_row = |content: &str| -> String {
            let clipped = if visible_width(content) > inner_text_w {
                clip_ansi(content, inner_text_w)
            } else {
                content.to_string()
            };
            let clipped_vis = visible_width(&clipped);
            let pad = " ".repeat(inner_text_w.saturating_sub(clipped_vis));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        // Header
        let title_styled = " \x1b[1;38;2;225;175;95mModel Context Protocol (MCP)\x1b[0m \x1b[38;2;160;155;145m(Tab 1-3 to switch)\x1b[0m ";
        let title_vis = visible_width(title_styled);
        let dashes = box_w.saturating_sub(title_vis + 1);
        lines.push((
            LineKind::System,
            format!("  {border_color}╭─{title_styled}{}╮{reset}", "─".repeat(dashes)),
        ));

        // Tab bar
        let mut tabs_line = String::from(" ");
        for tab in McpViewTab::all() {
            let is_cur = *tab == self.active_tab;
            let tab_lbl = tab.label();
            if is_cur {
                tabs_line.push_str(&format!("\x1b[1;38;2;225;175;95m[{tab_lbl}]\x1b[0m  "));
            } else {
                tabs_line.push_str(&format!("\x1b[38;2;135;130;125m {tab_lbl} \x1b[0m  "));
            }
        }
        lines.push((LineKind::System, pad_row(&tabs_line)));
        lines.push((
            LineKind::System,
            format!("  {border_color}├{}┤{reset}", "─".repeat(box_w)),
        ));

        match self.active_tab {
            McpViewTab::Overview => {
                lines.push((LineKind::System, pad_row(&format!("{TEXT_MUTED}Connect local tool providers and external servers via JSON-RPC 2.0{RESET}"))));
                lines.push((LineKind::System, pad_row("")));

                let active_count = self.servers.iter().filter(|s| s.state == ServerConnectionState::Active).count();
                let total_tools: usize = self.servers.iter().map(|s| s.tool_count).sum();
                let stats = format!(
                    "{TEXT_BRIGHT}Configured:{RESET} {}  ·  {TEXT_GREEN}Connected:{RESET} {}  ·  {TEXT_CYAN}Discovered Tools:{RESET} {}",
                    self.servers.len(),
                    active_count,
                    total_tools
                );
                lines.push((LineKind::System, pad_row(&stats)));

                let paths_display = if self.loaded_paths.is_empty() {
                    "None found (Checked .mcp.json, ~/.flashagent/mcp.json)".to_string()
                } else {
                    self.loaded_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
                };
                lines.push((LineKind::System, pad_row(&format!("{TEXT_MUTED}Config files: {paths_display}{RESET}"))));
                lines.push((LineKind::System, pad_row("")));

                let actions = [
                    ("Browse Configured Servers", "View tools, status, ping latency"),
                    ("Curated Extensions Marketplace", "Explore SQLite, GitHub, Postgres..."),
                    ("Reload MCP Configurations", "Restart child processes and re-discover tools"),
                ];
                for (idx, (action, desc)) in actions.iter().enumerate() {
                    let is_sel = idx == self.selected_index;
                    let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                    let row = if is_sel {
                        format!("{ptr} \x1b[1;38;2;240;235;225m{:<32}\x1b[0m \x1b[1;38;2;225;175;95m{desc}\x1b[0m", action)
                    } else {
                        format!("{ptr} \x1b[38;2;160;155;145m{:<32}\x1b[0m \x1b[38;2;135;130;125m{desc}\x1b[0m", action)
                    };
                    lines.push((LineKind::System, pad_row(&row)));
                }
            }
            McpViewTab::Servers => {
                if self.servers.is_empty() {
                    lines.push((LineKind::System, pad_row(&format!("{TEXT_MUTED}No MCP servers configured yet in .mcp.json or ~/.flashagent/mcp.json{RESET}"))));
                    lines.push((LineKind::System, pad_row("")));
                    lines.push((LineKind::System, pad_row(&format!("{TEXT_CYAN}Press Tab or '3' to browse curated marketplace extensions{RESET}"))));
                } else {
                    for (idx, s) in self.servers.iter().enumerate() {
                        let is_sel = idx == self.selected_index;
                        let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                        let st_badge = match &s.state {
                            ServerConnectionState::Active => "\x1b[38;2;135;220;145m● active\x1b[0m",
                            ServerConnectionState::Disabled => "\x1b[38;2;135;130;125m○ disabled\x1b[0m",
                            ServerConnectionState::Stopped => "\x1b[38;2;225;175;95m○ stopped\x1b[0m",
                            ServerConnectionState::Error(_) => "\x1b[38;2;245;120;120m✕ error\x1b[0m",
                        };
                        let row = if is_sel {
                            format!("{ptr} \x1b[1;38;2;240;235;225m{:<18}\x1b[0m {st_badge}  \x1b[1;38;2;225;175;95m{} tool(s)\x1b[0m", s.name, s.tool_count)
                        } else {
                            format!("{ptr} \x1b[38;2;160;155;145m{:<18}\x1b[0m {st_badge}  \x1b[38;2;135;130;125m{} tool(s)\x1b[0m", s.name, s.tool_count)
                        };
                        lines.push((LineKind::System, pad_row(&row)));
                    }
                }
            }
            McpViewTab::Marketplace => {
                let items = flashagent_tools::mcp::get_marketplace();
                for (idx, item) in items.iter().enumerate() {
                    let is_sel = idx == self.selected_index;
                    let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                    let desc = crate::truncate_middle(item.description, inner_text_w.saturating_sub(22));
                    let row = if is_sel {
                        format!("{ptr} \x1b[1;38;2;240;235;225m{:<14}\x1b[0m \x1b[1;38;2;225;175;95m{desc}\x1b[0m", item.id)
                    } else {
                        format!("{ptr} \x1b[38;2;110;175;230m{:<14}\x1b[0m \x1b[38;2;160;155;145m{desc}\x1b[0m", item.id)
                    };
                    lines.push((LineKind::System, pad_row(&row)));
                }
            }
        }

        if let Some(ref msg) = self.status_message {
            lines.push((LineKind::System, pad_row(&format!("\x1b[38;2;225;175;95m{msg}\x1b[0m"))));
        } else {
            lines.push((LineKind::System, pad_row("")));
        }

        lines.push((LineKind::System, pad_row("\x1b[38;2;135;130;125mTab/1-3 switch tab · ↑/↓ navigate · Enter select · Esc close\x1b[0m")));

        lines.push((
            LineKind::System,
            format!("  {border_color}╰{}╯{reset}", "─".repeat(box_w)),
        ));

        lines
    }
}

/// Format a single boxed line with padding.
fn box_line(content: &str, inner_w: usize, border: &str) -> String {
    let clipped = if visible_width(content) > inner_w {
        clip_ansi(content, inner_w)
    } else {
        content.to_string()
    };
    let vis_len = visible_width(&clipped);
    let pad_len = inner_w.saturating_sub(vis_len);
    format!("{border}│{RESET} {clipped}{RESET}{} {border}│{RESET}", " ".repeat(pad_len))
}

/// Render the top rounded border of a card.
fn box_top(title: &str, inner_w: usize, border: &str) -> String {
    let t_vis = visible_width(title);
    let dashes = inner_w.saturating_sub(t_vis + 1);
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
                ServerConnectionState::Stopped => (TEXT_MUTED, "Stopped (starts on first use)".to_string()),
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

    #[test]
    fn test_mcp_modal_borders_closed_and_consistent() {
        for tab in McpViewTab::all() {
            let modal = McpModal::new(
                vec![PathBuf::from("/test/.mcp.json")],
                vec![ServerStatus {
                    name: "test-server".into(),
                    command: "uvx mcp-test".into(),
                    state: ServerConnectionState::Active,
                    tool_count: 5,
                    tool_names: vec!["query".into()],
                    read_only: true,
                    description: None,
                }],
                *tab,
            );

            for width in [60, 80, 100, 120] {
                let rendered = modal.render(width);
                assert!(!rendered.is_empty());
                let expected_w = visible_width(&rendered[0].1);
                assert!(expected_w <= width, "Modal width {expected_w} exceeds terminal width {width}");

                for (idx, line) in rendered.iter().enumerate() {
                    let row_w = visible_width(&line.1);
                    assert_eq!(
                        row_w, expected_w,
                        "Tab {:?}: Row {idx} width {row_w} does not match expected {expected_w} at width {width}",
                        tab
                    );
                    let clean = &line.1;
                    assert!(
                        clean.ends_with("╮\x1b[0m")
                            || clean.ends_with("┤\x1b[0m")
                            || clean.ends_with("│\x1b[0m")
                            || clean.ends_with("╯\x1b[0m"),
                        "Tab {:?}: Row {idx} missing closed right border: {clean}",
                        tab
                    );
                }
            }
        }
    }
}

