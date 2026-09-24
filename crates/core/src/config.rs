//! Stored in `~/.flashagent/config.json`.

use std::path::{Path, PathBuf};
use flashagent_llm::{ApiProtocol, Endpoint};
use serde::{Deserialize, Serialize};
use crate::permissions::PermissionMode;

/// A known server: picking it fills in the address and how to talk to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendPreset {
    pub name: &'static str,
    pub protocol: ApiProtocol,
    pub url: &'static str,
    pub description: &'static str,
    /// Where a key is looked for when the provider has none saved, in order.
    /// Empty for a server on this computer, which needs none.
    pub key_env: &'static [&'static str],
}

/// Local servers first: FlashAgent is built for them. Two local servers can
/// share a default port (llama.cpp and LocalAI both take 8080).
const PRESETS: &[BackendPreset] = &[
    BackendPreset { name: "LM Studio", protocol: ApiProtocol::OpenAi, url: "http://localhost:1234/v1", description: "LM Studio's server; it reports the loaded model, its context and reasoning settings", key_env: &[] },
    BackendPreset { name: "Ollama", protocol: ApiProtocol::Ollama, url: "http://localhost:11434", description: "Local Ollama (native API: sets the context window)", key_env: &[] },
    BackendPreset { name: "llama.cpp", protocol: ApiProtocol::OpenAi, url: "http://localhost:8080/v1", description: "llama-server; start it with --jinja for tool calls", key_env: &[] },
    BackendPreset { name: "vLLM", protocol: ApiProtocol::OpenAi, url: "http://localhost:8000/v1", description: "vLLM's OpenAI-compatible server", key_env: &[] },
    BackendPreset { name: "Jan", protocol: ApiProtocol::OpenAi, url: "http://localhost:1337/v1", description: "Jan's local API server", key_env: &[] },
    BackendPreset { name: "KoboldCpp", protocol: ApiProtocol::OpenAi, url: "http://localhost:5001/v1", description: "KoboldCpp's OpenAI-compatible API", key_env: &[] },
    BackendPreset { name: "text-generation-webui", protocol: ApiProtocol::OpenAi, url: "http://localhost:5000/v1", description: "The oobabooga web UI, started with --api", key_env: &[] },
    BackendPreset { name: "LocalAI", protocol: ApiProtocol::OpenAi, url: "http://localhost:8080/v1", description: "LocalAI's OpenAI-compatible API", key_env: &[] },
    BackendPreset { name: "Anthropic", protocol: ApiProtocol::Anthropic, url: "https://api.anthropic.com", description: "Claude models through Anthropic's own API", key_env: &["ANTHROPIC_API_KEY"] },
    BackendPreset { name: "OpenAI", protocol: ApiProtocol::OpenAi, url: "https://api.openai.com/v1", description: "OpenAI's API", key_env: &["OPENAI_API_KEY"] },
    BackendPreset { name: "Gemini", protocol: ApiProtocol::Gemini, url: "https://generativelanguage.googleapis.com/v1beta", description: "Google Gemini through Google's own API", key_env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"] },
    BackendPreset { name: "OpenRouter", protocol: ApiProtocol::OpenAi, url: "https://openrouter.ai/api/v1", description: "Open and proprietary models behind one key", key_env: &["OPENROUTER_API_KEY"] },
    BackendPreset { name: "DeepSeek", protocol: ApiProtocol::OpenAi, url: "https://api.deepseek.com/v1", description: "DeepSeek's API", key_env: &["DEEPSEEK_API_KEY"] },
    BackendPreset { name: "Mistral", protocol: ApiProtocol::OpenAi, url: "https://api.mistral.ai/v1", description: "Mistral's API", key_env: &["MISTRAL_API_KEY"] },
    BackendPreset { name: "Groq", protocol: ApiProtocol::OpenAi, url: "https://api.groq.com/openai/v1", description: "Groq's API", key_env: &["GROQ_API_KEY"] },
    BackendPreset { name: "xAI", protocol: ApiProtocol::OpenAi, url: "https://api.x.ai/v1", description: "Grok models through xAI's API", key_env: &["XAI_API_KEY"] },
    BackendPreset { name: "Together", protocol: ApiProtocol::OpenAi, url: "https://api.together.xyz/v1", description: "Open models on Together AI", key_env: &["TOGETHER_API_KEY"] },
    BackendPreset { name: "Fireworks", protocol: ApiProtocol::OpenAi, url: "https://api.fireworks.ai/inference/v1", description: "Open models on Fireworks AI", key_env: &["FIREWORKS_API_KEY"] },
    BackendPreset { name: "Cerebras", protocol: ApiProtocol::OpenAi, url: "https://api.cerebras.ai/v1", description: "Cerebras' API", key_env: &["CEREBRAS_API_KEY"] },
];

/// Wherever a key is missing, the last place it is looked for.
pub const GENERAL_KEY_ENV: &str = "FLASHAGENT_API_KEY";

impl BackendPreset {
    pub fn all() -> &'static [BackendPreset] {
        PRESETS
    }

    /// A server on this computer; it needs no key.
    pub fn is_local(&self) -> bool {
        self.key_env.is_empty()
    }

    /// The preset at exactly this address, the first where two share it.
    pub fn for_url(url: &str) -> Option<&'static BackendPreset> {
        let url = normalize_url(url);
        PRESETS.iter().find(|p| p.url == url)
    }

    /// The preset of the same server, at this address or elsewhere on its
    /// host: Ollama's `/v1` is still Ollama.
    pub fn same_server(url: &str) -> Option<&'static BackendPreset> {
        Self::for_url(url).or_else(|| {
            let host = host_of(url)?;
            PRESETS.iter().find(|p| host_of(p.url).as_deref() == Some(host.as_str()))
        })
    }
}

/// Scheme and host lower-cased, no trailing slash: two spellings of one
/// address compare equal.
pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let path = if path.is_empty() { String::new() } else { format!("/{path}") };
            format!("{}://{}{path}", scheme.to_ascii_lowercase(), host.to_ascii_lowercase())
        }
        None => url.to_string(),
    }
}

/// `host:port`, lower-cased.
fn host_of(url: &str) -> Option<String> {
    let url = normalize_url(url);
    let host = url.split_once("://")?.1.split('/').next()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// Where a provider's key was found, for screens that say so without showing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// Saved with the provider.
    Saved,
    /// Read from this environment variable.
    Env(&'static str),
    None,
}

/// A saved provider: a server, how to talk to it, its key and the model to
/// start with there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderProfile {
    pub name: String,
    pub protocol: ApiProtocol,
    pub url: String,
    /// `None` looks in the provider's usual environment variable, then in
    /// `FLASHAGENT_API_KEY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Empty: whichever the server has loaded, else its first.
    pub model: String,
    /// Tokens of context to run models with, where the server lets the
    /// client choose (Ollama). `None`: FlashAgent's own choice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
}

impl ProviderProfile {
    pub fn new(name: impl Into<String>, protocol: ApiProtocol, url: impl Into<String>) -> Self {
        let url: String = url.into();
        Self { name: name.into(), protocol, url: url.trim().trim_end_matches('/').to_string(), api_key: None, model: String::new(), context_window: None }
    }

    pub fn from_preset(preset: &BackendPreset) -> Self {
        Self::new(preset.name, preset.protocol, preset.url)
    }

    /// An address typed by hand: named after the server it belongs to, else
    /// its host, and spoken to as `ApiProtocol::detect` reads it.
    pub fn from_url(url: &str) -> Self {
        let name = BackendPreset::same_server(url)
            .map(|p| p.name.to_string())
            .or_else(|| host_of(url))
            .unwrap_or_else(|| "Custom".to_string());
        Self::new(name, ApiProtocol::detect(url), url)
    }

    /// The variables its key may be in besides `FLASHAGENT_API_KEY`: those of
    /// the preset for its host, else those of its protocol.
    pub fn key_env(&self) -> &'static [&'static str] {
        let host = host_of(&self.url);
        if let Some(p) = PRESETS.iter().find(|p| !p.key_env.is_empty() && host.is_some() && host_of(p.url) == host) {
            return p.key_env;
        }
        match self.protocol {
            ApiProtocol::Anthropic => &["ANTHROPIC_API_KEY"],
            ApiProtocol::Gemini => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            ApiProtocol::OpenAi | ApiProtocol::Ollama => &[],
        }
    }

    /// The saved key, else the provider's variable, else `FLASHAGENT_API_KEY`.
    /// `env` reads a variable, so tests need not touch the real environment.
    pub fn resolve_key(&self, env: impl Fn(&str) -> Option<String>) -> (Option<String>, KeySource) {
        if let Some(key) = self.api_key.clone().filter(|k| !k.trim().is_empty()) {
            return (Some(key), KeySource::Saved);
        }
        for var in self.key_env().iter().copied().chain([GENERAL_KEY_ENV]) {
            if let Some(key) = env(var).filter(|k| !k.trim().is_empty()) {
                return (Some(key), KeySource::Env(var));
            }
        }
        (None, KeySource::None)
    }

    pub fn key_source(&self) -> KeySource {
        self.resolve_key(|var| std::env::var(var).ok()).1
    }

    /// Where requests go and with which key.
    pub fn endpoint(&self) -> Endpoint {
        let (key, _) = self.resolve_key(|var| std::env::var(var).ok());
        Endpoint::new(self.protocol, &self.url, key).with_context_window(self.context_window)
    }

    /// The same server under the same protocol, however the address is written.
    pub fn same_server(&self, other: &ProviderProfile) -> bool {
        self.protocol == other.protocol && normalize_url(&self.url) == normalize_url(&other.url)
    }
}

/// Hand-edited files spell the protocol many ways.
fn parse_protocol(text: &str) -> Option<ApiProtocol> {
    let key: String = text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    match key.as_str() {
        "openai" | "openaicompatible" | "oai" | "chatcompletions" => Some(ApiProtocol::OpenAi),
        "anthropic" | "claude" | "messages" => Some(ApiProtocol::Anthropic),
        "gemini" | "google" | "googlegemini" => Some(ApiProtocol::Gemini),
        "ollama" => Some(ApiProtocol::Ollama),
        _ => None,
    }
}

impl<'de> Deserialize<'de> for ProviderProfile {
    /// Field by field: a wrongly typed model or an unknown protocol (read from
    /// the address instead) must not cost the provider. Only one without an
    /// address is refused.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
        let url = text("url").ok_or_else(|| serde::de::Error::custom("a provider without a url"))?;
        let protocol = text("protocol").and_then(parse_protocol).unwrap_or_else(|| ApiProtocol::detect(url));
        let name = text("name").map_or_else(|| ProviderProfile::from_url(url).name, str::to_string);
        let mut profile = ProviderProfile::new(name, protocol, url);
        profile.api_key = text("api_key").map(str::to_string);
        profile.model = text("model").unwrap_or_default().to_string();
        profile.context_window = value.get("context_window").and_then(|v| v.as_u64()).filter(|&n| n > 0).map(|n| n as usize);
        Ok(profile)
    }
}

/// Entries that are not a provider are left out; the others are kept.
fn lenient_providers<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ProviderProfile>, D::Error> {
    let items = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

/// With nothing saved: LM Studio, where FlashAgent looked before there were
/// providers.
fn default_profile() -> &'static ProviderProfile {
    static DEFAULT: std::sync::LazyLock<ProviderProfile> = std::sync::LazyLock::new(|| ProviderProfile::from_preset(&PRESETS[0]));
    &DEFAULT
}

fn default_top_p() -> Option<f32> { Some(0.95) }
fn default_top_k() -> Option<u32> { Some(20) }
fn default_repeat_penalty() -> Option<f32> { Some(1.0) }
fn default_presence_penalty() -> Option<f32> { Some(0.0) }
fn default_min_p() -> Option<f32> { Some(0.0) }

/// UTF-8 with or without a BOM, or UTF-16 with one: Windows PowerShell 5.1
/// writes both, and a hand-edited config must not lose every setting to it.
fn decode_text(bytes: &[u8]) -> Option<String> {
    let utf16 = |little: bool| {
        let units: Vec<u16> = (2..bytes.len().saturating_sub(1))
            .step_by(2)
            .map(|i| [bytes[i], bytes[i + 1]])
            .map(|pair| if little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) })
            .collect();
        String::from_utf16(&units).ok()
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8(rest.to_vec()).ok(),
        [0xFF, 0xFE, ..] => utf16(true),
        [0xFE, 0xFF, ..] => utf16(false),
        _ => String::from_utf8(bytes.to_vec()).ok(),
    }
}

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
    /// Every saved provider. Empty until setup saves one; until then the
    /// active provider is LM Studio's default.
    #[serde(deserialize_with = "lenient_providers")]
    pub providers: Vec<ProviderProfile>,
    /// The name of the provider in use; the first one when it names none.
    pub active_provider: String,
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
    /// `--url`: the provider for this run, never saved, so a one-off launch
    /// does not move later ones. Choosing a saved provider drops it.
    #[serde(skip)]
    pub run_provider: Option<ProviderProfile>,
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
            providers: Vec::new(),
            active_provider: String::new(),
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
            run_provider: None,
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

/// Before providers there was one server: `backend_url`, `api_key` and
/// `model`. They become the one saved provider, named after the server's
/// preset. Where `providers` is already there, they are left behind unread.
fn migrate_single_backend(user: &mut serde_json::Map<String, serde_json::Value>) {
    let [url, key, model] = ["backend_url", "api_key", "model"].map(|k| user.remove(k));
    if user.get("providers").is_some_and(|p| p.is_array()) || (url.is_none() && key.is_none() && model.is_none()) {
        return;
    }
    let text = |v: Option<serde_json::Value>| v.and_then(|v| v.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty());
    let url = text(url).unwrap_or_else(|| default_profile().url.clone());
    let name = BackendPreset::same_server(&url).map_or("Default", |p| p.name);
    let mut profile = ProviderProfile::new(name, ApiProtocol::detect(&url), url);
    profile.api_key = text(key);
    profile.model = text(model).unwrap_or_default();
    if let Ok(value) = serde_json::to_value(&profile) {
        user.insert("providers".to_string(), serde_json::Value::Array(vec![value]));
        user.insert("active_provider".to_string(), serde_json::Value::String(profile.name));
    }
}

impl AppConfig {
    /// The provider turns go to: the one `--url` gave this run, else the
    /// saved active one, else LM Studio's default.
    pub fn active_profile(&self) -> &ProviderProfile {
        if let Some(run) = &self.run_provider {
            return run;
        }
        self.providers
            .iter()
            .find(|p| p.name == self.active_provider)
            .or_else(|| self.providers.first())
            .unwrap_or_else(|| default_profile())
    }

    /// Where a new model or key goes. With nothing saved yet, the default
    /// provider is saved first, as the one they belong to.
    pub fn active_profile_mut(&mut self) -> &mut ProviderProfile {
        match self.run_provider {
            Some(ref mut run) => run,
            None => {
                if self.providers.is_empty() {
                    self.providers.push(default_profile().clone());
                }
                let i = self.providers.iter().position(|p| p.name == self.active_provider).unwrap_or(0);
                self.active_provider = self.providers[i].name.clone();
                &mut self.providers[i]
            }
        }
    }

    /// The active provider's server, protocol and key.
    pub fn endpoint(&self) -> Endpoint {
        self.active_profile().endpoint()
    }

    pub fn profile(&self, name: &str) -> Option<&ProviderProfile> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// Makes a saved provider the active one; `false` when none has that name.
    pub fn activate(&mut self, name: &str) -> bool {
        if self.profile(name).is_none() {
            return false;
        }
        self.active_provider = name.to_string();
        self.run_provider = None;
        true
    }

    /// `base`, or `base 2`, `base 3`… if a saved provider already has it.
    pub fn unique_name(&self, base: &str) -> String {
        let base = base.trim();
        let base = if base.is_empty() { "Provider" } else { base };
        (1..)
            .map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") })
            .find(|name| self.profile(name).is_none())
            .unwrap_or_else(|| base.to_string())
    }

    /// Saved over the provider for the same server, whose name it keeps, or
    /// beside the others under a name of its own. Either way it becomes the
    /// active one. Returns its name.
    pub fn save_provider(&mut self, mut profile: ProviderProfile) -> String {
        let name = match self.providers.iter_mut().find(|p| p.same_server(&profile)) {
            Some(existing) => {
                existing.api_key = profile.api_key;
                existing.model = profile.model;
                existing.name.clone()
            }
            None => {
                profile.name = self.unique_name(&profile.name);
                let name = profile.name.clone();
                self.providers.push(profile);
                name
            }
        };
        self.activate(&name);
        name
    }

    /// Puts `profile` where `original` was. Refused when its name is taken
    /// by another provider or blank.
    pub fn replace_provider(&mut self, original: &str, profile: ProviderProfile) -> Result<(), String> {
        let name = profile.name.trim().to_string();
        if name.is_empty() {
            return Err("A provider needs a name".to_string());
        }
        if name != original && self.profile(&name).is_some() {
            return Err(format!("There is already a provider called {name}"));
        }
        if profile.url.trim().is_empty() {
            return Err("A provider needs an address".to_string());
        }
        let Some(i) = self.providers.iter().position(|p| p.name == original) else {
            return Err(format!("No provider called {original}"));
        };
        if self.active_provider == original {
            self.active_provider = name.clone();
        }
        self.providers[i] = ProviderProfile { name, ..profile };
        Ok(())
    }

    /// The active one hands over to the first that remains.
    pub fn remove_provider(&mut self, name: &str) -> bool {
        let before = self.providers.len();
        self.providers.retain(|p| p.name != name);
        if self.active_provider == name {
            self.active_provider = self.providers.first().map(|p| p.name.clone()).unwrap_or_default();
        }
        self.providers.len() != before
    }

    /// `--url`: this run talks to `url` and saves nothing about it. A saved
    /// provider at that address lends its name, key and model.
    pub fn use_url_for_this_run(&mut self, url: &str) {
        let wanted = ProviderProfile::from_url(url);
        let saved = self.providers.iter().find(|p| normalize_url(&p.url) == normalize_url(&wanted.url)).cloned();
        self.run_provider = Some(saved.unwrap_or(wanted));
    }

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

    /// Default when missing or invalid; a file that is not text is said so.
    pub fn load() -> Self {
        if let Some(path) = Self::default_path() {
            if path.exists() {
                if let Some(content) = std::fs::read(&path).ok().and_then(|bytes| decode_text(&bytes)) {
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
                eprintln!("Warning: {} could not be read as text; using the default settings.", path.display());
            }
        }
        Self::default()
    }

    /// One bad field must not cost the other thirty: failing fields fall back to
    /// their default and are named in the returned list. Only a file that is not
    /// a JSON object is a total loss.
    pub fn from_json_str(content: &str) -> (Self, Vec<String>) {
        let Ok(serde_json::Value::Object(mut user)) = serde_json::from_str::<serde_json::Value>(content)
        else {
            return (Self::default(), vec!["the file (it is not valid JSON)".to_string()]);
        };
        migrate_single_backend(&mut user);
        if let Ok(cfg) = serde_json::from_value::<Self>(serde_json::Value::Object(user.clone())) {
            return (cfg.normalised(), Vec::new());
        }

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
        // A provider is chosen by its name, so two may not share one.
        for i in 1..self.providers.len() {
            if self.providers[..i].iter().any(|p| p.name == self.providers[i].name) {
                let base = self.providers[i].name.clone();
                let name = (2..).map(|n| format!("{base} {n}")).find(|n| !self.providers.iter().any(|p| p.name == *n)).unwrap_or(base);
                self.providers[i].name = name;
            }
        }
        if self.profile(&self.active_provider).is_none() {
            self.active_provider = self.providers.first().map(|p| p.name.clone()).unwrap_or_default();
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
        let invalid = |e: serde_json::Error| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
        let mut value = serde_json::to_value(self).map_err(invalid)?;
        if let (Some(map), Some((url, key, model))) = (value.as_object_mut(), self.legacy_backend()) {
            map.insert("backend_url".into(), url.into());
            map.insert("model".into(), model.into());
            if let Some(key) = key {
                map.insert("api_key".into(), key.into());
            }
        }
        let json = serde_json::to_string_pretty(&value).map_err(invalid)?;
        // Written beside the file and renamed over it: a half-written config reads
        // as no config and sends the user back through the setup wizard.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, path)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// What a build from before providers reads (`backend_url`, `api_key`,
    /// `model`), written beside `providers` so going back to one keeps a
    /// working server. It spoke only the OpenAI protocol: the saved provider
    /// in use if that can be reached through it, else the first that can.
    /// Never read back: `providers` wins.
    fn legacy_backend(&self) -> Option<(String, Option<String>, String)> {
        let openai_url = |p: &ProviderProfile| match p.protocol {
            ApiProtocol::OpenAi => Some(p.url.clone()),
            ApiProtocol::Ollama => Some(format!("{}/v1", p.url.trim_end_matches("/api"))),
            ApiProtocol::Gemini => Some(format!("{}/openai", p.url)),
            ApiProtocol::Anthropic => None,
        };
        let active = self.providers.iter().find(|p| p.name == self.active_provider);
        active
            .into_iter()
            .chain(&self.providers)
            .find_map(|p| openai_url(p).map(|url| (url, p.api_key.clone(), p.model.clone())))
    }

    pub fn load_from(path: &Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        if !matches!(serde_json::from_str(&content), Ok(serde_json::Value::Object(_))) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a JSON object"));
        }
        Ok(Self::from_json_str(&content).0)
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
        cfg.active_profile_mut().url = "http://second/v1".into();
        cfg.save_to(&path).unwrap();
        assert_eq!(AppConfig::load_from(&path).unwrap().active_profile().url, "http://second/v1");
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(files.len(), 1, "a temporary file was left behind: {files:?}");
    }

    fn two_providers() -> AppConfig {
        let mut cfg = AppConfig::default();
        let mut local = ProviderProfile::new("Home", ApiProtocol::OpenAi, "http://on-disk/v1");
        local.model = "local-model".into();
        cfg.save_provider(local);
        let mut cloud = ProviderProfile::new("Cloud", ApiProtocol::Anthropic, "https://api.anthropic.com");
        cloud.api_key = Some("sk-ant-saved".into());
        cfg.save_provider(cloud);
        cfg.activate("Home");
        cfg
    }

    #[test]
    fn a_url_given_for_one_run_is_not_saved_but_one_chosen_later_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = two_providers();
        cfg.use_url_for_this_run("http://cli:9000/v1");
        assert_eq!(cfg.active_profile().url, "http://cli:9000/v1");
        assert_eq!(cfg.active_profile().model, "", "the run's server picks its own model");
        cfg.active_profile_mut().model = "cli-model".into();
        cfg.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path).unwrap();
        assert_eq!(loaded.providers, two_providers().providers, "nothing about the one-run server was saved");
        assert_eq!(loaded.active_profile().name, "Home");

        assert!(cfg.activate("Cloud"));
        cfg.save_to(&path).unwrap();
        assert_eq!(AppConfig::load_from(&path).unwrap().active_profile().name, "Cloud");
        assert!(cfg.run_provider.is_none(), "choosing a saved provider ends the one-run server");
    }

    #[test]
    fn a_url_given_for_one_run_at_a_saved_providers_address_borrows_its_key_and_model() {
        let mut cfg = two_providers();
        cfg.use_url_for_this_run("HTTPS://API.ANTHROPIC.COM/");
        let run = cfg.active_profile();
        assert_eq!((run.name.as_str(), run.protocol, run.api_key.as_deref()), ("Cloud", ApiProtocol::Anthropic, Some("sk-ant-saved")));
    }

    #[test]
    fn every_single_server_config_of_an_older_build_becomes_one_provider_named_after_its_preset() {
        for preset in BackendPreset::all() {
            let old = serde_json::json!({ "backend_url": preset.url, "api_key": "sk-old", "model": "old-model", "setup_completed": true });
            let (cfg, rejected) = AppConfig::from_json_str(&old.to_string());
            assert!(rejected.is_empty(), "{}: {rejected:?}", preset.name);
            assert_eq!(cfg.providers.len(), 1, "{}", preset.name);
            let p = cfg.active_profile();
            // Two presets can share an address; the first one names it.
            assert_eq!(p.name, BackendPreset::for_url(preset.url).unwrap().name);
            assert_eq!((p.url.as_str(), p.protocol), (preset.url, preset.protocol), "{}", preset.name);
            assert_eq!((p.api_key.as_deref(), p.model.as_str()), (Some("sk-old"), "old-model"), "{}", preset.name);
            assert_eq!(cfg.active_provider, p.name);
            assert!(cfg.setup_completed);
        }
    }

    #[test]
    fn an_older_config_keeps_the_protocol_its_address_was_spoken_to_with() {
        for (url, name, protocol) in [
            ("http://localhost:11434/v1", "Ollama", ApiProtocol::OpenAi),
            ("https://generativelanguage.googleapis.com/v1beta/openai", "Gemini", ApiProtocol::OpenAi),
            ("http://192.168.1.50:5000/v1/", "Default", ApiProtocol::OpenAi),
            ("http://gpu-box:1234/v1", "Default", ApiProtocol::OpenAi),
        ] {
            let (cfg, rejected) = AppConfig::from_json_str(&serde_json::json!({ "backend_url": url, "model": "m" }).to_string());
            assert!(rejected.is_empty(), "{url}: {rejected:?}");
            let p = cfg.active_profile();
            assert_eq!((p.name.as_str(), p.protocol, p.url.as_str()), (name, protocol, url.trim_end_matches('/')), "{url}");
            assert_eq!(p.api_key, None);
        }
    }

    #[test]
    fn an_older_config_that_named_only_a_model_keeps_it_on_the_default_server() {
        let (cfg, _) = AppConfig::from_json_str(r#"{"model":"kept"}"#);
        assert_eq!(cfg.active_profile().name, "LM Studio");
        assert_eq!(cfg.active_profile().model, "kept");
        let (fresh, _) = AppConfig::from_json_str("{}");
        assert!(fresh.providers.is_empty(), "nothing was chosen, so nothing is saved");
        assert_eq!(fresh.active_profile().url, "http://localhost:1234/v1");
    }

    #[test]
    fn an_older_build_gets_a_server_it_can_speak_to() {
        let mut cfg = AppConfig::default();
        let mut claude = ProviderProfile::new("Anthropic", ApiProtocol::Anthropic, "https://api.anthropic.com");
        claude.api_key = Some("sk-ant".into());
        let mut ollama = ProviderProfile::new("Ollama", ApiProtocol::Ollama, "http://localhost:11434");
        ollama.model = "qwen3".into();
        cfg.providers = vec![claude, ollama];
        cfg.active_provider = "Anthropic".into();
        assert_eq!(
            cfg.legacy_backend(),
            Some(("http://localhost:11434/v1".into(), None, "qwen3".into())),
            "Anthropic's own API meant nothing to it; Ollama's /v1 did"
        );
        cfg.providers.push(ProviderProfile::new("Gemini", ApiProtocol::Gemini, "https://generativelanguage.googleapis.com/v1beta"));
        cfg.active_provider = "Gemini".into();
        assert_eq!(cfg.legacy_backend().unwrap().0, "https://generativelanguage.googleapis.com/v1beta/openai");
        cfg.providers.retain(|p| p.protocol == ApiProtocol::Anthropic);
        assert_eq!(cfg.legacy_backend(), None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut ollama = ProviderProfile::new("Ollama", ApiProtocol::Ollama, "http://localhost:11434");
        ollama.context_window = Some(65_536);
        cfg.providers = vec![ollama];
        cfg.active_provider = "Ollama".into();
        cfg.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path).unwrap();
        assert_eq!(loaded.providers, cfg.providers, "the old fields are not read back: providers win");
        assert_eq!(loaded.active_profile().endpoint().context_window, Some(65_536));
    }

    #[test]
    fn the_old_fields_are_not_read_once_there_are_providers() {
        let json = serde_json::json!({
            "backend_url": "http://stale/v1",
            "model": "stale",
            "providers": [ { "name": "Home", "protocol": "openai", "url": "http://home/v1", "model": "fresh" } ],
            "active_provider": "Home"
        });
        let (cfg, rejected) = AppConfig::from_json_str(&json.to_string());
        assert!(rejected.is_empty(), "{rejected:?}");
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.active_profile().model, "fresh");
    }

    #[test]
    fn providers_survive_a_save_and_load_and_keys_stay_where_they_were_put() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = two_providers();
        cfg.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path).unwrap();
        assert_eq!(loaded.providers, cfg.providers);
        assert_eq!(loaded.active_provider, "Home");
        let text = std::fs::read_to_string(&path).unwrap();
        let on_disk: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(on_disk["backend_url"], "http://on-disk/v1", "an older build finds the server in use: {text}");
        assert_eq!(on_disk["model"], cfg.active_profile().model.as_str());
        assert_eq!(text.matches("\"api_key\"").count(), 1, "only the provider that has a key saves one: {text}");
        assert!(text.contains("\"protocol\": \"anthropic\""), "{text}");
    }

    #[test]
    fn a_garbled_provider_costs_only_itself() {
        let json = serde_json::json!({
            "model": "ignored, providers are there",
            "providers": [
                { "name": "Good", "protocol": "openai", "url": "http://good/v1" },
                { "name": "No address" },
                "not a provider",
                { "name": "Odd", "protocol": "Open AI-compatible", "url": "http://odd/v1", "model": 5, "api_key": "  " },
                { "url": "https://api.anthropic.com", "protocol": "klingon" }
            ],
            "active_provider": 7,
            "temperature": 0.25
        });
        let (cfg, rejected) = AppConfig::from_json_str(&json.to_string());
        assert_eq!(rejected, ["active_provider"]);
        assert_eq!(cfg.temperature, 0.25);
        let names: Vec<&str> = cfg.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Good", "Odd", "Anthropic"]);
        assert_eq!((cfg.providers[1].protocol, cfg.providers[1].model.as_str(), cfg.providers[1].api_key.as_deref()), (ApiProtocol::OpenAi, "", None));
        assert_eq!(cfg.providers[2].protocol, ApiProtocol::Anthropic, "an unreadable protocol is read from the address");
        assert_eq!(cfg.active_profile().name, "Good", "the first when the active one names none");
    }

    #[test]
    fn two_providers_never_share_a_name() {
        let json = r#"{"providers":[{"name":"Box","url":"http://a/v1"},{"name":"Box","url":"http://b/v1"}],"active_provider":"Box"}"#;
        let (cfg, _) = AppConfig::from_json_str(json);
        let names: Vec<&str> = cfg.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Box", "Box 2"]);
        assert_eq!(cfg.active_profile().url, "http://a/v1");
    }

    #[test]
    fn saving_a_provider_for_a_saved_server_updates_it_instead_of_adding_another() {
        let mut cfg = two_providers();
        let mut again = ProviderProfile::from_preset(BackendPreset::for_url("https://api.anthropic.com").unwrap());
        again.url = "https://API.anthropic.com/".into();
        again.model = "claude-new".into();
        assert_eq!(cfg.save_provider(again), "Cloud", "it keeps the name it was saved under");
        assert_eq!(cfg.providers.len(), 2);
        assert_eq!(cfg.active_profile().model, "claude-new");
        assert_eq!(cfg.active_profile().api_key, None, "the key given now replaces the old one");

        let other = ProviderProfile::new("Home", ApiProtocol::Ollama, "http://on-disk/v1");
        assert_eq!(cfg.save_provider(other), "Home 2", "another protocol at the same address is another provider");
        assert_eq!(cfg.providers.len(), 3);
    }

    #[test]
    fn a_provider_is_renamed_only_to_a_free_name_and_removing_the_active_one_hands_over() {
        let mut cfg = two_providers();
        let mut renamed = cfg.profile("Home").unwrap().clone();
        renamed.name = "Cloud".into();
        assert!(cfg.replace_provider("Home", renamed.clone()).is_err());
        renamed.name = "  ".into();
        assert!(cfg.replace_provider("Home", renamed.clone()).is_err());
        renamed.name = "Desk".into();
        cfg.replace_provider("Home", renamed).unwrap();
        assert_eq!(cfg.active_provider, "Desk", "the active one follows its new name");

        assert!(cfg.remove_provider("Desk"));
        assert_eq!(cfg.active_profile().name, "Cloud");
        assert!(!cfg.remove_provider("Desk"));
    }

    #[test]
    fn a_key_comes_from_the_provider_then_its_variable_then_flashagent_api_key() {
        let env = |vars: &'static [(&'static str, &'static str)]| move |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string());
        let all: &[(&str, &str)] = &[
            ("ANTHROPIC_API_KEY", "from-anthropic"),
            ("GEMINI_API_KEY", "from-gemini"),
            ("GOOGLE_API_KEY", "from-google"),
            ("OPENROUTER_API_KEY", "from-openrouter"),
            ("FLASHAGENT_API_KEY", "from-flashagent"),
        ];
        let preset = |name: &str| ProviderProfile::from_preset(BackendPreset::all().iter().find(|p| p.name == name).unwrap());

        let mut saved = preset("Anthropic");
        saved.api_key = Some("from-profile".into());
        assert_eq!(saved.resolve_key(env(all)), (Some("from-profile".into()), KeySource::Saved));
        saved.api_key = Some("   ".into());
        assert_eq!(saved.resolve_key(env(all)), (Some("from-anthropic".into()), KeySource::Env("ANTHROPIC_API_KEY")), "a blank key is no key");

        assert_eq!(preset("Gemini").resolve_key(env(all)).1, KeySource::Env("GEMINI_API_KEY"));
        assert_eq!(preset("Gemini").resolve_key(env(&[("GOOGLE_API_KEY", "g"), ("FLASHAGENT_API_KEY", "f")])).1, KeySource::Env("GOOGLE_API_KEY"));
        assert_eq!(preset("OpenRouter").resolve_key(env(all)).1, KeySource::Env("OPENROUTER_API_KEY"));
        assert_eq!(preset("OpenAI").resolve_key(env(all)).1, KeySource::Env("FLASHAGENT_API_KEY"), "no OPENAI_API_KEY set");
        assert_eq!(preset("LM Studio").resolve_key(env(all)).1, KeySource::Env("FLASHAGENT_API_KEY"));
        assert_eq!(preset("LM Studio").resolve_key(env(&[("ANTHROPIC_API_KEY", "a")])), (None, KeySource::None));
        assert_eq!(preset("DeepSeek").resolve_key(env(&[("DEEPSEEK_API_KEY", " ")])), (None, KeySource::None), "a blank variable is no key");

        // A hand-typed address of a known host, and a protocol alone, still find theirs.
        let typed = ProviderProfile::from_url("https://openrouter.ai/api/v1/");
        assert_eq!(typed.key_env(), ["OPENROUTER_API_KEY"]);
        let proxy = ProviderProfile::new("Proxy", ApiProtocol::Anthropic, "http://claude-proxy:8080");
        assert_eq!(proxy.resolve_key(env(all)).1, KeySource::Env("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn every_preset_has_an_address_that_parses_and_the_protocol_that_address_implies() {
        let mut names = std::collections::HashSet::new();
        for p in BackendPreset::all() {
            assert!(names.insert(p.name), "{} is listed twice", p.name);
            let (scheme, rest) = p.url.split_once("://").unwrap_or_else(|| panic!("{}: {}", p.name, p.url));
            let host = rest.split('/').next().unwrap_or_default();
            assert!(!host.is_empty() && !host.contains(' '), "{}: {}", p.name, p.url);
            assert_eq!(normalize_url(p.url), p.url, "{}: written as it is compared", p.name);
            assert_eq!(ApiProtocol::detect(p.url), p.protocol, "{}: a custom address typed the same way would speak another protocol", p.name);
            if p.is_local() {
                assert!(scheme == "http" && host.starts_with("localhost:"), "{}: {}", p.name, p.url);
            } else {
                assert!(scheme == "https", "{}: a key must not travel in the clear", p.name);
                assert_eq!(ProviderProfile::from_preset(p).key_env(), p.key_env, "{}", p.name);
            }
            assert_eq!(ProviderProfile::from_url(p.url).name, BackendPreset::for_url(p.url).unwrap().name);
        }
        let protocols: std::collections::HashSet<_> = BackendPreset::all().iter().map(|p| p.protocol).collect();
        assert_eq!(protocols.len(), ApiProtocol::ALL.len(), "every protocol is one pick away");
    }

    #[test]
    fn a_hand_typed_address_is_named_after_its_server_or_its_host() {
        assert_eq!(ProviderProfile::from_url("http://localhost:11434/v1").name, "Ollama");
        assert_eq!(ProviderProfile::from_url("http://localhost:11434/v1").protocol, ApiProtocol::OpenAi);
        assert_eq!(ProviderProfile::from_url("http://192.168.1.50:8000/v1").name, "192.168.1.50:8000");
        assert_eq!(ProviderProfile::from_url("  ").name, "Custom");
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
        assert_eq!(cfg.active_profile().url, "http://localhost:9999/v1");
        assert_eq!(cfg.active_profile().model, "my-model");
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
        assert_eq!(cfg.active_profile().model, "kept");
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
        assert_eq!(cfg.active_profile().model, "kept");
        assert_eq!(rejected, vec!["update_channel".to_string()]);
    }

    use super::*;

    #[test]
    fn test_config_default_and_roundtrip() {
        let temp = std::env::temp_dir().join("test_flashagent_config.json");
        let mut cfg = AppConfig {
            permission_mode: crate::PermissionMode::Bypass,
            goal_max_steps: Some(12),
            setup_completed: true,
            ..Default::default()
        };
        cfg.active_profile_mut().model = "test-model".into();

        cfg.save_to(&temp).expect("save should succeed");
        let loaded = AppConfig::load_from(&temp).expect("load should succeed");
        assert_eq!(loaded.active_profile().model, "test-model");
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

        let (cfg, rejected) = AppConfig::from_json_str(old_json);
        assert!(rejected.is_empty(), "older schema must deserialize cleanly: {rejected:?}");
        assert_eq!(cfg.active_profile().model, "qwen2.5-coder-32b");
        assert_eq!(cfg.active_profile().url, "http://localhost:1234/v1");
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
    fn a_config_saved_by_windows_powershell_keeps_its_settings() {
        let json = r#"{"model":"qwen","backend_url":"http://localhost:1234/v1"}"#;
        let bom = [&[0xEF, 0xBB, 0xBF][..], json.as_bytes()].concat();
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(json.encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        for bytes in [bom, utf16] {
            let text = decode_text(&bytes).expect("decoded");
            let (cfg, rejected) = AppConfig::from_json_str(&text);
            assert!(rejected.is_empty(), "{rejected:?}");
            assert_eq!(cfg.active_profile().model, "qwen");
        }
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

