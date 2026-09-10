//! Autocomplete popup for slash commands and skills in the input box.
//! Triggered whenever the user begins typing `/` in the prompt input.

use std::path::Path;
use crate::{visible_width, LineKind, RenderLine};

/// Category of suggestion item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutocompleteCategory {
    Command,
    Skill,
}

/// Single autocomplete suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    /// Command or skill invocation, e.g. "/help", "/expand", "/skill:rust-core".
    pub trigger: String,
    /// Short summary shown in the suggestion row.
    pub description: String,
    /// Category badge ([cmd] vs [skill]).
    pub category: AutocompleteCategory,
}

impl AutocompleteItem {
    pub fn new(
        trigger: impl Into<String>,
        description: impl Into<String>,
        category: AutocompleteCategory,
    ) -> Self {
        Self {
            trigger: trigger.into(),
            description: description.into(),
            category,
        }
    }
}

/// Built-in slash commands.
pub fn builtin_commands() -> Vec<AutocompleteItem> {
    vec![
        AutocompleteItem::new("/help", "Show command reference and keybindings", AutocompleteCategory::Command),
        AutocompleteItem::new("/settings", "Open interactive settings tab (or press Tab on empty prompt)", AutocompleteCategory::Command),
        AutocompleteItem::new("/sampling", "Open sampling parameters menu (or press F5)", AutocompleteCategory::Command),
        AutocompleteItem::new("/context", "Show detailed context window token breakdown", AutocompleteCategory::Command),
        AutocompleteItem::new("/clear", "Clear terminal screen and conversation scrollback", AutocompleteCategory::Command),
        AutocompleteItem::new("/effort", "Select thinking effort preset (off, low, medium, high)", AutocompleteCategory::Command),
        AutocompleteItem::new("/model", "Open model selection menu or switch model", AutocompleteCategory::Command),
        AutocompleteItem::new("/verbose", "Toggle verbose mode for thoughts and tool calls (all, last, off)", AutocompleteCategory::Command),
        AutocompleteItem::new("/expand", "Toggle verbose mode for thoughts and tool calls (all, last, off)", AutocompleteCategory::Command),
        AutocompleteItem::new("/mode", "Cycle permission mode (Manual, Auto, Planning, Bypass)", AutocompleteCategory::Command),
        AutocompleteItem::new("/goal", "Run autonomous task until goal is fully completed", AutocompleteCategory::Command),
        AutocompleteItem::new("/mcp", "List and manage Model Context Protocol servers", AutocompleteCategory::Command),
        AutocompleteItem::new("/compact", "Compact conversation context (optional: /compact <focus instructions>)", AutocompleteCategory::Command),
        AutocompleteItem::new("/regenerate", "Regenerate the last assistant response from scratch (or press Ctrl+R)", AutocompleteCategory::Command),
        AutocompleteItem::new("/retry", "Retry and regenerate the last assistant response from scratch (Ctrl+R)", AutocompleteCategory::Command),
        AutocompleteItem::new("/update", "Check and apply FlashAgent updates in-place", AutocompleteCategory::Command),
        AutocompleteItem::new("/channel", "Switch release channel: /channel <stable|beta>", AutocompleteCategory::Command),
        AutocompleteItem::new("/diff", "Preview unstaged git changes and diff stat", AutocompleteCategory::Command),
        AutocompleteItem::new("/commit", "Review changes and commit with /commit <message>", AutocompleteCategory::Command),
        AutocompleteItem::new("/editor", "Open external editor (nano/vim/code) to craft prompt", AutocompleteCategory::Command),
        AutocompleteItem::new("/export", "Export chat session to markdown, HTML, or JSONL", AutocompleteCategory::Command),
    ]
}

/// Sub-commands for commands that take specific options.
pub fn sub_commands(input: &str) -> Option<Vec<AutocompleteItem>> {
    let lower = input.to_lowercase();
    if lower.starts_with("/expand ") || lower == "/expand" || lower.starts_with("/verbose ") || lower == "/verbose" {
        Some(vec![
            AutocompleteItem::new("/verbose all", "Expand both thoughts and tool calls permanently", AutocompleteCategory::Command),
            AutocompleteItem::new("/verbose last", "Expand thoughts and tool calls for latest turn", AutocompleteCategory::Command),
            AutocompleteItem::new("/verbose off", "Collapse thoughts and tool calls to concise summary", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/effort ") || lower == "/effort" {
        Some(vec![
            AutocompleteItem::new("/effort default", "Use API server default preset", AutocompleteCategory::Command),
            AutocompleteItem::new("/effort off", "Disable model internal reasoning", AutocompleteCategory::Command),
            AutocompleteItem::new("/effort low", "Fast response, low reasoning budget", AutocompleteCategory::Command),
            AutocompleteItem::new("/effort medium", "Balanced reasoning depth and speed", AutocompleteCategory::Command),
            AutocompleteItem::new("/effort high", "Full reasoning exploration", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/mode ") || lower == "/mode" {
        Some(vec![
            AutocompleteItem::new("/mode manual", "Require confirmation for file writes and shell", AutocompleteCategory::Command),
            AutocompleteItem::new("/mode edits", "Auto-approve file edits, ask on shell commands", AutocompleteCategory::Command),
            AutocompleteItem::new("/mode planning", "Read-only planning mode (modifications blocked)", AutocompleteCategory::Command),
            AutocompleteItem::new("/mode bypass", "Bypass all approval gates (full autonomy)", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/mcp ") || lower == "/mcp" {
        Some(vec![
            AutocompleteItem::new("/mcp list", "List configured and connected MCP servers", AutocompleteCategory::Command),
            AutocompleteItem::new("/mcp market", "Browse vetted MCP marketplace extensions", AutocompleteCategory::Command),
            AutocompleteItem::new("/mcp test", "Test connection and discover tools for an MCP server", AutocompleteCategory::Command),
            AutocompleteItem::new("/mcp add", "Add and scaffold a curated MCP extension from marketplace", AutocompleteCategory::Command),
            AutocompleteItem::new("/mcp reload", "Reload MCP configurations and restart active servers", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/channel ") || lower == "/channel" {
        Some(vec![
            AutocompleteItem::new("/channel beta", "Switch to Beta channel (latest features & pre-releases)", AutocompleteCategory::Command),
            AutocompleteItem::new("/channel stable", "Switch to Stable channel (official releases)", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/compact ") || lower == "/compact" {
        Some(vec![
            AutocompleteItem::new("/compact", "Auto-compact prior context while keeping current state", AutocompleteCategory::Command),
            AutocompleteItem::new("/compact keep code details", "Compact context prioritizing code and architecture details", AutocompleteCategory::Command),
            AutocompleteItem::new("/compact keep task goals", "Compact context prioritizing remaining tasks and roadmap", AutocompleteCategory::Command),
        ])
    } else {
        None
    }
}

/// Load custom skills from `.agents/skills/*.md` and `~/.flashagent/skills/*.md`.
pub fn load_skills(cwd: &Path) -> Vec<AutocompleteItem> {
    let mut skills = Vec::new();
    let mut scanned_dirs = vec![cwd.join(".agents").join("skills")];
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        scanned_dirs.push(std::path::PathBuf::from(home).join(".flashagent").join("skills"));
    }

    for dir in scanned_dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            // Extract trigger/description from content
            let desc = if let Ok(content) = std::fs::read_to_string(&path) {
                let mut found_desc = None;
                for line in content.lines() {
                    let trimmed = line.trim();
                    if let Some(rest) = trimmed.strip_prefix("Trigger:") {
                        found_desc = Some(rest.trim().to_string());
                        break;
                    }
                }
                found_desc.unwrap_or_else(|| format!("Skill defined in {}", path.file_name().unwrap_or_default().to_string_lossy()))
            } else {
                format!("Skill {stem}")
            };

            // Register both /skill:<name> and /<name>
            skills.push(AutocompleteItem::new(
                format!("/skill:{stem}"),
                desc.clone(),
                AutocompleteCategory::Skill,
            ));
            skills.push(AutocompleteItem::new(
                format!("/{stem}"),
                desc,
                AutocompleteCategory::Skill,
            ));
        }
    }

    // Default fallback skills if none found on disk
    if skills.is_empty() {
        skills.push(AutocompleteItem::new("/skill:rust-core", "Build, test, or clippy verification of workspace", AutocompleteCategory::Skill));
        skills.push(AutocompleteItem::new("/rust-core", "Build, test, or clippy verification of workspace", AutocompleteCategory::Skill));
        skills.push(AutocompleteItem::new("/skill:ui-render", "UI text rendering and golden frame tests", AutocompleteCategory::Skill));
        skills.push(AutocompleteItem::new("/ui-render", "UI text rendering and golden frame tests", AutocompleteCategory::Skill));
        skills.push(AutocompleteItem::new("/skill:mcp", "MCP server configuration and protocol test", AutocompleteCategory::Skill));
        skills.push(AutocompleteItem::new("/mcp-skill", "MCP server configuration and protocol test", AutocompleteCategory::Skill));
        skills.push(AutocompleteItem::new("/skill:release", "Release checklist and audit verification", AutocompleteCategory::Skill));
        skills.push(AutocompleteItem::new("/release", "Release checklist and audit verification", AutocompleteCategory::Skill));
    }

    skills
}

/// Find all matching items for the given prompt input.
pub fn find_matches(input: &str, cwd: &Path) -> Vec<AutocompleteItem> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }

    // Check for specific sub-command contexts first
    if trimmed.contains(' ') {
        if let Some(subs) = sub_commands(trimmed) {
            let lower = trimmed.to_lowercase();
            return subs
                .into_iter()
                .filter(|s| s.trigger.to_lowercase().starts_with(&lower) || lower.starts_with(&s.trigger.to_lowercase()))
                .collect();
        }
    }

    let mut pool = builtin_commands();
    pool.extend(load_skills(cwd));

    let query = trimmed.to_lowercase();
    let sub_query = query.trim_start_matches('/');

    let mut prefix_matches = Vec::new();
    let mut substr_matches = Vec::new();
    let mut desc_matches = Vec::new();

    for item in pool {
        let trig_lower = item.trigger.to_lowercase();
        let desc_lower = item.description.to_lowercase();

        if trig_lower.starts_with(&query) {
            prefix_matches.push(item);
        } else if !sub_query.is_empty() && trig_lower.contains(sub_query) {
            substr_matches.push(item);
        } else if !sub_query.is_empty() && desc_lower.contains(sub_query) {
            desc_matches.push(item);
        }
    }

    prefix_matches.extend(substr_matches);
    prefix_matches.extend(desc_matches);
    prefix_matches
}

/// Active autocomplete popup model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompletePopup {
    pub items: Vec<AutocompleteItem>,
    pub selected: usize,
}

impl AutocompletePopup {
    /// Create popup for input if input starts with `/` and has matches.
    pub fn for_input(input: &str, cwd: &Path, prev_selected: usize) -> Option<Self> {
        let items = find_matches(input, cwd);
        if items.is_empty() {
            None
        } else {
            let selected = if items.is_empty() {
                0
            } else {
                prev_selected.min(items.len() - 1)
            };
            Some(Self { items, selected })
        }
    }

    /// Select next suggestion (wraps around).
    pub fn next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    /// Select previous suggestion (wraps around).
    pub fn prev(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + self.items.len() - 1) % self.items.len();
        }
    }

    /// Current selected item.
    pub fn current(&self) -> Option<&AutocompleteItem> {
        self.items.get(self.selected)
    }

    /// Complete current input with the selected item.
    pub fn complete_input(&self, current: &str) -> String {
        if let Some(item) = self.current() {
            if current == item.trigger || current == format!("{} ", item.trigger) {
                // If already completed, cycle to next item on Tab
                let next_idx = (self.selected + 1) % self.items.len();
                format!("{} ", self.items[next_idx].trigger)
            } else {
                format!("{} ", item.trigger)
            }
        } else {
            current.to_string()
        }
    }

    /// Render autocomplete card under the input box.
    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let inner_w = width.saturating_sub(2);
        let border_color = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";

        let title = " Suggestions (tab: complete · ↑/↓: select) ";
        let top_dashes = inner_w.saturating_sub(title.chars().count() + 1);
        lines.push((
            LineKind::System,
            format!("{border_color}┌─\x1b[38;2;180;175;165m{title}{border_color}{}┐{reset}", "─".repeat(top_dashes)),
        ));

        // Max 5 items visible at once
        const MAX_VISIBLE: usize = 5;
        let total = self.items.len();
        let start_idx = if total <= MAX_VISIBLE || self.selected < MAX_VISIBLE / 2 {
            0
        } else if self.selected + (MAX_VISIBLE / 2) >= total {
            total - MAX_VISIBLE
        } else {
            self.selected - (MAX_VISIBLE / 2)
        };
        let end_idx = (start_idx + MAX_VISIBLE).min(total);

        for (i, item) in self.items[start_idx..end_idx].iter().enumerate() {
            let actual_idx = start_idx + i;
            let is_sel = actual_idx == self.selected;

            let pointer = if is_sel {
                "\x1b[1;38;2;225;175;95m▸\x1b[0m"
            } else {
                " "
            };

            let (badge_style, badge_text) = match item.category {
                AutocompleteCategory::Command => ("\x1b[38;2;145;205;140m", "[cmd]"),
                AutocompleteCategory::Skill => ("\x1b[38;2;175;170;225m", "[skill]"),
            };

            let cmd_styled = if is_sel {
                format!("\x1b[1;38;2;240;235;225m{:<18}\x1b[0m", item.trigger)
            } else {
                format!("\x1b[38;2;180;175;165m{:<18}\x1b[0m", item.trigger)
            };

            // Calculate budget for description
            // 2 (left border + space) + 1 (pointer) + 1 (space) + 18 (trigger) + 1 (space) + badge + 2 (right space + border)
            let badge_w = badge_text.chars().count();
            let fixed_w = 2 + 1 + 1 + 18 + 1 + badge_w + 2;
            let desc_budget = inner_w.saturating_sub(fixed_w);

            let desc_clipped: String = item.description.chars().take(desc_budget).collect();
            let desc_styled = if is_sel {
                format!("\x1b[38;2;215;210;200m{desc_clipped}\x1b[0m")
            } else {
                format!("\x1b[38;2;135;130;125m{desc_clipped}\x1b[0m")
            };

            let row_raw = format!(" {pointer} {cmd_styled} {desc_styled}");
            let vis_len = visible_width(&row_raw) + badge_w;
            let pad = " ".repeat(inner_w.saturating_sub(vis_len));
            lines.push((
                LineKind::System,
                format!("{border_color}│{reset}{row_raw}{pad}{badge_style}{badge_text}{reset}{border_color}│{reset}"),
            ));
        }

        // Bottom border with indicator of more items if truncated
        let bot_hint = if total > MAX_VISIBLE {
            format!(" ({}/{} items) ", self.selected + 1, total)
        } else {
            String::new()
        };
        let bot_dashes = inner_w.saturating_sub(bot_hint.chars().count());
        lines.push((
            LineKind::System,
            format!("{border_color}└{}\x1b[38;2;130;125;120m{bot_hint}{border_color}┘{reset}", "─".repeat(bot_dashes)),
        ));

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_commands_exist() {
        let cmds = builtin_commands();
        assert!(cmds.iter().any(|c| c.trigger == "/help"));
        assert!(cmds.iter().any(|c| c.trigger == "/expand"));
        assert!(cmds.iter().any(|c| c.trigger == "/effort"));
    }

    #[test]
    fn test_find_matches_prefix_filtering() {
        let temp = std::env::temp_dir();
        let matches = find_matches("/ex", &temp);
        assert!(!matches.is_empty());
        assert_eq!(matches[0].trigger, "/expand");
    }

    #[test]
    fn test_autocomplete_popup_navigation() {
        let temp = std::env::temp_dir();
        let mut popup = AutocompletePopup::for_input("/e", &temp, 0).expect("should find matches for /e");
        assert!(popup.items.len() >= 2);
        let first = popup.current().unwrap().trigger.clone();
        popup.next();
        let second = popup.current().unwrap().trigger.clone();
        assert_ne!(first, second);
        popup.prev();
        assert_eq!(popup.current().unwrap().trigger, first);
    }

    #[test]
    fn test_autocomplete_render_lines() {
        let temp = std::env::temp_dir();
        let popup = AutocompletePopup::for_input("/", &temp, 0).expect("should find matches for /");
        let rendered = popup.render(80);
        assert!(rendered.len() >= 3); // top border + items + bottom border
        assert!(rendered[0].1.contains("Suggestions"));
    }

    #[test]
    fn test_tab_completion_formatting() {
        let temp = std::env::temp_dir();
        let popup = AutocompletePopup::for_input("/h", &temp, 0).expect("matches");
        let completed = popup.complete_input("/h");
        assert_eq!(completed, "/help ");
    }
}
