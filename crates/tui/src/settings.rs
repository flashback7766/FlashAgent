//! Interactive Settings Tab opened via `/settings` or `Tab` on empty input.
//! Allows live reconfiguration of backend, model, behavior, sampling, and search.

use flashagent_core::{AppConfig, BackendPreset};
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
}

pub struct SettingsView {
    pub config: AppConfig,
    pub selected_index: usize,
    pub tool_test_status: Option<String>,
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
            selected_index: 0,
            tool_test_status: None,
            editing_url: false,
            url_input,
            available_models,
            is_dirty: false,
        }
    }

    pub fn total_items(&self) -> usize {
        7 // URL, Model, Mode, Effort, ToolsetProfile, Sampling, Save&Back
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
                        let _ = self.config.save();
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
            KeyCode::Esc => {
                if self.is_dirty {
                    let _ = self.config.save();
                }
                SettingsAction::Close
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
                if self.selected_index == 5 {
                    return SettingsAction::OpenSamplingMenu;
                }
                self.adjust_selected(1);
                SettingsAction::None
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                match self.selected_index {
                    0 => {
                        // Open setup wizard for full backend configuration
                        SettingsAction::OpenWizard
                    }
                    1 => {
                        // Open dedicated F3 model selection menu
                        SettingsAction::OpenModelMenu
                    }
                    2 => {
                        // Permission mode
                        self.config.permission_mode = self.config.permission_mode.next();
                        self.is_dirty = true;
                        let _ = self.config.save();
                        SettingsAction::None
                    }
                    3 => {
                        // Open dedicated F4 thinking effort menu
                        SettingsAction::OpenEffortMenu
                    }
                    4 => {
                        // Toolset profile toggle
                        self.config.toolset_profile = self.config.toolset_profile.next();
                        self.is_dirty = true;
                        let _ = self.config.save();
                        SettingsAction::None
                    }
                    5 => {
                        // Open dedicated F5 sampling parameters menu
                        SettingsAction::OpenSamplingMenu
                    }
                    6 => {
                        // Save & Back
                        let _ = self.config.save();
                        SettingsAction::Close
                    }
                    _ => SettingsAction::None,
                }
            }
            _ => SettingsAction::None,
        }
    }

    fn adjust_selected(&mut self, _delta: i32) {
        match self.selected_index {
            0 => {
                self.cycle_backend_preset();
            }
            1 => {
                self.cycle_model();
            }
            2 => {
                self.config.permission_mode = self.config.permission_mode.next();
                self.is_dirty = true;
            }
            3 => {
                self.cycle_effort();
            }
            4 => {
                self.config.toolset_profile = self.config.toolset_profile.next();
                self.is_dirty = true;
            }
            _ => {}
        }
        if self.is_dirty {
            let _ = self.config.save();
        }
    }

    fn cycle_backend_preset(&mut self) {
        let presets = BackendPreset::all();
        let cur_url = &self.config.backend_url;
        let next_idx = presets.iter().position(|p| p.url == *cur_url)
            .map(|i| (i + 1) % presets.len())
            .unwrap_or(0);
        self.config.backend_url = presets[next_idx].url.clone();
        self.url_input = self.config.backend_url.clone();
        self.is_dirty = true;
        let _ = self.config.save();
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
        let _ = self.config.save();
    }

    fn cycle_effort(&mut self) {
        let presets = ["auto", "off", "low", "medium", "high"];
        let cur = self.config.thinking_effort.as_str();
        let next_idx = presets.iter().position(|&p| p == cur)
            .map(|i| (i + 1) % presets.len())
            .unwrap_or(0);
        self.config.thinking_effort = presets[next_idx].to_string();
        self.is_dirty = true;
        let _ = self.config.save();
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let border_color = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";
        let inner_w = (width.saturating_sub(6)).clamp(54, 110);

        let title = " Settings Tab (/settings · Tab) ";
        let dash_count = inner_w.saturating_sub(title.chars().count() + 1);
        lines.push((
            LineKind::System,
            format!("  {border_color}┌─\x1b[1;38;2;225;175;95m{title}{border_color}{}┐{reset}", "─".repeat(dash_count)),
        ));

        let pad_row = |content: &str| -> String {
            let max_w = inner_w.saturating_sub(2);
            let vis = crate::visible_width(content);
            let clipped = if vis > max_w {
                crate::clip_ansi(content, max_w)
            } else {
                content.to_string()
            };
            let clipped_vis = crate::visible_width(&clipped);
            let pad = " ".repeat(max_w.saturating_sub(clipped_vis));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        lines.push((LineKind::System, pad_row("\x1b[38;2;135;130;125mConfigure LLM backend, model, behavior, sampling, and search\x1b[0m")));
        lines.push((LineKind::System, pad_row("")));

        let max_val_w = inner_w.saturating_sub(24);
        let sampling_summary = format!(
            "[{}] Temp {:.2}, Top-P {:.2}",
            self.config.sampling_preset.label(),
            self.config.temperature,
            self.config.top_p.unwrap_or(0.95),
        );
        let items = [
            ("Backend URL", if self.editing_url { format!("{}█", self.url_input) } else { self.config.backend_url.clone() }),
            ("Active Model", if self.config.model.is_empty() { "(auto-detected)".to_string() } else { self.config.model.clone() }),
            ("Permission Mode", self.config.permission_mode.label().to_string()),
            ("Thinking Effort", self.config.thinking_effort.clone()),
            ("Toolset Profile", self.config.toolset_profile.label().to_string()),
            ("Sampling Params", sampling_summary),
            ("Save & Back", "Return to chat (Esc / Enter)".to_string()),
        ];

        for (idx, (label, val_raw)) in items.iter().enumerate() {
            let is_sel = idx == self.selected_index;
            let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
            let val = crate::truncate_middle(val_raw, max_val_w);
            let (label_styled, val_styled) = if is_sel {
                (
                    format!("\x1b[1;38;2;240;235;225m{:<18}\x1b[0m", label),
                    format!("\x1b[1;38;2;225;175;95m{val}\x1b[0m"),
                )
            } else {
                (
                    format!("\x1b[38;2;160;155;145m{:<18}\x1b[0m", label),
                    format!("\x1b[38;2;200;195;185m{val}\x1b[0m"),
                )
            };
            lines.push((LineKind::System, pad_row(&format!("{ptr} {label_styled} {val_styled}"))));
        }

        lines.push((LineKind::System, pad_row("")));
        lines.push((LineKind::System, pad_row("\x1b[38;2;135;130;125m↑/↓ — navigate · Enter — open menu/change · Esc — save and close\x1b[0m")));

        lines.push((
            LineKind::System,
            format!("  {border_color}└{}┘{reset}", "─".repeat(inner_w)),
        ));

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_navigation_and_toggle() {
        let cfg = AppConfig::default();
        let mut view = SettingsView::new(cfg, vec!["model-a".into(), "model-b".into()]);
        assert_eq!(view.selected_index, 0);

        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(view.selected_index, 1);

        // Model Enter opens model menu
        let act = view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(act, SettingsAction::OpenModelMenu);

        // Navigate to Toolset Profile (index 4)
        view.selected_index = 4;
        assert_eq!(view.config.toolset_profile, flashagent_core::ToolsetProfile::Auto);
        view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(view.config.toolset_profile, flashagent_core::ToolsetProfile::Full);
        view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(view.config.toolset_profile, flashagent_core::ToolsetProfile::Compact);

        // Navigate to Sampling Parameters (index 5)
        view.selected_index = 5;
        let act = view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(act, SettingsAction::OpenSamplingMenu);

        // Esc closes
        let act = view.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(act, SettingsAction::Close);
    }
}
