//! Settings screen (`/settings`, or Tab on empty input).

use flashagent_core::{AppConfig, BackendPreset, PersonalityTrait};
use crossterm::event::{KeyCode, KeyModifiers};
use crate::{LineKind, RenderLine};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsAction {
    None,
    Close,
    DiscoverModels,
    RunToolTest,
    /// The F3 model menu.
    OpenModelMenu,
    /// The F4 effort menu.
    OpenEffortMenu,
    OpenWizard,
    /// The sampling menu (advanced).
    OpenSamplingMenu,
    CheckUpdatesNow,
    OpenMcpMenu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Updates,
    Aesthetics,
    Reasoning,
    Goal,
    Tools,
    Style,
}

impl SettingsTab {
    pub fn all() -> &'static [SettingsTab] {
        &[
            SettingsTab::General,
            SettingsTab::Updates,
            SettingsTab::Aesthetics,
            SettingsTab::Reasoning,
            SettingsTab::Goal,
            SettingsTab::Tools,
            SettingsTab::Style,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "1 General",
            Self::Updates => "2 Updates",
            Self::Aesthetics => "3 UI",
            Self::Reasoning => "4 LLM",
            Self::Goal => "5 Goal",
            Self::Tools => "6 Tools",
            Self::Style => "7 Style",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::General => Self::Updates,
            Self::Updates => Self::Aesthetics,
            Self::Aesthetics => Self::Reasoning,
            Self::Reasoning => Self::Goal,
            Self::Goal => Self::Tools,
            Self::Tools => Self::Style,
            Self::Style => Self::General,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::General => Self::Style,
            Self::Style => Self::Tools,
            Self::Updates => Self::General,
            Self::Aesthetics => Self::Updates,
            Self::Reasoning => Self::Aesthetics,
            Self::Goal => Self::Reasoning,
            Self::Tools => Self::Goal,
        }
    }
}

/// `None` is "unlimited", where the cycle starts and wraps to.
const GOAL_STEP_CHOICES: &[Option<u32>] = &[None, Some(25), Some(50), Some(100), Some(250), Some(500), Some(1000)];
const GOAL_MINUTE_CHOICES: &[Option<u32>] = &[None, Some(15), Some(30), Some(60), Some(120), Some(240), Some(480)];
const GOAL_TOKEN_CHOICES: &[Option<i64>] =
    &[None, Some(50_000), Some(100_000), Some(200_000), Some(500_000), Some(1_000_000), Some(2_000_000)];

/// Negative `delta` goes back. A hand-typed value not on the list restarts
/// from the first choice.
fn cycle_choice<T: PartialEq + Copy>(choices: &[Option<T>], current: Option<T>, delta: i32) -> Option<T> {
    let n = choices.len();
    let next = match choices.iter().position(|c| *c == current) {
        Some(i) if delta < 0 => (i + n - 1) % n,
        Some(i) => (i + 1) % n,
        None => 0,
    };
    choices[next]
}

fn goal_minutes_label(minutes: Option<u32>) -> String {
    match minutes {
        None | Some(0) => "Unlimited".to_string(),
        Some(m) if m % 60 == 0 => format!("{} h", m / 60),
        Some(m) => format!("{m} min"),
    }
}

fn goal_tokens_label(tokens: Option<i64>) -> String {
    match tokens {
        None | Some(0) => "Unlimited".to_string(),
        Some(t) if t >= 1_000_000 && t % 1_000_000 == 0 => format!("{}M generated tokens", t / 1_000_000),
        Some(t) if t >= 1_000 => format!("{}k generated tokens", t / 1_000),
        Some(t) => format!("{t} generated tokens"),
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
    /// So an automatic threshold can show what it works out to.
    pub context_capacity: usize,
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
            context_capacity: 0,
        }
    }

    /// Drawing and selection both use this list, so every drawn row is reachable.
    fn items(&self) -> Vec<(&'static str, String)> {
    match self.active_tab {
        SettingsTab::General => vec![
            ("Backend URL *", if self.editing_url { format!("{}█", self.url_input) } else { self.config.backend_url.clone() }),
            ("Active model", if self.config.model.is_empty() { "(auto-detected)".to_string() } else { self.config.model.clone() }),
            ("Permission mode", self.config.permission_mode.label().to_string()),
            ("Auto-save sessions", if self.config.auto_save_sessions { "Enabled (auto-resume)".into() } else { "Disabled".into() }),
            ("External editor", self.config.external_editor.clone()),
            ("Setup wizard", "Run the first-start setup again".into()),
        ],
        SettingsTab::Updates => vec![
            ("Auto-update *", if self.config.auto_check_updates { "On (installs in the background)".into() } else { "Off (Ctrl+U or /update only)".into() }),
            ("Release channel", self.config.update_channel.label().to_string()),
            ("Check for updates", self.update_check_status.clone().unwrap_or_else(|| "Ask GitHub for the newest release".into())),
        ],
        SettingsTab::Aesthetics => vec![
            ("Swift mascot", if self.config.show_mascot { "Enabled (animated)".into() } else { "Disabled".into() }),
            ("Developer tips", if self.config.show_tips { "Enabled (rotating deck)".into() } else { "Disabled".into() }),
            ("Prefill speed", if self.config.show_ttft { "Enabled (TTFT and t/s after a turn starts)".into() } else { "Disabled".into() }),
            ("Generation speed", if self.config.show_tokens { "Enabled (t/s while the model writes)".into() } else { "Disabled".into() }),
            ("Clipboard toasts", if self.config.show_toasts { "Enabled".into() } else { "Disabled".into() }),
            ("Animations", if self.config.animations { "Enabled (sweeps, pulses, unfolding panels)".into() } else { "Reduced (spinners only)".into() }),
            ("Color theme", self.config.color_theme.label().to_string()),
        ],
        SettingsTab::Reasoning => vec![
            ("Thinking effort", self.config.thinking_effort.clone()),
            ("Context alert", if self.config.context_warn_threshold > 0 { format!("Warn at {}%", self.config.context_warn_threshold) } else { "Disabled".into() }),
            ("Auto-compact history", if self.config.auto_compact_context {
                // Zero follows the window; "0%" would read as "always".
                match self.config.context_compact_threshold {
                    0 => format!("Enabled (auto: {}% for this window)", flashagent_core::default_compact_threshold(self.context_capacity)),
                    pct => format!("Enabled (at {pct}%)"),
                }
            } else { "Disabled".into() }),
            ("Web tools", if self.config.web_tools { "Enabled (web_fetch, web_search)".into() } else { "Disabled".into() }),
            ("Network retries", format!("{} on connection failure", crate::plural(self.config.network_retries, "retry", "retries"))),
            // Last, and for those who know what the numbers do.
            ("Sampling (advanced)", format!("{} · temperature {:.2}", self.config.sampling_preset.label(), self.config.temperature)),
        ],
        SettingsTab::Goal => vec![
            ("Step limit", match self.config.goal_max_steps {
                None | Some(0) => "Unlimited".to_string(),
                Some(n) => crate::plural(n as usize, "step", "steps"),
            }),
            ("Time limit", goal_minutes_label(self.config.goal_max_minutes)),
            ("Token limit", goal_tokens_label(self.config.goal_max_output_tokens)),
        ],
        SettingsTab::Style => {
            let p = &self.config.personality;
            let mut rows = vec![("Base style and tone", format!("{} — {}", p.base.label(), p.base.blurb()))];
            for t in PersonalityTrait::ALL {
                let level = p.level(t);
                let blurb = t.blurb(level);
                rows.push((
                    t.label(),
                    if blurb.is_empty() { level.label().to_string() } else { format!("{} — {blurb}", level.label()) },
                ));
            }
            rows
        }
        SettingsTab::Tools => vec![
            ("Toolset profile", self.config.toolset_profile.label().to_string()),
            ("MCP servers", "Configured servers and the marketplace".into()),
            ("Run tool test", self.tool_test_status.clone().unwrap_or_else(|| "Probe function calling".into())),
        ],
    }
    }

    pub fn total_items(&self) -> usize {
        self.items().len()
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
            // Saving is the caller's job: live mode and effort must not become defaults
            // unasked.
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
                self.active_tab = SettingsTab::Goal;
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::Char('6') => {
                self.active_tab = SettingsTab::Tools;
                self.selected_index = 0;
                SettingsAction::None
            }
            KeyCode::Char('7') => {
                self.active_tab = SettingsTab::Style;
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
                2 => SettingsAction::CheckUpdatesNow,
                _ => SettingsAction::None,
            },
            SettingsTab::Goal | SettingsTab::Style => {
                self.adjust_selected(1);
                SettingsAction::None
            }
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
                5 => {
                    self.config.animations = !self.config.animations;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                6 => {
                    self.cycle_theme();
                    SettingsAction::None
                }
                _ => SettingsAction::None,
            },
            SettingsTab::Reasoning => match self.selected_index {
                0 => SettingsAction::OpenEffortMenu,
                1 => {
                    self.cycle_warn_threshold();
                    self.is_dirty = true;
                    SettingsAction::None
                }
                2 => {
                    self.config.auto_compact_context = !self.config.auto_compact_context;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                3 => {
                    self.config.web_tools = !self.config.web_tools;
                    self.is_dirty = true;
                    SettingsAction::None
                }
                4 => {
                    self.cycle_network_retries();
                    self.is_dirty = true;
                    SettingsAction::None
                }
                5 => SettingsAction::OpenSamplingMenu,
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
            SettingsTab::Style => {
                let personality = &mut self.config.personality;
                match self.selected_index.checked_sub(1).and_then(|i| PersonalityTrait::ALL.get(i)) {
                    None => personality.base = personality.base.cycle(delta > 0),
                    Some(t) => {
                        let level = personality.level_mut(*t);
                        *level = level.cycle(delta > 0);
                    }
                }
            }
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
                _ => {}
            },
            SettingsTab::Goal => match self.selected_index {
                0 => self.config.goal_max_steps = cycle_choice(GOAL_STEP_CHOICES, self.config.goal_max_steps, delta),
                1 => self.config.goal_max_minutes = cycle_choice(GOAL_MINUTE_CHOICES, self.config.goal_max_minutes, delta),
                2 => {
                    self.config.goal_max_output_tokens =
                        cycle_choice(GOAL_TOKEN_CHOICES, self.config.goal_max_output_tokens, delta)
                }
                _ => {}
            },
            SettingsTab::Aesthetics => match self.selected_index {
                0 => self.config.show_mascot = !self.config.show_mascot,
                1 => self.config.show_tips = !self.config.show_tips,
                2 => self.config.show_ttft = !self.config.show_ttft,
                3 => self.config.show_tokens = !self.config.show_tokens,
                4 => self.config.show_toasts = !self.config.show_toasts,
                5 => self.config.animations = !self.config.animations,
                6 => self.cycle_theme(),
                _ => {}
            },
            SettingsTab::Reasoning => match self.selected_index {
                0 => self.cycle_effort(),
                1 => self.cycle_warn_threshold(),
                2 => self.config.auto_compact_context = !self.config.auto_compact_context,
                3 => self.config.web_tools = !self.config.web_tools,
                4 => self.cycle_network_retries(),
                5 => self.cycle_sampling(),
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

    /// Applied at once: a theme picked by name is guesswork until seen.
    fn cycle_theme(&mut self) {
        self.config.color_theme = self.config.color_theme.next();
        crate::theme::set(self.config.color_theme);
        self.is_dirty = true;
    }

    fn cycle_sampling(&mut self) {
        let next = self.config.sampling_preset.next();
        next.apply_to_config(&mut self.config);
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
        let box_w = width.saturating_sub(6).min(100);
        let inner_text_w = box_w.saturating_sub(2);
        let border_color = "\x1b[38;2;160;155;145m";
        let reset = "\x1b[0m";

        let pad_row = |content: &str| -> String {
            let clipped = crate::tool_views::clip_ellipsis(content, inner_text_w);
            let clipped_vis = crate::visible_width(&clipped);
            let pad = " ".repeat(inner_text_w.saturating_sub(clipped_vis));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        let title_styled = " \x1b[1;38;2;225;175;95mSettings\x1b[0m ";
        let title_vis = crate::visible_width(title_styled);
        let dashes = box_w.saturating_sub(title_vis + 1);
        lines.push((
            LineKind::System,
            format!("  {border_color}╭─{title_styled}{}╮{reset}", "─".repeat(dashes)),
        ));

        // Tighter as the window narrows; at the narrowest the other tabs are their numbers.
        let tabs_line = |gap: usize, numbers_only: bool| -> String {
            let tabs: Vec<String> = SettingsTab::all()
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
            .find(|line| crate::visible_width(line) <= inner_text_w)
            .unwrap_or_else(|| tabs_line(0, true));
        lines.push((LineKind::System, pad_row(&tabs)));
        lines.push((
            LineKind::System,
            format!("  {border_color}├{}┤{reset}", "─".repeat(box_w)),
        ));

        let items = self.items();
        // Values get the room a narrow window leaves once labels are as long as they need.
        let longest = items.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0);
        let label_w = if inner_text_w < 60 { (longest + 1).min(24) } else { 24 };
        let max_val_w = inner_text_w.saturating_sub(label_w + 4);

        for (idx, (label, val_raw)) in items.iter().enumerate() {
            let is_sel = idx == self.selected_index;
            let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
            let val = crate::truncate_middle(val_raw, max_val_w);
            let (label_styled, val_styled) = if is_sel {
                (
                    format!("\x1b[1;38;2;240;235;225m{:<label_w$}\x1b[0m", label),
                    format!("\x1b[1;38;2;225;175;95m{val}\x1b[0m"),
                )
            } else {
                (
                    format!("\x1b[38;2;160;155;145m{:<label_w$}\x1b[0m", label),
                    format!("\x1b[38;2;200;195;185m{val}\x1b[0m"),
                )
            };
            lines.push((LineKind::System, pad_row(&format!("{ptr} {label_styled} {val_styled}"))));
        }

        let note = match self.active_tab {
            SettingsTab::General | SettingsTab::Updates => "\x1b[38;2;225;175;95m* Takes effect after a restart\x1b[0m",
            SettingsTab::Style => {
                "\x1b[38;2;135;130;125mTone only — tools and code are unaffected. Applies from your next message.\x1b[0m"
            }
            _ => "",
        };
        for row in crate::wrap_styled(note, inner_text_w.max(1)) {
            lines.push((LineKind::System, pad_row(&row)));
        }
        if note.is_empty() {
            lines.push((LineKind::System, pad_row("")));
        }

        let hints: &[(&str, &str)] = if self.editing_url {
            &[("Enter", "save"), ("Esc", "cancel")]
        } else {
            &[("Tab/1-7", "switch tab"), ("↑/↓", "move"), ("Enter/←/→", "change"), ("Esc", "save and close")]
        };
        lines.push((LineKind::System, pad_row(&crate::key_hints(hints, inner_text_w.saturating_sub(1)))));

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
        assert_eq!(tab.label(), "1 General");

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Updates);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Aesthetics);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Reasoning);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Goal);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Tools);
        assert_eq!(tab.prev(), SettingsTab::Goal);

        tab = tab.next();
        assert_eq!(tab, SettingsTab::Style);
        tab = tab.next();
        assert_eq!(tab, SettingsTab::General);
    }

    #[test]
    fn test_settings_wizard_key_handling() {
        let cfg = AppConfig::default();
        let mut view = SettingsView::new(cfg, vec!["model-a".into(), "model-b".into()]);
        assert_eq!(view.active_tab, SettingsTab::General);
        assert_eq!(view.selected_index, 0);

        view.handle_key(KeyCode::Tab, KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Updates);
        assert_eq!(view.selected_index, 0);

        assert!(view.config.auto_check_updates);
        view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(!view.config.auto_check_updates);

        view.handle_key(KeyCode::Char('3'), KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Aesthetics);
        assert_eq!(view.selected_index, 0);

        assert!(view.config.show_mascot);
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert!(!view.config.show_mascot);

        view.handle_key(KeyCode::Char('6'), KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Tools);
        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::empty()), SettingsAction::OpenMcpMenu);
        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::empty()), SettingsAction::RunToolTest);

        let act = view.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(act, SettingsAction::Close);
    }

    #[test]
    fn the_arrows_reach_every_row_of_every_tab() {
        // The row count was a separate number, so ↓ from Animations wrapped to the top.
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        for (n, tab) in SettingsTab::all().iter().enumerate() {
            view.handle_key(KeyCode::Char(char::from(b'1' + n as u8)), KeyModifiers::empty());
            assert_eq!(view.active_tab, *tab);
            let rows = view.items();
            for _ in 1..rows.len() {
                view.handle_key(KeyCode::Down, KeyModifiers::empty());
            }
            assert_eq!(view.selected_index, rows.len() - 1, "{tab:?}: the last row, {:?}, cannot be reached", rows.last());
        }
    }

    #[test]
    fn the_color_theme_row_changes_the_theme() {
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        view.handle_key(KeyCode::Char('3'), KeyModifiers::empty());
        while view.items()[view.selected_index].0 != "Color theme" {
            view.handle_key(KeyCode::Down, KeyModifiers::empty());
        }
        let before = view.config.color_theme;
        view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_ne!(view.config.color_theme, before);
    }

    #[test]
    fn goal_limits_start_unlimited_and_cycle_both_ways() {
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        view.handle_key(KeyCode::Char('5'), KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Goal);
        let shown = |v: &SettingsView| v.render(100).iter().map(|(_, t)| t.clone()).collect::<Vec<_>>().join("\n");
        assert!(shown(&view).contains("Unlimited"), "{}", shown(&view));

        view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(view.config.goal_max_steps, Some(25));
        view.handle_key(KeyCode::Left, KeyModifiers::empty());
        view.handle_key(KeyCode::Left, KeyModifiers::empty());
        assert_eq!(view.config.goal_max_steps, Some(1000), "going left from unlimited wraps to the largest");

        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.config.goal_max_minutes, Some(60));
        assert!(shown(&view).contains("1 h"), "{}", shown(&view));

        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.config.goal_max_output_tokens, Some(100_000));
        assert!(shown(&view).contains("100k generated tokens"), "{}", shown(&view));
    }

    #[test]
    fn test_settings_wizard_borders_closed_and_consistent() {
        let cfg = AppConfig::default();
        let view = SettingsView::new(cfg, vec!["model-a".into()]);

        for width in [44, 50, 60, 80, 100, 120] {
            let rendered = view.render(width);
            let tabs = crate::strip_ansi(&rendered[1].1);
            assert!(tabs.contains("[1 General]") && tabs.contains(" 7") && !tabs.contains('…'), "{width}: {tabs:?}");
            assert!(!rendered.is_empty());
            let expected_w = crate::visible_width(&rendered[0].1);
            assert!(expected_w <= width, "Row width {expected_w} exceeds terminal width {width}");

            for (idx, line) in rendered.iter().enumerate() {
                let row_w = crate::visible_width(&line.1);
                assert_eq!(
                    row_w, expected_w,
                    "Row {idx} width {row_w} does not match expected {expected_w} at terminal width {width}"
                );
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

    #[test]
    fn the_hints_end_with_the_way_out() {
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        let last_hint = |v: &SettingsView| {
            let rows = v.render(100);
            crate::strip_ansi(&rows[rows.len() - 2].1)
        };
        assert!(last_hint(&view).contains("Tab/1-7 switch tab"), "{}", last_hint(&view));
        assert!(last_hint(&view).trim_end_matches(['│', ' ']).ends_with("Esc save and close"), "{}", last_hint(&view));
        view.editing_url = true;
        assert!(last_hint(&view).trim_end_matches(['│', ' ']).ends_with("Enter save · Esc cancel"), "{}", last_hint(&view));
    }

    #[test]
    fn the_style_tab_cycles_the_voice_and_its_characteristics() {
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        view.handle_key(KeyCode::Char('7'), KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Style);
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.config.personality.base, flashagent_core::BaseStyle::Friendly);
        for _ in 0..4 {
            view.handle_key(KeyCode::Down, KeyModifiers::empty());
        }
        view.handle_key(KeyCode::Left, KeyModifiers::empty());
        assert_eq!(view.config.personality.emoji, flashagent_core::TraitLevel::Less);
        let shown = view.render(100).iter().map(|(_, t)| crate::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(shown.contains("Friendly — Warm and chatty"), "{shown}");
        assert!(shown.contains("Less — Don't use as many emoji"), "{shown}");
        assert!(shown.lines().all(|l| crate::visible_width(l) <= 100), "{shown}");
    }
}
