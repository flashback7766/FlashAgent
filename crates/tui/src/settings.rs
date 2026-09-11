//! Interactive Settings Wizard opened via `/settings` or `Tab` on empty input.
//! Multi-tab layout for configuring backend, models, updates, aesthetics, reasoning, and tools.

use flashagent_core::{AppConfig, BackendPreset, SamplingPreset};
use crossterm::event::{KeyCode, KeyModifiers};
use crate::{LineKind, RenderLine};

/// Action returned by the settings view on key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsAction {
    /// Keep settings open.
    None,
    /// Close settings and return to chat.
    Close,
    /// Request model list discovery from backend.
    DiscoverModels,
    /// Trigger tool-calling probe test.
    RunToolTest,
    /// Open the dedicated F3 model selection menu.
    OpenModelMenu,
    /// Open the dedicated F4 thinking effort menu.
    OpenEffortMenu,
    /// Open the setup wizard for full backend configuration.
    OpenWizard,
    /// Open the dedicated F5 sampling parameters menu.
    OpenSamplingMenu,
    /// Trigger manual update check.
    CheckUpdatesNow,
    /// Open MCP overview.
    OpenMcpMenu,
}

/// Tabs for categorizing settings in the Settings Wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Updates,
    Aesthetics,
    Reasoning,
    Tools,
}

impl SettingsTab {
    pub fn all() -> &'static [SettingsTab] {
        &[
            SettingsTab::General,
            SettingsTab::Updates,
            SettingsTab::Aesthetics,
            SettingsTab::Reasoning,
            SettingsTab::Tools,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "1. General",
            Self::Updates => "2. Updates",
            Self::Aesthetics => "3. UI & Themes",
            Self::Reasoning => "4. LLM & Reasoning",
            Self::Tools => "5. Tools & MCP",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::General => Self::Updates,
            Self::Updates => Self::Aesthetics,
            Self::Aesthetics => Self::Reasoning,
            Self::Reasoning => Self::Tools,
            Self::Tools => Self::General,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::General => Self::Tools,
            Self::Updates => Self::General,
            Self::Aesthetics => Self::Updates,
            Self::Reasoning => Self::Aesthetics,
            Self::Tools => Self::Reasoning,
        }
    }
}

pub struct SettingsView {
    pub config: AppConfig,
    pub active_tab: SettingsTab,
    pub selected_index: usize,
    pub tool_test_status: Option<String>,
    pub update_check_status: Option<String>,
    pub editing_url: bool,
    pub url_input: String,
    pub available_models: Vec<String>,
    pub is_dirty: bool,
}

impl SettingsView {
    pub fn new(config: AppConfig, available_models: Vec<String>) -> Self {
        let url_input = config.backend_url.clone();
        Self {
            config,
            active_tab: SettingsTab::General,
            selected_index: 0,
            tool_test_status: None,
            update_check_status: None,
            editing_url: false,
            url_input,
            available_models,
            is_dirty: false,
        }
    }

    pub fn total_items(&self) -> usize {
        match self.active_tab {
            SettingsTab::General => 6,
            SettingsTab::Updates => 4,
            SettingsTab::Aesthetics => 5,
            SettingsTab::Reasoning => 7,
            SettingsTab::Tools => 3,
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> SettingsAction {
        if self.editing_url {
            match code {
                KeyCode::Esc => {
                    self.editing_url = false;
                    self.url_input = self.config.backend_url.clone();
                    return SettingsAction::None;
                }
                KeyCode::Backspace => {
                    self.url_input.pop();
                    return SettingsAction::None;
                }
                KeyCode::Enter => {
                    let trimmed = self.url_input.trim().to_string();
                    if !trimmed.is_empty() {
                        self.config.backend_url = trimmed;
                        self.is_dirty = true;
                    }
                    self.editing_url = false;
                    return SettingsAction::DiscoverModels;
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                    self.url_input.push(c);
                    return SettingsAction::None;
                }
                _ => return SettingsAction::None,
            }
        }

        match code {
            // Persisting is the caller's job: this view holds live session
            // values (mode, effort) that must not become defaults unasked.
            KeyCode::Esc => SettingsAction::Close,
            KeyCode::Tab | KeyCode::Char(']') => {
                self.active_tab = self.active_tab.next();
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::BackTab | KeyCode::Char('[') => {
                self.active_tab = self.active_tab.prev();
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::Char('1') => {
                self.active_tab = SettingsTab::General;
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::Char('2') => {
                self.active_tab = SettingsTab::Updates;
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::Char('3') => {
                self.active_tab = SettingsTab::Aesthetics;
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::Char('4') => {
                self.active_tab = SettingsTab::Reasoning;
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::Char('5') => {
                self.active_tab = SettingsTab::Tools;
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let total = self.total_items();
                self.selected_index = (self.selected_index + total - 1) % total;
                SettingsAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let total = self.total_items();
                self.selected_index = (self.selected_index + 1) % total;
                SettingsAction::None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.adjust_selected(-1);
                SettingsAction::None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.adjust_selected(1);
                SettingsAction::None
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.trigger_action()
            }
            _ => SettingsAction::None,
        }
    }

    fn trigger_action(&mut self) -> SettingsAction {
        match self.active_tab {
            SettingsTab::General => match self.selected_index {
                0 => SettingsAction::OpenWizard,
                1 => SettingsAction::OpenModelMenu,
                2 => {
                    self.config.permission_mode = self.config.permission_mode.next();
                    self.is_dirty = true;
                    SettingsAction::None
                }
                3 => {
                    self.config.auto_save_sessions = !self.config.auto_save_sessions;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                4 => {
                    self.cycle_editor();
                    self.is_dirty = true;
                    SettingsAction::None
                }
                5 => SettingsAction::OpenWizard,
                _ => SettingsAction::None,
            },
            SettingsTab::Updates => match self.selected_index {
                0 => {
                    self.config.auto_check_updates = !self.config.auto_check_updates;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                1 => {
                    self.config.update_channel = self.config.update_channel.toggle();
                    self.is_dirty = true;
                    SettingsAction::None
                }
                2 => {
                    self.config.silent_update_check = !self.config.silent_update_check;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                3 => SettingsAction::CheckUpdatesNow,
                _ => SettingsAction::None,
            },
            SettingsTab::Aesthetics => match self.selected_index {
                0 => {
                    self.config.show_mascot = !self.config.show_mascot;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                1 => {
                    self.config.show_tips = !self.config.show_tips;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                2 => {
                    self.config.show_ttft = !self.config.show_ttft;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                3 => {
                    self.config.show_tokens = !self.config.show_tokens;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                4 => {
                    self.config.show_toasts = !self.config.show_toasts;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                _ => SettingsAction::None,
            },
            SettingsTab::Reasoning => match self.selected_index {
                0 => SettingsAction::OpenEffortMenu,
                1 => SettingsAction::OpenSamplingMenu,
                2 => {
                    self.adjust_temperature(0.10);
                    self.is_dirty = true;
                    SettingsAction::None
                }
                3 => {
                    self.cycle_warn_threshold();
                    self.is_dirty = true;
                    SettingsAction::None
                }
                4 => {
                    self.config.auto_compact_context = !self.config.auto_compact_context;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                5 => {
                    self.config.free_search = !self.config.free_search;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                6 => {
                    self.cycle_network_retries();
                    self.is_dirty = true;
                    SettingsAction::None
                }
                _ => SettingsAction::None,
            },
            SettingsTab::Tools => match self.selected_index {
                0 => {
                    self.config.toolset_profile = self.config.toolset_profile.next();
                    self.is_dirty = true;
                    SettingsAction::None
                }
                1 => SettingsAction::OpenMcpMenu,
                2 => SettingsAction::RunToolTest,
                _ => SettingsAction::None,
            },
        }
    }

    fn adjust_selected(&mut self, delta: i32) {
        match self.active_tab {
            SettingsTab::General => match self.selected_index {
                0 => self.cycle_backend_preset(),
                1 => self.cycle_model(),
                2 => self.config.permission_mode = self.config.permission_mode.next(),
                3 => self.config.auto_save_sessions = !self.config.auto_save_sessions,
                4 => self.cycle_editor(),
                _ => {}
            },
            SettingsTab::Updates => match self.selected_index {
                0 => self.config.auto_check_updates = !self.config.auto_check_updates,
                1 => self.config.update_channel = self.config.update_channel.toggle(),
                2 => self.config.silent_update_check = !self.config.silent_update_check,
                _ => {}
            },
            SettingsTab::Aesthetics => match self.selected_index {
                0 => self.config.show_mascot = !self.config.show_mascot,
                1 => self.config.show_tips = !self.config.show_tips,
                2 => self.config.show_ttft = !self.config.show_ttft,
                3 => self.config.show_tokens = !self.config.show_tokens,
                4 => self.config.show_toasts = !self.config.show_toasts,
                _ => {}
            },
            SettingsTab::Reasoning => match self.selected_index {
                0 => self.cycle_effort(),
                1 => self.cycle_sampling(),
                2 => self.adjust_temperature(if delta > 0 { 0.05 } else { -0.05 }),
                3 => self.cycle_warn_threshold(),
                4 => self.config.auto_compact_context = !self.config.auto_compact_context,
                5 => self.config.free_search = !self.config.free_search,
                6 => self.cycle_network_retries(),
                _ => {}
            },
            SettingsTab::Tools => {
                if self.selected_index == 0 {
                    self.config.toolset_profile = self.config.toolset_profile.next();
                }
            }
        }
        self.is_dirty = true;
    }

    fn cycle_backend_preset(&mut self) {
        let presets = BackendPreset::all();
        let cur_url = &self.config.backend_url;
        let next_idx = presets.iter().position(|p| p.url == *cur_url)
            .map(|i| (i + 1) % presets.len())
            .unwrap_or(0);
        self.config.backend_url = presets[next_idx].url.to_string();
        self.url_input = self.config.backend_url.clone();
        self.is_dirty = true;
    }

    fn cycle_model(&mut self) {
        if self.available_models.is_empty() {
            return;
        }
        let cur = &self.config.model;
        let next_idx = self.available_models.iter().position(|m| m == cur)
            .map(|i| (i + 1) % self.available_models.len())
            .unwrap_or(0);
        self.config.model = self.available_models[next_idx].clone();
        self.is_dirty = true;
    }

    fn cycle_editor(&mut self) {
        let editors = ["$EDITOR", "code", "cursor", "nvim", "zed", "nano"];
        let cur = self.config.external_editor.as_str();
        let next_idx = editors.iter().position(|&e| e == cur)
            .map(|i| (i + 1) % editors.len())
            .unwrap_or(0);
        self.config.external_editor = editors[next_idx].to_string();
        self.is_dirty = true;
    }

    fn cycle_effort(&mut self) {
        let presets = ["auto", "off", "low", "medium", "high"];
        let cur = self.config.thinking_effort.as_str();
        let next_idx = presets.iter().position(|&p| p == cur)
            .map(|i| (i + 1) % presets.len())
            .unwrap_or(0);
        self.config.thinking_effort = presets[next_idx].to_string();
        self.is_dirty = true;
    }

    fn cycle_sampling(&mut self) {
        let next = self.config.sampling_preset.next();
        next.apply_to_config(&mut self.config);
        self.is_dirty = true;
    }

    fn adjust_temperature(&mut self, delta: f32) {
        let new_temp = (self.config.temperature + delta).clamp(0.0, 2.0);
        self.config.temperature = (new_temp * 100.0).round() / 100.0;
        self.config.sampling_preset = SamplingPreset::Custom;
        self.is_dirty = true;
    }

    fn cycle_warn_threshold(&mut self) {
        let thresholds = [70, 80, 85, 90, 0];
        let cur = self.config.context_warn_threshold;
        let next_idx = thresholds.iter().position(|&t| t == cur)
            .map(|i| (i + 1) % thresholds.len())
            .unwrap_or(0);
        self.config.context_warn_threshold = thresholds[next_idx];
        self.is_dirty = true;
    }

    fn cycle_network_retries(&mut self) {
        let retries = [1, 3, 5];
        let cur = self.config.network_retries;
        let next_idx = retries.iter().position(|&r| r == cur)
            .map(|i| (i + 1) % retries.len())
            .unwrap_or(0);
        self.config.network_retries = retries[next_idx];
        self.is_dirty = true;
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let box_w = width.saturating_sub(6).clamp(52, 100);
        let inner_text_w = box_w.saturating_sub(2);
        let border_color = "\x1b[38;2;160;155;145m";
        let reset = "\x1b[0m";

        let pad_row = |content: &str| -> String {
            let clipped = if crate::visible_width(content) > inner_text_w {
                crate::clip_ansi(content, inner_text_w)
            } else {
                content.to_string()
            };
            let clipped_vis = crate::visible_width(&clipped);
            let pad = " ".repeat(inner_text_w.saturating_sub(clipped_vis));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        // Header
        let title_styled = " \x1b[1;38;2;225;175;95mFlashAgent Settings Wizard\x1b[0m \x1b[38;2;160;155;145m(Tab 1-5 to switch)\x1b[0m ";
        let title_vis = crate::visible_width(title_styled);
        let dashes = box_w.saturating_sub(title_vis + 1);
        lines.push((
            LineKind::System,
            format!("  {border_color}╭─{title_styled}{}╮{reset}", "─".repeat(dashes)),
        ));

        // Tab navigation bar
        let mut tabs_line = String::from(" ");
        for tab in SettingsTab::all() {
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

        // Tab items list
        let max_val_w = inner_text_w.saturating_sub(28);
        let items: Vec<(&str, String)> = match self.active_tab {
            SettingsTab::General => vec![
                ("Backend URL *", if self.editing_url { format!("{}█", self.url_input) } else { self.config.backend_url.clone() }),
                ("Active Model", if self.config.model.is_empty() { "(auto-detected)".to_string() } else { self.config.model.clone() }),
                ("Permission Mode", self.config.permission_mode.label().to_string()),
                ("Auto-Save Sessions", if self.config.auto_save_sessions { "Enabled (auto-resume)".into() } else { "Disabled".into() }),
                ("External Editor", self.config.external_editor.clone()),
                ("Setup Wizard", "Launch initial configuration wizard".into()),
            ],
            SettingsTab::Updates => vec![
                ("Auto-Check Updates *", if self.config.auto_check_updates { "Enabled (every 4m)".into() } else { "Disabled (manual only)".into() }),
                ("Release Channel", self.config.update_channel.label().to_string()),
                ("Silent Daily Notice", if self.config.silent_update_check { "Enabled (status bar notice)".into() } else { "Disabled".into() }),
                ("Check Updates Now", self.update_check_status.clone().unwrap_or_else(|| "Check GitHub Releases API now".into())),
            ],
            SettingsTab::Aesthetics => vec![
                ("Swift Mascot", if self.config.show_mascot { "Enabled (animated)".into() } else { "Disabled".into() }),
                ("Developer Tips", if self.config.show_tips { "Enabled (rotating deck)".into() } else { "Disabled".into() }),
                ("TTFT & Prefill Speed", if self.config.show_ttft { "Enabled (lightning badge)".into() } else { "Disabled".into() }),
                ("Token Counters", if self.config.show_tokens { "Enabled (prompt/gen count)".into() } else { "Disabled".into() }),
                ("Clipboard Toasts", if self.config.show_toasts { "Enabled".into() } else { "Disabled".into() }),
            ],
            SettingsTab::Reasoning => vec![
                ("Thinking Effort", self.config.thinking_effort.clone()),
                ("Sampling Preset", self.config.sampling_preset.label().into()),
                ("Temperature", format!("{:.2}", self.config.temperature)),
                ("Context Alert", if self.config.context_warn_threshold > 0 { format!("Warn at {}%", self.config.context_warn_threshold) } else { "Disabled".into() }),
                ("Auto-Compact History", if self.config.auto_compact_context { format!("Enabled (at {}%)", self.config.context_compact_threshold) } else { "Disabled".into() }),
                ("Web Tools", if self.config.free_search { "Enabled (web_fetch, web_search)".into() } else { "Disabled (local-first)".into() }),
                ("Network Retries", format!("{} retries on connection failure", self.config.network_retries)),
            ],
            SettingsTab::Tools => vec![
                ("Toolset Profile", self.config.toolset_profile.label().to_string()),
                ("MCP Manager", "Overview & Server Registry".into()),
                ("Run Tool Test", self.tool_test_status.clone().unwrap_or_else(|| "Probe function calling".into())),
            ],
        };

        for (idx, (label, val_raw)) in items.iter().enumerate() {
            let is_sel = idx == self.selected_index;
            let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
            let val = crate::truncate_middle(val_raw, max_val_w);
            let (label_styled, val_styled) = if is_sel {
                (
                    format!("\x1b[1;38;2;240;235;225m{:<24}\x1b[0m", label),
                    format!("\x1b[1;38;2;225;175;95m{val}\x1b[0m"),
                )
            } else {
                (
                    format!("\x1b[38;2;160;155;145m{:<24}\x1b[0m", label),
                    format!("\x1b[38;2;200;195;185m{val}\x1b[0m"),
                )
            };
            lines.push((LineKind::System, pad_row(&format!("{ptr} {label_styled} {val_styled}"))));
        }

        let has_restart_items = matches!(self.active_tab, SettingsTab::General | SettingsTab::Updates);
        if has_restart_items {
            lines.push((
                LineKind::System,
                pad_row("\x1b[38;2;225;175;95m* Marked options require app restart to take effect\x1b[0m"),
            ));
        } else {
            lines.push((LineKind::System, pad_row("")));
        }

        lines.push((LineKind::System, pad_row("\x1b[38;2;135;130;125mTab/1-5 switch tab · ↑/↓ navigate · Enter/←/→ toggle value · Esc save & return\x1b[0m")));

        lines.push((
            LineKind::System,
            format!("  {border_color}╰{}╯{reset}", "─".repeat(box_w)),
        ));

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_tab_navigation() {
        let mut tab = SettingsTab::General;
        assert_eq!(tab.label(), "1. General");

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Updates);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Aesthetics);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Reasoning);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Tools);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::General);
    }

    #[test]
    fn test_settings_wizard_key_handling() {
        let cfg = AppConfig::default();
        let mut view = SettingsView::new(cfg, vec!["model-a".into(), "model-b".into()]);
        assert_eq!(view.active_tab, SettingsTab::General);
        assert_eq!(view.selected_index, 0);

        // Tab switches to Updates
        view.handle_key(KeyCode::Tab, KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Updates);
        assert_eq!(view.selected_index, 0);

        // Toggle auto-check updates
        assert!(view.config.auto_check_updates);
        view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(!view.config.auto_check_updates);

        // Jump to Aesthetics via '3'
        view.handle_key(KeyCode::Char('3'), KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Aesthetics);
        assert_eq!(view.selected_index, 0);

        // Toggle the mascot (first Aesthetics item)
        assert!(view.config.show_mascot);
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert!(!view.config.show_mascot);

        // Jump to Tools via '5'; its items all reach real actions
        view.handle_key(KeyCode::Char('5'), KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Tools);
        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::empty()), SettingsAction::OpenMcpMenu);
        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::empty()), SettingsAction::RunToolTest);

        // Esc closes and saves
        let act = view.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(act, SettingsAction::Close);
    }

    #[test]
    fn test_settings_wizard_borders_closed_and_consistent() {
        let cfg = AppConfig::default();
        let view = SettingsView::new(cfg, vec!["model-a".into()]);

        for width in [60, 80, 100, 120] {
            let rendered = view.render(width);
            assert!(!rendered.is_empty());
            let expected_w = crate::visible_width(&rendered[0].1);
            assert!(expected_w <= width, "Row width {expected_w} exceeds terminal width {width}");

            for (idx, line) in rendered.iter().enumerate() {
                let row_w = crate::visible_width(&line.1);
                assert_eq!(
                    row_w, expected_w,
                    "Row {idx} width {row_w} does not match expected {expected_w} at terminal width {width}"
                );
                // Verify every line terminates with a closed right border
                let clean = &line.1;
                assert!(
                    clean.ends_with("╮\x1b[0m")
                        || clean.ends_with("┤\x1b[0m")
                        || clean.ends_with("│\x1b[0m")
                        || clean.ends_with("╯\x1b[0m"),
                    "Row {idx} is missing a closed right border: {clean}"
                );
            }
        }
    }
}
