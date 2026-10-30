//! Settings screen (`/settings`, or Tab on empty input).

use flashagent_core::{AppConfig, PersonalityTrait};
use crossterm::event::{KeyCode, KeyModifiers};
use crate::{LineKind, RenderLine};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsAction {
    None,
    Close,
    /// Settings -> Providers: add, edit and delete them.
    OpenProviders,
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
    /// Settings -> Tools: what the model may call.
    OpenTools,
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
        // The number stays first: it is how the tab is chosen.
        match self {
            Self::General => crate::i18n::tr("1 General"),
            Self::Updates => crate::i18n::tr("2 Updates"),
            Self::Aesthetics => crate::i18n::tr("3 UI"),
            Self::Reasoning => crate::i18n::tr("4 LLM"),
            Self::Goal => crate::i18n::tr("5 Goal"),
            Self::Tools => crate::i18n::tr("6 Tools"),
            Self::Style => crate::i18n::tr("7 Style"),
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
        None | Some(0) => crate::i18n::tr("Unlimited").to_string(),
        Some(m) if m % 60 == 0 => crate::i18n::tr("{} h").replace("{}", &(m / 60).to_string()),
        Some(m) => crate::i18n::tr("{m} min").replace("{m}", &m.to_string()),
    }
}

fn goal_tokens_label(tokens: Option<i64>) -> String {
    match tokens {
        None | Some(0) => crate::i18n::tr("Unlimited").to_string(),
        Some(t) if t >= 1_000_000 && t % 1_000_000 == 0 => crate::i18n::tr("{}M generated tokens").replace("{}", &(t / 1_000_000).to_string()),
        Some(t) if t >= 1_000 => crate::i18n::tr("{}k generated tokens").replace("{}", &(t / 1_000).to_string()),
        Some(t) => crate::i18n::tr("{t} generated tokens").replace("{t}", &t.to_string()),
    }
}

/// The first item row of the card: title, tabs line and separator before it.
const FIRST_ITEM_LINE: usize = 3;

pub struct SettingsView {
    pub config: AppConfig,
    pub active_tab: SettingsTab,
    pub selected_index: usize,
    pub tool_test_status: Option<String>,
    pub update_check_status: Option<String>,
    pub available_models: Vec<String>,
    /// So an automatic threshold can show what it works out to.
    pub context_capacity: usize,
    /// The efforts the model in use actually takes, read from what the server
    /// reported. Settings must not offer a level the API would refuse, and must
    /// not miss one it would accept.
    pub effort_choices: Vec<String>,
    /// The provider in use when Settings opened.
    opened_on: Option<String>,
}

/// What Settings offers before anything is known about the model in use: the
/// levels every reasoning API is expected to take, and nothing beyond them. A
/// real list replaces this as soon as the server has said what it has, so that
/// a model offering `xhigh` or `max` can reach them and a model offering fewer
/// is never asked for a level it would refuse.
pub fn effort_choices_defaults() -> Vec<String> {
    ["auto", "off", "low", "medium", "high"].iter().map(|s| s.to_string()).collect()
}

/// The level that will really be asked of `model`: a saved level the new model
/// does not offer cannot be carried across, or the setting would read as
/// applied while the request asked for something the gateway drops. Mirrors
/// the rule in `Client::resolve_effort`, so the card, the settings row and the
/// request cannot tell three different stories.
///
/// `None`, a model that cannot reason and a model that named no settings all
/// keep the level as it is: nothing was learned that contradicts it.
pub fn effort_for_model(saved: &str, profile: Option<&flashagent_llm::ThinkingProfile>) -> String {
    let Some(p) = profile.filter(|p| p.supported && !p.presets.is_empty()) else {
        return saved.to_string();
    };
    let level = saved.trim().to_ascii_lowercase();
    if level.is_empty() || level == "auto" || level == "default" || p.presets.iter().any(|x| x.eq_ignore_ascii_case(&level)) {
        return saved.to_string();
    }
    "auto".to_string()
}

impl SettingsView {
    pub fn new(config: AppConfig, available_models: Vec<String>) -> Self {
        let opened_on = Some(config.active_profile().name.clone());
        Self {
            config,
            active_tab: SettingsTab::General,
            selected_index: 0,
            tool_test_status: None,
            update_check_status: None,
            available_models,
            context_capacity: 0,
            effort_choices: effort_choices_defaults(),
            opened_on,
        }
    }

    /// Drawing and selection both use this list, so every drawn row is reachable.
    fn items(&self) -> Vec<(&'static str, String)> {
    match self.active_tab {
        SettingsTab::General => vec![
            (crate::i18n::tr("Provider"), {
                let p = self.config.active_profile();
                format!("{} \u{b7} {}", p.name, p.url)
            }),
            (crate::i18n::tr("Active model"), {
                let model = &self.config.active_profile().model;
                if model.is_empty() { crate::i18n::tr("(auto-detected)").to_string() } else { model.clone() }
            }),
            (crate::i18n::tr("Permission mode"), crate::i18n::tr(self.config.permission_mode.label()).to_string()),
            (crate::i18n::tr("Safe mode"), if self.config.safe_mode {
                crate::i18n::tr("On (reading and editing inside the project; no shell, no web, no subagents)").into()
            } else { crate::i18n::tr("Off").into() }),
            (crate::i18n::tr("Crash reports"), if self.config.crash_reports {
                crate::i18n::tr("On (a backtrace is written to ~/.flashagent/crash.log)").into()
            } else { crate::i18n::tr("Off (nothing is written when the app falls over)").into() }),
            (crate::i18n::tr("Auto-save sessions"), if self.config.auto_save_sessions { crate::i18n::tr("Enabled (auto-resume)").into() } else { crate::i18n::tr("Disabled").into() }),
            (crate::i18n::tr("External editor"), self.config.external_editor.clone()),
            // The language of what the user reads. It applies at once: the
            // hints, /help and the tips speak it from the next frame. The
            // Settings screen stays English for now: half of it in two
            // languages is worse than all of it in one.
            (
                crate::i18n::tr("Interface language"),
                crate::i18n::Lang::from_code(&self.config.interface_language).label().to_string(),
            ),
            (crate::i18n::tr("Setup wizard"), crate::i18n::tr("Run the setup again; a new server is added").into()),
        ],
        SettingsTab::Updates => vec![
            (crate::i18n::tr("Auto-update *"), if self.config.auto_check_updates { crate::i18n::tr("On (installs in the background)").into() } else { crate::i18n::tr("Off (/update only)").into() }),
            (crate::i18n::tr("Release channel"), self.config.update_channel.label().to_string()),
            (crate::i18n::tr("Check for updates"), self.update_check_status.clone().unwrap_or_else(|| crate::i18n::tr("Ask GitHub for the newest release").into())),
        ],
        SettingsTab::Aesthetics => vec![
            (crate::i18n::tr("Swift mascot"), if self.config.show_mascot { crate::i18n::tr("Enabled (animated)").into() } else { crate::i18n::tr("Disabled").into() }),
            (crate::i18n::tr("Developer tips"), if self.config.show_tips { crate::i18n::tr("Enabled (rotating deck)").into() } else { crate::i18n::tr("Disabled").into() }),
            (crate::i18n::tr("Prefill / cache"), if self.config.show_ttft { crate::i18n::tr("Enabled (TTFT and t/s on a local server, cache share on a hosted API)").into() } else { crate::i18n::tr("Disabled").into() }),
            (crate::i18n::tr("Generation speed"), if self.config.show_tokens { crate::i18n::tr("Enabled (t/s while the model writes)").into() } else { crate::i18n::tr("Disabled").into() }),
            (crate::i18n::tr("Clipboard toasts"), if self.config.show_toasts { crate::i18n::tr("Enabled").into() } else { crate::i18n::tr("Disabled").into() }),
            (crate::i18n::tr("Animations"), if self.config.animations { crate::i18n::tr("Enabled (sweeps, pulses, unfolding panels)").into() } else { crate::i18n::tr("Reduced (spinners only)").into() }),
            (crate::i18n::tr("Color theme"), crate::i18n::tr(self.config.color_theme.label()).to_string()),
        ],
        SettingsTab::Reasoning => vec![
            (crate::i18n::tr("Thinking effort"), self.config.thinking_effort.clone()),
            (crate::i18n::tr("Context alert"), if self.config.context_warn_threshold > 0 { crate::i18n::tr("Warn at {}%").replace("{}", &self.config.context_warn_threshold.to_string()) } else { crate::i18n::tr("Disabled").into() }),
            (crate::i18n::tr("Auto-compact history"), if self.config.auto_compact_context {
                // Zero follows the window; "0%" would read as "always".
                match self.config.context_compact_threshold {
                    0 => crate::i18n::tr("Enabled (auto: {}% for this window)").replace("{}", &flashagent_core::default_compact_threshold(self.context_capacity).to_string()),
                    pct => crate::i18n::tr("Enabled (at {pct}%)").replace("{pct}", &pct.to_string()),
                }
            } else { "Disabled".into() }),
            // Off by default: the warm-up is a request of its own, and only a
            // local server profits from it.
            (crate::i18n::tr("Context Auto Warm-up"), if self.config.auto_warm_prompt_cache {
                crate::i18n::tr("Enabled (the prefix is cached before the first message)").into()
            } else { crate::i18n::tr("Disabled").into() }),
            (crate::i18n::tr("Web tools"), if self.config.web_tools { crate::i18n::tr("Enabled (web_fetch, web_search)").into() } else { crate::i18n::tr("Disabled").into() }),
            // Not "on connection failure" any more: the retries also wait out the statuses
// a gateway returns while it is busy, and zero means zero. A label that named
// one of the two reasons was wrong about the other, and the setting it
// describes now does exactly what it says.
(crate::i18n::tr("Network retries"), {
                let n = crate::text::plural(self.config.network_retries, "retry", "retries");
                match self.config.network_retries {
                    0 => crate::i18n::tr("None · a rate limit is reported instead of waited out").into(),
                    _ => crate::i18n::tr("{n} · before anything is generated, so never twice").replace("{n}", &n),
                }
            }),
            // Last, and for those who know what the numbers do.
            (crate::i18n::tr("Sampling (advanced)"), {
                // Mirrors `Client::set_user_sampling`: the presets are tuned for local models.
                let maker_tuned = matches!(self.config.active_profile().protocol, flashagent_llm::ApiProtocol::Anthropic | flashagent_llm::ApiProtocol::Gemini);
                if maker_tuned && self.config.sampling_preset != flashagent_core::SamplingPreset::Custom {
                    crate::i18n::tr("{preset} · {protocol} chooses its own")
                        .replace("{preset}", crate::i18n::tr(self.config.sampling_preset.label()))
                        .replace("{protocol}", self.config.active_profile().protocol.label())
                } else {
                    crate::i18n::tr("{} · temperature {:.2}").replace("{}", crate::i18n::tr(self.config.sampling_preset.label())).replace("{:.2}", &format!("{:.2}", self.config.temperature))
                }
            }),
        ],
        SettingsTab::Goal => vec![
            (crate::i18n::tr("Step limit"), match self.config.goal_max_steps {
                None | Some(0) => crate::i18n::tr("Unlimited").to_string(),
                Some(n) => crate::text::plural(n as usize, "step", "steps"),
            }),
            (crate::i18n::tr("Time limit"), goal_minutes_label(self.config.goal_max_minutes)),
            (crate::i18n::tr("Token limit"), goal_tokens_label(self.config.goal_max_output_tokens)),
        ],
        SettingsTab::Style => {
            let p = &self.config.personality;
            let mut rows = vec![(crate::i18n::tr("Base style and tone"), format!("{} — {}", crate::i18n::tr(p.base.label()), crate::i18n::tr(p.base.blurb())))];
            for t in PersonalityTrait::ALL {
                let level = p.level(t);
                let blurb = t.blurb(level);
                // The trait, its level and what the level means: all three are
                // the interface's words.
                rows.push((
                    crate::i18n::tr(t.label()),
                    if blurb.is_empty() {
                        crate::i18n::tr(level.label()).to_string()
                    } else {
                        format!("{} — {}", crate::i18n::tr(level.label()), crate::i18n::tr(blurb))
                    },
                ));
            }
            rows
        }
        SettingsTab::Tools => vec![
            (crate::i18n::tr("Tools on/off"), {
                let off = self.config.disabled_tools.len();
                if off == 0 {
                    crate::i18n::tr("Every tool is on").into()
                } else {
                    crate::i18n::tr("{} switched off").replace("{}", &crate::text::plural(off, "tool", "tools"))
                }
            }),
            (crate::i18n::tr("MCP servers"), crate::i18n::tr("Configured servers and the marketplace").into()),
            (crate::i18n::tr("Run tool test"), self.tool_test_status.clone().unwrap_or_else(|| crate::i18n::tr("Probe function calling").into())),
        ],
    }
    }

    pub fn total_items(&self) -> usize {
        self.items().len()
    }

    pub fn handle_key(&mut self, code: KeyCode, _mods: KeyModifiers) -> SettingsAction {
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
                0 => SettingsAction::OpenProviders,
                1 => SettingsAction::OpenModelMenu,
                2 => {
                    self.config.permission_mode = self.config.permission_mode.next();
                    SettingsAction::None
                }
                3 => {
                    self.config.safe_mode = !self.config.safe_mode;
                    SettingsAction::None
                }
                4 => {
                    self.config.crash_reports = !self.config.crash_reports;
                    SettingsAction::None
                }
                5 => {
                    self.config.auto_save_sessions = !self.config.auto_save_sessions;
                    SettingsAction::None
                }
                6 => {
                    self.cycle_editor();
                    SettingsAction::None
                }
                7 => {
                    self.cycle_language();
                    SettingsAction::None
                }
                8 => SettingsAction::OpenWizard,
                _ => SettingsAction::None,
            },
            SettingsTab::Updates => match self.selected_index {
                0 => {
                    self.config.auto_check_updates = !self.config.auto_check_updates;
                    SettingsAction::None
                }
                1 => {
                    self.config.update_channel = self.config.update_channel.toggle();
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
                    SettingsAction::None
                }
                1 => {
                    self.config.show_tips = !self.config.show_tips;
                    SettingsAction::None
                }
                2 => {
                    self.config.show_ttft = !self.config.show_ttft;
                    SettingsAction::None
                }
                3 => {
                    self.config.show_tokens = !self.config.show_tokens;
                    SettingsAction::None
                }
                4 => {
                    self.config.show_toasts = !self.config.show_toasts;
                    SettingsAction::None
                }
                5 => {
                    self.config.animations = !self.config.animations;
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
                    SettingsAction::None
                }
                2 => {
                    self.config.auto_compact_context = !self.config.auto_compact_context;
                    SettingsAction::None
                }
                3 => {
                    self.config.auto_warm_prompt_cache = !self.config.auto_warm_prompt_cache;
                    SettingsAction::None
                }
                4 => {
                    self.config.web_tools = !self.config.web_tools;
                    SettingsAction::None
                }
                5 => {
                    self.cycle_network_retries();
                    SettingsAction::None
                }
                6 => SettingsAction::OpenSamplingMenu,
                _ => SettingsAction::None,
            },
            SettingsTab::Tools => match self.selected_index {
                0 => SettingsAction::OpenTools,
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
                0 => self.cycle_provider(delta),
                1 => self.cycle_model(),
                2 => self.config.permission_mode = self.config.permission_mode.next(),
                3 => self.config.safe_mode = !self.config.safe_mode,
                4 => self.config.crash_reports = !self.config.crash_reports,
                5 => self.config.auto_save_sessions = !self.config.auto_save_sessions,
                6 => self.cycle_editor(),
                7 => self.cycle_language(),
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
                3 => self.config.auto_warm_prompt_cache = !self.config.auto_warm_prompt_cache,
                4 => self.config.web_tools = !self.config.web_tools,
                5 => self.cycle_network_retries(),
                6 => self.cycle_sampling(),
                _ => {}
            },
            // Nothing on this tab is a value: the rows open other screens.
            SettingsTab::Tools => {}
        }
    }

    /// Among the saved providers; the switch happens when Settings closes.
    fn cycle_provider(&mut self, delta: i32) {
        let names: Vec<String> = self.config.providers.iter().map(|p| p.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        let n = names.len();
        let next = match names.iter().position(|name| *name == self.config.active_profile().name) {
            Some(i) if delta < 0 => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None => 0,
        };
        self.config.activate(&names[next]);
    }

    /// The models of the server in use; not while another provider is picked
    /// above, whose models are not known yet.
    fn cycle_model(&mut self) {
        if self.available_models.is_empty() || self.provider_picked() {
            return;
        }
        let cur = self.config.active_profile().model.clone();
        let next_idx = self.available_models.iter().position(|m| *m == cur)
            .map(|i| (i + 1) % self.available_models.len())
            .unwrap_or(0);
        self.config.active_profile_mut().model = self.available_models[next_idx].clone();
    }

    /// Another provider than the one Settings opened on.
    fn provider_picked(&self) -> bool {
        self.opened_on.as_deref().is_some_and(|name| name != self.config.active_profile().name)
    }

    fn cycle_editor(&mut self) {
        let editors = ["$EDITOR", "code", "cursor", "nvim", "zed", "nano"];
        let cur = self.config.external_editor.as_str();
        let next_idx = editors.iter().position(|&e| e == cur)
            .map(|i| (i + 1) % editors.len())
            .unwrap_or(0);
        self.config.external_editor = editors[next_idx].to_string();
    }

    /// The interface language, English and Russian. It applies at once: the
    /// footer hints, /help and the tips speak it from the next frame, and the
    /// config keeps it for the next start.
    fn cycle_language(&mut self) {
        let next = match crate::i18n::Lang::from_code(&self.config.interface_language) {
            crate::i18n::Lang::Russian => crate::i18n::Lang::English,
            crate::i18n::Lang::English => crate::i18n::Lang::Russian,
        };
        self.config.interface_language = next.code().to_string();
        crate::i18n::set(next);
    }

    /// Steps through what the model in use really accepts. A fixed ladder would
    /// stop at `high` and never reach the `xhigh` or `max` some models offer,
    /// and would offer levels the API refuses on a model that has fewer.
    fn cycle_effort(&mut self) {
        let presets: Vec<String> =
            if self.effort_choices.is_empty() { effort_choices_defaults() } else { self.effort_choices.clone() };
        let cur = self.config.thinking_effort.as_str();
        let next_idx = presets.iter().position(|p| p.eq_ignore_ascii_case(cur)).map(|i| (i + 1) % presets.len());
        self.config.thinking_effort = match next_idx {
            Some(i) => presets[i].clone(),
            // A saved level this model does not offer: the first one it does.
            None => presets[0].clone(),
        };
    }

    /// Applied at once: a theme picked by name is guesswork until seen.
    fn cycle_theme(&mut self) {
        self.config.color_theme = self.config.color_theme.next();
        crate::theme::set(self.config.color_theme);
    }

    fn cycle_sampling(&mut self) {
        let next = self.config.sampling_preset.next();
        next.apply_to_config(&mut self.config);
    }

    fn cycle_warn_threshold(&mut self) {
        let thresholds = [70, 80, 85, 90, 0];
        let cur = self.config.context_warn_threshold;
        let next_idx = thresholds.iter().position(|&t| t == cur)
            .map(|i| (i + 1) % thresholds.len())
            .unwrap_or(0);
        self.config.context_warn_threshold = thresholds[next_idx];
    }

    fn cycle_network_retries(&mut self) {
        let retries = [1, 3, 5];
        let cur = self.config.network_retries;
        let next_idx = retries.iter().position(|&r| r == cur)
            .map(|i| (i + 1) % retries.len())
            .unwrap_or(0);
        self.config.network_retries = retries[next_idx];
    }

    /// The wheel moves the selection the way the arrows do, but stops at the
    /// ends instead of wrapping: a list run through with a hand should not
    /// jump back to its start. True when it moved.
    pub fn wheel(&mut self, dir: i32) -> bool {
        let total = self.total_items();
        if total == 0 {
            return false;
        }
        let was = self.selected_index;
        self.selected_index = (self.selected_index as isize + dir as isize).clamp(0, total as isize - 1) as usize;
        self.selected_index != was
    }

    /// A click on the card. `line` counts from the card's first row on screen
    /// (the title), `column` from the screen's left edge, `width` is the width
    /// the tabs line was fitted to. A row selects its item; the row already
    /// selected acts, as Enter would — a click is a focus first, an action
    /// second, so a stray press on the way to another row changes nothing.
    pub fn click(&mut self, line: usize, column: usize, width: usize) -> SettingsAction {
        if line == 1 {
            if let Some(tab) = Self::tab_at(width, column, self.active_tab) {
                if tab != self.active_tab {
                    self.active_tab = tab;
                    self.selected_index = 0;
                }
            }
            return SettingsAction::None;
        }
        // The title, the tabs and the separator under them belong to the box;
        // the first item is the fourth row of the card.
        let Some(idx) = line.checked_sub(FIRST_ITEM_LINE) else { return SettingsAction::None };
        if idx >= self.items().len() {
            // The note, the hints and the border: part of the card, not of a row.
            return SettingsAction::None;
        }
        if idx != self.selected_index {
            self.selected_index = idx;
            return SettingsAction::None;
        }
        self.trigger_action()
    }

    /// Which tab a click on the tabs line landed on, if one. The line is
    /// fitted and the labels measured the way `render` fits and measures
    /// them, so a click means what the screen shows.
    fn tab_at(width: usize, column: usize, active: SettingsTab) -> Option<SettingsTab> {
        let box_w = width.saturating_sub(6).min(100);
        let inner_text_w = box_w.saturating_sub(2);
        let label_of = |tab: &SettingsTab, numbers_only: bool| -> (String, usize) {
            let label = tab.label();
            let shown = if numbers_only { label.split(' ').next().unwrap_or(label).to_string() } else { label.to_string() };
            let cells = if *tab == active {
                // The one in use sits in brackets, full label even when the
                // others are their numbers.
                label.chars().count() + 2
            } else {
                shown.chars().count() + 2
            };
            (shown, cells)
        };
        let fits = |gap: usize, numbers_only: bool| -> bool {
            let gap_line = " ".repeat(gap);
            let width = SettingsTab::all()
                .iter()
                .map(|tab| label_of(tab, numbers_only).1)
                .sum::<usize>()
                + gap_line.len() * (SettingsTab::all().len() - 1)
                + 1;
            width <= inner_text_w
        };
        let (gap, numbers_only) = [(2usize, false), (1, false), (0, false)]
            .into_iter()
            .find(|(gap, numbers_only)| fits(*gap, *numbers_only))
            .unwrap_or((0, true));
        // pad_row puts the content four cells in; the joined labels carry a
        // leading space of their own.
        let mut col = 5usize;
        for (i, tab) in SettingsTab::all().iter().enumerate() {
            let (_, cells) = label_of(tab, numbers_only);
            if column >= col && column < col + cells {
                return Some(*tab);
            }
            col += cells;
            if i + 1 < SettingsTab::all().len() {
                col += gap;
            }
        }
        None
    }

    pub fn render(&self, width: usize, hover: Option<(usize, u16)>, flash: Option<usize>) -> Vec<RenderLine> {
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

        let title_styled = format!(" \x1b[1;38;2;225;175;95m{}\x1b[0m ", crate::i18n::tr("Settings"));
        let title_vis = crate::visible_width(&title_styled);
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
                        // The tab the pointer is over takes the colour of the
                        // one in use, without its brackets: a hover says a
                        // press would go there, not that it is there.
                        let hovered =
                            hover.is_some_and(|(line, col)| line == 1 && Self::tab_at(width, col as usize, self.active_tab) == Some(*tab));
                        if hovered {
                            format!("\x1b[38;2;185;150;90m {shown} \x1b[0m")
                        } else {
                            format!("\x1b[38;2;135;130;125m {shown} \x1b[0m")
                        }
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

        let hover_line = hover.map(|(l, _)| l);
        for (idx, (label, val_raw)) in items.iter().enumerate() {
            let card_line = FIRST_ITEM_LINE + idx;
            let is_sel = idx == self.selected_index;
            // The pointer is what a hover shows where nothing else moves; a
            // click just made lights the whole row amber for a moment, so a
            // press answers even where the state it changed does not show.
            let is_hover = hover_line == Some(card_line) && !is_sel && flash != Some(card_line);
            let is_press = flash == Some(card_line);
            let ptr = if is_sel || is_press {
                "\x1b[1;38;2;225;175;95m▸\x1b[0m"
            } else if is_hover {
                "\x1b[38;2;135;130;125m·\x1b[0m"
            } else {
                " "
            };
            let val = crate::truncate_middle(val_raw, max_val_w);
            let (label_styled, val_styled) = if is_press {
                (
                    format!("\x1b[1;38;2;225;175;95m{:<label_w$}\x1b[0m", label),
                    format!("\x1b[1;38;2;225;175;95m{val}\x1b[0m"),
                )
            } else if is_sel {
                (
                    format!("\x1b[1;38;2;240;235;225m{:<label_w$}\x1b[0m", label),
                    format!("\x1b[1;38;2;225;175;95m{val}\x1b[0m"),
                )
            } else if is_hover {
                (
                    format!("\x1b[38;2;200;195;185m{:<label_w$}\x1b[0m", label),
                    format!("\x1b[38;2;220;215;205m{val}\x1b[0m"),
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
            SettingsTab::Updates => format!("\x1b[38;2;225;175;95m{}\x1b[0m", crate::i18n::tr("* Takes effect after a restart")),
            SettingsTab::General => format!(
                "\x1b[38;2;135;130;125m{}\x1b[0m",
                crate::i18n::tr("←/→ on Provider switches between the saved ones; Enter edits them.")
            ),
            SettingsTab::Style => {
                format!(
                    "\x1b[38;2;135;130;125m{}\x1b[0m",
                    crate::i18n::tr("Tone only — tools and code are unaffected. Applies from your next message.")
                )
            }
            _ => String::new(),
        };
        for row in crate::wrap_styled(&note, inner_text_w.max(1)) {
            lines.push((LineKind::System, pad_row(&row)));
        }
        if note.is_empty() {
            lines.push((LineKind::System, pad_row("")));
        }

        let hints: &[(&str, &str)] = &[("Tab/1-7", "switch tab"), ("↑/↓", "move"), ("Enter/←/→", "change"), ("Esc", "save and close")];
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

    fn profile(presets: &[&str]) -> flashagent_llm::ThinkingProfile {
        flashagent_llm::ThinkingProfile {
            presets: presets.iter().map(|s| s.to_string()).collect(),
            protocol: flashagent_llm::ThinkingProtocol::Anthropic,
            supported: true,
            default_preset: presets.last().map(|s| s.to_string()),
        }
    }

    #[test]
    fn a_level_the_new_model_does_not_have_is_not_carried_across() {
        // Seen live: xhigh saved, a model listing [high, low, none], and the
        // card still claiming xhigh while the request asked for high.
        let short = profile(&["high", "low", "none"]);
        assert_eq!(effort_for_model("xhigh", Some(&short)), "auto");
        assert_eq!(effort_for_model("high", Some(&short)), "high");
        assert_eq!(effort_for_model("none", Some(&short)), "none");
        // The two words that are never a preset are left alone.
        assert_eq!(effort_for_model("auto", Some(&short)), "auto");
        assert_eq!(effort_for_model("default", Some(&short)), "default");
        assert_eq!(effort_for_model("", Some(&short)), "");
        // Case is not a difference between levels.
        assert_eq!(effort_for_model("HIGH", Some(&short)), "HIGH");
        // A model that offers more keeps the level it was given.
        let deep = profile(&["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(effort_for_model("xhigh", Some(&deep)), "xhigh");
    }

    #[test]
    fn nothing_known_about_the_model_keeps_the_level_it_was_given() {
        // Replacing a working setting with a guess helps nobody, and this is
        // the same rule `Client::resolve_effort` follows.
        assert_eq!(effort_for_model("xhigh", None), "xhigh");
        assert_eq!(effort_for_model("xhigh", Some(&flashagent_llm::ThinkingProfile::unreported())), "xhigh");
        assert_eq!(effort_for_model("xhigh", Some(&flashagent_llm::ThinkingProfile::unsupported())), "xhigh");
    }

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
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::empty()), SettingsAction::OpenTools);
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
        let shown = |v: &SettingsView| v.render(100, None, None).iter().map(|(_, t)| t.clone()).collect::<Vec<_>>().join("\n");
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
            let rendered = view.render(width, None, None);
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
            let rows = v.render(100, None, None);
            crate::strip_ansi(&rows[rows.len() - 2].1)
        };
        assert!(last_hint(&view).contains("Tab/1-7 switch tab"), "{}", last_hint(&view));
        assert!(last_hint(&view).trim_end_matches(['│', ' ']).ends_with("Esc save and close"), "{}", last_hint(&view));
        view.handle_key(KeyCode::Char('2'), KeyModifiers::empty());
        assert!(last_hint(&view).trim_end_matches(['│', ' ']).ends_with("Esc save and close"), "{}", last_hint(&view));
    }

    fn with_providers() -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.save_provider(flashagent_core::config::ProviderProfile::new("Home", flashagent_llm::ApiProtocol::OpenAi, "http://localhost:1234/v1"));
        cfg.save_provider(flashagent_core::config::ProviderProfile::new("Claude", flashagent_llm::ApiProtocol::Anthropic, "https://api.anthropic.com"));
        cfg.activate("Home");
        cfg
    }

    #[test]
    fn every_level_the_model_reports_can_be_reached_in_settings() {
        let cfg = AppConfig { thinking_effort: "auto".into(), ..AppConfig::default() };
        let mut view = SettingsView::new(cfg, Vec::new());
        // What a model that reasons deeply actually reports.
        view.effort_choices = ["auto", "off", "low", "medium", "high", "xhigh", "max"].iter().map(|s| s.to_string()).collect();
        let reached: Vec<String> = (0..12).map(|_| {
            view.cycle_effort();
            view.config.thinking_effort.clone()
        }).collect();
        for wanted in ["xhigh", "max", "low", "medium", "high"] {
            assert!(reached.contains(&wanted.to_string()), "{wanted} was never offered: {reached:?}");
        }
    }

    #[test]
    fn settings_never_offer_a_level_the_model_does_not_have() {
        let cfg = AppConfig { thinking_effort: "auto".into(), ..AppConfig::default() };
        let mut view = SettingsView::new(cfg, Vec::new());
        // A model with two levels only: offering `medium` would be refused.
        view.effort_choices = ["auto", "off", "high"].iter().map(|s| s.to_string()).collect();
        let reached: Vec<String> = (0..8).map(|_| {
            view.cycle_effort();
            view.config.thinking_effort.clone()
        }).collect();
        assert!(!reached.contains(&"medium".to_string()), "{reached:?}");
        assert!(!reached.contains(&"low".to_string()), "{reached:?}");
    }

    #[test]
    fn a_saved_level_the_new_model_lacks_falls_back_instead_of_staying_broken() {
        let cfg = AppConfig { thinking_effort: "xhigh".into(), ..AppConfig::default() };
        let mut view = SettingsView::new(cfg, Vec::new());
        view.effort_choices = ["auto", "off", "high"].iter().map(|s| s.to_string()).collect();
        view.cycle_effort();
        assert_eq!(view.config.thinking_effort, "auto", "an unsupported saved level must not linger");
    }

    /// The warm-up of the prompt cache is a request of its own, so it is off
    /// until the owner turns it on, and it lives with the other context
    /// settings in the LLM tab.
    #[test]
    fn the_cache_warm_up_is_off_until_it_is_turned_on_in_the_llm_tab() {
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        view.handle_key(KeyCode::Char('4'), KeyModifiers::empty());
        assert_eq!(view.active_tab, SettingsTab::Reasoning);
        assert!(!view.config.auto_warm_prompt_cache, "off by default");
        let rows: Vec<&str> = view.items().into_iter().map(|(name, _)| name).collect();
        let row = rows.iter().position(|name| *name == "Context Auto Warm-up").expect("the row is there");
        view.selected_index = row;
        view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(view.config.auto_warm_prompt_cache, "the row turns it on");
        let shown = view.items()[row].1.clone();
        assert!(shown.starts_with("Enabled"), "{shown}");
        // And back off again.
        view.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(!view.config.auto_warm_prompt_cache);
        assert!(view.items()[row].1.contains("Disabled"), "{}", view.items()[row].1);
    }

    #[test]
    fn the_provider_row_shows_the_one_in_use_and_the_arrows_go_through_the_saved_ones() {
        let mut view = SettingsView::new(with_providers(), vec!["model-a".into(), "model-b".into()]);
        let shown = |v: &SettingsView| v.render(120, None, None).iter().map(|(_, t)| crate::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(shown(&view).contains("Home · http://localhost:1234/v1"), "{}", shown(&view));
        assert!(!shown(&view).contains("restart"), "a provider applies at once: {}", shown(&view));
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.config.active_profile().name, "Claude");
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.config.active_profile().name, "Home", "round again");
        view.handle_key(KeyCode::Left, KeyModifiers::empty());
        assert_eq!(view.config.active_profile().name, "Claude");

        // The model list belongs to the server Settings opened on.
        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.config.active_profile().model, "", "{:?}", view.config.active_profile());
        view.handle_key(KeyCode::Up, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        view.handle_key(KeyCode::Down, KeyModifiers::empty());
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.config.active_profile().model, "model-a");

        view.handle_key(KeyCode::Up, KeyModifiers::empty());
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::empty()), SettingsAction::OpenProviders);
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
        let shown = view.render(100, None, None).iter().map(|(_, t)| crate::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(shown.contains("Friendly — Warm and chatty"), "{shown}");
        assert!(shown.contains("Less — Don't use as many emoji"), "{shown}");
        assert!(shown.lines().all(|l| crate::visible_width(l) <= 100), "{shown}");
    }
}

#[cfg(test)]
mod mouse_tests {
    use super::*;

    #[test]
    fn the_wheel_stops_at_the_ends_instead_of_wrapping() {
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        let total = view.total_items();
        view.wheel(-1);
        assert_eq!(view.selected_index, 0, "already at the top: the wheel does nothing");
        for _ in 0..total + 3 {
            view.wheel(1);
        }
        assert_eq!(view.selected_index, total - 1, "the end holds, however far the hand spins");
    }

    #[test]
    fn a_click_selects_its_row_and_the_selected_row_acts() {
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        // General tab: 0 provider, 1 model, 2 mode, 3 safe mode.
        assert_eq!(view.click(FIRST_ITEM_LINE + 3, 10, 100), SettingsAction::None);
        assert_eq!(view.selected_index, 3, "the first click only focuses the row");
        let before = view.config.safe_mode;
        assert_eq!(view.click(FIRST_ITEM_LINE + 3, 10, 100), SettingsAction::None);
        assert_eq!(view.config.safe_mode, !before, "the focused row acts, as Enter would");
        // The note, the hints and the border belong to the card, not to a row.
        assert_eq!(view.click(FIRST_ITEM_LINE + 40, 10, 100), SettingsAction::None);
        assert_eq!(view.selected_index, 3, "nothing below the rows moved the selection");
    }

    #[test]
    fn a_click_on_the_tabs_line_switches_to_the_label_it_landed_on() {
        let mut view = SettingsView::new(AppConfig::default(), Vec::new());
        // The span the Updates label takes, laid out the way render lays it out.
        let mut first = None;
        let mut last = None;
        for column in 0..120 {
            if SettingsView::tab_at(100, column, SettingsTab::General) == Some(SettingsTab::Updates) {
                first.get_or_insert(column);
                last = Some(column);
            }
        }
        let first = first.expect("the Updates label sits somewhere on the tabs line");
        let last = last.expect("and it ends");
        for column in first..=last {
            assert_eq!(SettingsView::tab_at(100, column, SettingsTab::General), Some(SettingsTab::Updates), "column {column}");
        }
        // The click does what the label says: the tab changes, its rows start over.
        assert_eq!(view.click(1, first, 100), SettingsAction::None);
        assert_eq!(view.active_tab, SettingsTab::Updates);
        assert_eq!(view.selected_index, 0);
        // Past the box's right border there is no label at all.
        assert_eq!(SettingsView::tab_at(100, 130, SettingsTab::General), None);
    }
}
