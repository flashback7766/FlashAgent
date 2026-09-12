//! Application configuration persistence and preset management.
//! Stored in `~/.flashagent/config.json`.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::permissions::PermissionMode;

/// Predefined backend endpoints for 1-click selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendPreset {
    pub name: String,
    pub url: String,
    pub description: String,
    pub default_port: u16,
}

impl BackendPreset {
    pub fn all() -> Vec<Self> {
        vec![
            Self {
                name: "LM Studio".into(),
                url: "http://localhost:1234/v1".into(),
                description: "Local LM Studio REST API (port 1234)".into(),
                default_port: 1234,
            },
            Self {
                name: "Ollama".into(),
                url: "http://localhost:11434/v1".into(),
                description: "Local Ollama server OpenAI-compatible endpoint".into(),
                default_port: 11434,
            },
            Self {
                name: "vLLM".into(),
                url: "http://localhost:8000/v1".into(),
                description: "High-throughput local vLLM OpenAI-compatible server".into(),
                default_port: 8000,
            },
            Self {
                name: "llama.cpp".into(),
                url: "http://localhost:8080/v1".into(),
                description: "Local llama.cpp (llama-server) OpenAI-compatible server".into(),
                default_port: 8080,
            },
            Self {
                name: "OpenRouter".into(),
                url: "https://openrouter.ai/api/v1".into(),
                description: "Unified cloud API for open and proprietary models".into(),
                default_port: 443,
            },
        ]
    }
}

fn default_top_p() -> Option<f32> { Some(0.95) }
fn default_top_k() -> Option<u32> { Some(20) }
fn default_repeat_penalty() -> Option<f32> { Some(1.0) }
fn default_presence_penalty() -> Option<f32> { Some(0.0) }
fn default_min_p() -> Option<f32> { Some(0.0) }

/// Accept a stored enum value whatever its case or separators — `"Beta"`,
/// `"beta"`, `"accept_edits"` and `"AcceptEdits"` all mean what they look
/// like, and a config is written by hand often enough that insisting on one
/// spelling only costs the user their settings.
macro_rules! lenient_enum {
    ($ty:ty, $name:expr, { $($text:literal => $variant:expr),+ $(,)? }) => {
        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                let key: String = raw
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                match key.as_str() {
                    $($text => Ok($variant),)+
                    _ => Err(serde::de::Error::custom(format!(
                        "unknown {} {raw:?}", $name
                    ))),
                }
            }
        }
    };
}

/// Predefined sampling presets for model generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub enum SamplingPreset {
    #[default]
    Coding,
    MtpCoding,
    Mtp,
    Chatting,
    MtpChatting,
    Precise,
    Gemma,
    Custom,
}

impl SamplingPreset {
    pub fn all() -> &'static [SamplingPreset] {
        &[
            SamplingPreset::Coding,
            SamplingPreset::MtpCoding,
            SamplingPreset::Mtp,
            SamplingPreset::Chatting,
            SamplingPreset::MtpChatting,
            SamplingPreset::Precise,
            SamplingPreset::Gemma,
            SamplingPreset::Custom,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Coding => "Default Coding",
            Self::MtpCoding => "MTP Coding",
            Self::Mtp => "Multi Token Prediction (MTP)",
            Self::Chatting => "Chatting",
            Self::MtpChatting => "MTP Chatting",
            Self::Precise => "Precise",
            Self::Gemma => "Gemma",
            Self::Custom => "Custom",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Coding => Self::MtpCoding,
            Self::MtpCoding => Self::Mtp,
            Self::Mtp => Self::Chatting,
            Self::Chatting => Self::MtpChatting,
            Self::MtpChatting => Self::Precise,
            Self::Precise => Self::Gemma,
            Self::Gemma => Self::Custom,
            Self::Custom => Self::Coding,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Coding => Self::Custom,
            Self::MtpCoding => Self::Coding,
            Self::Mtp => Self::MtpCoding,
            Self::Chatting => Self::Mtp,
            Self::MtpChatting => Self::Chatting,
            Self::Precise => Self::MtpChatting,
            Self::Gemma => Self::Precise,
            Self::Custom => Self::Gemma,
        }
    }

    /// Values: (temp, top_p, top_k, repeat_penalty, presence_penalty, min_p)
    pub fn values(self) -> (f32, f32, u32, f32, f32, f32) {
        match self {
            Self::Coding => (0.60, 0.95, 20, 1.00, 0.00, 0.00),
            Self::MtpCoding => (0.30, 0.90, 40, 1.00, 0.00, 0.05),
            Self::Mtp => (0.50, 0.92, 30, 1.02, 0.00, 0.03),
            Self::Chatting => (0.80, 0.95, 50, 1.10, 0.10, 0.00),
            Self::MtpChatting => (0.65, 0.88, 40, 1.05, 0.05, 0.02),
            Self::Precise => (0.10, 0.75, 10, 1.00, 0.00, 0.00),
            Self::Gemma => (1.00, 0.95, 64, 1.08, 0.02, 0.03),
            Self::Custom => (0.60, 0.95, 20, 1.00, 0.00, 0.00),
        }
    }

    pub fn apply_to_config(self, cfg: &mut AppConfig) {
        cfg.sampling_preset = self;
        if self != Self::Custom {
            let (temp, top_p, top_k, rep, pres, min_p) = self.values();
            cfg.temperature = temp;
            cfg.top_p = Some(top_p);
            cfg.top_k = Some(top_k);
            cfg.repeat_penalty = Some(rep);
            cfg.presence_penalty = Some(pres);
            cfg.min_p = Some(min_p);
        }
    }
}

/// Global FlashAgent application configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// LLM server base URL.
    pub backend_url: String,
    /// Optional API key for remote providers or authenticated endpoints.
    pub api_key: Option<String>,
    /// Selected default model ID.
    pub model: String,
    /// Interface language ("ru" or "en"); taken from the locale when unset.
    #[serde(default = "default_language")]
    pub language: String,
    /// Default permission mode on startup.
    pub permission_mode: PermissionMode,
    /// Default thinking effort preset ("default", "off", "low", "medium", "high").
    pub thinking_effort: String,
    /// Active sampling preset.
    #[serde(default)]
    pub sampling_preset: SamplingPreset,
    /// Generation temperature (default 0.60 for coding preset).
    pub temperature: f32,
    /// Top P sampling nucleus (default 0.95).
    #[serde(default = "default_top_p")]
    pub top_p: Option<f32>,
    /// Top K sampling pool size (default 20).
    #[serde(default = "default_top_k")]
    pub top_k: Option<u32>,
    /// Repeat penalty (default 1.0).
    #[serde(default = "default_repeat_penalty")]
    pub repeat_penalty: Option<f32>,
    /// Presence penalty (default 0.0).
    #[serde(default = "default_presence_penalty")]
    pub presence_penalty: Option<f32>,
    /// Min P sampling threshold (default 0.0).
    #[serde(default = "default_min_p")]
    pub min_p: Option<f32>,
    /// Maximum autonomous loop steps per turn (None = unlimited).
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Context token budget for memory injection.
    pub token_budget: usize,
    /// Whether free web search (DuckDuckGo) is enabled.
    pub free_search: bool,
    /// Whether the first start setup wizard has been completed.
    pub setup_completed: bool,
    /// Toolset exposure profile (Auto / Full / Compact).
    #[serde(default)]
    pub toolset_profile: ToolsetProfile,
    /// Trusted working directories where trust confirmation is bypassed.
    #[serde(default)]
    pub trusted_directories: Vec<PathBuf>,
    /// Update release channel (Beta / Stable).
    #[serde(default = "default_update_channel")]
    pub update_channel: UpdateChannel,
    /// Whether background auto-check for updates is enabled.
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    /// Whether silent daily update notification check without auto-download is enabled.
    #[serde(default = "default_true")]
    pub silent_update_check: bool,
    /// Whether animated swift mascot is shown in welcome card.
    #[serde(default = "default_true")]
    pub show_mascot: bool,
    /// Whether rotating tips animation is enabled.
    #[serde(default = "default_true")]
    pub show_tips: bool,
    /// Whether TTFT (Time To First Token) and prefill speed metrics are displayed.
    #[serde(default = "default_true")]
    pub show_ttft: bool,
    /// Whether prompt/generation token counters are shown in status bar.
    #[serde(default = "default_true")]
    pub show_tokens: bool,
    /// Whether clipboard and notification toasts are shown.
    #[serde(default = "default_true")]
    pub show_toasts: bool,
    /// Whether reasoning/thinking accordion box is shown.
    #[serde(default = "default_true")]
    pub show_reasoning_accordion: bool,
    /// Color theme name ("dark", "midnight", "monokai", "high_contrast", "monochrome", "ansi16").
    #[serde(default = "default_theme")]
    pub color_theme: String,
    /// Approval gate policy: "destructive" (default), "all", "auto".
    #[serde(default = "default_approval_mode")]
    pub approval_mode: String,
    /// Whether automatic context compaction (/compact) is enabled.
    #[serde(default = "default_true")]
    pub auto_compact_context: bool,
    /// Warning threshold percent for context window usage (default 70).
    #[serde(default = "default_warn_threshold")]
    pub context_warn_threshold: usize,
    /// Auto-compaction threshold percent, or 0 to choose by window size.
    #[serde(default = "default_compact_threshold")]
    pub context_compact_threshold: usize,
    /// Whether sessions are automatically saved on exit.
    #[serde(default = "default_true")]
    pub auto_save_sessions: bool,
    /// Preferred external editor command ($EDITOR, code, cursor, nvim).
    #[serde(default = "default_editor")]
    pub external_editor: String,
    /// Whether compact git diff preview is shown before edits.
    #[serde(default = "default_true")]
    pub git_diff_preview: bool,
    /// Whether smart commit message generation (/commit) is offered.
    #[serde(default = "default_true")]
    pub git_smart_commit: bool,
    /// Number of network retry attempts for LLM requests (default 3).
    #[serde(default = "default_retries")]
    pub network_retries: usize,
    /// Last version whose changes were shown to the user, so an update can
    /// tell them what arrived exactly once.
    #[serde(default)]
    pub last_seen_version: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_approval_mode() -> String {
    "destructive".to_string()
}

fn default_warn_threshold() -> usize {
    70
}

/// The language the interface writes in when nothing has been chosen.
///
/// Read from the locale rather than assumed: the shipped default was Russian,
/// which meant everyone else met a Russian-labelled app until they typed
/// enough of their own language for it to be detected.
fn default_language() -> String {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default()
        .to_lowercase();
    if locale.starts_with("ru") {
        "ru".to_string()
    } else {
        "en".to_string()
    }
}

fn default_compact_threshold() -> usize {
    // Zero means "choose by the window": 85% of a 128k window, 97% of a
    // million, 75% of 32k. A single percentage cannot suit both ends.
    0
}

fn default_editor() -> String {
    "$EDITOR".to_string()
}

fn default_retries() -> usize {
    3
}

fn default_update_channel() -> UpdateChannel {
    let ver = option_env!("FLASHAGENT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    if ver.starts_with('b') {
        UpdateChannel::Beta
    } else {
        UpdateChannel::Stable
    }
}

/// Release channel for application updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Beta,
    Stable,
}

impl UpdateChannel {
    pub fn toggle(self) -> Self {
        match self {
            Self::Beta => Self::Stable,
            Self::Stable => Self::Beta,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Beta => "Beta",
            Self::Stable => "Stable",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Beta => "beta",
            Self::Stable => "stable",
        }
    }
}

/// Toolset exposure profile for adjusting advertised tools and schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub enum ToolsetProfile {
    #[default]
    Auto,
    Full,
    Compact,
}

impl ToolsetProfile {
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Full,
            Self::Full => Self::Compact,
            Self::Compact => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Full => "Full",
            Self::Compact => "Compact",
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            backend_url: "http://localhost:1234/v1".to_string(),
            api_key: None,
            model: String::new(),
            language: default_language(),
            permission_mode: PermissionMode::AcceptEdits,
            thinking_effort: "auto".to_string(),
            sampling_preset: SamplingPreset::Coding,
            temperature: 0.60,
            top_p: Some(0.95),
            top_k: Some(20),
            repeat_penalty: Some(1.00),
            presence_penalty: Some(0.00),
            min_p: Some(0.00),
            max_steps: None, // Unlimited steps per turn!
            token_budget: 2000,
            free_search: false, // PHILOSOPHY.md §3: local-first by default; web tools require opt-in
            setup_completed: false,
            toolset_profile: ToolsetProfile::Auto,
            trusted_directories: Vec::new(),
            update_channel: default_update_channel(),
            auto_check_updates: true,
            silent_update_check: true,
            show_mascot: true,
            show_tips: true,
            show_ttft: true,
            show_tokens: true,
            show_toasts: true,
            show_reasoning_accordion: true,
            color_theme: "dark".to_string(),
            approval_mode: "destructive".to_string(),
            auto_compact_context: true,
            context_warn_threshold: 70,
            context_compact_threshold: 0,
            auto_save_sessions: true,
            external_editor: "$EDITOR".to_string(),
            git_diff_preview: true,
            git_smart_commit: true,
            network_retries: 3,
            last_seen_version: None,
        }
    }
}

impl AppConfig {
    /// Path to user configuration file `~/.flashagent/config.json`.
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("FLASHAGENT_CONFIG_PATH") {
            if !custom.trim().is_empty() {
                return Some(PathBuf::from(custom));
            }
        }
        // Safety guard: test runner binaries (e.g. target/debug/deps/...) must never
        // overwrite or read real user configuration in ~/.flashagent/config.json.
        if let Ok(exe) = std::env::current_exe() {
            let exe_str = exe.to_string_lossy();
            if exe_str.contains("/deps/") || exe_str.contains("\\deps\\") {
                return Some(std::env::temp_dir().join("flashagent_test_runner_config.json"));
            }
        }
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .map(|h| PathBuf::from(h).join(".flashagent").join("config.json"))
    }

    /// Load configuration from disk, or return default if missing/invalid.
    pub fn load() -> Self {
        if let Some(path) = Self::default_path() {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let (cfg, rejected) = Self::from_json_str(&content);
                    for field in &rejected {
                        eprintln!(
                            "Warning: {} in {} is not valid; using the default for it. \
                             Every other setting was kept.",
                            field,
                            path.display()
                        );
                    }
                    return cfg;
                }
            }
        }
        Self::default()
    }

    /// Parse a config file, keeping every field that is valid.
    ///
    /// A config is a long list of independent settings, so one bad entry must
    /// not cost the user the other thirty: fields that fail are replaced by
    /// their default and named in the returned list, and the rest are applied.
    /// A file that is not a JSON object at all is the only total loss.
    pub fn from_json_str(content: &str) -> (Self, Vec<String>) {
        if let Ok(cfg) = serde_json::from_str::<Self>(content) {
            return (cfg.normalised(), Vec::new());
        }

        let Ok(serde_json::Value::Object(user)) = serde_json::from_str::<serde_json::Value>(content)
        else {
            return (Self::default(), vec!["the file (it is not valid JSON)".to_string()]);
        };

        // Start from the defaults and put back one field at a time; whatever
        // does not survive a round-trip is the field that is broken.
        let mut base = match serde_json::to_value(Self::default()) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => return (Self::default(), vec!["the file".to_string()]),
        };
        let mut rejected = Vec::new();
        for (key, value) in user {
            // Unknown keys are ignored by serde anyway; only known ones can
            // break, and only they are worth reporting.
            if !base.contains_key(&key) {
                continue;
            }
            let previous = base.insert(key.clone(), value);
            if serde_json::from_value::<Self>(serde_json::Value::Object(base.clone())).is_err() {
                if let Some(old) = previous {
                    base.insert(key.clone(), old);
                }
                rejected.push(key);
            }
        }

        let cfg = serde_json::from_value::<Self>(serde_json::Value::Object(base))
            .unwrap_or_default();
        (cfg.normalised(), rejected)
    }

    /// Settings that are stored but no longer meaningful on their own.
    fn normalised(mut self) -> Self {
        if self.toolset_profile == ToolsetProfile::Compact {
            self.toolset_profile = ToolsetProfile::Auto;
        }
        if self.thinking_effort == "default" || self.thinking_effort.is_empty() {
            self.thinking_effort = "auto".to_string();
        }
        // 90 was what every config shipped with rather than anything anyone
        // chose, and it is wrong at both ends of the range; it becomes
        // "choose by the window". A threshold that really was picked by hand
        // is any other number, and is kept.
        if self.context_compact_threshold == 90 {
            self.context_compact_threshold = 0;
        }
        self
    }

    /// Check if a directory is already in the trusted directories list.
    pub fn is_directory_trusted(&self, dir: &Path) -> bool {
        let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.trusted_directories.iter().any(|d| {
            let d_canon = std::fs::canonicalize(d).unwrap_or_else(|_| d.clone());
            d_canon == canon
        })
    }

    /// Mark a directory as trusted and deduplicate.
    pub fn trust_directory(&mut self, dir: &Path) {
        let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if !self.is_directory_trusted(&canon) {
            self.trusted_directories.push(canon);
        }
    }

    /// Save configuration to disk.
    pub fn save(&self) -> Result<(), std::io::Error> {
        if let Some(path) = Self::default_path() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let json = serde_json::to_string_pretty(self)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            std::fs::write(&path, json)?;
        }
        Ok(())
    }

    /// Save to explicit file path (useful for testing).
    pub fn save_to(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load from explicit file path.
    pub fn load_from(path: &Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        let cfg = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stored_values_are_read_whatever_their_case() {
        // A config is edited by hand; "Beta" must not mean something else
        // than "beta", and the mode may be copied from the label in the app.
        let (cfg, rejected) = AppConfig::from_json_str(
            r#"{"update_channel":"Beta","permission_mode":"Accept Edits","sampling_preset":"MTP_Coding","toolset_profile":"FULL"}"#,
        );
        assert!(rejected.is_empty(), "{rejected:?}");
        assert_eq!(cfg.update_channel, UpdateChannel::Beta);
        assert_eq!(cfg.permission_mode, crate::PermissionMode::AcceptEdits);
        assert_eq!(cfg.sampling_preset, SamplingPreset::MtpCoding);
        assert_eq!(cfg.toolset_profile, ToolsetProfile::Full);
    }

    #[test]
    fn the_interface_language_follows_the_locale() {
        // The shipped default was Russian for everyone, so an English speaker
        // met "Разбираю запрос" on their first run.
        let restore = std::env::var("LANG").ok();
        std::env::set_var("LANG", "ru_RU.UTF-8");
        std::env::remove_var("LC_ALL");
        std::env::remove_var("LC_MESSAGES");
        assert_eq!(AppConfig::default().language, "ru");
        std::env::set_var("LANG", "en_GB.UTF-8");
        assert_eq!(AppConfig::default().language, "en");
        std::env::set_var("LANG", "de_DE.UTF-8");
        assert_eq!(AppConfig::default().language, "en", "anything we do not speak is English");
        match restore {
            Some(v) => std::env::set_var("LANG", v),
            None => std::env::remove_var("LANG"),
        }
    }

    #[test]
    fn the_old_shipped_threshold_becomes_the_automatic_one() {
        // 90 was the number every config was born with, not a decision, and
        // it is wrong at both ends: too early for a million-token window, too
        // late for 32k.
        let (cfg, _) = AppConfig::from_json_str(r#"{"context_compact_threshold":90}"#);
        assert_eq!(cfg.context_compact_threshold, 0, "0 means choose by the window");

        // A number someone actually typed is theirs.
        let (chosen, _) = AppConfig::from_json_str(r#"{"context_compact_threshold":60}"#);
        assert_eq!(chosen.context_compact_threshold, 60);
        assert_eq!(AppConfig::default().context_compact_threshold, 0);
    }

    #[test]
    fn one_bad_field_does_not_cost_the_user_the_others() {
        // The old loader threw the whole file away on any error, so a single
        // typo silently reset every setting.
        let (cfg, rejected) = AppConfig::from_json_str(
            r#"{"backend_url":"http://localhost:9999/v1","model":"my-model","temperature":0.2,
                "update_channel":"purple","context_warn_threshold":55}"#,
        );
        assert_eq!(rejected, vec!["update_channel".to_string()]);
        assert_eq!(cfg.backend_url, "http://localhost:9999/v1");
        assert_eq!(cfg.model, "my-model");
        assert_eq!(cfg.context_warn_threshold, 55);
        assert_eq!(
            cfg.update_channel,
            AppConfig::default().update_channel,
            "the broken field falls back to what a fresh config would have"
        );
    }

    #[test]
    fn a_wrongly_typed_field_is_reported_by_name() {
        let (cfg, rejected) = AppConfig::from_json_str(
            r#"{"model":"kept","auto_save_sessions":"yes please","token_budget":"lots"}"#,
        );
        assert_eq!(cfg.model, "kept");
        assert!(rejected.contains(&"auto_save_sessions".to_string()), "{rejected:?}");
        assert!(rejected.contains(&"token_budget".to_string()), "{rejected:?}");
    }

    #[test]
    fn a_file_that_is_not_json_falls_back_whole() {
        let (cfg, rejected) = AppConfig::from_json_str("this is not json at all");
        assert_eq!(cfg, AppConfig::default().normalised());
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn unknown_settings_are_ignored_rather_than_reported() {
        // A key from an older build, or a typo in a key name, is not
        // something the user can act on — and serde drops it either way.
        let (cfg, rejected) = AppConfig::from_json_str(
            r#"{"model":"kept","colour_theme":"dark","update_channel":"nope"}"#,
        );
        assert_eq!(cfg.model, "kept");
        assert_eq!(rejected, vec!["update_channel".to_string()]);
    }

    use super::*;

    #[test]
    fn test_config_default_and_roundtrip() {
        let temp = std::env::temp_dir().join("test_flashagent_config.json");
        let cfg = AppConfig { model: "test-model".into(), language: "ru".into(), setup_completed: true, ..Default::default() };

        cfg.save_to(&temp).expect("save should succeed");
        let loaded = AppConfig::load_from(&temp).expect("load should succeed");
        assert_eq!(loaded.model, "test-model");
        assert_eq!(loaded.language, "ru");
        assert!(loaded.setup_completed);
        let _ = std::fs::remove_file(&temp);
    }

    #[test]
    fn test_backwards_compatible_deserialization_from_older_schema() {
        // Minimal older configuration JSON without newly added fields:
        // missing: sampling_preset, top_p, top_k, max_steps, free_search, toolset_profile, trusted_directories.
        let old_json = r#"{
            "backend_url": "http://localhost:1234/v1",
            "model": "qwen2.5-coder-32b",
            "language": "ru",
            "setup_completed": true
        }"#;

        let cfg: AppConfig = serde_json::from_str(old_json).expect("older schema must deserialize cleanly");
        assert_eq!(cfg.model, "qwen2.5-coder-32b");
        assert_eq!(cfg.backend_url, "http://localhost:1234/v1");
        assert_eq!(cfg.language, "ru");
        assert!(cfg.setup_completed, "setup_completed flag must be preserved!");
        assert_eq!(cfg.sampling_preset, SamplingPreset::Coding);
        assert!(!cfg.free_search);
        assert_eq!(cfg.toolset_profile, ToolsetProfile::Auto);
        assert!(cfg.trusted_directories.is_empty());
    }

    #[test]
    fn test_trusted_directories_logic() {
        let mut cfg = AppConfig::default();
        let temp_dir = std::env::temp_dir();

        assert!(!cfg.is_directory_trusted(&temp_dir));
        cfg.trust_directory(&temp_dir);
        assert!(cfg.is_directory_trusted(&temp_dir));

        // Adding again should not duplicate
        let prev_len = cfg.trusted_directories.len();
        cfg.trust_directory(&temp_dir);
        assert_eq!(cfg.trusted_directories.len(), prev_len);
    }

    #[test]
    fn test_backend_presets_exist() {
        let presets = BackendPreset::all();
        assert!(presets.iter().any(|p| p.name == "LM Studio"));
        assert!(presets.iter().any(|p| p.name == "Ollama"));
        assert!(presets.iter().any(|p| p.name == "vLLM"));
    }
}

lenient_enum!(SamplingPreset, "sampling preset", {
    "coding" => SamplingPreset::Coding,
    "mtpcoding" => SamplingPreset::MtpCoding,
    "mtp" => SamplingPreset::Mtp,
    "chatting" => SamplingPreset::Chatting,
    "mtpchatting" => SamplingPreset::MtpChatting,
    "precise" => SamplingPreset::Precise,
    "gemma" => SamplingPreset::Gemma,
    "custom" => SamplingPreset::Custom,
});

lenient_enum!(UpdateChannel, "update channel", {
    "beta" => UpdateChannel::Beta,
    "stable" => UpdateChannel::Stable,
    "release" => UpdateChannel::Stable,
});

lenient_enum!(ToolsetProfile, "toolset profile", {
    "auto" => ToolsetProfile::Auto,
    "full" => ToolsetProfile::Full,
    "compact" => ToolsetProfile::Compact,
});

