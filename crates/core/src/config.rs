//! Stored in `~/.flashagent/config.json`.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::permissions::PermissionMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendPreset {
    pub name: String,
    pub url: String,
    pub description: String,
}

impl BackendPreset {
    pub fn all() -> Vec<Self> {
        vec![
            Self {
                name: "LM Studio".into(),
                url: "http://localhost:1234/v1".into(),
                description: "Local LM Studio REST API (port 1234)".into(),
            },
            Self {
                name: "Ollama".into(),
                url: "http://localhost:11434/v1".into(),
                description: "Local Ollama server OpenAI-compatible endpoint".into(),
            },
            Self {
                name: "vLLM".into(),
                url: "http://localhost:8000/v1".into(),
                description: "High-throughput local vLLM OpenAI-compatible server".into(),
            },
            Self {
                name: "llama.cpp".into(),
                url: "http://localhost:8080/v1".into(),
                description: "Local llama.cpp (llama-server) OpenAI-compatible server".into(),
            },
            Self {
                name: "OpenRouter".into(),
                url: "https://openrouter.ai/api/v1".into(),
                description: "Unified cloud API for open and proprietary models".into(),
            },
        ]
    }
}

fn default_top_p() -> Option<f32> { Some(0.95) }
fn default_top_k() -> Option<u32> { Some(20) }
fn default_repeat_penalty() -> Option<f32> { Some(1.0) }
fn default_presence_penalty() -> Option<f32> { Some(0.0) }
fn default_min_p() -> Option<f32> { Some(0.0) }

/// Accepts any case or separators (`"Beta"`, `"accept_edits"`,
/// `"AcceptEdits"`): configs are edited by hand, and a strict spelling would
/// cost the user their settings.
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

    /// (temp, top_p, top_k, repeat_penalty, presence_penalty, min_p)
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub backend_url: String,
    pub api_key: Option<String>,
    pub model: String,
    /// The mode the user was in last time; Accept Edits on the first run.
    pub permission_mode: PermissionMode,
    /// "default", "off", "low", "medium", "high".
    pub thinking_effort: String,
    #[serde(default)]
    pub sampling_preset: SamplingPreset,
    pub temperature: f32,
    #[serde(default = "default_top_p")]
    pub top_p: Option<f32>,
    #[serde(default = "default_top_k")]
    pub top_k: Option<u32>,
    #[serde(default = "default_repeat_penalty")]
    pub repeat_penalty: Option<f32>,
    #[serde(default = "default_presence_penalty")]
    pub presence_penalty: Option<f32>,
    #[serde(default = "default_min_p")]
    pub min_p: Option<f32>,
    /// `None` means unlimited.
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Token budget for memory injection.
    pub token_budget: usize,
    /// On by default. A new key, so the old `free_search: false` that every
    /// earlier config was saved with does not keep the web tools off.
    #[serde(default = "default_true")]
    pub web_tools: bool,
    /// `None` means unlimited.
    #[serde(default)]
    pub goal_max_steps: Option<u32>,
    /// Minutes; `None` means unlimited.
    #[serde(default)]
    pub goal_max_minutes: Option<u32>,
    /// Generated tokens; `None` means unlimited.
    #[serde(default)]
    pub goal_max_output_tokens: Option<i64>,
    pub setup_completed: bool,
    #[serde(default)]
    pub toolset_profile: ToolsetProfile,
    /// Trust confirmation is skipped for these.
    #[serde(default)]
    pub trusted_directories: Vec<PathBuf>,
    #[serde(default = "default_update_channel")]
    pub update_channel: UpdateChannel,
    /// Check, download and install in the background.
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    #[serde(default = "default_true")]
    pub show_mascot: bool,
    #[serde(default = "default_true")]
    pub show_tips: bool,
    #[serde(default = "default_true")]
    pub show_ttft: bool,
    #[serde(default = "default_true")]
    pub show_tokens: bool,
    #[serde(default = "default_true")]
    pub show_toasts: bool,
    /// Settings → Style.
    #[serde(default)]
    pub personality: crate::personality::Personality,
    /// `--url` for this run only: (URL from the command line, URL from the
    /// file). Saving keeps the file's URL unless the user picked another in
    /// Settings, so a one-off launch does not move later launches.
    #[serde(skip)]
    pub url_override: Option<(String, String)>,
    /// Off keeps spinners but nothing decorative.
    #[serde(default = "default_true")]
    pub animations: bool,
    #[serde(default)]
    pub color_theme: ColorTheme,
    #[serde(default = "default_true")]
    pub auto_compact_context: bool,
    #[serde(default = "default_warn_threshold")]
    pub context_warn_threshold: usize,
    /// 0 chooses by window size.
    #[serde(default = "default_compact_threshold")]
    pub context_compact_threshold: usize,
    #[serde(default = "default_true")]
    pub auto_save_sessions: bool,
    /// $EDITOR, code, cursor, nvim.
    #[serde(default = "default_editor")]
    pub external_editor: String,
    #[serde(default = "default_retries")]
    pub network_retries: usize,
    /// So an update announces what arrived exactly once.
    #[serde(default)]
    pub last_seen_version: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_warn_threshold() -> usize {
    70
}

fn default_compact_threshold() -> usize {
    // Zero means "choose by the window": 85% of 128k, 97% of a million, 75% of 32k.
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

/// Every card is written in the `Dark` palette; a theme transforms it at
/// print time, so new colours are themed without registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorTheme {
    /// Warm greys, gold accents.
    #[default]
    Dark,
    /// Cooled towards blue.
    Midnight,
    /// Pushed away from mid-grey, for bright rooms and weak screens.
    HighContrast,
    /// Greys only, for e-ink and print.
    Monochrome,
    /// The terminal's own 16 colours, for terminals without 24-bit colour.
    Ansi16,
    /// For a white or light terminal background.
    Light,
}

impl ColorTheme {
    pub fn all() -> &'static [ColorTheme] {
        &[
            ColorTheme::Dark,
            ColorTheme::Midnight,
            ColorTheme::HighContrast,
            ColorTheme::Monochrome,
            ColorTheme::Ansi16,
            ColorTheme::Light,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Midnight => "Midnight",
            Self::HighContrast => "High contrast",
            Self::Monochrome => "Monochrome",
            Self::Ansi16 => "Terminal 16 colors",
            Self::Light => "Light background",
        }
    }

    pub fn next(self) -> Self {
        let all = Self::all();
        let i = all.iter().position(|t| *t == self).unwrap_or(0);
        all[(i + 1) % all.len()]
    }
}

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
            permission_mode: PermissionMode::AcceptEdits,
            thinking_effort: "auto".to_string(),
            sampling_preset: SamplingPreset::Coding,
            temperature: 0.60,
            top_p: Some(0.95),
            top_k: Some(20),
            repeat_penalty: Some(1.00),
            presence_penalty: Some(0.00),
            min_p: Some(0.00),
            max_steps: None, // no step limit per turn
            token_budget: 2000,
            web_tools: true,
            goal_max_steps: None,
            goal_max_minutes: None,
            goal_max_output_tokens: None,
            setup_completed: false,
            toolset_profile: ToolsetProfile::Auto,
            trusted_directories: Vec::new(),
            update_channel: default_update_channel(),
            auto_check_updates: true,
            show_mascot: true,
            show_tips: true,
            show_ttft: true,
            show_tokens: true,
            show_toasts: true,
            animations: true,
            url_override: None,
            personality: Default::default(),
            color_theme: ColorTheme::default(),
            auto_compact_context: true,
            context_warn_threshold: 70,
            context_compact_threshold: 0,
            auto_save_sessions: true,
            external_editor: "$EDITOR".to_string(),
            network_retries: 3,
            last_seen_version: None,
        }
    }
}

impl AppConfig {
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("FLASHAGENT_CONFIG_PATH") {
            if !custom.trim().is_empty() {
                return Some(PathBuf::from(custom));
            }
        }
        // Test binaries must never read or overwrite the real config.
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

    /// Default when missing or invalid.
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

    /// One bad field must not cost the other thirty: failing fields fall back to
    /// their default and are named in the returned list. Only a file that is not
    /// a JSON object is a total loss.
    pub fn from_json_str(content: &str) -> (Self, Vec<String>) {
        if let Ok(cfg) = serde_json::from_str::<Self>(content) {
            return (cfg.normalised(), Vec::new());
        }

        let Ok(serde_json::Value::Object(user)) = serde_json::from_str::<serde_json::Value>(content)
        else {
            return (Self::default(), vec!["the file (it is not valid JSON)".to_string()]);
        };

        // Put fields back one at a time onto the defaults; whatever does not survive
        // a round-trip is broken.
        let mut base = match serde_json::to_value(Self::default()) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => return (Self::default(), vec!["the file".to_string()]),
        };
        let mut rejected = Vec::new();
        for (key, value) in user {
            // Unknown keys are dropped by serde; only known ones are worth reporting.
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

    fn normalised(mut self) -> Self {
        if self.toolset_profile == ToolsetProfile::Compact {
            self.toolset_profile = ToolsetProfile::Auto;
        }
        if self.thinking_effort == "default" || self.thinking_effort.is_empty() {
            self.thinking_effort = "auto".to_string();
        }
        // 90 was what every config shipped with, not a choice, and it is wrong at
        // both ends of the window range, so it becomes "choose by the window". Any
        // other number was picked by hand and is kept.
        if self.context_compact_threshold == 90 {
            self.context_compact_threshold = 0;
        }
        self
    }

    pub fn is_directory_trusted(&self, dir: &Path) -> bool {
        let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.trusted_directories.iter().any(|d| {
            let d_canon = std::fs::canonicalize(d).unwrap_or_else(|_| d.clone());
            d_canon == canon
        })
    }

    pub fn trust_directory(&mut self, dir: &Path) {
        let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if !self.is_directory_trusted(&canon) {
            self.trusted_directories.push(canon);
        }
    }

    pub fn save(&self) -> Result<(), std::io::Error> {
        match Self::default_path() {
            Some(path) => self.save_to(&path),
            None => Ok(()),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = match &self.url_override {
            Some((cli, on_disk)) if *cli == self.backend_url => {
                let mut stored = self.clone();
                stored.backend_url = on_disk.clone();
                serde_json::to_string_pretty(&stored)
            }
            _ => serde_json::to_string_pretty(self),
        }
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Written beside the file and renamed over it: a half-written config reads
        // as no config and sends the user back through the setup wizard.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, path)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

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
    fn saving_replaces_the_config_whole_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = AppConfig::default();
        cfg.save_to(&path).unwrap();
        cfg.backend_url = "http://second/v1".into();
        cfg.save_to(&path).unwrap();
        assert_eq!(AppConfig::load_from(&path).unwrap().backend_url, "http://second/v1");
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(files.len(), 1, "a temporary file was left behind: {files:?}");
    }

    #[test]
    fn a_url_given_for_one_run_is_not_saved_but_one_chosen_later_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = AppConfig { backend_url: "http://on-disk/v1".into(), ..Default::default() };
        cfg.url_override = Some(("http://cli/v1".into(), cfg.backend_url.clone()));
        cfg.backend_url = "http://cli/v1".into();
        cfg.save_to(&path).unwrap();
        assert_eq!(AppConfig::load_from(&path).unwrap().backend_url, "http://on-disk/v1");

        cfg.backend_url = "http://picked-in-settings/v1".into();
        cfg.save_to(&path).unwrap();
        assert_eq!(AppConfig::load_from(&path).unwrap().backend_url, "http://picked-in-settings/v1");
    }

    #[test]
    fn stored_values_are_read_whatever_their_case() {
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
    fn web_tools_are_on_even_for_a_config_that_had_opted_out_by_default() {
        // Every earlier config was saved with the old flag set to false.
        let (cfg, rejected) = AppConfig::from_json_str(r#"{"model":"kept","free_search":false}"#);
        assert!(rejected.is_empty(), "{rejected:?}");
        assert!(cfg.web_tools);
        assert!(AppConfig::default().web_tools);
        let (off, _) = AppConfig::from_json_str(r#"{"web_tools":false}"#);
        assert!(!off.web_tools);
    }

    #[test]
    fn goal_limits_are_unlimited_unless_set() {
        let cfg = AppConfig::default();
        assert_eq!((cfg.goal_max_steps, cfg.goal_max_minutes, cfg.goal_max_output_tokens), (None, None, None));
        let (set, rejected) = AppConfig::from_json_str(
            r#"{"goal_max_steps":40,"goal_max_minutes":30,"goal_max_output_tokens":200000}"#,
        );
        assert!(rejected.is_empty(), "{rejected:?}");
        assert_eq!((set.goal_max_steps, set.goal_max_minutes, set.goal_max_output_tokens), (Some(40), Some(30), Some(200_000)));
    }

    #[test]
    fn the_old_shipped_threshold_becomes_the_automatic_one() {
        let (cfg, _) = AppConfig::from_json_str(r#"{"context_compact_threshold":90}"#);
        assert_eq!(cfg.context_compact_threshold, 0, "0 means choose by the window");

        let (chosen, _) = AppConfig::from_json_str(r#"{"context_compact_threshold":60}"#);
        assert_eq!(chosen.context_compact_threshold, 60);
        assert_eq!(AppConfig::default().context_compact_threshold, 0);
    }

    #[test]
    fn one_bad_field_does_not_cost_the_user_the_others() {
        // The old loader dropped the whole file on any error.
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
        // Unknown keys (older builds, typos) are not reported.
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
        let cfg = AppConfig {
            model: "test-model".into(),
            permission_mode: crate::PermissionMode::Bypass,
            goal_max_steps: Some(12),
            setup_completed: true,
            ..Default::default()
        };

        cfg.save_to(&temp).expect("save should succeed");
        let loaded = AppConfig::load_from(&temp).expect("load should succeed");
        assert_eq!(loaded.model, "test-model");
        assert_eq!(loaded.permission_mode, crate::PermissionMode::Bypass, "the last mode, Accept All included, comes back");
        assert_eq!(loaded.goal_max_steps, Some(12));
        assert!(loaded.setup_completed);
        let _ = std::fs::remove_file(&temp);
    }

    #[test]
    fn test_backwards_compatible_deserialization_from_older_schema() {
        // Missing fields added later; "language" and "silent_update_check" no longer
        // exist.
        let old_json = r#"{
            "backend_url": "http://localhost:1234/v1",
            "model": "qwen2.5-coder-32b",
            "language": "ru",
            "silent_update_check": false,
            "setup_completed": true
        }"#;

        let cfg: AppConfig = serde_json::from_str(old_json).expect("older schema must deserialize cleanly");
        assert_eq!(cfg.model, "qwen2.5-coder-32b");
        assert_eq!(cfg.backend_url, "http://localhost:1234/v1");
        assert!(cfg.setup_completed, "setup_completed flag must be preserved!");
        assert_eq!(cfg.sampling_preset, SamplingPreset::Coding);
        assert!(cfg.web_tools);
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

lenient_enum!(ColorTheme, "colour theme", {
    "dark" => ColorTheme::Dark,
    "default" => ColorTheme::Dark,
    "midnight" => ColorTheme::Midnight,
    "highcontrast" => ColorTheme::HighContrast,
    "contrast" => ColorTheme::HighContrast,
    "monochrome" => ColorTheme::Monochrome,
    "mono" => ColorTheme::Monochrome,
    "ansi16" => ColorTheme::Ansi16,
    "ansi" => ColorTheme::Ansi16,
    "light" => ColorTheme::Light,
    // Named by an older build that never used them.
    "monokai" => ColorTheme::Midnight,
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

