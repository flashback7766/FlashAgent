//! MCP cards and the `/mcp` modal.

use std::path::{Path, PathBuf};
use crossterm::event::{KeyCode, KeyModifiers};
use flashagent_tools::mcp::{MarketplaceItem, McpTestReport, ServerConnectionState, ServerStatus};
use crate::tool_views::clip_ellipsis;
use crate::{plural, visible_width, LineKind, RenderLine};

const RESET: &str = "\x1b[0m";
const BORDER_GOLD: &str = "\x1b[38;2;225;175;95m";
const TEXT_BRIGHT: &str = "\x1b[1;38;2;245;240;235m";
const TEXT_MUTED: &str = "\x1b[38;2;145;150;160m";
const TEXT_GREEN: &str = "\x1b[38;2;135;220;145m";
const TEXT_YELLOW: &str = "\x1b[38;2;240;210;115m";
const TEXT_CYAN: &str = "\x1b[38;2;110;175;230m";

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
            Self::Overview => "1 Overview",
            Self::Servers => "2 Servers",
            Self::Marketplace => "3 Marketplace",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpModalAction {
    None,
    Close,
    Reload,
    TestServer(String),
    InstallMarketplace(String),
}

/// Overview, servers and the curated marketplace.
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
        let box_w = width.saturating_sub(6).min(100);
        let inner_text_w = box_w.saturating_sub(2);
        let border_color = BORDER_GOLD;
        let reset = RESET;

        let pad_row = |content: &str| -> String {
            let clipped = clip_ellipsis(content, inner_text_w);
            let clipped_vis = visible_width(&clipped);
            let pad = " ".repeat(inner_text_w.saturating_sub(clipped_vis));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        // The heading shrinks first; a sheared box is worse than a shorter name.
        let title = [" Model Context Protocol (MCP) ", " MCP "]
            .into_iter()
            .find(|t| visible_width(t) < box_w.saturating_sub(1))
            .unwrap_or(" MCP ");
        let title_styled = format!("\x1b[1;38;2;225;175;95m{title}\x1b[0m");
        let dashes = box_w.saturating_sub(visible_width(title) + 1);
        lines.push((
            LineKind::System,
            format!("  {border_color}╭─{title_styled}{border_color}{}╮{reset}", "─".repeat(dashes)),
        ));

        // Tighter as the window narrows; at the narrowest the other tabs are their numbers.
        let tabs_line = |gap: usize, numbers_only: bool| -> String {
            let tabs: Vec<String> = McpViewTab::all()
                .iter()
                .map(|tab| {
                    let tab_lbl = tab.label();
                    if *tab == self.active_tab {
                        format!("\x1b[1;38;2;225;175;95m[{tab_lbl}]\x1b[0m")
                    } else {
                        let shown = if numbers_only { tab_lbl.split(' ').next().unwrap_or(tab_lbl) } else { tab_lbl };
                        format!("\x1b[38;2;135;130;125m {shown} \x1b[0m")
                    }
                })
                .collect();
            format!(" {}", tabs.join(&" ".repeat(gap)))
        };
        let tabs = [(2, false), (1, false), (0, false)]
            .into_iter()
            .map(|(gap, numbers_only)| tabs_line(gap, numbers_only))
            .find(|line| visible_width(line) <= inner_text_w)
            .unwrap_or_else(|| tabs_line(0, true));
        lines.push((LineKind::System, pad_row(&tabs)));
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
                    "{TEXT_BRIGHT}Configured:{RESET} {}  ·  {TEXT_GREEN}Connected:{RESET} {}  ·  {TEXT_CYAN}Tools found:{RESET} {}",
                    self.servers.len(),
                    active_count,
                    total_tools
                );
                lines.push((LineKind::System, pad_row(&stats)));

                let paths_display = if self.loaded_paths.is_empty() {
                    "none (looked for .mcp.json and ~/.flashagent/mcp.json)".to_string()
                } else {
                    self.loaded_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
                };
                lines.push((LineKind::System, pad_row(&format!("{TEXT_MUTED}Config files: {paths_display}{RESET}"))));
                lines.push((LineKind::System, pad_row("")));

                let actions = [
                    ("Browse configured servers", "Tools, status and ping time"),
                    ("Curated extensions marketplace", "SQLite, GitHub, Postgres and more"),
                    ("Reload MCP configurations", "Restart child processes and rediscover tools"),
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
                    lines.push((LineKind::System, pad_row(&format!("{TEXT_CYAN}Press Tab or 3 to browse the marketplace{RESET}"))));
                } else {
                    for (idx, s) in self.servers.iter().enumerate() {
                        let is_sel = idx == self.selected_index;
                        let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                        let st_badge = match &s.state {
                            ServerConnectionState::Active => "\x1b[38;2;135;220;145m● active\x1b[0m",
                            ServerConnectionState::Disabled => "\x1b[38;2;135;130;125m○ disabled\x1b[0m",
                            ServerConnectionState::Stopped => "\x1b[38;2;225;175;95m○ stopped\x1b[0m",
                            ServerConnectionState::Error(_) => "\x1b[38;2;245;120;120m× error\x1b[0m",
                        };
                        let tools = plural(s.tool_count, "tool", "tools");
                        let row = if is_sel {
                            format!("{ptr} \x1b[1;38;2;240;235;225m{:<18}\x1b[0m {st_badge}  \x1b[1;38;2;225;175;95m{tools}\x1b[0m", s.name)
                        } else {
                            format!("{ptr} \x1b[38;2;160;155;145m{:<18}\x1b[0m {st_badge}  \x1b[38;2;135;130;125m{tools}\x1b[0m", s.name)
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

        let hints = crate::key_hints(
            &[("Tab/1-3", "switch tab"), ("↑/↓", "move"), ("Enter", "select"), ("Esc", "close")],
            inner_text_w.saturating_sub(1),
        );
        lines.push((LineKind::System, pad_row(&hints)));

        lines.push((
            LineKind::System,
            format!("  {border_color}╰{}╯{reset}", "─".repeat(box_w)),
        ));

        lines
    }
}

fn box_line(content: &str, inner_w: usize, border: &str) -> String {
    let clipped = clip_ellipsis(content, inner_w);
    let vis_len = visible_width(&clipped);
    let pad_len = inner_w.saturating_sub(vis_len);
    format!("{border}│{RESET} {clipped}{RESET}{} {border}│{RESET}", " ".repeat(pad_len))
}

fn box_top(title: &str, inner_w: usize, border: &str) -> String {
    let title = clip_ellipsis(title, inner_w.saturating_sub(1));
    let t_vis = visible_width(&title);
    let dashes = inner_w.saturating_sub(t_vis + 1);
    format!("{border}╭─ {TEXT_BRIGHT}{title}{RESET}{border} {}╮{RESET}", "─".repeat(dashes))
}

fn box_bottom(inner_w: usize, border: &str) -> String {
    format!("{border}╰{}╯{RESET}", "─".repeat(inner_w + 2))
}

/// Outer width of the report cards.
fn report_width(width: usize) -> usize {
    width.saturating_sub(4).clamp(20, 110)
}

/// `/mcp test <server>`
pub fn render_mcp_test_report(report: &McpTestReport, width: usize) -> Vec<String> {
    let inner_w = report_width(width).saturating_sub(4);
    let mut lines = Vec::new();

    let title = format!("MCP test report: {}", report.name);
    lines.push(box_top(&title, inner_w, BORDER_GOLD));

    let mut status = format!("{TEXT_GREEN}● Handshake successful{RESET}");
    // Only what the server said: a made-up version reads as a fact.
    if let Some(ver) = report.server_version.as_deref().filter(|v| !v.trim().is_empty()) {
        status.push_str(&format!("  ·  {TEXT_MUTED}Version:{RESET} {ver}"));
    }
    status.push_str(&format!(
        "  ·  {TEXT_CYAN}Latency:{RESET} {:.2}ms",
        report.latency.as_secs_f64() * 1000.0
    ));
    lines.push(box_line(&status, inner_w, BORDER_GOLD));
    lines.push(box_line(&format!("{TEXT_MUTED}Command: {}{RESET}", report.command), inner_w, BORDER_GOLD));
    lines.push(box_line("", inner_w, BORDER_GOLD));

    lines.push(box_line(
        &format!("{TEXT_BRIGHT}Discovered tools ({}):{RESET}", report.tools.len()),
        inner_w,
        BORDER_GOLD,
    ));

    for (name, desc, read_only) in &report.tools {
        let tag = if *read_only {
            format!("{TEXT_GREEN}[Read-only: auto-approved]{RESET}")
        } else {
            format!("{TEXT_YELLOW}[Asks for approval]{RESET}")
        };
        let tool_head = format!("  • {TEXT_CYAN}{name}{RESET}  {tag}");
        lines.push(box_line(&tool_head, inner_w, BORDER_GOLD));

        if let Some(d) = desc {
            let clipped = clip_ellipsis(d, inner_w.saturating_sub(4));
            lines.push(box_line(&format!("    {TEXT_MUTED}{clipped}{RESET}"), inner_w, BORDER_GOLD));
        }
    }

    lines.push(box_bottom(inner_w, BORDER_GOLD));
    lines
}

/// How to set `var` in the shell the user starts FlashAgent from.
fn set_env_line(var: &str) -> String {
    if cfg!(windows) {
        format!("$env:{var} = \"your-key-here\"")
    } else {
        format!("export {var}=\"your-key-here\"")
    }
}

/// `/mcp add`
pub fn render_mcp_add_success(item: &MarketplaceItem, target_path: &Path, width: usize) -> Vec<String> {
    let inner_w = report_width(width).saturating_sub(4);
    let mut lines = Vec::new();

    let title = format!("Added MCP extension: {}", item.name);
    lines.push(box_top(&title, inner_w, BORDER_GOLD));

    lines.push(box_line(
        &format!("{TEXT_GREEN}√ Scaffolding saved to {}{RESET}", target_path.display()),
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
            lines.push(box_line(&format!("  {}", set_env_line(v)), inner_w, BORDER_GOLD));
        }
    }

    lines.push(box_line("", inner_w, BORDER_GOLD));
    lines.push(box_line(
        &format!("Run {TEXT_CYAN}/mcp test {}{RESET} to check the connection.", item.id),
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
        assert!(text.contains("Read-only: auto-approved"));
        assert!(text.contains("Version:\x1b[0m 1.0.0"));
    }

    #[test]
    fn a_version_the_server_did_not_report_is_not_made_up() {
        let report = McpTestReport {
            name: "sqlite".to_string(),
            command: "uvx mcp-server-sqlite".to_string(),
            latency: Duration::from_millis(4),
            tools: Vec::new(),
            server_version: None,
        };
        let text = crate::strip_ansi(&render_mcp_test_report(&report, 80).join("\n"));
        assert!(!text.contains("Version") && text.contains("Latency: 4.00ms"), "{text}");
        assert!(text.contains("Discovered tools (0)"), "{text}");
    }

    #[test]
    fn the_env_line_is_in_the_syntax_of_the_platforms_shell() {
        let line = set_env_line("GITHUB_TOKEN");
        if cfg!(windows) {
            assert_eq!(line, "$env:GITHUB_TOKEN = \"your-key-here\"");
        } else {
            assert_eq!(line, "export GITHUB_TOKEN=\"your-key-here\"");
        }
    }

    #[test]
    fn a_server_with_one_tool_says_tool() {
        let server = |tool_count| ServerStatus {
            name: "one".into(),
            command: "uvx one".into(),
            state: ServerConnectionState::Active,
            tool_count,
            tool_names: Vec::new(),
            read_only: true,
            description: None,
        };
        let modal = McpModal::new(Vec::new(), vec![server(1), server(3)], McpViewTab::Servers);
        let text = crate::strip_ansi(&modal.render(80).iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n"));
        assert!(text.contains("1 tool ") && text.contains("3 tools") && !text.contains("(s)"), "{text}");
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

            for width in [44, 52, 60, 80, 100, 120] {
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

